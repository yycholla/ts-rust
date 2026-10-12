use super::*;
use serde_json::json;
use ts_svelte_kit::build_remote::remote_hash;

fn identities() -> Vec<RemoteIdentity> {
    ["src/a.remote.ts", "src/b.remote.ts"]
        .into_iter()
        .map(|file| RemoteIdentity {
            hash: remote_hash(file),
            file: file.into(),
            module_id: format!("/project/{file}"),
        })
        .collect()
}
fn export(name: &str, kind: &str, dynamic: Value) -> Value {
    json!({"name":name,"kind":{"kind":"json","value":kind},"dynamic":dynamic})
}
fn receipt(expected: &[RemoteIdentity]) -> Value {
    json!({"serverBuildId":"server-sha256:current","remotes":expected.iter().map(|identity|json!({"hash":identity.hash,"file":identity.file,"exports":[]})).collect::<Vec<_>>()})
}
const BUILD: &str = "server-sha256:current";

#[test]
fn complete_receipt_produces_ordered_client_exports_and_retains_dynamic_tags() {
    let expected = identities();
    let mut value = receipt(&expected);
    value["remotes"][0]["exports"] = json!([
        export("z", "query", json!({"kind":"json","value":true})),
        export("a", "prerender", json!({"kind":"undefined"})),
        export("雪", "prerender", json!({"kind":"json","value":false})),
        export("same", "prerender", json!({"kind":"json","value":true})),
    ]);
    value["remotes"][1]["exports"] = json!([
        export("same", "command", json!({"kind":"json","value":true})),
        export("form", "form", json!({"kind":"json","value":true})),
        export("batch", "query_batch", json!({"kind":"json","value":true})),
        export("live", "query_live", json!({"kind":"json","value":true})),
    ]);
    let result = validate(BUILD, &expected, &value).unwrap();
    assert_eq!(result.server_build_id, BUILD);
    assert_eq!(
        result.evaluated[0]
            .exports
            .iter()
            .map(|e| e.name.as_str())
            .collect::<Vec<_>>(),
        ["z", "a", "雪", "same"]
    );
    assert_eq!(result.evaluated[1].exports[2].kind, RemoteKind::QueryBatch);
    assert_eq!(result.evaluated[1].exports[3].kind, RemoteKind::QueryLive);
    assert_eq!(
        result.remotes[0].exports[1].dynamic,
        RuntimeDynamic::Undefined
    );
    assert_eq!(
        result.remotes[0].exports[2].dynamic,
        RuntimeDynamic::Json(false)
    );
    let report = serde_json::to_value(&result).unwrap();
    assert_eq!(
        report["remotes"][0]["exports"][1]["dynamic"],
        json!({"kind":"undefined"})
    );
    assert_eq!(
        report["remotes"][0]["exports"][2]["dynamic"],
        json!({"kind":"json","value":false})
    );
    assert_eq!(
        report["evaluated"][0]["exports"][0],
        json!({"name":"z","kind":"query"})
    );
}
#[test]
fn old_build_wrong_order_wrong_hash_and_wrong_file_fail_without_mutation() {
    let expected = identities();
    let original = receipt(&expected);
    assert!(validate("", &expected, &original).is_err());
    assert!(
        validate("server-sha256:other", &expected, &original)
            .unwrap_err()
            .contains("build identity mismatch")
    );
    for variant in 0..5 {
        let mut value = original.clone();
        match variant {
            0 => value["remotes"].as_array_mut().unwrap().swap(0, 1),
            1 => value["remotes"][0]["hash"] = json!("other"),
            2 => value["remotes"][0]["file"] = json!("src/stale.remote.ts"),
            3 => {
                value["remotes"].as_array_mut().unwrap().pop();
            }
            _ => {
                let extra = value["remotes"][0].clone();
                value["remotes"].as_array_mut().unwrap().push(extra);
            }
        }
        let before = value.clone();
        assert!(validate(BUILD, &expected, &value).is_err());
        assert_eq!(value, before);
    }
    assert!(validate(BUILD, &expected, &original).is_ok());
}
#[test]
fn duplicate_or_invalid_expected_inventory_is_not_deduplicated_or_guessed() {
    let expected = identities();
    for (field, replacement) in [
        ("hash", expected[0].hash.clone()),
        ("file", expected[0].file.clone()),
        ("module", expected[0].module_id.clone()),
    ] {
        let mut bad = expected.clone();
        match field {
            "hash" => bad[1].hash = replacement,
            "file" => bad[1].file = replacement,
            _ => bad[1].module_id = replacement,
        }
        assert!(
            validate(BUILD, &bad, &receipt(&bad))
                .unwrap_err()
                .contains("duplicate or ambiguous")
        );
    }
    let mut bad = expected.clone();
    bad[0].hash = "not-the-hash-of-file".into();
    assert!(
        validate(BUILD, &bad, &receipt(&bad))
            .unwrap_err()
            .contains("hash mismatch")
    );
}
#[test]
fn duplicate_export_names_fail_but_distinct_string_names_are_not_reinterpreted() {
    let expected = identities();
    let mut value = receipt(&expected);
    let item = export("duplicate", "query", json!({"kind":"json","value":true}));
    value["remotes"][0]["exports"] = json!([item.clone(), item]);
    assert!(
        validate(BUILD, &expected, &value)
            .unwrap_err()
            .contains("Duplicate runtime remote export")
    );
    value["remotes"][0]["exports"] = json!([
        export("with space", "query", json!({"kind":"json","value":true})),
        export("__proto__", "query", json!({"kind":"json","value":true}))
    ]);
    let result = validate(BUILD, &expected, &value).unwrap();
    assert_eq!(result.evaluated[0].exports[0].name, "with space");
    assert_eq!(result.evaluated[0].exports[1].name, "__proto__");
    // Kit-version-specific client syntax validation belongs to its transform.
}
#[test]
fn all_dynamic_invalid_values_are_rejected_instead_of_coerced_or_dropped() {
    let expected = identities();
    for invalid in [
        json!({"kind":"absent"}),
        json!({"kind":"undefined","value":null}),
        json!({"kind":"json","value":null}),
        json!({"kind":"json","value":0}),
        json!({"kind":"json","value":"false"}),
        json!({"kind":"json","value":{}}),
        json!({"kind":"json","value":[]}),
        json!({"kind":"json"}),
        json!({"kind":"json","value":true,"extra":false}),
        json!(true),
    ] {
        let mut value = receipt(&expected);
        value["remotes"][0]["exports"] = json!([export("p", "prerender", invalid)]);
        assert!(validate(BUILD, &expected, &value).is_err(), "{value}");
    }
    for dynamic in [
        json!({"kind":"undefined"}),
        json!({"kind":"json","value":false}),
    ] {
        let mut value = receipt(&expected);
        value["remotes"][0]["exports"] = json!([export("q", "query", dynamic)]);
        assert!(
            validate(BUILD, &expected, &value)
                .unwrap_err()
                .contains("Non-prerender")
        );
    }
}
#[test]
fn kind_tags_and_result_shape_are_exact_and_unknown_fields_are_not_ignored() {
    let expected = identities();
    for kind in [
        json!("query"),
        json!({"kind":"undefined"}),
        json!({"kind":"json","value":"queryBatch"}),
        json!({"kind":"json","value":"mutation"}),
        json!({"kind":"json","value":true}),
        json!({"kind":"json","value":"query","unused":17}),
    ] {
        let mut value = receipt(&expected);
        let mut item = export("q", "query", json!({"kind":"json","value":true}));
        item["kind"] = kind;
        value["remotes"][0]["exports"] = json!([item]);
        assert!(validate(BUILD, &expected, &value).is_err());
    }
    let original = receipt(&expected);
    assert!(validate(BUILD, &expected, &json!({"ok":true,"result":original})).is_err());
    for target in ["root", "module", "export"] {
        let mut value = receipt(&expected);
        value["remotes"][0]["exports"] =
            json!([export("q", "query", json!({"kind":"json","value":true}))]);
        match target {
            "root" => value["extra"] = Value::Null,
            "module" => value["remotes"][0]["extra"] = Value::Null,
            _ => value["remotes"][0]["exports"][0]["extra"] = Value::Null,
        }
        assert!(validate(BUILD, &expected, &value).is_err());
    }
}
#[test]
fn missing_dynamic_and_null_inventory_are_errors_and_empty_inventory_is_valid() {
    let expected = identities();
    let mut value = receipt(&expected);
    value["remotes"][0]["exports"] = json!([{"name":"q","kind":{"kind":"json","value":"query"}}]);
    assert!(validate(BUILD, &expected, &value).is_err());
    value["remotes"] = Value::Null;
    assert!(validate(BUILD, &expected, &value).is_err());
    let result = validate(BUILD, &[], &receipt(&[])).unwrap();
    assert!(result.evaluated.is_empty());
    assert!(result.remotes.is_empty());
}
#[test]
fn inventory_export_and_string_limits_are_explicit() {
    let expected = identities();
    let huge_inventory = vec![expected[0].clone(); MAX_REMOTES + 1];
    assert!(
        validate(BUILD, &huge_inventory, &Value::Null)
            .unwrap_err()
            .contains("module limit")
    );
    let mut value = receipt(&expected);
    value["remotes"][0]["exports"] = Value::Array(vec![Value::Null; MAX_EXPORTS + 1]);
    assert!(
        validate(BUILD, &expected, &value)
            .unwrap_err()
            .contains("exports exceed native limit")
    );
    value = receipt(&expected);
    value["serverBuildId"] = json!("x".repeat(MAX_STRING + 1));
    assert!(
        validate(BUILD, &expected, &value)
            .unwrap_err()
            .contains("string limit")
    );
}
