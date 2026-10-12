//! Pure Kit build dependency traversal over explicit emitted-graph metadata.
//!
//! Ported from SvelteKit exports/vite/build/utils.js (`find_deps`,
//! `resolve_symlinks`, `filter_fonts`), client bootstrap dependency assembly and
//! server node eager-CSS selection. Copyright (c) Svelte contributors, MIT; see
//! SvelteKit/LICENSE. The registered Kit 2 profiles share traversal semantics;
//! Kit 3 changes font metadata and client runtime preloads.
//!
//! Manifest record keys are opaque source/graph IDs, not necessarily output
//! filenames. Edges refer to those keys or explicit aliases. CSS/assets are
//! output-relative paths. Keep absent arrays distinct from present empty arrays:
//! JavaScript's empty arrays still create stylesheet groups. `real_paths` is a
//! caller-provided filesystem snapshot, never populated by this pure module.
//! Only facade/src identities resolve entrypoints; arbitrary included module IDs
//! may occur in several chunks and are indexed separately, not guessed entries.
//!
//! All graph edges and asset references are validated before traversal. Missing
//! records never turn into empty successful dependencies. The iterative DFS
//! preserves upstream insertion order, including its first-visit behavior when
//! a shared chunk is encountered dynamically before a later static edge.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::build_nodes::BundleStrategy;
use crate::sync_versions::KitSyncVersion;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EmittedDependencyGraph {
    pub kit_version: String,
    pub project_root: PathBuf,
    pub records: BTreeMap<String, EmittedModuleRecord>,
    /// Exact source/manifest aliases, for already resolved symlink identities.
    pub aliases: BTreeMap<String, String>,
    /// Observed absolute path -> canonical absolute path, or None for confirmed
    /// absence. A missing key means no observation was supplied. Kit 3 package
    /// font fallback requires these observations rather than guessing existence.
    pub real_paths: BTreeMap<PathBuf, Option<PathBuf>>,
    /// Preserve bundler output order. Different source records may share one
    /// emitted asset, but every emitted filename occurs here exactly once.
    pub assets: Vec<EmittedAssetRecord>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EmittedModuleRecord {
    pub file: String,
    /// Vite manifest `src`; also used to recover original font filenames.
    pub source: Option<String>,
    pub facade: Option<String>,
    pub modules: Vec<String>,
    pub imports: Vec<String>,
    pub dynamic_imports: Vec<String>,
    /// Externals are classified by the host; they are not manifest graph edges
    /// and are not emitted preload assets.
    pub external_imports: Vec<String>,
    pub css: Option<Vec<String>>,
    pub assets: Option<Vec<String>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EmittedAssetRecord {
    pub file: String,
    pub original_names: Vec<String>,
    /// Exact decoded CSS source. Required only when assessing CSS for inlining.
    /// Binary assets need no content for dependency traversal.
    pub css_source: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum DependencyFont {
    Kit2(String),
    Kit3 { file: String, filename: String },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DependencyStylesheetGroup {
    pub source_id: String,
    pub css: Vec<String>,
    pub assets: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct EntryDependencies {
    pub file: String,
    pub imports: Vec<String>,
    pub stylesheets: Vec<String>,
    pub assets: Vec<String>,
    pub fonts: Vec<DependencyFont>,
    pub stylesheet_map: Vec<DependencyStylesheetGroup>,
}

impl EntryDependencies {
    /// Direct bridge to the existing pure server-node generator.
    pub fn server_node_assets(&self) -> crate::build_nodes::AssetDependencies {
        crate::build_nodes::AssetDependencies {
            imports: self.imports.clone(),
            stylesheets: self.stylesheets.clone(),
            fonts: self
                .fonts
                .iter()
                .map(|font| match font {
                    DependencyFont::Kit2(file) => {
                        crate::build_nodes::FontDependency::Kit2(file.clone())
                    }
                    DependencyFont::Kit3 { file, filename } => {
                        crate::build_nodes::FontDependency::Kit3 {
                            file: file.clone(),
                            filename: filename.clone(),
                        }
                    }
                })
                .collect(),
            stylesheet_map: self
                .stylesheet_map
                .iter()
                .map(|group| crate::build_nodes::StylesheetGroup {
                    source_id: group.source_id.clone(),
                    css: group.css.clone(),
                    assets: group.assets.clone(),
                })
                .collect(),
        }
    }
}

/// Prepared lookup indices borrow the immutable host graph; returned dependency
/// results are owned and serializable. Rebuild this index when the graph changes.
pub struct PreparedDependencyGraph<'a> {
    graph: &'a EmittedDependencyGraph,
    kit3: bool,
    entry_sources: BTreeMap<String, String>,
    module_sources: BTreeMap<String, Vec<String>>,
    assets: BTreeMap<String, &'a EmittedAssetRecord>,
    font_sources: BTreeMap<String, String>,
}

fn output_path(file: &str) -> Result<(), String> {
    if file.is_empty()
        || file.starts_with('/')
        || file.contains(['\\', '\0', '?', '#'])
        || file.chars().any(char::is_control)
        || file.split('/').any(|part| matches!(part, "" | "." | ".."))
    {
        return Err(format!("Invalid emitted output-relative filename {file:?}"));
    }
    Ok(())
}
fn path_id(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}
fn normalized(path: &Path) -> Result<PathBuf, String> {
    if !path.is_absolute() || path.to_str().is_none_or(|value| value.contains('\0')) {
        return Err(format!(
            "Expected absolute UTF-8 source path: {}",
            path.display()
        ));
    }
    let mut result = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !result.pop() {
                    return Err("Source path escapes filesystem root".into());
                }
            }
            _ => result.push(component.as_os_str()),
        }
    }
    Ok(result)
}
fn relative(root: &Path, path: &Path) -> String {
    let root: Vec<_> = root.components().collect();
    let path: Vec<_> = path.components().collect();
    let common = root.iter().zip(&path).take_while(|(a, b)| a == b).count();
    let mut output = PathBuf::new();
    for _ in common..root.len() {
        output.push("..");
    }
    for part in &path[common..] {
        output.push(part.as_os_str());
    }
    path_id(&output)
}
fn append_unique(values: &mut Vec<String>, seen: &mut BTreeSet<String>, value: &str) {
    if seen.insert(value.to_owned()) {
        values.push(value.to_owned());
    }
}
fn font_file(file: &str) -> bool {
    [".woff", ".woff2", ".ttf", ".otf"]
        .iter()
        .any(|suffix| file.ends_with(suffix))
}

impl<'a> PreparedDependencyGraph<'a> {
    pub fn new(graph: &'a EmittedDependencyGraph) -> Result<Self, String> {
        let kit3 = KitSyncVersion::parse(&graph.kit_version)?.is_kit3();
        if normalized(&graph.project_root)? != graph.project_root {
            return Err("projectRoot must be normalized".into());
        }
        if graph.records.len() > 1_000_000 {
            return Err("Dependency graph exceeds one million records".into());
        }
        let mut result = Self {
            graph,
            kit3,
            entry_sources: BTreeMap::new(),
            module_sources: BTreeMap::new(),
            assets: BTreeMap::new(),
            font_sources: BTreeMap::new(),
        };
        for (path, canonical) in &graph.real_paths {
            if normalized(path)? != *path {
                return Err("realPaths must contain normalized absolute paths".into());
            }
            if let Some(canonical) = canonical {
                if normalized(canonical)? != *canonical {
                    return Err("realPaths must contain normalized absolute paths".into());
                }
            }
        }
        for asset in &graph.assets {
            output_path(&asset.file)?;
            if result.assets.insert(asset.file.clone(), asset).is_some() {
                return Err(format!("Duplicate emitted asset {}", asset.file));
            }
            if asset.css_source.is_some() && !asset.file.ends_with(".css") {
                return Err(format!(
                    "CSS source supplied for non-CSS asset {}",
                    asset.file
                ));
            }
        }
        // JS Object.keys(...).sort() orders UTF-16 units, unlike Rust UTF-8 Ord.
        let mut keys: Vec<_> = graph.records.keys().collect();
        keys.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
        for key in keys {
            if key.is_empty() {
                return Err("Empty manifest record key".into());
            }
            let record = &graph.records[key];
            output_path(&record.file)?;
            if let Some(source) = record.source.as_ref().filter(|source| !source.is_empty()) {
                result
                    .font_sources
                    .entry(record.file.clone())
                    .or_insert_with(|| source.clone());
            }
            for source in [&record.source, &record.facade]
                .into_iter()
                .flatten()
                .filter(|source| !source.is_empty())
            {
                if let Some(previous) = result.entry_sources.get(source) {
                    if graph.records[previous].file != record.file {
                        return Err(format!(
                            "Ambiguous entry source {source}: {previous} and {key}"
                        ));
                    }
                } else {
                    result.entry_sources.insert(source.clone(), key.clone());
                }
            }
            for module in &record.modules {
                let chunks = result.module_sources.entry(module.clone()).or_default();
                if !chunks.contains(key) {
                    chunks.push(key.clone());
                }
            }
            for file in record.css.iter().flatten() {
                output_path(file)?;
                if !file.ends_with(".css") || !result.assets.contains_key(file) {
                    return Err(format!(
                        "Missing emitted CSS asset {file} referenced by {key}"
                    ));
                }
            }
            for file in record.assets.iter().flatten() {
                output_path(file)?;
                if !result.assets.contains_key(file) {
                    return Err(format!("Missing emitted asset {file} referenced by {key}"));
                }
            }
        }
        for alias in graph.aliases.keys() {
            result.resolve_entry(alias)?;
        }
        for (key, record) in &graph.records {
            for edge in record.imports.iter().chain(&record.dynamic_imports) {
                result
                    .resolve_entry(edge)
                    .map_err(|error| format!("Invalid dependency of {key}: {error}"))?;
            }
        }
        Ok(result)
    }

    /// Resolve a manifest key, facade/src ID or explicitly supplied symlink.
    /// Module membership alone cannot identify an importable entrypoint.
    pub fn resolve_entry(&self, source: &str) -> Result<String, String> {
        let mut current = source.to_owned();
        let mut seen = BTreeSet::new();
        for _ in 0..256 {
            if !seen.insert(current.clone()) {
                return Err(format!("Dependency alias cycle at {current}"));
            }
            if self.graph.records.contains_key(&current) {
                return Ok(current);
            }
            if let Some(next) = self.graph.aliases.get(&current) {
                current = next.clone();
                continue;
            }
            if let Some(key) = self.entry_sources.get(&current) {
                return Ok(key.clone());
            }
            if !current.contains('\0') {
                let absolute = normalized(&self.graph.project_root.join(&current))?;
                let relative = relative(&self.graph.project_root, &absolute);
                if self.graph.records.contains_key(&relative) {
                    return Ok(relative);
                }
                let absolute_id = path_id(&absolute);
                if self.graph.records.contains_key(&absolute_id) {
                    return Ok(absolute_id);
                }
                if let Some(key) = self.entry_sources.get(&relative) {
                    return Ok(key.clone());
                }
                if let Some(key) = self.entry_sources.get(&absolute_id) {
                    return Ok(key.clone());
                }
                if let Some(Some(next)) = self.graph.real_paths.get(&absolute) {
                    current = relative_path_id(&self.graph.project_root, next);
                    continue;
                }
            }
            return Err(format!(
                "Could not find source {source:?} in emitted dependency graph (last ID {current:?})"
            ));
        }
        Err(format!("Dependency alias depth exceeds 256 for {source}"))
    }

    pub fn module_chunks(&self, source: &str) -> &[String] {
        self.module_sources
            .get(source)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    pub fn find_dependencies(
        &self,
        entry: &str,
        include_dynamic_css: bool,
    ) -> Result<EntryDependencies, String> {
        #[derive(Clone)]
        enum Task {
            Enter {
                current: String,
                add_js: bool,
                initial: String,
                depth: usize,
            },
            AfterStatic {
                record: String,
                initial: String,
                depth: usize,
            },
        }
        let key = self.resolve_entry(entry)?;
        let mut output = EntryDependencies {
            file: self.graph.records[&key].file.clone(),
            imports: vec![],
            stylesheets: vec![],
            assets: vec![],
            fonts: vec![],
            stylesheet_map: vec![],
        };
        let mut seen = BTreeSet::new();
        let mut imports = BTreeSet::new();
        let mut css = BTreeSet::new();
        let mut assets = BTreeSet::new();
        let mut groups = BTreeMap::<String, usize>::new();
        let mut stack = vec![Task::Enter {
            current: key,
            add_js: true,
            initial: entry.into(),
            depth: 0,
        }];
        while let Some(task) = stack.pop() {
            match task {
                Task::Enter {
                    current,
                    add_js,
                    initial,
                    depth,
                } => {
                    // Deliberately before alias resolution, matching upstream.
                    if !seen.insert(current.clone()) {
                        continue;
                    }
                    let key = self.resolve_entry(&current)?;
                    let record = &self.graph.records[&key];
                    if add_js {
                        append_unique(&mut output.imports, &mut imports, &record.file);
                    }
                    for file in record.assets.iter().flatten() {
                        append_unique(&mut output.assets, &mut assets, file);
                    }
                    for file in record.css.iter().flatten() {
                        append_unique(&mut output.stylesheets, &mut css, file);
                    }
                    stack.push(Task::AfterStatic {
                        record: key,
                        initial: initial.clone(),
                        depth,
                    });
                    for imported in record.imports.iter().rev() {
                        stack.push(Task::Enter {
                            current: imported.clone(),
                            add_js,
                            initial: initial.clone(),
                            depth,
                        });
                    }
                }
                Task::AfterStatic {
                    record,
                    initial,
                    depth,
                } => {
                    if !include_dynamic_css {
                        continue;
                    }
                    let record = &self.graph.records[&record];
                    if (record.css.is_some() || record.assets.is_some()) && depth <= 1 {
                        let index = if let Some(index) = groups.get(&initial) {
                            *index
                        } else {
                            let index = output.stylesheet_map.len();
                            output.stylesheet_map.push(DependencyStylesheetGroup {
                                source_id: initial.clone(),
                                css: vec![],
                                assets: vec![],
                            });
                            groups.insert(initial, index);
                            index
                        };
                        let group = &mut output.stylesheet_map[index];
                        for file in record.css.iter().flatten() {
                            if !group.css.contains(file) {
                                group.css.push(file.clone());
                            }
                        }
                        for file in record.assets.iter().flatten() {
                            if !group.assets.contains(file) {
                                group.assets.push(file.clone());
                            }
                        }
                    }
                    for imported in record.dynamic_imports.iter().rev() {
                        stack.push(Task::Enter {
                            current: imported.clone(),
                            add_js: false,
                            initial: imported.clone(),
                            depth: depth + 1,
                        });
                    }
                }
            }
        }
        output.fonts = self.fonts(&output.assets)?;
        Ok(output)
    }

    pub fn fonts(&self, assets: &[String]) -> Result<Vec<DependencyFont>, String> {
        assets
            .iter()
            .filter(|file| font_file(file))
            .map(|file| {
                if !self.assets.contains_key(file) {
                    return Err(format!("Missing emitted font asset {file}"));
                }
                if !self.kit3 {
                    return Ok(DependencyFont::Kit2(file.clone()));
                }
                let source = self.font_sources.get(file).unwrap_or(file);
                let absolute = normalized(&self.graph.project_root.join(source))?;
                let mut filename = relative(&self.graph.project_root, &absolute);
                if let Some(index) = source.rfind("/node_modules/") {
                    let local =
                        format!("node_modules/{}", &source[index + "/node_modules/".len()..]);
                    let unresolved = normalized(&self.graph.project_root.join(&local))?;
                    let observed = self.graph.real_paths.get(&absolute).ok_or_else(|| {
                        format!(
                            "Missing filesystem identity observation for font source {}",
                            absolute.display()
                        )
                    })?;
                    if let Some(canonical) = observed {
                        let local_observed =
                            self.graph.real_paths.get(&unresolved).ok_or_else(|| {
                                format!(
                                    "Missing filesystem identity observation for local font {}",
                                    unresolved.display()
                                )
                            })?;
                        if local_observed.as_ref() == Some(canonical) {
                            filename = local;
                        }
                    }
                }
                Ok(DependencyFont::Kit3 {
                    file: file.clone(),
                    filename,
                })
            })
            .collect()
    }

    /// All CSS below the strict UTF-16 threshold, in emitted asset order. Actual
    /// URL rewriting stays with the CSS processor and must use each node's eager
    /// assets. This never returns unrewritten CSS as a resolved inline function.
    pub fn inline_candidates(
        &self,
        strategy: BundleStrategy,
        threshold: usize,
        rewrite_urls: bool,
    ) -> Result<Vec<InlineStyleCandidate>, String> {
        if strategy != BundleStrategy::Split || threshold == 0 {
            return Ok(vec![]);
        }
        let mut candidates = Vec::new();
        for asset in &self.graph.assets {
            if !asset.file.ends_with(".css") {
                continue;
            }
            let source = asset.css_source.as_ref().ok_or_else(|| {
                format!("Missing CSS source for inline eligibility: {}", asset.file)
            })?;
            let length = source.encode_utf16().count();
            if length < threshold {
                candidates.push(InlineStyleCandidate {
                    file: asset.file.clone(),
                    source: source.clone(),
                    utf16_length: length,
                    rewrite_urls,
                });
            }
        }
        Ok(candidates)
    }

    pub fn client_bootstrap(
        &self,
        request: &ClientBootstrapRequest,
    ) -> Result<ClientBootstrapDependencies, String> {
        let start = self.find_dependencies(&request.start, false)?;
        let mut entries = vec![start.clone()];
        let mut runtime = None;
        let mut app = None;
        let mut payload = None;
        if request.strategy == BundleStrategy::Split {
            if self.kit3 {
                let key = self.resolve_entry(&request.start)?;
                let runtime_entry = self.graph.records[&key]
                    .dynamic_imports
                    .first()
                    .ok_or("Could not find the Kit 3 client runtime chunk")?;
                let dependencies = self.find_dependencies(runtime_entry, false)?;
                runtime = Some(dependencies.file.clone());
                entries.push(dependencies);
                let payload_entry = request
                    .payload
                    .as_deref()
                    .ok_or("Kit 3 split client requires its payload entry")?;
                payload = Some(self.find_dependencies(payload_entry, false)?.file);
            } else if request.payload.is_some() {
                return Err("Kit 2 has no separate payload entry".into());
            }
            let dependencies = self.find_dependencies(
                request
                    .app
                    .as_deref()
                    .ok_or("Split client requires its app entry")?,
                false,
            )?;
            app = Some(dependencies.file.clone());
            entries.push(dependencies);
        } else if request.app.is_some() || request.payload.is_some() {
            return Err("Single/inline client uses only the bundle entry".into());
        }
        let mut imports = Vec::new();
        let mut seen = BTreeSet::new();
        let mut stylesheets = Vec::new();
        let mut fonts = Vec::new();
        for dependencies in entries {
            for imported in dependencies.imports {
                if self.kit3 && request.strategy == BundleStrategy::Split {
                    append_unique(&mut imports, &mut seen, &imported);
                } else {
                    imports.push(imported);
                }
            }
            stylesheets.extend(dependencies.stylesheets);
            fonts.extend(dependencies.fonts);
        }
        let uses_env_dynamic_public = if self.kit3 {
            if request.has_explicit_dynamic_public {
                let module = request.explicit_public_module.as_deref().ok_or(
                    "Kit 3 dynamic public environment requires its resolved module identity",
                )?;
                self.has_module(module)?
            } else {
                false
            }
        } else {
            self.module_sources
                .contains_key("\0virtual:env/dynamic/public")
                || (request.has_explicit_dynamic_public
                    && self
                        .module_sources
                        .contains_key("\0virtual:__sveltekit/env/public/client"))
        };
        Ok(ClientBootstrapDependencies {
            start: start.file,
            runtime,
            app,
            payload,
            imports,
            stylesheets,
            fonts,
            uses_env_dynamic_public,
            inline_bundle: request.strategy == BundleStrategy::Inline,
        })
    }

    fn has_module(&self, module: &str) -> Result<bool, String> {
        if self.module_sources.contains_key(module) {
            return Ok(true);
        }
        if module.contains('\0') {
            return Ok(false);
        }
        let absolute = normalized(&self.graph.project_root.join(module))?;
        Ok(self.module_sources.contains_key(&path_id(&absolute))
            || self
                .graph
                .real_paths
                .get(&absolute)
                .and_then(Option::as_ref)
                .is_some_and(|path| self.module_sources.contains_key(&path_id(path))))
    }

    /// Resolve a node's server modules and optional optimized-client entry, then
    /// reproduce eager CSS/font selection. No page/SSR options are inferred.
    pub fn node_dependencies(
        &self,
        client: Option<&PreparedDependencyGraph<'_>>,
        request: &NodeDependencyRequest,
    ) -> Result<NodeDependencies, String> {
        if request.client_entry.is_some()
            && (client.is_none()
                || request.strategy != BundleStrategy::Split
                || (request.component.is_none() && request.universal.is_none()))
        {
            return Err("Optimized client entry requires a split node with component/universal source and a client dependency graph".into());
        }
        if let Some(client) = client {
            if self.graph.kit_version != client.graph.kit_version
                || self.graph.project_root != client.graph.project_root
            {
                return Err(
                    "Server/client dependency graphs must share version and project root".into(),
                );
            }
        }
        let module = |source: &Option<String>| -> Result<Option<NodeModuleDependencies>, String> {
            source
                .as_ref()
                .map(|source| {
                    let key = self.resolve_entry(source)?;
                    Ok(NodeModuleDependencies {
                        source_id: source.clone(),
                        file: self.graph.records[&key].file.clone(),
                        dependencies: if request.strategy == BundleStrategy::Split {
                            Some(self.find_dependencies(source, true)?)
                        } else {
                            None
                        },
                    })
                })
                .transpose()
        };
        let component = module(&request.component)?;
        let universal = module(&request.universal)?;
        let server = request
            .server
            .as_ref()
            .map(|source| {
                self.resolve_entry(source)
                    .map(|key| NodeModuleDependencies {
                        source_id: source.clone(),
                        file: self.graph.records[&key].file.clone(),
                        dependencies: None,
                    })
            })
            .transpose()?;
        let mut output = NodeDependencies {
            component,
            universal,
            server,
            client: None,
            imports: vec![],
            stylesheets: vec![],
            fonts: vec![],
            eager_assets: vec![],
        };
        if request.strategy != BundleStrategy::Split
            || (request.component.is_none() && request.universal.is_none())
        {
            return Ok(output);
        }
        if let Some(client) = client {
            let source = request
                .client_entry
                .as_ref()
                .ok_or("Client manifest requires the node's resolved optimized entry")?;
            let dependencies = client.find_dependencies(source, true)?;
            let mut css_seen = BTreeSet::new();
            let mut assets_seen = BTreeSet::new();
            for group in &dependencies.stylesheet_map {
                let source_id = if group.source_id == *source {
                    request.component.as_ref().unwrap_or(&group.source_id)
                } else {
                    &group.source_id
                };
                let eager = [&output.component, &output.universal]
                    .into_iter()
                    .flatten()
                    .filter_map(|module| module.dependencies.as_ref())
                    .any(|deps| {
                        deps.stylesheet_map
                            .iter()
                            .any(|group| group.source_id == *source_id)
                    });
                if eager {
                    for file in &group.css {
                        append_unique(&mut output.stylesheets, &mut css_seen, file);
                    }
                    for file in &group.assets {
                        append_unique(&mut output.eager_assets, &mut assets_seen, file);
                    }
                }
            }
            output.imports = dependencies.imports.clone();
            output.fonts = client.fonts(&output.eager_assets)?;
            output.client = Some(NodeModuleDependencies {
                source_id: source.clone(),
                file: dependencies.file.clone(),
                dependencies: Some(dependencies),
            });
        } else {
            if request.client_entry.is_some() {
                return Err("Client entry supplied without a client dependency graph".into());
            }
            for module in [&output.component, &output.universal].into_iter().flatten() {
                if let Some(dependencies) = &module.dependencies {
                    // Intentional duplicates across component/universal inputs.
                    output.imports.extend(dependencies.imports.clone());
                    output.stylesheets.extend(dependencies.stylesheets.clone());
                    output.fonts.extend(dependencies.fonts.clone());
                }
            }
        }
        Ok(output)
    }
}

fn relative_path_id(root: &Path, path: &Path) -> String {
    relative(root, path)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClientBootstrapRequest {
    pub strategy: BundleStrategy,
    pub start: String,
    pub app: Option<String>,
    pub payload: Option<String>,
    pub has_explicit_dynamic_public: bool,
    /// Kit 3's generated env/public/client.js, using the real resolved identity.
    pub explicit_public_module: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientBootstrapDependencies {
    pub start: String,
    pub runtime: Option<String>,
    pub app: Option<String>,
    pub payload: Option<String>,
    pub imports: Vec<String>,
    pub stylesheets: Vec<String>,
    pub fonts: Vec<DependencyFont>,
    pub uses_env_dynamic_public: bool,
    /// Host must embed actual script/style bytes and remove their served files.
    pub inline_bundle: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InlineStyleCandidate {
    pub file: String,
    pub source: String,
    pub utf16_length: usize,
    pub rewrite_urls: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NodeDependencyRequest {
    pub component: Option<String>,
    pub universal: Option<String>,
    pub server: Option<String>,
    pub client_entry: Option<String>,
    pub strategy: BundleStrategy,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeModuleDependencies {
    pub source_id: String,
    pub file: String,
    pub dependencies: Option<EntryDependencies>,
}

impl NodeModuleDependencies {
    pub fn server_node_module(&self) -> crate::build_nodes::ResolvedModule {
        crate::build_nodes::ResolvedModule {
            source_id: self.source_id.clone(),
            chunk: Some(self.file.clone()),
            dependencies: self
                .dependencies
                .as_ref()
                .map(EntryDependencies::server_node_assets),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeDependencies {
    pub component: Option<NodeModuleDependencies>,
    pub universal: Option<NodeModuleDependencies>,
    pub server: Option<NodeModuleDependencies>,
    pub client: Option<NodeModuleDependencies>,
    pub imports: Vec<String>,
    pub stylesheets: Vec<String>,
    pub fonts: Vec<DependencyFont>,
    pub eager_assets: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    fn record(
        file: &str,
        imports: &[&str],
        dynamic: &[&str],
        css: Option<&[&str]>,
        assets: Option<&[&str]>,
    ) -> EmittedModuleRecord {
        EmittedModuleRecord {
            file: file.into(),
            source: None,
            facade: None,
            modules: vec![],
            imports: imports.iter().map(|s| (*s).into()).collect(),
            dynamic_imports: dynamic.iter().map(|s| (*s).into()).collect(),
            external_imports: vec![],
            css: css.map(|v| v.iter().map(|s| (*s).into()).collect()),
            assets: assets.map(|v| v.iter().map(|s| (*s).into()).collect()),
        }
    }
    fn graph(version: &str, records: &[(&str, EmittedModuleRecord)]) -> EmittedDependencyGraph {
        let mut assets = BTreeSet::new();
        for (_, record) in records {
            assets.extend(record.css.iter().flatten().cloned());
            assets.extend(record.assets.iter().flatten().cloned());
        }
        EmittedDependencyGraph {
            kit_version: version.into(),
            project_root: "/project".into(),
            records: records
                .iter()
                .cloned()
                .map(|(key, value)| (key.into(), value))
                .collect(),
            aliases: BTreeMap::new(),
            real_paths: BTreeMap::new(),
            assets: assets
                .into_iter()
                .map(|file| EmittedAssetRecord {
                    css_source: file.ends_with(".css").then(|| "a{}".into()),
                    file,
                    original_names: vec![],
                })
                .collect(),
        }
    }
    #[test]
    fn traversal_preserves_preorder_files_postorder_groups_and_dynamic_depth() {
        let graph = graph(
            "2.70.3",
            &[
                (
                    "root",
                    record("root.js", &["static"], &["lazy"], Some(&["root.css"]), None),
                ),
                (
                    "static",
                    record("static.js", &[], &[], Some(&["static.css"]), None),
                ),
                (
                    "lazy",
                    record(
                        "lazy.js",
                        &["lazy-static"],
                        &["deep"],
                        Some(&["lazy.css"]),
                        Some(&["a.woff2"]),
                    ),
                ),
                (
                    "lazy-static",
                    record("lazy-static.js", &[], &[], Some(&["lazy-static.css"]), None),
                ),
                (
                    "deep",
                    record("deep.js", &[], &[], Some(&["deep.css"]), None),
                ),
            ],
        );
        let prepared = PreparedDependencyGraph::new(&graph).unwrap();
        let deps = prepared.find_dependencies("root", true).unwrap();
        assert_eq!(deps.imports, ["root.js", "static.js"]);
        assert_eq!(
            deps.stylesheets,
            [
                "root.css",
                "static.css",
                "lazy.css",
                "lazy-static.css",
                "deep.css"
            ]
        );
        assert_eq!(
            deps.stylesheet_map
                .iter()
                .map(|g| g.source_id.as_str())
                .collect::<Vec<_>>(),
            ["root", "lazy"]
        );
        assert_eq!(deps.stylesheet_map[0].css, ["static.css", "root.css"]);
        assert_eq!(deps.stylesheet_map[1].css, ["lazy-static.css", "lazy.css"]);
        assert_eq!(deps.fonts, [DependencyFont::Kit2("a.woff2".into())]);
        let static_only = prepared.find_dependencies("root", false).unwrap();
        assert_eq!(static_only.stylesheets, ["root.css", "static.css"]);
        assert!(static_only.stylesheet_map.is_empty());
    }
    #[test]
    fn first_dynamic_visit_and_cycles_match_upstream_seen_contract() {
        let graph = graph(
            "2.69.2",
            &[
                (
                    "root",
                    record("root.js", &["first", "shared"], &[], None, None),
                ),
                (
                    "first",
                    record("first.js", &["root"], &["shared"], None, None),
                ),
                ("shared", record("shared.js", &[], &[], Some(&[]), None)),
            ],
        );
        let prepared = PreparedDependencyGraph::new(&graph).unwrap();
        assert_eq!(
            prepared.find_dependencies("root", true).unwrap().imports,
            ["root.js", "first.js"]
        );
        assert_eq!(
            prepared.find_dependencies("root", false).unwrap().imports,
            ["root.js", "first.js", "shared.js"]
        );
        assert_eq!(
            prepared
                .find_dependencies("root", true)
                .unwrap()
                .stylesheet_map[0]
                .source_id,
            "shared"
        );
    }
    #[test]
    fn missing_records_assets_and_alias_cycles_are_failures() {
        let mut graph = graph(
            "3.0.1",
            &[("root", record("root.js", &[], &["missing"], None, None))],
        );
        assert!(
            PreparedDependencyGraph::new(&graph)
                .err()
                .unwrap()
                .contains("Invalid dependency")
        );
        graph
            .records
            .get_mut("root")
            .unwrap()
            .dynamic_imports
            .clear();
        graph.records.get_mut("root").unwrap().css = Some(vec!["missing.css".into()]);
        assert!(
            PreparedDependencyGraph::new(&graph)
                .err()
                .unwrap()
                .contains("Missing emitted CSS")
        );
        graph.records.get_mut("root").unwrap().css = None;
        graph.aliases = BTreeMap::from([("a".into(), "b".into()), ("b".into(), "a".into())]);
        assert!(
            PreparedDependencyGraph::new(&graph)
                .err()
                .unwrap()
                .contains("alias cycle")
        );
    }
    #[test]
    fn facade_realpaths_and_modules_have_distinct_resolution_roles() {
        let mut graph = graph(
            "3.0.1",
            &[(
                "src/real.ts",
                record("_app/immutable/entry.js", &[], &[], None, None),
            )],
        );
        graph.records.get_mut("src/real.ts").unwrap().facade = Some("/project/src/real.ts".into());
        graph
            .records
            .get_mut("src/real.ts")
            .unwrap()
            .modules
            .push("/project/src/included.ts".into());
        graph.real_paths.insert(
            "/project/src/linked.ts".into(),
            Some("/project/src/real.ts".into()),
        );
        let prepared = PreparedDependencyGraph::new(&graph).unwrap();
        assert_eq!(
            prepared.resolve_entry("/project/src/linked.ts").unwrap(),
            "src/real.ts"
        );
        assert_eq!(
            prepared
                .find_dependencies("/project/src/real.ts", false)
                .unwrap()
                .file,
            "_app/immutable/entry.js"
        );
        assert_eq!(
            prepared.module_chunks("/project/src/included.ts"),
            ["src/real.ts"]
        );
        assert!(prepared.resolve_entry("/project/src/included.ts").is_err());
    }
    #[test]
    fn kit3_font_first_sorted_source_and_verified_local_package_path() {
        let mut graph = graph(
            "3.0.1",
            &[
                (
                    "root",
                    record(
                        "root.js",
                        &[],
                        &[],
                        None,
                        Some(&["_app/font.woff2", "UPPER.WOFF2"]),
                    ),
                ),
                ("z-font", record("_app/font.woff2", &[], &[], None, None)),
                ("a-font", record("_app/font.woff2", &[], &[], None, None)),
            ],
        );
        graph.records.get_mut("z-font").unwrap().source = Some("wrong.woff2".into());
        graph.records.get_mut("a-font").unwrap().source =
            Some("/store/pkg/node_modules/font/a.woff2".into());
        graph.real_paths.insert(
            "/store/pkg/node_modules/font/a.woff2".into(),
            Some("/store/real/a.woff2".into()),
        );
        graph.real_paths.insert(
            "/project/node_modules/font/a.woff2".into(),
            Some("/store/real/a.woff2".into()),
        );
        let prepared = PreparedDependencyGraph::new(&graph).unwrap();
        assert_eq!(
            prepared.find_dependencies("root", false).unwrap().fonts,
            [DependencyFont::Kit3 {
                file: "_app/font.woff2".into(),
                filename: "node_modules/font/a.woff2".into()
            }]
        );
        graph.real_paths.clear();
        assert!(
            PreparedDependencyGraph::new(&graph)
                .unwrap()
                .find_dependencies("root", false)
                .unwrap_err()
                .contains("Missing filesystem identity observation")
        );
        graph
            .real_paths
            .insert("/store/pkg/node_modules/font/a.woff2".into(), None);
        assert_eq!(
            PreparedDependencyGraph::new(&graph)
                .unwrap()
                .find_dependencies("root", false)
                .unwrap()
                .fonts,
            [DependencyFont::Kit3 {
                file: "_app/font.woff2".into(),
                filename: "../store/pkg/node_modules/font/a.woff2".into()
            }]
        );
    }
    #[test]
    fn bootstrap_duplicate_rules_and_kit3_runtime_entry_are_preserved() {
        let mut graph = graph(
            "2.70.3",
            &[
                (
                    "start",
                    record("start.js", &["shared"], &["runtime"], None, None),
                ),
                (
                    "runtime",
                    record("runtime.js", &["shared"], &[], None, None),
                ),
                ("app", record("app.js", &["shared"], &[], None, None)),
                ("shared", record("shared.js", &[], &[], None, None)),
                ("payload", record("payload.js", &[], &[], None, None)),
            ],
        );
        let mut request = ClientBootstrapRequest {
            strategy: BundleStrategy::Split,
            start: "start".into(),
            app: Some("app".into()),
            payload: None,
            has_explicit_dynamic_public: false,
            explicit_public_module: None,
        };
        assert_eq!(
            PreparedDependencyGraph::new(&graph)
                .unwrap()
                .client_bootstrap(&request)
                .unwrap()
                .imports,
            ["start.js", "shared.js", "app.js", "shared.js"]
        );
        graph.kit_version = "3.0.1".into();
        request.payload = Some("payload".into());
        let deps = PreparedDependencyGraph::new(&graph)
            .unwrap()
            .client_bootstrap(&request)
            .unwrap();
        assert_eq!(
            deps.imports,
            ["start.js", "shared.js", "runtime.js", "app.js"]
        );
        assert_eq!(deps.payload.as_deref(), Some("payload.js"));
        graph
            .records
            .get_mut("start")
            .unwrap()
            .dynamic_imports
            .clear();
        assert!(
            PreparedDependencyGraph::new(&graph)
                .unwrap()
                .client_bootstrap(&request)
                .unwrap_err()
                .contains("runtime chunk")
        );
    }
    #[test]
    fn node_css_intersects_server_groups_and_keeps_client_import_order() {
        let server = graph(
            "2.70.3",
            &[
                (
                    "Page.svelte",
                    record("page.js", &[], &["shared.svelte"], Some(&["ssr.css"]), None),
                ),
                (
                    "shared.svelte",
                    record("shared.js", &[], &[], Some(&["ssr-shared.css"]), None),
                ),
            ],
        );
        let client = graph(
            "2.70.3",
            &[
                (
                    "nodes/0",
                    record(
                        "node.js",
                        &[],
                        &["shared.svelte", "lazy.svelte"],
                        Some(&["client.css"]),
                        Some(&["font.woff2"]),
                    ),
                ),
                (
                    "shared.svelte",
                    record("shared.js", &[], &[], Some(&["shared.css"]), None),
                ),
                (
                    "lazy.svelte",
                    record("lazy.js", &[], &[], Some(&["lazy.css"]), None),
                ),
            ],
        );
        let server = PreparedDependencyGraph::new(&server).unwrap();
        let client = PreparedDependencyGraph::new(&client).unwrap();
        let deps = server
            .node_dependencies(
                Some(&client),
                &NodeDependencyRequest {
                    component: Some("Page.svelte".into()),
                    universal: None,
                    server: None,
                    client_entry: Some("nodes/0".into()),
                    strategy: BundleStrategy::Split,
                },
            )
            .unwrap();
        assert_eq!(deps.imports, ["node.js"]);
        assert_eq!(deps.stylesheets, ["client.css", "shared.css"]);
        assert_eq!(deps.fonts, [DependencyFont::Kit2("font.woff2".into())]);
        assert!(!deps.stylesheets.contains(&"lazy.css".into()));
        assert_eq!(
            deps.component
                .unwrap()
                .server_node_module()
                .chunk
                .as_deref(),
            Some("page.js")
        );
    }
    #[test]
    fn inline_threshold_is_strict_utf16_and_url_rewrite_stays_explicit() {
        let mut graph = graph(
            "3.0.1",
            &[(
                "root",
                record("_app/root.js", &[], &[], Some(&["_app/style.css"]), None),
            )],
        );
        graph.assets[0].css_source = Some("😀".into());
        let prepared = PreparedDependencyGraph::new(&graph).unwrap();
        assert!(
            prepared
                .inline_candidates(BundleStrategy::Split, 2, true)
                .unwrap()
                .is_empty()
        );
        let candidates = prepared
            .inline_candidates(BundleStrategy::Split, 3, true)
            .unwrap();
        assert_eq!(candidates[0].utf16_length, 2);
        assert!(candidates[0].rewrite_urls);
        assert!(
            prepared
                .inline_candidates(BundleStrategy::Single, 3, true)
                .unwrap()
                .is_empty()
        );
        graph.assets[0].css_source = None;
        assert!(
            PreparedDependencyGraph::new(&graph)
                .unwrap()
                .inline_candidates(BundleStrategy::Split, 3, false)
                .unwrap_err()
                .contains("Missing CSS source")
        );
        graph.assets[0].file = "../escape.css".into();
        assert!(
            PreparedDependencyGraph::new(&graph)
                .err()
                .unwrap()
                .contains("output-relative")
        );
    }
}
