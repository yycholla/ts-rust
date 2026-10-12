//! Native build-time environment modules for the registered Kit 2/3 profiles.
//!
//! Ported from SvelteKit's `src/core/env.js`, `src/exports/internal/env.js`,
//! Vite environment-module loader and production service-worker writer.
//! Copyright (c) Svelte contributors, MIT license (SvelteKit/LICENSE).
//!
//! `generate_build_environment` is pure: the caller supplies resolved configuration,
//! build values and variable metadata. It never loads dotenv, reads process env,
//! evaluates an application module or writes files. Custom static validators need
//! an explicit validated result tied to the input value. Dynamic validation stays
//! in the actual application/runtime modules and executes when the server calls
//! `set_env`. JSON values and undefined are supported static results; richer
//! devalue values must be resolved by a future typed serializer, not JS snippets.
//!
//! Materialize `files` unchanged, then use `resolve` for environment imports in
//! each graph (including resolved absolute IDs). Other runtime imports still need
//! the ordinary Kit aliases, package conditions and build defines. Kit 3's env
//! directory must be `<generated>/env`; its generated-module aliases are included.
//! This is a production-build API; development initialization is deliberately not
//! represented. Service-worker bootstrap source is returned separately for Kit 3.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::build_server::version_hash;
use crate::sync_versions::KitSyncVersion;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResolvedBuildEnvironment {
    pub kit_version: String,
    /// Absolute directory for the five Kit 3 generated environment files.
    /// Required for every profile so changing versions cannot change path roots.
    pub environment_directory: PathBuf,
    /// Absolute `@sveltejs/kit/src/runtime`, from the selected installed package.
    pub runtime_directory: PathBuf,
    pub version_name: String,
    pub base: String,
    pub app_dir: String,
    /// Already resolved build values, never implicitly augmented from the process.
    pub build_values: BTreeMap<String, String>,
    pub public_prefix: String,
    pub private_prefix: String,
    /// Kit 2 experimental flag. Kit 3 always uses explicit variables.
    pub explicit_environment_variables: bool,
    /// None means no env entry exists; Some with an empty map is an actual entry.
    pub definition: Option<ResolvedEnvDefinition>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResolvedEnvDefinition {
    /// Real application module exporting `variables`; retained for runtime schemas.
    pub entry: PathBuf,
    pub variables: BTreeMap<String, ResolvedEnvVariable>,
    /// Object.keys(variables) order from the resolved definition. Validator order
    /// is observable, so a sorted Rust map must not silently change it.
    pub declaration_order: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResolvedEnvVariable {
    pub public: bool,
    #[serde(rename = "static")]
    pub is_static: bool,
    pub schema: ResolvedEnvSchema,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum ResolvedEnvSchema {
    None,
    /// Metadata says the original variable has a Standard Schema validator.
    /// Dynamic variables must leave static_validation absent: runtime validates.
    Standard {
        #[serde(rename = "staticValidation")]
        static_validation: Option<ResolvedStaticValidation>,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResolvedStaticValidation {
    /// Exact original input, including absence, to prevent stale cached results.
    pub input: Option<String>,
    pub value: ResolvedEnvValue,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "camelCase")]
pub enum ResolvedEnvValue {
    Undefined,
    Json(Value),
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum EnvironmentTarget {
    Server,
    Client,
    ServiceWorker,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", content = "value", rename_all = "camelCase")]
pub enum EnvironmentModule {
    Source(String),
    /// An actual installed runtime file or one of the returned generated files.
    File(PathBuf),
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GeneratedBuildEnvironment {
    pub files: BTreeMap<PathBuf, String>,
    pub server_modules: BTreeMap<String, EnvironmentModule>,
    pub client_modules: BTreeMap<String, EnvironmentModule>,
    pub service_worker_modules: BTreeMap<String, EnvironmentModule>,
    /// Must run before the application's Kit 3 service-worker entry.
    pub service_worker_bootstrap: Option<PathBuf>,
    /// Reject these even when imported using resolved paths or internal IDs.
    pub server_only_modules: BTreeSet<String>,
    /// Environment-specific defines only; other Kit build defines are separate.
    pub defines: BTreeMap<String, String>,
    pub service_worker_defines: BTreeMap<String, String>,
    /// Runtime-served module URLs, which must not be bundled from the filesystem.
    pub service_worker_externals: Vec<String>,
}

impl GeneratedBuildEnvironment {
    /// None means this resolver does not own the import. A forbidden env import
    /// is an error, never None (which could fall through to a normal resolver).
    pub fn resolve(
        &self,
        id: &str,
        target: EnvironmentTarget,
    ) -> Result<Option<&EnvironmentModule>, String> {
        let id = id.split(['?', '#']).next().unwrap_or(id);
        if target != EnvironmentTarget::Server && self.server_only_modules.contains(id) {
            return Err(format!("Cannot import {id} into {target:?} code"));
        }
        let modules = match target {
            EnvironmentTarget::Server => &self.server_modules,
            EnvironmentTarget::Client => &self.client_modules,
            EnvironmentTarget::ServiceWorker => &self.service_worker_modules,
        };
        if let Some(module) = modules.get(id) {
            return Ok(Some(module));
        }
        if id.starts_with("$env/")
            || id.starts_with("__sveltekit/env")
            || id.starts_with("<sveltekit:generated>/env/")
            || matches!(id, "$app/env/private" | "$app/env/public")
        {
            return Err(format!(
                "Environment module {id} is unavailable in {target:?} code"
            ));
        }
        Ok(None)
    }
}

fn js_string(value: &str) -> String {
    // Also safe when later embedded into an HTML script by a downstream writer.
    serde_json::to_string(value)
        .expect("a Rust string is JSON serializable")
        .replace('<', "\\u003C")
        .replace('\u{2028}', "\\u2028")
        .replace('\u{2029}', "\\u2029")
}

fn value_expression(value: &ResolvedEnvValue) -> Result<String, String> {
    Ok(match value {
        ResolvedEnvValue::Undefined => "void 0".into(),
        ResolvedEnvValue::Json(value) => json_expression(value),
    })
}

fn json_expression(value: &Value) -> String {
    match value {
        // Computed keys preserve __proto__ as an own property. No ambient JSON
        // binding is needed, so a variable literally named JSON remains valid.
        Value::Object(object) => format!(
            "{{ {} }}",
            object
                .iter()
                .map(|(key, value)| format!("[{}]: {}", js_string(key), json_expression(value)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Value::Array(values) => format!(
            "[{}]",
            values
                .iter()
                .map(json_expression)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Value::String(value) => js_string(value),
        _ => value.to_string(),
    }
}

fn identifier(name: &str) -> bool {
    let mut bytes = name.bytes();
    bytes
        .next()
        .is_some_and(|byte| byte.is_ascii_alphabetic() || matches!(byte, b'_' | b'$'))
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'$'))
        && !matches!(
            name,
            "do" | "if"
                | "in"
                | "for"
                | "let"
                | "new"
                | "try"
                | "var"
                | "case"
                | "else"
                | "enum"
                | "eval"
                | "null"
                | "this"
                | "true"
                | "void"
                | "with"
                | "await"
                | "break"
                | "catch"
                | "class"
                | "const"
                | "false"
                | "super"
                | "throw"
                | "while"
                | "yield"
                | "delete"
                | "export"
                | "import"
                | "public"
                | "return"
                | "static"
                | "switch"
                | "typeof"
                | "default"
                | "extends"
                | "finally"
                | "package"
                | "private"
                | "continue"
                | "debugger"
                | "function"
                | "arguments"
                | "interface"
                | "protected"
                | "implements"
                | "instanceof"
        )
}

fn absolute(path: &Path, label: &str) -> Result<(), String> {
    if !path.is_absolute()
        || path
            .components()
            .any(|part| matches!(part, Component::ParentDir | Component::CurDir))
        || path
            .to_str()
            .is_none_or(|text| text.contains(['\0', '?', '#']))
    {
        return Err(format!(
            "{label} must be an absolute normalized UTF-8 path without query or fragment"
        ));
    }
    Ok(())
}

fn path_id(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn relative(from: &Path, to: &Path) -> String {
    let from: Vec<_> = from.components().collect();
    let to: Vec<_> = to.components().collect();
    let shared = from.iter().zip(&to).take_while(|(a, b)| a == b).count();
    let mut path = PathBuf::new();
    for _ in shared..from.len() {
        path.push("..");
    }
    for component in &to[shared..] {
        path.push(component.as_os_str());
    }
    let id = path_id(&path);
    if id.starts_with('.') {
        id
    } else {
        format!("./{id}")
    }
}

fn static_value(
    name: &str,
    variable: &ResolvedEnvVariable,
    input: &ResolvedBuildEnvironment,
    kit3: bool,
) -> Result<String, String> {
    let raw = input.build_values.get(name);
    match &variable.schema {
        ResolvedEnvSchema::None => {
            if raw.is_none() || (!kit3 && raw.is_some_and(String::is_empty)) {
                return Err(format!(
                    "Invalid environment variables: {name}: Value is missing. If it is optional, add {}validator declaring it as such.",
                    if kit3 { "a " } else { "a Standard Schema " }
                ));
            }
            Ok(js_string(raw.expect("absence rejected above")))
        }
        ResolvedEnvSchema::Standard { static_validation } => {
            let result = static_validation.as_ref().ok_or_else(|| {
                format!("Unresolved static environment schema for {name}: supply a validated static result; application JavaScript is not evaluated")
            })?;
            if result.input.as_ref() != raw {
                return Err(format!(
                    "Stale static environment validation for {name}: input does not match buildValues"
                ));
            }
            value_expression(&result.value)
        }
    }
}

fn disabled_legacy(id: &str, disabled: bool) -> String {
    if !disabled {
        return String::new();
    }
    let replacement = id
        .replace("$env/static", "$app/env")
        .replace("$env/dynamic", "$app/env");
    format!(
        "throw new Error({});\n",
        js_string(&format!(
            "Cannot import `{id}` when `experimental.explicitEnvironmentVariables` is enabled. Use `{replacement}` instead."
        ))
    )
}

fn legacy_static(input: &ResolvedBuildEnvironment, public: bool) -> String {
    let visibility = if public { "public" } else { "private" };
    let (allowed, disallowed) = if public {
        (&input.public_prefix, &input.private_prefix)
    } else {
        (&input.private_prefix, &input.public_prefix)
    };
    let mut source = disabled_legacy(
        &format!("$env/static/{visibility}"),
        input.explicit_environment_variables,
    );
    for (name, value) in &input.build_values {
        if name.starts_with(allowed)
            && (disallowed.is_empty() || !name.starts_with(disallowed))
            && identifier(name)
        {
            source.push_str(&format!("export const {name} = {};\n", js_string(value)));
        }
    }
    source
}

fn source(source: impl Into<String>) -> EnvironmentModule {
    EnvironmentModule::Source(source.into())
}

/// Generate production env modules without observing ambient process state.
pub fn generate_build_environment(
    input: &ResolvedBuildEnvironment,
) -> Result<GeneratedBuildEnvironment, String> {
    let kit3 = KitSyncVersion::parse(&input.kit_version)?.is_kit3();
    absolute(&input.environment_directory, "environmentDirectory")?;
    absolute(&input.runtime_directory, "runtimeDirectory")?;
    if input.app_dir.is_empty()
        || Path::new(&input.app_dir).is_absolute()
        || input.app_dir.contains(['\\', '\0', '?', '#'])
        || input
            .app_dir
            .split('/')
            .any(|part| matches!(part, "" | "." | ".."))
        || (!input.base.is_empty() && (!input.base.starts_with('/') || input.base.ends_with('/')))
        || input.base.contains(['\\', '\0', '?', '#'])
    {
        return Err("appDir must be a relative directory and base must be empty or an absolute URL path without a trailing slash".into());
    }
    if input.definition.is_some() && !kit3 && !input.explicit_environment_variables {
        return Err("Resolved explicit environment variables require experimental.explicitEnvironmentVariables in Kit 2".into());
    }
    let empty = BTreeMap::new();
    let variables = if let Some(definition) = &input.definition {
        absolute(&definition.entry, "definition.entry")?;
        if definition.declaration_order.len() != definition.variables.len()
            || definition.declaration_order.iter().collect::<BTreeSet<_>>()
                != definition.variables.keys().collect::<BTreeSet<_>>()
        {
            return Err("definition.declarationOrder must contain each variable exactly once, in its original declaration order".into());
        }
        &definition.variables
    } else {
        &empty
    };
    let mut public_exports = String::new();
    let mut private_exports = String::new();
    let mut declarations = String::new();
    let mut setters = String::new();
    let mut sw_properties = Vec::new();
    let mut sw_dynamic = false;
    // Avoid a generated namespace binding colliding with a legal env export.
    let mut namespace = "env".to_owned();
    while variables.contains_key(&namespace) {
        namespace.push('_');
    }
    let mut payload_namespace = "payload".to_owned();
    while variables.contains_key(&payload_namespace) {
        payload_namespace.push('_');
    }
    for name in input
        .definition
        .iter()
        .flat_map(|definition| &definition.declaration_order)
    {
        let variable = &variables[name];
        if !identifier(name) {
            return Err(format!("Invalid environment variable name {name:?}"));
        }
        let expression = if variable.is_static {
            let value = static_value(name, variable, input, kit3)?;
            if variable.public {
                declarations.push_str(&format!("explicit_public_env.{name} = {value};\n"));
                sw_properties.push(format!("{name}: {value}"));
            }
            value
        } else {
            if matches!(
                &variable.schema,
                ResolvedEnvSchema::Standard {
                    static_validation: Some(_)
                }
            ) {
                return Err(format!(
                    "Dynamic environment variable {name} must not supply a static validation result"
                ));
            }
            // Blocks avoid shadowing env/variables/validate for legal user names.
            setters.push_str(&format!(
                "{{ const value = validate(variables, env.{name}, {}, issues);\n",
                js_string(name)
            ));
            if variable.public {
                setters.push_str(&format!(
                    "explicit_public_env.{name} = value; rendered_env.{name} = value; }}\n"
                ));
                sw_dynamic = true;
            } else {
                setters.push_str(&format!("dynamic_private_env.{name} = value; }}\n"));
            }
            format!("{namespace}.{name}")
        };
        let exports = if variable.public {
            &mut public_exports
        } else {
            &mut private_exports
        };
        exports.push_str(&format!("export const {name} = {expression};\n"));
    }
    let config_imports = if let Some(definition) = &input.definition {
        format!(
            "import {{ variables }} from {};\nimport {{ validate, handle_issues }} from '@sveltejs/kit/internal/env';",
            js_string(&path_id(&definition.entry))
        )
    } else {
        "const variables = {};\nconst handle_issues = () => {};".into()
    };
    let config = format!(
        "{config_imports}\nconst issues = {{}};\nexport {{ variables }};\nexport const dynamic_private_env = {{}};\nexport const explicit_public_env = {{}};\nexport const rendered_env = {{}};\n{declarations}handle_issues(issues);\nexport function set_env(env) {{\nconst issues = {{}};\n{setters}handle_issues(issues);\n}}\n"
    );
    let module = |prelude: String, exports: &str| {
        if input.definition.is_some() {
            format!("{prelude}\n{exports}")
        } else {
            String::new()
        }
    };
    let global = format!(
        "globalThis.__sveltekit_{}",
        version_hash(&input.version_name)
    );
    let mut out = GeneratedBuildEnvironment {
        files: BTreeMap::new(),
        server_modules: BTreeMap::new(),
        client_modules: BTreeMap::new(),
        service_worker_modules: BTreeMap::new(),
        service_worker_bootstrap: None,
        server_only_modules: BTreeSet::from([
            "$env/static/private".into(),
            "$env/dynamic/private".into(),
            "$app/env/private".into(),
        ]),
        defines: if kit3 {
            BTreeMap::new()
        } else {
            BTreeMap::from([(
                "__SVELTEKIT_EXPERIMENTAL_EXPLICIT_ENVIRONMENT_VARIABLES__".into(),
                input.explicit_environment_variables.to_string(),
            )])
        },
        service_worker_defines: BTreeMap::new(),
        service_worker_externals: Vec::new(),
    };
    let runtime = &input.runtime_directory;
    let public_runtime = runtime.join("app/env/public/index.js");
    let private_runtime = runtime.join(if kit3 {
        "app/env/private/index.js"
    } else {
        "app/env/private.js"
    });
    out.server_only_modules.insert(path_id(&private_runtime));
    out.server_only_modules
        .insert(path_id(&runtime.join("app/env/public/server.js")));
    out.server_modules.insert(
        "$app/env/private".into(),
        EnvironmentModule::File(private_runtime),
    );
    for modules in [&mut out.server_modules, &mut out.client_modules] {
        modules.insert(
            "$app/env/public".into(),
            EnvironmentModule::File(public_runtime.clone()),
        );
    }
    if kit3 {
        let dir = &input.environment_directory;
        let sw_prelude = if sw_dynamic {
            format!(
                "import {{ env }} from {};\n",
                js_string(&format!("{}/{}/env.js", input.base, input.app_dir))
            )
        } else {
            String::new()
        };
        let sw_env = if sw_dynamic {
            "env".into()
        } else {
            format!("{{ {} }}", sw_properties.join(", "))
        };
        let sw = format!(
            "{sw_prelude}{global} = {{ base: location.pathname.split('/').slice(0, -1).join('/'), env: {sw_env}, version: {} }};\n",
            js_string(&input.version_name)
        );
        for (name, code, server_only) in [
            ("config.js", config, true),
            (
                "public/server.js",
                module(
                    format!("import {{ rendered_env as {namespace} }} from '../config.js';"),
                    &public_exports,
                ),
                true,
            ),
            (
                "private/server.js",
                module(
                    format!("import {{ dynamic_private_env as {namespace} }} from '../config.js';"),
                    &private_exports,
                ),
                true,
            ),
            (
                "public/client.js",
                module(
                    format!(
                        "import {{ payload as {payload_namespace} }} from {};\nconst {namespace} = {payload_namespace}.env;",
                        js_string(&relative(
                            &dir.join("public"),
                            &runtime.join("client/payload.js")
                        ))
                    ),
                    &public_exports,
                ),
                false,
            ),
            ("service-worker.js", sw, false),
        ] {
            let path = dir.join(name);
            let alias = format!("<sveltekit:generated>/env/{name}");
            out.files.insert(path.clone(), code);
            if server_only {
                out.server_only_modules.insert(alias.clone());
                out.server_only_modules.insert(path_id(&path));
                out.server_modules
                    .insert(alias, EnvironmentModule::File(path));
            } else if name == "public/client.js" {
                out.client_modules
                    .insert(alias, EnvironmentModule::File(path));
            } else {
                out.service_worker_modules
                    .insert(alias, EnvironmentModule::File(path.clone()));
                out.service_worker_bootstrap = Some(path);
            }
        }
        for kind in ["static", "dynamic"] {
            for visibility in ["public", "private"] {
                let id = format!("$env/{kind}/{visibility}");
                let path = runtime.join(format!("env/{kind}/{visibility}.js"));
                out.server_modules
                    .insert(id.clone(), EnvironmentModule::File(path.clone()));
                if visibility == "public" {
                    out.client_modules.insert(id, EnvironmentModule::File(path));
                } else {
                    out.server_only_modules.insert(path_id(&path));
                }
            }
        }
        // Kit 3 workers share client runtime modules, with a worker-specific
        // payload define. Unlike Kit 2, dynamic/public is available here too.
        out.service_worker_modules
            .extend(out.client_modules.clone());
        out.service_worker_defines
            .insert("__SVELTEKIT_PAYLOAD__".into(), global.clone());
        if sw_dynamic {
            out.service_worker_externals
                .push(format!("{}/{}/env.js", input.base, input.app_dir));
        }
    } else {
        for public in [false, true] {
            let visibility = if public { "public" } else { "private" };
            let static_id = format!("$env/static/{visibility}");
            let static_module = source(legacy_static(input, public));
            out.server_modules
                .insert(static_id.clone(), static_module.clone());
            let dynamic_id = format!("$env/dynamic/{visibility}");
            let dynamic = source(format!(
                "{}export {{ {visibility}_env as env }} from {};\n",
                disabled_legacy(&dynamic_id, input.explicit_environment_variables),
                js_string(&path_id(&runtime.join("shared-server.js")))
            ));
            out.server_modules.insert(dynamic_id.clone(), dynamic);
            if public {
                out.client_modules
                    .insert(static_id.clone(), static_module.clone());
                out.service_worker_modules.insert(static_id, static_module);
                out.client_modules.insert(
                    dynamic_id,
                    source(format!("export const env = {global}.env;\n")),
                );
            }
        }
        for (id, code) in [
            ("__sveltekit/env", config),
            (
                "__sveltekit/env/private",
                module(
                    format!(
                        "import {{ dynamic_private_env as {namespace} }} from '__sveltekit/env';"
                    ),
                    &private_exports,
                ),
            ),
            (
                "__sveltekit/env/public/server",
                module(
                    format!("import {{ rendered_env as {namespace} }} from '__sveltekit/env';"),
                    &public_exports,
                ),
            ),
        ] {
            out.server_only_modules.insert(id.into());
            out.server_modules.insert(id.into(), source(code));
        }
        out.client_modules.insert(
            "__sveltekit/env/public/client".into(),
            source(module(
                format!("const {namespace} = {global}.env;"),
                &public_exports,
            )),
        );
        let sw_prelude = if sw_dynamic {
            format!(
                "importScripts({}); const {namespace} = globalThis.__sveltekit_sw.env;",
                js_string(&format!("{}/{}/env.script.js", input.base, input.app_dir))
            )
        } else {
            String::new()
        };
        out.service_worker_modules.insert(
            "$app/env/public".into(),
            source(module(sw_prelude, &public_exports)),
        );
    }
    // Relative imports in installed compatibility wrappers must resolve through
    // the same target policies as public aliases, including private-file guards.
    for modules in [
        &mut out.server_modules,
        &mut out.client_modules,
        &mut out.service_worker_modules,
    ] {
        let files = modules
            .values()
            .filter_map(|module| match module {
                EnvironmentModule::File(path) => Some((path_id(path), module.clone())),
                EnvironmentModule::Source(_) => None,
            })
            .collect::<Vec<_>>();
        modules.extend(files);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn input(version: &str) -> ResolvedBuildEnvironment {
        ResolvedBuildEnvironment {
            kit_version: version.into(),
            environment_directory: "/app/.kit/generated/env".into(),
            runtime_directory: "/app/node_modules/@sveltejs/kit/src/runtime".into(),
            version_name: "one".into(),
            base: "/base".into(),
            app_dir: "_app".into(),
            build_values: BTreeMap::new(),
            public_prefix: "PUBLIC_".into(),
            private_prefix: "".into(),
            explicit_environment_variables: false,
            definition: None,
        }
    }
    fn variable(public: bool, is_static: bool) -> ResolvedEnvVariable {
        ResolvedEnvVariable {
            public,
            is_static,
            schema: ResolvedEnvSchema::None,
        }
    }
    fn define(input: &mut ResolvedBuildEnvironment, variables: &[(&str, bool, bool)]) {
        input.explicit_environment_variables = true;
        input.definition = Some(ResolvedEnvDefinition {
            entry: "/app/src/env.ts".into(),
            declaration_order: variables
                .iter()
                .map(|(name, _, _)| (*name).into())
                .collect(),
            variables: variables
                .iter()
                .map(|(name, public, is_static)| ((*name).into(), variable(*public, *is_static)))
                .collect(),
        });
    }
    fn text(module: &EnvironmentModule) -> &str {
        match module {
            EnvironmentModule::Source(text) => text,
            _ => panic!("expected virtual source"),
        }
    }

    #[test]
    fn kit2_filters_exports_and_keeps_runtime_dynamic_values() {
        let mut request = input("2.69.2");
        request.build_values = BTreeMap::from([
            ("PUBLIC_URL".into(), "</script>".into()),
            ("SECRET".into(), "secret".into()),
            ("bad-name".into(), "bad".into()),
            ("class".into(), "bad".into()),
        ]);
        let output = generate_build_environment(&request).unwrap();
        assert_eq!(
            text(
                output
                    .resolve("$env/static/public", EnvironmentTarget::Client)
                    .unwrap()
                    .unwrap()
            ),
            "export const PUBLIC_URL = \"\\u003C/script>\";\n"
        );
        let private = text(
            output
                .resolve("$env/static/private", EnvironmentTarget::Server)
                .unwrap()
                .unwrap(),
        );
        assert_eq!(private, "export const SECRET = \"secret\";\n");
        assert!(
            text(&output.server_modules["$env/dynamic/private"]).contains("private_env as env")
        );
        assert!(!text(&output.server_modules["$env/dynamic/private"]).contains("secret"));
        assert!(
            text(&output.client_modules["$env/dynamic/public"]).contains("globalThis.__sveltekit_")
        );
        assert!(
            output
                .resolve("$env/static/private?raw", EnvironmentTarget::Client)
                .is_err()
        );
        assert!(
            output
                .resolve("$env/dynamic/public", EnvironmentTarget::ServiceWorker)
                .is_err()
        );
    }

    #[test]
    fn kit2_explicit_flag_and_worker_dynamic_loader_match_runtime() {
        let mut request = input("2.70.3");
        define(
            &mut request,
            &[("COUNT", true, false), ("SECRET", false, false)],
        );
        let output = generate_build_environment(&request).unwrap();
        assert!(
            text(&output.server_modules["$env/static/public"])
                .contains("when `experimental.explicitEnvironmentVariables` is enabled")
        );
        assert!(!text(&output.client_modules["$env/dynamic/public"]).contains("throw"));
        let worker = text(&output.service_worker_modules["$app/env/public"]);
        assert!(worker.contains("importScripts(\"/base/_app/env.script.js\")"));
        assert!(worker.contains("export const COUNT = env.COUNT"));
        assert!(!worker.contains("SECRET"));
        let config = text(&output.server_modules["__sveltekit/env"]);
        assert!(config.contains("import { variables } from \"/app/src/env.ts\""));
        assert!(config.contains("validate(variables, env.COUNT"));
        assert!(config.contains("dynamic_private_env.SECRET = value"));
        assert!(
            output
                .resolve("__sveltekit/env", EnvironmentTarget::Client)
                .is_err()
        );
        request.explicit_environment_variables = false;
        assert!(
            generate_build_environment(&request)
                .unwrap_err()
                .contains("require experimental")
        );
    }

    #[test]
    fn kit3_generates_payload_and_real_runtime_wrappers() {
        let mut request = input("3.0.1");
        define(
            &mut request,
            &[
                ("TITLE", true, true),
                ("LIVE", true, false),
                ("SECRET", false, true),
            ],
        );
        request.build_values = BTreeMap::from([
            ("TITLE".into(), "app".into()),
            ("SECRET".into(), "private".into()),
        ]);
        let output = generate_build_environment(&request).unwrap();
        assert_eq!(output.files.len(), 5);
        let dir = &request.environment_directory;
        let client = &output.files[&dir.join("public/client.js")];
        assert!(client.contains("import { payload as payload } from \"../../../../node_modules/@sveltejs/kit/src/runtime/client/payload.js\""));
        assert!(client.contains("const env = payload.env"));
        assert!(client.contains("export const TITLE = \"app\""));
        assert!(!client.contains("private"));
        assert!(
            output.files[&dir.join("private/server.js")]
                .contains("export const SECRET = \"private\"")
        );
        assert!(
            output.files[&dir.join("service-worker.js")]
                .contains("import { env } from \"/base/_app/env.js\"")
        );
        assert!(matches!(
            output.client_modules["$env/static/public"],
            EnvironmentModule::File(_)
        ));
        assert!(
            output
                .resolve(
                    &path_id(&dir.join("private/server.js")),
                    EnvironmentTarget::Client
                )
                .is_err()
        );
        assert!(output.service_worker_bootstrap.is_some());
        assert!(
            output
                .resolve("$env/dynamic/public", EnvironmentTarget::ServiceWorker)
                .unwrap()
                .is_some()
        );
        assert_eq!(output.service_worker_externals, vec!["/base/_app/env.js"]);
        assert!(
            output.service_worker_defines["__SVELTEKIT_PAYLOAD__"]
                .starts_with("globalThis.__sveltekit_")
        );
    }

    #[test]
    fn missing_and_empty_values_have_versioned_validation() {
        for version in [
            "2.67.0",
            "2.68.0",
            "2.69.2",
            "2.69.3",
            "2.70.3",
            "3.0.1",
            "3.0.0-next.32",
        ] {
            let mut request = input(version);
            define(&mut request, &[("VALUE", true, true)]);
            assert!(
                generate_build_environment(&request)
                    .unwrap_err()
                    .contains("Value is missing")
            );
            request.build_values.insert("VALUE".into(), "".into());
            assert_eq!(
                generate_build_environment(&request).is_ok(),
                version.starts_with('3')
            );
        }
    }

    #[test]
    fn custom_static_schema_requires_current_typed_result() {
        let mut request = input("3.0.1");
        define(&mut request, &[("VALUE", true, true)]);
        request.build_values.insert("VALUE".into(), "42".into());
        let variable = request
            .definition
            .as_mut()
            .unwrap()
            .variables
            .get_mut("VALUE")
            .unwrap();
        variable.schema = ResolvedEnvSchema::Standard {
            static_validation: None,
        };
        assert!(
            generate_build_environment(&request)
                .unwrap_err()
                .contains("Unresolved static")
        );
        request
            .definition
            .as_mut()
            .unwrap()
            .variables
            .get_mut("VALUE")
            .unwrap()
            .schema = ResolvedEnvSchema::Standard {
            static_validation: Some(ResolvedStaticValidation {
                input: Some("42".into()),
                value: ResolvedEnvValue::Json(json!(42)),
            }),
        };
        let output = generate_build_environment(&request).unwrap();
        assert!(
            output.files[&request.environment_directory.join("public/server.js")]
                .contains("export const VALUE = 42")
        );
        request.build_values.insert("VALUE".into(), "43".into());
        assert!(
            generate_build_environment(&request)
                .unwrap_err()
                .contains("Stale static")
        );
    }

    #[test]
    fn undefined_and_json_objects_are_values_not_code() {
        assert_eq!(
            value_expression(&ResolvedEnvValue::Undefined).unwrap(),
            "void 0"
        );
        assert_eq!(
            value_expression(&ResolvedEnvValue::Json(json!({"__proto__": {"x": 1}}))).unwrap(),
            "{ [\"__proto__\"]: { [\"x\"]: 1 } }"
        );
        assert!(
            serde_json::from_value::<ResolvedEnvValue>(
                json!({"kind":"javascript", "value":"process.env"})
            )
            .is_err()
        );
    }

    #[test]
    fn empty_definitions_no_definitions_and_static_workers() {
        let absent = generate_build_environment(&input("3.0.1")).unwrap();
        assert_eq!(
            absent.files[Path::new("/app/.kit/generated/env/public/client.js")],
            ""
        );
        assert!(
            absent.files[Path::new("/app/.kit/generated/env/config.js")]
                .contains("const variables = {}")
        );
        let mut request = input("3.0.1");
        define(&mut request, &[("NAME", true, true)]);
        request.build_values.insert("NAME".into(), "ok".into());
        let output = generate_build_environment(&request).unwrap();
        let worker = &output.files[&request.environment_directory.join("service-worker.js")];
        assert!(worker.contains("env: { NAME: \"ok\" }"));
        assert!(!worker.contains("import { env }"));
        assert!(!worker.contains("SECRET"));
    }

    #[test]
    fn invalid_names_versions_and_paths_are_explicit_errors() {
        let mut request = input("3.0.1");
        define(&mut request, &[("default", true, false)]);
        assert!(
            generate_build_environment(&request)
                .unwrap_err()
                .contains("Invalid environment variable name")
        );
        request.kit_version = "3.1.0".into();
        assert!(
            generate_build_environment(&request)
                .unwrap_err()
                .contains("Unsupported native Kit")
        );
        request.kit_version = "3.0.1".into();
        request.environment_directory = "relative".into();
        assert!(
            generate_build_environment(&request)
                .unwrap_err()
                .contains("absolute normalized")
        );
    }

    #[test]
    fn resolved_declaration_order_and_namespace_collisions_are_preserved() {
        let mut request = input("3.0.1");
        define(
            &mut request,
            &[
                ("ZZZ", true, false),
                ("AAA", true, false),
                ("env", true, false),
                ("payload", true, false),
            ],
        );
        let output = generate_build_environment(&request).unwrap();
        let config = &output.files[&request.environment_directory.join("config.js")];
        assert!(config.find("env.ZZZ").unwrap() < config.find("env.AAA").unwrap());
        let client = &output.files[&request.environment_directory.join("public/client.js")];
        assert!(client.contains("import { payload as payload_ }"));
        assert!(client.contains("const env_ = payload_.env;"));
        assert!(client.contains("export const env = env_.env;"));
        assert!(client.contains("export const payload = env_.payload;"));
        request.definition.as_mut().unwrap().declaration_order[0] = "AAA".into();
        assert!(
            generate_build_environment(&request)
                .unwrap_err()
                .contains("each variable exactly once")
        );
    }

    #[test]
    fn overlapping_prefixes_exclude_values_from_both_legacy_modules() {
        let mut request = input("2.70.3");
        request.public_prefix = "ENV_".into();
        request.private_prefix = "ENV_PRIVATE_".into();
        request.build_values = BTreeMap::from([
            ("ENV_PUBLIC".into(), "public".into()),
            ("ENV_PRIVATE_TOKEN".into(), "private".into()),
            ("OTHER".into(), "other".into()),
        ]);
        let output = generate_build_environment(&request).unwrap();
        assert_eq!(
            text(&output.server_modules["$env/static/public"]),
            "export const ENV_PUBLIC = \"public\";\n"
        );
        assert_eq!(text(&output.server_modules["$env/static/private"]), "");
    }
}
