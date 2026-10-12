//! Native filesystem host for the pure Tailwind 4.2.1 compiler. No JavaScript
//! configuration/plugin execution, environment lookup, or application mutation.
//! Source walking uses Tailwind's exact vendored ignore fork; see reference/.
pub mod compile;
mod discovery;
mod resolver;
mod snapshot_fs;
pub use snapshot_fs::{EntryStylesheet, FileSnapshot, HostSnapshots, ResolverProfile};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Read,
    path::{Path, PathBuf},
};
use ts_tailwind::{
    imports::ImportOptions,
    snapshot::{PluginRegistration, ProjectSnapshot},
    source_selection::{SourceInventory, SourceSelection},
    stylesheet::{StylesheetMetadata, StylesheetOptions, prepare_stylesheet},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Alias {
    /// Enhanced-resolve prefix matching; a trailing `$` restricts to exact matches.
    pub find: String,
    pub replacement: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct HostLimits {
    pub files: usize,
    pub directories: usize,
    pub stylesheets: usize,
    pub file_bytes: usize,
    pub total_bytes: usize,
}
impl Default for HostLimits {
    fn default() -> Self {
        Self {
            files: 100_000,
            directories: 100_000,
            stylesheets: 4096,
            file_bytes: 16 * 1024 * 1024,
            total_bytes: 64 * 1024 * 1024,
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HostRequest {
    /// Absolute project working directory, also the default automatic scan root.
    pub root: PathBuf,
    /// Absolute filename or filename relative to root; must already exist.
    pub entry: PathBuf,
    #[serde(default)]
    pub plugins: Vec<PluginRegistration>,
    #[serde(default)]
    pub aliases: Vec<Alias>,
    /// Explicit additional package lookup roots; no ambient NODE_PATH reads.
    #[serde(default)]
    pub module_paths: Vec<PathBuf>,
    #[serde(default)]
    pub limits: HostLimits,
}
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WatchSet {
    /// Authoritative in-memory dependencies; a disk watch cannot invalidate these.
    #[serde(default)]
    pub snapshots: BTreeMap<String, SnapshotIdentity>,
    pub files: BTreeSet<String>,
    pub directories: BTreeSet<String>,
    pub missing: BTreeSet<String>,
    /// Full content identity; snapshot version is a deterministic 53-bit digest.
    pub sha256: BTreeMap<String, String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotIdentity {
    pub version: i64,
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostProject {
    /// Input CSS maps, keyed by stylesheet identity, for final composition.
    pub input_maps: BTreeMap<String, serde_json::Value>,
    /// Real origin paths for distinct extracted stylesheet identities.
    pub stylesheet_origins: BTreeMap<String, String>,
    pub snapshot: ProjectSnapshot,
    pub options: StylesheetOptions,
    /// Filesystem-normalized source metadata. Auto uses the explicit host root.
    pub source_metadata: StylesheetMetadata,
    pub inventory: SourceInventory,
    pub selection: SourceSelection,
    pub watch: WatchSet,
    /// .env and .env.* are never read, including under automatic discovery.
    /// Explicit references to them fail. This host policy is visible to callers.
    pub unread_environment_files: BTreeSet<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostError {
    pub kind: String,
    pub path: String,
    pub message: String,
    /// Includes missing dependencies even when resolution fails.
    pub watch: WatchSet,
}
impl std::fmt::Display for HostError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.path, self.message)
    }
}
impl std::error::Error for HostError {}
pub(crate) fn fail(kind: &str, path: &Path, message: impl ToString) -> HostError {
    HostError {
        kind: kind.into(),
        path: path.to_string_lossy().into(),
        message: message.to_string(),
        watch: WatchSet::default(),
    }
}
pub(crate) fn path_string(path: &Path) -> Result<String, HostError> {
    let text = path.to_str().ok_or_else(|| {
        fail(
            "path",
            path,
            "Non-UTF-8 filesystem paths cannot be represented in a snapshot",
        )
    })?;
    let text = if cfg!(windows) {
        text.replace('\\', "/")
    } else {
        text.into()
    };
    if text.contains(['\0', '\\']) {
        return Err(fail(
            "path",
            path,
            "Path is not an unambiguous forward-slash snapshot path",
        ));
    }
    Ok(text)
}
pub(crate) fn canonical(path: &Path) -> Result<PathBuf, HostError> {
    dunce::canonicalize(path).map_err(|e| fail("io", path, e))
}
pub(crate) fn environment_file(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n == ".env" || n.starts_with(".env."))
}
pub(crate) struct State<'a> {
    pub request: &'a HostRequest,
    pub snapshots: &'a HostSnapshots,
    pub overlay: snapshot_fs::SnapshotFs,
    pub watch: WatchSet,
    pub bytes: usize,
    pub unread: BTreeSet<String>,
}
impl State<'_> {
    pub fn read(&mut self, path: &Path) -> Result<(String, i64), HostError> {
        if environment_file(path) {
            return Err(fail(
                "protectedInput",
                path,
                "Environment files are never read by the Tailwind host",
            ));
        }
        if let Some(file) = self.overlay.get(path) {
            if self.bytes.saturating_add(file.text.len()) > self.request.limits.total_bytes {
                return Err(fail("limit", path, "Snapshot byte limit exceeded"));
            }
            self.bytes += file.text.len();
            self.watch.snapshots.insert(
                path_string(&file.path)?,
                SnapshotIdentity {
                    version: file.version,
                    sha256: format!("{:x}", Sha256::digest(file.text.as_bytes())),
                },
            );
            return Ok((file.text.clone(), file.version));
        }
        let file = fs::File::open(path).map_err(|e| fail("io", path, e))?;
        let before = file.metadata().map_err(|e| fail("io", path, e))?;
        if !before.is_file() || before.len() > self.request.limits.file_bytes as u64 {
            return Err(fail(
                "limit",
                path,
                "Input is not a regular file or exceeds the file byte limit",
            ));
        }
        let mut bytes = Vec::new();
        file.take(self.request.limits.file_bytes as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| fail("io", path, e))?;
        if bytes.len() > self.request.limits.file_bytes
            || self.bytes.saturating_add(bytes.len()) > self.request.limits.total_bytes
        {
            return Err(fail("limit", path, "Snapshot byte limit exceeded"));
        }
        let after = fs::metadata(path).map_err(|e| fail("io", path, e))?;
        if before.len() != after.len() || before.modified().ok() != after.modified().ok() {
            return Err(fail(
                "changedInput",
                path,
                "Input changed while being read; retry the snapshot",
            ));
        }
        self.bytes += bytes.len();
        let hash = Sha256::digest(&bytes);
        let version =
            (u64::from_be_bytes(hash[..8].try_into().unwrap()) & ((1_u64 << 53) - 1)) as i64;
        let id = path_string(path)?;
        self.watch.files.insert(id.clone());
        self.watch.sha256.insert(id, format!("{hash:x}"));
        // Node's fs.readFile(..., 'utf8') replaces malformed UTF-8 identically.
        Ok((String::from_utf8_lossy(&bytes).into_owned(), version))
    }
}

/// Resolve the complete CSS graph, prepare native directives, and discover
/// source snapshots using the resolved source directives. No compilation run,
/// output write, background watcher, JS import, or ambient config evaluation.
pub fn load_project(request: &HostRequest) -> Result<HostProject, HostError> {
    load_project_with_snapshots(request, &HostSnapshots::default())
}

pub fn load_project_with_snapshots(
    request: &HostRequest,
    snapshots: &HostSnapshots,
) -> Result<HostProject, HostError> {
    let limits = &request.limits;
    if !request.root.is_absolute()
        || limits.files == 0
        || limits.files > 100_000
        || limits.directories == 0
        || limits.directories > 100_000
        || limits.stylesheets == 0
        || limits.stylesheets > 4096
        || limits.file_bytes == 0
        || limits.file_bytes > 64 * 1024 * 1024
        || limits.total_bytes == 0
        || limits.total_bytes > 64 * 1024 * 1024
        || request.plugins.len() > 256
        || request.aliases.len() > 1024
        || request.module_paths.len() > 1024
    {
        return Err(fail(
            "input",
            &request.root,
            "Root must be absolute and resource limits must be positive within compiler bounds",
        ));
    }
    let overlay = snapshot_fs::SnapshotFs::prepare(snapshots, &request.limits)?;
    let root = canonical(&request.root)?;
    if !root.is_dir() {
        return Err(fail("input", &root, "Project root is not a directory"));
    }
    let entry = if request.entry.is_absolute() {
        request.entry.clone()
    } else {
        root.join(&request.entry)
    };
    let mut state = State {
        request,
        snapshots,
        overlay,
        watch: WatchSet::default(),
        bytes: 0,
        unread: BTreeSet::new(),
    };
    let operation = (|| {
        let (mut snapshot, imports) = resolver::load(&mut state, &root, &entry)?;
        let options = StylesheetOptions {
            imports,
            property_fallbacks: true,
            license_comment: true,
        };
        let prepared = prepare_stylesheet(&snapshot, &options).map_err(|e| {
            fail(
                "stylesheet",
                e.source
                    .as_ref()
                    .map_or(entry.as_path(), |s| Path::new(&s.file)),
                e.to_string(),
            )
        })?;
        let (metadata, inventory, sources, selection) =
            discovery::discover(&mut state, &root, prepared.metadata())?;
        snapshot.sources = sources;
        snapshot
            .validate()
            .map_err(|e| fail("snapshot", &entry, e))?;
        let mut input_maps = BTreeMap::new();
        let mut stylesheet_origins = BTreeMap::new();
        for sheet in &snapshot.stylesheets {
            if let Some(file) = state.overlay.get(Path::new(&sheet.id)) {
                if let Some(map) = &file.map {
                    input_maps.insert(sheet.id.clone(), map.clone());
                }
            }
            stylesheet_origins.insert(sheet.id.clone(), sheet.id.clone());
        }
        if let Some(entry) = &snapshots.entry {
            if let Some(map) = &entry.map {
                input_maps.insert(entry.id.clone(), map.clone());
            }
            stylesheet_origins.insert(
                entry.id.clone(),
                path_string(
                    &snapshot_fs::identity(&entry.origin)
                        .map_err(|e| fail("path", &entry.origin, e))?,
                )?,
            );
        }
        Ok(HostProject {
            input_maps,
            stylesheet_origins,
            snapshot,
            options,
            source_metadata: metadata,
            inventory,
            selection,
            watch: state.watch.clone(),
            unread_environment_files: state.unread.clone(),
        })
    })();
    operation.map_err(|mut e: HostError| {
        e.watch = state.watch;
        e
    })
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod snapshot_tests;
