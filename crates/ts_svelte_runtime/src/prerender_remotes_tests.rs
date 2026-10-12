use super::*;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use ts_js_runtime::{HostSnapshot, ModuleSource, Resolution, Runtime, RuntimeLimits, RuntimeValue};

fn runtime(source: &str) -> Runtime {
    Runtime::new(
        HostSnapshot {
            modules: BTreeMap::from([
                (
                    "entry".into(),
                    ModuleSource {
                        source: source.into(),
                        import_meta_url: "file:///entry.js".into(),
                    },
                ),
                (
                    "remote".into(),
                    ModuleSource {
                        source: "export default { ignored: null };".into(),
                        import_meta_url: "file:///remote.js".into(),
                    },
                ),
            ]),
            resolutions: BTreeMap::from([(
                "entry".into(),
                BTreeMap::from([(
                    "remote".into(),
                    Resolution::Module {
                        id: "remote".into(),
                    },
                )]),
            )]),
        },
        RuntimeLimits::default(),
    )
    .unwrap()
}
fn export(s: &mut NativeSession<'_>, ns: &JsObject, name: &str) -> Result<JsValue> {
    Ok(ns.get(JsString::from(name), s.context())?)
}
fn encoding(
    s: &mut NativeSession<'_>,
    ns: &JsObject,
    kit3: bool,
) -> Result<RemoteArgumentEncoding> {
    let stringify = export(s, ns, "encode")?;
    Ok(if kit3 {
        RemoteArgumentEncoding::Kit3 {
            stringify,
            fix_stack_trace: export(s, ns, "fix")?,
        }
    } else {
        RemoteArgumentEncoding::Kit2 {
            stringify,
            transport: export(s, ns, "transport")?,
        }
    })
}
fn normalized(s: &mut NativeSession<'_>, ns: &JsObject, name: &str) -> Result<Value> {
    let value = export(s, ns, name)?;
    let RuntimeValue::Json(value) = s.normalize(value)? else {
        panic!("Expected JSON test observation")
    };
    Ok(value)
}
fn profile(kit3: bool) -> MetadataProfile {
    if kit3 {
        MetadataProfile::Kit301
    } else {
        MetadataProfile::Kit2692
    }
}

#[test]
fn ordered_discovery_real_inputs_and_profile_specific_evaluation() {
    for kit3 in [false, true] {
        let mut runtime = runtime(
            r#"
export const events=[];
export const transport={token:Symbol('transport')};
const actual=[new Date(0),12n,new Map([['a',1]]),undefined];
let current=0;
const meta={type:'prerender',has_arg:true,
 get id(){events.push('id');return {[Symbol.toPrimitive](hint){events.push('hint:'+hint);return '雪 space'}}},
 async inputs(){if(this!==meta)throw Error('inputs receiver');events.push('inputs');await 0;return actual}
};
function handler(){throw Error('handler must not execute')}
let reads=0;
Object.defineProperty(handler,'__',{get(){events.push('meta:'+ ++reads);return reads===1?{type:'prerender'}:meta}});
const noarg={__:{type:'prerender',has_arg:0,id:'plain',inputs(){events.push('noarg-inputs');return []}}};
const remotes={
 get b(){events.push('get:b');return async function(){if(this!==undefined)throw Error('loader receiver');events.push('b:start');await import('remote');events.push('b:end');return {default:{handler,noarg}}}},
 get a(){events.push('get:a');return async function(){events.push('a:start');await 0;events.push('a:end');return {default:{ignored:null}}}}
};
export const manifest={remotes,_:{remotes}};
// A spy encoder tests the explicit injection seam, not a devalue substitute.
export function encode(arg,t){
 if(this!==undefined||arg!==actual[current])throw Error('encoder lost actual value');
 if(arguments.length===2&&t!==transport)throw Error('transport identity');
 events.push('encode:'+arguments.length);current++;
 return arg===undefined?'':{[Symbol.toPrimitive](hint){events.push('suffix:'+hint);return 'arg'+current}};
}
export function fix(){throw Error('unexpected stack formatting')}
export function initialized(){events.push('init')}
globalThis.encodeURI=()=>{throw Error('mutated encodeURI')};
Object.values=()=>{throw Error('mutated Object.values')};
"#,
        );
        let result = runtime
            .native_operation(|s| {
                let captured = RemotePrerenderIntrinsics::capture(s)?;
                let ns = s.namespace("entry")?;
                let manifest = export(s, &ns, "manifest")?.as_object().unwrap();
                let remotes = captured.discover(s, &manifest, profile(kit3))?;
                assert_eq!(remotes.len(), 2);
                let init = export(s, &ns, "initialized")?;
                s.call(&init, &JsValue::undefined(), &[])?;
                let encoder = encoding(s, &ns, kit3)?;
                let requests = remotes.into_requests(s, "/base/_app/remote/", encoder)?;
                assert_eq!(requests.len(), 5);
                assert!(requests[3].argument.as_ref().unwrap().is_undefined());
                assert!(requests[4].argument.is_none());
                assert_eq!(requests[4].has_arg.as_number(), Some(0.0));
                assert_eq!(requests[4].function_index, 1);
                assert_eq!(requests[0].decoded_path, "/base/_app/remote/雪 space/arg1");
                assert_eq!(
                    requests[0].encoded_path,
                    "/base/_app/remote/%E9%9B%AA%20space/arg1"
                );
                assert_eq!(requests[3].decoded_path, "/base/_app/remote/雪 space/");
                normalized(s, &ns, "events")
            })
            .unwrap();
        let mut expected = vec![
            "get:b", "get:a", "b:start", "b:end", "meta:1", "meta:2", "a:start", "a:end", "init",
            "inputs",
        ];
        for index in 0..4 {
            expected.extend([
                "id",
                "hint:default",
                if kit3 { "encode:1" } else { "encode:2" },
            ]);
            if index != 3 {
                expected.push("suffix:default");
            }
        }
        if kit3 {
            expected.push("noarg-inputs");
        }
        assert_eq!(result, json!(expected));
    }
}

#[test]
fn iterator_close_on_encoding_failure_preserves_original_error() {
    for failure in ["encode", "next", "value"] {
        let source=r#"
export const events=[];export const transport={};export function fix(){}
const iterator={
 next(){events.push('next');if(FAIL==='next')throw Error('next failed');return {done:false,get value(){if(FAIL==='value')throw Error('value failed');return 9n}}},
 return(){events.push('close');throw Error('close must not replace failure')}
};
const internals={type:'prerender',has_arg:true,id:'a',inputs(){return {[Symbol.iterator](){return iterator}}}};
const remotes={load:async()=>({default:{f:{__:internals}}})};
export const manifest={_:{remotes}};
export function encode(){events.push('encode');throw Error('encode failed')}
"#.replace("FAIL",&format!("'{failure}'"));
        let mut runtime = runtime(&source);
        let value = runtime
            .native_operation(|s| {
                let capture = RemotePrerenderIntrinsics::capture(s)?;
                let ns = s.namespace("entry")?;
                let manifest = export(s, &ns, "manifest")?.as_object().unwrap();
                let remotes = capture.discover(s, &manifest, profile(false))?;
                let encoder = encoding(s, &ns, false)?;
                let error = remotes
                    .into_requests(s, "/_app/remote/", encoder)
                    .err()
                    .unwrap();
                assert!(
                    error.message.contains(&format!("{failure} failed")),
                    "{error:?}"
                );
                normalized(s, &ns, "events")
            })
            .unwrap();
        assert_eq!(
            value,
            if failure == "encode" {
                json!(["next", "encode", "close"])
            } else {
                json!(["next"])
            }
        );
    }
}

#[test]
fn kit3_formats_only_input_errors_and_kit2_skips_no_argument_inputs() {
    for kit3 in [false, true] {
        for failure in ["async", "getter", "primitive"] {
            let input = match failure {
                "async" => {
                    "async inputs(){events.push('inputs');await 0;throw Error('input failure')}"
                }
                "getter" => "get inputs(){events.push('inputs');throw Error('input failure')}",
                _ => "inputs(){events.push('inputs');throw 'primitive failure'}",
            };
            let source = format!(
                r#"
export const events=[];export const transport={{}};
const meta={{type:'prerender',has_arg:false,id:'a',{input}}};
const remotes={{load:async()=>({{default:{{f:{{__:meta}}}}}})}};
export const manifest={{remotes,_:{{remotes}}}};
export function encode(){{throw Error('encoder must not run')}}
export function fix(error){{events.push('formatted:'+error.message)}}
"#
            );
            let mut runtime = runtime(&source);
            let value = runtime
                .native_operation(|s| {
                    let capture = RemotePrerenderIntrinsics::capture(s)?;
                    let ns = s.namespace("entry")?;
                    let manifest = export(s, &ns, "manifest")?.as_object().unwrap();
                    let remotes = capture.discover(s, &manifest, profile(kit3))?;
                    let encoder = encoding(s, &ns, kit3)?;
                    let result = remotes.into_requests(s, "/_app/remote/", encoder);
                    if kit3 {
                        assert!(result.is_err());
                    } else {
                        assert_eq!(result.unwrap().len(), 1);
                    }
                    normalized(s, &ns, "events")
                })
                .unwrap();
            assert_eq!(
                value,
                if !kit3 {
                    json!([])
                } else if failure == "primitive" {
                    json!(["inputs"])
                } else {
                    json!(["inputs", "formatted:input failure"])
                }
            );
        }
    }
}

#[test]
fn nullish_inputs_empty_discovery_and_failure_poisoning() {
    let mut runtime = runtime(
        r#"
const remotes={load:async()=>({default:{a:{__:{type:'prerender',has_arg:true,id:'a',inputs:undefined}},b:{__:{type:'prerender',has_arg:true,id:'b',inputs:async()=>null}}}})};
export const manifest={remotes};export const empty={remotes:{}};
export function encode(){throw Error('must not encode empty inputs')};export function fix(){}
"#,
    );
    runtime
        .native_operation(|s| {
            let capture = RemotePrerenderIntrinsics::capture(s)?;
            let ns = s.namespace("entry")?;
            for name in ["manifest", "empty"] {
                let manifest = export(s, &ns, name)?.as_object().unwrap();
                let remotes = capture.discover(s, &manifest, profile(true))?;
                if name == "empty" {
                    assert!(remotes.is_empty());
                }
                let encoder = encoding(s, &ns, true)?;
                assert!(
                    remotes
                        .into_requests(s, "/_app/remote/", encoder)?
                        .is_empty()
                );
            }
            Ok(Value::Null)
        })
        .unwrap();
    let error = runtime
        .native_operation(|s| {
            let capture = RemotePrerenderIntrinsics::capture(s)?;
            let ns = s.namespace("entry")?;
            let manifest = export(s, &ns, "empty")?.as_object().unwrap();
            let remotes = capture.discover(s, &manifest, profile(true))?;
            let encoder = RemoteArgumentEncoding::Kit2 {
                stringify: export(s, &ns, "encode")?,
                transport: JsValue::undefined(),
            };
            remotes.into_requests(s, "/_app/remote/", encoder)?;
            Ok(Value::Null)
        })
        .unwrap_err();
    assert!(error.message.contains("profile mismatch"));
    assert_eq!(
        runtime.evaluate("entry").unwrap_err().code,
        "runtime_failed"
    );
}
