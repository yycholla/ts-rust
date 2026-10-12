use super::*;
use oxc_resolver::{AliasValue, ResolveContext, ResolveOptions, ResolverGeneric};
use ts_tailwind::{
    TAILWIND_VERSION,
    ast::{AstNode, NodeKind},
    css,
    imports::parse_import_params,
    snapshot::{ResolvedImport, StylesheetSnapshot},
};

pub(super) fn load(
    state: &mut State<'_>,
    root: &Path,
    entry: &Path,
) -> Result<(ProjectSnapshot, ImportOptions), HostError> {
    let mut modules = vec!["node_modules".into()];
    for path in &state.request.module_paths {
        if !path.is_absolute() {
            return Err(fail(
                "input",
                path,
                "Additional module lookup paths must be absolute",
            ));
        }
        modules.push(path_string(path)?);
    }
    let mut alias = Vec::new();
    for value in &state.request.aliases {
        if value.find.is_empty()
            || value.replacement.is_empty()
            || value.find.contains('\0')
            || value.replacement.contains('\0')
        {
            return Err(fail(
                "input",
                root,
                "Invalid empty or NUL-containing CSS alias",
            ));
        }
        alias.push((
            value.find.clone(),
            vec![AliasValue::Path(value.replacement.clone())],
        ));
    }
    let vite = !matches!(state.snapshots.resolver, ResolverProfile::Node);
    let conditions = match state.snapshots.resolver {
        ResolverProfile::Node => vec!["style".into()],
        ResolverProfile::ViteDevelopment => vec!["style".into(), "development".into()],
        ResolverProfile::ViteProduction => vec!["style".into(), "production".into()],
    };
    let resolver = ResolverGeneric::new_with_file_system(
        state.overlay.clone(),
        ResolveOptions {
            cwd: Some(root.into()),
            condition_names: conditions,
            prefer_relative: vite,
            main_files: if vite { vec![] } else { vec!["index".into()] },
            extensions: vec![".css".into()],
            main_fields: vec!["style".into()],
            modules,
            alias,
            node_path: false,
            ..ResolveOptions::default()
        },
    );
    let entry = match state.overlay.canonical(
        state
            .snapshots
            .entry
            .as_ref()
            .map_or(entry, |e| e.origin.as_path()),
    ) {
        Ok(p) => p,
        Err(_) if state.snapshots.entry.is_some() => {
            crate::snapshot_fs::identity(&state.snapshots.entry.as_ref().unwrap().origin)
                .map_err(|e| fail("path", entry, e))?
        }
        Err(e) => {
            state.watch.missing.insert(path_string(entry)?);
            return Err(e);
        }
    };
    let entry_id = state
        .snapshots
        .entry
        .as_ref()
        .map_or_else(|| path_string(&entry), |e| Ok(e.id.clone()))?;
    let mut snapshot = ProjectSnapshot {
        tailwind_version: TAILWIND_VERSION.into(),
        entry: entry_id,
        stylesheets: vec![],
        sources: vec![],
        imports: vec![],
        plugins: state.request.plugins.clone(),
    };
    let mut options = ImportOptions {
        bases: BTreeMap::new(),
        limits: Default::default(),
    };
    let mut pending = vec![(entry, true)];
    let mut visited = BTreeSet::new();
    while let Some((path, is_entry)) = pending.pop() {
        let extracted = if is_entry {
            state.snapshots.entry.as_ref()
        } else {
            None
        };
        let id = extracted.map_or_else(|| path_string(&path), |e| Ok(e.id.clone()))?;
        if !is_entry && state.snapshots.entry.as_ref().is_some_and(|e| e.id == id) {
            return Err(fail(
                "snapshot",
                &path,
                "Extracted stylesheet identity collides with imported file",
            ));
        }
        if !visited.insert(id.clone()) {
            continue;
        }
        if visited.len() > state.request.limits.stylesheets {
            return Err(fail("limit", &path, "CSS graph exceeds stylesheet limit"));
        }
        let (text, version) = if let Some(entry) = extracted {
            state.bytes = state.bytes.saturating_add(entry.css.len());
            if state.bytes > state.request.limits.total_bytes {
                return Err(fail("limit", &path, "Snapshot byte limit exceeded"));
            }
            state.watch.snapshots.insert(
                id.clone(),
                SnapshotIdentity {
                    version: entry.version,
                    sha256: format!("{:x}", Sha256::digest(entry.css.as_bytes())),
                },
            );
            (entry.css.clone(), entry.version)
        } else {
            state.read(&path)?
        };
        let ast = css::parse(&text, &id).map_err(|e| fail("css", &path, e))?;
        let base = path
            .parent()
            .ok_or_else(|| fail("path", &path, "Stylesheet has no parent directory"))?;
        options.bases.insert(id.clone(), path_string(base)?);
        let mut specifiers = BTreeSet::new();
        inspect(&ast, &path, &mut specifiers)?;
        for specifier in specifiers {
            if specifier.starts_with("data:")
                || specifier.starts_with("http://")
                || specifier.starts_with("https://")
            {
                continue;
            }
            if environment_file(Path::new(&specifier)) {
                return Err(fail(
                    "protectedInput",
                    &path,
                    "CSS import refers to an environment file",
                ));
            }
            let mut context = ResolveContext::default();
            let result = resolver.resolve_with_context(base, &specifier, None, &mut context);
            for dependency in context.file_dependencies {
                if let Some(file) = state.overlay.get(&dependency) {
                    state.watch.snapshots.insert(
                        path_string(&file.path)?,
                        SnapshotIdentity {
                            version: file.version,
                            sha256: format!("{:x}", Sha256::digest(file.text.as_bytes())),
                        },
                    );
                } else {
                    state.watch.files.insert(path_string(&dependency)?);
                }
            }
            for dependency in context.missing_dependencies {
                state.watch.missing.insert(path_string(&dependency)?);
            }
            let result = result.map_err(|e| {
                fail(
                    "resolve",
                    &path,
                    format!("Cannot resolve CSS import {specifier:?}: {e}"),
                )
            })?;
            if result.query().is_some() || result.fragment().is_some() {
                return Err(fail(
                    "unsupported",
                    &path,
                    format!("CSS import queries/fragments need an explicit loader: {specifier}"),
                ));
            }
            let target = state.overlay.canonical(result.path())?;
            snapshot.imports.push(ResolvedImport {
                from: id.clone(),
                specifier,
                target: path_string(&target)?,
            });
            if snapshot.imports.len() > 16_384 {
                return Err(fail("limit", &path, "CSS graph exceeds import limit"));
            }
            pending.push((target, false));
        }
        snapshot.stylesheets.push(StylesheetSnapshot {
            id,
            version,
            css: text,
        });
    }
    snapshot.stylesheets.sort_by(|a, b| a.id.cmp(&b.id));
    snapshot
        .imports
        .sort_by(|a, b| (&a.from, &a.specifier).cmp(&(&b.from, &b.specifier)));
    Ok((snapshot, options))
}
fn inspect(
    nodes: &[AstNode],
    path: &Path,
    imports: &mut BTreeSet<String>,
) -> Result<(), HostError> {
    for node in nodes {
        if let NodeKind::AtRule { name, params, .. } = &node.kind {
            if name == "@config" {
                return Err(fail(
                    "unsupported",
                    path,
                    "@config requires JavaScript execution; supply native resolved theme/plugin inputs instead",
                ));
            }
            if name == "@import" || name == "@reference" {
                if let Some(parsed) =
                    parse_import_params(params).map_err(|e| fail("css", path, e))?
                {
                    imports.insert(parsed.uri);
                }
            }
        }
        if let Some(children) = node.children() {
            inspect(children, path, imports)?;
        }
    }
    Ok(())
}
