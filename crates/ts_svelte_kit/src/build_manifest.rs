//! Pure generation of Kit's client manifest, node modules and matcher module.
//!
//! The inventory owner resolves hooks, components and module extensions before
//! calling this module. Paths may be absolute or relative to `project_root`;
//! `output_directory` is the absolute generated client directory. Returned keys
//! are absolute output paths. No configuration is executed and no files are read
//! or written. Route order and node indices are the resolved Kit inventory order.
//! Root/error-template generation and import-boundary validation belong to the
//! caller; this module references those real generated modules, never stubs them.
//!
//! Contracts: installed Kit 2.67.0/2.68.0/2.69.2/2.69.3/2.70.3
//! `core/sync/write_client_manifest.js`, and Kit 3.0.1/3.0.0-next.32.

use crate::sync_versions::KitSyncVersion;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClientManifestInput {
    pub kit_version: String,
    pub project_root: PathBuf,
    pub output_directory: PathBuf,
    /// Kit 2 supports Svelte 4 and 5. Kit 3 requires Svelte 5.
    pub svelte_major: u32,
    pub config: ClientRouterConfig,
    /// Index 0 is the root layout and index 1 the root error component.
    pub nodes: Vec<ClientNode>,
    pub routes: Vec<ClientRoute>,
    #[serde(default)]
    pub hooks: ClientHooks,
    /// Kit 2's independently resolved matcher modules.
    #[serde(default)]
    pub matchers: BTreeMap<String, PathBuf>,
    /// Kit 3's resolved module exporting `params`.
    pub params_module: Option<PathBuf>,
    /// Analysis results for EVERY node. Absent means upstream's pre-analysis
    /// behavior: a server module is assumed to export a load function.
    pub has_server_load: Option<Vec<bool>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClientRouterConfig {
    pub resolution: RouterResolution,
    #[serde(rename = "type")]
    pub router_type: RouterType,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RouterResolution {
    Client,
    Server,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RouterType {
    Pathname,
    Hash,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClientNode {
    pub component: Option<PathBuf>,
    pub universal: Option<PathBuf>,
    pub server: Option<PathBuf>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClientHooks {
    pub client: Option<PathBuf>,
    pub universal: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClientRoute {
    pub id: String,
    /// Endpoint-only routes have no page; their matchers still matter in Kit 3.
    pub page: Option<ClientPage>,
    #[serde(default)]
    pub params: Vec<ClientParam>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClientPage {
    /// The resolved leaf node, corresponding to both route.leaf and page.leaf.
    pub leaf: usize,
    /// Includes root index 0. None entries are meaningful holes.
    pub layouts: Vec<Option<usize>>,
    /// Includes root index 1. None entries are meaningful holes.
    pub errors: Vec<Option<usize>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClientParam {
    pub matcher: Option<String>,
}

fn js(value: &str) -> String {
    // Serialization of a Rust string cannot fail. Escape JS line separators too
    // so generated modules are consumable by older JavaScript tooling.
    serde_json::to_string(value)
        .expect("string serialization")
        .replace('\u{2028}', "\\u2028")
        .replace('\u{2029}', "\\u2029")
}

fn normalize(path: &Path) -> Result<PathBuf, String> {
    if !path.is_absolute() {
        return Err(format!("Expected absolute path: {}", path.display()));
    }
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() {
                    return Err(format!("Path escapes filesystem root: {}", path.display()));
                }
            }
            _ => normalized.push(component.as_os_str()),
        }
    }
    Ok(normalized)
}

fn import_path(root: &Path, from: &Path, target: &Path) -> Result<String, String> {
    if target.as_os_str().is_empty() {
        return Err("An imported module path is empty".into());
    }
    let target = normalize(&root.join(target))?;
    let from_parts: Vec<_> = from.components().collect();
    let target_parts: Vec<_> = target.components().collect();
    let common = from_parts
        .iter()
        .zip(&target_parts)
        .take_while(|(a, b)| a == b)
        .count();
    if common == 0 {
        return Err("Generated modules and imports must share a filesystem root".into());
    }
    let mut result = PathBuf::new();
    for _ in common..from_parts.len() {
        result.push("..");
    }
    for part in &target_parts[common..] {
        result.push(part.as_os_str());
    }
    let text = result
        .to_str()
        .ok_or_else(|| "Imported module paths must be UTF-8".to_string())?;
    if text.is_empty() || text.contains(['\0', '?', '#']) {
        return Err(format!(
            "Module path cannot be encoded as an import: {text:?}"
        ));
    }
    let text = text.replace(std::path::MAIN_SEPARATOR, "/");
    Ok(if text.starts_with("../") {
        text
    } else {
        format!("./{text}")
    })
}

fn sparse_tail(nodes: &[Option<usize>]) -> String {
    let tail = nodes.get(1..).unwrap_or_default();
    let length = tail.iter().rposition(Option::is_some).map_or(0, |i| i + 1);
    tail[..length]
        .iter()
        .map(|n| n.map_or_else(String::new, |n| n.to_string()))
        .collect::<Vec<_>>()
        .join(",")
}

pub fn generate_client_manifest(
    input: &ClientManifestInput,
) -> Result<BTreeMap<PathBuf, String>, String> {
    let version = KitSyncVersion::parse(&input.kit_version)?;
    if !matches!(input.svelte_major, 4 | 5) || (version.is_kit3() && input.svelte_major != 5) {
        return Err(
            "Client manifest supports Svelte 4/5 with Kit 2, and Svelte 5 with Kit 3".into(),
        );
    }
    let root = normalize(&input.project_root)?;
    let output = normalize(&input.output_directory)?;
    if input.nodes.len() < 2 || input.nodes.len() > i32::MAX as usize {
        return Err(
            "Client nodes must contain the root layout/error and fit Kit's signed indices".into(),
        );
    }
    if input.nodes[0].component.is_none() || input.nodes[1].component.is_none() {
        return Err("Root layout and error nodes require resolved component paths".into());
    }
    if input
        .has_server_load
        .as_ref()
        .is_some_and(|m| m.len() != input.nodes.len())
    {
        return Err("hasServerLoad must contain exactly one result per node".into());
    }
    if version.is_kit3() && !input.matchers.is_empty() {
        return Err("Kit 3 uses paramsModule, not individual matcher modules".into());
    }
    if !version.is_kit3() && input.params_module.is_some() {
        return Err("Kit 2 uses individual matcher modules, not paramsModule".into());
    }
    let server_load = |index: usize| {
        input.has_server_load.as_ref().map_or_else(
            || input.nodes[index].server.is_some(),
            |metadata| metadata[index],
        )
    };
    let client_routing = input.config.resolution == RouterResolution::Client;
    let mut files = BTreeMap::new();
    for (i, node) in input.nodes.iter().enumerate() {
        let mut declarations = Vec::new();
        if let Some(path) = &node.universal {
            declarations.push(format!(
                "import * as universal from {};\nexport {{ universal }};",
                js(&import_path(&root, &output.join("nodes"), path)?)
            ));
        }
        if let Some(path) = &node.component {
            declarations.push(format!(
                "export {{ default as component }} from {};",
                js(&import_path(&root, &output.join("nodes"), path)?)
            ));
        }
        files.insert(
            output.join(format!("nodes/{i}.js")),
            declarations.join("\n"),
        );
    }
    let mut dictionary = Vec::new();
    // Upstream uses a JS Set; preserve first route occurrence rather than sorting.
    let mut server_loads = Vec::new();
    let mut route_ids = BTreeSet::new();
    for route in &input.routes {
        if !route.id.starts_with('/') || !route_ids.insert(&route.id) {
            return Err(format!("Invalid or duplicate route id: {}", route.id));
        }
        let Some(page) = &route.page else { continue };
        for index in std::iter::once(page.leaf)
            .chain(page.layouts.iter().chain(&page.errors).flatten().copied())
        {
            if index >= input.nodes.len() {
                return Err(format!(
                    "Route {} references missing node {index}",
                    route.id
                ));
            }
        }
        if page.layouts.first() != Some(&Some(0)) || page.errors.first() != Some(&Some(1)) {
            return Err(format!(
                "Route {} must include root layout/error indices",
                route.id
            ));
        }
        let mut array = vec![format!(
            "{}{}",
            if server_load(page.leaf) { "~" } else { "" },
            page.leaf
        )];
        for index in page.layouts.iter().flatten().copied() {
            if server_load(index) && !server_loads.contains(&index) {
                server_loads.push(index);
            }
        }
        let layouts = sparse_tail(&page.layouts);
        let errors = sparse_tail(&page.errors);
        if !layouts.is_empty() || !errors.is_empty() {
            array.push(format!("[{layouts}]"));
        }
        if !errors.is_empty() {
            array.push(format!("[{errors}]"));
        }
        dictionary.push(format!("{}: [{}]", js(&route.id), array.join(",")));
    }
    if !client_routing {
        dictionary.clear();
        server_loads.retain(|index| *index == 0);
    }
    let hook_import = |name: &str, path: &Option<PathBuf>| -> Result<String, String> {
        path.as_ref().map_or(Ok(String::new()), |path| {
            Ok(format!(
                "import * as {name}_hooks from {};\n",
                js(&import_path(&root, &output, path)?)
            ))
        })
    };
    let count = if client_routing { input.nodes.len() } else { 2 };
    let nodes = (0..count)
        .map(|i| format!("() => import('./nodes/{i}')"))
        .collect::<Vec<_>>()
        .join(",\n");
    let handler = if version.is_kit3() {
        "(({ kind, error }) => { if (kind === 'unknown') { console.error(error); } })"
    } else {
        "(({ error }) => { console.error(error) })"
    };
    let mut app = String::new();
    if !version.is_kit3() {
        app.push_str("import.meta.hot;\n");
    }
    app.push_str(&hook_import("client", &input.hooks.client)?);
    app.push_str(&hook_import("universal", &input.hooks.universal)?);
    app.push_str(if client_routing {
        "export { matchers } from './matchers.js';\n"
    } else {
        "export const matchers = {};\n"
    });
    app.push_str(&format!("export const nodes = [\n{nodes}\n];\nexport const server_loads = [{}];\nexport const dictionary = {{\n{}\n}};\n", server_loads.iter().map(usize::to_string).collect::<Vec<_>>().join(","), dictionary.join(",\n")));
    app.push_str(&format!("export const hooks = {{\nhandleError: {}{handler},\n{}reroute: {}(() => {{}}),\ntransport: {}{{}}\n}};\n",
        if input.hooks.client.is_some() { "client_hooks.handleError || " } else { "" },
        if input.hooks.client.is_some() { "init: client_hooks.init,\n" } else { "" },
        if input.hooks.universal.is_some() { "universal_hooks.reroute || " } else { "" },
        if input.hooks.universal.is_some() { "universal_hooks.transport || " } else { "" }));
    app.push_str("export const decoders = Object.fromEntries(Object.entries(hooks.transport).map(([k, v]) => [k, v.decode]));\nexport const encoders = Object.fromEntries(Object.entries(hooks.transport).map(([k, v]) => [k, v.encode]));\n");
    app.push_str(&format!(
        "export const hash = {};\nexport const decode = (type, value) => decoders[type](value);\n",
        input.config.router_type == RouterType::Hash
    ));
    if !version.is_kit3() {
        app.push_str(&format!(
            "export {{ default as root }} from '../root.{}';\n",
            if input.svelte_major >= 5 {
                "js"
            } else {
                "svelte"
            }
        ));
    }
    app.push_str("export const get_error_template = () => import('../shared/error-template.js').then(m => m.default);\n");
    files.insert(output.join("app.js"), app);
    if client_routing {
        let matcher_module = if version.is_kit3() {
            let uses_matchers = input.routes.iter().any(|r| {
                r.params
                    .iter()
                    .any(|p| p.matcher.as_ref().is_some_and(|s| !s.is_empty()))
            });
            if let Some(path) = input.params_module.as_ref().filter(|_| uses_matchers) {
                format!(
                    "import {{ params as matchers }} from {};\n\nexport {{ matchers }};",
                    js(&import_path(&root, &output, path)?)
                )
            } else {
                "export const matchers = {};".into()
            }
        } else {
            let mut imports = Vec::new();
            let mut entries = Vec::new();
            for (index, (name, path)) in input.matchers.iter().enumerate() {
                imports.push(format!(
                    "import {{ match as matcher_{index} }} from {};",
                    js(&import_path(&root, &output, path)?)
                ));
                entries.push(format!("[{}]: matcher_{index}", js(name)));
            }
            if imports.is_empty() {
                "export const matchers = {};".into()
            } else {
                format!(
                    "{}\n\nexport const matchers = {{ {} }};",
                    imports.join("\n"),
                    entries.join(", ")
                )
            }
        };
        files.insert(output.join("matchers.js"), matcher_module);
    }
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> ClientManifestInput {
        ClientManifestInput {
            kit_version: "2.69.2".into(),
            project_root: "/project".into(),
            output_directory: "/project/.svelte-kit/generated/client".into(),
            svelte_major: 5,
            config: ClientRouterConfig {
                resolution: RouterResolution::Client,
                router_type: RouterType::Pathname,
            },
            nodes: (0..5)
                .map(|i| ClientNode {
                    component: Some(format!("src/{i}.svelte").into()),
                    universal: None,
                    server: Some(format!("src/{i}.server.ts").into()),
                })
                .collect(),
            routes: vec![ClientRoute {
                id: "/a/[id=integer]".into(),
                page: Some(ClientPage {
                    leaf: 4,
                    layouts: vec![Some(0), None, Some(2), None],
                    errors: vec![Some(1), None, Some(3), None],
                }),
                params: vec![ClientParam {
                    matcher: Some("integer".into()),
                }],
            }],
            hooks: ClientHooks::default(),
            matchers: BTreeMap::from([("integer".into(), "src/params/integer.ts".into())]),
            params_module: None,
            has_server_load: None,
        }
    }

    #[test]
    fn kit2_preserves_sparse_routes_and_server_encoding() {
        let input = fixture();
        let files = generate_client_manifest(&input).unwrap();
        let app = &files[&input.output_directory.join("app.js")];
        assert!(app.contains("\"/a/[id=integer]\": [~4,[,2],[,3]]"));
        assert!(app.contains("server_loads = [0,2]"));
        assert!(app.contains("default as root } from '../root.js'"));
        assert!(app.contains("import.meta.hot;"));
        assert!(
            files[&input.output_directory.join("nodes/4.js")].contains("../../../../src/4.svelte")
        );
        assert!(
            files[&input.output_directory.join("matchers.js")].contains("[\"integer\"]: matcher_0")
        );
    }

    #[test]
    fn metadata_overrides_server_file_presence_and_server_router_keeps_only_root() {
        let mut input = fixture();
        input.has_server_load = Some(vec![true, false, false, false, false]);
        input.config.resolution = RouterResolution::Server;
        let files = generate_client_manifest(&input).unwrap();
        let app = &files[&input.output_directory.join("app.js")];
        assert!(app.contains("server_loads = [0]"));
        assert!(app.contains("dictionary = {\n\n}"));
        assert!(app.contains("import('./nodes/1')"));
        assert!(!app.contains("import('./nodes/2')"));
        assert!(files.contains_key(&input.output_directory.join("nodes/4.js")));
        assert!(!files.contains_key(&input.output_directory.join("matchers.js")));
    }

    #[test]
    fn kit3_uses_params_module_and_kind_aware_handler_without_root_export() {
        let mut input = fixture();
        input.kit_version = "3.0.1".into();
        input.matchers.clear();
        input.params_module = Some("src/params.ts".into());
        input.hooks.client = Some("src/hooks.client.ts".into());
        input.hooks.universal = Some("src/hooks.ts".into());
        let files = generate_client_manifest(&input).unwrap();
        let app = &files[&input.output_directory.join("app.js")];
        assert!(app.contains("kind === 'unknown'"));
        assert!(!app.contains("default as root"));
        assert!(!app.contains("import.meta.hot"));
        assert!(app.contains("init: client_hooks.init"));
        assert!(app.contains("universal_hooks.transport || {}"));
        assert!(files[&input.output_directory.join("matchers.js")].contains("params as matchers"));
        input.kit_version = "3.0.0-next.32".into();
        assert_eq!(generate_client_manifest(&input).unwrap(), files);
    }

    #[test]
    fn explicit_metadata_and_indices_are_validated() {
        let mut input = fixture();
        input.has_server_load = Some(vec![false]);
        assert!(
            generate_client_manifest(&input)
                .unwrap_err()
                .contains("hasServerLoad")
        );
        input.has_server_load = None;
        input.routes[0].page.as_mut().unwrap().leaf = 20;
        assert!(
            generate_client_manifest(&input)
                .unwrap_err()
                .contains("missing node 20")
        );
        input.kit_version = "3.1.0".into();
        assert!(
            generate_client_manifest(&input)
                .unwrap_err()
                .contains("Unsupported")
        );
    }

    #[test]
    fn svelte4_and_unused_kit3_params_have_correct_contracts() {
        let mut input = fixture();
        input.svelte_major = 4;
        let files = generate_client_manifest(&input).unwrap();
        assert!(files[&input.output_directory.join("app.js")].contains("../root.svelte"));
        input.kit_version = "3.0.1".into();
        input.matchers.clear();
        assert!(generate_client_manifest(&input).is_err());
        input.svelte_major = 5;
        input.routes[0].params.clear();
        input.params_module = Some("src/params.ts".into());
        let files = generate_client_manifest(&input).unwrap();
        assert_eq!(
            files[&input.output_directory.join("matchers.js")],
            "export const matchers = {};"
        );
    }

    #[test]
    fn imports_are_relative_and_escaped_and_bad_paths_fail() {
        assert_eq!(
            import_path(
                Path::new("/project"),
                Path::new("/project/out"),
                Path::new("out/a'b.ts")
            )
            .unwrap(),
            "./a'b.ts"
        );
        assert_eq!(js("a\"\nb"), "\"a\\\"\\nb\"");
        assert!(
            import_path(
                Path::new("/project"),
                Path::new("/project/out"),
                Path::new("src/file?raw.ts")
            )
            .is_err()
        );
        assert!(normalize(Path::new("/../../escape")).is_err());
    }
}
