use oxc_allocator::Allocator;
use oxc_parser::Parser;
use oxc_span::SourceType;
use ts_svelte_transpile::{Jsx, Phase, Severity, TransformOptions, TransformOutput, transform};

fn emit(source: &str, filename: &str, options: &TransformOptions) -> TransformOutput {
    let output = transform(source, filename, options).unwrap();
    assert!(
        !output
            .diagnostics
            .iter()
            .any(|d| d.severity == Severity::Error)
    );
    let allocator = Allocator::default();
    let parsed = Parser::new(&allocator, &output.code, SourceType::mjs()).parse();
    assert!(!parsed.fatal_error);
    assert!(
        parsed.diagnostics.is_empty(),
        "Generated JavaScript must parse without TypeScript/JSX: {:?}\n{}",
        parsed.diagnostics,
        output.code
    );
    output
}

#[test]
fn rune_calls_and_class_fields_survive_type_erasure() {
    let source = "interface Counter { value: number }\nexport class Store { count = $state<number>(0); total = $derived(this.count * 2); }\nexport const create = (value: Counter) => $state<Counter>(value);";
    let output = emit(source, "store.svelte.ts", &TransformOptions::default());
    assert!(!output.code.contains("interface Counter"));
    assert!(!output.code.contains("<number>"));
    assert!(!output.code.contains(": Counter"));
    assert!(output.code.contains("count = $state(0)"));
    assert!(output.code.contains("total = $derived(this.count * 2)"));
    assert!(output.code.contains("$state(value)"));
    assert!(!output.code.contains("constructor"));
}

#[test]
fn explicit_type_imports_are_erased_and_value_import_policy_is_resolved() {
    let source = "import type { Removed } from './only-types'; import { type AlsoRemoved, kept } from './mixed'; import { Unused } from './unused'; export const value: Removed = kept;";
    let output = emit(source, "imports.ts", &TransformOptions::default());
    assert!(!output.code.contains("only-types"));
    assert!(!output.code.contains("AlsoRemoved"));
    assert!(output.code.contains("kept"));
    assert!(output.code.contains("./unused"));
    let options = TransformOptions {
        only_remove_type_imports: false,
        ..TransformOptions::default()
    };
    let output = emit(source, "imports.ts", &options);
    assert!(!output.code.contains("./unused"));
}

#[test]
fn runtime_enums_namespaces_and_parameter_properties_are_lowered() {
    let source = "export enum Status { Ready = 2, Done, Label = 'done' }\nexport namespace Values { export const answer = Status.Done; export function read() { return answer; } }\nexport class Item { constructor(public id: number, private label = 'x') {} read() { return this.id + this.label; } }";
    let output = emit(source, "runtime.ts", &TransformOptions::default());
    assert!(!output.code.contains("enum Status"));
    assert!(!output.code.contains("namespace Values"));
    assert!(!output.code.contains("public id"));
    assert!(!output.code.contains("private label"));
    assert!(output.code.contains("this.id = id"));
    assert!(output.code.contains("this.label = label"));
    assert!(output.code.contains("Status"));
    assert!(output.code.contains("Ready"));
    assert!(output.code.contains("Done"));
    assert!(
        output.code.contains("= 3"),
        "enum auto-increment must retain the runtime value: {}",
        output.code
    );
    assert!(output.code.contains("Values"));
    assert!(output.code.contains("read"));
}

#[test]
fn const_enums_keep_runtime_objects_in_isolated_emission() {
    let output = emit(
        "export const enum Flag { Enabled = 4 } export const value = Flag.Enabled;",
        "flags.ts",
        &TransformOptions::default(),
    );
    assert!(!output.code.contains("const enum"));
    assert!(output.code.contains("Flag"));
    assert!(output.code.contains("Enabled"));
    assert!(output.code.contains("= 4"));
}

#[test]
fn jsx_is_lowered_only_with_an_explicit_runtime() {
    let source = "export const View = (p: {name: string}) => <><h1>{p.name}</h1></>;";
    let failure = transform(source, "view.tsx", &TransformOptions::default()).unwrap_err();
    assert!(
        failure
            .diagnostics
            .iter()
            .any(|d| d.message.contains("JSX requires"))
    );
    let automatic = TransformOptions {
        jsx: Jsx::Automatic {
            import_source: "preact".into(),
            development: false,
        },
        ..TransformOptions::default()
    };
    let output = emit(source, "view.tsx", &automatic);
    assert!(output.code.contains("preact/jsx-runtime"));
    assert!(output.code.contains("h1"));
    let classic = TransformOptions {
        jsx: Jsx::Classic {
            factory: "h".into(),
            fragment: "Fragment".into(),
        },
        ..TransformOptions::default()
    };
    let output = emit(source, "view.tsx", &classic);
    assert!(output.code.contains("h(Fragment"));
    assert!(output.code.contains("h(\"h1\""));
    assert!(!output.code.contains("jsx-runtime"));
}

#[test]
fn invalid_javascript_and_semantic_early_errors_keep_diagnostics() {
    let failure = transform(
        "export const broken = ;",
        "broken.js",
        &TransformOptions::default(),
    )
    .unwrap_err();
    assert!(
        failure.diagnostics.iter().any(|d| d.phase == Phase::Parse
            && d.severity == Severity::Error
            && !d.labels.is_empty())
    );
    assert!(
        failure
            .diagnostics
            .iter()
            .all(|d| d.filename == "broken.js")
    );
    let failure = transform(
        "let duplicate = 1; let duplicate = 2;",
        "duplicate.ts",
        &TransformOptions::default(),
    )
    .unwrap_err();
    assert!(
        failure
            .diagnostics
            .iter()
            .any(|d| d.phase == Phase::Semantic && d.severity == Severity::Error)
    );
    let output = emit(
        "export const latest = object?.field ?? 7;",
        "valid.js",
        &TransformOptions::default(),
    );
    assert!(output.code.contains("object?.field ?? 7"));
}

#[test]
fn source_maps_preserve_original_content_identity_and_rune_positions() {
    let source = "type Count = number;\nexport const value: Count = $state<Count>(1);\n";
    let output = emit(
        source,
        "/project/state.svelte.ts",
        &TransformOptions::default(),
    );
    let map = output.map.unwrap();
    assert_eq!(map["version"], 3);
    assert_eq!(map["sources"][0], "/project/state.svelte.ts");
    assert_eq!(map["sourcesContent"][0], source);
    let serialized = map.to_string();
    let decoded = oxc_sourcemap::SourceMap::from_json_string(&serialized).unwrap();
    let source_column = source.lines().nth(1).unwrap().find("$state").unwrap() as u32;
    assert!(
        decoded
            .get_source_view_tokens()
            .any(|token| token.get_src_line() == 1 && token.get_src_col() == source_column)
    );
    let output = emit(
        source,
        "no-map.ts",
        &TransformOptions {
            source_map: false,
            ..TransformOptions::default()
        },
    );
    assert!(output.map.is_none());
}

#[test]
fn unsupported_contracts_are_errors_without_partial_output() {
    for (source, filename, message) in [
        (
            "export declare const value: number;",
            "types.d.ts",
            "Declaration files",
        ),
        ("@decorator class Example {}", "decorated.ts", "Decorator"),
        (
            "import fs = require('node:fs'); export {fs};",
            "require.ts",
            "CommonJS",
        ),
        ("const value = 1; export = value;", "exports.ts", "CommonJS"),
        ("const value = 1;", "config.cjs", "CommonJS"),
    ] {
        let failure = transform(source, filename, &TransformOptions::default()).unwrap_err();
        assert!(
            failure.to_string().contains(message),
            "{filename}: {failure}"
        );
    }
    let options = TransformOptions {
        use_define_for_class_fields: false,
        ..TransformOptions::default()
    };
    assert!(
        transform("class Example {}", "fields.ts", &options)
            .unwrap_err()
            .to_string()
            .contains("useDefineForClassFields")
    );
    assert!(serde_json::from_str::<TransformOptions>(r#"{"target":"es2015"}"#).is_err());
    assert!(serde_json::from_str::<TransformOptions>(r#"{"module":"commonjs"}"#).is_err());
    assert!(
        serde_json::from_str::<TransformOptions>(r#"{"experimentalDecorators":true}"#).is_err()
    );
    let options: TransformOptions = serde_json::from_str(
        r#"{"jsx":{"mode":"automatic","importSource":"preact","development":false}}"#,
    )
    .unwrap();
    assert!(matches!(options.jsx, Jsx::Automatic { .. }));
}
