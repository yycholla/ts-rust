//! Native explicit-project checker host. Each semantic check runs in an isolated process.
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use ts_svelte::{
    Snapshot,
    compatibility::{self, Versions},
};

static NEXT: AtomicU64 = AtomicU64::new(0);
#[cfg(unix)]
pub(crate) static ACTIVE_CHECK: std::sync::Mutex<Option<ActiveCheck>> = std::sync::Mutex::new(None);
#[cfg(unix)]
pub(crate) struct ActiveCheck {
    pub(crate) pid: Option<u32>,
    pub(crate) directory: PathBuf,
}

/// Terminate the active checker process group when the editor stops its server.
/// # Errors
/// Returns signal registration or thread creation errors.
#[cfg(unix)]
pub fn install_termination_handler() -> std::io::Result<()> {
    use signal_hook::{
        consts::{SIGINT, SIGTERM},
        iterator::Signals,
    };
    let mut signals = Signals::new([SIGINT, SIGTERM])?;
    std::thread::Builder::new()
        .name("svelte-lsp-signals".into())
        .spawn(move || {
            if let Some(signal) = signals.forever().next() {
                // Serialize against spawn/registration and reaping, avoiding a gap
                // where the signal handler could overlook a newly spawned child.
                let active = ACTIVE_CHECK
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if let Some(check) = active.as_ref() {
                    if let Some(pid) = check.pid {
                        let _ = Command::new("kill")
                            .args(["-KILL", "--", &format!("-{pid}")])
                            .status();
                    }
                    let _ = std::fs::remove_dir_all(&check.directory);
                }
                let provider = crate::project_provider::ACTIVE_PROVIDER
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if let Some(pid) = *provider {
                    let _ = Command::new("kill")
                        .args(["-KILL", "--", &format!("-{pid}")])
                        .status();
                }
                std::process::exit(128 + signal);
            }
        })?;
    Ok(())
}
const INPUT_LIMIT: usize = 64 * 1024 * 1024;
const OUTPUT_LIMIT: usize = 64 * 1024 * 1024;
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Config {
    pub root: PathBuf,
    pub dependencies: PathBuf,
    pub components: Vec<ComponentInput>,
    #[serde(default)]
    pub files: Vec<String>,
    #[serde(default)]
    pub auxiliary_files: Vec<String>,
    #[serde(default)]
    pub compiler_options: Option<serde_json::Map<String, Value>>,
    #[serde(default)]
    pub rune_modules: bool,
    #[serde(default)]
    pub snapshot_guard: Option<SnapshotGuard>,
    #[serde(default)]
    pub runes: Option<bool>,
    #[serde(default)]
    pub allow_prerelease: bool,
    #[serde(default)]
    pub project_provider: Option<crate::project_provider::ProjectProvider>,
    #[serde(skip)]
    resolved_versions: Option<Value>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComponentInput {
    pub path: String,
    #[serde(default)]
    pub typescript: Option<bool>,
    #[serde(default = "default_check")]
    pub check: bool,
}
const fn default_check() -> bool {
    true
}

/// Generic guards for generated project inputs supplied by a framework plugin.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SnapshotGuard {
    files: BTreeMap<String, String>,
    directories: BTreeMap<String, Vec<String>>,
    dependencies: BTreeMap<String, String>,
}

fn guard_path(root: &Path, path: &str) -> Result<PathBuf, String> {
    if Path::new(path).is_absolute()
        || Path::new(path)
            .components()
            .any(|p| !matches!(p, Component::Normal(_) | Component::CurDir))
    {
        return Err("Snapshot guard path must remain within its root".into());
    }
    let full = root.join(path).canonicalize().map_err(|e| e.to_string())?;
    if !full.starts_with(root) {
        return Err("Snapshot guard symlink escapes root".into());
    }
    Ok(full)
}

// Package managers may link a package directory outside node_modules. Resolve
// that explicit package boundary, then keep every guarded file inside it.
fn dependency_guard_path(root: &Path, path: &str) -> Result<PathBuf, String> {
    let parts = path.split('/').collect::<Vec<_>>();
    if parts
        .iter()
        .any(|p| p.is_empty() || *p == "." || *p == ".." || p.contains('\\'))
    {
        return Err("Invalid dependency snapshot path".into());
    }
    let count = if parts[0].starts_with('@') { 2 } else { 1 };
    if parts.len() <= count {
        return guard_path(root, path);
    }
    let package = root
        .join(parts[..count].join("/"))
        .canonicalize()
        .map_err(|e| e.to_string())?;
    if !package.is_dir() {
        return Err("Dependency package root is not a directory".into());
    }
    guard_path(&package, &parts[count..].join("/"))
}

fn bounded_read(path: &Path, limit: usize) -> Result<String, String> {
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    if file.metadata().map_err(|e| e.to_string())?.len() > limit as u64 {
        return Err(format!("Input exceeds {limit} bytes"));
    }
    let mut text = String::new();
    file.take(limit as u64 + 1)
        .read_to_string(&mut text)
        .map_err(|e| e.to_string())?;
    if text.len() > limit {
        return Err(format!("Input exceeds {limit} bytes"));
    }
    Ok(text)
}
pub(crate) struct Workspace(pub(crate) PathBuf);
impl Workspace {
    fn create(directory: PathBuf) -> Result<Self, String> {
        #[cfg(unix)]
        let mut registration = ACTIVE_CHECK
            .lock()
            .map_err(|_| "Checker registration lock poisoned")?;
        #[cfg(unix)]
        if registration.is_some() {
            return Err("Concurrent project checks are unsupported".into());
        }
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(&directory).map_err(|e| e.to_string())?;
        #[cfg(unix)]
        {
            *registration = Some(ActiveCheck {
                pid: None,
                directory: directory.clone(),
            });
        }
        Ok(Self(directory))
    }
}
impl Drop for Workspace {
    fn drop(&mut self) {
        #[cfg(unix)]
        let mut registration = ACTIVE_CHECK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _ = std::fs::remove_dir_all(&self.0);
        #[cfg(unix)]
        {
            *registration = None;
        }
    }
}
impl Config {
    /// Validate the explicit graph and its installed compatibility profile.
    /// # Errors
    /// Rejects invalid, duplicate, reserved or escaping paths and unsupported profiles.
    pub fn validate(&mut self) -> Result<(), String> {
        if let Some(provider) = &self.project_provider {
            provider.validate()?;
        }
        self.root = self.root.canonicalize().map_err(|e| e.to_string())?;
        self.dependencies = self
            .dependencies
            .canonicalize()
            .map_err(|e| e.to_string())?;
        if self.components.is_empty() && self.files.is_empty() && self.auxiliary_files.is_empty()
            || self.components.len() + self.files.len() + self.auxiliary_files.len() > 4096
        {
            return Err("Explicit project requires 1..4096 inputs".into());
        }
        let mut paths = BTreeSet::new();
        for path in self
            .components
            .iter()
            .map(|c| &c.path)
            .chain(self.files.iter())
            .chain(self.auxiliary_files.iter())
        {
            if path.is_empty()
                || Path::new(path)
                    .components()
                    .any(|c| !matches!(c, Component::Normal(_)))
                || path
                    .split(['/', '\\'])
                    .any(|c| matches!(c, "node_modules" | "__svelte" | "tsconfig.json"))
            {
                return Err(
                    "Project input paths must be relative without traversal or reserved names"
                        .into(),
                );
            }
            if !paths.insert(self.input_path(path)?) {
                return Err("Duplicate project input".into());
            }
        }
        self.resolved_versions =
            Some(serde_json::to_value(self.versions()?).map_err(|e| e.to_string())?);
        self.validate_snapshot(&[])?;
        Ok(())
    }
    /// Rediscover through the host-selected framework plugin. The provider and
    /// project root remain fixed for this editor session.
    pub fn rediscover(&self) -> Result<Option<Self>, String> {
        let Some(provider) = &self.project_provider else {
            return Ok(None);
        };
        let value = provider.discover(&self.root)?;
        let mut next: Self = serde_json::from_value(value).map_err(|e| e.to_string())?;
        next.project_provider = Some(provider.clone());
        next.validate()?;
        if next.root != self.root {
            return Err("Project provider cannot change the session root".into());
        }
        Ok(Some(next))
    }
    pub(crate) fn validate_snapshot(&self, documents: &[Snapshot]) -> Result<(), String> {
        let Some(guard) = &self.snapshot_guard else {
            return Ok(());
        };
        if guard.files.len() + guard.directories.len() > 8192 || guard.dependencies.len() > 32 {
            return Err("Snapshot guard exceeds limits".into());
        }
        for (path, expected) in &guard.files {
            let full = guard_path(&self.root, path)?;
            let mut text = bounded_read(&full, 1024 * 1024)?;
            for document in documents {
                if url::Url::parse(&document.uri)
                    .ok()
                    .and_then(|u| u.to_file_path().ok())
                    .and_then(|p| p.canonicalize().ok())
                    .as_ref()
                    == Some(&full)
                {
                    text.clone_from(&document.text);
                }
            }
            if format!("{:x}", Sha256::digest(text.as_bytes())) != *expected {
                return Err(format!(
                    "Generated project snapshot is stale after {path} changed; regenerate framework types and reinitialize"
                ));
            }
        }
        for (path, expected) in &guard.directories {
            let full = guard_path(&self.root, path)?;
            let mut names = Vec::new();
            for entry in std::fs::read_dir(full).map_err(|e| e.to_string())? {
                if names.len() > 16384 {
                    return Err("Snapshot directory exceeds entry limit".into());
                }
                names.push(
                    entry
                        .map_err(|e| e.to_string())?
                        .file_name()
                        .into_string()
                        .map_err(|_| "Non-UTF8 directory entry")?,
                );
            }
            names.sort();
            if names != *expected {
                return Err(format!(
                    "Project file roster changed in {path}; rediscover and reinitialize"
                ));
            }
        }
        for (path, expected) in &guard.dependencies {
            let full = dependency_guard_path(&self.dependencies, path)?;
            let text = bounded_read(&full, 1024 * 1024)?;
            if format!("{:x}", Sha256::digest(text.as_bytes())) != *expected {
                return Err("Project dependency snapshot changed; reinitialize".into());
            }
        }
        Ok(())
    }
    fn input_path(&self, path: &str) -> Result<PathBuf, String> {
        let resolved = self
            .root
            .join(path)
            .canonicalize()
            .map_err(|e| format!("{path}: {e}"))?;
        if !resolved.starts_with(&self.root) || !resolved.is_file() {
            return Err("Project input escapes root or is not a file".into());
        }
        Ok(resolved)
    }
    fn versions(&self) -> Result<Versions, String> {
        let package = |name: &str, required: bool| -> Result<Option<String>, String> {
            let path = self.dependencies.join(name).join("package.json");
            if !required && !path.exists() {
                return Ok(None);
            }
            let value: Value = serde_json::from_str(&bounded_read(&path, 1024 * 1024)?)
                .map_err(|e| e.to_string())?;
            Ok(Some(
                value["version"]
                    .as_str()
                    .ok_or("Missing package version")?
                    .into(),
            ))
        };
        let versions = Versions {
            svelte_version: package("svelte", true)?.ok_or("Missing Svelte")?,
            kit_version: package("@sveltejs/kit", false)?,
            typescript_version: package("typescript", false)?,
            allow_prerelease: self.allow_prerelease,
        };
        compatibility::resolve(&versions)?;
        Ok(versions)
    }
    /// Build a session configuration from the project's single resolved profile.
    /// # Errors
    /// Explicit settings conflicting with the project are rejected atomically.
    pub fn session_configuration(&self, settings: &Value) -> Result<Value, String> {
        let versions = serde_json::to_value(self.versions()?).map_err(|e| e.to_string())?;
        if self.resolved_versions.as_ref() != Some(&versions) {
            return Err("Project dependencies changed; reinitialize the server".into());
        }
        if let Some(requested) = settings.get("runes")
            && *requested != json!(self.runes)
        {
            return Err("Session runes conflicts with explicit project runes".into());
        }
        if let Some(requested) = settings.get("compatibility") {
            for (key, value) in requested
                .as_object()
                .ok_or("Expected compatibility object")?
            {
                if versions.get(key) != Some(value) {
                    return Err(format!(
                        "Session {key} conflicts with installed project profile"
                    ));
                }
            }
        }
        Ok(
            json!({"op":"sessionConfigure","compatibility":versions,"runes":self.runes,"lintConfig":settings["lintConfig"]}),
        )
    }
    /// Reject ambiguous editor ownership before adding a buffer.
    /// # Errors
    /// Project sessions require file URIs and a single URI per canonical source.
    pub fn validate_open_uri(&self, uri: &str, documents: &[Snapshot]) -> Result<(), String> {
        let parsed = url::Url::parse(uri).map_err(|e| e.to_string())?;
        if parsed.query().is_some() || parsed.fragment().is_some() {
            return Err("Project file URI cannot contain query or fragment".into());
        }
        let path = parsed
            .to_file_path()
            .map_err(|()| "Project session requires file URIs")?;
        if let Ok(canonical) = path.canonicalize() {
            for document in documents {
                if document.uri != uri
                    && url::Url::parse(&document.uri)
                        .ok()
                        .and_then(|url| url.to_file_path().ok())
                        .and_then(|path| path.canonicalize().ok())
                        .as_ref()
                        == Some(&canonical)
                {
                    return Err("Source already open under another URI".into());
                }
            }
        }
        Ok(())
    }
    pub(crate) fn source(&self, path: &str, documents: &[Snapshot]) -> Result<Snapshot, String> {
        let resolved = self.input_path(path)?;
        let mut overlay = None;
        for document in documents {
            if let Ok(uri) = url::Url::parse(&document.uri)
                && uri.query().is_none()
                && uri.fragment().is_none()
                && let Ok(file) = uri.to_file_path()
                && file.canonicalize().ok().as_ref() == Some(&resolved)
            {
                if overlay.is_some() {
                    return Err("Multiple open URI aliases for one source file".into());
                }
                overlay = Some(document.clone());
            }
        }
        if let Some(document) = overlay {
            return Ok(document);
        }
        Ok(Snapshot {
            uri: url::Url::from_file_path(self.root.join(path))
                .map_err(|()| "Invalid file URI")?
                .into(),
            version: 0,
            text: bounded_read(&resolved, ts_svelte_compiler::MAX_SOURCE_BYTES)?,
        })
    }
    pub(crate) fn prepare(&self, documents: &[Snapshot]) -> Result<(Workspace, Value), String> {
        let (cleanup, request) = self.workspace_request(documents, true)?;
        let response = run_checker(&cleanup.0, &request)?;
        if response["ok"] != true || response["stage"] != "prepared" {
            return Err(format!("Project preparation failed: {response}"));
        }
        Ok((cleanup, response))
    }
    fn workspace_request(
        &self,
        documents: &[Snapshot],
        prepare_only: bool,
    ) -> Result<(Workspace, Vec<u8>), String> {
        self.validate_snapshot(documents)?;
        self.session_configuration(&json!({}))?;
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "ts-rust-lsp-{}-{stamp}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let cleanup = Workspace::create(directory)?;
        let workspace = cleanup.0.join("project");
        let mut bytes = 0;
        let mut source = |path: &str| -> Result<Snapshot, String> {
            let document = self.source(path, documents)?;
            bytes += document.text.len();
            if document.text.len() > ts_svelte_compiler::MAX_SOURCE_BYTES || bytes > INPUT_LIMIT {
                return Err("Project source limit exceeded".into());
            }
            Ok(document)
        };
        let components = self
            .components
            .iter()
            .map(|c| {
                Ok(json!({"path":c.path,"typescript":c.typescript,"check":c.check,"document":source(&c.path)?}))
            })
            .collect::<Result<Vec<_>, String>>()?;
        let files = self
            .files
            .iter()
            .map(|path| (path, true))
            .chain(self.auxiliary_files.iter().map(|path| (path, false)))
            .map(|(path, check)| Ok(json!({"path":path,"text":source(path)?.text,"check":check})))
            .collect::<Result<Vec<_>, String>>()?;
        let request = serde_json::to_vec(&json!({"workspace":workspace,"dependencies":self.dependencies,"components":components,"files":files,"runes":self.runes,"allowPrerelease":self.allow_prerelease,"compilerOptions":self.compiler_options,"runeModules":self.rune_modules,"prepareOnly":prepare_only})).map_err(|e| e.to_string())?;
        if request.len() > INPUT_LIMIT {
            return Err("Serialized project request exceeds 64 MiB".into());
        }
        Ok((cleanup, request))
    }
    /// Check disk sources with open-buffer overlays and map diagnostics back.
    /// # Errors
    /// Rejects oversized inputs, incomplete checks, invalid output or a 30-second timeout.
    #[allow(clippy::too_many_lines)]
    pub fn check(&self, documents: &[Snapshot]) -> Result<BTreeMap<String, Vec<Value>>, String> {
        let (cleanup, request) = self.workspace_request(documents, false)?;
        let workspace = cleanup.0.join("project");
        let response = run_checker(&cleanup.0, &request)?;
        if response["ok"] != true {
            return Err(format!(
                "Project check incomplete: {}",
                response
                    .get("error")
                    .or_else(|| response.get("failure"))
                    .unwrap_or(&response)
            ));
        }
        let checker_diagnostics = response["checker"]["diagnostics"]
            .as_array()
            .ok_or("Checker response missing diagnostics")?;
        // The checker preserves TypeScript exit statuses: 0 is clean; 1/2 are
        // completed checks with diagnostics (see ts_svelte_check::check).
        if response["checker"]["ok"] != true
            || !matches!(response["checker"]["status"].as_i64(), Some(0..=2))
        {
            return Err("Checker response missing valid completion status".into());
        }
        let mut diagnostics: BTreeMap<String, Vec<Value>> = BTreeMap::new();
        for diagnostic in response["compilerDiagnostics"]
            .as_array()
            .into_iter()
            .flatten()
        {
            let Some(path) = diagnostic["uri"].as_str() else {
                return Err("Compiler diagnostic missing URI".into());
            };
            // Component diagnostics are already present in the session's own
            // compiler result. Rune modules are checked by the project host.
            if self
                .files
                .iter()
                .chain(self.auxiliary_files.iter())
                .any(|p| p == path)
            {
                let document = self.source(path, documents)?;
                let mut diagnostic = diagnostic.clone();
                diagnostic["uri"] = json!(document.uri);
                diagnostics
                    .entry(document.uri)
                    .or_default()
                    .push(diagnostic);
            }
        }
        for d in checker_diagnostics {
            let original = &d["original"];
            let uri = original["uri"].as_str().map(str::to_owned).or_else(|| {
                d["fileName"]
                    .as_str()
                    .and_then(|p| Path::new(p).strip_prefix(&workspace).ok())
                    .and_then(|p| self.source(p.to_str()?, documents).ok().map(|d| d.uri))
            });
            let Some(uri) = uri else {
                return Err(format!("Unattached project diagnostic: {}", d["text"]));
            };
            let range = if original["status"] == "mapped" {
                original["range"].clone()
            } else if original.is_null() {
                json!({"start":d["startPosition"],"end":d["endPosition"]})
            } else {
                Value::Null
            };
            diagnostics.entry(uri.clone()).or_default().push(json!({"uri":uri,"range":range,"code":d["code"],"severity":if d["category"]==1{"error"}else{"warning"},"message":d["text"],"checker":d}));
        }
        Ok(diagnostics)
    }
}

fn run_checker(directory: &Path, request: &[u8]) -> Result<Value, String> {
    let input_path = directory.join("input.json");
    std::fs::File::create(&input_path)
        .and_then(|mut f| f.write_all(request))
        .map_err(|e| e.to_string())?;
    let output_path = directory.join("output.json");
    let output = std::fs::File::create(&output_path).map_err(|e| e.to_string())?;
    let sibling = std::env::current_exe()
        .map_err(|e| e.to_string())?
        .with_file_name(format!("svelte-project{}", std::env::consts::EXE_SUFFIX));
    let mut command = Command::new(sibling);
    command
        .stdin(std::fs::File::open(input_path).map_err(|e| e.to_string())?)
        .stdout(output)
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    #[cfg(unix)]
    let mut registration = ACTIVE_CHECK
        .lock()
        .map_err(|_| "Checker registration lock poisoned")?;
    let mut child = command.spawn().map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        registration
            .as_mut()
            .ok_or("Missing workspace registration")?
            .pid = Some(child.id());
        drop(registration);
    }
    let start = Instant::now();
    let outcome = loop {
        #[cfg(unix)]
        let mut registration = ACTIVE_CHECK
            .lock()
            .map_err(|_| "Checker registration lock poisoned")?;
        match child.try_wait() {
            Ok(Some(status)) => {
                #[cfg(unix)]
                {
                    registration
                        .as_mut()
                        .ok_or("Missing workspace registration")?
                        .pid = None;
                }
                break Ok(status);
            }
            Err(error) => break Err(error.to_string()),
            Ok(None) => {}
        }
        #[cfg(unix)]
        drop(registration);
        if start.elapsed() > Duration::from_secs(30) {
            break Err("Project check exceeded 30 seconds".into());
        }
        match std::fs::metadata(&output_path) {
            Ok(meta) if meta.len() <= OUTPUT_LIMIT as u64 => {}
            _ => break Err("Checker output exceeds 64 MiB or is unavailable".into()),
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    if outcome.is_err() {
        #[cfg(unix)]
        {
            let mut registration = ACTIVE_CHECK
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let _ = Command::new("kill")
                .args(["-KILL", "--", &format!("-{}", child.id())])
                .status();
            let _ = child.kill();
            let _ = child.wait();
            if let Some(check) = registration.as_mut() {
                check.pid = None;
            }
        }
        #[cfg(not(unix))]
        {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
    let status = outcome?;
    if !matches!(status.code(), Some(0 | 1)) {
        return Err(format!("Invalid checker exit status: {status}"));
    }
    let response: Value = serde_json::from_str(&bounded_read(&output_path, OUTPUT_LIMIT)?)
        .map_err(|e| format!("Invalid checker response: {e}"))?;
    Ok(response)
}

#[cfg(test)]
mod snapshot_tests {
    use super::*;
    #[cfg(unix)]
    #[test]
    fn dependency_guards_follow_package_links_but_reject_file_escape() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "svelte-linked-guard-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(root.join("node_modules/@scope")).unwrap();
        std::fs::create_dir(root.join("package")).unwrap();
        std::fs::write(root.join("package/package.json"), "{}").unwrap();
        std::fs::write(root.join("outside.json"), "{}").unwrap();
        std::os::unix::fs::symlink(root.join("package"), root.join("node_modules/@scope/pkg"))
            .unwrap();
        std::os::unix::fs::symlink(root.join("outside.json"), root.join("package/escape.json"))
            .unwrap();
        let dependencies = root.join("node_modules").canonicalize().unwrap();
        assert_eq!(
            dependency_guard_path(&dependencies, "@scope/pkg/package.json").unwrap(),
            root.join("package/package.json").canonicalize().unwrap()
        );
        assert!(dependency_guard_path(&dependencies, "@scope/pkg/escape.json").is_err());
        for path in [
            "../outside.json",
            "@scope/pkg/../../outside.json",
            "/outside.json",
            "",
        ] {
            assert!(
                dependency_guard_path(&dependencies, path).is_err(),
                "{path}"
            );
        }
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn guards_reject_disk_buffer_roster_and_dependency_changes() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("svelte-guard-{}-{unique}", std::process::id()));
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(root.join("deps")).unwrap();
        std::fs::write(root.join("route.ts"), "original").unwrap();
        std::fs::write(root.join("deps/package.json"), "package").unwrap();
        let config: Config = serde_json::from_value(
            json!({"root":root,"dependencies":root.join("deps"),"components":[],"snapshotGuard":{
                "files":{"route.ts":format!("{:x}",Sha256::digest(b"original"))},
                "directories":{".":["deps","route.ts"]},
                "dependencies":{"package.json":format!("{:x}",Sha256::digest(b"package"))}
            }}),
        )
        .unwrap();
        assert!(config.validate_snapshot(&[]).is_ok());
        let buffer = Snapshot {
            uri: url::Url::from_file_path(root.join("route.ts"))
                .unwrap()
                .to_string(),
            version: 1,
            text: "changed".into(),
        };
        assert!(
            config
                .validate_snapshot(&[buffer])
                .unwrap_err()
                .contains("stale")
        );
        std::fs::write(root.join("route.ts"), "changed").unwrap();
        assert!(config.validate_snapshot(&[]).unwrap_err().contains("stale"));
        std::fs::write(root.join("route.ts"), "original").unwrap();
        std::fs::write(root.join("new.ts"), "").unwrap();
        assert!(
            config
                .validate_snapshot(&[])
                .unwrap_err()
                .contains("roster")
        );
        std::fs::remove_file(root.join("new.ts")).unwrap();
        std::fs::write(root.join("deps/package.json"), "changed").unwrap();
        assert!(config.validate_snapshot(&[]).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
}
