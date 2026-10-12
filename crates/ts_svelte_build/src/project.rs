//! Native project loader: turns an installed SvelteKit 2 or 3 project directory
//! into a `BuildRequest` without executing `svelte.config.js` or `vite.config.*`.
//! Configs are read as static trees (`sync_config::static_config_tree`); Kit 3's
//! options are the argument of the `sveltekit(...)` Vite plugin call.
//! Values that need execution come from explicit `ProjectInput` fields or are
//! reported as requirements; nothing is guessed.
//!
//! Sources: pinned Kit 2.69.2 and 3.0.1 `exports/vite/index.js` (config hook,
//! templates, env, aliases), `core/config/options.js` (defaults), Vite 7.3.1
//! config resolution and adapter-node 6.0.0's Vite plugin. Application inputs are read only; nothing is written.
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
    vite_env, vite_ssr,
};

const ADAPTER_NODE: &str = "@sveltejs/adapter-node";
/// adapter-node 6.0.0 (Kit 3): the files its Vite plugin adds to the SSR
/// build and its `adapt` module, whose packaging is ported natively.
const ADAPTER_NODE_KIT3_VERSION: &str = "6.0.0";
const ADAPTER_NODE_KIT3_FILES: [(&str, &str); 6] = [
    (
        "index.js",
        "0077781f631c2714c6eacc822d4c47e7e529067f7509598f95d8f7d774e85fc9",
    ),
    (
        "src/env.js",
        "72d4998e23273730a7da7c9374e3f9a15ce6d11ecb985cc89ac91a7db67819d2",
    ),
    (
        "src/handler.js",
        "d8033fcc70fda60761e2844f2de4cdc2fa17d6704658847f7143ff7484daaa92",
    ),
    (
        "src/index.js",
        "afc3643455f0aa6f1353699ad11df2a5fae2cdb3469676020b291cf7bcfc0920",
    ),
    (
        "src/static.js",
        "40581957725dec58b26f6b53376210849bfcda87abab809885a3b9c9d9a3e4f7",
    ),
    (
        "src/utils.js",
        "3ca785e4545caab2a828b231cfa691611a7ace10b568269be8c3f5a05f67198b",
    ),
];
/// Kit's MIME lookup is `mrmime`; this version's database is read as data.
const MRMIME_VERSION: &str = "2.0.1";
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
fn call(value: &Value) -> Option<(&str, &str, &[Value])> {
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
    let kit3 = kit_version.starts_with("3.");
    if !kit3 && !kit_version.starts_with("2.") {
        return Err(format!(
            "The native project loader reads Kit 2 and Kit 3 configs; installed Kit is {kit_version}"
        ));
    }
    let svelte_version = loader.package_version("svelte")?;
    let svelte_major: u32 = svelte_version
        .split('.')
        .next()
        .and_then(|major| major.parse().ok())
        .ok_or("Invalid installed Svelte version")?;

    // Kit 2: svelte.config.js, Kit options under `kit`. Kit 3: the flat options
    // of the `sveltekit(...)` Vite plugin (Kit 3.0.1 `split_config`), where
    // any option Kit does not own is forwarded to vite-plugin-svelte.
    let sync: SyncConfig = sync_config::load(&root, None)?;
    let tree = if kit3 {
        sync_config::kit3_plugin_options(&root)?
    } else {
        let config_file = ["svelte.config.js", "svelte.config.mjs", "svelte.config.ts"]
            .into_iter()
            .map(|name| root.join(name))
            .find(|path| path.is_file())
            .ok_or("No svelte.config.js")?;
        sync_config::static_config_tree(&read_text(&config_file)?)?
    };
    let top = object(Some(&tree), "Svelte config")?.ok_or("Svelte config")?;
    let kit = if kit3 {
        Some(top)
    } else {
        object(top.get("kit"), "kit")?
    };
    let experimental = object(
        kit.and_then(|kit| kit.get("experimental")),
        "kit.experimental",
    )?;
    let known_top: &[&str] = if kit3 {
        &[
            "extensions",
            "preprocess",
            "compilerOptions",
            "adapter",
            "alias",
            "experimental",
            "files",
            "env",
            "outDir",
            "moduleExtensions",
            "tracing",
            "$keys",
        ]
    } else {
        &[
            "extensions",
            "preprocess",
            "compilerOptions",
            "kit",
            "$keys",
        ]
    };
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
        .filter(|_| !kit3)
        .flat_map(|kit| kit.keys())
        .filter(|key| !known_kit.contains(&key.as_str()))
    {
        loader.require(
            "config",
            "kit_config_field",
            format!("Unsupported kit config field kit.{key}"),
        );
    }
    // Kit 3 moved tracing out of `experimental` and enables instrumentation by
    // file presence; its remaining flags (remoteFunctions, forkPreloads) and
    // vite-plugin-svelte's are not ported.
    let known_experimental: &[&str] = if kit3 {
        &["$keys"]
    } else {
        &[
            "tracing",
            "instrumentation",
            "explicitEnvironmentVariables",
            "$keys",
        ]
    };
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
    let mut runes_project_only = false;
    let runes = match compiler_options.and_then(|map| map.get("runes")) {
        None | Some(Value::Null) => None,
        Some(Value::Bool(value)) => Some(*value),
        Some(value) if kit3 && runes_template_callback(&root, value)? => {
            runes_project_only = true;
            Some(true)
        }
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
            Some(("@sveltejs/vite-plugin-svelte", "vitePreprocess", [])) => {}
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

    // Kit 3 uses adapter-node 6, whose server entries join the Vite SSR build;
    // that packaging is ported for 6.0.0 exactly (files verified below).
    let adapter_node_kit3_version = if kit3 && adapter_name.is_some() {
        Some(loader.package_version(ADAPTER_NODE)?)
    } else {
        None
    };
    if let Some(version) = &adapter_node_kit3_version
        && version != ADAPTER_NODE_KIT3_VERSION
    {
        loader.require(
            "adapter",
            "native_adapter",
            format!("Native Kit 3 adapter-node packaging is {ADAPTER_NODE_KIT3_VERSION}; installed {version}"),
        );
    }

    // instrumentation/tracing: Kit 2 flags under `experimental`; Kit 3 reads
    // `tracing.server` and uses src/instrumentation.server whenever it exists.
    let tracing = object(
        if kit3 {
            top.get("tracing")
        } else {
            experimental.and_then(|map| map.get("tracing"))
        },
        "tracing",
    )?;
    let tracing_server = bool_at(tracing, "server", false)?;
    let instrumentation = object(
        experimental.and_then(|map| map.get("instrumentation")),
        "kit.experimental.instrumentation",
    )?;
    let instrumentation_entry = with_extension(
        &root.join(&sync.src).join("instrumentation.server"),
        &sync.module_extensions,
    );
    if kit3 && instrumentation_entry.is_some() && adapter_name.is_some() {
        loader.require(
            "adapter",
            "instrumentation",
            "adapter-node 6 `builder.instrument` packaging is not ported",
        );
    }
    let instrumentation = if kit3 && let Some(entry) = instrumentation_entry {
        Some(ResolvedInstrumentation {
            entry,
            enabled: true,
            adapter_supports: adapter_name.as_ref().map(|_| true),
        })
    } else if !kit3 && bool_at(instrumentation, "server", false)? {
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
    // Kit 3 always derives env modules from src/env when it exists.
    let kit3_env = kit3
        && with_extension(&root.join(&sync.src).join("env"), &sync.module_extensions).is_some();
    if sync.explicit_environment_variables || kit3_env {
        loader.require(
            "environment",
            "explicit_environment_definition",
            "Explicit environment variables (src/env or kit.experimental.explicitEnvironmentVariables) need the evaluated env definition",
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
    let vite = vite_config(&mut loader, &root, input, kit3)?;
    // Kit 3 removed `$lib` (projects use package.json `#` imports).
    let kit_alias_names: BTreeSet<String> = aliases
        .iter()
        .map(|alias| alias.name.trim_end_matches("/*").to_owned())
        .chain(["$app".into(), "$env".into()])
        .chain((!kit3).then(|| "$lib".to_owned()))
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

    let sentry = match &vite.sentry {
        Some(options) => Some(sentry_options(
            &mut loader,
            options,
            tracing_server,
            adapter_name.as_deref(),
            adapter_options_out(&adapter_options),
        )?),
        None => None,
    };
    let tailwind = if vite.tailwind {
        Some(tailwind_options(&mut loader, &root, &root.join(&sync.src))?)
    } else {
        None
    };

    // Vite's own loadEnv: envDir defaults to the root, envPrefix to VITE_. It
    // runs after Kit's (which may have set VITE_USER_NODE_ENV and friends).
    let mut vite_process = input.environment.clone();
    if vite_process.get("NODE_ENV").is_none_or(String::is_empty) {
        vite_process.insert("NODE_ENV".into(), "production".into());
    }
    let _ = vite_env::load_env(
        &input.mode,
        &root.join(&sync.env_dir),
        &[""],
        &mut vite_process,
    )?;
    let vite_values = vite_env::load_env(&input.mode, &root, &["VITE_"], &mut vite_process)?;
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
        // Vite's externalization (below) covers configured ssr.external too.
        server_externals: vec![],
        vite_env: vite_values,
    };
    let package_json: Value = serde_json::from_str(&read_text(&root.join("package.json"))?)
        .map_err(|e| format!("package.json: {e}"))?;
    let ssr_external = if adapter_node_kit3_version.is_some() {
        if !vite.ssr_external.is_empty() {
            loader.require(
                "vite",
                "ssr_external",
                "ssr.external with adapter-node 6 is not ported",
            );
        }
        vite_ssr::kit3_adapter_node_policy(&root, &package_json)?
    } else {
        vite_ssr::kit2_server_policy(&root, &vite.ssr_external, &[])?
    };
    let mime_types = mime_table(&root, kit3)?;
    let mime_of = |file: &str| {
        let extension = file.rsplit('.').next().unwrap_or(file).to_lowercase();
        mime_types.get(&format!(".{extension}")).cloned()
    };

    // Module snapshots for native analysis: every universal/server node module
    // and endpoint, plus the project-local modules they import at runtime.
    // Resolutions are checked against the actual server graph by the build.
    let mut module_files: BTreeSet<PathBuf> = BTreeSet::new();
    for node in &inventory.nodes {
        module_files.extend(node.universal.iter().cloned());
        module_files.extend(node.server.iter().cloned());
    }
    for route in &inventory.routes {
        module_files.extend(route.endpoint.iter().cloned());
    }
    let resolver = LocalResolver::new(&root, &config, kit3)?;
    let modules = local_module_graph(&resolver, module_files)?;

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
                let mime_type = mime_of(&relative);
                static_assets.insert(
                    relative,
                    json!({"source": base64::engine::general_purpose::STANDARD.encode(bytes), "mimeType": mime_type}),
                );
            }
        }
    }

    // adapter-node templates from the installed package (5.5.4, Kit 2).
    let adapter_node = if adapter_name.is_some() && !kit3 {
        let directory = root.join("node_modules").join(ADAPTER_NODE);
        let version = loader.package_version(ADAPTER_NODE)?;
        let mut files = BTreeMap::new();
        for name in ADAPTER_NODE_FILES {
            files.insert(
                name.to_owned(),
                read_text(&directory.join("files").join(name))?,
            );
        }
        Some(json!({
            "templates": {"version": version, "files": files},
            "options": adapter_options.clone().unwrap_or_else(|| json!({})),
            "packageJson": package_json,
            "serverMapBase": root.join("dist"),
        }))
    } else {
        None
    };
    let adapter_node_kit3 = match &adapter_node_kit3_version {
        Some(version) if version == ADAPTER_NODE_KIT3_VERSION => {
            let directory = root
                .join("node_modules")
                .join(ADAPTER_NODE)
                .canonicalize()
                .map_err(|e| format!("{ADAPTER_NODE}: {e}"))?;
            for (file, expected) in ADAPTER_NODE_KIT3_FILES {
                let bytes = std::fs::read(directory.join(file))
                    .map_err(|e| format!("{ADAPTER_NODE}/{file}: {e}"))?;
                if ts_svelte_compress::sha256(&bytes) != expected {
                    return Err(format!(
                        "{ADAPTER_NODE}@{version} {file} differs from the pinned release"
                    ));
                }
            }
            Some(json!({
                "directory": directory,
                "options": adapter_options.clone().unwrap_or_else(|| json!({})),
            }))
        }
        _ => None,
    };

    let request = json!({
        "adapterNode": adapter_node,
        "adapterNodeKit3": adapter_node_kit3,
        "mimeTypes": mime_types,
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
            "runesProjectOnly": runes_project_only,
            "minify": vite.minify,
            "serverMinify": vite.server_minify,
            "sourcemap": vite.sourcemap,
            "serverSourcemap": vite.server_sourcemap,
            "tsconfig": null,
            "mdsvex": mdsvex,
            "typescript": {},
        },
        "moduleExtensions": sync.module_extensions,
        "modules": modules,
        "staticAssets": static_assets,
        "staticAssetsComplete": true,
        "tailwind": tailwind,
        "sentry": sentry,
        "treeshake": vite.treeshake,
        "define": vite.define,
        "ssrExternal": ssr_external,
    });
    Ok(LoadedProject {
        request,
        requirements: loader.requirements,
    })
}

/// The `compilerOptions.runes` callback of the `sv create` templates (sv
/// 1.1.1): runes for every file outside `node_modules`, the compiler's
/// default inside it. Matched on its source text without whitespace.
const RUNES_TEMPLATE_CALLBACKS: [&str; 2] = [
    r"({filename})=>filename.split(/[/\\]/).includes('node_modules')?undefined:true",
    r"({filename})=>filename.includes('node_modules')?undefined:true",
];
fn runes_template_callback(root: &Path, value: &Value) -> Result<bool, String> {
    let Some([start, end]) = value
        .get("$nativeDynamic")
        .and_then(Value::as_array)
        .map(|span| span.iter().filter_map(Value::as_u64).collect::<Vec<_>>())
        .as_deref()
        .and_then(|span| <[u64; 2]>::try_from(span).ok())
    else {
        return Ok(false);
    };
    let Some((file, _)) = sync_config::vite_config_tree(root)? else {
        return Ok(false);
    };
    let source = read_text(&file)?;
    let text: String = source
        .get(start as usize..end as usize)
        .unwrap_or_default()
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    Ok(RUNES_TEMPLATE_CALLBACKS.contains(&text.as_str()))
}

/// `mrmime` 2.0.1's database (`.ext` -> type) from the copy Kit resolves,
/// read as JSON data; Kit 3 overrides `.ico` with `image/x-icon`.
fn mime_table(root: &Path, kit3: bool) -> Result<BTreeMap<String, String>, String> {
    // Node resolution from Kit's real directory (pnpm keeps its dependencies
    // beside it in the store): each ancestor's node_modules, nearest first.
    let kit = root
        .join("node_modules/@sveltejs/kit")
        .canonicalize()
        .map_err(|e| format!("@sveltejs/kit: {e}"))?;
    let directory = kit
        .ancestors()
        .filter(|ancestor| {
            ancestor
                .file_name()
                .is_none_or(|name| name != "node_modules")
        })
        .map(|ancestor| ancestor.join("node_modules/mrmime"))
        .find(|path| path.join("package.json").is_file())
        .ok_or("Cannot find the mrmime package Kit uses")?;
    let package: Value = serde_json::from_str(&read_text(&directory.join("package.json"))?)
        .map_err(|e| format!("mrmime package.json: {e}"))?;
    if package["version"] != MRMIME_VERSION {
        return Err(format!(
            "Native MIME lookup reads mrmime {MRMIME_VERSION}; installed {}",
            package["version"]
        ));
    }
    let source = read_text(&directory.join("index.mjs"))?;
    let table = source
        .strip_prefix("const mimes = ")
        .and_then(|rest| rest.find("\n};\n").map(|end| &rest[..end + 2]))
        .ok_or("Unexpected mrmime index.mjs layout")?;
    let mimes: BTreeMap<String, String> =
        serde_json::from_str(table).map_err(|e| format!("mrmime database: {e}"))?;
    let mut lookup: BTreeMap<String, String> = mimes
        .into_iter()
        .map(|(extension, mime)| (format!(".{extension}"), mime))
        .collect();
    if kit3 {
        lookup.insert(".ico".into(), "image/x-icon".into());
    }
    Ok(lookup)
}

fn adapter_options_out(options: &Option<Value>) -> String {
    options
        .as_ref()
        .and_then(|options| options.get("out"))
        .and_then(Value::as_str)
        .unwrap_or("build")
        .to_owned()
}

/// `sentrySvelteKit` 10.43.0 option resolution for the module rewrites.
fn sentry_options(
    loader: &mut Loader,
    options: &Value,
    tracing_server: bool,
    adapter: Option<&str>,
    adapter_out: String,
) -> Result<Value, String> {
    let (load, server_load) = match options.get("autoInstrument") {
        None | Some(Value::Bool(true)) => (true, true),
        Some(Value::Bool(false)) => (false, false),
        Some(Value::Object(map)) => (
            map.get("load").and_then(Value::as_bool).unwrap_or(true),
            map.get("serverLoad")
                .and_then(Value::as_bool)
                .unwrap_or(true),
        ),
        Some(_) => return Err("sentrySvelteKit autoInstrument must be static".into()),
    };
    let upload = options.get("autoUploadSourceMaps").map_or(Ok(true), |v| {
        v.as_bool().ok_or("autoUploadSourceMaps must be a boolean")
    })?;
    // getAdapterOutputDir: adapter-node's `out`; other adapters are not ported.
    let mut global_values = Map::new();
    if upload {
        if adapter != Some(ADAPTER_NODE) {
            loader.require(
                "vite",
                "sentry",
                "Sentry output directory is only derived for adapter-node",
            );
        }
        global_values.insert("__sentry_sveltekit_output_dir".into(), json!(adapter_out));
    }
    Ok(json!({
        "load": load,
        "serverLoad": server_load,
        "onlyInstrumentClient": tracing_server,
        "serverHooks": "src/hooks.server",
        "globalValues": global_values,
    }))
}

/// Kit's Vite aliases in precedence order (`$lib`, `kit.alias`, then user
/// `resolve.alias`), relative paths and Vite's default extension probing.
/// Only project-local, non-`node_modules` script files are followed.
struct LocalResolver {
    root: PathBuf,
    aliases: Vec<(String, PathBuf, bool)>,
    names: BTreeSet<String>,
    /// package.json `imports`: `#` key (without `/*`) -> target, wildcard.
    imports: Vec<(String, PathBuf, bool)>,
}
const SCRIPT_EXTENSIONS: [&str; 7] = [".mjs", ".js", ".mts", ".ts", ".jsx", ".tsx", ".json"];
impl LocalResolver {
    fn new(root: &Path, config: &ResolvedBuildPlanConfig, kit3: bool) -> Result<Self, String> {
        let mut aliases = vec![];
        if !kit3 {
            aliases.push(("$lib".to_owned(), config.lib_directory.clone(), false));
        }
        for alias in &config.aliases {
            match alias.name.strip_suffix("/*") {
                Some(base) => aliases.push((base.to_owned(), alias.path.clone(), true)),
                None => aliases.push((alias.name.clone(), alias.path.clone(), false)),
            }
        }
        let names = config
            .aliases
            .iter()
            .map(|alias| alias.name.clone())
            .collect();
        Ok(Self {
            root: root.to_path_buf(),
            aliases,
            names,
            imports: package_imports(root)?,
        })
    }
    /// Node's PACKAGE_IMPORTS_RESOLVE for string (or `{default}`) targets.
    fn import(&self, specifier: &str) -> Option<PathBuf> {
        self.imports.iter().find_map(|(key, target, wildcard)| {
            if *wildcard {
                let rest = specifier.strip_prefix(key.as_str())?.strip_prefix('/')?;
                Some(target.join(rest))
            } else {
                (specifier == key).then(|| target.clone())
            }
        })
    }
    fn alias(&self, specifier: &str) -> Option<PathBuf> {
        for (name, path, children) in &self.aliases {
            if *children {
                if let Some(rest) = specifier.strip_prefix(&format!("{name}/")) {
                    return Some(path.join(rest));
                }
            } else if specifier == name {
                return Some(path.clone());
            } else if !self.names.contains(&format!("{name}/*"))
                && let Some(rest) = specifier.strip_prefix(&format!("{name}/"))
            {
                return Some(path.join(rest));
            }
        }
        None
    }
    fn resolve(&self, specifier: &str, importer: &Path) -> Option<PathBuf> {
        let base = if specifier.starts_with("./") || specifier.starts_with("../") {
            resolve(importer.parent()?, specifier)
        } else if specifier.starts_with('#') {
            self.import(specifier)?
        } else {
            self.alias(specifier)?
        };
        let mut candidates = vec![base.clone()];
        for extension in SCRIPT_EXTENSIONS {
            candidates.push(PathBuf::from(format!("{}{extension}", base.display())));
        }
        // Vite resolves a `.js` import from TypeScript to the `.ts` source.
        if let Some(stem) = base.to_str().and_then(|path| path.strip_suffix(".js")) {
            candidates.push(PathBuf::from(format!("{stem}.ts")));
            candidates.push(PathBuf::from(format!("{stem}.tsx")));
        }
        for extension in SCRIPT_EXTENSIONS {
            candidates.push(base.join(format!("index{extension}")));
        }
        let found = candidates
            .into_iter()
            .find(|path| path.is_file())?
            .canonicalize()
            .ok()?;
        let script = found
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| matches!(e, "mjs" | "js" | "mts" | "ts" | "jsx" | "tsx"));
        let local = found.starts_with(&self.root)
            && !found.components().any(|c| c.as_os_str() == "node_modules");
        (script && local).then_some(found)
    }
}

/// The project package.json `imports` entries with a local string target
/// (or a conditions object with a string `default`), `#x/*` -> `./dir/*`.
fn package_imports(root: &Path) -> Result<Vec<(String, PathBuf, bool)>, String> {
    let package: Value = serde_json::from_str(&read_text(&root.join("package.json"))?)
        .map_err(|e| format!("package.json: {e}"))?;
    let mut imports = vec![];
    for (key, value) in package["imports"].as_object().into_iter().flatten() {
        let target = match value {
            Value::String(target) => target.as_str(),
            Value::Object(conditions) => match conditions.get("default") {
                Some(Value::String(target)) => target.as_str(),
                _ => continue,
            },
            _ => continue,
        };
        if !key.starts_with('#') || !target.starts_with("./") {
            continue;
        }
        match (key.strip_suffix("/*"), target.strip_suffix("/*")) {
            (Some(key), Some(target)) if !key.contains('*') && !target.contains('*') => {
                imports.push((key.to_owned(), resolve(root, target), true))
            }
            (None, _) if !key.contains('*') && !target.contains('*') => {
                imports.push((key.clone(), resolve(root, target), false))
            }
            _ => {}
        }
    }
    Ok(imports)
}

/// Source snapshots for the analysis roots and their transitive local imports.
fn local_module_graph(
    resolver: &LocalResolver,
    roots: BTreeSet<PathBuf>,
) -> Result<BTreeMap<String, Value>, String> {
    let mut modules = BTreeMap::new();
    let mut queue: std::collections::VecDeque<PathBuf> = roots.into_iter().collect();
    while let Some(file) = queue.pop_front() {
        let id = file.to_string_lossy().into_owned();
        if modules.contains_key(&id) {
            continue;
        }
        if modules.len() >= 20_000 {
            return Err("Analysis module graph exceeds 20000 local modules".into());
        }
        let source = read_text(&file)?;
        let mut imports = Map::new();
        for specifier in crate::analysis::runtime_imports(&id, &source)? {
            if let Some(target) = resolver.resolve(&specifier, &file) {
                imports.insert(specifier, json!(target));
                queue.push_back(target);
            }
        }
        modules.insert(id, json!({"source": source, "imports": imports}));
    }
    Ok(modules)
}

/// Installed plugins verified to define only dev-server hooks
/// (`configureServer`): they cannot change a build.
const DEV_SERVER_PLUGINS: &[(&str, &str)] = &[("vite-plugin-devtools-json", "1.0.0")];

/// A local plugin factory whose returned object only names itself and uses
/// dev/preview server hooks, which never run during `vite build`.
fn dev_server_only(plugin: &Value) -> bool {
    let Some(returns) = plugin
        .get("$localCall")
        .and_then(|call| call.get("returns"))
    else {
        return false;
    };
    let Some(object) = returns.as_object() else {
        return false;
    };
    !object.contains_key("$nativeDynamicSpread")
        && ordered_keys(object).iter().all(|key| {
            matches!(
                key.as_str(),
                "name" | "configureServer" | "configurePreviewServer"
            )
        })
}

/// Vite `build` options for the Kit client/server builds. Vite merges user
/// `build` options into both environments, so an explicit `minify` also
/// minifies the server output. Logging-only options are accepted without effect.
fn vite_build(
    loader: &mut Loader,
    project: &mut ViteProject,
    build: &Map<String, Value>,
) -> Result<(), String> {
    for key in ordered_keys(build) {
        let value = &build[&key];
        match key.as_str() {
            "minify" => {
                project.minify = match value {
                    Value::Bool(enabled) => *enabled,
                    Value::String(name) if name == "esbuild" || name == "oxc" => true,
                    Value::String(name) if name == "false" => false,
                    _ => {
                        loader.require(
                            "vite",
                            "build",
                            format!("build.minify {value} is not ported"),
                        );
                        true
                    }
                };
                project.server_minify = project.minify;
            }
            "sourcemap" => match value {
                Value::Bool(enabled) => project.sourcemap = *enabled,
                _ => loader.require(
                    "vite",
                    "build",
                    format!("build.sourcemap {value} is not ported"),
                ),
            },
            // Rolldown's default output target is esnext.
            "target" if value == "esnext" => {}
            "rollupOptions" => {
                let options =
                    object(Some(value), "build.rollupOptions")?.ok_or("build.rollupOptions")?;
                for option in ordered_keys(options) {
                    let value = &options[&option];
                    match option.as_str() {
                        // A warning filter changes logs only.
                        "onwarn" => {}
                        "output"
                            if object(Some(value), "output")
                                .ok()
                                .flatten()
                                .is_some_and(|o| ordered_keys(o).is_empty()) => {}
                        "treeshake" => {
                            let map = object(Some(value), "treeshake")?.ok_or("treeshake")?;
                            let mut treeshake = Map::new();
                            for name in ordered_keys(map) {
                                match (name.as_str(), &map[&name]) {
                                    (
                                        "moduleSideEffects"
                                        | "propertyReadSideEffects"
                                        | "unknownGlobalSideEffects",
                                        Value::Bool(flag),
                                    ) => {
                                        treeshake.insert(name.clone(), json!(flag));
                                    }
                                    _ => loader.require(
                                        "vite",
                                        "build",
                                        format!("treeshake.{name} is not ported"),
                                    ),
                                }
                            }
                            project.treeshake = Some(Value::Object(treeshake));
                        }
                        other => loader.require(
                            "vite",
                            "build",
                            format!("build.rollupOptions.{other} is not ported"),
                        ),
                    }
                }
            }
            // Logging and size reporting only.
            "reportCompressedSize" | "chunkSizeWarningLimit" => {}
            // Vite defaults the native pipeline already uses.
            "assetsInlineLimit" if value.as_f64() == Some(4096.0) => {}
            "cssCodeSplit" if value == true => {}
            "cssMinify" if value == true => {}
            other => loader.require(
                "vite",
                "build",
                format!("build.{other} = {value} is not ported"),
            ),
        }
    }
    Ok(())
}

/// Tailwind features that make `@tailwindcss/vite` rewrite a stylesheet.
const TAILWIND_DIRECTIVES: [&str; 12] = [
    "@tailwind",
    "@apply",
    "@theme",
    "@utility",
    "@variant",
    "@custom-variant",
    "@source",
    "@plugin",
    "@config",
    "@reference",
    "theme(",
    "--theme(",
];

/// `@tailwindcss/vite` transforms every CSS module but only rewrites those using
/// Tailwind features. The native pipeline needs those stylesheets explicitly:
/// project CSS files under `src` that import `tailwindcss` or use a directive.
/// A dependency stylesheet with directives fails loudly in the CSS pipeline.
fn tailwind_options(loader: &mut Loader, root: &Path, src: &Path) -> Result<Value, String> {
    let mut entries = vec![];
    let mut plugins = BTreeSet::new();
    let mut stack = vec![src.to_path_buf()];
    while let Some(directory) = stack.pop() {
        let mut items = std::fs::read_dir(&directory)
            .map_err(|e| format!("{}: {e}", directory.display()))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        items.sort_by_key(|item| item.file_name());
        for item in items {
            let path = item.path();
            if path.is_dir() {
                if item.file_name() != "node_modules" {
                    stack.push(path);
                }
                continue;
            }
            if path.extension().is_none_or(|ext| ext != "css") {
                continue;
            }
            let text = read_text(&path)?;
            let imports_tailwind = text.contains("'tailwindcss") || text.contains("\"tailwindcss");
            if !imports_tailwind && !TAILWIND_DIRECTIVES.iter().any(|d| text.contains(d)) {
                continue;
            }
            for line in text.lines() {
                if let Some(rest) = line.trim_start().strip_prefix("@plugin") {
                    let rest = rest.trim_start();
                    let quote = rest.chars().next().filter(|c| *c == '\'' || *c == '"');
                    if let Some(quote) = quote
                        && let Some(name) = rest[1..].split(quote).next()
                    {
                        plugins.insert(name.to_owned());
                    }
                }
            }
            entries.push(path);
        }
    }
    let mut registrations = vec![];
    for plugin in plugins {
        if plugin == "@tailwindcss/typography" {
            let version = loader.package_version(&plugin)?;
            if version != "0.5.19" {
                loader.require(
                    "tailwind",
                    "plugin_version",
                    format!("Native Typography is 0.5.19; installed {version}"),
                );
            }
            registrations
                .push(json!({"specifier": plugin, "implementation": plugin, "version": version}));
        } else {
            loader.require(
                "tailwind",
                "plugin",
                format!("Tailwind plugin {plugin} has no native implementation"),
            );
        }
    }
    Ok(json!({"entries": entries, "root": root, "plugins": registrations}))
}

struct ViteProject {
    /// `@tailwindcss/vite` is among the plugins.
    tailwind: bool,
    /// The static options object of `sentrySvelteKit(...)`, when used.
    sentry: Option<Value>,
    /// Client minification: Vite's client default is esbuild minification.
    minify: bool,
    /// Server minification: Vite's server default is off, but an explicit
    /// `build.minify` is merged into every environment.
    server_minify: bool,
    sourcemap: bool,
    /// Kit 3 sets the SSR environment's sourcemap to `build.sourcemap ?? true`.
    server_sourcemap: Option<bool>,
    treeshake: Option<Value>,
    aliases: Vec<(String, PathBuf)>,
    ssr_external: Vec<String>,
    define: BTreeMap<String, String>,
}

/// Static reading of the fields of `vite.config.*` that change the build.
fn vite_config(
    loader: &mut Loader,
    root: &Path,
    input: &ProjectInput,
    kit3: bool,
) -> Result<ViteProject, String> {
    let mut project = ViteProject {
        tailwind: false,
        sentry: None,
        minify: true,
        server_minify: false,
        sourcemap: false,
        server_sourcemap: None,
        treeshake: None,
        aliases: vec![],
        ssr_external: vec![],
        define: input.define.clone(),
    };
    let tree = match sync_config::vite_config_tree(root) {
        Ok(Some((_, tree))) => tree,
        Ok(None) => {
            if kit3 {
                project.server_sourcemap = Some(true);
            }
            return Ok(project);
        }
        Err(error) => {
            loader.require(
                "vite",
                "vite_config",
                format!("Cannot read vite config statically: {error}"),
            );
            return Ok(project);
        }
    };
    let Some(config) = sync_config::vite_config_object(&tree) else {
        loader.require(
            "vite",
            "vite_config",
            "vite config is not a static object or defineConfig({...})",
        );
        return Ok(project);
    };
    let config = object(Some(&Value::Object(config.clone())), "vite config")?
        .ok_or("vite config")?
        .clone();
    let config = &config;

    // resolve.alias as an object of find -> path.resolve(...) or string.
    if let Some(resolve_config) = object(config.get("resolve"), "vite resolve")?
        && let Some(alias) = resolve_config.get("alias")
    {
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
        if let Some(Value::Array(items)) = ssr.get("noExternal")
            && !items.is_empty()
        {
            loader.require(
                "vite",
                "ssr_no_external",
                "ssr.noExternal entries are not ported",
            );
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
                Some(("@sveltejs/kit/vite", "sveltekit", [])) => {}
                // Kit 3 options are read by the loader (`kit3_plugin_options`).
                Some(("@sveltejs/kit/vite", "sveltekit", [_])) if kit3 => {}
                Some(("@tailwindcss/vite", "default", [])) => project.tailwind = true,
                Some(("@sentry/sveltekit", "sentrySvelteKit", arguments))
                    if arguments.len() <= 1 =>
                {
                    let options = arguments.first().map(plain).unwrap_or_else(|| json!({}));
                    if options.to_string().contains("$nativeDynamic") {
                        loader.require("vite", "sentry", "sentrySvelteKit options must be static");
                    }
                    loader.require(
                        "vite",
                        "sentry",
                        "Sentry release/debug-ID injection and source-map flattening (@sentry/vite-plugin) are not ported; load wrapping and global values are",
                    );
                    project.sentry = Some(options);
                }
                Some((module, "default", []))
                    if DEV_SERVER_PLUGINS.iter().any(|(name, version)| {
                        *name == module && loader.package_version(name).is_ok_and(|v| v == *version)
                    }) => {}
                Some((module, export, _)) => loader.require(
                    "vite",
                    "plugin",
                    format!("Vite plugin {module}#{export} has no native equivalent"),
                ),
                None if dev_server_only(plugin) => {}
                None => loader.require(
                    "vite",
                    "plugin",
                    format!("Local Vite plugin {plugin} has no native equivalent"),
                ),
            }
        }
    }
    if let Some(build) = object(config.get("build"), "vite build")? {
        vite_build(loader, &mut project, build)?;
    }
    if kit3 {
        let explicit = object(config.get("build"), "vite build")?
            .is_some_and(|build| build.contains_key("sourcemap"));
        project.server_sourcemap = Some(if explicit { project.sourcemap } else { true });
        if config.contains_key("environments") {
            loader.require(
                "vite",
                "environments",
                "Vite `environments` options are not ported",
            );
        }
    }
    if let Some(esbuild) = object(config.get("esbuild"), "vite esbuild")? {
        for key in ordered_keys(esbuild) {
            match (key.as_str(), &esbuild[&key]) {
                // `drop: ['debugger']`: Rolldown's client minifier drops debugger
                // statements; esbuild's TS-transform drop is not ported.
                ("drop", value) if is_dynamic(value) || value == &json!(["debugger"]) || value == &json!([]) => {
                    loader.require(
                        "vite",
                        "esbuild",
                        "esbuild.drop on TypeScript transforms is not ported (client minification drops debugger)",
                    )
                }
                (other, _) => loader.require("vite", "esbuild", format!("esbuild.{other} is not ported")),
            }
        }
    }
    Ok(project)
}

#[cfg(test)]
#[path = "project_tests.rs"]
mod tests;
