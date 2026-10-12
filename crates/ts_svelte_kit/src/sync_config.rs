//! Static Kit configuration needed by native type synchronization.
//! JavaScript expressions are parsed, never executed. Dynamic fields that affect
//! generation must be supplied through the explicit JSON configuration contract.
use oxc_allocator::Allocator;
use oxc_ast::ast::{
    BindingPattern, Declaration, Expression, ImportDeclarationSpecifier, ObjectPropertyKind,
    PropertyKind, Statement, VariableDeclarationKind,
};
use oxc_parser::Parser;
use oxc_span::{GetSpan, SourceType};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct SyncConfig {
    pub out_dir: String,
    pub routes: String,
    pub lib: String,
    pub src: String,
    pub assets: String,
    pub params: String,
    pub service_worker: String,
    pub extensions: Vec<String>,
    pub module_extensions: Vec<String>,
    pub alias: BTreeMap<String, String>,
    pub env_dir: String,
    pub public_prefix: String,
    pub private_prefix: String,
    pub explicit_environment_variables: bool,
    /// Host-resolved declaration metadata for an env entry whose factory calls
    /// or imports cannot be reduced statically. Contains no environment values.
    pub resolved_env: Option<ResolvedEnv>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResolvedEnv {
    /// Project-relative path to the actual configured src/env entry.
    pub entry: String,
    pub variables: Vec<ResolvedEnvVariable>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResolvedEnvVariable {
    pub name: String,
    #[serde(default)]
    pub public: bool,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub has_schema: bool,
}

pub struct ExplicitEnvInput {
    pub entry: Option<PathBuf>,
    pub variables: Vec<ResolvedEnvVariable>,
    pub warnings: Vec<String>,
    pub host_resolved: bool,
}
impl Default for SyncConfig {
    fn default() -> Self {
        Self {
            out_dir: ".svelte-kit".into(),
            routes: "src/routes".into(),
            lib: "src/lib".into(),
            src: "src".into(),
            assets: "static".into(),
            params: "src/params".into(),
            service_worker: "src/service-worker".into(),
            extensions: vec![".svelte".into()],
            module_extensions: vec![".js".into(), ".ts".into()],
            alias: BTreeMap::new(),
            env_dir: ".".into(),
            public_prefix: "PUBLIC_".into(),
            private_prefix: String::new(),
            explicit_environment_variables: false,
            resolved_env: None,
        }
    }
}

/// Read an explicit resolved config, or statically parse svelte.config.js.
pub fn load(root: &Path, resolved: Option<&Path>) -> Result<SyncConfig, String> {
    let config = if let Some(file) = resolved {
        serde_json::from_str(&std::fs::read_to_string(file).map_err(|e| e.to_string())?)
            .map_err(|e| format!("Invalid resolved Kit sync config: {e}"))?
    } else {
        let files = ["svelte.config.js", "svelte.config.ts", "svelte.config.mjs"];
        let file = files.iter().map(|p| root.join(p)).find(|p| p.is_file());
        if let Some(file) = file {
            from_source_for_version(
                &std::fs::read_to_string(file).map_err(|e| e.to_string())?,
                installed_kit_version(root)?.starts_with("3."),
            )?
        } else {
            SyncConfig::default()
        }
    };
    validate(&config)?;
    Ok(config)
}

fn validate(config: &SyncConfig) -> Result<(), String> {
    for p in [
        &config.out_dir,
        &config.routes,
        &config.lib,
        &config.src,
        &config.assets,
        &config.params,
        &config.service_worker,
        &config.env_dir,
    ] {
        let path = Path::new(p);
        if p.is_empty()
            || path.is_absolute()
            || path
                .components()
                .any(|c| matches!(c, Component::ParentDir | Component::Prefix(_)))
        {
            return Err(format!(
                "Native sync paths must stay inside the project: {p}"
            ));
        }
    }
    if Path::new(&config.out_dir)
        .components()
        .all(|c| c == Component::CurDir)
    {
        return Err("Kit output directory must not be the project root".into());
    }
    if config.extensions.is_empty()
        || config.module_extensions.is_empty()
        || config
            .extensions
            .iter()
            .chain(&config.module_extensions)
            .any(|s| !s.starts_with('.') || s.contains('/') || s.contains('\\'))
    {
        return Err("Kit extensions must be nonempty lists of dot-prefixed file extensions".into());
    }
    if let Some(env) = &config.resolved_env {
        let entry = Path::new(&env.entry);
        if env.entry.is_empty()
            || entry.is_absolute()
            || entry
                .components()
                .any(|part| !matches!(part, Component::Normal(_) | Component::CurDir))
        {
            return Err("resolvedEnv.entry must be a project-relative source path".into());
        }
        validate_env_variables(&env.variables)?;
    }
    Ok(())
}

fn dynamic(expr: &Expression<'_>) -> Value {
    let s = expr.span();
    json!({"$nativeDynamic": [s.start, s.end]})
}
/// Local import binding -> (module specifier, imported name or "default").
type ImportBindings = BTreeMap<String, (String, String)>;
fn evaluate<'a>(
    expr: &'a Expression<'a>,
    bindings: &BTreeMap<String, &'a Expression<'a>>,
    depth: usize,
) -> Result<Value, String> {
    evaluate_with(expr, bindings, None, depth)
}
fn evaluate_with<'a>(
    expr: &'a Expression<'a>,
    bindings: &BTreeMap<String, &'a Expression<'a>>,
    calls: Option<&ImportBindings>,
    depth: usize,
) -> Result<Value, String> {
    if depth > 64 {
        return Err("Cyclic or excessively nested configuration expression".into());
    }
    Ok(match expr {
        Expression::StringLiteral(s) => json!(s.value.as_str()),
        Expression::BooleanLiteral(b) => json!(b.value),
        Expression::NumericLiteral(n) => json!(n.value),
        Expression::NullLiteral(_) => Value::Null,
        Expression::Identifier(id) => match bindings.get(id.name.as_str()) {
            Some(v) => evaluate_with(v, bindings, calls, depth + 1)?,
            None => dynamic(expr),
        },
        Expression::ParenthesizedExpression(e) => {
            evaluate_with(&e.expression, bindings, calls, depth + 1)?
        }
        Expression::TSAsExpression(e) => evaluate_with(&e.expression, bindings, calls, depth + 1)?,
        Expression::TSSatisfiesExpression(e) => {
            evaluate_with(&e.expression, bindings, calls, depth + 1)?
        }
        Expression::ObjectExpression(obj) => {
            let mut out = Map::new();
            // Tree mode records JS property order, which serde_json's sorted
            // Map loses. Array-index keys are not reordered: config keys here
            // are names, never indices.
            let mut keys: Vec<String> = Vec::new();
            let record = |keys: &mut Vec<String>, key: &str| {
                if !keys.iter().any(|k| k == key) {
                    keys.push(key.into());
                }
            };
            for prop in &obj.properties {
                match prop {
                    ObjectPropertyKind::ObjectProperty(p) => {
                        let key = p
                            .key
                            .static_name()
                            .ok_or("Dynamic configuration property key; use a resolved config")?;
                        let value = if p.method || p.kind != PropertyKind::Init {
                            dynamic(&p.value)
                        } else {
                            evaluate_with(&p.value, bindings, calls, depth + 1)?
                        };
                        record(&mut keys, &key);
                        out.insert(key.into_owned(), value);
                    }
                    ObjectPropertyKind::SpreadProperty(p) => {
                        let v = evaluate_with(&p.argument, bindings, calls, depth + 1)?;
                        let map = match v.as_object().filter(|m| !m.contains_key("$nativeDynamic"))
                        {
                            Some(map) => map,
                            // Tree mode: the object's other properties stay
                            // readable; consumers see that keys may be missing.
                            None if calls.is_some() => {
                                out.insert("$nativeDynamicSpread".into(), json!(true));
                                continue;
                            }
                            None => {
                                return Err(
                                    "Dynamic configuration spread; use a resolved config".into()
                                );
                            }
                        };
                        let spread_keys: Vec<String> = match map.get("$keys") {
                            Some(Value::Array(order)) => order
                                .iter()
                                .filter_map(|k| k.as_str().map(str::to_owned))
                                .collect(),
                            _ => map.keys().cloned().collect(),
                        };
                        for key in spread_keys {
                            record(&mut keys, &key);
                        }
                        out.extend(map.clone().into_iter().filter(|(k, _)| k != "$keys"));
                    }
                }
            }
            if calls.is_some() {
                out.insert("$keys".into(), json!(keys));
            }
            Value::Object(out)
        }
        Expression::ArrayExpression(array) => {
            let mut out = Vec::new();
            for entry in &array.elements {
                let expr = entry
                    .as_expression()
                    .ok_or("Configuration array spreads/holes require a resolved config")?;
                out.push(evaluate_with(expr, bindings, calls, depth + 1)?);
            }
            Value::Array(out)
        }
        Expression::CallExpression(call) if calls.is_some() => {
            let imported = match &call.callee {
                Expression::Identifier(id) if !bindings.contains_key(id.name.as_str()) => {
                    calls.and_then(|calls| calls.get(id.name.as_str()))
                }
                _ => None,
            };
            match imported {
                Some((module, export)) if call.arguments.iter().all(|a| !a.is_spread()) => {
                    let mut arguments = Vec::new();
                    for argument in &call.arguments {
                        let argument = argument.as_expression().expect("non-spread argument");
                        arguments.push(evaluate_with(argument, bindings, calls, depth + 1)?);
                    }
                    json!({"$call": {"module": module, "export": export, "arguments": arguments}})
                }
                _ => dynamic(expr),
            }
        }
        _ => dynamic(expr),
    })
}

/// The default-exported Svelte config as a static JSON tree for build planning.
/// Calls of imported functions (`adapter()`, `mdsvex({...})`) become
/// `{"$call": {"module", "export", "arguments"}}` with statically evaluated
/// arguments; every other non-literal stays `{"$nativeDynamic": [start, end]}`.
/// Every object carries `"$keys"`: its property names in source order.
/// Nothing is executed and no imported module is read.
pub fn static_config_tree(source: &str) -> Result<Value, String> {
    let allocator = Allocator::default();
    let parsed = Parser::new(&allocator, source, SourceType::ts()).parse();
    if !parsed.diagnostics.is_empty() {
        return Err(format!(
            "Cannot parse Svelte config: {:?}",
            parsed.diagnostics[0]
        ));
    }
    let mut bindings = BTreeMap::new();
    let mut imports = ImportBindings::new();
    let mut exported = None;
    for statement in &parsed.program.body {
        match statement {
            Statement::VariableDeclaration(d) if d.kind == VariableDeclarationKind::Const => {
                for decl in &d.declarations {
                    if let (BindingPattern::BindingIdentifier(id), Some(value)) =
                        (&decl.id, &decl.init)
                    {
                        bindings.insert(id.name.to_string(), value);
                    } else {
                        return Err(
                            "Destructured/uninitialized config binding requires a resolved config"
                                .into(),
                        );
                    }
                }
            }
            Statement::ExportDefaultDeclaration(d) => {
                exported = d.declaration.as_expression();
            }
            Statement::ImportDeclaration(d) => {
                for specifier in d.specifiers.iter().flatten() {
                    let (local, export) = match specifier {
                        ImportDeclarationSpecifier::ImportDefaultSpecifier(s) => {
                            (s.local.name.as_str(), "default".to_owned())
                        }
                        ImportDeclarationSpecifier::ImportSpecifier(s) => {
                            (s.local.name.as_str(), s.imported.name().to_string())
                        }
                        ImportDeclarationSpecifier::ImportNamespaceSpecifier(_) => continue,
                    };
                    imports.insert(local.into(), (d.source.value.to_string(), export));
                }
            }
            Statement::EmptyStatement(_) => {}
            _ => {
                return Err(
                    "Executable configuration statements require an explicit resolved config"
                        .into(),
                );
            }
        }
    }
    evaluate_with(
        exported.ok_or("Expected default-exported config object")?,
        &bindings,
        Some(&imports),
        0,
    )
}

pub fn from_source(source: &str) -> Result<SyncConfig, String> {
    from_source_for_version(source, false)
}

pub fn installed_kit_version(root: &Path) -> Result<String, String> {
    let path = root.join("node_modules/@sveltejs/kit/package.json");
    let package: Value =
        serde_json::from_str(&std::fs::read_to_string(path).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    package["version"]
        .as_str()
        .map(str::to_owned)
        .ok_or("Missing installed Kit version".into())
}

pub fn from_source_for_version(source: &str, kit3: bool) -> Result<SyncConfig, String> {
    let allocator = Allocator::default();
    let parsed = Parser::new(&allocator, source, SourceType::ts()).parse();
    if !parsed.diagnostics.is_empty() {
        return Err(format!(
            "Cannot parse Svelte config: {:?}",
            parsed.diagnostics[0]
        ));
    }
    let mut bindings = BTreeMap::new();
    let mut exported = None;
    for statement in &parsed.program.body {
        match statement {
            Statement::VariableDeclaration(d) if d.kind == VariableDeclarationKind::Const => {
                for decl in &d.declarations {
                    if let (BindingPattern::BindingIdentifier(id), Some(value)) =
                        (&decl.id, &decl.init)
                    {
                        bindings.insert(id.name.to_string(), value);
                    } else {
                        return Err(
                            "Destructured/uninitialized config binding requires a resolved config"
                                .into(),
                        );
                    }
                }
            }
            Statement::ExportDefaultDeclaration(d) => {
                exported = d.declaration.as_expression();
            }
            Statement::ImportDeclaration(_) | Statement::EmptyStatement(_) => {}
            _ => {
                return Err(
                    "Executable configuration statements require an explicit resolved sync config"
                        .into(),
                );
            }
        }
    }
    let value = evaluate(
        exported.ok_or("Expected default-exported config object")?,
        &bindings,
        0,
    )?;
    let top = value
        .as_object()
        .filter(|v| !v.contains_key("$nativeDynamic"))
        .ok_or("Expected static configuration object")?;
    let mut result = SyncConfig::default();
    if let Some(v) = top.get("extensions") {
        result.extensions = serde_json::from_value(v.clone())
            .map_err(|_| "extensions must be a static string array")?;
    }
    if let Some(kit) = if kit3 { Some(&value) } else { top.get("kit") } {
        let kit = kit
            .as_object()
            .filter(|v| !v.contains_key("$nativeDynamic"))
            .ok_or("kit must be a static object")?;
        read_string(kit, "outDir", &mut result.out_dir)?;
        if let Some(v) = kit.get("alias") {
            result.alias = serde_json::from_value(v.clone())
                .map_err(|_| "kit.alias must contain static string paths")?;
        }
        if let Some(v) = kit.get("moduleExtensions") {
            result.module_extensions = serde_json::from_value(v.clone())
                .map_err(|_| "kit.moduleExtensions must be static strings")?;
        }
        if let Some(experimental) = kit.get("experimental") {
            let experimental = experimental
                .as_object()
                .filter(|v| !v.contains_key("$nativeDynamic"))
                .ok_or("experimental must be a static object")?;
            if let Some(value) = experimental.get("explicitEnvironmentVariables") {
                result.explicit_environment_variables = value
                    .as_bool()
                    .ok_or("explicitEnvironmentVariables must be a static boolean")?;
            }
        }
        if let Some(v) = kit
            .get("typescript")
            .and_then(Value::as_object)
            .and_then(|v| v.get("config"))
        {
            if !v.is_null() {
                return Err("kit.typescript.config callback requires a resolved generated tsconfig; native static sync cannot evaluate it".into());
            }
        }
        if let Some(v) = kit.get("files") {
            let files = v
                .as_object()
                .filter(|v| !v.contains_key("$nativeDynamic"))
                .ok_or("kit.files must be a static object")?;
            for (key, target) in [
                ("routes", &mut result.routes),
                ("lib", &mut result.lib),
                ("src", &mut result.src),
                ("assets", &mut result.assets),
                ("params", &mut result.params),
                ("serviceWorker", &mut result.service_worker),
            ] {
                read_string(files, key, target)?;
            }
        }
        if let Some(v) = kit.get("env") {
            let env = v
                .as_object()
                .filter(|v| !v.contains_key("$nativeDynamic"))
                .ok_or("kit.env must be a static object")?;
            read_string(env, "dir", &mut result.env_dir)?;
            read_string(env, "publicPrefix", &mut result.public_prefix)?;
            read_string(env, "privatePrefix", &mut result.private_prefix)?;
        }
    }
    validate(&result)?;
    Ok(result)
}
fn read_string(map: &Map<String, Value>, key: &str, target: &mut String) -> Result<(), String> {
    if let Some(value) = map.get(key) {
        *target = value
            .as_str()
            .ok_or_else(|| format!("{key} must be a static string; provide a resolved config"))?
            .into();
    }
    Ok(())
}

fn validate_env_variables(variables: &[ResolvedEnvVariable]) -> Result<(), String> {
    if variables.len() > 16_384 {
        return Err("Explicit environment declaration count exceeds 16384".into());
    }
    let mut names = std::collections::BTreeSet::new();
    for variable in variables {
        if !crate::sync_ambient::valid_env_identifier(&variable.name)
            || crate::sync_ambient::reserved_env_identifier(&variable.name)
        {
            return Err(format!(
                "Invalid environment variable name {:?}",
                variable.name
            ));
        }
        if !names.insert(&variable.name) {
            return Err(format!(
                "Duplicate environment variable name {:?}",
                variable.name
            ));
        }
    }
    Ok(())
}

/// Resolve the configured environment entry with Kit's directory-index-first
/// behavior. No source, schema, environment value or module is executed.
/// # Errors
/// Rejects entries outside the root, oversized modules and dynamic metadata
/// without an explicit host-resolved contract.
pub fn explicit_env(
    root: &Path,
    config: &SyncConfig,
    kit3: bool,
) -> Result<ExplicitEnvInput, String> {
    let extensions = if kit3 {
        config.module_extensions.clone()
    } else {
        vec![".js".into(), ".ts".into()]
    };
    let entry = resolve_env_entry(&root.join(&config.src).join("env"), &extensions)?;
    let enabled = kit3 || config.explicit_environment_variables;
    let mut warnings = Vec::new();
    if !enabled {
        if entry.is_some() {
            warnings.push(
                "src/env requires experimental.explicitEnvironmentVariables to be set for Kit 2"
                    .into(),
            );
        }
        if config.resolved_env.is_some() {
            return Err(
                "resolvedEnv requires experimental.explicitEnvironmentVariables for Kit 2".into(),
            );
        }
        return Ok(ExplicitEnvInput {
            entry: None,
            variables: Vec::new(),
            warnings,
            host_resolved: false,
        });
    }
    let Some(entry) = entry else {
        if config.resolved_env.is_some() {
            return Err(
                "resolvedEnv was supplied but the configured src/env entry does not exist".into(),
            );
        }
        if !kit3 {
            warnings.push("experimental.explicitEnvironmentVariables was set, but no src/env.ts or src/env.js file could be found".into());
        }
        return Ok(ExplicitEnvInput {
            entry: None,
            variables: Vec::new(),
            warnings,
            host_resolved: false,
        });
    };
    let canonical = entry.canonicalize().map_err(|error| error.to_string())?;
    let canonical_root = root.canonicalize().map_err(|error| error.to_string())?;
    if !canonical.starts_with(&canonical_root) {
        return Err("Environment entry escapes project root through a symlink".into());
    }
    let metadata = std::fs::metadata(&entry).map_err(|error| error.to_string())?;
    if metadata.len() > 1024 * 1024 {
        return Err("Environment source module exceeds 1 MiB".into());
    }
    let variables = if let Some(resolved) = &config.resolved_env {
        let selected = root
            .join(&resolved.entry)
            .canonicalize()
            .map_err(|error| error.to_string())?;
        if selected != canonical {
            return Err("resolvedEnv.entry does not match the configured src/env module".into());
        }
        validate_env_variables(&resolved.variables)?;
        resolved.variables.clone()
    } else {
        let source = std::fs::read_to_string(&entry).map_err(|error| error.to_string())?;
        if source.len() > 1024 * 1024 {
            return Err("Environment source module exceeds 1 MiB".into());
        }
        parse_explicit_env_source(&source, kit3)
            .map_err(|error| format!("{}: {error}", entry.display()))?
    };
    Ok(ExplicitEnvInput {
        entry: Some(entry),
        variables,
        warnings,
        host_resolved: config.resolved_env.is_some(),
    })
}

fn resolve_env_entry(base: &Path, extensions: &[String]) -> Result<Option<PathBuf>, String> {
    if base.is_file() {
        return Ok(Some(base.to_path_buf()));
    }
    if base.is_dir()
        && extensions
            .iter()
            .any(|extension| base.join(format!("index{extension}")).is_file())
    {
        return resolve_env_entry(&base.join("index"), extensions);
    }
    let Some(directory) = base.parent().filter(|directory| directory.is_dir()) else {
        return Ok(None);
    };
    let name = base
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or("Environment entry path is not UTF-8")?;
    let mut files = std::fs::read_dir(directory)
        .map_err(|error| error.to_string())?
        .map(|entry| {
            entry
                .map(|entry| entry.path())
                .map_err(|error| error.to_string())
        })
        .collect::<Result<Vec<_>, _>>()?;
    if files.len() > 100_000 {
        return Err("Environment source directory exceeds 100000 entries".into());
    }
    files.sort();
    Ok(files.into_iter().find(|file| {
        file.is_file()
            && file
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|file| {
                    file == name
                        || extensions
                            .iter()
                            .any(|extension| file == format!("{name}{extension}"))
                })
    }))
}

fn env_value<'a>(
    expression: &'a Expression<'a>,
    bindings: &BTreeMap<String, &'a Expression<'a>>,
    helpers: &std::collections::BTreeSet<String>,
    depth: usize,
) -> Result<Value, String> {
    if depth > 64 {
        return Err("Cyclic or excessively nested environment metadata".into());
    }
    Ok(match expression {
        Expression::Identifier(identifier) => match bindings.get(identifier.name.as_str()) {
            Some(value) => env_value(value, bindings, helpers, depth + 1)?,
            None if identifier.name == "undefined" => Value::Null,
            None => dynamic(expression),
        },
        Expression::ParenthesizedExpression(value) => {
            env_value(&value.expression, bindings, helpers, depth + 1)?
        }
        Expression::TSAsExpression(value) => {
            env_value(&value.expression, bindings, helpers, depth + 1)?
        }
        Expression::TSSatisfiesExpression(value) => {
            env_value(&value.expression, bindings, helpers, depth + 1)?
        }
        Expression::CallExpression(call) if matches!(&call.callee, Expression::Identifier(name) if helpers.contains(name.name.as_str()) && !bindings.contains_key(name.name.as_str())) =>
        {
            if call.arguments.len() != 1 {
                return Err("defineEnvVars requires one statically known object argument".into());
            }
            let argument = call.arguments[0]
                .as_expression()
                .ok_or("defineEnvVars spread arguments require resolvedEnv")?;
            env_value(argument, bindings, helpers, depth + 1)?
        }
        Expression::ObjectExpression(object) => {
            let mut output = Map::new();
            for property in &object.properties {
                match property {
                    ObjectPropertyKind::ObjectProperty(property) => {
                        let name = property
                            .key
                            .static_name()
                            .ok_or("Computed environment keys require resolvedEnv")?;
                        if name == "__proto__" && !property.computed {
                            return Err(
                                "Environment object prototype mutation requires resolvedEnv".into(),
                            );
                        }
                        let value = if property.method || property.kind != PropertyKind::Init {
                            if property.kind == PropertyKind::Init {
                                json!({"$nativeKnownTruthy":true})
                            } else {
                                dynamic(&property.value)
                            }
                        } else {
                            env_value(&property.value, bindings, helpers, depth + 1)?
                        };
                        output.insert(name.into_owned(), value);
                    }
                    ObjectPropertyKind::SpreadProperty(property) => {
                        let value = env_value(&property.argument, bindings, helpers, depth + 1)?;
                        let object = value
                            .as_object()
                            .filter(|object| {
                                !object.contains_key("$nativeDynamic")
                                    && !object.contains_key("$nativeKnownTruthy")
                            })
                            .ok_or("Dynamic environment object spread requires resolvedEnv")?;
                        output.extend(object.clone());
                    }
                }
            }
            Value::Object(output)
        }
        Expression::ArrowFunctionExpression(_) | Expression::FunctionExpression(_) => {
            json!({"$nativeKnownTruthy":true})
        }
        Expression::ArrayExpression(_) => json!({"$nativeKnownTruthy":true}),
        _ => evaluate(expression, bindings, depth + 1)?,
    })
}

fn known_truthiness(value: &Value, field: &str) -> Result<bool, String> {
    Ok(match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(value) => value.as_f64().is_some_and(|value| value != 0.0),
        Value::String(value) => !value.is_empty(),
        Value::Object(object) if object.contains_key("$nativeDynamic") => {
            return Err(format!(
                "Dynamic environment {field} requires resolvedEnv metadata; native sync does not execute factories or schemas"
            ));
        }
        Value::Array(_) | Value::Object(_) => true,
    })
}

/// Extract declaration metadata from a static `variables` export. Known Kit
/// `defineEnvVars` helpers preserve the metadata and infer schema output through
/// the original module. Imported/factory-produced schema truthiness is never
/// guessed: provide `resolvedEnv` for such modules.
/// # Errors
/// Rejects dynamic exports, mutation, unknown factories and invalid names.
pub fn parse_explicit_env_source(
    source: &str,
    kit3: bool,
) -> Result<Vec<ResolvedEnvVariable>, String> {
    let allocator = Allocator::default();
    let parsed = Parser::new(&allocator, source, SourceType::ts()).parse();
    if !parsed.diagnostics.is_empty() {
        return Err(format!(
            "Cannot parse environment source: {:?}",
            parsed.diagnostics[0]
        ));
    }
    let mut bindings = BTreeMap::new();
    let mut helpers = std::collections::BTreeSet::new();
    let mut exported = None;
    for statement in &parsed.program.body {
        let declaration = match statement {
            Statement::ImportDeclaration(import) => {
                let expected = if kit3 { "@sveltejs/kit/env" } else { "@sveltejs/kit/hooks" };
                if import.source.value == expected && !import.import_kind.is_type() {
                    for specifier in import.specifiers.iter().flatten() {
                        if let ImportDeclarationSpecifier::ImportSpecifier(specifier) = specifier {
                            if specifier.imported.name() == "defineEnvVars" && !specifier.import_kind.is_type() { helpers.insert(specifier.local.name.to_string()); }
                        }
                    }
                }
                if import.source.value.starts_with("$app/") && import.source.value != "$app/env" { return Err("Environment entry cannot import $app modules other than $app/env".into()); }
                continue;
            }
            Statement::VariableDeclaration(declaration) => declaration,
            Statement::ExportDeclaration(export) => match &export.declaration {
                Declaration::VariableDeclaration(declaration) => {
                    for declaration in &declaration.declarations {
                        if let BindingPattern::BindingIdentifier(name) = &declaration.id {
                            if name.name == "variables" { exported = Some("variables".to_owned()); }
                        }
                    }
                    declaration
                }
                _ => return Err("Non-variable environment exports require resolvedEnv metadata".into()),
            },
            Statement::ExportNamedDeclaration(export) => {
                for specifier in &export.specifiers {
                    if !export.export_kind.is_type() && !specifier.export_kind.is_type() && specifier.exported.name() == "variables" { exported = Some(specifier.local.name().to_string()); }
                }
                continue;
            }
            Statement::EmptyStatement(_) => continue,
            _ => return Err("Executable statements, re-exports or mutable environment configuration require resolvedEnv metadata".into()),
        };
        if declaration.kind != VariableDeclarationKind::Const {
            return Err("Mutable environment declarations require resolvedEnv metadata".into());
        }
        for declaration in &declaration.declarations {
            let (BindingPattern::BindingIdentifier(name), Some(value)) =
                (&declaration.id, &declaration.init)
            else {
                return Err("Destructured or uninitialized environment declarations require resolvedEnv metadata".into());
            };
            bindings.insert(name.name.to_string(), value);
        }
    }
    let name = exported.ok_or("Environment entry must export a variables object")?;
    let expression = bindings
        .get(&name)
        .ok_or("Environment variables re-export requires resolvedEnv metadata")?;
    let value = env_value(expression, &bindings, &helpers, 0)?;
    let object = value.as_object().filter(|object| !object.contains_key("$nativeDynamic") && !object.contains_key("$nativeKnownTruthy")).ok_or("Environment variables must be a static object or a recognized defineEnvVars call; otherwise supply resolvedEnv metadata")?;
    let mut variables = Vec::new();
    for (name, value) in object {
        let config = value.as_object().filter(|object| !object.contains_key("$nativeDynamic") && !object.contains_key("$nativeKnownTruthy")).ok_or_else(|| format!("Environment variable {name:?} requires a static configuration object or resolvedEnv metadata"))?;
        let public = config
            .get("public")
            .map(|value| known_truthiness(value, "public flag"))
            .transpose()?
            .unwrap_or(false);
        let has_schema = config
            .get("schema")
            .map(|value| known_truthiness(value, "schema"))
            .transpose()?
            .unwrap_or(false);
        let description = match config.get("description") {
            None | Some(Value::Null) => None,
            Some(Value::String(value)) => Some(value.clone()),
            Some(value) if !known_truthiness(value, "description")? => None,
            _ => {
                return Err(format!(
                    "Environment variable {name:?} description must be a static string or resolvedEnv metadata"
                ));
            }
        };
        variables.push(ResolvedEnvVariable {
            name: name.clone(),
            public,
            description,
            has_schema,
        });
    }
    validate_env_variables(&variables)?;
    Ok(variables)
}

/// Lexical relative path; inputs are project-root relative and validated.
pub fn relative(from: &str, to: &str) -> String {
    fn pieces(p: &str) -> Vec<&str> {
        p.split('/')
            .filter(|p| !p.is_empty() && *p != ".")
            .collect()
    }
    let from = pieces(from);
    let to = pieces(to);
    let same = from.iter().zip(&to).take_while(|(a, b)| a == b).count();
    let mut out = vec![".."; from.len() - same];
    out.extend_from_slice(&to[same..]);
    if out.is_empty() {
        ".".into()
    } else {
        out.join("/")
    }
}
fn config_relative(config: &SyncConfig, path: &str) -> String {
    relative(&config.out_dir, path)
}

pub fn tsconfig(root: &Path, config: &SyncConfig) -> Value {
    let mut alias = config.alias.clone();
    if root.join(&config.lib).is_dir() {
        alias.insert("$lib".into(), config.lib.clone());
    }
    let mut paths = Map::new();
    for (key, value) in &alias {
        let value = value.strip_suffix("/*").unwrap_or(value);
        let mut path = config_relative(config, value);
        if !path.starts_with("..") {
            path = format!("./{path}");
        }
        if key.ends_with("/*") {
            paths.insert(key.clone(), json!([format!("{path}/*")]));
        } else {
            paths.insert(key.clone(), json!([path]));
            if Path::new(value).extension().is_none() && !alias.contains_key(&format!("{key}/*")) {
                paths.insert(format!("{key}/*"), json!([format!("{path}/*")]));
            }
        }
    }
    paths.insert("$app/types".into(), json!(["./types/index.d.ts"]));
    let mut include: Vec<String> = [
        "ambient.d.ts",
        "env.d.ts",
        "non-ambient.d.ts",
        "./types/**/$types.d.ts",
    ]
    .iter()
    .map(|s| (*s).into())
    .collect();
    include.extend(["vite.config.js", "vite.config.ts"].map(|s| config_relative(config, s)));
    let mut dirs = Vec::new();
    for dir in [&config.routes, &config.lib, &config.src] {
        let rel = relative(&config.src, dir);
        if (rel == "." || rel.starts_with("..")) && !dirs.contains(dir) {
            dirs.push(dir.clone());
        }
    }
    for dir in dirs.iter().map(String::as_str).chain(["test", "tests"]) {
        for ext in ["js", "ts", "svelte"] {
            let entry = config_relative(config, &format!("{dir}/**/*.{ext}"));
            if !include.contains(&entry) {
                include.push(entry);
            }
        }
    }
    let mut exclude = vec![config_relative(config, "node_modules/**")];
    if Path::new(&config.service_worker).extension().is_some() {
        exclude.push(config_relative(config, &config.service_worker));
    } else {
        for ext in ["js", "ts", "d.ts"] {
            for middle in ["", "/**/*"] {
                exclude.push(config_relative(
                    config,
                    &format!("{}{middle}.{ext}", config.service_worker),
                ));
            }
        }
    }
    json!({"compilerOptions":{"paths": paths,"rootDirs":[config_relative(config,"."),"./types"],"verbatimModuleSyntax":true,"isolatedModules":true,"lib":["esnext","DOM","DOM.Iterable"],"moduleResolution":"bundler","module":"esnext","noEmit":true,"target":"esnext"},"include":include,"exclude":exclude})
}

/// Resolve a writable generation path without following symlinks outside root.
pub fn output_path(root: &Path, config: &SyncConfig) -> Result<PathBuf, String> {
    let path = root.join(&config.out_dir);
    let mut cursor = root.to_path_buf();
    for c in Path::new(&config.out_dir).components() {
        cursor.push(c);
        if cursor.exists()
            && !cursor
                .canonicalize()
                .map_err(|e| e.to_string())?
                .starts_with(root)
        {
            return Err("Kit output directory escapes project through a symlink".into());
        }
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn static_config_resolves_aliases_without_executing_plugins() {
        let cfg=from_source("import adapter from 'adapter'; const aliases = { $x: './src/x' }; const config = { extensions: ['.svelte','.md'], preprocess: plugin(), kit: { adapter: adapter(), alias: { ...aliases }, env: { publicPrefix:'APP_' } } }; export default config;").unwrap();
        assert_eq!(cfg.alias["$x"], "./src/x");
        assert_eq!(cfg.public_prefix, "APP_");
        assert_eq!(cfg.extensions.len(), 2);
    }
    #[test]
    fn rejects_dynamic_generation_and_mutations() {
        assert!(
            from_source("const config={}; config.kit = other; export default config;").is_err()
        );
        assert!(from_source("export default {kit:{alias: getAliases()}};").is_err());
        assert!(from_source("export default {kit:{outDir:'../outside'}};").is_err());
    }
    #[test]
    fn relative_paths_and_tsconfig_contract() {
        assert_eq!(relative(".svelte-kit", "src/routes"), "../src/routes");
        let c = SyncConfig::default();
        let j = tsconfig(Path::new("/nonexistent"), &c);
        assert_eq!(j["compilerOptions"]["rootDirs"], json!(["..", "./types"]));
        assert_eq!(j["exclude"].as_array().unwrap().len(), 7);
    }

    #[test]
    fn explicit_environment_config_flag_is_preserved() {
        let config = from_source("export default { kit: { experimental: { explicitEnvironmentVariables: true }, env: { dir: 'config', publicPrefix: 'APP_' } } };").unwrap();
        assert!(config.explicit_environment_variables);
        assert_eq!(config.env_dir, "config");
        assert_eq!(config.public_prefix, "APP_");
        assert!(from_source("export default { kit: { experimental: { explicitEnvironmentVariables: getFlag() } } };").is_err());
    }

    #[test]
    fn static_env_exports_spreads_helpers_and_inline_schema_preserve_types() {
        let variables = parse_explicit_env_source(r#"
            import { defineEnvVars as define } from '@sveltejs/kit/env';
            const defaults = { public: true };
            const fields = { SITE: { ...defaults, description: 'Site name' }, PORT: { schema: (value: string | undefined) => Number(value) } };
            const defined = define({ ...fields, TOKEN: {} });
            export { defined as variables };
        "#, true).unwrap();
        assert_eq!(variables.len(), 3);
        let site = variables.iter().find(|v| v.name == "SITE").unwrap();
        assert!(site.public);
        assert_eq!(site.description.as_deref(), Some("Site name"));
        assert!(
            variables
                .iter()
                .find(|v| v.name == "PORT")
                .unwrap()
                .has_schema
        );
        assert!(
            !variables
                .iter()
                .find(|v| v.name == "TOKEN")
                .unwrap()
                .has_schema
        );
        let kit2 = parse_explicit_env_source("import { defineEnvVars } from '@sveltejs/kit/hooks'; export const variables=defineEnvVars({ NAME: { schema: { '~standard': { validate(value) { return { value }; } } } } });", false).unwrap();
        assert!(kit2[0].has_schema);
    }

    #[test]
    fn env_factories_mutations_and_invalid_names_are_explicit_errors() {
        for source in [
            "import * as z from 'zod'; export const variables={ PORT:{schema:z.number()} };",
            "export const variables={ NAME:{} }; variables.NAME.public=true;",
            "export const variables=makeVariables();",
            "export const variables={ default:{} };",
            "export const variables={ NAME:{ get public(){ return true; } } };",
            "import type {defineEnvVars} from '@sveltejs/kit/env'; export const variables=defineEnvVars({NAME:{}});",
            "const variables={NAME:{}}; export type {variables};",
        ] {
            assert!(parse_explicit_env_source(source, true).is_err(), "{source}");
        }
        let variable = parse_explicit_env_source(
            "export const variables={ NAME:{schema:null,public:false} };",
            true,
        )
        .unwrap();
        assert!(!variable[0].has_schema);
        assert!(!variable[0].public);
    }

    #[test]
    fn static_config_tree_keeps_imported_calls_with_static_arguments() {
        let tree = static_config_tree(
            r#"import adapter from '@sveltejs/adapter-node';
import { vitePreprocess } from '@sveltejs/vite-plugin-svelte';
import { mdsvex as md } from 'mdsvex';
const local = () => 1;
const config = {
    extensions: ['.svelte', '.md'],
    preprocess: [vitePreprocess(), md({ extensions: ['.md'] })],
    compilerOptions: { runes: true },
    kit: {
        adapter: adapter({ out: 'dist', precompress: false }),
        other: local(),
        spread: adapter(...[]),
        experimental: { instrumentation: { server: true } }
    }
};
export default config;"#,
        )
        .unwrap();
        assert_eq!(
            tree["preprocess"],
            json!([
                {"$call": {"module": "@sveltejs/vite-plugin-svelte", "export": "vitePreprocess", "arguments": []}},
                {"$call": {"module": "mdsvex", "export": "mdsvex", "arguments": [{"extensions": [".md"], "$keys": ["extensions"]}]}}
            ])
        );
        assert_eq!(
            tree["kit"]["adapter"],
            json!({"$call": {"module": "@sveltejs/adapter-node", "export": "default", "arguments": [{"out": "dist", "precompress": false, "$keys": ["out", "precompress"]}]}})
        );
        // Calls of local bindings and spread arguments stay dynamic.
        assert!(tree["kit"]["other"].get("$nativeDynamic").is_some());
        assert!(tree["kit"]["spread"].get("$nativeDynamic").is_some());
        assert_eq!(
            tree["kit"]["experimental"]["instrumentation"]["server"],
            true
        );
        assert_eq!(tree["compilerOptions"]["runes"], true);
        assert_eq!(
            tree["kit"]["$keys"],
            json!(["adapter", "other", "spread", "experimental"])
        );
    }
}
