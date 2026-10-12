//! Adapter for the pinned rsvelte lint registry. Only explicitly configured
//! rules run; neither dependency defaults nor its forgiving JSON API are used.
use super::config::{Config, SUPPORTED};
use rsvelte_lint::{LintConfig, RuleMeta, Severity};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::OnceLock;
use ts_svelte::{ByteRange, Document, compatibility::Profile};

fn registry() -> &'static BTreeMap<&'static str, &'static RuleMeta> {
    static REGISTRY: OnceLock<BTreeMap<&str, &RuleMeta>> = OnceLock::new();
    REGISTRY.get_or_init(|| {
        let mut rules = BTreeMap::new();
        for rule in rsvelte_lint::registry::all_rules() {
            rules.insert(rule.meta().name, rule.meta());
        }
        for rule in rsvelte_lint::registry::all_script_rules() {
            rules.insert(rule.meta().name, rule.meta());
        }
        rules
    })
}

fn unsupported(meta: &RuleMeta) -> Option<&'static str> {
    if meta.type_aware {
        Some("requires a checker type-information bridge")
    } else if rsvelte_lint::sveltekit::is_sveltekit_only(meta.name) {
        Some("requires an explicit SvelteKit project-context bridge")
    } else if matches!(
        meta.name,
        "svelte/no-companion-module-shadow" | "svelte/no-conflicting-module-names"
    ) {
        Some("requires a host filesystem snapshot")
    } else if !rsvelte_lint::svelte_version::supports_svelte5(meta.name) {
        Some("does not support the selected Svelte 5 profiles")
    } else {
        None
    }
}

pub(super) fn supported_rules() -> Vec<&'static str> {
    let mut names = SUPPORTED.to_vec();
    names.extend(
        registry()
            .values()
            .filter(|meta| unsupported(meta).is_none())
            .map(|meta| meta.name),
    );
    names.sort_unstable();
    names.dedup();
    names
}

pub(super) fn validate_rule(name: &str, options: &[Value]) -> Result<(), String> {
    let meta = registry()
        .get(name)
        .ok_or_else(|| format!("Unsupported configured lint rule {name}"))?;
    if let Some(reason) = unsupported(meta) {
        return Err(format!("{name} {reason}"));
    }
    // The pinned registry omits these constraints from the official rule's
    // variadic schema. Keep ESLint's contract, not the weaker metadata copy.
    if name == "svelte/no-restricted-html-elements" {
        if options.is_empty()
            || options
                .iter()
                .enumerate()
                .any(|(index, value)| options[..index].contains(value))
        {
            return Err("Expected at least one unique restricted-element option".into());
        }
        for option in options {
            if let Some(elements) = option.get("elements").and_then(Value::as_array)
                && (elements.is_empty()
                    || elements
                        .iter()
                        .enumerate()
                        .any(|(index, value)| elements[..index].contains(value)))
            {
                return Err("elements must contain at least one unique element".into());
            }
            if option.get("message").and_then(Value::as_str) == Some("") {
                return Err("message must not be empty".into());
            }
        }
    }
    if options.is_empty() {
        return Ok(());
    }
    let Some(schema) = meta.options_schema else {
        return Err("This rule does not accept options".into());
    };
    // Regex-bearing and selector mini-languages need semantic validation in
    // addition to the registry's structural schema. Do not silently accept
    // patterns that the backend compiles with a fallback or simply ignores.
    if matches!(
        name,
        "svelte/sort-attributes" | "svelte/no-unused-class-name"
    ) {
        return Err("Custom options for this rule require pattern validation; default options are supported".into());
    }
    if name == "svelte/no-unknown-style-directive-property"
        && options
            .first()
            .and_then(|option| option.get("ignoreProperties"))
            .and_then(Value::as_array)
            .is_some_and(|patterns| {
                patterns.iter().any(|pattern| {
                    pattern
                        .as_str()
                        .is_some_and(|pattern| pattern.starts_with('/'))
                })
            })
    {
        return Err("ignoreProperties regular expressions require a JavaScript pattern-validation bridge; literal property names are supported".into());
    }
    let schema: Value =
        serde_json::from_str(schema).map_err(|e| format!("Invalid pinned rule schema: {e}"))?;
    if let Some(tuple) = schema.as_array() {
        if options.len() > tuple.len() {
            return Err("Too many rule options".into());
        }
        for (index, (value, schema)) in options.iter().zip(tuple).enumerate() {
            validate_schema(schema, value).map_err(|e| format!("Option {index}: {e}"))?;
        }
        Ok(())
    } else if schema["type"] == "array" {
        validate_schema(&schema, &json!(options))
    } else if options.len() == 1 {
        validate_schema(&schema, &options[0])
    } else {
        Err("Expected at most one options value".into())
    }
}

/// Deliberately bounded JSON-schema interpreter. Unknown assertion keywords
/// fail closed, so an upstream schema extension cannot disable validation.
fn validate_schema(schema: &Value, value: &Value) -> Result<(), String> {
    if let Some(allowed) = schema.as_bool() {
        return if allowed {
            Ok(())
        } else {
            Err("Value is not allowed".into())
        };
    }
    let object = schema
        .as_object()
        .ok_or("Unsupported rule options schema")?;
    const KEYS: &[&str] = &[
        "type",
        "enum",
        "const",
        "properties",
        "additionalProperties",
        "required",
        "items",
        "additionalItems",
        "minItems",
        "maxItems",
        "uniqueItems",
        "minimum",
        "maximum",
        "minLength",
        "maxLength",
        "anyOf",
        "oneOf",
        "allOf",
        "not",
        "description",
        "default",
        "title",
    ];
    if let Some(key) = object.keys().find(|key| !KEYS.contains(&key.as_str())) {
        return Err(format!("Unsupported options schema keyword {key}"));
    }
    if let Some(types) = object.get("type") {
        let matches = |kind: &str| match kind {
            "object" => value.is_object(),
            "array" => value.is_array(),
            "string" => value.is_string(),
            "boolean" => value.is_boolean(),
            "null" => value.is_null(),
            "number" => value.is_number(),
            "integer" => value.as_f64().is_some_and(|number| number.fract() == 0.0),
            _ => false,
        };
        if !types.as_str().is_some_and(matches)
            && !types
                .as_array()
                .is_some_and(|types| types.iter().any(|kind| kind.as_str().is_some_and(matches)))
        {
            return Err(format!("Expected type {types}"));
        }
    }
    if object
        .get("enum")
        .and_then(Value::as_array)
        .is_some_and(|allowed| !allowed.contains(value))
    {
        return Err("Value is outside the allowed enum".into());
    }
    if object
        .get("const")
        .is_some_and(|expected| expected != value)
    {
        return Err("Value differs from the required constant".into());
    }
    for key in ["anyOf", "oneOf", "allOf"] {
        if let Some(branches) = object.get(key) {
            let branches = branches.as_array().ok_or("Invalid alternative schema")?;
            let count = branches
                .iter()
                .filter(|schema| validate_schema(schema, value).is_ok())
                .count();
            if match key {
                "anyOf" => count == 0,
                "oneOf" => count != 1,
                _ => count != branches.len(),
            } {
                return Err(format!("Value does not satisfy {key}"));
            }
        }
    }
    if object
        .get("not")
        .is_some_and(|schema| validate_schema(schema, value).is_ok())
    {
        return Err("Value matches a forbidden schema".into());
    }
    if let Some(values) = value.as_object()
        && (object.get("type") == Some(&json!("object"))
            || object.contains_key("properties")
            || object.contains_key("additionalProperties"))
    {
        if let Some(required) = object.get("required").and_then(Value::as_array) {
            for name in required {
                if !values.contains_key(name.as_str().ok_or("Invalid required property schema")?) {
                    return Err(format!("Missing required property {name}"));
                }
            }
        }
        for (name, value) in values {
            if let Some(schema) = object.get("properties").and_then(|p| p.get(name)) {
                validate_schema(schema, value).map_err(|error| format!("{name}: {error}"))?;
            } else if let Some(schema) = object.get("additionalProperties") {
                validate_schema(schema, value).map_err(|error| format!("{name}: {error}"))?;
            } else {
                // Rule option objects without an explicit open property contract
                // must not accept misspellings that the implementation ignores.
                return Err(format!("Unknown property {name}"));
            }
        }
    }
    if let Some(values) = value.as_array() {
        for (key, too_short) in [("minItems", true), ("maxItems", false)] {
            if object
                .get(key)
                .and_then(Value::as_u64)
                .is_some_and(|bound| {
                    if too_short {
                        (values.len() as u64) < bound
                    } else {
                        (values.len() as u64) > bound
                    }
                })
            {
                return Err(format!("Array violates {key}"));
            }
        }
        if object.get("uniqueItems") == Some(&Value::Bool(true))
            && values
                .iter()
                .enumerate()
                .any(|(i, value)| values[..i].contains(value))
        {
            return Err("Array items must be unique".into());
        }
        if let Some(schema) = object.get("items") {
            for (index, value) in values.iter().enumerate() {
                if let Some(tuple) = schema.as_array() {
                    if let Some(schema) = tuple.get(index).or_else(|| object.get("additionalItems"))
                    {
                        validate_schema(schema, value)?;
                    }
                } else {
                    validate_schema(schema, value)?;
                }
            }
        }
    }
    if let Some(number) = value.as_f64() {
        if object
            .get("minimum")
            .and_then(Value::as_f64)
            .is_some_and(|min| number < min)
            || object
                .get("maximum")
                .and_then(Value::as_f64)
                .is_some_and(|max| number > max)
        {
            return Err("Number is outside the allowed range".into());
        }
    }
    if let Some(text) = value.as_str() {
        let length = text.chars().count() as u64;
        if object
            .get("minLength")
            .and_then(Value::as_u64)
            .is_some_and(|min| length < min)
            || object
                .get("maxLength")
                .and_then(Value::as_u64)
                .is_some_and(|max| length > max)
        {
            return Err("String length is outside the allowed range".into());
        }
    }
    Ok(())
}

pub(super) fn validate_context(
    config: &Config,
    filename: &str,
    runes: Option<bool>,
    profile: &Profile,
) -> Result<(), String> {
    for name in config
        .rules
        .keys()
        .filter(|name| !SUPPORTED.contains(&name.as_str()))
    {
        let meta = registry()[name.as_str()];
        if !profile.svelte_version.starts_with("5.") {
            return Err(format!("{name} requires the Svelte 5 lint engine"));
        }
        if !filename.ends_with(".svelte")
            && !filename.ends_with(".svelte.js")
            && !filename.ends_with(".svelte.ts")
        {
            return Err(format!("{name} requires a Svelte component or rune module"));
        }
        if filename.ends_with(".svelte")
            && runes.is_some()
            && (meta.conditions.legacy_only || meta.conditions.runes_only)
        {
            return Err(format!(
                "{name} requires inferred runes mode; the pinned lint engine cannot accept a forced compiler mode"
            ));
        }
    }
    Ok(())
}

pub(super) fn run(
    document: &Document,
    filename: &str,
    config: &Config,
) -> Result<Vec<Value>, String> {
    let component = filename.ends_with(".svelte");
    let mut selected = LintConfig::empty();
    let mut active = false;
    for (name, setting) in &config.rules {
        if SUPPORTED.contains(&name.as_str()) {
            continue;
        }
        active = true;
        selected = selected.with_override(
            name,
            if setting.severity == "error" {
                Severity::Error
            } else {
                Severity::Warn
            },
        );
        if !setting.options.is_empty() {
            selected = selected.with_options(name, json!(setting.options));
        }
    }
    // The existing AST implementation owns script occurrences; the native
    // template pass extends no-inspect to template expressions without repeats.
    let mut findings = if active && component {
        rsvelte_lint::engine::run_script_rules(document.text(), filename, &selected)
    } else if active {
        rsvelte_lint::engine::run_script_rules_module(
            document.text(),
            filename,
            filename.ends_with(".ts"),
            &selected,
            None,
        )
    } else {
        Vec::new()
    };
    if component {
        if let Some(setting) = config.rules.get("svelte/no-inspect") {
            active = true;
            selected = selected.with_override(
                "svelte/no-inspect",
                if setting.severity == "error" {
                    Severity::Error
                } else {
                    Severity::Warn
                },
            );
        }
        if active {
            findings.extend(rsvelte_lint::engine::run_native_rules(
                document.text(),
                filename,
                &selected,
                None,
            ));
        }
    }
    let suppressions = if component {
        rsvelte_lint::suppression::Suppressions::collect(document.text())
    } else {
        rsvelte_lint::suppression::Suppressions::collect_module(document.text())
    };
    let lines = rsvelte_lint::line_index::LineIndex::new(document.text());
    let mut diagnostics = Vec::new();
    for finding in findings {
        if suppressions.is_suppressed(&finding.rule, finding.report_span(&lines).0.0) {
            continue;
        }
        let bytes = ByteRange {
            start: finding.start as usize,
            end: finding.end as usize,
        };
        let range = document.editor_range(bytes).map_err(|error| {
            format!(
                "{} returned invalid diagnostic bytes: {error}",
                finding.rule
            )
        })?;
        let fix = finding
            .fix
            .as_ref()
            .map(|fix| map_fix(document, fix))
            .transpose()?;
        let suggestions = finding
            .suggestions
            .iter()
            .map(|suggestion| {
                Ok(json!({"description":suggestion.desc,"fix":map_fix(document, &suggestion.fix)?}))
            })
            .collect::<Result<Vec<Value>, String>>()?;
        let setting = config
            .rules
            .get(&finding.rule)
            .ok_or_else(|| format!("Backend returned unconfigured rule {}", finding.rule))?;
        diagnostics.push(json!({"code":setting.code,"message":finding.message,"severity":setting.severity,"uri":document.snapshot().uri,"version":document.snapshot().version,"bytes":bytes,"range":range,"omitEnd":finding.omit_end,"help":finding.help,"fix":fix,"suggestions":suggestions}));
    }
    // Dual registered rules should normally split their work, but identical
    // reports must never create duplicate fixes in editor actions.
    diagnostics.sort_by_key(|d| {
        (
            d["bytes"]["start"].as_u64(),
            d["bytes"]["end"].as_u64(),
            d["code"].as_str().unwrap_or("").to_owned(),
        )
    });
    diagnostics.dedup();
    Ok(diagnostics)
}

fn map_fix(document: &Document, fix: &rsvelte_lint::Fix) -> Result<Value, String> {
    let mut edits = fix.edits.iter().collect::<Vec<_>>();
    edits.sort_by_key(|edit| (edit.start, edit.end));
    for pair in edits.windows(2) {
        if pair[0].end > pair[1].start || pair[0].start == pair[1].start {
            return Err(
                "Native lint fix contains conflicting edits; the complete fix was rejected".into(),
            );
        }
    }
    let edits = edits
        .into_iter()
        .map(|edit| {
            let bytes = ByteRange {
                start: edit.start as usize,
                end: edit.end as usize,
            };
            let range = document
                .editor_range(bytes)
                .map_err(|error| format!("Native lint fix has invalid source bytes: {error}"))?;
            Ok(json!({"range":range,"bytes":bytes,"newText":edit.new_text}))
        })
        .collect::<Result<Vec<Value>, String>>()?;
    Ok(
        json!({"message":fix.message,"uri":document.snapshot().uri,"version":document.snapshot().version,"edits":edits}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn catalog_excludes_unbridged_rules_and_validates_options() {
        assert!(supported_rules().contains(&"svelte/require-each-key"));
        for name in [
            "svelte/no-navigation-without-resolve",
            "svelte/no-companion-module-shadow",
            "svelte/no-conflicting-module-names",
            "svelte/valid-prop-names-in-kit-pages",
        ] {
            assert!(!supported_rules().contains(&name));
            assert!(validate_rule(name, &[]).is_err());
        }
        assert!(validate_rule("svelte/button-has-type", &[json!({"button":false})]).is_ok());
        assert!(validate_rule("svelte/button-has-type", &[json!({"button":"false"})]).is_err());
        assert!(validate_rule("svelte/button-has-type", &[json!({"unknown":true})]).is_err());
        assert!(validate_rule("svelte/spaced-html-comment", &[json!("never")]).is_ok());
        assert!(validate_rule("svelte/spaced-html-comment", &[json!("sometimes")]).is_err());
        assert!(
            validate_rule(
                "svelte/no-restricted-html-elements",
                &[json!("div"), json!({"elements":["span"],"message":"Use p"})]
            )
            .is_ok()
        );
        assert!(validate_rule("svelte/no-restricted-html-elements", &[]).is_err());
        assert!(
            validate_rule(
                "svelte/no-restricted-html-elements",
                &[json!("div"), json!("div")]
            )
            .is_err()
        );
        assert!(
            validate_rule(
                "svelte/no-restricted-html-elements",
                &[json!({"elements":[]})]
            )
            .is_err()
        );
        assert!(
            validate_rule(
                "svelte/no-restricted-html-elements",
                &[json!({"elements":["div"],"message":""})]
            )
            .is_err()
        );
        assert!(validate_rule("svelte/html-quotes", &[json!({"prefer":"double"})]).is_ok());
        assert!(validate_rule("svelte/html-quotes", &[json!("double")]).is_err());
    }
    #[test]
    fn invalid_fixes_fail_atomically() {
        let document = Document::new(ts_svelte::Snapshot {
            uri: "file:///A.svelte".into(),
            version: 1,
            text: "<p>🙂</p>".into(),
        });
        let fix = rsvelte_lint::Fix {
            message: "bad".into(),
            edits: vec![rsvelte_lint::TextEdit {
                start: 4,
                end: 5,
                new_text: String::new(),
            }],
        };
        assert!(map_fix(&document, &fix).is_err());
        let fix = rsvelte_lint::Fix {
            message: "bad".into(),
            edits: vec![
                rsvelte_lint::TextEdit {
                    start: 0,
                    end: 3,
                    new_text: String::new(),
                },
                rsvelte_lint::TextEdit {
                    start: 2,
                    end: 3,
                    new_text: String::new(),
                },
            ],
        };
        assert!(map_fix(&document, &fix).is_err());
    }
}
