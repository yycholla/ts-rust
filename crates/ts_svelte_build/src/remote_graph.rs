//! Framework plugin construction and actual emitted remote facade identities.
use crate::*;
use ts_svelte_kit::{build_remote::RemoteTarget, build_ssr_manifest::RemoteChunk};
use ts_svelte_kit_bundle::{
    EmittedFacade, EmittedRemote, RemotePlugin, RemotePluginOptions, RemoteState,
};

pub(crate) fn plugin(
    request: &BuildRequest,
    client: bool,
    metadata: Option<&runtime_remotes::RuntimeRemotes>,
) -> Result<RemotePlugin, String> {
    RemotePlugin::new(RemotePluginOptions {
        kit_version: request.kit.inventory.kit_version.clone(),
        project_root: path(&request.kit.inventory.project_root)?,
        target: if client {
            RemoteTarget::Client
        } else {
            RemoteTarget::Server
        },
        enabled: request.kit.config.remote_functions_enabled,
        development: false,
        sourcemap: request.compiler.sourcemap,
        module_extensions: request.module_extensions.clone(),
        package_peers: request.remote_package_peers.clone(),
        seeds: if client {
            vec![]
        } else {
            request
                .kit
                .config
                .remote_modules
                .iter()
                .map(|p| path(p))
                .collect::<Result<_, _>>()?
        },
        client_runtime_id: Some(path(
            &request
                .kit
                .inventory
                .runtime_directory
                .join("client/remote-functions/index.js"),
        )?),
        metadata: metadata
            .map(|m| {
                m.evaluated
                    .iter()
                    .map(|e| (e.hash.clone(), e.clone()))
                    .collect()
            })
            .unwrap_or_default(),
        typescript: serde_json::from_value(request.compiler.typescript.clone())
            .map_err(|e| format!("Remote TypeScript options: {e}"))?,
    })
    .map_err(|e| e.to_string())
}

pub(crate) fn emitted(
    state: &RemoteState,
    bundle: &BundleResult,
) -> Result<Vec<EmittedRemote>, String> {
    state
        .emitted_chunks(
            &bundle
                .chunks
                .iter()
                .map(|c| EmittedFacade {
                    facade_module_id: c.facade_module_id.clone(),
                    file_name: c.file_name.clone(),
                    exports: c.exports.clone(),
                })
                .collect::<Vec<_>>(),
        )
        .map_err(|e| e.to_string())
}

/// The plugin has checked each source identity and emitted default namespace.
/// Manifest generation also verifies unique hashes and real default facades,
/// including when called independently with a previously emitted bundle.
pub(crate) fn manifest_chunks(bundle: &BundleResult) -> Result<Vec<RemoteChunk>, String> {
    let mut entries = BTreeMap::new();
    for chunk in &bundle.chunks {
        let Some(hash) = chunk
            .facade_module_id
            .as_deref()
            .and_then(|id| id.strip_prefix("\0sveltekit-remote:"))
        else {
            continue;
        };
        if hash.is_empty()
            || !hash
                .bytes()
                .all(|b| b.is_ascii_digit() || b.is_ascii_lowercase())
        {
            return Err("Invalid emitted remote facade hash".into());
        }
        if !chunk.exports.iter().any(|e| e == "default") {
            return Err(format!(
                "Emitted remote {hash} has no namespace default export"
            ));
        }
        if entries
            .insert(hash.to_owned(), chunk.file_name.clone())
            .is_some()
        {
            return Err(format!("Duplicate emitted remote facade {hash}"));
        }
    }
    Ok(entries
        .into_iter()
        .map(|(hash, chunk)| RemoteChunk { hash, chunk })
        .collect())
}
