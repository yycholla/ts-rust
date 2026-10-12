//! Explicit native bundle entry for Kit's real prerender argument encoder.
//!
//! Source contracts: @sveltejs/kit 2.69.2 and 3.0.1
//! `src/core/postbuild/prerender.js`, `src/runtime/shared.js`, and Kit3
//! `src/runtime/server/sourcemaps.js`. The registered 2.67.0/2.68.0/2.69.3/
//! 2.70.3 encoder signatures agree with 2.69.2; next.32 agrees with 3.0.1.
//! This module emits reexports, not a port or substitute for their algorithms.
//! Kit2's `(await internal.get_hooks()).transport ?? {}` remains a runtime step.
//! Kit3's live transport exports and optional process.getBuiltinModule support
//! remain in the actual framework dependency graph.
use serde::Serialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Component, Path, PathBuf},
};
use ts_svelte_bundle::{BundleRequest, BundleResult, Target, output::EntrySignatures};
use ts_svelte_kit::{build_inventory::BuildInventory, sync_versions::KitSyncVersion};

const ENTRY_NAME: &str = "prerender-remote-helpers";
const STRINGIFY: &str = "stringify_remote_arg";
const FIX_STACK: &str = "fix_stack_trace";

/// Immutable identity carried from graph preparation through output selection.
/// Construct with canonical inventory paths; no filesystem access occurs here.
#[derive(Clone, Debug)]
pub struct RemotePrerenderEntry {
    root: PathBuf,
    kit_version: String,
    module_id: PathBuf,
    source: String,
    imports: Vec<(PathBuf, &'static str)>,
    kit3: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EmittedRemotePrerenderEntry {
    /// Actual bundler output name, relative to the server artifact root.
    pub file_name: String,
    /// Exact virtual facade identity, not a name inferred from generated code.
    pub facade_module_id: String,
    pub stringify_export: String,
    /// Absent on Kit2; do not substitute an unrelated internal export.
    pub fix_stack_trace_export: Option<String>,
    /// Kit2 must acquire the actual hooks transport before invoking the encoder.
    pub requires_hooks_transport: bool,
}

impl RemotePrerenderEntry {
    pub fn from_inventory(inventory: &BuildInventory) -> Result<Self, String> {
        Self::new(
            &inventory.kit_version,
            &inventory.project_root,
            &inventory.generated_directory,
            &inventory.runtime_directory,
        )
    }

    /// Resolved-parts constructor also supports immutable host snapshots.
    /// `runtime_directory` must be the canonical installed Kit runtime identity.
    pub fn new(
        version: &str,
        root: &Path,
        generated: &Path,
        runtime_directory: &Path,
    ) -> Result<Self, String> {
        let version_profile = KitSyncVersion::parse(version)?;
        for path in [root, generated, runtime_directory] {
            identity(path)?;
        }
        let module_id = generated.join(".native-virtual/prerender-remote-helpers.js");
        identity(&module_id)?;
        let kit3 = version_profile.is_kit3();
        let mut imports = vec![(runtime_directory.join("shared.js"), STRINGIFY)];
        if kit3 {
            imports.push((runtime_directory.join("server/sourcemaps.js"), FIX_STACK));
        }
        let mut source = String::new();
        for (path, export) in &imports {
            if path == &module_id {
                return Err("Prerender helper cannot import its own facade".into());
            }
            let specifier = serde_json::to_string(identity(path)?)
                .map_err(|e| e.to_string())?
                .replace('\u{2028}', "\\u2028")
                .replace('\u{2029}', "\\u2029");
            source.push_str(&format!("export {{ {export} }} from {specifier};\n"));
        }
        Ok(Self {
            root: root.into(),
            kit_version: version.into(),
            module_id,
            source,
            imports,
            kit3,
        })
    }

    pub fn module_id(&self) -> &Path {
        &self.module_id
    }
    pub fn entry_name(&self) -> &'static str {
        ENTRY_NAME
    }
    pub fn source(&self) -> &str {
        &self.source
    }
    pub fn required_modules(&self) -> impl Iterator<Item = &Path> {
        self.imports.iter().map(|(path, _)| path.as_path())
    }

    /// Add to the existing server bundle, retaining its aliases, conditions,
    /// transforms and framework snapshots. Validation completes before mutation.
    /// Reapplying the same plan is idempotent; conflicting identities fail.
    pub fn apply(&self, request: &mut BundleRequest) -> Result<(), String> {
        if request.target != Target::Server || request.node_rebundle {
            return Err("Prerender remote helper requires the framework server graph".into());
        }
        if request.root != self.root
            || request.compatibility.kit_version.as_deref() != Some(self.kit_version.as_str())
        {
            return Err(
                "Prerender helper root or Kit version differs from the server graph".into(),
            );
        }
        if request.output.code_splitting == Some(false) {
            return Err("Prerender helper requires the existing split server graph".into());
        }
        if matches!(
            request.output.preserve_entry_signatures,
            Some(EntrySignatures::False | EntrySignatures::AllowExtension)
        ) {
            return Err("Prerender helper requires strict or exports-only entry signatures".into());
        }
        if let Some(entry) = request.entries.get(ENTRY_NAME) {
            if entry != &self.module_id {
                return Err("Prerender helper entry name is already occupied".into());
            }
        }
        if request.entries.iter().any(|(name, path)| {
            name != ENTRY_NAME && absolute(&request.root, path) == self.module_id
        }) {
            return Err("Prerender helper facade already has another entry name".into());
        }
        for (path, source) in &request.virtual_modules {
            if absolute(&request.root, path) == self.module_id
                && (path != &self.module_id || source != &self.source)
            {
                return Err(
                    "Prerender helper source snapshot conflicts with an existing module".into(),
                );
            }
        }
        if request
            .virtual_module_maps
            .keys()
            .any(|p| absolute(&request.root, p) == self.module_id)
        {
            return Err("Prerender helper must not inherit another source's map".into());
        }
        for path in std::iter::once(&self.module_id).chain(self.imports.iter().map(|(p, _)| p)) {
            let id = identity(path)?;
            if request.external.iter().any(|external| external == id)
                || request
                    .external_packages
                    .iter()
                    .any(|package| package == "@sveltejs/kit")
            {
                return Err(format!("Prerender helper module must be bundled: {id}"));
            }
            if request
                .aliases
                .iter()
                .filter_map(|a| a.resolve(id))
                .any(|replacement| replacement != *path)
            {
                return Err(format!(
                    "Alias redirects the pinned prerender helper identity: {id}"
                ));
            }
        }
        request
            .entries
            .insert(ENTRY_NAME.into(), self.module_id.clone());
        request
            .virtual_modules
            .insert(self.module_id.clone(), self.source.clone());
        if request.output.preserve_entry_signatures.is_none() {
            request.output.preserve_entry_signatures = Some(EntrySignatures::Strict);
        }
        Ok(())
    }

    /// Resolve the facade in the actual bundle and validate its retained export
    /// contract and direct import identities. No fallback to names, substrings,
    /// source-code export matching, or a source module's shared chunk is used.
    /// Root binds this result to its server-build receipt before runtime use.
    pub fn emitted(&self, bundle: &BundleResult) -> Result<EmittedRemotePrerenderEntry, String> {
        self.emitted_with_real_paths(bundle, &BTreeMap::new())
    }

    /// The filesystem host supplies observed real paths when package locations
    /// contain symlinks. Virtual modules retain their exact snapshot identity.
    /// This pure helper never probes disk or infers identity from a basename.
    pub fn emitted_with_real_paths(
        &self,
        bundle: &BundleResult,
        real_paths: &BTreeMap<PathBuf, PathBuf>,
    ) -> Result<EmittedRemotePrerenderEntry, String> {
        if bundle.target != Target::Server {
            return Err("Prerender helper output is not a server bundle".into());
        }
        let id = identity(&self.module_id)?;
        let matches: Vec<_> = bundle
            .chunks
            .iter()
            .filter(|chunk| chunk.facade_module_id.as_deref() == Some(id))
            .collect();
        if matches.len() != 1 {
            return Err(format!(
                "Expected one emitted prerender helper facade {id:?}, found {}",
                matches.len()
            ));
        }
        let chunk = matches[0];
        if !chunk.is_entry {
            return Err("Prerender helper facade is not an entry chunk".into());
        }
        output_name(&chunk.file_name)?;
        if bundle
            .chunks
            .iter()
            .filter(|c| c.file_name == chunk.file_name)
            .count()
            != 1
            || bundle.assets.iter().any(|a| a.file_name == chunk.file_name)
        {
            return Err("Emitted prerender helper filename collides with another output".into());
        }
        let expected: BTreeSet<_> = self.imports.iter().map(|(_, export)| *export).collect();
        if chunk.exports.len() != expected.len()
            || chunk
                .exports
                .iter()
                .map(String::as_str)
                .collect::<BTreeSet<_>>()
                != expected
        {
            return Err(
                "Emitted prerender helper exports do not match the pinned entry contract".into(),
            );
        }
        let module = bundle
            .module_graph
            .get(id)
            .ok_or("Missing prerender helper module metadata")?;
        if module.id != id || !module.is_entry || module.is_external {
            return Err("Prerender helper metadata has a different or external identity".into());
        }
        if module.exports.len() != expected.len()
            || module
                .exports
                .iter()
                .map(String::as_str)
                .collect::<BTreeSet<_>>()
                != expected
        {
            return Err(
                "Prerender helper module metadata has an unexpected export contract".into(),
            );
        }
        let expected_imports: BTreeSet<_> = self
            .imports
            .iter()
            .map(|(path, _)| identity(real_paths.get(path).unwrap_or(path)))
            .collect::<Result<_, _>>()?;
        if !module.dynamic_imports.is_empty()
            || module.imports.len() != expected_imports.len()
            || module
                .imports
                .iter()
                .map(String::as_str)
                .collect::<BTreeSet<_>>()
                != expected_imports
        {
            return Err(format!(
                "Prerender helper direct imports differ from canonical Kit identities: expected {expected_imports:?}, actual {:?}, dynamic {:?}",
                module.imports, module.dynamic_imports
            ));
        }
        for (path, export) in &self.imports {
            let path = identity(real_paths.get(path).unwrap_or(path))?;
            let imported = bundle
                .module_graph
                .get(path)
                .ok_or_else(|| format!("Missing pinned helper dependency metadata: {path}"))?;
            if imported.id != path
                || imported.is_external
                || !imported.exports.iter().any(|name| name == export)
            {
                return Err(format!(
                    "Pinned helper dependency {path} does not provide bundled export {export}"
                ));
            }
        }
        Ok(EmittedRemotePrerenderEntry {
            file_name: chunk.file_name.clone(),
            facade_module_id: id.into(),
            stringify_export: STRINGIFY.into(),
            fix_stack_trace_export: self.kit3.then(|| FIX_STACK.into()),
            requires_hooks_transport: !self.kit3,
        })
    }
}

fn identity(path: &Path) -> Result<&str, String> {
    let text = path.to_str().ok_or("Prerender helper paths must be UTF8")?;
    if !path.is_absolute()
        || text.contains(['\0', '\\', '?', '#'])
        || path
            .components()
            .any(|p| matches!(p, Component::CurDir | Component::ParentDir))
        || path.components().collect::<PathBuf>().as_os_str() != path.as_os_str()
    {
        return Err(format!(
            "Prerender helper requires a normalized absolute module identity: {text:?}"
        ));
    }
    Ok(text)
}
fn absolute(root: &Path, path: &Path) -> PathBuf {
    // BundleRequest's virtual paths are lexical; never resolve by reading disk.
    let path = root.join(path);
    let mut result = PathBuf::new();
    for part in path.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                result.pop();
            }
            _ => result.push(part.as_os_str()),
        }
    }
    result
}
fn output_name(name: &str) -> Result<(), String> {
    if name.is_empty()
        || name.contains(['\\', '\0', '?', '#', ':'])
        || name
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err(format!("Invalid prerender helper output name {name:?}"));
    }
    Ok(())
}

#[cfg(test)]
#[path = "prerender_remote_entry_tests.rs"]
mod tests;
