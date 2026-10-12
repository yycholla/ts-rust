//! Native Kit 3 worker sidecar generation. This is the Vite build manifest at
//! `.vite/service-worker-manifest.json`, not the runtime `$app/manifest` module.
//! Contract: Kit 3.0.1 service-worker build plugin + Vite 8.0.12 manifest plugin
//! + Rolldown v1.0.0 `rolldown_plugin_vite_manifest/src/{lib,utils}.rs`.
//! Derived under MIT, copyright Svelte contributors, Vite contributors and
//! Rolldown contributors. Native bundle filenames remain authoritative.
//!
//! Integration: after worker tracking/finalization and before output collision
//! checks, call `generate_worker_manifest(&prepared.root, &bundle, &options)`
//! when generated.bundler_manifest is Some. Insert returned JSON with
//! artifacts::insert(outputs, format!("client/{}", result.file_name),
//! result.json.as_bytes(), "service-worker-manifest"). Never replace the client
//! `.vite/manifest.json`. Remove the existing pre-bundle Kit 3 rejection only
//! when this returned artifact is inserted. No filesystem reads/writes occur.
use serde::{Deserialize, Serialize, ser::SerializeMap};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Component, Path},
};
use ts_svelte_bundle::{BundleResult, Chunk, Target};

pub const WORKER_MANIFEST_FILE: &str = ".vite/service-worker-manifest.json";
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkerManifestOptions {
    /// Additional resolved external IDs, e.g. runtime env.js imports. Loaded
    /// module_graph isExternal records are also authoritative. Unknown edges
    /// fail instead of being silently mistaken for external imports.
    pub external_imports: BTreeSet<String>,
    /// Emitted CSS entry filename -> real entry name. A CSS module isn't an
    /// entry just because it is emitted; this information must be supplied.
    pub css_entries: BTreeMap<String, String>,
    /// Additional actual importedAssets metadata, keyed by emitted chunk file.
    /// CSS-owned asset relationships are derived from bundle.css directly.
    pub chunk_assets: BTreeMap<String, Vec<String>>,
    /// Explicit manifest keys for non-filesystem facade IDs. No ambient cwd or
    /// made-up source path is used to interpret virtual/bare facade identities.
    pub non_path_facade_keys: BTreeMap<String, String>,
}
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkerManifestEntry {
    pub file: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub names: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub src: Option<String>,
    #[serde(skip_serializing_if = "is_false")]
    pub is_entry: bool,
    #[serde(skip_serializing_if = "is_false")]
    pub is_dynamic_entry: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub imports: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub dynamic_imports: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub css: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub assets: Vec<String>,
}
fn is_false(value: &bool) -> bool {
    !value
}
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GeneratedWorkerManifest {
    pub file_name: String,
    pub entries: BTreeMap<String, WorkerManifestEntry>,
    /// Exact pretty-printed manifest object, including field ordering. Use this
    /// string for the output artifact instead of reserializing `entries`.
    pub json: String,
}
fn safe_file(file: &str) -> Result<(), String> {
    crate::artifacts::relative(file)
}
fn basename(file: &str) -> &str {
    file.rsplit('/').next().unwrap_or(file)
}
fn js_file(file: &str) -> bool {
    [".js", ".mjs", ".cjs"]
        .iter()
        .any(|ext| file.ends_with(ext))
}
fn relative_source(
    root: &Path,
    id: &str,
    options: &WorkerManifestOptions,
) -> Result<String, String> {
    let id = id.replace('\\', "/");
    if !Path::new(&id).is_absolute() {
        return options
            .non_path_facade_keys
            .get(&id)
            .cloned()
            .filter(|s| !s.is_empty() && !s.contains('\0'))
            .ok_or_else(|| {
                format!("Non-path worker facade {id:?} requires an explicit manifest key")
            });
    }
    fn parts(path: &Path) -> Result<Vec<String>, String> {
        let mut parts = vec![];
        for part in path.components() {
            match part {
                Component::RootDir | Component::CurDir => {}
                Component::ParentDir => {
                    parts.pop();
                }
                Component::Normal(value) => parts.push(
                    value
                        .to_str()
                        .ok_or("Worker manifest path is not UTF-8")?
                        .to_string(),
                ),
                Component::Prefix(_) => {
                    return Err(
                        "Cross-platform worker facade roots require explicit host normalization"
                            .into(),
                    );
                }
            }
        }
        Ok(parts)
    }
    let root = parts(root)?;
    let source = parts(Path::new(&id))?;
    let common = root.iter().zip(&source).take_while(|(a, b)| a == b).count();
    let result = std::iter::repeat_n("..", root.len() - common)
        .chain(source[common..].iter().map(String::as_str))
        .collect::<Vec<_>>()
        .join("/")
        .replace('\0', "");
    if result.is_empty() {
        return Err("Worker facade resolves to the project directory".into());
    }
    Ok(result)
}
fn chunk_key(
    root: &Path,
    chunk: &Chunk,
    options: &WorkerManifestOptions,
) -> Result<String, String> {
    match &chunk.facade_module_id {
        Some(id) => relative_source(root, id, options),
        None => Ok(format!("_{}", basename(&chunk.file_name))),
    }
}
fn push_unique(values: &mut Vec<String>, value: &str) {
    if !values.iter().any(|v| v == value) {
        values.push(value.into());
    }
}
fn array_index(key: &str) -> Option<u32> {
    key.parse::<u32>()
        .ok()
        .filter(|n| *n != u32::MAX && n.to_string() == key)
}
struct ManifestJson<'a> {
    entries: &'a BTreeMap<String, WorkerManifestEntry>,
    javascript_order: bool,
}
impl Serialize for ManifestJson<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut entries = self.entries.iter().collect::<Vec<_>>();
        if self.javascript_order {
            // Vite's CSS metadata compatibility hook JSON.parse/stringifies the
            // native sorted object, moving array-index keys ahead of all others.
            entries.sort_by(|(a, _), (b, _)| match (array_index(a), array_index(b)) {
                (Some(a), Some(b)) => a.cmp(&b),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => a.cmp(b),
            });
        }
        let mut map = serializer.serialize_map(Some(entries.len()))?;
        for (key, value) in entries {
            map.serialize_entry(key, value)?;
        }
        map.end()
    }
}

pub fn generate_worker_manifest(
    root: &Path,
    bundle: &BundleResult,
    options: &WorkerManifestOptions,
) -> Result<GeneratedWorkerManifest, String> {
    if bundle.target != Target::Client {
        return Err("Worker manifest requires a client-target worker bundle".into());
    }
    if !root.is_absolute()
        || root
            .components()
            .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
    {
        return Err("Worker manifest root must be absolute and normalized".into());
    }
    if bundle.chunks.len() + bundle.assets.len() > 100_000 {
        return Err("Worker manifest output count exceeds bounds".into());
    }
    let mut files = BTreeSet::new();
    let mut chunk_keys = BTreeMap::new();
    let mut identities = BTreeSet::new();
    for chunk in &bundle.chunks {
        safe_file(&chunk.file_name)?;
        if !files.insert(chunk.file_name.as_str()) {
            return Err("Duplicate worker output filename".into());
        }
        let key = chunk_key(root, chunk, options)?;
        if !identities.insert(key.clone()) {
            return Err(format!("Worker chunk manifest identity collision: {key}"));
        }
        chunk_keys.insert(chunk.file_name.clone(), key);
    }
    let mut assets = BTreeMap::new();
    for asset in &bundle.assets {
        safe_file(&asset.file_name)?;
        if !files.insert(asset.file_name.as_str()) {
            return Err("Duplicate worker asset/chunk filename".into());
        }
        assets.insert(asset.file_name.as_str(), asset);
    }
    if files.contains(WORKER_MANIFEST_FILE) {
        return Err("Worker bundle already emits the reserved worker manifest path".into());
    }
    for (file, name) in &options.css_entries {
        if name.is_empty()
            || !file.ends_with(".css")
            || assets
                .get(file.as_str())
                .is_none_or(|asset| asset.names.is_empty())
        {
            return Err(format!(
                "CSS entry {file} has no named emitted CSS asset or real entry name"
            ));
        }
    }
    for file in options.chunk_assets.keys().chain(bundle.css.chunks.keys()) {
        if !chunk_keys.contains_key(file) {
            return Err(format!(
                "Worker asset/CSS metadata names a missing chunk {file}"
            ));
        }
    }
    let mut entries = BTreeMap::new();
    let mut compatibility_metadata = false;
    for chunk in &bundle.chunks {
        let key = chunk_keys[&chunk.file_name].clone();
        let imports = |imports: &[String]| -> Result<Vec<String>, String> {
            imports
                .iter()
                .filter_map(|file| {
                    if let Some(key) = chunk_keys.get(file) {
                        return Some(Ok(key.clone()));
                    }
                    if options.external_imports.contains(file)
                        || bundle.module_graph.get(file).is_some_and(|m| m.is_external)
                    {
                        return None;
                    }
                    Some(Err(format!(
                        "Worker chunk {} refers to missing/unclassified output {file}",
                        chunk.file_name
                    )))
                })
                .collect()
        };
        let css = bundle
            .css
            .chunks
            .get(&chunk.file_name)
            .map(|m| m.own.clone())
            .unwrap_or_default();
        let mut imported_assets = vec![];
        // Preserve actual module evaluation order, then explicit host metadata.
        // CSS module maps themselves are key-sorted and are not evaluation order.
        for id in &chunk.module_ids {
            if let Some(module) = bundle.css.modules.get(id) {
                for asset in &module.assets {
                    push_unique(&mut imported_assets, asset);
                }
            }
        }
        for file in &css {
            if !assets.contains_key(file.as_str()) {
                return Err(format!("Worker CSS dependency {file} was not emitted"));
            }
            for module in bundle.css.modules.values().filter(|m| m.file_name == *file) {
                for asset in &module.assets {
                    push_unique(&mut imported_assets, asset);
                }
            }
        }
        for asset in options
            .chunk_assets
            .get(&chunk.file_name)
            .into_iter()
            .flatten()
        {
            push_unique(&mut imported_assets, asset);
        }
        for asset in &imported_assets {
            if !assets.contains_key(asset.as_str()) {
                return Err(format!("Worker asset dependency {asset} was not emitted"));
            }
        }
        if !css.is_empty() || !imported_assets.is_empty() {
            compatibility_metadata = true;
        }
        entries.insert(
            key.clone(),
            WorkerManifestEntry {
                file: chunk.file_name.clone(),
                name: Some(chunk.name.clone()),
                src: chunk.facade_module_id.as_ref().map(|_| key),
                is_entry: chunk.is_entry,
                is_dynamic_entry: chunk.is_dynamic_entry,
                imports: imports(&chunk.imports)?,
                dynamic_imports: imports(&chunk.dynamic_imports)?,
                css,
                assets: imported_assets,
                ..Default::default()
            },
        );
    }
    for asset in &bundle.assets {
        if asset.names.is_empty() {
            continue;
        }
        let key = asset
            .original_file_names
            .first()
            .cloned()
            .unwrap_or_else(|| format!("_{}", basename(&asset.file_name)));
        if key.is_empty()
            || asset
                .original_file_names
                .iter()
                .any(|name| name.is_empty() || name.contains('\0'))
        {
            return Err("Worker asset has an invalid original source identity".into());
        }
        let name = options.css_entries.get(&asset.file_name).cloned();
        let mut record = WorkerManifestEntry {
            file: asset.file_name.clone(),
            src: Some(key.clone()),
            is_entry: name.is_some(),
            names: name.as_ref().map(|_| asset.names.clone()),
            name,
            ..Default::default()
        };
        // The compatibility plugin also attaches actual CSS asset metadata to
        // asset records. Its own emitted bytes are not an imported CSS relation.
        for module in bundle
            .css
            .modules
            .values()
            .filter(|m| m.file_name == asset.file_name)
        {
            for file in &module.assets {
                if !assets.contains_key(file.as_str()) {
                    return Err(format!(
                        "Worker CSS asset refers to missing emitted asset {file}"
                    ));
                }
                push_unique(&mut record.assets, file);
            }
        }
        compatibility_metadata |= !record.assets.is_empty();
        for key in std::iter::once(&key).chain(asset.original_file_names.iter()) {
            if entries
                .get(key)
                .is_some_and(|prior: &WorkerManifestEntry| js_file(&prior.file))
            {
                continue;
            }
            // Output vector is the only actual asset emission order available.
            // Upstream overwrites non-JS asset aliases with the later emission.
            entries.insert(key.clone(), record.clone());
        }
    }
    let json = serde_json::to_string_pretty(&ManifestJson {
        entries: &entries,
        javascript_order: compatibility_metadata,
    })
    .map_err(|e| e.to_string())?;
    if json.len() > 16 * 1024 * 1024 {
        return Err("Worker manifest exceeds output bounds".into());
    }
    Ok(GeneratedWorkerManifest {
        file_name: WORKER_MANIFEST_FILE.into(),
        entries,
        json,
    })
}

#[cfg(test)]
#[path = "worker_manifest_tests.rs"]
mod tests;
