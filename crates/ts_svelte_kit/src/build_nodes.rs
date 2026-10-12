//! Pure generation of server `nodes/N.js` modules for registered Kit 2/3.
//!
//! The host supplies ordered, resolved transitive dependencies (the result of
//! Kit's `find_deps` contract), emitted chunk paths, client node entry IDs, font
//! source filenames, and explicit page-analysis results. Paths in the returned
//! map are relative to the server output directory. No filesystem operations,
//! import resolution, application execution or bundler dependency occurs here.
//!
//! `PageOptions::Unknown` represents upstream `null` (analysis failed), while
//! `Absent` represents undefined. Neither invents analyzed page options. For
//! layouts, `child_page_options` must contain every child page's analysis result.
//! For other nodes it is `None`. Client dependencies preserve upstream Map/Set
//! insertion order; server-only dependencies deliberately retain duplicates.
//!
//! CSS URL adjustment belongs to the CSS processor. When enabled, inline CSS
//! containing `url(` requires resolved literal/assets/base segments for that
//! node. Plain CSS needs no adjustment. Segments are data, never JavaScript;
//! this module emits the actual `css(assets, base)` runtime function safely.
//!
//! Derived from SvelteKit exports/vite/build/build_server.js and utils.js (MIT).
//! Copyright (c) 2020 these people (https://github.com/sveltejs/kit/graphs/contributors)
//!
//! Permission is hereby granted, free of charge, to any person obtaining a copy
//! of this software and associated documentation files (the "Software"), to deal
//! in the Software without restriction, including without limitation the rights
//! to use, copy, modify, merge, publish, distribute, sublicense, and/or sell copies
//! of the Software, and to permit persons to whom the Software is furnished to do
//! so, subject to the following conditions:
//!
//! The above copyright notice and this permission notice shall be included in
//! all copies or substantial portions of the Software.
//!
//! THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
//! IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
//! FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
//! AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
//! LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
//! OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
//! SOFTWARE.

use crate::{build_manifest::RouterType, sync_versions::KitSyncVersion};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResolvedServerNodesInput {
    pub kit_version: String,
    pub router_type: RouterType,
    pub bundle_strategy: BundleStrategy,
    pub nodes: Vec<ResolvedServerNode>,
    /// Whether a client manifest exists, independently of any individual node.
    pub client_manifest_present: bool,
    /// Kit 3: emitted font file -> resolved original filename relative to root.
    /// Resolve package-manager symlinks before supplying these values.
    pub client_font_sources: BTreeMap<String, String>,
    /// Upstream threshold uses JS string length (UTF-16 units), strictly less.
    pub inline_style_threshold: usize,
    /// True when paths.assets is nonempty OR paths.relative is true.
    pub rewrite_inline_css_urls: bool,
    pub css_assets: Vec<CssAsset>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BundleStrategy {
    Split,
    Single,
    Inline,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResolvedServerNode {
    pub component: Option<ResolvedModule>,
    pub universal: Option<ResolvedModule>,
    pub server: Option<ResolvedModule>,
    pub page_options: PageOptions,
    pub child_page_options: Option<Vec<PageOptions>>,
    pub client: Option<ClientEntry>,
    /// CSS values after URL processing with THIS node's eager asset set.
    pub resolved_inline_css: BTreeMap<String, InlineCss>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "state",
    content = "values",
    rename_all = "camelCase",
    deny_unknown_fields
)]
pub enum PageOptions {
    Absent,
    Unknown,
    /// Actual JSON-serializable result of static page analysis, including load
    /// presence/entries/config if supplied. Non-static functions are Unknown.
    Known(BTreeMap<String, serde_json::Value>),
}
impl PageOptions {
    fn ssr(&self) -> Option<bool> {
        match self {
            Self::Known(v) => v.get("ssr").and_then(serde_json::Value::as_bool),
            _ => None,
        }
    }
    fn validate(&self) -> Result<(), String> {
        if let Self::Known(values) = self {
            for (key, value) in values {
                match key.as_str() {
                    "ssr" | "csr" if value.is_boolean() => {}
                    "prerender" if value.is_boolean() || value.as_str() == Some("auto") => {}
                    "trailingSlash"
                        if matches!(value.as_str(), Some("always" | "never" | "ignore")) => {}
                    "config" | "entries" | "load" => {}
                    _ => return Err(format!("Invalid resolved page option {key}: {value}")),
                }
            }
        }
        Ok(())
    }
    fn javascript(&self) -> String {
        match self {
            Self::Absent => "undefined".into(),
            Self::Unknown => "null".into(),
            Self::Known(v) => js(v),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResolvedModule {
    /// Original source identifier exported as universal_id/server_id.
    pub source_id: String,
    /// Emitted path relative to server output; may be absent only when upstream
    /// omits that runtime import because of statically analyzed SSR options.
    pub chunk: Option<String>,
    /// Required for component/universal modules when strategy is split.
    pub dependencies: Option<AssetDependencies>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClientEntry {
    /// Kit 2 generated/client-optimized/nodes/N.js, Kit 3
    /// generated/build/client-optimized/nodes/N.js, resolved by the host.
    pub source_id: String,
    pub dependencies: AssetDependencies,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AssetDependencies {
    pub imports: Vec<String>,
    pub stylesheets: Vec<String>,
    pub fonts: Vec<FontDependency>,
    /// Ordered equivalent of find_deps.stylesheet_map, including its initial
    /// importer grouping and at most one dynamic-import level in that map.
    pub stylesheet_map: Vec<StylesheetGroup>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum FontDependency {
    Kit2(String),
    Kit3 { file: String, filename: String },
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StylesheetGroup {
    pub source_id: String,
    pub css: Vec<String>,
    pub assets: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CssAsset {
    pub file: String,
    pub source: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "value",
    rename_all = "camelCase",
    deny_unknown_fields
)]
pub enum InlineCss {
    /// Includes explicit confirmation that an apparent url(...) requires no
    /// path rewriting, when its source contains only external/data URLs.
    Static(String),
    Dynamic(Vec<CssPart>),
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "value",
    rename_all = "camelCase",
    deny_unknown_fields
)]
pub enum CssPart {
    Literal(String),
    Assets,
    Base,
}

fn js<T: Serialize + ?Sized>(value: &T) -> String {
    serde_json::to_string(value)
        .expect("finite resolved metadata")
        .replace('\u{2028}', "\\u2028")
        .replace('\u{2029}', "\\u2029")
}
fn relative_output(file: &str) -> Result<(), String> {
    if file.is_empty()
        || file.starts_with('/')
        || file.contains(['\\', '\0', '?', '#'])
        || file.chars().any(char::is_control)
        || file
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err(format!("Invalid emitted relative output path {file:?}"));
    }
    Ok(())
}
fn chunk(module: &ResolvedModule) -> Result<String, String> {
    let file = module
        .chunk
        .as_deref()
        .ok_or_else(|| format!("Missing emitted chunk for {}", module.source_id))?;
    relative_output(file)?;
    Ok(js(&format!("../{file}")))
}
fn dependencies(module: &ResolvedModule) -> Result<&AssetDependencies, String> {
    module
        .dependencies
        .as_ref()
        .ok_or_else(|| format!("Missing split dependency metadata for {}", module.source_id))
}
fn push_unique(out: &mut Vec<String>, seen: &mut BTreeSet<String>, value: &str) {
    if seen.insert(value.into()) {
        out.push(value.into());
    }
}
fn validate_fonts(fonts: &[FontDependency], version: KitSyncVersion) -> Result<(), String> {
    for font in fonts {
        match font {
            FontDependency::Kit2(file) if !version.is_kit3() => relative_output(file)?,
            FontDependency::Kit3 { file, filename }
                if version.is_kit3() && !filename.is_empty() && !filename.contains('\0') =>
            {
                relative_output(file)?
            }
            _ => return Err("Font dependencies do not match the registered Kit version".into()),
        }
    }
    Ok(())
}
fn fonts_for_assets(
    assets: &[String],
    input: &ResolvedServerNodesInput,
    version: KitSyncVersion,
) -> Result<Vec<FontDependency>, String> {
    assets
        .iter()
        .filter(|file| {
            [".woff", ".woff2", ".ttf", ".otf"]
                .iter()
                .any(|ext| file.ends_with(ext))
        })
        .map(|file| {
            if version.is_kit3() {
                // The filename fallback is upstream's behavior only when the asset
                // has no source entry; the host supplies that resolution explicitly.
                let filename = input
                    .client_font_sources
                    .get(file)
                    .ok_or_else(|| format!("Missing Kit 3 font source resolution for {file}"))?;
                Ok(FontDependency::Kit3 {
                    file: file.clone(),
                    filename: filename.clone(),
                })
            } else {
                Ok(FontDependency::Kit2(file.clone()))
            }
        })
        .collect()
}
fn inline_expression(value: &InlineCss) -> String {
    match value {
        InlineCss::Static(source) => js(source),
        InlineCss::Dynamic(parts) => {
            let mut text = String::new();
            for part in parts {
                match part {
                    CssPart::Literal(s) => {
                        let encoded = js(s);
                        text.push_str(
                            &encoded[1..encoded.len() - 1]
                                .replace('`', "\\`")
                                .replace('$', "\\$"),
                        );
                    }
                    CssPart::Assets => text.push_str("${assets}"),
                    CssPart::Base => text.push_str("${base}"),
                }
            }
            format!("function css(assets, base) {{ return `{text}`; }}")
        }
    }
}

/// Emit node loaders and selected inline stylesheet modules, relative to the
/// server output directory. Existing files are neither read nor overwritten.
pub fn generate_server_nodes(
    input: &ResolvedServerNodesInput,
) -> Result<BTreeMap<PathBuf, String>, String> {
    let version = KitSyncVersion::parse(&input.kit_version)?;
    if input.nodes.len() > 100_000 {
        return Err("Server node inventory exceeds bound".into());
    }
    let mut inline_assets = BTreeMap::new();
    let mut asset_names = BTreeSet::new();
    for asset in &input.css_assets {
        relative_output(&asset.file)?;
        if !asset_names.insert(&asset.file) {
            return Err(format!("Duplicate CSS asset {}", asset.file));
        }
        if input.bundle_strategy == BundleStrategy::Split
            && asset.file.ends_with(".css")
            && asset.source.encode_utf16().count() < input.inline_style_threshold
        {
            inline_assets.insert(asset.file.as_str(), asset);
        }
    }
    let mut files = BTreeMap::new();
    let mut stylesheet_origins = BTreeMap::new();
    for (index, node) in input.nodes.iter().enumerate() {
        node.page_options.validate()?;
        if let Some(children) = &node.child_page_options {
            for options in children {
                options.validate()?;
            }
        }
        let uses_server_component = node.child_page_options.as_ref().map_or_else(
            || node.page_options.ssr() != Some(false),
            |children| children.iter().any(|o| o.ssr() != Some(false)),
        );
        let mut imports = Vec::new();
        let mut exports = vec![format!("export const index = {index};")];
        if let Some(component) = node.component.as_ref().filter(|_| uses_server_component) {
            exports.push("let component_cache;".into());
            exports.push(format!("export const component = async () => component_cache ??= (await import({})).default;", chunk(component)?));
        }
        if let Some(universal) = &node.universal {
            let static_options = (input.router_type == RouterType::Hash
                && !matches!(node.page_options, PageOptions::Unknown)
                && node.page_options.ssr().is_none())
                || node.page_options.ssr() == Some(false);
            if static_options {
                exports.push(format!(
                    "export const universal = {};",
                    node.page_options.javascript()
                ));
            } else {
                imports.push(format!("import * as universal from {};", chunk(universal)?));
                exports.push("export { universal };".into());
            }
            exports.push(format!(
                "export const universal_id = {};",
                js(&universal.source_id)
            ));
        }
        if let Some(server) = &node.server {
            imports.push(format!("import * as server from {};", chunk(server)?));
            exports.push("export { server };".into());
            exports.push(format!(
                "export const server_id = {};",
                js(&server.source_id)
            ));
        }
        let mut imported = Vec::new();
        let mut stylesheets = Vec::new();
        let mut fonts = Vec::new();
        if (node.component.is_some() || node.universal.is_some())
            && input.bundle_strategy == BundleStrategy::Split
        {
            let component = node.component.as_ref().map(dependencies).transpose()?;
            let universal = node.universal.as_ref().map(dependencies).transpose()?;
            if input.client_manifest_present {
                let client = node
                    .client
                    .as_ref()
                    .ok_or_else(|| format!("Missing client node entry {index}"))?;
                let mut seen_css = BTreeSet::new();
                let mut eager_assets = Vec::new();
                let mut seen_assets = BTreeSet::new();
                let mut seen_sources = BTreeSet::new();
                for group in &client.dependencies.stylesheet_map {
                    if !seen_sources.insert(&group.source_id) {
                        return Err(format!("Duplicate stylesheet-map key {}", group.source_id));
                    }
                    let source_id = if group.source_id == client.source_id {
                        node.component
                            .as_ref()
                            .map_or(group.source_id.as_str(), |m| m.source_id.as_str())
                    } else {
                        &group.source_id
                    };
                    let eager = component
                        .into_iter()
                        .chain(universal)
                        .any(|deps| deps.stylesheet_map.iter().any(|g| g.source_id == source_id));
                    if eager {
                        for file in &group.css {
                            push_unique(&mut stylesheets, &mut seen_css, file);
                        }
                        for file in &group.assets {
                            push_unique(&mut eager_assets, &mut seen_assets, file);
                        }
                    }
                }
                imported.clone_from(&client.dependencies.imports);
                fonts = fonts_for_assets(&eager_assets, input, version)?;
            } else {
                for entry in component.into_iter().chain(universal) {
                    imported.extend(entry.imports.iter().cloned());
                    stylesheets.extend(entry.stylesheets.iter().cloned());
                    fonts.extend(entry.fonts.iter().cloned());
                }
            }
        }
        for file in imported.iter().chain(&stylesheets) {
            relative_output(file)?;
        }
        validate_fonts(&fonts, version)?;
        exports.push(format!("export const imports = {};", js(&imported)));
        exports.push(format!("export const stylesheets = {};", js(&stylesheets)));
        exports.push(format!("export const fonts = {};", js(&fonts)));
        let mut inline_styles = Vec::new();
        for (style_index, file) in stylesheets.iter().enumerate() {
            let Some(asset) = inline_assets.get(file.as_str()) else {
                continue;
            };
            let basename = file
                .rsplit('/')
                .next()
                .expect("validated relative output path");
            let destination = PathBuf::from(format!("stylesheets/{basename}.js"));
            if let Some(previous) = stylesheet_origins.insert(destination.clone(), file.clone()) {
                if previous != *file {
                    return Err(format!(
                        "Inline stylesheet basename collision: {previous} and {file}"
                    ));
                }
            }
            let value = if let Some(resolved) = node.resolved_inline_css.get(file) {
                resolved.clone()
            } else if input.rewrite_inline_css_urls
                && asset.source.to_ascii_lowercase().contains("url(")
            {
                return Err(format!(
                    "Missing resolved inline CSS URLs for node {index}: {file}"
                ));
            } else {
                InlineCss::Static(asset.source.clone())
            };
            let module = format!(
                "// {basename}\nexport default {};\n",
                inline_expression(&value)
            );
            if let Some(previous) = files.insert(destination, module.clone()) {
                if previous != module {
                    return Err(format!(
                        "Inconsistent inline CSS URL resolutions for shared stylesheet {file}"
                    ));
                }
            }
            let name = format!("stylesheet_{style_index}");
            imports.push(format!(
                "import {name} from {};",
                js(&format!("../stylesheets/{basename}.js"))
            ));
            inline_styles.push(format!("\t{}: {name}", js(file)));
        }
        if !inline_styles.is_empty() {
            exports.push(format!(
                "export const inline_styles = () => ({{\n{}\n}});",
                inline_styles.join(",\n")
            ));
        }
        files.insert(
            PathBuf::from(format!("nodes/{index}.js")),
            format!("{}\n\n{}\n", imports.join("\n"), exports.join("\n")),
        );
    }
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn deps(source: &str, css: &str) -> AssetDependencies {
        AssetDependencies {
            imports: vec!["chunks/shared.js".into()],
            stylesheets: vec![css.into()],
            fonts: vec![FontDependency::Kit2("assets/font.woff2".into())],
            stylesheet_map: vec![StylesheetGroup {
                source_id: source.into(),
                css: vec![css.into()],
                assets: vec!["assets/font.woff2".into()],
            }],
        }
    }
    fn module(id: &str, chunk: &str, css: &str) -> ResolvedModule {
        ResolvedModule {
            source_id: id.into(),
            chunk: Some(chunk.into()),
            dependencies: Some(deps(id, css)),
        }
    }
    fn fixture() -> ResolvedServerNodesInput {
        ResolvedServerNodesInput {
            kit_version: "2.69.2".into(),
            router_type: RouterType::Pathname,
            bundle_strategy: BundleStrategy::Split,
            client_manifest_present: false,
            client_font_sources: BTreeMap::new(),
            inline_style_threshold: 0,
            rewrite_inline_css_urls: false,
            css_assets: vec![],
            nodes: vec![ResolvedServerNode {
                component: Some(module(
                    "src/+page.svelte",
                    "entries/pages/page.svelte.js",
                    "assets/page.css",
                )),
                universal: Some(module(
                    "src/+page.js",
                    "entries/pages/page.js",
                    "assets/universal.css",
                )),
                server: Some(module(
                    "src/+page.server.js",
                    "entries/pages/page.server.js",
                    "assets/server.css",
                )),
                page_options: PageOptions::Unknown,
                child_page_options: None,
                client: None,
                resolved_inline_css: BTreeMap::new(),
            }],
        }
    }
    fn node_code(input: &ResolvedServerNodesInput) -> String {
        generate_server_nodes(input).unwrap()[&PathBuf::from("nodes/0.js")].clone()
    }
    fn known_ssr(ssr: bool) -> PageOptions {
        PageOptions::Known(BTreeMap::from([(
            "ssr".into(),
            serde_json::Value::Bool(ssr),
        )]))
    }
    #[test]
    fn real_runtime_loaders_keep_async_component_cache_and_module_ids() {
        let code = node_code(&fixture());
        assert!(code.contains("export const index = 0;"));
        assert!(code.contains("export const component = async () => component_cache ??= (await import(\"../entries/pages/page.svelte.js\")).default;"));
        assert!(code.contains("import * as universal from \"../entries/pages/page.js\";"));
        assert!(code.contains("import * as server from \"../entries/pages/page.server.js\";"));
        assert!(code.contains("export const universal_id = \"src/+page.js\";"));
        assert!(code.contains("export const server_id = \"src/+page.server.js\";"));
        assert!(!code.contains("inline_styles"));
    }
    #[test]
    fn ssr_options_and_layout_children_control_only_the_correct_loaders() {
        let mut input = fixture();
        input.nodes[0].page_options = known_ssr(false);
        input.nodes[0].component.as_mut().unwrap().chunk = None;
        input.nodes[0].universal.as_mut().unwrap().chunk = None;
        let code = node_code(&input);
        assert!(!code.contains("component_cache"));
        assert!(code.contains("export const universal = {\"ssr\":false};"));
        assert!(code.contains("import * as server"));
        input.nodes[0].child_page_options = Some(vec![known_ssr(false), PageOptions::Unknown]);
        assert!(
            generate_server_nodes(&input)
                .unwrap_err()
                .contains("Missing emitted chunk")
        );
        input.nodes[0].component.as_mut().unwrap().chunk = Some("layout.js".into());
        assert!(node_code(&input).contains("component_cache"));
        input.nodes[0].child_page_options = Some(vec![]);
        assert!(!node_code(&input).contains("component_cache"));
    }
    #[test]
    fn hash_router_distinguishes_absent_unknown_and_known_options() {
        let mut input = fixture();
        input.router_type = RouterType::Hash;
        assert!(node_code(&input).contains("import * as universal"));
        input.nodes[0].page_options = PageOptions::Absent;
        assert!(node_code(&input).contains("export const universal = undefined;"));
        input.nodes[0].page_options = PageOptions::Known(BTreeMap::new());
        assert!(node_code(&input).contains("export const universal = {};"));
        input.nodes[0].page_options = known_ssr(true);
        assert!(node_code(&input).contains("import * as universal"));
    }
    #[test]
    fn client_eager_dependencies_use_original_component_ids_and_versioned_fonts() {
        let mut input = fixture();
        input.client_manifest_present = true;
        let entry = "generated/client-optimized/nodes/0.js";
        input.nodes[0].client = Some(ClientEntry {
            source_id: entry.into(),
            dependencies: AssetDependencies {
                imports: vec!["immutable/nodes/0.js".into(), "chunks/browser.js".into()],
                stylesheet_map: vec![
                    StylesheetGroup {
                        source_id: entry.into(),
                        css: vec!["assets/shared.css".into(), "assets/page.css".into()],
                        assets: vec!["assets/font.woff2".into(), "assets/logo.svg".into()],
                    },
                    StylesheetGroup {
                        source_id: "src/+page.js".into(),
                        css: vec!["assets/shared.css".into(), "assets/universal.css".into()],
                        assets: vec!["assets/font.woff2".into()],
                    },
                    StylesheetGroup {
                        source_id: "browser-only.js".into(),
                        css: vec!["assets/lazy.css".into()],
                        assets: vec!["assets/lazy.ttf".into()],
                    },
                ],
                ..AssetDependencies::default()
            },
        });
        let code = node_code(&input);
        assert!(code.contains("imports = [\"immutable/nodes/0.js\",\"chunks/browser.js\"]"));
        assert!(code.contains(
            "stylesheets = [\"assets/shared.css\",\"assets/page.css\",\"assets/universal.css\"]"
        ));
        assert!(code.contains("fonts = [\"assets/font.woff2\"]"));
        assert!(!code.contains("assets/lazy"));
        assert!(!code.contains("assets/logo"));
        input.kit_version = "3.0.1".into();
        assert!(
            generate_server_nodes(&input)
                .unwrap_err()
                .contains("font source")
        );
        input.client_font_sources.insert(
            "assets/font.woff2".into(),
            "node_modules/font/font.woff2".into(),
        );
        let code = node_code(&input);
        assert!(code.contains("fonts = [{\"file\":\"assets/font.woff2\",\"filename\":\"node_modules/font/font.woff2\"}]"));
        input.kit_version = "3.0.0-next.32".into();
        assert_eq!(node_code(&input), code);
    }
    #[test]
    fn server_only_dependencies_concatenate_and_single_bundles_export_empty_lists() {
        let mut input = fixture();
        let code = node_code(&input);
        assert!(code.contains("imports = [\"chunks/shared.js\",\"chunks/shared.js\"]"));
        assert!(code.contains("stylesheets = [\"assets/page.css\",\"assets/universal.css\"]"));
        assert!(!code.contains("assets/server.css"));
        for strategy in [BundleStrategy::Single, BundleStrategy::Inline] {
            input.bundle_strategy = strategy;
            input.nodes[0].component.as_mut().unwrap().dependencies = None;
            input.nodes[0].universal.as_mut().unwrap().dependencies = None;
            let code = node_code(&input);
            assert!(code.contains("export const imports = [];"));
            assert!(code.contains("export const stylesheets = [];"));
            assert!(code.contains("export const fonts = [];"));
            assert!(code.contains("component_cache"));
        }
    }
    #[test]
    fn inline_styles_use_strict_utf16_threshold_and_real_function_values() {
        let mut input = fixture();
        let source = "a{content:'😀'}";
        input.css_assets.push(CssAsset {
            file: "assets/page.css".into(),
            source: source.into(),
        });
        input.inline_style_threshold = source.encode_utf16().count();
        assert!(!node_code(&input).contains("inline_styles"));
        input.inline_style_threshold += 1;
        let files = generate_server_nodes(&input).unwrap();
        assert_eq!(
            files[&PathBuf::from("stylesheets/page.css.js")],
            format!("// page.css\nexport default {};\n", js(source))
        );
        assert!(files[&PathBuf::from("nodes/0.js")].contains("\"assets/page.css\": stylesheet_0"));
        input.inline_style_threshold = 1000;
        input.rewrite_inline_css_urls = true;
        input.css_assets[0].source = "a{background:url(./logo.svg)}".into();
        assert!(
            generate_server_nodes(&input)
                .unwrap_err()
                .contains("resolved inline CSS")
        );
        input.nodes[0].resolved_inline_css.insert(
            "assets/page.css".into(),
            InlineCss::Dynamic(vec![
                CssPart::Literal("a{background:url(".into()),
                CssPart::Assets,
                CssPart::Literal("/logo.svg)} b{background:url(".into()),
                CssPart::Base,
                CssPart::Literal("/favicon.ico)} /* `${not_code}` */".into()),
            ]),
        );
        let files = generate_server_nodes(&input).unwrap();
        let style = &files[&PathBuf::from("stylesheets/page.css.js")];
        assert!(style.contains("function css(assets, base)"));
        assert!(style.contains("${assets}/logo.svg"));
        assert!(style.contains("${base}/favicon.ico"));
        assert!(style.contains("\\`\\${not_code}\\`"));
    }
    #[test]
    fn incomplete_metadata_and_colliding_output_paths_fail_explicitly() {
        let mut input = fixture();
        input.nodes[0].component.as_mut().unwrap().dependencies = None;
        assert!(
            generate_server_nodes(&input)
                .unwrap_err()
                .contains("dependency metadata")
        );
        input = fixture();
        input.client_manifest_present = true;
        assert!(
            generate_server_nodes(&input)
                .unwrap_err()
                .contains("client node entry")
        );
        input = fixture();
        input.nodes[0].server.as_mut().unwrap().chunk = Some("../escape.js".into());
        assert!(
            generate_server_nodes(&input)
                .unwrap_err()
                .contains("relative output path")
        );
        input = fixture();
        input.inline_style_threshold = 100;
        input.nodes[0]
            .universal
            .as_mut()
            .unwrap()
            .dependencies
            .as_mut()
            .unwrap()
            .stylesheets = vec!["different/page.css".into()];
        input.css_assets = vec![
            CssAsset {
                file: "assets/page.css".into(),
                source: "a{}".into(),
            },
            CssAsset {
                file: "different/page.css".into(),
                source: "b{}".into(),
            },
        ];
        assert!(
            generate_server_nodes(&input)
                .unwrap_err()
                .contains("basename collision")
        );
    }
}
