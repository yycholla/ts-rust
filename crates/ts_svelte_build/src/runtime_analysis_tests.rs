use super::*;
use crate::pipeline_tests::Fixture;
use serde_json::json;
use ts_svelte_kit::build_analysis::{
    self, AnalysisInput, AnalysisNode, AnalysisPage, AnalysisRoute, StaticValue,
};

fn analyze(request: &BuildRequest) -> AnalysisOutput {
    let path = |path: &std::path::PathBuf| path.to_string_lossy().into_owned();
    build_analysis::analyze(&AnalysisInput {
        kit_version: request.kit.inventory.kit_version.clone(),
        modules: request.modules.clone(),
        nodes: request
            .kit
            .inventory
            .nodes
            .iter()
            .map(|node| AnalysisNode {
                universal: node.universal.as_ref().map(path),
                server: node.server.as_ref().map(path),
            })
            .collect(),
        routes: request
            .kit
            .inventory
            .routes
            .iter()
            .map(|route| AnalysisRoute {
                id: route.id.clone(),
                page: route.page.as_ref().map(|page| AnalysisPage {
                    layouts: page.layouts.clone(),
                    leaf: page.leaf,
                }),
                endpoint: route.endpoint.as_ref().map(path),
            })
            .collect(),
    })
    .unwrap()
}
fn fixture() -> (Fixture, AnalysisOutput, Value) {
    let fixture = Fixture::new(
        "export const ssr=(()=>true)(); export const entries=()=>[{}]; export function load(){}",
    );
    let original = analyze(&fixture.request);
    assert!(!original.requirements.is_empty());
    let payload = json!({
        "serverBuildId":"actual-server-sha",
        "nodes":[
            {"index":0,"hasServerLoad":false,"hasUniversalLoad":false,"serverLoadCallable":false,"universalLoadCallable":false},
            {"index":1,"hasServerLoad":false,"hasUniversalLoad":false,"serverLoadCallable":false,"universalLoadCallable":false},
            {"index":2,"hasServerLoad":false,"hasUniversalLoad":true,"serverLoadCallable":false,"universalLoadCallable":true}
        ],
        "routes":[{
            "id":"/", "config":{"kind":"json","value":{"runtime":true}},
            "prerender":{"kind":"json","value":false},
            "entries":{"kind":"json","value":["/"]},
            "methods":["GET"],"pageMethods":["GET"],"endpointMethods":[],
            "page":{"csr":{"kind":"json","value":true},"ssr":{"kind":"json","value":true},
                "prerender":{"kind":"json","value":false},"trailingSlash":{"kind":"json","value":"never"},"actions":[]},
            "endpoint":null
        }]
    });
    (fixture, original, payload)
}
fn merged(
    fixture: &Fixture,
    original: &AnalysisOutput,
    payload: &Value,
) -> Result<RuntimeAnalysis, String> {
    merge(&fixture.request, "actual-server-sha", original, payload)
}

#[test]
fn merges_actual_facts_without_replacing_module_facts_or_entries_exports() {
    let (fixture, mut original, payload) = fixture();
    original
        .errors
        .push("Earlier static validation superseded by successful runtime validation".into());
    let result = merged(&fixture, &original, &payload).unwrap();
    assert!(result.analysis.requirements.is_empty());
    assert!(result.analysis.errors.is_empty());
    assert_eq!(result.analysis.modules, original.modules);
    assert_eq!(
        result.analysis.routes["/"].entries,
        original.routes["/"].entries
    );
    assert!(matches!(
        result.analysis.routes["/"].entries,
        StaticValue::Unknown(_) | StaticValue::Function
    ));
    assert_eq!(result.analysis.routes["/"].ssr, known(true));
    assert_eq!(
        result.resolved_entries["/"],
        RuntimeEntries::Json(json!(["/"]))
    );
    assert!(
        !original.requirements.is_empty(),
        "Merge is atomic and leaves original intact"
    );
}

#[test]
fn rejects_stale_identity_omissions_duplicates_and_reordered_indices() {
    let (fixture, original, payload) = fixture();
    let mut bad = payload.clone();
    bad["serverBuildId"] = json!("stale");
    assert!(
        merged(&fixture, &original, &bad)
            .unwrap_err()
            .contains("identity")
    );
    let mut bad = payload.clone();
    bad["nodes"].as_array_mut().unwrap().pop();
    assert!(merged(&fixture, &original, &bad).is_err());
    let mut bad = payload.clone();
    bad["nodes"][2]["index"] = json!(1);
    assert!(merged(&fixture, &original, &bad).is_err());
    let mut bad = payload.clone();
    bad["nodes"].as_array_mut().unwrap().swap(0, 1);
    assert!(merged(&fixture, &original, &bad).is_err());
    let mut bad = payload.clone();
    bad["routes"][0]["id"] = json!("/other");
    assert!(merged(&fixture, &original, &bad).is_err());
    let mut bad = payload.clone();
    bad["routes"].as_array_mut().unwrap().clear();
    assert!(merged(&fixture, &original, &bad).is_err());
    let mut bad = payload.clone();
    bad["routes"]
        .as_array_mut()
        .unwrap()
        .push(payload["routes"][0].clone());
    assert!(merged(&fixture, &original, &bad).is_err());
    assert!(merge(&fixture.request, "", &original, &payload).is_err());
}

#[test]
fn tagged_values_reject_absence_unknown_kinds_and_wrong_types() {
    let (fixture, original, payload) = fixture();
    for value in [
        json!({"kind":"absent"}),
        json!({"kind":"function"}),
        json!({"kind":"undefined"}),
        json!({"kind":"json","value":"true"}),
        json!({"kind":"json","value":true,"extra":1}),
    ] {
        let mut bad = payload.clone();
        bad["routes"][0]["page"]["ssr"] = value;
        assert!(merged(&fixture, &original, &bad).is_err());
    }
    for value in [
        json!(true),
        json!({"path":"/"}),
        json!([{}]),
        json!(["/", null]),
    ] {
        let mut bad = payload.clone();
        bad["routes"][0]["entries"] = json!({"kind":"json","value":value});
        assert!(merged(&fixture, &original, &bad).is_err());
    }
    for value in [
        json!({"kind":"undefined"}),
        json!({"kind":"json","value":false}),
        json!({"kind":"json","value":null}),
        json!({"kind":"json","value":0}),
        json!({"kind":"json","value":""}),
        json!({"kind":"json","value":[]}),
    ] {
        let mut valid = payload.clone();
        valid["routes"][0]["entries"] = value;
        merged(&fixture, &original, &valid).unwrap();
    }
    for literal in ["1e9999", "-1e9999", "1.7976931348623159e308"] {
        let number: Value = serde_json::from_str(literal).unwrap();
        for pointer in [
            "/routes/0/config/value/runtime",
            "/routes/0/entries/value",
            "/nodes/0/index",
        ] {
            let mut bad = payload.clone();
            *bad.pointer_mut(pointer).unwrap() = number.clone();
            let error = merged(&fixture, &original, &bad).unwrap_err();
            assert!(
                error.contains("finite JavaScript Number"),
                "{pointer}: {error}"
            );
        }
        let mut bad = payload.clone();
        bad["routes"][0]["config"]["value"] = json!({"nested":[{"number":number}]});
        assert!(
            merged(&fixture, &original, &bad)
                .unwrap_err()
                .contains("finite JavaScript Number")
        );
    }
    let mut finite = payload.clone();
    finite["routes"][0]["config"]["value"] =
        serde_json::from_str(r#"{"max":1.7976931348623157e308,"min":5e-324}"#).unwrap();
    merged(&fixture, &original, &finite).unwrap();
}

#[test]
fn validates_module_presence_page_actions_and_method_union() {
    let (mut fixture, original, payload) = fixture();
    let mut bad = payload.clone();
    bad["nodes"][0]["hasServerLoad"] = json!(true);
    assert!(merged(&fixture, &original, &bad).is_err());
    let mut bad = payload.clone();
    bad["nodes"][2]["hasUniversalLoad"] = json!(false);
    assert!(merged(&fixture, &original, &bad).is_err());
    let mut bad = payload.clone();
    bad["routes"][0]["methods"] = json!(["HEAD"]);
    assert!(merged(&fixture, &original, &bad).is_err());
    let mut bad = payload.clone();
    bad["routes"][0]["page"]["actions"] = json!([{"name":"save","callable":true}]);
    assert!(merged(&fixture, &original, &bad).is_err());
    fixture.request.kit.inventory.nodes[2].server =
        Some(fixture.root.join("src/routes/+page.server.ts"));
    bad["routes"][0]["methods"] = json!(["GET", "POST"]);
    bad["routes"][0]["pageMethods"] = json!(["GET", "POST"]);
    let valid = merged(&fixture, &original, &bad).unwrap();
    assert_eq!(
        valid.analysis.routes["/"].actions,
        known(BTreeMap::from([("save".into(), true)]))
    );
    bad["routes"][0]["page"]["actions"]
        .as_array_mut()
        .unwrap()
        .push(json!({"name":"save","callable":false}));
    assert!(merged(&fixture, &original, &bad).is_err());
}

#[test]
fn preserves_original_indices_across_compact_manifest_and_unused_node() {
    let (mut fixture, _, mut payload) = fixture();
    let unused = fixture.request.kit.inventory.nodes[0].clone();
    fixture.request.kit.inventory.nodes.insert(2, unused);
    fixture.request.kit.inventory.routes[0]
        .page
        .as_mut()
        .unwrap()
        .leaf = 3;
    let original = analyze(&fixture.request);
    payload["nodes"][2]["index"] = json!(3);
    let output = merged(&fixture, &original, &payload).unwrap();
    assert_eq!(output.analysis.nodes[2].has_universal_load, known(false));
    assert_eq!(output.analysis.nodes[3].has_universal_load, known(true));
    payload["nodes"][2]["index"] = json!(2);
    assert!(merged(&fixture, &original, &payload).is_err());
}

#[test]
fn endpoint_only_metadata_resolves_trailing_slash_without_inventing_page_facts() {
    let (mut fixture, _, mut payload) = fixture();
    let endpoint = fixture.root.join("src/routes/+server.ts");
    fixture.request.kit.inventory.routes[0].page = None;
    fixture.request.kit.inventory.routes[0].endpoint = Some(endpoint.clone());
    fixture.request.kit.inventory.nodes[2].universal = None;
    fixture.request.modules.insert(
        endpoint.to_string_lossy().into_owned(),
        ts_svelte_kit::build_analysis::ModuleSnapshot {
            source: "export const trailingSlash=(()=> 'always')(); export function GET(){}".into(),
            imports: BTreeMap::new(),
        },
    );
    // Unused page has statically resolved load facts but keeps its module options.
    let original = analyze(&fixture.request);
    payload["nodes"].as_array_mut().unwrap().pop();
    let route = &mut payload["routes"][0];
    route["page"] = Value::Null;
    route["endpoint"] = json!({"trailingSlash":{"kind":"json","value":"always"}});
    route["pageMethods"] = json!([]);
    route["endpointMethods"] = json!(["GET"]);
    route["prerender"] = json!({"kind":"undefined"});
    route["entries"] = json!({"kind":"undefined"});
    let output = merged(&fixture, &original, &payload).unwrap();
    assert_eq!(
        output.analysis.routes["/"].trailing_slash,
        known("always".into())
    );
    assert_eq!(output.analysis.routes["/"].csr, original.routes["/"].csr);
    assert_eq!(output.analysis.routes["/"].ssr, original.routes["/"].ssr);
    assert_eq!(output.analysis.routes["/"].prerender, known(json!(false)));
    payload["routes"][0]["endpoint"]["trailingSlash"] = json!({"kind":"undefined"});
    assert_eq!(
        merged(&fixture, &original, &payload)
            .unwrap()
            .analysis
            .routes["/"]
            .trailing_slash,
        known("never".into())
    );
}

#[test]
fn kit_version_controls_query_method_and_fallback_order() {
    let (mut fixture, _, mut payload) = fixture();
    fixture.request.kit.inventory.routes[0].endpoint =
        Some(fixture.root.join("src/routes/+server.ts"));
    let original = analyze(&fixture.request);
    payload["routes"][0]["endpoint"] = json!({"trailingSlash":{"kind":"undefined"}});
    payload["routes"][0]["endpointMethods"] = json!(["GET", "QUERY", "*"]);
    payload["routes"][0]["methods"] = json!(["GET", "QUERY", "*"]);
    assert!(merged(&fixture, &original, &payload).is_err());
    fixture.request.kit.inventory.kit_version = "3.0.1".into();
    merged(&fixture, &original, &payload).unwrap();
    payload["routes"][0]["endpointMethods"] = json!(["QUERY", "GET", "*"]);
    assert!(merged(&fixture, &original, &payload).is_err());
}
