//! Native production worker phase. The ordered cache inventory is validated
//! against actual client output before generating or bundling the worker.
use crate::*;
use serde::{Deserialize, Serialize};
use ts_svelte_kit::build_guards::{BuildGuardReport, GuardStatus, GuardTarget};
use ts_svelte_kit::build_service_worker::{
    self, ClientManifestEntry, GeneratedServiceWorker, ResolvedServiceWorkerInput,
    StaticFilesFilter,
};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ServiceWorkerRequest {
    /// Object.values(manifest) order from the resolved native client manifest.
    /// Every record is checked against actual chunk/asset metadata below.
    pub client_manifest: Vec<ClientManifestEntry>,
    /// manifest_data.assets order, with exactly the BuildRequest inventory keys.
    pub static_assets: Vec<String>,
    pub static_files_filter: Option<StaticFilesFilter>,
    pub prerendered_paths: Vec<String>,
    #[serde(default)]
    pub inlined_files: BTreeSet<String>,
    pub code_splitting: Option<bool>,
}

/// User-resolved options independent of runtime-dependent output filenames.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct ServiceWorkerOptions {
    pub static_files_filter: Option<StaticFilesFilter>,
    /// None uses the native BuildRequest inventory's deterministic key order.
    /// A host retaining manifest_data.assets order can supply that exact order.
    pub static_asset_order: Option<Vec<String>>,
    pub code_splitting: Option<bool>,
}

fn array_index(key: &str) -> Option<u32> {
    key.parse::<u32>()
        .ok()
        .filter(|index| *index < u32::MAX && index.to_string() == key)
}

/// Native manifest records follow chunk emission order, then asset emission
/// order. Source-map assets are excluded. This retains native output ordering;
/// it does not pretend that Rolldown and Vite emit their graphs in equal order.
pub fn derive_manifest(
    build: &BuildRequest,
    client: &BundleResult,
) -> Result<Vec<ClientManifestEntry>, String> {
    if client.target != ts_svelte_bundle::Target::Client {
        return Err("Worker manifest requires a client bundle".into());
    }
    let graph = graph::dependencies(build, client)?;
    let maps = client
        .chunks
        .iter()
        .filter_map(|chunk| chunk.sourcemap_file_name.as_deref())
        .chain(
            client
                .css
                .modules
                .values()
                .filter_map(|module| module.map_file_name.as_deref()),
        )
        .collect::<BTreeSet<_>>();
    let mut keys = BTreeSet::new();
    let mut output_files = BTreeSet::new();
    let mut output = Vec::new();
    for chunk in &client.chunks {
        if !output_files.insert(&chunk.file_name) {
            return Err("Duplicate client chunk output".into());
        }
        let id = chunk
            .facade_module_id
            .as_deref()
            .unwrap_or(&chunk.file_name);
        // Real facade/file identities are retained; a numeric filename receives
        // an explicit namespace rather than being re-ordered as an Object index.
        let key = if array_index(id).is_some() {
            format!("chunk:{id}")
        } else {
            id.into()
        };
        if !keys.insert(key.clone()) {
            return Err(format!("Duplicate client manifest identity {key}"));
        }
        let record = graph
            .records
            .get(id)
            .ok_or_else(|| format!("Missing emitted dependency record {id}"))?;
        output.push(ClientManifestEntry {
            key,
            file: chunk.file_name.clone(),
            css: record.css.clone().unwrap_or_default(),
            assets: record.assets.clone().unwrap_or_default(),
        });
    }
    for asset in &client.assets {
        if !output_files.insert(&asset.file_name) {
            return Err("Duplicate client asset/chunk output".into());
        }
        if maps.contains(asset.file_name.as_str()) {
            continue;
        }
        let key = if array_index(&asset.file_name).is_some() || keys.contains(&asset.file_name) {
            format!("asset:{}", asset.file_name)
        } else {
            asset.file_name.clone()
        };
        if !keys.insert(key.clone()) {
            return Err(format!("Duplicate client asset identity {key}"));
        }
        output.push(ClientManifestEntry {
            key,
            file: asset.file_name.clone(),
            css: vec![],
            assets: vec![],
        });
    }
    Ok(output)
}

pub fn derive_request(
    build: &BuildRequest,
    client: &BundleResult,
    options: &ServiceWorkerOptions,
    prerendered_paths: &[String],
) -> Result<ServiceWorkerRequest, String> {
    let static_assets = options
        .static_asset_order
        .clone()
        .unwrap_or_else(|| build.static_assets.keys().cloned().collect::<Vec<_>>());
    if static_assets.len() != build.static_assets.len()
        || static_assets.iter().cloned().collect::<BTreeSet<_>>()
            != build.static_assets.keys().cloned().collect::<BTreeSet<_>>()
    {
        return Err(
            "Worker static asset order does not exactly match the resolved inventory".into(),
        );
    }
    Ok(ServiceWorkerRequest {
        client_manifest: derive_manifest(build, client)?,
        static_assets,
        static_files_filter: options.static_files_filter.clone(),
        prerendered_paths: prerendered_paths.to_vec(),
        inlined_files: BTreeSet::new(),
        code_splitting: options.code_splitting,
    })
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceWorkerOutput {
    /// Guard failures/incomplete external classifications retain their reports.
    pub status: String,
    pub generated: GeneratedServiceWorker,
    pub bundle: BundleResult,
    pub guards: BuildGuardReport,
    pub tracked_features: BTreeMap<String, Vec<String>>,
    /// Only new output/client files. Existing identical artifacts are omitted;
    /// an attempt to change an earlier client artifact fails atomically.
    pub artifacts: BTreeMap<String, Artifact>,
}

fn client_inventory(
    build: &BuildRequest,
    request: &ServiceWorkerRequest,
    client: &BundleResult,
    artifacts: &BTreeMap<String, Artifact>,
) -> Result<BTreeSet<String>, String> {
    if client.target != ts_svelte_bundle::Target::Client {
        return Err("Worker inventory requires the final client bundle".into());
    }
    if !build.static_assets_complete {
        return Err("Worker requires a complete static asset inventory".into());
    }
    if request
        .static_assets
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>()
        != build.static_assets.keys().cloned().collect::<BTreeSet<_>>()
        || request.static_assets.len() != build.static_assets.len()
    {
        return Err(
            "Worker static asset order does not match the resolved static inventory".into(),
        );
    }
    if !request.inlined_files.is_empty() {
        return Err(
            "Native inline client extraction must complete before worker inventory can be supplied"
                .into(),
        );
    }
    let files = artifacts
        .keys()
        .filter_map(|file| file.strip_prefix("client/").map(str::to_owned))
        .collect::<BTreeSet<_>>();
    let verify = |file: &str, expected: &[u8]| -> Result<(), String> {
        let actual = artifacts
            .get(&format!("client/{file}"))
            .ok_or_else(|| format!("Missing final client artifact {file}"))?;
        if artifacts::bytes(actual)? != expected {
            return Err(format!("Stale final client artifact {file}"));
        }
        Ok(())
    };
    for chunk in &client.chunks {
        verify(&chunk.file_name, chunk.code.as_bytes())?;
    }
    for asset in &client.assets {
        verify(&asset.file_name, &asset_bytes(asset)?)?;
    }
    for (name, asset) in &build.static_assets {
        let source = base64::engine::general_purpose::STANDARD
            .decode(&asset.source)
            .map_err(|e| e.to_string())?;
        verify(name, &source)?;
    }
    let graph = graph::dependencies(build, client)?;
    let mut covered = BTreeSet::new();
    let mut keys = BTreeSet::new();
    let mut non_index_seen = false;
    let mut last_index = None;
    for record in &request.client_manifest {
        if !keys.insert(&record.key) {
            return Err("Duplicate worker client manifest key".into());
        }
        // JavaScript Object.values enumerates canonical array-index keys first.
        let index = array_index(&record.key);
        if let Some(index) = index {
            if non_index_seen || last_index.is_some_and(|previous| previous >= index) {
                return Err("Worker client manifest is not in JavaScript property order".into());
            }
            last_index = Some(index);
        } else {
            non_index_seen = true;
        }
        if let Some(chunk) = client
            .chunks
            .iter()
            .find(|chunk| chunk.file_name == record.file)
        {
            let emitted = graph
                .records
                .values()
                .find(|entry| entry.file == chunk.file_name)
                .ok_or("Missing client dependency record")?;
            if record.css != emitted.css.clone().unwrap_or_default()
                || record.assets != emitted.assets.clone().unwrap_or_default()
            {
                return Err(format!(
                    "Worker cache dependencies differ from emitted chunk {}",
                    record.file
                ));
            }
            covered.insert(chunk.file_name.clone());
        } else if client
            .assets
            .iter()
            .any(|asset| asset.file_name == record.file)
        {
            if !record.css.is_empty() || !record.assets.is_empty() {
                return Err("Asset manifest entry cannot invent nested dependencies".into());
            }
            if client
                .chunks
                .iter()
                .any(|chunk| chunk.sourcemap_file_name.as_deref() == Some(record.file.as_str()))
                || client
                    .css
                    .modules
                    .values()
                    .any(|module| module.map_file_name.as_deref() == Some(record.file.as_str()))
            {
                return Err("Source maps are not client cache manifest records".into());
            }
        } else {
            return Err(format!(
                "Worker manifest record has no emitted output {}",
                record.file
            ));
        }
    }
    if covered
        != client
            .chunks
            .iter()
            .map(|chunk| chunk.file_name.clone())
            .collect::<BTreeSet<_>>()
    {
        return Err("Worker cache manifest omits emitted client chunks".into());
    }
    Ok(files)
}

/// Build one resolved worker after client, prerender and static asset phases.
/// This executes Rust transformations only; it never runs the worker source.
pub async fn build(
    build: &BuildRequest,
    plan: &NativeBuildPlan,
    request: &ServiceWorkerRequest,
    client: &BundleResult,
    artifacts: &BTreeMap<String, Artifact>,
) -> Result<ServiceWorkerOutput, String> {
    let emitted_client_files = client_inventory(build, request, client, artifacts)?;
    let graph = plan
        .service_worker
        .as_ref()
        .ok_or("No planned service-worker entry")?;
    let generated = build_service_worker::generate_service_worker(&ResolvedServiceWorkerInput {
        inventory: build.kit.inventory.clone(),
        graph: graph.clone(),
        base: build.kit.environment.base.clone(),
        assets_url: build.kit.config.assets_url.clone(),
        app_dir: build.kit.environment.app_dir.clone(),
        version_name: build.kit.environment.version_name.clone(),
        bundle_strategy: build.kit.config.bundle_strategy,
        client_manifest: request.client_manifest.clone(),
        emitted_client_files,
        inlined_files: request.inlined_files.clone(),
        static_assets: request.static_assets.clone(),
        static_files_filter: request.static_files_filter.clone(),
        prerendered_paths: request.prerendered_paths.clone(),
        code_splitting: request.code_splitting,
        minify: json!(build.compiler.minify),
        legacy_mjs_workaround: false,
    })?;
    let mut prepared = graph::worker_bundle_request(build, plan, &generated)?;
    // Both registered Kit generations evaluate URLs against the worker itself,
    // including references inside split chunks. Let the native asset plugin
    // render this during AST generation so its source maps stay coherent.
    prepared.output.asset_url_mode = ts_svelte_bundle::output::AssetUrlMode::ServiceWorkerLocation;
    for id in prepared
        .external
        .iter()
        .chain(prepared.aliases.iter().map(|alias| &alias.find))
    {
        generated.check_module_id(id)?;
    }
    let mut bundle = ts_svelte_bundle::generate(prepared.clone())
        .await
        .map_err(|error| error.message)?;
    for id in bundle.module_graph.keys() {
        generated.check_module_id(id)?;
    }
    // The ordinary CSS plugin installs browser document/link behavior. It is
    // not a worker stylesheet transform; don't accidentally accept that runtime
    // merely because native JS asset imports are now supported.
    if !bundle.css.modules.is_empty() {
        return Err("Worker stylesheet imports require a worker-specific CSS transform; the native browser CSS runtime is not valid in a service worker".into());
    }
    if !generated.graph.output.code_splitting && bundle.chunks.len() != 1 {
        return Err(
            "Worker output must inline dynamic imports into a single JavaScript chunk".into(),
        );
    }
    if !bundle
        .chunks
        .iter()
        .any(|chunk| chunk.file_name == "service-worker.js" && chunk.is_entry)
    {
        return Err("Native worker output is missing service-worker.js".into());
    }
    let guards =
        guards::validate_for_target(&bundle, &prepared, plan, build, GuardTarget::ServiceWorker)?;
    let status =
        if [guards.imports_status, guards.configuration_status].contains(&GuardStatus::Failed) {
            "failed"
        } else if guards.imports_status == GuardStatus::Passed
            && guards.configuration_status == GuardStatus::Passed
        {
            "complete"
        } else {
            "incomplete"
        };
    let mut report = BuildReport::new();
    tracking::collect(
        &mut report,
        &mut bundle,
        &build.kit.inventory.kit_version,
        "serviceWorker",
    )?;
    let tracked_features = report
        .tracked_features
        .remove("serviceWorker")
        .ok_or("Missing worker tracking result")?;
    let chunk_assets = imported_assets(&bundle)?;
    let mut outputs = BTreeMap::new();
    for chunk in &bundle.chunks {
        artifacts::insert(
            &mut outputs,
            format!("client/{}", chunk.file_name),
            chunk.code.as_bytes(),
            "service-worker",
        )?;
    }
    for asset in &bundle.assets {
        let is_map = bundle
            .chunks
            .iter()
            .any(|chunk| chunk.sourcemap_file_name.as_deref() == Some(asset.file_name.as_str()));
        artifacts::insert(
            &mut outputs,
            format!("client/{}", asset.file_name),
            &asset_bytes(asset)?,
            if is_map {
                "source-map"
            } else {
                "service-worker-asset"
            },
        )?;
    }
    if let Some(path) = &generated.bundler_manifest {
        let options = worker_manifest::WorkerManifestOptions {
            external_imports: prepared.external.iter().cloned().collect(),
            chunk_assets,
            ..Default::default()
        };
        let manifest =
            worker_manifest::generate_worker_manifest(&prepared.root, &bundle, &options)?;
        if path != &manifest.file_name {
            return Err("Worker manifest output path differs from the native Kit plan".into());
        }
        artifacts::insert(
            &mut outputs,
            format!("client/{}", manifest.file_name),
            manifest.json.as_bytes(),
            "service-worker-manifest",
        )?;
    }
    outputs.retain(|file, artifact| {
        !artifacts
            .get(file)
            .is_some_and(|old| old.encoding == artifact.encoding && old.source == artifact.source)
    });
    if let Some(file) = outputs.keys().find(|file| artifacts.contains_key(*file)) {
        return Err(format!(
            "Worker output conflicts with existing client artifact {file}"
        ));
    }
    Ok(ServiceWorkerOutput {
        status: status.into(),
        generated,
        bundle,
        guards,
        tracked_features,
        artifacts: outputs,
    })
}

/// Actual retained native file references, not guessed associations from asset
/// provenance. Preserve module/reference order and reject dangling metadata.
fn imported_assets(bundle: &BundleResult) -> Result<BTreeMap<String, Vec<String>>, String> {
    let emitted = bundle
        .assets
        .iter()
        .map(|asset| asset.file_name.as_str())
        .collect::<BTreeSet<_>>();
    let maps = bundle
        .chunks
        .iter()
        .filter_map(|chunk| chunk.sourcemap_file_name.as_deref())
        .collect::<BTreeSet<_>>();
    let mut result = BTreeMap::new();
    for chunk in &bundle.chunks {
        let mut seen = BTreeSet::new();
        let mut files = Vec::new();
        for file in &chunk.referenced_files {
            artifacts::relative(file)?;
            if !emitted.contains(file.as_str()) {
                return Err(format!(
                    "Worker chunk {} refers to missing emitted asset {file}",
                    chunk.file_name
                ));
            }
            if !maps.contains(file.as_str()) && seen.insert(file.clone()) {
                files.push(file.clone());
            }
        }
        if !files.is_empty() {
            result.insert(chunk.file_name.clone(), files);
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture(
        source: &str,
    ) -> (
        BuildRequest,
        NativeBuildPlan,
        ServiceWorkerRequest,
        BundleResult,
    ) {
        fixture_at_root(source, std::env::current_dir().unwrap())
    }
    pub(super) fn fixture_at_root(
        source: &str,
        root: PathBuf,
    ) -> (
        BuildRequest,
        NativeBuildPlan,
        ServiceWorkerRequest,
        BundleResult,
    ) {
        let generated = root.join(".native-worker-tests/generated");
        let runtime = root.join("node_modules/@sveltejs/kit/src/runtime");
        let worker = root.join("src/native-worker-fixture.ts");
        let mut build:BuildRequest=serde_json::from_value(json!({
            "kit":{
                "inventory":{"kitVersion":"2.69.2","projectRoot":root,"runtimeDirectory":runtime,"generatedDirectory":generated,
                    "options":{"svelteMajor":5,"router":{"type":"pathname","resolution":"client"},"handleRenderingErrors":false,"asyncCompiler":false,"dev":false},
                    "nodes":[
                        {"kind":"layout","routeId":"/","depth":0,"component":runtime.join("components/svelte-5/layout.svelte"),"universal":null,"server":null,"parent":null,"childPages":[]},
                        {"kind":"error","routeId":"/","depth":0,"component":runtime.join("components/svelte-5/error.svelte"),"universal":null,"server":null,"parent":null,"childPages":[]}
                    ],"routes":[],"hooks":{},"matchers":{},"paramsModule":null,"serviceWorker":worker,"inputs":[],"pageOptionsAnalyzed":false},
                "server":{"kitVersion":"2.69.2","output":generated,"appTemplate":"%sveltekit.head% %sveltekit.body%","errorTemplate":"%sveltekit.status% %sveltekit.error.message%","versionName":"worker-test","runtimeDirectory":runtime,"svelteMajor":5,"serviceWorkerPresent":true,"options":{}},
                "environment":{"kitVersion":"2.69.2","environmentDirectory":generated.join("env"),"runtimeDirectory":runtime,"versionName":"worker-test","base":"","appDir":"_app","buildValues":{},"publicPrefix":"PUBLIC_","privatePrefix":"","explicitEnvironmentVariables":false,"definition":null},
                "config":{"outDir":root.join(".native-worker-tests"),"routesDirectory":root.join("src/routes"),"libDirectory":root.join("src/lib"),"assetsDirectory":root.join("static"),"aliases":[],"bundleStrategy":"split","mode":"production","assetsUrl":"","pathsRelative":true,"pathsOrigin":null,"forkPreloads":false,"tracingServer":false,"linkHeaderPreload":false,"versionPollInterval":0,"adapterName":null,"instrumentation":null,"remoteFunctionsEnabled":false,"remoteModules":[],"serverConditions":["svelte","node","production"],"clientConditions":["svelte","browser","production"],"serverExternals":[]},
                "clientAnalysis":null},
            "compiler":{"svelteVersion":"5.53.11","typescriptVersion":null,"allowPrerelease":false,"runes":true,"minify":false,"sourcemap":true,"options":{},"tsconfig":null,"mdsvex":null,"typescript":{}},
            "moduleExtensions":[".js",".ts"],"modules":{},"sources":{},"staticAssets":{},"staticAssetsComplete":true
        })).unwrap();
        build.sources.insert(worker, source.into());
        let plan = build_plan::create_build_plan(&build.kit).unwrap();
        let request = ServiceWorkerRequest {
            client_manifest: vec![],
            static_assets: vec![],
            static_files_filter: None,
            prerendered_paths: vec![],
            inlined_files: BTreeSet::new(),
            code_splitting: None,
        };
        let client = BundleResult {
            rolldown_version: "1.2.13",
            target: ts_svelte_bundle::Target::Client,
            css_mode: "external",
            css: Default::default(),
            module_graph: BTreeMap::new(),
            chunks: vec![],
            assets: vec![],
            diagnostics: vec![],
            compiler_responses: BTreeMap::new(),
        };
        (build, plan, request, client)
    }
    #[tokio::test]
    async fn kit3_worker_emits_native_bundler_manifest_with_actual_output() {
        let (mut input, _, mut request, client) = fixture("globalThis.workerReady=true;");
        input.kit.inventory.kit_version = "3.0.1".into();
        input.kit.server.kit_version = "3.0.1".into();
        input.kit.environment.kit_version = "3.0.1".into();
        input.compiler.svelte_version = "5.57.2".into();
        let generated = input.kit.config.out_dir.join("generated/build");
        input.kit.inventory.generated_directory = generated.clone();
        input.kit.server.output = generated.clone();
        input.kit.environment.environment_directory = generated.join("env");
        request.code_splitting = Some(false);
        let plan = build_plan::create_build_plan(&input.kit).unwrap();
        let output = build(&input, &plan, &request, &client, &BTreeMap::new())
            .await
            .unwrap();
        assert_eq!(output.status, "complete");
        let artifact = &output.artifacts["client/.vite/service-worker-manifest.json"];
        let manifest: Value = serde_json::from_slice(
            &base64::engine::general_purpose::STANDARD
                .decode(&artifact.source)
                .unwrap(),
        )
        .unwrap();
        let entry = manifest
            .as_object()
            .unwrap()
            .values()
            .find(|entry| entry["isEntry"] == true)
            .unwrap();
        assert_eq!(entry["file"], "service-worker.js");
        assert!(output.artifacts.contains_key("client/service-worker.js"));
        assert!(!output.artifacts.contains_key("client/.vite/manifest.json"));
    }

    #[tokio::test]
    async fn native_worker_inlines_dynamic_imports_and_preserves_tracking_maps() {
        let (mut input, plan, request, client) = fixture(
            "import {build,version} from '$service-worker'; globalThis.workerCache={build,version}; __SVELTEKIT_TRACK__('fixture:worker'); import('./native-worker-lazy.js').then(value=>globalThis.lazy=value.answer);",
        );
        input.sources.insert(
            input
                .kit
                .inventory
                .project_root
                .join("src/native-worker-lazy.js"),
            "export const answer=42;".into(),
        );
        let output = build(&input, &plan, &request, &client, &BTreeMap::new())
            .await
            .unwrap();
        assert_eq!(output.status, "complete");
        assert_eq!(output.bundle.chunks.len(), 1);
        assert!(output.artifacts.contains_key("client/service-worker.js"));
        assert!(
            output
                .artifacts
                .contains_key("client/service-worker.js.map")
        );
        assert_eq!(
            output.tracked_features["service-worker.js"],
            ["fixture:worker"]
        );
        assert!(!output.bundle.chunks[0].code.contains("__SVELTEKIT_TRACK__"));
        assert_eq!(output.bundle.chunks[0].map.as_ref().unwrap()["version"], 3);
    }
    #[tokio::test]
    async fn worker_guard_retains_dead_server_import_and_detailed_failure() {
        let (mut input, plan, request, client) =
            fixture("import './native-worker-secret.server.js'; globalThis.workerReady=true;");
        input.sources.insert(
            input
                .kit
                .inventory
                .project_root
                .join("src/native-worker-secret.server.js"),
            "export const secret='unused';".into(),
        );
        let output = build(&input, &plan, &request, &client, &BTreeMap::new())
            .await
            .unwrap();
        assert_eq!(output.status, "failed");
        assert_eq!(output.guards.imports_status, GuardStatus::Failed);
        assert!(!output.guards.import_diagnostics[0].chain.is_empty());
    }
    #[tokio::test]
    async fn worker_bootstrap_facade_preserves_evaluation_order_and_conflicts_are_atomic() {
        let (mut input, mut plan, request, client) =
            fixture("globalThis.workerOrder.push('entry');");
        let bootstrap = input
            .kit
            .inventory
            .project_root
            .join("src/native-worker-bootstrap.js");
        input.sources.insert(
            bootstrap.clone(),
            "globalThis.workerOrder=['bootstrap'];".into(),
        );
        plan.service_worker
            .as_mut()
            .unwrap()
            .entry_preludes
            .push(format!("import {};", json!(bootstrap)));
        let output = build(&input, &plan, &request, &client, &BTreeMap::new())
            .await
            .unwrap();
        let code = &output.bundle.chunks[0].code;
        assert!(code.find("bootstrap").unwrap() < code.find("entry").unwrap());
        assert_eq!(output.status, "complete");
        let mut previous = BTreeMap::new();
        artifacts::insert(
            &mut previous,
            "client/service-worker.js".into(),
            b"different",
            "service-worker",
        )
        .unwrap();
        assert!(
            build(&input, &plan, &request, &client, &previous)
                .await
                .unwrap_err()
                .contains("conflicts")
        );
        assert_eq!(
            artifacts::bytes(&previous["client/service-worker.js"]).unwrap(),
            b"different"
        );
    }
    #[test]
    fn derived_manifest_keeps_emission_order_and_css_assets_without_maps() {
        let (mut input, _, _, mut client) = fixture("export {};");
        for (name, facade) in [
            ("_app/immutable/z.js", Some("/project/z.svelte")),
            ("_app/immutable/a.js", None),
        ] {
            client.chunks.push(ts_svelte_bundle::Chunk {
                file_name: name.into(),
                name: name.into(),
                code: "export {};".into(),
                is_entry: facade.is_some(),
                is_dynamic_entry: false,
                facade_module_id: facade.map(str::to_owned),
                module_ids: facade.into_iter().map(str::to_owned).collect(),
                imports: vec![],
                dynamic_imports: vec![],
                referenced_files: vec![],
                exports: vec![],
                map: None,
                sourcemap_file_name: Some(format!("{name}.map")),
            });
        }
        client.css.chunks.insert(
            "_app/immutable/z.js".into(),
            ts_svelte_bundle::css::CssChunkOutput {
                own: vec!["_app/immutable/z.css".into()],
                ..Default::default()
            },
        );
        client.css.modules.insert(
            "style".into(),
            ts_svelte_bundle::css::CssModuleOutput {
                tailwind: None,
                module_id: "style".into(),
                origin: "/project/z.svelte".into(),
                file_name: "_app/immutable/z.css".into(),
                map_file_name: Some("_app/immutable/z.css.map".into()),
                sources: vec![],
                assets: vec!["_app/immutable/font.woff2".into()],
                exports: json!({}),
                dependencies: vec![],
            },
        );
        for (file, origin) in [
            ("_app/immutable/z.css", Some("/project/z.svelte")),
            ("_app/immutable/z.js.map", Some("/project/z.svelte")),
            ("_app/immutable/z.css.map", Some("/project/z.svelte")),
            ("_app/immutable/font.woff2", Some("/project/font.woff2")),
        ] {
            client.assets.push(ts_svelte_bundle::Asset {
                file_name: file.into(),
                names: vec![],
                original_file_names: origin.into_iter().map(str::to_owned).collect(),
                encoding: "base64",
                source: base64::engine::general_purpose::STANDARD.encode(b"fixture"),
            });
        }
        let records = derive_manifest(&input, &client).unwrap();
        assert_eq!(
            records
                .iter()
                .map(|record| record.file.as_str())
                .collect::<Vec<_>>(),
            [
                "_app/immutable/z.js",
                "_app/immutable/a.js",
                "_app/immutable/z.css",
                "_app/immutable/font.woff2"
            ]
        );
        assert_eq!(records[0].key, "/project/z.svelte");
        assert_eq!(records[0].css, ["_app/immutable/z.css"]);
        assert_eq!(records[0].assets, ["_app/immutable/font.woff2"]);
        assert!(
            records
                .iter()
                .all(|record| array_index(&record.key).is_none())
        );
        for name in ["z.txt", "a.txt"] {
            input.static_assets.insert(
                name.into(),
                StaticAsset {
                    source: String::new(),
                    mime_type: None,
                },
            );
        }
        let defaults = derive_request(
            &input,
            &client,
            &ServiceWorkerOptions::default(),
            &["/".into()],
        )
        .unwrap();
        assert_eq!(defaults.static_assets, ["a.txt", "z.txt"]);
        let options = ServiceWorkerOptions {
            static_asset_order: Some(vec!["z.txt".into(), "a.txt".into()]),
            ..Default::default()
        };
        assert_eq!(
            derive_request(&input, &client, &options, &[])
                .unwrap()
                .static_assets,
            ["z.txt", "a.txt"]
        );
        let invalid = ServiceWorkerOptions {
            static_asset_order: Some(vec!["z.txt".into(), "z.txt".into()]),
            ..Default::default()
        };
        assert!(derive_request(&input, &client, &invalid, &[]).is_err());
    }
    #[test]
    fn worker_inventory_rejects_phantom_outputs_and_missing_static_files() {
        let (mut input, _, mut request, client) = fixture("export {};");
        request.client_manifest.push(ClientManifestEntry {
            key: "ghost".into(),
            file: "ghost.js".into(),
            css: vec![],
            assets: vec![],
        });
        assert!(
            client_inventory(&input, &request, &client, &BTreeMap::new())
                .unwrap_err()
                .contains("no emitted output")
        );
        request.client_manifest.clear();
        input.static_assets.insert(
            "robots.txt".into(),
            StaticAsset {
                source: base64::engine::general_purpose::STANDARD.encode(b"robots"),
                mime_type: None,
            },
        );
        request.static_assets.push("robots.txt".into());
        assert!(
            client_inventory(&input, &request, &client, &BTreeMap::new())
                .unwrap_err()
                .contains("Missing final client artifact")
        );
    }
}

#[cfg(test)]
#[path = "service_worker_tests.rs"]
mod asset_tests;
