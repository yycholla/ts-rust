//! Native, server-first SvelteKit phase orchestration. Compiler, bundler and Kit
//! generators remain separate crates. Configuration modules are never executed.
//! Application metadata execution requires an explicit runtime evaluator. `build` returns artifacts, metadata and unresolved stages;
//! `write_new` is a separate explicit operation that refuses existing outputs.
pub mod adapter;
mod analysis;
mod artifacts;
#[cfg(test)]
mod execution_runtime_tests;
pub mod execution_snapshot;
pub mod finalize;
mod graph;
mod guards;
pub mod instrumentation;
mod manifest;
pub mod native_runtime;
mod remote_graph;
#[cfg(test)]
mod remote_runtime_tests;
pub mod runtime_analysis;
pub mod runtime_remotes;
pub use manifest::PreliminaryManifest;
#[cfg(test)]
mod pipeline_tests;
pub mod prerender_output;
pub mod prerender_policy;
pub mod prerender_remote_entry;
#[cfg(test)]
mod prerender_runtime_tests;
pub mod prerender_transport;
pub mod project;
pub mod service_worker;
mod tracking;
pub mod worker_manifest;
pub use artifacts::write_new;
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};
use ts_svelte_bundle::{BundleRequest, BundleResult, CompilerOptions};
use ts_svelte_kit::{
    build_analysis::{
        self, AnalysisInput, AnalysisNode, AnalysisOutput, AnalysisPage, AnalysisRoute, Fact,
        ModuleSnapshot,
    },
    build_nodes::BundleStrategy,
    build_plan::{
        self, AnalyzedNodeLoads, NativeBuildPlan, ResolvedClientLoadAnalysis,
        ResolvedNativeBuildInput,
    },
};

pub const MAX_REQUEST_BYTES: usize = 96 * 1024 * 1024;
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BuildRequest {
    /// Explicit native stylesheet pipeline, independent of Svelte compilation.
    #[serde(default)]
    pub tailwind: Option<ts_svelte_bundle::css::TailwindOptions>,
    #[serde(default)]
    pub adapter_node: Option<adapter::AdapterRequest>,
    #[serde(default)]
    pub service_worker_options: service_worker::ServiceWorkerOptions,
    pub kit: ResolvedNativeBuildInput,
    pub compiler: CompilerSettings,
    /// Resolved Kit moduleExtensions, including leading dots. Required rather
    /// than inferred from the set of files that happened to be loaded.
    pub module_extensions: Vec<String>,
    /// Exact resolved dependency module ID -> inspected SvelteKit peer evidence.
    #[serde(default)]
    pub remote_package_peers: BTreeMap<String, bool>,
    /// Exact external IDs with host-established exposure evidence. Unspecified
    /// externals remain Unknown; node: builtins are always server-only.
    #[serde(default)]
    pub external_exposure: BTreeMap<String, ts_svelte_kit::build_guards::ExternalExposure>,
    /// Optional host resolver observations supplement native realpath probes.
    #[serde(default)]
    pub guard_real_paths: BTreeMap<PathBuf, PathBuf>,
    #[serde(default)]
    pub guard_resolutions: Vec<ts_svelte_kit::build_guards::GuardResolution>,
    /// Original application modules used by native metadata analysis. These
    /// exact sources also overlay the bundler filesystem in both phases.
    pub modules: BTreeMap<String, ModuleSnapshot>,
    /// Additional original component/Markdown snapshots. Generated sources must
    /// not be supplied here or allowed to replace planner-owned modules.
    #[serde(default)]
    pub sources: BTreeMap<PathBuf, String>,
    /// Vite server-build externalization for the Kit server graph (bare ids
    /// after aliasing). None keeps only `kit.config.serverExternals`.
    #[serde(default)]
    pub ssr_external: Option<ts_svelte_kit::vite_ssr::SsrExternalPolicy>,
    /// The application's `build.rollupOptions.treeshake` for the Kit graphs.
    #[serde(default)]
    pub treeshake: Option<ts_svelte_bundle::Treeshake>,
    /// `@sentry/sveltekit` Vite plugin module rewrites, when that plugin is used.
    #[serde(default)]
    pub sentry: Option<SentryBuildOptions>,
    /// User Vite `define` entries (identifier -> JavaScript expression source).
    /// Kit's own defines take precedence on the same key, as in Vite's merge.
    #[serde(default)]
    pub define: BTreeMap<String, String>,
    /// Explicit static asset bytes and MIME metadata; no directory scan occurs.
    #[serde(default)]
    pub static_assets: BTreeMap<String, StaticAsset>,
    /// The resolved caller inventory must include every static asset, including
    /// an explicitly empty directory. False prevents final manifest completion.
    pub static_assets_complete: bool,
}
/// `sentrySvelteKit()` options that change application modules (10.43.0).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SentryBuildOptions {
    pub load: bool,
    pub server_load: bool,
    /// Kit tracing is enabled: only the client graph is wrapped.
    pub only_instrument_client: bool,
    pub server_hooks: String,
    #[serde(default)]
    pub global_values: serde_json::Map<String, Value>,
}
impl SentryBuildOptions {
    /// The plugin's per-build `load` hook decisions.
    pub fn bundle_options(&self, server: bool) -> ts_svelte_bundle::sentry::SentryOptions {
        ts_svelte_bundle::sentry::SentryOptions {
            wrap_load: self.load && !(self.only_instrument_client && server),
            wrap_server_load: self.server_load && !self.only_instrument_client,
            server_hooks: self.server_hooks.clone(),
            global_values: self.global_values.clone(),
        }
    }
}
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CompilerSettings {
    pub svelte_version: String,
    pub typescript_version: Option<String>,
    #[serde(default)]
    pub allow_prerelease: bool,
    #[serde(default)]
    pub runes: Option<bool>,
    /// Client (and service worker) JavaScript and CSS minification.
    #[serde(default)]
    pub minify: bool,
    /// Server graph minification; Vite's server environment defaults to off.
    #[serde(default)]
    pub server_minify: bool,
    #[serde(default = "enabled")]
    pub sourcemap: bool,
    #[serde(default)]
    pub options: CompilerOptions,
    pub tsconfig: Option<PathBuf>,
    pub mdsvex: Option<Value>,
    #[serde(default = "object")]
    pub typescript: Value,
}
fn enabled() -> bool {
    true
}
fn object() -> Value {
    json!({})
}
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StaticAsset {
    pub source: String,
    pub mime_type: Option<String>,
}
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Artifact {
    pub encoding: String,
    pub source: String,
    pub kind: String,
}
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Requirement {
    pub stage: String,
    pub code: String,
    pub message: String,
}
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Phase {
    pub name: String,
    pub status: String,
    pub outputs: usize,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BuildReport {
    /// complete = all requested framework phases complete; no deployment or
    /// application runtime execution is implied. Selected unsupported adapters
    /// and requested prerender execution prevent this status.
    pub status: String,
    pub phases: Vec<Phase>,
    pub artifacts: BTreeMap<String, Artifact>,
    pub requirements: Vec<Requirement>,
    pub diagnostics: Vec<Value>,
    pub server_build_id: Option<String>,
    pub analysis: Option<AnalysisOutput>,
    pub runtime_snapshot: Option<manifest::PreliminaryManifest>,
    pub execution_snapshot: Option<execution_snapshot::ExecutionSnapshot>,
    pub resolved_entries: BTreeMap<String, runtime_analysis::RuntimeEntries>,
    pub remote_metadata: Option<runtime_remotes::RuntimeRemotes>,
    pub remote_prerender_entry: Option<prerender_remote_entry::EmittedRemotePrerenderEntry>,
    pub guards: BTreeMap<String, ts_svelte_kit::build_guards::BuildGuardReport>,
    pub tracked_features: BTreeMap<String, BTreeMap<String, Vec<String>>>,
    pub server: Option<BundleResult>,
    pub client: Option<BundleResult>,
    pub adapter: Option<adapter::AdapterOutput>,
    pub instrumentation: Option<instrumentation::ServerInstrumentation>,
    pub service_worker: Option<service_worker::ServiceWorkerOutput>,
    pub generated_sources: BTreeMap<PathBuf, String>,
    /// Requirements the caller explicitly accepted (`stage/code`); they do not
    /// block completion but stay reported.
    pub accepted_requirements: Vec<Requirement>,
}
impl BuildReport {
    /// Loader requirements block completion like the build's own, except those
    /// whose `stage/code` the caller accepted explicitly.
    pub fn add_loader_requirements(&mut self, requirements: Vec<Requirement>, accepted: &[String]) {
        let (accepted, blocking): (Vec<_>, Vec<_>) = requirements
            .into_iter()
            .partition(|r| accepted.contains(&format!("{}/{}", r.stage, r.code)));
        self.accepted_requirements.extend(accepted);
        self.add_requirements(blocking);
    }
    /// Loader requirements block completion like the build's own.
    pub fn add_requirements(&mut self, requirements: Vec<Requirement>) {
        if !requirements.is_empty() && self.status == "complete" {
            self.status = "incomplete".into();
        }
        self.requirements.splice(0..0, requirements);
    }
    fn new() -> Self {
        Self {
            status: "incomplete".into(),
            phases: vec![],
            artifacts: BTreeMap::new(),
            requirements: vec![],
            diagnostics: vec![],
            server_build_id: None,
            analysis: None,
            runtime_snapshot: None,
            execution_snapshot: None,
            resolved_entries: BTreeMap::new(),
            remote_metadata: None,
            remote_prerender_entry: None,
            guards: BTreeMap::new(),
            tracked_features: BTreeMap::new(),
            server: None,
            client: None,
            adapter: None,
            instrumentation: None,
            service_worker: None,
            generated_sources: BTreeMap::new(),
            accepted_requirements: vec![],
        }
    }
    fn requirement(&mut self, stage: &str, code: &str, message: impl Into<String>) {
        self.requirements.push(Requirement {
            stage: stage.into(),
            code: code.into(),
            message: message.into(),
        });
    }
    fn phase(&mut self, name: &str, status: &str, outputs: usize) {
        self.phases.push(Phase {
            name: name.into(),
            status: status.into(),
            outputs,
        });
    }
    fn fail(&mut self, stage: &str, message: impl Into<String>) {
        self.status = "failed".into();
        self.diagnostics.push(json!({"source":"native-kit-build","stage":stage,"severity":"error","message":message.into()}));
        self.phase(stage, "failed", 0);
    }
}
pub(crate) fn sha(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
pub(crate) fn path(path: &Path) -> Result<String, String> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| "Build paths must be UTF-8".into())
}
fn known<T: Clone>(fact: &Fact<T>) -> Option<T> {
    if let Fact::Known { value } = fact {
        Some(value.clone())
    } else {
        None
    }
}
pub(crate) fn asset_bytes(asset: &ts_svelte_bundle::Asset) -> Result<Vec<u8>, String> {
    if asset.encoding != "base64" {
        return Err("Unknown bundler asset encoding".into());
    }
    base64::engine::general_purpose::STANDARD
        .decode(&asset.source)
        .map_err(|e| e.to_string())
}
fn add_bundle(
    report: &mut BuildReport,
    bundle: &BundleResult,
    directory: &str,
) -> Result<(), String> {
    for chunk in &bundle.chunks {
        artifacts::insert(
            &mut report.artifacts,
            format!("{directory}/{}", chunk.file_name),
            chunk.code.as_bytes(),
            "javascript",
        )?;
    }
    for asset in &bundle.assets {
        artifacts::insert(
            &mut report.artifacts,
            format!("{directory}/{}", asset.file_name),
            &asset_bytes(asset)?,
            "asset",
        )?;
    }
    report
        .diagnostics
        .extend(bundle.diagnostics.iter().cloned());
    Ok(())
}
fn analysis_input(request: &BuildRequest) -> Result<AnalysisInput, String> {
    let inventory = &request.kit.inventory;
    for node in &inventory.nodes {
        for file in [&node.universal, &node.server].into_iter().flatten() {
            if !request.modules.contains_key(&path(file)?) {
                return Err(format!(
                    "Native analysis needs the actual source snapshot for {}",
                    file.display()
                ));
            }
        }
    }
    for route in &inventory.routes {
        if let Some(file) = &route.endpoint {
            if !request.modules.contains_key(&path(file)?) {
                return Err(format!(
                    "Native analysis needs endpoint source {}",
                    file.display()
                ));
            }
        }
    }
    Ok(AnalysisInput {
        kit_version: inventory.kit_version.clone(),
        modules: request.modules.clone(),
        nodes: inventory
            .nodes
            .iter()
            .map(|node| {
                Ok(AnalysisNode {
                    universal: node.universal.as_ref().map(|p| path(p)).transpose()?,
                    server: node.server.as_ref().map(|p| path(p)).transpose()?,
                })
            })
            .collect::<Result<_, String>>()?,
        routes: inventory
            .routes
            .iter()
            .map(|route| {
                Ok(AnalysisRoute {
                    id: route.id.clone(),
                    page: route.page.as_ref().map(|page| AnalysisPage {
                        layouts: page.layouts.clone(),
                        leaf: page.leaf,
                    }),
                    endpoint: route.endpoint.as_ref().map(|p| path(p)).transpose()?,
                })
            })
            .collect::<Result<_, String>>()?,
    })
}
fn unsupported(request: &BuildRequest, report: &mut BuildReport) -> Result<bool, String> {
    let kit = &request.kit;
    if let Some(adapter) = &request.adapter_node {
        let emitted_map_base = kit.inventory.project_root.join("dist");
        if adapter.server_map_base != emitted_map_base {
            return Err(format!(
                "adapterNode.serverMapBase must match the native emitter's map base {}",
                emitted_map_base.display()
            ));
        }
    }
    if kit.config.bundle_strategy != BundleStrategy::Split {
        report.requirement(
            "planning",
            "bundle_strategy",
            "This orchestrator currently implements split ESM output",
        );
    }
    if kit.config.instrumentation.is_some() && request.adapter_node.is_some() {
        instrumentation::validate_adapter_node_version(&kit.inventory.kit_version)?;
    }
    if kit.inventory.options.router.router_type == ts_svelte_kit::build_manifest::RouterType::Hash {
        report.requirement(
            "analysis",
            "hash_router_analysis",
            "Hash-router option validation requires its separate native analysis contract",
        );
    }
    if kit.client_analysis.is_some() {
        return Err("Caller clientAnalysis is not accepted: this pipeline derives it from its own server build and source snapshots".into());
    }
    Ok(report.requirements.iter().any(|requirement| {
        matches!(
            requirement.stage.as_str(),
            "planning" | "server" | "analysis"
        )
    }))
}
/// Run the supported native graph phases. Errors and incomplete requirements
/// retain completed artifacts. No files are written and no app code is executed.
pub async fn build(request: BuildRequest) -> BuildReport {
    build_inner(request, None).await
}
/// Execute the server's actual metadata in the supplied native embedding, then
/// continue client bundling and packaging from source-bound evaluated facts.
pub async fn build_with_runtime(
    request: BuildRequest,
    evaluator: &mut dyn native_runtime::MetadataEvaluator,
) -> BuildReport {
    build_inner(request, Some(evaluator)).await
}
async fn build_inner(
    request: BuildRequest,
    mut evaluator: Option<&mut dyn native_runtime::MetadataEvaluator>,
) -> BuildReport {
    let mut report = BuildReport::new();
    let initial = match build_plan::create_build_plan(&request.kit) {
        Ok(plan) => plan,
        Err(error) => {
            report.fail("planning", error);
            return report;
        }
    };
    report.generated_sources = initial.generated_files.clone();
    report.phase("planning", "complete", initial.generated_files.len());
    match unsupported(&request, &mut report) {
        Ok(true) => return report,
        Err(error) => {
            report.fail("planning", error);
            return report;
        }
        _ => {}
    }
    let mut analysis_request = match analysis_input(&request) {
        Ok(input) => input,
        Err(error) => {
            report.fail("analysis inputs", error);
            return report;
        }
    };
    let mut server_request = match graph::bundle_request(&request, &initial, false) {
        Ok(request) => request,
        Err(error) => {
            report.fail("server inputs", error);
            return report;
        }
    };
    let remote_prerender_entry = if request.kit.config.remote_functions_enabled {
        match prerender_remote_entry::RemotePrerenderEntry::from_inventory(&request.kit.inventory)
            .and_then(|entry| {
                entry.apply(&mut server_request)?;
                Ok(entry)
            }) {
            Ok(entry) => Some(entry),
            Err(error) => {
                report.fail("remote prerender helper inputs", error);
                return report;
            }
        }
    } else {
        None
    };
    let remote_plugin = match remote_graph::plugin(&request, false, None) {
        Ok(plugin) => plugin,
        Err(error) => {
            report.fail("server remote inputs", error);
            return report;
        }
    };
    let remote_state = remote_plugin.state();
    let mut server = match ts_svelte_bundle::generate_with_plugins(
        server_request.clone(),
        vec![remote_plugin.into_plugin()],
    )
    .await
    {
        Ok(bundle) => bundle,
        Err(error) => {
            report.diagnostics.extend(error.diagnostics);
            report.fail("server", error.message);
            return report;
        }
    };
    let remote_identities = match remote_graph::emitted(&remote_state, &server) {
        Ok(remotes) => remotes
            .into_iter()
            .map(|remote| ts_svelte_kit::build_remote::RemoteIdentity {
                hash: remote.hash,
                file: remote.file,
                module_id: remote.module_id,
            })
            .collect::<Vec<_>>(),
        Err(error) => {
            report.server = Some(server);
            report.fail("server remote entries", error);
            return report;
        }
    };
    if let Some(entry) = remote_prerender_entry {
        let observed = entry
            .required_modules()
            .filter(|path| !server_request.virtual_modules.contains_key(*path))
            .map(|path| {
                std::fs::canonicalize(path)
                    .map(|resolved| (path.to_path_buf(), resolved))
                    .map_err(|error| {
                        format!("Resolve prerender helper {}: {error}", path.display())
                    })
            })
            .collect::<Result<BTreeMap<_, _>, _>>();
        match observed.and_then(|paths| entry.emitted_with_real_paths(&server, &paths)) {
            Ok(emitted) => report.remote_prerender_entry = Some(emitted),
            Err(error) => {
                report.server = Some(server);
                report.fail("remote prerender helper output", error);
                return report;
            }
        }
    }
    if let Ok(diagnostics) = remote_state.diagnostics() {
        report.diagnostics.extend(diagnostics);
    }
    if let Err(error) = tracking::collect(
        &mut report,
        &mut server,
        &request.kit.inventory.kit_version,
        "server",
    ) {
        report.fail("server feature tracking", error);
        return report;
    }
    if let Err(error) = add_bundle(&mut report, &server, "server") {
        report.fail("server artifacts", error);
        return report;
    }
    let mut finalization_metadata = match finalize::metadata(&request, None, false, None) {
        Ok(metadata) => metadata,
        Err(error) => {
            report.server = Some(server);
            report.fail("manifest metadata", error);
            return report;
        }
    };
    let mut server = match finalize::BundleFinalization::server(
        &request.kit.inventory.kit_version,
        server,
        &finalization_metadata,
    ) {
        Ok(state) => state,
        Err(error) => {
            report.fail("server manifest placeholders", error);
            return report;
        }
    };
    if let Err(error) = server.commit_artifacts(&mut report.artifacts, "server") {
        report.server = Some(server.abort_into_bundle());
        report.fail("server manifest artifacts", error);
        return report;
    }
    if !guards::record(
        &mut report,
        "server",
        guards::validate(server.bundle(), &server_request, &initial, &request),
    ) {
        report.server = Some(server.abort_into_bundle());
        return report;
    }
    if let Err(error) = analysis::enrich(
        &mut analysis_request,
        &request,
        &initial,
        &server_request,
        server.bundle(),
    ) {
        report.server = Some(server.abort_into_bundle());
        report.fail("analysis graph consistency", error);
        return report;
    }
    let serialized = match serde_json::to_vec(server.bundle()) {
        Ok(bytes) => bytes,
        Err(error) => {
            report.server = Some(server.abort_into_bundle());
            report.fail("server metadata", error.to_string());
            return report;
        }
    };
    report.server_build_id = Some(sha(&serialized));
    report.phase(
        "server",
        "complete",
        server.bundle().chunks.len() + server.bundle().assets.len(),
    );
    let mut analysis = match build_analysis::analyze(&analysis_request) {
        Ok(analysis) => analysis,
        Err(error) => {
            report.server = Some(server.abort_into_bundle());
            report.fail("analysis", error);
            return report;
        }
    };
    if !analysis.errors.is_empty() {
        for error in &analysis.errors {
            report
                .diagnostics
                .push(json!({"source":"native-kit-analysis","severity":"error","message":error}));
        }
        report.analysis = Some(analysis);
        report.server = Some(server.abort_into_bundle());
        report.fail("analysis", "Route metadata failed validation");
        return report;
    }
    if evaluator.is_some() || !analysis.requirements.is_empty() || !remote_identities.is_empty() {
        match manifest::preliminary(&request, &initial, &analysis, server.bundle()) {
            Ok(snapshot) => {
                match execution_snapshot::prepare(&request, &snapshot) {
                    Ok(execution) => report.execution_snapshot = Some(execution),
                    Err(error) => {
                        report.runtime_snapshot = Some(snapshot);
                        report.server = Some(server.abort_into_bundle());
                        report.analysis = Some(analysis);
                        report.fail("runtime execution snapshot", error);
                        return report;
                    }
                }
                report.runtime_snapshot = Some(snapshot);
            }
            Err(error) => {
                report.server = Some(server.abort_into_bundle());
                report.analysis = Some(analysis);
                report.fail("runtime analysis snapshot", error);
                return report;
            }
        }
    }
    if let Some(evaluator) = evaluator.as_mut() {
        let snapshot = report
            .execution_snapshot
            .as_ref()
            .expect("Prepared runtime snapshot");
        let hash = request.kit.inventory.options.router.router_type
            == ts_svelte_kit::build_manifest::RouterType::Hash;
        let evaluated = evaluator
            .analyze_with_remotes(
                snapshot,
                &request.kit.inventory.kit_version,
                hash,
                &remote_identities,
            )
            .and_then(|receipt| {
                let routes = runtime_analysis::merge(
                    &request,
                    &snapshot.bootstrap.server_build_id,
                    &analysis,
                    &receipt.routes,
                )?;
                let remotes = match receipt.remotes {
                    Some(value) => Some(runtime_remotes::validate(
                        &snapshot.bootstrap.server_build_id,
                        &remote_identities,
                        &value,
                    )?),
                    None if remote_identities.is_empty() => None,
                    None => return Err("Runtime omitted required remote metadata".into()),
                };
                Ok((routes, remotes))
            });
        match evaluated {
            Ok((result, remotes)) => {
                analysis = result.analysis;
                report.resolved_entries = result.resolved_entries;
                report.remote_metadata = remotes;
                report.phase("runtime analysis", "complete", analysis.routes.len());
            }
            Err(error) => {
                report.server = Some(server.abort_into_bundle());
                report.analysis = Some(analysis);
                report.fail("runtime analysis", error);
                return report;
            }
        }
    }
    if !remote_identities.is_empty() && report.remote_metadata.is_none() {
        report.requirement(
            "analysis",
            "remote_metadata",
            "Remote client generation requires native evaluated server metadata",
        );
    }
    for requirement in &analysis.requirements {
        report.requirement(
            "analysis",
            "runtime_value",
            format!(
                "{}: {}: {}",
                requirement.module, requirement.subject, requirement.reason
            ),
        );
    }
    let mut node_loads = vec![];
    for (index, node) in analysis.nodes.iter().enumerate() {
        let source = &request.kit.inventory.nodes[index];
        match (
            known(&node.has_server_load),
            known(&node.has_universal_load),
        ) {
            (Some(server), Some(universal)) => node_loads.push(AnalyzedNodeLoads {
                component: source.component.clone(),
                universal: source.universal.clone(),
                server: source.server.clone(),
                has_server_load: server,
                has_universal_load: universal,
            }),
            _ => {
                report.requirement(
                    "analysis",
                    "load_presence",
                    format!("Node {index} load presence remains unknown"),
                );
            }
        }
    }
    if !report.requirements.is_empty() {
        report.phase("analysis", "incomplete", analysis.routes.len());
        report.analysis = Some(analysis);
        report.server = Some(server.abort_into_bundle());
        return report;
    }
    report.phase("analysis", "complete", analysis.routes.len());
    let mut optimized_input = request.kit.clone();
    optimized_input.client_analysis = Some(ResolvedClientLoadAnalysis {
        server_build_id: report.server_build_id.clone().unwrap(),
        nodes: node_loads,
    });
    let optimized = match build_plan::create_build_plan(&optimized_input) {
        Ok(plan) => plan,
        Err(error) => {
            report.server = Some(server.abort_into_bundle());
            report.analysis = Some(analysis);
            report.fail("optimized planning", error);
            return report;
        }
    };
    report.generated_sources = optimized.generated_files.clone();
    let mut csr = false;
    for route in &request.kit.inventory.routes {
        if route.page.is_some() {
            match known(&analysis.routes[&route.id].csr) {
                Some(value) => csr |= value,
                None => report.requirement(
                    "client",
                    "csr_unknown",
                    format!("CSR option is unknown for {}", route.id),
                ),
            }
        }
    }
    if !report.requirements.is_empty() {
        report.server = Some(server.abort_into_bundle());
        report.analysis = Some(analysis);
        return report;
    }
    let mut client = if csr {
        let client_request = match graph::bundle_request(&request, &optimized, true) {
            Ok(request) => request,
            Err(error) => {
                report.server = Some(server.abort_into_bundle());
                report.analysis = Some(analysis);
                report.fail("client inputs", error);
                return report;
            }
        };
        let remote_plugin =
            match remote_graph::plugin(&request, true, report.remote_metadata.as_ref()) {
                Ok(plugin) => plugin,
                Err(error) => {
                    report.server = Some(server.abort_into_bundle());
                    report.analysis = Some(analysis);
                    report.fail("client remote inputs", error);
                    return report;
                }
            };
        match ts_svelte_bundle::generate_with_plugins(
            client_request.clone(),
            vec![remote_plugin.into_plugin()],
        )
        .await
        {
            Ok(mut bundle) => {
                if let Err(error) = tracking::collect(
                    &mut report,
                    &mut bundle,
                    &request.kit.inventory.kit_version,
                    "client",
                ) {
                    report.server = Some(server.abort_into_bundle());
                    report.analysis = Some(analysis);
                    report.fail("client feature tracking", error);
                    return report;
                }
                if let Err(error) = add_bundle(&mut report, &bundle, "client") {
                    report.server = Some(server.abort_into_bundle());
                    report.analysis = Some(analysis);
                    report.fail("client artifacts", error);
                    return report;
                }
                if !guards::record(
                    &mut report,
                    "client",
                    guards::validate(&bundle, &client_request, &optimized, &request),
                ) {
                    report.server = Some(server.abort_into_bundle());
                    report.client = Some(bundle);
                    report.analysis = Some(analysis);
                    return report;
                }
                report.phase(
                    "client",
                    "complete",
                    bundle.chunks.len() + bundle.assets.len(),
                );
                let metadata = match finalize::metadata(&request, Some(&bundle), true, None) {
                    Ok(metadata) => metadata,
                    Err(error) => {
                        report.server = Some(server.abort_into_bundle());
                        report.client = Some(bundle);
                        report.analysis = Some(analysis);
                        report.fail("client manifest metadata", error);
                        return report;
                    }
                };
                let mut state = match finalize::BundleFinalization::client(
                    &request.kit.inventory.kit_version,
                    bundle,
                    &metadata,
                ) {
                    Ok(state) => state,
                    Err(error) => {
                        report.server = Some(server.abort_into_bundle());
                        report.analysis = Some(analysis);
                        report.fail("client manifest placeholders", error);
                        return report;
                    }
                };
                if let Err(error) = state.commit_artifacts(&mut report.artifacts, "client") {
                    report.server = Some(server.abort_into_bundle());
                    report.client = Some(state.abort_into_bundle());
                    report.analysis = Some(analysis);
                    report.fail("client manifest artifacts", error);
                    return report;
                }
                finalization_metadata = metadata;
                Some(state)
            }
            Err(error) => {
                report.diagnostics.extend(error.diagnostics);
                report.server = Some(server.abort_into_bundle());
                report.analysis = Some(analysis);
                report.fail("client", error.message);
                return report;
            }
        }
    } else {
        report.phase("client", "skippedAllCsrFalse", 0);
        None
    };
    if client.is_none() {
        finalization_metadata.client_manifest = Some(vec![]);
    }
    if let Err(error) = server
        .resolve_immutable(&finalization_metadata)
        .and_then(|()| server.commit_artifacts(&mut report.artifacts, "server"))
    {
        report.server = Some(server.abort_into_bundle());
        report.client = client.map(finalize::BundleFinalization::abort_into_bundle);
        report.analysis = Some(analysis);
        report.fail("immutable manifest finalization", error);
        return report;
    }
    for (id, route) in &analysis.routes {
        match known(&route.prerender) {
            Some(Value::Bool(false)) => {}
            Some(_) => report.requirement(
                "prerender",
                "runtime_execution",
                format!("Route {id} requires prerender execution; it was not executed"),
            ),
            None => report.requirement(
                "prerender",
                "unknown_policy",
                format!("Route {id} prerender policy remains unknown"),
            ),
        }
    }
    if let Some(metadata) = &report.remote_metadata {
        let pending = metadata
            .remotes
            .iter()
            .flat_map(|remote| {
                remote.exports.iter().filter_map(|export| {
                    (export.kind == ts_svelte_kit::build_remote::RemoteKind::Prerender)
                        .then(|| format!("{}/{}", remote.hash, export.name))
                })
            })
            .collect::<Vec<_>>();
        for id in pending {
            report.requirement(
                "prerender",
                "remote_execution",
                format!("Remote {id} requires prerender input evaluation and response generation"),
            );
        }
    }
    if report.status != "failed" && report.requirements.is_empty() && request.static_assets_complete
    {
        // All analyzed route policies are explicitly false. This is completed
        // empty prerender output, never a stand-in for pending runtime work.
        finalization_metadata.prerendered_paths = Some(vec![]);
        let result = server
            .finish(&finalization_metadata)
            .and_then(|()| server.commit_artifacts(&mut report.artifacts, "server"))
            .and_then(|()| {
                if let Some(state) = &mut client {
                    state.finish(&finalization_metadata)?;
                    state.commit_artifacts(&mut report.artifacts, "client")?;
                }
                Ok(())
            });
        match result {
            Ok(()) => report.phase(
                "manifest finalization",
                "complete",
                server.bundle().chunks.len()
                    + client
                        .as_ref()
                        .map_or(0, |state| state.bundle().chunks.len()),
            ),
            Err(error) => report.fail("manifest finalization", error),
        }
    }
    match instrumentation::validate_server(&request, server.bundle(), &report.artifacts) {
        Ok(receipt) => {
            if receipt.is_some() {
                report.phase("instrumentation entry", "complete", 1);
            }
            report.instrumentation = receipt;
        }
        Err(error) => report.fail("instrumentation entry", error),
    }
    if report.instrumentation.is_some() && request.adapter_node.is_none() {
        report.requirement("instrumentation", "adapter_wrapper", "The emitted instrumentation entry requires an adapter wrapper that initializes it before the application entry");
    }
    let manifest_expression = match manifest::generate(
        &request,
        &optimized,
        &analysis,
        server.bundle(),
        client.as_ref().map(finalize::BundleFinalization::bundle),
        &mut report.artifacts,
    ) {
        Err(error) => {
            report.fail("nodes and manifests", error);
            None
        }
        Ok(expression) => {
            report.phase(
                "nodes and manifests",
                "complete",
                request.kit.inventory.nodes.len() + 1,
            );
            Some(expression)
        }
    };
    if let Some(adapter) = &request.kit.config.adapter_name {
        if adapter != "@sveltejs/adapter-node" || request.adapter_node.is_none() {
            report.requirement(
                "adapter",
                "native_adapter",
                format!("Selected adapter {adapter:?} requires its native implementation"),
            );
        }
    } else if request.adapter_node.is_some() {
        report.fail(
            "adapter",
            "adapterNode supplied without selecting @sveltejs/adapter-node",
        );
    }
    if !request.static_assets_complete {
        report.requirement(
            "static assets",
            "complete_inventory",
            "Caller has not supplied a complete static asset inventory",
        );
    }
    if let Err(error) = copy_public_assets(
        &request,
        &optimized,
        server.bundle(),
        client.is_some(),
        &mut report.artifacts,
    ) {
        report.fail("public assets", error);
    }
    if report.status != "failed" && report.requirements.is_empty() {
        if request.kit.inventory.service_worker.is_some() {
            if let Some(client_state) = &client {
                let client_bundle = client_state.bundle();
                let worker_request = service_worker::derive_request(
                    &request,
                    client_bundle,
                    &request.service_worker_options,
                    &[],
                );
                match worker_request {
                    Err(error) => report.fail("service worker inputs", error),
                    Ok(worker_request) => match service_worker::build(
                        &request,
                        &optimized,
                        &worker_request,
                        client_bundle,
                        &report.artifacts,
                    )
                    .await
                    {
                        Err(error) => report.fail("service worker", error),
                        Ok(output) => {
                            if output.status == "complete" {
                                let mut merged = report.artifacts.clone();
                                let result =
                                    output.artifacts.iter().try_for_each(|(file, artifact)| {
                                        artifacts::insert(
                                            &mut merged,
                                            file.clone(),
                                            &artifacts::bytes(artifact)?,
                                            &artifact.kind,
                                        )
                                    });
                                match result {
                                    Ok(()) => {
                                        report.artifacts = merged;
                                        report.phase(
                                            "service worker",
                                            "complete",
                                            output.artifacts.len(),
                                        );
                                    }
                                    Err(error) => report.fail("service worker artifacts", error),
                                }
                            } else if output.status == "failed" {
                                report.fail(
                                    "service worker",
                                    "Worker import or configuration guards failed",
                                );
                            } else {
                                report.requirement(
                                    "service worker",
                                    "incomplete_guards",
                                    "Worker graph guards require additional resolved evidence",
                                );
                            }
                            report.service_worker = Some(output);
                        }
                    },
                }
            } else {
                report.requirement(
                    "service worker",
                    "client_inventory",
                    "Service-worker build needs the resolved client output inventory",
                );
            }
        }
    }
    if report.status != "failed" && report.requirements.is_empty() {
        if let Some(adapter_request) = &request.adapter_node {
            match adapter::package(
                &request,
                adapter_request,
                &report.artifacts,
                manifest_expression.as_deref().expect("completed manifest"),
                ts_svelte_kit::adapter_node::Prerendered::default(),
                report.instrumentation.as_ref(),
            )
            .await
            {
                Ok(output) => {
                    let count = output.artifacts.len();
                    let mut merged = report.artifacts.clone();
                    let result = output.artifacts.iter().try_for_each(|(file, artifact)| {
                        artifacts::insert(
                            &mut merged,
                            file.clone(),
                            &artifacts::bytes(artifact)?,
                            &artifact.kind,
                        )
                    });
                    match result {
                        Ok(()) => {
                            report.artifacts = merged;
                            report.phase("adapter", "complete", count);
                            report.adapter = Some(output);
                        }
                        Err(error) => report.fail("adapter artifacts", error),
                    }
                }
                Err(error) => report.fail("adapter", error),
            }
        }
    }
    if report.status != "failed"
        && report.requirements.is_empty()
        && (!server.is_final() || client.as_ref().is_some_and(|state| !state.is_final()))
    {
        report.fail(
            "manifest finalization",
            "Build cannot complete with pending manifest receipts",
        );
    }
    report.server = Some(server.abort_into_bundle());
    report.client = client.map(finalize::BundleFinalization::abort_into_bundle);
    report.analysis = Some(analysis);
    if report.status != "failed" && report.requirements.is_empty() {
        report.status = "complete".into();
        report.phase("prerender", "skippedExplicitFalse", 0);
        if request.adapter_node.is_none() {
            report.phase("adapter", "notSelected", 0);
        }
    }
    report
}
fn copy_public_assets(
    request: &BuildRequest,
    plan: &NativeBuildPlan,
    server: &BundleResult,
    client_built: bool,
    artifacts: &mut BTreeMap<String, Artifact>,
) -> Result<(), String> {
    for (file, asset) in &request.static_assets {
        artifacts::relative(file)?;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&asset.source)
            .map_err(|e| e.to_string())?;
        artifacts::insert(artifacts, format!("client/{file}"), &bytes, "static")?;
    }
    // Only CSS graph assets are public copies. Server read()/asset imports may
    // contain private files and are deliberately not copied indiscriminately.
    let mut public = BTreeSet::new();
    for module in server.css.modules.values() {
        public.insert(module.file_name.clone());
        public.extend(module.map_file_name.iter().cloned());
        public.extend(module.assets.iter().cloned());
    }
    for asset in &server.assets {
        if public.contains(&asset.file_name) {
            artifacts::insert(
                artifacts,
                format!("client/{}", asset.file_name),
                &asset_bytes(asset)?,
                "cssAsset",
            )?;
        }
    }
    if client_built {
        for (file, code) in &plan.client_output_files {
            let relative = file
                .strip_prefix(&plan.client.output.directory)
                .map_err(|_| "Client generated output escapes its directory")?;
            artifacts::insert(
                artifacts,
                format!("client/{}", path(relative)?),
                code.as_bytes(),
                "generated",
            )?;
        }
    }
    Ok(())
}
