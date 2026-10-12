use super::*;
use ts_js_runtime::{HostSnapshot, ModuleSource, Resolution, RuntimeLimits, RuntimeValue};

fn input(kit3: bool, scenario: &str) -> PrerenderInput {
    let state = r#"
export const events=[]; export const state={}; export const manifest={assets:new Set()};
export function set_building(){state.building=true;events.push('building')}
export function set_prerendering(){state.prerendering=true;events.push('prerendering')}
export function set_env(env){state.env=env;events.push('env')}
export function set_private_env(env){state.private=env}
export function set_public_env(env){state.public=env}
export function set_manifest(value){if(value!==manifest)throw Error('manifest')}
export function set_read_implementation(read){state.read=read}
"#;
    let server = r#"
import {events,state,manifest} from 'state';
const NativeResponse=Response, NativeURL=URL, NativeArray=Uint8Array;
const scenario=SCENARIO;
const poison=()=>{throw Error('Overridden host access')};
globalThis.Request=poison; globalThis.URL=poison; globalThis.decodeURI=poison;
Object.defineProperty(NativeURL.prototype,'pathname',{get:poison,configurable:true});
let remote,ids,priorDependencies;
async function hooks(){
 events.push('init:start');await 0;
 if(scenario==='init-fail')throw Error('hook failed');
 const text=await new NativeResponse(state.read('init.txt')).text();
 if(text!=='server-init'||state.env.VALUE!=='provided')throw Error('init snapshot');
 events.push('init:end');
}
function response(bytes,headers=[]){
 const value=new NativeResponse(bytes,{status:201,statusText:'caf\u00e9',headers});
 for(const key of ['status','statusText','headers','body','bodyUsed','arrayBuffer']){
  Object.defineProperty(value,key,{get:poison});
 }
 return value;
}
async function respond(request,options){
 events.push('respond');
 if(request.method!=='GET')throw Error('GET required');
 const p=options.prerendering;
 if(remote&&remote!==p.remote_responses)throw Error('remote map not retained');remote=p.remote_responses;
 if(priorDependencies===p.dependencies)throw Error('dependency map reused');priorDependencies=p.dependencies;
 if(KIT3){if(!(p.resolved_route_ids instanceof Set))throw Error('route IDs must be Set');if(ids&&ids!==p.resolved_route_ids)throw Error('route set not retained');ids=p.resolved_route_ids;ids.add('/test')}
 else if('resolved_route_ids' in p)throw Error('Kit2 options');
 if(scenario==='client-address')options.getClientAddress();
 if(scenario==='missing')options.read('missing.txt');
 if(scenario==='null')return new NativeResponse(null,{status:204});
 if(scenario==='response-getters') {
  for (const supplied of [false,true]) {
   let reads=0;
   p.dependencies.set('/getter-'+supplied,{
    get response(){
     reads++;events.push('response:'+supplied+':'+reads);
     // The first response supplies only headers; the fallback response only
     // bytes. Neither route-id should be decoded at these stages.
     return new NativeResponse('body-'+reads, {status:200+reads,
      headers:[['x-sveltekit-routeid',reads===(supplied?2:3)?'/saved%20route':'/%GG']]});
    },
    get body(){events.push('body:'+supplied);return supplied?'override':null}
   });
  }
  return new NativeResponse('main');
 }
 if(scenario==='staged') {
  p.dependencies.set('/dependency',{
   response: response(new ReadableStream({pull(c){
    events.push('staged:dependency');c.enqueue(options.read('saved-main'));c.close();
    p.dependencies.set('/late',{response:response(new ReadableStream({pull(c){
     events.push('staged:late');c.enqueue(options.read('saved-dependency'));c.close();
    }},{highWaterMark:0}))});
   }},{highWaterMark:0})),
   get body(){events.push('staged:body-getter');return null}
  });
  return response(new ReadableStream({pull(c){events.push('staged:main');c.enqueue(new NativeArray([7,8]));c.close()}},{highWaterMark:0}));
 }

 if(scenario==='poisoned-body') { const r=new NativeResponse('used');await r.text();return r; }
 const visits=remote.get('visits')??0;remote.set('visits',visits+1);
 const staticBytes=options.read('static.txt');
 const appBytes=options.read('_app/asset.bin');
 if(!(staticBytes instanceof NativeArray)||!(appBytes instanceof NativeArray))throw Error('sync reads must be bytes');
 const main=new ReadableStream({pull(controller){
  events.push('main:pull');
  if(scenario==='main-fail')throw Error('main failed');
  const poisonBody=new ReadableStream({pull(){throw Error('Supplied body ignored')}});
  p.dependencies.set('/empty',{response:response(poisonBody),body:''});
  const bytes=new NativeArray([99,0,255,99]);
  p.dependencies.set('/a%2Fb/%F0%9F%98%80?q=1#fragment',{response:response('ignored',[['x-sveltekit-routeid','/a%2Fb/%F0%9F%98%80']]),body:bytes.subarray(1,3)});
  p.dependencies.set('/fallback',{response:response(new ReadableStream({pull(c){
   events.push('dependency:pull');p.dependencies.set('/late',{response:response('ignored'),body:'late\ud800'});
   c.enqueue(new NativeArray([3,128]));c.close();
  }},{highWaterMark:0})),body:null});
  if(scenario==='bad-decode')p.dependencies.set('/%GG',{response:response(''),body:''});
  if(scenario==='bad-type')p.dependencies.set('/bad',{response:response(''),body:new Int16Array([1])});
  controller.enqueue(new NativeArray([...staticBytes,...appBytes,visits]));controller.close();
 }});
 return response(main,[['set-cookie','a=1'],['set-cookie','b=2'],['x-sveltekit-routeid','/root%2Fitem/%F0%9F%98%80']]);
}
PROFILE
"#.replace("SCENARIO", &serde_json::to_string(scenario).unwrap())
        .replace("KIT3", if kit3 { "true" } else { "false" })
        .replace("PROFILE", if kit3 {
            r#"export async function configure(config){events.push('configure:start');await 0;if(!config.building||!config.prerendering||config.manifest!==manifest)throw Error('configure');state.read=config.read;state.env=config.env;events.push('configure:end');return {init:hooks,respond}}"#
        } else {
            r#"if(!state.building||!state.prerendering||!state.env)throw Error('module order');export class Server{constructor(m){if(m!==manifest)throw Error('manifest');events.push('construct')}async init(options){state.read=options.read;state.env=options.env;await hooks()}respond(request,options){return respond(request,options)}}"#
        });
    PrerenderInput {
        bootstrap: BootstrapRequest {
            server_build_id: "server-build".into(),
            snapshot: HostSnapshot {
                modules: BTreeMap::from([
                    (
                        "state".into(),
                        ModuleSource {
                            source: state.into(),
                            import_meta_url: "file:///state.js".into(),
                        },
                    ),
                    (
                        "server".into(),
                        ModuleSource {
                            source: server,
                            import_meta_url: "file:///server.js".into(),
                        },
                    ),
                ]),
                resolutions: BTreeMap::from([(
                    "server".into(),
                    BTreeMap::from([("state".into(), Resolution::Module { id: "state".into() })]),
                )]),
            },
            manifest_entry: "state".into(),
            initialization: if kit3 {
                Initialization::Kit3 {
                    server: "server".into(),
                }
            } else {
                Initialization::Kit2 {
                    internal: "state".into(),
                    environment: "state".into(),
                    public_prefix: "PUBLIC_".into(),
                    private_prefix: String::new(),
                }
            },
            environment: BTreeMap::from([("VALUE".into(), "provided".into())]),
            assets: BTreeMap::from([
                ("init.txt".into(), b"server-init".to_vec()),
                ("_app/asset.bin".into(), vec![0, 255]),
            ]),
            limits: RuntimeLimits::default(),
        },
        server_entry: "server".into(),
        origin: "https://example.test".into(),
        base: "/base".into(),
        app_dir: "_app".into(),
        static_assets: BTreeMap::from([
            ("static.txt".into(), vec![1, 2]),
            ("_app/asset.bin".into(), vec![99]),
        ]),
        limits: PrerenderLimits::default(),
    }
}
fn events(session: &mut PrerenderSession) -> Value {
    let RuntimeValue::Json(events) = session.runtime.read_export("state", "events").unwrap() else {
        panic!("events")
    };
    events
}

#[test]
fn both_profiles_defer_hooks_and_collect_main_then_live_dependencies_with_raw_metadata() {
    for kit3 in [false, true] {
        let mut session =
            PrerenderSession::configure(input(kit3, "normal"), HostOptions::default()).unwrap();
        assert!(
            !events(&mut session)
                .as_array()
                .unwrap()
                .iter()
                .any(|event| event == "construct" || event == "init:start")
        );
        session.start().unwrap();
        let started = events(&mut session);
        session.start().unwrap();
        assert_eq!(events(&mut session), started);
        let result = session.visit("/base/%F0%9F%98%80%2Fpage").unwrap();
        assert_eq!(result.decoded_path, "/base/😀%2Fpage");
        assert_eq!(result.server_build_id, "server-build");
        assert_eq!(result.response.status, 201);
        assert_eq!(result.response.status_text, b"caf\xe9");
        assert_eq!(result.response.body, vec![1, 2, 0, 255, 0]);
        assert_eq!(result.response.route_id.as_deref(), Some("/root%2Fitem/😀"));
        assert_eq!(
            &result.response.headers[..2],
            &[
                (b"set-cookie".to_vec(), b"a=1".to_vec()),
                (b"set-cookie".to_vec(), b"b=2".to_vec())
            ]
        );
        assert_eq!(
            result
                .dependencies
                .iter()
                .map(|d| d.path.as_str())
                .collect::<Vec<_>>(),
            vec![
                "/empty",
                "/a%2Fb/%F0%9F%98%80?q=1#fragment",
                "/fallback",
                "/late"
            ]
        );
        assert!(result.dependencies[0].response.body.is_empty());
        assert_eq!(result.dependencies[1].encoded_path, "/a%2Fb/%F0%9F%98%80");
        assert_eq!(result.dependencies[1].decoded_path, "/a%2Fb/😀");
        assert_eq!(result.dependencies[1].response.body, vec![0, 255]);
        assert_eq!(result.dependencies[2].response.body, vec![3, 128]);
        assert_eq!(result.dependencies[3].response.body, "late�".as_bytes());
        let order = events(&mut session);
        let order = order.as_array().unwrap();
        assert!(
            order.iter().position(|v| v == "main:pull").unwrap()
                < order.iter().position(|v| v == "dependency:pull").unwrap()
        );
        session.publish("static.txt".into(), vec![8]).unwrap();
        session.publish("_app/asset.bin".into(), vec![9]).unwrap();
        assert_eq!(
            session.visit("/base/next").unwrap().response.body,
            vec![8, 9, 1]
        );
    }
}

#[test]
fn lifecycle_and_asset_read_failures_poison_only_the_owned_session() {
    let mut early =
        PrerenderSession::configure(input(false, "normal"), HostOptions::default()).unwrap();
    assert!(early.visit("/base").unwrap_err().message.contains("Start"));
    assert_eq!(early.start().unwrap_err().code, "runtime_failed");
    let mut failed =
        PrerenderSession::configure(input(true, "init-fail"), HostOptions::default()).unwrap();
    assert!(failed.start().unwrap_err().message.contains("hook failed"));
    assert_eq!(failed.start().unwrap_err().code, "runtime_failed");
    for scenario in [
        "missing",
        "client-address",
        "main-fail",
        "bad-decode",
        "bad-type",
        "poisoned-body",
    ] {
        let mut session =
            PrerenderSession::configure(input(true, scenario), HostOptions::default()).unwrap();
        session.start().unwrap();
        assert!(session.visit("/base").is_err(), "{scenario}");
        assert_eq!(
            session.visit("/base").unwrap_err().code,
            "runtime_failed",
            "{scenario}"
        );
    }
}

#[test]
fn null_bodies_and_explicit_limits_are_observed() {
    let mut request = input(false, "null");
    request.limits.visits = 2;
    let mut session = PrerenderSession::configure(request, HostOptions::default()).unwrap();
    session.start().unwrap();
    for _ in 0..2 {
        let result = session.visit("/base").unwrap();
        assert_eq!(result.response.status, 204);
        assert!(result.response.body.is_empty());
        assert!(result.dependencies.is_empty());
    }
    assert!(
        session
            .visit("/base")
            .unwrap_err()
            .message
            .contains("visit limit")
    );
    for (bytes, dependencies) in [(1, 100), (1024, 1)] {
        let mut request = input(true, "normal");
        request.limits.visit_bytes = bytes;
        request.limits.dependencies = dependencies;
        let mut session = PrerenderSession::configure(request, HostOptions::default()).unwrap();
        session.start().unwrap();
        assert!(session.visit("/base").is_err());
    }
}

#[test]
fn rejects_ambiguous_origins_asset_names_and_atomic_overlarge_publication() {
    for origin in [
        "file:///tmp",
        "https://example.test/path",
        "https://user:pass@example.test",
        "https://example.test/?query",
    ] {
        let mut request = input(true, "normal");
        request.origin = origin.into();
        assert!(PrerenderSession::configure(request, HostOptions::default()).is_err());
    }
    let mut request = input(false, "normal");
    request.static_assets.insert("../escape".into(), vec![]);
    assert!(PrerenderSession::configure(request, HostOptions::default()).is_err());
    let mut request = input(false, "normal");
    request.limits.asset_bytes = 11;
    let mut session = PrerenderSession::configure(request, HostOptions::default()).unwrap();
    session.publish("static.txt".into(), vec![7]).unwrap();
    assert!(session.publish("static.txt".into(), vec![0; 12]).is_err());
    assert_eq!(session.assets.borrow().published["static.txt"], vec![7]);
}

#[test]
fn staged_visits_publish_saved_bytes_before_dependency_getters_and_streams() {
    for kit3 in [false, true] {
        let mut session =
            PrerenderSession::configure(input(kit3, "staged"), HostOptions::default()).unwrap();
        session.start().unwrap();
        for _ in 0..2 {
            let before = events(&mut session).as_array().unwrap().len();
            let head = session.begin_visit("/base/staged").unwrap();
            assert_eq!(head.response.status, 201);
            assert_eq!(
                events(&mut session).as_array().unwrap()[before..],
                [Value::from("respond")]
            );
            let main = session.read_main(head.token).unwrap();
            assert_eq!(main.body, vec![7, 8]);
            session.publish("saved-main".into(), main.body).unwrap();
            let count = events(&mut session).as_array().unwrap().len();
            let dependency = session.next_dependency(head.token).unwrap().unwrap();
            assert_eq!(dependency.path, "/dependency");
            assert_eq!(
                events(&mut session).as_array().unwrap().len(),
                count,
                "Inspecting headers must not access the body getter"
            );
            let bytes = session.read_dependency(head.token).unwrap().response.body;
            assert_eq!(bytes, vec![7, 8]);
            session
                .publish("saved-dependency".into(), vec![9, 0, 255])
                .unwrap();
            assert_eq!(
                session.next_dependency(head.token).unwrap().unwrap().path,
                "/late"
            );
            assert_eq!(
                session.read_dependency(head.token).unwrap().response.body,
                vec![9, 0, 255]
            );
            assert!(session.next_dependency(head.token).unwrap().is_none());
            assert_eq!(
                events(&mut session).as_array().unwrap()[before..],
                [
                    "respond",
                    "staged:main",
                    "staged:body-getter",
                    "staged:dependency",
                    "staged:late"
                ]
                .map(Value::from)
            );
        }
    }
}
#[test]
fn staged_visit_order_and_tokens_are_enforced_without_reusing_failed_sessions() {
    for action in 0..4 {
        let mut session =
            PrerenderSession::configure(input(false, "staged"), HostOptions::default()).unwrap();
        session.start().unwrap();
        let head = session.begin_visit("/base/").unwrap();
        let result = match action {
            0 => session.read_main(head.token + 1).map(|_| ()),
            1 => session.next_dependency(head.token).map(|_| ()),
            2 => session.read_dependency(head.token).map(|_| ()),
            _ => session.begin_visit("/base/other").map(|_| ()),
        };
        assert!(result.is_err());
        assert_eq!(
            session.read_main(head.token).unwrap_err().code,
            "runtime_failed"
        );
    }
}

#[test]
fn staged_dependency_response_getters_are_reread_for_body_and_save() {
    for kit3 in [false, true] {
        let mut session =
            PrerenderSession::configure(input(kit3, "response-getters"), HostOptions::default())
                .unwrap();
        session.start().unwrap();
        let before = events(&mut session).as_array().unwrap().len();
        let main = session.begin_visit("/base/getters").unwrap();
        session.read_main(main.token).unwrap();
        for (supplied, status, body) in [
            (false, 203, b"body-2".as_slice()),
            (true, 202, b"override".as_slice()),
        ] {
            let head = session.next_dependency(main.token).unwrap().unwrap();
            assert_eq!(head.path, format!("/getter-{supplied}"));
            assert_eq!(head.response.status, 201);
            assert_eq!(head.response.route_id, None);
            let result = session.read_dependency(main.token).unwrap();
            assert_eq!(result.response.status, status);
            assert_eq!(result.response.body, body);
            assert_eq!(result.response.route_id.as_deref(), Some("/saved route"));
        }
        assert!(session.next_dependency(main.token).unwrap().is_none());
        assert_eq!(
            events(&mut session).as_array().unwrap()[before..],
            [
                "respond",
                "response:false:1",
                "body:false",
                "response:false:2",
                "response:false:3",
                "response:true:1",
                "body:true",
                "response:true:2"
            ]
            .map(Value::from)
        );
    }
}

#[test]
fn unwritten_saved_alias_shadows_existing_static_and_server_fallback() {
    for file in ["static.txt", "_app/asset.bin"] {
        let mut session =
            PrerenderSession::configure(input(true, "normal"), HostOptions::default()).unwrap();
        session.start().unwrap();
        session.publish(file.into(), vec![42]).unwrap();
        session.mark_saved_missing(file.into()).unwrap();
        assert!(
            session
                .visit("/base")
                .unwrap_err()
                .message
                .contains("Saved snapshot asset is missing")
        );
    }
    let mut session =
        PrerenderSession::configure(input(false, "normal"), HostOptions::default()).unwrap();
    session.start().unwrap();
    session.mark_saved_missing("static.txt".into()).unwrap();
    session.publish("static.txt".into(), vec![]).unwrap();
    assert_eq!(
        session.visit("/base").unwrap().response.body,
        vec![0, 255, 0]
    );
}
