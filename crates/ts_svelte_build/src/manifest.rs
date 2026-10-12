//! Final framework modules are derived from emitted chunks, never guessed names.
use crate::*;
use ts_svelte_kit::{
    build_analysis::StaticValue,
    build_dependencies::{
        ClientBootstrapRequest, DependencyFont, NodeDependencyRequest, PreparedDependencyGraph,
    },
    build_manifest::RouterResolution,
    build_nodes::{self, ClientEntry, PageOptions, ResolvedServerNode, ResolvedServerNodesInput},
    build_ssr_manifest as ssr,
};

fn static_json(value: &StaticValue) -> Option<Value> {
    match value {
        StaticValue::Json(value) => Some(value.clone()),
        StaticValue::Object(values) => values
            .iter()
            .map(|(key, value)| Some((key.clone(), static_json(value)?)))
            .collect::<Option<serde_json::Map<_, _>>>()
            .map(Value::Object),
        StaticValue::Array(values) => values
            .iter()
            .map(static_json)
            .collect::<Option<Vec<_>>>()
            .map(Value::Array),
        _ => None,
    }
}
/// Kit 2's node option cache merges ancestor -> server -> universal. The load
/// null marker is upstream metadata, never an executable replacement handler.
fn node_options(
    index: usize,
    request: &BuildRequest,
    analysis: &AnalysisOutput,
    active: &mut BTreeSet<usize>,
) -> Result<PageOptions, String> {
    if !active.insert(index) {
        return Err("Cycle in node parent inventory".into());
    }
    let node = request
        .kit
        .inventory
        .nodes
        .get(index)
        .ok_or("Invalid parent node index")?;
    let mut values = if let Some(parent) = node.parent {
        match node_options(parent, request, analysis, active)? {
            PageOptions::Known(values) => values,
            _ => {
                active.remove(&index);
                return Ok(PageOptions::Unknown);
            }
        }
    } else {
        BTreeMap::new()
    };
    for file in [&node.server, &node.universal].into_iter().flatten() {
        let exports = analysis
            .modules
            .get(&path(file)?)
            .ok_or_else(|| format!("Missing native module analysis for {}", file.display()))?;
        for name in [
            "ssr",
            "csr",
            "prerender",
            "trailingSlash",
            "config",
            "entries",
            "load",
        ] {
            if let Some(value) = exports.get(name) {
                if matches!(value, StaticValue::Undefined) {
                    continue;
                }
                let value = if name == "load" && matches!(value, StaticValue::Function) {
                    Some(Value::Null)
                } else {
                    static_json(value)
                };
                let Some(value) = value else {
                    active.remove(&index);
                    return Ok(PageOptions::Unknown);
                };
                values.insert(name.into(), value);
            }
        }
    }
    active.remove(&index);
    Ok(PageOptions::Known(values))
}
fn dependencies_for(
    graph: &PreparedDependencyGraph<'_>,
    files: impl Iterator<Item = String>,
) -> Result<Vec<String>, String> {
    let mut result = vec![];
    for file in files {
        for asset in graph.find_dependencies(&file, true)?.assets {
            if !result.contains(&asset) {
                result.push(asset);
            }
        }
    }
    Ok(result)
}
fn mime(file: &str) -> Option<String> {
    Some(
        match file.rsplit('.').next()? {
            "css" => "text/css",
            "js" | "mjs" => "text/javascript",
            "json" | "map" => "application/json",
            "svg" => "image/svg+xml",
            "png" => "image/png",
            "jpg" | "jpeg" => "image/jpeg",
            "webp" => "image/webp",
            "gif" => "image/gif",
            "ico" => "image/vnd.microsoft.icon",
            "woff" => "font/woff",
            "woff2" => "font/woff2",
            "ttf" => "font/ttf",
            "otf" => "font/otf",
            "wasm" => "application/wasm",
            _ => return None,
        }
        .into(),
    )
}
pub(crate) fn generate(
    request: &BuildRequest,
    plan: &NativeBuildPlan,
    analysis: &AnalysisOutput,
    server: &BundleResult,
    client: Option<&BundleResult>,
    artifacts: &mut BTreeMap<String, Artifact>,
) -> Result<String, String> {
    generate_into(
        request,
        plan,
        analysis,
        server,
        client,
        artifacts,
        "manifest.js",
    )
}

fn generate_into(
    request: &BuildRequest,
    plan: &NativeBuildPlan,
    analysis: &AnalysisOutput,
    server: &BundleResult,
    client: Option<&BundleResult>,
    artifacts: &mut BTreeMap<String, Artifact>,
    manifest_file: &str,
) -> Result<String, String> {
    let inventory = &request.kit.inventory;
    let server_records = graph::dependencies(request, server)?;
    let client_records = client
        .map(|bundle| graph::dependencies(request, bundle))
        .transpose()?;
    let server_graph = PreparedDependencyGraph::new(&server_records)?;
    let client_graph = client_records
        .as_ref()
        .map(PreparedDependencyGraph::new)
        .transpose()?;
    let options = (0..inventory.nodes.len())
        .map(|index| node_options(index, request, analysis, &mut BTreeSet::new()))
        .collect::<Result<Vec<_>, _>>()?;
    let mut nodes = vec![];
    let mut manifest_nodes = vec![];
    let mut client_files = vec![];
    let mut client_css = vec![];
    for (index, node) in inventory.nodes.iter().enumerate() {
        let client_entry =
            if client.is_some() && (node.component.is_some() || node.universal.is_some()) {
                Some(
                    plan.client
                        .entries
                        .get(&format!("nodes/{index}"))
                        .ok_or_else(|| format!("Missing optimized client node {index}"))?
                        .clone(),
                )
            } else {
                None
            };
        let dependency = server_graph.node_dependencies(
            client_graph.as_ref(),
            &NodeDependencyRequest {
                component: node.component.as_deref().map(path).transpose()?,
                universal: node.universal.as_deref().map(path).transpose()?,
                server: node.server.as_deref().map(path).transpose()?,
                client_entry,
                strategy: BundleStrategy::Split,
            },
        )?;
        let client_node = dependency
            .client
            .as_ref()
            .map(|entry| {
                let dependencies = entry
                    .dependencies
                    .as_ref()
                    .ok_or("Missing optimized client dependency metadata")?;
                Ok::<_, String>(ClientEntry {
                    source_id: entry.source_id.clone(),
                    dependencies: dependencies.server_node_assets(),
                })
            })
            .transpose()?;
        client_files.push(dependency.client.as_ref().map(|entry| entry.file.clone()));
        client_css.push(
            dependency
                .client
                .as_ref()
                .and_then(|entry| entry.dependencies.as_ref())
                .map(|entry| entry.stylesheets.clone()),
        );
        nodes.push(ResolvedServerNode {
            component: dependency
                .component
                .as_ref()
                .map(|module| module.server_node_module()),
            universal: dependency
                .universal
                .as_ref()
                .map(|module| module.server_node_module()),
            server: dependency
                .server
                .as_ref()
                .map(|module| module.server_node_module()),
            page_options: options[index].clone(),
            child_page_options: if node.kind == ts_svelte_kit::build_inventory::NodeKind::Layout {
                Some(
                    node.child_pages
                        .iter()
                        .map(|index| {
                            options
                                .get(*index)
                                .cloned()
                                .ok_or("Invalid child page index".to_string())
                        })
                        .collect::<Result<_, _>>()?,
                )
            } else {
                None
            },
            client: client_node,
            resolved_inline_css: BTreeMap::new(),
        });
        manifest_nodes.push(ssr::ServerNode {
            assets: dependencies_for(
                &server_graph,
                [&node.universal, &node.server]
                    .into_iter()
                    .flatten()
                    .map(|file| path(file))
                    .collect::<Result<Vec<_>, _>>()?
                    .into_iter(),
            )?,
        });
    }
    let files = build_nodes::generate_server_nodes(&ResolvedServerNodesInput {
        kit_version: inventory.kit_version.clone(),
        router_type: inventory.options.router.router_type,
        bundle_strategy: BundleStrategy::Split,
        nodes,
        client_manifest_present: client.is_some(),
        client_font_sources: BTreeMap::new(),
        inline_style_threshold: 0,
        rewrite_inline_css_urls: false,
        css_assets: vec![],
    })?;
    let mut pending = BTreeMap::new();
    for (file, code) in files {
        artifacts::insert(
            &mut pending,
            format!("server/{}", path(&file)?),
            code.as_bytes(),
            "serverNode",
        )?;
    }
    let client_metadata = if let Some(graph) = &client_graph {
        let bootstrap = graph.client_bootstrap(&ClientBootstrapRequest {
            strategy: BundleStrategy::Split,
            start: plan
                .client
                .entries
                .get("entry/start")
                .ok_or("Missing client start entry")?
                .clone(),
            app: Some(
                plan.client
                    .entries
                    .get("entry/app")
                    .ok_or("Missing client app entry")?
                    .clone(),
            ),
            payload: plan.client.entries.get("entry/payload").cloned(),
            has_explicit_dynamic_public: request.kit.environment.definition.as_ref().is_some_and(
                |definition| {
                    definition
                        .variables
                        .values()
                        .any(|variable| variable.public && !variable.is_static)
                },
            ),
            explicit_public_module: Some(path(
                &request
                    .kit
                    .environment
                    .environment_directory
                    .join("public/client.js"),
            )?),
        })?;
        let server_resolution = inventory.options.router.resolution == RouterResolution::Server;
        let routes = if server_resolution {
            Some(
                inventory
                    .routes
                    .iter()
                    .filter_map(|route| route.page.as_ref().map(|page| (route, page)))
                    .map(|(route, page)| {
                        let load = |index: usize| {
                            known(&analysis.nodes[index].has_server_load)
                                .map(|value| (value, index))
                                .ok_or("Unknown server load flag".to_string())
                        };
                        Ok(ssr::ClientResolvedRoute {
                            id: route.id.clone(),
                            errors: page.errors.clone(),
                            layouts: page
                                .layouts
                                .iter()
                                .map(|index| index.map(load).transpose())
                                .collect::<Result<_, _>>()?,
                            leaf: load(page.leaf)?,
                        })
                    })
                    .collect::<Result<Vec<_>, String>>()?,
            )
        } else {
            None
        };
        Some(ssr::ClientMetadata {
            start: bootstrap.start,
            app: bootstrap.app,
            imports: bootstrap.imports,
            nodes: server_resolution.then_some(client_files),
            css: server_resolution.then_some(client_css),
            routes,
            stylesheets: bootstrap.stylesheets,
            fonts: bootstrap
                .fonts
                .into_iter()
                .map(|font| match font {
                    DependencyFont::Kit2(file) => ssr::ClientFont::Kit2(file),
                    DependencyFont::Kit3 { file, filename } => {
                        ssr::ClientFont::Kit3 { file, filename }
                    }
                })
                .collect(),
            uses_env_dynamic_public: bootstrap.uses_env_dynamic_public,
            inline: None,
        })
    } else {
        None
    };
    let routes = inventory
        .routes
        .iter()
        .map(|route| {
            let endpoint = route
                .endpoint
                .as_ref()
                .map(|file| {
                    let dependency = server_graph.find_dependencies(&path(file)?, true)?;
                    Ok::<_, String>(ssr::Endpoint {
                        chunk: dependency.file,
                        assets: dependency.assets,
                    })
                })
                .transpose()?;
            // Prerendered routes remain included until actual prerender results exist.
            Ok(ssr::ServerRoute {
                id: route.id.clone(),
                page: route.page.clone(),
                endpoint,
                included: true,
            })
        })
        .collect::<Result<_, String>>()?;
    let hooks = [&inventory.hooks.server, &inventory.hooks.universal]
        .into_iter()
        .flatten()
        .map(|file| path(file))
        .collect::<Result<Vec<_>, _>>()?;
    let catalog = server
        .assets
        .iter()
        .map(|asset| {
            Ok((
                asset.file_name.clone(),
                ssr::ServerAsset {
                    size: asset_bytes(asset)?.len() as u64,
                    mime_type: mime(&asset.file_name),
                },
            ))
        })
        .collect::<Result<_, String>>()?;
    let manifest = ssr::generate_ssr_manifest(&ssr::ResolvedSsrManifestInput {
        kit_version: inventory.kit_version.clone(),
        app_dir: request.kit.environment.app_dir.clone(),
        app_path: format!(
            "{}/{}",
            request.kit.environment.base.trim_start_matches('/'),
            request.kit.environment.app_dir
        )
        .trim_start_matches('/')
        .into(),
        relative_path: ".".into(),
        nodes: manifest_nodes,
        routes,
        assets: request
            .static_assets
            .iter()
            .map(|(file, asset)| ssr::StaticAsset {
                file: file.clone(),
                mime_type: asset.mime_type.clone(),
            })
            .collect(),
        service_worker: inventory
            .service_worker
            .as_ref()
            .map(|_| "service-worker.js".into()),
        client: client_metadata,
        remotes: remote_graph::manifest_chunks(server)?,
        prerendered_paths: vec![],
        matchers: inventory.matchers.keys().cloned().collect(),
        params_module_present: inventory.params_module.is_some(),
        server_assets: catalog,
        hook_assets: dependencies_for(&server_graph, hooks.into_iter())?,
        mime_lookup: BTreeMap::new(),
    })?;
    artifacts::insert(
        &mut pending,
        format!("server/{manifest_file}"),
        manifest.module.as_bytes(),
        "serverManifest",
    )?;
    // The output is ESM regardless of the application's package type.
    artifacts::insert(
        &mut pending,
        "server/package.json".into(),
        b"{\"type\":\"module\"}\n",
        "package",
    )?;
    for (name, artifact) in pending {
        artifacts::insert(
            artifacts,
            name,
            &artifacts::bytes(&artifact)?,
            &artifact.kind,
        )?;
    }
    Ok(manifest.expression)
}
/// A disposable analysis-stage server snapshot. These artifacts must not be
/// committed into final build output: nodes and manifest metadata are regenerated
/// after the optimized client completes. `generated_files` separates the native
/// framework modules from exact copies of the supplied emitted server artifacts.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreliminaryManifest {
    pub manifest_path: String,
    pub expression: String,
    pub server_build_id: String,
    pub artifacts: BTreeMap<String, Artifact>,
    pub generated_files: BTreeSet<String>,
}

/// Match Kit's first build_server_nodes(..., client_manifest=null) and
/// manifest-full.js phase. Call after server feature tracking and Kit3 interim
/// placeholder finalization, before evaluating metadata or building the client.
/// Unknown static page options retain actual universal imports; they are never
/// replaced with invented values. The caller's final artifact map is not accepted
/// by this API, so even a later generation error cannot overwrite final output.
pub(crate) fn preliminary(
    request: &BuildRequest,
    plan: &NativeBuildPlan,
    analysis: &AnalysisOutput,
    server: &BundleResult,
) -> Result<PreliminaryManifest, String> {
    if server.target != ts_svelte_bundle::Target::Server {
        return Err("Preliminary metadata requires the actual server bundle".into());
    }
    if !request.kit.config.remote_modules.is_empty()
        && remote_graph::manifest_chunks(server)?.is_empty()
    {
        return Err("Preliminary manifest requires emitted remote namespace entries; remote modules cannot be omitted".into());
    }
    if analysis.nodes.len() != request.kit.inventory.nodes.len() {
        return Err("Preliminary manifest analysis does not match the node inventory".into());
    }
    let mut generated = BTreeMap::new();
    let expression = generate_into(
        request,
        plan,
        analysis,
        server,
        None,
        &mut generated,
        "manifest-full.js",
    )?;
    let generated_files = generated.keys().cloned().collect();
    let mut artifacts = BTreeMap::new();
    for chunk in &server.chunks {
        artifacts::insert(
            &mut artifacts,
            format!("server/{}", chunk.file_name),
            chunk.code.as_bytes(),
            "server",
        )?;
    }
    for asset in &server.assets {
        let is_map = server
            .chunks
            .iter()
            .any(|chunk| chunk.sourcemap_file_name.as_deref() == Some(asset.file_name.as_str()));
        artifacts::insert(
            &mut artifacts,
            format!("server/{}", asset.file_name),
            &asset_bytes(asset)?,
            if is_map { "source-map" } else { "server-asset" },
        )?;
    }
    for (file, artifact) in generated {
        // Unlike ordinary idempotent artifact insertion, a pre-existing emitted
        // file cannot acquire framework ownership even if its bytes coincide.
        if artifacts.contains_key(&file) {
            return Err(format!(
                "Preliminary framework artifact collides with emitted server file {file}"
            ));
        }
        artifacts.insert(file, artifact);
    }
    Ok(PreliminaryManifest {
        manifest_path: "server/manifest-full.js".into(),
        expression,
        server_build_id: sha(&serde_json::to_vec(server).map_err(|error| error.to_string())?),
        artifacts,
        generated_files,
    })
}

#[cfg(test)]
#[path = "preliminary_manifest_tests.rs"]
mod preliminary_tests;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn functions_are_not_json_placeholders() {
        assert!(static_json(&StaticValue::Function).is_none());
        assert!(static_json(&StaticValue::Array(vec![StaticValue::Undefined])).is_none());
        assert_eq!(
            static_json(&StaticValue::Object(BTreeMap::from([(
                "csr".into(),
                StaticValue::Json(Value::Bool(false))
            )]))),
            Some(json!({"csr":false}))
        );
    }
}
