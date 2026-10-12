//! SvelteKit-specific native Rolldown plugins. No dependency on the component
//! compiler, generic component bundler, JS execution host, or application FS.
//!
//! Insert RemotePlugin after component preprocessing. It lowers still-typed
//! remote scripts itself because Rolldown's ordinary TypeScript transform runs
//! after plugin transform hooks. Maps returned here address this hook's input;
//! Rolldown composes earlier load/transform maps exactly once.
//!
//! Each plugin instance is single-build. Retain its RemoteState across generation
//! to recover discovered identities, diagnostics and actual emitted facade paths.
//! Build seeds force known client-only remotes into the server graph; otherwise
//! imports discovered by Rolldown register remotes lazily. No inferred export
//! metadata or filename substitutes are used.
use anyhow::{Context, Result, bail, ensure};
use rolldown_common::{EmittedChunk, ModuleType, PreserveEntrySignatures};
use rolldown_plugin::{
    HookBuildStartArgs, HookLoadArgs, HookLoadOutput, HookLoadReturn, HookResolveIdArgs,
    HookResolveIdOutput, HookResolveIdReturn, HookTransformArgs, HookTransformOutput,
    HookTransformOutputMap, HookTransformReturn, HookUsage, Plugin, PluginContext, Pluginable,
    SharedLoadPluginContext, SharedTransformPluginContext,
};
use rolldown_sourcemap::SourceMap;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    borrow::Cow,
    collections::BTreeMap,
    sync::{Arc, Mutex, MutexGuard},
};
use ts_svelte_kit::{
    build_finalize::{JavaScriptArtifact, SourceMapPolicy},
    build_remote::{
        self, EvaluatedRemoteExports, RemoteDiscoveryInput, RemoteIdentity, RemoteTarget,
        RemoteTransformInput, RemoteTransformOutput,
    },
    sync_versions::KitSyncVersion,
};

const VIRTUAL_PREFIX: &str = "\0sveltekit-remote:";
const MAX_REMOTES: usize = 16_384;
const MAX_DIAGNOSTICS: usize = 65_536;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RemotePluginOptions {
    pub kit_version: String,
    pub project_root: String,
    pub target: RemoteTarget,
    pub enabled: bool,
    #[serde(default)]
    pub development: bool,
    #[serde(default = "enabled")]
    pub sourcemap: bool,
    pub module_extensions: Vec<String>,
    /// Exact resolved module ID -> inspected package ancestry has a Kit peer.
    /// Kit3 dependency remotes require evidence; omission is an error, not false.
    #[serde(default)]
    pub package_peers: BTreeMap<String, bool>,
    /// Known remotes to force into a production server graph, including modules
    /// referenced only by client code. Use actual resolver identities.
    #[serde(default)]
    pub seeds: Vec<String>,
    pub client_runtime_id: Option<String>,
    /// Hash -> actual evaluated server metadata. Empty metadata is not a fallback
    /// for missing client exports; every transformed client remote must have it.
    #[serde(default)]
    pub metadata: BTreeMap<String, EvaluatedRemoteExports>,
    /// Explicit remote TypeScript lowering policy, not implicit tsconfig lookup.
    #[serde(default)]
    pub typescript: ts_svelte_transpile::TransformOptions,
}
fn enabled() -> bool {
    true
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteRecord {
    pub identity: RemoteIdentity,
    pub virtual_id: String,
    pub entry_name: String,
    pub reference_id: Option<String>,
    pub transformed: bool,
    /// Hash of the actual hook input, not its transformed output or original TS
    /// source before any earlier plugin. Detects inconsistent duplicate input.
    pub source_sha256: Option<String>,
}
#[derive(Debug, Default)]
struct StateData {
    started: bool,
    production_server: bool,
    records: BTreeMap<String, RemoteRecord>,
    diagnostics: Vec<Value>,
}
#[derive(Debug, Clone, Default)]
pub struct RemoteState(Arc<Mutex<StateData>>);
impl RemoteState {
    fn lock(&self) -> Result<MutexGuard<'_, StateData>> {
        self.0
            .lock()
            .map_err(|_| anyhow::anyhow!("Remote plugin state lock poisoned"))
    }
    pub fn records(&self) -> Result<Vec<RemoteRecord>> {
        Ok(self.lock()?.records.values().cloned().collect())
    }
    pub fn diagnostics(&self) -> Result<Vec<Value>> {
        Ok(self.lock()?.diagnostics.clone())
    }
    fn diagnose(&self, id: &str, phase: &str, message: &str, details: Vec<Value>) -> Result<()> {
        let mut data = self.lock()?;
        ensure!(
            data.diagnostics.len() + details.len() + 1 <= MAX_DIAGNOSTICS,
            "Remote plugin diagnostic limit exceeded"
        );
        data.diagnostics.extend(details);
        data.diagnostics.push(json!({"source":"native-kit-remote","id":id,"phase":phase,"severity":"error","message":message}));
        Ok(())
    }
    fn reserve(&self, identity: RemoteIdentity, source: Option<&str>) -> Result<bool> {
        let mut data = self.lock()?;
        let hash = identity.hash.clone();
        if let Some(record) = data.records.get_mut(&hash) {
            ensure!(
                record.identity == identity,
                "Remote hash collision {} between {} and {}",
                hash,
                record.identity.module_id,
                identity.module_id
            );
            if let Some(source) = source {
                let digest = format!("{:x}", Sha256::digest(source.as_bytes()));
                ensure!(
                    record
                        .source_sha256
                        .as_ref()
                        .is_none_or(|old| old == &digest),
                    "Remote module {} was transformed with different input in one build",
                    identity.module_id
                );
                record.source_sha256 = Some(digest);
                record.transformed = true;
            }
            return Ok(false);
        }
        ensure!(
            data.records.len() < MAX_REMOTES,
            "Remote module count exceeds {MAX_REMOTES}"
        );
        data.records.insert(
            hash.clone(),
            RemoteRecord {
                identity,
                virtual_id: format!("{VIRTUAL_PREFIX}{hash}"),
                entry_name: format!("remote-{hash}"),
                reference_id: None,
                transformed: source.is_some(),
                source_sha256: source
                    .map(|source| format!("{:x}", Sha256::digest(source.as_bytes()))),
            },
        );
        Ok(true)
    }
    fn virtual_source(&self, id: &str) -> Result<String> {
        let hash = id
            .strip_prefix(VIRTUAL_PREFIX)
            .context("Not a remote virtual entry")?;
        let data = self.lock()?;
        let record = data
            .records
            .get(hash)
            .with_context(|| format!("No remote identity registered for virtual entry {id:?}"))?;
        Ok(format!(
            "import * as m from {};\nexport default m;",
            serde_json::to_string(&record.identity.module_id)?
        ))
    }
    /// Resolve entries solely from emitted facade identities. Missing, duplicate,
    /// untransformed or renamed-away entries are errors; no filename is invented.
    pub fn emitted_chunks(&self, facades: &[EmittedFacade]) -> Result<Vec<EmittedRemote>> {
        let data = self.lock()?;
        ensure!(
            data.production_server,
            "Remote emitted chunk lookup requires a production server build"
        );
        let mut by_id = BTreeMap::new();
        for facade in facades {
            if let Some(id) = &facade.facade_module_id {
                if id.starts_with(VIRTUAL_PREFIX) {
                    safe_emitted_name(&facade.file_name)?;
                    ensure!(
                        by_id.insert(id.clone(), facade).is_none(),
                        "Duplicate emitted remote facade {id:?}"
                    );
                }
            }
        }
        let mut output = vec![];
        for record in data.records.values() {
            ensure!(
                record.transformed,
                "Remote source {} was not transformed",
                record.identity.module_id
            );
            let reference_id = record
                .reference_id
                .as_ref()
                .context("Remote entry was not emitted")?;
            let facade = by_id.remove(&record.virtual_id).with_context(|| {
                format!("Missing emitted facade for {}", record.identity.module_id)
            })?;
            ensure!(
                facade.exports.iter().any(|name| name == "default"),
                "Remote facade {} has no namespace default export",
                facade.file_name
            );
            output.push(EmittedRemote {
                hash: record.identity.hash.clone(),
                file: record.identity.file.clone(),
                module_id: record.identity.module_id.clone(),
                virtual_id: record.virtual_id.clone(),
                reference_id: reference_id.clone(),
                chunk: facade.file_name.clone(),
            });
        }
        ensure!(
            by_id.is_empty(),
            "Emitted remote facade has no registered source identity"
        );
        Ok(output)
    }
}
/// Neutral facts copied from actual bundle chunks; no bundler crate dependency.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EmittedFacade {
    pub facade_module_id: Option<String>,
    pub file_name: String,
    pub exports: Vec<String>,
}
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EmittedRemote {
    pub hash: String,
    pub file: String,
    pub module_id: String,
    pub virtual_id: String,
    pub reference_id: String,
    pub chunk: String,
}

#[derive(Debug, Clone)]
pub struct RemotePlugin {
    options: Arc<RemotePluginOptions>,
    state: RemoteState,
}
impl RemotePlugin {
    pub fn new(options: RemotePluginOptions) -> Result<Self> {
        KitSyncVersion::parse(&options.kit_version).map_err(anyhow::Error::msg)?;
        absolute(&options.project_root)?;
        ensure!(
            options.seeds.len() <= MAX_REMOTES && options.metadata.len() <= MAX_REMOTES,
            "Remote plugin inventory exceeds {MAX_REMOTES}"
        );
        let state = RemoteState::default();
        state.lock()?.production_server =
            options.target == RemoteTarget::Server && !options.development;
        let plugin = Self {
            options: Arc::new(options),
            state,
        };
        for (hash, metadata) in &plugin.options.metadata {
            ensure!(
                hash == &metadata.hash && build_remote::remote_hash(&metadata.file) == *hash,
                "Invalid evaluated remote metadata identity for {hash}"
            );
        }
        for id in &plugin.options.seeds {
            ensure!(
                plugin.discover(id)?,
                "Seed {id} is not a remote module under this Kit profile"
            );
            ensure!(
                plugin.options.enabled,
                "Remote functions require experimental.remoteFunctions: true"
            );
            ensure!(
                plugin.options.target != RemoteTarget::ServiceWorker,
                "Remote functions cannot run in a service worker"
            );
            // Validate/collision-check seeds before any emit callback can run.
            plugin.state.reserve(plugin.identity(id)?, None)?;
        }
        Ok(plugin)
    }
    pub fn state(&self) -> RemoteState {
        self.state.clone()
    }
    pub fn into_plugin(self) -> Arc<Pluginable> {
        Plugin::new_shared(self)
    }
    fn discovery(&self, id: &str) -> RemoteDiscoveryInput {
        RemoteDiscoveryInput {
            kit_version: self.options.kit_version.clone(),
            module_id: id.into(),
            module_extensions: self.options.module_extensions.clone(),
            package_has_kit_peer: self.options.package_peers.get(id).copied(),
        }
    }
    fn discover(&self, id: &str) -> Result<bool> {
        if id.starts_with('\0') {
            return Ok(false);
        }
        // Fast exclusion only. Candidate IDs still receive full exact-version,
        // path/query and package-peer validation in the pure framework layer.
        let basename = id.rsplit('/').next().unwrap_or("");
        if !basename.contains(".remote.") && !basename.starts_with("remote.") {
            return Ok(false);
        }
        build_remote::discover_remote(&self.discovery(id)).map_err(anyhow::Error::msg)
    }
    fn identity(&self, id: &str) -> Result<RemoteIdentity> {
        let root = absolute(&self.options.project_root)?;
        let module = absolute(id)?;
        let common = root.iter().zip(&module).take_while(|(a, b)| a == b).count();
        let file = std::iter::repeat_n("..", root.len() - common)
            .chain(module[common..].iter().copied())
            .collect::<Vec<_>>()
            .join("/");
        Ok(RemoteIdentity {
            hash: build_remote::remote_hash(&file),
            file,
            module_id: id.into(),
        })
    }
    fn emit(&self, ctx: &PluginContext, hash: &str) -> Result<()> {
        // Emit under this short native-only lock: emit_chunk registers metadata
        // synchronously and does not execute callbacks, so no await/reentrancy.
        // This makes reservation+emission atomic across concurrent transform hooks.
        let mut state = self.state.lock()?;
        let record = state
            .records
            .get_mut(hash)
            .context("Remote entry reservation missing")?;
        if record.reference_id.is_some() {
            return Ok(());
        }
        let reference = ctx.emit_chunk(EmittedChunk {
            id: record.virtual_id.clone(),
            name: Some(record.entry_name.clone().into()),
            preserve_entry_signatures: Some(PreserveEntrySignatures::Strict),
            ..Default::default()
        })?;
        record.reference_id = Some(reference.to_string());
        Ok(())
    }
    fn prepare(
        &self,
        id: &str,
        source: &str,
        module_type: &ModuleType,
    ) -> Result<Option<RemoteTransformOutput>> {
        let discovered = match self.discover(id) {
            Ok(value) => value,
            Err(error) => {
                self.state
                    .diagnose(id, "discovery", &format!("{error:#}"), vec![])?;
                return Err(error);
            }
        };
        if !discovered {
            return Ok(None);
        }
        let prepared: Result<RemoteTransformOutput> = (|| {
            ensure!(
                self.options.enabled,
                "Remote functions require experimental.remoteFunctions: true"
            );
            ensure!(
                self.options.target != RemoteTarget::ServiceWorker,
                "Remote functions cannot be transformed for service workers"
            );
            let (code, map) = match module_type {
                ModuleType::Js => (source.to_owned(), None),
                ModuleType::Ts | ModuleType::Tsx | ModuleType::Jsx => {
                    let mut options = self.options.typescript.clone();
                    options.source_map = self.options.sourcemap;
                    let transformed = match ts_svelte_transpile::transform(source, id, &options) {
                        Ok(value) => value,
                        Err(failure) => {
                            let message = failure.to_string();
                            let details = failure
                                .diagnostics
                                .iter()
                                .map(serde_json::to_value)
                                .collect::<std::result::Result<Vec<_>, _>>()?;
                            self.state.diagnose(id, "typescript", &message, details)?;
                            return Err(anyhow::anyhow!(
                                "Native remote TypeScript preprocessing failed for {id}: {message}"
                            ));
                        }
                    };
                    if !transformed.diagnostics.is_empty() {
                        let mut state = self.state.lock()?;
                        ensure!(
                            state.diagnostics.len() + transformed.diagnostics.len()
                                <= MAX_DIAGNOSTICS,
                            "Remote plugin diagnostic limit exceeded"
                        );
                        for diagnostic in &transformed.diagnostics {
                            state.diagnostics.push(serde_json::to_value(diagnostic)?);
                        }
                    }
                    (transformed.code, transformed.map)
                }
                _ => bail!(
                    "Remote module {id} requires JavaScript/TypeScript preprocessing; unsupported module type {module_type:?}"
                ),
            };
            let identity = self.identity(id)?;
            let input = RemoteTransformInput {
                discovery: self.discovery(id),
                project_root: self.options.project_root.clone(),
                enabled: self.options.enabled,
                development: self.options.development,
                target: self.options.target,
                artifact_name: format!("remote-{}.js", identity.hash),
                artifact: JavaScriptArtifact {
                    code,
                    map,
                    map_file: None,
                    pending: vec![],
                    receipt_sha256: None,
                },
                source_maps: if self.options.sourcemap {
                    SourceMapPolicy::Preserve
                } else {
                    SourceMapPolicy::Drop
                },
                client_runtime_id: self.options.client_runtime_id.clone(),
                metadata: self.options.metadata.get(&identity.hash).cloned(),
            };
            let output = build_remote::transform_remote(&input).map_err(anyhow::Error::msg)?;
            self.state.reserve(output.identity.clone(), Some(source))?;
            Ok(output)
        })();
        match prepared {
            Ok(value) => Ok(Some(value)),
            Err(error) => {
                self.state
                    .diagnose(id, "transform", &format!("{error:#}"), vec![])?;
                Err(error)
            }
        }
    }
}
fn absolute(path: &str) -> Result<Vec<&str>> {
    ensure!(
        path.starts_with('/')
            && !path.contains(['\\', '\0', '?', '#'])
            && !path.chars().any(char::is_control),
        "Expected normalized absolute POSIX module identity: {path:?}"
    );
    if path == "/" {
        return Ok(vec![]);
    }
    let parts: Vec<_> = path[1..].split('/').collect();
    ensure!(
        parts
            .iter()
            .all(|part| !part.is_empty() && !matches!(*part, "." | "..")),
        "Unnormalized absolute module identity: {path:?}"
    );
    Ok(parts)
}
fn safe_emitted_name(path: &str) -> Result<()> {
    ensure!(
        !path.is_empty()
            && !path.starts_with('/')
            && !path.contains(['\\', '\0', ':', '?', '#'])
            && !path.chars().any(char::is_control)
            && path
                .split('/')
                .all(|part| !part.is_empty() && !matches!(part, "." | "..")),
        "Invalid emitted remote filename {path:?}"
    );
    Ok(())
}
impl Plugin for RemotePlugin {
    fn name(&self) -> Cow<'static, str> {
        "ts-rust-sveltekit-remote".into()
    }
    fn register_hook_usage(&self) -> HookUsage {
        HookUsage::BuildStart | HookUsage::ResolveId | HookUsage::Load | HookUsage::Transform
    }
    async fn build_start(&self, ctx: &PluginContext, _args: &HookBuildStartArgs<'_>) -> Result<()> {
        {
            let mut state = self.state.lock()?;
            ensure!(
                !state.started,
                "RemotePlugin instances are single-build; create a new instance for another graph"
            );
            state.started = true;
        }
        if self.options.target == RemoteTarget::Server && !self.options.development {
            for record in self.state.records()? {
                self.emit(ctx, &record.identity.hash)?;
            }
        }
        Ok(())
    }
    async fn resolve_id(
        &self,
        _ctx: &PluginContext,
        args: &HookResolveIdArgs<'_>,
    ) -> HookResolveIdReturn {
        if !args.specifier.starts_with(VIRTUAL_PREFIX) {
            return Ok(None);
        }
        ensure!(
            self.options.target == RemoteTarget::Server,
            "Server remote virtual entry cannot be imported into a client/worker graph"
        );
        self.state.virtual_source(args.specifier)?;
        Ok(Some(HookResolveIdOutput::from_id(args.specifier)))
    }
    async fn load(&self, _ctx: SharedLoadPluginContext, args: &HookLoadArgs<'_>) -> HookLoadReturn {
        if !args.id.starts_with(VIRTUAL_PREFIX) {
            return Ok(None);
        }
        ensure!(
            self.options.target == RemoteTarget::Server,
            "Server remote virtual entry cannot be loaded into a client/worker graph"
        );
        Ok(Some(HookLoadOutput {
            code: self.state.virtual_source(args.id)?.into(),
            module_type: Some(ModuleType::Js),
            ..Default::default()
        }))
    }
    async fn transform(
        &self,
        ctx: SharedTransformPluginContext,
        args: &HookTransformArgs<'_>,
    ) -> HookTransformReturn {
        let Some(output) = self.prepare(args.id, args.code.as_ref(), args.module_type)? else {
            return Ok(None);
        };
        if output.entry.is_some() {
            self.emit(&ctx, &output.identity.hash)?;
        }
        let map = if self.options.sourcemap {
            if let Some(map) = output.artifact.map {
                HookTransformOutputMap::from(SourceMap::from_json(serde_json::from_value(map)?)?)
            } else {
                HookTransformOutputMap::from(rolldown_sourcemap::empty_sourcemap())
            }
        } else {
            HookTransformOutputMap::Null
        };
        Ok(Some(HookTransformOutput {
            code: Some(output.artifact.code),
            map,
            module_type: Some(ModuleType::Js),
            ..Default::default()
        }))
    }
}
#[cfg(test)]
mod tests;
