//! Vite 7.3.1 server-build externalization in `vite:resolve` (MIT): a bare id
//! that survives aliasing is decided by `createIsExternal`
//! (`ts_svelte_kit::vite_ssr::decide`). An external id must then resolve from
//! its importer (`tryNodeResolve(..., externalize)`) to a file Vite can leave
//! external; deep imports into packages without `exports` keep the resolved
//! file suffix. Otherwise the module is bundled like any other.
use oxc_resolver::{ResolveOptions, Resolver};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use ts_svelte_kit::vite_ssr::{
    self, NoExternalFilter, Probe, SsrExternalPolicy, can_externalize_file, is_bare_import,
    is_node_builtin, package_name,
};

pub(crate) struct SsrExternals {
    policy: SsrExternalPolicy,
    filter: NoExternalFilter,
    root: PathBuf,
    resolver: Resolver,
    /// Vite's `processedIds`: the first decision for an id is reused.
    decisions: Mutex<HashMap<String, bool>>,
}

impl std::fmt::Debug for SsrExternals {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SsrExternals")
            .field("policy", &self.policy)
            .field("root", &self.root)
            .finish_non_exhaustive()
    }
}

impl SsrExternals {
    pub(crate) fn new(root: &Path, policy: SsrExternalPolicy) -> Result<Self, String> {
        let filter = NoExternalFilter::new(&policy)?;
        // `resolve.externalConditions` (node, module-sync) plus `import`;
        // server main fields and Vite's default extensions; symlinks resolved.
        let resolver = Resolver::new(ResolveOptions {
            condition_names: ["node", "module-sync", "import", "default"]
                .map(String::from)
                .to_vec(),
            main_fields: ["module", "jsnext:main", "jsnext", "main"]
                .map(String::from)
                .to_vec(),
            extensions: [".mjs", ".js", ".mts", ".ts", ".jsx", ".tsx", ".json"]
                .map(String::from)
                .to_vec(),
            builtin_modules: false,
            ..ResolveOptions::default()
        });
        Ok(Self {
            policy,
            filter,
            root: root.to_path_buf(),
            resolver,
            decisions: Mutex::new(HashMap::new()),
        })
    }

    fn resolve(&self, base: &Path, id: &str) -> Option<PathBuf> {
        self.resolver
            .resolve(base, id)
            .ok()
            .map(|resolution| resolution.full_path())
    }

    /// The external id for `specifier`, or None to bundle it.
    pub(crate) fn external(&self, specifier: &str, importer: Option<&str>) -> Option<String> {
        if !is_bare_import(specifier) {
            return None;
        }
        let cached = self
            .decisions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(specifier)
            .copied();
        let decided = cached.unwrap_or_else(|| {
            let decided = vite_ssr::decide(&self.policy, &self.filter, specifier, |id| {
                // In build, isExternalizable resolves from the root.
                self.resolve(&self.root, id)
                    .map_or(Probe::Unresolved, Probe::Resolved)
            });
            self.decisions
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(specifier.to_owned(), decided);
            decided
        });
        if !decided {
            return None;
        }
        let base = importer
            .map(Path::new)
            .filter(|importer| importer.is_absolute() && importer.exists())
            .and_then(Path::parent)
            .unwrap_or(&self.root);
        let Some(resolved) = self.resolve(base, specifier) else {
            // tryNodeResolve failed: only builtins are still external.
            return is_node_builtin(specifier).then(|| specifier.to_owned());
        };
        if !can_externalize_file(&resolved) {
            return None;
        }
        Some(deep_external_id(specifier, &resolved))
    }
}

/// Vite `processResult`: for a deep import into a package without `exports`
/// whose extension differs from the resolved file, the external id becomes the
/// resolved path from the first occurrence of the id onward.
fn deep_external_id(id: &str, resolved: &Path) -> String {
    let Some(package) = package_name(id) else {
        return id.into();
    };
    if id.len() == package.len() {
        return id.into();
    }
    let extension = |path: &str| {
        Path::new(path)
            .extension()
            .map(|e| e.to_string_lossy().into_owned())
    };
    let resolved_text = resolved.to_string_lossy();
    if extension(id) == extension(&resolved_text) {
        return id.into();
    }
    let has_exports = resolved
        .ancestors()
        .find(|dir| {
            dir.ends_with(&package)
                && dir
                    .parent()
                    .is_some_and(|parent| parent.ends_with("node_modules"))
        })
        .and_then(|dir| std::fs::read(dir.join("package.json")).ok())
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .is_some_and(|package| package.get("exports").is_some());
    if has_exports {
        return id.into();
    }
    match resolved_text.find(id) {
        Some(index) => resolved_text[index..].to_owned(),
        None => id.into(),
    }
}
