//! Pure validation of complete loaded Kit module graphs, before tree shaking.
//!
//! Based on SvelteKit's MIT-licensed Vite illegal-import guards, worker guards,
//! normalize_id, and postbuild feature checks (registered Kit 2/3 profiles).
//! Copyright (c) Svelte contributors; see SvelteKit/LICENSE.
//!
//! No files, environment or application code are read or executed. The host
//! supplies loaded modules, resolved edges, optional source spans, aliases and
//! canonical filesystem identities. All static/dynamic edges are followed,
//! including namespace packages and modules absent from final output chunks.
//! Filename heuristics exempt installed dependencies as upstream does; traversal
//! NEVER exempts them, so a package importing private env still fails.
//!
//! Unlike Kit 2's greedy importer walk, chains search the complete graph. This
//! avoids its known missed-client-branch failure. Queries are stripped for guard
//! identity, but #package-import aliases are preserved. Original and canonical
//! paths both participate so a symlink cannot erase a server-only identity.
//!
//! Runtime feature evidence is separate: exports/import names do not establish
//! actual feature use. Optional feature checks require host-proven AST uses or
//! retained emitted tracking markers and explicit adapter capability results.
//! Unanalysed reachable modules and opaque externals remain unresolved. The
//! optional route-feature pass follows upstream STATIC-import reachability;
//! it is not a proof of arbitrary dynamic application behavior.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::build_plan::BuildAlias;
use crate::sync_versions::KitSyncVersion;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum GuardTarget {
    Server,
    Client,
    ServiceWorker,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ImportKind {
    Static,
    Dynamic,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LoadedGuardModule {
    pub id: String,
    pub is_entry: bool,
    /// Authoritative bundler metadata, not inferred from empty imports/code.
    /// Legacy graphs default to normal loaded modules; new hosts must forward
    /// this flag and supply GuardExternal classification for every true value.
    #[serde(default)]
    pub is_external: bool,
    pub imports: Vec<String>,
    pub dynamic_imports: Vec<String>,
    pub importers: Vec<String>,
    pub dynamic_importers: Vec<String>,
    pub exports: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GuardSourceSpan {
    /// Original source file identity. Offsets are UTF-8 bytes supplied by the
    /// resolver/parser, never guessed from a module identifier.
    pub source: String,
    pub start: usize,
    pub end: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GuardResolution {
    pub importer: String,
    pub original_id: String,
    pub resolved_id: String,
    pub kind: ImportKind,
    pub span: Option<GuardSourceSpan>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum ExternalExposure {
    /// The host has explicit public-module/asset evidence. Package namespaces
    /// are never automatically assigned this state.
    Public {
        evidence: String,
    },
    ServerOnly,
    Unknown,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GuardExternal {
    pub id: String,
    pub exposure: ExternalExposure,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BuildGuardInput {
    pub kit_version: String,
    pub target: GuardTarget,
    pub project_root: PathBuf,
    pub runtime_directory: PathBuf,
    pub lib_directory: PathBuf,
    pub routes_directory: PathBuf,
    pub assets_directory: PathBuf,
    pub aliases: Vec<BuildAlias>,
    /// Observed physical path -> canonical physical path. No filesystem probes.
    pub real_paths: BTreeMap<PathBuf, PathBuf>,
    pub modules: Vec<LoadedGuardModule>,
    pub externals: Vec<GuardExternal>,
    pub resolutions: Vec<GuardResolution>,
    /// Explicit browser/worker roots, e.g. inventory component/universal/hooks.
    /// If empty, loaded isEntry roots are used. No roots is an input error.
    pub entrypoints: Vec<String>,
    /// Include the generated env guard IDs from NativeBuildPlan.guards.
    pub additional_server_only_ids: BTreeSet<String>,
    pub module_extensions: Vec<String>,
    /// None is unresolved configuration, never equivalent to disabled.
    pub remote_functions_enabled: Option<bool>,
    /// Kit 2 only; Kit 3's compatibility env modules have no experimental flag.
    pub explicit_environment_variables: Option<bool>,
    pub runtime_features: Option<RuntimeFeatureInput>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum GuardStatus {
    Passed,
    Failed,
    Incomplete,
    NotRequested,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportChainLink {
    pub importer: String,
    pub imported: String,
    pub kind: ImportKind,
    pub original_id: Option<String>,
    pub span: Option<GuardSourceSpan>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GuardDiagnostic {
    pub code: String,
    pub message: String,
    pub module_id: String,
    pub display_module: String,
    pub entry_id: String,
    pub chain: Vec<ImportChainLink>,
    pub source: String,
    pub span: Option<GuardSourceSpan>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GuardUnknown {
    pub code: String,
    pub module_id: String,
    pub message: String,
    pub chain: Vec<ImportChainLink>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BuildGuardReport {
    pub imports_status: GuardStatus,
    pub import_diagnostics: Vec<GuardDiagnostic>,
    pub import_unknowns: Vec<GuardUnknown>,
    pub configuration_status: GuardStatus,
    pub configuration_diagnostics: Vec<GuardDiagnostic>,
    pub configuration_unknowns: Vec<GuardUnknown>,
    pub runtime_features: RuntimeFeatureReport,
    pub reachable_modules: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FeatureProof {
    AstProvenUse,
    RetainedTrackingMarker,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProvenRuntimeFeature {
    pub feature: String,
    pub proof: FeatureProof,
    pub span: Option<GuardSourceSpan>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AdapterCapability {
    Supported,
    Unsupported,
    Unknown,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RouteFeatureInput {
    pub route_id: String,
    /// Resolved leaf/ancestor server modules, endpoint and server hooks, in
    /// upstream order. No universal/component roots are inferred by this API.
    pub server_roots: Vec<String>,
    pub adapter_name: Option<String>,
    /// Actual adapter.supports.read({route, config}) result. Unknown if the
    /// adapter callback/config could not be statically resolved.
    pub adapter_read: AdapterCapability,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuntimeFeatureInput {
    pub routes: Vec<RouteFeatureInput>,
    /// Missing module metadata is unknown, not an empty proven feature set.
    pub proven_modules: BTreeMap<String, Vec<ProvenRuntimeFeature>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RouteFeatureDiagnostic {
    pub code: String,
    pub route_id: String,
    pub module_id: String,
    pub feature: String,
    pub message: String,
    pub span: Option<GuardSourceSpan>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RouteFeatureUnknown {
    pub route_id: String,
    pub module_id: String,
    pub message: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeFeatureReport {
    pub status: GuardStatus,
    pub diagnostics: Vec<RouteFeatureDiagnostic>,
    pub unknowns: Vec<RouteFeatureUnknown>,
    pub route_features: BTreeMap<String, Vec<String>>,
}

#[derive(Clone)]
struct Identity {
    key: String,
    variants: BTreeSet<String>,
    paths: BTreeSet<PathBuf>,
}
#[derive(Clone)]
struct Edge {
    to: String,
    kind: ImportKind,
    original: Option<String>,
    span: Option<GuardSourceSpan>,
}
struct Graph {
    nodes: BTreeMap<String, Identity>,
    edges: BTreeMap<String, Vec<Edge>>,
    externals: BTreeMap<String, ExternalExposure>,
    roots: Vec<String>,
}

fn path_id(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}
fn clean_id(id: &str) -> String {
    // A leading # belongs to package imports. Only ? begins a Vite query.
    let id = id.split('?').next().unwrap_or(id).replace('\\', "/");
    let id = id
        .strip_prefix("/@id/__x00__")
        .map(|id| format!("\0{id}"))
        .unwrap_or(id);
    let virtual_id = id
        .strip_prefix("\0virtual:")
        .or_else(|| id.strip_prefix("virtual:"));
    if let Some(id) = virtual_id {
        if id.starts_with("env/") || id.starts_with("app/") || id == "service-worker" {
            return format!("${id}");
        }
        return id.to_owned();
    }
    if let Some(id) = id.strip_prefix("/@fs/") {
        return if id.starts_with('/') {
            id.into()
        } else {
            format!("/{id}")
        };
    }
    id
}
fn normalize_path(path: &Path) -> Result<PathBuf, String> {
    if !path.is_absolute() || path.to_str().is_none_or(|p| p.contains('\0')) {
        return Err("Guard filesystem paths must be absolute UTF-8 without NUL".into());
    }
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    return Err("Guard path escapes filesystem root".into());
                }
            }
            _ => out.push(part.as_os_str()),
        }
    }
    Ok(out)
}
fn runtime_name(input: &BuildGuardInput, path: &Path) -> Option<String> {
    let relative = path.strip_prefix(&input.runtime_directory).ok()?;
    let relative = path_id(relative);
    if let Some(app) = relative.strip_prefix("app/") {
        let app = app
            .strip_suffix("/index.js")
            .or_else(|| app.strip_suffix(".js"))
            .unwrap_or(app);
        if matches!(app, "env/public/client" | "env/public/server") {
            return Some("$app/env/public".into());
        }
        return Some(format!("$app/{app}"));
    }
    relative
        .strip_prefix("env/")
        .map(|env| format!("$env/{}", env.strip_suffix(".js").unwrap_or(env)))
}
fn identity(
    input: &BuildGuardInput,
    kit3: bool,
    id: &str,
    importer: Option<&str>,
) -> Result<Identity, String> {
    let raw = clean_id(id);
    let mut variants = BTreeSet::from([raw.clone()]);
    let mut physical = match raw.as_str() {
        "$app/server" => Some(input.runtime_directory.join("app/server/index.js")),
        "$app/env/private" => Some(input.runtime_directory.join(if kit3 {
            "app/env/private/index.js"
        } else {
            "app/env/private.js"
        })),
        "$app/env/public" => Some(input.runtime_directory.join("app/env/public/index.js")),
        _ if kit3 && raw.starts_with("$env/") => Some(
            input
                .runtime_directory
                .join(format!("env/{}.js", &raw[5..])),
        ),
        _ => None,
    };
    if physical.is_none() {
        for alias in &input.aliases {
            if let Some(path) = alias.resolve(&raw) {
                physical = Some(path);
                break;
            }
        }
    }
    if physical.is_none() && raw.starts_with("file:") {
        physical = Some(
            url::Url::parse(&raw)
                .map_err(|error| error.to_string())?
                .to_file_path()
                .map_err(|_| "Invalid guard file URL")?,
        );
    }
    if physical.is_none() && Path::new(&raw).is_absolute() {
        physical = Some(PathBuf::from(&raw));
    }
    if physical.is_none() && (raw.starts_with("./") || raw.starts_with("../")) {
        let base = importer
            .and_then(|id| Path::new(id).parent())
            .filter(|path| path.is_absolute())
            .unwrap_or(&input.project_root);
        physical = Some(base.join(&raw));
    }
    let mut paths = BTreeSet::new();
    let key = if let Some(path) = physical {
        let mut path = normalize_path(&path)?;
        let mut seen = BTreeSet::new();
        loop {
            if !seen.insert(path.clone()) {
                return Err("Guard realpath identity cycle".into());
            }
            paths.insert(path.clone());
            variants.insert(path_id(&path));
            if let Some(name) = runtime_name(input, &path) {
                variants.insert(name);
            }
            match input.real_paths.get(&path) {
                Some(next) if next != &path => path = normalize_path(next)?,
                _ => break,
            }
            if seen.len() > 256 {
                return Err("Guard realpath depth exceeds 256".into());
            }
        }
        // Reverse identities preserve an application server-only path even if
        // the loaded graph uses the realpath of an installed symlink target.
        for (original, canonical) in &input.real_paths {
            if canonical == &path {
                paths.insert(original.clone());
                variants.insert(path_id(original));
                if let Some(name) = runtime_name(input, original) {
                    variants.insert(name);
                }
            }
        }
        // Native bundlers may replace a virtual $env/$app import with an
        // absolute hashed module path. Exact aliases establish that identity
        // even when the graph no longer carries the original import spelling.
        // Prefix aliases do not prove an original specifier and must not invent
        // one. Respect the first matching alias, as the resolver does.
        for (index, alias) in input.aliases.iter().enumerate() {
            if alias.matching != crate::build_plan::AliasMatch::Exact
                || input.aliases[..index]
                    .iter()
                    .any(|previous| previous.resolve(&alias.name).is_some())
            {
                continue;
            }
            let original = normalize_path(&alias.replacement)?;
            let mut canonical = original.clone();
            let mut seen = BTreeSet::new();
            loop {
                if !seen.insert(canonical.clone()) {
                    return Err("Guard exact-alias realpath identity cycle".into());
                }
                match input.real_paths.get(&canonical) {
                    Some(next) if next != &canonical => canonical = normalize_path(next)?,
                    _ => break,
                }
                if seen.len() > 256 {
                    return Err("Guard exact-alias realpath depth exceeds 256".into());
                }
            }
            if canonical == path {
                variants.insert(clean_id(&alias.name));
                variants.insert(path_id(&original));
                paths.insert(original);
            }
        }
        path_id(&path)
    } else {
        raw
    };
    Ok(Identity {
        key,
        variants,
        paths,
    })
}
fn merge_identity(nodes: &mut BTreeMap<String, Identity>, value: Identity) {
    if let Some(existing) = nodes.get_mut(&value.key) {
        existing.variants.extend(value.variants);
        existing.paths.extend(value.paths);
    } else {
        nodes.insert(value.key.clone(), value);
    }
}
fn validate_span(span: &Option<GuardSourceSpan>) -> Result<(), String> {
    if span
        .as_ref()
        .is_some_and(|span| span.source.is_empty() || span.start > span.end)
    {
        return Err("Invalid original-source guard span".into());
    }
    Ok(())
}
fn graph(input: &BuildGuardInput, kit3: bool) -> Result<Graph, String> {
    let mut graph = Graph {
        nodes: BTreeMap::new(),
        edges: BTreeMap::new(),
        externals: BTreeMap::new(),
        roots: vec![],
    };
    let mut module_kinds = BTreeMap::new();
    for module in &input.modules {
        let value = identity(input, kit3, &module.id, None)?;
        if let Some(previous) = module_kinds.insert(value.key.clone(), module.is_external) {
            if previous != module.is_external {
                return Err(format!(
                    "Conflicting loaded/external module identity: {}",
                    module.id
                ));
            }
        }
        if module.is_external
            && (module.is_entry || !module.imports.is_empty() || !module.dynamic_imports.is_empty())
        {
            return Err(format!(
                "External module cannot be an entrypoint or claim a loaded import graph: {}",
                module.id
            ));
        }
        if !module.is_external
            && value.paths.is_empty()
            && !value.key.starts_with(['\0', '$', '#', '<'])
            && !value.key.starts_with("__sveltekit/")
            && !value.key.contains(':')
        {
            return Err(format!(
                "Loaded module must have a resolved filesystem or virtual ID: {}",
                module.id
            ));
        }
        if module.is_entry && input.entrypoints.is_empty() && !graph.roots.contains(&value.key) {
            graph.roots.push(value.key.clone());
        }
        merge_identity(&mut graph.nodes, value);
    }
    for external in &input.externals {
        if let ExternalExposure::Public { evidence } = &external.exposure {
            if evidence.trim().is_empty() {
                return Err("A public external classification requires explicit evidence".into());
            }
        }
        let value = identity(input, kit3, &external.id, None)?;
        if module_kinds.get(&value.key) == Some(&false) {
            return Err(format!(
                "Module is both loaded and external: {}",
                external.id
            ));
        }
        if graph
            .externals
            .insert(value.key.clone(), external.exposure.clone())
            .is_some()
        {
            return Err(format!("Duplicate external {}", external.id));
        }
        merge_identity(&mut graph.nodes, value);
    }
    for (id, is_external) in &module_kinds {
        if *is_external && !graph.externals.contains_key(id) {
            return Err(format!(
                "External module requires explicit exposure classification: {id}"
            ));
        }
    }
    for entry in &input.entrypoints {
        let key = identity(input, kit3, entry, None)?.key;
        if !graph.nodes.contains_key(&key) || graph.externals.contains_key(&key) {
            return Err(format!("Missing loaded guard entrypoint {entry}"));
        }
        if !graph.roots.contains(&key) {
            graph.roots.push(key);
        }
    }
    if graph.roots.is_empty() {
        return Err("Complete guard graph requires at least one entrypoint".into());
    }
    let mut resolutions: BTreeMap<(String, bool), Vec<&GuardResolution>> = BTreeMap::new();
    for resolution in &input.resolutions {
        validate_span(&resolution.span)?;
        let importer = identity(input, kit3, &resolution.importer, None)?.key;
        resolutions
            .entry((importer, resolution.kind == ImportKind::Dynamic))
            .or_default()
            .push(resolution);
    }
    let mut add_edge = |from: &str, to: &str, kind: ImportKind| -> Result<(), String> {
        let importer = identity(input, kit3, from, None)?.key;
        if !graph.nodes.contains_key(&importer) || graph.externals.contains_key(&importer) {
            return Err(format!("Missing loaded importer {from}"));
        }
        let mut imported = identity(input, kit3, to, Some(&importer))?;
        let mut matched = None;
        for resolution in resolutions
            .get(&(importer.clone(), kind == ImportKind::Dynamic))
            .into_iter()
            .flatten()
            .copied()
        {
            let resolved = identity(input, kit3, &resolution.resolved_id, Some(&importer))?;
            if clean_id(&resolution.original_id) == clean_id(to) || resolved.key == imported.key {
                if let Some(previous) = matched {
                    let previous: &GuardResolution = previous;
                    if clean_id(&previous.resolved_id) != clean_id(&resolution.resolved_id) {
                        return Err(format!(
                            "Conflicting resolution metadata for {from} -> {to}"
                        ));
                    }
                }
                imported = resolved;
                let original = identity(input, kit3, &resolution.original_id, Some(&importer))?;
                imported.variants.extend(original.variants);
                imported.paths.extend(original.paths);
                matched = Some(resolution);
            }
        }
        if !graph.nodes.contains_key(&imported.key) {
            return Err(format!(
                "Incomplete loaded graph: {from} imports missing module {to}; classify external modules explicitly"
            ));
        }
        let key = imported.key.clone();
        merge_identity(&mut graph.nodes, imported);
        let edge = Edge {
            to: key,
            kind,
            original: matched.map(|resolution| resolution.original_id.clone()),
            span: matched.and_then(|resolution| resolution.span.clone()),
        };
        let edges = graph.edges.entry(importer).or_default();
        if !edges
            .iter()
            .any(|existing| existing.to == edge.to && existing.kind == edge.kind)
        {
            edges.push(edge);
        }
        Ok(())
    };
    for module in &input.modules {
        for imported in &module.imports {
            add_edge(&module.id, imported, ImportKind::Static)?;
        }
        for imported in &module.dynamic_imports {
            add_edge(&module.id, imported, ImportKind::Dynamic)?;
        }
    }
    // Importer metadata is also evidence, including loaded edges removed during
    // transforms. Union both directions rather than silently discard those edges.
    for module in &input.modules {
        for importer in &module.importers {
            add_edge(importer, &module.id, ImportKind::Static)?;
        }
        for importer in &module.dynamic_importers {
            add_edge(importer, &module.id, ImportKind::Dynamic)?;
        }
    }
    Ok(graph)
}

fn display(input: &BuildGuardInput, value: &Identity) -> String {
    if let Some(known) = value
        .variants
        .iter()
        .find(|name| name.starts_with("$app/") || name.starts_with("$env/"))
    {
        return known.clone();
    }
    // Longest alias path first, matching Kit 3's diagnostic normalization.
    let mut aliases = input.aliases.iter().collect::<Vec<_>>();
    aliases.sort_by_key(|alias| std::cmp::Reverse(alias.replacement.as_os_str().len()));
    for path in &value.paths {
        if let Ok(relative) = path.strip_prefix(&input.lib_directory) {
            if !relative.as_os_str().is_empty() {
                return format!("$lib/{}", path_id(relative));
            }
        }
        for alias in &aliases {
            if let Ok(relative) = path.strip_prefix(&alias.replacement) {
                return if relative.as_os_str().is_empty() {
                    alias.name.clone()
                } else {
                    format!("{}/{}", alias.name, path_id(relative))
                };
            }
        }
    }
    Path::new(&value.key)
        .strip_prefix(&input.project_root)
        .map(path_id)
        .unwrap_or_else(|_| value.key.clone())
}
fn server_only(
    input: &BuildGuardInput,
    kit3: bool,
    value: &Identity,
    additional: &BTreeSet<String>,
) -> bool {
    if value.variants.iter().any(|id| {
        additional.contains(id)
            || matches!(
                id.as_str(),
                "$app/server"
                    | "$app/env/private"
                    | "$env/static/private"
                    | "$env/dynamic/private"
                    | "__sveltekit/server"
                    | "__sveltekit/env"
                    | "__sveltekit/env/private"
                    | "__sveltekit/env/public/server"
            )
            || id.starts_with("$lib/server/")
    }) {
        return true;
    }
    for path in &value.paths {
        if !kit3
            && path.starts_with(input.lib_directory.join("server"))
            && path != &input.lib_directory.join("server")
        {
            return true;
        }
        let internal = path.starts_with(&input.project_root)
            && !path.starts_with(input.project_root.join("node_modules"));
        if !internal {
            continue;
        }
        let filename = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("");
        if filename
            .split_once(".server.")
            .is_some_and(|(_, extension)| !extension.is_empty())
            || (kit3
                && filename
                    .strip_prefix("server.")
                    .is_some_and(|ext| !ext.is_empty()))
        {
            return true;
        }
        if kit3
            && path.components().any(|part| part.as_os_str() == "server")
            && !path.starts_with(&input.routes_directory)
            && !path.starts_with(&input.assets_directory)
        {
            return true;
        }
    }
    false
}
fn worker_forbidden(input: &BuildGuardInput, kit3: bool, value: &Identity) -> bool {
    if input.target != GuardTarget::ServiceWorker {
        return false;
    }
    if kit3 {
        return value
            .variants
            .iter()
            .any(|id| matches!(id.as_str(), "$app/forms" | "$app/navigation" | "$app/state"));
    }
    value.variants.iter().any(|id| {
        (id.starts_with("$app/") || id.starts_with("$env/"))
            && !matches!(id.as_str(), "$app/env/public" | "$env/static/public")
    })
}
fn reachable(
    graph: &Graph,
    roots: &[String],
    dynamic: bool,
) -> (
    Vec<String>,
    BTreeMap<String, (String, Edge)>,
    BTreeMap<String, String>,
) {
    let mut queue = VecDeque::new();
    let mut seen = BTreeSet::new();
    let mut order = vec![];
    let mut parents = BTreeMap::new();
    let mut origins = BTreeMap::new();
    for root in roots {
        if seen.insert(root.clone()) {
            queue.push_back(root.clone());
            origins.insert(root.clone(), root.clone());
        }
    }
    while let Some(current) = queue.pop_front() {
        order.push(current.clone());
        for edge in graph.edges.get(&current).into_iter().flatten() {
            if !dynamic && edge.kind == ImportKind::Dynamic {
                continue;
            }
            if seen.insert(edge.to.clone()) {
                parents.insert(edge.to.clone(), (current.clone(), edge.clone()));
                origins.insert(edge.to.clone(), origins[&current].clone());
                queue.push_back(edge.to.clone());
            }
        }
    }
    (order, parents, origins)
}
fn chain(module: &str, parents: &BTreeMap<String, (String, Edge)>) -> Vec<ImportChainLink> {
    let mut result = Vec::new();
    let mut current = module;
    while let Some((parent, edge)) = parents.get(current) {
        result.push(ImportChainLink {
            importer: parent.clone(),
            imported: current.into(),
            kind: edge.kind,
            original_id: edge.original.clone(),
            span: edge.span.clone(),
        });
        current = parent;
    }
    result.reverse();
    result
}
fn diagnostic(
    input: &BuildGuardInput,
    node: &Identity,
    entry: &str,
    chain: Vec<ImportChainLink>,
    code: &str,
    message: String,
) -> GuardDiagnostic {
    let span = chain.last().and_then(|link| link.span.clone());
    let source = span
        .as_ref()
        .map(|span| span.source.clone())
        .or_else(|| chain.last().map(|link| link.importer.clone()))
        .unwrap_or_else(|| node.key.clone());
    GuardDiagnostic {
        code: code.into(),
        message,
        module_id: node.key.clone(),
        display_module: display(input, node),
        entry_id: entry.into(),
        chain,
        source,
        span,
    }
}
fn remote_file(input: &BuildGuardInput, kit3: bool, id: &str) -> bool {
    let filename = Path::new(id)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(id);
    if kit3 {
        filename
            .split_once(".remote.")
            .is_some_and(|(_, ext)| !ext.is_empty())
            || filename
                .strip_prefix("remote.")
                .is_some_and(|ext| !ext.is_empty())
    } else {
        input
            .module_extensions
            .iter()
            .any(|extension| filename.ends_with(&format!(".remote{extension}")))
    }
}
fn status(diagnostics: usize, unknowns: usize) -> GuardStatus {
    if diagnostics > 0 {
        GuardStatus::Failed
    } else if unknowns > 0 {
        GuardStatus::Incomplete
    } else {
        GuardStatus::Passed
    }
}

/// Validate the complete graph without treating successful private-import checks
/// as proof that unrequested/unknown runtime adapter features are supported.
pub fn validate_build_imports(input: &BuildGuardInput) -> Result<BuildGuardReport, String> {
    let kit3 = KitSyncVersion::parse(&input.kit_version)?.is_kit3();
    for path in [
        &input.project_root,
        &input.runtime_directory,
        &input.lib_directory,
        &input.routes_directory,
        &input.assets_directory,
    ] {
        if normalize_path(path)? != *path {
            return Err("Guard configuration paths must be normalized".into());
        }
    }
    for (path, canonical) in &input.real_paths {
        if normalize_path(path)? != *path || normalize_path(canonical)? != *canonical {
            return Err("Guard realpath metadata must be normalized".into());
        }
    }
    for alias in &input.aliases {
        normalize_path(&alias.replacement)?;
    }
    if input.module_extensions.is_empty()
        || input
            .module_extensions
            .iter()
            .any(|extension| !extension.starts_with('.') || extension.contains(['/', '\\', '\0']))
    {
        return Err("Invalid module extension in guard configuration".into());
    }
    let graph = graph(input, kit3)?;
    let mut additional = BTreeSet::new();
    for id in &input.additional_server_only_ids {
        let value = identity(input, kit3, id, None)?;
        additional.extend(value.variants);
        additional.insert(value.key);
    }
    let (order, parents, origins) = reachable(&graph, &graph.roots, true);
    let mut report = BuildGuardReport {
        imports_status: GuardStatus::Passed,
        import_diagnostics: vec![],
        import_unknowns: vec![],
        configuration_status: GuardStatus::Passed,
        configuration_diagnostics: vec![],
        configuration_unknowns: vec![],
        runtime_features: RuntimeFeatureReport {
            status: GuardStatus::NotRequested,
            diagnostics: vec![],
            unknowns: vec![],
            route_features: BTreeMap::new(),
        },
        reachable_modules: order.clone(),
    };
    for id in &order {
        let value = &graph.nodes[id];
        let chain = chain(id, &parents);
        let entry = &origins[id];
        if input.target != GuardTarget::Server {
            let private = server_only(input, kit3, value, &additional)
                || matches!(graph.externals.get(id), Some(ExternalExposure::ServerOnly));
            if private || worker_forbidden(input, kit3, value) {
                let code = if private {
                    "server_only_import"
                } else {
                    "service_worker_invalid_import"
                };
                let name = display(input, value);
                let message = format!(
                    "Cannot import {name} into {} code. Follow the recorded import chain; use import type for imports used only as types.",
                    if input.target == GuardTarget::Client {
                        "browser"
                    } else {
                        "service-worker"
                    }
                );
                report.import_diagnostics.push(diagnostic(
                    input,
                    value,
                    entry,
                    chain.clone(),
                    code,
                    message,
                ));
                if chain
                    .iter()
                    .any(|link| remote_file(input, kit3, &link.importer))
                {
                    match input.remote_functions_enabled {
                        Some(false) => report.configuration_diagnostics.push(diagnostic(input, value, entry, chain.clone(), "remote_functions_disabled", format!("Remote functions require {}experimental.remoteFunctions to be enabled", if kit3 { "" } else { "kit." }))),
                        None => report.configuration_unknowns.push(GuardUnknown { code: "unresolved_remote_functions_flag".into(), module_id: id.clone(), message: "Remote import chain needs the resolved remoteFunctions flag and actual client transform".into(), chain: chain.clone() }),
                        Some(true) => {},
                    }
                }
            }
            if matches!(graph.externals.get(id), Some(ExternalExposure::Unknown)) {
                report.import_unknowns.push(GuardUnknown { code: "opaque_external_module".into(), module_id: id.clone(), message: "Cannot establish private-import safety through an opaque external module; supply its graph or explicit public evidence".into(), chain: chain.clone() });
            }
        }
        if !kit3 {
            let legacy = value
                .variants
                .iter()
                .find(|id| id.starts_with("$env/static/") || id.starts_with("$env/dynamic/"));
            let explicit_public = value.variants.contains("$app/env/public");
            // Kit 2's browser dynamic/public loader intentionally bypasses the
            // experimental-flag throw; its worker build forbids the module.
            let legacy_guard = legacy.is_some_and(|id| {
                !(id == "$env/dynamic/public" && input.target == GuardTarget::Client)
            });
            if legacy_guard || (explicit_public && input.target != GuardTarget::ServiceWorker) {
                match input.explicit_environment_variables {
                    Some(enabled) if (legacy_guard && enabled) || (explicit_public && !enabled) => report.configuration_diagnostics.push(diagnostic(input, value, entry, chain.clone(), "environment_module_feature_flag", format!("{} is unavailable with experimental.explicitEnvironmentVariables={enabled}", display(input, value)))),
                    None => report.configuration_unknowns.push(GuardUnknown { code: "unresolved_explicit_environment_flag".into(), module_id: id.clone(), message: "Kit 2 environment module availability requires the resolved experimental.explicitEnvironmentVariables flag".into(), chain }),
                    _ => {},
                }
            }
        }
    }
    report.imports_status = status(
        report.import_diagnostics.len(),
        report.import_unknowns.len(),
    );
    report.configuration_status = status(
        report.configuration_diagnostics.len(),
        report.configuration_unknowns.len(),
    );
    if let Some(features) = &input.runtime_features {
        report.runtime_features = runtime_features(input, kit3, &graph, features)?;
    }
    Ok(report)
}

fn runtime_features(
    input: &BuildGuardInput,
    kit3: bool,
    graph: &Graph,
    request: &RuntimeFeatureInput,
) -> Result<RuntimeFeatureReport, String> {
    let mut proven = BTreeMap::new();
    for (module, features) in &request.proven_modules {
        let key = identity(input, kit3, module, None)?.key;
        if !graph.nodes.contains_key(&key) {
            return Err(format!(
                "Feature evidence names an unloaded module: {module}"
            ));
        }
        for feature in features {
            validate_span(&feature.span)?;
        }
        if proven.insert(key, features).is_some() {
            return Err("Conflicting feature evidence identities".into());
        }
    }
    let mut result = RuntimeFeatureReport {
        status: GuardStatus::Passed,
        diagnostics: vec![],
        unknowns: vec![],
        route_features: BTreeMap::new(),
    };
    for route in &request.routes {
        if result.route_features.contains_key(&route.route_id) {
            return Err("Duplicate route feature request".into());
        }
        let mut roots = Vec::new();
        for source in &route.server_roots {
            let key = identity(input, kit3, source, None)?.key;
            if !graph.nodes.contains_key(&key) {
                return Err(format!("Missing server feature root {source}"));
            }
            roots.push(key);
        }
        // Upstream list_features visits each root's static graph depth first.
        // Keep this distinct from the breadth-first diagnostic-chain search.
        let mut pending = roots.into_iter().rev().collect::<Vec<_>>();
        let mut visited = BTreeSet::new();
        let mut reachable = Vec::new();
        while let Some(module) = pending.pop() {
            if !visited.insert(module.clone()) {
                continue;
            }
            if let Some(edges) = graph.edges.get(&module) {
                pending.extend(
                    edges
                        .iter()
                        .rev()
                        .filter(|edge| edge.kind == ImportKind::Static)
                        .map(|edge| edge.to.clone()),
                );
            }
            reachable.push(module);
        }
        let mut features = Vec::new();
        let mut seen = BTreeSet::new();
        for module in reachable {
            let Some(uses) = proven.get(&module) else {
                result.unknowns.push(RouteFeatureUnknown {
                    route_id: route.route_id.clone(),
                    module_id: module,
                    message: "Runtime feature use has not been proven for this reachable module"
                        .into(),
                });
                continue;
            };
            for usage in *uses {
                if !seen.insert(usage.feature.clone()) {
                    continue;
                }
                features.push(usage.feature.clone());
                if usage.feature != "$app/server:read" {
                    result.unknowns.push(RouteFeatureUnknown {
                        route_id: route.route_id.clone(),
                        module_id: module.clone(),
                        message: format!("Unported runtime feature constraint {}", usage.feature),
                    });
                    continue;
                }
                if let Some(adapter) = &route.adapter_name {
                    match route.adapter_read {
                        AdapterCapability::Supported => {},
                        AdapterCapability::Unsupported => result.diagnostics.push(RouteFeatureDiagnostic { code: "adapter_read_unsupported".into(), route_id: route.route_id.clone(), module_id: module.clone(), feature: usage.feature.clone(), message: format!("Cannot use read from $app/server in route {} with adapter {adapter}: the resolved adapter capability does not support it", route.route_id), span: usage.span.clone() }),
                        AdapterCapability::Unknown => result.unknowns.push(RouteFeatureUnknown { route_id: route.route_id.clone(), module_id: module.clone(), message: format!("Adapter {adapter} read capability is unresolved for route {}", route.route_id) }),
                    }
                }
            }
        }
        result
            .route_features
            .insert(route.route_id.clone(), features);
    }
    result.status = status(result.diagnostics.len(), result.unknowns.len());
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::build_plan::AliasMatch;
    fn module(id: &str, entry: bool, imports: &[&str], dynamic: &[&str]) -> LoadedGuardModule {
        LoadedGuardModule {
            id: id.into(),
            is_entry: entry,
            is_external: false,
            imports: imports.iter().map(|id| (*id).into()).collect(),
            dynamic_imports: dynamic.iter().map(|id| (*id).into()).collect(),
            importers: vec![],
            dynamic_importers: vec![],
            exports: vec![],
        }
    }
    fn input(version: &str, modules: Vec<LoadedGuardModule>) -> BuildGuardInput {
        BuildGuardInput {
            kit_version: version.into(),
            target: GuardTarget::Client,
            project_root: "/app".into(),
            runtime_directory: "/app/node_modules/@sveltejs/kit/src/runtime".into(),
            lib_directory: "/app/src/lib".into(),
            routes_directory: "/app/src/routes".into(),
            assets_directory: "/app/static".into(),
            aliases: vec![BuildAlias {
                name: "$lib".into(),
                replacement: "/app/src/lib".into(),
                matching: AliasMatch::Prefix,
            }],
            real_paths: BTreeMap::new(),
            modules,
            externals: vec![],
            resolutions: vec![],
            entrypoints: vec![],
            additional_server_only_ids: BTreeSet::new(),
            module_extensions: vec![".js".into(), ".ts".into()],
            remote_functions_enabled: Some(false),
            explicit_environment_variables: Some(false),
            runtime_features: None,
        }
    }
    #[test]
    fn dynamic_package_chain_keeps_private_env_and_source_location() {
        let mut request = input(
            "2.70.3",
            vec![
                module(
                    "/app/src/Page.svelte",
                    true,
                    &["/app/node_modules/@scope/pkg/index.js"],
                    &[],
                ),
                module(
                    "/app/node_modules/@scope/pkg/index.js",
                    false,
                    &[],
                    &["\0virtual:env/static/private?x"],
                ),
                module("\0virtual:env/static/private", false, &[], &[]),
            ],
        );
        request.resolutions.push(GuardResolution {
            importer: "/app/node_modules/@scope/pkg/index.js".into(),
            original_id: "$env/static/private".into(),
            resolved_id: "\0virtual:env/static/private?x".into(),
            kind: ImportKind::Dynamic,
            span: Some(GuardSourceSpan {
                source: "/app/node_modules/@scope/pkg/index.js".into(),
                start: 10,
                end: 29,
            }),
        });
        let report = validate_build_imports(&request).unwrap();
        assert_eq!(report.imports_status, GuardStatus::Failed);
        assert_eq!(report.import_diagnostics.len(), 1);
        let diagnostic = &report.import_diagnostics[0];
        assert_eq!(diagnostic.chain.len(), 2);
        assert_eq!(diagnostic.chain[1].kind, ImportKind::Dynamic);
        assert_eq!(diagnostic.display_module, "$env/static/private");
        assert_eq!(diagnostic.span.as_ref().unwrap().start, 10);
    }
    #[test]
    fn cycles_and_dead_server_branch_do_not_hide_browser_chain() {
        let request = input(
            "3.0.1",
            vec![
                module(
                    "/app/src/only.server.ts",
                    false,
                    &["/app/src/secret.server.ts"],
                    &[],
                ),
                module("/app/src/client.ts", true, &["/app/src/a.ts"], &[]),
                module(
                    "/app/src/a.ts",
                    false,
                    &["/app/src/client.ts", "/app/src/secret.server.ts"],
                    &[],
                ),
                module("/app/src/secret.server.ts", false, &[], &[]),
            ],
        );
        let report = validate_build_imports(&request).unwrap();
        assert_eq!(report.import_diagnostics.len(), 1);
        assert_eq!(report.import_diagnostics[0].entry_id, "/app/src/client.ts");
        assert_eq!(report.import_diagnostics[0].chain.len(), 2);
    }
    #[test]
    fn directory_filename_rules_are_versioned_and_package_files_are_not_roots() {
        let modules = vec![
            module(
                "/app/src/client.ts",
                true,
                &[
                    "/app/src/server.ts",
                    "/app/src/server/secret.ts",
                    "/app/src/routes/server/public.ts",
                    "/app/static/server/public.ts",
                    "/app/node_modules/pkg/foo.server.ts",
                ],
                &[],
            ),
            module("/app/src/server.ts", false, &[], &[]),
            module("/app/src/server/secret.ts", false, &[], &[]),
            module("/app/src/routes/server/public.ts", false, &[], &[]),
            module("/app/static/server/public.ts", false, &[], &[]),
            module("/app/node_modules/pkg/foo.server.ts", false, &[], &[]),
        ];
        assert_eq!(
            validate_build_imports(&input("2.69.2", modules.clone()))
                .unwrap()
                .imports_status,
            GuardStatus::Passed
        );
        let report = validate_build_imports(&input("3.0.1", modules)).unwrap();
        assert_eq!(report.import_diagnostics.len(), 2);
    }
    #[test]
    fn symlinks_query_ids_and_hash_aliases_preserve_private_identity() {
        let mut request = input(
            "2.70.3",
            vec![
                module("/app/src/client.ts", true, &["/store/secret.ts?raw"], &[]),
                module("/store/secret.ts?raw", false, &[], &[]),
            ],
        );
        request.aliases.push(BuildAlias {
            name: "#secret".into(),
            replacement: "/app/src/lib/server/secret.ts".into(),
            matching: AliasMatch::Exact,
        });
        request.real_paths.insert(
            "/app/src/lib/server/secret.ts".into(),
            "/store/secret.ts".into(),
        );
        request.resolutions.push(GuardResolution {
            importer: "/app/src/client.ts".into(),
            original_id: "#secret".into(),
            resolved_id: "/store/secret.ts?raw".into(),
            kind: ImportKind::Static,
            span: None,
        });
        let report = validate_build_imports(&request).unwrap();
        assert_eq!(report.imports_status, GuardStatus::Failed);
        assert_eq!(
            report.import_diagnostics[0].chain[0].original_id.as_deref(),
            Some("#secret")
        );
    }
    #[test]
    fn workers_have_versioned_public_env_and_runtime_restrictions() {
        let mut kit2 = input(
            "2.70.3",
            vec![
                module(
                    "/app/src/sw.ts",
                    true,
                    &["\0virtual:env/dynamic/public"],
                    &[],
                ),
                module("\0virtual:env/dynamic/public", false, &[], &[]),
            ],
        );
        kit2.target = GuardTarget::ServiceWorker;
        assert_eq!(
            validate_build_imports(&kit2).unwrap().import_diagnostics[0].code,
            "service_worker_invalid_import"
        );
        let mut kit3 = input(
            "3.0.1",
            vec![
                module(
                    "/app/src/sw.ts",
                    true,
                    &[
                        "/app/node_modules/@sveltejs/kit/src/runtime/env/dynamic/public.js",
                        "/app/node_modules/@sveltejs/kit/src/runtime/app/navigation/index.js",
                    ],
                    &[],
                ),
                module(
                    "/app/node_modules/@sveltejs/kit/src/runtime/env/dynamic/public.js",
                    false,
                    &[],
                    &[],
                ),
                module(
                    "/app/node_modules/@sveltejs/kit/src/runtime/app/navigation/index.js",
                    false,
                    &[],
                    &[],
                ),
            ],
        );
        kit3.target = GuardTarget::ServiceWorker;
        let report = validate_build_imports(&kit3).unwrap();
        assert_eq!(report.import_diagnostics.len(), 1);
        assert_eq!(
            report.import_diagnostics[0].display_module,
            "$app/navigation"
        );
    }
    #[test]
    fn missing_edges_error_and_opaque_externals_are_not_success() {
        let mut request = input(
            "3.0.1",
            vec![module("/app/client.ts", true, &["@scope/package"], &[])],
        );
        assert!(
            validate_build_imports(&request)
                .unwrap_err()
                .contains("Incomplete loaded graph")
        );
        request.externals.push(GuardExternal {
            id: "@scope/package".into(),
            exposure: ExternalExposure::Unknown,
        });
        assert_eq!(
            validate_build_imports(&request).unwrap().imports_status,
            GuardStatus::Incomplete
        );
        request.externals[0].exposure = ExternalExposure::Public {
            evidence: "Host supplied public runtime module contract".into(),
        };
        assert_eq!(
            validate_build_imports(&request).unwrap().imports_status,
            GuardStatus::Passed
        );
    }
    #[test]
    fn server_graph_allows_private_modules_but_remote_client_flags_stay_separate() {
        let mut request = input(
            "2.70.3",
            vec![
                module("/app/client.ts", true, &["/app/data.remote.ts"], &[]),
                module("/app/data.remote.ts", false, &["/app/data.server.ts"], &[]),
                module("/app/data.server.ts", false, &[], &[]),
            ],
        );
        let report = validate_build_imports(&request).unwrap();
        assert_eq!(report.imports_status, GuardStatus::Failed);
        assert_eq!(
            report.configuration_diagnostics[0].code,
            "remote_functions_disabled"
        );
        request.target = GuardTarget::Server;
        assert_eq!(
            validate_build_imports(&request).unwrap().imports_status,
            GuardStatus::Passed
        );
    }
    #[test]
    fn feature_uses_and_adapter_capability_are_never_inferred_from_exports() {
        let mut request = input(
            "3.0.1",
            vec![module("/app/route.server.ts", true, &[], &[])],
        );
        request.target = GuardTarget::Server;
        request.modules[0].exports.push("read".into());
        request.runtime_features = Some(RuntimeFeatureInput {
            routes: vec![RouteFeatureInput {
                route_id: "/".into(),
                server_roots: vec!["/app/route.server.ts".into()],
                adapter_name: Some("adapter".into()),
                adapter_read: AdapterCapability::Unsupported,
            }],
            proven_modules: BTreeMap::new(),
        });
        assert_eq!(
            validate_build_imports(&request)
                .unwrap()
                .runtime_features
                .status,
            GuardStatus::Incomplete
        );
        request
            .runtime_features
            .as_mut()
            .unwrap()
            .proven_modules
            .insert(
                "/app/route.server.ts".into(),
                vec![ProvenRuntimeFeature {
                    feature: "$app/server:read".into(),
                    proof: FeatureProof::RetainedTrackingMarker,
                    span: None,
                }],
            );
        let report = validate_build_imports(&request).unwrap();
        assert_eq!(report.imports_status, GuardStatus::Passed);
        assert_eq!(report.runtime_features.status, GuardStatus::Failed);
        assert_eq!(
            report.runtime_features.diagnostics[0].code,
            "adapter_read_unsupported"
        );
        request.runtime_features.as_mut().unwrap().routes[0].adapter_read =
            AdapterCapability::Unknown;
        assert_eq!(
            validate_build_imports(&request)
                .unwrap()
                .runtime_features
                .status,
            GuardStatus::Incomplete
        );
    }
    #[test]
    fn explicit_external_nodes_require_classification_and_retain_reverse_edges() {
        let mut external = module("@scope/external", false, &[], &[]);
        external.is_external = true;
        external.dynamic_importers.push("/app/client.ts".into());
        let mut request = input(
            "3.0.1",
            vec![module("/app/client.ts", true, &[], &[]), external],
        );
        assert!(
            validate_build_imports(&request)
                .unwrap_err()
                .contains("requires explicit exposure classification")
        );
        request.externals.push(GuardExternal {
            id: "@scope/external".into(),
            exposure: ExternalExposure::Unknown,
        });
        let report = validate_build_imports(&request).unwrap();
        assert_eq!(report.imports_status, GuardStatus::Incomplete);
        assert_eq!(report.import_unknowns[0].chain.len(), 1);
        assert_eq!(report.import_unknowns[0].chain[0].kind, ImportKind::Dynamic);
        request.externals[0].exposure = ExternalExposure::ServerOnly;
        assert_eq!(
            validate_build_imports(&request).unwrap().imports_status,
            GuardStatus::Failed
        );
        request.externals[0].exposure = ExternalExposure::Public {
            evidence: "Resolved browser external contract".into(),
        };
        assert_eq!(
            validate_build_imports(&request).unwrap().imports_status,
            GuardStatus::Passed
        );
    }
    #[test]
    fn external_flag_cannot_conflict_with_loaded_metadata_or_entrypoints() {
        let mut external = module("/app/external.js?browser", false, &[], &[]);
        external.is_external = true;
        let mut request = input(
            "3.0.1",
            vec![module("/app/client.ts", true, &[], &[]), external],
        );
        request.externals.push(GuardExternal {
            id: "/app/external.js".into(),
            exposure: ExternalExposure::Unknown,
        });
        request
            .modules
            .push(module("/app/external.js", false, &[], &[]));
        assert!(
            validate_build_imports(&request)
                .unwrap_err()
                .contains("Conflicting loaded/external")
        );
        request.modules.pop();
        request.modules[1].is_entry = true;
        assert!(
            validate_build_imports(&request)
                .unwrap_err()
                .contains("cannot be an entrypoint")
        );
        request.modules[1].is_entry = false;
        request.entrypoints = vec!["/app/external.js".into()];
        assert!(
            validate_build_imports(&request)
                .unwrap_err()
                .contains("Missing loaded guard entrypoint")
        );
    }
    #[test]
    fn legacy_serialized_module_defaults_external_flag_to_false() {
        let legacy: LoadedGuardModule = serde_json::from_value(serde_json::json!({
            "id": "/app/client.ts", "isEntry": true, "imports": [],
            "dynamicImports": [], "importers": [], "dynamicImporters": [], "exports": []
        }))
        .unwrap();
        assert!(!legacy.is_external);
        assert_eq!(
            validate_build_imports(&input("3.0.1", vec![legacy]))
                .unwrap()
                .imports_status,
            GuardStatus::Passed
        );
    }
    #[test]
    fn hashed_exact_environment_aliases_enforce_kit2_flags() {
        for (name, flag, failed) in [
            ("$env/static/public", true, true),
            ("$env/static/public", false, false),
            ("$env/dynamic/public", true, false),
            ("$app/env/public", false, true),
            ("$app/env/public", true, false),
        ] {
            let mut request = input(
                "2.70.3",
                vec![
                    module(
                        "/app/client.ts",
                        true,
                        &["/app/.native/virtual/abc123.js?build"],
                        &[],
                    ),
                    module("/app/.native/virtual/abc123.js?build", false, &[], &[]),
                ],
            );
            request.explicit_environment_variables = Some(flag);
            request.aliases.push(BuildAlias {
                name: name.into(),
                matching: AliasMatch::Exact,
                replacement: "/app/.native/virtual/abc123.js".into(),
            });
            let report = validate_build_imports(&request).unwrap();
            assert_eq!(report.imports_status, GuardStatus::Passed);
            assert_eq!(
                report.configuration_status,
                if failed {
                    GuardStatus::Failed
                } else {
                    GuardStatus::Passed
                },
                "{name} flag={flag}"
            );
            if failed {
                assert_eq!(report.configuration_diagnostics[0].display_module, name);
                assert_eq!(
                    report.configuration_diagnostics[0].code,
                    "environment_module_feature_flag"
                );
                assert_eq!(report.configuration_diagnostics[0].chain.len(), 1);
                assert_eq!(
                    report.configuration_diagnostics[0].chain[0].original_id,
                    None
                );
            }
        }
    }
    #[test]
    fn hashed_private_alias_survives_realpaths_and_nul_css_graph_nodes() {
        let mut request = input(
            "2.69.2",
            vec![
                module("/app/client.ts", true, &["\0native:component.css"], &[]),
                module(
                    "\0native:component.css",
                    false,
                    &[],
                    &["/store/private.js?import"],
                ),
                module("/store/private.js?import", false, &[], &[]),
            ],
        );
        request.aliases.push(BuildAlias {
            name: "$env/static/private".into(),
            matching: AliasMatch::Exact,
            replacement: "/app/.native/virtual/hashed.js".into(),
        });
        request.real_paths.insert(
            "/app/.native/virtual/hashed.js".into(),
            "/cache/private.js".into(),
        );
        request
            .real_paths
            .insert("/cache/private.js".into(), "/store/private.js".into());
        let report = validate_build_imports(&request).unwrap();
        assert_eq!(report.imports_status, GuardStatus::Failed);
        let diagnostic = &report.import_diagnostics[0];
        assert_eq!(diagnostic.display_module, "$env/static/private");
        assert_eq!(diagnostic.chain.len(), 2);
        assert_eq!(diagnostic.chain[1].importer, "\0native:component.css");
        assert_eq!(diagnostic.chain[1].kind, ImportKind::Dynamic);
    }
    #[test]
    fn reverse_aliases_do_not_invent_prefix_or_shadowed_specifiers() {
        let mut request = input(
            "2.70.3",
            vec![
                module(
                    "/app/client.ts",
                    true,
                    &["/app/.native/virtual/env.js"],
                    &[],
                ),
                module("/app/.native/virtual/env.js", false, &[], &[]),
            ],
        );
        request.explicit_environment_variables = Some(true);
        request.aliases.push(BuildAlias {
            name: "$env/static/public".into(),
            matching: AliasMatch::Prefix,
            replacement: "/app/.native/virtual/env.js".into(),
        });
        assert_eq!(
            validate_build_imports(&request)
                .unwrap()
                .configuration_status,
            GuardStatus::Passed
        );
        request.aliases.last_mut().unwrap().replacement = "/app/elsewhere.js".into();
        request.aliases.push(BuildAlias {
            name: "$env/static/public".into(),
            matching: AliasMatch::Exact,
            replacement: "/app/.native/virtual/env.js".into(),
        });
        assert_eq!(
            validate_build_imports(&request)
                .unwrap()
                .configuration_status,
            GuardStatus::Passed
        );
    }
    #[test]
    fn reverse_only_loaded_edges_are_not_dropped_after_tree_shaking() {
        let mut request = input(
            "3.0.1",
            vec![
                module("/app/client.ts", true, &[], &[]),
                module("/app/hidden.server.ts", false, &[], &[]),
            ],
        );
        request.modules[1]
            .dynamic_importers
            .push("/app/client.ts".into());
        let report = validate_build_imports(&request).unwrap();
        assert_eq!(report.import_diagnostics.len(), 1);
        assert_eq!(
            report.import_diagnostics[0].chain[0].kind,
            ImportKind::Dynamic
        );
    }
}
