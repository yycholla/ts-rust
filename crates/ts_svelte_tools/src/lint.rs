//! Native syntax-aware lint rules with resolved JSON settings. Project JavaScript
//! configuration is never evaluated here; hosts resolve overrides and presets.
use oxc_allocator::Allocator;
use oxc_ast_visit::Visit;
use oxc_parser::Parser;
use oxc_semantic::SemanticBuilder;
use oxc_span::SourceType;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use ts_svelte::compatibility::Profile;
use ts_svelte::{Document, Snapshot};

mod config;
mod native;
mod rules;
use config::Config;
use rules::Rules;

/// Validate resolved rule settings before mutating an editor session.
/// # Errors
/// Unknown enabled rules, unsupported options and invalid severity values fail.
pub fn validate_config(config: &Value) -> Result<(), String> {
    Config::parse(config).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(uri: &str, source: &str, rules: Value) -> Value {
        let profile = ts_svelte::compatibility::resolve(&Default::default()).unwrap();
        lint_configured(
            Snapshot {
                uri: uri.into(),
                version: 1,
                text: source.into(),
            },
            Some(true),
            &profile,
            json!({"rules":rules}),
        )
    }

    fn codes(result: &Value) -> Vec<&str> {
        result["result"]["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .map(|d| d["code"].as_str().unwrap())
            .collect()
    }

    #[test]
    fn settings_severity_aliases_disabled_and_unknown_rules() {
        let result = check(
            "file:///test.ts",
            "debugger; var x: any = 1;",
            json!({"no-debugger":0,"no-var":2,"typescript/no-explicit-any":["warn",{"fixToUnknown":true}]}),
        );
        assert_eq!(result["ok"], true);
        assert_eq!(codes(&result), vec!["no-var", "typescript/no-explicit-any"]);
        assert_eq!(result["result"]["diagnostics"][0]["severity"], "error");
        assert_eq!(result["result"]["diagnostics"][1]["severity"], "warning");
        assert!(validate_config(&json!({"rules":{"unknown/rule":"off"}})).is_ok());
        assert!(validate_config(&json!({"rules":{"unknown/rule":"warn"}})).is_err());
        assert!(
            validate_config(&json!({"rules":{"no-console":["warn",{"unknown":true}]}})).is_err()
        );
        assert!(validate_config(&json!({"rules":{"no-var":"fatal"}})).is_err());
        assert!(validate_config(&Value::Null).is_ok());
        assert!(validate_config(&json!({})).is_ok());
    }

    #[test]
    fn globals_are_scope_aware_and_console_allow_list_is_respected() {
        let result = check(
            "file:///test.ts",
            "console.log('bad'); console.warn('ok'); function local(console: {log():void}, Promise: any, Array: any) { console.log(); new Promise(async () => {}); Array(); } new Promise(async () => {}); new Array();",
            json!({"no-console":["error",{"allow":["warn"]}],"no-async-promise-executor":"error","no-array-constructor":"warn"}),
        );
        assert_eq!(result["ok"], true);
        assert_eq!(
            codes(&result),
            vec![
                "no-console",
                "no-async-promise-executor",
                "no-array-constructor"
            ]
        );
    }

    #[test]
    fn script_comments_and_type_only_names_do_not_trigger_runtime_rules() {
        let result = check(
            "file:///test.ts",
            "// console.log(); var any = 1;\nconst text = 'console.log debugger any'; type T = { console: { log(): void } }; type U = typeof console.log;\n// eslint-disable-next-line no-console\nconsole.log('hidden');\nconsole.log('shown'); // eslint-disable-line no-console\n/* eslint-disable no-var */ var hidden = 1; /* eslint-enable no-var */ var shown = 2;",
            json!({"no-console":"warn","no-var":"error","@typescript-eslint/no-explicit-any":"error"}),
        );
        assert_eq!(result["ok"], true);
        assert_eq!(codes(&result), vec!["no-var"]);
        let range = &result["result"]["diagnostics"][0]["bytes"];
        assert_eq!(
            &"// console.log(); var any = 1;\nconst text = 'console.log debugger any'; type T = { console: { log(): void } }; type U = typeof console.log;\n// eslint-disable-next-line no-console\nconsole.log('hidden');\nconsole.log('shown'); // eslint-disable-line no-console\n/* eslint-disable no-var */ var hidden = 1; /* eslint-enable no-var */ var shown = 2;"
                [range["start"].as_u64().unwrap() as usize
                    ..range["end"].as_u64().unwrap() as usize],
            "var shown = 2;"
        );
    }

    #[test]
    fn equality_typeof_nan_and_negation_options() {
        let result = check(
            "file:///test.ts",
            "let x: unknown; if (x == null) {} if (typeof x == 'number') {} if (1 == 2) {} if (x == 2) {} if (x === -0) {} if (x === NaN) {} if (typeof x === 'strnig') {} if (typeof x === 42) {} if (!x in {}) {} if ((!x) in {}) {}",
            json!({"eqeqeq":["warn","smart"],"valid-typeof":"error","use-isnan":"error","no-compare-neg-zero":"error","no-unsafe-negation":"error"}),
        );
        assert_eq!(result["ok"], true);
        assert_eq!(
            codes(&result),
            vec![
                "eqeqeq",
                "no-compare-neg-zero",
                "use-isnan",
                "valid-typeof",
                "valid-typeof",
                "no-unsafe-negation"
            ]
        );
        let result = check(
            "file:///test.ts",
            "let x: unknown; x === null; x == null;",
            json!({"eqeqeq":["warn","always",{"null":"never"}]}),
        );
        assert_eq!(codes(&result), vec!["eqeqeq"]);
    }

    #[test]
    fn native_structural_and_typescript_rules_use_ast_spans() {
        let result = check(
            "file:///test.ts",
            "const [] = []; const {} = {}; const sparse = [1,,2]; class C { static {} } try { work(); } catch (error) { throw error; } switch (x) { case 1: let y = 2; break; } type Wrapped = String; type Generic<T extends unknown> = T; let value: any; value!!; value?.field!;",
            json!({"no-empty-pattern":"warn","no-sparse-arrays":"warn","no-empty-static-block":"warn","no-useless-catch":"warn","no-case-declarations":"warn","typescript/no-wrapper-object-types":"warn","typescript/no-unnecessary-type-constraint":"warn","typescript/no-explicit-any":"warn","typescript/no-extra-non-null-assertion":"warn","typescript/no-non-null-asserted-optional-chain":"warn"}),
        );
        assert_eq!(result["ok"], true);
        assert_eq!(
            codes(&result),
            vec![
                "no-empty-pattern",
                "no-empty-pattern",
                "no-sparse-arrays",
                "no-empty-static-block",
                "no-useless-catch",
                "no-case-declarations",
                "typescript/no-wrapper-object-types",
                "typescript/no-unnecessary-type-constraint",
                "typescript/no-explicit-any",
                "typescript/no-extra-non-null-assertion",
                "typescript/no-non-null-asserted-optional-chain"
            ]
        );
        assert!(
            result["result"]["diagnostics"]
                .as_array()
                .unwrap()
                .iter()
                .all(|d| !d["range"].is_null())
        );
    }

    #[test]
    fn svelte_script_regions_preserve_template_and_module_scope_distinctions() {
        let result = check(
            "file:///Component.svelte",
            "<script module>const console = { log() {} };</script><script lang=\"ts\">console.log(); var count: any = 1;</script><p>var any; console.log();</p>",
            json!({"no-console":"error","no-var":"warn","typescript/no-explicit-any":"warn","svelte/compiler":"off"}),
        );
        assert_eq!(result["ok"], true, "{result}");
        assert_eq!(codes(&result), vec!["no-var", "typescript/no-explicit-any"]);
        assert!(validate_config(&json!({"rules":{"svelte/require-each-key":"warn"}})).is_ok());
    }

    #[test]
    fn native_template_rules_are_opt_in_and_respect_directives() {
        let source = "<!-- eslint-disable-next-line svelte/button-has-type -->\n<button>Hidden</button>\n<button>Shown</button>\n{#each [1,2] as item}<p>{item}</p>{/each}";
        let result = check(
            "file:///Template.svelte",
            source,
            json!({"svelte/compiler":"off","svelte/button-has-type":"error","svelte/require-each-key":"warn"}),
        );
        assert_eq!(result["ok"], true, "{result}");
        assert_eq!(
            codes(&result),
            vec!["svelte/button-has-type", "svelte/require-each-key"]
        );
        let result = check(
            "file:///Template.svelte",
            source,
            json!({"svelte/compiler":"off"}),
        );
        assert_eq!(codes(&result), Vec::<&str>::new());
    }

    #[test]
    fn native_template_fixes_use_original_utf16_ranges() {
        let source = "<p title='🙂'>Hello</p>";
        let result = check(
            "file:///Fix.svelte",
            source,
            json!({"svelte/compiler":"off","svelte/html-quotes":["error",{"prefer":"double"}]}),
        );
        assert_eq!(result["ok"], true, "{result}");
        assert_eq!(codes(&result), vec!["svelte/html-quotes"]);
        let fix = &result["result"]["diagnostics"][0]["fix"];
        assert_eq!(fix["uri"], "file:///Fix.svelte");
        assert_eq!(fix["version"], 1);
        let document = Document::new(Snapshot {
            uri: "file:///Fix.svelte".into(),
            version: 1,
            text: source.into(),
        });
        let mut fixed = source.to_owned();
        for edit in fix["edits"].as_array().unwrap().iter().rev() {
            let bytes = ts_svelte::ByteRange {
                start: edit["bytes"]["start"].as_u64().unwrap() as usize,
                end: edit["bytes"]["end"].as_u64().unwrap() as usize,
            };
            assert_eq!(
                serde_json::to_value(document.editor_range(bytes).unwrap()).unwrap(),
                edit["range"]
            );
            fixed.replace_range(bytes.start..bytes.end, edit["newText"].as_str().unwrap());
        }
        assert_eq!(fixed, "<p title=\"🙂\">Hello</p>");
    }

    #[test]
    fn native_legacy_script_suggestions_and_forced_mode_contract() {
        let profile = ts_svelte::compatibility::resolve(&Default::default()).unwrap();
        let source = "<script>let value; $: value = 1;</script><p>{value}</p>";
        let snapshot = Snapshot {
            uri: "file:///Legacy.svelte".into(),
            version: 2,
            text: source.into(),
        };
        let config =
            json!({"rules":{"svelte/compiler":"off","svelte/no-reactive-literals":"warn"}});
        let result = lint_configured(snapshot.clone(), None, &profile, config.clone());
        assert_eq!(result["ok"], true, "{result}");
        assert_eq!(codes(&result), vec!["svelte/no-reactive-literals"]);
        assert!(
            !result["result"]["diagnostics"][0]["suggestions"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        let result = lint_configured(snapshot, Some(false), &profile, config);
        assert_eq!(result["error"]["code"], "unsupported_lint_context");
    }

    #[test]
    fn require_imports_this_alias_and_svelte_internal_rules() {
        let result = check(
            "file:///state.svelte.ts",
            "import type { X } from 'svelte/internal/client'; const self = this; const { value } = this; require('pkg'); function f(require: any) { require('ok'); } $inspect(1); $inspect.trace();",
            json!({"svelte/no-svelte-internal":"error","svelte/no-inspect":"warn","typescript/no-this-alias":["warn",{"allowDestructuring":true}],"typescript/no-require-imports":"warn"}),
        );
        assert_eq!(result["ok"], true);
        assert_eq!(
            codes(&result),
            vec![
                "svelte/no-svelte-internal",
                "typescript/no-this-alias",
                "typescript/no-require-imports",
                "svelte/no-inspect",
                "svelte/no-inspect"
            ]
        );
    }
}

pub fn lint_configured(
    snapshot: Snapshot,
    runes: Option<bool>,
    profile: &Profile,
    configuration: Value,
) -> Value {
    let config = match Config::parse(&configuration) {
        Ok(config) => config,
        Err(error) => {
            return json!({"ok":false,"error":{"code":"unsupported_lint_configuration","message":error},"supportedRules":native::supported_rules()});
        }
    };
    if snapshot.text.len() > ts_svelte_compiler::MAX_SOURCE_BYTES {
        return super::failure("size_limit", "Source exceeds 1 MiB");
    }
    let filename = if snapshot.uri.contains("://") {
        let parsed = match url::Url::parse(&snapshot.uri) {
            Ok(uri) if uri.query().is_none() && uri.fragment().is_none() => uri,
            _ => {
                return super::failure(
                    "unsupported_uri",
                    "Expected a URI without query or fragment",
                );
            }
        };
        match percent_encoding::percent_decode_str(parsed.path()).decode_utf8() {
            Ok(path) => path.into_owned(),
            Err(_) => return super::failure("unsupported_uri", "URI path is not UTF-8"),
        }
    } else {
        snapshot.uri.clone()
    };
    let document = Document::new(snapshot);
    let mut diagnostics = Vec::new();
    let mut regions = Vec::new();
    let component = filename.ends_with(".svelte");
    if let Err(error) = native::validate_context(&config, &filename, runes, profile) {
        return super::failure("unsupported_lint_context", &error);
    }
    if component {
        let compiled=ts_svelte_compiler::dispatch(json!({"op":"compile","document":document.snapshot(),"target":"server","runes":runes,"compatibility":{"svelteVersion":profile.svelte_version}}).to_string().as_bytes());
        let body = if compiled["ok"] == true {
            &compiled["result"]
        } else {
            &compiled
        };
        for mut diagnostic in body["diagnostics"].as_array().cloned().unwrap_or_default() {
            if diagnostic["severity"] != "error" {
                let Some(setting) = config.rules.get("svelte/compiler") else {
                    continue;
                };
                diagnostic["severity"] = json!(setting.severity);
            }
            diagnostics.push(diagnostic);
        }
        if compiled["ok"] != true {
            return json!({"ok":false,"error":compiled["error"],"diagnostics":diagnostics});
        }
        match rsvelte::Engine::new().prepare(
            document.text(),
            rsvelte::ComponentOptions::new()
                .filename(&document.snapshot().uri)
                .runes(runes),
        ) {
            Ok(prepared) => {
                for script in &prepared.facts().scripts {
                    regions.push((
                        script.content.start() as usize,
                        script.content.end() as usize,
                        script.typescript,
                        matches!(script.kind, rsvelte::ScriptKind::Module),
                    ));
                }
            }
            Err(error) => return super::failure("svelte_projection", &error.diagnostic.message),
        }
    } else {
        regions.push((
            0,
            document.text().len(),
            [".ts", ".tsx", ".mts", ".cts"]
                .iter()
                .any(|ext| filename.ends_with(ext)),
            false,
        ));
    }
    regions.sort_by_key(|(_, _, _, module)| !module);
    let mut module_bindings = BTreeSet::new();
    let mut module_type_bindings = BTreeSet::new();
    let mut incomplete = false;
    for (start, end, typescript, module) in regions {
        let allocator = Allocator::default();
        let source = &document.text()[start..end];
        let source_type = if std::path::Path::new(&filename)
            .extension()
            .is_some_and(|ext| ext == "tsx")
        {
            SourceType::tsx()
        } else if std::path::Path::new(&filename)
            .extension()
            .is_some_and(|ext| ext == "jsx")
        {
            SourceType::jsx()
        } else if typescript {
            SourceType::ts()
        } else {
            SourceType::mjs()
        };
        let parsed = Parser::new(&allocator, source, source_type).parse();
        if parsed.diagnostics.is_empty() {
            let semantic = SemanticBuilder::new().build(&parsed.program);
            if !semantic.diagnostics.is_empty() {
                incomplete = true;
                for error in &semantic.diagnostics {
                    diagnostics.push(json!({"code":"js_semantic_error","message":error.to_string(),"severity":"error","uri":document.snapshot().uri,"range":null,"bytes":null}));
                }
            }
            if module {
                let scope = semantic.semantic.scoping();
                for (name, symbol) in scope.get_bindings(scope.root_scope_id()) {
                    module_type_bindings.insert(name.to_string());
                    if scope.symbol_flags(*symbol).is_value() {
                        module_bindings.insert(name.to_string());
                    }
                }
            }
            let mut visitor = Rules {
                document: &document,
                source,
                offset: start,
                config: &config,
                scoping: semantic.semantic.scoping(),
                module_bindings: &module_bindings,
                module_type_bindings: &module_type_bindings,
                comments: &parsed.program.comments,
                svelte: component
                    || filename.ends_with(".svelte.ts")
                    || filename.ends_with(".svelte.js"),
                diagnostics: Vec::new(),
            };
            visitor.visit_program(&parsed.program);
            diagnostics.extend(visitor.diagnostics);
        } else {
            incomplete = true;
            for error in &parsed.diagnostics {
                diagnostics.push(json!({"code":"js_parse_error","message":error.to_string(),"severity":"error","uri":document.snapshot().uri,"range":null,"bytes":null}));
            }
        }
    }
    if !incomplete {
        match native::run(&document, &filename, &config) {
            Ok(native) => diagnostics.extend(native),
            Err(message) => {
                return json!({"ok":false,"error":{"code":"lint_mapping_error","message":message},"diagnostics":diagnostics});
            }
        }
    }
    diagnostics.sort_by_key(|d| d["bytes"]["start"].as_u64().unwrap_or(u64::MAX));
    let result = json!({"uri":document.snapshot().uri,"version":document.snapshot().version,"diagnostics":diagnostics,"rules":config.rules.values().map(|setting|&setting.code).collect::<Vec<_>>(),"supportedRules":native::supported_rules(),"nativeBackend":{"name":"rsvelte_lint","revision":"5ed8ea3a3401b9fbbe780d3b18d6f90073291d6b","svelteMajor":5,"mode":"inferred from source for mode-dependent rules"},"scope":"Configured script AST and Svelte template/script rules, with original-source fixes and suggestions. Type-aware and filesystem/Kit-context rules require host bridges; JavaScript config evaluation and full ESLint compatibility are not implemented."});
    if incomplete {
        json!({"ok":false,"error":{"code":"lint_parse_incomplete","message":"Script parsing or scope analysis failed"},"diagnostics":result["diagnostics"],"result":result})
    } else {
        json!({"ok":true,"result":result})
    }
}
