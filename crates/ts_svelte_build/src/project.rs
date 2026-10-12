//! Native project loader: turns an installed SvelteKit 2 project directory into
//! a `BuildRequest` without executing `svelte.config.js` or `vite.config.*`.
//! Both configs are read as static trees (`sync_config::static_config_tree`).
//! Values that need execution come from explicit `ProjectInput` fields or are
//! reported as requirements; nothing is guessed.
//!
//! Sources: pinned Kit 2.69.2 `exports/vite/index.js` (config hook, templates,
//! env, aliases), `core/config/options.js` (defaults) and Vite 7.3.1 config
//! resolution. Application inputs are read only; nothing is written.
use crate::Requirement;
use base64::Engine as _;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use ts_svelte_kit::{
    build_env::ResolvedBuildEnvironment,
    build_inventory::{BuildInventoryOptions, discover_build_inventory},
    build_manifest::{ClientRouterConfig, RouterResolution, RouterType},
    build_nodes::BundleStrategy,
    build_plan::{ResolvedBuildPlanConfig, ResolvedConfigAlias, ResolvedInstrumentation},
    build_server::{ResolvedServerInput, ResolvedServerOptions},
    sync_config::{self, SyncConfig},
    vite_env,
};

const ADAPTER_NODE: &str = "@sveltejs/adapter-node";
const ADAPTER_NODE_FILES: [&str; 4] = ["env.js", "handler.js", "index.js", "shims.js"];
const MAX_STATIC_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProjectInput {
    /// Absolute project root containing svelte.config.js and node_modules.
    pub root: PathBuf,
    #[serde(default = "production")]
    pub mode: String,
    /// The process environment Vite would see. Kit's build values include all
    /// of it (`loadEnv(mode, dir, '')`), so it is required and explicit.
    pub environment: BTreeMap<String, String>,
    /// `kit.version.name`. Kit's default is `Date.now().toString()`; omitting it
    /// uses the current time the same way, which makes builds differ.
    #[serde(default)]
    pub version_name: Option<String>,
    /// Vite `define` entries (identifier -> JavaScript expression source) for
    /// values the static Vite config reader cannot evaluate.
    #[serde(default)]
    pub define: BTreeMap<String, String>,
}
fn production() -> String {
    "production".into()
}

pub struct LoadedProject {
    pub request: Value,
    pub requirements: Vec<Requirement>,
}

struct Loader {
    root: PathBuf,
    requirements: Vec<Requirement>,
}
impl Loader {
    fn require(&mut self, stage: &str, code: &str, message: impl Into<String>) {
        self.requirements.push(Requirement {
            stage: stage.into(),
            code: code.into(),
            message: message.into(),
        });
    }
    fn package_version(&self, package: &str) -> Result<String, String> {
        let file = self
            .root
            .join("node_modules")
            .join(package)
            .join("package.json");
        let value: Value = serde_json::from_slice(
            &std::fs::read(&file).map_err(|e| format!("{}: {e}", file.display()))?,
        )
        .map_err(|e| format!("{}: {e}", file.display()))?;
        value["version"]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| format!("{} has no version", file.display()))
    }
}

/// A plain static object from the config tree, without bookkeeping keys.
fn object<'a>(
    value: Option<&'a Value>,
    label: &str,
) -> Result<Option<&'a Map<String, Value>>, String> {
    match value {
        None => Ok(None),
        Some(Value::Object(map))
            if !map.contains_key("$nativeDynamic")
                && !map.contains_key("$call")
                && !map.contains_key("$nativeDynamicSpread") =>
        {
            Ok(Some(map))
        }
        Some(_) => Err(format!("{label} must be a static object")),
    }
}
fn plain(value: &Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.iter()
                .filter(|(key, _)| *key != "$keys")
                .map(|(key, value)| (key.clone(), plain(value)))
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(plain).collect()),
        other => other.clone(),
    }
}
/// Property names in source order (the tree's `$keys`), excluding bookkeeping.
fn ordered_keys(map: &Map<String, Value>) -> Vec<String> {
    match map.get("$keys") {
        Some(Value::Array(keys)) => keys
            .iter()
            .filter_map(|key| key.as_str().map(str::to_owned))
            .collect(),
        _ => map.keys().filter(|key| *key != "$keys").cloned().collect(),
    }
}
fn is_dynamic(value: &Value) -> bool {
    value.get("$nativeDynamic").is_some()
}
fn call<'a>(value: &'a Value) -> Option<(&'a str, &'a str, &'a [Value])> {
    let call = value.get("$call")?;
    Some((
        call["module"].as_str()?,
        call["export"].as_str()?,
        call["arguments"].as_array()?.as_slice(),
    ))
}
fn bool_at(map: Option<&Map<String, Value>>, key: &str, default: bool) -> Result<bool, String> {
    match map.and_then(|map| map.get(key)) {
        None => Ok(default),
        Some(Value::Bool(value)) => Ok(*value),
        Some(_) => Err(format!("{key} must be a static boolean")),
    }
}
/// Node `path.resolve(root, value)` for a project-relative or absolute path.
fn resolve(root: &Path, value: &str) -> PathBuf {
    let mut out = PathBuf::new();
    for component in root.join(value).components() {
        match component {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}
fn read_text(path: &Path) -> Result<String, String> {
    std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))
}
/// The first existing `base.<ext>` for Kit's module extensions.
fn with_extension(base: &Path, extensions: &[String]) -> Option<PathBuf> {
    extensions.iter().find_map(|extension| {
        let candidate = PathBuf::from(format!("{}{extension}", base.display()));
        candidate.is_file().then_some(candidate)
    })
}

pub fn load(input: &ProjectInput) -> Result<LoadedProject, String> {
    if !input.root.is_absolute() {
        return Err("Project root must be absolute".into());
    }
    let root = input
        .root
        .canonicalize()
        .map_err(|e| format!("{}: {e}", input.root.display()))?;
    let mut loader = Loader {
        root: root.clone(),
        requirements: vec![],
    };
    let kit_version = sync_config::installed_kit_version(&root)?;
    if !kit_version.starts_with("2.") {
        return Err(format!(
            "The native project loader currently reads Kit 2 configs; installed Kit is {kit_version}"
        ));
    }
    let svelte_version = loader.package_version("svelte")?;
    let svelte_major: u32 = svelte_version
        .split('.')
        .next()
        .and_then(|major| major.parse().ok())
        .ok_or("Invalid installed Svelte version")?;

    // svelte.config.js: the sync subset plus the static tree for build fields.
    let config_file = ["svelte.config.js", "svelte.config.mjs", "svelte.config.ts"]
        .into_iter()
        .map(|name| root.join(name))
        .find(|path| path.is_file())
        .ok_or("No svelte.config.js")?;
    let sync: SyncConfig = sync_config::load(&root, None)?;
    let tree = sync_config::static_config_tree(&read_text(&config_file)?)?;
    let top = object(Some(&tree), "Svelte config")?.ok_or("Svelte config")?;
    let kit = object(top.get("kit"), "kit")?;
    let experimental = object(
        kit.and_then(|kit| kit.get("experimental")),
        "kit.experimental",
    )?;
    let known_top = [
        "extensions",
        "preprocess",
        "compilerOptions",
        "kit",
        "$keys",
    ];
    for key in top.keys().filter(|key| !known_top.contains(&key.as_str())) {
        loader.require(
            "config",
            "svelte_config_field",
            format!("Unsupported svelte.config field {key}"),
        );
    }
    let known_kit = [
        "adapter",
        "alias",
        "experimental",
        "files",
        "env",
        "outDir",
        "moduleExtensions",
        "$keys",
    ];
    for key in kit
        .into_iter()
        .flat_map(|kit| kit.keys())
        .filter(|key| !known_kit.contains(&key.as_str()))
    {
        loader.require(
            "config",
            "kit_config_field",
            format!("Unsupported kit config field kit.{key}"),
        );
    }
    let known_experimental = [
        "tracing",
        "instrumentation",
        "explicitEnvironmentVariables",
        "$keys",
    ];
    for key in experimental
        .into_iter()
        .flat_map(|map| map.keys())
        .filter(|key| !known_experimental.contains(&key.as_str()))
    {
        loader.require(
            "config",
            "kit_experimental_field",
            format!("Unsupported kit.experimental.{key}"),
        );
    }

    // compilerOptions: runes is a dedicated setting; the rest is unported.
    let compiler_options = object(top.get("compilerOptions"), "compilerOptions")?;
    let runes = match compiler_options.and_then(|map| map.get("runes")) {
        None | Some(Value::Null) => None,
        Some(Value::Bool(value)) => Some(*value),
        Some(_) => return Err("compilerOptions.runes must be a static boolean".into()),
    };
    for key in compiler_options
        .into_iter()
        .flat_map(|map| map.keys())
        .filter(|key| !["runes", "$keys"].contains(&key.as_str()))
    {
        loader.require(
            "config",
            "compiler_option",
            format!("Unsupported compilerOptions.{key}"),
        );
    }

    // preprocess: vitePreprocess() (TypeScript, handled natively) and mdsvex.
    let mut mdsvex = None;
    let preprocessors = match top.get("preprocess") {
        None => vec![],
        Some(Value::Array(items)) => items.clone(),
        Some(single) => vec![single.clone()],
    };
    for preprocessor in &preprocessors {
        match call(preprocessor) {
            Some(("@sveltejs/vite-plugin-svelte", "vitePreprocess", arguments))
                if arguments.is_empty() => {}
            Some(("mdsvex", "mdsvex", arguments)) if arguments.len() <= 1 => {
                let options = arguments.first().map(plain).unwrap_or_else(|| json!({}));
                if options.to_string().contains("$nativeDynamic") {
                    loader.require("config", "mdsvex_options", "mdsvex options must be static");
                }
                mdsvex = Some(options);
            }
            _ => loader.require(
                "config",
                "preprocessor",
                format!("Unsupported preprocessor {preprocessor}"),
            ),
        }
    }

    // adapter
    let adapter = kit.and_then(|kit| kit.get("adapter"));
    let (adapter_name, adapter_options) = match adapter.map(|value| (value, call(value))) {
        None => (None, None),
        Some((_, Some((module, "default", arguments))))
            if module == ADAPTER_NODE && arguments.len() <= 1 =>
        {
            (
                Some(ADAPTER_NODE.to_owned()),
                Some(arguments.first().map(plain).unwrap_or_else(|| json!({}))),
            )
        }
        Some((value, _)) => {
            loader.require(
                "adapter",
                "native_adapter",
                format!("Unsupported adapter {value}"),
            );
            (None, None)
        }
    };

    // experimental instrumentation/tracing
    let tracing = object(
        experimental.and_then(|map| map.get("tracing")),
        "kit.experimental.tracing",
    )?;
    let tracing_server = bool_at(tracing, "server", false)?;
    let instrumentation = object(
        experimental.and_then(|map| map.get("instrumentation")),
        "kit.experimental.instrumentation",
    )?;
    let instrumentation = if bool_at(instrumentation, "server", false)? {
        // Kit: path.join(config.kit.files.src, 'instrumentation.server') with a
        // module extension; adapter-node 5.5.4 supports it.
        let entry = with_extension(&root.join(&sync.src).join("instrumentation.server"), &sync.module_extensions)
            .ok_or("kit.experimental.instrumentation.server is enabled but no instrumentation.server module exists")?;
        Some(ResolvedInstrumentation {
            entry,
            enabled: true,
            adapter_supports: adapter_name.as_ref().map(|_| true),
        })
    } else {
        None
    };

    // Inventory
    let options = BuildInventoryOptions {
        svelte_major,
        router: ClientRouterConfig {
            resolution: RouterResolution::Client,
            router_type: RouterType::Pathname,
        },
        hooks: None,
        handle_rendering_errors: false,
        async_compiler: false,
        dev: false,
    };
    let inventory = discover_build_inventory(&root, &sync, &options)?;

    // Templates: kit.files.appTemplate / errorTemplate default to src/*.html.
    let app_template = read_text(&root.join(&sync.src).join("app.html"))?;
    for tag in ["%sveltekit.head%", "%sveltekit.body%"] {
        if !app_template.contains(tag) {
            return Err(format!("src/app.html is missing {tag}"));
        }
    }
    let error_file = root.join(&sync.src).join("error.html");
    let error_template = if error_file.is_file() {
        read_text(&error_file)?
    } else {
        read_text(&root.join("node_modules/@sveltejs/kit/src/core/config/default-error.html"))?
    };
    let version_name = match &input.version_name {
        Some(name) => name.clone(),
        None => std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_millis()
            .to_string(),
    };

    let server = ResolvedServerInput {
        kit_version: kit_version.clone(),
        output: inventory.generated_directory.clone(),
        app_template,
        error_template,
        version_name: version_name.clone(),
        server_hooks: inventory.hooks.server.clone(),
        universal_hooks: inventory.hooks.universal.clone(),
        runtime_directory: Some(inventory.runtime_directory.clone()),
        svelte_major,
        service_worker_present: inventory.service_worker.is_some(),
        options: ResolvedServerOptions {
            public_prefix: sync.public_prefix.clone(),
            private_prefix: sync.private_prefix.clone(),
            explicit_environment_variables: sync.explicit_environment_variables,
            ..ResolvedServerOptions::default()
        },
    };
    let build_values =
        vite_env::kit_build_env(&root, &sync.env_dir, &input.mode, &input.environment)?;
    let environment = ResolvedBuildEnvironment {
        kit_version: kit_version.clone(),
        environment_directory: inventory.generated_directory.join("env"),
        runtime_directory: inventory.runtime_directory.clone(),
        version_name,
        base: String::new(),
        app_dir: "_app".into(),
        build_values,
        public_prefix: sync.public_prefix.clone(),
        private_prefix: sync.private_prefix.clone(),
        explicit_environment_variables: sync.explicit_environment_variables,
        definition: None,
    };
    if sync.explicit_environment_variables {
        loader.require(
            "environment",
            "explicit_environment_definition",
            "kit.experimental.explicitEnvironmentVariables needs the evaluated env definition",
        );
    }

    // Kit aliases in config order: path.resolve(cwd, value) with a trailing /* removed.
    let mut aliases = vec![];
    if let Some(alias) = object(kit.and_then(|kit| kit.get("alias")), "kit.alias")? {
        for key in ordered_keys(alias) {
            let value = alias[&key]
                .as_str()
                .ok_or_else(|| format!("kit.alias.{key} must be a static string"))?;
            let value = value.strip_suffix("/*").unwrap_or(value);
            aliases.push(ResolvedConfigAlias {
                name: key,
                path: resolve(&root, value),
            });
        }
    }
    let vite = vite_config(&mut loader, &root, input)?;
    let kit_alias_names: BTreeSet<String> = aliases
        .iter()
        .map(|alias| alias.name.trim_end_matches("/*").to_owned())
        .chain(["$lib".into(), "$app".into(), "$env".into()])
        .collect();
    for (name, path) in &vite.aliases {
        // Kit's aliases are merged ahead of the user's and win on the same name.
        if !kit_alias_names.contains(name) {
            aliases.push(ResolvedConfigAlias {
                name: name.clone(),
                path: path.clone(),
            });
        }
    }

    let config = ResolvedBuildPlanConfig {
        out_dir: root.join(&sync.out_dir),
        routes_directory: root.join(&sync.routes),
        lib_directory: root.join(&sync.lib),
        assets_directory: root.join(&sync.assets),
        aliases,
        bundle_strategy: BundleStrategy::Split,
        mode: input.mode.clone(),
        assets_url: String::new(),
        paths_relative: true,
        paths_origin: None,
        fork_preloads: false,
        tracing_server,
        link_header_preload: false,
        version_poll_interval: 0,
        adapter_name: adapter_name.clone(),
        instrumentation,
        remote_functions_enabled: false,
        remote_modules: vec![],
        server_conditions: vec!["svelte".into(), "node".into(), "production".into()],
        client_conditions: vec!["svelte".into(), "browser".into(), "production".into()],
        server_externals: vite.ssr_external.clone(),
    };

    // Module snapshots for native analysis: every universal/server node module,
    // endpoint and hook. Imports stay unresolved until the bundle graph fills them.
    let mut modules = BTreeMap::new();
    let mut module_files: BTreeSet<PathBuf> = BTreeSet::new();
    for node in &inventory.nodes {
        module_files.extend(node.universal.iter().cloned());
        module_files.extend(node.server.iter().cloned());
    }
    for route in &inventory.routes {
        module_files.extend(route.endpoint.iter().cloned());
    }
    for file in module_files {
        modules.insert(
            file.to_string_lossy().into_owned(),
            json!({"source": read_text(&file)?, "imports": {}}),
        );
    }

    // Static assets: Kit copies kit.files.assets verbatim.
    let mut static_assets = BTreeMap::new();
    let assets = root.join(&sync.assets);
    let mut total = 0;
    if assets.is_dir() {
        let mut stack = vec![assets.clone()];
        while let Some(directory) = stack.pop() {
            let mut entries = std::fs::read_dir(&directory)
                .map_err(|e| format!("{}: {e}", directory.display()))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| e.to_string())?;
            entries.sort_by_key(|entry| entry.file_name());
            for entry in entries {
                let path = entry.path();
                let metadata =
                    std::fs::metadata(&path).map_err(|e| format!("{}: {e}", path.display()))?;
                if metadata.is_dir() {
                    stack.push(path);
                    continue;
                }
                total += metadata.len();
                if total > MAX_STATIC_BYTES {
                    return Err("Static assets exceed the 64 MiB native build limit".into());
                }
                let relative = path
                    .strip_prefix(&assets)
                    .map_err(|e| e.to_string())?
                    .to_string_lossy()
                    .replace('\\', "/");
                let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
                static_assets.insert(
                    relative,
                    json!({"source": base64::engine::general_purpose::STANDARD.encode(bytes), "mimeType": null}),
                );
            }
        }
    }

    // adapter-node templates from the installed package.
    let adapter_node = if adapter_name.is_some() {
        let directory = root.join("node_modules").join(ADAPTER_NODE);
        let version = loader.package_version(ADAPTER_NODE)?;
        let mut files = BTreeMap::new();
        for name in ADAPTER_NODE_FILES {
            files.insert(
                name.to_owned(),
                read_text(&directory.join("files").join(name))?,
            );
        }
        let package_json: Value = serde_json::from_str(&read_text(&root.join("package.json"))?)
            .map_err(|e| format!("package.json: {e}"))?;
        Some(json!({
            "templates": {"version": version, "files": files},
            "options": adapter_options.unwrap_or_else(|| json!({})),
            "packageJson": package_json,
            "serverMapBase": root.join("dist"),
        }))
    } else {
        None
    };

    let request = json!({
        "adapterNode": adapter_node,
        "kit": {
            "inventory": inventory,
            "server": server,
            "environment": environment,
            "config": config,
            "clientAnalysis": null,
        },
        "compiler": {
            "svelteVersion": svelte_version,
            "typescriptVersion": null,
            "runes": runes,
            "minify": false,
            "sourcemap": true,
            "tsconfig": null,
            "mdsvex": mdsvex,
            "typescript": {},
        },
        "moduleExtensions": sync.module_extensions,
        "modules": modules,
        "staticAssets": static_assets,
        "staticAssetsComplete": true,
        "define": vite.define,
    });
    Ok(LoadedProject {
        request,
        requirements: loader.requirements,
    })
}

struct ViteProject {
    aliases: Vec<(String, PathBuf)>,
    ssr_external: Vec<String>,
    define: BTreeMap<String, String>,
}

/// Static reading of the fields of `vite.config.*` that change the build.
fn vite_config(
    loader: &mut Loader,
    root: &Path,
    input: &ProjectInput,
) -> Result<ViteProject, String> {
    let mut project = ViteProject {
        aliases: vec![],
        ssr_external: vec![],
        define: input.define.clone(),
    };
    let Some(file) = [
        "vite.config.ts",
        "vite.config.js",
        "vite.config.mjs",
        "vite.config.mts",
    ]
    .into_iter()
    .map(|name| root.join(name))
    .find(|path| path.is_file()) else {
        return Ok(project);
    };
    let tree = match sync_config::static_config_tree(&read_text(&file)?) {
        Ok(tree) => tree,
        Err(error) => {
            loader.require(
                "vite",
                "vite_config",
                format!("Cannot read vite config statically: {error}"),
            );
            return Ok(project);
        }
    };
    let config = match call(&tree) {
        Some(("vite", "defineConfig", [config])) => config.clone(),
        _ if tree.is_object() && !is_dynamic(&tree) => tree,
        _ => {
            loader.require(
                "vite",
                "vite_config",
                "vite config is not a static object or defineConfig({...})",
            );
            return Ok(project);
        }
    };
    let config = object(Some(&config), "vite config")?.ok_or("vite config")?;

    // resolve.alias as an object of find -> path.resolve(...) or string.
    if let Some(resolve_config) = object(config.get("resolve"), "vite resolve")? {
        if let Some(alias) = resolve_config.get("alias") {
            let alias = object(Some(alias), "vite resolve.alias")?.ok_or("vite resolve.alias")?;
            for key in ordered_keys(alias) {
                let value = &alias[&key];
                let path = match (value.as_str(), call(value)) {
                    (Some(text), _) => Some(PathBuf::from(text)),
                    (None, Some(("path", "resolve", arguments))) => {
                        let mut path = root.to_path_buf();
                        let mut ok = true;
                        for argument in arguments {
                            match argument.as_str() {
                                Some(part) => path = resolve(&path, part),
                                None => ok = false,
                            }
                        }
                        ok.then_some(path)
                    }
                    _ => None,
                };
                match path {
                    Some(path) => project.aliases.push((key, path)),
                    None => loader.require(
                        "vite",
                        "resolve_alias",
                        format!("Dynamic resolve.alias entry {key}"),
                    ),
                }
            }
        }
    }
    // ssr.external: exact package names in this config.
    if let Some(ssr) = object(config.get("ssr"), "vite ssr")? {
        if let Some(external) = ssr.get("external") {
            match external {
                Value::Array(items) => {
                    for item in items {
                        match item.as_str() {
                            Some(name) => project.ssr_external.push(name.into()),
                            None => {
                                loader.require("vite", "ssr_external", "Dynamic ssr.external entry")
                            }
                        }
                    }
                }
                Value::Bool(true) => {
                    loader.require("vite", "ssr_external", "ssr.external: true is not ported")
                }
                _ => loader.require("vite", "ssr_external", "Dynamic ssr.external"),
            }
        }
        if let Some(Value::Array(items)) = ssr.get("noExternal") {
            if !items.is_empty() {
                loader.require(
                    "vite",
                    "ssr_no_external",
                    "ssr.noExternal entries are not ported",
                );
            }
        }
    }
    // define: static string values are JS expression sources; others must be explicit.
    if let Some(define) = object(config.get("define"), "vite define")? {
        for key in ordered_keys(define) {
            if project.define.contains_key(&key) {
                continue;
            }
            match &define[&key] {
                Value::String(source) => {
                    project.define.insert(key, source.clone());
                }
                _ => loader.require(
                    "vite",
                    "define",
                    format!("define.{key} needs execution; supply it in ProjectInput.define"),
                ),
            }
        }
    }
    if let Some(Value::Array(plugins)) = config.get("plugins") {
        for plugin in plugins {
            match call(plugin) {
                Some(("@sveltejs/kit/vite", "sveltekit", [])) | Some(("@tailwindcss/vite", "default", [])) => {}
                Some((module, export, _)) => loader.require(
                    "vite",
                    "plugin",
                    format!("Vite plugin {module}#{export} has no native equivalent"),
                ),
                None => loader.require(
                    "vite",
                    "plugin",
                    "Local Vite plugin has no native equivalent (development-only plugins can be ignored)",
                ),
            }
        }
    }
    for (key, label) in [("build", "build options"), ("esbuild", "esbuild options")] {
        if config.contains_key(key) {
            loader.require("vite", key, format!("Vite {label} are not ported"));
        }
    }
    Ok(project)
}

#[cfg(test)]
#[path = "project_tests.rs"]
mod tests;
