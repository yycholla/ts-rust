//! Tailwind 4.2.1 source selection over a host-supplied inventory.
//! Derived from Oxide scanner/sources.rs and scanner/mod.rs (MIT, Tailwind Labs).
//! Uses the same pinned fast-glob and bexpand matchers; no filesystem operations.
//! Hosts resolve symlinks/case and supply gitignore/binary/reachability facts.
use crate::{
    snapshot::SourceSnapshot,
    stylesheet::{SourceRoot, StylesheetMetadata},
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SourcePath {
    pub id: String,
    /// Canonical absolute path using forward slashes on every target. Drive
    /// letters are uppercase; symlink resolution belongs to the host.
    pub path: String,
    /// Host classification against the pinned scanner's binary extension list.
    /// An explicit file/pattern may select binary-classified input deliberately.
    pub binary: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum SourceOrigin {
    Root,
    Directive { index: usize },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UnreachableSources {
    pub origin: SourceOrigin,
    /// Files blocked by ancestor ignore rules under this source's traversal
    /// root. File-level ignore rules do not belong here: explicit patterns may
    /// override those. Omitted origins assert no additional blocked ancestors.
    pub ids: Vec<String>,
}
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct SourceInventory {
    pub files: Vec<SourcePath>,
    /// Additional known directories, including empty roots. File ancestors are
    /// inferred structurally; a missing literal path is never guessed to be a folder.
    pub directories: Vec<String>,
    /// Inputs allowed by normal automatic discovery (gitignore, ignored
    /// directories/extensions/files, and binaries have already been considered).
    pub automatic_eligible_ids: Vec<String>,
    pub unreachable: Vec<UnreachableSources>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SelectionMode {
    Automatic,
    External,
    File,
    Pattern,
    Excluded,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectionReason {
    pub origin: SourceOrigin,
    pub mode: SelectionMode,
    pub base: String,
    /// Normalized pattern relative to base; **/* for directory discovery.
    pub pattern: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectedSource {
    pub id: String,
    pub path: String,
    pub reasons: Vec<SelectionReason>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExcludedSource {
    pub id: String,
    pub path: String,
    pub reasons: Vec<SelectionReason>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceSelection {
    /// Ordered by canonical path, independent of input map/inventory order.
    pub selected: Vec<SelectedSource>,
    pub excluded: Vec<ExcludedSource>,
    pub unmatched_ids: Vec<String>,
    /// Includes patterns with zero current matches, suitable for host watching.
    pub sources: Vec<SelectionReason>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SelectionErrorKind {
    Inventory,
    Path,
    MissingPath,
    UnsupportedPattern,
    Limit,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectionError {
    pub kind: SelectionErrorKind,
    pub message: String,
    pub origin: Option<SourceOrigin>,
}
impl std::fmt::Display for SelectionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for SelectionError {}
fn error(kind: SelectionErrorKind, message: impl Into<String>) -> SelectionError {
    SelectionError {
        kind,
        message: message.into(),
        origin: None,
    }
}

const IGNORED_DIRS: &[&str] = &[
    ".git",
    ".hg",
    ".jj",
    ".next",
    ".parcel-cache",
    ".pnpm-store",
    ".svelte-kit",
    ".svn",
    ".turbo",
    ".venv",
    ".vercel",
    ".yarn",
    "__pycache__",
    "node_modules",
    "venv",
];
const MAX_FILES: usize = 100_000;
const MAX_PATTERNS: usize = 16_384;
const MAX_COMPARISONS: usize = 10_000_000;

pub fn select_sources(
    metadata: &StylesheetMetadata,
    snapshots: &[SourceSnapshot],
    inventory: &SourceInventory,
) -> Result<SourceSelection, SelectionError> {
    if snapshots.len() > MAX_FILES
        || inventory.files.len() > MAX_FILES
        || inventory.directories.len() > MAX_FILES
        || metadata.sources.len() > 4096
    {
        return Err(error(
            SelectionErrorKind::Limit,
            "Source inventory exceeds item limits",
        ));
    }
    let mut snapshot_ids = BTreeSet::new();
    for snapshot in snapshots {
        if snapshot.id.is_empty()
            || snapshot.id.len() > 16_384
            || snapshot.id.contains('\0')
            || !snapshot_ids.insert(snapshot.id.as_str())
        {
            return Err(error(
                SelectionErrorKind::Inventory,
                "Source snapshot IDs are empty, invalid, or duplicated",
            ));
        }
    }
    let mut files = BTreeMap::new();
    let mut ids = BTreeMap::new();
    let mut directories = BTreeSet::new();
    for path in &inventory.directories {
        canonical(path)?;
        if !directories.insert(path.clone()) {
            return Err(error(
                SelectionErrorKind::Inventory,
                format!("Duplicate directory path {path}"),
            ));
        }
    }
    for file in &inventory.files {
        canonical(&file.path)?;
        if !snapshot_ids.contains(file.id.as_str()) {
            return Err(error(
                SelectionErrorKind::Inventory,
                format!("Path mapping refers to missing snapshot {}", file.id),
            ));
        }
        if ids.insert(file.id.as_str(), file).is_some() {
            return Err(error(
                SelectionErrorKind::Inventory,
                format!("Duplicate path mapping for {}", file.id),
            ));
        }
        if files.insert(file.path.as_str(), file).is_some() {
            return Err(error(
                SelectionErrorKind::Inventory,
                format!("Ambiguous duplicate file path {}", file.path),
            ));
        }
        if parent(&file.path).is_none() {
            return Err(error(
                SelectionErrorKind::Inventory,
                "A filesystem root cannot be a source file",
            ));
        }
        let mut path = parent(&file.path);
        while let Some(ancestor) = path {
            directories.insert(ancestor.into());
            path = parent(ancestor);
        }
    }
    if ids.len() != snapshot_ids.len() {
        return Err(error(
            SelectionErrorKind::Inventory,
            "Every source snapshot requires exactly one path mapping",
        ));
    }
    for path in files.keys() {
        if directories.contains(*path) {
            return Err(error(
                SelectionErrorKind::Inventory,
                format!("Path {path} is both a file and a directory"),
            ));
        }
    }
    let mut automatic = BTreeSet::new();
    for id in &inventory.automatic_eligible_ids {
        let file = ids.get(id.as_str()).ok_or_else(|| {
            error(
                SelectionErrorKind::Inventory,
                format!("Automatic eligibility names absent source {id}"),
            )
        })?;
        if !automatic.insert(id.as_str()) || file.binary {
            return Err(error(
                SelectionErrorKind::Inventory,
                format!("Invalid automatic eligibility for {id}"),
            ));
        }
    }
    let mut blocked = BTreeMap::new();
    for entry in &inventory.unreachable {
        if matches!(&entry.origin, SourceOrigin::Directive { index } if *index >= metadata.sources.len())
        {
            return Err(error(
                SelectionErrorKind::Inventory,
                "Reachability names an absent source directive",
            ));
        }
        let mut entries = BTreeSet::new();
        for id in &entry.ids {
            if !ids.contains_key(id.as_str()) || !entries.insert(id.as_str()) {
                return Err(error(
                    SelectionErrorKind::Inventory,
                    "Reachability IDs are absent or duplicated",
                ));
            }
        }
        if blocked.insert(entry.origin.clone(), entries).is_some() {
            return Err(error(
                SelectionErrorKind::Inventory,
                "Duplicate source reachability origin",
            ));
        }
    }
    let mut sources = Vec::new();
    match &metadata.root {
        SourceRoot::Auto => sources.extend(normalize_source(
            &metadata.entry_base,
            "**/*",
            false,
            SourceOrigin::Root,
            &files,
            &directories,
        )?),
        SourceRoot::None => {}
        SourceRoot::Pattern { base, pattern } => sources.extend(normalize_source(
            base,
            pattern,
            false,
            SourceOrigin::Root,
            &files,
            &directories,
        )?),
    }
    for (index, source) in metadata.sources.iter().enumerate() {
        sources.extend(normalize_source(
            &source.base,
            &source.pattern,
            source.negated,
            SourceOrigin::Directive { index },
            &files,
            &directories,
        )?);
        if sources.len() > MAX_PATTERNS {
            return Err(error(
                SelectionErrorKind::Limit,
                "Expanded source pattern limit exceeded",
            ));
        }
    }
    if sources.len().saturating_mul(files.len()) > MAX_COMPARISONS {
        return Err(error(
            SelectionErrorKind::Limit,
            "Source selection exceeds bounded comparison budget",
        ));
    }
    let mut result = SourceSelection {
        selected: Vec::new(),
        excluded: Vec::new(),
        unmatched_ids: Vec::new(),
        sources,
    };
    let mut comparisons = 0;
    for file in files.values() {
        let mut reasons = Vec::new();
        let mut exclusions = Vec::new();
        for source in &result.sources {
            let Some(relative) = relative_to(&file.path, &source.base) else {
                continue;
            };
            let matches = if source.mode == SelectionMode::Excluded {
                let mut path = Some(relative);
                let mut found = false;
                while let Some(value) = path {
                    comparisons += 1;
                    if comparisons > MAX_COMPARISONS {
                        return Err(error(
                            SelectionErrorKind::Limit,
                            "Source exclusion ancestry exceeds comparison budget",
                        ));
                    }
                    if fast_glob::glob_match(&source.pattern, format!("/{value}")) {
                        found = true;
                        break;
                    }
                    path = value.rsplit_once('/').map(|(parent, _)| parent);
                }
                found
            } else {
                comparisons += 1;
                if comparisons > MAX_COMPARISONS {
                    return Err(error(
                        SelectionErrorKind::Limit,
                        "Source matching exceeds comparison budget",
                    ));
                }
                fast_glob::glob_match(&source.pattern, format!("/{relative}"))
            };
            if !matches {
                continue;
            }
            if source.mode == SelectionMode::Excluded {
                exclusions.push(source.clone());
                continue;
            }
            if blocked
                .get(&source.origin)
                .is_some_and(|ids| ids.contains(file.id.as_str()))
            {
                continue;
            }
            let eligible = match source.mode {
                SelectionMode::Automatic => automatic.contains(file.id.as_str()),
                SelectionMode::External => !file.binary,
                SelectionMode::File => true,
                SelectionMode::Pattern => {
                    !source.pattern.contains("**")
                        || extension(&source.pattern).is_some()
                        || automatic.contains(file.id.as_str())
                }
                SelectionMode::Excluded => false,
            };
            if eligible {
                reasons.push(source.clone());
            }
        }
        if !exclusions.is_empty() {
            result.excluded.push(ExcludedSource {
                id: file.id.clone(),
                path: file.path.clone(),
                reasons: exclusions,
            });
        } else if reasons.is_empty() {
            result.unmatched_ids.push(file.id.clone());
        } else {
            result.selected.push(SelectedSource {
                id: file.id.clone(),
                path: file.path.clone(),
                reasons,
            });
        }
    }
    Ok(result)
}

fn normalize_source(
    base: &str,
    pattern: &str,
    negated: bool,
    origin: SourceOrigin,
    files: &BTreeMap<&str, &SourcePath>,
    directories: &BTreeSet<String>,
) -> Result<Vec<SelectionReason>, SelectionError> {
    let work = || {
        canonical(base)?;
        if !directories.contains(base) {
            return Err(error(
                SelectionErrorKind::MissingPath,
                format!("Source base directory {base} is absent from the inventory"),
            ));
        }
        if pattern.is_empty() || pattern.len() > 16_384 || pattern.contains('\0') {
            return Err(error(
                SelectionErrorKind::UnsupportedPattern,
                "Source pattern is empty or exceeds bounds",
            ));
        }
        validate_glob(pattern, true)?;
        let patterns = expand_braces(pattern)?;
        let mut result = Vec::new();
        for pattern in patterns {
            validate_glob(&pattern, false)?;
            let (absolute, literal_anchor) = resolve_pattern_parts(base, &pattern)?;
            if let Some(file) = files.get(absolute.as_str()) {
                let parent = parent(&file.path).unwrap();
                let filename = relative_to(&file.path, parent).unwrap();
                result.push(SelectionReason {
                    origin: origin.clone(),
                    mode: if negated {
                        SelectionMode::Excluded
                    } else {
                        SelectionMode::File
                    },
                    base: parent.into(),
                    pattern: format!("/{}", escape_glob(filename)),
                });
                continue;
            }
            if directories.contains(&absolute) {
                result.push(directory_reason(absolute, negated, origin.clone()));
                continue;
            }
            // Pinned PublicSourceEntry::optimize hoists only the static prefix
            // before the first * or !. SourceEntry then classifies every suffix
            // **/* as Auto, even when dynamic directory segments remain. Keep
            // that broader upstream base visible in normalized reasons.
            if pattern.ends_with("**/*") && !negated {
                let mut slash = None;
                for (index, c) in pattern.char_indices() {
                    if c == '/' {
                        slash = Some(index);
                    }
                    if matches!(c, '*' | '!') {
                        break;
                    }
                }
                let hoisted = slash
                    .filter(|i| *i > 0)
                    .and_then(|i| resolve_pattern(base, &pattern[..i]).ok())
                    .filter(|path| directories.contains(path))
                    .unwrap_or_else(|| base.into());
                result.push(directory_reason(hoisted, false, origin.clone()));
                continue;
            }
            let (static_base, dynamic) = split_dynamic(&absolute, &literal_anchor);
            let Some(dynamic) = dynamic else {
                return Err(error(
                    SelectionErrorKind::MissingPath,
                    format!("Literal source path {absolute} is absent from the inventory"),
                ));
            };
            if !directories.contains(&static_base) {
                return Err(error(
                    SelectionErrorKind::MissingPath,
                    format!("Static glob directory {static_base} is absent from the inventory"),
                ));
            }
            result.push(SelectionReason {
                origin: origin.clone(),
                mode: if negated {
                    SelectionMode::Excluded
                } else {
                    SelectionMode::Pattern
                },
                base: static_base,
                pattern: format!("/{dynamic}"),
            });
        }
        Ok(result)
    };
    work().map_err(|mut e| {
        e.origin = Some(origin);
        e
    })
}
fn directory_reason(base: String, negated: bool, origin: SourceOrigin) -> SelectionReason {
    let external = base.split('/').any(|part| IGNORED_DIRS.contains(&part));
    SelectionReason {
        origin,
        mode: if negated {
            SelectionMode::Excluded
        } else if external {
            SelectionMode::External
        } else {
            SelectionMode::Automatic
        },
        base,
        pattern: "/**/*".into(),
    }
}
fn extension(pattern: &str) -> Option<&str> {
    let filename = pattern.rsplit('/').next()?;
    let (stem, extension) = filename.rsplit_once('.')?;
    (!stem.is_empty()).then_some(extension)
}
fn root_len(path: &str) -> Option<usize> {
    if path.starts_with("//") {
        let server_end = path[2..].find('/').map(|i| i + 2)?;
        if server_end == 2 {
            return None;
        }
        let share_end = path[server_end + 1..]
            .find('/')
            .map_or(path.len(), |i| server_end + 1 + i);
        return (share_end > server_end + 1
            && !matches!(&path[2..server_end], "." | "..")
            && !matches!(&path[server_end + 1..share_end], "." | ".."))
        .then_some(share_end);
    }
    if path.starts_with('/') {
        return Some(1);
    }
    let b = path.as_bytes();
    (b.len() >= 3 && b[0].is_ascii_uppercase() && b[1] == b':' && b[2] == b'/').then_some(3)
}
fn canonical(path: &str) -> Result<(), SelectionError> {
    let root = root_len(path).ok_or_else(|| {
        error(
            SelectionErrorKind::Path,
            format!("Expected an absolute normalized forward-slash path: {path}"),
        )
    })?;
    if path.len() > 16_384
        || path.contains(['\0', '\\'])
        || (path.len() > root && path.ends_with('/'))
    {
        return Err(error(
            SelectionErrorKind::Path,
            format!("Noncanonical source path {path}"),
        ));
    }
    let rest = path[root..].strip_prefix('/').unwrap_or(&path[root..]);
    if !rest.is_empty() && rest.split('/').any(|part| matches!(part, "" | "." | "..")) {
        return Err(error(
            SelectionErrorKind::Path,
            format!("Noncanonical source path {path}"),
        ));
    }
    Ok(())
}
fn parent(path: &str) -> Option<&str> {
    let root = root_len(path)?;
    if path.len() <= root {
        return None;
    }
    let slash = path.rfind('/')?;
    Some(&path[..slash.max(root)])
}
fn relative_to<'a>(path: &'a str, base: &str) -> Option<&'a str> {
    if path == base {
        return None;
    }
    if base.ends_with('/') {
        path.strip_prefix(base)
    } else {
        path.strip_prefix(base)?.strip_prefix('/')
    }
}
fn resolve_pattern(base: &str, pattern: &str) -> Result<String, SelectionError> {
    resolve_pattern_parts(base, pattern).map(|(absolute, _)| absolute)
}
fn resolve_pattern_parts(base: &str, pattern: &str) -> Result<(String, String), SelectionError> {
    // The host's canonical base is a literal path, even if its directory names
    // contain glob metacharacters. Only the supplied pattern introduces globs.
    let (prefix, mut parts, tail) = if let Some(root) = root_len(pattern) {
        (&pattern[..root], Vec::new(), &pattern[root..])
    } else {
        if pattern.as_bytes().get(1) == Some(&b':') || pattern.starts_with("//") {
            return Err(error(
                SelectionErrorKind::Path,
                "Ambiguous drive-relative or UNC source pattern",
            ));
        }
        let root = root_len(base).ok_or_else(|| {
            error(
                SelectionErrorKind::Path,
                "Source base has no resolvable root",
            )
        })?;
        (
            &base[..root],
            base[root..]
                .split('/')
                .filter(|part| !part.is_empty())
                .collect(),
            pattern,
        )
    };
    let mut literal_parts = parts.len();
    let mut dynamic = false;
    for part in tail.trim_start_matches('/').split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if dynamic {
                    return Err(error(
                        SelectionErrorKind::UnsupportedPattern,
                        "Parent traversal after a dynamic glob segment cannot be resolved without filesystem traversal",
                    ));
                }
                if parts.pop().is_none() {
                    return Err(error(
                        SelectionErrorKind::Path,
                        "Source pattern traverses above its filesystem root",
                    ));
                }
                literal_parts = literal_parts.min(parts.len());
            }
            _ => {
                dynamic |= has_dynamic(part);
                parts.push(part);
            }
        }
    }
    let join = |parts: &[&str]| {
        if parts.is_empty() {
            prefix.to_owned()
        } else {
            format!(
                "{}{}{}",
                prefix,
                if prefix.ends_with('/') { "" } else { "/" },
                parts.join("/")
            )
        }
    };
    Ok((join(&parts), join(&parts[..literal_parts])))
}
fn split_dynamic(pattern: &str, base: &str) -> (String, Option<String>) {
    let root = root_len(pattern).unwrap();
    let mut start = if relative_to(pattern, base).is_some() {
        base.len()
    } else {
        root
    };
    if pattern.as_bytes().get(start) == Some(&b'/') {
        start += 1;
    }
    for segment in pattern[start..].split('/') {
        if has_dynamic(segment) {
            let end = if start == root || start == root + 1 {
                root
            } else {
                start - 1
            };
            return (pattern[..end].into(), Some(pattern[start..].into()));
        }
        start += segment.len() + 1;
    }
    (pattern.into(), None)
}
fn has_dynamic(value: &str) -> bool {
    value.contains(['*', '?', '[', '\\'])
}
fn escape_glob(value: &str) -> String {
    value
        .chars()
        .flat_map(|c| {
            if matches!(c, '*' | '?' | '[' | ']' | '{' | '}' | '\\' | '!') {
                vec!['\\', c]
            } else {
                vec![c]
            }
        })
        .collect()
}
fn validate_glob(pattern: &str, allow_braces: bool) -> Result<(), SelectionError> {
    let mut escaped = false;
    let mut bracket = false;
    let mut braces = 0usize;
    let mut previous = None;
    for c in pattern.chars() {
        if escaped {
            if c == '/' {
                return Err(error(
                    SelectionErrorKind::UnsupportedPattern,
                    "Escaped path separators are not supported",
                ));
            }
            escaped = false;
            previous = None;
            continue;
        }
        if c == '\\' {
            escaped = true;
            continue;
        }
        if c == '[' {
            if bracket {
                return Err(error(
                    SelectionErrorKind::UnsupportedPattern,
                    "Nested/POSIX character classes are unsupported",
                ));
            }
            bracket = true;
        } else if c == ']' {
            if !bracket {
                return Err(error(
                    SelectionErrorKind::UnsupportedPattern,
                    "Unmatched glob character class",
                ));
            }
            bracket = false;
        } else if !bracket {
            if c == '{' {
                if !allow_braces {
                    return Err(error(
                        SelectionErrorKind::UnsupportedPattern,
                        "Unexpanded brace pattern",
                    ));
                }
                braces += 1;
                if braces > 32 {
                    return Err(error(
                        SelectionErrorKind::Limit,
                        "Source brace nesting exceeds 32",
                    ));
                }
            }
            if c == '}' {
                if braces == 0 {
                    return Err(error(
                        SelectionErrorKind::UnsupportedPattern,
                        "Unmatched source brace",
                    ));
                }
                braces -= 1;
            }
            if c == '(' && previous.is_some_and(|c| matches!(c, '@' | '+' | '?' | '*' | '!')) {
                return Err(error(
                    SelectionErrorKind::UnsupportedPattern,
                    "Extglob syntax is not supported by the pinned Oxide matcher",
                ));
            }
        }
        previous = Some(c);
    }
    if escaped || bracket || braces > 0 {
        return Err(error(
            SelectionErrorKind::UnsupportedPattern,
            "Unbalanced source glob or trailing escape",
        ));
    }
    for part in pattern.split('/') {
        if part.contains("**") && part != "**" {
            return Err(error(
                SelectionErrorKind::UnsupportedPattern,
                "Globstar must occupy an entire path segment",
            ));
        }
    }
    if pattern.starts_with('!') {
        return Err(error(
            SelectionErrorKind::UnsupportedPattern,
            "Use the source directive negated flag rather than leading ! in a path",
        ));
    }
    Ok(())
}
fn expand_braces(pattern: &str) -> Result<Vec<String>, SelectionError> {
    let expression = match bexpand::Expression::try_from(pattern) {
        Ok(expression) => expression,
        Err(_) if !pattern.contains(['{', '}']) => return Ok(vec![pattern.into()]),
        Err(message) => {
            return Err(error(
                SelectionErrorKind::UnsupportedPattern,
                format!("Invalid source brace expression: {message}"),
            ));
        }
    };
    let mut result = Vec::new();
    let mut bytes = 0;
    for value in expression {
        let value = value
            .map_err(|_| {
                error(
                    SelectionErrorKind::UnsupportedPattern,
                    "Source brace range produced an invalid Unicode scalar",
                )
            })?
            .into_owned();
        bytes += value.len();
        if result.len() >= MAX_PATTERNS || bytes > 16 * 1024 * 1024 {
            return Err(error(
                SelectionErrorKind::Limit,
                "Source brace expansion exceeds bounds",
            ));
        }
        result.push(value);
    }
    Ok(result)
}

#[cfg(test)]
#[path = "source_selection_tests.rs"]
mod tests;
