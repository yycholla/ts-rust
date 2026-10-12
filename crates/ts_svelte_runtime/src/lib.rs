//! Framework server initialization on the separate native ECMAScript runtime.
//! Matches Kit 2.69.2 / 3.0.1 postbuild initialization contracts. This layer
//! does not execute config files or provide implicit filesystem/network access.
//! Route validation, entries and prerendering build on this initialized session;
//! node load facts alone are not a complete metadata-analysis result.
pub mod host_channel;
pub mod metadata;
pub mod prerender_remotes;
pub mod prerender_session;
pub mod prerender_visit;
pub mod protocol;
pub mod remotes;
#[cfg(test)]
mod remotes_protocol_tests;
pub mod server_dispatch;

#[cfg(target_arch = "wasm32")]
thread_local! {
    static SERVICE: std::cell::RefCell<protocol::Service> = std::cell::RefCell::new(protocol::Service::default());
}
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn svelte_runtime_json(request: &str) -> String {
    SERVICE.with(|service| service.borrow_mut().request_json(request))
}

use boa_engine::{
    JsNativeError, JsObject, JsString, JsValue, NativeFunction,
    object::{FunctionObjectBuilder, ObjectInitializer, builtins::JsArray},
    property::Attribute,
};
use boa_gc::{Finalize, Trace};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use ts_js_runtime::{
    HostCapability, HostOptions, HostSnapshot, Runtime, RuntimeError, RuntimeLimits,
    native_session::NativeSession,
};
type Result<T> = std::result::Result<T, RuntimeError>;
fn invalid(message: impl Into<String>) -> RuntimeError {
    RuntimeError {
        code: "kit_runtime_input".into(),
        message: message.into(),
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "profile", rename_all = "camelCase", deny_unknown_fields)]
pub enum Initialization {
    Kit2 {
        internal: String,
        environment: String,
        #[serde(rename = "publicPrefix")]
        public_prefix: String,
        #[serde(rename = "privatePrefix")]
        private_prefix: String,
    },
    Kit3 {
        server: String,
    },
}
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BootstrapRequest {
    pub server_build_id: String,
    pub snapshot: HostSnapshot,
    pub manifest_entry: String,
    pub initialization: Initialization,
    pub environment: BTreeMap<String, String>,
    /// Exact server-relative asset identities. Byte arrays remain data only.
    pub assets: BTreeMap<String, Vec<u8>>,
    #[serde(default)]
    pub limits: RuntimeLimits,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeLoads {
    pub index: usize,
    pub has_server_load: bool,
    pub has_universal_load: bool,
    pub server_load_callable: bool,
    pub universal_load_callable: bool,
}
#[derive(Trace, Finalize)]
struct AssetSnapshot {
    #[unsafe_ignore_trace]
    files: BTreeMap<String, Vec<u8>>,
}
pub struct ServerRuntime {
    runtime: Runtime,
    manifest: JsObject,
    kit3: bool,
    server_build_id: String,
    nodes: Option<Vec<JsObject>>,
    route_metadata: Option<(metadata::MetadataProfile, bool, metadata::MetadataAnalysis)>,
    remote_metadata: Option<(Vec<remotes::RemoteChunk>, remotes::RemoteAnalysis)>,
}
fn object(value: JsValue, label: &str) -> Result<JsObject> {
    value
        .as_object()
        .ok_or_else(|| invalid(format!("{label} must be an object")))
}
fn get(session: &mut NativeSession<'_>, object: &JsObject, key: &str) -> Result<JsValue> {
    Ok(object.get(JsString::from(key), session.context())?)
}
fn invoke(
    session: &mut NativeSession<'_>,
    module: &JsObject,
    name: &str,
    args: &[JsValue],
) -> Result<JsValue> {
    let function = get(session, module, name)?;
    session.call(&function, &JsValue::undefined(), args)
}
fn filter_env(env: &BTreeMap<String, String>, allowed: &str, disallowed: &str) -> Value {
    json!(
        env.iter()
            .filter(|(key, _)| key.starts_with(allowed)
                && (disallowed.is_empty() || !key.starts_with(disallowed)))
            .collect::<BTreeMap<_, _>>()
    )
}
impl ServerRuntime {
    pub fn initialize(request: BootstrapRequest, options: HostOptions) -> Result<Self> {
        if request.server_build_id.is_empty() {
            return Err(invalid("Explicit server build identity required"));
        }
        let mut total = 0usize;
        for (file, bytes) in &request.assets {
            if file.is_empty()
                || file.contains(['\\', '\0'])
                || file
                    .split('/')
                    .any(|s| s.is_empty() || s == "." || s == "..")
            {
                return Err(invalid(format!("Invalid server asset identity {file:?}")));
            }
            total = total
                .checked_add(bytes.len())
                .ok_or_else(|| invalid("Asset size overflow"))?;
            if bytes.len() > 16 * 1024 * 1024 || total > 64 * 1024 * 1024 {
                return Err(invalid("Server asset snapshot exceeds native bounds"));
            }
        }
        let capabilities = BTreeSet::from([
            HostCapability::RequestResponse,
            HostCapability::Crypto,
            HostCapability::NodeAsyncHooks,
        ]);
        let mut runtime =
            Runtime::with_host_options(request.snapshot, request.limits, &capabilities, options)?;
        let kit3 = matches!(request.initialization, Initialization::Kit3 { .. });
        let mut retained = None;
        runtime.native_operation(|session| {
            server_dispatch::claim_metadata_session(session)?;
            let module = session.namespace(&request.manifest_entry)?;
            let manifest = object(get(session, &module, "manifest")?, "Server manifest")?;
            let read = FunctionObjectBuilder::new(
                session.context().realm(),
                NativeFunction::from_copy_closure_with_captures(
                    |_, args, assets, context| {
                        let file = args
                            .first()
                            .and_then(JsValue::as_string)
                            .ok_or_else(|| {
                                JsNativeError::typ()
                                    .with_message("Server read requires a string asset identity")
                            })?
                            .to_std_string()
                            .map_err(|_| {
                                JsNativeError::typ().with_message("Invalid asset string")
                            })?;
                        let bytes = assets.files.get(&file).ok_or_else(|| {
                            JsNativeError::typ()
                                .with_message(format!("Missing snapshot server asset {file}"))
                        })?;
                        ts_js_runtime::web_streams::from_bytes(bytes.clone(), context)
                            .map(JsValue::from)
                    },
                    AssetSnapshot {
                        files: request.assets,
                    },
                ),
            )
            .name(JsString::from("read"))
            .length(1)
            .build();
            let env = JsValue::from_json(&json!(request.environment), session.context())?;
            match &request.initialization {
                Initialization::Kit2 {
                    internal,
                    environment,
                    public_prefix,
                    private_prefix,
                } => {
                    let internal = session.namespace(internal)?;
                    invoke(session, &internal, "set_building", &[])?;
                    let private = JsValue::from_json(
                        &filter_env(&request.environment, private_prefix, public_prefix),
                        session.context(),
                    )?;
                    let public = JsValue::from_json(
                        &filter_env(&request.environment, public_prefix, private_prefix),
                        session.context(),
                    )?;
                    invoke(session, &internal, "set_private_env", &[private])?;
                    invoke(session, &internal, "set_public_env", &[public])?;
                    invoke(
                        session,
                        &internal,
                        "set_manifest",
                        &[manifest.clone().into()],
                    )?;
                    invoke(
                        session,
                        &internal,
                        "set_read_implementation",
                        &[read.into()],
                    )?;
                    let environment = session.namespace(environment)?;
                    invoke(session, &environment, "set_env", &[env])?;
                }
                Initialization::Kit3 { server } => {
                    let server = session.namespace(server)?;
                    let config = ObjectInitializer::new(session.context())
                        .property(JsString::from("building"), true, Attribute::all())
                        .property(
                            JsString::from("manifest"),
                            manifest.clone(),
                            Attribute::all(),
                        )
                        .property(JsString::from("env"), env, Attribute::all())
                        .property(JsString::from("read"), read, Attribute::all())
                        .build();
                    let pending = invoke(session, &server, "configure", &[config.into()])?;
                    session.settle(pending)?;
                }
            }
            retained = Some(manifest);
            Ok(Value::Null)
        })?;
        Ok(Self {
            runtime,
            manifest: retained.expect("Successful initialization retains manifest"),
            kit3,
            server_build_id: request.server_build_id,
            nodes: None,
            route_metadata: None,
            remote_metadata: None,
        })
    }
    pub fn server_build_id(&self) -> &str {
        &self.server_build_id
    }
    /// The upstream postbuild order evaluates routes before remote modules.
    pub fn analyze_remotes(&mut self, chunks: Vec<remotes::RemoteChunk>) -> Result<Value> {
        let profile = self
            .route_metadata
            .as_ref()
            .ok_or_else(|| invalid("Analyze route metadata before remote modules"))?
            .0;
        if self
            .remote_metadata
            .as_ref()
            .is_some_and(|(previous, _)| previous != &chunks)
        {
            return Err(invalid(
                "Remote chunk inventory cannot change within an initialized session",
            ));
        }
        let manifest = self.manifest.clone();
        let mut cached = self.remote_metadata.take();
        let result = self.runtime.native_operation(|session| {
            if cached.is_none() {
                let metadata = remotes::analyse(session, &manifest, &chunks, profile)?;
                cached = Some((chunks, metadata));
            }
            let mut output = vec![];
            for remote in &cached.as_ref().unwrap().1.remotes {
                let mut exports = vec![];
                for export in &remote.exports {
                    let name = export.name.to_std_string().map_err(|_| invalid("Remote export name cannot be represented by scalar UTF-8 JSON"))?;
                    exports.push(json!({"name":name,"kind":session.normalize(export.kind.clone())?,"dynamic":session.normalize(export.dynamic.clone())?}));
                }
                output.push(json!({"hash":remote.hash,"file":remote.file,"exports":exports}));
            }
            Ok(json!({"serverBuildId":self.server_build_id,"remotes":output}))
        });
        self.remote_metadata = cached;
        result
    }
    /// Evaluate postbuild route metadata once, retaining engine values for later
    /// framework operations. The JSON view preserves undefined with tagged values.
    pub fn analyze_metadata(&mut self, options: metadata::MetadataOptions) -> Result<Value> {
        if options.profile.is_kit3() != self.kit3 {
            return Err(invalid(
                "Metadata profile differs from initialized Kit family",
            ));
        }
        if self
            .route_metadata
            .as_ref()
            .is_some_and(|(profile, hash, _)| *profile != options.profile || *hash != options.hash)
        {
            return Err(invalid(
                "Metadata options cannot change within an initialized session",
            ));
        }
        let facts = self.load_nodes()?;
        let nodes = self.nodes.as_ref().expect("Nodes loaded").clone();
        let manifest = self.manifest.clone();
        let mut cached = self.route_metadata.take();
        let result = self.runtime.native_operation(|session| {
            if cached.is_none() {
                cached = Some((
                    options.profile,
                    options.hash,
                    metadata::analyse(session, &manifest, &nodes, &options)?,
                ));
            }
            let mut routes = vec![];
            for route in &cached.as_ref().unwrap().2.routes {
                let page = if let Some(nodes) = &route.page_nodes {
                    let opts = metadata::page_options(session, nodes)?;
                    json!({
                        "csr":session.normalize(opts.csr)?,
                        "ssr":session.normalize(opts.ssr)?,
                        "prerender":session.normalize(opts.prerender)?,
                        "trailingSlash":session.normalize(opts.trailing_slash)?,
                        "actions":metadata::page_actions(session, nodes)?,
                    })
                } else {
                    Value::Null
                };
                let endpoint = if let Some(module) = &route.endpoint {
                    let slash = get(session, module, "trailingSlash")?;
                    json!({"trailingSlash":session.normalize(slash)?})
                } else {
                    Value::Null
                };
                routes.push(json!({
                    "id":route.id,
                    "config":session.normalize(route.config.clone())?,
                    "prerender":session.normalize(route.prerender.clone())?,
                    "entries":session.normalize(route.entries.clone())?,
                    "methods":route.methods,
                    "pageMethods":route.page_methods,
                    "endpointMethods":route.api_methods,
                    "page":page,
                    "endpoint":endpoint,
                }));
            }
            Ok(json!({"serverBuildId":self.server_build_id,"nodes":facts,"routes":routes}))
        });
        self.route_metadata = cached;
        result
    }
    /// Evaluate all actual node loaders in array order before awaiting results.
    /// Initialization does not call application init hooks, loads or handlers.
    pub fn load_nodes(&mut self) -> Result<Vec<NodeLoads>> {
        let manifest = self.manifest.clone();
        let kit3 = self.kit3;
        let mut nodes = self.nodes.clone();
        let mut facts = vec![];
        self.runtime.native_operation(|session| {
            if nodes.is_none() {
                let container = if kit3 {
                    manifest.clone()
                } else {
                    object(get(session, &manifest, "_")?, "Kit2 manifest internals")?
                };
                let loaders = JsArray::from_object(object(
                    get(session, &container, "nodes")?,
                    "Manifest node loaders",
                )?)?;
                let len = loaders.length(session.context())?;
                if len > 4096 {
                    return Err(invalid("Node loader count exceeds 4096"));
                }
                let mut pending = vec![];
                for index in 0..len {
                    let loader = loaders.get(index, session.context())?;
                    pending.push(session.call(&loader, &JsValue::undefined(), &[])?);
                }
                // Match Promise.all's rejection race, not index-ordered awaits.
                let global = session.context().global_object();
                let promise = get(session, &global, "Promise")?;
                let all = get(
                    session,
                    &object(promise.clone(), "Promise constructor")?,
                    "all",
                )?;
                let inputs = JsArray::from_iter(pending, session.context());
                let pending = session.call(&all, &promise, &[inputs.into()])?;
                let values =
                    JsArray::from_object(object(session.settle(pending)?, "Loaded nodes")?)?;
                let mut loaded = vec![];
                for index in 0..len {
                    loaded.push(object(
                        values.get(index, session.context())?,
                        "Loaded server node",
                    )?);
                }
                nodes = Some(loaded);
            }
            let mut indices = BTreeSet::new();
            for node in nodes.as_ref().unwrap() {
                let actual = get(session, node, "index")?;
                let actual = actual
                    .as_number()
                    .ok_or_else(|| invalid("Node index must be an integer"))?;
                if !actual.is_finite() || actual < 0.0 || actual >= 4096.0 || actual.fract() != 0.0
                {
                    return Err(invalid("Node index is outside the native node bound"));
                }
                let index = actual as usize;
                if !indices.insert(index) {
                    return Err(invalid("Duplicate original node index"));
                }
                let server = get(session, node, "server")?;
                let universal = get(session, node, "universal")?;
                let mut read = |module: &JsValue, key: &str| -> Result<JsValue> {
                    if module.is_null_or_undefined() {
                        Ok(JsValue::undefined())
                    } else {
                        get(session, &object(module.clone(), "Node module")?, key)
                    }
                };
                let server_load = read(&server, "load")?;
                let server_slash = read(&server, "trailingSlash")?;
                let universal_load = read(&universal, "load")?;
                facts.push(NodeLoads {
                    index,
                    has_server_load: !server_load.is_undefined() || !server_slash.is_undefined(),
                    has_universal_load: !universal_load.is_undefined(),
                    server_load_callable: server_load.is_callable(),
                    universal_load_callable: universal_load.is_callable(),
                });
            }
            Ok(Value::Null)
        })?;
        self.nodes = nodes;
        Ok(facts)
    }
}

#[cfg(test)]
mod tests;
