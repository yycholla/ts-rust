//! Pure, bundler-independent production planning for the registered Kit profiles.
//!
//! Derived from SvelteKit's MIT-licensed exports/vite/index.js, Kit 3
//! exports/vite/build/index.js, alias utilities, import guards and sync writers.
//! Copyright (c) Svelte contributors. See SvelteKit/LICENSE.
//!
//! `create_build_plan` combines resolved inventory, templates, environment and
//! configuration. It does not discover files, execute JS, build graphs or mark
//! any build phase complete. The server uses upstream's conservative load flags;
//! optimized client files/flags require complete source-matched load analysis.
//! This analysis is deliberately narrower than page options, endpoint methods,
//! remote export metadata and prerender entries, which remain later prerequisites.
//!
//! Consumers must implement the graph's requirements and apply import guards to
//! the complete static/dynamic graph, including aliases and resolved file IDs.
//! Kit 3 manifest placeholders are the actual upstream build protocol: they MUST
//! be replaced after emitted assets/prerender results exist, before publication.
//! The plan carries no bundler crate dependency and performs no filesystem writes.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::build_env::{
    GeneratedBuildEnvironment, ResolvedBuildEnvironment, generate_build_environment,
};
use crate::build_inventory::BuildInventory;
use crate::build_manifest::{RouterResolution, RouterType, generate_client_manifest};
use crate::build_nodes::BundleStrategy;
use crate::build_root::{RootEntry, generate_root, root_entry};
use crate::build_server::{PreloadStrategy, ResolvedServerInput, generate_server, version_hash};
use crate::sync_versions::KitSyncVersion;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResolvedNativeBuildInput {
    pub inventory: BuildInventory,
    pub server: ResolvedServerInput,
    pub environment: ResolvedBuildEnvironment,
    pub config: ResolvedBuildPlanConfig,
    pub client_analysis: Option<ResolvedClientLoadAnalysis>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResolvedBuildPlanConfig {
    pub out_dir: PathBuf,
    pub routes_directory: PathBuf,
    pub lib_directory: PathBuf,
    pub assets_directory: PathBuf,
    /// Preserve original config alias order, including Kit 3's resolved default
    /// $lib alias. Values are absolute paths; a terminal /* is allowed and
    /// removed as in upstream get_config_aliases.
    pub aliases: Vec<ResolvedConfigAlias>,
    pub bundle_strategy: BundleStrategy,
    pub mode: String,
    pub assets_url: String,
    pub paths_relative: bool,
    pub paths_origin: Option<String>,
    pub fork_preloads: bool,
    pub tracing_server: bool,
    pub link_header_preload: bool,
    pub version_poll_interval: u64,
    pub adapter_name: Option<String>,
    pub instrumentation: Option<ResolvedInstrumentation>,
    pub remote_functions_enabled: bool,
    /// Resolved modules requiring Kit's remote transforms. The caller must have
    /// completed remote-module discovery; this plan never scans application JS.
    pub remote_modules: Vec<PathBuf>,
    /// Explicit package export conditions; platform/import/default are also a
    /// bundler responsibility. Require svelte, production and target conditions.
    pub server_conditions: Vec<String>,
    pub client_conditions: Vec<String>,
    /// Explicit adapter/runtime externals, in addition to pinned Kit requirements.
    pub server_externals: Vec<String>,
    /// Vite `loadEnv(mode, envDir, envPrefix)` values exposed on `import.meta.env`
    /// (default prefix `VITE_`, default envDir the project root).
    #[serde(default)]
    pub vite_env: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResolvedInstrumentation {
    pub entry: PathBuf,
    /// Kit 2's experimental.instrumentation.server flag; Kit 3 has no flag.
    pub enabled: bool,
    /// Required when an adapter was selected. None means not resolved.
    pub adapter_supports: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResolvedConfigAlias {
    pub name: String,
    pub path: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AliasMatch {
    Exact,
    /// Match the bare name and slash-delimited descendants, never e.g. $library.
    Prefix,
    /// Match name/foo only, not bare name or name/ with an empty suffix.
    Children,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BuildAlias {
    pub name: String,
    pub replacement: PathBuf,
    pub matching: AliasMatch,
}

impl BuildAlias {
    pub fn resolve(&self, id: &str) -> Option<PathBuf> {
        if id == self.name && self.matching != AliasMatch::Children {
            return Some(self.replacement.clone());
        }
        if self.matching != AliasMatch::Exact {
            let suffix = id.strip_prefix(&self.name)?.strip_prefix('/')?;
            if self.matching == AliasMatch::Prefix || !suffix.is_empty() {
                return Some(self.replacement.join(suffix));
            }
        }
        None
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResolvedClientLoadAnalysis {
    /// Caller-owned identity of the actual analyzed server build; not a claim
    /// that this pure planner has executed or validated that build.
    pub server_build_id: String,
    pub nodes: Vec<AnalyzedNodeLoads>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AnalyzedNodeLoads {
    pub component: Option<PathBuf>,
    pub universal: Option<PathBuf>,
    pub server: Option<PathBuf>,
    pub has_server_load: bool,
    pub has_universal_load: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BuildGraphTarget {
    Server,
    Client,
    ServiceWorker,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BuildRequirement {
    NativeComponentAndScriptTransforms,
    CssAndAssetGraph,
    CompleteImportBoundaryChecks,
    ClientLoadAnalysis,
    ServerBundle,
    CompletePageAndEndpointAnalysis,
    ServerRemoteModuleTransforms,
    RemoteExportAnalysisAndTransforms,
    RuntimeFeatureTrackingTransform,
    InstrumentationOrdering,
    ClientBundleMetadata,
    ServerNodeAndManifestGeneration,
    PrerenderExecutionOrExplicitNoPrerender,
    PrerenderResults,
    Kit3ManifestPlaceholderFinalization,
    Kit3ManifestInterimValues,
    StaticAssetInventory,
    ServiceWorkerAssetInventory,
    ServiceWorkerBundle,
    AdapterImplementation,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OutputNaming {
    pub directory: PathBuf,
    pub format: String,
    pub global_name: Option<String>,
    pub entries: String,
    pub chunks: String,
    pub assets: String,
    pub code_splitting: bool,
    pub css_code_splitting: bool,
    pub preserve_entry_signatures: String,
    /// None preserves the selected bundler's default, as Kit 3 does.
    pub hoist_transitive_imports: Option<bool>,
    /// Kit 3's fixed manifest chunk breaks the manifest/content-hash cycle.
    pub fixed_chunks: Vec<FixedChunk>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FixedChunk {
    pub name: String,
    pub module: String,
    pub file: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BuildEntryGraph {
    pub target: BuildGraphTarget,
    /// Stable output entry name -> real source path or owned virtual module ID.
    pub entries: BTreeMap<String, String>,
    pub aliases: Vec<BuildAlias>,
    pub virtual_modules: BTreeMap<String, String>,
    pub defines: BTreeMap<String, String>,
    /// These values must be supplied before bundling, never defaulted to false.
    pub unresolved_defines: BTreeMap<String, BuildRequirement>,
    pub conditions: Vec<String>,
    pub external: Vec<String>,
    pub must_bundle: Vec<String>,
    pub javascript_target: Option<String>,
    pub output: OutputNaming,
    pub requires: Vec<BuildRequirement>,
    /// Prepend these imports to the application entry before graph traversal.
    pub entry_preludes: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BuildPhaseKind {
    Server,
    Analysis,
    OptimizedClient,
    NodesAndManifests,
    Prerender,
    ManifestFinalization,
    ServiceWorker,
    Adapter,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BuildPhase {
    pub kind: BuildPhaseKind,
    pub depends_on: Vec<BuildPhaseKind>,
    pub requires: Vec<BuildRequirement>,
    pub skipped: bool,
    /// Client emission depends on actual CSR page analysis. Load flags alone
    /// cannot decide whether upstream's all-csr-false branch applies.
    pub conditional_on_client_rendering: bool,
    /// All phases are instructions. No returned phase represents completed work.
    pub outputs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportGuardRequirements {
    pub kit_version: String,
    pub project_root: PathBuf,
    pub routes_directory: PathBuf,
    pub lib_directory: PathBuf,
    pub assets_directory: PathBuf,
    pub server_only_ids: BTreeSet<String>,
    pub client_entrypoints: BTreeSet<PathBuf>,
    /// Kit 2 is limited to $lib/server; Kit 3 guards project /server/ folders
    /// except routes and static, ignoring dependency paths/outside-project files.
    pub kit3_server_directories: bool,
    /// Applied only to project files outside node_modules, after canonical
    /// resolution. Kit 2 applies this pattern to the basename; Kit 3 to the ID.
    pub server_filename_pattern: String,
    /// Kit 2 worker builds allow only these framework imports. Kit 3 uses the
    /// forbidden runtime set below plus ordinary private/server graph guards.
    pub service_worker_allowed_framework_ids: Option<BTreeSet<String>>,
    pub service_worker_forbidden_runtime_modules: BTreeSet<PathBuf>,
    pub validate_transitive_and_dynamic_imports: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeBuildPlan {
    pub kit_version: String,
    pub generated_files: BTreeMap<PathBuf, String>,
    pub root: RootEntry,
    pub environment: GeneratedBuildEnvironment,
    pub server: BuildEntryGraph,
    pub client: BuildEntryGraph,
    pub service_worker: Option<BuildEntryGraph>,
    pub phases: Vec<BuildPhase>,
    pub guards: ImportGuardRequirements,
    pub assets_directory: PathBuf,
    pub copy_assets_to: PathBuf,
    pub asset_url_base: String,
    pub relative_asset_urls: bool,
    pub client_load_analysis_build_id: Option<String>,
    /// Actual upstream identifiers, left unresolved through bundling, then
    /// finalized from real asset/route/prerender metadata. Never deploy these.
    pub manifest_placeholders: Vec<String>,
    /// Emit during the actual client build, not when CSR analysis skips it.
    pub client_output_files: BTreeMap<PathBuf, String>,
}

fn js(value: impl Serialize) -> String {
    serde_json::to_string(&value)
        .expect("resolved JSON value")
        .replace('<', "\\u003C")
        .replace('\u{2028}', "\\u2028")
        .replace('\u{2029}', "\\u2029")
}
fn id(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}
fn absolute(path: &Path, name: &str) -> Result<(), String> {
    if !path.is_absolute()
        || path
            .components()
            .any(|part| matches!(part, Component::CurDir | Component::ParentDir))
        || path
            .to_str()
            .is_none_or(|path| path.contains(['\0', '?', '#']))
    {
        return Err(format!(
            "{name} must be an absolute normalized UTF-8 path without query/fragment"
        ));
    }
    Ok(())
}
fn insert_files(
    target: &mut BTreeMap<PathBuf, String>,
    files: BTreeMap<PathBuf, String>,
) -> Result<(), String> {
    for (path, content) in files {
        if target
            .get(&path)
            .is_some_and(|existing| existing != &content)
        {
            return Err(format!(
                "Generated build file collision: {}",
                path.display()
            ));
        }
        target.insert(path, content);
    }
    Ok(())
}
fn entry(
    entries: &mut BTreeMap<String, String>,
    name: String,
    source: String,
) -> Result<(), String> {
    if name.is_empty()
        || name.starts_with('/')
        || name.contains(['\\', '\0', '?', '#'])
        || name.split('/').any(|part| matches!(part, "" | "." | ".."))
    {
        return Err(format!("Unsafe build entry name {name:?}"));
    }
    if entries
        .get(&name)
        .is_some_and(|previous| previous != &source)
    {
        return Err(format!(
            "Build entry collision for {name}: {} and {source}",
            entries[&name]
        ));
    }
    entries.insert(name, source);
    Ok(())
}
fn decode_uri(value: &str) -> Result<String, String> {
    let mut decoded = Vec::new();
    let bytes = value.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hex = bytes
                .get(index + 1..index + 3)
                .ok_or("Malformed URI escape in route source path")?;
            let number = |digit: u8| {
                (digit as char)
                    .to_digit(16)
                    .ok_or("Malformed URI escape in route source path")
            };
            decoded.push((number(hex[0])? * 16 + number(hex[1])?) as u8);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).map_err(|_| "Route path URI escapes are not valid UTF-8".into())
}
fn source_entry(
    entries: &mut BTreeMap<String, String>,
    file: &Path,
    routes: &Path,
    endpoint: bool,
) -> Result<(), String> {
    absolute(file, "source entry")?;
    let name = match file.strip_prefix(routes) {
        Ok(relative) => {
            let relative = decode_uri(&id(relative))?;
            let relative = relative.strip_suffix(".js").unwrap_or(&relative);
            format!(
                "entries/{}/{relative}",
                if endpoint { "endpoints" } else { "pages" }
            )
        }
        Err(_) if !endpoint => format!(
            "entries/fallbacks/{}",
            file.file_name()
                .and_then(|name| name.to_str())
                .ok_or("Fallback component filename must be UTF-8")?
        ),
        Err(_) => return Err("Endpoint source must be inside routesDirectory".into()),
    };
    entry(entries, name, id(file))
}

fn aliases(input: &ResolvedNativeBuildInput, kit3: bool) -> Result<Vec<BuildAlias>, String> {
    let inventory = &input.inventory;
    let mut aliases = vec![BuildAlias {
        name: "$app".into(),
        replacement: inventory.runtime_directory.join("app"),
        matching: AliasMatch::Prefix,
    }];
    if kit3 {
        aliases.push(BuildAlias {
            name: "$env".into(),
            replacement: inventory.runtime_directory.join("env"),
            matching: AliasMatch::Prefix,
        });
        aliases.push(BuildAlias {
            name: "<sveltekit:generated>".into(),
            replacement: inventory.generated_directory.clone(),
            matching: AliasMatch::Prefix,
        });
    } else {
        aliases.insert(
            0,
            BuildAlias {
                name: "__SERVER__".into(),
                replacement: inventory.generated_directory.join("server"),
                matching: AliasMatch::Prefix,
            },
        );
        aliases.push(BuildAlias {
            name: "$lib".into(),
            replacement: input.config.lib_directory.clone(),
            matching: AliasMatch::Prefix,
        });
        aliases.push(BuildAlias {
            name: "__sveltekit/manifest".into(),
            replacement: inventory
                .generated_directory
                .join("client-optimized/app.js"),
            matching: AliasMatch::Exact,
        });
        aliases.push(BuildAlias {
            name: "__sveltekit/remote".into(),
            replacement: inventory
                .runtime_directory
                .join("client/remote-functions/index.js"),
            matching: AliasMatch::Exact,
        });
    }
    let names: BTreeSet<_> = input
        .config
        .aliases
        .iter()
        .map(|alias| alias.name.clone())
        .collect();
    if names.len() != input.config.aliases.len() {
        return Err("Duplicate configuration alias".into());
    }
    for alias in &input.config.aliases {
        let (name, matching) = if let Some(name) = alias.name.strip_suffix("/*") {
            (name, AliasMatch::Children)
        } else if names.contains(&format!("{}/*", alias.name)) {
            (alias.name.as_str(), AliasMatch::Exact)
        } else {
            (alias.name.as_str(), AliasMatch::Prefix)
        };
        if name.is_empty() || name.contains(['*', '\0', '?', '#']) || name.ends_with('/') {
            return Err(format!("Unsupported configuration alias {:?}", alias.name));
        }
        let path = id(&alias.path);
        let replacement = PathBuf::from(path.strip_suffix("/*").unwrap_or(&path));
        absolute(&replacement, "alias path")?;
        aliases.push(BuildAlias {
            name: name.into(),
            replacement,
            matching,
        });
    }
    Ok(aliases)
}

fn validate(input: &ResolvedNativeBuildInput) -> Result<bool, String> {
    let inventory = &input.inventory;
    let config = &input.config;
    let kit3 = KitSyncVersion::parse(&inventory.kit_version)?.is_kit3();
    for (name, path) in [
        ("projectRoot", &inventory.project_root),
        ("runtimeDirectory", &inventory.runtime_directory),
        ("generatedDirectory", &inventory.generated_directory),
        ("outDir", &config.out_dir),
        ("routesDirectory", &config.routes_directory),
        ("libDirectory", &config.lib_directory),
        ("assetsDirectory", &config.assets_directory),
    ] {
        absolute(path, name)?;
    }
    if !config.out_dir.starts_with(&inventory.project_root)
        || config.out_dir == inventory.project_root
    {
        return Err("Build outDir must be a dedicated directory inside projectRoot".into());
    }
    for source in [
        &config.routes_directory,
        &config.lib_directory,
        &config.assets_directory,
        &inventory.runtime_directory,
    ] {
        if config.out_dir.starts_with(source) || source.starts_with(&config.out_dir) {
            return Err(
                "Build outDir must not overlap routes, library, assets or installed runtime inputs"
                    .into(),
            );
        }
    }
    let generated = config
        .out_dir
        .join(if kit3 { "generated/build" } else { "generated" });
    if inventory.options.dev || inventory.generated_directory != generated {
        return Err(
            "Inventory must use the selected profile's production generated directory".into(),
        );
    }
    if inventory.kit_version != input.server.kit_version
        || inventory.kit_version != input.environment.kit_version
        || input.server.output != generated
        || input.environment.environment_directory != generated.join("env")
        || input.server.runtime_directory.as_ref() != Some(&inventory.runtime_directory)
        || input.environment.runtime_directory != inventory.runtime_directory
        || input.server.version_name != input.environment.version_name
        || input.server.server_hooks != inventory.hooks.server
        || input.server.universal_hooks != inventory.hooks.universal
        || input.server.svelte_major != inventory.options.svelte_major
        || input.server.service_worker_present != inventory.service_worker.is_some()
        || input.server.options.async_compilation != inventory.options.async_compiler
        || input.server.options.server_error_boundaries != inventory.options.handle_rendering_errors
        || input.server.options.hash_routing
            != (inventory.options.router.router_type == RouterType::Hash)
        || input.server.options.public_prefix != input.environment.public_prefix
        || input.server.options.private_prefix != input.environment.private_prefix
        || input.server.options.explicit_environment_variables
            != input.environment.explicit_environment_variables
    {
        return Err("Inventory, server and environment inputs disagree on their resolved version, paths, hooks or configuration".into());
    }
    if inventory.service_worker.is_some() && !config.assets_url.is_empty() {
        return Err("Service workers cannot be built with a separate paths.assets URL".into());
    }
    if config.mode.is_empty() || config.mode.contains('\0') {
        return Err("Build mode must be explicit".into());
    }
    for (label, conditions, platform) in [
        ("server", &config.server_conditions, "node"),
        ("client", &config.client_conditions, "browser"),
    ] {
        for required in ["svelte", "production", platform] {
            if !conditions.iter().any(|condition| condition == required) {
                return Err(format!(
                    "{label} conditions must explicitly contain {required}"
                ));
            }
        }
        if conditions.iter().any(|condition| {
            condition == "development"
                || condition
                    == if platform == "node" {
                        "browser"
                    } else {
                        "node"
                    }
        }) {
            return Err(format!(
                "Conflicting development/platform export conditions in {label} graph"
            ));
        }
    }
    if let Some(instrumentation) = &config.instrumentation {
        absolute(&instrumentation.entry, "instrumentation.entry")?;
        if !kit3 && !instrumentation.enabled {
            return Err(
                "Kit 2 instrumentation requires experimental.instrumentation.server".into(),
            );
        }
        if config.adapter_name.is_some() && instrumentation.adapter_supports != Some(true) {
            return Err("Selected adapter must explicitly support server instrumentation".into());
        }
    }
    if !config.remote_modules.is_empty() && !config.remote_functions_enabled {
        return Err("Remote modules require the resolved remoteFunctions feature flag".into());
    }
    for path in &config.remote_modules {
        absolute(path, "remote module")?;
    }
    if let Some(analysis) = &input.client_analysis {
        if analysis.server_build_id.is_empty() || analysis.nodes.len() != inventory.nodes.len() {
            return Err(
                "Client load analysis requires a serverBuildId and one record per inventory node"
                    .into(),
            );
        }
        for (index, (actual, expected)) in analysis.nodes.iter().zip(&inventory.nodes).enumerate() {
            if actual.component != expected.component
                || actual.universal != expected.universal
                || actual.server != expected.server
                || (actual.has_server_load && actual.server.is_none())
                || (actual.has_universal_load && actual.universal.is_none())
            {
                return Err(format!(
                    "Client load analysis does not match inventory node {index}"
                ));
            }
        }
    }
    Ok(kit3)
}

/// Vite 7.3.1 `vite:define` for `import.meta.env`: each env key is replaced by
/// its JSON value, and the bare object by `serializeDefine` of all keys plus
/// SSR (sorted). Vite replaces an unknown `import.meta.env.X` with `undefined`;
/// here it reads a missing property of that object, which is also undefined
/// except for inherited `Object.prototype` names.
fn import_meta_env(input: &ResolvedNativeBuildInput, ssr: bool) -> BTreeMap<String, String> {
    let config = &input.config;
    // Kit sets Vite `base` to './' for relative paths; Vite resolves that to
    // '/' for SSR builds and keeps './' for the client.
    let base_url = if config.paths_relative || !config.assets_url.is_empty() {
        if ssr { "/".to_owned() } else { "./".to_owned() }
    } else {
        format!("{}/", input.environment.base)
    };
    let mut values: BTreeMap<String, String> = config
        .vite_env
        .iter()
        .map(|(key, value)| (key.clone(), js(value)))
        .collect();
    values.insert("BASE_URL".into(), js(&base_url));
    values.insert("MODE".into(), js(&config.mode));
    values.insert("DEV".into(), "false".into());
    values.insert("PROD".into(), "true".into());
    values.insert("SSR".into(), ssr.to_string());
    let object = values
        .iter()
        .map(|(key, value)| format!("{}: {value}", js(key)))
        .collect::<Vec<_>>()
        .join(", ");
    BTreeMap::from([
        ("import.meta.env.BASE_URL".into(), js(&base_url)),
        ("import.meta.env".into(), format!("{{{object}}}")),
    ])
}
fn common_defines(input: &ResolvedNativeBuildInput, kit3: bool) -> BTreeMap<String, String> {
    let config = &input.config;
    let server = &input.server;
    let environment = &input.environment;
    let inventory = &input.inventory;
    let mut definitions = BTreeMap::from([
        ("__SVELTEKIT_APP_DIR__".into(), js(&environment.app_dir)),
        (
            "__SVELTEKIT_APP_VERSION__".into(),
            js(&environment.version_name),
        ),
        (
            "__SVELTEKIT_APP_VERSION_FILE__".into(),
            js(format!("{}/version.json", environment.app_dir)),
        ),
        (
            "__SVELTEKIT_APP_VERSION_POLL_INTERVAL__".into(),
            config.version_poll_interval.to_string(),
        ),
        (
            "__SVELTEKIT_ADAPTER_NAME__".into(),
            config
                .adapter_name
                .as_ref()
                .map(js)
                .unwrap_or_else(|| "undefined".into()),
        ),
        ("__SVELTEKIT_EMBEDDED__".into(), js(server.options.embedded)),
        (
            "__SVELTEKIT_FORK_PRELOADS__".into(),
            js(config.fork_preloads),
        ),
        ("__SVELTEKIT_PATHS_ASSETS__".into(), js(&config.assets_url)),
        ("__SVELTEKIT_PATHS_BASE__".into(), js(&environment.base)),
        (
            "__SVELTEKIT_PATHS_RELATIVE__".into(),
            js(config.paths_relative),
        ),
        (
            "__SVELTEKIT_CLIENT_ROUTING__".into(),
            js(inventory.options.router.resolution == RouterResolution::Client),
        ),
        (
            "__SVELTEKIT_HASH_ROUTING__".into(),
            js(inventory.options.router.router_type == RouterType::Hash),
        ),
        (
            "__SVELTEKIT_SERVER_TRACING_ENABLED__".into(),
            js(config.tracing_server),
        ),
        ("__SVELTEKIT_DEV__".into(), "false".into()),
        ("import.meta.env.DEV".into(), "false".into()),
        ("import.meta.env.PROD".into(), "true".into()),
        ("import.meta.env.MODE".into(), js(&config.mode)),
        ("import.meta.hot".into(), "undefined".into()),
    ]);
    for (key, value) in &config.vite_env {
        definitions.insert(format!("import.meta.env.{key}"), js(value));
    }
    if kit3 {
        definitions.extend([
            (
                "__SVELTEKIT_APP_VERSION_CHECKS_ENABLED__".into(),
                js(config.bundle_strategy != BundleStrategy::Inline),
            ),
            (
                "__SVELTEKIT_SUPPORTS_ASYNC__".into(),
                js(inventory.options.async_compiler),
            ),
            (
                "__SVELTEKIT_GLOBAL_NAME__".into(),
                js(format!(
                    "__sveltekit_{}",
                    version_hash(&environment.version_name)
                )),
            ),
            (
                "__SVELTEKIT_CSRF_CHECK_ORIGIN__".into(),
                js(!server
                    .options
                    .csrf_trusted_origins
                    .iter()
                    .any(|origin| origin == "*")),
            ),
            (
                "__SVELTEKIT_LINK_HEADER_PRELOAD__".into(),
                js(config.link_header_preload),
            ),
            (
                "__SVELTEKIT_PATHS_ORIGIN__".into(),
                config
                    .paths_origin
                    .as_ref()
                    .map(js)
                    .unwrap_or_else(|| "undefined".into()),
            ),
            (
                "__SVELTEKIT_SERVICE_WORKER__".into(),
                js(server.options.service_worker_register && inventory.service_worker.is_some()),
            ),
        ]);
    } else {
        definitions.insert(
            "__SVELTEKIT_EXPERIMENTAL_USE_TRANSFORM_ERROR__".into(),
            js(inventory.options.handle_rendering_errors),
        );
        definitions.insert(
            "__SVELTEKIT_EXPERIMENTAL_EXPLICIT_ENVIRONMENT_VARIABLES__".into(),
            js(environment.explicit_environment_variables),
        );
    }
    definitions
}

/// Construct a deterministic production plan. Nothing in this API executes a
/// build, evaluates page options, writes output or certifies runtime readiness.
pub fn create_build_plan(input: &ResolvedNativeBuildInput) -> Result<NativeBuildPlan, String> {
    let kit3 = validate(input)?;
    let inventory = &input.inventory;
    let config = &input.config;
    let generated = &inventory.generated_directory;
    let runtime = &inventory.runtime_directory;
    let output = config.out_dir.join("output");
    let immutable = format!("{}/immutable", input.environment.app_dir);
    let global = format!(
        "__sveltekit_{}",
        version_hash(&input.environment.version_name)
    );
    let environment = generate_build_environment(&input.environment)?;
    let root_input = inventory.root_input();
    let root = root_entry(&root_input)?;
    let mut generated_files = generate_server(&input.server)?;
    insert_files(&mut generated_files, generate_root(&root_input)?)?;
    insert_files(&mut generated_files, environment.files.clone())?;
    insert_files(
        &mut generated_files,
        generate_client_manifest(&inventory.client_manifest_input(None))?,
    )?;
    if let Some(analysis) = &input.client_analysis {
        let mut manifest = inventory.client_manifest_input(Some(
            analysis
                .nodes
                .iter()
                .map(|node| node.has_server_load)
                .collect(),
        ));
        manifest.output_directory = generated.join("client-optimized");
        insert_files(&mut generated_files, generate_client_manifest(&manifest)?)?;
    }
    let mut manifest_placeholders = Vec::new();
    if kit3 {
        let mut manifest = String::new();
        for (export, key) in [
            ("immutable", "IMMUTABLE"),
            ("assets", "ASSETS"),
            ("prerendered", "PRERENDERED"),
            ("routes", "ROUTES"),
        ] {
            let name = format!("__SVELTEKIT_MANIFEST_{key}__");
            manifest.push_str(&format!("export const {export} = {name};\n"));
            manifest_placeholders.push(name);
        }
        insert_files(
            &mut generated_files,
            BTreeMap::from([(generated.join("app-manifest.js"), manifest)]),
        )?;
    }
    let aliases = aliases(input, kit3)?;
    let common = common_defines(input, kit3);
    let mut server_entries = BTreeMap::new();
    entry(
        &mut server_entries,
        "index".into(),
        id(&runtime.join("server/index.js")),
    )?;
    entry(
        &mut server_entries,
        "remote-entry".into(),
        id(&runtime.join("app/server/remote/index.js")),
    )?;
    entry(
        &mut server_entries,
        "env".into(),
        if kit3 {
            "<sveltekit:generated>/env/config.js".into()
        } else {
            "__sveltekit/env".into()
        },
    )?;
    if !kit3 {
        entry(
            &mut server_entries,
            "internal".into(),
            id(&generated.join("server/internal.js")),
        )?;
    }
    for route in &inventory.routes {
        if let Some(file) = &route.endpoint {
            source_entry(&mut server_entries, file, &config.routes_directory, true)?;
        }
    }
    for node in &inventory.nodes {
        for file in [&node.component, &node.universal, &node.server]
            .into_iter()
            .flatten()
        {
            source_entry(&mut server_entries, file, &config.routes_directory, false)?;
        }
    }
    if kit3 {
        if let Some(params) = &inventory.params_module {
            absolute(params, "paramsModule")?;
            entry(&mut server_entries, "entries/params".into(), id(params))?;
        }
    } else {
        for (name, matcher) in &inventory.matchers {
            absolute(matcher, "matcher")?;
            entry(
                &mut server_entries,
                format!("entries/matchers/{name}"),
                id(matcher),
            )?;
        }
    }
    for (name, hook) in [
        ("entries/hooks.server", &inventory.hooks.server),
        ("entries/hooks.universal", &inventory.hooks.universal),
    ] {
        if let Some(hook) = hook {
            absolute(hook, "hook")?;
            entry(&mut server_entries, name.into(), id(hook))?;
        }
    }
    if let Some(instrumentation) = &config.instrumentation {
        entry(
            &mut server_entries,
            "instrumentation.server".into(),
            id(&instrumentation.entry),
        )?;
    }
    let mut server_defines = common.clone();
    server_defines.extend(import_meta_env(input, true));
    server_defines.extend([
        ("__SVELTEKIT_HAS_SERVER_LOAD__".into(), "true".into()),
        ("__SVELTEKIT_HAS_UNIVERSAL_LOAD__".into(), "true".into()),
        ("__SVELTEKIT_PAYLOAD__".into(), "{}".into()),
        ("import.meta.env.SSR".into(), "true".into()),
    ]);
    let mut virtual_modules = BTreeMap::new();
    if !kit3 {
        virtual_modules.insert("__sveltekit/server".into(), "export let read_implementation = null;\nexport let manifest = null;\nexport function set_read_implementation(fn) { read_implementation = fn; }\nexport function set_manifest(_) { manifest = _; }\n".into());
    }
    let mut server_requires = vec![
        BuildRequirement::NativeComponentAndScriptTransforms,
        BuildRequirement::CssAndAssetGraph,
        BuildRequirement::RuntimeFeatureTrackingTransform,
    ];
    if !config.remote_modules.is_empty() {
        server_requires.push(BuildRequirement::ServerRemoteModuleTransforms);
    }
    if config.instrumentation.is_some() {
        server_requires.push(BuildRequirement::InstrumentationOrdering);
    }
    let mut external = config.server_externals.clone();
    if kit3 && !external.iter().any(|name| name == "@opentelemetry/api") {
        external.push("@opentelemetry/api".into());
    }
    let server = BuildEntryGraph {
        target: BuildGraphTarget::Server,
        entries: server_entries,
        aliases: aliases.clone(),
        virtual_modules,
        defines: server_defines,
        unresolved_defines: BTreeMap::new(),
        conditions: config.server_conditions.clone(),
        external,
        must_bundle: vec!["esm-env".into(), "@sveltejs/kit/src/runtime".into()],
        javascript_target: Some(if kit3 { "node22" } else { "node18.13" }.into()),
        output: OutputNaming {
            directory: output.join("server"),
            format: "esm".into(),
            global_name: None,
            entries: "[name].js".into(),
            chunks: "chunks/[name].js".into(),
            assets: format!("{immutable}/assets/[name].[hash][extname]"),
            code_splitting: true,
            css_code_splitting: config.bundle_strategy != BundleStrategy::Inline,
            preserve_entry_signatures: "strict".into(),
            hoist_transitive_imports: if kit3 { None } else { Some(false) },
            fixed_chunks: Vec::new(),
        },
        requires: server_requires,
        entry_preludes: Vec::new(),
    };
    let mut client_entries = BTreeMap::new();
    if config.bundle_strategy == BundleStrategy::Split {
        entry(
            &mut client_entries,
            "entry/start".into(),
            id(&runtime.join("client/entry.js")),
        )?;
        entry(
            &mut client_entries,
            "entry/app".into(),
            id(&generated.join("client-optimized/app.js")),
        )?;
        if kit3 {
            entry(
                &mut client_entries,
                "entry/payload".into(),
                id(&runtime.join("client/payload.js")),
            )?;
        }
        for (index, node) in inventory.nodes.iter().enumerate() {
            if node.component.is_some() || node.universal.is_some() {
                entry(
                    &mut client_entries,
                    format!("nodes/{index}"),
                    id(&generated.join(format!("client-optimized/nodes/{index}.js"))),
                )?;
            }
        }
    } else {
        entry(
            &mut client_entries,
            "bundle".into(),
            id(&runtime.join("client/bundle.js")),
        )?;
    }
    let mut client_defines = common;
    client_defines.extend(import_meta_env(input, false));
    client_defines.insert("import.meta.env.SSR".into(), "false".into());
    client_defines.insert(
        "__SVELTEKIT_PAYLOAD__".into(),
        if kit3 && config.bundle_strategy == BundleStrategy::Split {
            "undefined".into()
        } else {
            format!("globalThis.{global}")
        },
    );
    if kit3 && config.bundle_strategy == BundleStrategy::Inline {
        client_defines.insert("import.meta".into(), "{}".into());
    }
    let mut unresolved_defines = BTreeMap::new();
    for (name, universal) in [
        ("__SVELTEKIT_HAS_SERVER_LOAD__", false),
        ("__SVELTEKIT_HAS_UNIVERSAL_LOAD__", true),
    ] {
        if let Some(analysis) = &input.client_analysis {
            client_defines.insert(
                name.into(),
                js(analysis.nodes.iter().any(|node| {
                    if universal {
                        node.has_universal_load
                    } else {
                        node.has_server_load
                    }
                })),
            );
        } else {
            unresolved_defines.insert(name.into(), BuildRequirement::ClientLoadAnalysis);
        }
    }
    let extension = if !kit3
        && matches!(
            input.server.options.preload_strategy,
            PreloadStrategy::PreloadMjs
        ) {
        "mjs"
    } else {
        "js"
    };
    let mut client_requires = vec![
        BuildRequirement::NativeComponentAndScriptTransforms,
        BuildRequirement::CssAndAssetGraph,
        BuildRequirement::CompleteImportBoundaryChecks,
        BuildRequirement::CompletePageAndEndpointAnalysis,
        BuildRequirement::RuntimeFeatureTrackingTransform,
    ];
    if input.client_analysis.is_none() {
        client_requires.push(BuildRequirement::ClientLoadAnalysis);
    }
    if !config.remote_modules.is_empty() {
        client_requires.push(BuildRequirement::RemoteExportAnalysisAndTransforms);
    }
    let client = BuildEntryGraph {
        target: BuildGraphTarget::Client,
        entries: client_entries,
        aliases: aliases.clone(),
        virtual_modules: BTreeMap::new(),
        defines: client_defines,
        unresolved_defines,
        conditions: config.client_conditions.clone(),
        external: Vec::new(),
        must_bundle: vec!["esm-env".into(), "@sveltejs/kit/src/runtime".into()],
        javascript_target: None,
        output: OutputNaming {
            directory: output.join("client"),
            format: if config.bundle_strategy == BundleStrategy::Inline {
                "iife"
            } else {
                "esm"
            }
            .into(),
            global_name: Some(format!("{global}.app")),
            entries: format!("{immutable}/[name].[hash].{extension}"),
            chunks: format!("{immutable}/chunks/[hash].{extension}"),
            assets: format!("{immutable}/assets/[name].[hash][extname]"),
            code_splitting: config.bundle_strategy == BundleStrategy::Split,
            css_code_splitting: config.bundle_strategy != BundleStrategy::Inline,
            preserve_entry_signatures: "strict".into(),
            hoist_transitive_imports: if kit3 { None } else { Some(false) },
            fixed_chunks: if kit3 && config.bundle_strategy == BundleStrategy::Split {
                vec![FixedChunk {
                    name: "sveltekit-manifest".into(),
                    module: "<sveltekit:generated>/app-manifest.js".into(),
                    file: format!("{}/manifest.js", input.environment.app_dir),
                }]
            } else {
                Vec::new()
            },
        },
        requires: client_requires,
        entry_preludes: Vec::new(),
    };
    let service_worker = if let Some(path) = &inventory.service_worker {
        absolute(path, "serviceWorker")?;
        let mut defines = common_defines(input, kit3);
        defines.insert("import.meta.env.SSR".into(), "false".into());
        defines.extend(environment.service_worker_defines.clone());
        Some(BuildEntryGraph {
            target: BuildGraphTarget::ServiceWorker,
            entries: BTreeMap::from([("service-worker".into(), id(path))]),
            aliases,
            virtual_modules: BTreeMap::new(),
            defines,
            unresolved_defines: if kit3 {
                manifest_placeholders
                    .iter()
                    .map(|name| (name.clone(), BuildRequirement::ServiceWorkerAssetInventory))
                    .collect()
            } else {
                BTreeMap::new()
            },
            conditions: config.client_conditions.clone(),
            external: environment.service_worker_externals.clone(),
            must_bundle: vec!["esm-env".into()],
            javascript_target: None,
            output: OutputNaming {
                directory: output.join("client"),
                format: "esm".into(),
                global_name: None,
                entries: "service-worker.js".into(),
                chunks: format!("{immutable}/chunks/[hash].js"),
                assets: format!("{immutable}/assets/[name].[hash][extname]"),
                code_splitting: false,
                css_code_splitting: true,
                preserve_entry_signatures: "strict".into(),
                hoist_transitive_imports: None,
                fixed_chunks: Vec::new(),
            },
            requires: vec![
                BuildRequirement::NativeComponentAndScriptTransforms,
                BuildRequirement::CompleteImportBoundaryChecks,
                BuildRequirement::ServiceWorkerAssetInventory,
                BuildRequirement::PrerenderResults,
            ],
            entry_preludes: environment
                .service_worker_bootstrap
                .iter()
                .map(|path| format!("import {};", js(id(path))))
                .collect(),
        })
    } else {
        None
    };
    let mut server_only_ids = environment.server_only_modules.clone();
    server_only_ids.extend([
        "$app/server".into(),
        id(&runtime.join("app/server/index.js")),
        "__sveltekit/server".into(),
    ]);
    let mut client_entrypoints = BTreeSet::new();
    for node in &inventory.nodes {
        client_entrypoints.extend(node.component.clone());
        client_entrypoints.extend(node.universal.clone());
    }
    client_entrypoints.extend(inventory.hooks.client.clone());
    client_entrypoints.extend(inventory.hooks.universal.clone());
    client_entrypoints.extend(inventory.service_worker.clone());
    let guards = ImportGuardRequirements {
        kit_version: inventory.kit_version.clone(),
        project_root: inventory.project_root.clone(),
        routes_directory: config.routes_directory.clone(),
        lib_directory: config.lib_directory.clone(),
        assets_directory: config.assets_directory.clone(),
        server_only_ids,
        client_entrypoints,
        kit3_server_directories: kit3,
        server_filename_pattern: if kit3 {
            r"[/.]server\.[^/]+$"
        } else {
            r".*\.server\..+"
        }
        .into(),
        service_worker_allowed_framework_ids: if kit3 {
            None
        } else {
            Some(
                ["$service-worker", "$env/static/public", "$app/env/public"]
                    .into_iter()
                    .map(str::to_owned)
                    .collect(),
            )
        },
        service_worker_forbidden_runtime_modules: if kit3 {
            ["forms", "navigation", "state"]
                .into_iter()
                .map(|name| runtime.join(format!("app/{name}/index.js")))
                .collect()
        } else {
            BTreeSet::new()
        },
        validate_transitive_and_dynamic_imports: true,
    };
    let phase = |kind, depends_on, requires, skipped, outputs: &[&str]| BuildPhase {
        kind,
        depends_on,
        requires,
        skipped,
        conditional_on_client_rendering: kind == BuildPhaseKind::OptimizedClient,
        outputs: outputs.iter().map(|output| (*output).into()).collect(),
    };
    let phases = vec![
        phase(
            BuildPhaseKind::Server,
            vec![],
            server.requires.clone(),
            false,
            &[
                "server chunks and source-module metadata",
                "provisional server node loaders",
            ],
        ),
        phase(
            BuildPhaseKind::Analysis,
            vec![BuildPhaseKind::Server],
            if kit3 {
                vec![
                    BuildRequirement::ServerBundle,
                    BuildRequirement::CompletePageAndEndpointAnalysis,
                    BuildRequirement::StaticAssetInventory,
                    BuildRequirement::Kit3ManifestInterimValues,
                ]
            } else {
                vec![
                    BuildRequirement::ServerBundle,
                    BuildRequirement::CompletePageAndEndpointAnalysis,
                ]
            },
            false,
            &[
                "actual load exports",
                "page/endpoint options and entries",
                "remote export metadata",
            ],
        ),
        phase(
            BuildPhaseKind::OptimizedClient,
            vec![BuildPhaseKind::Analysis],
            client.requires.clone(),
            false,
            &[
                "optimized client manifest",
                "client chunks/CSS/assets/dependency metadata",
            ],
        ),
        phase(
            BuildPhaseKind::NodesAndManifests,
            vec![BuildPhaseKind::OptimizedClient],
            vec![
                BuildRequirement::ClientBundleMetadata,
                BuildRequirement::ServerNodeAndManifestGeneration,
            ],
            false,
            &[
                "final server nodes",
                "SSR manifest",
                "client manifest and preload metadata",
            ],
        ),
        phase(
            BuildPhaseKind::Prerender,
            vec![BuildPhaseKind::NodesAndManifests],
            vec![BuildRequirement::PrerenderExecutionOrExplicitNoPrerender],
            false,
            &[
                "prerendered paths/pages/dependencies",
                "prerender route exclusions",
            ],
        ),
        phase(
            BuildPhaseKind::ManifestFinalization,
            vec![BuildPhaseKind::Prerender],
            vec![
                BuildRequirement::PrerenderResults,
                BuildRequirement::Kit3ManifestPlaceholderFinalization,
            ],
            !kit3,
            &["final Kit 3 app-manifest values in all output graphs"],
        ),
        phase(
            BuildPhaseKind::ServiceWorker,
            vec![
                BuildPhaseKind::Prerender,
                BuildPhaseKind::ManifestFinalization,
            ],
            vec![BuildRequirement::ServiceWorkerAssetInventory],
            service_worker.is_none(),
            &["service-worker.js"],
        ),
        phase(
            BuildPhaseKind::Adapter,
            vec![BuildPhaseKind::ServiceWorker],
            vec![BuildRequirement::AdapterImplementation],
            false,
            &["adapter-specific executable deployment output"],
        ),
    ];
    Ok(NativeBuildPlan {
        kit_version: inventory.kit_version.clone(),
        generated_files,
        root,
        environment,
        server,
        client,
        service_worker,
        phases,
        guards,
        assets_directory: config.assets_directory.clone(),
        copy_assets_to: output.join("client"),
        asset_url_base: format!(
            "{}/",
            if config.assets_url.is_empty() {
                &input.environment.base
            } else {
                &config.assets_url
            }
        ),
        relative_asset_urls: config.paths_relative || !config.assets_url.is_empty(),
        client_load_analysis_build_id: input
            .client_analysis
            .as_ref()
            .map(|analysis| analysis.server_build_id.clone()),
        manifest_placeholders,
        client_output_files: BTreeMap::from([(
            output
                .join("client")
                .join(&input.environment.app_dir)
                .join("version.json"),
            js(serde_json::json!({"version": input.environment.version_name})),
        )]),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::build_inventory::{
        BuildInventoryOptions, HookPaths, InventoryNode, InventoryRoute, NodeKind,
    };
    use crate::build_manifest::{ClientPage, ClientRouterConfig};
    use crate::build_server::ResolvedServerOptions;

    fn input(version: &str) -> ResolvedNativeBuildInput {
        let kit3 = version.starts_with('3');
        let runtime = PathBuf::from("/app/node_modules/@sveltejs/kit/src/runtime");
        let generated = PathBuf::from(if kit3 {
            "/app/.kit/generated/build"
        } else {
            "/app/.kit/generated"
        });
        let inventory = BuildInventory {
            kit_version: version.into(),
            project_root: "/app".into(),
            runtime_directory: runtime.clone(),
            generated_directory: generated.clone(),
            options: BuildInventoryOptions {
                svelte_major: 5,
                router: ClientRouterConfig {
                    resolution: RouterResolution::Client,
                    router_type: RouterType::Pathname,
                },
                hooks: None,
                handle_rendering_errors: false,
                async_compiler: false,
                dev: false,
            },
            nodes: vec![
                InventoryNode {
                    kind: NodeKind::Layout,
                    route_id: "/".into(),
                    depth: 0,
                    component: Some(runtime.join("components/layout.svelte")),
                    universal: None,
                    server: None,
                    parent: None,
                    child_pages: vec![2],
                },
                InventoryNode {
                    kind: NodeKind::Error,
                    route_id: "/".into(),
                    depth: 0,
                    component: Some(runtime.join("components/error.svelte")),
                    universal: None,
                    server: None,
                    parent: None,
                    child_pages: vec![],
                },
                InventoryNode {
                    kind: NodeKind::Page,
                    route_id: "/".into(),
                    depth: 1,
                    component: Some("/app/src/routes/+page.svelte".into()),
                    universal: Some("/app/src/routes/+page.ts".into()),
                    server: Some("/app/src/routes/+page.server.ts".into()),
                    parent: Some(0),
                    child_pages: vec![],
                },
            ],
            routes: vec![InventoryRoute {
                id: "/".into(),
                parent_id: None,
                params: vec![],
                page: Some(ClientPage {
                    leaf: 2,
                    layouts: vec![Some(0)],
                    errors: vec![Some(1)],
                }),
                layout: Some(0),
                error: Some(1),
                endpoint: Some("/app/src/routes/+server.ts".into()),
            }],
            hooks: HookPaths::default(),
            matchers: BTreeMap::new(),
            params_module: None,
            service_worker: None,
            inputs: BTreeSet::new(),
            page_options_analyzed: false,
        };
        ResolvedNativeBuildInput {
            inventory,
            server: ResolvedServerInput { kit_version: version.into(), output: generated.clone(), app_template: "<!doctype html><html><head>%sveltekit.head%</head><body>%sveltekit.body%</body></html>".into(), error_template: "%sveltekit.status% %sveltekit.error.message%".into(), version_name: "v1".into(), server_hooks: None, universal_hooks: None, runtime_directory: Some(runtime.clone()), svelte_major: 5, service_worker_present: false, options: ResolvedServerOptions::default() },
            environment: ResolvedBuildEnvironment { kit_version: version.into(), environment_directory: generated.join("env"), runtime_directory: runtime, version_name: "v1".into(), base: String::new(), app_dir: "_app".into(), build_values: BTreeMap::new(), public_prefix: "PUBLIC_".into(), private_prefix: String::new(), explicit_environment_variables: false, definition: None },
            config: ResolvedBuildPlanConfig { out_dir: "/app/.kit".into(), routes_directory: "/app/src/routes".into(), lib_directory: "/app/src/lib".into(), assets_directory: "/app/static".into(), aliases: vec![], bundle_strategy: BundleStrategy::Split, mode: "production".into(), assets_url: String::new(), paths_relative: true, paths_origin: None, fork_preloads: false, tracing_server: false, link_header_preload: false, version_poll_interval: 0, adapter_name: None, instrumentation: None, remote_functions_enabled: false, remote_modules: vec![], server_conditions: vec!["svelte".into(), "node".into(), "production".into()], client_conditions: vec!["svelte".into(), "browser".into(), "production".into()], server_externals: vec![], vite_env: BTreeMap::new() },
            client_analysis: None,
        }
    }
    fn analyze(input: &mut ResolvedNativeBuildInput) {
        input.client_analysis = Some(ResolvedClientLoadAnalysis {
            server_build_id: "fixture-server-build".into(),
            nodes: input
                .inventory
                .nodes
                .iter()
                .map(|node| AnalyzedNodeLoads {
                    component: node.component.clone(),
                    universal: node.universal.clone(),
                    server: node.server.clone(),
                    has_server_load: node.server.is_some(),
                    has_universal_load: false,
                })
                .collect(),
        });
    }
    #[test]
    fn kit2_server_entries_and_blocked_client_have_real_paths() {
        let request = input("2.69.2");
        let plan = create_build_plan(&request).unwrap();
        assert_eq!(
            plan.server.entries["internal"],
            "/app/.kit/generated/server/internal.js"
        );
        assert_eq!(
            plan.server.entries["entries/endpoints/+server.ts"],
            "/app/src/routes/+server.ts"
        );
        assert_eq!(
            plan.server.entries["entries/pages/+page.server.ts"],
            "/app/src/routes/+page.server.ts"
        );
        assert_eq!(plan.server.defines["__SVELTEKIT_HAS_SERVER_LOAD__"], "true");
        assert_eq!(plan.client.unresolved_defines.len(), 2);
        assert!(
            !plan
                .generated_files
                .contains_key(Path::new("/app/.kit/generated/client-optimized/app.js"))
        );
        assert!(
            plan.server.virtual_modules["__sveltekit/server"].contains("set_read_implementation")
        );
        assert_eq!(plan.phases[0].kind, BuildPhaseKind::Server);
        assert_eq!(plan.phases[1].kind, BuildPhaseKind::Analysis);
        assert!(!plan.phases[1].skipped);
    }
    #[test]
    fn analyzed_client_flags_are_source_matched_and_optimized() {
        let mut request = input("2.70.3");
        analyze(&mut request);
        let plan = create_build_plan(&request).unwrap();
        assert!(plan.client.unresolved_defines.is_empty());
        assert_eq!(plan.client.defines["__SVELTEKIT_HAS_SERVER_LOAD__"], "true");
        assert_eq!(
            plan.client.defines["__SVELTEKIT_HAS_UNIVERSAL_LOAD__"],
            "false"
        );
        assert!(
            plan.generated_files
                .contains_key(Path::new("/app/.kit/generated/client-optimized/app.js"))
        );
        request.client_analysis.as_mut().unwrap().nodes[2].server = None;
        assert!(
            create_build_plan(&request)
                .unwrap_err()
                .contains("does not match inventory node 2")
        );
    }
    #[test]
    fn kit3_payload_params_manifest_and_workers_remain_versioned() {
        let mut request = input("3.0.1");
        request.inventory.params_module = Some("/app/src/params.ts".into());
        request.inventory.service_worker = Some("/app/src/service-worker.ts".into());
        request.server.service_worker_present = true;
        let plan = create_build_plan(&request).unwrap();
        assert!(!plan.server.entries.contains_key("internal"));
        assert_eq!(plan.server.entries["entries/params"], "/app/src/params.ts");
        assert!(plan.client.entries.contains_key("entry/payload"));
        assert_eq!(plan.client.defines["__SVELTEKIT_PAYLOAD__"], "undefined");
        assert_eq!(plan.client.output.fixed_chunks[0].file, "_app/manifest.js");
        assert_eq!(plan.manifest_placeholders.len(), 4);
        assert!(
            plan.generated_files[Path::new("/app/.kit/generated/build/app-manifest.js")]
                .contains("__SVELTEKIT_MANIFEST_PRERENDERED__")
        );
        assert!(
            plan.service_worker.as_ref().unwrap().entry_preludes[0]
                .contains("env/service-worker.js")
        );
        assert!(plan.server.external.contains(&"@opentelemetry/api".into()));
    }
    #[test]
    fn aliases_distinguish_bare_and_children_and_keep_order() {
        let mut request = input("2.67.0");
        request.config.aliases = vec![
            ResolvedConfigAlias {
                name: "thing".into(),
                path: "/app/exact.ts".into(),
            },
            ResolvedConfigAlias {
                name: "thing/*".into(),
                path: "/app/modules/*".into(),
            },
        ];
        let plan = create_build_plan(&request).unwrap();
        let exact = plan
            .client
            .aliases
            .iter()
            .find(|alias| alias.name == "thing" && alias.matching == AliasMatch::Exact)
            .unwrap();
        let children = plan
            .client
            .aliases
            .iter()
            .find(|alias| alias.name == "thing" && alias.matching == AliasMatch::Children)
            .unwrap();
        assert_eq!(exact.resolve("thing"), Some("/app/exact.ts".into()));
        assert_eq!(exact.resolve("thing/foo"), None);
        assert_eq!(children.resolve("thing"), None);
        assert_eq!(children.resolve("thing/"), None);
        assert_eq!(
            children.resolve("thing/foo"),
            Some("/app/modules/foo".into())
        );
        assert_eq!(children.resolve("things/foo"), None);
    }
    #[test]
    fn collisions_and_decoded_traversal_fail_without_overwriting_entries() {
        let mut request = input("2.70.3");
        request.inventory.nodes[1].component = Some("/other/layout.svelte".into());
        assert!(
            create_build_plan(&request)
                .unwrap_err()
                .contains("Build entry collision")
        );
        let mut entries = BTreeMap::new();
        assert!(
            source_entry(
                &mut entries,
                Path::new("/app/src/routes/%2e%2e/escape.js"),
                Path::new("/app/src/routes"),
                true
            )
            .unwrap_err()
            .contains("Unsafe build entry")
        );
        assert!(decode_uri("bad%ZZ").is_err());
    }
    #[test]
    fn incomplete_instrumentation_and_inconsistent_roots_are_errors() {
        let mut request = input("2.70.3");
        request.config.instrumentation = Some(ResolvedInstrumentation {
            entry: "/app/src/instrumentation.server.ts".into(),
            enabled: false,
            adapter_supports: None,
        });
        assert!(
            create_build_plan(&request)
                .unwrap_err()
                .contains("experimental.instrumentation.server")
        );
        request.config.instrumentation.as_mut().unwrap().enabled = true;
        request.config.adapter_name = Some("adapter".into());
        assert!(
            create_build_plan(&request)
                .unwrap_err()
                .contains("explicitly support")
        );
        request.config.instrumentation = None;
        request.environment.version_name = "different".into();
        assert!(
            create_build_plan(&request)
                .unwrap_err()
                .contains("inputs disagree")
        );
    }
    #[test]
    fn inline_and_kit2_mjs_naming_are_explicit() {
        let mut request = input("2.70.3");
        request.config.bundle_strategy = BundleStrategy::Inline;
        request.server.options.preload_strategy = PreloadStrategy::PreloadMjs;
        let plan = create_build_plan(&request).unwrap();
        assert_eq!(plan.client.entries.len(), 1);
        assert!(plan.client.entries.contains_key("bundle"));
        assert_eq!(plan.client.output.format, "iife");
        assert!(!plan.client.output.code_splitting);
        assert!(!plan.client.output.css_code_splitting);
        assert!(plan.client.output.entries.ends_with(".mjs"));
        assert!(plan.server.output.code_splitting);
    }
}
