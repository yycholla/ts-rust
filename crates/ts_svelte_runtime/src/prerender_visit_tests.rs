use super::*;
use crate::server_dispatch::{RenderInput, RenderProfile};
use boa_engine::builtins::promise::PromiseState;
use serde_json::{Value, json};
use std::{cell::RefCell, collections::BTreeSet};
use ts_js_runtime::{
    HostCapability, HostSnapshot, ModuleSource, Runtime, RuntimeLimits, RuntimeValue,
};

const PRELUDE: &str = r#"
export const events=[];
export const reason={marker:Symbol('original failure')};
export const manifest={};
export function read(){}
export function request(){return new Request('https://example.test/')}
export function options(){return {prerendering:{dependencies:new Map()}}}
export function configure(){return {init(){},respond}}
"#;

#[test]
fn body_await_has_the_same_reaction_order_as_native_response_array_buffer() {
    fn run(native: bool, body: &str) -> Value {
        let mut runtime = runtime(&format!(
            r#"
function respond(){{events.push('respond');return new Response({body})}}
export function notify(kind){{events.push(kind)}}
export async function reference(){{
 const response=await respond();notify('main-head');
 await response.arrayBuffer();notify('main-body');notify('crawl-head');
}}
export function ticks(){{let p=Promise.resolve();for(let i=0;i<20;i++)p=p.then(()=>events.push('tick'+i))}}
"#
        ));
        runtime
            .native_operation(|s| {
                let intrinsics = VisitIntrinsics::capture(s.context())?;
                let ns = s.namespace("entry")?;
                let renderer = renderer(s, &ns);
                let promise = if native {
                    let recorder = Rc::new(Recorder {
                        notify: Some(get(&ns, "notify", s.context())?),
                        ..Recorder::default()
                    });
                    let (request, options) = args(&ns, s.context())?;
                    start(
                        &renderer,
                        s.context(),
                        request,
                        options,
                        intrinsics,
                        VisitLimits::default(),
                        recorder,
                    )?
                    .into()
                } else {
                    let reference = get(&ns, "reference", s.context())?;
                    s.call(&reference, &JsValue::undefined(), &[])?
                };
                let ticks = get(&ns, "ticks", s.context())?;
                s.call(&ticks, &JsValue::undefined(), &[])?;
                s.settle(promise)?;
                let events = get(&ns, "events", s.context())?;
                let RuntimeValue::Json(events) = s.normalize(events)? else {
                    panic!("events")
                };
                Ok(events)
            })
            .unwrap()
    }
    for body in [
        "null",
        "'text'",
        "new ReadableStream({async pull(c){await 0;c.enqueue(new Uint8Array([0,255]));c.close()}},{highWaterMark:0})",
    ] {
        assert_eq!(run(true, body), run(false, body), "{body}");
    }
}
fn runtime(source: &str) -> Runtime {
    Runtime::with_capabilities(
        HostSnapshot {
            modules: BTreeMap::from([(
                "entry".into(),
                ModuleSource {
                    source: format!("{PRELUDE}\n{source}"),
                    import_meta_url: "file:///visitor.js".into(),
                },
            )]),
            resolutions: BTreeMap::new(),
        },
        RuntimeLimits::default(),
        &BTreeSet::from([HostCapability::RequestResponse]),
    )
    .unwrap()
}
fn renderer(
    session: &mut ts_js_runtime::native_session::NativeSession<'_>,
    ns: &JsObject,
) -> Renderer {
    let input = RenderInput {
        profile: RenderProfile::Kit3,
        server_entry: "entry".into(),
        manifest: object(get(ns, "manifest", session.context()).unwrap(), "manifest").unwrap(),
        environment: BTreeMap::new(),
        read: get(ns, "read", session.context()).unwrap(),
    };
    Renderer::initialize(session, input).unwrap()
}
fn args(ns: &JsObject, context: &mut Context) -> JsResult<(JsValue, JsObject)> {
    let request = get(ns, "request", context)?.as_callable().unwrap().call(
        &JsValue::undefined(),
        &[],
        context,
    )?;
    let options = get(ns, "options", context)?.as_callable().unwrap().call(
        &JsValue::undefined(),
        &[],
        context,
    )?;
    Ok((request, object(options, "options")?))
}
#[derive(Default)]
struct Recorder {
    log: RefCell<Vec<Value>>,
    notify: Option<JsValue>,
    failure: Option<(&'static str, JsValue)>,
}
impl VisitObserver for Recorder {
    fn event(&self, event: VisitEvent, context: &mut Context) -> JsResult<()> {
        let (kind, response, dependencies, details) = match event {
            VisitEvent::MainHead {
                response,
                dependencies,
                head,
            } => ("main-head", response, dependencies, json!({"head":head})),
            VisitEvent::MainBody {
                response,
                dependencies,
                body,
            } => ("main-body", response, dependencies, json!({"body":body})),
            VisitEvent::DependencyHead {
                response,
                dependencies,
                head,
            } => (
                "dependency-head",
                response,
                dependencies,
                json!({"head":head}),
            ),
            VisitEvent::DependencyBody {
                response,
                dependencies,
                body,
            } => (
                "dependency-body",
                response,
                dependencies,
                json!({"body":body}),
            ),
            VisitEvent::CrawlHead {
                response,
                dependencies,
                head,
            } => ("crawl-head", response, dependencies, json!({"head":head})),
        };
        self.log
            .borrow_mut()
            .push(json!({"kind":kind,"details":details}));
        if let Some(notify) = &self.notify {
            notify.as_callable().unwrap().call(
                &JsValue::undefined(),
                &[
                    JsString::from(kind).into(),
                    dependencies.into(),
                    response.into(),
                ],
                context,
            )?;
        }
        if let Some((phase, reason)) = &self.failure
            && *phase == kind
        {
            return Err(JsError::from_opaque(reason.clone()));
        }
        Ok(())
    }
}

#[test]
fn ordered_publication_live_dependencies_overrides_and_fresh_crawl_headers() {
    let mut runtime = runtime(
        r#"
let main,published=false,inspected=false,added=false;
export function notify(kind,dependencies,response){
 events.push(kind);
 if(kind==='main-body'){
  published=true;
  dependencies.set('/inserted',{response:new Response('unused'),body:''});
 }
 if(kind==='dependency-head')inspected=true;
 if(kind==='dependency-body'&&!added){
  added=true;
  dependencies.set('/late',{response:new Response('unused'),body:'late\ud800'});
  main.headers.set('x-phase','after-dependency');
 }
}
function respond(request,options){
 events.push('respond');const dependencies=options.prerendering.dependencies;
 const dep=new Response(new ReadableStream({pull(c){
  if(!published||!inspected)throw Error('publication/inspection order');
  events.push('dependency:pull');c.enqueue(new Uint8Array([0,255]));c.close();
 }},{highWaterMark:0}),{headers:[['x-sveltekit-routeid','/a%2Fb/%F0%9F%98%80']]});
 dependencies.set('/a%2Fb/%F0%9F%98%80?q=ignored#fragment',{
  get response(){events.push('dependency:response');return dep},
  get body(){if(!inspected)throw Error('body getter before head');events.push('dependency:body');return null}
 });
 const bytes=new Uint8Array([99,2,128,99]);
 dependencies.set('/slice',{response:new Response(new ReadableStream({pull(){throw Error('override must prevent read')}},{highWaterMark:0})),body:bytes.subarray(1,3)});
 main=new Response(new ReadableStream({pull(c){
  events.push('main:pull');main.headers.set('x-phase','after-main');main.headers.set('x-sveltekit-routeid','/saved');
  c.enqueue(new Uint8Array([77]));c.close();
 }},{highWaterMark:0}),{status:201,statusText:'caf\u00e9',headers:[['x-phase','before'],['x-sveltekit-routeid','/page']]});
 return main;
}
const poison=()=>{throw Error('overridden global')};
globalThis.URL=poison;globalThis.decodeURI=poison;
"#,
    );
    let result = runtime
        .native_operation(|s| {
            let intrinsics = VisitIntrinsics::capture(s.context())?;
            let ns = s.namespace("entry")?;
            let renderer = renderer(s, &ns);
            let recorder = Rc::new(Recorder {
                notify: Some(get(&ns, "notify", s.context())?),
                ..Recorder::default()
            });
            let (request, options) = args(&ns, s.context())?;
            let promise = start(
                &renderer,
                s.context(),
                request,
                options,
                intrinsics,
                VisitLimits::default(),
                recorder.clone(),
            )?;
            assert!(matches!(promise.state(), PromiseState::Pending));
            assert!(recorder.log.borrow().is_empty());
            s.settle(promise.into())?;
            let events = get(&ns, "events", s.context())?;
            let RuntimeValue::Json(events) = s.normalize(events)? else {
                panic!("events")
            };
            assert_eq!(s.context().get_data::<Visits>().unwrap().active, 0);
            let log = recorder.log.borrow().clone();
            Ok(json!({"events":events,"log":log}))
        })
        .unwrap();
    assert_eq!(
        result["events"],
        json!([
            "respond",
            "main-head",
            "main:pull",
            "main-body",
            "dependency:response",
            "dependency-head",
            "dependency:body",
            "dependency:response",
            "dependency:pull",
            "dependency:response",
            "dependency-body",
            "dependency-head",
            "dependency-body",
            "dependency-head",
            "dependency-body",
            "dependency-head",
            "dependency-body",
            "crawl-head"
        ])
    );
    let log = result["log"].as_array().unwrap();
    assert_eq!(log.len(), 11);
    assert_eq!(
        log[0]["details"]["head"]["statusText"],
        json!([99, 97, 102, 233])
    );
    assert_eq!(log[1]["details"]["body"]["body"], json!([77]));
    assert_eq!(log[0]["details"]["head"]["routeId"], "/page");
    assert_eq!(log[1]["details"]["body"]["routeId"], "/saved");
    assert_eq!(log[2]["details"]["head"]["decodedPath"], "/a%2Fb/😀");
    assert_eq!(
        log[3]["details"]["body"]["response"]["body"],
        json!([0, 255])
    );
    assert_eq!(
        log[3]["details"]["body"]["response"]["routeId"],
        "/a%2Fb/😀"
    );
    assert_eq!(
        log[5]["details"]["body"]["response"]["body"],
        json!([2, 128])
    );
    assert_eq!(log[7]["details"]["body"]["response"]["body"], json!([]));
    assert_eq!(
        log[9]["details"]["body"]["response"]["body"],
        json!([108, 97, 116, 101, 239, 191, 189])
    );
    assert!(
        log[10]["details"]["head"]["headers"]
            .as_array()
            .unwrap()
            .contains(&json!([b"x-phase".to_vec(), b"after-dependency".to_vec()]))
    );
    assert!(log[10]["details"]["head"]["routeId"].is_null());
}

#[test]
fn two_visits_begin_before_jobs_without_consuming_null_bodies() {
    let mut runtime = runtime(
        "async function respond(){events.push('start');await 0;events.push('finish');return new Response(null,{status:204})}",
    );
    runtime
        .native_operation(|s| {
            let intrinsics = VisitIntrinsics::capture(s.context())?;
            let ns = s.namespace("entry")?;
            let renderer = renderer(s, &ns);
            let (request1, options1) = args(&ns, s.context())?;
            let (request2, options2) = args(&ns, s.context())?;
            let first = Rc::new(Recorder::default());
            let second = Rc::new(Recorder::default());
            let a = start(
                &renderer,
                s.context(),
                request1,
                options1,
                intrinsics.clone(),
                VisitLimits::default(),
                first.clone(),
            )?;
            let b = start(
                &renderer,
                s.context(),
                request2,
                options2,
                intrinsics,
                VisitLimits::default(),
                second.clone(),
            )?;
            let events = JsArray::from_object(object(get(&ns, "events", s.context())?, "events")?)?;
            assert_eq!(events.length(s.context())?, 2);
            assert!(first.log.borrow().is_empty() && second.log.borrow().is_empty());
            s.settle(a.into())?;
            s.settle(b.into())?;
            for recorder in [first, second] {
                let log = recorder.log.borrow();
                assert_eq!(
                    log.iter()
                        .map(|v| v["kind"].as_str().unwrap())
                        .collect::<Vec<_>>(),
                    vec!["main-head", "main-body", "crawl-head"]
                );
                assert_eq!(log[1]["details"]["body"]["body"], json!([]));
            }
            assert_eq!(s.context().get_data::<Visits>().unwrap().active, 0);
            Ok(Value::Null)
        })
        .unwrap();
}

#[test]
fn observer_and_stream_errors_preserve_opaque_identity_and_release_state() {
    for failure in [
        "main-head",
        "main-body",
        "dependency-head",
        "dependency-body",
        "crawl-head",
        "respond",
        "stream",
        "body-getter",
    ] {
        let source=r#"
function respond(request,options){
 if(FAIL==='respond')throw reason;
 options.prerendering.dependencies.set('/dep',{response:new Response('x'),get body(){if(FAIL==='body-getter')throw reason;return ''}});
 return new Response(new ReadableStream({pull(c){if(FAIL==='stream')throw reason;c.enqueue(new Uint8Array([1]));c.close()}},{highWaterMark:0}));
}
"#.replace("FAIL",&format!("'{failure}'"));
        let mut runtime = runtime(&source);
        runtime
            .native_operation(|s| {
                let intrinsics = VisitIntrinsics::capture(s.context())?;
                let ns = s.namespace("entry")?;
                let renderer = renderer(s, &ns);
                let reason = get(&ns, "reason", s.context())?;
                let recorder = Rc::new(Recorder {
                    failure: Some((failure, reason.clone())),
                    ..Recorder::default()
                });
                let (request, options) = args(&ns, s.context())?;
                let promise = start(
                    &renderer,
                    s.context(),
                    request,
                    options,
                    intrinsics,
                    VisitLimits::default(),
                    recorder,
                )?;
                assert!(s.settle(promise.clone().into()).is_err());
                let PromiseState::Rejected(actual) = promise.state() else {
                    panic!("visit rejected")
                };
                assert!(JsValue::same_value(&reason, &actual), "{failure}");
                let registry = s.context().get_data::<Visits>().unwrap();
                assert_eq!(registry.active, 0);
                assert!(registry.states.is_empty());
                Ok(Value::Null)
            })
            .unwrap();
    }
}

#[test]
fn bounded_bytes_and_live_dependency_counts_reject_without_crawl() {
    for failure in ["response", "visit", "dependencies", "invalid-body"] {
        let mut runtime = runtime(if failure == "invalid-body" {
            "function respond(r,o){o.prerendering.dependencies.set('/dep',{response:new Response(''),body:new Int16Array([1])});return new Response('x')}"
        } else {
            "function respond(r,o){o.prerendering.dependencies.set('/a',{response:new Response(''),body:'abc'});o.prerendering.dependencies.set('/b',{response:new Response(''),body:''});return new Response('abc')}"
        });
        runtime
            .native_operation(|s| {
                let intrinsics = VisitIntrinsics::capture(s.context())?;
                let ns = s.namespace("entry")?;
                let renderer = renderer(s, &ns);
                let recorder = Rc::new(Recorder::default());
                let mut limits = VisitLimits::default();
                match failure {
                    "response" => limits.response_bytes = 2,
                    "visit" => limits.visit_bytes = 5,
                    "dependencies" => limits.dependencies = 1,
                    _ => {}
                }
                let (request, options) = args(&ns, s.context())?;
                let promise = start(
                    &renderer,
                    s.context(),
                    request,
                    options,
                    intrinsics,
                    limits,
                    recorder.clone(),
                )?;
                assert!(s.settle(promise.into()).is_err());
                assert!(
                    !recorder
                        .log
                        .borrow()
                        .iter()
                        .any(|v| v["kind"] == "crawl-head")
                );
                assert_eq!(s.context().get_data::<Visits>().unwrap().active, 0);
                Ok(Value::Null)
            })
            .unwrap();
    }
}

struct Reentrant {
    renderer: Rc<Renderer>,
    intrinsics: VisitIntrinsics,
    request: JsValue,
    options: JsObject,
    child: RefCell<Option<JsPromise>>,
    log: RefCell<Vec<&'static str>>,
}
impl VisitObserver for Reentrant {
    fn event(&self, event: VisitEvent, context: &mut Context) -> JsResult<()> {
        if matches!(event, VisitEvent::MainBody { .. }) {
            self.log.borrow_mut().push("before-child");
            let child = start(
                &self.renderer,
                context,
                self.request.clone(),
                self.options.clone(),
                self.intrinsics.clone(),
                VisitLimits::default(),
                Rc::new(Recorder::default()),
            )?;
            assert!(matches!(child.state(), PromiseState::Pending));
            assert_eq!(context.get_data::<Visits>().unwrap().active, 2);
            *self.child.borrow_mut() = Some(child);
            self.log.borrow_mut().push("after-child");
        }
        Ok(())
    }
}
#[test]
fn observer_can_start_another_visit_without_registry_borrow_or_nested_drain() {
    let mut runtime =
        runtime("function respond(){events.push('respond');return new Response('x')}");
    runtime
        .native_operation(|s| {
            let intrinsics = VisitIntrinsics::capture(s.context())?;
            let ns = s.namespace("entry")?;
            let renderer = Rc::new(renderer(s, &ns));
            let (request, options) = args(&ns, s.context())?;
            let child_args = args(&ns, s.context())?;
            let observer = Rc::new(Reentrant {
                renderer: renderer.clone(),
                intrinsics: intrinsics.clone(),
                request: child_args.0,
                options: child_args.1,
                child: RefCell::new(None),
                log: RefCell::new(vec![]),
            });
            let promise = start(
                &renderer,
                s.context(),
                request,
                options,
                intrinsics,
                VisitLimits::default(),
                observer.clone(),
            )?;
            s.settle(promise.into())?;
            s.settle(observer.child.borrow_mut().take().unwrap().into())?;
            assert_eq!(*observer.log.borrow(), vec!["before-child", "after-child"]);
            assert_eq!(s.context().get_data::<Visits>().unwrap().active, 0);
            Ok(Value::Null)
        })
        .unwrap();
}
