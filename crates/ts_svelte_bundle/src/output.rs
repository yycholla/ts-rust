//! Resolved native output policy. Names stay inside the returned artifact root.
use rolldown::BundlerOptions;
use rolldown_common::{
    BundlerTransformOptions, ChunkFilenamesOutputOption, CodeSplittingMode, Either,
    ManualCodeSplittingOptions, MatchGroup, MatchGroupName, MatchGroupTest,
    PreserveEntrySignatures,
};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, sync::Arc};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OutputOptions {
    #[serde(default)]
    pub asset_url_mode: AssetUrlMode,
    pub entry_file_names: Option<String>,
    pub chunk_file_names: Option<String>,
    pub asset_file_names: Option<String>,
    pub preserve_entry_signatures: Option<EntrySignatures>,
    /// Rolldown does not implement Rollup's optional transitive-import preload
    /// hoisting pass. Only false is accepted; true would promise unavailable work.
    pub hoist_transitive_imports: Option<bool>,
    pub target: Option<String>,
    /// False produces one graph with dynamic imports inlined.
    pub code_splitting: Option<bool>,
    /// Exact output filenames -> exact resolved module IDs. Dependencies may
    /// join the group to preserve strict entry exports; Kit's fixed manifest
    /// module contains only literals and has no dependencies.
    #[serde(default)]
    pub fixed_chunks: BTreeMap<String, Vec<String>>,
    /// Ordered `chunkFileNames` rules: a chunk holding any module under
    /// `modulePrefix` uses `pattern` (adapter-node 6's `adapter-node-[name].js`).
    #[serde(default)]
    pub chunk_file_name_rules: Vec<ChunkFileNameRule>,
    /// Rollup `output.paths`: exact external id -> rendered import path.
    #[serde(default)]
    pub paths: BTreeMap<String, String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChunkFileNameRule {
    pub module_prefix: String,
    pub pattern: String,
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AssetUrlMode {
    #[default]
    ModuleRelative,
    ServiceWorkerLocation,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EntrySignatures {
    Strict,
    ExportsOnly,
    AllowExtension,
    False,
}
fn name(value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.contains(['\\', '\0', '?', '#', ':'])
        || value
            .split('/')
            .any(|s| s.is_empty() || s == "." || s == "..")
    {
        return Err(format!("Unsafe output filename {value:?}"));
    }
    Ok(())
}
pub(crate) fn configure(
    options: &mut BundlerOptions,
    policy: &OutputOptions,
) -> Result<(), String> {
    if policy.code_splitting == Some(false) {
        if !policy.fixed_chunks.is_empty() {
            return Err("Fixed chunks require code splitting".into());
        }
        options.code_splitting = Some(CodeSplittingMode::Bool(false));
    }
    for value in [
        &policy.entry_file_names,
        &policy.chunk_file_names,
        &policy.asset_file_names,
    ]
    .into_iter()
    .flatten()
    {
        name(value)?;
    }
    for rule in &policy.chunk_file_name_rules {
        name(&rule.pattern)?;
        if rule.module_prefix.is_empty() {
            return Err("chunkFileNames rules need a module prefix".into());
        }
    }
    if !policy.paths.is_empty() {
        options.paths = Some(rolldown_common::PathsOutputOption::FxHashMap(
            policy.paths.clone().into_iter().collect(),
        ));
    }
    if policy.hoist_transitive_imports == Some(true) {
        return Err("Native bundler does not support hoistTransitiveImports:true".into());
    }
    if let Some(value) = &policy.entry_file_names {
        options.entry_filenames = Some(value.clone().into());
    }
    if let Some(value) = &policy.chunk_file_names {
        options.chunk_filenames = Some(value.clone().into());
    }
    if let Some(value) = &policy.asset_file_names {
        options.asset_filenames = Some(value.clone().into());
    }
    if let Some(value) = policy.preserve_entry_signatures {
        options.preserve_entry_signatures = Some(match value {
            EntrySignatures::Strict => PreserveEntrySignatures::Strict,
            EntrySignatures::ExportsOnly => PreserveEntrySignatures::ExportsOnly,
            EntrySignatures::AllowExtension => PreserveEntrySignatures::AllowExtension,
            EntrySignatures::False => PreserveEntrySignatures::False,
        });
    }
    if let Some(target) = &policy.target {
        options.transform = Some(BundlerTransformOptions {
            target: Some(Either::Left(target.clone())),
            ..Default::default()
        });
    }
    let mut fixed_names = BTreeMap::new();
    if !policy.fixed_chunks.is_empty() {
        // A fixed manifest's literal exports must remain in its own chunk;
        // cross-module constant folding would erase that required boundary.
        options
            .optimization
            .get_or_insert_with(Default::default)
            .inline_const = Some(rolldown_common::InlineConstOption::Bool(false));
        let mut groups = Vec::new();
        let mut names = BTreeMap::new();
        let mut owners = BTreeMap::new();
        for (index, (filename, ids)) in policy.fixed_chunks.iter().enumerate() {
            name(filename)?;
            if filename.contains(['[', ']']) || ids.is_empty() {
                return Err(
                    "Fixed chunks require literal filenames and at least one module ID".into(),
                );
            }
            for id in ids {
                if id.is_empty() || owners.insert(id, filename).is_some() {
                    return Err("Fixed chunk module IDs must be nonempty and uniquely owned".into());
                }
            }
            let group_name = format!("__ts_svelte_fixed_{index}");
            names.insert(group_name.clone(), filename.clone());
            let ids = ids.clone();
            groups.push(MatchGroup {
                name: MatchGroupName::Static(group_name),
                test: Some(MatchGroupTest::Function(Arc::new(move |modules| {
                    let result = modules.iter().map(|id| ids.contains(id)).collect();
                    Box::pin(async move { Ok(result) })
                }))),
                // Rolldown requires recursive ownership with strict entry
                // signatures. Disabling it would discard the export contract.
                include_dependencies_recursively: Some(true),
                min_share_count: Some(1),
                ..Default::default()
            });
        }
        options.code_splitting = Some(CodeSplittingMode::Advanced(ManualCodeSplittingOptions {
            groups: Some(groups),
            ..Default::default()
        }));
        fixed_names = names;
    }
    if !fixed_names.is_empty() || !policy.chunk_file_name_rules.is_empty() {
        let fallback = policy
            .chunk_file_names
            .clone()
            .unwrap_or_else(|| "chunks/[name]-[hash].js".into());
        let rules = policy.chunk_file_name_rules.clone();
        options.chunk_filenames = Some(ChunkFilenamesOutputOption::Fn(Arc::new(move |chunk| {
            let result = fixed_names
                .get(chunk.name.as_str())
                .cloned()
                .or_else(|| {
                    rules
                        .iter()
                        .find(|rule| {
                            chunk
                                .module_ids
                                .iter()
                                .any(|id| id.as_str().starts_with(&rule.module_prefix))
                        })
                        .map(|rule| rule.pattern.clone())
                })
                .unwrap_or_else(|| fallback.clone());
            Box::pin(async move { Ok(result) })
        })));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn filename_policy_rejects_escapes_and_fixed_module_collisions() {
        for invalid in ["../app.js", "/app.js", "chunks//a.js", "C:/app.js"] {
            assert!(
                configure(
                    &mut BundlerOptions::default(),
                    &OutputOptions {
                        entry_file_names: Some(invalid.into()),
                        ..Default::default()
                    }
                )
                .is_err()
            );
        }
        assert!(
            configure(
                &mut BundlerOptions::default(),
                &OutputOptions {
                    fixed_chunks: BTreeMap::from([
                        ("one.js".into(), vec!["/a.js".into()]),
                        ("two.js".into(), vec!["/a.js".into()])
                    ]),
                    ..Default::default()
                }
            )
            .is_err()
        );
    }
}
