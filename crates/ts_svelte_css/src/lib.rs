//! Snapshot-only native CSS bundling. Filesystem resolution and asset emission
//! belong to the host; no JavaScript, PostCSS or Tailwind engine is invoked.
use lightningcss::{
    bundler::{Bundler, ResolveResult, SourceProvider},
    css_modules::{Config, Pattern},
    dependencies::{Dependency, DependencyOptions},
    printer::PrinterOptions,
    stylesheet::{MinifyOptions, ParserOptions},
    targets::{Browsers, Targets},
};
use parcel_sourcemap::SourceMap;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Component, Path, PathBuf},
    sync::Mutex,
};

pub const MAX_REQUEST_BYTES: usize = 64 * 1024 * 1024;
mod link;
pub use link::{AssetLink, LinkedCss, link};
const MAX_SOURCE_BYTES: usize = 4 * 1024 * 1024;
const MAX_TOTAL_BYTES: usize = 32 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BundleInput {
    pub entry: String,
    pub sources: BTreeMap<String, String>,
    #[serde(default)]
    pub imports: Vec<ImportResolution>,
    #[serde(default)]
    pub assets: Vec<AssetResolution>,
    #[serde(default)]
    pub options: Options,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ImportResolution {
    pub from: String,
    pub specifier: String,
    pub target: ImportTarget,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum ImportTarget {
    File { path: String },
    External { url: String },
}
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AssetResolution {
    pub from: String,
    pub url: String,
    pub path: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields, default)]
pub struct Options {
    pub minify: bool,
    pub source_map: bool,
    pub project_root: String,
    pub css_modules: Option<ModulesOptions>,
    /// Explicit packed browser versions; no browserslist filesystem discovery.
    pub targets: BTreeMap<String, u32>,
}
impl Default for Options {
    fn default() -> Self {
        Self {
            minify: false,
            source_map: true,
            project_root: "/".into(),
            css_modules: None,
            targets: BTreeMap::new(),
        }
    }
}
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields, default)]
pub struct ModulesOptions {
    pub pattern: String,
    pub dashed_idents: bool,
    pub pure: bool,
}
impl Default for ModulesOptions {
    fn default() -> Self {
        Self {
            pattern: "[hash]_[local]".into(),
            dashed_idents: false,
            pure: false,
        }
    }
}
#[derive(Clone, Debug, Serialize)]
pub struct CssError {
    pub code: &'static str,
    pub message: String,
}
impl CssError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}
impl std::fmt::Display for CssError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for CssError {}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BundleOutput {
    pub code: String,
    pub map: Option<Value>,
    pub exports: Value,
    pub references: Value,
    pub dependencies: Vec<Value>,
    pub sources: Vec<String>,
    pub resolved_imports: Vec<Value>,
    /// Relative URL placeholders still require host asset emission/replacement.
    pub requires_asset_linking: bool,
    pub mapping_encoding: String,
}

fn validate_path(path: &str) -> Result<(), CssError> {
    if !path.starts_with('/')
        || path.contains(['\\', '\0', '?', '#'])
        || path.contains("//")
        || path.ends_with('/') && path != "/"
        || Path::new(path)
            .components()
            .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
        || path.contains("/./")
        || path.ends_with("/.")
    {
        return Err(CssError::new(
            "invalid_path",
            format!("Expected normalized absolute POSIX snapshot path: {path}"),
        ));
    }
    Ok(())
}

struct SnapshotProvider<'a> {
    input: &'a BundleInput,
    imports: BTreeMap<(&'a str, &'a str), &'a ImportTarget>,
    used: Mutex<BTreeSet<(String, String)>>,
}
impl SourceProvider for SnapshotProvider<'_> {
    type Error = CssError;
    fn read<'a>(&'a self, file: &Path) -> Result<&'a str, CssError> {
        self.input
            .sources
            .get(
                file.to_str()
                    .ok_or_else(|| CssError::new("invalid_path", "Non-UTF8 source path"))?,
            )
            .map(String::as_str)
            .ok_or_else(|| {
                CssError::new(
                    "missing_source",
                    format!("Snapshot has no source {}", file.display()),
                )
            })
    }
    fn resolve(&self, specifier: &str, from: &Path) -> Result<ResolveResult, CssError> {
        let from = from
            .to_str()
            .ok_or_else(|| CssError::new("invalid_path", "Non-UTF8 importer"))?;
        let resolution = self.imports.get(&(from, specifier)).ok_or_else(|| {
            CssError::new(
                "unresolved_import",
                format!("No explicit resolution for {specifier:?} from {from}"),
            )
        })?;
        self.used
            .lock()
            .map_err(|_| CssError::new("internal_error", "Resolution bookkeeping poisoned"))?
            .insert((from.into(), specifier.into()));
        Ok(match resolution {
            ImportTarget::File { path } => ResolveResult::File(PathBuf::from(path)),
            ImportTarget::External { url } => ResolveResult::External(url.clone()),
        })
    }
}

fn scan_tokens<'i, 't>(
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
                    | "apply"
                    | "theme"
                    | "utility"
                    | "source"
                    | "reference"
                    | "config"
                    | "plugin"
                    | "custom-variant"
                    | "variant"
                    | "screen"
                    | "responsive"
                    | "variants"
            ) {
                return Err(parser.new_custom_error(format!(
                    "Unsupported Tailwind directive @{name}; a native Tailwind compiler is required"
                )));
            }
        }
        if let cssparser::Token::Function(name) = &token {
            if name.eq_ignore_ascii_case("theme") || name.eq_ignore_ascii_case("screen") {
                return Err(
                    parser.new_custom_error(format!("Unsupported Tailwind function {name}()"))
                );
            }
        }
        if matches!(
            token,
            cssparser::Token::Function(_)
                | cssparser::Token::ParenthesisBlock
                | cssparser::Token::SquareBracketBlock
                | cssparser::Token::CurlyBracketBlock
        ) {
            parser.parse_nested_block(|nested| scan_tokens(nested, depth + 1))?;
        }
    }
    Ok(())
}

fn browser_targets(values: &BTreeMap<String, u32>) -> Result<Targets, CssError> {
    let mut browsers = Browsers::default();
    for (name, value) in values {
        if *value > 0x00ff_ffff {
            return Err(CssError::new(
                "invalid_options",
                "Browser versions must be 24-bit major.minor.patch values",
            ));
        }
        let field = match name.as_str() {
            "android" => &mut browsers.android,
            "chrome" => &mut browsers.chrome,
            "edge" => &mut browsers.edge,
            "firefox" => &mut browsers.firefox,
            "ie" => &mut browsers.ie,
            "ios_saf" => &mut browsers.ios_saf,
            "opera" => &mut browsers.opera,
            "safari" => &mut browsers.safari,
            "samsung" => &mut browsers.samsung,
            _ => {
                return Err(CssError::new(
                    "invalid_options",
                    format!("Unknown browser target {name}"),
                ));
            }
        };
        *field = Some(*value);
    }
    Ok(Targets {
        browsers: (!values.is_empty()).then_some(browsers),
        ..Default::default()
    })
}

pub fn bundle(input: &BundleInput) -> Result<BundleOutput, CssError> {
    validate_path(&input.entry)?;
    if input.entry == "/" {
        return Err(CssError::new(
            "invalid_path",
            "Entry must identify a CSS file",
        ));
    }
    validate_path(&input.options.project_root)?;
    if input.sources.len() > 4096 || input.imports.len() > 32768 || input.assets.len() > 32768 {
        return Err(CssError::new("size_limit", "Snapshot graph is too large"));
    }
    let mut total = 0usize;
    for (path, source) in &input.sources {
        validate_path(path)?;
        if path == "/" {
            return Err(CssError::new(
                "invalid_path",
                "Source must identify a CSS file",
            ));
        }
        total = total
            .checked_add(source.len())
            .ok_or_else(|| CssError::new("size_limit", "Snapshot size overflow"))?;
        if source.len() > MAX_SOURCE_BYTES || total > MAX_TOTAL_BYTES {
            return Err(CssError::new(
                "size_limit",
                "CSS source or total snapshot limit exceeded",
            ));
        }
        let mut tokens = cssparser::ParserInput::new(source);
        scan_tokens(&mut cssparser::Parser::new(&mut tokens), 0).map_err(|error| {
            CssError::new("unsupported_css_syntax", format!("{path}: {error:?}"))
        })?;
    }
    if !input.sources.contains_key(&input.entry) {
        return Err(CssError::new(
            "missing_source",
            "Entry is absent from source snapshot",
        ));
    }
    let mut imports = BTreeMap::new();
    for resolution in &input.imports {
        validate_path(&resolution.from)?;
        if !input.sources.contains_key(&resolution.from) {
            return Err(CssError::new(
                "missing_source",
                format!("Importer {} is absent", resolution.from),
            ));
        }
        match &resolution.target {
            ImportTarget::File { path } => {
                validate_path(path)?;
                if !input.sources.contains_key(path) {
                    return Err(CssError::new(
                        "missing_source",
                        format!("Resolved CSS source {path} is absent"),
                    ));
                }
            }
            ImportTarget::External { url } => {
                if url.is_empty() || url.contains('\0') {
                    return Err(CssError::new(
                        "invalid_resolution",
                        "External URL must be nonempty",
                    ));
                }
            }
        }
        if imports
            .insert(
                (resolution.from.as_str(), resolution.specifier.as_str()),
                &resolution.target,
            )
            .is_some()
        {
            return Err(CssError::new(
                "duplicate_resolution",
                "Duplicate CSS import resolution",
            ));
        }
    }
    let mut assets = BTreeMap::new();
    for asset in &input.assets {
        validate_path(&asset.from)?;
        validate_path(&asset.path)?;
        if !input.sources.contains_key(&asset.from) {
            return Err(CssError::new(
                "missing_source",
                "Asset origin is absent from source snapshot",
            ));
        }
        if assets
            .insert(
                (asset.from.as_str(), asset.url.as_str()),
                asset.path.as_str(),
            )
            .is_some()
        {
            return Err(CssError::new(
                "duplicate_resolution",
                "Duplicate asset resolution",
            ));
        }
    }
    let modules = input
        .options
        .css_modules
        .as_ref()
        .map(|options| {
            Ok::<_, CssError>(Config {
                pattern: Pattern::parse(&options.pattern)
                    .map_err(|error| CssError::new("invalid_options", error.to_string()))?,
                dashed_idents: options.dashed_idents,
                pure: options.pure,
                ..Default::default()
            })
        })
        .transpose()?;
    let targets = browser_targets(&input.options.targets)?;
    let provider = SnapshotProvider {
        input,
        imports,
        used: Mutex::new(BTreeSet::new()),
    };
    let mut map = input
        .options
        .source_map
        .then(|| SourceMap::new(&input.options.project_root));
    let mut stylesheet = {
        let mut bundler = Bundler::new(
            &provider,
            map.as_mut(),
            ParserOptions {
                css_modules: modules,
                error_recovery: false,
                ..Default::default()
            },
        );
        bundler
            .bundle(Path::new(&input.entry))
            .map_err(|error| CssError::new("css_bundle_error", error.to_string()))?
    };
    let sources = stylesheet.sources.clone();
    if input.options.minify || !input.options.targets.is_empty() {
        stylesheet
            .minify(MinifyOptions {
                targets,
                ..Default::default()
            })
            .map_err(|error| CssError::new("css_transform_error", error.to_string()))?;
    }
    let output = stylesheet
        .to_css(PrinterOptions {
            minify: input.options.minify,
            source_map: map.as_mut(),
            project_root: Some(&input.options.project_root),
            targets,
            analyze_dependencies: Some(DependencyOptions {
                remove_imports: false,
            }),
            ..Default::default()
        })
        .map_err(|error| CssError::new("css_print_error", error.to_string()))?;
    let mut dependencies = Vec::new();
    let mut requires_asset_linking = false;
    let mut placeholders = BTreeMap::new();
    for dependency in output.dependencies.unwrap_or_default() {
        requires_asset_linking = true;
        let mut value = serde_json::to_value(&dependency)
            .map_err(|error| CssError::new("serialization_error", error.to_string()))?;
        let (placeholder, origin, url) = match &dependency {
            Dependency::Url(url) => (
                url.placeholder.as_str(),
                url.loc.file_path.as_str(),
                url.url.as_str(),
            ),
            Dependency::Import(import) => (
                import.placeholder.as_str(),
                import.loc.file_path.as_str(),
                import.url.as_str(),
            ),
        };
        if let Some(previous) =
            placeholders.insert(placeholder.to_owned(), (origin.to_owned(), url.to_owned()))
        {
            if previous != (origin.to_owned(), url.to_owned()) {
                return Err(CssError::new(
                    "placeholder_collision",
                    "Distinct CSS URLs produced the same placeholder",
                ));
            }
        }
        if let Dependency::Url(url) = &dependency {
            let path = assets.get(&(url.loc.file_path.as_str(), url.url.as_str()));
            value["resolvedPath"] = json!(path);
            let external = url.url.starts_with(['#', '/'])
                || url.url.split_once(':').is_some_and(|(scheme, _)| {
                    !scheme.is_empty()
                        && scheme.starts_with(|c: char| c.is_ascii_alphabetic())
                        && scheme
                            .chars()
                            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
                });
            value["resolutionStatus"] = json!(if path.is_some() {
                "resolved"
            } else if external {
                "external"
            } else {
                "requiresHostResolution"
            });
            if external && path.is_none() {
                value["resolvedUrl"] = json!(url.url);
            }
        } else if let Dependency::Import(import) = &dependency {
            value["resolutionStatus"] = json!("external");
            value["resolvedUrl"] = json!(import.url);
        }
        dependencies.push(value);
    }
    let map=map.map(|mut map| {
        let mut mappings=Vec::new();map.write_vlq(&mut mappings).map_err(|error|CssError::new("source_map_error",format!("{error:?}")))?;
        Ok::<_,CssError>(json!({"version":3,"mappings":String::from_utf8(mappings).map_err(|error|CssError::new("source_map_error",error.to_string()))?,"sources":map.get_sources(),"sourcesContent":map.get_sources_content(),"names":map.get_names()}))
    }).transpose()?;
    let resolved_imports=provider.used.lock().map_err(|_|CssError::new("internal_error","Resolution bookkeeping poisoned"))?.iter().map(|(from,specifier)| {
        let target=provider.imports[&(from.as_str(),specifier.as_str())];
        json!({"from":from,"specifier":specifier,"target":match target {ImportTarget::File {path}=>json!({"kind":"file","path":path}),ImportTarget::External {url}=>json!({"kind":"external","url":url})}})
    }).collect();
    Ok(BundleOutput {
        code: output.code,
        map,
        exports: json!(output.exports),
        references: json!(output.references),
        dependencies,
        sources,
        resolved_imports,
        requires_asset_linking,
        mapping_encoding: "utf16-lightningcss-alpha72-patched".into(),
    })
}

#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "camelCase", deny_unknown_fields)]
enum Request {
    Capabilities {},
    Bundle {
        input: BundleInput,
    },
    Link {
        bundle: BundleOutput,
        links: Vec<AssetLink>,
    },
}
pub fn dispatch(bytes: &[u8]) -> Value {
    if bytes.len() > MAX_REQUEST_BYTES {
        return json!({"ok":false,"error":CssError::new("size_limit","Request exceeds 64 MiB")});
    }
    let request = match serde_json::from_slice::<Request>(bytes) {
        Ok(request) => request,
        Err(error) => {
            return json!({"ok":false,"error":CssError::new("invalid_request",error.to_string())});
        }
    };
    match request {
        Request::Capabilities {} => {
            json!({"ok":true,"result":{"backend":"lightningcss","version":"1.0.0-alpha.72","snapshotOnly":true,"assetUrls":"typed atomic linker with original source identity; host selects emitted URLs","assetLinking":true,"mappingEncoding":"utf16-lightningcss-alpha72-patched","cssModules":true,"sourceMaps":true,"tailwind":false,"postcssPlugins":false}})
        }
        Request::Bundle { input } => match bundle(&input) {
            Ok(output) => json!({"ok":true,"result":output}),
            Err(error) => json!({"ok":false,"error":error}),
        },
        Request::Link { bundle, links } => match link(&bundle, &links) {
            Ok(output) => json!({"ok":true,"result":output}),
            Err(error) => json!({"ok":false,"error":error}),
        },
    }
}

#[cfg(test)]
mod tests;
