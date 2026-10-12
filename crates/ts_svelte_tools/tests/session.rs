use serde_json::{Value, json};
use ts_svelte_tools::session::Session;
fn call(s: &mut Session, r: &Value) -> Value {
    s.request(r.to_string().as_bytes())
}
#[test]
fn repairs_diagnostics_and_preserves_snapshot_on_invalid_edit() {
    let mut s = Session::default();
    let uri = "file:///a.svelte";
    let r = call(
        &mut s,
        &json!({"op":"sessionOpen","document":{"uri":uri,"version":1,"text":"{#if}"}}),
    );
    assert!(!r["result"]["diagnostics"].as_array().unwrap().is_empty());
    let r = call(
        &mut s,
        &json!({"op":"sessionChange","uri":uri,"version":2,"edits":[{"text":"<p>okay</p>","range":null}]}),
    );
    assert_eq!(r["result"]["diagnostics"], json!([]));
    let r = call(
        &mut s,
        &json!({"op":"sessionChange","uri":uri,"version":3,"edits":[{"text":"<p>temporary</p>","range":null},{"text":"bad","range":{"start":{"line":99,"character":0},"end":{"line":99,"character":0}}}]}),
    );
    assert_eq!(r["ok"], false);
    assert_eq!(s.snapshots()[0].text, "<p>okay</p>");
    assert_eq!(s.snapshots()[0].version, 2);
}
#[test]
fn profile_change_invalidates_same_document() {
    let mut s = Session::default();
    let uri = "file:///a.svelte";
    call(
        &mut s,
        &json!({"op":"sessionOpen","document":{"uri":uri,"version":1,"text":"{const value = 1}<p>{value}</p>"}}),
    );
    let old = call(&mut s, &json!({"op":"sessionDiagnostics","uri":uri}));
    assert!(!old["result"]["diagnostics"].as_array().unwrap().is_empty());
    assert_eq!(
        call(
            &mut s,
            &json!({"op":"sessionConfigure","compatibility":{"svelteVersion":"5.57.2"}})
        )["ok"],
        true
    );
    let new = call(&mut s, &json!({"op":"sessionDiagnostics","uri":uri}));
    assert_eq!(new["result"]["diagnostics"], json!([]));
}
#[test]
fn close_releases_snapshots_and_caches() {
    let mut s = Session::default();
    for n in 0..20 {
        let uri = format!("file:///{n}.svelte");
        call(
            &mut s,
            &json!({"op":"sessionOpen","document":{"uri":uri,"version":1,"text":"<p>Hello</p>"}}),
        );
        call(&mut s, &json!({"op":"sessionClose","uri":uri}));
    }
    assert!(s.snapshots().is_empty());
    assert_eq!(
        call(&mut s, &json!({"op":"sessionStats"}))["result"]["cachedDiagnostics"],
        0
    );
}
#[test]
fn lint_walks_syntax_not_strings_and_supports_jsx() {
    for (uri, text, expected) in [
        ("x.ts", "// debugger\nconst x='debugger; ==';", 0),
        ("x.ts", "debugger; const x = 1 == 2;", 2),
        ("x.tsx", "const node=<div/>;", 0),
        ("x.jsx", "const node=<div/>;", 0),
    ] {
        let r = ts_svelte_tools::dispatch(
            json!({"op":"lint","document":{"uri":uri,"version":1,"text":text}})
                .to_string()
                .as_bytes(),
        );
        assert_eq!(r["ok"], true);
        assert_eq!(
            r["result"]["diagnostics"].as_array().unwrap().len(),
            expected,
            "{r}"
        );
    }
}
