//! Worker assets use the worker's URL, including from nested split chunks.
//! File URL expressions enter Rolldown's AST before printing/maps, so generated
//! JavaScript is never rewritten with string matching after compilation.
use rolldown::plugin::{
    HookGenerateBundleArgs, HookLoadArgs, HookLoadOutput, HookLoadReturn, HookResolveFileUrlArgs,
    HookResolveFileUrlReturn, HookUsage, Plugin, PluginContext, SharedLoadPluginContext,
};
use rolldown_common::{EmittedAsset, ModuleType, Output, side_effects::HookSideEffects};
use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet},
    path::Path,
    sync::{Arc, Mutex},
};

#[derive(Debug, Default)]
struct State {
    modules: BTreeMap<String, String>,
    bytes: usize,
    files: BTreeMap<String, String>,
    chunks: BTreeMap<String, Vec<String>>,
}
#[derive(Debug, Default, Clone)]
pub(crate) struct WorkerAssets(Arc<Mutex<State>>);
impl WorkerAssets {
    pub fn chunk_files(&self, file: &str) -> Vec<String> {
        self.0
            .lock()
            .expect("worker asset state poisoned")
            .chunks
            .get(file)
            .cloned()
            .unwrap_or_default()
    }
}
impl Plugin for WorkerAssets {
    fn name(&self) -> Cow<'static, str> {
        "ts-rust-worker-assets".into()
    }
    fn register_hook_usage(&self) -> HookUsage {
        HookUsage::Load | HookUsage::ResolveFileUrl | HookUsage::GenerateBundle
    }
    async fn load(&self, ctx: SharedLoadPluginContext, args: &HookLoadArgs<'_>) -> HookLoadReturn {
        let end = args.id.find(['?', '#']).unwrap_or(args.id.len());
        let clean = &args.id[..end];
        let extension = Path::new(clean)
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or("");
        if !args
            .asserted_module_type
            .is_some_and(|t| matches!(t, ModuleType::Asset))
            && ![
                "png", "jpg", "jpeg", "gif", "svg", "webp", "avif", "ico", "woff", "woff2", "ttf",
                "otf", "mp4", "webm", "mp3", "wav", "pdf", "wasm",
            ]
            .contains(&extension)
        {
            return Ok(None);
        }
        let mut state = self
            .0
            .lock()
            .map_err(|_| anyhow::anyhow!("Worker asset state poisoned"))?;
        let reference = if let Some(reference) = state.files.get(clean) {
            reference.clone()
        } else {
            let metadata = std::fs::metadata(clean)?;
            anyhow::ensure!(
                metadata.is_file() && metadata.len() <= 64 * 1024 * 1024,
                "Worker asset is not a bounded regular file: {clean}"
            );
            let bytes = std::fs::read(clean)?;
            let total = state
                .bytes
                .checked_add(bytes.len())
                .ok_or_else(|| anyhow::anyhow!("Worker asset size overflow"))?;
            anyhow::ensure!(
                bytes.len() <= 64 * 1024 * 1024 && total <= 256 * 1024 * 1024,
                "Worker assets exceed native byte limit"
            );
            let reference = ctx
                .emit_file(
                    EmittedAsset {
                        name: Path::new(clean)
                            .file_name()
                            .map(|s| s.to_string_lossy().into_owned()),
                        original_file_name: Some(clean.into()),
                        file_name: None,
                        source: bytes.into(),
                    },
                    None,
                    None,
                )?
                .to_string();
            state.files.insert(clean.into(), reference.clone());
            state.bytes = total;
            reference
        };
        state.modules.insert(args.id.into(), reference.clone());
        ctx.associate_module_with_file_ref(args.id, &reference);
        ctx.add_watch_file(clean);
        Ok(Some(HookLoadOutput {
            code: format!("export default import.meta.ROLLDOWN_FILE_URL_{reference};").into(),
            module_type: Some(ModuleType::Js),
            side_effects: Some(HookSideEffects::False),
            ..Default::default()
        }))
    }
    async fn resolve_file_url(
        &self,
        _ctx: &PluginContext,
        args: &HookResolveFileUrlArgs<'_>,
    ) -> HookResolveFileUrlReturn {
        let state = self
            .0
            .lock()
            .map_err(|_| anyhow::anyhow!("Worker asset state poisoned"))?;
        if state
            .modules
            .get(args.module_id)
            .is_none_or(|r| r != args.reference_id)
        {
            return Ok(None);
        }
        let suffix = args
            .module_id
            .find(['?', '#'])
            .map_or("", |i| &args.module_id[i..]);
        let url = serde_json::to_string(&format!("{}{suffix}", args.file_name))?;
        Ok(Some(format!("new URL({url}, location.href).pathname")))
    }
    async fn generate_bundle(
        &self,
        ctx: &PluginContext,
        args: &mut HookGenerateBundleArgs<'_>,
    ) -> anyhow::Result<()> {
        let mut state = self
            .0
            .lock()
            .map_err(|_| anyhow::anyhow!("Worker asset state poisoned"))?;
        for output in args.bundle.iter() {
            if let Output::Chunk(chunk) = output {
                let files = chunk
                    .module_ids
                    .iter()
                    .filter_map(|id| state.modules.get(id.as_str()))
                    .map(|reference| ctx.get_file_name(reference).map(|s| s.to_string()))
                    .collect::<anyhow::Result<BTreeSet<_>>>()?;
                state
                    .chunks
                    .insert(chunk.filename.to_string(), files.into_iter().collect());
            }
        }
        Ok(())
    }
}
