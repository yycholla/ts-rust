//! Module-parser regressions for valid export declarations at EOF. Keep the
//! missing semicolons: adding one would hide the vendored Boa parser defect.
use crate::{HostSnapshot, ModuleSource, Resolution, Runtime, RuntimeLimits, RuntimeValue};
use serde_json::json;
use std::collections::BTreeMap;

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
                    "dep".into(),
                    ModuleSource {
                        source: "const value=41;export {value,value as 'with space'};".into(),
                        import_meta_url: "file:///dep.js".into(),
                    },
                ),
            ]),
            resolutions: BTreeMap::from([(
                "entry".into(),
                BTreeMap::from([("./dep.js".into(), Resolution::Module { id: "dep".into() })]),
            )]),
        },
        RuntimeLimits::default(),
    )
    .unwrap()
}
fn exported(source: &str, name: &str) {
    let value = runtime(source)
        .read_export("entry", name)
        .unwrap_or_else(|error| panic!("{source:?}: {error:?}"));
    match value {
        RuntimeValue::Json(value) => assert_eq!(value, json!(41), "{source:?}"),
        _ => panic!("Missing actual exported value for {source:?}"),
    }
}
#[test]
fn named_exports_at_eof_accept_asi_and_arbitrary_string_aliases() {
    for suffix in [
        "",
        "\n",
        "\r\n",
        " // final line comment",
        " /* final block comment */",
        "\n// final comment",
    ] {
        exported(&format!("const value=41;export {{value}}{suffix}"), "value");
        exported(
            &format!("const value=41;export {{value as 'with space'}}{suffix}"),
            "with space",
        );
        exported(
            &format!("const value=41;export {{value as '雪/💙'}}{suffix}"),
            "雪/💙",
        );
        runtime(&format!("export {{}}{suffix}"))
            .evaluate("entry")
            .unwrap();
    }
}
#[test]
fn reexports_and_star_exports_at_eof_accept_asi_and_resolve_actual_bindings() {
    for suffix in ["", "\n", " // final comment", " /* final comment */"] {
        exported(
            &format!("export {{value as answer}} from './dep.js'{suffix}"),
            "answer",
        );
        exported(
            &format!("export {{'with space' as answer}} from './dep.js'{suffix}"),
            "answer",
        );
        exported(&format!("export * from './dep.js'{suffix}"), "value");
        runtime(&format!("export * as namespace from './dep.js'{suffix}"))
            .evaluate("entry")
            .unwrap();
        runtime(&format!("export {{}} from './dep.js'{suffix}"))
            .evaluate("entry")
            .unwrap();
    }
}
#[test]
fn export_eof_repair_does_not_accept_missing_bindings_or_incomplete_reexports() {
    for source in [
        "export {missing}",
        "export {value} from",
        "export {value} from 42",
        "export * from",
        "export * as namespace from",
        "export {missing} from './dep.js'",
        "const value=41;export {value as",
        "const value=41;export {value",
        "const value=41;export {value} unexpected",
    ] {
        assert!(
            runtime(source).evaluate("entry").is_err(),
            "Accepted invalid module {source:?}"
        );
    }
}
