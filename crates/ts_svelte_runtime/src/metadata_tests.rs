use super::*;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use ts_js_runtime::{HostSnapshot, ModuleSource, Runtime, RuntimeLimits, RuntimeValue};
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
fn normalized(s: &mut NativeSession<'_>, value: JsValue) -> Result<Value> {
    if value.is_undefined() {
        return Ok(json!({"undefined":true}));
    }
    match s.normalize(value)? {
        RuntimeValue::Json(value) => Ok(value),
        _ => Err(invalid("Expected JSON test value")),
    }
}
fn analyse_source(source: &str, profile: MetadataProfile, hash: bool) -> Result<Value> {
    let mut runtime = runtime(&format!(
        "{source}\nexport const manifest={{nodes:[],routes,_:{{nodes:[],routes}}}};export {{nodes}};"
    ));
    runtime.native_operation(|s|{
        let module=s.namespace("entry")?;
        let nodes=get(s,&module,"nodes")?;
        let (nodes,len)=array(s,nodes,"Fixture loaded nodes")?;
        let mut values=vec![];for i in 0..len{values.push(object(nodes.get(i,s.context())?,"Fixture node")?);}
        let manifest=object(get(s,&module,"manifest")?,"Fixture manifest")?;
        let result=analyse(s,&manifest,&values,&MetadataOptions{profile,hash})?;
        let mut routes=vec![];
        for route in result.routes{
            let options=if let Some(nodes)=&route.page_nodes{
                let opts=page_options(s,nodes)?;
                json!({"csr":normalized(s,opts.csr)?,"ssr":normalized(s,opts.ssr)?,"slash":normalized(s,opts.trailing_slash)?,"actions":page_actions(s,nodes)?})
            }else{Value::Null};
            routes.push(json!({"id":route.id,"config":normalized(s,route.config)?,"prerender":normalized(s,route.prerender)?,"entries":normalized(s,route.entries)?,"methods":route.methods,"page":route.page_methods,"api":route.api_methods,"options":options}));
        }
        let events=get(s,&module,"events")?;
        Ok(json!({"routes":routes,"events":normalized(s,events)?}))
    })
}
#[test]
fn config_precedence_options_and_reset_ancestry_are_versioned() {
    let source = r#"
      const nodes=[
        {index:0,universal:{config:{region:'u',ancestor:1},csr:false,ssr:false,prerender:true,trailingSlash:'always'},server:{config:{region:'s'}}},
        {index:1,universal:{config:{leaf:true},ssr:true,prerender:false},server:{actions:{save(){throw Error('not invoked')},bad:17}}},
        {index:2,universal:{config:{reset:true}}}
      ];
      const routes=[{id:'/inherited',page:{layouts:[0,undefined],leaf:1}},{id:'/reset',page:{layouts:[],leaf:2}}];
    "#;
    for (profile, region) in [
        (MetadataProfile::Kit2692, "s"),
        (MetadataProfile::Kit301, "u"),
        (MetadataProfile::Kit3Next32, "u"),
    ] {
        let output = analyse_source(source, profile, false).unwrap();
        let first = &output["routes"][0];
        assert_eq!(
            first["config"],
            json!({"region":region,"ancestor":1,"leaf":true})
        );
        assert_eq!(first["methods"], json!(["GET", "POST"]));
        assert_eq!(first["prerender"], false);
        assert_eq!(
            first["options"],
            json!({"csr":false,"ssr":true,"slash":"always","actions":[{"name":"save","callable":true},{"name":"bad","callable":false}]})
        );
        assert_eq!(output["routes"][1]["config"], json!({"reset":true}));
        assert_eq!(output["routes"][1]["options"]["csr"], true);
    }
}
#[test]
fn actual_async_entries_endpoint_receivers_and_order_are_preserved() {
    let output = analyse_source(
        r#"
      export const events=[];
      const nodes=[{index:0,universal:{entries:async function(){
        if(this!==undefined)throw Error('entries receiver');
        events.push('entries:start');await 0;events.push('entries:end');
        return [{slug:'雪/💙',rest:''},{slug:'literal[x+2f]',rest:'a/b'}];
      }}}];
      const routes=[{id:'/(group)/[slug]/[...rest]/',page:{layouts:[],leaf:0},async endpoint(){
        if(this!==routes[0])throw Error('endpoint receiver');
        events.push('endpoint:start');await 0;events.push('endpoint:end');
        return {GET(){},HEAD(){},fallback(){},entries(){throw Error('page entries wins')}};
      }}];
    "#,
        MetadataProfile::Kit301,
        false,
    )
    .unwrap();
    assert_eq!(
        output["events"],
        json!([
            "endpoint:start",
            "endpoint:end",
            "entries:start",
            "entries:end"
        ])
    );
    assert_eq!(
        output["routes"][0]["entries"],
        json!(["/雪/💙/", "/literal[x+2f]/a/b/"])
    );
    assert_eq!(output["routes"][0]["methods"], json!(["GET", "HEAD", "*"]));
}
#[test]
fn ordered_endpoint_methods_do_not_infer_head_and_kit3_adds_query() {
    let source = "const nodes=[];const routes=[{id:'/api',endpoint:async()=>({HEAD:1,OPTIONS:1,DELETE:1,PATCH:1,PUT:1,POST:1,GET:1,fallback:1,QUERY:1})}];";
    let result = analyse_source(source, MetadataProfile::Kit301, false).unwrap();
    assert_eq!(
        result["routes"][0]["api"],
        json!([
            "GET", "POST", "PUT", "PATCH", "DELETE", "OPTIONS", "HEAD", "QUERY", "*"
        ])
    );
    assert!(
        analyse_source(source, MetadataProfile::Kit2692, false)
            .unwrap_err()
            .message
            .contains("QUERY")
    );
    let result = analyse_source(
        "const nodes=[];const routes=[{id:'/api',endpoint:async()=>({GET:1})}];",
        MetadataProfile::Kit2692,
        false,
    )
    .unwrap();
    assert_eq!(result["routes"][0]["methods"], json!(["GET"]));
}
#[test]
fn prerender_rejects_body_methods_and_dual_routes_but_versions_differ_on_fallback() {
    let fallback =
        "const nodes=[];const routes=[{id:'/api',endpoint:()=>({prerender:true,fallback(){}})}];";
    assert!(analyse_source(fallback, MetadataProfile::Kit2692, false).is_ok());
    assert!(
        analyse_source(fallback, MetadataProfile::Kit301, false)
            .unwrap_err()
            .message
            .contains("Cannot prerender")
    );
    for source in [
        "const nodes=[];const routes=[{id:'/api',endpoint:()=>({prerender:'auto',POST(){}})}];",
        "const nodes=[{index:0,universal:{prerender:true}}];const routes=[{id:'/both',page:{layouts:[],leaf:0},endpoint:()=>({prerender:true,GET(){}})}];",
    ] {
        assert!(
            analyse_source(source, MetadataProfile::Kit301, false)
                .unwrap_err()
                .message
                .contains("Cannot prerender")
        );
    }
}
#[test]
fn config_comparison_uses_json_stringify_tojson_order_and_undefined() {
    let ok = r#"
      const nodes=[{index:0,universal:{config:{x:{toJSON(){return {value:1}}},omitted:undefined}}}];
      const routes=[{id:'/ok',page:{layouts:[],leaf:0},endpoint:()=>({config:{x:{value:1}}})}];
    "#;
    // Native normalization rejects functions, so inspect the raw analysis result
    // in the owning engine; comparison itself must invoke toJSON.
    let mut runtime = runtime(&format!(
        "{ok} export {{nodes}};export const manifest={{nodes:[],routes}};"
    ));
    runtime
        .native_operation(|s| {
            let module = s.namespace("entry")?;
            let nodes = get(s, &module, "nodes")?;
            let (nodes, _) = array(s, nodes, "nodes")?;
            let node = object(nodes.get(0, s.context())?, "node")?;
            let manifest = object(get(s, &module, "manifest")?, "manifest")?;
            let result = analyse(
                s,
                &manifest,
                &[node],
                &MetadataOptions {
                    profile: MetadataProfile::Kit301,
                    hash: false,
                },
            )?;
            assert_eq!(result.routes.len(), 1);
            Ok(Value::Null)
        })
        .unwrap();
    let mismatch = "const nodes=[{index:0,universal:{config:{x:{a:1,b:2}}}}];const routes=[{id:'/order',page:{layouts:[],leaf:0},endpoint:()=>({config:{x:{b:2,a:1}}})}];";
    assert!(
        analyse_source(mismatch, MetadataProfile::Kit301, false)
            .unwrap_err()
            .message
            .contains("Mismatched route config")
    );
}
#[test]
fn exports_include_prototypes_and_suppress_shadowed_non_enumerable_keys() {
    let invalid = "const nodes=[{index:0,universal:Object.assign(Object.create({actions(){}}),{load(){}}),universal_id:'src/+page.ts'}];const routes=[{id:'/bad',page:{layouts:[],leaf:0}}];";
    let error = analyse_source(invalid, MetadataProfile::Kit2692, false).unwrap_err();
    assert!(
        error
            .message
            .contains("Invalid export 'actions' in src/+page.ts")
    );
    assert!(error.message.contains("+page.server.ts"));
    let valid = "const universal=Object.create({wrong:true});Object.defineProperty(universal,'wrong',{value:true});universal._helper=17;const nodes=[{index:0,universal}];const routes=[{id:'/ok',page:{layouts:[],leaf:0}}];";
    assert!(analyse_source(valid, MetadataProfile::Kit301, false).is_ok());
}
#[test]
fn hash_router_options_validate_before_any_endpoint_is_loaded() {
    let source = "const nodes=[{index:0,universal:{load(){},_helper:1},universal_id:'src/+page.js'}];const routes=[{id:'/api',endpoint(){throw Error('must not load')}}];";
    let error = analyse_source(source, MetadataProfile::Kit301, true).unwrap_err();
    assert!(error.message.contains("router.type === 'hash'"));
    assert!(error.message.contains("'_helper'"));
}
#[test]
fn entries_rejections_are_real_and_falsy_exports_and_sparse_arrays_remain_distinct() {
    let error=analyse_source("const nodes=[];const routes=[{id:'/api',endpoint:()=>({entries:async()=>{await 0;throw Error('entry failure')}})}];",MetadataProfile::Kit301,false).unwrap_err();
    assert!(error.message.contains("entry failure"));
    let value=analyse_source("const nodes=[{index:0,universal:{entries:false}}];const routes=[{id:'/a',page:{layouts:[],leaf:0},endpoint:()=>({entries(){throw Error('not invoked')}})}];",MetadataProfile::Kit301,false).unwrap();
    assert_eq!(value["routes"][0]["entries"], false);
}
fn resolve(id: &str, expression: &str, profile: MetadataProfile) -> Result<Value> {
    let mut runtime = runtime(&format!("export const params={expression}"));
    runtime.native_operation(|s| {
        let module = s.namespace("entry")?;
        let params = get(s, &module, "params")?;
        let path = resolve_route(s, id, &params, profile)?;
        normalized(s, path.into())
    })
}
#[test]
fn route_resolution_matches_static_escape_and_scalar_version_distinctions() {
    assert_eq!(
        resolve(
            "/(g)/[x+2f]/[u+1f499]/[[optional]]/[v=number]/[...rest]/",
            "({v:0,rest:'a/b'})",
            MetadataProfile::Kit301
        )
        .unwrap(),
        "/%2F/💙/0/a/b/"
    );
    assert_eq!(
        resolve(
            "/[v]/[b]/[n]",
            "({v:false,b:2n,n:NaN})",
            MetadataProfile::Kit301
        )
        .unwrap(),
        "/false/2/NaN"
    );
    assert_eq!(
        resolve("/[x+2f]/[v]", "({v:'[u+2f]'})", MetadataProfile::Kit2692).unwrap(),
        "/[x+2f]/[u+2f]"
    );
    assert_eq!(
        resolve(
            "/[[v]]/[...rest]",
            "({v:false,rest:null})",
            MetadataProfile::Kit2692
        )
        .unwrap(),
        "/"
    );
    assert!(
        resolve("/[v]", "({v:0})", MetadataProfile::Kit2692)
            .unwrap_err()
            .message
            .contains("Missing parameter")
    );
    assert!(resolve("/[v]", "({v:null})", MetadataProfile::Kit301).is_err());
    assert!(
        resolve("/[v]", "({v:'/bad'})", MetadataProfile::Kit301)
            .unwrap_err()
            .message
            .contains("slash")
    );
    assert!(
        resolve("/[...rest]", "({})", MetadataProfile::Kit301)
            .unwrap_err()
            .message
            .contains("Missing parameter")
    );
}
#[test]
fn option_getters_are_short_circuiting_and_supplementary_queries_are_explicit() {
    let source = r#"
      export const events=[];
      const nodes=[{index:0,universal:{get prerender(){events.push('universal');return false}},server:{get prerender(){throw Error('unreachable server getter')}}}];
      const routes=[{id:'/',page:{layouts:[],leaf:0}}];
    "#;
    let value = analyse_source(source, MetadataProfile::Kit301, false).unwrap();
    // Once in postbuild analysis, once in the explicit page_options query.
    assert_eq!(value["events"], json!(["universal", "universal"]));
}

#[test]
fn entries_mapping_preserves_sparse_species_and_custom_map_behavior() {
    let mut runtime = runtime(
        r#"
      class Entries extends Array { static get [Symbol.species]() { return Array; } }
      const entries=new Entries();entries.length=2;entries[1]={x:'yes'};
      export const manifest={nodes:[],routes:[
        {id:'/[x]',endpoint:()=>({entries:()=>entries})},
        {id:'/custom/[x]',endpoint:()=>({entries:()=>({map(fn){return {resolved:fn({x:42})}}})})},
        {id:'/null',endpoint:()=>({entries:null})}
      ]};
    "#,
    );
    runtime
        .native_operation(|s| {
            let module = s.namespace("entry")?;
            let manifest = object(get(s, &module, "manifest")?, "manifest")?;
            let result = analyse(
                s,
                &manifest,
                &[],
                &MetadataOptions {
                    profile: MetadataProfile::Kit301,
                    hash: false,
                },
            )?;
            let entries = object(result.routes[0].entries.clone(), "mapped entries")?;
            assert!(!entries.has_property(0, s.context())?);
            assert_eq!(
                text(&entries.get(1, s.context())?.as_string().unwrap())?,
                "/yes"
            );
            assert_eq!(
                normalized(s, result.routes[1].entries.clone())?,
                json!({"resolved":"/custom/42"})
            );
            assert!(result.routes[2].entries.is_null());
            Ok(Value::Null)
        })
        .unwrap();
}
