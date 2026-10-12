//! Trusted native prerender server lifecycle, separate from metadata inspection.
//! Matches Kit 2.69.2 and Kit 3.0.1 postbuild/prerender.js initialization order.
//! Call only inside Runtime::native_operation; no host APIs are synthesized.
//! The caller provides a fresh runtime, actual manifest, snapshot read function,
//! branded Request and complete framework dispatch options (including maps).
use boa_engine::{
    Context, JsError, JsNativeError, JsObject, JsResult, JsString, JsValue,
    builtins::promise::PromiseState,
    object::{
        ObjectInitializer,
        builtins::{JsMap, JsPromise},
    },
    property::Attribute,
    realm::Realm,
};
use std::collections::BTreeMap;
use ts_js_runtime::{RuntimeError, native_session::NativeSession, web_fetch};

type Result<T> = std::result::Result<T, RuntimeError>;
fn invalid(message: impl Into<String>) -> RuntimeError {
    RuntimeError {
        code: "kit_dispatch_input".into(),
        message: message.into(),
    }
}
// Retain engine errors until the caller selects the context or session boundary.
enum DispatchError {
    Input(String),
    Engine(JsError),
}
impl From<JsError> for DispatchError {
    fn from(error: JsError) -> Self {
        Self::Engine(error)
    }
}
impl DispatchError {
    fn input(message: impl Into<String>) -> Self {
        Self::Input(message.into())
    }
    fn runtime(self) -> RuntimeError {
        match self {
            Self::Input(message) => invalid(message),
            Self::Engine(error) => error.into(),
        }
    }
    fn engine(self) -> JsError {
        match self {
            Self::Input(message) => JsNativeError::typ().with_message(message).into(),
            Self::Engine(error) => error,
        }
    }
}
fn dispatch_object(value: JsValue, label: &str) -> std::result::Result<JsObject, DispatchError> {
    value
        .as_object()
        .ok_or_else(|| DispatchError::input(format!("{label} must be an object")))
}
struct LifecycleClaim;

/// Metadata initialization must claim the session before loading its modules.
/// Prerender initialization refuses a realm already used for metadata or render.
pub fn claim_metadata_session(session: &mut NativeSession<'_>) -> Result<()> {
    claim(session)
}
fn claim(session: &mut NativeSession<'_>) -> Result<()> {
    if session.context().get_data::<LifecycleClaim>().is_some() {
        return Err(invalid(
            "Kit lifecycle already initialized: use a fresh runtime for rendering",
        ));
    }
    session.context().insert_data(LifecycleClaim);
    Ok(())
}
pub enum RenderProfile {
    Kit2 {
        internal_entry: String,
        environment_entry: String,
        public_prefix: String,
        private_prefix: String,
    },
    Kit3,
}
pub struct RenderInput {
    pub profile: RenderProfile,
    pub server_entry: String,
    pub manifest: JsObject,
    pub environment: BTreeMap<String, String>,
    /// Native snapshot-backed function returning ReadableStream (or its promise).
    pub read: JsValue,
}
/// Owns rooted engine values. Never transfer this handle to another realm.
pub struct Renderer {
    realm: Realm,
    state: RenderState,
}
enum RenderState {
    Kit2 {
        constructor: JsObject,
        manifest: JsObject,
        options: JsObject,
    },
    Kit3 {
        init: JsValue,
        respond: JsValue,
    },
    Started {
        receiver: JsValue,
        respond: JsValue,
    },
    Failed,
}
/// The response body is deliberately unconsumed. Dependency response/body
/// records remain their actual engine values, suitable for a future crawler.
pub struct DispatchResult {
    pub response: JsObject,
    pub dependencies: JsMap,
}
/// Started response computation, without draining any engine jobs. The promise
/// preserves actual rejection values and adopts thenables through native
/// Promise.resolve. Dependencies remain the caller's live Map.
pub struct PendingDispatch {
    realm: Realm,
    pub response: JsPromise,
    pub dependencies: JsMap,
}
impl PendingDispatch {
    /// Normalize an already settled response without running jobs. Consumes the
    /// receipt; a pending promise is an error, never an implicit wait.
    pub fn finish(self, session: &mut NativeSession<'_>) -> Result<DispatchResult> {
        self.finish_inner(session.context())
            .map_err(DispatchError::runtime)
    }
    /// Completion from a native promise continuation. No job drain or error
    /// serialization; a rejected promise throws its original opaque reason.
    pub fn finish_context(self, context: &mut Context) -> JsResult<DispatchResult> {
        self.finish_inner(context).map_err(DispatchError::engine)
    }
    fn finish_inner(
        self,
        context: &mut Context,
    ) -> std::result::Result<DispatchResult, DispatchError> {
        if &self.realm != context.realm() {
            return Err(DispatchError::input(
                "Pending dispatch belongs to a different runtime realm",
            ));
        }
        let response = match self.response.state() {
            PromiseState::Fulfilled(response) => response,
            PromiseState::Rejected(reason) => return Err(JsError::from_opaque(reason).into()),
            PromiseState::Pending => {
                return Err(DispatchError::input("Response promise has not settled"));
            }
        };
        if !web_fetch::is_response(&response) {
            return Err(DispatchError::input(
                "Server.respond did not return a native Response",
            ));
        }
        Ok(DispatchResult {
            response: dispatch_object(response, "Response")?,
            dependencies: self.dependencies,
        })
    }
}
fn get(session: &mut NativeSession<'_>, object: &JsObject, name: &str) -> Result<JsValue> {
    Ok(object.get(JsString::from(name), session.context())?)
}
fn object(value: JsValue, label: &str) -> Result<JsObject> {
    value
        .as_object()
        .ok_or_else(|| invalid(format!("{label} must be an object")))
}
fn callable(value: JsValue, label: &str) -> Result<JsValue> {
    if value.is_callable() {
        Ok(value)
    } else {
        Err(invalid(format!("{label} must be callable")))
    }
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
fn filtered(
    env: &BTreeMap<String, String>,
    allowed: &str,
    excluded: &str,
) -> BTreeMap<String, String> {
    env.iter()
        .filter(|(name, _)| {
            name.starts_with(allowed) && (excluded.is_empty() || !name.starts_with(excluded))
        })
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect()
}
impl Renderer {
    /// Deliberately runs application init hooks. Unlike metadata configuration,
    /// this may reject because application-required host capabilities are absent.
    pub fn initialize(session: &mut NativeSession<'_>, input: RenderInput) -> Result<Self> {
        let mut renderer = Self::configure(session, input)?;
        renderer.start(session)?;
        Ok(renderer)
    }
    /// Configure module-level state without constructing Kit 2 Server or running
    /// init hooks. Remote-work discovery can happen before an explicit start.
    pub fn configure(session: &mut NativeSession<'_>, input: RenderInput) -> Result<Self> {
        if input.server_entry.is_empty() {
            return Err(invalid("Explicit server entry required"));
        }
        callable(input.read.clone(), "Server snapshot read")?;
        claim(session)?;
        let environment =
            JsValue::from_json(&serde_json::json!(input.environment), session.context())?;
        let state = match input.profile {
            RenderProfile::Kit2 {
                internal_entry,
                environment_entry,
                public_prefix,
                private_prefix,
            } => {
                let internal = session.namespace(&internal_entry)?;
                invoke(session, &internal, "set_building", &[])?;
                invoke(session, &internal, "set_prerendering", &[])?;
                let env_module = session.namespace(&environment_entry)?;
                invoke(
                    session,
                    &env_module,
                    "set_env",
                    std::slice::from_ref(&environment),
                )?;
                let server_module = session.namespace(&input.server_entry)?;
                let constructor = object(
                    get(session, &server_module, "Server")?,
                    "Server constructor",
                )?;
                if !constructor.is_constructor() {
                    return Err(invalid("Server export must be a constructor"));
                }
                let private = JsValue::from_json(
                    &serde_json::json!(filtered(
                        &input.environment,
                        &private_prefix,
                        &public_prefix
                    )),
                    session.context(),
                )?;
                let public = JsValue::from_json(
                    &serde_json::json!(filtered(
                        &input.environment,
                        &public_prefix,
                        &private_prefix
                    )),
                    session.context(),
                )?;
                invoke(session, &internal, "set_private_env", &[private])?;
                invoke(session, &internal, "set_public_env", &[public])?;
                invoke(
                    session,
                    &internal,
                    "set_manifest",
                    &[input.manifest.clone().into()],
                )?;
                invoke(
                    session,
                    &internal,
                    "set_read_implementation",
                    std::slice::from_ref(&input.read),
                )?;
                let options = ObjectInitializer::new(session.context())
                    .property(JsString::from("env"), environment, Attribute::all())
                    .property(JsString::from("read"), input.read, Attribute::all())
                    .build();
                RenderState::Kit2 {
                    constructor,
                    manifest: input.manifest,
                    options,
                }
            }
            RenderProfile::Kit3 => {
                let module = session.namespace(&input.server_entry)?;
                let options = ObjectInitializer::new(session.context())
                    .property(JsString::from("building"), true, Attribute::all())
                    .property(JsString::from("prerendering"), true, Attribute::all())
                    .property(JsString::from("manifest"), input.manifest, Attribute::all())
                    .property(JsString::from("env"), environment, Attribute::all())
                    .property(JsString::from("read"), input.read, Attribute::all())
                    .build();
                let pending = invoke(session, &module, "configure", &[options.into()])?;
                let instance = object(session.settle(pending)?, "configure result")?;
                // Upstream destructures these ESM functions, so `this` is undefined.
                let init = callable(get(session, &instance, "init")?, "Configured init")?;
                let respond = callable(get(session, &instance, "respond")?, "Configured respond")?;
                RenderState::Kit3 { init, respond }
            }
        };
        Ok(Self {
            realm: session.context().realm().clone(),
            state,
        })
    }
    /// Await init exactly once. Repeated successful starts are idempotent.
    pub fn start(&mut self, session: &mut NativeSession<'_>) -> Result<()> {
        self.same_realm(session)?;
        if matches!(self.state, RenderState::Started { .. }) {
            return Ok(());
        }
        let state = std::mem::replace(&mut self.state, RenderState::Failed);
        let (receiver, respond) = match state {
            RenderState::Kit2 {
                constructor,
                manifest,
                options,
            } => {
                let instance =
                    constructor.construct(&[manifest.into()], None, session.context())?;
                let init = get(session, &instance, "init")?;
                let receiver: JsValue = instance.clone().into();
                let pending = session.call(&init, &receiver, &[options.into()])?;
                session.settle(pending)?;
                let respond = callable(get(session, &instance, "respond")?, "Server.respond")?;
                (receiver, respond)
            }
            RenderState::Kit3 { init, respond } => {
                let pending = session.call(&init, &JsValue::undefined(), &[])?;
                session.settle(pending)?;
                (JsValue::undefined(), respond)
            }
            RenderState::Failed => return Err(invalid("Server initialization previously failed")),
            RenderState::Started { .. } => unreachable!(),
        };
        self.state = RenderState::Started { receiver, respond };
        Ok(())
    }
    fn same_realm(&self, session: &mut NativeSession<'_>) -> Result<()> {
        if &self.realm != session.context().realm() {
            return Err(invalid("Renderer belongs to a different runtime realm"));
        }
        Ok(())
    }
    /// Options are actual engine objects, with prerendering.dependencies a real
    /// Map. The caller supplies version-specific remote_responses/resolved_route_ids,
    /// read/getClientAddress/emulator; this seam never invents their semantics.
    pub fn dispatch(
        &self,
        session: &mut NativeSession<'_>,
        request: JsValue,
        options: JsObject,
    ) -> Result<DispatchResult> {
        let pending = self.dispatch_pending(session, request, options)?;
        session.settle(pending.response.clone().into())?;
        pending.finish(session)
    }
    /// Start respond synchronously, retaining its native promise and dependency
    /// Map. No jobs are drained, allowing the caller to start other work before
    /// awaiting responses. A synchronous application throw becomes a rejected
    /// promise, as it does in Kit's async visit; validation errors remain errors.
    pub fn dispatch_pending(
        &self,
        session: &mut NativeSession<'_>,
        request: JsValue,
        options: JsObject,
    ) -> Result<PendingDispatch> {
        self.dispatch_pending_inner(session.context(), request, options)
            .map_err(DispatchError::runtime)
    }
    /// Context-only equivalent for native promise continuations. It neither
    /// constructs a NativeSession nor borrows its enclosing Runtime. Validation
    /// errors are TypeErrors; application errors remain actual JS values.
    pub fn dispatch_pending_context(
        &self,
        context: &mut Context,
        request: JsValue,
        options: JsObject,
    ) -> JsResult<PendingDispatch> {
        self.dispatch_pending_inner(context, request, options)
            .map_err(DispatchError::engine)
    }
    fn dispatch_pending_inner(
        &self,
        context: &mut Context,
        request: JsValue,
        options: JsObject,
    ) -> std::result::Result<PendingDispatch, DispatchError> {
        if &self.realm != context.realm() {
            return Err(DispatchError::input(
                "Renderer belongs to a different runtime realm",
            ));
        }
        let RenderState::Started { receiver, respond } = &self.state else {
            return Err(DispatchError::input(
                "Server must be started before dispatch",
            ));
        };
        if !web_fetch::is_request(&request) {
            return Err(DispatchError::input("Dispatch requires a native Request"));
        }
        let prerendering = dispatch_object(
            options.get(JsString::from("prerendering"), context)?,
            "prerendering options",
        )?;
        let dependencies = JsMap::from_object(dispatch_object(
            prerendering.get(JsString::from("dependencies"), context)?,
            "prerender dependencies",
        )?)?;
        // Avoid NativeSession::call's error serialization: a thrown object must
        // remain the same rejection value for native promise continuations.
        let returned = respond.as_callable().expect("validated respond").call(
            receiver,
            &[request, options.into()],
            context,
        );
        let response = match returned {
            Ok(value) => match JsPromise::resolve(value, context) {
                Ok(promise) => promise,
                Err(error) => JsPromise::reject(error, context)?,
            },
            Err(error) => JsPromise::reject(error, context)?,
        };
        Ok(PendingDispatch {
            realm: self.realm.clone(),
            response,
            dependencies,
        })
    }
}

#[cfg(test)]
#[path = "server_dispatch_tests.rs"]
mod tests;
