//! Pure production service-worker generation for the registered Kit 2/3 builds.
//!
//! Derived from SvelteKit's MIT-licensed build_service_worker.js (Kit 2),
//! service-worker.js and collect_immutable (Kit 3). Copyright Svelte contributors.
//! Inputs are resolved inventories, never filesystem probes or JS callbacks.
//! Kit 3 removed `$service-worker` and serviceWorker.files: its actual replacement
//! is `$app/manifest`, `$app/env` and `$app/paths`, not a compatibility shim.
use crate::build_inventory::BuildInventory;
use crate::build_nodes::BundleStrategy;
use crate::build_plan::{
    AliasMatch, BuildAlias, BuildEntryGraph, BuildGraphTarget, BuildRequirement,
};
use crate::build_server::version_hash;
use crate::sync_versions::KitSyncVersion;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

/// Array order must be Object.values(client_manifest) order, including JS's
/// integer-key ordering. A sorted Rust map is not an equivalent inventory.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClientManifestEntry {
    pub key: String,
    pub file: String,
    #[serde(default)]
    pub css: Vec<String>,
    #[serde(default)]
    pub assets: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum StaticFilesFilter {
    /// Kit 2's exact default: exclude every filename containing `.DS_Store`.
    Default,
    /// Explicit callback results for every static asset, no missing/extra keys.
    Resolved { decisions: BTreeMap<String, bool> },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResolvedServiceWorkerInput {
    pub inventory: BuildInventory,
    /// Service-worker graph from create_build_plan, retaining resolved env
    /// modules, aliases, conditions, private-import guards and bootstrap imports.
    pub graph: BuildEntryGraph,
    pub base: String,
    pub assets_url: String,
    pub app_dir: String,
    pub version_name: String,
    pub bundle_strategy: BundleStrategy,
    pub client_manifest: Vec<ClientManifestEntry>,
    /// Complete final client output inventory, relative to output/client.
    pub emitted_client_files: BTreeSet<String>,
    /// Exact inline entry/style files deleted after extracting inline content.
    /// Kit 2 uses actual existence; Kit 3 uses this precise exclusion set.
    pub inlined_files: BTreeSet<String>,
    /// manifest_data.assets order. Names remain verbatim, as in upstream.
    pub static_assets: Vec<String>,
    /// None selects Kit 2's default; Kit 3 requires None (option was removed).
    pub static_files_filter: Option<StaticFilesFilter>,
    /// Complete prerendered.paths, including base and original URL escaping.
    pub prerendered_paths: Vec<String>,
    /// Kit 3's explicit output.codeSplitting override; Kit 2 requires None.
    pub code_splitting: Option<bool>,
    /// false/true/"esbuild"/"terser", preserved for the native bundling host.
    pub minify: Value,
    /// Only needed by a legacy Vite/Rollup host with esbuild's .js export bug.
    /// Native and Rolldown hosts emit service-worker.js directly.
    pub legacy_mjs_workaround: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GeneratedServiceWorker {
    pub kit_version: String,
    pub version_hash: String,
    pub graph: BuildEntryGraph,
    pub files: BTreeMap<PathBuf, String>,
    /// Kit 2 arrays are relative URL suffixes added to runtime-derived base.
    /// Kit 3 arrays are `{path}` records without base or leading slash.
    pub manifest: Value,
    pub module_preload: bool,
    pub empty_out_dir: bool,
    pub public_dir: bool,
    pub minify: Value,
    pub bundler_manifest: Option<String>,
    pub post_bundle_renames: BTreeMap<String, String>,
    pub removed_modules: Vec<String>,
}

impl GeneratedServiceWorker {
    /// Must run during resolution, including query-suffixed IDs. Import-boundary
    /// graph checks remain mandatory via build_guards::validate_build_imports.
    pub fn check_module_id(&self, id: &str) -> Result<(), String> {
        let bare = id.split('?').next().unwrap_or(id);
        if !self.removed_modules.is_empty()
            && matches!(bare, "$service-worker" | "\0virtual:service-worker")
        {
            return Err(
                "$service-worker was removed in Kit 3; use $app/manifest, $app/env and $app/paths"
                    .into(),
            );
        }
        Ok(())
    }

    /// Matches renderBuiltUrl without baking a configured deployment base into
    /// resource URLs; names are JSON escaped, never percent-encoded a second time.
    pub fn render_built_url(&self, filename: &str) -> Result<String, String> {
        relative(filename)?;
        Ok(format!("new URL({}, location.href).pathname", js(filename)))
    }
}

fn js(value: impl Serialize) -> String {
    serde_json::to_string(&value).expect("service-worker data is JSON serializable")
}
fn relative(value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.starts_with('/')
        || value.contains(['\\', '\0'])
        || value
            .split('/')
            .any(|part| part.is_empty() || matches!(part, "." | ".."))
    {
        return Err(format!(
            "Expected a canonical relative asset path: {value:?}"
        ));
    }
    Ok(())
}
fn absolute(value: &Path) -> Result<(), String> {
    if !value.is_absolute()
        || value
            .components()
            .any(|part| matches!(part, Component::ParentDir | Component::CurDir))
        || value
            .to_str()
            .is_none_or(|path| path.contains(['\\', '\0']))
    {
        return Err("Expected a canonical absolute build path".into());
    }
    Ok(())
}
fn insert_unique(list: &mut Vec<String>, seen: &mut BTreeSet<String>, name: &str) {
    if seen.insert(name.into()) {
        list.push(name.into());
    }
}
fn prefixed_array(name: &str, paths: &[String]) -> String {
    format!(
        "export const {name} = [\n{}\n];\n",
        paths
            .iter()
            .map(|path| format!("  base + {}", js(path)))
            .collect::<Vec<_>>()
            .join(",\n")
    )
}
fn add_alias(graph: &mut BuildEntryGraph, name: &str, replacement: PathBuf) -> Result<(), String> {
    if let Some(previous) = graph.aliases.iter().find(|alias| alias.name == name) {
        if previous.replacement != replacement || previous.matching != AliasMatch::Exact {
            return Err(format!(
                "Service-worker generated alias conflicts with configured {name}"
            ));
        }
    } else {
        graph.aliases.insert(
            0,
            BuildAlias {
                name: name.into(),
                replacement,
                matching: AliasMatch::Exact,
            },
        );
    }
    Ok(())
}

pub fn generate_service_worker(
    input: &ResolvedServiceWorkerInput,
) -> Result<GeneratedServiceWorker, String> {
    let profile = KitSyncVersion::parse(&input.inventory.kit_version)?;
    let kit3 = profile.is_kit3();
    if input.inventory.options.dev {
        return Err("Production service-worker inventory cannot be a dev inventory".into());
    }
    let entry = input
        .inventory
        .service_worker
        .as_ref()
        .ok_or("No resolved service-worker entry")?;
    absolute(entry)?;
    absolute(&input.inventory.generated_directory)?;
    absolute(&input.graph.output.directory)?;
    if input.graph.target != BuildGraphTarget::ServiceWorker
        || input.graph.entries.len() != 1
        || input
            .graph
            .entries
            .get("service-worker")
            .map(String::as_str)
            != entry.to_str()
    {
        return Err("Service-worker graph and inventory entry disagree".into());
    }
    if !input.assets_url.is_empty() {
        return Err("Service workers cannot be built with a separate paths.assets URL".into());
    }
    if !input.base.is_empty() {
        relative(
            input
                .base
                .strip_prefix('/')
                .ok_or("paths.base must begin with /")?,
        )?;
        if input.base.contains(['?', '#']) {
            return Err("paths.base cannot contain query or fragment".into());
        }
    }
    relative(&input.app_dir)?;
    if input.app_dir.contains(['?', '#']) {
        return Err("appDir cannot contain URL query or fragment delimiters".into());
    }
    if input.version_name.contains('\0') {
        return Err("Version cannot contain NUL".into());
    }
    if !input.minify.is_boolean() && !matches!(input.minify.as_str(), Some("esbuild" | "terser")) {
        return Err("Unresolved or unsupported minify option".into());
    }
    if kit3 && (input.static_files_filter.is_some() || input.legacy_mjs_workaround) {
        return Err(
            "Kit 3 removed serviceWorker.files and the legacy .mjs worker workaround".into(),
        );
    }
    if !kit3 && input.code_splitting.is_some() {
        return Err("Kit 2 does not expose service-worker codeSplitting overrides".into());
    }
    if input.bundle_strategy != BundleStrategy::Inline && !input.inlined_files.is_empty() {
        return Err("Inline deletions require inline bundle strategy".into());
    }
    if input.client_manifest.len() > 100_000
        || input.static_assets.len() > 100_000
        || input.prerendered_paths.len() > 100_000
    {
        return Err("Service-worker asset inventory exceeds limits".into());
    }
    for name in input
        .emitted_client_files
        .iter()
        .chain(input.inlined_files.iter())
    {
        relative(name)?;
    }
    let mut all = Vec::new();
    let mut seen = BTreeSet::new();
    let mut keys = BTreeSet::new();
    for record in &input.client_manifest {
        if !keys.insert(&record.key) {
            return Err("Duplicate client manifest key".into());
        }
        for file in std::iter::once(&record.file)
            .chain(record.css.iter())
            .chain(record.assets.iter())
        {
            relative(file)?;
            insert_unique(&mut all, &mut seen, file);
        }
    }
    if !input.inlined_files.is_subset(&seen) {
        return Err("Inline deletion is absent from client manifest".into());
    }
    if !input.inlined_files.is_disjoint(&input.emitted_client_files) {
        return Err("Deleted inline file is still in final output inventory".into());
    }
    let mut build = Vec::new();
    let immutable_prefix = format!("{}/immutable", input.app_dir);
    for file in all {
        let include = if kit3 {
            // Deliberately preserve upstream startsWith(prefix), including its
            // lack of a slash boundary after "immutable".
            file.starts_with(&immutable_prefix) && !input.inlined_files.contains(&file)
        } else {
            input.bundle_strategy != BundleStrategy::Inline
                || input.emitted_client_files.contains(&file)
        };
        if include {
            if !input.emitted_client_files.contains(&file) {
                return Err(format!(
                    "Cache asset missing from final client output: {file}"
                ));
            }
            build.push(file);
        }
    }
    let mut static_names = BTreeSet::new();
    for asset in &input.static_assets {
        relative(asset)?;
        if !static_names.insert(asset.clone()) {
            return Err("Duplicate static asset".into());
        }
    }
    let decisions = match &input.static_files_filter {
        Some(StaticFilesFilter::Resolved { decisions }) => Some(decisions),
        _ => None,
    };
    if let Some(decisions) = decisions {
        if decisions.keys().cloned().collect::<BTreeSet<_>>() != static_names {
            return Err(
                "Static file callback results do not exactly match current asset inventory".into(),
            );
        }
    }
    let assets = input
        .static_assets
        .iter()
        .filter(|name| {
            kit3 || decisions
                .map_or_else(|| !name.contains(".DS_Store"), |decisions| decisions[*name])
        })
        .cloned()
        .collect::<Vec<_>>();
    let mut prerendered = Vec::new();
    for path in &input.prerendered_paths {
        if !path.starts_with('/') || path.contains(['\0', '?', '#', '\\']) {
            return Err("Prerender paths must be absolute URL pathnames".into());
        }
        // Upstream uses String.replace(base, ''). Validated Kit paths put base
        // at the front; require that boundary rather than removing an unrelated
        // equal substring further inside the pathname.
        let suffix = path
            .strip_prefix(&input.base)
            .ok_or("Prerender path is outside configured base")?;
        if !suffix.is_empty() && !suffix.starts_with('/') {
            return Err("Prerender path does not have the configured base boundary".into());
        }
        prerendered.push(if kit3 {
            suffix.strip_prefix('/').unwrap_or(suffix).to_owned()
        } else {
            suffix.to_owned()
        });
    }
    let mut graph = input.graph.clone();
    let version = js(&input.version_name);
    if graph
        .defines
        .get("__SVELTEKIT_APP_VERSION__")
        .is_some_and(|old| old != &version)
    {
        return Err("Service-worker version disagrees with build graph".into());
    }
    graph
        .defines
        .insert("__SVELTEKIT_APP_VERSION__".into(), version);
    let mut files = BTreeMap::new();
    let manifest;
    if kit3 {
        let records = |names: &[String]| {
            names
                .iter()
                .map(|name| json!({"path":name}))
                .collect::<Vec<_>>()
        };
        let routes = input.inventory.routes.iter().filter(|route| route.page.is_some() || route.endpoint.is_some()).map(|route| json!({"id":route.id,"page":route.page.is_some(),"endpoint":route.endpoint.is_some()})).collect::<Vec<_>>();
        manifest = json!({"immutable":records(&build),"assets":records(&assets),"prerendered":records(&prerendered),"routes":routes});
        let mut code = String::new();
        for (name, constant) in [
            ("immutable", "IMMUTABLE"),
            ("assets", "ASSETS"),
            ("prerendered", "PRERENDERED"),
            ("routes", "ROUTES"),
        ] {
            let value = js(&manifest[name]);
            let placeholder = format!("__SVELTEKIT_MANIFEST_{constant}__");
            graph.defines.insert(placeholder.clone(), value.clone());
            graph.unresolved_defines.remove(&placeholder);
            code.push_str(&format!("export const {name} = {value};\n"));
        }
        let generated = input.inventory.generated_directory.join("app-manifest.js");
        files.insert(generated.clone(), code);
        add_alias(
            &mut graph,
            "<sveltekit:generated>/app-manifest.js",
            generated,
        )?;
        let env = format!("{}/{}/env.js", input.base, input.app_dir);
        if !graph.external.contains(&env) {
            graph.external.push(env);
        }
        graph.output.code_splitting = input.code_splitting.unwrap_or(false);
    } else {
        let build = build
            .iter()
            .map(|name| format!("/{name}"))
            .collect::<Vec<_>>();
        let assets = assets
            .iter()
            .map(|name| format!("/{name}"))
            .collect::<Vec<_>>();
        let mut code = "export const base = /*@__PURE__*/ location.pathname.split('/').slice(0, -1).join('/');\n\n".to_owned();
        code.push_str(&prefixed_array("build", &build));
        code.push_str(&prefixed_array("files", &assets));
        code.push_str(&prefixed_array("prerendered", &prerendered));
        code.push_str(&format!(
            "export const version = {};\n",
            js(&input.version_name)
        ));
        let generated = input
            .inventory
            .generated_directory
            .join("service-worker.js");
        files.insert(generated.clone(), code.clone());
        graph
            .virtual_modules
            .insert("\0virtual:service-worker".into(), code);
        add_alias(&mut graph, "$service-worker", generated)?;
        graph.output.code_splitting = false;
        manifest = json!({"build":build,"files":assets,"prerendered":prerendered,"version":input.version_name});
    }
    graph.output.format = "esm".into();
    graph.output.entries = if input.legacy_mjs_workaround {
        "service-worker.mjs"
    } else {
        "service-worker.js"
    }
    .into();
    graph.output.assets = format!("{}/immutable/assets/[name].[hash][extname]", input.app_dir);
    graph.requires.retain(|requirement| {
        !matches!(
            requirement,
            BuildRequirement::ServiceWorkerAssetInventory | BuildRequirement::PrerenderResults
        )
    });
    Ok(GeneratedServiceWorker {
        kit_version: input.inventory.kit_version.clone(),
        version_hash: version_hash(&input.version_name),
        graph,
        files,
        manifest,
        module_preload: false,
        empty_out_dir: false,
        public_dir: false,
        minify: input.minify.clone(),
        bundler_manifest: kit3.then(|| ".vite/service-worker-manifest.json".into()),
        post_bundle_renames: if input.legacy_mjs_workaround {
            BTreeMap::from([("service-worker.mjs".into(), "service-worker.js".into())])
        } else {
            BTreeMap::new()
        },
        removed_modules: if kit3 {
            vec!["$service-worker".into()]
        } else {
            Vec::new()
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn input(version: &str) -> ResolvedServiceWorkerInput {
        let inventory = serde_json::from_value(json!({"kitVersion":version,"projectRoot":"/app","runtimeDirectory":"/app/node_modules/@sveltejs/kit/src/runtime",
            "generatedDirectory":"/app/.svelte-kit/generated","options":{"svelteMajor":5,"router":{"type":"pathname","resolution":"client"},"handleRenderingErrors":false,"asyncCompiler":false,"dev":false},
            "nodes":[],"routes":[],"hooks":{},"matchers":{},"paramsModule":null,"serviceWorker":"/app/src/service-worker.ts","inputs":[],"pageOptionsAnalyzed":false})).unwrap();
        let graph = serde_json::from_value(json!({"target":"serviceWorker","entries":{"service-worker":"/app/src/service-worker.ts"},"aliases":[],"virtualModules":{},"defines":{},"unresolvedDefines":{},
            "conditions":["svelte","production","browser"],"external":[],"mustBundle":["esm-env"],"javascriptTarget":null,
            "output":{"directory":"/app/.svelte-kit/output/client","format":"esm","globalName":null,"entries":"service-worker.js","chunks":"_app/immutable/chunks/[hash].js","assets":"_app/immutable/assets/[name].[hash][extname]","codeSplitting":false,"cssCodeSplitting":true,"preserveEntrySignatures":"strict","hoistTransitiveImports":null,"fixedChunks":[]},
            "requires":["completeImportBoundaryChecks","serviceWorkerAssetInventory","prerenderResults"],"entryPreludes":["import '/env-bootstrap.js';"]})).unwrap();
        ResolvedServiceWorkerInput {
            inventory,
            graph,
            base: "/hr".into(),
            assets_url: String::new(),
            app_dir: "_app".into(),
            version_name: "v😀".into(),
            bundle_strategy: BundleStrategy::Split,
            client_manifest: vec![
                ClientManifestEntry {
                    key: "entry".into(),
                    file: "_app/immutable/main.js".into(),
                    css: vec!["_app/immutable/style.css".into()],
                    assets: vec!["image.png".into()],
                },
                ClientManifestEntry {
                    key: "shared".into(),
                    file: "_app/immutable/main.js".into(),
                    css: vec![],
                    assets: vec![],
                },
            ],
            emitted_client_files: BTreeSet::from([
                "_app/immutable/main.js".into(),
                "_app/immutable/style.css".into(),
                "image.png".into(),
            ]),
            inlined_files: BTreeSet::new(),
            static_assets: vec![
                "a space/😀%20.txt".into(),
                ".DS_Store".into(),
                "x.DS_Store.backup".into(),
            ],
            static_files_filter: None,
            prerendered_paths: vec!["/hr/".into(), "/hr/a%20b".into()],
            code_splitting: None,
            minify: json!(false),
            legacy_mjs_workaround: false,
        }
    }
    #[test]
    fn kit2_preserves_order_encoding_runtime_base_and_default_filter() {
        for version in ["2.67.0", "2.68.0", "2.69.2", "2.69.3", "2.70.3"] {
            let output = generate_service_worker(&input(version)).unwrap();
            assert_eq!(
                output.manifest["build"],
                json!([
                    "/_app/immutable/main.js",
                    "/_app/immutable/style.css",
                    "/image.png"
                ])
            );
            assert_eq!(output.manifest["files"], json!(["/a space/😀%20.txt"]));
            assert_eq!(output.manifest["prerendered"], json!(["/", "/a%20b"]));
            let code =
                &output.files[&PathBuf::from("/app/.svelte-kit/generated/service-worker.js")];
            assert!(code.contains("location.pathname.split('/')"));
            assert!(code.contains("base + \"/a space/😀%20.txt\""));
            assert!(!code.contains("/hr/"));
            assert_eq!(output.version_hash, version_hash("v😀"));
            assert!(
                output
                    .graph
                    .requires
                    .contains(&BuildRequirement::CompleteImportBoundaryChecks)
            );
            assert_eq!(
                output.graph.entry_preludes,
                vec!["import '/env-bootstrap.js';"]
            );
        }
    }
    #[test]
    fn kit3_uses_real_manifest_contract_and_rejects_removed_module_and_filter() {
        for version in ["3.0.1", "3.0.0-next.32"] {
            let mut input = input(version);
            input.code_splitting = Some(true);
            let output = generate_service_worker(&input).unwrap();
            assert_eq!(
                output.manifest["immutable"],
                json!([{"path":"_app/immutable/main.js"},{"path":"_app/immutable/style.css"}])
            );
            assert_eq!(output.manifest["assets"].as_array().unwrap().len(), 3);
            assert_eq!(
                output.manifest["prerendered"],
                json!([{"path":""},{"path":"a%20b"}])
            );
            assert!(output.check_module_id("$service-worker?raw").is_err());
            assert!(output.check_module_id("$app/manifest").is_ok());
            assert!(output.graph.external.contains(&"/hr/_app/env.js".into()));
            assert_eq!(
                output.bundler_manifest.as_deref(),
                Some(".vite/service-worker-manifest.json")
            );
            assert!(output.graph.output.code_splitting);
            assert!(
                output
                    .graph
                    .defines
                    .contains_key("__SVELTEKIT_MANIFEST_ROUTES__")
            );
            input.static_files_filter = Some(StaticFilesFilter::Default);
            assert!(generate_service_worker(&input).is_err());
        }
    }
    #[test]
    fn inline_exclusions_use_version_specific_source_of_truth() {
        let mut legacy = input("2.69.2");
        legacy.bundle_strategy = BundleStrategy::Inline;
        legacy.emitted_client_files.remove("_app/immutable/main.js");
        assert_eq!(
            generate_service_worker(&legacy).unwrap().manifest["build"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        legacy.inventory.kit_version = "3.0.1".into();
        assert!(generate_service_worker(&legacy).is_err());
        legacy.inlined_files.insert("_app/immutable/main.js".into());
        assert_eq!(
            generate_service_worker(&legacy).unwrap().manifest["immutable"],
            json!([{"path":"_app/immutable/style.css"}])
        );
    }
    #[test]
    fn resolved_callbacks_are_complete_and_never_executed() {
        let mut input = input("2.70.3");
        input.static_files_filter = Some(StaticFilesFilter::Resolved {
            decisions: BTreeMap::from([(".DS_Store".into(), true)]),
        });
        assert!(generate_service_worker(&input).is_err());
        input.static_files_filter = Some(StaticFilesFilter::Resolved {
            decisions: input
                .static_assets
                .iter()
                .map(|name| (name.clone(), name == ".DS_Store"))
                .collect(),
        });
        assert_eq!(
            generate_service_worker(&input).unwrap().manifest["files"],
            json!(["/.DS_Store"])
        );
    }
    #[test]
    fn paths_graph_identity_and_legacy_output_rules_are_explicit() {
        let mut input = input("2.69.3");
        input.legacy_mjs_workaround = true;
        let output = generate_service_worker(&input).unwrap();
        assert_eq!(output.graph.output.entries, "service-worker.mjs");
        assert_eq!(
            output.post_bundle_renames["service-worker.mjs"],
            "service-worker.js"
        );
        assert_eq!(
            output.render_built_url("x%20y/😀.js").unwrap(),
            "new URL(\"x%20y/😀.js\", location.href).pathname"
        );
        input.prerendered_paths = vec!["/hr-other/page".into()];
        assert!(generate_service_worker(&input).is_err());
        input.prerendered_paths.clear();
        input.assets_url = "https://cdn.example".into();
        assert!(generate_service_worker(&input).is_err());
        input.assets_url.clear();
        input.client_manifest[0].file = "../escape.js".into();
        assert!(generate_service_worker(&input).is_err());
    }
}
