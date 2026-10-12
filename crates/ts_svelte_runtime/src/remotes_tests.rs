use super::*;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use ts_js_runtime::{HostSnapshot, ModuleSource, Resolution, Runtime, RuntimeLimits, RuntimeValue};
fn runtime(source: &str) -> Runtime {
    Runtime::new(
        HostSnapshot {
            modules: BTreeMap::from([(
                "entry".into(),
                ModuleSource {
                    source: source.into(),
                    import_meta_url: "file:///entry.js".into(),
                },
            )]),
            resolutions: BTreeMap::new(),
        },
        RuntimeLimits::default(),
    )
    .unwrap()
}
fn chunks(names: &[&str]) -> Vec<RemoteChunk> {
    names
        .iter()
        .map(|name| RemoteChunk {
            hash: (*name).into(),
            file: format!("src/{name}.remote.ts"),
        })
        .collect()
}
fn json_value(s: &mut NativeSession<'_>, value: JsValue) -> Result<Value> {
    if value.is_undefined() {
        return Ok(json!({"undefined":true}));
    }
    match s.normalize(value)? {
        RuntimeValue::Json(value) => Ok(value),
        _ => Err(invalid("Expected JSON fixture value")),
    }
}
fn summarize(s: &mut NativeSession<'_>, value: RemoteAnalysis) -> Result<Value> {
    let mut output = vec![];
    for remote in value.remotes {
        let mut exports = vec![];
        for export in remote.exports {
            exports.push(json!({"name":export.name.to_std_string().unwrap(),"type":json_value(s,export.kind)?,"dynamic":json_value(s,export.dynamic)?}));
        }
        output.push(json!({"hash":remote.hash,"file":remote.file,"exports":exports}));
    }
    Ok(json!(output))
}
#[test]
fn real_loaders_are_sequential_awaited_and_duplicate_hashes_replace_in_place() {
    for profile in [
        MetadataProfile::Kit2692,
        MetadataProfile::Kit301,
        MetadataProfile::Kit3Next32,
    ] {
        let mut runtime = runtime(
            r#"
            export const events=[];let count=0;
            const remotes={
              async a(){if(this!==undefined)throw Error('bound loader');events.push('a:start');await 0;events.push('a:end');return {default:{['v'+(++count)]:{__:{type:'query_live'}}}}},
              async b(){events.push('b:start');await 0;events.push('b:end');return {default:{run:Object.assign(()=>{throw Error('handler executed')},{__:{type:'command'}})}}}
            };
            export const manifest={remotes,_:{remotes}};
        "#,
        );
        runtime
            .native_operation(|s| {
                let module = s.namespace("entry")?;
                let manifest = object(get(s, &module, "manifest")?, "manifest")?;
                let result = analyse(s, &manifest, &chunks(&["a", "b", "a"]), profile)?;
                let result = summarize(s, result)?;
                assert_eq!(result[0]["hash"], "a");
                assert_eq!(result[0]["exports"][0]["name"], "v2");
                assert_eq!(result[1]["hash"], "b");
                let events = get(s, &module, "events")?;
                assert_eq!(
                    json_value(s, events)?,
                    json!(["a:start", "a:end", "b:start", "b:end", "a:start", "a:end"])
                );
                Ok(Value::Null)
            })
            .unwrap();
    }
}
#[test]
fn actual_module_namespace_wrappers_keep_export_order_and_dynamic_import_identity() {
    let snapshot=HostSnapshot{
        modules:BTreeMap::from([
            ("entry".into(),ModuleSource{source:"export const manifest={remotes:{x:()=>import('./wrapper.js')}}".into(),import_meta_url:"file:///entry.js".into()}),
            ("wrapper".into(),ModuleSource{source:"import * as functions from './functions.js';export default functions".into(),import_meta_url:"file:///wrapper.js".into()}),
            ("functions".into(),ModuleSource{source:"function f(){throw Error('must not call')}Object.defineProperty(f,'__',{value:{type:'prerender'}});export {f as z,f as a,f as 'with space'}".into(),import_meta_url:"file:///functions.js".into()}),
        ]),
        resolutions:BTreeMap::from([
            ("entry".into(),BTreeMap::from([("./wrapper.js".into(),Resolution::Module{id:"wrapper".into()})])),
            ("wrapper".into(),BTreeMap::from([("./functions.js".into(),Resolution::Module{id:"functions".into()})])),
        ]),
    };
    let mut runtime = Runtime::new(snapshot, RuntimeLimits::default()).unwrap();
    runtime
        .native_operation(|s| {
            let module = s.namespace("entry")?;
            let manifest = object(get(s, &module, "manifest")?, "manifest")?;
            let result = analyse(s, &manifest, &chunks(&["x"]), MetadataProfile::Kit301)?;
            let exports = &result.remotes[0].exports;
            assert_eq!(
                exports
                    .iter()
                    .map(|e| e.name.to_std_string().unwrap())
                    .collect::<Vec<_>>(),
                ["a", "with space", "z"]
            );
            assert!(exports.iter().all(|e| e.dynamic.is_undefined()));
            Ok(Value::Null)
        })
        .unwrap();
}
#[test]
fn raw_metadata_values_and_short_circuiting_are_preserved_without_initialization() {
    let mut runtime = runtime(
        r#"
      export const sentinel={same:true};
      const remotes={x:()=>({default:{
        absent:{__:{type:'prerender'}},nil:{__:{type:'prerender',dynamic:null}},
        no:{__:{type:'prerender',dynamic:false}},raw:{__:{type:'prerender',dynamic:sentinel}},
        query:{__:{type:'query',get dynamic(){throw Error('not read')},set id(v){throw Error('not initialized')}}},
        unknown:{__:{type:sentinel,get dynamic(){throw Error('not read')}}},
        untyped:{__:{}}
      }})};export const manifest={remotes};
    "#,
    );
    runtime
        .native_operation(|s| {
            let module = s.namespace("entry")?;
            let manifest = object(get(s, &module, "manifest")?, "manifest")?;
            let result = analyse(s, &manifest, &chunks(&["x"]), MetadataProfile::Kit301)?;
            let e = &result.remotes[0].exports;
            assert!(e[0].dynamic.is_undefined());
            assert!(e[1].dynamic.is_null());
            assert_eq!(e[2].dynamic.as_boolean(), Some(false));
            let sentinel = get(s, &module, "sentinel")?;
            assert!(e[3].dynamic.strict_equals(&sentinel));
            assert!(e[5].kind.strict_equals(&sentinel));
            assert!(e[4..].iter().all(|e| e.dynamic.as_boolean() == Some(true)));
            assert!(e[6].kind.is_undefined());
            Ok(Value::Null)
        })
        .unwrap();
}
#[test]
fn lazy_for_in_observes_deleted_exports_inheritance_shadowing_and_symbols() {
    let mut runtime = runtime(
        r#"
      const valid={__:{type:'query'}};
      const prototype={hidden:null,inherited:valid};const functions=Object.create(prototype);
      Object.defineProperty(functions,'hidden',{value:null});
      Object.defineProperty(functions,'first',{enumerable:true,get(){delete functions.later;return valid}});
      functions.later=null;functions[Symbol('ignored')]=null;
      export const manifest={remotes:{x:()=>({default:functions})}};
    "#,
    );
    runtime
        .native_operation(|s| {
            let module = s.namespace("entry")?;
            let manifest = object(get(s, &module, "manifest")?, "manifest")?;
            let result = analyse(s, &manifest, &chunks(&["x"]), MetadataProfile::Kit301)?;
            assert_eq!(
                result.remotes[0]
                    .exports
                    .iter()
                    .map(|e| e.name.to_std_string().unwrap())
                    .collect::<Vec<_>>(),
                ["first", "inherited"]
            );
            Ok(Value::Null)
        })
        .unwrap();
}
#[test]
fn actual_loader_and_property_failures_propagate_without_skipping() {
    for (source, expected) in [
        (
            "export const manifest={remotes:{x:async()=>{await 0;throw Error('loader failed')}}}",
            "loader failed",
        ),
        (
            "export const manifest={remotes:{x:()=>({default:{get broken(){throw Error('getter failed')}}})}}",
            "getter failed",
        ),
        (
            "export const manifest={remotes:{x:()=>({default:{broken:{}}})}}",
            "TypeError",
        ),
        ("export const manifest={remotes:{}}", "callable"),
    ] {
        let mut runtime = runtime(source);
        let error = runtime
            .native_operation(|s| {
                let module = s.namespace("entry")?;
                let manifest = object(get(s, &module, "manifest")?, "manifest")?;
                analyse(s, &manifest, &chunks(&["x"]), MetadataProfile::Kit301)?;
                Ok(Value::Null)
            })
            .unwrap_err();
        assert!(error.message.contains(expected), "{}", error.message);
    }
}
#[test]
fn initializer_validates_tags_assigns_real_internals_and_does_not_call_handlers() {
    let mut runtime = runtime(
        r#"
      export const functions=Object.fromEntries(['command','form','prerender','query','query_batch','query_live'].map(type=>[type,Object.assign(()=>{throw Error('not invoked')},{__:{type}})]));
      functions['雪/💙']={__:{type:'query'}};
    "#,
    );
    runtime
        .native_operation(|s| {
            let module = s.namespace("entry")?;
            let functions = get(s, &module, "functions")?;
            initialize_functions(
                s,
                &functions,
                "src/a.remote.ts",
                "hash",
                MetadataProfile::Kit301,
            )?;
            for name in TYPES.iter().copied().chain(["雪/💙"]) {
                let function = property(s, &functions, name)?;
                let internals = property(s, &function, "__")?;
                assert_eq!(
                    property(s, &internals, "id")?
                        .as_string()
                        .unwrap()
                        .to_std_string()
                        .unwrap(),
                    format!("hash/{name}")
                );
                assert_eq!(
                    property(s, &internals, "name")?
                        .as_string()
                        .unwrap()
                        .to_std_string()
                        .unwrap(),
                    name
                );
            }
            Ok(Value::Null)
        })
        .unwrap();
}
#[test]
fn initializer_diagnostics_are_versioned_and_default_export_check_precedes_getters() {
    for profile in [MetadataProfile::Kit267, MetadataProfile::Kit301] {
        for (source, code) in [
            (
                "export const functions={default:1,get later(){throw Error('must not read')}}",
                "remote_module_default_export",
            ),
            (
                "export const functions={invalid:()=>{}}",
                "remote_module_invalid_export",
            ),
            (
                "export const functions={default:false}",
                "remote_module_invalid_export",
            ),
        ] {
            let mut runtime = runtime(source);
            let error = runtime
                .native_operation(|s| {
                    let module = s.namespace("entry")?;
                    let functions = get(s, &module, "functions")?;
                    initialize_functions(s, &functions, "src/a.remote.ts", "h", profile)?;
                    Ok(Value::Null)
                })
                .unwrap_err();
            assert_eq!(error.code, code);
            assert!(error.message.contains("src/a.remote.ts"));
            assert_eq!(
                error.message.contains("https://svelte.dev/e/kit/"),
                profile.is_kit3()
            );
        }
    }
}
#[test]
fn initializer_snapshots_entries_before_validation_and_rereads_each_assignment() {
    let mut runtime = runtime(
        r#"
      export const events=[];export const first={type:'query'},second={},third={};let count=0;
      const fn={get __(){events.push('internals');return [first,second,third][count++]}};
      export const functions={get a(){events.push('a');return fn},get b(){events.push('b');return {__:{type:'command'}}}};
    "#,
    );
    runtime
        .native_operation(|s| {
            let module = s.namespace("entry")?;
            let functions = get(s, &module, "functions")?;
            initialize_functions(
                s,
                &functions,
                "src/a.remote.ts",
                "hash",
                MetadataProfile::Kit2692,
            )?;
            let events = get(s, &module, "events")?;
            assert_eq!(
                json_value(s, events)?,
                json!(["a", "b", "internals", "internals", "internals"])
            );
            let second = get(s, &module, "second")?;
            assert_eq!(
                property(s, &second, "id")?
                    .as_string()
                    .unwrap()
                    .to_std_string()
                    .unwrap(),
                "hash/a"
            );
            let third = get(s, &module, "third")?;
            assert_eq!(
                property(s, &third, "name")?
                    .as_string()
                    .unwrap()
                    .to_std_string()
                    .unwrap(),
                "a"
            );
            Ok(Value::Null)
        })
        .unwrap();
}
#[test]
fn empty_inventory_does_not_read_remotes_and_null_default_is_empty_for_in() {
    let mut empty_runtime = runtime("export const manifest={get remotes(){throw Error('unused')}}");
    empty_runtime
        .native_operation(|s| {
            let module = s.namespace("entry")?;
            let manifest = object(get(s, &module, "manifest")?, "manifest")?;
            assert!(
                analyse(s, &manifest, &[], MetadataProfile::Kit301)?
                    .remotes
                    .is_empty()
            );
            Ok(Value::Null)
        })
        .unwrap();
    let mut runtime = runtime("export const manifest={remotes:{x:()=>({default:null})}}");
    runtime
        .native_operation(|s| {
            let module = s.namespace("entry")?;
            let manifest = object(get(s, &module, "manifest")?, "manifest")?;
            assert!(
                analyse(s, &manifest, &chunks(&["x"]), MetadataProfile::Kit301)?.remotes[0]
                    .exports
                    .is_empty()
            );
            Ok(Value::Null)
        })
        .unwrap();
}
