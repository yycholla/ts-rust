//! Receipt-owning bridge between emitted bundles and Kit's manifest finalizer.
//! Run feature tracking before constructing a phase. No mutable bundle access is
//! exposed: edits between passes would invalidate the finalizer's source receipts.
use crate::{Artifact, artifacts, asset_bytes};
use base64::Engine as _;
use std::collections::BTreeMap;
use ts_svelte_bundle::BundleResult;
use ts_svelte_kit::{
    build_finalize::{self as kit, JavaScriptArtifact, ManifestValues, ResolvedManifestMetadata},
    sync_versions::KitSyncVersion,
};

/// Derive finalizer input from the actual emitted client inventory. The boolean
/// distinguishes a completed empty client from one that has not been built.
pub(crate) fn metadata(
    request: &crate::BuildRequest,
    client: Option<&BundleResult>,
    client_complete: bool,
    prerendered_paths: Option<Vec<String>>,
) -> Result<ResolvedManifestMetadata, String> {
    if client.is_some() && !client_complete {
        return Err("Client output cannot be supplied as an unresolved inventory".into());
    }
    let client_manifest = if let Some(client) = client {
        // Kit 2 does not consume this protocol. Avoid imposing additional Kit 3
        // inventory requirements on the explicit legacy no-op path.
        if KitSyncVersion::parse(&request.kit.inventory.kit_version)?.is_kit3() {
            Some(
                crate::service_worker::derive_manifest(request, client)?
                    .into_iter()
                    .map(|entry| kit::EmittedManifestRecord {
                        file: entry.file,
                        css: entry.css,
                        assets: entry.assets,
                    })
                    .collect(),
            )
        } else {
            Some(vec![])
        }
    } else {
        client_complete.then(Vec::new)
    };
    let assets = request
        .service_worker_options
        .static_asset_order
        .clone()
        .unwrap_or_else(|| request.static_assets.keys().cloned().collect());
    if assets.len() != request.static_assets.len()
        || assets.iter().collect::<std::collections::BTreeSet<_>>()
            != request
                .static_assets
                .keys()
                .collect::<std::collections::BTreeSet<_>>()
    {
        return Err("Static asset order must contain every supplied asset exactly once".into());
    }
    Ok(ResolvedManifestMetadata {
        app_dir: request.kit.environment.app_dir.clone(),
        base: request.kit.environment.base.clone(),
        client_manifest,
        inlined_files: Default::default(),
        assets,
        routes: request
            .kit
            .inventory
            .routes
            .iter()
            .filter(|route| route.page.is_some() || route.endpoint.is_some())
            .map(|route| kit::ManifestRoute {
                id: route.id.clone(),
                page: route.page.is_some(),
                endpoint: route.endpoint.is_some(),
            })
            .collect(),
        prerendered_paths,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinalizationPhase {
    AwaitingClient,
    AwaitingPrerender,
    Final,
    Kit2Noop,
}

/// Own this value across server/client/prerender stages. Metadata is resolved
/// input, not inferred from filenames or application execution.
pub struct BundleFinalization {
    version: String,
    bundle: BundleResult,
    receipts: BTreeMap<String, JavaScriptArtifact>,
    values: ManifestValues,
    app_dir: String,
    base: String,
    phase: FinalizationPhase,
    published: BTreeMap<String, Vec<u8>>,
    published_prefix: Option<String>,
}

impl BundleFinalization {
    pub fn server(
        version: &str,
        bundle: BundleResult,
        metadata: &ResolvedManifestMetadata,
    ) -> Result<Self, String> {
        Self::begin(version, bundle, metadata, false)
    }

    pub fn client(
        version: &str,
        bundle: BundleResult,
        metadata: &ResolvedManifestMetadata,
    ) -> Result<Self, String> {
        Self::begin(version, bundle, metadata, true)
    }

    fn begin(
        version: &str,
        bundle: BundleResult,
        metadata: &ResolvedManifestMetadata,
        client: bool,
    ) -> Result<Self, String> {
        let kit3 = KitSyncVersion::parse(version)?.is_kit3();
        if kit3
            && (metadata.prerendered_paths.is_some()
                || metadata.client_manifest.is_some() != client)
        {
            return Err("Initial server/client metadata has incorrect phase completeness".into());
        }
        let published = snapshot(&bundle)?;
        let receipts = capture(&bundle)?;
        let values = kit::values_from_metadata(metadata)?;
        let mut state = Self {
            version: version.into(),
            bundle,
            receipts,
            values,
            app_dir: metadata.app_dir.clone(),
            base: metadata.base.clone(),
            phase: if !kit3 {
                FinalizationPhase::Kit2Noop
            } else if client {
                FinalizationPhase::AwaitingPrerender
            } else {
                FinalizationPhase::AwaitingClient
            },
            published,
            published_prefix: None,
        };
        if kit3 {
            let expected_counts = state
                .receipts
                .iter()
                .map(|(file, artifact)| {
                    Ok((file.clone(), kit::inspect_variable_counts(&artifact.code)?))
                })
                .collect::<Result<_, String>>()?;
            let output = kit::replace_variables(&kit::VariableReplacementInput {
                kit_version: version.into(),
                artifacts: state.receipts.clone(),
                values: state.values.clone(),
                expected_counts,
                source_maps: kit::SourceMapPolicy::Preserve,
            })?;
            state.apply(output)?;
        }
        Ok(state)
    }

    pub fn phase(&self) -> FinalizationPhase {
        self.phase
    }
    pub fn is_final(&self) -> bool {
        matches!(
            self.phase,
            FinalizationPhase::Final | FinalizationPhase::Kit2Noop
        )
    }
    pub fn bundle(&self) -> &BundleResult {
        &self.bundle
    }
    pub fn into_bundle(self) -> Result<BundleResult, String> {
        if !self.is_final() {
            return Err("Cannot release an unfinished manifest bundle".into());
        }
        Ok(self.bundle)
    }

    /// Retain emitted evidence in a failed/incomplete build report. This aborts
    /// finalization and discards receipts; the result must never be presented as
    /// a finalized deployable bundle or used to restart a variable pass.
    pub fn abort_into_bundle(self) -> BundleResult {
        self.bundle
    }

    fn metadata(&self, metadata: &ResolvedManifestMetadata) -> Result<ManifestValues, String> {
        let values = kit::values_from_metadata(metadata)?;
        if metadata.app_dir != self.app_dir
            || metadata.base != self.base
            || values.assets != self.values.assets
            || values.routes != self.values.routes
            || (self.values.immutable.is_some() && values.immutable != self.values.immutable)
        {
            return Err("Manifest metadata changed between finalization phases".into());
        }
        Ok(values)
    }

    /// Some([]) is a completed empty client, including an all-CSR-false build.
    /// None still means unknown and cannot advance this stage.
    pub fn resolve_immutable(&mut self, metadata: &ResolvedManifestMetadata) -> Result<(), String> {
        if self.phase == FinalizationPhase::Kit2Noop {
            return Ok(());
        }
        if self.phase != FinalizationPhase::AwaitingClient {
            return Err("Immutable resolution requires the server's pending client phase".into());
        }
        let values = self.metadata(metadata)?;
        if values.immutable.is_none() || values.prerendered.is_some() {
            return Err(
                "Immutable resolution requires a completed client and unknown prerender results"
                    .into(),
            );
        }
        self.sentinels(&values, false)?;
        self.values = values;
        self.phase = FinalizationPhase::AwaitingPrerender;
        Ok(())
    }

    /// The caller must supply actual completed prerender results. An explicitly
    /// empty list is appropriate only when no route requires runtime prerender.
    pub fn finish(&mut self, metadata: &ResolvedManifestMetadata) -> Result<(), String> {
        if self.phase == FinalizationPhase::Kit2Noop {
            return Ok(());
        }
        if self.phase != FinalizationPhase::AwaitingPrerender {
            return Err("Finalization requires completed client resolution".into());
        }
        let values = self.metadata(metadata)?;
        if values.immutable.is_none() || values.prerendered.is_none() {
            return Err("Finalization requires completed client and prerender metadata".into());
        }
        self.sentinels(&values, true)?;
        self.values = values;
        self.phase = FinalizationPhase::Final;
        Ok(())
    }

    fn sentinels(&mut self, values: &ManifestValues, require_final: bool) -> Result<(), String> {
        let output = kit::replace_sentinels(&kit::SentinelReplacementInput {
            kit_version: self.version.clone(),
            artifacts: self.receipts.clone(),
            immutable: values.immutable.clone(),
            prerendered: values.prerendered.clone(),
            require_final,
            source_maps: kit::SourceMapPolicy::Preserve,
        })?;
        self.apply(output)
    }

    fn apply(&mut self, output: kit::ReplacementOutput) -> Result<(), String> {
        if !output.removed_map_files.is_empty() || !output.removed_mapping_urls.is_empty() {
            return Err("Manifest finalization unexpectedly removed source-map metadata".into());
        }
        if output.artifacts.keys().ne(self.receipts.keys()) {
            return Err("Finalizer changed the emitted chunk inventory".into());
        }
        // Validate/serialize every update before touching any emitted bytes.
        let mut maps = BTreeMap::new();
        for (file, map) in &output.updated_map_files {
            if self
                .bundle
                .assets
                .iter()
                .filter(|a| &a.file_name == file)
                .count()
                != 1
            {
                return Err(format!(
                    "Expected exactly one emitted source-map asset {file}"
                ));
            }
            maps.insert(
                file.clone(),
                base64::engine::general_purpose::STANDARD
                    .encode(serde_json::to_vec(map).map_err(|e| e.to_string())?),
            );
        }
        for chunk in &mut self.bundle.chunks {
            let next = &output.artifacts[&chunk.file_name];
            chunk.code.clone_from(&next.code);
            chunk.map.clone_from(&next.map);
            chunk.sourcemap_file_name.clone_from(&next.map_file);
        }
        for asset in &mut self.bundle.assets {
            if let Some(source) = maps.remove(&asset.file_name) {
                asset.source = source;
                asset.encoding = "base64";
            }
        }
        self.receipts = output.artifacts;
        Ok(())
    }

    /// Publish all current bundle bytes atomically. First publication accepts
    /// absent files or matching original/current bytes. Later publications must
    /// still contain our previously published bytes; unrelated output survives.
    /// A phase cannot be published under a different prefix subsequently.
    pub fn commit_artifacts(
        &mut self,
        destination: &mut BTreeMap<String, Artifact>,
        prefix: &str,
    ) -> Result<(), String> {
        artifacts::relative(prefix)?;
        if self
            .published_prefix
            .as_deref()
            .is_some_and(|old| old != prefix)
        {
            return Err("Cannot change the finalized bundle's artifact prefix".into());
        }
        let current = snapshot(&self.bundle)?;
        let mut next = destination.clone();
        for (file, bytes) in &current {
            let name = format!("{prefix}/{file}");
            let kind = if let Some(previous) = destination.get(&name) {
                let previous_bytes = artifacts::bytes(previous)?;
                if previous_bytes != self.published[file]
                    && !(self.published_prefix.is_none() && &previous_bytes == bytes)
                {
                    return Err(format!("Stale or conflicting manifest artifact {name}"));
                }
                previous.kind.clone()
            } else {
                if self.published_prefix.is_some() {
                    return Err(format!(
                        "Previously published manifest artifact is missing: {name}"
                    ));
                }
                if self.receipts.contains_key(file) {
                    "javascript"
                } else {
                    "asset"
                }
                .into()
            };
            next.remove(&name);
            artifacts::insert(&mut next, name, bytes, &kind)?;
        }
        *destination = next;
        self.published = current;
        self.published_prefix = Some(prefix.into());
        Ok(())
    }
}

fn snapshot(bundle: &BundleResult) -> Result<BTreeMap<String, Vec<u8>>, String> {
    let mut files = BTreeMap::new();
    for item in bundle
        .chunks
        .iter()
        .map(|c| Ok((&c.file_name, c.code.as_bytes().to_vec())))
        .chain(
            bundle
                .assets
                .iter()
                .map(|a| Ok::<_, String>((&a.file_name, asset_bytes(a)?))),
        )
    {
        let (name, bytes) = item?;
        artifacts::relative(name)?;
        if files.insert(name.clone(), bytes).is_some() {
            return Err(format!("Duplicate emitted file {name}"));
        }
    }
    Ok(files)
}

fn capture(bundle: &BundleResult) -> Result<BTreeMap<String, JavaScriptArtifact>, String> {
    let files = snapshot(bundle)?;
    let mut result = BTreeMap::new();
    let mut maps = std::collections::BTreeSet::new();
    for chunk in &bundle.chunks {
        if let Some(file) = &chunk.sourcemap_file_name {
            if !maps.insert(file) || chunk.map.is_none() {
                return Err(format!("Invalid or shared source-map ownership for {file}"));
            }
            let map: serde_json::Value = serde_json::from_slice(
                files
                    .get(file)
                    .ok_or_else(|| format!("Missing source-map asset {file}"))?,
            )
            .map_err(|e| format!("Invalid source-map asset {file}: {e}"))?;
            if Some(&map) != chunk.map.as_ref() {
                return Err(format!(
                    "Chunk map disagrees with emitted source-map asset {file}"
                ));
            }
        }
        result.insert(
            chunk.file_name.clone(),
            JavaScriptArtifact {
                code: chunk.code.clone(),
                map: chunk.map.clone(),
                map_file: chunk.sourcemap_file_name.clone(),
                pending: vec![],
                receipt_sha256: None,
            },
        );
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    use ts_svelte_bundle::{Asset, Chunk, Target};

    fn bundle(file: &str) -> BundleResult {
        let code = "const unicode = '😀'; export const files = __SVELTEKIT_MANIFEST_IMMUTABLE__; export const pages = __SVELTEKIT_MANIFEST_PRERENDERED__; export const assets = __SVELTEKIT_MANIFEST_ASSETS__; export const routes = __SVELTEKIT_MANIFEST_ROUTES__; export const userString = '__sveltekit_manifest_build__';\n";
        let map_file = format!("{file}.map");
        let map = json!({"version":3,"sources":["original.ts"],"sourcesContent":[code],"names":[],"mappings":"AAAA"});
        BundleResult {
            rolldown_version: "1.2.13",
            target: Target::Server,
            css_mode: "native",
            css: Default::default(),
            module_graph: BTreeMap::new(),
            diagnostics: vec![],
            compiler_responses: BTreeMap::new(),
            chunks: vec![Chunk {
                referenced_files: vec![],
                file_name: file.into(),
                name: "manifest".into(),
                code: format!(
                    "{code}//# sourceMappingURL={}\n",
                    map_file.rsplit('/').next().unwrap()
                ),
                is_entry: true,
                is_dynamic_entry: false,
                facade_module_id: Some("/app/generated/app-manifest.js".into()),
                module_ids: vec!["/app/generated/app-manifest.js".into()],
                imports: vec![],
                dynamic_imports: vec![],
                exports: vec!["files".into()],
                map: Some(map.clone()),
                sourcemap_file_name: Some(map_file.clone()),
            }],
            assets: vec![Asset {
                file_name: map_file,
                names: vec![],
                original_file_names: vec![],
                encoding: "base64",
                source: base64::engine::general_purpose::STANDARD
                    .encode(serde_json::to_vec(&map).unwrap()),
            }],
        }
    }
    fn metadata() -> ResolvedManifestMetadata {
        ResolvedManifestMetadata {
            app_dir: "_app".into(),
            base: "/base".into(),
            client_manifest: None,
            inlined_files: Default::default(),
            assets: vec!["favicon.svg".into()],
            routes: vec![kit::ManifestRoute {
                id: "/".into(),
                page: true,
                endpoint: false,
            }],
            prerendered_paths: None,
        }
    }
    fn client_metadata() -> ResolvedManifestMetadata {
        let mut metadata = metadata();
        metadata.client_manifest = Some(vec![kit::EmittedManifestRecord {
            file: "_app/immutable/entry/start.hash.js".into(),
            css: vec!["_app/immutable/style.hash.css".into()],
            assets: vec![],
        }]);
        metadata
    }
    fn assert_maps(state: &BundleFinalization, files: &BTreeMap<String, Artifact>, prefix: &str) {
        let chunk = &state.bundle().chunks[0];
        let file = chunk.sourcemap_file_name.as_ref().unwrap();
        let asset = state
            .bundle()
            .assets
            .iter()
            .find(|a| &a.file_name == file)
            .unwrap();
        let map: Value = serde_json::from_slice(&asset_bytes(asset).unwrap()).unwrap();
        assert_eq!(Some(&map), chunk.map.as_ref());
        assert_eq!(
            artifacts::bytes(&files[&format!("{prefix}/{file}")]).unwrap(),
            asset_bytes(asset).unwrap()
        );
        assert_eq!(
            artifacts::bytes(&files[&format!("{prefix}/{}", chunk.file_name)]).unwrap(),
            chunk.code.as_bytes()
        );
        assert_eq!(map["sources"], json!(["original.ts"]));
    }

    #[test]
    fn server_and_client_receipts_maps_and_publications_advance_together() {
        let original_map = bundle("chunks/manifest.js").chunks.remove(0).map.unwrap();
        let mut server =
            BundleFinalization::server("3.0.1", bundle("chunks/manifest.js"), &metadata()).unwrap();
        let mut files = BTreeMap::new();
        server.commit_artifacts(&mut files, "server").unwrap();
        assert_maps(&server, &files, "server");
        assert_ne!(server.bundle().chunks[0].map.as_ref(), Some(&original_map));
        assert!(!server.is_final());
        let completed_client = client_metadata();
        let mut client =
            BundleFinalization::client("3.0.1", bundle("_app/manifest.js"), &completed_client)
                .unwrap();
        client.commit_artifacts(&mut files, "client").unwrap();
        server.resolve_immutable(&completed_client).unwrap();
        server.commit_artifacts(&mut files, "server").unwrap();
        assert_maps(&server, &files, "server");
        assert!(server.bundle().chunks[0].code.contains("start.hash.js"));
        assert!(
            server
                .finish(&completed_client)
                .unwrap_err()
                .contains("completed client and prerender")
        );
        assert_eq!(server.phase(), FinalizationPhase::AwaitingPrerender);
        let mut final_metadata = completed_client;
        final_metadata.prerendered_paths = Some(vec!["/base/ready".into()]);
        for (state, prefix) in [(&mut server, "server"), (&mut client, "client")] {
            state.finish(&final_metadata).unwrap();
            state.commit_artifacts(&mut files, prefix).unwrap();
            assert!(state.is_final());
            assert!(
                state
                    .receipts
                    .values()
                    .all(|a| a.pending.is_empty() && a.receipt_sha256.is_none())
            );
            assert_maps(state, &files, prefix);
            let code = &state.bundle().chunks[0].code;
            assert!(code.contains("\"path\":\"ready\""));
            assert!(code.contains("'__sveltekit_manifest_build__'"));
            assert!(!code.contains("__sveltekit_manifest_prerendered__"));
            assert_eq!(
                state.bundle().chunks[0].file_name,
                if prefix == "server" {
                    "chunks/manifest.js"
                } else {
                    "_app/manifest.js"
                }
            );
        }
        assert!(server.into_bundle().is_ok());
    }

    #[test]
    fn completed_empty_client_is_distinct_from_unknown() {
        let mut state =
            BundleFinalization::server("3.0.0-next.32", bundle("index.js"), &metadata()).unwrap();
        assert!(state.resolve_immutable(&metadata()).is_err());
        let mut empty = metadata();
        empty.client_manifest = Some(vec![]);
        state.resolve_immutable(&empty).unwrap();
        assert!(!state.is_final());
        empty.prerendered_paths = Some(vec![]);
        state.finish(&empty).unwrap();
        assert!(state.bundle().chunks[0].code.contains("files = []"));
        assert!(state.bundle().chunks[0].code.contains("pages = []"));
    }

    #[test]
    fn changed_metadata_and_stale_publication_fail_atomically() {
        let mut state =
            BundleFinalization::server("3.0.1", bundle("index.js"), &metadata()).unwrap();
        let before = serde_json::to_value(state.bundle()).unwrap();
        let mut changed = client_metadata();
        changed.assets.push("other.svg".into());
        assert!(state.resolve_immutable(&changed).is_err());
        assert_eq!(serde_json::to_value(state.bundle()).unwrap(), before);
        let mut files = BTreeMap::new();
        state.commit_artifacts(&mut files, "server").unwrap();
        state.resolve_immutable(&client_metadata()).unwrap();
        files.get_mut("server/index.js.map").unwrap().source =
            base64::engine::general_purpose::STANDARD.encode(b"unrelated");
        let before = serde_json::to_value(&files).unwrap();
        assert!(state.commit_artifacts(&mut files, "server").is_err());
        assert_eq!(serde_json::to_value(&files).unwrap(), before);
        assert!(state.commit_artifacts(&mut files, "client").is_err());
    }

    #[test]
    fn missing_or_disagreeing_emitted_maps_are_rejected() {
        let mut missing = bundle("index.js");
        missing.assets.clear();
        assert!(BundleFinalization::server("3.0.1", missing, &metadata()).is_err());
        let mut mismatch = bundle("index.js");
        mismatch.chunks[0].map.as_mut().unwrap()["mappings"] = json!("");
        assert!(BundleFinalization::server("3.0.1", mismatch, &metadata()).is_err());
    }

    #[test]
    fn kit2_is_an_explicit_unchanged_noop() {
        let input = bundle("index.js");
        let before = serde_json::to_value(&input).unwrap();
        let mut state = BundleFinalization::server("2.69.2", input, &metadata()).unwrap();
        assert_eq!(state.phase(), FinalizationPhase::Kit2Noop);
        state.finish(&metadata()).unwrap();
        assert_eq!(
            serde_json::to_value(state.into_bundle().unwrap()).unwrap(),
            before
        );
    }
}
