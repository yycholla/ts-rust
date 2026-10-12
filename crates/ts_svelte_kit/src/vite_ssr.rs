//! Vite 7.3.1 server externalization as configured for a SvelteKit 2 build:
//! vite-plugin-svelte 6.2.4 `buildExtraViteConfig` (vitefu 1.1.2
//! `crawlFrameworkPkgs`), Kit 2.69.2's `ssr.noExternal` and the user's `ssr`
//! config, merged as Vite's `mergeConfig` concatenates them. All MIT.
//!
//! `decide` is Vite's `createIsExternal`/`createIsConfiguredAsExternal`. The
//! caller supplies the resolver probe (`isExternalizable`'s `tryNodeResolve`
//! from the root with the external conditions); this module reads only
//! package.json files.
use regress::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SsrExternalPolicy {
    /// Merged `ssr.external`: exact ids or package names.
    pub external: Vec<String>,
    /// Merged `ssr.noExternal` package-name filters.
    pub no_external: Vec<NoExternal>,
    /// `ssr.noExternal: true`: only builtins and `external` stay external.
    #[serde(default)]
    pub no_external_all: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "camelCase")]
pub enum NoExternal {
    /// A package name. Glob characters are rejected rather than approximated.
    Name(String),
    /// ECMAScript regular expression source, no flags.
    Pattern(String),
}

/// vite-plugin-svelte `COMMON_DEPENDENCIES_WITHOUT_SVELTE_FIELD`.
const COMMON_DEPENDENCIES: &[&str] = &[
    "@lukeed/uuid",
    "@playwright/test",
    "@sveltejs/kit",
    "@sveltejs/package",
    "@sveltejs/vite-plugin-svelte",
    "autoprefixer",
    "cookie",
    "dotenv",
    "esbuild",
    "eslint",
    "jest",
    "mdsvex",
    "playwright",
    "postcss",
    "prettier",
    "svelte",
    "svelte2tsx",
    "svelte-check",
    "svelte-preprocess",
    "tslib",
    "typescript",
    "vite",
    "vitest",
    "__vite-browser-external",
];
/// vite-plugin-svelte `COMMON_PREFIXES_WITHOUT_SVELTE_FIELD`.
const COMMON_PREFIXES: &[&str] = &[
    "@fontsource/",
    "@postcss-plugins/",
    "@rollup/",
    "@sveltejs/adapter-",
    "@types/",
    "@typescript-eslint/",
    "eslint-",
    "jest-",
    "postcss-plugin-",
    "prettier-plugin-",
    "rollup-plugin-",
    "vite-plugin-",
];

fn is_common_dep_without_svelte_field(dependency: &str) -> bool {
    COMMON_DEPENDENCIES.contains(&dependency)
        || COMMON_PREFIXES.iter().any(|prefix| {
            if prefix.starts_with('@') {
                dependency.starts_with(prefix)
            } else {
                let name = dependency.rsplit('/').next().unwrap_or(dependency);
                name.starts_with(prefix)
            }
        })
}

/// vite-plugin-svelte `isFrameworkPkgByJson`: a `svelte` key anywhere in an
/// object `exports`, or a truthy `svelte` field.
fn is_framework_by_json(package: &Value) -> bool {
    fn has_svelte_key(value: &Value) -> bool {
        match value {
            Value::Object(map) => map
                .iter()
                .any(|(key, value)| key == "svelte" || has_svelte_key(value)),
            Value::Array(items) => items.iter().any(has_svelte_key),
            _ => false,
        }
    }
    let condition = package["exports"].is_object() && has_svelte_key(&package["exports"]);
    condition || truthy(&package["svelte"])
}
fn is_semi_framework_by_json(package: &Value) -> bool {
    truthy(&package["dependencies"]["svelte"]) || truthy(&package["peerDependencies"]["svelte"])
}
fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|n| n != 0.0 && !n.is_nan()),
        Value::String(s) => !s.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}
fn keys(value: &Value) -> Vec<String> {
    value
        .as_object()
        .map(|map| map.keys().cloned().collect())
        .unwrap_or_default()
}
fn read_json(path: &Path) -> Option<Value> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}
/// vitefu `_findDepPkgJsonPath` (non-PnP): walk up from the parent package.json
/// path through `node_modules/<dep>/package.json`, returning the real path.
fn find_dep_package_json(dep: &str, parent: &Path) -> Option<PathBuf> {
    let mut root = Some(parent);
    while let Some(directory) = root {
        let candidate = directory
            .join("node_modules")
            .join(dep)
            .join("package.json");
        if candidate.is_file() {
            return std::fs::canonicalize(candidate).ok();
        }
        root = directory.parent();
    }
    None
}

/// vitefu `crawlFrameworkPkgs` in build mode for vite-plugin-svelte's
/// predicates: returns `ssr.noExternal` in discovery order (duplicates kept).
/// The crawl is sequential; vitefu's concurrent siblings only reorder pushes,
/// which a set membership filter cannot observe.
pub fn crawl_svelte_packages(root: &Path) -> Result<Vec<String>, String> {
    let mut directory = Some(root);
    let mut package_json = None;
    while let Some(current) = directory {
        let candidate = current.join("package.json");
        if candidate.is_file() {
            package_json = Some(candidate);
            break;
        }
        directory = current.parent();
    }
    let Some(package_json) = package_json else {
        return Ok(vec![]);
    };
    let package = read_json(&package_json)
        .ok_or_else(|| format!("Unable to read {}", package_json.display()))?;
    let mut no_external = vec![];
    crawl(&package_json, &package, &[], &mut no_external);
    Ok(no_external)
}
fn crawl(package_json: &Path, package: &Value, parents: &[String], out: &mut Vec<String>) {
    // Workspace detection (private workspace packages) is not reached for an
    // ordinary single-package root: only the root crawls devDependencies.
    let is_root = parents.is_empty();
    let mut deps = keys(&package["dependencies"]);
    if is_root {
        deps.extend(keys(&package["devDependencies"]));
    }
    let deps: Vec<String> = deps
        .into_iter()
        .filter(|dep| !parents.contains(dep) && !is_common_dep_without_svelte_field(dep))
        .collect();
    let parent_directory = package_json.parent().unwrap_or(package_json);
    for dep in deps {
        let Some(dep_json) = find_dep_package_json(&dep, parent_directory) else {
            continue;
        };
        let Some(dep_package) = read_json(&dep_json) else {
            continue;
        };
        let nested = || {
            parents
                .iter()
                .cloned()
                .chain([dep.clone()])
                .collect::<Vec<_>>()
        };
        if out.contains(&dep) {
            crawl(&dep_json, &dep_package, &nested(), out);
            continue;
        }
        if is_framework_by_json(&dep_package) || is_semi_framework_by_json(&dep_package) {
            out.push(dep.clone());
            crawl(&dep_json, &dep_package, &nested(), out);
        }
    }
}

/// The merged server policy for a Kit 2 build with vite-plugin-svelte 6.
pub fn kit2_server_policy(
    root: &Path,
    user_external: &[String],
    user_no_external: &[NoExternal],
) -> Result<SsrExternalPolicy, String> {
    let externaled = |dep: &str| user_external.iter().any(|item| item == dep);
    let mut no_external = user_no_external.to_vec();
    // Kit's setup plugin config hook.
    no_external.push(NoExternal::Name("esm-env".into()));
    no_external.push(NoExternal::Name("@sveltejs/kit/src/runtime".into()));
    // vite-plugin-svelte buildExtraConfigForSvelte.
    if !externaled("svelte") {
        no_external.push(NoExternal::Name("svelte".into()));
        no_external.push(NoExternal::Pattern(r"^svelte\/".into()));
    }
    if !externaled("esm-env") {
        no_external.push(NoExternal::Name("esm-env".into()));
    }
    // buildExtraConfigForDependencies, minus deps the user externalized.
    for dep in crawl_svelte_packages(root)? {
        if !externaled(&dep) {
            no_external.push(NoExternal::Name(dep));
        }
    }
    for entry in &no_external {
        if let NoExternal::Name(name) = entry {
            if name.contains(['*', '?', '[', ']', '{', '}', '!', '(', ')']) {
                return Err(format!(
                    "ssr.noExternal glob {name:?} is not supported natively"
                ));
            }
        } else if let NoExternal::Pattern(pattern) = entry {
            Regex::new(pattern).map_err(|e| format!("Invalid noExternal pattern: {e:?}"))?;
        }
    }
    Ok(SsrExternalPolicy {
        external: user_external.to_vec(),
        no_external,
        no_external_all: false,
    })
}

/// The server policy of a Kit 3 build with adapter-node 6.0.0: its post
/// plugin sets `ssr.noExternal: true` and externalizes the package's
/// production `dependencies` except those already in `ssr.noExternal` (Kit's
/// and vite-plugin-svelte's entries, matched by name or pattern).
pub fn kit3_adapter_node_policy(
    root: &Path,
    package_json: &serde_json::Value,
) -> Result<SsrExternalPolicy, String> {
    let base = kit2_server_policy(root, &[], &[])?;
    let filter = NoExternalFilter::new(&base)?;
    let external = package_json["dependencies"]
        .as_object()
        .into_iter()
        .flatten()
        .map(|(name, _)| name.clone())
        .filter(|name| !filter.excludes(name))
        .collect();
    Ok(SsrExternalPolicy {
        external,
        no_external: vec![],
        no_external_all: true,
    })
}

/// Vite `getNpmPackageName`.
pub fn package_name(id: &str) -> Option<String> {
    let mut parts = id.split('/');
    let first = parts.next()?;
    if first.starts_with('@') {
        let second = parts.next().filter(|second| !second.is_empty())?;
        Some(format!("{first}/{second}"))
    } else {
        Some(first.into())
    }
}
/// Vite `bareImportRE`: `/^(?![a-zA-Z]:)[\w@](?!.*:\/\/)/`.
pub fn is_bare_import(id: &str) -> bool {
    let bytes = id.as_bytes();
    let drive = bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':';
    let first = bytes
        .first()
        .is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_' || *b == b'@');
    !drive && first && !id[1..].contains("://")
}

/// Node.js 22 `module.builtinModules` (without `node:`-only entries) plus
/// Vite's `node:` prefix rule.
pub fn is_node_builtin(id: &str) -> bool {
    const BUILTINS: &[&str] = &[
        "_http_agent",
        "_http_client",
        "_http_common",
        "_http_incoming",
        "_http_outgoing",
        "_http_server",
        "_stream_duplex",
        "_stream_passthrough",
        "_stream_readable",
        "_stream_transform",
        "_stream_wrap",
        "_stream_writable",
        "_tls_common",
        "_tls_wrap",
        "assert",
        "assert/strict",
        "async_hooks",
        "buffer",
        "child_process",
        "cluster",
        "console",
        "constants",
        "crypto",
        "dgram",
        "diagnostics_channel",
        "dns",
        "dns/promises",
        "domain",
        "events",
        "fs",
        "fs/promises",
        "http",
        "http2",
        "https",
        "inspector",
        "inspector/promises",
        "module",
        "net",
        "os",
        "path",
        "path/posix",
        "path/win32",
        "perf_hooks",
        "process",
        "punycode",
        "querystring",
        "readline",
        "readline/promises",
        "repl",
        "stream",
        "stream/consumers",
        "stream/promises",
        "stream/web",
        "string_decoder",
        "sys",
        "timers",
        "timers/promises",
        "tls",
        "trace_events",
        "tty",
        "url",
        "util",
        "util/types",
        "v8",
        "vm",
        "wasi",
        "worker_threads",
        "zlib",
    ];
    id.starts_with("node:") || BUILTINS.contains(&id)
}

/// Compiled `noExternal` filter (`createFilter(undefined, noExternal)`).
pub struct NoExternalFilter {
    names: BTreeSet<String>,
    patterns: Vec<Regex>,
}
impl NoExternalFilter {
    pub fn new(policy: &SsrExternalPolicy) -> Result<Self, String> {
        let mut names = BTreeSet::new();
        let mut patterns = vec![];
        for entry in &policy.no_external {
            match entry {
                NoExternal::Name(name) => {
                    names.insert(name.clone());
                }
                NoExternal::Pattern(pattern) => patterns.push(
                    Regex::new(pattern)
                        .map_err(|e| format!("Invalid noExternal pattern: {e:?}"))?,
                ),
            }
        }
        Ok(Self { names, patterns })
    }
    fn excludes(&self, package: &str) -> bool {
        self.names.contains(package) || self.patterns.iter().any(|p| p.find(package).is_some())
    }
}

/// How `isExternalizable` resolved a bare id from the project root.
pub enum Probe {
    /// `tryNodeResolve` found nothing or threw.
    Unresolved,
    Resolved(PathBuf),
}

/// Vite `createIsExternal` for one id (callers cache per id like Vite's
/// `processedIds`). `probe` resolves the id from the root with the external
/// conditions. Linked packages are only externalized when configured.
pub fn decide(
    policy: &SsrExternalPolicy,
    filter: &NoExternalFilter,
    id: &str,
    probe: impl FnOnce(&str) -> Probe,
) -> bool {
    if id.starts_with('.') || Path::new(id).is_absolute() {
        return false;
    }
    if is_node_builtin(id) || policy.external.iter().any(|item| item == id) {
        return true;
    }
    let externalizable = |configured: bool, probe: Probe| -> bool {
        if !is_bare_import(id) || id.contains('\0') {
            return false;
        }
        match probe {
            Probe::Unresolved => false,
            Probe::Resolved(path) => {
                let text = path.to_string_lossy();
                if !configured && !text.contains("node_modules") {
                    return false;
                }
                can_externalize_file(&path)
            }
        }
    };
    let Some(package) = package_name(id) else {
        return externalizable(false, probe(id));
    };
    if policy.external.iter().any(|item| *item == package) {
        return externalizable(true, probe(id));
    }
    if policy.no_external_all || filter.excludes(&package) {
        return false;
    }
    externalizable(false, probe(id))
}

/// Vite `canExternalizeFile`.
pub fn can_externalize_file(path: &Path) -> bool {
    match path.extension().and_then(|e| e.to_str()) {
        None => true,
        Some(ext) => matches!(ext, "js" | "mjs" | "cjs"),
    }
}

#[cfg(test)]
#[path = "vite_ssr_tests.rs"]
mod tests;
