//! Native SvelteKit route declarations. Templates follow Kit 2.69's public
//! contracts; `any` in those contracts is intentional upstream generic freedom,
//! never a replacement for inferred application return types.
use crate::sync_versions::KitSyncVersion;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Component, Path, PathBuf};

#[path = "sync_types/proxy.rs"]
mod proxy;

/// Paths are absolute, or relative to `root`. `out_dir` is Kit's outDir,
/// not its `types` subdirectory. Generation is read-only and transactional:
/// callers write `files` only after this function succeeds.
#[derive(Debug, Clone)]
pub struct RouteTypesConfig {
    pub root: PathBuf,
    pub routes: PathBuf,
    pub params: PathBuf,
    pub out_dir: PathBuf,
    pub component_extensions: Vec<String>,
    pub module_extensions: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct RouteTypesOutput {
    /// Absolute output paths and UTF-8 contents.
    pub files: BTreeMap<PathBuf, String>,
    /// All source modules read, suitable for dependency/watch registration.
    pub inputs: BTreeSet<PathBuf>,
    pub route_ids: Vec<String>,
    pub discovered_routes: Vec<DiscoveredRoute>,
}

#[derive(Debug, Clone)]
pub struct DiscoveredRoute {
    pub id: String,
    pub params: Vec<RouteParam>,
    pub page: bool,
    pub layout: bool,
    pub endpoint: bool,
    /// None means static analysis cannot establish the inherited option. Both
    /// slash variants must be included in AppTypes in that case.
    pub page_trailing_slash: Option<String>,
    pub endpoint_trailing_slash: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct Node {
    pub(crate) component: Option<PathBuf>,
    pub(crate) server: Option<PathBuf>,
    pub(crate) universal: Option<PathBuf>,
    pub(crate) reset: Option<String>,
    pub(crate) parents: Vec<usize>,
}

#[derive(Debug, Clone)]
pub struct RouteParam {
    pub name: String,
    pub matcher: Option<String>,
    pub optional: bool,
}

type Param = RouteParam;

#[derive(Debug, Clone)]
pub(crate) struct Route {
    pub(crate) id: String,
    pub(crate) parent: Option<usize>,
    pub(crate) page: Option<Node>,
    pub(crate) layout: Option<Node>,
    pub(crate) endpoint: Option<PathBuf>,
    pub(crate) params: Vec<Param>,
    pub(crate) error_component: bool,
    /// Preserved for native build inventory; declaration generation only needs
    /// error_component and keeps its existing behavior.
    pub(crate) error_file: Option<PathBuf>,
}

fn absolute(root: &Path, value: &Path) -> PathBuf {
    let path = if value.is_absolute() {
        value.to_owned()
    } else {
        root.join(value)
    };
    let mut normalized = PathBuf::new();
    for part in path.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            _ => normalized.push(part.as_os_str()),
        }
    }
    normalized
}

fn slash(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn resolve_entry(path: &Path, extensions: &[String]) -> Option<PathBuf> {
    if path.is_file() {
        return Some(path.to_owned());
    }
    if path.is_dir()
        && let Some(index) = resolve_entry(&path.join("index"), extensions)
    {
        return Some(index);
    }
    let mut candidates: Vec<_> = extensions
        .iter()
        .map(|ext| PathBuf::from(format!("{}{ext}", path.display())))
        .collect();
    candidates.sort();
    candidates.into_iter().find(|path| path.is_file())
}

fn resolve_star_module(
    file: &Path,
    specifier: &str,
    extensions: &[String],
) -> Result<PathBuf, String> {
    if !specifier.starts_with('.') && !Path::new(specifier).is_absolute() {
        return Err(format!(
            "Star reexport {specifier:?} in {} requires a package/alias resolver; use an explicit named reexport",
            file.display()
        ));
    }
    let path = absolute(
        file.parent().ok_or("Route module has no parent")?,
        Path::new(specifier),
    );
    let replacements: &[&str] = match path.extension().and_then(|x| x.to_str()) {
        Some("js") => &["ts", "tsx", "d.ts", "js", "jsx"],
        Some("mjs") => &["mts", "d.mts", "mjs"],
        Some("cjs") => &["cts", "d.cts", "cjs"],
        _ => &[],
    };
    for ext in replacements {
        let candidate = path.with_extension(ext);
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    resolve_entry(&path, extensions).ok_or_else(|| {
        format!(
            "Cannot resolve star reexport {specifier:?} from {}",
            file.display()
        )
    })
}

fn resolve_proxy(
    file: &Path,
    server: bool,
    version: KitSyncVersion,
    extensions: &[String],
    inputs: &mut BTreeSet<PathBuf>,
    active: &mut BTreeSet<PathBuf>,
) -> Result<proxy::Proxy, String> {
    if active.len() >= 64 || inputs.len() >= 100_000 {
        return Err("Route reexport graph exceeds bounded traversal limit".into());
    }
    let source = fs::read_to_string(file).map_err(|e| format!("{}: {e}", file.display()))?;
    if source.len() > 1024 * 1024 {
        return Err(format!("Route module exceeds 1 MiB: {}", file.display()));
    }
    let mut module = proxy::transform_for_version(&source, server, version)
        .map_err(|e| format!("{}: {e}", file.display()))?;
    inputs.insert(file.to_owned());
    let real_file = file.canonicalize().map_err(|e| e.to_string())?;
    if !active.insert(real_file.clone()) {
        return Ok(module);
    }
    for specifier in &module.star_exports {
        let target = resolve_star_module(file, specifier, extensions)?;
        let imported = resolve_proxy(&target, server, version, extensions, inputs, active)?;
        // Keep the original export declarations intact. TypeScript itself
        // resolves duplicate/ambiguous bindings and reports their diagnostics;
        // presence here only selects the data/action inference template.
        module.exports.extend(imported.exports);
    }
    active.remove(&real_file);
    Ok(module)
}

fn relative(from: &Path, to: &Path) -> String {
    let a: Vec<_> = from.components().collect();
    let b: Vec<_> = to.components().collect();
    let common = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let mut path = PathBuf::new();
    for _ in common..a.len() {
        path.push("..");
    }
    for part in &b[common..] {
        path.push(part.as_os_str());
    }
    let text = slash(&path);
    if text.starts_with('.') {
        text
    } else {
        format!("./{text}")
    }
}

fn quoted(text: &str) -> String {
    // JSON strings are also TypeScript string literals, including escaped paths.
    serde_json::to_string(text).expect("string serialization")
}

fn parse_params(id: &str, version: KitSyncVersion) -> Result<Vec<Param>, String> {
    let mut unescaped = id.to_owned();
    let mut cursor = 0;
    while let Some(offset) = unescaped[cursor..].find('[') {
        let start = cursor + offset;
        let tail = &unescaped[start + 1..];
        if (tail.starts_with("x+") || tail.starts_with("u+"))
            && let Some(end) = tail.find(']')
        {
            if let Ok(value) = u32::from_str_radix(&tail[2..end], 16) {
                let value = if version.is_kit3() {
                    value
                } else {
                    value & 0xffff
                };
                let character = char::from_u32(value).unwrap_or('\u{fffd}');
                unescaped.replace_range(start..start + end + 2, &character.to_string());
                cursor = start + character.len_utf8();
                continue;
            }
        }
        cursor = start + 1;
    }
    if unescaped.contains("][") {
        return Err(format!("Route parameters must be separated in {id}"));
    }
    if id.bytes().filter(|&c| c == b'[').count() != id.bytes().filter(|&c| c == b']').count() {
        return Err(format!("Unbalanced route parameter brackets in {id}"));
    }
    if id.contains('#') {
        return Err(format!("Use [x+23] instead of # in route {id}"));
    }
    let mut params = Vec::new();
    let mut preceding_rest = false;
    for segment in id.split('/') {
        if segment.starts_with('(') && segment.ends_with(')') {
            continue;
        }
        let mut tail = segment;
        while let Some(start) = tail.find('[') {
            tail = &tail[start + 1..];
            let optional = tail.starts_with('[');
            if optional {
                tail = &tail[1..];
            }
            let end = tail
                .find(']')
                .ok_or_else(|| format!("Unclosed route parameter in {id}"))?;
            let value = &tail[..end];
            tail = &tail[end + 1..];
            if optional {
                tail = tail
                    .strip_prefix(']')
                    .ok_or_else(|| format!("Unclosed optional parameter in {id}"))?;
            }
            if value.starts_with("x+") || value.starts_with("u+") {
                let hex = &value[2..];
                let valid_length = if value.starts_with('x') {
                    hex.len() == 2
                } else {
                    (4..=6).contains(&hex.len())
                };
                if !valid_length
                    || !hex
                        .bytes()
                        .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
                {
                    return Err(format!("Invalid character escape [{value}] in {id}"));
                }
                continue;
            }
            let rest = value.starts_with("...");
            if optional && (rest || preceding_rest) {
                return Err(format!(
                    "Optional parameter cannot be a rest parameter or follow one in {id}"
                ));
            }
            let value = value.strip_prefix("...").unwrap_or(value);
            let (name, matcher) = value
                .split_once('=')
                .map_or((value, None), |(a, b)| (a, Some(b)));
            let valid = |s: &str| {
                !s.is_empty()
                    && s.bytes().all(|c| {
                        c.is_ascii_alphanumeric() || c == b'_' || (version.is_kit3() && c == b'-')
                    })
            };
            if !valid(name) || matcher.is_some_and(|m| !valid(m)) {
                return Err(format!("Invalid route parameter {value:?} in {id}"));
            }
            if params.iter().any(|p: &Param| p.name == name) {
                return Err(format!("Duplicate route parameter {name} in {id}"));
            }
            params.push(Param {
                name: name.into(),
                matcher: matcher.map(str::to_owned),
                optional,
            });
        }
        preceding_rest = segment.starts_with("[...") && segment.ends_with(']');
    }
    Ok(params)
}

fn set_file(slot: &mut Option<PathBuf>, path: &Path) -> Result<(), String> {
    if let Some(old) = slot {
        return Err(format!(
            "Duplicate route files: {} and {}",
            old.display(),
            path.display()
        ));
    }
    *slot = Some(path.to_owned());
    Ok(())
}

pub(crate) fn discover(
    config: &RouteTypesConfig,
    directory: PathBuf,
    id: String,
    parent: Option<usize>,
    routes: &mut Vec<Route>,
    ancestry: &mut BTreeSet<PathBuf>,
    version: KitSyncVersion,
) -> Result<(), String> {
    if routes.len() >= 100_000 {
        return Err("Route directory limit exceeded".into());
    }
    let real_directory = directory
        .canonicalize()
        .map_err(|e| format!("{}: {e}", directory.display()))?;
    if !ancestry.insert(real_directory.clone()) {
        return Err(format!(
            "Route directory symlink cycle at {}",
            directory.display()
        ));
    }
    let index = routes.len();
    routes.push(Route {
        params: parse_params(&id, version)?,
        id: id.clone(),
        parent,
        page: None,
        layout: None,
        endpoint: None,
        error_component: false,
        error_file: None,
    });
    let mut entries = fs::read_dir(&directory)
        .map_err(|e| format!("{}: {e}", directory.display()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    entries.sort_by_key(|entry| entry.file_name());
    let mut children = Vec::new();
    for entry in entries {
        let kind = fs::metadata(entry.path()).map_err(|e| e.to_string())?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if kind.is_dir() {
            children.push((name, entry.path()));
            continue;
        }
        if !name.starts_with('+') || name.ends_with(".d.ts") {
            continue;
        }
        let route = &mut routes[index];
        if let Some(ext) = config
            .component_extensions
            .iter()
            .find(|ext| name.ends_with(ext.as_str()))
        {
            let base = &name[..name.len() - ext.len()];
            let (base, reset) = base
                .split_once('@')
                .map_or((base, None), |(a, b)| (a, Some(b.to_owned())));
            if base == "+error" && reset.is_none() {
                route.error_component = true;
                route.error_file = Some(entry.path());
                continue;
            }
            let slot = match base {
                "+page" => &mut route.page,
                "+layout" => &mut route.layout,
                _ => return Err(format!("Reserved route filename: {name}")),
            };
            let node = slot.get_or_insert_with(Node::default);
            set_file(&mut node.component, &entry.path())?;
            node.reset = reset;
        } else if let Some(ext) = config
            .module_extensions
            .iter()
            .find(|ext| name.ends_with(ext.as_str()))
        {
            let base = &name[..name.len() - ext.len()];
            if base == "+server" {
                set_file(&mut route.endpoint, &entry.path())?;
                continue;
            }
            let (base, server) = base
                .strip_suffix(".server")
                .map_or((base, false), |b| (b, true));
            let slot = match base {
                "+page" => &mut route.page,
                "+layout" => &mut route.layout,
                _ => return Err(format!("Reserved route filename: {name}")),
            };
            let node = slot.get_or_insert_with(Node::default);
            set_file(
                if server {
                    &mut node.server
                } else {
                    &mut node.universal
                },
                &entry.path(),
            )?;
        }
    }
    for (name, child) in children {
        discover(
            config,
            child,
            format!("{}/{name}", id.trim_end_matches('/')),
            Some(index),
            routes,
            ancestry,
            version,
        )?;
    }
    ancestry.remove(&real_directory);
    Ok(())
}

fn ancestors(
    routes: &[Route],
    index: usize,
    node: &Node,
    page: bool,
) -> Result<Vec<usize>, String> {
    let mut next = if page {
        Some(index)
    } else {
        routes[index].parent
    };
    let mut reset = node.reset.as_deref();
    let mut parents = Vec::new();
    while let Some(i) = next {
        let route = &routes[i];
        let segment = route.id.rsplit('/').next().unwrap_or("");
        if reset.is_none() || reset == Some(segment) {
            if let Some(layout) = &route.layout {
                parents.push(i);
                reset = layout.reset.as_deref();
            } else {
                reset = None;
            }
        }
        next = route.parent;
    }
    if let Some(reset) = reset {
        return Err(format!(
            "{} references missing layout segment {reset:?}",
            routes[index].id
        ));
    }
    parents.reverse();
    Ok(parents)
}

fn params_type(
    params: &[Param],
    out: &Path,
    config: &RouteTypesConfig,
    version: KitSyncVersion,
) -> String {
    let entries = params
        .iter()
        .map(|param| {
            let ty = param.matcher.as_ref().map_or_else(
                || "string".into(),
                |matcher| {
                    if version.is_kit3() {
                        let configured = absolute(&config.root, &config.params);
                        let path = resolve_entry(&configured, &config.module_extensions)
                            .unwrap_or_else(|| configured.with_extension("js"));
                        return format!(
                            "MatcherParam<(typeof import({}).params)[{}]>",
                            quoted(&relative(out, &path)),
                            quoted(matcher)
                        );
                    }
                    let path = absolute(&config.root, &config.params).join(format!("{matcher}.js"));
                    format!(
                        "MatcherParam<typeof import({}).match>",
                        quoted(&relative(out, &path))
                    )
                },
            );
            format!(
                "{}{}: {ty}{}",
                quoted(&param.name),
                if param.optional { "?" } else { "" },
                if param.optional { " | undefined" } else { "" }
            )
        })
        .collect::<Vec<_>>();
    format!("{{ {} }}", entries.join("; "))
}

fn parent_type(
    node: &Node,
    routes: &[Route],
    out: &Path,
    outputs: &[PathBuf],
    kind: &str,
) -> String {
    let mut value = String::new();
    for &index in &node.parents {
        let _ = &routes[index];
        let ty = if outputs[index] == out {
            kind.to_owned()
        } else {
            format!(
                "import({}).{kind}",
                quoted(&relative(out, &outputs[index].join("$types.js")))
            )
        };
        value = if value.is_empty() {
            format!("EnsureDefined<{ty}>")
        } else {
            format!("Omit<{value}, keyof {ty}> & EnsureDefined<{ty}>")
        };
    }
    if value.is_empty() {
        "EnsureDefined<{}>".into()
    } else {
        value
    }
}

const HELPERS: &str = "type Expand<T> = T extends infer O ? { [K in keyof O]: O[K] } : never;\ntype MatcherParam<M> = M extends (param: string) => param is (infer U extends string) ? U : string;";
const PAGE_HELPERS: &str = "type MaybeWithVoid<T> = {} extends T ? T | void : T;\nexport type RequiredKeys<T> = { [K in keyof T]-?: {} extends { [P in K]: T[K] } ? never : K; }[keyof T];\ntype OutputDataShape<T> = MaybeWithVoid<Omit<App.PageData, RequiredKeys<T>> & Partial<Pick<App.PageData, keyof T & keyof App.PageData>> & Record<string, any>>;\ntype EnsureDefined<T> = T extends null | undefined ? {} : T;\ntype OptionalUnion<U extends Record<string, any>, A extends keyof U = U extends U ? keyof U : never> = U extends unknown ? { [P in Exclude<A, keyof U>]?: never } & U : never;\nexport type Snapshot<T = any> = Kit.Snapshot<T>;";

struct Generator<'a> {
    routes: &'a [Route],
    outputs: &'a [PathBuf],
    proxies: &'a BTreeMap<PathBuf, proxy::Proxy>,
    version: KitSyncVersion,
}

impl Generator<'_> {
    fn import(&self, file: &Path, out: &Path) -> String {
        if self.proxies[file].modified {
            let mut name = PathBuf::from(format!(
                "proxy{}",
                file.file_name().unwrap_or_default().to_string_lossy()
            ));
            name.set_extension("js");
            format!("./{}", slash(&name))
        } else {
            relative(out, &file.with_extension("js"))
        }
    }

    fn data_type(&self, file: &Path, out: &Path, fallback: &str, expand: bool) -> String {
        if !self.proxies[file].exports.contains("load") {
            return fallback.into();
        }
        let value = format!(
            "Kit.LoadProperties<Awaited<ReturnType<typeof import({}).load>>>",
            quoted(&self.import(file, out))
        );
        if expand {
            format!("Expand<OptionalUnion<EnsureDefined<{value}>>>")
        } else {
            value
        }
    }

    fn node(&self, node: &Node, out: &Path, page: bool, all_load: bool) -> String {
        let prefix = if page { "Page" } else { "Layout" };
        let params = if page { "RouteParams" } else { "LayoutParams" };
        let route_id = if page { "RouteId" } else { "LayoutRouteId" };
        let mut lines = Vec::new();
        let server_data = if let Some(server) = &node.server {
            let parent = format!("{prefix}ServerParentData");
            lines.push(format!(
                "type {parent} = {};",
                parent_type(node, self.routes, out, self.outputs, "LayoutServerData")
            ));
            let shape = if node.universal.is_some() || (!page && all_load) {
                "Partial<App.PageData> & Record<string, any> | void".into()
            } else {
                format!("OutputDataShape<{parent}>")
            };
            lines.push(format!("export type {prefix}ServerLoad<OutputData extends {shape} = {shape}> = Kit.ServerLoad<{params}, {parent}, OutputData, {route_id}>;"));
            lines.push(format!(
                "export type {prefix}ServerLoadEvent = Parameters<{prefix}ServerLoad>[0];"
            ));
            if page {
                if self.proxies[server].exports.contains("actions") {
                    lines.push("type ExcludeActionFailure<T> = T extends Kit.ActionFailure<any> ? never : T extends void ? never : T;\ntype ActionsSuccess<T extends Record<string, (...args: any) => any>> = { [Key in keyof T]: ExcludeActionFailure<Awaited<ReturnType<T[Key]>>>; }[keyof T];\ntype ExtractActionFailure<T> = T extends Kit.ActionFailure<infer X> ? X extends void ? never : X : never;\ntype ActionsFailure<T extends Record<string, (...args: any) => any>> = { [Key in keyof T]: Exclude<ExtractActionFailure<Awaited<ReturnType<T[Key]>>>, void>; }[keyof T];".into());
                    lines.push(format!(
                        "type ActionsExport = typeof import({}).actions;",
                        quoted(&self.import(server, out))
                    ));
                    let submit = if self.version.is_kit3() {
                        "import('$app/forms').SubmitFunction"
                    } else {
                        "Kit.SubmitFunction"
                    };
                    lines.push(format!("export type SubmitFunction = {submit}<Expand<ActionsSuccess<ActionsExport>>, Expand<ActionsFailure<ActionsExport>>>;\nexport type ActionData = Expand<Kit.AwaitedActions<ActionsExport>> | null;"));
                } else {
                    lines.push("export type ActionData = unknown;".into());
                }
            }
            self.data_type(server, out, "null", true)
        } else {
            "null".into()
        };
        lines.push(format!("export type {prefix}ServerData = {server_data};"));
        let parent = format!("{prefix}ParentData");
        lines.push(format!(
            "type {parent} = {};",
            parent_type(node, self.routes, out, self.outputs, "LayoutData")
        ));
        let data = if let Some(universal) = &node.universal {
            let ty = self.data_type(
                universal,
                out,
                &format!("{parent} & EnsureDefined<{prefix}ServerData>"),
                false,
            );
            let shape = if !page && all_load {
                "Partial<App.PageData> & Record<string, any> | void".into()
            } else {
                format!("OutputDataShape<{parent}>")
            };
            lines.push(format!("export type {prefix}Load<OutputData extends {shape} = {shape}> = Kit.Load<{params}, {prefix}ServerData, {parent}, OutputData, {route_id}>;\nexport type {prefix}LoadEvent = Parameters<{prefix}Load>[0];"));
            format!("Expand<Omit<{parent}, keyof {ty}> & OptionalUnion<EnsureDefined<{ty}>>>")
        } else if server_data == "null" {
            format!("Expand<{parent}>")
        } else {
            format!(
                "Expand<Omit<{parent}, keyof {prefix}ServerData> & EnsureDefined<{prefix}ServerData>>"
            )
        };
        lines.push(format!("export type {prefix}Data = {data};"));
        lines.join("\n")
    }
}

/// Generate all route declaration/proxy files without invoking Node or writing.
pub fn generate_route_types(config: &RouteTypesConfig) -> Result<RouteTypesOutput, String> {
    generate_route_types_for_version(config, &KitSyncVersion::default())
}

/// Generate the contract of an explicitly registered installed Kit version.
pub fn generate_route_types_for_version(
    config: &RouteTypesConfig,
    version: &KitSyncVersion,
) -> Result<RouteTypesOutput, String> {
    let version = *version;
    if !config.root.is_absolute() {
        return Err("RouteTypesConfig.root must be absolute".into());
    }
    let routes_dir = absolute(&config.root, &config.routes);
    let params_dir = absolute(&config.root, &config.params);
    let mut matchers = BTreeMap::new();
    if !version.is_kit3() && params_dir.is_dir() {
        for entry in fs::read_dir(&params_dir).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(ext) = config
                .module_extensions
                .iter()
                .find(|ext| name.ends_with(ext.as_str()))
            else {
                continue;
            };
            let stem = &name[..name.len() - ext.len()];
            if !stem.is_empty() && stem.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_') {
                if let Some(previous) = matchers.insert(stem.to_owned(), entry.path()) {
                    return Err(format!(
                        "Duplicate matchers: {} and {}",
                        previous.display(),
                        entry.path().display()
                    ));
                }
            } else if !stem.ends_with(".test") && !stem.ends_with(".spec") {
                return Err(format!("Invalid matcher filename: {name}"));
            }
        }
    }
    let mut routes = Vec::new();
    if routes_dir.exists() {
        discover(
            config,
            routes_dir.clone(),
            "/".into(),
            None,
            &mut routes,
            &mut BTreeSet::new(),
            version,
        )?;
    } else {
        routes.push(Route {
            id: "/".into(),
            parent: None,
            page: None,
            layout: None,
            endpoint: None,
            params: Vec::new(),
            error_component: false,
            error_file: None,
        });
    }
    routes[0].layout.get_or_insert_with(Node::default);
    for index in 0..routes.len() {
        if let Some(node) = &routes[index].page {
            let parents = ancestors(&routes, index, node, true)?;
            routes[index].page.as_mut().expect("page").parents = parents;
        }
        if let Some(node) = &routes[index].layout {
            let parents = ancestors(&routes, index, node, false)?;
            routes[index].layout.as_mut().expect("layout").parents = parents;
        }
    }
    let route_relative = relative(&config.root, &routes_dir);
    let stripped: PathBuf = Path::new(&route_relative)
        .components()
        .filter_map(|c| match c {
            Component::Normal(p) => Some(p),
            _ => None,
        })
        .collect();
    let types_dir = absolute(&config.root, &config.out_dir).join("types");
    let outputs: Vec<_> = routes
        .iter()
        .map(|route| {
            types_dir
                .join(&stripped)
                .join(route.id.trim_start_matches('/'))
        })
        .collect();
    let mut result = RouteTypesOutput {
        files: BTreeMap::new(),
        inputs: BTreeSet::new(),
        route_ids: Vec::new(),
        discovered_routes: Vec::new(),
    };
    let mut proxies = BTreeMap::new();
    for route in &routes {
        for node in [&route.page, &route.layout].into_iter().flatten() {
            if let Some(component) = &node.component {
                result.inputs.insert(component.clone());
            }
            for (file, server) in [(&node.server, true), (&node.universal, false)] {
                if let Some(file) = file {
                    proxies.insert(
                        file.clone(),
                        resolve_proxy(
                            file,
                            server,
                            version,
                            &config.module_extensions,
                            &mut result.inputs,
                            &mut BTreeSet::new(),
                        )?,
                    );
                    result.inputs.insert(file.clone());
                }
            }
        }
        if let Some(endpoint) = &route.endpoint {
            result.inputs.insert(endpoint.clone());
            let source =
                fs::read_to_string(endpoint).map_err(|e| format!("{}: {e}", endpoint.display()))?;
            if source.len() > 1024 * 1024 {
                return Err(format!(
                    "Route module exceeds 1 MiB: {}",
                    endpoint.display()
                ));
            }
            proxies.insert(
                endpoint.clone(),
                proxy::transform_for_version(&source, false, version)
                    .map_err(|e| format!("{}: {e}", endpoint.display()))?,
            );
        }
        for param in &route.params {
            if let Some(matcher) = &param.matcher {
                if version.is_kit3() {
                    if let Some(path) = resolve_entry(&params_dir, &config.module_extensions) {
                        result.inputs.insert(path);
                    }
                    continue;
                }
                let file = matchers
                    .get(matcher)
                    .ok_or_else(|| format!("Missing matcher {matcher} for {}", route.id))?;
                result.inputs.insert(file.clone());
            }
        }
    }
    let generator = Generator {
        routes: &routes,
        outputs: &outputs,
        proxies: &proxies,
        version,
    };
    let mut metadata = BTreeMap::new();
    for (index, route) in routes.iter().enumerate() {
        if version.is_kit3()
            && route.page.is_none()
            && route.layout.is_none()
            && route.endpoint.is_none()
            && !route.error_component
        {
            continue;
        }
        let mut page_slash = Some("never".to_owned());
        if let Some(page) = &route.page {
            let nodes = page
                .parents
                .iter()
                .filter_map(|&p| routes[p].layout.as_ref())
                .chain(std::iter::once(page));
            for node in nodes {
                for file in [&node.server, &node.universal].into_iter().flatten() {
                    let module = &proxies[file];
                    if module.options_unknown {
                        page_slash = None;
                    } else if page_slash.is_some() && module.trailing_slash.is_some() {
                        page_slash = module.trailing_slash.clone();
                    }
                }
            }
        }
        let endpoint_slash = route.endpoint.as_ref().and_then(|file| {
            let module = &proxies[file];
            if module.options_unknown {
                None
            } else {
                Some(
                    module
                        .trailing_slash
                        .clone()
                        .unwrap_or_else(|| "never".into()),
                )
            }
        });
        result.discovered_routes.push(DiscoveredRoute {
            id: route.id.clone(),
            params: route.params.clone(),
            page: route.page.is_some(),
            layout: route.layout.is_some(),
            endpoint: route.endpoint.is_some(),
            page_trailing_slash: page_slash,
            endpoint_trailing_slash: endpoint_slash,
        });
        if route.page.is_none() && route.layout.is_none() && route.endpoint.is_none() {
            continue;
        }
        let out = &outputs[index];
        let mut lines = vec![
            "import type * as Kit from '@sveltejs/kit';".into(),
            if version.is_kit3() {
                "type Expand<T> = T extends infer O ? { [K in keyof O]: O[K] } : never;\nimport { MatcherParam } from '@sveltejs/kit/params';".into()
            } else {
                HELPERS.into()
            },
            format!(
                "type RouteParams = {};",
                params_type(&route.params, out, config, version)
            ),
            format!("type RouteId = {};", quoted(&route.id)),
        ];
        if !route.params.is_empty() {
            lines.push("export type EntryGenerator = () => Promise<Array<RouteParams>> | Array<RouteParams>;".into());
        }
        if route.page.is_some() || route.layout.is_some() {
            lines.push(if version.is_kit3() { PAGE_HELPERS.replace("export type Snapshot", "/** @deprecated Use the `snapshot` helper from `$app/navigation` instead. */\nexport type Snapshot") + "\nexport type ErrorProps = { error: App.Error };" } else { PAGE_HELPERS.into() });
        }
        if let Some(page) = &route.page {
            lines.push(generator.node(page, out, true, true));
            if page.server.is_some() {
                lines.push("export type Action<OutputData extends Record<string, any> | void = Record<string, any> | void> = Kit.Action<RouteParams, OutputData, RouteId>;\nexport type Actions<OutputData extends Record<string, any> | void = Record<string, any> | void> = Kit.Actions<RouteParams, OutputData, RouteId>;\nexport type PageProps = { params: RouteParams; data: PageData; form: ActionData };".into());
            } else {
                lines.push(
                    "export type PageProps = { params: RouteParams; data: PageData };".into(),
                );
            }
        }
        if let Some(layout) = &route.layout {
            let mut ids = if !version.is_kit3() || route.page.is_some() {
                vec!["RouteId".into()]
            } else {
                Vec::new()
            };
            let mut params: Vec<Param> = Vec::new();
            let mut all_load = true;
            for child in &routes {
                let Some(page) = &child.page else { continue };
                if !page.parents.contains(&index) {
                    continue;
                }
                ids.push(quoted(&child.id));
                for param in &child.params {
                    if !params.iter().any(|p| p.name == param.name) {
                        let mut param = param.clone();
                        param.optional = true;
                        params.push(param);
                    }
                }
                if page.server.is_none() && page.universal.is_none() {
                    all_load = false;
                }
                for file in [&page.server, &page.universal].into_iter().flatten() {
                    if !proxies[file].exports.contains("load") {
                        all_load = false;
                    }
                }
            }
            if route.id == "/" {
                ids.push("null".into());
            }
            lines.push(format!(
                "type LayoutRouteId = {};",
                if ids.is_empty() {
                    "never".into()
                } else {
                    ids.join(" | ")
                }
            ));
            lines.push(format!(
                "type LayoutParams = RouteParams & {};",
                params_type(&params, out, config, version)
            ));
            lines.push(generator.node(layout, out, false, all_load));
            lines.push("export type LayoutProps = { params: LayoutParams; data: LayoutData; children: import('svelte').Snippet };".into());
        }
        if route.endpoint.is_some() {
            lines.push(
                "export type RequestHandler = Kit.RequestHandler<RouteParams, RouteId>;".into(),
            );
        }
        if route.endpoint.is_some()
            || route.page.as_ref().is_some_and(|n| n.server.is_some())
            || route.layout.as_ref().is_some_and(|n| n.server.is_some())
        {
            lines.push("export type RequestEvent = Kit.RequestEvent<RouteParams, RouteId>;".into());
        }
        let mut input_files = Vec::new();
        for node in [&route.page, &route.layout].into_iter().flatten() {
            for file in [&node.server, &node.universal].into_iter().flatten() {
                let proxy = &proxies[file];
                if proxy.modified {
                    result.files.insert(
                        out.join(format!(
                            "proxy{}",
                            file.file_name().unwrap_or_default().to_string_lossy()
                        )),
                        proxy.code.clone(),
                    );
                }
                input_files.push(slash(Path::new(
                    &relative(&config.root, file).trim_start_matches("./"),
                )));
            }
            for &parent in &node.parents {
                let parent = routes[parent].layout.as_ref().expect("layout parent");
                for file in [&parent.server, &parent.universal].into_iter().flatten() {
                    input_files.push(
                        relative(&config.root, file)
                            .trim_start_matches("./")
                            .to_owned(),
                    );
                }
            }
        }
        if let Some(file) = &route.endpoint {
            input_files.push(
                relative(&config.root, file)
                    .trim_start_matches("./")
                    .to_owned(),
            );
        }
        metadata.insert(route.id.clone(), input_files);
        result
            .files
            .insert(out.join("$types.d.ts"), lines.join("\n\n"));
        result.route_ids.push(route.id.clone());
    }
    result.files.insert(
        types_dir.join("route_meta_data.json"),
        serde_json::to_string_pretty(&metadata).map_err(|e| e.to_string())?,
    );
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "ts-svelte-kit-types-{}-{}",
                std::process::id(),
                NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&root).unwrap();
            Self(root)
        }

        fn write(&self, path: &str, source: &str) {
            let path = self.0.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, source).unwrap();
        }

        fn config(&self) -> RouteTypesConfig {
            RouteTypesConfig {
                root: self.0.clone(),
                routes: "src/routes".into(),
                params: "src/params".into(),
                out_dir: ".svelte-kit".into(),
                component_extensions: vec![".svelte".into(), ".md".into()],
                module_extensions: vec![".js".into(), ".ts".into()],
            }
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn route_parameters_preserve_optional_rest_and_matcher_contracts() {
        let params = parse_params(
            "/(group)/[[lang=language]]/files/[...rest]/[x+23]",
            KitSyncVersion::default(),
        )
        .unwrap();
        assert_eq!(params.len(), 2);
        assert!(params[0].optional);
        assert_eq!(params[0].matcher.as_deref(), Some("language"));
        assert!(!params[1].optional);
        assert_eq!(params[1].name, "rest");
        assert!(parse_params("/[id]/[id]", KitSyncVersion::default()).is_err());
    }
    #[test]
    fn layout_reset_skips_intermediate_layout() {
        let node = || Node::default();
        let route = |id: &str, parent| Route {
            id: id.into(),
            parent,
            page: None,
            layout: Some(node()),
            endpoint: None,
            params: Vec::new(),
            error_component: false,
            error_file: None,
        };
        let routes = vec![
            route("/", None),
            route("/(group)", Some(0)),
            route("/(group)/leaf", Some(1)),
        ];
        let mut page = node();
        page.reset = Some(String::new());
        assert_eq!(ancestors(&routes, 2, &page, true).unwrap(), vec![0]);
        page.reset = Some("(group)".into());
        assert_eq!(ancestors(&routes, 2, &page, true).unwrap(), vec![0, 1]);
        page.reset = Some("missing".into());
        assert!(ancestors(&routes, 2, &page, true).is_err());
    }

    #[test]
    fn complete_tree_preserves_load_action_matcher_and_layout_contracts() {
        let fixture = Fixture::new();
        fixture.write("src/routes/+layout.server.ts", "export const load = () => ({ root: 'root', replaced: 1 }); export const trailingSlash = 'always';");
        fixture.write(
            "src/routes/(app)/+layout.ts",
            "export const load = () => ({ replaced: 'child' });",
        );
        fixture.write(
            "src/routes/(app)/[id=integer]/+page.svelte",
            "<h1>Page</h1>",
        );
        fixture.write("src/routes/(app)/[id=integer]/+page.server.ts", "import type { PageServerLoad, Actions } from './$types'; export const load: PageServerLoad = ({ params }) => ({ id: params.id }); export const actions: Actions = { save: event => ({ saved: event.params.id }) };");
        fixture.write(
            "src/routes/(app)/api/+server.ts",
            "export const GET = () => new Response('ok');",
        );
        fixture.write(
            "src/params/integer.ts",
            "export const match = (value: string): value is `${number}` => /^\\d+$/.test(value);",
        );
        let generated = generate_route_types(&fixture.config()).unwrap();
        let page = &generated.files[&fixture
            .0
            .join(".svelte-kit/types/src/routes/(app)/[id=integer]/$types.d.ts")];
        assert!(page.contains("MatcherParam<typeof import("));
        assert!(page.contains("proxy+page.server.js"));
        assert!(page.contains("Kit.AwaitedActions<ActionsExport>"));
        assert!(page.contains("Omit<EnsureDefined<import("));
        assert!(
            page.contains("PageProps = { params: RouteParams; data: PageData; form: ActionData }")
        );
        let endpoint = &generated.files[&fixture
            .0
            .join(".svelte-kit/types/src/routes/(app)/api/$types.d.ts")];
        assert!(endpoint.contains("Kit.RequestHandler<RouteParams, RouteId>"));
        assert!(!endpoint.contains("PageData"));
        let route = generated
            .discovered_routes
            .iter()
            .find(|r| r.id.ends_with("[id=integer]"))
            .unwrap();
        assert_eq!(route.page_trailing_slash.as_deref(), Some("always"));
        assert_eq!(
            generated
                .files
                .keys()
                .filter(|p| p
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("proxy"))
                .count(),
            1
        );
        assert!(
            !fixture.0.join(".svelte-kit").exists(),
            "generation must remain read-only"
        );
    }

    #[test]
    fn missing_matcher_and_duplicate_route_modules_fail_before_output() {
        let fixture = Fixture::new();
        fixture.write("src/routes/[id=missing]/+page.svelte", "<p>Page</p>");
        assert!(
            generate_route_types(&fixture.config())
                .unwrap_err()
                .contains("Missing matcher")
        );
        fixture.write("src/params/missing.ts", "export const match = () => true;");
        fixture.write(
            "src/routes/[id=missing]/+page.ts",
            "export const load = () => ({});",
        );
        fixture.write(
            "src/routes/[id=missing]/+page.js",
            "export const load = () => ({});",
        );
        assert!(
            generate_route_types(&fixture.config())
                .unwrap_err()
                .contains("Duplicate route files")
        );
    }

    #[test]
    fn absent_routes_still_generate_root_layout_types() {
        let fixture = Fixture::new();
        let generated = generate_route_types(&fixture.config()).unwrap();
        assert!(
            generated.files[&fixture.0.join(".svelte-kit/types/src/routes/$types.d.ts")]
                .contains("LayoutRouteId = RouteId | null")
        );
    }

    #[test]
    fn kit3_uses_schema_params_forms_and_real_layout_route_ids() {
        let fixture = Fixture::new();
        fixture.write("src/params.ts", "export const params = { 'positive-int': { '~standard': { validate: (s: string) => ({ value: Number(s) }) } } };");
        fixture.write("src/routes/(app)/+layout.svelte", "<slot />");
        fixture.write("src/routes/(app)/empty/+layout.svelte", "<slot />");
        fixture.write("src/routes/(app)/[user-id=positive-int]/+page.server.ts", "export const load = ({ params }) => ({ id: params['user-id'] }); export const actions = { save: () => ({ saved: true }) };");
        fixture.write(
            "src/routes/placeholder/leaf/+server.ts",
            "export const GET = () => new Response('ok');",
        );
        let config = fixture.config();
        let generated = generate_route_types_for_version(&config, &KitSyncVersion::Kit3).unwrap();
        let page = &generated.files[&fixture
            .0
            .join(".svelte-kit/types/src/routes/(app)/[user-id=positive-int]/$types.d.ts")];
        assert!(page.contains("from '@sveltejs/kit/params'"));
        assert!(page.contains(".params)[\"positive-int\"]>"));
        assert!(page.contains("params.ts"));
        assert!(page.contains("import('$app/forms').SubmitFunction"));
        assert!(page.contains("export type ErrorProps = { error: App.Error }"));
        assert!(page.contains("@deprecated Use the `snapshot` helper"));
        let root = &generated.files[&fixture.0.join(".svelte-kit/types/src/routes/$types.d.ts")];
        assert!(!root.contains("LayoutRouteId = RouteId"));
        let empty = &generated.files[&fixture
            .0
            .join(".svelte-kit/types/src/routes/(app)/empty/$types.d.ts")];
        assert!(empty.contains("LayoutRouteId = never"));
        assert!(
            !generated
                .discovered_routes
                .iter()
                .any(|route| route.id == "/placeholder")
        );
        assert!(
            generate_route_types(&config).is_err(),
            "Kit2 must reject hyphenated parameter contracts"
        );
    }

    #[test]
    fn local_star_reexports_infer_without_removing_unrelated_exports() {
        let fixture = Fixture::new();
        fixture.write(
            "src/routes/+page.server.ts",
            "export * from './barrel.js'; export const prerender = false;",
        );
        fixture.write(
            "src/routes/barrel.ts",
            "export * from './shared.js'; export const unrelated = 42;",
        );
        fixture.write("src/routes/shared.ts", "export * from './barrel.js'; export const load = () => ({ name: 'Ada' }); export const actions = { save: () => ({ saved: true }) };");
        let generated = generate_route_types(&fixture.config()).unwrap();
        let page = &generated.files[&fixture.0.join(".svelte-kit/types/src/routes/$types.d.ts")];
        assert!(page.contains(".load>>>"));
        assert!(page.contains("ActionsExport"));
        assert!(
            generated
                .inputs
                .contains(&fixture.0.join("src/routes/barrel.ts"))
        );
        assert!(
            generated
                .inputs
                .contains(&fixture.0.join("src/routes/shared.ts"))
        );
        assert_eq!(
            fs::read_to_string(fixture.0.join("src/routes/barrel.ts")).unwrap(),
            "export * from './shared.js'; export const unrelated = 42;"
        );
    }

    #[test]
    fn revision_profiles_control_destructured_exports() {
        let fixture = Fixture::new();
        fixture.write("src/routes/+page.server.ts", "export const { load, actions } = { load: () => ({ x: 1 }), actions: { save: () => ({ saved: true }) } };");
        let old = generate_route_types(&fixture.config()).unwrap();
        let new = generate_route_types_for_version(
            &fixture.config(),
            &KitSyncVersion::Kit2DestructuredExports,
        )
        .unwrap();
        let path = fixture.0.join(".svelte-kit/types/src/routes/$types.d.ts");
        assert!(old.files[&path].contains("PageServerData = null"));
        assert!(new.files[&path].contains(".load>>>"));
        assert!(new.files[&path].contains("ActionsExport"));
    }
}
