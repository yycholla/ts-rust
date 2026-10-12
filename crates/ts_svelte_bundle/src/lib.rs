//! Native Rolldown adapter for resolved Svelte client/server entry graphs.
//!
//! This is a build plugin, not the Svelte compiler or a SvelteKit orchestrator.
//! No JavaScript configuration or application modules are executed. `generate`
//! reads imports and returns artifacts without writing any output files. Ordinary
//! TypeScript is transformed by Rolldown's native OXC pipeline; Svelte and rune
//! JavaScript modules use `ts_svelte_compiler::dispatch`.
//!
//! Rolldown 1.2.13 removed built-in CSS bundling. The optional native CSS plugin
//! extracts component styles, bundles stylesheet imports and emits linked assets.
//! Without that plugin, components use Svelte's injected-CSS mode. Explicit
//! mdsvex options enable native Markdown; TypeScript rune modules and component
//! scripts use the separate native TypeScript preprocessor with composed maps.

pub mod css;
pub mod metadata;
pub mod output;
mod plugin;
mod preprocess;
pub mod sentry;
mod ssr_external;
mod vite_features;
mod worker_assets;

/// File extensions bundled as assets when imported from JavaScript.
pub(crate) const ASSET_EXTENSIONS: [&str; 18] = [
    "png", "jpg", "jpeg", "gif", "svg", "webp", "avif", "ico", "woff", "woff2", "ttf", "otf",
    "mp4", "webm", "mp3", "wav", "pdf", "wasm",
];

use base64::Engine as _;
use rolldown::plugin::Plugin;
use rolldown::{
    Bundler, BundlerOptions, InputItem, IsExternal, ModuleType, OutputFormat, Platform,
    RawMinifyOptions, RawMinifyOptionsDetailed, ResolveOptions, SourceMapType, TsConfig,
};
use rolldown_common::Output;
use rolldown_error::{BatchedBuildDiagnostic, BuildDiagnostic};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};
use ts_svelte::compatibility::Versions;

pub const ROLLDOWN_VERSION: &str = "1.2.13";
pub const MAX_REQUEST_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Target {
    Client,
    Server,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BundleRequest {
    /// Absolute directory containing package.json and installed dependencies.
    pub root: PathBuf,
    /// Named entry modules. Values are root-relative or absolute module paths.
    pub entries: BTreeMap<String, PathBuf>,
    pub target: Target,
    /// Rebundle already compiled Node application code without Svelte export
    /// conditions or compile-time NODE_ENV substitution.
    #[serde(default)]
    pub node_rebundle: bool,
    #[serde(default)]
    pub strict_execution_order: Option<bool>,
    pub compatibility: Versions,
    #[serde(default)]
    pub dev: bool,
    #[serde(default)]
    pub runes: Option<bool>,
    /// `runes` applies only to components outside `node_modules` (the `sv
    /// create` callback); others use the compiler's default.
    #[serde(default)]
    pub runes_project_only: bool,
    #[serde(default)]
    pub minify: bool,
    #[serde(default = "enabled")]
    pub sourcemap: bool,
    #[serde(default)]
    pub aliases: Vec<Alias>,
    /// Exact module IDs (including deep IDs) to leave external.
    #[serde(default)]
    pub external: Vec<String>,
    /// Package names to externalize together with every package subpath.
    #[serde(default)]
    pub external_packages: Vec<String>,
    /// Vite server-build externalization policy, applied to bare ids after
    /// aliasing. Only valid for the server target.
    #[serde(default)]
    pub ssr_external: Option<ts_svelte_kit::vite_ssr::SsrExternalPolicy>,
    /// Rollup-style `treeshake` options from the application's build config.
    #[serde(default)]
    pub treeshake: Option<Treeshake>,
    /// `@sentry/sveltekit` module rewrites (load wrapping, global values).
    #[serde(default)]
    pub sentry: Option<sentry::SentryOptions>,
    /// Additional package export conditions. Target and mode conditions are
    /// always supplied, including the Svelte condition.
    #[serde(default)]
    pub conditions: Vec<String>,
    /// Replacement expressions, not arbitrary plugin functions.
    #[serde(default)]
    pub define: BTreeMap<String, String>,
    /// Explicit tsconfig, or disabled automatic config discovery when absent.
    pub tsconfig: Option<PathBuf>,
    /// Generated modules at root-relative/absolute file paths. Exact import
    /// resolution works before disk resolution; no temporary files are written.
    #[serde(default)]
    pub virtual_modules: BTreeMap<PathBuf, String>,
    #[serde(default)]
    pub virtual_module_maps: BTreeMap<PathBuf, Value>,
    #[serde(default)]
    pub compiler_options: CompilerOptions,
    /// Resolved native mdsvex options. Absence disables Markdown components.
    #[serde(default)]
    pub mdsvex: Option<ts_svelte_preprocess::Options>,
    /// Explicit preprocessing policy for rune modules and component scripts.
    /// This is separate from Rolldown's ordinary-module tsconfig.
    #[serde(default)]
    pub typescript: ts_svelte_transpile::TransformOptions,
    /// Included only to make unsupported preprocessing impossible to ignore.
    #[serde(default)]
    pub preprocessors: Vec<String>,
    /// Native extracted styles and assets. None retains injected component CSS
    /// and rejects stylesheet imports requiring graph processing.
    #[serde(default)]
    pub css: Option<css::CssOptions>,
    #[serde(default)]
    pub output: output::OutputOptions,
}
fn enabled() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Alias {
    pub find: String,
    pub replacement: PathBuf,
    #[serde(default)]
    pub matching: AliasMatch,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum AliasMatch {
    Exact,
    #[default]
    Prefix,
    Children,
}
impl Alias {
    pub fn resolve(&self, id: &str) -> Option<PathBuf> {
        if id == self.find && self.matching != AliasMatch::Children {
            return Some(self.replacement.clone());
        }
        if self.matching != AliasMatch::Exact {
            let suffix = id.strip_prefix(&self.find)?.strip_prefix('/')?;
            if self.matching == AliasMatch::Prefix || !suffix.is_empty() {
                return Some(self.replacement.join(suffix));
            }
        }
        None
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CompilerOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preserve_comments: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preserve_whitespace: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disclose_version: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub accessors: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub immutable: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub experimental: Option<Experimental>,
}
/// Rollup `treeshake` object options; absent fields keep Rolldown's defaults.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Treeshake {
    pub module_side_effects: Option<bool>,
    pub property_read_side_effects: Option<bool>,
    pub unknown_global_side_effects: Option<bool>,
}
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Experimental {
    pub r#async: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BundleResult {
    pub rolldown_version: &'static str,
    pub target: Target,
    pub css_mode: &'static str,
    pub css: css::CssOutput,
    pub module_graph: BTreeMap<String, metadata::Module>,
    pub chunks: Vec<Chunk>,
    pub assets: Vec<Asset>,
    pub diagnostics: Vec<Value>,
    pub compiler_responses: BTreeMap<String, Value>,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Chunk {
    /// Native emitted file references owned by retained modules in this chunk.
    pub referenced_files: Vec<String>,
    pub file_name: String,
    pub name: String,
    pub code: String,
    pub is_entry: bool,
    pub is_dynamic_entry: bool,
    pub facade_module_id: Option<String>,
    pub module_ids: Vec<String>,
    pub imports: Vec<String>,
    pub dynamic_imports: Vec<String>,
    pub exports: Vec<String>,
    pub map: Option<Value>,
    pub sourcemap_file_name: Option<String>,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Asset {
    pub file_name: String,
    pub names: Vec<String>,
    pub original_file_names: Vec<String>,
    /// All asset bytes use base64; this preserves binary assets exactly.
    pub encoding: &'static str,
    pub source: String,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BundleError {
    pub code: &'static str,
    pub message: String,
    pub diagnostics: Vec<Value>,
    pub compiler_responses: BTreeMap<String, Value>,
}
impl std::fmt::Display for BundleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.message.fmt(f)
    }
}
impl std::error::Error for BundleError {}
impl BundleError {
    fn invalid(message: impl Into<String>) -> Self {
        Self {
            code: "invalid_bundle_request",
            message: message.into(),
            diagnostics: vec![],
            compiler_responses: BTreeMap::new(),
        }
    }
}

pub(crate) fn absolute(root: &Path, path: &Path) -> anyhow::Result<PathBuf> {
    let joined = root.join(path);
    anyhow::ensure!(
        joined.is_absolute(),
        "Expected absolute module path: {}",
        joined.display()
    );
    let mut normalized = PathBuf::new();
    for part in joined.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                anyhow::ensure!(normalized.pop(), "Path escapes filesystem root");
            }
            _ => normalized.push(part.as_os_str()),
        }
    }
    anyhow::ensure!(normalized.to_str().is_some(), "Module paths must be UTF-8");
    Ok(normalized)
}
fn path_text(path: &Path) -> Result<String, BundleError> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| BundleError::invalid("Module paths must be UTF-8"))
}
pub(crate) fn diagnostic(value: &BuildDiagnostic) -> Value {
    // BuildDiagnostic::Display loses the text of wrapped plugin errors; render
    // through to_diagnostic(), which invokes their on_diagnostic hook.
    let rendered = value.to_diagnostic();
    json!({"source":"rolldown","code":value.kind().to_string(),
        "severity":format!("{:?}", value.severity()).to_lowercase(),
        "message":rendered.convert_to_string(false), "id":value.id(),
        "plugin":value.plugin(), "primaryLocation":rendered.get_primary_location()})
}
fn failure(
    error: BatchedBuildDiagnostic,
    responses: &Arc<Mutex<BTreeMap<String, Value>>>,
) -> BundleError {
    let mut diagnostics: Vec<_> = error.into_vec().iter().map(diagnostic).collect();
    let compiler_responses = responses.lock().unwrap_or_else(|e| e.into_inner()).clone();
    for (id, response) in &compiler_responses {
        if response["ok"] == false {
            if let Some(message) = response["error"]["message"].as_str() {
                diagnostics.push(
                    json!({"source":"ts-rust-svelte","code":response["error"]["code"],
                    "severity":"error","id":id,"message":message}),
                );
            }
        }
    }
    BundleError {
        code: "bundle_failed",
        message: diagnostics
            .iter()
            .filter_map(|d| d["message"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        diagnostics,
        compiler_responses,
    }
}

/// Bundle an explicit graph using native Rust transforms. The host must call
/// this inside a Tokio runtime. Results contain all emitted files; this API does
/// not create directories, write files, execute app code or run JS plugins.
pub async fn generate(request: BundleRequest) -> Result<BundleResult, BundleError> {
    generate_with_plugins(request, Vec::new()).await
}

/// Embed additional native graph plugins after component preprocessing and
/// before graph metadata collection. Framework integrations own these plugins;
/// the component bundler has no framework dependency. Plugins execute trusted
/// Rust code and are intentionally unavailable through the JSON request API.
pub async fn generate_with_plugins(
    mut request: BundleRequest,
    additional_plugins: Vec<Arc<rolldown::plugin::Pluginable>>,
) -> Result<BundleResult, BundleError> {
    if !request.root.is_absolute() || !request.root.is_dir() {
        return Err(BundleError::invalid(
            "root must be an existing absolute directory",
        ));
    }
    request.root =
        absolute(&request.root, Path::new(".")).map_err(|e| BundleError::invalid(e.to_string()))?;
    if request.entries.is_empty() {
        return Err(BundleError::invalid("At least one named entry is required"));
    }
    if !request.preprocessors.is_empty() {
        return Err(BundleError::invalid(format!(
            "Native preprocessors are not registered: {}",
            request.preprocessors.join(", ")
        )));
    }
    ts_svelte::compatibility::resolve(&request.compatibility).map_err(BundleError::invalid)?;
    let mut virtual_modules = BTreeMap::new();
    for (path, text) in &request.virtual_modules {
        let path =
            absolute(&request.root, path).map_err(|e| BundleError::invalid(e.to_string()))?;
        if virtual_modules.insert(path, text.clone()).is_some() {
            return Err(BundleError::invalid(
                "Duplicate normalized virtual module path",
            ));
        }
    }
    request.virtual_modules = virtual_modules;
    let mut maps = BTreeMap::new();
    for (file, map) in &request.virtual_module_maps {
        let file =
            absolute(&request.root, file).map_err(|e| BundleError::invalid(e.to_string()))?;
        if !request.virtual_modules.contains_key(&file) {
            return Err(BundleError::invalid(
                "Virtual source map has no corresponding module",
            ));
        }
        if maps.insert(file, map.clone()).is_some() {
            return Err(BundleError::invalid(
                "Duplicate normalized virtual source map",
            ));
        }
    }
    request.virtual_module_maps = maps;
    if request.node_rebundle && request.target != Target::Server {
        return Err(BundleError::invalid(
            "Node rebundling requires the server target",
        ));
    }
    let mut entries = Vec::new();
    for (name, path) in &request.entries {
        if name.is_empty()
            || name.contains(['\\', '\0'])
            || Path::new(name)
                .components()
                .any(|c| !matches!(c, Component::Normal(_)))
        {
            return Err(BundleError::invalid(format!("Unsafe entry name {name:?}")));
        }
        let path =
            absolute(&request.root, path).map_err(|e| BundleError::invalid(e.to_string()))?;
        if !path.is_file() && !request.virtual_modules.contains_key(&path) {
            return Err(BundleError::invalid(format!(
                "Entry does not exist: {}",
                path.display()
            )));
        }
        entries.push(InputItem {
            name: Some(name.clone()),
            import: path_text(&path)?,
        });
    }
    for alias in &mut request.aliases {
        if alias.find.is_empty() {
            return Err(BundleError::invalid("Alias names cannot be empty"));
        }
        let replacement = absolute(&request.root, &alias.replacement)
            .map_err(|e| BundleError::invalid(e.to_string()))?;
        alias.replacement = replacement;
    }
    let mut conditions = if request.node_rebundle {
        vec!["node".into(), "import".into(), "default".into()]
    } else {
        vec![
            "svelte".to_string(),
            if request.target == Target::Client {
                "browser"
            } else {
                "node"
            }
            .into(),
            if request.dev {
                "development"
            } else {
                "production"
            }
            .into(),
            "module".into(),
        ]
    };
    for condition in &request.conditions {
        if (request.target == Target::Client && condition == "node")
            || (request.target == Target::Server && condition == "browser")
            || (!request.node_rebundle && request.dev && condition == "production")
            || (!request.node_rebundle && !request.dev && condition == "development")
        {
            return Err(BundleError::invalid(format!(
                "Export condition {condition:?} conflicts with the selected target/mode"
            )));
        }
        if !conditions.contains(condition) {
            conditions.push(condition.clone());
        }
    }
    let tsconfig = if let Some(path) = &request.tsconfig {
        let path =
            absolute(&request.root, path).map_err(|e| BundleError::invalid(e.to_string()))?;
        if !path.is_file() {
            return Err(BundleError::invalid("Explicit tsconfig does not exist"));
        }
        TsConfig::Manual(path)
    } else {
        TsConfig::Auto(false)
    };
    let external = Arc::new(request.external.clone());
    let packages = Arc::new(request.external_packages.clone());
    let is_external = IsExternal::Fn(Some(Arc::new(move |id, _, _| {
        let matches = external.iter().any(|item| item == id)
            || packages.iter().any(|pkg| {
                id == pkg
                    || id
                        .strip_prefix(pkg.as_str())
                        .is_some_and(|tail| tail.starts_with('/'))
            });
        Box::pin(async move { Ok(matches) })
    })));
    let mut defines = request.define.clone();
    // Vite's define plugin replaces process.env only for client environments;
    // server environments keep it for runtime (`keepProcessEnv`).
    if !request.node_rebundle && request.target == Target::Client {
        let node_env = if request.dev {
            "\"development\""
        } else {
            "\"production\""
        };
        for key in [
            "process.env.NODE_ENV",
            "global.process.env.NODE_ENV",
            "globalThis.process.env.NODE_ENV",
        ] {
            defines.entry(key.into()).or_insert_with(|| node_env.into());
        }
        for key in [
            "process.env",
            "global.process.env",
            "globalThis.process.env",
        ] {
            defines.entry(key.into()).or_insert_with(|| "{}".into());
        }
    }
    let mut module_types = ASSET_EXTENSIONS
        .into_iter()
        .map(|ext| (format!(".{ext}"), ModuleType::Asset))
        .collect::<BTreeMap<_, _>>();
    module_types.insert(".svelte".into(), ModuleType::Js);
    let mut resolve_extensions = [
        ".mjs", ".js", ".mts", ".ts", ".jsx", ".tsx", ".json", ".svelte",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    if let Some(options) = &request.mdsvex {
        // Validates version/options even if the graph has no Markdown files.
        ts_svelte_preprocess::preprocess("", "__validate__.none", options)
            .map_err(|e| BundleError::invalid(e.to_string()))?;
        for extension in &options.extensions {
            if [
                ".svelte", ".js", ".ts", ".mjs", ".mts", ".cjs", ".cts", ".jsx", ".tsx", ".json",
                ".css",
            ]
            .iter()
            .any(|suffix| extension.ends_with(suffix))
            {
                return Err(BundleError::invalid(
                    "Markdown extensions cannot override script/component/stylesheet module kinds",
                ));
            }
            module_types.insert(extension.clone(), ModuleType::Js);
            if !resolve_extensions.contains(extension) {
                resolve_extensions.push(extension.clone());
            }
        }
    }
    let mut options = BundlerOptions {
        strict_execution_order: if request.node_rebundle {
            Some(true)
        } else {
            request.strict_execution_order
        },
        cwd: Some(request.root.clone()),
        input: Some(entries),
        format: Some(OutputFormat::Esm),
        platform: Some(if request.target == Target::Client {
            Platform::Browser
        } else {
            Platform::Node
        }),
        external: Some(is_external),
        resolve: Some(ResolveOptions {
            condition_names: Some(conditions),
            extensions: Some(resolve_extensions),
            ..Default::default()
        }),
        sourcemap: request.sourcemap.then_some(SourceMapType::File),
        // A Node rebundle of minified code removes whitespace only, the
        // nearest equivalent of Rollup keeping its already minified input.
        minify: Some(if request.node_rebundle && request.minify {
            RawMinifyOptions::Object(RawMinifyOptionsDetailed {
                mangle: None,
                mangle_properties: None,
                compress: None,
                remove_whitespace: true,
                ascii_only: false,
            })
        } else {
            RawMinifyOptions::Bool(request.minify)
        }),
        treeshake: match &request.treeshake {
            Some(settings) => {
                use rolldown_common::{
                    InnerOptions, ModuleSideEffects, PropertyReadSideEffects, TreeshakeOptions,
                };
                TreeshakeOptions::Option(InnerOptions {
                    module_side_effects: ModuleSideEffects::Boolean(
                        settings.module_side_effects.unwrap_or(true),
                    ),
                    property_read_side_effects: settings.property_read_side_effects.map(|value| {
                        if value {
                            PropertyReadSideEffects::Always
                        } else {
                            PropertyReadSideEffects::False
                        }
                    }),
                    unknown_global_side_effects: settings.unknown_global_side_effects,
                    ..InnerOptions::default()
                })
            }
            None => Default::default(),
        },
        tsconfig: Some(tsconfig),
        define: Some(defines.into_iter().collect()),
        module_types: Some(module_types.into_iter().collect()),
        entry_filenames: Some("[name].js".to_owned().into()),
        chunk_filenames: Some("chunks/[name]-[hash].js".to_owned().into()),
        asset_filenames: Some(css::DEFAULT_ASSET_FILE_NAMES.to_owned().into()),
        ..Default::default()
    };
    output::configure(&mut options, &request.output).map_err(BundleError::invalid)?;
    let responses = Arc::new(Mutex::new(BTreeMap::new()));
    let css_state = css::CssState::default();
    let mut plugins = Vec::new();
    let worker_assets = worker_assets::WorkerAssets::default();
    if request.output.asset_url_mode == output::AssetUrlMode::ServiceWorkerLocation {
        plugins.push(Plugin::new_shared(worker_assets.clone()));
    }
    if let Some(css_options) = &request.css {
        plugins.push(Plugin::new_shared(
            css::CssPlugin::new(
                Arc::new(request.clone()),
                css_options.clone(),
                css_state.clone(),
            )
            .map_err(|error| BundleError::invalid(error.to_string()))?,
        ));
    }
    let ssr_externals = match &request.ssr_external {
        Some(_) if request.target != Target::Server => {
            return Err(BundleError::invalid(
                "ssrExternal applies only to the server target",
            ));
        }
        Some(policy) => Some(Arc::new(
            ssr_external::SsrExternals::new(&request.root, policy.clone())
                .map_err(BundleError::invalid)?,
        )),
        None => None,
    };
    if let Some(options) = &request.sentry {
        // Sentry's load hook is `enforce: 'pre'`: it precedes source overlays.
        plugins.push(Plugin::new_shared(
            sentry::SentryPlugin::new(options.clone())
                .map_err(|error| BundleError::invalid(error.to_string()))?,
        ));
    }
    let plugin = plugin::SveltePlugin {
        request: Arc::new(request.clone()),
        responses: Arc::clone(&responses),
        css: request.css.as_ref().map(|_| css_state.clone()),
        ssr_externals,
    };
    plugins.push(Plugin::new_shared(plugin));
    plugins.push(Plugin::new_shared(vite_features::ViteFeatures {
        root: request.root.clone(),
    }));
    plugins.extend(additional_plugins);
    let metadata = metadata::Metadata::default();
    plugins.push(Plugin::new_shared(metadata.clone()));
    let mut bundler =
        Bundler::with_plugins(options, plugins).map_err(|e| failure(e, &responses))?;
    let generated = bundler.generate().await;
    // Always close even after a failed graph; preserve both errors if close fails.
    let closed = bundler.close().await;
    let output = match generated {
        Ok(output) => {
            closed.map_err(|e| failure(e, &responses))?;
            output
        }
        Err(error) => {
            let mut error = failure(error, &responses);
            error.diagnostics.extend(css_state.diagnostics());
            if let Err(close_error) = closed {
                let close = failure(close_error, &responses);
                error
                    .message
                    .push_str(&format!("\nClosing bundler: {}", close.message));
                error.diagnostics.extend(close.diagnostics);
            }
            return Err(error);
        }
    };
    let mut result = BundleResult {
        rolldown_version: ROLLDOWN_VERSION,
        target: request.target,
        css_mode: if request.css.is_some() {
            "external"
        } else {
            "injected"
        },
        css: css_state
            .output()
            .map_err(|error| BundleError::invalid(error.to_string()))?,
        module_graph: metadata
            .0
            .lock()
            .map_err(|_| BundleError::invalid("Metadata lock poisoned"))?
            .clone(),
        chunks: vec![],
        assets: vec![],
        diagnostics: output
            .warnings
            .iter()
            .map(diagnostic)
            .chain(css_state.diagnostics())
            .collect(),
        compiler_responses: responses.lock().unwrap_or_else(|e| e.into_inner()).clone(),
    };
    for response in result.compiler_responses.values() {
        if let Some(diagnostics) = response["diagnostics"].as_array() {
            result.diagnostics.extend(diagnostics.iter().cloned());
        }
    }
    for file in output.assets {
        match file {
            Output::Chunk(chunk) => result.chunks.push(Chunk {
                referenced_files: worker_assets.chunk_files(chunk.filename.as_str()),
                file_name: chunk.filename.to_string(),
                name: chunk.name.to_string(),
                code: chunk.code.clone(),
                is_entry: chunk.is_entry,
                is_dynamic_entry: chunk.is_dynamic_entry,
                facade_module_id: chunk
                    .facade_module_id
                    .as_ref()
                    .map(|id| id.as_str().to_string()),
                module_ids: chunk
                    .module_ids
                    .iter()
                    .map(|id| id.as_str().to_string())
                    .collect(),
                imports: chunk.imports.iter().map(ToString::to_string).collect(),
                dynamic_imports: chunk
                    .dynamic_imports
                    .iter()
                    .map(ToString::to_string)
                    .collect(),
                exports: chunk.exports.iter().map(ToString::to_string).collect(),
                map: chunk
                    .map
                    .as_ref()
                    .map(|map| serde_json::from_str(&map.to_json_string()))
                    .transpose()
                    .map_err(|e| {
                        BundleError::invalid(format!("Invalid emitted source map: {e}"))
                    })?,
                sourcemap_file_name: chunk.sourcemap_filename.clone(),
            }),
            Output::Asset(asset) => result.assets.push(Asset {
                file_name: asset.filename.to_string(),
                names: asset.names.clone(),
                original_file_names: asset.original_file_names.clone(),
                encoding: "base64",
                source: base64::engine::general_purpose::STANDARD.encode(asset.source.as_bytes()),
            }),
        }
    }
    result.chunks.sort_by(|a, b| a.file_name.cmp(&b.file_name));
    result.assets.sort_by(|a, b| a.file_name.cmp(&b.file_name));
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn paths_normalize_without_filesystem_mutation() {
        assert_eq!(
            absolute(Path::new("/project"), Path::new("src/../main.ts")).unwrap(),
            Path::new("/project/main.ts")
        );
        assert!(absolute(Path::new("/project"), Path::new("/../../escape")).is_err());
    }
    #[test]
    fn unsupported_request_fields_are_rejected() {
        assert!(serde_json::from_value::<BundleRequest>(json!({"root":"/project","entries":{"app":"app.js"},"target":"client","compatibility":{},"plugins":["anything"]})).is_err());
    }
}
