//! Native CSS graph/asset plugin. The compiler registers extracted component CSS;
//! Lightning CSS parses and bundles styles; Rolldown owns JavaScript graph output.
//! Client CSS proxies use top-level await, so static and dynamic import evaluation
//! both wait for their styles. Output must remain ESM with top-level-await support.
//! Explicit file entries may opt into the separate native Tailwind compiler.
//! No JavaScript configuration or PostCSS implementation is executed.
use crate::{BundleRequest, Target, absolute};
use anyhow::{Context, bail, ensure};
use lightningcss::{
    properties::{Property, css_modules::Specifier},
    rules::CssRule,
    stylesheet::{ParserOptions, StyleSheet},
    visitor::{Visit, VisitTypes, Visitor},
};
use rolldown::ModuleType;
use rolldown::plugin::{
    HookGenerateBundleArgs, HookLoadArgs, HookLoadOutput, HookLoadReturn, HookResolveIdArgs,
    HookResolveIdOutput, HookResolveIdReturn, HookUsage, Plugin, PluginContext,
    SharedLoadPluginContext,
};
use rolldown_common::{EmittedAsset, Output, side_effects::HookSideEffects};
use rolldown_sourcemap::{SourceMap, SourceMapBuilder};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet, VecDeque},
    io::Read,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
use ts_svelte_css::{
    AssetLink, BundleInput, ImportResolution, ImportTarget, ModulesOptions, Options,
};

const COMPONENT_PREFIX: &str = "\0ts-svelte-css:component:";
const RUNTIME_ID: &str = "\0ts-svelte-css:runtime";
const MAX_CSS: usize = 4 * 1024 * 1024;
const MAX_ASSET: usize = 64 * 1024 * 1024;
const MAX_ASSETS: usize = 256 * 1024 * 1024;
const DEFAULT_ASSETS_INLINE_LIMIT: u64 = 4096;
const GIT_LFS_PREFIX: &[u8] = b"version https://git-lfs.github.com";
pub(crate) const DEFAULT_ASSET_FILE_NAMES: &str = "assets/[name]-[hash][extname]";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CssOptions {
    /// Vite `build.assetsInlineLimit`: assets smaller than this many bytes
    /// are inlined as data URLs. Absent uses Vite's default of 4096.
    #[serde(default)]
    pub assets_inline_limit: Option<u64>,
    /// Optional origin/root URL prefix for emitted CSS; absent uses Rolldown's
    /// file URL relative to the importing JS chunk (including dynamic chunks).
    pub public_base: Option<String>,
    /// CSP nonce for runtime-created stylesheet links.
    pub nonce: Option<String>,
    /// Native Tailwind configuration. Enables the listed CSS entries and
    /// Tailwind directives in extracted component/virtual CSS. No JS execution.
    #[serde(default)]
    pub tailwind: Option<TailwindOptions>,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct TailwindOptions {
    /// Root-relative or absolute CSS entries imported by the JS graph.
    pub entries: Vec<PathBuf>,
    /// Signed versions for supplied virtual modules. Missing entries use a
    /// deterministic content-derived version; no editor revision is inferred.
    pub snapshot_versions: BTreeMap<PathBuf, i64>,
    /// Source-discovery working directory; defaults to BundleRequest.root.
    pub root: Option<PathBuf>,
    pub plugins: Vec<ts_tailwind::snapshot::PluginRegistration>,
    /// Ordered native CSS resolver aliases, independent of JS graph aliases.
    pub aliases: Vec<ts_tailwind_host::Alias>,
    pub module_paths: Vec<PathBuf>,
    pub limits: ts_tailwind_host::HostLimits,
}
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TailwindModuleOutput {
    pub version: String,
    pub generated_candidates: Vec<String>,
    pub invalid_candidates: Vec<String>,
    pub diagnostics: Vec<Value>,
    pub watch: ts_tailwind_host::WatchSet,
}
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CssModuleOutput {
    pub module_id: String,
    pub origin: String,
    pub file_name: String,
    pub map_file_name: Option<String>,
    pub sources: Vec<String>,
    pub assets: Vec<String>,
    pub exports: Value,
    pub dependencies: Vec<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tailwind: Option<TailwindModuleOutput>,
}
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CssChunkOutput {
    /// Styles owned by this chunk, in module evaluation order.
    pub own: Vec<String>,
    /// Dependency-first static closure, excluding dynamic edges.
    pub imports: Vec<String>,
    /// Each direct dynamic chunk and its static CSS closure.
    pub dynamic_imports: BTreeMap<String, Vec<String>>,
}
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CssOutput {
    pub modules: BTreeMap<String, CssModuleOutput>,
    pub chunks: BTreeMap<String, CssChunkOutput>,
}
#[derive(Debug, Clone)]
struct Component {
    origin: String,
    code: String,
    map: Option<Value>,
}
#[derive(Debug, Default)]
struct State {
    components: BTreeMap<String, Component>,
    output: CssOutput,
    emitted: BTreeMap<String, (Vec<u8>, String)>,
    asset_bytes: usize,
    diagnostics: Vec<Value>,
}
/// Shared between the Svelte compiler plugin and CSS plugin for one build.
/// Construct a fresh state for every generate call; stale watch snapshots are
/// never reused implicitly.
#[derive(Debug, Default, Clone)]
pub struct CssState(Arc<Mutex<State>>);
impl CssState {
    /// `map` must already describe emitted CSS against original source documents
    /// (compose any Markdown/TypeScript preprocessing before registering it).
    /// Append `import <JSON encoded returned id>;` to the component JavaScript.
    pub fn register_component(
        &self,
        origin: &str,
        code: String,
        map: Option<Value>,
    ) -> anyhow::Result<String> {
        ensure!(
            Path::new(origin).is_absolute() && !origin.contains(['\0', '?', '#']),
            "Component CSS requires an absolute source filename"
        );
        ensure!(code.len() <= MAX_CSS, "Component CSS exceeds 4 MiB");
        if let Some(map) = &map {
            parse_map(map)?;
        }
        let id = format!("{COMPONENT_PREFIX}{origin}");
        let mut state = self
            .0
            .lock()
            .map_err(|_| anyhow::anyhow!("CSS state lock poisoned"))?;
        if let Some(previous) = state.components.get(&id) {
            ensure!(
                previous.code == code && previous.map == map,
                "Component CSS was registered with conflicting snapshots: {origin}"
            );
        } else {
            state.components.insert(
                id.clone(),
                Component {
                    origin: origin.into(),
                    code,
                    map,
                },
            );
        }
        Ok(id)
    }
    /// Exact native error chains retained even when a Rolldown wrapper omits
    /// its load-hook cause. Merge into failed-build diagnostics in the host.
    pub fn diagnostics(&self) -> Vec<Value> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .diagnostics
            .clone()
    }
    pub fn output(&self) -> anyhow::Result<CssOutput> {
        Ok(self
            .0
            .lock()
            .map_err(|_| anyhow::anyhow!("CSS state lock poisoned"))?
            .output
            .clone())
    }
}
#[derive(Debug)]
pub struct CssPlugin {
    request: Arc<BundleRequest>,
    options: CssOptions,
    state: CssState,
}
impl CssPlugin {
    /// Vite 7.3.1 `shouldInline` for a CSS `url()` in an application build.
    fn should_inline(&self, file: &str, url: &str, bytes: &[u8]) -> bool {
        let query = url
            .split_once('?')
            .map_or("", |(_, query)| query.split('#').next().unwrap_or(""));
        let flag = |name: &str| {
            query
                .split('&')
                .any(|part| part == name || part.starts_with(&format!("{name}=")))
        };
        if flag("no-inline") {
            return false;
        }
        if flag("inline") {
            return true;
        }
        if file.ends_with(".html") || (file.ends_with(".svg") && url.contains('#')) {
            return false;
        }
        let limit = self
            .options
            .assets_inline_limit
            .unwrap_or(DEFAULT_ASSETS_INLINE_LIMIT);
        (bytes.len() as u64) < limit && !bytes.starts_with(GIT_LFS_PREFIX)
    }
    /// `output.assetFileNames` with its `[name]`, `[hash]`, `[ext]` and
    /// `[extname]` placeholders. CSS links its assets as `./file`, so the
    /// directory part of the pattern must be fixed.
    fn asset_file_name(&self, name: &str, extension: &str, bytes: &[u8]) -> anyhow::Result<String> {
        let pattern = self
            .request
            .output
            .asset_file_names
            .as_deref()
            .unwrap_or(DEFAULT_ASSET_FILE_NAMES);
        let (directory, file) = pattern.rsplit_once('/').unwrap_or(("", pattern));
        ensure!(
            !directory.contains('['),
            "assetFileNames with placeholders in directories is not supported: {pattern}"
        );
        let file = file
            .replace("[name]", &sanitize_name(name))
            .replace("[hash]", &short_hash(bytes))
            .replace("[extname]", &format!(".{extension}"))
            .replace("[ext]", extension);
        ensure!(
            !file.contains('['),
            "Unsupported assetFileNames placeholder: {pattern}"
        );
        Ok(if directory.is_empty() {
            file
        } else {
            format!("{directory}/{file}")
        })
    }
    pub fn new(
        request: Arc<BundleRequest>,
        mut options: CssOptions,
        state: CssState,
    ) -> anyhow::Result<Self> {
        if let Some(base) = &options.public_base {
            ensure!(
                base.starts_with('/') && !base.starts_with("//")
                    || base.starts_with("https://")
                    || base.starts_with("http://"),
                "CSS publicBase must be a root path or HTTP(S) URL"
            );
            ensure!(
                !base.contains(['?', '#', '\0', '\\', '\r', '\n']),
                "CSS publicBase contains invalid characters"
            );
        }
        ensure!(
            options
                .nonce
                .as_ref()
                .is_none_or(|nonce| !nonce.contains('\0')),
            "CSS nonce contains NUL"
        );
        if let Some(tailwind) = &mut options.tailwind {
            ensure!(
                tailwind.entries.len() <= 4096,
                "css.tailwind.entries may name at most 4096 CSS files"
            );
            let mut entries = BTreeSet::new();
            for entry in &mut tailwind.entries {
                *entry = absolute(&request.root, entry)?;
                ensure!(
                    entry.extension().is_some_and(|ext| ext == "css"),
                    "Native Tailwind entry must be a CSS file: {}",
                    entry.display()
                );
                if entry.exists() {
                    *entry = std::fs::canonicalize(&entry)?;
                }
                ensure!(
                    entries.insert(entry.clone()),
                    "Duplicate native Tailwind entry: {}",
                    entry.display()
                );
            }
            let mut versions = BTreeMap::new();
            for (path, version) in &tailwind.snapshot_versions {
                let path = absolute(&request.root, path)?;
                ensure!(
                    request.virtual_modules.contains_key(&path),
                    "Tailwind snapshot version requires a supplied virtual module: {}",
                    path.display()
                );
                ensure!(
                    versions.insert(path, *version).is_none(),
                    "Duplicate Tailwind snapshot version identity"
                );
            }
            tailwind.snapshot_versions = versions;
            tailwind.root = Some(absolute(
                &request.root,
                tailwind.root.as_deref().unwrap_or(&request.root),
            )?);
            for path in &mut tailwind.module_paths {
                *path = absolute(&request.root, path)?;
            }
        }
        Ok(Self {
            request,
            options,
            state,
        })
    }
    fn read_css(&self, path: &str) -> anyhow::Result<String> {
        if let Some(code) = self.request.virtual_modules.get(Path::new(path)) {
            ensure!(code.len() <= MAX_CSS, "CSS exceeds 4 MiB: {path}");
            return Ok(code.clone());
        }
        String::from_utf8(read_bounded(Path::new(path), MAX_CSS)?)
            .with_context(|| format!("CSS must be UTF-8: {path}"))
    }
    async fn resolve_css(
        &self,
        ctx: &PluginContext,
        from: &str,
        specifier: &str,
    ) -> anyhow::Result<ImportTarget> {
        if external_url(specifier) {
            return Ok(ImportTarget::External {
                url: specifier.into(),
            });
        }
        ensure!(
            !specifier.contains(['?', '#']),
            "Local CSS import query/fragment needs an explicit module policy: {specifier}"
        );
        let specifier = decode_path(specifier)?;
        let candidate = absolute(
            Path::new(from).parent().context("Missing CSS parent")?,
            Path::new(&specifier),
        )?;
        if self.request.virtual_modules.contains_key(&candidate) || candidate.is_file() {
            return Ok(ImportTarget::File {
                path: path_string(&candidate)?,
            });
        }
        let resolved = ctx
            .resolve(&specifier, Some(from), None)
            .await?
            .map_err(|error| {
                anyhow::anyhow!("CSS import {specifier:?} from {from} failed: {error:?}")
            })?;
        if resolved.external.is_external() {
            return Ok(ImportTarget::External {
                url: resolved.id.to_string(),
            });
        }
        ensure!(
            Path::new(resolved.id.as_str()).is_absolute(),
            "CSS resolution did not return a file: {}",
            resolved.id
        );
        Ok(ImportTarget::File {
            path: resolved.id.to_string(),
        })
    }
    async fn graph(
        &self,
        ctx: &PluginContext,
        origin: &str,
        code: String,
        modules: bool,
    ) -> anyhow::Result<BundleInput> {
        let mut input = BundleInput {
            entry: origin.into(),
            sources: BTreeMap::new(),
            imports: vec![],
            assets: vec![],
            options: Options {
                minify: self.request.minify,
                source_map: self.request.sourcemap,
                project_root: path_string(&self.request.root)?,
                css_modules: modules.then(ModulesOptions::default),
                ..Default::default()
            },
        };
        let mut queue = VecDeque::from([(origin.to_string(), code)]);
        let mut bytes = 0usize;
        while let Some((path, code)) = queue.pop_front() {
            if input.sources.contains_key(&path) {
                continue;
            }
            bytes = bytes
                .checked_add(code.len())
                .context("CSS graph size overflow")?;
            ensure!(
                bytes <= 32 * 1024 * 1024 && input.sources.len() < 4096,
                "CSS graph exceeds limits"
            );
            ctx.add_watch_file(&path);
            reject_uncompiled_tailwind(&path, &code)?;
            let imports = discover(&path, &code, modules)?;
            input.sources.insert(path.clone(), code);
            for specifier in imports {
                ensure!(input.imports.len() < 32768, "Too many CSS graph edges");
                let target = self.resolve_css(ctx, &path, &specifier).await?;
                if let ImportTarget::File { path: target } = &target {
                    ensure!(
                        target.ends_with(".css"),
                        "Native CSS preprocessing is unavailable for {target}"
                    );
                    if !input.sources.contains_key(target) {
                        queue.push_back((target.clone(), self.read_css(target)?));
                    }
                }
                input.imports.push(ImportResolution {
                    from: path.clone(),
                    specifier,
                    target,
                });
            }
        }
        Ok(input)
    }
    fn emit(
        &self,
        ctx: &PluginContext,
        file_name: &str,
        bytes: Vec<u8>,
        origin: &str,
    ) -> anyhow::Result<String> {
        let mut state = self
            .state
            .0
            .lock()
            .map_err(|_| anyhow::anyhow!("CSS state lock poisoned"))?;
        if let Some((previous, reference)) = state.emitted.get(file_name) {
            ensure!(
                *previous == bytes,
                "CSS asset output collision: {file_name}"
            );
            return Ok(reference.clone());
        }
        ensure!(
            state
                .asset_bytes
                .checked_add(bytes.len())
                .is_some_and(|size| size <= MAX_ASSETS),
            "CSS assets exceed 256 MiB"
        );
        let reference = ctx
            .emit_file(
                EmittedAsset {
                    file_name: Some(file_name.into()),
                    name: Path::new(file_name)
                        .file_name()
                        .map(|name| name.to_string_lossy().into_owned()),
                    original_file_name: Some(origin.into()),
                    source: bytes.clone().into(),
                },
                None,
                None,
            )?
            .to_string();
        state.asset_bytes += bytes.len();
        state
            .emitted
            .insert(file_name.into(), (bytes, reference.clone()));
        Ok(reference)
    }
    async fn compile(&self, ctx: &PluginContext, id: &str) -> anyhow::Result<HookLoadOutput> {
        let component = self
            .state
            .0
            .lock()
            .map_err(|_| anyhow::anyhow!("CSS state lock poisoned"))?
            .components
            .get(id)
            .cloned();
        let (origin, mut code, mut component_map) = if let Some(component) = component {
            (component.origin, component.code, component.map)
        } else {
            ensure!(
                !id.starts_with(COMPONENT_PREFIX),
                "Unknown component CSS virtual module"
            );
            (id.to_string(), self.read_css(id)?, None)
        };
        ensure!(
            !self.request.sourcemap || !id.starts_with(COMPONENT_PREFIX) || component_map.is_some(),
            "Extracted component CSS must include its original source map"
        );
        let selected = self.options.tailwind.as_ref().filter(|options| {
            let file = Path::new(&origin);
            let canonical = file.canonicalize().ok();
            options
                .entries
                .iter()
                .any(|entry| entry == file || canonical.as_ref() == Some(entry))
                || ((id.starts_with(COMPONENT_PREFIX)
                    || self.request.virtual_modules.contains_key(file))
                    && reject_uncompiled_tailwind(&origin, &code).is_err())
        });
        let mut tailwind_output = None;
        let mut tailwind_assets = BTreeMap::new();
        let mut tailwind_sources = None;
        if let Some(options) = selected {
            let host_request = ts_tailwind_host::HostRequest {
                root: options.root.clone().unwrap(),
                entry: PathBuf::from(&origin),
                plugins: options.plugins.clone(),
                aliases: options.aliases.clone(),
                module_paths: options.module_paths.clone(),
                limits: options.limits.clone(),
            };
            let snapshots = tailwind_snapshots(
                &self.request,
                options,
                id,
                &origin,
                &code,
                component_map.as_ref(),
            )?;
            let compiled = match ts_tailwind_host::compile::compile_project_with_snapshots(
                &host_request,
                &Default::default(),
                &snapshots,
            ) {
                Ok(compiled) => compiled,
                Err(error) => {
                    watch_tailwind(ctx, &error.watch);
                    return Err(error.into());
                }
            };
            watch_tailwind(ctx, &compiled.project.watch);
            let diagnostics = compiled
                .diagnostics
                .iter()
                .map(|diagnostic| {
                    let mut value = serde_json::to_value(diagnostic)
                        .expect("Tailwind diagnostics are serializable");
                    value["source"] = json!("ts-rust-native-tailwind");
                    value["location"] = serde_json::to_value(&diagnostic.source).unwrap();
                    value["id"] = json!(origin);
                    value
                })
                .collect::<Vec<_>>();
            self.state
                .0
                .lock()
                .map_err(|_| anyhow::anyhow!("CSS state lock poisoned"))?
                .diagnostics
                .extend(diagnostics.clone());
            ensure!(
                !compiled
                    .diagnostics
                    .iter()
                    .any(|diagnostic| diagnostic.severity == "error"),
                "Native Tailwind has unsupported candidates or incomplete CSS passes in {origin}; see structured Tailwind diagnostics"
            );
            tailwind_sources = Some(
                compiled.map["sources"]
                    .as_array()
                    .context("Tailwind map lacks source identities")?
                    .iter()
                    .map(|source| {
                        source
                            .as_str()
                            .context("Invalid Tailwind source identity")
                            .map(str::to_owned)
                    })
                    .collect::<anyhow::Result<Vec<_>>>()?,
            );
            tailwind_output = Some(TailwindModuleOutput {
                version: ts_tailwind::TAILWIND_VERSION.into(),
                generated_candidates: compiled.build.stylesheet.generated.clone(),
                invalid_candidates: compiled.build.stylesheet.invalid.clone(),
                diagnostics,
                watch: compiled.project.watch.clone(),
            });
            tailwind_assets = compiled
                .assets
                .into_iter()
                .map(|asset| (asset.token.clone(), asset))
                .collect();
            code = compiled.build.stylesheet.css;
            component_map = Some(compiled.map);
            // The host deliberately preserves CSS url(...) imports as browser
            // imports. They require a stylesheet-resource linker, not image/font
            // asset treatment. Quoted @import is resolved by the native host.
            for specifier in discover(&origin, &code, false)? {
                let token = specifier.split(['?', '#']).next().unwrap();
                ensure!(
                    !tailwind_assets.contains_key(token),
                    "Native Tailwind local @import url(...) needs stylesheet-resource linking; use a quoted @import for compile-time CSS imports: {origin}"
                );
            }
        } else if id.starts_with(COMPONENT_PREFIX)
            || self
                .request
                .virtual_modules
                .contains_key(Path::new(&origin))
        {
            reject_uncompiled_tailwind(&origin, &code).with_context(|| format!("Enable css.tailwind to process Tailwind directives in virtual/extracted CSS: {origin}"))?;
        }
        let input = self
            .graph(ctx, &origin, code, origin.ends_with(".module.css"))
            .await?;
        let bundle = ts_svelte_css::bundle(&input)?;
        let mut links = vec![];
        let mut seen = BTreeSet::new();
        let mut assets = BTreeSet::new();
        for dependency in &bundle.dependencies {
            if dependency["resolutionStatus"] == "external" {
                continue;
            }
            let from = dependency["loc"]["filePath"]
                .as_str()
                .context("CSS URL has no origin")?;
            let url = dependency["url"]
                .as_str()
                .context("CSS dependency has no URL")?;
            if !seen.insert((from.to_string(), url.to_string())) {
                continue;
            }
            let path_part = url.split(['?', '#']).next().unwrap();
            ensure!(
                !path_part.is_empty(),
                "Empty relative CSS URL cannot identify an asset"
            );
            let expected = tailwind_assets.get(path_part);
            let asset = if let Some(expected) = expected {
                PathBuf::from(&expected.path)
            } else {
                ensure!(
                    !path_part.starts_with("__ts_tailwind_asset_"),
                    "Unknown native Tailwind asset token: {path_part}"
                );
                absolute(
                    Path::new(from).parent().context("Missing asset parent")?,
                    Path::new(&decode_path(path_part)?),
                )?
            };
            let bytes = if let Some(expected) = expected {
                verified_tailwind_asset(expected)?
            } else {
                read_bounded(&asset, MAX_ASSET)?
            };
            ctx.add_watch_file(&path_string(&asset)?);
            let extension = asset.extension().and_then(|s| s.to_str()).unwrap_or("bin");
            ensure!(
                !extension.is_empty()
                    && extension.len() <= 24
                    && extension.bytes().all(|b| b.is_ascii_alphanumeric()),
                "Asset extension needs an explicit output policy: {}",
                asset.display()
            );
            let file = path_string(&asset)?;
            if self.should_inline(&file, url, &bytes) {
                links.push(AssetLink {
                    from: from.into(),
                    url: url.into(),
                    emitted_url: asset_data_url(&file, &bytes),
                });
                continue;
            }
            let stem = asset
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("asset");
            let name = self.asset_file_name(stem, &extension.to_ascii_lowercase(), &bytes)?;
            self.emit(ctx, &name, bytes, &path_string(&asset)?)?;
            links.push(AssetLink {
                from: from.into(),
                url: url.into(),
                emitted_url: format!("./{}", file_base(&name)),
            });
            assets.insert(name);
        }
        let mut linked = ts_svelte_css::link(&bundle, &links)?;
        ensure!(
            !tailwind_assets
                .keys()
                .any(|token| linked.code.contains(token)),
            "Native Tailwind asset tokens remain after CSS linking"
        );
        if let (Some(map), Some(component_map)) = (&linked.map, component_map.as_ref()) {
            linked.map = Some(compose_component(
                map,
                component_map,
                &origin,
                &self.request.root,
            )?);
        }
        // Maps are emitted beside CSS assets, not beside the original project.
        // Preserve resolvable original identities rather than making project
        // relative sources accidentally relative to the output assets folder.
        if let Some(map) = &mut linked.map {
            let sources = map["sources"]
                .as_array()
                .context("CSS map is missing sources")?
                .iter()
                .map(|source| {
                    let source = source
                        .as_str()
                        .context("CSS source identity must be a string")?;
                    if source.contains("://") {
                        Ok(source.to_string())
                    } else {
                        path_string(&absolute(&self.request.root, Path::new(source))?)
                    }
                })
                .collect::<anyhow::Result<Vec<_>>>()?;
            map["sources"] = json!(sources);
            *map = canonical_source_order(map)?;
        }
        let classes = class_exports(&linked.exports, &linked.references)?;
        let map_bytes = linked.map.as_ref().map(serde_json::to_vec).transpose()?;
        let mut identity = linked.code.as_bytes().to_vec();
        if let Some(map) = &map_bytes {
            identity.extend_from_slice(map);
        }
        let stem = Path::new(origin.split(['?', '#']).next().unwrap_or(&origin))
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("style");
        let file_name = self.asset_file_name(stem, "css", &identity)?;
        let map_file_name = if let Some(map) = map_bytes {
            let name = format!("{file_name}.map");
            self.emit(ctx, &name, map, &origin)?;
            linked
                .code
                .push_str(&format!("\n/*# sourceMappingURL={} */\n", file_base(&name)));
            Some(name)
        } else {
            None
        };
        let reference = self.emit(ctx, &file_name, linked.code.into_bytes(), &origin)?;
        ctx.associate_module_with_file_ref(id, &reference);
        self.state
            .0
            .lock()
            .map_err(|_| anyhow::anyhow!("CSS state lock poisoned"))?
            .output
            .modules
            .insert(
                id.into(),
                CssModuleOutput {
                    module_id: id.into(),
                    origin,
                    file_name: file_name.clone(),
                    map_file_name,
                    sources: tailwind_sources.unwrap_or(bundle.sources),
                    assets: assets.into_iter().collect(),
                    exports: linked.exports,
                    dependencies: linked.dependencies,
                    tailwind: tailwind_output,
                },
            );
        let url = if let Some(base) = &self.options.public_base {
            json!(format!("{}/{file_name}", base.trim_end_matches('/'))).to_string()
        } else {
            format!("import.meta.ROLLUP_FILE_URL_{reference}")
        };
        let code = proxy(&classes, self.request.target == Target::Client, &url)?;
        Ok(HookLoadOutput {
            code: code.into(),
            module_type: Some(ModuleType::Js),
            side_effects: Some(HookSideEffects::NoTreeshake),
            ..Default::default()
        })
    }
}
impl Plugin for CssPlugin {
    fn name(&self) -> Cow<'static, str> {
        "ts-rust-native-css".into()
    }
    fn register_hook_usage(&self) -> HookUsage {
        HookUsage::ResolveId | HookUsage::Load | HookUsage::GenerateBundle
    }
    async fn resolve_id(
        &self,
        _ctx: &PluginContext,
        args: &HookResolveIdArgs<'_>,
    ) -> HookResolveIdReturn {
        if args.specifier == RUNTIME_ID || args.specifier.starts_with(COMPONENT_PREFIX) {
            return Ok(Some(HookResolveIdOutput {
                id: args.specifier.into(),
                side_effects: Some(HookSideEffects::NoTreeshake),
                ..Default::default()
            }));
        }
        Ok(None)
    }
    async fn load(&self, ctx: SharedLoadPluginContext, args: &HookLoadArgs<'_>) -> HookLoadReturn {
        if args.id == RUNTIME_ID {
            return Ok(Some(HookLoadOutput {
                code: runtime(self.options.nonce.as_deref()).into(),
                module_type: Some(ModuleType::Js),
                ..Default::default()
            }));
        }
        if args.id.starts_with(COMPONENT_PREFIX) || args.id.ends_with(".css") {
            return self.compile(&ctx, args.id).await.inspect_err(|error| {
                self.state.0.lock().unwrap_or_else(|poisoned|poisoned.into_inner()).diagnostics.push(json!({"source":"ts-rust-native-css","code":"native_css_failed","severity":"error","id":args.id,"message":format!("{error:#}")}));
            }).map(Some);
        }
        if args
            .id
            .split_once('?')
            .is_some_and(|(path, _)| path.ends_with(".css"))
        {
            bail!(
                "CSS query imports need an explicit native policy: {}",
                args.id
            );
        }
        Ok(None)
    }
    async fn generate_bundle(
        &self,
        _ctx: &PluginContext,
        args: &mut HookGenerateBundleArgs<'_>,
    ) -> anyhow::Result<()> {
        let mut state = self
            .state
            .0
            .lock()
            .map_err(|_| anyhow::anyhow!("CSS state lock poisoned"))?;
        let mut graph = BTreeMap::new();
        let mut own = BTreeMap::new();
        let mut dynamic = BTreeMap::new();
        for output in args.bundle.iter() {
            if let Output::Chunk(chunk) = output {
                let name = chunk.filename.to_string();
                graph.insert(
                    name.clone(),
                    chunk
                        .imports
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>(),
                );
                dynamic.insert(
                    name.clone(),
                    chunk
                        .dynamic_imports
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>(),
                );
                let mut files = vec![];
                for id in &chunk.modules.keys {
                    if let Some(module) = state.output.modules.get(id.as_str()) {
                        if !files.contains(&module.file_name) {
                            files.push(module.file_name.clone());
                        }
                    }
                }
                own.insert(name, files);
            }
        }
        state.output.chunks = chunk_metadata(&graph, &dynamic, &own);
        Ok(())
    }
}

#[derive(Default)]
struct Imports(BTreeSet<String>);
impl<'i> Visitor<'i> for Imports {
    type Error = std::convert::Infallible;
    fn visit_types(&self) -> VisitTypes {
        VisitTypes::RULES | VisitTypes::PROPERTIES
    }
    fn visit_rule(&mut self, rule: &mut CssRule<'i>) -> Result<(), Self::Error> {
        if let CssRule::Import(import) = rule {
            self.0.insert(import.url.to_string());
        }
        rule.visit_children(self)
    }
    fn visit_property(&mut self, property: &mut Property<'i>) -> Result<(), Self::Error> {
        if let Property::Composes(composes) = property {
            if let Some(Specifier::File(specifier)) = &composes.from {
                self.0.insert(specifier.to_string());
            }
        }
        property.visit_children(self)
    }
}
fn discover(path: &str, source: &str, modules: bool) -> anyhow::Result<BTreeSet<String>> {
    let mut parsed = StyleSheet::parse(
        source,
        ParserOptions {
            filename: path.into(),
            css_modules: modules.then(Default::default),
            ..Default::default()
        },
    )
    .map_err(|error| anyhow::anyhow!("Invalid CSS {path}: {error}"))?;
    let mut visitor = Imports::default();
    parsed.visit(&mut visitor).unwrap();
    Ok(visitor.0)
}
fn watch_tailwind(ctx: &PluginContext, watch: &ts_tailwind_host::WatchSet) {
    for path in watch
        .files
        .iter()
        .chain(&watch.directories)
        .chain(&watch.missing)
    {
        ctx.add_watch_file(path);
    }
}
fn verified_tailwind_asset(
    asset: &ts_tailwind_host::compile::AssetReference,
) -> anyhow::Result<Vec<u8>> {
    let bytes = read_bounded(Path::new(&asset.path), MAX_ASSET)?;
    ensure!(
        bytes.len() as u64 == asset.byte_length && hash(&bytes) == asset.sha256,
        "Native Tailwind asset changed after compilation; retry the build: {}",
        asset.path
    );
    Ok(bytes)
}
fn snapshot_version(text: &str) -> i64 {
    (u64::from_be_bytes(Sha256::digest(text.as_bytes())[..8].try_into().unwrap())
        & ((1_u64 << 53) - 1)) as i64
}
fn tailwind_snapshots(
    request: &BundleRequest,
    options: &TailwindOptions,
    id: &str,
    origin: &str,
    css: &str,
    map: Option<&Value>,
) -> anyhow::Result<ts_tailwind_host::HostSnapshots> {
    let mut files = Vec::with_capacity(request.virtual_modules.len());
    for (path, text) in &request.virtual_modules {
        let path = absolute(&request.root, path)?;
        let map = if path.extension().is_some_and(|e| e == "css") {
            request
                .virtual_module_maps
                .get(&path)
                .map(|map| resolved_snapshot_map(map, &request.root))
                .transpose()?
        } else {
            None
        };
        files.push(ts_tailwind_host::FileSnapshot {
            version: options
                .snapshot_versions
                .get(&path)
                .copied()
                .unwrap_or_else(|| snapshot_version(text)),
            path,
            text: text.clone(),
            map,
        });
    }
    let entry = if id.starts_with(COMPONENT_PREFIX) {
        Some(ts_tailwind_host::EntryStylesheet {
            id: format!("{origin}?ts-rust-tailwind-style"),
            origin: origin.into(),
            version: options
                .snapshot_versions
                .get(Path::new(origin))
                .copied()
                .unwrap_or_else(|| snapshot_version(css)),
            css: css.into(),
            map: map
                .map(|map| resolved_snapshot_map(map, &request.root))
                .transpose()?,
        })
    } else {
        None
    };
    Ok(ts_tailwind_host::HostSnapshots {
        files,
        entry,
        resolver: if request.dev {
            ts_tailwind_host::ResolverProfile::ViteDevelopment
        } else {
            ts_tailwind_host::ResolverProfile::ViteProduction
        },
    })
}
fn resolved_snapshot_map(map: &Value, root: &Path) -> anyhow::Result<Value> {
    let parsed = parse_map(map)?;
    let mut map: Value = serde_json::from_str(&parsed.to_json_string())?;
    let sources = map["sources"]
        .as_array()
        .context("Snapshot map lacks sources")?
        .iter()
        .map(|source| {
            path_string(&absolute(
                root,
                Path::new(source.as_str().context("Invalid map source")?),
            )?)
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    map["sources"] = json!(sources);
    Ok(map)
}
fn reject_uncompiled_tailwind(path: &str, source: &str) -> anyhow::Result<()> {
    fn scan<'i, 't>(
        parser: &mut cssparser::Parser<'i, 't>,
        depth: usize,
    ) -> Result<(), cssparser::ParseError<'i, String>> {
        if depth > 256 {
            return Err(parser.new_custom_error("CSS nesting exceeds 256 levels"));
        }
        while !parser.is_exhausted() {
            let token = parser.next_including_whitespace_and_comments()?.clone();
            if let cssparser::Token::AtKeyword(name) = &token {
                if matches!(
                    name.to_ascii_lowercase().as_str(),
                    "tailwind"
                        | "theme"
                        | "source"
                        | "utility"
                        | "apply"
                        | "variant"
                        | "custom-variant"
                        | "reference"
                        | "config"
                        | "plugin"
                        | "screen"
                        | "responsive"
                        | "variants"
                ) {
                    return Err(parser.new_custom_error(format!("Uncompiled Tailwind directive @{name}; configure this real stylesheet in css.tailwind.entries")));
                }
            }
            if let cssparser::Token::Function(name) = &token {
                if matches!(
                    name.to_ascii_lowercase().as_str(),
                    "theme"
                        | "screen"
                        | "--theme"
                        | "--alpha"
                        | "--spacing"
                        | "--value"
                        | "--modifier"
                ) {
                    return Err(parser.new_custom_error(format!("Uncompiled Tailwind function {name}(); configure this real stylesheet in css.tailwind.entries")));
                }
            }
            if matches!(
                token,
                cssparser::Token::Function(_)
                    | cssparser::Token::ParenthesisBlock
                    | cssparser::Token::SquareBracketBlock
                    | cssparser::Token::CurlyBracketBlock
            ) {
                parser.parse_nested_block(|parser| scan(parser, depth + 1))?;
            }
        }
        Ok(())
    }
    let mut input = cssparser::ParserInput::new(source);
    scan(&mut cssparser::Parser::new(&mut input), 0)
        .map_err(|error| anyhow::anyhow!("{path}: {error:?}"))
}
fn external_url(url: &str) -> bool {
    url.starts_with(['#', '/'])
        || url.split_once(':').is_some_and(|(scheme, _)| {
            !scheme.is_empty()
                && scheme.starts_with(|c: char| c.is_ascii_alphabetic())
                && scheme
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'+' | b'-' | b'.'))
        })
}
fn decode_path(path: &str) -> anyhow::Result<String> {
    let mut bytes = Vec::with_capacity(path.len());
    let mut input = path.bytes();
    while let Some(byte) = input.next() {
        if byte == b'%' {
            let high = input
                .next()
                .and_then(|b| (b as char).to_digit(16))
                .context("Invalid percent-encoded asset path")?;
            let low = input
                .next()
                .and_then(|b| (b as char).to_digit(16))
                .context("Invalid percent-encoded asset path")?;
            bytes.push(((high << 4) | low) as u8);
        } else {
            bytes.push(byte);
        }
    }
    let decoded = String::from_utf8(bytes).context("Asset URL path is not UTF-8")?;
    ensure!(
        !decoded.contains(['\0', '\\']),
        "Asset URL contains invalid path characters"
    );
    Ok(decoded)
}
fn read_bounded(path: &Path, limit: usize) -> anyhow::Result<Vec<u8>> {
    let file = std::fs::File::open(path)
        .with_context(|| format!("Cannot read CSS graph file {}", path.display()))?;
    ensure!(
        file.metadata()?.is_file(),
        "CSS graph input is not a regular file: {}",
        path.display()
    );
    let mut bytes = vec![];
    file.take(limit as u64 + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= limit,
        "CSS graph file exceeds {limit} bytes: {}",
        path.display()
    );
    Ok(bytes)
}
fn path_string(path: &Path) -> anyhow::Result<String> {
    path.to_str()
        .map(str::to_owned)
        .context("CSS graph path must be UTF-8")
}
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
/// Vite `assetToDataURL`: SVG as URL-encoded text unless it holds text,
/// foreign objects or nested quotes, other files as base64 with their MIME type.
fn asset_data_url(file: &str, bytes: &[u8]) -> String {
    use base64::Engine as _;
    let base64 = base64::engine::general_purpose::STANDARD.encode(bytes);
    if file.ends_with(".svg") {
        let text = String::from_utf8_lossy(bytes);
        let nested_quotes = NESTED_QUOTES.with(|re| re.find(&text).is_some());
        if text.contains("<text") || text.contains("<foreignObject") || nested_quotes {
            return format!("data:image/svg+xml;base64,{base64}");
        }
        // `String.prototype.trim` also removes U+FEFF.
        let trimmed = text.trim_matches(|c: char| c.is_whitespace() || c == '\u{feff}');
        let tags = BETWEEN_TAGS.with(|re| replace_all(re, trimmed, "><"));
        let encoded = tags
            .replace('"', "'")
            .replace('%', "%25")
            .replace('#', "%23")
            .replace('<', "%3c")
            .replace('>', "%3e");
        let encoded = WHITESPACE.with(|re| replace_all(re, &encoded, "%20"));
        return format!("data:image/svg+xml,{encoded}");
    }
    let extension = Path::new(file)
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase);
    let mime = match extension.as_deref() {
        Some("apng") => "image/apng",
        Some("avif") => "image/avif",
        Some("bmp") => "image/bmp",
        Some("gif") => "image/gif",
        Some("ico") => "image/x-icon",
        Some("jpg" | "jpeg" | "jfif" | "pjpeg" | "pjp") => "image/jpeg",
        Some("png") => "image/png",
        Some("webp") => "image/webp",
        Some("otf") => "font/otf",
        Some("ttf") => "font/ttf",
        Some("woff") => "font/woff",
        Some("woff2") => "font/woff2",
        Some("eot") => "application/vnd.ms-fontobject",
        Some("mp4") => "video/mp4",
        Some("webm") => "video/webm",
        Some("mp3") => "audio/mpeg",
        Some("wav") => "audio/wav",
        Some("ogg") => "audio/ogg",
        _ => "application/octet-stream",
    };
    format!("data:{mime};base64,{base64}")
}
thread_local! {
    static NESTED_QUOTES: regress::Regex =
        regress::Regex::new(r#""[^"']*'[^"]*"|'[^'"]*"[^']*'"#).unwrap();
    static BETWEEN_TAGS: regress::Regex = regress::Regex::new(r">\s+<").unwrap();
    static WHITESPACE: regress::Regex = regress::Regex::new(r"\s+").unwrap();
}
/// `String.prototype.replaceAll` with a global regular expression.
fn replace_all(re: &regress::Regex, text: &str, with: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut last = 0;
    for found in re.find_iter(text) {
        result.push_str(&text[last..found.start()]);
        result.push_str(with);
        last = found.end();
    }
    result.push_str(&text[last..]);
    result
}
fn file_base(name: &str) -> &str {
    name.rsplit_once('/').map_or(name, |(_, base)| base)
}
/// Rollup's `sanitizeFileName` for a `[name]` placeholder.
fn sanitize_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c == '\0' || c == '?' || c == '*' || c == '/' || c == '\\' || c.is_control() {
                '_'
            } else {
                c
            }
        })
        .collect()
}
/// An 8-character content hash in Rollup's base64url alphabet.
fn short_hash(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let digest = Sha256::digest(bytes);
    let bits = digest[..6]
        .iter()
        .fold(0u64, |bits, byte| (bits << 8) | u64::from(*byte));
    (0..8)
        .map(|i| ALPHABET[((bits >> (42 - 6 * i)) & 63) as usize] as char)
        .collect()
}
fn class_exports(exports: &Value, references: &Value) -> anyhow::Result<BTreeMap<String, String>> {
    ensure!(
        references.is_null() || references.as_object().is_some_and(|refs| refs.is_empty()),
        "Cross-file CSS dashed identifier references are not yet linked"
    );
    let mut result = BTreeMap::new();
    if exports.is_null() {
        return Ok(result);
    }
    for (name, export) in exports
        .as_object()
        .context("CSS modules exports must be an object")?
    {
        let mut names = vec![
            export["name"]
                .as_str()
                .context("CSS export has no name")?
                .to_string(),
        ];
        for compose in export["composes"]
            .as_array()
            .context("CSS export has no composition list")?
        {
            ensure!(
                compose["type"] == "local" || compose["type"] == "global",
                "Unresolved CSS modules composition dependency"
            );
            names.push(
                compose["name"]
                    .as_str()
                    .context("CSS composition has no name")?
                    .into(),
            );
        }
        result.insert(name.clone(), names.join(" "));
    }
    Ok(result)
}
fn proxy(classes: &BTreeMap<String, String>, client: bool, url: &str) -> anyhow::Result<String> {
    let mut code = String::new();
    if client {
        code.push_str(&format!(
            "import {{load}} from {};\nawait load({url});\n",
            json!(RUNTIME_ID)
        ));
    }
    code.push_str(&format!(
        "const classes=JSON.parse({});\nexport default classes;\n",
        json!(serde_json::to_string(classes)?)
    ));
    for (index, name) in classes
        .keys()
        .filter(|name| name.as_str() != "default")
        .enumerate()
    {
        code.push_str(&format!(
            "const c{index}=classes[{}];export {{c{index} as {}}};\n",
            json!(name),
            json!(name)
        ));
    }
    Ok(code)
}
fn runtime(nonce: Option<&str>) -> String {
    format!(
        r#"const pending=new Map();
export function load(url){{
 if(typeof document==='undefined')return Promise.reject(new Error('CSS client output requires a document; use the server target for SSR'));
 const href=new URL(url,document.baseURI).href;
 if(pending.has(href))return pending.get(href);
 let link=[...document.querySelectorAll('link[rel="stylesheet"]')].find(link=>link.href===href&&!link.disabled&&(!link.media||link.media==='all'));
 if(link&&link.sheet)return Promise.resolve();
 const created=!link;if(created){{link=document.createElement('link');link.rel='stylesheet';link.href=href;const nonce={nonce};if(nonce!==null)link.nonce=nonce;}}
 const promise=new Promise((resolve,reject)=>{{
  const done=()=>{{clearTimeout(timer);link.removeEventListener('load',loaded);link.removeEventListener('error',failed);}};
  const loaded=()=>{{done();resolve();}};
  const failed=()=>{{done();pending.delete(href);if(created)link.remove();reject(new Error('Failed to load stylesheet '+href));}};
  const timer=setTimeout(failed,30000);link.addEventListener('load',loaded);link.addEventListener('error',failed);
  if(created)document.head.appendChild(link);
 }});pending.set(href,promise);return promise;
}}
"#,
        nonce = json!(nonce)
    )
}
fn chunk_metadata(
    graph: &BTreeMap<String, Vec<String>>,
    dynamic: &BTreeMap<String, Vec<String>>,
    own: &BTreeMap<String, Vec<String>>,
) -> BTreeMap<String, CssChunkOutput> {
    fn visit(
        id: &str,
        graph: &BTreeMap<String, Vec<String>>,
        own: &BTreeMap<String, Vec<String>>,
        seen: &mut BTreeSet<String>,
        styles: &mut Vec<String>,
    ) {
        if !seen.insert(id.into()) {
            return;
        }
        if let Some(dependencies) = graph.get(id) {
            for dependency in dependencies {
                visit(dependency, graph, own, seen, styles);
            }
        }
        if let Some(files) = own.get(id) {
            for file in files {
                if !styles.contains(file) {
                    styles.push(file.clone());
                }
            }
        }
    }
    let closure = |id: &str| {
        let mut result = vec![];
        visit(id, graph, own, &mut BTreeSet::new(), &mut result);
        result
    };
    own.iter()
        .map(|(id, styles)| {
            (
                id.clone(),
                CssChunkOutput {
                    own: styles.clone(),
                    imports: closure(id),
                    dynamic_imports: dynamic
                        .get(id)
                        .into_iter()
                        .flatten()
                        .filter(|id| own.contains_key(*id))
                        .map(|id| (id.clone(), closure(id)))
                        .collect(),
                },
            )
        })
        .collect()
}
fn parse_map(map: &Value) -> anyhow::Result<SourceMap> {
    let map = if let Some(text) = map.as_str() {
        serde_json::from_str(text)?
    } else {
        map.clone()
    };
    ensure!(
        map["version"] == 3
            && map.get("sections").is_none()
            && map
                .get("sourceRoot")
                .is_none_or(|root| root.is_null() || root == ""),
        "Expected flat CSS map version 3 with resolved sources and no sourceRoot"
    );
    Ok(SourceMap::from_json(serde_json::from_value(map)?)?)
}
/// lightningcss assigns bundle source indexes in parallel load order. Number
/// sources by first use in the output instead (unused ones after, by path),
/// so equal CSS always gets an equal map and file name.
fn canonical_source_order(map: &Value) -> anyhow::Result<Value> {
    let parsed = parse_map(map)?;
    let mut order = Vec::new();
    let mut seen = BTreeSet::new();
    for token in parsed.get_source_view_tokens() {
        if let Some(id) = token.get_source_id()
            && seen.insert(id)
        {
            order.push(id);
        }
    }
    let mut unused: Vec<u32> = (0..parsed.get_sources().count() as u32)
        .filter(|id| !seen.contains(id))
        .collect();
    unused.sort_by_key(|id| parsed.get_source(*id).map(|s| s.to_string()));
    order.extend(unused);
    if order
        .iter()
        .enumerate()
        .all(|(index, id)| index as u32 == *id)
    {
        return Ok(map.clone());
    }
    let mut builder = SourceMapBuilder::default();
    let mut ids = BTreeMap::new();
    for id in order {
        let source = parsed
            .get_source(id)
            .context("CSS map source index is invalid")?;
        let content = parsed
            .get_source_content(id)
            .context("CSS map lacks source content")?;
        ids.insert(id, builder.add_source_and_content(source, content));
    }
    for token in parsed.get_source_view_tokens() {
        let name = token.get_name().map(|name| builder.add_name(name));
        builder.add_token(
            token.get_dst_line(),
            token.get_dst_col(),
            token.get_src_line(),
            token.get_src_col(),
            token.get_source_id().map(|id| ids[&id]),
            name,
        );
    }
    let mut result: Value = serde_json::from_str(&builder.into_owned_sourcemap().to_json_string())?;
    if let Some(file) = map.get("file") {
        result["file"] = file.clone();
    }
    Ok(result)
}
fn compose_component(
    last: &Value,
    first: &Value,
    origin: &str,
    root: &Path,
) -> anyhow::Result<Value> {
    let first = parse_map(first)?;
    let last = parse_map(last)?;
    let lookup = first.generate_lookup_table();
    let source_paths = last
        .get_sources()
        .map(|source| absolute(root, Path::new(source)).and_then(|path| path_string(&path)))
        .collect::<anyhow::Result<Vec<_>>>()?;
    let mut builder = SourceMapBuilder::default();
    // Keep original documents even when Tailwind generated their declarations
    // without source anchors (for example inline-theme asset utilities).
    for (index, source) in source_paths.iter().enumerate() {
        if Path::new(source) == Path::new(origin) {
            for (index, source) in first.get_sources().enumerate() {
                let content = first
                    .get_source_content(index as u32)
                    .context("Input CSS map lacks original source content")?;
                builder.add_source_and_content(source, content);
            }
        } else {
            let content = last
                .get_source_content(index as u32)
                .context("CSS map lacks source content")?;
            builder.add_source_and_content(source, content);
        }
    }
    for token in last.get_source_view_tokens() {
        let Some(source_id) = token.get_source_id() else {
            builder.add_token(token.get_dst_line(), token.get_dst_col(), 0, 0, None, None);
            continue;
        };
        let source_path = source_paths
            .get(source_id as usize)
            .context("CSS map source index is invalid")?;
        if Path::new(source_path) == Path::new(origin) {
            let mapped = first
                .lookup_source_view_token(&lookup, token.get_src_line(), token.get_src_col())
                .filter(|mapped| {
                    mapped.get_dst_line() == token.get_src_line()
                        && mapped.get_source_id().is_some()
                });
            if let Some(mapped) = mapped {
                let id = mapped.get_source_id().unwrap();
                let source = first
                    .get_source(id)
                    .context("Component CSS map source is invalid")?;
                let content = first
                    .get_source_content(id)
                    .context("Component CSS map lacks original source content")?;
                let output_id = builder.add_source_and_content(source, content);
                let name = mapped
                    .get_name()
                    .or_else(|| token.get_name())
                    .map(|name| builder.add_name(name));
                builder.add_token(
                    token.get_dst_line(),
                    token.get_dst_col(),
                    mapped.get_src_line(),
                    mapped.get_src_col(),
                    Some(output_id),
                    name,
                );
            } else {
                builder.add_token(token.get_dst_line(), token.get_dst_col(), 0, 0, None, None);
            }
        } else {
            let content = last
                .get_source_content(source_id)
                .context("CSS map lacks original source content")?;
            let output_id = builder.add_source_and_content(source_path, content);
            let name = token.get_name().map(|name| builder.add_name(name));
            builder.add_token(
                token.get_dst_line(),
                token.get_dst_col(),
                token.get_src_line(),
                token.get_src_col(),
                Some(output_id),
                name,
            );
        }
    }
    Ok(serde_json::from_str(
        &builder.into_owned_sourcemap().to_json_string(),
    )?)
}

#[cfg(test)]
#[path = "css_tailwind_tests.rs"]
mod tailwind_tests;

#[cfg(test)]
mod tests {
    #[test]
    fn css_map_source_order_is_canonical() {
        // The same output mapped from a.css (line 0) then b.css (line 1),
        // with the bundler's source list in either order.
        let first = json!({"version":3,"sources":["/b.css","/a.css"],"sourcesContent":["b{}","a{}"],"names":[],"mappings":"ACAA;ADAA"});
        let second = json!({"version":3,"sources":["/a.css","/b.css"],"sourcesContent":["a{}","b{}"],"names":[],"mappings":"AAAA;ACAA"});
        let first = canonical_source_order(&first).unwrap();
        assert_eq!(first, canonical_source_order(&second).unwrap());
        assert_eq!(first["sources"], json!(["/a.css", "/b.css"]));
        assert_eq!(first["sourcesContent"], json!(["a{}", "b{}"]));
    }
    #[test]
    fn small_assets_inline_like_vite() {
        let request: BundleRequest = serde_json::from_value(json!({"root":"/app","entries":{"entry":"entry.js"},"target":"client","compatibility":{},"output":{"assetFileNames":"_app/immutable/assets/[name].[hash][extname]"}})).unwrap();
        let plugin = CssPlugin::new(
            Arc::new(request),
            CssOptions::default(),
            CssState::default(),
        )
        .unwrap();
        let svg = b"<svg xmlns='http://www.w3.org/2000/svg'>\n  <path d=\"M0 0\" fill=\"#000\"/>\n</svg>\n";
        assert!(plugin.should_inline("/app/x.svg", "x.svg", svg));
        assert!(!plugin.should_inline("/app/x.svg", "x.svg#icon", svg));
        assert!(!plugin.should_inline("/app/x.svg", "x.svg?no-inline", svg));
        assert!(plugin.should_inline("/app/x.png", "x.png?inline", &[0; 5000]));
        assert!(!plugin.should_inline("/app/x.png", "x.png", &[0; 4096]));
        assert!(plugin.should_inline("/app/x.png", "x.png", &[0; 4095]));
        assert_eq!(
            asset_data_url("/app/x.svg", svg),
            "data:image/svg+xml,%3csvg%20xmlns='http://www.w3.org/2000/svg'%3e%3cpath%20d='M0%200'%20fill='%23000'/%3e%3c/svg%3e"
        );
        assert!(
            asset_data_url("/app/t.svg", b"<svg><text>a</text></svg>")
                .starts_with("data:image/svg+xml;base64,")
        );
        assert_eq!(
            asset_data_url("/app/x.gif", b"GIF"),
            "data:image/gif;base64,R0lG"
        );
        let name = plugin
            .asset_file_name("Button.svelte", "css", b"a")
            .unwrap();
        assert!(
            name.starts_with("_app/immutable/assets/Button.svelte.") && name.ends_with(".css"),
            "{name}"
        );
        assert_eq!(file_base(&name).len(), "Button.svelte.".len() + 8 + 4);
    }
    use super::*;
    #[test]
    fn discovers_imports_and_modules_with_the_css_parser() {
        let imports=discover("/x.module.css",r#"@import './a.css' layer(a);.a{composes: base from './base.css';content:"@import './fake.css'"}"#,true).unwrap();
        assert_eq!(
            imports,
            BTreeSet::from(["./a.css".into(), "./base.css".into()])
        );
    }
    #[test]
    fn chunk_css_closure_preserves_order_and_dynamic_boundaries() {
        let graph = BTreeMap::from([
            ("entry".into(), vec!["shared".into()]),
            ("shared".into(), vec![]),
            ("lazy".into(), vec!["shared".into()]),
        ]);
        let own = BTreeMap::from([
            ("entry".into(), vec!["entry.css".into()]),
            ("shared".into(), vec!["shared.css".into()]),
            ("lazy".into(), vec!["lazy.css".into()]),
        ]);
        let result = chunk_metadata(
            &graph,
            &BTreeMap::from([("entry".into(), vec!["lazy".into()])]),
            &own,
        );
        assert_eq!(result["entry"].imports, vec!["shared.css", "entry.css"]);
        assert_eq!(
            result["entry"].dynamic_imports["lazy"],
            vec!["shared.css", "lazy.css"]
        );
    }
    #[test]
    fn urls_and_css_exports_preserve_real_identity() {
        assert_eq!(
            decode_path("../my%20picture%3F.svg").unwrap(),
            "../my picture?.svg"
        );
        assert!(decode_path("%Q0").is_err());
        assert!(decode_path("%00").is_err());
        for url in [
            "/public.svg",
            "//cdn.test/x",
            "#mask",
            "data:image/svg+xml,a",
            "https://example.test/a.css",
        ] {
            assert!(external_url(url));
        }
        assert!(!external_url("./x.svg?url#mask"));
        let classes=class_exports(&json!({"a":{"name":"hash_a","composes":[{"type":"local","name":"hash_b"},{"type":"global","name":"global"}]}}),&Value::Null).unwrap();
        assert_eq!(classes["a"], "hash_a hash_b global");
        assert!(
            class_exports(
                &json!({"a":{"name":"a","composes":[{"type":"dependency","name":"b"}]}}),
                &Value::Null
            )
            .is_err()
        );
        let client = proxy(&classes, true, "import.meta.ROLLUP_FILE_URL_abc").unwrap();
        assert!(client.contains("await load("));
        assert!(!proxy(&classes, false, "unused").unwrap().contains("await"));
    }
    #[test]
    fn component_registration_is_atomic_and_rejects_conflicting_snapshots() {
        let state = CssState::default();
        let id = state
            .register_component("/App.svelte", ".a{}".into(), None)
            .unwrap();
        assert!(id.starts_with(COMPONENT_PREFIX));
        assert_eq!(
            state
                .register_component("/App.svelte", ".a{}".into(), None)
                .unwrap(),
            id
        );
        assert!(
            state
                .register_component("/App.svelte", ".b{}".into(), None)
                .is_err()
        );
    }
    #[test]
    fn composition_retains_imported_css_and_unmapped_boundaries() {
        let first = json!({"version":3,"sources":["/src/App.svelte"],"sourcesContent":["<style>.a{color:red}</style>"],"names":[],"mappings":"AAAO,C"});
        let last = json!({"version":3,"sources":["src/App.svelte","src/import.css"],"sourcesContent":[".a{}",".b{}"],"names":[],"mappings":"AAAA,CAAC;ACAD"});
        let value = compose_component(&last, &first, "/src/App.svelte", Path::new("/")).unwrap();
        let map = parse_map(&value).unwrap();
        assert_eq!(map.get_sources().len(), 2);
        let tokens = map.get_source_view_tokens().collect::<Vec<_>>();
        assert_eq!(tokens[0].get_src_col(), 7);
        assert!(tokens[1].get_source_id().is_none());
        assert_eq!(tokens[2].get_source(), Some("/src/import.css"));
    }

    #[tokio::test]
    async fn bundler_emits_assets_and_keeps_static_dynamic_css_graphs() {
        use rolldown::{Bundler, BundlerOptions, InputItem, OutputFormat, Platform};
        use std::sync::atomic::{AtomicUsize, Ordering};
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "ts-svelte-css-plugin-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(root.join("nested")).unwrap();
        struct Fixture(std::path::PathBuf);
        impl Drop for Fixture {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _fixture = Fixture(root.clone());
        for (path, source) in [
            (
                "entry.js",
                "import './entry.css';export const lazy=()=>import('./lazy.js');",
            ),
            (
                "entry.css",
                "@import './nested/base.css' layer(base);.entry{color:red}",
            ),
            (
                "nested/base.css",
                ".base{background:url('./image.svg?q=1#icon')}",
            ),
            (
                "lazy.js",
                "import styles from './lazy.module.css';export default styles.primary;",
            ),
            (
                "lazy.module.css",
                ".primary{composes: base from './base.module.css';color:blue}",
            ),
            ("base.module.css", ".base{display:block}"),
            (
                "nested/image.svg",
                "<svg xmlns='http://www.w3.org/2000/svg'/>",
            ),
        ] {
            std::fs::write(root.join(path), source).unwrap();
        }
        let request:BundleRequest=serde_json::from_value(json!({"root":root,"entries":{"entry":"entry.js"},"target":"client","compatibility":{},"sourcemap":true})).unwrap();
        let state = CssState::default();
        let plugin =
            CssPlugin::new(Arc::new(request), CssOptions::default(), state.clone()).unwrap();
        let mut bundler = Bundler::with_plugins(
            BundlerOptions {
                cwd: Some(root.clone()),
                input: Some(vec![InputItem {
                    name: Some("entry".into()),
                    import: root.join("entry.js").to_string_lossy().into_owned(),
                }]),
                format: Some(OutputFormat::Esm),
                platform: Some(Platform::Browser),
                entry_filenames: Some("[name].js".to_string().into()),
                chunk_filenames: Some("chunks/[name]-[hash].js".to_string().into()),
                ..Default::default()
            },
            vec![Plugin::new_shared(plugin)],
        )
        .unwrap();
        let output = bundler.generate().await.unwrap();
        bundler.close().await.unwrap();
        let metadata = state.output().unwrap();
        assert_eq!(metadata.modules.len(), 2);
        let entry = metadata.chunks.get("entry.js").unwrap();
        assert_eq!(entry.imports.len(), 1);
        assert_eq!(entry.dynamic_imports.len(), 1);
        assert!(
            entry
                .dynamic_imports
                .values()
                .all(|files| files.len() == 1 && files[0] != entry.imports[0])
        );
        let css = output
            .assets
            .iter()
            .filter_map(|output| match output {
                Output::Asset(asset) if asset.filename.ends_with(".css") => {
                    Some(String::from_utf8(asset.source.as_bytes().to_vec()).unwrap())
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(css.len(), 2);
        assert!(
            css.iter()
                .any(|code| code.contains("?q=1#icon") && code.contains("@layer base"))
        );
        assert!(
            output
                .assets
                .iter()
                .any(|output| output.filename().ends_with(".svg"))
        );
        assert!(
            output
                .assets
                .iter()
                .filter_map(|output| match output {
                    Output::Chunk(chunk) => Some(&chunk.code),
                    _ => None,
                })
                .any(|code| code.contains("await "))
        );
    }
    /// Reduced from pdfjs-dist/web/pdf_viewer.css and @event-calendar/core:
    /// a relative url() in a custom property is an asset of the defining
    /// stylesheet (as in Vite), and multi-line unparsed values keep the
    /// emitted source map aligned with the emitted CSS.
    #[tokio::test]
    async fn custom_property_urls_and_multiline_values_link_and_map() {
        use rolldown::{Bundler, BundlerOptions, InputItem, OutputFormat, Platform};
        let root = std::env::temp_dir().join(format!(
            "ts-svelte-css-custom-property-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(root.join("web/images")).unwrap();
        struct Fixture(std::path::PathBuf);
        impl Drop for Fixture {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _fixture = Fixture(root.clone());
        let viewer = ":root {\n  --close-icon:url(images/close.svg);\n}\n.bar {\n  background-image:\n    linear-gradient(to top, var(--c) 1px, transparent 1px),\n    var(--close-icon);\n}\n.after {\n  color: red;\n}\n";
        for (path, source) in [
            ("entry.js", "import './web/viewer.css';"),
            ("web/viewer.css", viewer),
            (
                "web/images/close.svg",
                "<svg xmlns='http://www.w3.org/2000/svg'/>",
            ),
        ] {
            std::fs::write(root.join(path), source).unwrap();
        }
        let request: BundleRequest = serde_json::from_value(json!({"root":root,"entries":{"entry":"entry.js"},"target":"client","compatibility":{},"sourcemap":true})).unwrap();
        let plugin = CssPlugin::new(
            Arc::new(request),
            CssOptions {
                assets_inline_limit: Some(0),
                ..CssOptions::default()
            },
            CssState::default(),
        )
        .unwrap();
        let mut bundler = Bundler::with_plugins(
            BundlerOptions {
                cwd: Some(root.clone()),
                input: Some(vec![InputItem {
                    name: Some("entry".into()),
                    import: root.join("entry.js").to_string_lossy().into_owned(),
                }]),
                format: Some(OutputFormat::Esm),
                platform: Some(Platform::Browser),
                entry_filenames: Some("[name].js".to_string().into()),
                ..Default::default()
            },
            vec![Plugin::new_shared(plugin)],
        )
        .unwrap();
        let output = bundler.generate().await.unwrap();
        bundler.close().await.unwrap();
        let asset = |suffix: &str| {
            output
                .assets
                .iter()
                .find_map(|output| match output {
                    Output::Asset(asset) if asset.filename.ends_with(suffix) => Some(asset),
                    _ => None,
                })
                .unwrap_or_else(|| panic!("No {suffix} asset"))
        };
        let svg = asset(".svg").filename.to_string();
        let css = String::from_utf8(asset(".css").source.as_bytes().to_vec()).unwrap();
        let linked = format!(
            "--close-icon: url(\"./{}\")",
            svg.strip_prefix("assets/").unwrap()
        );
        assert!(css.contains(&linked), "{css}");
        let map = parse_map(&serde_json::from_slice(asset(".css.map").source.as_bytes()).unwrap())
            .unwrap();
        let generated = ts_svelte::Document::new(ts_svelte::Snapshot {
            uri: "out.css".into(),
            version: 0,
            text: css.clone(),
        });
        for token in map.get_source_view_tokens() {
            generated
                .offset_at(ts_svelte::Position {
                    line: token.get_dst_line(),
                    character: token.get_dst_col(),
                })
                .unwrap_or_else(|error| panic!("{error}: {css}"));
        }
        let after = generated.position_at(css.find(".after").unwrap()).unwrap();
        let token = map
            .get_source_view_tokens()
            .find(|token| {
                (token.get_dst_line(), token.get_dst_col()) == (after.line, after.character)
            })
            .unwrap_or_else(|| panic!("No mapping for .after: {css}"));
        assert!(token.get_source().unwrap().ends_with("/web/viewer.css"));
        assert_eq!((token.get_src_line(), token.get_src_col()), (8, 0));
    }
}
