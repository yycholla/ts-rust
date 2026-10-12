use serde_json::{Value, json};
use std::collections::BTreeMap;

pub(super) const SUPPORTED: &[&str] = &[
    "svelte/compiler",
    "no-debugger",
    "eqeqeq",
    "no-var",
    "no-console",
    "no-async-promise-executor",
    "no-compare-neg-zero",
    "no-sparse-arrays",
    "no-empty-pattern",
    "no-empty-static-block",
    "no-case-declarations",
    "no-useless-catch",
    "no-array-constructor",
    "valid-typeof",
    "use-isnan",
    "no-unsafe-negation",
    "@typescript-eslint/no-explicit-any",
    "@typescript-eslint/no-this-alias",
    "@typescript-eslint/no-extra-non-null-assertion",
    "@typescript-eslint/no-non-null-asserted-optional-chain",
    "@typescript-eslint/no-unnecessary-type-constraint",
    "@typescript-eslint/no-wrapper-object-types",
    "@typescript-eslint/no-require-imports",
    "svelte/no-inspect",
    "svelte/no-svelte-internal",
];

pub(super) fn canonical(name: &str) -> String {
    name.strip_prefix("typescript/").map_or_else(
        || name.to_owned(),
        |name| format!("@typescript-eslint/{name}"),
    )
}

#[derive(Debug, Clone)]
pub(super) struct Setting {
    pub code: String,
    pub severity: &'static str,
    pub options: Vec<Value>,
}

impl Setting {
    pub fn bool_option(&self, name: &str, fallback: bool) -> bool {
        self.options
            .first()
            .and_then(|value| value.get(name))
            .and_then(Value::as_bool)
            .unwrap_or(fallback)
    }

    pub fn strings(&self, name: &str) -> Vec<&str> {
        self.options
            .first()
            .and_then(|value| value.get(name))
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect()
    }
}

#[derive(Debug, Clone)]
pub(super) struct Config {
    pub rules: BTreeMap<String, Setting>,
}

fn severity(value: &Value) -> Result<Option<&'static str>, String> {
    match value {
        Value::String(value) => match value.as_str() {
            "off" => Ok(None),
            "warn" => Ok(Some("warning")),
            "error" => Ok(Some("error")),
            _ => Err(format!("Unknown severity {value:?}")),
        },
        Value::Number(value) => match value.as_u64() {
            Some(0) => Ok(None),
            Some(1) => Ok(Some("warning")),
            Some(2) => Ok(Some("error")),
            _ => Err(format!("Unknown severity {value}")),
        },
        _ => Err("Severity must be off/warn/error or 0/1/2".into()),
    }
}

fn object_options(options: &[Value], keys: &[&str]) -> Result<(), String> {
    if options.is_empty() {
        return Ok(());
    }
    if options.len() != 1 {
        return Err("Expected at most one options object".into());
    }
    let object = options[0].as_object().ok_or("Expected an options object")?;
    for (key, value) in object {
        if !keys.contains(&key.as_str()) {
            return Err(format!("Unsupported option {key:?}"));
        }
        if matches!(key.as_str(), "allow" | "allowedNames") {
            let values = value
                .as_array()
                .ok_or_else(|| format!("{key} must be a string array"))?;
            if values.iter().any(|value| !value.is_string()) {
                return Err(format!("{key} must be a string array"));
            }
        } else if !value.is_boolean() {
            return Err(format!("{key} must be boolean"));
        }
    }
    Ok(())
}

fn validate_options(rule: &str, options: &[Value]) -> Result<(), String> {
    match rule {
        "eqeqeq" => {
            if options.is_empty() {
                return Ok(());
            }
            if options.len() > 2
                || !matches!(options[0].as_str(), Some("always" | "smart" | "allow-null"))
            {
                return Err("eqeqeq accepts always, smart or allow-null".into());
            }
            if options.len() == 2 {
                if options[0] != "always" {
                    return Err("Null comparison options require eqeqeq=always".into());
                }
                let options = options[1]
                    .as_object()
                    .ok_or("Expected eqeqeq options object")?;
                if options.len() != 1
                    || !matches!(
                        options.get("null").and_then(Value::as_str),
                        Some("always" | "never" | "ignore")
                    )
                {
                    return Err("eqeqeq null must be always, never or ignore".into());
                }
            }
            Ok(())
        }
        "no-console" => object_options(options, &["allow"]),
        "valid-typeof" => object_options(options, &["requireStringLiterals"]),
        "use-isnan" => object_options(options, &["enforceForSwitchCase", "enforceForIndexOf"]),
        "no-unsafe-negation" => object_options(options, &["enforceForOrderingRelations"]),
        "@typescript-eslint/no-explicit-any" => {
            object_options(options, &["fixToUnknown", "ignoreRestArgs"])?;
            if options
                .first()
                .and_then(|v| v.get("ignoreRestArgs"))
                .and_then(Value::as_bool)
                == Some(true)
            {
                return Err("ignoreRestArgs=true is not implemented; implicit skipping would lose diagnostics".into());
            }
            Ok(())
        }
        "@typescript-eslint/no-this-alias" => {
            object_options(options, &["allowDestructuring", "allowedNames"])
        }
        "@typescript-eslint/no-require-imports" => {
            object_options(options, &["allow", "allowAsImport"])?;
            if options
                .first()
                .and_then(|v| v.get("allow"))
                .and_then(Value::as_array)
                .is_some_and(|a| !a.is_empty())
            {
                return Err("no-require-imports allow patterns require regex matching, which is not implemented".into());
            }
            Ok(())
        }
        _ if options.is_empty() => Ok(()),
        _ => Err(format!("Rule {rule} does not accept options")),
    }
}

impl Config {
    pub fn parse(value: &Value) -> Result<Self, String> {
        if value.is_null() || value.as_object().is_some_and(serde_json::Map::is_empty) {
            return Self::parse(
                &json!({"rules":{"svelte/compiler":"warn","no-debugger":"warn","eqeqeq":"warn"}}),
            );
        }
        let object = value
            .as_object()
            .ok_or("Resolved lint configuration must be an object")?;
        if object.keys().any(|key| key != "rules") {
            return Err("Only resolved rules are supported; resolve file overrides, presets, plugins and ignores before calling lint".into());
        }
        let rules = object
            .get("rules")
            .and_then(Value::as_object)
            .ok_or("Resolved configuration requires a rules object")?;
        let mut config = Self {
            rules: BTreeMap::new(),
        };
        // Compiler warnings remain enabled unless explicitly disabled. Parsing
        // errors remain errors regardless of this warning severity setting.
        config.rules.insert(
            "svelte/compiler".into(),
            Setting {
                code: "svelte/compiler".into(),
                severity: "warning",
                options: Vec::new(),
            },
        );
        let mut configured = std::collections::BTreeSet::new();
        for (name, value) in rules {
            let rule = canonical(name);
            if !configured.insert(rule.clone()) {
                return Err(format!(
                    "Rule {rule} is configured through multiple aliases"
                ));
            }
            let (level, options) = if let Some(array) = value.as_array() {
                let first = array
                    .first()
                    .ok_or_else(|| format!("Rule {name} has an empty setting"))?;
                (first, array[1..].to_vec())
            } else {
                (value, Vec::new())
            };
            let Some(severity) = severity(level).map_err(|error| format!("{name}: {error}"))?
            else {
                config.rules.remove(&rule);
                continue;
            };
            if SUPPORTED.contains(&rule.as_str()) {
                validate_options(&rule, &options).map_err(|error| format!("{name}: {error}"))?;
            } else {
                super::native::validate_rule(&rule, &options)
                    .map_err(|error| format!("{name}: {error}"))?;
            }
            config.rules.insert(
                rule,
                Setting {
                    code: name.clone(),
                    severity,
                    options,
                },
            );
        }
        Ok(config)
    }
}
