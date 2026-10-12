//! Apply the framework's retained feature transform to emitted code and maps.
use crate::*;
use ts_svelte_kit::{
    build_finalize::{JavaScriptArtifact, SourceMapPolicy},
    build_tracking::{TrackingInput, collect_emitted_features},
};

pub(crate) fn collect(
    report: &mut BuildReport,
    bundle: &mut BundleResult,
    kit_version: &str,
    target: &str,
) -> Result<(), String> {
    let mut artifacts = BTreeMap::new();
    for chunk in &bundle.chunks {
        if artifacts
            .insert(
                chunk.file_name.clone(),
                JavaScriptArtifact {
                    code: chunk.code.clone(),
                    map: chunk.map.clone(),
                    map_file: chunk.sourcemap_file_name.clone(),
                    pending: vec![],
                    receipt_sha256: None,
                },
            )
            .is_some()
        {
            return Err(format!("Duplicate chunk {}", chunk.file_name));
        }
    }
    let output = collect_emitted_features(&TrackingInput {
        kit_version: kit_version.into(),
        after_tree_shaking: true,
        artifacts,
        source_maps: SourceMapPolicy::Preserve,
    })?;
    // Validate and serialize all map updates before mutating the bundle.
    let mut updates = BTreeMap::new();
    for (file, map) in &output.updated_map_files {
        let matches = bundle
            .assets
            .iter()
            .filter(|asset| &asset.file_name == file)
            .count();
        if matches != 1 {
            return Err(format!(
                "Expected one emitted source-map asset {file}, found {matches}"
            ));
        }
        updates.insert(
            file.clone(),
            base64::engine::general_purpose::STANDARD
                .encode(serde_json::to_vec(map).map_err(|error| error.to_string())?),
        );
    }
    if !output.removed_map_files.is_empty() || !output.removed_mapping_urls.is_empty() {
        return Err("Feature tracking unexpectedly removed source-map metadata".into());
    }
    for chunk in &mut bundle.chunks {
        let artifact = &output.artifacts[&chunk.file_name];
        chunk.code.clone_from(&artifact.code);
        chunk.map.clone_from(&artifact.map);
        chunk.sourcemap_file_name.clone_from(&artifact.map_file);
    }
    for asset in &mut bundle.assets {
        if let Some(source) = updates.remove(&asset.file_name) {
            asset.source = source;
            asset.encoding = "base64";
        }
    }
    report
        .tracked_features
        .insert(target.into(), output.tracked_features);
    Ok(())
}
