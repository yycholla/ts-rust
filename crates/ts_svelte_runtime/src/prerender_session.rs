//! Snapshot-only native prerender server driver. This supplies execution and
//! response collection, not crawling, output naming, HTTP-error policy or saving.
//! Kit postbuild/prerender.js (MIT, Svelte contributors) supplies lifecycle,
//! asset read precedence, dependency ordering and decodeURI semantics.
use crate::{
    BootstrapRequest, Initialization,
    server_dispatch::{RenderInput, RenderProfile, Renderer},
};
use boa_engine::{
    JsNativeError, JsObject, JsString, JsValue, NativeFunction,
    builtins::object::OrdinaryObject,
    object::{
        FunctionObjectBuilder, ObjectInitializer,
        builtins::{JsArray, JsMap, JsMapIterator, JsSet, JsUint8Array},
    },
    property::Attribute,
};
use boa_gc::{Finalize, Trace};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
    rc::Rc,
};
use ts_js_runtime::{
    HostCapability, HostOptions, Runtime, RuntimeError, native_session::NativeSession, web_fetch,
    web_headers::HeaderList, web_streams,
};

type Result<T> = std::result::Result<T, RuntimeError>;
fn invalid(message: impl Into<String>) -> RuntimeError {
    RuntimeError {
        code: "kit_prerender_input".into(),
        message: message.into(),
    }
}
fn get(s: &mut NativeSession<'_>, object: &JsObject, name: &str) -> Result<JsValue> {
    Ok(object.get(JsString::from(name), s.context())?)
}
fn object(value: JsValue, label: &str) -> Result<JsObject> {
    value
        .as_object()
        .ok_or_else(|| invalid(format!("{label} must be an object")))
}
fn string(value: JsValue, label: &str) -> Result<String> {
    value
        .as_string()
        .ok_or_else(|| invalid(format!("{label} must be a string")))?
        .to_std_string()
        .map_err(|_| invalid(format!("{label} has unpaired UTF-16")))
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PrerenderLimits {
    pub asset_bytes: usize,
    pub total_asset_bytes: usize,
    pub asset_files: usize,
    pub response_bytes: usize,
    pub visit_bytes: usize,
    pub dependencies: usize,
    pub visits: usize,
}
impl Default for PrerenderLimits {
    fn default() -> Self {
        Self {
            asset_bytes: 16 * 1024 * 1024,
            total_asset_bytes: 64 * 1024 * 1024,
            asset_files: 16384,
            response_bytes: 64 * 1024 * 1024,
            visit_bytes: 64 * 1024 * 1024,
            dependencies: 10000,
            visits: 10000,
        }
    }
}
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PrerenderInput {
    pub bootstrap: BootstrapRequest,
    pub server_entry: String,
    pub origin: String,
    pub base: String,
    pub app_dir: String,
    pub static_assets: BTreeMap<String, Vec<u8>>,
    #[serde(default)]
    pub limits: PrerenderLimits,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResponseRecord {
    pub status: u16,
    /// Raw HTTP ByteString bytes, not UTF-8.
    pub status_text: Vec<u8>,
    pub headers: HeaderList,
    pub body: Vec<u8>,
    pub route_id: Option<String>,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DependencyRecord {
    pub path: String,
    pub encoded_path: String,
    pub decoded_path: String,
    pub response: ResponseRecord,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VisitResult {
    pub server_build_id: String,
    pub encoded_path: String,
    pub decoded_path: String,
    pub response: ResponseRecord,
    pub dependencies: Vec<DependencyRecord>,
}
struct Assets {
    server: BTreeMap<String, Vec<u8>>,
    statics: BTreeMap<String, Vec<u8>>,
    published: BTreeMap<String, Vec<u8>>,
    missing_saved: BTreeSet<String>,
    app_dir: String,
    bytes: usize,
}
#[derive(Trace, Finalize)]
struct ReadCapture {
    #[unsafe_ignore_trace]
    assets: Rc<RefCell<Assets>>,
    server: bool,
}
fn read_function(s: &mut NativeSession<'_>, assets: Rc<RefCell<Assets>>, server: bool) -> JsObject {
    FunctionObjectBuilder::new(
        s.context().realm(),
        NativeFunction::from_copy_closure_with_captures(
            |_, args, capture, context| {
                let file = args
                    .first()
                    .and_then(JsValue::as_string)
                    .ok_or_else(|| {
                        JsNativeError::typ()
                            .with_message("Snapshot read requires a string asset identity")
                    })?
                    .to_std_string()
                    .map_err(|_| JsNativeError::typ().with_message("Invalid asset identity"))?;
                let bytes = {
                    let assets = capture.assets.borrow();
                    if !capture.server && assets.missing_saved.contains(&file) {
                        return Err(JsNativeError::typ()
                            .with_message(format!("Saved snapshot asset is missing: {file}"))
                            .into());
                    }
                    let bytes = if capture.server {
                        assets.server.get(&file)
                    } else {
                        assets.published.get(&file).or_else(|| {
                            if file.starts_with(&assets.app_dir) {
                                assets.server.get(&file)
                            } else {
                                assets.statics.get(&file)
                            }
                        })
                    };
                    bytes.cloned().ok_or_else(|| {
                        JsNativeError::typ().with_message(format!("Missing snapshot asset {file}"))
                    })?
                };
                if capture.server {
                    web_streams::from_bytes(bytes, context).map(JsValue::from)
                } else {
                    JsUint8Array::from_iter(bytes, context).map(JsValue::from)
                }
            },
            ReadCapture { assets, server },
        ),
    )
    .length(1)
    .build()
    .into()
}
struct Intrinsics {
    request: JsObject,
    url: JsObject,
    pathname: JsValue,
    decode_uri: JsValue,
}
impl Intrinsics {
    fn capture(s: &mut NativeSession<'_>) -> Result<Self> {
        let global = s.context().global_object();
        let request = object(get(s, &global, "Request")?, "Request constructor")?;
        let url = object(get(s, &global, "URL")?, "URL constructor")?;
        let prototype = object(get(s, &url, "prototype")?, "URL prototype")?;
        let descriptor = OrdinaryObject::get_own_property_descriptor(
            &JsValue::undefined(),
            &[prototype.into(), JsString::from("pathname").into()],
            s.context(),
        )?;
        let descriptor = object(descriptor, "URL pathname descriptor")?;
        let pathname = get(s, &descriptor, "get")?;
        let decode_uri = get(s, &global, "decodeURI")?;
        if !request.is_constructor()
            || !url.is_constructor()
            || !pathname.is_callable()
            || !decode_uri.is_callable()
        {
            return Err(invalid("Required native prerender intrinsics unavailable"));
        }
        Ok(Self {
            request,
            url,
            pathname,
            decode_uri,
        })
    }
    fn pathname(&self, s: &mut NativeSession<'_>, value: &str) -> Result<String> {
        let url = self.url.construct(
            &[
                JsString::from(value).into(),
                JsString::from("http://localhost").into(),
            ],
            None,
            s.context(),
        )?;
        string(s.call(&self.pathname, &url.into(), &[])?, "URL pathname")
    }
    fn decode(&self, s: &mut NativeSession<'_>, value: &str) -> Result<String> {
        let result = s
            .call(
                &self.decode_uri,
                &JsValue::undefined(),
                &[JsString::from(value).into()],
            )
            .map_err(|mut error| {
                error.message = format!("Failed to decode URI: {value}\n{}", error.message);
                error
            })?;
        string(result, "Decoded URI")
    }
}
fn asset_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name.len() > 1024 * 1024
        || name.contains(['\\', '\0'])
        || name
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err(invalid(format!("Invalid relative asset identity {name:?}")));
    }
    Ok(())
}
fn validate_limits(limits: &PrerenderLimits) -> Result<()> {
    let max = PrerenderLimits::default();
    for (value, cap) in [
        (limits.asset_bytes, max.asset_bytes),
        (limits.total_asset_bytes, max.total_asset_bytes),
        (limits.asset_files, max.asset_files),
        (limits.response_bytes, max.response_bytes),
        (limits.visit_bytes, max.visit_bytes),
        (limits.dependencies, max.dependencies),
        (limits.visits, max.visits),
    ] {
        if value == 0 || value > cap {
            return Err(invalid(
                "Prerender limits must be positive and within native caps",
            ));
        }
    }
    Ok(())
}
/// Header inspection precedes body consumption so policy callbacks can run in
/// Kit order, and saved main output is available while dependencies are read.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResponseHead {
    pub status: u16,
    pub status_text: Vec<u8>,
    pub headers: HeaderList,
    pub route_id: Option<String>,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VisitHead {
    pub token: u64,
    pub server_build_id: String,
    pub encoded_path: String,
    pub decoded_path: String,
    pub response: ResponseHead,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DependencyHead {
    pub path: String,
    pub encoded_path: String,
    pub decoded_path: String,
    pub response: ResponseHead,
}
struct PendingDependency {
    record: JsObject,
    path: String,
    encoded: String,
    decoded: String,
}
struct PendingVisit {
    token: u64,
    response: JsValue,
    dependencies: JsMap,
    iterator: Option<JsMapIterator>,
    dependency: Option<PendingDependency>,
    main_read: bool,
    remaining: usize,
    count: usize,
}
pub struct PrerenderSession {
    runtime: Runtime,
    renderer: Renderer,
    intrinsics: Intrinsics,
    assets: Rc<RefCell<Assets>>,
    read: JsObject,
    remote_responses: JsMap,
    resolved_route_ids: Option<JsSet>,
    get_client_address: JsObject,
    origin: String,
    base: String,
    build_id: String,
    limits: PrerenderLimits,
    visits: usize,
    started: bool,
    pending: Option<PendingVisit>,
}
impl PrerenderSession {
    pub fn configure(input: PrerenderInput, options: HostOptions) -> Result<Self> {
        validate_limits(&input.limits)?;
        asset_name(&input.app_dir)?;
        if input.bootstrap.server_build_id.is_empty() || input.server_entry.is_empty() {
            return Err(invalid(
                "Explicit server build and entry identities required",
            ));
        }
        if (!input.base.is_empty() && (!input.base.starts_with('/') || input.base.ends_with('/')))
            || input.base.contains(['?', '#', '\\', '\0'])
            || input.origin.len() > 1024 * 1024
        {
            return Err(invalid("Invalid explicit prerender base/origin"));
        }
        let kit3 = matches!(input.bootstrap.initialization, Initialization::Kit3 { .. });
        if let Initialization::Kit3 { server } = &input.bootstrap.initialization
            && server != &input.server_entry
        {
            return Err(invalid(
                "Kit3 bootstrap and rendering server entries differ",
            ));
        }
        let mut bytes = 0usize;
        let files = input
            .bootstrap
            .assets
            .len()
            .checked_add(input.static_assets.len())
            .ok_or_else(|| invalid("Asset count overflow"))?;
        if files > input.limits.asset_files {
            return Err(invalid("Too many prerender asset snapshots"));
        }
        for (file, source) in input.bootstrap.assets.iter().chain(&input.static_assets) {
            asset_name(file)?;
            bytes = bytes
                .checked_add(source.len())
                .ok_or_else(|| invalid("Asset bytes overflow"))?;
            if source.len() > input.limits.asset_bytes || bytes > input.limits.total_asset_bytes {
                return Err(invalid("Prerender asset bytes exceed bounds"));
            }
        }
        let assets = Rc::new(RefCell::new(Assets {
            server: input.bootstrap.assets,
            statics: input.static_assets,
            published: BTreeMap::new(),
            missing_saved: BTreeSet::new(),
            app_dir: input.app_dir,
            bytes,
        }));
        let capabilities = BTreeSet::from([
            HostCapability::RequestResponse,
            HostCapability::Crypto,
            HostCapability::NodeAsyncHooks,
        ]);
        let mut runtime = Runtime::with_host_options(
            input.bootstrap.snapshot,
            input.bootstrap.limits,
            &capabilities,
            options,
        )?;
        let mut retained = None;
        runtime.native_operation(|s| {
            let intrinsics = Intrinsics::capture(s)?;
            // Validate origin-only syntax using the captured native URL before
            // executing application modules.
            let origin_url = intrinsics.url.construct(
                &[JsString::from(input.origin.as_str()).into()],
                None,
                s.context(),
            )?;
            let pathname = string(
                s.call(&intrinsics.pathname, &origin_url.clone().into(), &[])?,
                "Origin pathname",
            )?;
            let origin = string(get(s, &origin_url, "origin")?, "Origin")?;
            let href = string(get(s, &origin_url, "href")?, "Origin URL")?;
            if origin == "null"
                || !(origin.starts_with("http://") || origin.starts_with("https://"))
                || pathname != "/"
                || href != format!("{origin}/")
            {
                return Err(invalid(
                    "Prerender origin must be an HTTP(S) origin without path, query or credentials",
                ));
            }
            let module = s.namespace(&input.bootstrap.manifest_entry)?;
            let manifest = object(get(s, &module, "manifest")?, "Manifest")?;
            let init_read = read_function(s, assets.clone(), true);
            let read = read_function(s, assets.clone(), false);
            let profile = match input.bootstrap.initialization {
                Initialization::Kit2 {
                    internal,
                    environment,
                    public_prefix,
                    private_prefix,
                } => RenderProfile::Kit2 {
                    internal_entry: internal,
                    environment_entry: environment,
                    public_prefix,
                    private_prefix,
                },
                Initialization::Kit3 { .. } => RenderProfile::Kit3,
            };
            let remote_responses = JsMap::new(s.context());
            let resolved_route_ids = if kit3 {
                Some(JsSet::new(s.context()))
            } else {
                None
            };
            let get_client_address = FunctionObjectBuilder::new(
                s.context().realm(),
                NativeFunction::from_copy_closure(move |_, _, _| {
                    Err(JsNativeError::error()
                        .with_message(if kit3 {
                            "Cannot read `clientAddress` during prerendering"
                        } else {
                            "Cannot read clientAddress during prerendering"
                        })
                        .into())
                }),
            )
            .build()
            .into();
            let renderer = Renderer::configure(
                s,
                RenderInput {
                    profile,
                    server_entry: input.server_entry,
                    manifest,
                    environment: input.bootstrap.environment,
                    read: init_read.into(),
                },
            )?;
            retained = Some((
                renderer,
                intrinsics,
                read,
                remote_responses,
                resolved_route_ids,
                get_client_address,
                origin,
            ));
            Ok(Value::Null)
        })?;
        let (
            renderer,
            intrinsics,
            read,
            remote_responses,
            resolved_route_ids,
            get_client_address,
            origin,
        ) = retained.unwrap();
        Ok(Self {
            runtime,
            renderer,
            intrinsics,
            assets,
            read,
            remote_responses,
            resolved_route_ids,
            get_client_address,
            origin,
            base: input.base,
            build_id: input.bootstrap.server_build_id,
            limits: input.limits,
            visits: 0,
            started: false,
            pending: None,
        })
    }
    pub fn server_build_id(&self) -> &str {
        &self.build_id
    }
    pub fn start(&mut self) -> Result<()> {
        self.runtime.native_operation(|s| {
            self.renderer.start(s)?;
            Ok(Value::Null)
        })?;
        self.started = true;
        Ok(())
    }
    /// Explicitly publish bytes made by the caller. These take precedence over
    /// emitted server and static assets on subsequent synchronous read calls.
    pub fn publish(&mut self, file: String, bytes: Vec<u8>) -> Result<()> {
        self.runtime.native_operation(|_| {
            asset_name(&file)?;
            let mut assets = self.assets.borrow_mut();
            let prior = assets.published.get(&file).map_or(0, Vec::len);
            let total = assets.bytes - prior;
            let total = total
                .checked_add(bytes.len())
                .ok_or_else(|| invalid("Publication bytes overflow"))?;
            let files = assets.server.len()
                + assets.statics.len()
                + assets.published.len()
                + assets.missing_saved.len()
                + usize::from(
                    !assets.published.contains_key(&file) && !assets.missing_saved.contains(&file),
                );
            if bytes.len() > self.limits.asset_bytes
                || total > self.limits.total_asset_bytes
                || files > self.limits.asset_files
            {
                return Err(invalid("Publication exceeds snapshot limits"));
            }
            assets.missing_saved.remove(&file);
            assets.published.insert(file, bytes);
            assets.bytes = total;
            Ok(Value::Null)
        })?;
        Ok(())
    }
    /// A real saved alias can name an unwritten file (for example a 204). It
    /// must shadow static/server fallback with a read failure until the caller
    /// supplies actual saved bytes through publish; an empty file is different.
    pub fn mark_saved_missing(&mut self, file: String) -> Result<()> {
        self.runtime.native_operation(|_| {
            asset_name(&file)?;
            let mut assets = self.assets.borrow_mut();
            let count = assets.server.len()
                + assets.statics.len()
                + assets.published.len()
                + assets.missing_saved.len()
                + usize::from(
                    !assets.published.contains_key(&file) && !assets.missing_saved.contains(&file),
                );
            if count > self.limits.asset_files {
                return Err(invalid("Saved aliases exceed snapshot limits"));
            }
            if let Some(bytes) = assets.published.remove(&file) {
                assets.bytes -= bytes.len();
            }
            assets.missing_saved.insert(file);
            Ok(Value::Null)
        })?;
        Ok(())
    }
    pub fn begin_visit(&mut self, encoded_path: &str) -> Result<VisitHead> {
        let mut output = None;
        self.runtime.native_operation(|s| {
            if self.pending.is_some() {
                return Err(invalid("Finish the active visit before beginning another"));
            }
            if !self.started {
                return Err(invalid("Start the prerender server before visiting"));
            }
            if self.visits >= self.limits.visits {
                return Err(invalid("Prerender visit limit exceeded"));
            }
            self.visits += 1;
            if encoded_path.len() > 1024 * 1024
                || !encoded_path.starts_with('/')
                || encoded_path.starts_with("//")
                || encoded_path.contains(['?', '#', '\\', '\0'])
            {
                return Err(invalid(
                    "Visit requires an encoded absolute pathname without query or fragment",
                ));
            }
            let decoded = self.intrinsics.decode(s, encoded_path)?;
            if !decoded.starts_with(&self.base) {
                return Err(invalid("Visit is outside configured base"));
            }
            let request = self.intrinsics.request.construct(
                &[JsString::from(format!("{}{encoded_path}", self.origin)).into()],
                None,
                s.context(),
            )?;
            let dependencies = JsMap::new(s.context());
            let mut prerendering = ObjectInitializer::new(s.context());
            prerendering
                .property(
                    JsString::from("dependencies"),
                    dependencies.clone(),
                    Attribute::all(),
                )
                .property(
                    JsString::from("remote_responses"),
                    self.remote_responses.clone(),
                    Attribute::all(),
                );
            if let Some(ids) = &self.resolved_route_ids {
                prerendering.property(
                    JsString::from("resolved_route_ids"),
                    ids.clone(),
                    Attribute::all(),
                );
            }
            let prerendering = prerendering.build();
            let options = ObjectInitializer::new(s.context())
                .property(
                    JsString::from("getClientAddress"),
                    self.get_client_address.clone(),
                    Attribute::all(),
                )
                .property(
                    JsString::from("prerendering"),
                    prerendering,
                    Attribute::all(),
                )
                .property(JsString::from("read"), self.read.clone(), Attribute::all())
                .build();
            let result = self.renderer.dispatch(s, request.into(), options)?;
            let response: JsValue = result.response.into();
            let head = response_head(s, &self.intrinsics, &response, true)?;
            let token = self.visits as u64;
            self.pending = Some(PendingVisit {
                token,
                response,
                dependencies: result.dependencies,
                iterator: None,
                dependency: None,
                main_read: false,
                remaining: self.limits.visit_bytes,
                count: 0,
            });
            output = Some(VisitHead {
                token,
                server_build_id: self.build_id.clone(),
                encoded_path: encoded_path.into(),
                decoded_path: decoded,
                response: head,
            });
            Ok(Value::Null)
        })?;
        Ok(output.unwrap())
    }
    pub fn read_main(&mut self, token: u64) -> Result<ResponseRecord> {
        let mut output = None;
        self.runtime.native_operation(|s| {
            let pending = self
                .pending
                .as_mut()
                .filter(|v| v.token == token)
                .ok_or_else(|| invalid("Unknown active visit token"))?;
            if pending.main_read {
                return Err(invalid("Main response was already read"));
            }
            output = Some(collect_response(
                s,
                &self.intrinsics,
                pending.response.clone(),
                None,
                true,
                &self.limits,
                &mut pending.remaining,
            )?);
            pending.main_read = true;
            Ok(Value::Null)
        })?;
        Ok(output.unwrap())
    }
    /// Inspect the next dependency without reading its body getter or stream.
    /// A None result finishes the visit and releases its retained JS objects.
    pub fn next_dependency(&mut self, token: u64) -> Result<Option<DependencyHead>> {
        let mut output = None;
        self.runtime.native_operation(|s| {
            let pending = self
                .pending
                .as_mut()
                .filter(|v| v.token == token)
                .ok_or_else(|| invalid("Unknown active visit token"))?;
            if !pending.main_read || pending.dependency.is_some() {
                return Err(invalid(
                    "Read the current response before advancing dependencies",
                ));
            }
            if pending.iterator.is_none() {
                pending.iterator = Some(pending.dependencies.entries(s.context())?);
            }
            let next = object(
                pending.iterator.as_ref().unwrap().next(s.context())?,
                "Dependency iterator result",
            )?;
            if get(s, &next, "done")?.to_boolean() {
                self.pending = None;
                let remote_count = self
                    .remote_responses
                    .get_size(s.context())?
                    .as_number()
                    .ok_or_else(|| invalid("Invalid native remote response map size"))?;
                if remote_count > self.limits.dependencies as f64
                    || self
                        .resolved_route_ids
                        .as_ref()
                        .is_some_and(|ids| ids.size() > self.limits.dependencies)
                {
                    return Err(invalid(
                        "Persistent prerender metadata exceeds dependency limit",
                    ));
                }
                return Ok(Value::Null);
            }
            if pending.count >= self.limits.dependencies {
                return Err(invalid("Prerender dependency limit exceeded"));
            }
            pending.count += 1;
            let pair = JsArray::from_object(object(get(s, &next, "value")?, "Dependency pair")?)?;
            let path = string(pair.get(0, s.context())?, "Dependency path")?;
            if path.len() > 1024 * 1024 {
                return Err(invalid("Dependency path exceeds limit"));
            }
            let record = object(pair.get(1, s.context())?, "Dependency record")?;
            let encoded = self.intrinsics.pathname(s, &path)?;
            let decoded = self.intrinsics.decode(s, &encoded)?;
            let response = get(s, &record, "response")?;
            let head = response_head(s, &self.intrinsics, &response, false)?;
            output = Some(DependencyHead {
                path: path.clone(),
                encoded_path: encoded.clone(),
                decoded_path: decoded.clone(),
                response: head,
            });
            pending.dependency = Some(PendingDependency {
                record,
                path,
                encoded,
                decoded,
            });
            Ok(Value::Null)
        })?;
        Ok(output)
    }
    pub fn read_dependency(&mut self, token: u64) -> Result<DependencyRecord> {
        let mut output = None;
        self.runtime.native_operation(|s| {
            let pending = self
                .pending
                .as_mut()
                .filter(|v| v.token == token)
                .ok_or_else(|| invalid("Unknown active visit token"))?;
            let dependency = pending
                .dependency
                .take()
                .ok_or_else(|| invalid("Inspect the next dependency before reading it"))?;
            let body = get(s, &dependency.record, "body")?;
            let body = (!body.is_null_or_undefined()).then_some(body);
            // Kit reads result.response separately for headers, fallback body,
            // and save. A getter may return a different Response each time.
            let fallback = if body.is_none() {
                Some(get(s, &dependency.record, "response")?)
            } else {
                None
            };
            let bytes = response_bytes(
                s,
                fallback.as_ref(),
                body,
                &self.limits,
                &mut pending.remaining,
            )?;
            let saved_response = get(s, &dependency.record, "response")?;
            let head = response_head(s, &self.intrinsics, &saved_response, true)?;
            let response = ResponseRecord {
                status: head.status,
                status_text: head.status_text,
                headers: head.headers,
                body: bytes,
                route_id: head.route_id,
            };
            output = Some(DependencyRecord {
                path: dependency.path,
                encoded_path: dependency.encoded,
                decoded_path: dependency.decoded,
                response,
            });
            Ok(Value::Null)
        })?;
        Ok(output.unwrap())
    }
    /// Convenience collector. Build orchestration uses the staged methods to
    /// apply route policy and publish each saved response between body reads.
    pub fn visit(&mut self, encoded_path: &str) -> Result<VisitResult> {
        let head = self.begin_visit(encoded_path)?;
        let response = self.read_main(head.token)?;
        let mut dependencies = vec![];
        while self.next_dependency(head.token)?.is_some() {
            dependencies.push(self.read_dependency(head.token)?);
        }
        Ok(VisitResult {
            server_build_id: head.server_build_id,
            encoded_path: head.encoded_path,
            decoded_path: head.decoded_path,
            response,
            dependencies,
        })
    }
}
fn route_id(headers: &HeaderList) -> Option<String> {
    let mut value = Vec::new();
    let mut found = false;
    for (name, part) in headers {
        if name == b"x-sveltekit-routeid" {
            if found {
                value.extend_from_slice(b", ");
            }
            value.extend_from_slice(part);
            found = true;
        }
    }
    found.then(|| value.into_iter().map(char::from).collect())
}
fn response_head(
    s: &mut NativeSession<'_>,
    intrinsics: &Intrinsics,
    response: &JsValue,
    decode: bool,
) -> Result<ResponseHead> {
    let snapshot = web_fetch::response_snapshot(response)?;
    let route_id = if decode {
        route_id(&snapshot.headers)
            .map(|id| intrinsics.decode(s, &id))
            .transpose()?
    } else {
        None
    };
    Ok(ResponseHead {
        status: snapshot.status,
        status_text: snapshot.status_text,
        headers: snapshot.headers,
        route_id,
    })
}
fn collect_response(
    s: &mut NativeSession<'_>,
    intrinsics: &Intrinsics,
    response: JsValue,
    body: Option<JsValue>,
    route_before: bool,
    limits: &PrerenderLimits,
    remaining: &mut usize,
) -> Result<ResponseRecord> {
    let before = web_fetch::response_snapshot(&response)?;
    let initial_route = if route_before {
        route_id(&before.headers)
            .map(|id| intrinsics.decode(s, &id))
            .transpose()?
    } else {
        None
    };
    let bytes = response_bytes(s, Some(&response), body, limits, remaining)?;
    let snapshot = web_fetch::response_snapshot(&response)?;
    let route_id = if route_before {
        initial_route
    } else {
        route_id(&snapshot.headers)
            .map(|id| intrinsics.decode(s, &id))
            .transpose()?
    };
    Ok(ResponseRecord {
        status: snapshot.status,
        status_text: snapshot.status_text,
        headers: snapshot.headers,
        body: bytes,
        route_id,
    })
}
fn response_bytes(
    s: &mut NativeSession<'_>,
    response: Option<&JsValue>,
    body: Option<JsValue>,
    limits: &PrerenderLimits,
    remaining: &mut usize,
) -> Result<Vec<u8>> {
    let bytes = if let Some(body) = body {
        if let Some(text) = body.as_string() {
            if text.len() > limits.response_bytes {
                return Err(invalid("Dependency string exceeds response limit"));
            }
            text.to_std_string_lossy().into_bytes()
        } else {
            web_streams::copy_uint8_chunk(&body, s.context())?
        }
    } else if let Some(body) =
        web_fetch::response_snapshot(response.ok_or_else(|| invalid("Missing fallback Response"))?)?
            .body
    {
        if body.unusable()? {
            return Err(invalid("Response body is locked or disturbed"));
        }
        let pending = web_streams::fully_read_bytes(&body.stream().into(), s.context())?;
        let bytes = s.settle(pending.into())?;
        web_streams::copy_uint8_chunk(&bytes, s.context())?
    } else {
        Vec::new()
    };
    if bytes.len() > limits.response_bytes || bytes.len() > *remaining {
        return Err(invalid("Prerender response bytes exceed limit"));
    }
    *remaining -= bytes.len();
    Ok(bytes)
}

#[cfg(test)]
#[path = "prerender_session_tests.rs"]
mod tests;
