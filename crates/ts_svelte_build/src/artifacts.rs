//! Output materialization is deliberately separate from graph generation.
use crate::Artifact;
use base64::Engine as _;
use std::{
    collections::BTreeMap,
    io::Write,
    path::{Component, Path},
};

pub fn relative(path: &str) -> Result<(), String> {
    if path.is_empty()
        || path.contains(['\\', '\0', '?', '#'])
        || Path::new(path)
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
        || path
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err(format!("Invalid relative artifact path {path:?}"));
    }
    Ok(())
}
pub fn insert(
    artifacts: &mut BTreeMap<String, Artifact>,
    path: String,
    bytes: &[u8],
    kind: &str,
) -> Result<(), String> {
    relative(&path)?;
    let artifact = Artifact {
        encoding: "base64".into(),
        source: base64::engine::general_purpose::STANDARD.encode(bytes),
        kind: kind.into(),
    };
    if let Some(previous) = artifacts.get(&path) {
        if previous.source != artifact.source {
            return Err(format!("Conflicting generated artifact {path}"));
        }
        return Ok(());
    }
    artifacts.insert(path, artifact);
    Ok(())
}
pub fn bytes(artifact: &Artifact) -> Result<Vec<u8>, String> {
    if artifact.encoding != "base64" {
        return Err("Artifact encoding must be base64".into());
    }
    base64::engine::general_purpose::STANDARD
        .decode(&artifact.source)
        .map_err(|e| e.to_string())
}
/// Write only into a newly created output directory. Existing destinations are
/// rejected, including symlinks. Partial I/O failure leaves the new directory
/// for inspection; no existing project or historical evidence is deleted.
pub fn write_new(directory: &Path, artifacts: &BTreeMap<String, Artifact>) -> Result<(), String> {
    if !directory.is_absolute()
        || directory
            .components()
            .any(|part| matches!(part, Component::ParentDir | Component::CurDir))
    {
        return Err("Output directory must be absolute and normalized".into());
    }
    // A path cannot be both a file and an ancestor directory of another file.
    // Reject this before creating the destination, not halfway through writes.
    for name in artifacts.keys() {
        let mut parent = Path::new(name).parent();
        while let Some(directory) = parent {
            if let Some(directory) = directory.to_str() {
                if artifacts.contains_key(directory) {
                    return Err(format!("Artifact file/directory collision at {directory}"));
                }
            }
            parent = directory.parent();
        }
    }
    let mut decoded = Vec::with_capacity(artifacts.len());
    let mut size = 0usize;
    for (path, artifact) in artifacts {
        relative(path)?;
        let bytes = bytes(artifact)?;
        size = size
            .checked_add(bytes.len())
            .ok_or("Artifact size overflow")?;
        if size > 512 * 1024 * 1024 {
            return Err("Artifacts exceed 512 MiB materialization limit".into());
        }
        decoded.push((path, bytes));
    }
    std::fs::create_dir(directory)
        .map_err(|e| format!("Output directory must be new with an existing parent: {e}"))?;
    for (path, bytes) in decoded {
        let destination = directory.join(path);
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&destination)
            .map_err(|e| format!("Cannot create {}: {e}", destination.display()))?;
        file.write_all(&bytes).map_err(|e| e.to_string())?;
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn output_paths_and_collisions_are_strict() {
        let mut files = BTreeMap::new();
        insert(&mut files, "client/a.js".into(), b"first", "js").unwrap();
        assert!(insert(&mut files, "client/a.js".into(), b"second", "js").is_err());
        for path in ["../a", "/a", "a/../b", "a//b", "a\\b", "a?x"] {
            assert!(relative(path).is_err());
        }
    }
    #[test]
    fn file_directory_collision_is_rejected_before_creating_output() {
        let mut files = BTreeMap::new();
        insert(&mut files, "server".into(), b"file", "asset").unwrap();
        insert(
            &mut files,
            "server/index.js".into(),
            b"module",
            "javascript",
        )
        .unwrap();
        let destination = std::env::temp_dir().join(format!(
            "ts-svelte-output-never-created-{}",
            std::process::id()
        ));
        assert!(
            write_new(&destination, &files)
                .unwrap_err()
                .contains("collision")
        );
        assert!(!destination.exists());
    }
}
