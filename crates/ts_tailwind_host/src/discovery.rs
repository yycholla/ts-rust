use super::*;
use ignore::{
    WalkBuilder,
    gitignore::{Gitignore, GitignoreBuilder},
};
use ts_tailwind::{
    snapshot::SourceSnapshot,
    source_selection::{SelectionMode, SelectionReason, SourcePath, select_sources},
    stylesheet::{SourceDirective, SourceRoot},
};

const DIRS: &str = include_str!("../reference/fixtures/ignored-content-dirs.txt");
const EXTENSIONS: &str = include_str!("../reference/fixtures/ignored-extensions.txt");
const FILES: &str = include_str!("../reference/fixtures/ignored-files.txt");
const BINARY: &str = include_str!("../reference/fixtures/binary-extensions.txt");
fn group(text: &str) -> String {
    format!("{{{}}}", text.trim().lines().collect::<Vec<_>>().join(","))
}
fn binary(path: &Path) -> bool {
    path.extension()
        .and_then(|s| s.to_str())
        .is_some_and(|ext| BINARY.lines().any(|line| line == ext))
}
fn defaults() -> Result<Vec<Gitignore>, HostError> {
    let mut general = GitignoreBuilder::new("");
    for pattern in [
        format!("{}/", group(DIRS)),
        format!("*.{}", group(EXTENSIONS)),
        group(FILES),
    ] {
        general
            .add_line(None, &pattern)
            .map_err(|e| fail("ignore", Path::new(""), e))?;
    }
    let mut files = GitignoreBuilder::new("");
    files
        .only_on_files(true)
        .add_line(None, &format!("*.{}", group(BINARY)))
        .map_err(|e| fail("ignore", Path::new(""), e))?;
    Ok(vec![
        general
            .build()
            .map_err(|e| fail("ignore", Path::new(""), e))?,
        files
            .build()
            .map_err(|e| fail("ignore", Path::new(""), e))?,
    ])
}
fn lexical(path: &Path) -> Result<PathBuf, HostError> {
    use std::path::Component;
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                if !out.pop() {
                    return Err(fail("path", path, "Source escapes its filesystem root"));
                }
            }
            Component::CurDir => {}
            _ => out.push(component.as_os_str()),
        }
    }
    Ok(out)
}
fn probe_path(
    state: &mut State<'_>,
    path: &Path,
    dirs: &mut BTreeSet<String>,
    files: &mut BTreeMap<String, SourcePath>,
) -> Result<(), HostError> {
    let id = path_string(path)?;
    match state.overlay.kind(path) {
        Ok((is_file, is_dir)) => {
            if is_dir {
                dirs.insert(id.clone());
            } else if is_file {
                files.insert(
                    id.clone(),
                    SourcePath {
                        id: id.clone(),
                        path: id,
                        binary: binary(path),
                    },
                );
            } else {
                return Err(fail(
                    "path",
                    path,
                    "Source is not a regular file or directory",
                ));
            }
            for parent in path.ancestors().skip(1) {
                dirs.insert(path_string(parent)?);
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            state.watch.missing.insert(id);
        }
        Err(e) => return Err(fail("io", path, e)),
    }
    Ok(())
}
fn probes(
    state: &mut State<'_>,
    metadata: &StylesheetMetadata,
) -> Result<(Vec<SourceSnapshot>, SourceInventory), HostError> {
    let mut dirs = BTreeSet::new();
    let mut files = BTreeMap::new();
    let mut sources = Vec::new();
    match &metadata.root {
        SourceRoot::Auto => sources.push((metadata.entry_base.as_str(), "**/*")),
        SourceRoot::Pattern { base, pattern } => sources.push((base.as_str(), pattern.as_str())),
        SourceRoot::None => {}
    }
    sources.extend(
        metadata
            .sources
            .iter()
            .map(|s| (s.base.as_str(), s.pattern.as_str())),
    );
    let mut count = 0;
    for (base, pattern) in sources {
        let base = Path::new(base);
        if pattern.len() > 16_384 || pattern.matches('{').count() > 32 {
            return Err(fail("limit", base, "Source pattern exceeds parsing bounds"));
        }
        probe_path(state, base, &mut dirs, &mut files)?;
        let expanded: Box<dyn Iterator<Item = Result<String, String>> + '_> =
            match bexpand::Expression::try_from(pattern) {
                Ok(expression) => Box::new(
                    expression
                        .into_iter()
                        .map(|item| item.map(|s| s.into_owned()).map_err(|e| e.to_string())),
                ),
                Err(_) if !pattern.contains(['{', '}']) => {
                    Box::new(std::iter::once(Ok(pattern.into())))
                }
                Err(e) => {
                    return Err(fail(
                        "source",
                        base,
                        format!("Invalid source brace pattern: {e:?}"),
                    ));
                }
            };
        for value in expanded {
            let value = value.map_err(|e| fail("source", base, e))?;
            count += 1;
            if count > 16_384 || value.len() > 16_384 {
                return Err(fail(
                    "limit",
                    base,
                    "Source pattern expansion exceeds bounds",
                ));
            }
            if environment_file(Path::new(&value)) {
                return Err(fail(
                    "protectedInput",
                    base.join(&value).as_path(),
                    "Explicit environment-file sources are not allowed",
                ));
            }
            let absolute = lexical(&base.join(&value))?;
            // Probe only the static prefix. No recursive unrestricted traversal
            // is needed to normalize the scanner's roots and literal inputs.
            let mut prefix = PathBuf::new();
            for component in absolute.components() {
                let text = component
                    .as_os_str()
                    .to_str()
                    .ok_or_else(|| fail("path", &absolute, "Non-UTF-8 source pattern"))?;
                if text.contains(['*', '?', '[', '\\'])
                    && !base.starts_with(prefix.join(component.as_os_str()))
                {
                    break;
                }
                prefix.push(component.as_os_str());
                probe_path(state, &prefix, &mut dirs, &mut files)?;
            }
            // A literal path with glob characters can exist and wins upstream.
            if !value.contains(['*', '?', '[', '\\']) {
                probe_path(state, &absolute, &mut dirs, &mut files)?;
            }
        }
    }
    let snapshots = files
        .keys()
        .map(|id| SourceSnapshot {
            id: id.clone(),
            version: 0,
            extension: String::new(),
            text: String::new(),
        })
        .collect();
    Ok((
        snapshots,
        SourceInventory {
            files: files.into_values().collect(),
            directories: dirs.into_iter().collect(),
            automatic_eligible_ids: vec![],
            unreachable: vec![],
        },
    ))
}

type Discovery = (
    StylesheetMetadata,
    SourceInventory,
    Vec<SourceSnapshot>,
    SourceSelection,
);
pub(super) fn discover(
    state: &mut State<'_>,
    root: &Path,
    metadata: &StylesheetMetadata,
) -> Result<Discovery, HostError> {
    let mut metadata = metadata.clone();
    if metadata.root == SourceRoot::Auto {
        metadata.root = SourceRoot::Pattern {
            base: path_string(root)?,
            pattern: "**/*".into(),
        };
    }
    let (probe_snapshots, probe_inventory) = probes(state, &metadata)?;
    let normalized = select_sources(&metadata, &probe_snapshots, &probe_inventory)
        .map_err(|e| fail("source", root, e))?;
    // Resolve each traversal base through the filesystem before walking, so
    // symlinked CSS packages/sources share canonical document identities.
    metadata.root = SourceRoot::None;
    metadata.sources = normalized
        .sources
        .iter()
        .map(|source| {
            let base = state.overlay.canonical(Path::new(&source.base))?;
            let mut pattern = source.pattern.trim_start_matches('/').to_owned();
            // Keep exact-file patterns matchable even when a negative source means
            // that file is absent from the final (read-only-selected) inventory.
            if !pattern.contains(['*', '?', '[', '\\']) {
                let first = pattern
                    .chars()
                    .next()
                    .ok_or_else(|| fail("source", &base, "Normalized source filename is empty"))?;
                if first.is_ascii_alphanumeric() {
                    // fast-glob treats \b, \n, \r and \t as control bytes,
                    // whereas gitignore/globset treat escaped letters literally.
                    // A singleton class has the same literal meaning in both.
                    pattern = format!("[{first}]{}", &pattern[first.len_utf8()..]);
                } else {
                    pattern.insert(0, '\\');
                }
            }
            Ok(SourceDirective {
                base: path_string(&base)?,
                pattern,
                negated: source.mode == SelectionMode::Excluded,
                source: None,
            })
        })
        .collect::<Result<Vec<_>, HostError>>()?;
    let (probe_snapshots, probe_inventory) = probes(state, &metadata)?;
    let normalized = select_sources(&metadata, &probe_snapshots, &probe_inventory)
        .map_err(|e| fail("source", root, e))?;
    let mut inventory = SourceInventory {
        directories: probe_inventory.directories.clone(),
        ..Default::default()
    };
    let mut snapshots = Vec::new();
    let mut directories: BTreeSet<String> = inventory.directories.iter().cloned().collect();
    let sources = &normalized.sources;
    let positive: Vec<_> = sources
        .iter()
        .filter(|s| s.mode != SelectionMode::Excluded)
        .collect();
    if positive.is_empty() {
        let selection = select_sources(&metadata, &snapshots, &inventory)
            .map_err(|e| fail("source", root, e))?;
        return Ok((metadata, inventory, snapshots, selection));
    }
    let disk_roots = positive
        .iter()
        .filter(|s| Path::new(&s.base).exists())
        .collect::<Vec<_>>();
    let mut builder = WalkBuilder::new(
        disk_roots
            .first()
            .map_or(positive[0].base.as_str(), |s| s.base.as_str()),
    );
    builder
        .follow_links(true)
        .hidden(false)
        .git_global(false)
        .require_git(false)
        .sort_by_file_path(|a, b| a.cmp(b));
    if Path::new(&positive[0].base)
        .ancestors()
        .any(|p| p.join(".git").exists())
    {
        builder.require_git(true);
    }
    for source in disk_roots.iter().skip(1) {
        builder.add(&source.base);
    }
    for rules in defaults()? {
        builder.add_gitignore(rules);
    }
    let mut rules: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for source in sources {
        let entries = rules.entry(source.base.clone()).or_default();
        match source.mode {
            SelectionMode::Automatic => {}
            SelectionMode::External => {
                entries.insert("!/**/*".into());
                entries.insert(format!("*.{}", group(BINARY)));
            }
            SelectionMode::Excluded => {
                entries.insert(source.pattern.clone());
            }
            SelectionMode::File | SelectionMode::Pattern => {
                if !source.pattern.contains("**") {
                    entries.insert(format!("!{}", source.pattern));
                } else if let Some(extension) = Path::new(&source.pattern)
                    .extension()
                    .and_then(|s| s.to_str())
                {
                    entries.insert(format!("!*.{extension}"));
                }
            }
        }
    }
    for (base, rules) in rules {
        let mut matcher = GitignoreBuilder::new(&base);
        for rule in rules {
            matcher
                .add_line(None, &rule)
                .map_err(|e| fail("ignore", Path::new(&base), e))?;
        }
        builder.add_gitignore(
            matcher
                .build()
                .map_err(|e| fail("ignore", Path::new(&base), e))?,
        );
    }
    // Selection is applied before file reads. The ignore walker handles actual
    // directory reachability, including combined source whitelist precedence.
    let positive_owned = positive.iter().map(|s| (*s).clone()).collect::<Vec<_>>();
    let match_sources = positive_owned.clone();
    let exclusions = sources
        .iter()
        .filter(|source| source.mode == SelectionMode::Excluded)
        .cloned()
        .collect::<Vec<_>>();
    let virtual_exclusions = exclusions.clone();
    builder.filter_entry(move |entry| {
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            return true;
        }
        matches_source(entry.path(), &match_sources)
            && !matches_exclusion(entry.path(), &exclusions)
    });
    let mut seen = BTreeSet::new();
    let mut seen_lexical = BTreeSet::new();
    if !disk_roots.is_empty() {
        for entry in builder.build() {
            let entry = entry.map_err(|e| fail("walk", root, e))?;
            if let Some(error) = entry.error() {
                return Err(fail("ignore", entry.path(), error));
            }
            if !seen_lexical.insert(entry.path().to_path_buf()) {
                continue;
            }
            let path = state.overlay.canonical(entry.path())?;
            let id = path_string(&path)?;
            if entry.file_type().is_some_and(|t| t.is_dir()) {
                directories.insert(id.clone());
                state.watch.directories.insert(id);
                if directories.len() > state.request.limits.directories {
                    return Err(fail(
                        "limit",
                        &path,
                        "Source discovery exceeds directory limit",
                    ));
                }
                watch_ignores(state, &path)?;
                continue;
            }
            if !entry.file_type().is_some_and(|t| t.is_file()) {
                continue;
            }
            if environment_file(&path) {
                state.unread.insert(id);
                continue;
            }
            if !seen.insert(id.clone()) {
                continue;
            }
            if seen.len() > state.request.limits.files {
                return Err(fail("limit", &path, "Source discovery exceeds file limit"));
            }
            // The normalized patterns are anchored to canonical source bases. A
            // symlink within those bases can point outside them; record an explicit
            // canonical file source for that reached input instead of losing it.
            if path != entry.path() && !matches_source(&path, &positive_owned) {
                let base = path
                    .parent()
                    .ok_or_else(|| fail("path", &path, "Source has no parent"))?;
                metadata.sources.push(SourceDirective {
                    base: path_string(base)?,
                    pattern: escape(path.file_name().unwrap().to_str().unwrap()),
                    negated: false,
                    source: None,
                });
                directories.insert(path_string(base)?);
            }
            let is_binary = binary(&path);
            let (text, version) = state.read(&path)?;
            snapshots.push(SourceSnapshot {
                id: id.clone(),
                version,
                extension: path
                    .extension()
                    .and_then(|s| s.to_str())
                    .unwrap_or("")
                    .into(),
                text,
            });
            inventory.files.push(SourcePath {
                id: id.clone(),
                path: id.clone(),
                binary: is_binary,
            });
            if !is_binary {
                inventory.automatic_eligible_ids.push(id);
            }
        }
    }
    // The disk walker cannot enumerate new snapshots. Evaluate their complete
    // ancestor chain using its same ignore matcher, then apply the same source
    // predicates. Existing snapshots rejected by the walker stay rejected too.
    let virtual_paths = state.overlay.files.keys().cloned().collect::<Vec<_>>();
    for path in virtual_paths {
        let id = path_string(&path)?;
        if seen.contains(&id)
            || !matches_source(&path, &positive_owned)
            || matches_exclusion(&path, &virtual_exclusions)
        {
            continue;
        }
        let mut reachable = false;
        for source in &positive_owned {
            let base = Path::new(&source.base);
            if path.starts_with(base)
                && !builder
                    .snapshot_path_ignored(base, &path)
                    .map_err(|e| fail("ignore", &path, e))?
            {
                reachable = true;
                break;
            }
        }
        if !reachable {
            continue;
        }
        if !seen.insert(id.clone()) {
            continue;
        }
        if seen.len() > state.request.limits.files {
            return Err(fail("limit", &path, "Source discovery exceeds file limit"));
        }
        for parent in path.ancestors().skip(1) {
            let parent_id = path_string(parent)?;
            directories.insert(parent_id.clone());
            state.watch.directories.insert(parent_id);
            if !parent.exists() {
                state.watch.missing.insert(path_string(parent)?);
            }
            watch_ignores(state, parent)?;
        }
        if directories.len() > state.request.limits.directories {
            return Err(fail(
                "limit",
                &path,
                "Source discovery exceeds directory limit",
            ));
        }
        let (text, version) = state.read(&path)?;
        let is_binary = binary(&path);
        snapshots.push(SourceSnapshot {
            id: id.clone(),
            version,
            extension: path
                .extension()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .into(),
            text,
        });
        inventory.files.push(SourcePath {
            id: id.clone(),
            path: id.clone(),
            binary: is_binary,
        });
        if !is_binary {
            inventory.automatic_eligible_ids.push(id);
        }
    }
    for source in sources {
        for ancestor in Path::new(&source.base).ancestors() {
            watch_ignores(state, ancestor)?;
        }
    }
    inventory.directories = directories.into_iter().collect();
    snapshots.sort_by(|a, b| a.id.cmp(&b.id));
    inventory.files.sort_by(|a, b| a.id.cmp(&b.id));
    inventory.automatic_eligible_ids.sort();
    let selection =
        select_sources(&metadata, &snapshots, &inventory).map_err(|e| fail("source", root, e))?;
    if selection.selected.len() != snapshots.len() {
        return Err(fail(
            "unsupported",
            root,
            "Canonical source identities conflict with logical source exclusions (for example, a symlink into an excluded target); this requires per-alias selection metadata",
        ));
    }
    Ok((metadata, inventory, snapshots, selection))
}
fn escape(value: &str) -> String {
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
fn matches_source(path: &Path, sources: &[SelectionReason]) -> bool {
    sources.iter().any(|source| {
        let Ok(relative) = path.strip_prefix(&source.base) else {
            return false;
        };
        if matches!(
            source.mode,
            SelectionMode::Automatic | SelectionMode::External
        ) {
            return true;
        }
        let Ok(relative) = path_string(relative) else {
            return false;
        };
        fast_glob::glob_match(&source.pattern, format!("/{relative}"))
    })
}
fn matches_exclusion(path: &Path, sources: &[SelectionReason]) -> bool {
    sources.iter().any(|source| {
        let Ok(relative) = path.strip_prefix(&source.base) else {
            return false;
        };
        relative.ancestors().any(|ancestor| {
            let Ok(text) = path_string(ancestor) else {
                return false;
            };
            !text.is_empty() && fast_glob::glob_match(&source.pattern, format!("/{text}"))
        })
    })
}
fn watch_ignores(state: &mut State<'_>, directory: &Path) -> Result<(), HostError> {
    for name in [".gitignore", ".ignore", ".git/info/exclude"] {
        let path = directory.join(name);
        let id = path_string(&path)?;
        match fs::metadata(&path) {
            Ok(metadata) if metadata.is_file() => {
                state.watch.files.insert(id);
            }
            Ok(_) => {}
            Err(e)
                if e.kind() == std::io::ErrorKind::NotFound
                    || e.kind() == std::io::ErrorKind::NotADirectory =>
            {
                state.watch.missing.insert(id);
            }
            Err(e) => return Err(fail("io", &path, e)),
        }
    }
    Ok(())
}
