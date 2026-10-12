//! Pure generation of the pinned Kit server configuration modules.
//!
//! Inputs are already-resolved config values, absolute module paths and loaded
//! templates. This module neither executes config nor reads/writes files. Kit
//! 2.67–2.70 share one writer; registered Kit 3 profiles share the second writer.
use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::sync_versions::KitSyncVersion;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResolvedServerInput {
    pub kit_version: String,
    /// The generated directory passed to Kit's write_server, not Kit's outDir.
    pub output: PathBuf,
    pub app_template: String,
    /// Host-loaded src/error.html or the installed Kit default-error.html.
    pub error_template: String,
    pub version_name: String,
    #[serde(default)]
    pub server_hooks: Option<PathBuf>,
    #[serde(default)]
    pub universal_hooks: Option<PathBuf>,
    /// Required by Kit 2: the resolved Kit runtime directory.
    #[serde(default)]
    pub runtime_directory: Option<PathBuf>,
    #[serde(default = "svelte_major")]
    pub svelte_major: u32,
    #[serde(default)]
    pub service_worker_present: bool,
    #[serde(default)]
    pub options: ResolvedServerOptions,
}

fn svelte_major() -> u32 {
    5
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
pub enum PreloadStrategy {
    #[serde(rename = "modulepreload")]
    Modulepreload,
    #[serde(rename = "preload-js")]
    PreloadJs,
    #[serde(rename = "preload-mjs")]
    PreloadMjs,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct ResolvedServerOptions {
    pub csp: Value,
    pub csrf_check_origin: bool,
    pub csrf_trusted_origins: Vec<String>,
    pub embedded: bool,
    pub public_prefix: String,
    pub private_prefix: String,
    pub hash_routing: bool,
    #[serde(rename = "async")]
    pub async_compilation: bool,
    pub preload_strategy: PreloadStrategy,
    pub service_worker_register: bool,
    /// None is JavaScript `undefined`, not `{}` or `null`. Kit emits `null`
    /// instead when registration is disabled, regardless of this field.
    pub service_worker_options: Option<Value>,
    pub server_error_boundaries: bool,
    pub explicit_environment_variables: bool,
}

impl Default for ResolvedServerOptions {
    fn default() -> Self {
        Self {
            csp: json!({"mode":"auto","directives":{},"reportOnly":{}}),
            csrf_check_origin: true,
            csrf_trusted_origins: Vec::new(),
            embedded: false,
            public_prefix: "PUBLIC_".into(),
            private_prefix: String::new(),
            hash_routing: false,
            async_compilation: false,
            preload_strategy: PreloadStrategy::Modulepreload,
            service_worker_register: true,
            service_worker_options: None,
            server_error_boundaries: false,
            explicit_environment_variables: false,
        }
    }
}

fn stringify<T: Serialize + ?Sized>(value: &T) -> Result<String, String> {
    serde_json::to_string(value).map_err(|error| error.to_string())
}

fn validate_path(path: &Path, field: &str) -> Result<(), String> {
    if !path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::ParentDir | Component::CurDir))
    {
        return Err(format!(
            "{field} must be an absolute resolved path without parent segments"
        ));
    }
    let path = path
        .to_str()
        .ok_or_else(|| format!("{field} must be UTF-8"))?;
    if path.contains('\0') {
        return Err(format!("{field} contains a NUL byte"));
    }
    Ok(())
}

fn relative(from: &Path, to: &Path) -> String {
    let from = from.components().collect::<Vec<_>>();
    let to = to.components().collect::<Vec<_>>();
    let shared = from.iter().zip(&to).take_while(|(a, b)| a == b).count();
    let mut path = PathBuf::new();
    for _ in shared..from.len() {
        path.push("..");
    }
    for part in &to[shared..] {
        path.push(part.as_os_str());
    }
    path.to_string_lossy().replace('\\', "/")
}

/// Kit's reversed UTF-16 djb2 hash, including its unsigned 32-bit wraparound and
/// lowercase base-36 encoding. UTF-8 bytes would give different Unicode hashes.
#[must_use]
pub fn version_hash(version: &str) -> String {
    let mut hash = 5381_u32;
    for unit in version.encode_utf16().collect::<Vec<_>>().into_iter().rev() {
        hash = hash.wrapping_mul(33) ^ u32::from(unit);
    }
    if hash == 0 {
        return "0".into();
    }
    let mut digits = Vec::new();
    while hash != 0 {
        digits.push(char::from(
            b"0123456789abcdefghijklmnopqrstuvwxyz"[(hash % 36) as usize],
        ));
        hash /= 36;
    }
    digits.into_iter().rev().collect()
}

fn escape_html(value: &str) -> String {
    value.replace('&', "&amp;").replace('<', "&lt;")
}

/// JS String.replace with a string replacement interprets these dollar tokens.
/// The pinned writers use this form for the app version rather than a callback.
fn replacement_text(replacement: &str, matched: &str, prefix: &str, suffix: &str) -> String {
    let mut output = String::new();
    let mut characters = replacement.chars().peekable();
    while let Some(character) = characters.next() {
        if character == '$' {
            match characters.peek() {
                Some('$') => {
                    output.push('$');
                    characters.next();
                }
                Some('&') => {
                    output.push_str(matched);
                    characters.next();
                }
                Some('`') => {
                    output.push_str(prefix);
                    characters.next();
                }
                Some('\'') => {
                    output.push_str(suffix);
                    characters.next();
                }
                _ => output.push('$'),
            }
        } else {
            output.push(character);
        }
    }
    output
}

fn replace_version(input: &str, version: &str) -> String {
    let needle = "%sveltekit.version%";
    let replacement = escape_html(version);
    let mut output = String::new();
    let mut end = 0;
    for (start, _) in input.match_indices(needle) {
        output.push_str(&input[end..start]);
        end = start + needle.len();
        output.push_str(&replacement_text(
            &replacement,
            needle,
            &input[..start],
            &input[end..],
        ));
    }
    output.push_str(&input[end..]);
    output
}

fn env_captures(template: &str) -> Vec<&str> {
    let prefix = "%sveltekit.env.";
    let mut rest = template;
    let mut names = Vec::new();
    while let Some(start) = rest.find(prefix) {
        rest = &rest[start + prefix.len()..];
        let Some(end) = rest.find('%') else {
            break;
        };
        if end != 0 {
            names.push(&rest[..end]);
            rest = &rest[end + 1..];
        }
        // An empty capture doesn't match the upstream regex. Keep its `%` as
        // a possible start of the next token.
    }
    names
}

fn substitute_env(mut expression: &str) -> Result<String, String> {
    let prefix = "%sveltekit.env.";
    let mut output = String::new();
    while let Some(start) = expression.find(prefix) {
        output.push_str(&expression[..start]);
        let after_prefix = &expression[start + prefix.len()..];
        let Some(end) = after_prefix.find('%') else {
            output.push_str(&expression[start..]);
            return Ok(output);
        };
        if end == 0 {
            output.push_str(prefix);
            expression = after_prefix;
        } else {
            output.push_str(&format!(
                "\" + (env[{}] ?? \"\") + \"",
                stringify(&after_prefix[..end])?
            ));
            expression = &after_prefix[end + 1..];
        }
    }
    output.push_str(expression);
    Ok(output)
}

fn app_expression(input: &ResolvedServerInput, version: KitSyncVersion) -> Result<String, String> {
    for tag in ["%sveltekit.head%", "%sveltekit.body%"] {
        if !input.app_template.contains(tag) {
            return Err(format!("App template is missing {tag}"));
        }
    }
    if !version.is_kit3() && !input.options.explicit_environment_variables {
        for name in env_captures(&input.app_template) {
            if !name.starts_with(&input.options.public_prefix) {
                return Err(format!(
                    "Environment variables in app template must start with {} (saw %sveltekit.env.{name}%)",
                    input.options.public_prefix
                ));
            }
        }
    }
    // Preserve the order and first-only head/body behavior of the JS writer.
    let expression = stringify(&input.app_template)?
        .replacen("%sveltekit.head%", "\" + head + \"", 1)
        .replacen("%sveltekit.body%", "\" + body + \"", 1)
        .replace("%sveltekit.assets%", "\" + assets + \"")
        .replace("%sveltekit.nonce%", "\" + nonce + \"");
    substitute_env(&replace_version(&expression, &input.version_name))
}

fn hook_statement(names: &str, hook: Option<&Path>, directory: &Path) -> Result<String, String> {
    hook.map_or_else(
        || Ok(String::new()),
        |path| {
            Ok(format!(
                "({{ {names} }} = await import({}));",
                stringify(&relative(directory, path))?
            ))
        },
    )
}

/// Generate the error-template and server options modules for registered Kit
/// profiles. Dynamic callbacks cannot be represented by this resolved serde
/// contract; hosts must resolve them before invoking the native generator.
///
/// Template error messages are intentionally interpolated without additional
/// escaping, matching Kit: the runtime calling the error template owns escaping.
/// # Errors
/// Rejects unknown versions, incomplete config, unsafe unresolved paths and
/// template/config forms for which the pinned writer cannot emit valid JS.
pub fn generate_server(input: &ResolvedServerInput) -> Result<BTreeMap<PathBuf, String>, String> {
    let version = KitSyncVersion::parse(&input.kit_version)?;
    validate_path(&input.output, "output")?;
    for (name, path) in [
        ("serverHooks", &input.server_hooks),
        ("universalHooks", &input.universal_hooks),
        ("runtimeDirectory", &input.runtime_directory),
    ] {
        if let Some(path) = path {
            validate_path(path, name)?;
        }
    }
    if input.app_template.len() > 16 * 1024 * 1024 || input.error_template.len() > 16 * 1024 * 1024
    {
        return Err("Server template exceeds 16 MiB".into());
    }
    if !input.options.csp.is_object() {
        return Err("Resolved CSP configuration must be an object".into());
    }
    if input
        .options
        .service_worker_options
        .as_ref()
        .is_some_and(|options| !options.is_object())
    {
        return Err("Resolved serviceWorkerOptions must be an object when present".into());
    }
    // The JS writer inserts version text into a JSON-string expression without
    // JS escaping. Reject inputs that would create invalid/injected code instead
    // of silently producing a broken module. `$&`/`$$` retain JS replace semantics.
    if input.app_template.contains("%sveltekit.version%")
        && (input
            .version_name
            .chars()
            .any(|c| matches!(c, '"' | '\\') || c.is_control())
            || input.version_name.contains("$`")
            || input.version_name.contains("$'"))
    {
        return Err("Version interpolation containing quotes, backslashes, control characters or replacement context tokens is unsupported by the pinned Kit writer".into());
    }
    let options = &input.options;
    let app = app_expression(input, version)?;
    let error = format!(
        "export default ({{ status, message }}) => {};",
        stringify(&input.error_template)?
            .replace("%sveltekit.status%", "\" + status + \"")
            .replace("%sveltekit.error.message%", "\" + message + \"")
    );
    let service_worker_options = if options.service_worker_register {
        options
            .service_worker_options
            .as_ref()
            .map(stringify)
            .transpose()?
            .unwrap_or_else(|| "undefined".into())
    } else {
        "null".into()
    };
    let nonce = input.app_template.contains("%sveltekit.nonce%");
    let csp = stringify(&options.csp)?;
    let trusted = stringify(&options.csrf_trusted_origins)?;
    let mut files = BTreeMap::new();
    files.insert(input.output.join("shared/error-template.js"), error);
    if version.is_kit3() {
        let server = hook_statement(
            "handle, handleFetch, handleError, init",
            input.server_hooks.as_deref(),
            &input.output,
        )?;
        let universal = hook_statement(
            "reroute, transport",
            input.universal_hooks.as_deref(),
            &input.output,
        )?;
        files.insert(
            input.output.join("server.js"),
            format!(
                r#"
import error from './shared/error-template.js';

export const options = {{
	app_template_contains_nonce: {nonce},
	csp: {csp},
	csrf_trusted_origins: {trusted},
	service_worker_options: {service_worker_options},
	templates: {{
		app: ({{ head, body, assets, nonce, env }}) => {app},
		error
	}}
}};

export async function get_hooks() {{
	let handle;
	let handleFetch;
	let handleError;
	let init;
	{server}

	let reroute;
	let transport;
	{universal}

	return {{
		handle,
		handleFetch,
		handleError,
		init,
		reroute,
		transport
	}};
}}
"#
            ),
        );
    } else {
        let runtime = input
            .runtime_directory
            .as_ref()
            .ok_or("Kit 2 server generation requires runtimeDirectory")?;
        let directory = input.output.join("server");
        let runtime_import = stringify(&format!(
            "{}/shared-server.js",
            relative(&directory, runtime)
        ))?;
        let server = hook_statement(
            "handle, handleFetch, handleError, handleValidationError, init",
            input.server_hooks.as_deref(),
            &directory,
        )?;
        let universal = hook_statement(
            "reroute, transport",
            input.universal_hooks.as_deref(),
            &directory,
        )?;
        let root_extension = if input.svelte_major >= 5 {
            "js"
        } else {
            "svelte"
        };
        let csrf = options.csrf_check_origin
            && !options
                .csrf_trusted_origins
                .iter()
                .any(|origin| origin == "*");
        let worker = options.service_worker_register && input.service_worker_present;
        let hash = stringify(&version_hash(&input.version_name))?;
        let public = stringify(&options.public_prefix)?;
        let private = stringify(&options.private_prefix)?;
        let preload = stringify(&options.preload_strategy)?;
        let async_compilation = options.async_compilation;
        let embedded = options.embedded;
        let hash_routing = options.hash_routing;
        let boundaries = options.server_error_boundaries;
        files.insert(input.output.join("server/internal.js"), format!(r#"
import root from '../root.{root_extension}';
import {{ set_building, set_prerendering }} from '$app/env/internal';
import {{ set_assets }} from '$app/paths/internal/server';
import {{ set_manifest, set_read_implementation }} from '__sveltekit/server';
import {{ set_private_env, set_public_env }} from {runtime_import};
import error from '../shared/error-template.js';

export const options = {{
	app_template_contains_nonce: {nonce},
	async: {async_compilation},
	csp: {csp},
	csrf_check_origin: {csrf},
	csrf_trusted_origins: {trusted},
	embedded: {embedded},
	env_public_prefix: {public},
	env_private_prefix: {private},
	hash_routing: {hash_routing},
	hooks: null, // added lazily, via `get_hooks`
	preload_strategy: {preload},
	root,
	service_worker: {worker},
	service_worker_options: {service_worker_options},
	server_error_boundaries: {boundaries},
	templates: {{
		app: ({{ head, body, assets, nonce, env }}) => {app},
		error
	}},
	version_hash: {hash}
}};

export async function get_hooks() {{
	let handle;
	let handleFetch;
	let handleError;
	let handleValidationError;
	let init;
	{server}

	let reroute;
	let transport;
	{universal}

	return {{
		handle,
		handleFetch,
		handleError,
		handleValidationError,
		init,
		reroute,
		transport
	}};
}}

export {{ set_assets, set_building, set_manifest, set_prerendering, set_private_env, set_public_env, set_read_implementation }};
"#));
    }
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(version: &str) -> ResolvedServerInput {
        ResolvedServerInput {
            kit_version: version.into(),
            output: "/app/.svelte-kit/generated".into(),
            app_template: "<head>%sveltekit.head%</head><body>%sveltekit.body%</body>".into(),
            error_template: "<h1>%sveltekit.status% %sveltekit.error.message%</h1>".into(),
            version_name: "release-1".into(),
            server_hooks: Some("/app/src/hooks.server.ts".into()),
            universal_hooks: Some("/app/src/hooks.ts".into()),
            runtime_directory: Some("/app/node_modules/@sveltejs/kit/src/runtime".into()),
            svelte_major: 5,
            service_worker_present: true,
            options: ResolvedServerOptions::default(),
        }
    }

    #[test]
    fn kit2_options_imports_and_csrf_wildcard() {
        let mut input = input("2.69.2");
        input.options.csrf_trusted_origins.push("*".into());
        input.options.async_compilation = true;
        let files = generate_server(&input).unwrap();
        let module = &files[&input.output.join("server/internal.js")];
        assert!(module.contains("import root from '../root.js'"));
        assert!(module.contains("csrf_check_origin: false"));
        assert!(module.contains("async: true"));
        assert!(module.contains("service_worker: true"));
        assert!(module.contains("service_worker_options: undefined"));
        assert!(module.contains("import(\"../../../src/hooks.server.ts\")"));
        assert!(module.contains("handleValidationError"));
        assert!(module.contains(&format!("version_hash: {:?}", version_hash("release-1"))));
        input.svelte_major = 4;
        input.options.service_worker_register = false;
        let files = generate_server(&input).unwrap();
        let module = &files[&input.output.join("server/internal.js")];
        assert!(module.contains("root.svelte"));
        assert!(module.contains("service_worker_options: null"));
        assert!(module.contains("service_worker: false"));
    }

    #[test]
    fn kit3_contract_omits_removed_options_and_changes_hook_base() {
        for version in ["3.0.1", "3.0.0-next.32"] {
            let mut input = input(version);
            input.runtime_directory = None;
            let files = generate_server(&input).unwrap();
            let module = &files[&input.output.join("server.js")];
            assert_eq!(files.len(), 2);
            assert!(module.contains("import error from './shared/error-template.js'"));
            assert!(module.contains("import(\"../../src/hooks.server.ts\")"));
            for removed in [
                "version_hash",
                "csrf_check_origin",
                "handleValidationError",
                "env_public_prefix",
                "import root",
                "set_building",
            ] {
                assert!(!module.contains(removed));
            }
        }
    }

    #[test]
    fn template_replacement_order_first_head_and_escaped_version() {
        let mut input = input("2.70.3");
        input.version_name = "a<&😀".into();
        input.app_template = "%sveltekit.head% %sveltekit.head% %sveltekit.body% %sveltekit.assets% %sveltekit.nonce% %sveltekit.version% %sveltekit.env.PUBLIC_NAME%".into();
        let expression = app_expression(&input, KitSyncVersion::Kit2DestructuredExports).unwrap();
        assert_eq!(
            expression,
            "\"\" + head + \" %sveltekit.head% \" + body + \" \" + assets + \" \" + nonce + \" a&lt;&amp;😀 \" + (env[\"PUBLIC_NAME\"] ?? \"\") + \"\""
        );
        let files = generate_server(&input).unwrap();
        assert_eq!(
            files[&input.output.join("shared/error-template.js")],
            "export default ({ status, message }) => \"<h1>\" + status + \" \" + message + \"</h1>\";"
        );
    }

    #[test]
    fn template_env_privacy_and_unresolved_contract_errors() {
        let mut input = input("2.69.2");
        input.app_template.push_str("%sveltekit.env.SECRET%");
        assert!(generate_server(&input).unwrap_err().contains("PUBLIC_"));
        input.options.explicit_environment_variables = true;
        assert!(generate_server(&input).is_ok());
        input.kit_version = "3.0.1".into();
        input.options.explicit_environment_variables = false;
        assert!(generate_server(&input).is_ok());
        input.app_template = "no required tags".into();
        assert!(generate_server(&input).unwrap_err().contains("missing"));
        input.kit_version = "4.0.0".into();
        assert!(generate_server(&input).unwrap_err().contains("Unsupported"));
    }

    #[test]
    fn js_replacement_dollars_and_utf16_hash_contract() {
        assert_eq!(version_hash(""), "45h");
        assert_eq!(version_hash("a"), "3t1g");
        // A surrogate pair is visited low-unit first, matching charCodeAt(--i).
        let mut expected = 5381_u32;
        for unit in [0xde00_u32, 0xd83d] {
            expected = expected.wrapping_mul(33) ^ unit;
        }
        let mut value = 0_u32;
        for digit in version_hash("😀").chars() {
            value = value * 36 + digit.to_digit(36).unwrap();
        }
        assert_eq!(value, expected);
        assert_eq!(
            replace_version("[%sveltekit.version%]", "$$-$&"),
            "[$-%sveltekit.version%amp;]"
        );
        assert_eq!(
            substitute_env("\"%sveltekit.env.%%sveltekit.env.PUBLIC_X%\"").unwrap(),
            "\"%sveltekit.env.%\" + (env[\"PUBLIC_X\"] ?? \"\") + \"\""
        );
    }
}
