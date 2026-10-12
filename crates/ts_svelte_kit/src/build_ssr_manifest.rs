//! Pure SSR manifest generation for registered Kit 2 and Kit 3 versions.
//!
//! Call `generate_ssr_manifest(&ResolvedSsrManifestInput)` after native bundling
//! and route analysis. The input supplies emitted endpoint/remotes paths, asset
//! byte sizes/MIME lookup, client dependency metadata and explicit route inclusion.
//! `included = false` means the host deliberately excluded that route (for example
//! after successful complete prerendering); merely appearing in prerendered_paths
//! never removes a dynamic route. Asset dependency lists must already include the
//! transitive imports of each server/universal node, endpoint and hooks.
//!
//! Returns an expression, an `export const manifest` module and old->new node
//! indices. No application/config JavaScript is evaluated; no filesystem is read
//! or written. `route_pattern` derives the real RegExp source and parameter
//! metadata from validated route syntax, including NFC and versioned escapes.
//!
//! Derived from SvelteKit core/generate_manifest and utils/routing (MIT):
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
use crate::build_manifest::ClientPage;
use crate::sync_versions::KitSyncVersion;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use unicode_normalization::UnicodeNormalization;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResolvedSsrManifestInput {
    pub kit_version: String,
    pub app_dir: String,
    pub app_path: String,
    /// Relative import base from the manifest module to the server output.
    pub relative_path: String,
    /// Full node inventory. Node loader file names retain these original indices.
    pub nodes: Vec<ServerNode>,
    /// Already sorted by the Kit inventory route comparator.
    pub routes: Vec<ServerRoute>,
    pub assets: Vec<StaticAsset>,
    pub service_worker: Option<String>,
    pub client: Option<ClientMetadata>,
    pub remotes: Vec<RemoteChunk>,
    pub prerendered_paths: Vec<String>,
    /// All Kit 2 matcher names, including those used only by excluded routes.
    pub matchers: Vec<String>,
    /// Kit 3: whether the resolved consolidated params entry exists.
    pub params_module_present: bool,
    /// Asset catalog from emitted server files, including byte size and MIME.
    pub server_assets: BTreeMap<String, ServerAsset>,
    /// Transitive assets imported by server AND universal hooks.
    pub hook_assets: Vec<String>,
    /// Host-resolved extension MIME lookup (keys include '.'). Needed for Kit 3
    /// extensions appearing only in prerendered output. Unknown values are ''.
    pub mime_lookup: BTreeMap<String, String>,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ServerNode {
    /// Transitive assets of this node's universal/server modules, not component.
    pub assets: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ServerRoute {
    pub id: String,
    pub page: Option<ClientPage>,
    pub endpoint: Option<Endpoint>,
    /// Explicit host selection, normally prerender_map.get(id) !== true.
    pub included: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Endpoint {
    pub chunk: String,
    pub assets: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StaticAsset {
    pub file: String,
    pub mime_type: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ServerAsset {
    pub size: u64,
    pub mime_type: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RemoteChunk {
    pub hash: String,
    pub chunk: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClientMetadata {
    pub start: String,
    pub app: Option<String>,
    pub imports: Vec<String>,
    /// None omits the property; null array elements represent JS undefined.
    pub nodes: Option<Vec<Option<String>>>,
    pub css: Option<Vec<Option<Vec<String>>>>,
    /// Full client route table, including successfully prerendered routes.
    pub routes: Option<Vec<ClientResolvedRoute>>,
    pub stylesheets: Vec<String>,
    pub fonts: Vec<ClientFont>,
    pub uses_env_dynamic_public: bool,
    pub inline: Option<InlineClient>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ClientFont {
    Kit2(String),
    Kit3 { file: String, filename: String },
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InlineClient {
    pub script: String,
    pub style: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClientResolvedRoute {
    pub id: String,
    pub errors: Vec<Option<usize>>,
    pub layouts: Vec<Option<(bool, usize)>>,
    pub leaf: (bool, usize),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RoutePattern {
    pub source: String,
    pub params: Vec<RouteParam>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RouteParam {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matcher: Option<String>,
    pub optional: bool,
    pub rest: bool,
    pub chained: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResolvedSsrManifestOutput {
    pub expression: String,
    pub module: String,
    pub node_indices: BTreeMap<usize, usize>,
    pub included_routes: Vec<String>,
}

fn js<T: Serialize + ?Sized>(value: &T) -> String {
    serde_json::to_string(value)
        .expect("finite manifest data serialization")
        .replace('\u{2028}', "\\u2028")
        .replace('\u{2029}', "\\u2029")
}
fn escaped(text: &str) -> String {
    let mut result = String::new();
    for ch in text.nfc() {
        match ch {
            '%' => result.push_str("%25"),
            '/' => result.push_str("%2[Ff]"),
            '?' => result.push_str("%3[Ff]"),
            '#' => result.push_str("%23"),
            '[' | ']' | '.' | '*' | '+' | '^' | '$' | '{' | '}' | '(' | ')' | '|' | '\\' => {
                result.push('\\');
                result.push(ch);
            }
            _ => result.push(ch),
        }
    }
    result
}
fn escape_code(content: &str, version: KitSyncVersion) -> Result<String, String> {
    let code = &content[2..];
    let valid_length = if content.starts_with("x+") {
        code.len() == 2
    } else {
        (4..=6).contains(&code.len())
    };
    if !valid_length
        || !code
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(format!("Invalid route character escape [{content}]"));
    }
    let mut number = u32::from_str_radix(code, 16).map_err(|e| e.to_string())?;
    if !version.is_kit3() {
        number &= 0xffff;
    }
    if (0xd800..=0xdfff).contains(&number) {
        return Ok(format!("\\u{number:04x}"));
    }
    let ch = char::from_u32(number).ok_or("Route character escape exceeds Unicode range")?;
    Ok(escaped(&ch.to_string()))
}

/// Compile Kit route syntax to RegExp source and ordered runtime parameters.
/// The generated manifest constructs RegExp using a quoted source, avoiding
/// source-code interpolation of route IDs or user parameter names.
pub fn route_pattern(id: &str, kit_version: &str) -> Result<RoutePattern, String> {
    let version = KitSyncVersion::parse(kit_version)?;
    if !id.starts_with('/') || id.contains(['\0', '#']) || id.contains("[[...") {
        return Err(format!("Invalid route ID {id:?}"));
    }
    // Adjacency is checked after character escapes are decoded by Kit. Two
    // escaped characters beside each other are not two dynamic parameters.
    let mut unescaped = String::new();
    let mut offset = 0;
    while let Some(start) = id[offset..].find('[').map(|n| offset + n) {
        unescaped.push_str(&id[offset..start]);
        if id[start..].starts_with("[x+") || id[start..].starts_with("[u+") {
            let end = id[start..].find(']').ok_or("Unbalanced route escape")? + start;
            let content = &id[start + 1..end];
            escape_code(content, version)?;
            let mut code = u32::from_str_radix(&content[2..], 16).map_err(|e| e.to_string())?;
            if !version.is_kit3() {
                code &= 0xffff;
            }
            // A surrogate cannot affect bracket adjacency.
            unescaped.push(char::from_u32(code).unwrap_or('\u{fffd}'));
            offset = end + 1;
        } else {
            unescaped.push('[');
            offset = start + 1;
        }
    }
    unescaped.push_str(&id[offset..]);
    if unescaped.contains("][") {
        return Err(format!("Adjacent parameters in route ID {id}"));
    }
    let mut source = String::from("^");
    let mut params = Vec::new();
    let mut names = BTreeSet::new();
    let mut previous_rest = false;
    let is_group = |s: &str| {
        s.starts_with('(') && s.ends_with(')') && s.len() > 2 && !s[1..s.len() - 1].contains(')')
    };
    if id == "/" || is_group(&id[1..]) {
        return Ok(RoutePattern {
            source: "^/$".into(),
            params,
        });
    }
    let segments = id[1..]
        .split('/')
        .filter(|s| !s.is_empty() && !is_group(s))
        .collect::<Vec<_>>();
    for segment in segments {
        if previous_rest && segment.starts_with("[[") {
            return Err("Optional route segment cannot follow rest segment".into());
        }
        previous_rest =
            segment.starts_with("[...") && segment.ends_with(']') && !segment.contains('=');
        let mut cursor = 0;
        let mut body = String::new();
        let mut segment_params = Vec::new();
        while let Some(start) = segment[cursor..].find('[').map(|p| p + cursor) {
            let literal = &segment[cursor..start];
            if literal.contains(']') {
                return Err(format!("Unbalanced route ID {id}"));
            }
            body.push_str(&escaped(literal));
            let optional = segment[start..].starts_with("[[");
            let opener = if optional { 2 } else { 1 };
            let closer = if optional { "]]" } else { "]" };
            let end = segment[start + opener..]
                .find(closer)
                .ok_or_else(|| format!("Unbalanced route ID {id}"))?
                + start
                + opener;
            let token_end = end + closer.len();
            let content = &segment[start + opener..end];
            if !optional && (content.starts_with("x+") || content.starts_with("u+")) {
                body.push_str(&escape_code(content, version)?);
                cursor = token_end;
                continue;
            }
            let rest = content.starts_with("...");
            if optional && rest {
                return Err("Rest route parameters cannot be double-optional".into());
            }
            let value = content.strip_prefix("...").unwrap_or(content);
            let (name, matcher) = value
                .split_once('=')
                .map_or((value, None), |(n, m)| (n, Some(m)));
            let valid = |name: &str| {
                !name.is_empty()
                    && name.bytes().all(|b| {
                        b.is_ascii_alphanumeric() || b == b'_' || (version.is_kit3() && b == b'-')
                    })
            };
            if !valid(name) || matcher.is_some_and(|m| !valid(m)) || !names.insert(name.to_owned())
            {
                return Err(format!("Invalid or duplicate route parameter {value:?}"));
            }
            let whole = start == 0 && token_end == segment.len();
            segment_params.push(RouteParam {
                name: name.into(),
                matcher: matcher.map(str::to_owned),
                optional,
                rest,
                chained: if rest { start == 0 } else { optional && whole },
            });
            body.push_str(if whole && rest {
                "(?:/([^]*))?"
            } else if whole && optional {
                "(?:/([^/]+))?"
            } else if rest {
                "([^]*?)"
            } else if optional {
                "([^/]*)?"
            } else {
                "([^/]+?)"
            });
            cursor = token_end;
        }
        if segment[cursor..].contains(']') {
            return Err(format!("Unbalanced route ID {id}"));
        }
        body.push_str(&escaped(&segment[cursor..]));
        let whole_optional = segment_params.len() == 1
            && segment_params[0].chained
            && (body == "(?:/([^]*))?" || body == "(?:/([^/]+))?");
        if !whole_optional {
            source.push('/');
        }
        source.push_str(&body);
        params.extend(segment_params);
    }
    source.push_str("/?$");
    Ok(RoutePattern { source, params })
}
fn regexp(pattern: &RoutePattern) -> String {
    format!("new RegExp({})", js(&pattern.source))
}

fn module_path(base: &str, file: &str) -> Result<String, String> {
    if base.starts_with('/')
        || file.starts_with('/')
        || base.contains(['\0', '\\', '?', '#'])
        || file.contains(['\0', '\\', '?', '#'])
        || file.is_empty()
    {
        return Err("Manifest imports must be relative POSIX paths without query/fragment".into());
    }
    let mut parts = Vec::new();
    for part in base.split('/').chain(file.split('/')) {
        match part {
            "" | "." => {}
            ".." => {
                if parts.last().is_some_and(|p| *p != "..") {
                    parts.pop();
                } else {
                    parts.push("..");
                }
            }
            _ => parts.push(part),
        }
    }
    let path = parts.join("/");
    Ok(if path.starts_with("../") {
        path
    } else {
        format!("./{path}")
    })
}
fn loader(base: &str, file: &str) -> Result<String, String> {
    Ok(format!(
        "__memo(() => import({}))",
        js(&module_path(base, file)?)
    ))
}
fn extension(file: &str) -> String {
    let name = file.rsplit('/').next().unwrap_or(file);
    if name == ".." {
        return String::new();
    }
    name.rfind('.')
        .filter(|index| *index > 0)
        .map_or(String::new(), |index| name[index..].to_owned())
}
fn remapped_array(indices: &[Option<usize>], map: &BTreeMap<usize, usize>) -> String {
    if indices.is_empty() {
        return "[]".into();
    }
    format!(
        "[{},]",
        indices
            .iter()
            .map(|i| i
                .and_then(|i| map.get(&i))
                .map_or(String::new(), usize::to_string))
            .collect::<Vec<_>>()
            .join(",")
    )
}
fn undefined_array<T>(values: &[Option<T>], render: impl Fn(&T) -> String) -> String {
    format!(
        "[{}]",
        values
            .iter()
            .map(|v| v.as_ref().map_or("void 0".into(), &render))
            .collect::<Vec<_>>()
            .join(",")
    )
}
fn client_metadata(
    client: &ClientMetadata,
    version: KitSyncVersion,
    name: &str,
) -> Result<String, String> {
    let mut fields = vec![
        format!("start:{}", js(&client.start)),
        format!("imports:{}", js(&client.imports)),
        format!("stylesheets:{}", js(&client.stylesheets)),
        format!("uses_env_dynamic_public:{}", client.uses_env_dynamic_public),
    ];
    if let Some(app) = &client.app {
        fields.push(format!("app:{}", js(app)));
    }
    if let Some(nodes) = &client.nodes {
        fields.push(format!("nodes:{}", undefined_array(nodes, js)));
    }
    if let Some(css) = &client.css {
        fields.push(format!("css:{}", undefined_array(css, js)));
    }
    let mut fonts = Vec::new();
    for font in &client.fonts {
        match font {
            ClientFont::Kit2(file) if !version.is_kit3() => fonts.push(js(file)),
            ClientFont::Kit3 { file, filename } if version.is_kit3() => {
                fonts.push(format!("{{file:{},filename:{}}}", js(file), js(filename)))
            }
            _ => return Err("Client font metadata does not match selected Kit version".into()),
        }
    }
    fields.push(format!("fonts:[{}]", fonts.join(",")));
    if let Some(inline) = &client.inline {
        fields.push(format!(
            "inline:{{script:{},style:{}}}",
            js(&inline.script),
            inline.style.as_ref().map_or("void 0".into(), js)
        ));
    }
    if let Some(routes) = &client.routes {
        let mut rendered = Vec::new();
        for route in routes {
            let pattern = route_pattern(&route.id, name)?;
            rendered.push(format!(
                "{{id:{},pattern:{},params:{},errors:{},layouts:{},leaf:{}}}",
                js(&route.id),
                regexp(&pattern),
                js(&pattern.params),
                undefined_array(&route.errors, js),
                undefined_array(&route.layouts, js),
                js(&route.leaf)
            ));
        }
        fields.push(format!("routes:[{}]", rendered.join(",")));
    }
    Ok(format!("{{{}}}", fields.join(",")))
}

pub fn generate_ssr_manifest(
    input: &ResolvedSsrManifestInput,
) -> Result<ResolvedSsrManifestOutput, String> {
    let version = KitSyncVersion::parse(&input.kit_version)?;
    if input.nodes.len() < 2 || input.nodes.len() > 100_000 || input.routes.len() > 100_000 {
        return Err(
            "SSR manifest requires root layout/error and bounded node/route inventories".into(),
        );
    }
    if version.is_kit3() && !input.matchers.is_empty() {
        return Err("Kit 3 uses consolidated params, not individual matchers".into());
    }
    if !version.is_kit3() && input.params_module_present {
        return Err("Kit 2 uses individual matcher modules".into());
    }
    let mut patterns = Vec::new();
    let mut ids = BTreeSet::new();
    let mut used = BTreeSet::from([0, 1]);
    let mut asset_files = BTreeSet::new();
    for route in &input.routes {
        if !ids.insert(&route.id) {
            return Err(format!("Duplicate manifest route {}", route.id));
        }
        patterns.push(route_pattern(&route.id, &input.kit_version)?);
        if let Some(page) = &route.page {
            for index in std::iter::once(page.leaf)
                .chain(page.layouts.iter().chain(&page.errors).flatten().copied())
            {
                if index >= input.nodes.len() {
                    return Err(format!(
                        "Route {} references missing node {index}",
                        route.id
                    ));
                }
                if route.included {
                    used.insert(index);
                }
            }
        }
        if route.included {
            if let Some(endpoint) = &route.endpoint {
                asset_files.extend(endpoint.assets.iter().cloned());
            }
        }
    }
    for index in &used {
        asset_files.extend(input.nodes[*index].assets.iter().cloned());
    }
    asset_files.extend(input.hook_assets.iter().cloned());
    let node_indices = used
        .iter()
        .enumerate()
        .map(|(new, old)| (*old, new))
        .collect::<BTreeMap<_, _>>();
    let nodes = used
        .iter()
        .map(|i| loader(&input.relative_path, &format!("nodes/{i}.js")))
        .collect::<Result<Vec<_>, _>>()?
        .join(",\n");
    let mut assets = input
        .assets
        .iter()
        .map(|a| a.file.clone())
        .collect::<Vec<_>>();
    if let Some(worker) = &input.service_worker {
        assets.push(worker.clone());
    }
    let mut mime_types = BTreeMap::new();
    for asset in &input.assets {
        if let Some(mime) = asset.mime_type.as_ref().filter(|m| !m.is_empty()) {
            mime_types.insert(extension(&asset.file), mime.clone());
        }
    }
    let mut server_assets = BTreeMap::new();
    for file in asset_files {
        let asset = input
            .server_assets
            .get(&file)
            .ok_or_else(|| format!("Missing resolved server asset metadata for {file}"))?;
        if asset.size > 9_007_199_254_740_991 {
            return Err(format!(
                "Server asset size exceeds JavaScript exact integer range: {file}"
            ));
        }
        let ext = extension(&file);
        mime_types.entry(ext.clone()).or_insert_with(|| {
            asset
                .mime_type
                .clone()
                .or_else(|| input.mime_lookup.get(&ext).cloned())
                .unwrap_or_default()
        });
        server_assets.insert(file, asset.size);
    }
    if version.is_kit3() {
        for path in &input.prerendered_paths {
            let ext = extension(path);
            if !ext.is_empty() {
                mime_types
                    .entry(ext.clone())
                    .or_insert_with(|| input.mime_lookup.get(&ext).cloned().unwrap_or_default());
            }
        }
    }
    let client = input
        .client
        .as_ref()
        .map(|c| client_metadata(c, version, &input.kit_version))
        .transpose()?
        .unwrap_or("null".into());
    let mut matcher_names = Vec::new();
    if !version.is_kit3() && input.client.as_ref().is_some_and(|c| c.nodes.is_some()) {
        for name in &input.matchers {
            if !matcher_names.contains(name) {
                matcher_names.push(name.clone());
            }
        }
    }
    let mut rendered_routes = Vec::new();
    let mut included_routes = Vec::new();
    for (route, pattern) in input
        .routes
        .iter()
        .zip(&patterns)
        .filter(|(r, _)| r.included && (r.page.is_some() || r.endpoint.is_some()))
    {
        if !version.is_kit3() {
            for matcher in pattern.params.iter().filter_map(|p| p.matcher.as_ref()) {
                if !input.matchers.contains(matcher) {
                    return Err(format!(
                        "Missing resolved matcher {matcher:?} for {}",
                        route.id
                    ));
                }
                if !matcher_names.contains(matcher) {
                    matcher_names.push(matcher.clone());
                }
            }
        }
        let page = route.page.as_ref().map_or("null".into(), |page| {
            format!(
                "{{layouts:{},errors:{},leaf:{}}}",
                remapped_array(&page.layouts, &node_indices),
                remapped_array(&page.errors, &node_indices),
                node_indices[&page.leaf]
            )
        });
        let endpoint = route
            .endpoint
            .as_ref()
            .map(|endpoint| loader(&input.relative_path, &endpoint.chunk))
            .transpose()?
            .unwrap_or("null".into());
        rendered_routes.push(format!(
            "{{id:{},pattern:{},params:{},page:{page},endpoint:{endpoint}}}",
            js(&route.id),
            regexp(pattern),
            js(&pattern.params)
        ));
        included_routes.push(route.id.clone());
    }
    let mut remotes = Vec::new();
    let mut remote_hashes = BTreeSet::new();
    for remote in &input.remotes {
        if remote.hash.is_empty() || !remote_hashes.insert(&remote.hash) {
            return Err("Remote manifest hash is empty or duplicated".into());
        }
        remotes.push(format!(
            "[{}]:{}",
            js(&remote.hash),
            loader(&input.relative_path, &remote.chunk)?
        ));
    }
    let matchers = if version.is_kit3() {
        if input.params_module_present
            && patterns
                .iter()
                .any(|p| p.params.iter().any(|p| p.matcher.is_some()))
        {
            format!(
                "const {{ params }} = await import({}); return params;",
                js(&module_path(&input.relative_path, "entries/params.js")?)
            )
        } else {
            "return {};".into()
        }
    } else {
        let mut imports = Vec::new();
        let mut fields = Vec::new();
        for (i, matcher) in matcher_names.iter().enumerate() {
            if matcher.is_empty()
                || !matcher
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_')
            {
                return Err(format!("Invalid Kit 2 matcher name {matcher:?}"));
            }
            imports.push(format!(
                "const {{ match: matcher_{i} }} = await import({});",
                js(&module_path(
                    &input.relative_path,
                    &format!("entries/matchers/{matcher}.js")
                )?)
            ));
            fields.push(format!("[{}]:matcher_{i}", js(matcher)));
        }
        format!("{} return {{{}}};", imports.join("\n"), fields.join(","))
    };
    let runtime = format!(
        "client:{client},\nnodes:[{nodes}],\nremotes:{{{}}},\nroutes:[{}],\nprerendered_routes:new Set({}),\nmatchers:async () => {{{matchers}}},\nserver_assets:{}",
        remotes.join(","),
        rendered_routes.join(",\n"),
        js(&input.prerendered_paths),
        js(&server_assets)
    );
    let manifest = if version.is_kit3() {
        format!(
            "{{app_dir:{},app_path:{},assets:new Set({}),mime_types:{},{runtime}}}",
            js(&input.app_dir),
            js(&input.app_path),
            js(&assets),
            js(&mime_types)
        )
    } else {
        format!(
            "{{appDir:{},appPath:{},assets:new Set({}),mimeTypes:{},_:{{{runtime}}}}}",
            js(&input.app_dir),
            js(&input.app_path),
            js(&assets),
            js(&mime_types)
        )
    };
    let expression = format!(
        "(() => {{\nfunction __memo(fn) {{ let value; return () => value ??= (value = fn()); }}\nreturn {manifest};\n}})()"
    );
    let module = format!("export const manifest = {expression};\n");
    Ok(ResolvedSsrManifestOutput {
        expression,
        module,
        node_indices,
        included_routes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> ResolvedSsrManifestInput {
        ResolvedSsrManifestInput {
            kit_version: "2.69.2".into(),
            app_dir: "_app".into(),
            app_path: "base/_app".into(),
            relative_path: "./server".into(),
            nodes: (0..5).map(|_| ServerNode::default()).collect(),
            routes: vec![ServerRoute {
                id: "/[id=integer]".into(),
                page: Some(ClientPage {
                    leaf: 4,
                    layouts: vec![Some(0), None, Some(2), None],
                    errors: vec![Some(1), None],
                }),
                endpoint: Some(Endpoint {
                    chunk: "entries/endpoints/id.js".into(),
                    assets: vec!["assets/schema.json".into()],
                }),
                included: true,
            }],
            assets: vec![StaticAsset {
                file: "favicon.png".into(),
                mime_type: Some("image/png".into()),
            }],
            service_worker: Some("service-worker.js".into()),
            client: None,
            remotes: vec![RemoteChunk {
                hash: "abc".into(),
                chunk: "chunks/remote-abc.js".into(),
            }],
            prerendered_paths: vec!["/static.ico".into()],
            matchers: vec!["integer".into()],
            params_module_present: false,
            server_assets: BTreeMap::from([(
                "assets/schema.json".into(),
                ServerAsset {
                    size: 21,
                    mime_type: Some("application/json".into()),
                },
            )]),
            hook_assets: vec![],
            mime_lookup: BTreeMap::from([(".ico".into(), "image/x-icon".into())]),
        }
    }
    #[test]
    fn route_patterns_preserve_captures_and_chaining() {
        let p =
            route_pattern("/(group)/[[lang=language]]/prefix-[id]/[...rest]", "2.69.2").unwrap();
        assert_eq!(p.source, "^(?:/([^/]+))?/prefix-([^/]+?)(?:/([^]*))?/?$");
        assert_eq!(
            p.params
                .iter()
                .map(|p| (p.name.as_str(), p.optional, p.rest, p.chained))
                .collect::<Vec<_>>(),
            vec![
                ("lang", true, false, true),
                ("id", false, false, false),
                ("rest", false, true, true)
            ]
        );
        assert_eq!(route_pattern("/", "2.69.2").unwrap().source, "^/$");
        assert_eq!(route_pattern("/(one)", "2.69.2").unwrap().source, "^/$");
        assert_eq!(
            route_pattern("/(one)/(two)", "2.69.2").unwrap().source,
            "^/?$"
        );
        assert!(route_pattern("/[one][two]", "2.69.2").is_err());
        assert!(route_pattern("/[[...rest]]", "2.69.2").is_err());
    }
    #[test]
    fn unicode_normalization_and_versioned_escape_semantics_are_exact() {
        assert_eq!(
            route_pattern("/cafe\u{301}/[x+2f]", "3.0.1")
                .unwrap()
                .source,
            "^/café/%2[Ff]/?$"
        );
        assert_eq!(
            route_pattern("/[u+1f600]", "3.0.1").unwrap().source,
            "^/😀/?$"
        );
        assert_eq!(
            route_pattern("/[u+1f600]", "2.69.2").unwrap().source,
            format!("^/{}/?$", '\u{f600}')
        );
        assert!(route_pattern("/[name-with-hyphen]", "2.69.2").is_err());
        assert!(route_pattern("/[name-with-hyphen]", "3.0.1").is_ok());
        assert_eq!(
            route_pattern("/100%/[x+3f]", "3.0.1").unwrap().source,
            "^/100%25/%3[Ff]/?$"
        );
        assert_eq!(
            route_pattern("/[x+61][x+62]", "3.0.1").unwrap().source,
            "^/ab/?$"
        );
        assert!(route_pattern("/[x+5d][x+5b]", "3.0.1").is_err());
    }
    #[test]
    fn kit2_reindexes_nodes_preserves_trailing_holes_and_memoizes_loaders() {
        let output = generate_ssr_manifest(&fixture()).unwrap();
        assert_eq!(
            output.node_indices,
            BTreeMap::from([(0, 0), (1, 1), (2, 2), (4, 3)])
        );
        assert!(
            output
                .expression
                .contains("layouts:[0,,2,,],errors:[1,,],leaf:3")
        );
        assert!(output.expression.contains("./server/nodes/4.js"));
        assert!(
            output
                .expression
                .contains("__memo(() => import(\"./server/entries/endpoints/id.js\"))")
        );
        assert!(output.expression.contains("_:"));
        assert!(output.expression.contains("appDir:"));
        assert!(output.expression.contains("entries/matchers/integer.js"));
        assert!(!output.expression.contains("image/x-icon"));
        assert!(output.expression.contains("\"assets/schema.json\":21"));
        assert!(output.module.starts_with("export const manifest ="));
    }
    #[test]
    fn kit3_flat_shape_params_and_prerender_mime_remain_versioned() {
        let mut input = fixture();
        input.kit_version = "3.0.1".into();
        input.matchers.clear();
        input.params_module_present = true;
        let output = generate_ssr_manifest(&input).unwrap();
        assert!(output.expression.contains("app_dir:"));
        assert!(!output.expression.contains("_:"));
        assert!(output.expression.contains("entries/params.js"));
        assert!(output.expression.contains("image/x-icon"));
        input.kit_version = "3.0.0-next.32".into();
        assert_eq!(
            generate_ssr_manifest(&input).unwrap().expression,
            output.expression
        );
    }
    #[test]
    fn only_explicit_route_exclusion_prunes_nodes_and_assets() {
        let mut input = fixture();
        input.prerendered_paths.push("/123".into());
        assert_eq!(
            generate_ssr_manifest(&input).unwrap().included_routes.len(),
            1
        );
        input.routes[0].included = false;
        let output = generate_ssr_manifest(&input).unwrap();
        assert!(output.included_routes.is_empty());
        assert_eq!(output.node_indices.len(), 2);
        assert!(!output.expression.contains("assets/schema.json"));
        assert!(!output.expression.contains("nodes/4.js"));
        input.hook_assets.push("assets/schema.json".into());
        assert!(
            generate_ssr_manifest(&input)
                .unwrap()
                .expression
                .contains("assets/schema.json")
        );
    }
    #[test]
    fn unknown_assets_and_out_of_range_node_indices_are_errors() {
        let mut input = fixture();
        input.server_assets.clear();
        assert!(
            generate_ssr_manifest(&input)
                .unwrap_err()
                .contains("Missing resolved server asset")
        );
        let mut input = fixture();
        input.routes[0].page.as_mut().unwrap().leaf = 99;
        assert!(
            generate_ssr_manifest(&input)
                .unwrap_err()
                .contains("missing node")
        );
    }
    #[test]
    fn client_route_metadata_preserves_regex_undefined_and_all_matchers() {
        let mut input = fixture();
        input.matchers.push("prerendered_only".into());
        input.client = Some(ClientMetadata {
            start: "entry/start.js".into(),
            app: None,
            imports: vec![],
            nodes: Some(vec![None, Some("nodes/error.js".into())]),
            css: Some(vec![None, Some(vec!["error.css".into()])]),
            routes: Some(vec![ClientResolvedRoute {
                id: "/[slug]".into(),
                errors: vec![Some(1), None],
                layouts: vec![Some((true, 0)), None],
                leaf: (false, 4),
            }]),
            stylesheets: vec![],
            fonts: vec![ClientFont::Kit2("font.woff2".into())],
            uses_env_dynamic_public: true,
            inline: Some(InlineClient {
                script: "bootstrap()".into(),
                style: None,
            }),
        });
        let output = generate_ssr_manifest(&input).unwrap();
        assert!(
            output
                .expression
                .contains("nodes:[void 0,\"nodes/error.js\"]")
        );
        assert!(output.expression.contains("css:[void 0,[\"error.css\"]]"));
        assert!(
            output
                .expression
                .contains("pattern:new RegExp(\"^/([^/]+?)/?$\")")
        );
        assert!(
            output
                .expression
                .contains("errors:[1,void 0],layouts:[[true,0],void 0],leaf:[false,4]")
        );
        assert!(output.expression.contains("style:void 0"));
        assert!(
            output
                .expression
                .contains("entries/matchers/prerendered_only.js")
        );
        input.client.as_mut().unwrap().nodes = None;
        assert!(
            !generate_ssr_manifest(&input)
                .unwrap()
                .expression
                .contains("entries/matchers/prerendered_only.js")
        );
        input.kit_version = "3.0.1".into();
        input.matchers.clear();
        input.params_module_present = true;
        assert!(
            generate_ssr_manifest(&input)
                .unwrap_err()
                .contains("font metadata")
        );
    }
}
