use super::*;
use serde_json::{Value, json};
use std::{cell::RefCell, collections::BTreeMap, rc::Rc};
use ts_js_runtime::{HostSnapshot, ModuleSource, Runtime, RuntimeLimits, RuntimeValue};

fn runtime(source: &str) -> Runtime {
    Runtime::new(
        HostSnapshot {
            modules: BTreeMap::from([(
                "entry".into(),
                ModuleSource {
                    source: source.into(),
                    import_meta_url: "file:///remote-producer.js".into(),
                },
            )]),
            resolutions: BTreeMap::new(),
        },
        RuntimeLimits::default(),
    )
    .unwrap()
}
fn get(s: &mut NativeSession<'_>, ns: &JsObject, name: &str) -> Result<JsValue> {
    Ok(ns.get(JsString::from(name), s.context())?)
}
fn json_value(s: &mut NativeSession<'_>, ns: &JsObject, name: &str) -> Result<Value> {
    let value = get(s, ns, name)?;
    let RuntimeValue::Json(value) = s.normalize(value)? else {
        panic!("JSON fixture value")
    };
    Ok(value)
}
type Paths = Rc<RefCell<Vec<(String, String, usize, bool)>>>;
struct Observer {
    callback: JsValue,
    paths: Paths,
}
impl RemotePrerenderObserver for Observer {
    fn request(
        &self,
        request: RemotePrerenderRequest,
        context: &mut Context,
    ) -> boa_engine::JsResult<()> {
        self.paths.borrow_mut().push((
            request.decoded_path,
            request.encoded_path,
            request.function_index,
            request.argument.is_some(),
        ));
        // Start the fixture's asynchronous response continuation, but never
        // await/settle it here. Production observers use Renderer similarly.
        producer_call(
            &self.callback,
            &JsValue::undefined(),
            &[
                request.argument.unwrap_or_else(JsValue::undefined),
                request.id,
                request.has_arg,
            ],
            context,
        )?;
        Ok(())
    }
}
fn begin(
    s: &mut NativeSession<'_>,
    ns: &JsObject,
    capture: &RemotePrerenderIntrinsics,
    kit3: bool,
    paths: Paths,
) -> Result<JsPromise> {
    let manifest = get(s, ns, "manifest")?.as_object().unwrap();
    let remotes = capture.discover(
        s,
        &manifest,
        if kit3 {
            MetadataProfile::Kit301
        } else {
            MetadataProfile::Kit2692
        },
    )?;
    let stringify = get(s, ns, "encode")?;
    let encoding = if kit3 {
        RemoteProducerEncoding::Kit3 {
            stringify,
            fix_stack_trace: get(s, ns, "fix")?,
        }
    } else {
        let receiver = get(s, ns, "internal")?;
        let get_hooks = producer_get(&receiver, "get_hooks", s.context())?;
        RemoteProducerEncoding::Kit2 {
            stringify,
            get_hooks,
            receiver,
        }
    };
    let observer = Rc::new(Observer {
        callback: get(s, ns, "request")?,
        paths,
    });
    Ok(remotes.start_producer_context(s.context(), "/_app/remote/", encoding, observer)?)
}
fn finish(s: &mut NativeSession<'_>, ns: &JsObject, promise: JsPromise) -> Result<()> {
    let observe = get(s, ns, "observe")?;
    let observed = s.call(&observe, &JsValue::undefined(), &[promise.into()])?;
    s.settle(observed)?;
    let producers = s.context().get_data::<Producers>().unwrap();
    assert_eq!(producers.active, 0);
    assert!(producers.states.is_empty());
    Ok(())
}

#[test]
fn producer_yields_at_awaits_but_enqueues_each_input_before_later_inputs_resolve() {
    for kit3 in [false, true] {
        let mut runtime = runtime(
            r#"
export const events=[];
let release;const gate=new Promise(r=>release=r);
const args=[new Date(0),new Map([['x',1]])];let seen=0;
const first={type:'prerender',has_arg:true,
 inputs(){events.push('inputs:first');return args},
 get id(){events.push('id');return {[Symbol.toPrimitive](hint){events.push('id:'+hint);return '雪 '+(++seen)}}}
};
const second={type:'prerender',id:'second',has_arg:true,
 async inputs(){events.push('inputs:second');return await gate}
};
function f(){throw Error('Handler must not run')}f.__=first;
function g(){throw Error('Handler must not run')}g.__=second;
const remotes={load:async()=>({default:{f,g}})};
export const manifest={remotes,_:{remotes}};
const transport={marker:Symbol('actual')};
export const internal={get_hooks(){if(this!==internal)throw Error('hooks receiver');events.push('hooks');return {get transport(){events.push('transport');return transport}}}};
export function encode(arg,t){
 if(this!==undefined)throw Error('encoder receiver');
 if(arguments.length===2&&t!==transport)throw Error('transport identity');
 if(arg!==undefined&&arg!==args[seen-1])throw Error('argument identity');
 events.push('encode');return arg===undefined?'':String(seen);
}
export function request(arg){
 events.push('enqueue:'+seen);
 (async()=>{await 0;events.push('response:'+ (arg===args[0]?'first':arg===args[1]?'second':'last'));if(arg===args[0])release([undefined])})();
}
export function fix(){throw Error('unexpected formatter')}
export function observe(p){return p.then(()=>events.push('complete'))}
"#,
        );
        let paths = Rc::new(RefCell::new(vec![]));
        let result = runtime
            .native_operation(|s| {
                let capture = RemotePrerenderIntrinsics::capture(s)?;
                let ns = s.namespace("entry")?;
                let pending = begin(s, &ns, &capture, kit3, paths.clone())?;
                assert!(paths.borrow().is_empty(), "No draining during start");
                assert_eq!(
                    json_value(s, &ns, "events")?,
                    if kit3 {
                        json!(["inputs:first"])
                    } else {
                        json!(["hooks"])
                    }
                );
                finish(s, &ns, pending)?;
                json_value(s, &ns, "events")
            })
            .unwrap();
        let mut expected = if kit3 {
            vec![]
        } else {
            vec!["hooks", "transport"]
        };
        expected.extend([
            "inputs:first",
            "id",
            "id:default",
            "encode",
            "enqueue:1",
            "id",
            "id:default",
            "encode",
            "enqueue:2",
            "inputs:second",
            "response:first",
            "response:second",
            "encode",
            "enqueue:2",
            "response:last",
            "complete",
        ]);
        assert_eq!(result, json!(expected));
        assert_eq!(
            *paths.borrow(),
            vec![
                (
                    "/_app/remote/雪 1/1".into(),
                    "/_app/remote/%E9%9B%AA%201/1".into(),
                    0,
                    true
                ),
                (
                    "/_app/remote/雪 2/2".into(),
                    "/_app/remote/%E9%9B%AA%202/2".into(),
                    0,
                    true
                ),
                (
                    "/_app/remote/second/".into(),
                    "/_app/remote/second/".into(),
                    1,
                    true
                ),
            ]
        );
    }
}

#[test]
fn empty_kit2_still_calls_awaits_hooks_and_observes_transport() {
    for kit3 in [false, true] {
        let mut runtime = runtime(
            r#"
export const events=[];const remotes={};export const manifest={remotes,_:{remotes}};
export const internal={
 get_hooks(){
  events.push('call');
  return {then(resolve){
   events.push('then');
   resolve({get transport(){events.push('transport');return null}});
  }};
 }
};
export function encode(){throw Error('encode')};export function fix(){throw Error('fix')};export function request(){throw Error('request')};
export function observe(p){return p.then(()=>events.push('done'))}
"#,
        );
        let result = runtime
            .native_operation(|s| {
                let capture = RemotePrerenderIntrinsics::capture(s)?;
                let ns = s.namespace("entry")?;
                let pending = begin(s, &ns, &capture, kit3, Rc::new(RefCell::new(vec![])))?;
                assert_eq!(
                    json_value(s, &ns, "events")?,
                    if kit3 { json!([]) } else { json!(["call"]) }
                );
                finish(s, &ns, pending)?;
                json_value(s, &ns, "events")
            })
            .unwrap();
        assert_eq!(
            result,
            if kit3 {
                json!(["done"])
            } else {
                json!(["call", "then", "transport", "done"])
            }
        );
    }
}

#[test]
fn has_arg_read_and_optional_thenable_inputs_follow_each_family() {
    for kit3 in [false, true] {
        let mut runtime = runtime(
            r#"
export const events=[];
const noarg={type:'prerender',id:'noarg',get has_arg(){events.push('has:noarg');return 0},
 inputs(){events.push('inputs:noarg');return {then(resolve){events.push('then:noarg');resolve(null)}}}};
const absent={type:'prerender',id:'absent',get has_arg(){events.push('has:absent');return true}};
const actual={type:'prerender',id:'actual',get has_arg(){events.push('has:actual');return true},inputs(){events.push('inputs:actual');return {then(resolve){events.push('then:actual');resolve([9n])}}}};
const remotes={load:async()=>({default:{a:{__:noarg},b:{__:absent},c:{__:actual}}})};export const manifest={remotes,_:{remotes}};
export const internal={get_hooks(){return {transport:null}}};
export function encode(value,transport){if(value!==9n)throw Error('argument');if(arguments.length===2&&(Object.getPrototypeOf(transport)!==Object.prototype||Object.keys(transport).length))throw Error('nullish transport');events.push('encode');return 'nine'};
export function request(){events.push('enqueue')};export function fix(){throw Error('fix')};export function observe(p){return p}
"#,
        );
        let result = runtime
            .native_operation(|s| {
                let capture = RemotePrerenderIntrinsics::capture(s)?;
                let ns = s.namespace("entry")?;
                let pending = begin(s, &ns, &capture, kit3, Rc::new(RefCell::new(vec![])))?;
                finish(s, &ns, pending)?;
                json_value(s, &ns, "events")
            })
            .unwrap();
        assert_eq!(
            result,
            if kit3 {
                json!([
                    "inputs:noarg",
                    "then:noarg",
                    "has:noarg",
                    "enqueue",
                    "has:absent",
                    "inputs:actual",
                    "then:actual",
                    "has:actual",
                    "encode",
                    "enqueue"
                ])
            } else {
                json!([
                    "has:noarg",
                    "enqueue",
                    "has:absent",
                    "has:actual",
                    "inputs:actual",
                    "then:actual",
                    "encode",
                    "enqueue"
                ])
            }
        );
    }
}

#[test]
fn kit3_formats_only_actual_input_errors_and_preserves_opaque_rejections() {
    for (input, formats) in [
        ("get inputs(){throw thrown}", true),
        (
            "inputs(){return {then(resolve,reject){reject(thrown)}}}",
            true,
        ),
        ("inputs(){throw thrown}", true),
        ("inputs(){return Promise.reject(thrown)}", true),
    ] {
        for actual_error in [false, true] {
            let source = format!(
                r#"
export const events=[];const thrown={};
const meta={{type:'prerender',has_arg:false,id:'a',{input}}};
const remotes={{load:async()=>({{default:{{f:{{__:meta}}}}}})}};export const manifest={{remotes}};
export function encode(){{throw Error('encode')}};export function request(){{throw Error('request')}};
export function fix(value){{if(value!==thrown)throw Error('lost error identity');events.push('fix')}};
export function observe(p){{return p.then(()=>{{throw Error('must reject')}},reason=>{{events.push(reason===thrown?'same':'different')}})}};
"#,
                if actual_error {
                    "new Error('inputs')"
                } else {
                    "{marker:Symbol('opaque')}"
                }
            );
            let mut runtime = runtime(&source);
            let result = runtime
                .native_operation(|s| {
                    let capture = RemotePrerenderIntrinsics::capture(s)?;
                    let ns = s.namespace("entry")?;
                    let pending = begin(s, &ns, &capture, true, Rc::new(RefCell::new(vec![])))?;
                    finish(s, &ns, pending)?;
                    json_value(s, &ns, "events")
                })
                .unwrap();
            assert_eq!(
                result,
                if actual_error && formats {
                    json!(["fix", "same"])
                } else {
                    json!(["same"])
                }
            );
        }
    }
}

#[test]
fn iterator_closing_preserves_original_observer_or_encoder_failure() {
    for failure in ["observer", "encode", "next", "done", "value", "has_arg"] {
        let source=r#"
export const events=[];const thrown={opaque:true};const failure=FAIL;
const iterator={
 next(){events.push('next');if(failure==='next')throw thrown;return {
 get done(){if(failure==='done')throw thrown;return false},get value(){if(failure==='value')throw thrown;return 1}
 }},get return(){events.push('return');return function(){if(this!==iterator)throw Error('return receiver');events.push('close');throw Error('close replacement')}}};
const meta={type:'prerender',id:'a',get has_arg(){if(failure==='has_arg')throw thrown;return true},inputs(){return {[Symbol.iterator](){return iterator}}}};
const remotes={load:async()=>({default:{f:{__:meta}}})};export const manifest={remotes};
export function encode(){events.push('encode');if(failure==='encode')throw thrown;return 'a'};
export function request(){events.push('observer');throw thrown};export function fix(){events.push('BAD:fix')};
export function observe(p){return p.then(()=>{throw Error('must reject')},reason=>events.push(reason===thrown?'same':'different'))}
"#.replace("FAIL",&format!("'{failure}'"));
        let mut runtime = runtime(&source);
        let result = runtime
            .native_operation(|s| {
                let capture = RemotePrerenderIntrinsics::capture(s)?;
                let ns = s.namespace("entry")?;
                let pending = begin(s, &ns, &capture, true, Rc::new(RefCell::new(vec![])))?;
                finish(s, &ns, pending)?;
                json_value(s, &ns, "events")
            })
            .unwrap();
        assert_eq!(
            result,
            match failure {
                "observer" => json!(["next", "encode", "observer", "return", "close", "same"]),
                "encode" => json!(["next", "encode", "return", "close", "same"]),
                "has_arg" => json!(["same"]),
                _ => json!(["next", "same"]),
            }
        );
    }
}

#[test]
fn kit2_hooks_rejections_remain_opaque_and_never_call_inputs() {
    for body in [
        "throw thrown",
        "return Promise.reject(thrown)",
        "return {get transport(){throw thrown}}",
    ] {
        let source = format!(
            r#"
export const events=[];const thrown={{opaque:true}};const remotes={{}};export const manifest={{_:{{remotes}}}};
export const internal={{get_hooks(){{events.push('hooks');{body}}}}};
export function encode(){{throw Error('encode')}};export function request(){{throw Error('request')}};export function fix(){{throw Error('fix')}};
export function observe(p){{return p.then(()=>{{throw Error('must reject')}},reason=>events.push(reason===thrown?'same':'different'))}}
"#
        );
        let mut runtime = runtime(&source);
        let result = runtime
            .native_operation(|s| {
                let capture = RemotePrerenderIntrinsics::capture(s)?;
                let ns = s.namespace("entry")?;
                let pending = begin(s, &ns, &capture, false, Rc::new(RefCell::new(vec![])))?;
                finish(s, &ns, pending)?;
                json_value(s, &ns, "events")
            })
            .unwrap();
        assert_eq!(result, json!(["hooks", "same"]));
    }
}

#[test]
fn missing_inputs_uses_actual_empty_array_iterator_and_adoption_errors_are_formatted() {
    for kit3 in [false, true] {
        let mut runtime = runtime(
            r#"
export const events=[];const remotes={load:async()=>({default:{f:{__:{type:'prerender',id:'a',has_arg:true}}}})};
export const manifest={remotes,_:{remotes}};export const internal={get_hooks(){return {}}};
export function mutate(){Array.prototype[Symbol.iterator]=function(){if(this.length)throw Error('expected fallback array');events.push('empty-iterator');let done=false;return {next(){if(done)return {done:true};done=true;return {done:false,value:7}}}}}
export function encode(value){if(value!==7)throw Error('custom iterator argument');return 'seven'};
export function request(){events.push('request')};export function fix(){throw Error('fix')};export function observe(p){return p}
"#,
        );
        let result = runtime
            .native_operation(|s| {
                let capture = RemotePrerenderIntrinsics::capture(s)?;
                let ns = s.namespace("entry")?;
                let mutate = get(s, &ns, "mutate")?;
                s.call(&mutate, &JsValue::undefined(), &[])?;
                let pending = begin(s, &ns, &capture, kit3, Rc::new(RefCell::new(vec![])))?;
                finish(s, &ns, pending)?;
                json_value(s, &ns, "events")
            })
            .unwrap();
        assert_eq!(result, json!(["empty-iterator", "request"]));
    }
    let mut runtime = runtime(
        r#"
export const events=[];const thrown=Error('adoption');
const internals={
 type:'prerender',has_arg:false,
 inputs(){
  const p=Promise.resolve([]);
  Object.defineProperty(p,'constructor',{get(){throw thrown}});
  return p;
 }
};
const remotes={load:async()=>({default:{f:{__:internals}}})};
export const manifest={remotes};export function encode(){throw Error('encode')};export function request(){throw Error('request')};
export function fix(e){if(e!==thrown)throw Error('identity');events.push('fix')};
export function observe(p){return p.then(()=>{throw Error('must reject')},e=>events.push(e===thrown?'same':'different'))}
"#,
    );
    let result = runtime
        .native_operation(|s| {
            let capture = RemotePrerenderIntrinsics::capture(s)?;
            let ns = s.namespace("entry")?;
            let pending = begin(s, &ns, &capture, true, Rc::new(RefCell::new(vec![])))?;
            finish(s, &ns, pending)?;
            json_value(s, &ns, "events")
        })
        .unwrap();
    assert_eq!(result, json!(["fix", "same"]));
}

#[test]
fn formatter_exception_replaces_input_error_without_losing_opaque_identity() {
    let mut runtime = runtime(
        r#"
export const events=[];const replacement={formatter:true};
const remotes={load:async()=>({default:{f:{__:{type:'prerender',inputs(){throw Error('original')},has_arg:false}}}})};
export const manifest={remotes};export function encode(){throw Error('encode')};export function request(){throw Error('request')};
export function fix(error){if(error.message!=='original')throw Error('wrong error');throw replacement};
export function observe(p){return p.then(()=>{throw Error('must reject')},e=>events.push(e===replacement?'replacement':'different'))}
"#,
    );
    let result = runtime
        .native_operation(|s| {
            let capture = RemotePrerenderIntrinsics::capture(s)?;
            let ns = s.namespace("entry")?;
            let pending = begin(s, &ns, &capture, true, Rc::new(RefCell::new(vec![])))?;
            finish(s, &ns, pending)?;
            json_value(s, &ns, "events")
        })
        .unwrap();
    assert_eq!(result, json!(["replacement"]));
}
