//! Native instrumentation orchestration over actual emitted modules and bytes.
//!
//! Kit's MIT-licensed builder instrument/createInstrumentationInitializer algorithms
//! live in ts_svelte_kit::build_instrumentation. This layer validates bundle identity,
//! dependencies and source maps before invoking them. It performs no I/O or JS execution.
//! Adapter-node 5.5.4 calls the Kit 2 instrument contract (no initializer). Its
//! installed templates are not silently treated as a Kit 3 instrumentation adapter.
use crate::{Artifact, BuildRequest, artifacts};
use serde::Serialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};
use ts_svelte_bundle::{BundleResult, Chunk, Target};
use ts_svelte_kit::{
    adapter_node,
    build_instrumentation::{
        self as kit, InstrumentServerInput, InstrumentationAdapter, InstrumentationEnvironment,
        InstrumentationInitializer,
    },
    sync_versions::KitSyncVersion,
};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerInstrumentation {
    kit_version: String,
    source: String,
    emitted_file: String,
    /// Includes both static and dynamically reachable emitted chunks.
    dependencies: Vec<String>,
    external_dependencies: Vec<String>,
    environment_initializer_required: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactIdentity {
    /// Prior emitted artifact whose bytes/coordinates this file retains.
    pub original_artifact: Option<String>,
    pub original_module: Option<String>,
    /// The generated facade has no invented source map. Retained old entry maps
    /// are recorded against start, matching upstream's unchanged map-copy contract.
    pub source_map_for: Option<String>,
    pub generated: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstrumentedArtifacts {
    pub artifacts: BTreeMap<String, Artifact>,
    pub identities: BTreeMap<String, ArtifactIdentity>,
    pub entrypoint: String,
    pub start: String,
    pub static_preludes: Vec<String>,
    pub dynamic_start: String,
    pub map_copy: Option<kit::InstrumentationMapCopy>,
}

fn path(prefix: &str, file: &str) -> Result<String, String> {
    artifacts::relative(file)?;
    if prefix.is_empty() {
        return Ok(file.to_owned());
    }
    artifacts::relative(prefix)?;
    Ok(format!("{prefix}/{file}"))
}
fn chunk<'a>(bundle: &'a BundleResult, file: &str) -> Result<&'a Chunk, String> {
    let mut found = bundle.chunks.iter().filter(|chunk| chunk.file_name == file);
    let chunk = found
        .next()
        .ok_or_else(|| format!("Missing emitted instrumentation dependency {file}"))?;
    if found.next().is_some() {
        return Err(format!("Duplicate emitted chunk {file}"));
    }
    Ok(chunk)
}
fn bytes(files: &BTreeMap<String, Artifact>, file: &str) -> Result<Vec<u8>, String> {
    artifacts::bytes(
        files
            .get(file)
            .ok_or_else(|| format!("Missing instrumentation artifact {file}"))?,
    )
}
fn validate_chunk(
    chunk: &Chunk,
    prefix: &str,
    files: &BTreeMap<String, Artifact>,
) -> Result<(), String> {
    let file = path(prefix, &chunk.file_name)?;
    if bytes(files, &file)? != chunk.code.as_bytes() {
        return Err(format!("Stale instrumentation artifact {file}"));
    }
    match (&chunk.map, &chunk.sourcemap_file_name) {
        (Some(map), Some(map_file)) => {
            let map_file = path(prefix, map_file)?;
            let actual: serde_json::Value = serde_json::from_slice(&bytes(files, &map_file)?)
                .map_err(|error| format!("Invalid instrumentation map {map_file}: {error}"))?;
            if &actual != map
                || map["version"] != 3
                || !map["sources"].is_array()
                || !map["names"].is_array()
                || !map["mappings"].is_string()
            {
                return Err(format!(
                    "Stale or invalid instrumentation source map {map_file}"
                ));
            }
        }
        (None, None) => {}
        _ => return Err(format!("Incomplete emitted source-map metadata for {file}")),
    }
    Ok(())
}

fn closure(
    bundle: &BundleResult,
    entry: &str,
    prefix: &str,
    files: &BTreeMap<String, Artifact>,
) -> Result<(Vec<String>, Vec<String>), String> {
    if bundle.target != Target::Server {
        return Err("Instrumentation needs a server-target bundle".into());
    }
    let mut seen = BTreeSet::new();
    let mut external = BTreeSet::new();
    let mut pending = vec![entry.to_owned()];
    while let Some(file) = pending.pop() {
        if !seen.insert(file.clone()) {
            continue;
        }
        let current = chunk(bundle, &file)?;
        validate_chunk(current, prefix, files)?;
        for import in current.imports.iter().chain(&current.dynamic_imports) {
            if bundle.chunks.iter().any(|chunk| &chunk.file_name == import) {
                pending.push(import.clone());
            } else if bundle
                .module_graph
                .get(import)
                .is_some_and(|module| module.is_external)
            {
                external.insert(import.clone());
            } else {
                return Err(format!(
                    "Unresolved emitted instrumentation edge {file} -> {import}"
                ));
            }
        }
    }
    Ok((seen.into_iter().collect(), external.into_iter().collect()))
}

/// Validate the independent server entry after finalization and feature tracking,
/// against the current artifact snapshot. A receipt does not mean an adapter ran.
pub fn validate_server(
    build: &BuildRequest,
    bundle: &BundleResult,
    files: &BTreeMap<String, Artifact>,
) -> Result<Option<ServerInstrumentation>, String> {
    let Some(config) = &build.kit.config.instrumentation else {
        return Ok(None);
    };
    validate_entry(
        &build.kit.inventory.kit_version,
        &config.entry,
        Some(config.enabled),
        build
            .kit
            .config
            .adapter_name
            .as_ref()
            .map(|name| InstrumentationAdapter {
                name: name.clone(),
                supports_instrumentation: config.adapter_supports,
            }),
        bundle,
        "server",
        files,
    )
    .map(Some)
}

pub fn validate_entry(
    version: &str,
    source: &Path,
    enabled: Option<bool>,
    adapter: Option<InstrumentationAdapter>,
    bundle: &BundleResult,
    prefix: &str,
    files: &BTreeMap<String, Artifact>,
) -> Result<ServerInstrumentation, String> {
    let plan = kit::plan_instrumentation_entry(&kit::InstrumentationEntryInput {
        kit_version: version.into(),
        source: Some(source.into()),
        kit2_experimental_enabled: enabled,
        adapter,
    })?;
    let file = plan
        .output_file
        .as_ref()
        .and_then(|file| file.to_str())
        .ok_or("Missing instrumentation output filename")?;
    let entry = chunk(bundle, file)?;
    let source = source
        .to_str()
        .ok_or("Instrumentation source must be UTF-8")?;
    if !entry.is_entry || entry.facade_module_id.as_deref() != Some(source) {
        return Err(format!(
            "Instrumentation facade does not identify its resolved source {source}"
        ));
    }
    if !bundle
        .module_graph
        .get(source)
        .is_some_and(|module| module.is_entry && !module.is_external)
    {
        return Err("Instrumentation source is absent from the actual loaded entry graph".into());
    }
    let (dependencies, external_dependencies) = closure(bundle, file, prefix, files)?;
    Ok(ServerInstrumentation {
        kit_version: version.into(),
        source: source.into(),
        emitted_file: file.into(),
        dependencies,
        external_dependencies,
        environment_initializer_required: plan.environment_initializer_required,
    })
}

/// Early adapter eligibility check. Kit 3's generic bridge below is available,
/// but node 5.5.4's actual API call omits its required environment initializer.
pub fn validate_adapter_node_version(version: &str) -> Result<(), String> {
    if KitSyncVersion::parse(version)?.is_kit3() {
        return Err(format!(
            "adapter-node {} uses the Kit 2 instrumentation contract; Kit 3 requires an adapter supplying createInstrumentationInitializer and its real emitted env.js",
            adapter_node::ADAPTER_VERSION
        ));
    }
    Ok(())
}

fn transform(
    version: &str,
    files: &BTreeMap<String, Artifact>,
    entrypoint: String,
    instrumentation: String,
    exports: Vec<String>,
    initializer: Option<InstrumentationInitializer>,
    source_module: Option<String>,
) -> Result<InstrumentedArtifacts, String> {
    let mut input = kit::InstrumentationFiles::new();
    let mut total = 0usize;
    for (file, artifact) in files {
        artifacts::relative(file)?;
        let bytes = artifacts::bytes(artifact)?;
        total = total
            .checked_add(bytes.len())
            .ok_or("Instrumentation artifact length overflow")?;
        if total > 512 * 1024 * 1024 {
            return Err("Instrumentation snapshot exceeds 512 MiB".into());
        }
        input.insert(PathBuf::from(file), bytes);
    }
    let plan = kit::instrument_server_entry(&InstrumentServerInput {
        kit_version: version.into(),
        files: input,
        entrypoint: entrypoint.clone().into(),
        instrumentation: instrumentation.into(),
        start: None,
        exports,
        initializer,
    })?;
    let start = plan.start.to_str().ok_or("Invalid start path")?.to_owned();
    let mut output = files.clone();
    let mut identities = BTreeMap::new();
    for (file, contents) in &plan.files {
        let file = file
            .to_str()
            .ok_or("Invalid generated artifact path")?
            .to_owned();
        let changed = output
            .get(&file)
            .map(artifacts::bytes)
            .transpose()?
            .as_deref()
            != Some(contents.as_slice());
        if !changed {
            continue;
        }
        output.remove(&file);
        artifacts::insert(&mut output, file.clone(), contents, "instrumentation")?;
        let copied_map = plan
            .map_copy
            .as_ref()
            .is_some_and(|copy| copy.copied == Path::new(&file));
        let original_artifact = if file == start {
            Some(entrypoint.clone())
        } else if copied_map {
            Some(format!("{entrypoint}.map"))
        } else {
            None
        };
        identities.insert(
            file.clone(),
            ArtifactIdentity {
                original_artifact,
                original_module: (file == start).then(|| source_module.clone()).flatten(),
                source_map_for: copied_map.then(|| start.clone()),
                generated: file != start && !copied_map,
            },
        );
    }
    if let Some(copy) = &plan.map_copy {
        identities.insert(
            copy.original.to_string_lossy().into_owned(),
            ArtifactIdentity {
                original_artifact: Some(copy.original.to_string_lossy().into_owned()),
                original_module: source_module,
                source_map_for: Some(start.clone()),
                generated: false,
            },
        );
    }
    Ok(InstrumentedArtifacts {
        artifacts: output,
        identities,
        entrypoint,
        start,
        static_preludes: plan.entry_preludes,
        dynamic_start: plan.dynamic_start_specifier,
        map_copy: plan.map_copy,
    })
}

/// Apply Kit's versioned wrapper to an actual emitted ESM entry. This is the
/// generic adapter seam, not a claim that adapter-node 5.5.4 supports Kit 3.
/// Prefix identifies bundle filenames inside the supplied deployment snapshot.
/// Kit 3 verifies the real sibling env.js entry and set_env export first.
pub fn instrument_bundle_entry(
    version: &str,
    bundle: &BundleResult,
    files: &BTreeMap<String, Artifact>,
    prefix: &str,
    entry_file: &str,
    instrumentation_file: &str,
    environment: Option<InstrumentationEnvironment>,
) -> Result<InstrumentedArtifacts, String> {
    artifacts::relative(prefix)?;
    let kit3 = KitSyncVersion::parse(version)?.is_kit3();
    let entry = chunk(bundle, entry_file)?;
    let instrument = chunk(bundle, instrumentation_file)?;
    if !entry.is_entry || !instrument.is_entry {
        return Err("Instrumentation and application must be emitted entry chunks".into());
    }
    closure(bundle, entry_file, prefix, files)?;
    closure(bundle, instrumentation_file, prefix, files)?;
    let entrypoint = path(prefix, entry_file)?;
    let instrumentation = path(prefix, instrumentation_file)?;
    let initializer = if kit3 {
        let environment =
            environment.ok_or("Kit 3 instrumentation requires an explicit environment provider")?;
        let env = chunk(bundle, "env.js")?;
        if !env.is_entry || !env.exports.iter().any(|name| name == "set_env") {
            return Err("Actual emitted env.js must export set_env".into());
        }
        let (dependencies, _) = closure(bundle, "env.js", prefix, files)?;
        if dependencies
            .iter()
            .any(|file| file == entry_file || file == instrumentation_file)
        {
            return Err("Environment initializer depends on application/instrumentation entry and cannot run first".into());
        }
        Some(InstrumentationInitializer::Generate {
            output_directory: Path::new(&entrypoint)
                .parent()
                .ok_or("Missing entry directory")?
                .into(),
            server_directory: PathBuf::from(prefix),
            environment,
        })
    } else {
        if environment.is_some() {
            return Err("Kit 2 has no instrumentation environment initializer".into());
        }
        None
    };
    transform(
        version,
        files,
        entrypoint,
        instrumentation,
        entry.exports.clone(),
        initializer,
        entry.facade_module_id.clone(),
    )
}

/// Complete the pinned node adapter after rebundling/materialization. `prepare`
/// already copied index to start and installed its Kit2 facade; authenticate that
/// exact facade, reconstruct the original entry snapshot, then run the shared
/// versioned transform. This avoids double-wrapping and preserves sibling maps.
/// The caller atomically replaces AdapterOutput.artifacts with the returned map;
/// AdapterOutput.bundle still describes only the unchanged server rebundle.
pub fn finalize_adapter_node(
    version: &str,
    out: &str,
    staging_directory: &str,
    server: &ServerInstrumentation,
    bundle: &BundleResult,
    files: &BTreeMap<String, Artifact>,
) -> Result<InstrumentedArtifacts, String> {
    validate_adapter_node_version(version)?;
    artifacts::relative(out)?;
    artifacts::relative(staging_directory)?;
    if server.kit_version != version {
        return Err("Instrumentation receipt has a different Kit version".into());
    }
    let server_prefix = format!("{out}/server");
    let emitted = chunk(bundle, &server.emitted_file)?;
    let expected_source = format!("{staging_directory}/{}", server.emitted_file);
    if !emitted.is_entry
        || emitted
            .facade_module_id
            .as_deref()
            .is_none_or(|id| !id.ends_with(&format!("/{expected_source}")))
    {
        return Err("Rebundled instrumentation does not identify the adapter staging entry".into());
    }
    closure(bundle, &server.emitted_file, &server_prefix, files)?;
    let entrypoint = format!("{out}/index.js");
    let start = format!("{out}/start.js");
    let expected = b"import './server/instrumentation.server.js';\nconst { path, host, port, server } = await import('./start.js');\nexport { path, host, port, server };";
    if bytes(files, &entrypoint)? != expected {
        return Err(
            "Adapter-node prepared instrumentation facade does not match pinned 5.5.4".into(),
        );
    }
    let original = bytes(files, &start)?;
    let mut original_files = files.clone();
    original_files.remove(&start);
    original_files.remove(&entrypoint);
    artifacts::insert(
        &mut original_files,
        entrypoint.clone(),
        &original,
        "adapter-node",
    )?;
    // Upstream's copied start map, if present, must be the identical original.
    if let Some(map) = original_files.remove(&format!("{start}.map")) {
        if artifacts::bytes(&map)? != bytes(&original_files, &format!("{entrypoint}.map"))? {
            return Err("Adapter start map differs from its original entry map".into());
        }
    }
    let mut result = transform(
        version,
        &original_files,
        entrypoint,
        path(&server_prefix, &server.emitted_file)?,
        vec!["path".into(), "host".into(), "port".into(), "server".into()],
        None,
        Some("@sveltejs/adapter-node@5.5.4/files/index.js".into()),
    )?;
    if let Some(identity) = result.identities.get_mut(&start) {
        // The prepared adapter snapshot already named these original bytes start.
        identity.original_artifact = Some(start);
    }
    Ok(result)
}

#[cfg(test)]
#[path = "instrumentation_tests.rs"]
mod tests;
