//! Read-only native build inventory for the explicitly registered Kit versions.
//!
//! Node numbering follows create_manifest_data: filesystem preorder layouts and
//! errors first, then leaves. Route priority sorting happens AFTER numbering.
//! Layout resets and sparse error ancestry use the upstream parent walk. This
//! module does not evaluate page options, hooks, matchers or JavaScript config.
//!
//! Derived SvelteKit portions are MIT licensed:
//! Copyright (c) 2020 these people (https://github.com/sveltejs/kit/graphs/contributors)
//!
//! Permission is hereby granted, free of charge, to any person obtaining a copy
//! of this software and associated documentation files (the "Software"), to deal
//! in the Software without restriction, including without limitation the rights
//! to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
//! copies of the Software, and to permit persons to whom the Software is
//! furnished to do so, subject to the following conditions:
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
use crate::build_manifest::{
    ClientHooks, ClientManifestInput, ClientNode, ClientPage, ClientParam, ClientRoute,
    ClientRouterConfig, RouterType,
};
use crate::build_root::{ResolvedRootInput, ResolvedRootRoute};
use crate::sync_config::{self, SyncConfig};
use crate::sync_types::{self, Node, Route, RouteTypesConfig};
use crate::sync_versions::KitSyncVersion;
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BuildInventoryOptions {
    pub svelte_major: u32,
    pub router: ClientRouterConfig,
    /// None selects hooks.client/hooks.server/hooks under SyncConfig.src.
    pub hooks: Option<HookPaths>,
    pub handle_rendering_errors: bool,
    pub async_compiler: bool,
    /// Kit 3 separates generated/build and generated/dev.
    pub dev: bool,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HookPaths {
    pub client: Option<PathBuf>,
    pub server: Option<PathBuf>,
    pub universal: Option<PathBuf>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BuildInventory {
    pub kit_version: String,
    pub project_root: PathBuf,
    pub runtime_directory: PathBuf,
    pub generated_directory: PathBuf,
    pub options: BuildInventoryOptions,
    pub nodes: Vec<InventoryNode>,
    pub routes: Vec<InventoryRoute>,
    pub hooks: HookPaths,
    pub matchers: BTreeMap<String, PathBuf>,
    pub params_module: Option<PathBuf>,
    pub service_worker: Option<PathBuf>,
    /// Selected source paths and scanned directories, for host watch/provenance.
    pub inputs: BTreeSet<PathBuf>,
    /// No page exports were evaluated. A later analysis pass can supply complete
    /// hasServerLoad metadata to client_manifest_input.
    pub page_options_analyzed: bool,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NodeKind {
    Layout,
    Error,
    Page,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InventoryNode {
    pub kind: NodeKind,
    pub route_id: String,
    pub depth: usize,
    pub component: Option<PathBuf>,
    pub universal: Option<PathBuf>,
    pub server: Option<PathBuf>,
    pub parent: Option<usize>,
    pub child_pages: Vec<usize>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InventoryRoute {
    pub id: String,
    pub parent_id: Option<String>,
    pub params: Vec<InventoryParam>,
    pub page: Option<ClientPage>,
    pub layout: Option<usize>,
    pub error: Option<usize>,
    pub endpoint: Option<PathBuf>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InventoryParam {
    pub name: String,
    pub matcher: Option<String>,
    pub optional: bool,
    pub rest: bool,
    pub chained: bool,
}

impl BuildInventory {
    pub fn client_manifest_input(&self, has_server_load: Option<Vec<bool>>) -> ClientManifestInput {
        ClientManifestInput {
            kit_version: self.kit_version.clone(),
            project_root: self.project_root.clone(),
            output_directory: self.generated_directory.join("client"),
            svelte_major: self.options.svelte_major,
            config: self.options.router.clone(),
            nodes: self
                .nodes
                .iter()
                .map(|n| ClientNode {
                    component: n.component.clone(),
                    universal: n.universal.clone(),
                    server: n.server.clone(),
                })
                .collect(),
            routes: self
                .routes
                .iter()
                .map(|r| ClientRoute {
                    id: r.id.clone(),
                    page: r.page.clone(),
                    params: r
                        .params
                        .iter()
                        .map(|p| ClientParam {
                            matcher: p.matcher.clone(),
                        })
                        .collect(),
                })
                .collect(),
            hooks: ClientHooks {
                client: self.hooks.client.clone(),
                universal: self.hooks.universal.clone(),
            },
            matchers: self.matchers.clone(),
            params_module: self.params_module.clone(),
            has_server_load,
        }
    }
    pub fn root_input(&self) -> ResolvedRootInput {
        ResolvedRootInput {
            kit_version: self.kit_version.clone(),
            output: self.generated_directory.clone(),
            routes: self
                .routes
                .iter()
                .map(|r| ResolvedRootRoute {
                    layouts: r
                        .page
                        .as_ref()
                        .map(|p| p.layouts.iter().map(|n| n.map(|n| n as u32)).collect()),
                })
                .collect(),
            svelte_major: self.options.svelte_major,
            handle_rendering_errors: self.options.handle_rendering_errors,
            async_compiler: self.options.async_compiler,
            runtime_directory: Some(self.runtime_directory.clone()),
            relocate_kit3_root: false,
        }
    }
}

fn inside(root: &Path, path: &Path) -> Result<PathBuf, String> {
    let path = root.join(path);
    let mut result = PathBuf::new();
    for part in path.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                if !result.pop() {
                    return Err("Path escapes filesystem root".into());
                }
            }
            _ => result.push(part.as_os_str()),
        }
    }
    if !result.starts_with(root) {
        return Err(format!(
            "Build inventory path escapes project: {}",
            path.display()
        ));
    }
    if result.exists()
        && !result
            .canonicalize()
            .map_err(|e| e.to_string())?
            .starts_with(root)
    {
        return Err(format!(
            "Build inventory source escapes project through a symlink: {}",
            result.display()
        ));
    }
    Ok(result)
}
fn resolve_entry(
    path: &Path,
    extensions: &[String],
    active: &mut BTreeSet<PathBuf>,
) -> Result<Option<PathBuf>, String> {
    if path.is_file() {
        return Ok(Some(path.to_owned()));
    }
    if path.is_dir() {
        let real = path.canonicalize().map_err(|e| e.to_string())?;
        if !active.insert(real.clone()) || active.len() > 64 {
            return Err("Entry directory cycle or depth limit".into());
        }
        let found = resolve_entry(&path.join("index"), extensions, active)?;
        active.remove(&real);
        return Ok(found);
    }
    let Some(parent) = path.parent().filter(|p| p.is_dir()) else {
        return Ok(None);
    };
    let Some(base) = path.file_name().and_then(|s| s.to_str()) else {
        return Err("Entry path must be UTF-8".into());
    };
    let mut candidates = fs::read_dir(parent)
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    candidates.sort_by_key(|e| e.file_name());
    Ok(candidates
        .into_iter()
        .find(|e| {
            let name = e.file_name();
            let Some(name) = name.to_str() else {
                return false;
            };
            extensions
                .iter()
                .any(|ext| name.strip_suffix(ext) == Some(base))
                && e.path().is_file()
        })
        .map(|e| e.path()))
}
fn resolved(
    root: &Path,
    path: Option<&Path>,
    extensions: &[String],
    inputs: &mut BTreeSet<PathBuf>,
) -> Result<Option<PathBuf>, String> {
    let Some(path) = path else { return Ok(None) };
    let path = inside(root, path)?;
    if let Some(parent) = path.parent() {
        inputs.insert(parent.to_owned());
    }
    let path = resolve_entry(&path, extensions, &mut BTreeSet::new())?;
    if let Some(path) = &path {
        inside(root, path)?;
        inputs.insert(path.clone());
    }
    Ok(path)
}

pub fn discover_build_inventory(
    root: &Path,
    config: &SyncConfig,
    options: &BuildInventoryOptions,
) -> Result<BuildInventory, String> {
    if config.extensions.is_empty()
        || config.module_extensions.is_empty()
        || config
            .extensions
            .iter()
            .chain(&config.module_extensions)
            .any(|ext| !ext.starts_with('.') || ext.len() < 2 || ext.contains(['/', '\\']))
    {
        return Err(
            "Build inventory requires explicit nonempty component/module extensions".into(),
        );
    }
    let root = root.canonicalize().map_err(|e| e.to_string())?;
    let kit_version = sync_config::installed_kit_version(&root)?;
    let version = KitSyncVersion::parse(&kit_version)?;
    if !matches!(options.svelte_major, 4 | 5) || (version.is_kit3() && options.svelte_major != 5) {
        return Err("Unsupported Svelte/Kit build inventory version pair".into());
    }
    let runtime = root
        .join("node_modules/@sveltejs/kit/src/runtime")
        .canonicalize()
        .map_err(|e| format!("Installed Kit runtime: {e}"))?;
    let fallback = if version.is_kit3() {
        runtime.join("components")
    } else {
        runtime.join(format!("components/svelte-{}", options.svelte_major))
    };
    for name in ["layout.svelte", "error.svelte"] {
        if !fallback.join(name).is_file() {
            return Err(format!(
                "Installed Kit fallback is missing: {}",
                fallback.join(name).display()
            ));
        }
    }
    let mut inputs = BTreeSet::from([root.join("node_modules/@sveltejs/kit/package.json")]);
    let route_dir = inside(&root, Path::new(&config.routes))?;
    let out = inside(&root, Path::new(&config.out_dir))?;
    let generated = if version.is_kit3() {
        out.join(if options.dev {
            "generated/dev"
        } else {
            "generated/build"
        })
    } else {
        out.join("generated")
    };
    let discovery = RouteTypesConfig {
        root: root.clone(),
        routes: route_dir.clone(),
        params: inside(&root, Path::new(&config.params))?,
        out_dir: out,
        component_extensions: config.extensions.clone(),
        module_extensions: config.module_extensions.clone(),
    };
    let mut routes = Vec::new();
    if route_dir.exists() {
        sync_types::discover(
            &discovery,
            route_dir.clone(),
            "/".into(),
            None,
            &mut routes,
            &mut BTreeSet::new(),
            version,
        )?;
        if routes.len() == 1
            && routes[0].page.is_none()
            && routes[0].layout.is_none()
            && routes[0].error_file.is_none()
            && routes[0].endpoint.is_none()
        {
            return Err("No routes found in configured routes directory".into());
        }
    } else {
        routes.push(Route {
            id: "/".into(),
            parent: None,
            page: None,
            layout: None,
            endpoint: None,
            params: vec![],
            error_component: false,
            error_file: None,
        });
    }
    for route in &routes {
        inputs.insert(inside(
            &root,
            &route_dir.join(route.id.trim_start_matches('/')),
        )?);
    }
    prevent_conflicts(&routes, version)?;
    routes[0].layout.get_or_insert_with(Node::default);
    if routes[0].error_file.is_none() {
        routes[0].error_file = Some(fallback.join("error.svelte"));
    }
    for route in &mut routes {
        if let Some(layout) = &mut route.layout {
            if layout.component.is_none() {
                layout.component = Some(fallback.join("layout.svelte"));
            }
        }
    }
    let mut nodes = Vec::new();
    let mut layouts = vec![None; routes.len()];
    let mut errors = vec![None; routes.len()];
    let mut leaves = vec![None; routes.len()];
    let node = |route: &Route, kind: NodeKind, n: &Node| InventoryNode {
        kind,
        route_id: route.id.clone(),
        depth: route
            .id
            .trim_matches('/')
            .split('/')
            .filter(|s| !s.is_empty())
            .count(),
        component: n.component.clone(),
        universal: n.universal.clone(),
        server: n.server.clone(),
        parent: None,
        child_pages: vec![],
    };
    for (i, route) in routes.iter().enumerate() {
        if let Some(layout) = &route.layout {
            layouts[i] = Some(nodes.len());
            nodes.push(node(route, NodeKind::Layout, layout));
        }
        if let Some(error) = &route.error_file {
            errors[i] = Some(nodes.len());
            nodes.push(node(
                route,
                NodeKind::Error,
                &Node {
                    component: Some(error.clone()),
                    ..Default::default()
                },
            ));
        }
    }
    for (i, route) in routes.iter().enumerate() {
        if let Some(page) = &route.page {
            leaves[i] = Some(nodes.len());
            nodes.push(node(route, NodeKind::Page, page));
        }
    }
    let mut inventory_routes = Vec::new();
    for (i, route) in routes.iter().enumerate() {
        let page = if let Some(leaf) = leaves[i] {
            let mut page = ClientPage {
                leaf,
                layouts: vec![],
                errors: vec![],
            };
            let mut next = Some(i);
            let mut reset = route.page.as_ref().and_then(|n| n.reset.as_deref());
            let mut current_node = leaf;
            while let Some(at) = next {
                let current = &routes[at];
                let segment = current.id.rsplit('/').next().unwrap_or("");
                if reset.is_none() || reset == Some(segment) {
                    if layouts[at].is_some() || errors[at].is_some() {
                        page.layouts.push(layouts[at]);
                        page.errors.push(errors[at]);
                    }
                    if let Some(layout) = layouts[at] {
                        nodes[layout].child_pages.push(leaf);
                        nodes[current_node].parent = Some(layout);
                        current_node = layout;
                        reset = current.layout.as_ref().and_then(|n| n.reset.as_deref());
                    } else {
                        reset = None;
                    }
                }
                next = current.parent;
            }
            if let Some(reset) = reset {
                return Err(format!(
                    "Route {} references missing layout segment {reset:?}",
                    route.id
                ));
            }
            page.layouts.reverse();
            page.errors.reverse();
            Some(page)
        } else {
            None
        };
        if let Some(error) = errors[i] {
            let mut next = Some(i);
            while let Some(at) = next {
                if let Some(layout) = layouts[at] {
                    nodes[error].parent = Some(layout);
                    break;
                }
                next = routes[at].parent;
            }
        }
        if options.router.router_type == RouterType::Hash
            && (route.endpoint.is_some()
                || route.layout.as_ref().is_some_and(|n| n.server.is_some())
                || route.page.as_ref().is_some_and(|n| n.server.is_some()))
        {
            return Err(format!(
                "Hash routing cannot use server modules in {}",
                route.id
            ));
        }
        if version.is_kit3()
            && page.is_none()
            && route.endpoint.is_none()
            && layouts[i].is_none()
            && errors[i].is_none()
        {
            continue;
        }
        inventory_routes.push(InventoryRoute {
            id: route.id.clone(),
            parent_id: route.parent.map(|p| routes[p].id.clone()),
            params: inventory_params(route),
            page,
            layout: layouts[i],
            error: errors[i],
            endpoint: route.endpoint.clone(),
        });
    }
    let mut keyed = inventory_routes
        .into_iter()
        .map(|route| Ok((sort_segments(&route.id)?, route)))
        .collect::<Result<Vec<_>, String>>()?;
    keyed.sort_by(|(a, ra), (b, rb)| compare_routes(a, &ra.id, b, &rb.id));
    let inventory_routes = keyed.into_iter().map(|(_, r)| r).collect::<Vec<_>>();
    for node in &nodes {
        for path in [&node.component, &node.universal, &node.server]
            .into_iter()
            .flatten()
        {
            if !path.starts_with(&runtime) {
                inside(&root, path)?;
            }
            inputs.insert(path.clone());
        }
    }
    for route in &inventory_routes {
        if let Some(path) = &route.endpoint {
            inside(&root, path)?;
            inputs.insert(path.clone());
        }
    }
    let default_hooks = HookPaths {
        client: Some(Path::new(&config.src).join("hooks.client")),
        server: Some(Path::new(&config.src).join("hooks.server")),
        universal: Some(Path::new(&config.src).join("hooks")),
    };
    let hooks = options.hooks.as_ref().unwrap_or(&default_hooks);
    let hook_extensions = if version.is_kit3() {
        config.module_extensions.clone()
    } else {
        vec![".js".into(), ".ts".into()]
    };
    let hooks = HookPaths {
        client: resolved(
            &root,
            hooks.client.as_deref(),
            &hook_extensions,
            &mut inputs,
        )?,
        server: resolved(
            &root,
            hooks.server.as_deref(),
            &hook_extensions,
            &mut inputs,
        )?,
        universal: resolved(
            &root,
            hooks.universal.as_deref(),
            &hook_extensions,
            &mut inputs,
        )?,
    };
    let mut matchers = BTreeMap::new();
    let mut params_module = None;
    if version.is_kit3() {
        params_module = resolved(
            &root,
            Some(Path::new(&config.params)),
            &config.module_extensions,
            &mut inputs,
        )?;
    } else {
        let params = inside(&root, Path::new(&config.params))?;
        inputs.insert(params.clone());
        if params.is_dir() {
            let mut files = fs::read_dir(&params)
                .map_err(|e| e.to_string())?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| e.to_string())?;
            files.sort_by_key(|f| f.file_name());
            for file in files {
                let name = file.file_name().to_string_lossy().to_string();
                let Some(ext) = Path::new(&name).extension().and_then(|s| s.to_str()) else {
                    continue;
                };
                if !config
                    .module_extensions
                    .iter()
                    .any(|e| e == &format!(".{ext}"))
                {
                    continue;
                }
                let stem = &name[..name.len() - ext.len() - 1];
                if stem.ends_with(".test") || stem.ends_with(".spec") {
                    continue;
                }
                if stem.is_empty() || !stem.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
                {
                    return Err(format!("Invalid matcher name: {name}"));
                }
                let path = inside(&root, &file.path())?;
                if !path.is_file() {
                    return Err(format!("Matcher is not a file: {}", path.display()));
                }
                if matchers.insert(stem.to_owned(), path.clone()).is_some() {
                    return Err(format!("Duplicate matcher: {stem}"));
                }
                inputs.insert(path);
            }
        }
        for route in &inventory_routes {
            for param in &route.params {
                if let Some(matcher) = &param.matcher {
                    if !matchers.contains_key(matcher) {
                        return Err(format!("No matcher found for {matcher:?} in {}", route.id));
                    }
                }
            }
        }
    }
    let service_worker = resolved(
        &root,
        Some(Path::new(&config.service_worker)),
        &hook_extensions,
        &mut inputs,
    )?;
    Ok(BuildInventory {
        kit_version,
        project_root: root,
        runtime_directory: runtime,
        generated_directory: generated,
        options: options.clone(),
        nodes,
        routes: inventory_routes,
        hooks,
        matchers,
        params_module,
        service_worker,
        inputs,
        page_options_analyzed: false,
    })
}

fn inventory_params(route: &Route) -> Vec<InventoryParam> {
    route
        .params
        .iter()
        .map(|param| {
            let (rest, chained) = route
                .id
                .split('/')
                .filter(|segment| !group(segment))
                .find_map(|segment| {
                    let parts = split_parts(segment).ok()?;
                    parts.iter().enumerate().find_map(|(index, part)| {
                        if part.kind == PartType::Static {
                            return None;
                        }
                        let value = part.content.trim_start_matches('[').trim_end_matches(']');
                        let value = value.strip_prefix("...").unwrap_or(value);
                        let name = value.split('=').next()?;
                        if name != param.name {
                            return None;
                        }
                        let rest = part.kind == PartType::Rest;
                        let at_start = index == 1 && parts[0].content.is_empty();
                        let whole = at_start && parts.len() == 3 && parts[2].content.is_empty();
                        Some((
                            rest,
                            if rest {
                                at_start
                            } else {
                                param.optional && whole
                            },
                        ))
                    })
                })
                .expect("Parameters were produced by the same validated route syntax");
            InventoryParam {
                name: param.name.clone(),
                matcher: param.matcher.clone(),
                optional: param.optional,
                rest,
                chained,
            }
        })
        .collect()
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum PartType {
    Static,
    Required,
    Optional,
    Rest,
}
#[derive(Clone, Debug)]
struct Part {
    kind: PartType,
    content: String,
    matched: bool,
}
fn split_parts(segment: &str) -> Result<Vec<Part>, String> {
    let mut parts = Vec::new();
    let mut cursor = 0;
    loop {
        let Some(start) = segment[cursor..].find('[').map(|p| p + cursor) else {
            parts.push(Part {
                kind: PartType::Static,
                content: segment[cursor..].into(),
                matched: false,
            });
            break;
        };
        parts.push(Part {
            kind: PartType::Static,
            content: segment[cursor..start].into(),
            matched: false,
        });
        let kind = if segment[start..].starts_with("[[") {
            PartType::Optional
        } else if segment[start..].starts_with("[...") {
            PartType::Rest
        } else {
            PartType::Required
        };
        let close = if kind == PartType::Optional {
            "]]"
        } else {
            "]"
        };
        let end = segment[start..]
            .find(close)
            .ok_or_else(|| format!("Invalid route segment {segment}"))?
            + start
            + close.len();
        let content = &segment[start..end];
        parts.push(Part {
            kind,
            content: content.into(),
            matched: content.contains('='),
        });
        cursor = end;
    }
    Ok(parts)
}
fn group(segment: &str) -> bool {
    segment.len() > 2 && segment.starts_with('(') && segment.ends_with(')')
}
fn sort_segments(id: &str) -> Result<Vec<Vec<Part>>, String> {
    let mut normalized = String::new();
    let mut cursor = 0;
    while let Some(start) = id[cursor..].find("[[").map(|p| p + cursor) {
        normalized.push_str(&id[cursor..start]);
        let end = id[start..]
            .find("]]")
            .ok_or("Invalid optional route segment")?
            + start
            + 2;
        let suffix = &id[end..];
        let only_groups =
            suffix.is_empty() || (suffix.starts_with('/') && suffix[1..].split('/').all(group));
        if only_groups {
            normalized.push_str(&id[start..end]);
        }
        cursor = end;
    }
    normalized.push_str(&id[cursor..]);
    normalized
        .split('/')
        .filter(|s| !s.is_empty() && !group(s))
        .map(split_parts)
        .collect()
}
fn static_order(a: &str, b: &str) -> Ordering {
    let a = a.encode_utf16().collect::<Vec<_>>();
    let b = b.encode_utf16().collect::<Vec<_>>();
    for (a, b) in a.iter().zip(&b) {
        if a != b {
            return a.cmp(b);
        }
    }
    b.len().cmp(&a.len())
}
fn compare_routes(a: &[Vec<Part>], aid: &str, b: &[Vec<Part>], bid: &str) -> Ordering {
    let empty = vec![Part {
        kind: PartType::Static,
        content: String::new(),
        matched: false,
    }];
    for i in 0..a.len().max(b.len()) {
        let sa = a.get(i).unwrap_or(&empty);
        let sb = b.get(i).unwrap_or(&empty);
        for j in 0..sa.len().max(sb.len()) {
            let pa = sa.get(j);
            let pb = sb.get(j);
            if j % 2 == 1 {
                let Some(pa) = pa else { return Ordering::Less };
                let Some(pb) = pb else {
                    return Ordering::Greater;
                };
                let next = |segments: &[Vec<Part>], segment: &[Part]| {
                    segment.get(j + 1).is_some_and(|p| !p.content.is_empty())
                        || segments
                            .get(i + 1)
                            .and_then(|s| s.first())
                            .is_some_and(|p| !p.content.is_empty())
                };
                let na = next(a, sa);
                let nb = next(b, sb);
                if pa.kind == PartType::Rest && pb.kind == PartType::Rest {
                    if na && nb {
                        continue;
                    }
                    if na {
                        return Ordering::Less;
                    }
                    if nb {
                        return Ordering::Greater;
                    }
                }
                if pa.kind == PartType::Rest {
                    return if na && !nb {
                        Ordering::Less
                    } else {
                        Ordering::Greater
                    };
                }
                if pb.kind == PartType::Rest {
                    return if nb && !na {
                        Ordering::Greater
                    } else {
                        Ordering::Less
                    };
                }
                if pa.matched != pb.matched {
                    return if pa.matched {
                        Ordering::Less
                    } else {
                        Ordering::Greater
                    };
                }
                if pa.kind != pb.kind {
                    if pa.kind == PartType::Required {
                        return Ordering::Less;
                    }
                    if pb.kind == PartType::Required {
                        return Ordering::Greater;
                    }
                }
            } else if let (Some(pa), Some(pb)) = (pa, pb) {
                if pa.content != pb.content {
                    if i >= a.len() {
                        return Ordering::Less;
                    }
                    if i >= b.len() {
                        return Ordering::Greater;
                    }
                    return static_order(&pa.content, &pb.content);
                }
            }
        }
    }
    bid.encode_utf16().cmp(aid.encode_utf16())
}

fn normalize_conflict(id: &str, version: KitSyncVersion) -> Result<String, String> {
    let mut output = Vec::new();
    for segment in id.split('/') {
        if group(segment) {
            output.push(String::new());
            continue;
        }
        let mut text = String::new();
        for part in split_parts(segment)? {
            if part.kind == PartType::Static {
                text.push_str(&part.content);
                continue;
            }
            let value = part.content.trim_start_matches('[').trim_end_matches(']');
            if value.starts_with("x+") || value.starts_with("u+") {
                let mut code = u32::from_str_radix(&value[2..], 16).map_err(|e| e.to_string())?;
                if !version.is_kit3() {
                    code &= 0xffff;
                }
                let ch = char::from_u32(code)
                    .ok_or("Route character escape is not a Unicode scalar value")?;
                if ch == '/' {
                    text.push_str("%2f");
                } else {
                    text.push(ch);
                }
            } else {
                let matcher = value
                    .split_once('=')
                    .map_or("*".to_owned(), |(_, m)| format!("={m}"));
                text.push_str(&format!(
                    "<{}{}{matcher}>",
                    if part.kind == PartType::Optional {
                        "?"
                    } else {
                        ""
                    },
                    if part.kind == PartType::Rest {
                        "..."
                    } else {
                        ""
                    }
                ));
            }
        }
        output.push(text);
    }
    Ok(output.join("/"))
}
fn prevent_conflicts(routes: &[Route], version: KitSyncVersion) -> Result<(), String> {
    let mut seen = BTreeMap::new();
    for route in routes
        .iter()
        .filter(|r| r.page.is_some() || r.endpoint.is_some())
    {
        let normalized = normalize_conflict(&route.id, version)?;
        let ends_optional = normalized
            .rfind('<')
            .is_some_and(|at| normalized[at..].starts_with("<?") && normalized.ends_with('>'));
        let mut variants = vec![String::new()];
        let mut cursor = 0;
        if !ends_optional {
            while let Some(start) = normalized[cursor..].find("<?").map(|p| p + cursor) {
                let end = normalized[start..]
                    .find('>')
                    .ok_or("Invalid normalized route")?
                    + start;
                let before = &normalized[cursor..start];
                let matcher = &normalized[start + 2..end];
                let mut next = Vec::new();
                for mut variant in variants {
                    variant.push_str(before);
                    next.push(variant.clone());
                    if !(matcher == "*" && variant.ends_with("//")) {
                        variant.push_str(&format!("<{matcher}>"));
                        next.push(variant);
                    }
                }
                if next.len() > 65536 {
                    return Err("Optional route conflict permutations exceed 65536".into());
                }
                variants = next;
                cursor = end + 1;
            }
        }
        for mut variant in variants {
            variant.push_str(&normalized[cursor..]);
            if let Some(start) = variant.find("//") {
                let end = start + variant[start..].bytes().take_while(|b| *b == b'/').count();
                variant.replace_range(start..end, "/");
            }
            let key = variant.strip_prefix('/').unwrap_or(&variant);
            let key = key.strip_suffix('/').unwrap_or(key).to_owned();
            if let Some(previous) = seen.insert(key, route.id.clone()) {
                return Err(format!("Routes {previous:?} and {:?} conflict", route.id));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::build_manifest::{RouterResolution, generate_client_manifest};
    use crate::build_root::{RootEntry, root_entry};
    struct Fixture(PathBuf);
    impl Fixture {
        fn new(version: &str) -> Self {
            let root = std::env::temp_dir().join(format!(
                "kit-inventory-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            fs::create_dir_all(&root).unwrap();
            let fixture = Self(root);
            fixture.write(
                "node_modules/@sveltejs/kit/package.json",
                &format!("{{\"version\":\"{version}\"}}"),
            );
            let fallback = if version.starts_with('3') {
                "components"
            } else {
                "components/svelte-5"
            };
            for name in ["layout.svelte", "error.svelte"] {
                fixture.write(
                    &format!("node_modules/@sveltejs/kit/src/runtime/{fallback}/{name}"),
                    "<slot />",
                );
            }
            fixture
        }
        fn write(&self, path: &str, source: &str) {
            let file = self.0.join(path);
            fs::create_dir_all(file.parent().unwrap()).unwrap();
            fs::write(file, source).unwrap();
        }
        fn discover(&self) -> Result<BuildInventory, String> {
            discover_build_inventory(&self.0, &SyncConfig::default(), &options())
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn options() -> BuildInventoryOptions {
        BuildInventoryOptions {
            svelte_major: 5,
            router: ClientRouterConfig {
                resolution: RouterResolution::Client,
                router_type: RouterType::Pathname,
            },
            hooks: None,
            handle_rendering_errors: false,
            async_compiler: false,
            dev: false,
        }
    }

    #[test]
    fn node_numbering_precedes_route_priority_and_layout_resets_skip_ancestry() {
        let f = Fixture::new("2.69.2");
        f.write(
            "src/routes/a/+layout.server.ts",
            "export const load = () => ({});",
        );
        f.write("src/routes/a/+error.svelte", "<p>Error</p>");
        f.write("src/routes/a/b/+page@.svelte", "<p>Reset</p>");
        f.write("src/routes/a/c/+page.svelte", "<p>Nested</p>");
        let inventory = f.discover().unwrap();
        assert_eq!(
            inventory.nodes.iter().map(|n| n.kind).collect::<Vec<_>>(),
            vec![
                NodeKind::Layout,
                NodeKind::Error,
                NodeKind::Layout,
                NodeKind::Error,
                NodeKind::Page,
                NodeKind::Page
            ]
        );
        let reset = inventory
            .routes
            .iter()
            .find(|r| r.id == "/a/b")
            .unwrap()
            .page
            .as_ref()
            .unwrap();
        assert_eq!(reset.leaf, 4);
        assert_eq!(reset.layouts, vec![Some(0)]);
        assert_eq!(reset.errors, vec![Some(1)]);
        let nested = inventory
            .routes
            .iter()
            .find(|r| r.id == "/a/c")
            .unwrap()
            .page
            .as_ref()
            .unwrap();
        assert_eq!(nested.layouts, vec![Some(0), Some(2)]);
        assert_eq!(nested.errors, vec![Some(1), Some(3)]);
        assert_eq!(inventory.nodes[3].parent, Some(2));
        assert_eq!(inventory.nodes[2].child_pages, vec![5]);
        assert!(
            inventory.nodes[0]
                .component
                .as_ref()
                .unwrap()
                .ends_with("runtime/components/svelte-5/layout.svelte")
        );
        let files = generate_client_manifest(&inventory.client_manifest_input(None)).unwrap();
        assert!(
            files[&inventory.generated_directory.join("client/app.js")]
                .contains("server_loads = [2]")
        );
        assert!(matches!(
            root_entry(&inventory.root_input()).unwrap(),
            RootEntry::Generated { .. }
        ));
    }

    #[test]
    fn kit3_filters_empty_directories_and_resolves_unified_params_and_hooks() {
        let f = Fixture::new("3.0.1");
        f.write("src/routes/group/[id=integer]/+page.svelte", "<p>Page</p>");
        f.write(
            "src/params.ts",
            "export const params = { integer: (id) => /^\\d+$/.test(id) };",
        );
        f.write("src/hooks.client.ts", "export function init() {}");
        let inventory = f.discover().unwrap();
        assert!(!inventory.routes.iter().any(|r| r.id == "/group"));
        assert!(
            inventory
                .params_module
                .as_ref()
                .unwrap()
                .ends_with("src/params.ts")
        );
        assert!(
            inventory
                .hooks
                .client
                .as_ref()
                .unwrap()
                .ends_with("src/hooks.client.ts")
        );
        assert!(
            inventory
                .generated_directory
                .ends_with(".svelte-kit/generated/build")
        );
        assert!(matches!(
            root_entry(&inventory.root_input()).unwrap(),
            RootEntry::Installed { .. }
        ));
        let files = generate_client_manifest(&inventory.client_manifest_input(None)).unwrap();
        assert!(
            files[&inventory.generated_directory.join("client/matchers.js")]
                .contains("params as matchers")
        );
    }

    #[test]
    fn priority_matches_static_matched_required_optional_rest_order() {
        let ids = [
            "/[...rest]",
            "/[[optional]]",
            "/[param]",
            "/[param=matcher]",
            "/food",
            "/foo",
            "/",
            "/[...rest]/x",
        ];
        let mut keyed = ids
            .iter()
            .map(|id| (sort_segments(id).unwrap(), *id))
            .collect::<Vec<_>>();
        keyed.sort_by(|(a, aid), (b, bid)| compare_routes(a, aid, b, bid));
        let sorted = keyed.iter().map(|(_, id)| *id).collect::<Vec<_>>();
        assert_eq!(
            sorted,
            vec![
                "/",
                "/food",
                "/foo",
                "/[...rest]/x",
                "/[param=matcher]",
                "/[param]",
                "/[[optional]]",
                "/[...rest]"
            ]
        );
    }

    #[test]
    fn grouped_and_optional_route_conflicts_are_rejected() {
        let f = Fixture::new("2.69.2");
        f.write("src/routes/(a)/same/+page.svelte", "<p>A</p>");
        f.write("src/routes/(b)/same/+page.svelte", "<p>B</p>");
        assert!(f.discover().unwrap_err().contains("conflict"));
        let f = Fixture::new("2.69.2");
        f.write("src/routes/[[optional]]/same/+page.svelte", "<p>A</p>");
        f.write("src/routes/same/+page.svelte", "<p>B</p>");
        assert!(f.discover().unwrap_err().contains("conflict"));
    }

    #[test]
    fn missing_matcher_and_hash_server_modules_fail() {
        let f = Fixture::new("2.69.2");
        f.write("src/routes/[id=integer]/+page.svelte", "<p>A</p>");
        assert!(f.discover().unwrap_err().contains("No matcher"));
        f.write(
            "src/params/integer.ts",
            "export const match = value => true;",
        );
        assert!(f.discover().is_ok());
        f.write("src/routes/+page.server.ts", "export const load=()=>({});");
        let mut opts = options();
        opts.router.router_type = RouterType::Hash;
        assert!(
            discover_build_inventory(&f.0, &SyncConfig::default(), &opts)
                .unwrap_err()
                .contains("Hash routing")
        );
    }

    #[test]
    fn params_retain_rest_and_optional_chaining_without_prefix_confusion() {
        let f = Fixture::new("2.69.2");
        f.write(
            "src/routes/[[identifier]]/prefix-[id]/[...rest]/+server.ts",
            "export const GET = () => new Response('ok');",
        );
        let inventory = f.discover().unwrap();
        let params = &inventory
            .routes
            .iter()
            .find(|r| r.endpoint.is_some())
            .unwrap()
            .params;
        assert!(params[0].optional && params[0].chained);
        assert!(!params[1].chained);
        assert!(params[2].rest && params[2].chained);
    }
}
