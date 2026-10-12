use super::*;
use ts_svelte_kit::build_remote::{RemoteExport, RemoteKind};
fn options(target: RemoteTarget) -> RemotePluginOptions {
    RemotePluginOptions {
        kit_version: "3.0.1".into(),
        project_root: "/project".into(),
        target,
        enabled: true,
        development: false,
        sourcemap: true,
        module_extensions: vec![".js".into(), ".ts".into()],
        package_peers: BTreeMap::new(),
        seeds: vec![],
        client_runtime_id: Some(
            "/project/node_modules/@sveltejs/kit/src/runtime/client/remote-functions/index.js"
                .into(),
        ),
        metadata: BTreeMap::new(),
        typescript: Default::default(),
    }
}
const ID: &str = "/project/src/data.remote.ts";
#[test]
fn ordinary_typescript_is_lowered_before_kit_instrumentation_and_maps_keep_ts() {
    let plugin = RemotePlugin::new(options(RemoteTarget::Server)).unwrap();
    let source = "import { query } from '$app/server';\ntype Name = string;\nexport const greeting = query((name: Name): string => `Hello ${name}`);";
    let output = plugin
        .prepare(ID, source, &ModuleType::Ts)
        .unwrap()
        .unwrap();
    assert!(!output.artifact.code.contains("type Name"));
    assert!(!output.artifact.code.contains("name: Name"));
    assert!(output.artifact.code.contains("query("));
    assert!(
        output
            .artifact
            .code
            .contains("@sveltejs/kit/internal/server")
    );
    assert!(output.artifact.code.contains("./data.remote.ts"));
    let map = output.artifact.map.unwrap();
    assert_eq!(map["sourcesContent"], json!([source]));
    assert_eq!(map["sources"], json!([ID]));
    let records = plugin.state.records().unwrap();
    assert_eq!(records.len(), 1);
    assert!(records[0].transformed);
    assert_eq!(
        records[0].source_sha256.as_deref(),
        Some(format!("{:x}", Sha256::digest(source)).as_str())
    );
    // The framework transformer output map can be consumed by Rolldown directly.
    let parsed = SourceMap::from_json(serde_json::from_value(map).unwrap()).unwrap();
    assert_eq!(parsed.get_source_content(0), Some(source));
}
#[test]
fn lowering_error_retains_real_diagnostics_and_never_reserves_success() {
    let plugin = RemotePlugin::new(options(RemoteTarget::Server)).unwrap();
    let error = plugin
        .prepare(ID, "export const broken: = ;", &ModuleType::Ts)
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("TypeScript preprocessing failed")
    );
    let diagnostics = plugin.state.diagnostics().unwrap();
    assert!(
        diagnostics
            .iter()
            .any(|d| d["filename"] == ID && d["phase"] == "parse")
    );
    assert!(
        diagnostics
            .iter()
            .any(|d| d["source"] == "native-kit-remote" && d["phase"] == "transform")
    );
    assert!(plugin.state.records().unwrap().is_empty());
}
#[test]
fn typed_client_uses_evaluated_exports_and_contains_no_private_dependencies() {
    let mut settings = options(RemoteTarget::Client);
    let hash = build_remote::remote_hash("src/data.remote.ts");
    settings.metadata.insert(
        hash.clone(),
        EvaluatedRemoteExports {
            hash: hash.clone(),
            file: "src/data.remote.ts".into(),
            exports: vec![RemoteExport {
                name: "greeting".into(),
                kind: RemoteKind::Query,
            }],
        },
    );
    let plugin = RemotePlugin::new(settings).unwrap();
    let output = plugin
        .prepare(
            ID,
            "import {SECRET} from '$env/static/private'; export const greeting: unknown = SECRET;",
            &ModuleType::Ts,
        )
        .unwrap()
        .unwrap();
    assert!(!output.artifact.code.contains("SECRET"));
    assert!(!output.artifact.code.contains("$env"));
    assert!(
        output
            .artifact
            .code
            .contains(&format!("__remote.query(\"{hash}/greeting\")"))
    );
    assert!(output.artifact.map.is_none());
    assert!(output.entry.is_none());
    assert!(
        plugin
            .state
            .emitted_chunks(&[])
            .unwrap_err()
            .to_string()
            .contains("production server")
    );
}
#[test]
fn missing_metadata_version_and_peer_proof_are_real_errors() {
    let plugin = RemotePlugin::new(options(RemoteTarget::Client)).unwrap();
    assert!(
        plugin
            .prepare(ID, "export const x=1;", &ModuleType::Js)
            .unwrap_err()
            .to_string()
            .contains("requires evaluated")
    );
    let mut settings = options(RemoteTarget::Server);
    settings.kit_version = "3.1.0".into();
    assert!(
        RemotePlugin::new(settings)
            .unwrap_err()
            .to_string()
            .contains("Unsupported native Kit")
    );
    let plugin = RemotePlugin::new(options(RemoteTarget::Server)).unwrap();
    assert!(
        plugin
            .discover("/project/node_modules/lib/data.remote.js")
            .unwrap_err()
            .to_string()
            .contains("evidence")
    );
    let mut settings = options(RemoteTarget::Server);
    settings
        .package_peers
        .insert("/project/node_modules/lib/data.remote.js".into(), false);
    assert!(
        !RemotePlugin::new(settings)
            .unwrap()
            .discover("/project/node_modules/lib/data.remote.js")
            .unwrap()
    );
}
#[test]
fn seed_namespace_is_real_and_duplicate_seeds_reserve_only_one_entry() {
    let mut settings = options(RemoteTarget::Server);
    settings.seeds = vec![ID.into(), ID.into()];
    let plugin = RemotePlugin::new(settings).unwrap();
    let records = plugin.state.records().unwrap();
    assert_eq!(records.len(), 1);
    assert!(!records[0].transformed);
    let source = plugin.state.virtual_source(&records[0].virtual_id).unwrap();
    assert_eq!(
        source,
        "import * as m from \"/project/src/data.remote.ts\";\nexport default m;"
    );
    assert!(
        plugin
            .state
            .virtual_source("\0sveltekit-remote:missing")
            .is_err()
    );
    let output = plugin
        .prepare(ID, "export const x=1;", &ModuleType::Js)
        .unwrap()
        .unwrap();
    assert_eq!(output.identity, records[0].identity);
    assert_eq!(plugin.state.records().unwrap().len(), 1);
    assert!(plugin.state.records().unwrap()[0].transformed);
}
#[test]
fn emitted_facades_are_resolved_by_identity_not_guessed_filename() {
    let plugin = RemotePlugin::new(options(RemoteTarget::Server)).unwrap();
    plugin
        .prepare(ID, "export const x=1;", &ModuleType::Js)
        .unwrap();
    let record = plugin.state.records().unwrap().pop().unwrap();
    plugin
        .state
        .lock()
        .unwrap()
        .records
        .get_mut(&record.identity.hash)
        .unwrap()
        .reference_id = Some("actual-reference".into());
    let facade = EmittedFacade {
        facade_module_id: Some(record.virtual_id.clone()),
        file_name: "chunks/custom-actual-hash.js".into(),
        exports: vec!["default".into()],
    };
    let result = plugin.state.emitted_chunks(&[facade.clone()]).unwrap();
    assert_eq!(result[0].chunk, facade.file_name);
    assert_eq!(result[0].file, "src/data.remote.ts");
    assert_eq!(result[0].reference_id, "actual-reference");
    assert!(
        plugin
            .state
            .emitted_chunks(&[])
            .unwrap_err()
            .to_string()
            .contains("Missing emitted facade")
    );
    assert!(
        plugin
            .state
            .emitted_chunks(&[facade.clone(), facade.clone()])
            .unwrap_err()
            .to_string()
            .contains("Duplicate")
    );
    let mut no_default = facade.clone();
    no_default.exports.clear();
    assert!(
        plugin
            .state
            .emitted_chunks(&[no_default])
            .unwrap_err()
            .to_string()
            .contains("default export")
    );
    let mut traversal = facade;
    traversal.file_name = "../remote.js".into();
    assert!(plugin.state.emitted_chunks(&[traversal]).is_err());
}
#[test]
fn concurrent_reservations_are_deduplicated_and_collisions_fail() {
    let state = RemoteState::default();
    let identity = RemoteIdentity {
        hash: build_remote::remote_hash("src/data.remote.ts"),
        file: "src/data.remote.ts".into(),
        module_id: ID.into(),
    };
    let threads = (0..8)
        .map(|_| {
            let state = state.clone();
            let identity = identity.clone();
            std::thread::spawn(move || state.reserve(identity, Some("same actual input")))
        })
        .collect::<Vec<_>>();
    assert_eq!(
        threads
            .into_iter()
            .map(|thread| thread.join().unwrap().unwrap())
            .filter(|first| *first)
            .count(),
        1
    );
    assert_eq!(state.records().unwrap().len(), 1);
    let conflicting = RemoteIdentity {
        module_id: "/another/src/data.remote.ts".into(),
        ..identity.clone()
    };
    assert!(
        state
            .reserve(conflicting, None)
            .unwrap_err()
            .to_string()
            .contains("collision")
    );
    assert!(
        state
            .reserve(identity, Some("different input"))
            .unwrap_err()
            .to_string()
            .contains("different input")
    );
}
#[test]
fn disabled_service_worker_and_non_js_modules_are_not_silent_passthroughs() {
    let mut settings = options(RemoteTarget::Server);
    settings.enabled = false;
    let plugin = RemotePlugin::new(settings).unwrap();
    assert!(
        plugin
            .prepare(ID, "export const x=1;", &ModuleType::Js)
            .unwrap_err()
            .to_string()
            .contains("experimental.remoteFunctions")
    );
    let plugin = RemotePlugin::new(options(RemoteTarget::ServiceWorker)).unwrap();
    assert!(
        plugin
            .prepare(ID, "export const x=1;", &ModuleType::Js)
            .unwrap_err()
            .to_string()
            .contains("service workers")
    );
    let plugin = RemotePlugin::new(options(RemoteTarget::Server)).unwrap();
    assert!(
        plugin
            .prepare("/project/src/data.remote.json", "{}", &ModuleType::Json)
            .unwrap_err()
            .to_string()
            .contains("unsupported module type")
    );
    assert!(
        plugin
            .prepare(
                "/project/src/ordinary.ts",
                "not parsed by this plugin",
                &ModuleType::Ts
            )
            .unwrap()
            .is_none()
    );
    assert!(
        plugin
            .prepare("\0component.svelte", "already generated", &ModuleType::Js)
            .unwrap()
            .is_none()
    );
}
#[test]
fn kit2_dev_alias_and_sourcemap_off_use_registered_contract() {
    let mut settings = options(RemoteTarget::Client);
    settings.kit_version = "2.69.2".into();
    settings.development = true;
    settings.sourcemap = false;
    let hash = build_remote::remote_hash("src/data.remote.ts");
    settings.metadata.insert(
        hash.clone(),
        EvaluatedRemoteExports {
            hash,
            file: "src/data.remote.ts".into(),
            exports: vec![RemoteExport {
                name: "x".into(),
                kind: RemoteKind::Command,
            }],
        },
    );
    let plugin = RemotePlugin::new(settings).unwrap();
    let output = plugin
        .prepare(ID, "export const x:number=1;", &ModuleType::Ts)
        .unwrap()
        .unwrap();
    assert!(output.artifact.code.contains("\"__sveltekit/remote\""));
    assert!(output.artifact.code.contains("import.meta.hot?.accept()"));
    assert!(output.artifact.map.is_none());
}
