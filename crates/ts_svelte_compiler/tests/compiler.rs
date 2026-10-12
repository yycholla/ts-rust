use serde_json::{Value, json};
use ts_svelte_compiler::dispatch;

fn request(op: &str, source: &str, extra: Value) -> Value {
    let mut input =
        json!({"op":op,"document":{"uri":"Fixture.svelte","version":3,"text":source},"runes":true});
    let Value::Object(extra) = extra else {
        panic!("expected request options object")
    };
    input.as_object_mut().unwrap().extend(extra);
    dispatch(input.to_string().as_bytes())
}

#[test]
fn executable_client_and_server_from_typescript() {
    for target in ["client", "server"] {
        let response = request(
            "compile",
            "<script lang=\"ts\">let {count}: {count: number} = $props();</script><p>{count}</p>",
            json!({"target":target}),
        );
        assert_eq!(response["ok"], true, "{response}");
        let code = response["result"]["javascript"]["code"].as_str().unwrap();
        assert!(code.contains("svelte/internal/"));
        assert!(!code.contains("count: number"));
        assert_eq!(response["result"]["version"], 3);
    }
}

#[test]
fn projection_preserves_types_and_copied_source_spans() {
    let source = "<script lang=\"ts\">let {count}: {count: number} = $props();</script><p>😀{count.toFixed()}</p>";
    let response = request("project", source, json!({"typescript":true}));
    assert_eq!(response["ok"], true, "{response}");
    let result = &response["result"];
    let code = result["code"].as_str().unwrap();
    assert!(code.contains("count: number"));
    assert!(code.contains("count.toFixed()"));
    let mappings = result["mappings"].as_array().unwrap();
    assert!(!mappings.is_empty());
    for mapping in mappings {
        let slice = |text: &str, range: &Value| {
            text[usize::try_from(range["start"].as_u64().unwrap()).unwrap()
                ..usize::try_from(range["end"].as_u64().unwrap()).unwrap()]
                .to_owned()
        };
        assert_eq!(
            slice(source, &mapping["source"]),
            slice(code, &mapping["generated"])
        );
    }
}

#[test]
fn error_does_not_poison_later_request() {
    let invalid = request("compile", "<script><script>", json!({"target":"server"}));
    assert_eq!(invalid["ok"], false);
    assert!(!invalid["diagnostics"].as_array().unwrap().is_empty());
    assert_eq!(
        request("compile", "<p>valid</p>", json!({"target":"server"}))["ok"],
        true
    );
}

#[test]
fn unsupported_and_malformed_requests_are_explicit() {
    assert_eq!(
        request("compile", "\u{feff}<p>x</p>", json!({"target":"client"}))["error"]["code"],
        "unsupported_bom"
    );
    assert_eq!(
        dispatch(br#"{"op":"capabilities","extra":true}"#)["error"]["code"],
        "invalid_request"
    );
    assert_eq!(
        dispatch(br#"{"op":"check"}"#)["error"]["code"],
        "invalid_request"
    );
    assert_eq!(
        dispatch(br#"{"op":"capabilities"}"#)["result"]["typeChecking"],
        false
    );
}

#[test]
fn warning_coordinates_follow_original_unicode_and_crlf() {
    let source = "😀\r\n<img src=\"x.png\">";
    let response = request("compile", source, json!({"target":"client"}));
    assert_eq!(response["ok"], true, "{response}");
    let warnings = response["result"]["diagnostics"].as_array().unwrap();
    let warning = warnings
        .iter()
        .find(|d| d["code"] == "a11y_missing_attribute")
        .unwrap();
    assert_eq!(warning["bytes"], json!({"start":6,"end":23}));
    assert_eq!(
        warning["range"],
        json!({"start":{"line":1,"character":0},"end":{"line":1,"character":17}})
    );
    let map: Value =
        serde_json::from_str(response["result"]["javascript"]["map"].as_str().unwrap()).unwrap();
    assert_eq!(map["sourcesContent"][0], source);
}

#[test]
fn analysis_failure_preserves_structured_message_and_span() {
    let response = request(
        "compile",
        "<script>const doubled = $derived();</script>",
        json!({"target":"server"}),
    );
    assert_eq!(response["ok"], false, "{response}");
    let error = &response["diagnostics"][0];
    assert_eq!(error["code"], "rune_invalid_arguments_length");
    assert_eq!(error["bytes"], json!({"start":24,"end":34}));
    assert_eq!(
        error["message"],
        "`$derived` must be called with exactly one argument\nhttps://svelte.dev/e/rune_invalid_arguments_length"
    );
}

#[test]
fn projection_language_uses_parsed_scripts() {
    for source in [
        "<script lang=\"ts\">let count: number = 1;</script>{count}",
        "<script module lang=\"ts\">export const count: number = 1;</script>{count}",
    ] {
        assert_eq!(
            request("project", source, json!({"typescript":false}))["error"]["code"],
            "projection_language"
        );
    }
    let source = "<script>const text = '<script lang=\"ts\">';</script>{text}";
    for typescript in [false, true] {
        let response = request("project", source, json!({"typescript":typescript}));
        assert_eq!(response["ok"], true, "{response}");
        assert!(
            response["result"]["code"]
                .as_str()
                .unwrap()
                .contains("const text")
        );
    }
}

#[test]
fn selected_profile_follows_success_and_failure_without_leaking() {
    for version in ts_svelte::compatibility::SVELTE_VERSIONS {
        for source in ["<p>valid</p>", "<script><script>"] {
            let response = request(
                "compile",
                source,
                json!({"target":"server","compatibility":{"svelteVersion":version}}),
            );
            let body = if response["ok"] == true {
                &response["result"]
            } else {
                &response
            };
            assert_eq!(body["compatibility"]["svelteVersion"], *version);
        }
    }
    let response = request("compile", "<p>default</p>", json!({"target":"server"}));
    assert_eq!(
        response["result"]["compatibility"]["svelteVersion"],
        "5.53.11"
    );
}

#[test]
fn unknown_version_is_not_silently_compiled_with_the_default() {
    for op in ["compile", "project"] {
        let options = if op == "compile" {
            json!({"target":"server","compatibility":{"svelteVersion":"latest"}})
        } else {
            json!({"typescript":true,"compatibility":{"svelteVersion":"latest"}})
        };
        let response = request(op, "<p>valid</p>", options);
        assert_eq!(response["ok"], false);
        assert_eq!(response["error"]["code"], "unsupported_version");
        assert!(response.get("result").is_none());
    }
}
