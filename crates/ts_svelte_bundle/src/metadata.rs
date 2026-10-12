//! Complete resolved module graph, including modules removed by tree shaking.
use rolldown::plugin::{HookGenerateBundleArgs, HookUsage, Plugin, PluginContext};
use serde::Serialize;
use std::{
    borrow::Cow,
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Module {
    pub id: String,
    pub is_entry: bool,
    pub is_external: bool,
    pub imports: Vec<String>,
    pub dynamic_imports: Vec<String>,
    pub importers: Vec<String>,
    pub dynamic_importers: Vec<String>,
    pub exports: Vec<String>,
}
#[derive(Debug, Default, Clone)]
pub struct Metadata(pub Arc<Mutex<BTreeMap<String, Module>>>);
impl Plugin for Metadata {
    fn name(&self) -> Cow<'static, str> {
        "ts-rust-build-metadata".into()
    }
    fn register_hook_usage(&self) -> HookUsage {
        HookUsage::GenerateBundle
    }
    async fn generate_bundle(
        &self,
        ctx: &PluginContext,
        _args: &mut HookGenerateBundleArgs<'_>,
    ) -> anyhow::Result<()> {
        let mut result = BTreeMap::new();
        for id in ctx.get_module_ids() {
            let info = ctx
                .get_module_info(&id)
                .ok_or_else(|| anyhow::anyhow!("Missing resolved module metadata for {id}"))?;
            result.insert(
                id.to_string(),
                Module {
                    id: id.to_string(),
                    is_entry: info.is_entry,
                    // Rolldown 1.2.13 supplies code for every loaded normal
                    // module and None only for ExternalModuleTask records.
                    is_external: info.code.is_none(),
                    imports: info.imported_ids.iter().map(ToString::to_string).collect(),
                    dynamic_imports: info
                        .dynamically_imported_ids
                        .iter()
                        .map(ToString::to_string)
                        .collect(),
                    importers: info.importers.iter().map(ToString::to_string).collect(),
                    dynamic_importers: info
                        .dynamic_importers
                        .iter()
                        .map(ToString::to_string)
                        .collect(),
                    exports: info.exports.iter().map(ToString::to_string).collect(),
                },
            );
        }
        *self
            .0
            .lock()
            .map_err(|_| anyhow::anyhow!("Metadata lock poisoned"))? = result;
        Ok(())
    }
}
