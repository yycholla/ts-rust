use super::*;
use ts_js_runtime::{ModuleSource, Resolution, RuntimeValue};

fn request(kit3: bool) -> BootstrapRequest {
    let internal = r#"
      export const events=[];
      export const state={};
      export function set_building(){events.push('building');state.building=true}
      export function set_private_env(value){events.push('private');state.private=value}
      export function set_public_env(value){events.push('public');state.public=value}
      export function set_manifest(value){events.push('manifest');state.manifest=value}
      export function set_read_implementation(value){events.push('read');state.read=value}
      export function init(){throw Error('Initialization hooks must not run during analysis')}
      export async function configure(config){
        events.push('configure:start');await 0;
        state.building=config.building;state.manifest=config.manifest;
        state.env=config.env;state.read=config.read;events.push('configure:end');
      }
    "#;
    let manifest = r#"
      import {state,events} from './internal.js';
      const nodes=[
        async()=>{events.push('node0:start');await 0;events.push('node0:end');
          if(!state.building||state.env.SECRET!=='private')throw Error('Server environment not initialized');
          const text=await new Response(state.read('asset.txt')).text();
          if(text!=='native asset')throw Error('Invalid snapshot read');
          return {index:0,server:{load:null},universal:{load:()=>{throw Error('Do not invoke route load')}}};
        },
        async()=>{events.push('node1:start');await 0;events.push('node1:end');return {index:1,server:{trailingSlash:'always'},universal:{}}}
      ];
    "#.to_owned()+if kit3 {"export const manifest={nodes,assets:new Set(['asset.txt'])};"} else {"export const manifest={_:{nodes},assets:new Set(['asset.txt'])};"};
    let modules = [
        ("manifest",manifest),
        ("internal",internal.into()),
        ("environment","import {state,events} from './internal.js';export function set_env(env){state.env=env;events.push('env')}".into()),
    ].into_iter().map(|(id,source)|(id.into(),ModuleSource{source,import_meta_url:format!("file:///{id}.js")})).collect();
    let resolutions = ["manifest", "environment"]
        .into_iter()
        .map(|id| {
            (
                id.into(),
                BTreeMap::from([(
                    "./internal.js".into(),
                    Resolution::Module {
                        id: "internal".into(),
                    },
                )]),
            )
        })
        .collect();
    BootstrapRequest {
        server_build_id: "fixture-server-build".into(),
        snapshot: HostSnapshot {
            modules,
            resolutions,
        },
        manifest_entry: "manifest".into(),
        initialization: if kit3 {
            Initialization::Kit3 {
                server: "internal".into(),
            }
        } else {
            Initialization::Kit2 {
                internal: "internal".into(),
                environment: "environment".into(),
                public_prefix: "PUBLIC_".into(),
                private_prefix: String::new(),
            }
        },
        environment: BTreeMap::from([
            ("PUBLIC_NAME".into(), "public".into()),
            ("SECRET".into(), "private".into()),
        ]),
        assets: BTreeMap::from([("asset.txt".into(), b"native asset".to_vec())]),
        limits: RuntimeLimits::default(),
    }
}
fn events(server: &mut ServerRuntime) -> Value {
    let RuntimeValue::Json(value) = server.runtime.read_export("internal", "events").unwrap()
    else {
        panic!("Expected events")
    };
    value
}
#[test]
fn kit2_initializes_manifest_env_and_read_before_concurrent_node_loaders() {
    let mut server = ServerRuntime::initialize(request(false), HostOptions::default()).unwrap();
    assert_eq!(server.server_build_id(), "fixture-server-build");
    assert_eq!(
        events(&mut server),
        json!(["building", "private", "public", "manifest", "read", "env"])
    );
    let facts = server.load_nodes().unwrap();
    assert_eq!(facts.len(), 2);
    assert!(facts[0].has_server_load && !facts[0].server_load_callable);
    assert!(facts[0].has_universal_load && facts[0].universal_load_callable);
    assert!(facts[1].has_server_load && !facts[1].has_universal_load);
    let first = events(&mut server);
    assert_eq!(
        first,
        json!([
            "building",
            "private",
            "public",
            "manifest",
            "read",
            "env",
            "node0:start",
            "node1:start",
            "node0:end",
            "node1:end"
        ])
    );
    server.load_nodes().unwrap();
    assert_eq!(events(&mut server), first);
    server
        .runtime
        .native_operation(|session| {
            let ns = session.namespace("internal")?;
            let state = object(get(session, &ns, "state")?, "state")?;
            let private = get(session, &state, "private")?;
            let public = get(session, &state, "public")?;
            assert_eq!(
                session.normalize(private)?,
                RuntimeValue::Json(json!({"SECRET":"private"}))
            );
            assert_eq!(
                session.normalize(public)?,
                RuntimeValue::Json(json!({"PUBLIC_NAME":"public"}))
            );
            Ok(Value::Null)
        })
        .unwrap();
}
#[test]
fn kit3_awaits_configure_without_invoking_init_hooks() {
    let mut server = ServerRuntime::initialize(request(true), HostOptions::default()).unwrap();
    assert_eq!(
        events(&mut server),
        json!(["configure:start", "configure:end"])
    );
    server.load_nodes().unwrap();
    assert_eq!(
        events(&mut server),
        json!([
            "configure:start",
            "configure:end",
            "node0:start",
            "node1:start",
            "node0:end",
            "node1:end"
        ])
    );
}
#[test]
fn failed_node_asset_read_is_not_replaced_by_empty_data() {
    let mut input = request(true);
    input.assets.clear();
    let mut server = ServerRuntime::initialize(input, HostOptions::default()).unwrap();
    let error = server.load_nodes().unwrap_err();
    assert!(
        error.message.contains("Missing snapshot server asset"),
        "{error}"
    );
    assert_eq!(server.load_nodes().unwrap_err().code, "runtime_failed");
}
#[test]
fn invalid_node_index_and_invalid_asset_paths_are_rejected() {
    let mut input = request(true);
    let source = &mut input.snapshot.modules.get_mut("manifest").unwrap().source;
    *source = source.replace("index:1", "index:-1");
    let mut server = ServerRuntime::initialize(input, HostOptions::default()).unwrap();
    assert_eq!(server.load_nodes().unwrap_err().code, "kit_runtime_input");
    let mut input = request(false);
    input.assets.insert("../secret".into(), vec![]);
    assert!(ServerRuntime::initialize(input, HostOptions::default()).is_err());
}
#[test]
fn compact_manifest_preserves_original_node_identity() {
    let mut input = request(true);
    let source = &mut input.snapshot.modules.get_mut("manifest").unwrap().source;
    *source = source.replace("index:1", "index:7");
    let mut server = ServerRuntime::initialize(input, HostOptions::default()).unwrap();
    let facts = server.load_nodes().unwrap();
    assert_eq!(
        facts.iter().map(|n| n.index).collect::<Vec<_>>(),
        vec![0, 7]
    );
    let mut input = request(true);
    let source = &mut input.snapshot.modules.get_mut("manifest").unwrap().source;
    *source = source.replace("index:1", "index:0");
    let mut server = ServerRuntime::initialize(input, HostOptions::default()).unwrap();
    assert!(
        server
            .load_nodes()
            .unwrap_err()
            .message
            .contains("Duplicate")
    );
}
#[test]
fn rejected_configuration_prevents_node_loading() {
    let mut input = request(true);
    let source = &mut input.snapshot.modules.get_mut("internal").unwrap().source;
    *source = source.replace(
        "events.push('configure:end');",
        "throw Error('configuration rejected');",
    );
    let error = match ServerRuntime::initialize(input, HostOptions::default()) {
        Ok(_) => panic!("Expected rejection"),
        Err(error) => error,
    };
    assert!(error.message.contains("configuration rejected"), "{error}");
}

#[test]
fn loader_rejection_uses_promise_all_settlement_order() {
    let mut input = request(true);
    input.snapshot.modules.get_mut("manifest").unwrap().source="export const manifest={nodes:[async()=>{await 0;await 0;throw Error('slow first')},async()=>{throw Error('fast second')}]};".into();
    let mut server = ServerRuntime::initialize(input, HostOptions::default()).unwrap();
    let error = server.load_nodes().unwrap_err();
    assert!(error.message.contains("fast second"), "{error}");
}
