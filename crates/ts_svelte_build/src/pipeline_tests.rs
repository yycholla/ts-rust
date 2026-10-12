//! Real native graph generation followed by the production guard/analysis bridge.
//! Fixtures contain no installed Node toolchain and execute no JavaScript.
use crate::*;
use std::sync::atomic::{AtomicU64, Ordering};
use ts_svelte_kit::{
    build_env::ResolvedBuildEnvironment,
    build_guards::{ExternalExposure, GuardStatus},
    build_inventory::{
        BuildInventory, BuildInventoryOptions, HookPaths, InventoryNode, InventoryRoute, NodeKind,
    },
    build_manifest::{ClientPage, ClientRouterConfig, RouterResolution, RouterType},
    build_plan::ResolvedBuildPlanConfig,
    build_server::{ResolvedServerInput, ResolvedServerOptions},
};
pub(crate) struct Fixture {
    pub(crate) root: PathBuf,
    pub(crate) request: BuildRequest,
}
impl Fixture {
    pub(crate) fn new(source: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "ts-svelte-build-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&root).unwrap();
        let runtime = root.join("node_modules/@sveltejs/kit/src/runtime");
        let generated = root.join(".kit/generated");
        let page = root.join("src/routes/+page.ts");
        let inventory = BuildInventory {
            kit_version: "2.69.2".into(),
            project_root: root.clone(),
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
                    component: Some(runtime.join("components/svelte-5/layout.svelte")),
                    universal: None,
                    server: None,
                    parent: None,
                    child_pages: vec![2],
                },
                InventoryNode {
                    kind: NodeKind::Error,
                    route_id: "/".into(),
                    depth: 0,
                    component: Some(runtime.join("components/svelte-5/error.svelte")),
                    universal: None,
                    server: None,
                    parent: None,
                    child_pages: vec![],
                },
                InventoryNode {
                    kind: NodeKind::Page,
                    route_id: "/".into(),
                    depth: 1,
                    component: None,
                    universal: Some(page.clone()),
                    server: None,
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
                endpoint: None,
            }],
            hooks: HookPaths::default(),
            matchers: BTreeMap::new(),
            params_module: None,
            service_worker: None,
            inputs: BTreeSet::new(),
            page_options_analyzed: false,
        };
        let kit=ResolvedNativeBuildInput {inventory,server:ResolvedServerInput {kit_version:"2.69.2".into(),output:generated.clone(),app_template:"<!doctype html><html><head>%sveltekit.head%</head><body>%sveltekit.body%</body></html>".into(),error_template:"%sveltekit.status% %sveltekit.error.message%".into(),version_name:"test".into(),server_hooks:None,universal_hooks:None,runtime_directory:Some(runtime.clone()),svelte_major:5,service_worker_present:false,options:ResolvedServerOptions::default()},environment:ResolvedBuildEnvironment {kit_version:"2.69.2".into(),environment_directory:generated.join("env"),runtime_directory:runtime,version_name:"test".into(),base:String::new(),app_dir:"_app".into(),build_values:BTreeMap::new(),public_prefix:"PUBLIC_".into(),private_prefix:String::new(),explicit_environment_variables:false,definition:None},config:ResolvedBuildPlanConfig {out_dir:root.join(".kit"),routes_directory:root.join("src/routes"),lib_directory:root.join("src/lib"),assets_directory:root.join("static"),aliases:vec![],bundle_strategy:BundleStrategy::Split,mode:"production".into(),assets_url:String::new(),paths_relative:true,paths_origin:None,fork_preloads:false,tracing_server:false,link_header_preload:false,version_poll_interval:0,adapter_name:None,instrumentation:None,remote_functions_enabled:false,remote_modules:vec![],server_conditions:vec!["svelte".into(),"node".into(),"production".into()],client_conditions:vec!["svelte".into(),"browser".into(),"production".into()],server_externals:vec![]},client_analysis:None};
        Self {
            root,
            request: BuildRequest {
                tailwind: None,
                adapter_node: None,
                service_worker_options: Default::default(),
                kit,
                compiler: CompilerSettings {
                    svelte_version: "5.53.11".into(),
                    typescript_version: None,
                    allow_prerelease: false,
                    runes: Some(true),
                    minify: false,
                    sourcemap: true,
                    options: Default::default(),
                    tsconfig: None,
                    mdsvex: None,
                    typescript: json!({}),
                },
                module_extensions: vec![".js".into(), ".ts".into()],
                remote_package_peers: BTreeMap::new(),
                external_exposure: BTreeMap::new(),
                guard_real_paths: BTreeMap::new(),
                guard_resolutions: vec![],
                modules: BTreeMap::from([(
                    page.to_string_lossy().into_owned(),
                    ModuleSnapshot {
                        source: source.into(),
                        imports: BTreeMap::new(),
                    },
                )]),
                sources: BTreeMap::new(),
                define: BTreeMap::new(),
                static_assets: BTreeMap::new(),
                static_assets_complete: true,
            },
        }
    }
    pub(crate) fn write(&self, file: &str, source: &str) {
        let file = self.root.join(file);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, source).unwrap();
    }
    fn prepared(&self, client: bool) -> (NativeBuildPlan, BundleRequest) {
        let mut kit = self.request.kit.clone();
        if client {
            kit.client_analysis = Some(ResolvedClientLoadAnalysis {
                server_build_id: "guard-fixture".into(),
                nodes: kit
                    .inventory
                    .nodes
                    .iter()
                    .map(|node| AnalyzedNodeLoads {
                        component: node.component.clone(),
                        universal: node.universal.clone(),
                        server: node.server.clone(),
                        has_server_load: false,
                        has_universal_load: false,
                    })
                    .collect(),
            });
        }
        let plan = build_plan::create_build_plan(&kit).unwrap();
        let mut prepared = graph::bundle_request(&self.request, &plan, client).unwrap();
        // These cases exercise actual native graph generation and the production
        // bridge, independently of installed framework runtime fixtures.
        prepared.entries = BTreeMap::from([("app".into(), self.root.join("src/routes/+page.ts"))]);
        (plan, prepared)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[tokio::test]
async fn framework_graphs_use_the_configured_native_tailwind_pipeline() {
    let mut fixture = Fixture::new("import '../../app.css'; export const prerender=false;");
    std::fs::create_dir_all(fixture.root.join("src")).unwrap();
    std::fs::write(
        fixture.root.join("app.css"),
        "@tailwind utilities source(none); @source inline('flex');",
    )
    .unwrap();
    fixture.request.tailwind =
        Some(serde_json::from_value(json!({"entries":["app.css"]})).unwrap());
    for client in [false, true] {
        let (_, request) = fixture.prepared(client);
        let result = ts_svelte_bundle::generate(request).await.unwrap();
        let module = result.css.modules.values().next().unwrap();
        assert!(
            module
                .tailwind
                .as_ref()
                .unwrap()
                .generated_candidates
                .contains(&"flex".into())
        );
        let asset = result
            .assets
            .iter()
            .find(|a| a.file_name == module.file_name)
            .unwrap();
        let css = String::from_utf8(asset_bytes(asset).unwrap()).unwrap();
        assert!(css.contains("display: flex"));
        assert!(!css.contains("@tailwind"));
        assert!(module.map_file_name.is_some());
    }
}

#[tokio::test]
async fn resolved_fixed_chunk_uses_the_owned_virtual_module_identity() {
    let fixture =
        Fixture::new("import { value } from 'fixture:manifest'; export const config={value};");
    let (mut plan, _) = fixture.prepared(false);
    plan.server
        .virtual_modules
        .insert("fixture:manifest".into(), "export const value=42;".into());
    plan.server
        .output
        .fixed_chunks
        .push(ts_svelte_kit::build_plan::FixedChunk {
            name: "fixture-manifest".into(),
            module: "fixture:manifest".into(),
            file: "manifest.js".into(),
        });
    let mut prepared = graph::bundle_request(&fixture.request, &plan, false).unwrap();
    prepared.entries = BTreeMap::from([("app".into(), fixture.root.join("src/routes/+page.ts"))]);
    assert_eq!(
        prepared.output.fixed_chunks["manifest.js"],
        [graph::virtual_path(&fixture.request, "fixture:manifest")
            .to_str()
            .unwrap()]
    );
    let bundle = ts_svelte_bundle::generate(prepared).await.unwrap();
    let fixed = bundle
        .chunks
        .iter()
        .find(|chunk| chunk.file_name == "manifest.js")
        .unwrap();
    assert!(fixed.code.contains("42"));
    assert!(
        bundle
            .chunks
            .iter()
            .any(|chunk| chunk.imports.iter().any(|id| id == "manifest.js"))
    );
}

#[tokio::test]
async fn generated_static_env_sources_feed_actual_server_graph_analysis() {
    let mut fixture = Fixture::new(
        "import { PUBLIC_VALUE } from '$env/static/public'; export const config={value:PUBLIC_VALUE}; export const prerender=false;",
    );
    fixture
        .request
        .kit
        .environment
        .build_values
        .insert("PUBLIC_VALUE".into(), "native 😀".into());
    let (plan, prepared) = fixture.prepared(false);
    let bundle = ts_svelte_bundle::generate(prepared.clone()).await.unwrap();
    let guard = guards::validate(&bundle, &prepared, &plan, &fixture.request).unwrap();
    assert_eq!(guard.imports_status, GuardStatus::Passed);
    assert_eq!(guard.configuration_status, GuardStatus::Passed);
    let mut input = analysis_input(&fixture.request).unwrap();
    assert!(
        !build_analysis::analyze(&input)
            .unwrap()
            .requirements
            .is_empty()
    );
    analysis::enrich(&mut input, &fixture.request, &plan, &prepared, &bundle).unwrap();
    let output = build_analysis::analyze(&input).unwrap();
    assert!(output.requirements.is_empty(), "{:?}", output.requirements);
    assert_eq!(
        known(&output.routes["/"].config),
        Some(json!({"value":"native 😀"}))
    );
    let generated = graph::virtual_path(&fixture.request, "$env/static/public");
    assert_eq!(
        input.modules[generated.to_str().unwrap()].source,
        prepared.virtual_modules[&generated]
    );
}
#[tokio::test]
async fn dynamic_env_is_not_replaced_with_static_build_values() {
    let mut fixture = Fixture::new(
        "import { env } from '$env/dynamic/public'; export const config={value:env.PUBLIC_VALUE};",
    );
    fixture
        .request
        .kit
        .environment
        .build_values
        .insert("PUBLIC_VALUE".into(), "build-time".into());
    fixture.write(
        "node_modules/@sveltejs/kit/src/runtime/shared-server.js",
        "export let public_env={PUBLIC_VALUE:'runtime'};",
    );
    let (plan, prepared) = fixture.prepared(false);
    let bundle = ts_svelte_bundle::generate(prepared.clone()).await.unwrap();
    let mut input = analysis_input(&fixture.request).unwrap();
    analysis::enrich(&mut input, &fixture.request, &plan, &prepared, &bundle).unwrap();
    let output = build_analysis::analyze(&input).unwrap();
    assert!(known(&output.routes["/"].config).is_none());
    assert!(!output.requirements.is_empty());
    assert!(
        !input.modules.contains_key(
            graph::virtual_path(&fixture.request, "$env/dynamic/public")
                .to_str()
                .unwrap()
        )
    );
}
#[tokio::test]
async fn actual_css_virtual_runtime_is_traversed_and_private_dead_imports_fail() {
    let fixture = Fixture::new("import './page.css'; export const answer=42;");
    fixture.write("src/routes/page.css", "p { color: red }");
    let (plan, prepared) = fixture.prepared(true);
    let bundle = ts_svelte_bundle::generate(prepared.clone()).await.unwrap();
    assert!(bundle.module_graph.contains_key("\0ts-svelte-css:runtime"));
    let guard = guards::validate(&bundle, &prepared, &plan, &fixture.request).unwrap();
    assert_eq!(guard.imports_status, GuardStatus::Passed);
    assert!(
        guard
            .reachable_modules
            .iter()
            .any(|id| id == "\0ts-svelte-css:runtime")
    );
    let fixture = Fixture::new("import '../lib/server/dead.js'; export const answer=42;");
    fixture.write("src/lib/server/dead.js", "export const removed=1;");
    let (plan, prepared) = fixture.prepared(true);
    let bundle = ts_svelte_bundle::generate(prepared.clone()).await.unwrap();
    let guard = guards::validate(&bundle, &prepared, &plan, &fixture.request).unwrap();
    assert_eq!(guard.imports_status, GuardStatus::Failed);
    assert!(!guard.import_diagnostics[0].chain.is_empty());
}
#[tokio::test]
async fn real_external_metadata_requires_explicit_public_evidence() {
    let mut fixture =
        Fixture::new("import { value } from 'opaque-package'; export const answer=value;");
    let (plan, mut prepared) = fixture.prepared(true);
    prepared.external.push("opaque-package".into());
    let bundle = ts_svelte_bundle::generate(prepared.clone()).await.unwrap();
    assert!(bundle.module_graph["opaque-package"].is_external);
    let guard = guards::validate(&bundle, &prepared, &plan, &fixture.request).unwrap();
    assert_eq!(guard.imports_status, GuardStatus::Incomplete);
    let mut report = BuildReport::new();
    assert!(!guards::record(&mut report, "client", Ok(guard)));
    assert_eq!(
        report.guards["client"].imports_status,
        GuardStatus::Incomplete
    );
    assert!(!report.requirements.is_empty());
    fixture.request.external_exposure.insert(
        "opaque-package".into(),
        ExternalExposure::Public {
            evidence: "Fixture host supplies the public module named value".into(),
        },
    );
    assert_eq!(
        guards::validate(&bundle, &prepared, &plan, &fixture.request)
            .unwrap()
            .imports_status,
        GuardStatus::Passed
    );
}
#[tokio::test]
async fn hashed_env_aliases_retain_feature_flags_and_prerelease_policy() {
    let mut fixture = Fixture::new(
        "import { PUBLIC_VALUE } from '$env/static/public'; export const value=PUBLIC_VALUE;",
    );
    fixture
        .request
        .kit
        .environment
        .build_values
        .insert("PUBLIC_VALUE".into(), "literal".into());
    fixture
        .request
        .kit
        .environment
        .explicit_environment_variables = true;
    fixture
        .request
        .kit
        .server
        .options
        .explicit_environment_variables = true;
    fixture.request.compiler.allow_prerelease = true;
    let (plan, prepared) = fixture.prepared(false);
    assert!(prepared.compatibility.allow_prerelease);
    let bundle = ts_svelte_bundle::generate(prepared.clone()).await.unwrap();
    let guard = guards::validate(&bundle, &prepared, &plan, &fixture.request).unwrap();
    assert_eq!(guard.configuration_status, GuardStatus::Failed);
    assert!(
        guard
            .configuration_diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "environment_module_feature_flag")
    );

    // A real prerelease plan must keep all independently resolved version/path
    // inputs consistent. This only builds the fixture entry, not Kit 3's full
    // server/client pipeline or manifest-finalization protocol.
    let mut beta = Fixture::new("export const value=42;");
    let version = "3.0.0-next.32";
    let generated = beta.root.join(".kit/generated/build");
    beta.request.kit.inventory.kit_version = version.into();
    beta.request.kit.inventory.generated_directory = generated.clone();
    beta.request.kit.server.kit_version = version.into();
    beta.request.kit.server.output = generated.clone();
    beta.request.kit.environment.kit_version = version.into();
    beta.request.kit.environment.environment_directory = generated.join("env");
    beta.request.compiler.svelte_version = "5.57.2".into();
    beta.request.compiler.allow_prerelease = true;
    let (_, prepared) = beta.prepared(false);
    assert!(prepared.compatibility.allow_prerelease);
    ts_svelte_bundle::generate(prepared).await.unwrap();
}

#[cfg(unix)]
#[tokio::test]
#[ignore = "requires explicit TS_SVELTE_BUILD_KIT_FIXTURE with installed Kit 2.69.2 and Svelte 5.53.11"]
async fn full_resolved_kit2_build_runs_server_analysis_client_and_manifest() {
    let dependency_project = PathBuf::from(
        std::env::var_os("TS_SVELTE_BUILD_KIT_FIXTURE")
            .expect("Set TS_SVELTE_BUILD_KIT_FIXTURE to the read-only dependency project"),
    );
    assert!(dependency_project.is_absolute());
    for (package, expected) in [("@sveltejs/kit", "2.69.2"), ("svelte", "5.53.11")] {
        let value: Value = serde_json::from_slice(
            &std::fs::read(
                dependency_project
                    .join("node_modules")
                    .join(package)
                    .join("package.json"),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(
            value["version"], expected,
            "Fixture package version must match the registered profile"
        );
    }
    let mut fixture =
        Fixture::new("export const prerender=false; export const config={native:true};");
    std::os::unix::fs::symlink(
        dependency_project.join("node_modules"),
        fixture.root.join("node_modules"),
    )
    .unwrap();
    fixture.write(
        "package.json",
        r#"{"name":"native-kit-pipeline-fixture","type":"module"}"#,
    );
    let runtime = fixture.request.kit.inventory.runtime_directory.clone();
    fixture.request.kit.inventory.nodes[0].component =
        Some(runtime.join("components/svelte-5/layout.svelte"));
    fixture.request.kit.inventory.nodes[1].component =
        Some(runtime.join("components/svelte-5/error.svelte"));
    let component = fixture.root.join("src/routes/+page.svelte");
    fixture.request.kit.inventory.nodes[2].component = Some(component.clone());
    fixture.request.sources.insert(
        component,
        "<h1>Native framework pipeline</h1><style>h1 { color: green; }</style>".into(),
    );
    let report = build(fixture.request.clone()).await;
    let details=serde_json::to_string(&json!({"status":report.status,"phases":report.phases,"requirements":report.requirements,"diagnostics":report.diagnostics})).unwrap();
    assert!(report.server.is_some(), "{details}");
    assert!(report.analysis.is_some(), "{details}");
    assert!(report.client.is_some(), "{details}");
    assert!(
        report.artifacts.contains_key("server/manifest.js"),
        "{details}"
    );
    assert!(
        report.artifacts.contains_key("server/nodes/2.js"),
        "{details}"
    );
    assert_eq!(
        report.guards["server"].configuration_status,
        GuardStatus::Passed,
        "{details}"
    );
    assert_eq!(
        report.guards["client"].imports_status,
        GuardStatus::Passed,
        "{details}"
    );
    assert!(
        report
            .phases
            .iter()
            .any(|phase| phase.name == "analysis" && phase.status == "complete"),
        "{details}"
    );
    assert!(
        report
            .phases
            .iter()
            .any(|phase| phase.name == "nodes and manifests" && phase.status == "complete"),
        "{details}"
    );
    assert_eq!(report.status, "complete", "{details}");
    assert!(report.requirements.is_empty(), "{details}");
    let adapter_directory = dependency_project.join("node_modules/@sveltejs/adapter-node");
    let metadata: Value =
        serde_json::from_slice(&std::fs::read(adapter_directory.join("package.json")).unwrap())
            .unwrap();
    let templates = ["env.js", "handler.js", "index.js", "shims.js"]
        .into_iter()
        .map(|name| {
            (
                name.to_string(),
                std::fs::read_to_string(adapter_directory.join("files").join(name)).unwrap(),
            )
        })
        .collect();
    fixture.request.kit.config.adapter_name = Some("@sveltejs/adapter-node".into());
    fixture.request.adapter_node = Some(adapter::AdapterRequest {
        templates: ts_svelte_kit::adapter_node::RuntimeTemplates {
            version: metadata["version"].as_str().unwrap().into(),
            files: templates,
        },
        options: Default::default(),
        package_json: json!({"name":"native-kit-pipeline-fixture","type":"module"}),
        server_map_base: fixture.root.join("dist"),
    });
    let packaged = build(fixture.request.clone()).await;
    let details = serde_json::to_string(&json!({"status":packaged.status,"requirements":packaged.requirements,"diagnostics":packaged.diagnostics})).unwrap();
    assert_eq!(packaged.status, "complete", "{details}");
    assert!(packaged.adapter.is_some(), "{details}");
    assert!(packaged.artifacts.contains_key("build/index.js"));
    assert!(packaged.artifacts.contains_key("build/server/index.js"));
    assert!(
        packaged
            .artifacts
            .keys()
            .any(|name| name.starts_with("build/client/") && name.ends_with(".gz"))
    );
    assert!(
        packaged
            .artifacts
            .keys()
            .any(|name| name.starts_with("build/client/") && name.ends_with(".br"))
    );
}

#[cfg(unix)]
#[tokio::test]
#[ignore = "requires explicit TS_SVELTE_BUILD_KIT_FIXTURE with installed Kit 2.69.2, Svelte 5.53.11 and adapter-node 5.5.4"]
async fn full_resolved_kit2_instrumentation_build_packages_ordered_adapter_entry() {
    let dependency_project = PathBuf::from(
        std::env::var_os("TS_SVELTE_BUILD_KIT_FIXTURE")
            .expect("Set TS_SVELTE_BUILD_KIT_FIXTURE to the read-only dependency project"),
    );
    assert!(dependency_project.is_absolute());
    for (package, expected) in [
        ("@sveltejs/kit", "2.69.2"),
        ("svelte", "5.53.11"),
        ("@sveltejs/adapter-node", "5.5.4"),
    ] {
        let metadata: Value = serde_json::from_slice(
            &std::fs::read(
                dependency_project
                    .join("node_modules")
                    .join(package)
                    .join("package.json"),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(
            metadata["version"], expected,
            "Instrumentation fixture must match its registered package profiles"
        );
    }
    let mut fixture =
        Fixture::new("export const prerender=false; export const config={native:true};");
    std::os::unix::fs::symlink(
        dependency_project.join("node_modules"),
        fixture.root.join("node_modules"),
    )
    .unwrap();
    fixture.write(
        "package.json",
        r#"{"name":"native-instrumentation-pipeline-fixture","type":"module"}"#,
    );
    let component = fixture.root.join("src/routes/+page.svelte");
    fixture.request.kit.inventory.nodes[2].component = Some(component.clone());
    fixture.request.sources.insert(
        component,
        "<h1>Instrumented native build</h1><style>h1 { color: navy; }</style>".into(),
    );

    // These modules are supplied source snapshots. The native builders emit
    // their side effects and top-level await; the test never executes them.
    let instrument = fixture.root.join("src/instrumentation.server.ts");
    let dependency = fixture.root.join("src/instrumentation-marker.ts");
    let bootstrap = fixture.root.join("src/instrumentation-bootstrap.ts");
    fixture.request.sources.insert(instrument.clone(), "import { marker } from './instrumentation-marker.ts';\nconst { install } = await import('./instrumentation-bootstrap.ts');\nawait install(marker);\n".into());
    fixture.request.sources.insert(
        dependency.clone(),
        "export const marker = 'native-instrumentation-pipeline';\n".into(),
    );
    fixture.request.sources.insert(bootstrap.clone(), "export async function install(marker) { await Promise.resolve(); globalThis.__native_instrumentation_fixture = marker; }\n".into());
    fixture.request.kit.config.instrumentation =
        Some(ts_svelte_kit::build_plan::ResolvedInstrumentation {
            entry: instrument.clone(),
            enabled: true,
            adapter_supports: Some(true),
        });
    fixture.request.kit.config.adapter_name = Some("@sveltejs/adapter-node".into());
    let adapter_directory = dependency_project.join("node_modules/@sveltejs/adapter-node/files");
    let templates = ["env.js", "handler.js", "index.js", "shims.js"]
        .into_iter()
        .map(|name| {
            (
                name.to_owned(),
                std::fs::read_to_string(adapter_directory.join(name)).unwrap(),
            )
        })
        .collect();
    fixture.request.adapter_node = Some(adapter::AdapterRequest {
        templates: ts_svelte_kit::adapter_node::RuntimeTemplates {
            version: "5.5.4".into(),
            files: templates,
        },
        options: Default::default(),
        package_json: json!({"name":"native-instrumentation-pipeline-fixture","type":"module"}),
        server_map_base: fixture.root.join("dist"),
    });

    let report = build(fixture.request.clone()).await;
    let details = serde_json::to_string(&json!({"status":report.status,"phases":report.phases,"requirements":report.requirements,"diagnostics":report.diagnostics})).unwrap();
    assert_eq!(report.status, "complete", "{details}");
    assert!(report.requirements.is_empty(), "{details}");
    for phase in [
        "analysis",
        "instrumentation entry",
        "nodes and manifests",
        "adapter",
    ] {
        assert!(
            report
                .phases
                .iter()
                .any(|actual| actual.name == phase && actual.status == "complete"),
            "Missing completed phase {phase}: {details}"
        );
    }
    assert!(report.analysis.is_some(), "{details}");
    assert!(report.client.is_some(), "{details}");
    assert_eq!(
        report.guards["server"].imports_status,
        GuardStatus::Passed,
        "{details}"
    );
    assert_eq!(
        report.guards["client"].imports_status,
        GuardStatus::Passed,
        "{details}"
    );
    assert!(
        report.artifacts.contains_key("server/nodes/2.js"),
        "{details}"
    );
    assert!(
        report.artifacts.contains_key("server/manifest.js"),
        "{details}"
    );
    let receipt = serde_json::to_value(report.instrumentation.as_ref().expect(&details)).unwrap();
    assert_eq!(receipt["source"], instrument.to_str().unwrap());
    assert_eq!(receipt["emittedFile"], "instrumentation.server.js");
    assert_eq!(receipt["environmentInitializerRequired"], false);

    let server = report.server.as_ref().expect(&details);
    let entry = server
        .chunks
        .iter()
        .find(|chunk| chunk.file_name == "instrumentation.server.js")
        .expect(&details);
    assert!(entry.is_entry);
    assert_eq!(entry.facade_module_id.as_deref(), instrument.to_str());
    for module in [&instrument, &dependency, &bootstrap] {
        assert!(
            server.module_graph.contains_key(module.to_str().unwrap()),
            "Missing instrumentation module {}",
            module.display()
        );
    }
    assert!(
        !entry.dynamic_imports.is_empty(),
        "The actual asynchronous instrumentation dependency must remain in the server graph"
    );
    for dependency in &entry.dynamic_imports {
        assert!(
            receipt["dependencies"]
                .as_array()
                .unwrap()
                .iter()
                .any(|file| file == dependency)
        );
        assert!(
            report
                .artifacts
                .contains_key(&format!("server/{dependency}"))
        );
    }

    let adapter = report.adapter.as_ref().expect(&details);
    let ordering = adapter
        .instrumentation
        .as_ref()
        .expect("Completed adapter must retain instrumentation ordering metadata");
    assert_eq!(ordering.entrypoint, "build/index.js");
    assert_eq!(ordering.start, "build/start.js");
    assert_eq!(
        ordering.static_preludes,
        ["./server/instrumentation.server.js"]
    );
    assert_eq!(ordering.dynamic_start, "./start.js");
    assert!(
        ordering.map_copy.is_none(),
        "Pinned adapter runtime template has no input map to copy"
    );
    assert!(ordering.identities["build/index.js"].generated);
    assert_eq!(
        ordering.identities["build/start.js"]
            .original_module
            .as_deref(),
        Some("@sveltejs/adapter-node@5.5.4/files/index.js")
    );
    let wrapper =
        String::from_utf8(artifacts::bytes(&report.artifacts["build/index.js"]).unwrap()).unwrap();
    assert!(wrapper.starts_with("import \"./server/instrumentation.server.js\";\n"));
    assert!(wrapper.contains("await import(\"./start.js\")"));
    assert!(
        !wrapper.contains("__sveltekit_env_init"),
        "Kit 2 must retain its own adapter contract"
    );
    let start =
        String::from_utf8(artifacts::bytes(&report.artifacts["build/start.js"]).unwrap()).unwrap();
    assert!(start.contains("./handler.js"));
    assert!(
        !start.contains("./start.js"),
        "Application start must not be wrapped twice"
    );
    assert!(
        !report.artifacts.contains_key("build/index.js.map"),
        "Do not attach a stale source map to the generated facade"
    );

    let rebundled = adapter
        .bundle
        .chunks
        .iter()
        .find(|chunk| chunk.file_name == "instrumentation.server.js")
        .expect("Adapter must retain a separate instrumentation entry");
    assert!(rebundled.is_entry);
    assert!(!rebundled.dynamic_imports.is_empty());
    for import in &rebundled.dynamic_imports {
        assert!(
            report
                .artifacts
                .contains_key(&format!("build/server/{import}"))
        );
    }
    assert_eq!(
        artifacts::bytes(&report.artifacts["build/server/instrumentation.server.js"]).unwrap(),
        rebundled.code.as_bytes()
    );
    let map_name = rebundled
        .sourcemap_file_name
        .as_ref()
        .expect("Adapter rebundle must emit an instrumentation map");
    let map: Value = serde_json::from_slice(
        &artifacts::bytes(&report.artifacts[&format!("build/server/{map_name}")]).unwrap(),
    )
    .unwrap();
    assert_eq!(rebundled.map.as_ref(), Some(&map));
    assert_eq!(map["version"], 3);
    assert!(
        map["sources"]
            .as_array()
            .unwrap()
            .iter()
            .any(|source| source.as_str() == instrument.to_str()),
        "Rebundled map must retain original instrumentation source identity: {}",
        map["sources"]
    );
    assert!(
        map["sourcesContent"]
            .as_array()
            .unwrap()
            .iter()
            .any(|source| source.as_str()
                == fixture.request.sources.get(&instrument).map(String::as_str)),
        "Rebundled map must retain the exact source snapshot"
    );
    assert!(
        report
            .artifacts
            .keys()
            .any(|file| file.starts_with("build/client/") && file.ends_with(".gz"))
    );
    assert!(
        report
            .artifacts
            .keys()
            .any(|file| file.starts_with("build/client/") && file.ends_with(".br"))
    );
}

#[cfg(unix)]
#[tokio::test]
#[ignore = "requires explicit TS_SVELTE_BUILD_KIT3_FIXTURE with installed Kit 3.0.1 and Svelte 5.57.2"]
async fn full_resolved_kit3_build_finalizes_server_and_fixed_client_manifest() {
    let dependency_project = PathBuf::from(
        std::env::var_os("TS_SVELTE_BUILD_KIT3_FIXTURE")
            .expect("Set TS_SVELTE_BUILD_KIT3_FIXTURE to the read-only Kit 3 dependency project"),
    );
    assert!(dependency_project.is_absolute());
    for (package, expected) in [("@sveltejs/kit", "3.0.1"), ("svelte", "5.57.2")] {
        let value: Value = serde_json::from_slice(
            &std::fs::read(
                dependency_project
                    .join("node_modules")
                    .join(package)
                    .join("package.json"),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(
            value["version"], expected,
            "Fixture must match the registered profile"
        );
    }
    let mut fixture =
        Fixture::new("export const prerender=false; export const config={native:true};");
    std::os::unix::fs::symlink(
        dependency_project.join("node_modules"),
        fixture.root.join("node_modules"),
    )
    .unwrap();
    fixture.write(
        "package.json",
        r#"{"name":"native-kit3-pipeline-fixture","type":"module"}"#,
    );
    let generated = fixture.root.join(".kit/generated/build");
    let runtime = fixture.request.kit.inventory.runtime_directory.clone();
    fixture.request.kit.inventory.kit_version = "3.0.1".into();
    fixture.request.kit.inventory.generated_directory = generated.clone();
    fixture.request.kit.inventory.nodes[0].component =
        Some(runtime.join("components/layout.svelte"));
    fixture.request.kit.inventory.nodes[1].component =
        Some(runtime.join("components/error.svelte"));
    fixture.request.kit.server.kit_version = "3.0.1".into();
    fixture.request.kit.server.output = generated.clone();
    fixture.request.kit.environment.kit_version = "3.0.1".into();
    fixture.request.kit.environment.environment_directory = generated.join("env");
    fixture.request.compiler.svelte_version = "5.57.2".into();
    let component = fixture.root.join("src/routes/+page.svelte");
    fixture.request.kit.inventory.nodes[2].component = Some(component.clone());
    fixture.request.sources.insert(
        component,
        r#"<script>
import { immutable, assets, prerendered, routes } from '$app/manifest';
</script>
<h1>Native Kit 3 😀</h1>
<p>{immutable.length} {assets.length} {prerendered.length} {routes.length}</p>
<style>h1 { color: green; }</style>"#
            .into(),
    );
    fixture.request.static_assets.insert(
        "robots.txt".into(),
        StaticAsset {
            source: base64::engine::general_purpose::STANDARD.encode(b"User-agent: *\n"),
            mime_type: Some("text/plain".into()),
        },
    );
    let report = build(fixture.request.clone()).await;
    let details = serde_json::to_string(&json!({"status":report.status,"phases":report.phases,"requirements":report.requirements,"diagnostics":report.diagnostics})).unwrap();
    assert_eq!(report.status, "complete", "{details}");
    assert!(report.requirements.is_empty(), "{details}");
    assert!(report.analysis.is_some(), "{details}");
    assert!(
        report.adapter.is_none(),
        "No adapter is selected by this fixture"
    );
    for file in [
        "server/manifest.js",
        "server/nodes/2.js",
        "client/_app/manifest.js",
        "client/robots.txt",
    ] {
        assert!(
            report.artifacts.contains_key(file),
            "Missing {file}: {details}"
        );
    }
    assert!(
        report
            .phases
            .iter()
            .any(|p| p.name == "manifest finalization" && p.status == "complete")
    );
    for (target, bundle) in [
        ("server", report.server.as_ref().unwrap()),
        ("client", report.client.as_ref().unwrap()),
    ] {
        assert_eq!(
            report.guards[target].imports_status,
            GuardStatus::Passed,
            "{details}"
        );
        assert_eq!(
            report.guards[target].configuration_status,
            GuardStatus::Passed,
            "{details}"
        );
        for chunk in &bundle.chunks {
            assert!(
                ts_svelte_kit::build_finalize::inspect_variable_counts(&chunk.code)
                    .unwrap()
                    .is_empty(),
                "Bare placeholders in {target}/{}",
                chunk.file_name
            );
            assert!(!chunk.code.contains("__sveltekit_manifest_build__"));
            assert!(!chunk.code.contains("__sveltekit_manifest_prerendered__"));
            assert_eq!(
                artifacts::bytes(&report.artifacts[&format!("{target}/{}", chunk.file_name)])
                    .unwrap(),
                chunk.code.as_bytes()
            );
            if let Some(file) = &chunk.sourcemap_file_name {
                let map: Value = serde_json::from_slice(
                    &artifacts::bytes(&report.artifacts[&format!("{target}/{file}")]).unwrap(),
                )
                .unwrap();
                assert_eq!(chunk.map.as_ref(), Some(&map));
            }
        }
    }
    let client = report.client.as_ref().unwrap();
    let manifest = client
        .chunks
        .iter()
        .find(|chunk| chunk.file_name == "_app/manifest.js")
        .unwrap();
    assert!(manifest.code.contains("robots.txt"));
    for chunk in &client.chunks {
        if chunk.file_name.starts_with("_app/immutable/") {
            assert!(
                manifest.code.contains(&chunk.file_name),
                "Manifest lacks emitted {}",
                chunk.file_name
            );
        }
    }

    // An explicitly disabled client completes with an empty immutable list;
    // this is distinct from a build that has not resolved its client yet.
    fixture.request.modules.values_mut().next().unwrap().source =
        "export const prerender=false; export const csr=false;".into();
    let empty = build(fixture.request.clone()).await;
    let details = serde_json::to_string(&json!({"status":empty.status,"phases":empty.phases,"requirements":empty.requirements,"diagnostics":empty.diagnostics})).unwrap();
    assert_eq!(empty.status, "complete", "{details}");
    assert!(empty.client.is_none(), "{details}");
    assert!(
        empty
            .phases
            .iter()
            .any(|p| p.name == "client" && p.status == "skippedAllCsrFalse")
    );
    for chunk in &empty.server.as_ref().unwrap().chunks {
        assert!(!chunk.code.contains("__sveltekit_manifest_build__"));
        assert!(!chunk.code.contains("__sveltekit_manifest_prerendered__"));
    }
}
