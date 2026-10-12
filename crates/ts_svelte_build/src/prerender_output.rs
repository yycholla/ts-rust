//! Pure output materialization after Kit's completed prerender policy.
//! Source: pinned Kit 2.69.2/3.0.1 postbuild/prerender.js final queue/hashlink/
//! unseen-route checks and exports/vite build manifest filtering.
//!
//! A Finished receipt must come from the SAME live policy/build as the supplied
//! snapshots, after all visitor promises and effects have completed. Snapshots
//! cannot authenticate that relationship: integration must enforce it. This
//! helper neither runs visits nor declares a framework build complete. Kit 3
//! placeholder finalization, remote tree-shaking, final manifest generation and
//! adapter packaging remain later operations, in their version-specific order.
use crate::{
    Artifact, artifacts,
    prerender_policy::{PolicyResult, Status},
};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use ts_svelte_kit::{
    adapter_node::Prerendered,
    prerender_queue::{PrerenderValue, QueueSnapshot},
    prerender_save::{SaveSnapshot, output_filename},
};

const MAX_FILES: usize = 100_000;
const MAX_BYTES: usize = 256 * 1024 * 1024;
const MAX_TEXT: usize = 1024 * 1024;
pub struct CompletedPrerender<'a> {
    pub completion: &'a PolicyResult,
    pub save: &'a SaveSnapshot,
    pub queue: &'a QueueSnapshot,
    /// Actual applied Write effects retained relative to output/prerendered.
    /// This is NOT the published alias map: redirects may have no saved alias.
    pub files: &'a BTreeMap<String, Vec<u8>>,
    pub base: &'a str,
    /// Inventory order, including routes absent from the prerender Map.
    pub route_ids: &'a [String],
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RouteInclusion {
    pub id: String,
    pub prerender: Option<PrerenderValue>,
    /// Exact upstream `prerender_map.get(id) !== true` (Auto is retained).
    pub include_in_server_manifest: bool,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OutputCounts {
    pub files: usize,
    pub bytes: usize,
    pub page_files: usize,
    pub dependency_files: usize,
    pub data_files: usize,
    pub page_entries: usize,
    pub asset_entries: usize,
    pub redirect_entries: usize,
    /// May contain repeated paths; this is the real upstream array length.
    pub paths: usize,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PrerenderOutput {
    /// Validated output delta only; the caller's existing artifacts are untouched.
    pub artifacts: BTreeMap<String, Artifact>,
    pub adapter: Prerendered,
    /// Retain all redirect/page/asset/alias metadata, including insertion order.
    pub save: SaveSnapshot,
    pub queue: QueueSnapshot,
    pub routes: Vec<RouteInclusion>,
    pub unwritten_aliases: Vec<(String, String)>,
    pub counts: OutputCounts,
}
fn text(value: &str) -> Result<(), String> {
    if value.len() > MAX_TEXT || value.contains('\0') {
        return Err("Prerender metadata text exceeds bounds or contains NUL".into());
    }
    Ok(())
}
fn relative(value: &str) -> Result<(), String> {
    text(value)?;
    artifacts::relative(value)
}
fn url_path(value: &str) -> Result<(), String> {
    text(value)?;
    if !value.starts_with('/') || value.contains(['?', '#', '\\']) {
        return Err(format!("Invalid prerender pathname {value:?}"));
    }
    Ok(())
}
fn unique<'a>(
    values: impl IntoIterator<Item = &'a str>,
    label: &str,
) -> Result<BTreeSet<&'a str>, String> {
    let mut set = BTreeSet::new();
    for value in values {
        text(value)?;
        if !set.insert(value) {
            return Err(format!("Duplicate {label}: {value}"));
        }
        if set.len() > MAX_FILES {
            return Err(format!("Too many {label}"));
        }
    }
    Ok(set)
}
fn destination(path: &str) -> Result<(&str, &str), String> {
    relative(path)?;
    let (category, file) = path
        .split_once('/')
        .ok_or_else(|| format!("Missing prerender category: {path}"))?;
    if !matches!(category, "pages" | "dependencies" | "data") {
        return Err(format!("Unknown prerender category: {category}"));
    }
    relative(file)?;
    Ok((category, file))
}
fn no_file_directory_collisions<'a>(
    paths: impl IntoIterator<Item = &'a str>,
) -> Result<(), String> {
    let paths: BTreeSet<_> = paths.into_iter().collect();
    for path in &paths {
        let mut path = *path;
        while let Some((parent, _)) = path.rsplit_once('/') {
            if paths.contains(parent) {
                return Err(format!(
                    "Prerender artifact file/directory collision: {parent}"
                ));
            }
            path = parent;
        }
    }
    Ok(())
}
/// Validate completely, then produce the artifact delta and adapter maps.
/// Missing non-writing aliases are preserved as missing; no file is fabricated.
pub fn materialize(
    input: CompletedPrerender<'_>,
    existing: &BTreeMap<String, Artifact>,
) -> Result<PrerenderOutput, String> {
    if input.completion.status != Status::Finished {
        return Err("Prerender materialization requires a Finished policy receipt".into());
    }
    text(input.base)?;
    if !input.base.is_empty()
        && (!input.base.starts_with('/')
            || input.base.ends_with('/')
            || input.base.contains(['?', '#', '\\']))
    {
        return Err("Expected resolved prerender base".into());
    }
    let save = input.save;
    let queue = input.queue;
    if input.files.len() > MAX_FILES || save.paths.len() > MAX_FILES {
        return Err("Prerender output count exceeds bounds".into());
    }
    let written = unique(save.written.iter().map(String::as_str), "written filename")?;
    for file in &written {
        relative(file)?;
    }
    unique(
        save.pages.iter().map(|(path, _)| path.as_str()),
        "page entry",
    )?;
    unique(
        save.assets.iter().map(|(path, _)| path.as_str()),
        "asset entry",
    )?;
    unique(
        save.redirects.iter().map(|(path, _)| path.as_str()),
        "redirect entry",
    )?;
    let saved = unique(
        save.saved.iter().map(|(file, _)| file.as_str()),
        "saved alias",
    )?;
    let manifest_assets = unique(
        save.manifest_assets.iter().map(String::as_str),
        "manifest asset",
    )?;
    if saved != manifest_assets {
        return Err("Saved aliases and manifest asset records disagree".into());
    }
    unique(
        save.prerendered_routes.iter().map(String::as_str),
        "saved route",
    )?;
    unique(
        queue.prerendered_routes.iter().map(String::as_str),
        "queue route",
    )?;
    if save.prerendered_routes != queue.prerendered_routes {
        return Err("Save/queue route observation order disagrees".into());
    }
    unique(
        queue.prerender_map.iter().map(|(id, _)| id.as_str()),
        "prerender route policy",
    )?;
    unique(
        input.route_ids.iter().map(String::as_str),
        "inventory route",
    )?;

    let mut by_file = BTreeMap::new();
    let mut bytes = 0usize;
    for (path, body) in input.files {
        let (_, file) = destination(path)?;
        if by_file.insert(file, path.as_str()).is_some() {
            return Err(format!(
                "Written filename occurs in multiple categories: {file}"
            ));
        }
        if !written.contains(file) {
            return Err(format!("Retained bytes have no written record: {path}"));
        }
        bytes = bytes
            .checked_add(body.len())
            .ok_or("Prerender byte count overflow")?;
        if body.len() > 64 * 1024 * 1024 || bytes > MAX_BYTES {
            return Err("Prerender output bytes exceed bounds".into());
        }
    }
    if by_file.len() != written.len() {
        let missing = written
            .iter()
            .find(|file| !by_file.contains_key(**file))
            .unwrap();
        return Err(format!("Written file bytes are missing: {missing}"));
    }
    let mut unwritten_aliases = vec![];
    for (file, path) in &save.saved {
        relative(file)?;
        let (_, actual) = destination(path)?;
        if actual != file {
            return Err(format!(
                "Saved alias filename disagrees with destination: {file} -> {path}"
            ));
        }
        if !input.files.contains_key(path) {
            unwritten_aliases.push((file.clone(), path.clone()));
        }
    }
    // Metadata identifies what was actually written independently of aliases.
    // A redirect is a written HTML file but does not pass through alias().
    let mut metadata_files = BTreeSet::new();
    let mut metadata_paths = BTreeMap::<&str, usize>::new();
    for (path, file) in &save.pages {
        url_path(path)?;
        relative(file)?;
        if output_filename(input.base, path, true)? != *file {
            return Err(format!(
                "Page filename disagrees with resolved base/path: {path}"
            ));
        }
        metadata_files.insert(file.clone());
        *metadata_paths.entry(path).or_default() += 1;
    }
    for (path, content_type) in &save.assets {
        url_path(path)?;
        if let Some(value) = content_type {
            text(value)?;
        }
        metadata_files.insert(output_filename(input.base, path, false)?);
        *metadata_paths.entry(path).or_default() += 1;
    }
    for (path, (status, location)) in &save.redirects {
        url_path(path)?;
        text(location)?;
        if *status / 100 != 3 || location.is_empty() {
            return Err(format!("Invalid redirect metadata for {path}"));
        }
        metadata_files.insert(output_filename(input.base, path, true)?);
        *metadata_paths.entry(path).or_default() += 1;
    }
    if metadata_files
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>()
        != written
    {
        return Err("Page/asset/redirect metadata disagrees with written filenames".into());
    }
    let mut actual_paths = BTreeMap::<&str, usize>::new();
    for path in &save.paths {
        url_path(path)?;
        *actual_paths.entry(path).or_default() += 1;
    }
    if actual_paths != metadata_paths {
        return Err("Prerender paths disagree with saved metadata entries".into());
    }

    for path in existing.keys() {
        relative(path)?;
    }
    let names: Vec<_> = input
        .files
        .keys()
        .map(|path| format!("prerendered/{path}"))
        .collect();
    no_file_directory_collisions(
        existing
            .keys()
            .map(String::as_str)
            .chain(names.iter().map(String::as_str)),
    )?;
    for (path, name) in input.files.keys().zip(&names) {
        if let Some(previous) = existing.get(name) {
            if artifacts::bytes(previous)? != input.files[path] {
                return Err(format!("Conflicting prerender artifact {name}"));
            }
        }
    }
    let mut output = BTreeMap::new();
    let mut adapter = Prerendered {
        paths: save.paths.clone(),
        ..Prerendered::default()
    };
    for (path, body) in input.files {
        let (category, file) = destination(path)?;
        let files = match category {
            "pages" => &mut adapter.pages,
            "dependencies" => &mut adapter.dependencies,
            "data" => &mut adapter.data,
            _ => unreachable!(),
        };
        files.insert(file.into(), body.clone());
        artifacts::insert(
            &mut output,
            format!("prerendered/{path}"),
            body,
            "prerender",
        )?;
    }
    let policies: BTreeMap<_, _> = queue
        .prerender_map
        .iter()
        .map(|(id, value)| (id.as_str(), *value))
        .collect();
    let routes = input
        .route_ids
        .iter()
        .map(|id| {
            let prerender = policies.get(id.as_str()).copied();
            RouteInclusion {
                id: id.clone(),
                prerender,
                include_in_server_manifest: prerender != Some(PrerenderValue::True),
            }
        })
        .collect();
    let counts = OutputCounts {
        files: output.len(),
        bytes,
        page_files: adapter.pages.len(),
        dependency_files: adapter.dependencies.len(),
        data_files: adapter.data.len(),
        page_entries: save.pages.len(),
        asset_entries: save.assets.len(),
        redirect_entries: save.redirects.len(),
        paths: save.paths.len(),
    };
    Ok(PrerenderOutput {
        artifacts: output,
        adapter,
        save: save.clone(),
        queue: queue.clone(),
        routes,
        unwritten_aliases,
        counts,
    })
}

#[cfg(test)]
#[path = "prerender_output_tests.rs"]
mod tests;
