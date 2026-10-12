use super::*;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use ts_js_runtime::{
    HostCapability, HostSnapshot, ModuleSource, Resolution, Runtime, RuntimeLimits, RuntimeValue,
};

fn runtime(kit3: bool, failure: &str) -> Runtime {
    let state = r#"
export const events=[];
export const state={};
export const manifest={assets:new Set(['asset.txt'])};
export function read(){return new ReadableStream({start(c){c.enqueue(new TextEncoder().encode('asset'));c.close()}})}
export function request(){return new Request('https://example.test/path')}
export function options(){return {prerendering:{dependencies:new Map(),remote_responses:new Map(),resolved_route_ids:new Map()},getClientAddress(){throw Error('Cannot read clientAddress during prerendering')},read}}
export function set_building(){events.push('building');state.building=true}
export function set_prerendering(){events.push('prerendering');state.prerendering=true}
export function set_env(env){events.push('env');state.env=env}
export function set_private_env(env){state.private=env;events.push('private')}
export function set_public_env(env){state.public=env;events.push('public')}
export function set_manifest(value){if(value!==manifest)throw Error('manifest identity');events.push('manifest')}
export function set_read_implementation(value){if(value!==read)throw Error('read identity');events.push('read')}
"#;
    let body = if kit3 {
        r#"
export async function configure(config){
  if(this!==undefined)throw Error('configure receiver');
  events.push('configure:start');await 0;
  if(!config.building||!config.prerendering||config.manifest!==manifest||config.read!==read)throw Error('configuration identity');
  state.env=config.env;events.push('configure:end');
  return {init,respond};
}
async function init(){
  if(this!==undefined||arguments.length)throw Error('init receiver/arguments');
  events.push('init:start');await 0;
  if(state.env.SECRET!=='private')throw Error('init environment');
  FAIL_INIT
  state.initialized=true;events.push('init:end');
}
async function respond(request,options){
  if(this!==undefined)throw Error('respond receiver');
  return respond_common(request,options);
}
"#
    } else {
        r#"
if(!state.building||!state.prerendering||state.env.SECRET!=='private')throw Error('server import order');
events.push('server:import');
export class Server {
 constructor(value){if(value!==manifest)throw Error('manifest constructor identity');this.valid=true;events.push('construct')}
 async init(options){
  if(!this.valid||options.read!==read||options.env.SECRET!=='private')throw Error('init receiver/options');
  if(state.private.PUBLIC_NAME!==undefined||state.public.SECRET!==undefined)throw Error('env prefix filtering');
  events.push('init:start');await 0;
  FAIL_INIT
  state.initialized=true;events.push('init:end');
 }
 async respond(request,options){if(!this.valid)throw Error('respond receiver');return respond_common(request,options)}
}
"#
    };
    let common = r#"
async function respond_common(request,options){
 if(!state.initialized)throw Error('dispatch before hooks');
 events.push('respond:start');await 0;
 FAIL_RESPOND
 const text=await new Response(read('asset.txt')).text();
 options.prerendering.dependencies.set('/dependency',{response:new Response('dependency'),body:'dependency'});
 events.push('respond:end');
 return new Response(text+':'+new URL(request.url).pathname,{headers:{'x-sveltekit-routeid':'/path'}});
}
"#;
    let source = format!("import {{events,state,manifest,read}} from 'state';\n{body}\n{common}")
        .replace(
            "FAIL_INIT",
            if failure == "init" {
                "throw Error('init failed');"
            } else {
                ""
            },
        )
        .replace(
            "FAIL_RESPOND",
            if failure == "respond" {
                "throw Error('respond failed');"
            } else {
                ""
            },
        );
    let source = if failure == "import" {
        "throw Error('server import failed');".into()
    } else {
        source
    };
    Runtime::with_capabilities(
        HostSnapshot {
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
                        source,
                        import_meta_url: "file:///server.js".into(),
                    },
                ),
            ]),
            resolutions: BTreeMap::from([(
                "server".into(),
                BTreeMap::from([("state".into(), Resolution::Module { id: "state".into() })]),
            )]),
        },
        RuntimeLimits::default(),
        &BTreeSet::from([HostCapability::RequestResponse]),
    )
    .unwrap()
}
fn input(session: &mut NativeSession<'_>, kit3: bool) -> Result<RenderInput> {
    let state = session.namespace("state")?;
    Ok(RenderInput {
        profile: if kit3 {
            RenderProfile::Kit3
        } else {
            RenderProfile::Kit2 {
                internal_entry: "state".into(),
                environment_entry: "state".into(),
                public_prefix: "PUBLIC_".into(),
                private_prefix: String::new(),
            }
        },
        server_entry: "server".into(),
        manifest: object(get(session, &state, "manifest")?, "manifest")?,
        environment: BTreeMap::from([
            ("SECRET".into(), "private".into()),
            ("PUBLIC_NAME".into(), "public".into()),
        ]),
        read: get(session, &state, "read")?,
    })
}
fn initialize(runtime: &mut Runtime, kit3: bool) -> Result<Renderer> {
    let mut renderer = None;
    runtime.native_operation(|session| {
        let input = input(session, kit3)?;
        renderer = Some(Renderer::initialize(session, input)?);
        Ok(Value::Null)
    })?;
    Ok(renderer.unwrap())
}
fn args(session: &mut NativeSession<'_>) -> Result<(JsValue, JsObject)> {
    let state = session.namespace("state")?;
    let request = invoke(session, &state, "request", &[])?;
    let options = object(invoke(session, &state, "options", &[])?, "options")?;
    Ok((request, options))
}
fn events(runtime: &mut Runtime) -> Value {
    let RuntimeValue::Json(events) = runtime.read_export("state", "events").unwrap() else {
        panic!("events array")
    };
    events
}

#[test]
fn kit2_constructs_and_awaits_hooks_before_retaining_response_and_dependency_map() {
    roundtrip(false);
}
#[test]
fn kit3_awaits_configure_and_calls_unbound_init_and_respond() {
    roundtrip(true);
}
fn roundtrip(kit3: bool) {
    let mut runtime = runtime(kit3, "");
    let renderer = initialize(&mut runtime, kit3).unwrap();
    assert_eq!(
        events(&mut runtime),
        if kit3 {
            json!(["configure:start", "configure:end", "init:start", "init:end"])
        } else {
            json!([
                "building",
                "prerendering",
                "env",
                "server:import",
                "private",
                "public",
                "manifest",
                "read",
                "construct",
                "init:start",
                "init:end"
            ])
        }
    );
    runtime
        .native_operation(|session| {
            let (request, options) = args(session)?;
            let prerendering = object(get(session, &options, "prerendering")?, "prerendering")?;
            let expected = get(session, &prerendering, "dependencies")?;
            let result = renderer.dispatch(session, request, options)?;
            let actual: JsValue = result.dependencies.clone().into();
            assert!(JsValue::same_value(&expected, &actual));
            assert_eq!(
                result.dependencies.get_size(session.context())?.as_number(),
                Some(1.0)
            );
            assert_eq!(
                get(session, &result.response, "bodyUsed")?.as_boolean(),
                Some(false)
            );
            let text = get(session, &result.response, "text")?;
            let pending = session.call(&text, &result.response.into(), &[])?;
            let text = session.settle(pending)?;
            assert_eq!(
                session.normalize(text)?,
                RuntimeValue::Json(json!("asset:/path"))
            );
            Ok(Value::Null)
        })
        .unwrap();
    let events = events(&mut runtime);
    assert_eq!(
        &events.as_array().unwrap()[events.as_array().unwrap().len() - 2..],
        &[json!("respond:start"), json!("respond:end")]
    );
}

#[test]
fn import_and_async_hook_failures_propagate_and_poison_session() {
    for kit3 in [false, true] {
        for failure in ["import", "init"] {
            let mut runtime = runtime(kit3, failure);
            let error = initialize(&mut runtime, kit3)
                .err()
                .expect("Initialization failure");
            assert!(error.message.contains("failed"), "{error}");
            assert_eq!(
                runtime.evaluate("state").unwrap_err().code,
                "runtime_failed"
            );
        }
    }
}

#[test]
fn rejected_async_respond_is_not_a_successful_response() {
    for kit3 in [false, true] {
        let mut runtime = runtime(kit3, "respond");
        let renderer = initialize(&mut runtime, kit3).unwrap();
        let error = runtime
            .native_operation(|session| {
                let (request, options) = args(session)?;
                renderer.dispatch(session, request, options)?;
                Ok(Value::Null)
            })
            .unwrap_err();
        assert!(error.message.contains("respond failed"));
        assert_eq!(
            runtime.evaluate("state").unwrap_err().code,
            "runtime_failed"
        );
    }
}

#[test]
fn metadata_and_render_initialization_require_separate_sessions() {
    let mut runtime = runtime(true, "");
    runtime
        .native_operation(|session| {
            claim_metadata_session(session)?;
            Ok(Value::Null)
        })
        .unwrap();
    assert!(
        initialize(&mut runtime, true)
            .err()
            .unwrap()
            .message
            .contains("fresh runtime")
    );
    let mut runtime = self::runtime(false, "");
    initialize(&mut runtime, false).unwrap();
    assert!(
        initialize(&mut runtime, false)
            .err()
            .unwrap()
            .message
            .contains("fresh runtime")
    );
}

#[test]
fn renderer_rejects_cross_realm_calls_and_non_requests() {
    let mut first = runtime(true, "");
    let renderer = initialize(&mut first, true).unwrap();
    let mut second = runtime(true, "");
    let error = second
        .native_operation(|session| {
            let (request, options) = args(session)?;
            renderer.dispatch(session, request, options)?;
            Ok(Value::Null)
        })
        .unwrap_err();
    assert!(error.message.contains("different runtime realm"));
    let error = first
        .native_operation(|session| {
            let (_, options) = args(session)?;
            renderer.dispatch(session, JsValue::undefined(), options)?;
            Ok(Value::Null)
        })
        .unwrap_err();
    assert!(error.message.contains("native Request"));
}

#[test]
fn configure_defers_construction_and_hooks_until_one_explicit_start() {
    for kit3 in [false, true] {
        let mut runtime = runtime(kit3, "");
        let mut retained = None;
        runtime
            .native_operation(|session| {
                let input = input(session, kit3)?;
                retained = Some(Renderer::configure(session, input)?);
                Ok(Value::Null)
            })
            .unwrap();
        assert!(
            !events(&mut runtime)
                .as_array()
                .unwrap()
                .iter()
                .any(|event| event == "construct" || event == "init:start")
        );
        let mut renderer = retained.unwrap();
        runtime
            .native_operation(|session| {
                renderer.start(session)?;
                renderer.start(session)?;
                Ok(Value::Null)
            })
            .unwrap();
        assert_eq!(
            events(&mut runtime)
                .as_array()
                .unwrap()
                .iter()
                .filter(|event| *event == "init:start")
                .count(),
            1
        );
    }
}

#[test]
fn pending_dispatch_starts_two_responses_before_draining_either() {
    for kit3 in [false, true] {
        let mut runtime = runtime(kit3, "");
        let renderer = initialize(&mut runtime, kit3).unwrap();
        runtime
            .native_operation(|session| {
                // Resolve fixture namespaces/arguments before starting either task:
                // namespace loading itself may drain jobs on first evaluation.
                let (first_request, first_options) = args(session)?;
                let (second_request, second_options) = args(session)?;
                let state = session.namespace("state")?;
                let first = renderer.dispatch_pending(session, first_request, first_options)?;
                let second = renderer.dispatch_pending(session, second_request, second_options)?;
                assert!(matches!(first.response.state(), PromiseState::Pending));
                assert!(matches!(second.response.state(), PromiseState::Pending));
                let log = pending_events(session, &state)?;
                assert_eq!(&log[log.len() - 2..], &["respond:start", "respond:start"]);
                assert!(!log.iter().any(|v| v == "respond:end"));
                session.settle(first.response.clone().into())?;
                let first = first.finish(session)?;
                let second = second.finish(session)?;
                assert!(!JsValue::same_value(
                    &first.dependencies.into(),
                    &second.dependencies.into()
                ));
                Ok(Value::Null)
            })
            .unwrap();
    }
}

fn pending_runtime(mode: &str) -> Runtime {
    let source = r#"
export const events=[];
export const reason={marker:Symbol('opaque rejection')};
export const manifest={};
export function read(){}
export function request(){return new Request('https://example.test/')}
export function options(){return {prerendering:{dependencies:new Map()}}}
export function poison(){Promise.resolve=()=>{throw Error('mutable Promise.resolve')}}
export function configure(){return {init(){},respond(request,options){
 if(this!==undefined)throw Error('respond receiver');
 events.push('start');
 const response=new Response('sync');
 options.prerendering.dependencies.set('/seen',{response});
 switch(MODE){
 case 'throw':throw reason;
 case 'reject':return Promise.reject(reason);
 case 'thenable':return {get then(){events.push('get:then');return function(resolve){events.push('then');resolve(response)}}};
 case 'getter':return {get then(){throw reason}};
 case 'fake':return Object.create(Response.prototype);
 case 'pending':return new Promise(()=>{});
 default:return response;
 }
}}}
"#.replace("MODE", &format!("'{mode}'"));
    Runtime::with_capabilities(
        HostSnapshot {
            modules: BTreeMap::from([(
                "state".into(),
                ModuleSource {
                    source,
                    import_meta_url: "file:///pending.js".into(),
                },
            )]),
            resolutions: BTreeMap::new(),
        },
        RuntimeLimits::default(),
        &BTreeSet::from([HostCapability::RequestResponse]),
    )
    .unwrap()
}
// NativeSession::normalize deliberately drains jobs; observe this plain fixture
// array directly so assertions cannot accidentally settle pending dispatches.
fn pending_events(session: &mut NativeSession<'_>, state: &JsObject) -> Result<Vec<String>> {
    let array = boa_engine::object::builtins::JsArray::from_object(object(
        get(session, state, "events")?,
        "events",
    )?)?;
    (0..array.length(session.context())?)
        .map(|i| {
            Ok(array
                .get(i, session.context())?
                .as_string()
                .unwrap()
                .to_std_string()
                .unwrap())
        })
        .collect()
}
fn pending_renderer(session: &mut NativeSession<'_>, start: bool) -> Result<Renderer> {
    let state = session.namespace("state")?;
    let input = RenderInput {
        profile: RenderProfile::Kit3,
        server_entry: "state".into(),
        manifest: object(get(session, &state, "manifest")?, "manifest")?,
        environment: BTreeMap::new(),
        read: get(session, &state, "read")?,
    };
    let mut renderer = Renderer::configure(session, input)?;
    if start {
        renderer.start(session)?;
    }
    Ok(renderer)
}

#[test]
fn pending_dispatch_adopts_thenables_and_supports_synchronous_responses() {
    for mode in ["sync", "thenable"] {
        let mut runtime = pending_runtime(mode);
        runtime
            .native_operation(|session| {
                let renderer = pending_renderer(session, true)?;
                let (request, options) = args(session)?;
                let state = session.namespace("state")?;
                invoke(session, &state, "poison", &[])?;
                let expected = object(get(session, &options, "prerendering")?, "prerendering")?;
                let expected = get(session, &expected, "dependencies")?;
                let pending = renderer.dispatch_pending(session, request, options)?;
                assert_eq!(
                    pending_events(session, &state)?,
                    if mode == "thenable" {
                        vec!["start", "get:then"]
                    } else {
                        vec!["start"]
                    }
                );
                if mode == "thenable" {
                    assert!(matches!(pending.response.state(), PromiseState::Pending));
                } else {
                    assert!(matches!(
                        pending.response.state(),
                        PromiseState::Fulfilled(_)
                    ));
                }
                session.settle(pending.response.clone().into())?;
                let result = pending.finish(session)?;
                assert!(JsValue::same_value(&expected, &result.dependencies.into()));
                assert_eq!(
                    get(session, &result.response, "bodyUsed")?.as_boolean(),
                    Some(false)
                );
                Ok(Value::Null)
            })
            .unwrap();
    }
}

#[test]
fn pending_dispatch_preserves_actual_thrown_and_rejected_values() {
    for mode in ["throw", "reject", "getter"] {
        let mut runtime = pending_runtime(mode);
        runtime
            .native_operation(|session| {
                let renderer = pending_renderer(session, true)?;
                let (request, options) = args(session)?;
                let state = session.namespace("state")?;
                let reason = get(session, &state, "reason")?;
                let pending = renderer.dispatch_pending(session, request, options)?;
                let PromiseState::Rejected(actual) = pending.response.state() else {
                    panic!("expected rejected promise")
                };
                assert!(JsValue::same_value(&reason, &actual));
                assert!(pending.finish(session).is_err());
                Ok(Value::Null)
            })
            .unwrap();
    }
}

#[test]
fn pending_dispatch_guards_lifecycle_request_response_and_unsettled_receipts() {
    for mode in ["unstarted", "request", "fake", "pending"] {
        let mut runtime = pending_runtime(mode);
        runtime
            .native_operation(|session| {
                let renderer = pending_renderer(session, mode != "unstarted")?;
                let (mut request, options) = args(session)?;
                if mode == "request" {
                    request = JsValue::undefined();
                }
                let result = renderer.dispatch_pending(session, request, options);
                let error = match mode {
                    "unstarted" | "request" => result.err().unwrap(),
                    _ => result.unwrap().finish(session).err().unwrap(),
                };
                assert!(
                    error.message.contains(match mode {
                        "unstarted" => "started",
                        "request" => "native Request",
                        "fake" => "native Response",
                        _ => "not settled",
                    }),
                    "{error}"
                );
                Ok(Value::Null)
            })
            .unwrap();
    }
}

#[test]
fn pending_dispatch_and_completion_reject_cross_realm_handles() {
    let mut first = pending_runtime("sync");
    let mut renderer = None;
    let mut pending = None;
    first
        .native_operation(|session| {
            let created = pending_renderer(session, true)?;
            let (request, options) = args(session)?;
            pending = Some(created.dispatch_pending(session, request, options)?);
            renderer = Some(created);
            Ok(Value::Null)
        })
        .unwrap();
    let mut second = pending_runtime("sync");
    second
        .native_operation(|session| {
            let (request, options) = args(session)?;
            assert!(
                renderer
                    .unwrap()
                    .dispatch_pending(session, request, options)
                    .err()
                    .unwrap()
                    .message
                    .contains("different runtime realm")
            );
            assert!(
                pending
                    .unwrap()
                    .finish(session)
                    .err()
                    .unwrap()
                    .message
                    .contains("different runtime realm")
            );
            Ok(Value::Null)
        })
        .unwrap();
}

// These values are held in the Rust context host-data map, not hidden inside
// an untraced JS closure capture. Callbacks remove/restore the host state to
// avoid borrowing Context while executing application code.
struct ContinuationDispatch {
    renderer: Renderer,
    request: JsValue,
    options: JsObject,
    expected_map: JsValue,
    pending: Option<PendingDispatch>,
    finished: bool,
}
#[test]
fn context_dispatch_and_finish_run_inside_native_promise_continuations() {
    use boa_engine::NativeFunction;
    for mode in ["thenable", "throw", "getter", "fake"] {
        let mut runtime = pending_runtime(mode);
        runtime
            .native_operation(|session| {
                let renderer = pending_renderer(session, true)?;
                let (request, options) = args(session)?;
                let prerendering = object(get(session, &options, "prerendering")?, "options")?;
                let expected_map = get(session, &prerendering, "dependencies")?;
                let state = session.namespace("state")?;
                let reason = get(session, &state, "reason")?;
                session.context().insert_data(ContinuationDispatch {
                    renderer,
                    request,
                    options,
                    expected_map,
                    pending: None,
                    finished: false,
                });
                let begin = NativeFunction::from_fn_ptr(|_, _, context| {
                    let mut state = context.remove_data::<ContinuationDispatch>().unwrap();
                    let pending = state.renderer.dispatch_pending_context(
                        context,
                        state.request.clone(),
                        state.options.clone(),
                    );
                    let result = pending.map(|pending| {
                        let promise = pending.response.clone();
                        state.pending = Some(pending);
                        promise
                    });
                    context.insert_data(*state);
                    Ok(result?.into())
                })
                .to_js_function(session.context().realm());
                let finish = NativeFunction::from_fn_ptr(|_, _, context| {
                    let mut state = context.remove_data::<ContinuationDispatch>().unwrap();
                    let result = state.pending.take().unwrap().finish_context(context);
                    if let Ok(result) = &result {
                        assert!(JsValue::same_value(
                            &state.expected_map,
                            &result.dependencies.clone().into()
                        ));
                        assert!(web_fetch::is_response(&result.response.clone().into()));
                    }
                    state.finished = true;
                    context.insert_data(*state);
                    Ok(result?.response.into())
                })
                .to_js_function(session.context().realm());
                let seed = JsPromise::resolve(JsValue::undefined(), session.context())?;
                let started = seed.then_intrinsic(Some(begin), None, session.context())?;
                let completed = started.then_intrinsic(
                    Some(finish.clone()),
                    Some(finish),
                    session.context(),
                )?;
                assert!(
                    !session
                        .context()
                        .get_data::<ContinuationDispatch>()
                        .unwrap()
                        .finished
                );
                let settled = session.settle(completed.clone().into());
                assert!(
                    session
                        .context()
                        .get_data::<ContinuationDispatch>()
                        .unwrap()
                        .finished
                );
                if mode == "thenable" {
                    assert!(web_fetch::is_response(&settled?));
                } else {
                    assert!(settled.is_err());
                    let PromiseState::Rejected(actual) = completed.state() else {
                        panic!("expected rejection")
                    };
                    if mode == "fake" {
                        assert!(
                            JsError::from_opaque(actual)
                                .try_native(session.context())
                                .unwrap()
                                .message()
                                .contains("native Response")
                        );
                    } else {
                        assert!(JsValue::same_value(&reason, &actual));
                    }
                }
                session.context().remove_data::<ContinuationDispatch>();
                Ok(Value::Null)
            })
            .unwrap();
    }
}
