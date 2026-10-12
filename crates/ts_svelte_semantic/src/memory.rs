//! Synchronous, case-sensitive POSIX memory filesystem. No symlinks or host IO.
use serde_json::Value;
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::time::SystemTime;
use ts_goport::frontend::{
    tspath,
    vfs::{Entries, FileInfo, FileMode, Fs, FsError},
};

pub(crate) const MAX_FILES: usize = 100_000;
pub(crate) const MAX_TOTAL: usize = 256 * 1024 * 1024;
pub(crate) const MAX_FILE: usize = 8 * 1024 * 1024;

pub(crate) fn checked_path(path: &str) -> Result<String, String> {
    if !path.starts_with('/')
        || path.contains(['\0', '\\'])
        || path.split('/').any(|part| part == ".." || part == ".")
    {
        return Err("Expected an absolute POSIX path without dot segments or backslashes".into());
    }
    let normalized = tspath::normalize_path(path);
    if normalized != path || (path.len() > 1 && path.ends_with('/')) {
        return Err("Path must be canonical".into());
    }
    Ok(normalized)
}

#[derive(Default)]
pub(crate) struct MemoryFs {
    files: RefCell<BTreeMap<String, String>>,
}

impl MemoryFs {
    pub(crate) fn new(value: &Value) -> Result<Self, String> {
        let fs = Self::default();
        let changes = fs.prepare(value)?;
        if changes.iter().any(|(_, text)| text.is_none()) {
            return Err("Initial files must have string contents".into());
        }
        fs.apply(changes);
        Ok(fs)
    }

    pub(crate) fn prepare(&self, value: &Value) -> Result<Vec<(String, Option<String>)>, String> {
        let files = value
            .as_object()
            .ok_or("files must map absolute paths to strings or null")?;
        let current = self.files.borrow();
        let mut paths: BTreeSet<String> = current.keys().cloned().collect();
        let mut total: usize = current.values().map(String::len).sum();
        let mut changes = Vec::with_capacity(files.len());
        for (path, value) in files {
            let path = checked_path(path)?;
            if path == "/" {
                return Err("Root cannot be a file".into());
            }
            let text = if value.is_null() {
                None
            } else {
                let text = value
                    .as_str()
                    .ok_or("File contents must be a string or null")?;
                if text.len() > MAX_FILE {
                    return Err("File exceeds 8 MiB".into());
                }
                Some(text.to_owned())
            };
            total -= current.get(&path).map_or(0, String::len);
            total += text.as_ref().map_or(0, String::len);
            if text.is_some() {
                paths.insert(path.clone());
            } else {
                paths.remove(&path);
            }
            changes.push((path, text));
        }
        if total > MAX_TOTAL || paths.len() > MAX_FILES {
            return Err("Filesystem exceeds 256 MiB or 100000 files".into());
        }
        for path in &paths {
            let mut parent = path.as_str();
            while let Some((next, _)) = parent.rsplit_once('/') {
                if paths.contains(next) {
                    return Err(format!("File is also a directory: {next}"));
                }
                parent = next;
            }
        }
        Ok(changes)
    }

    pub(crate) fn apply(&self, changes: Vec<(String, Option<String>)>) {
        let mut files = self.files.borrow_mut();
        for (path, text) in changes {
            if let Some(text) = text {
                files.insert(path, text);
            } else {
                files.remove(&path);
            }
        }
    }
}

impl Fs for MemoryFs {
    fn use_case_sensitive_file_names(&self) -> bool {
        true
    }
    fn file_exists(&self, path: &str) -> bool {
        self.files
            .borrow()
            .contains_key(&tspath::normalize_path(path))
    }
    fn read_file(&self, path: &str) -> (String, bool) {
        self.files
            .borrow()
            .get(&tspath::normalize_path(path))
            .map(|text| {
                (
                    text.strip_prefix('\u{feff}').unwrap_or(text).to_owned(),
                    true,
                )
            })
            .unwrap_or_default()
    }
    fn write_file(&self, _: &str, _: &str) -> Result<(), FsError> {
        Err(FsError::Permission)
    }
    fn append_file(&self, _: &str, _: &str) -> Result<(), FsError> {
        Err(FsError::Permission)
    }
    fn remove(&self, _: &str) -> Result<(), FsError> {
        Err(FsError::Permission)
    }
    fn chtimes(
        &self,
        _: &str,
        _: Option<SystemTime>,
        _: Option<SystemTime>,
    ) -> Result<(), FsError> {
        Err(FsError::Permission)
    }
    fn directory_exists(&self, path: &str) -> bool {
        let path = tspath::normalize_path(path);
        let prefix = format!("{}/", path.trim_end_matches('/'));
        path == "/"
            || self
                .files
                .borrow()
                .range(prefix.clone()..)
                .next()
                .is_some_and(|(key, _)| key.starts_with(&prefix))
    }
    fn get_accessible_entries(&self, path: &str) -> Entries {
        let prefix = format!("{}/", tspath::normalize_path(path).trim_end_matches('/'));
        let mut directories = BTreeSet::new();
        let mut files = Vec::new();
        for (key, _) in self.files.borrow().range(prefix.clone()..) {
            let Some(rest) = key.strip_prefix(&prefix) else {
                break;
            };
            if let Some((dir, _)) = rest.split_once('/') {
                directories.insert(dir.to_owned());
            } else {
                files.push(rest.to_owned());
            }
        }
        Entries {
            files,
            directories: directories.into_iter().collect(),
            symlinks: Some(Default::default()),
        }
    }
    fn stat(&self, path: &str) -> Option<FileInfo> {
        let path = tspath::normalize_path(path);
        let directory = self.directory_exists(&path);
        let files = self.files.borrow();
        let text = files.get(&path);
        if !directory && text.is_none() {
            return None;
        }
        Some(FileInfo {
            name: path.rsplit('/').next().unwrap_or("").into(),
            size: text.map_or(0, |text| text.len() as i64),
            mode: if directory {
                FileMode::DIR
            } else {
                FileMode(0)
            },
            mod_time: None,
        })
    }
    fn realpath(&self, path: &str) -> String {
        tspath::normalize_path(path)
    }
}
