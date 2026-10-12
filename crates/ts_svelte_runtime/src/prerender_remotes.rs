//! Native remote prerender discovery/input evaluation from SvelteKit's MIT
//! core/postbuild/prerender.js (Kit 2.69.2 and Kit 3.0.1).
//! Discover after server configuration, before init; evaluate after init.
//! The actual snapshot's stringify_remote_arg function is supplied explicitly:
//! no JSON conversion, substitute serializer, handler execution or implicit I/O.
use crate::metadata::MetadataProfile;
use boa_engine::value::PreferredType;
use boa_engine::{
    Context, JsError, JsNativeError, JsObject, JsString, JsSymbol, JsValue, NativeFunction,
    builtins::object::OrdinaryObject,
    object::builtins::{JsArray, JsPromise},
    realm::Realm,
};
use boa_gc::{Finalize, Trace};
use ts_js_runtime::{RuntimeError, native_session::NativeSession};

type Result<T> = std::result::Result<T, RuntimeError>;
const MAX_FUNCTIONS: usize = 16_384;
const MAX_EXPORTS: usize = 65_536;
const MAX_REQUESTS: usize = 65_536;
const MAX_PATH: usize = 1024 * 1024;
const MAX_PATH_BYTES: usize = 64 * 1024 * 1024;
fn invalid(message: impl Into<String>) -> RuntimeError {
    RuntimeError {
        code: "kit_remote_prerender_input".into(),
        message: message.into(),
    }
}
fn property(s: &mut NativeSession<'_>, value: &JsValue, name: &str) -> Result<JsValue> {
    Ok(value
        .to_object(s.context())?
        .get(JsString::from(name), s.context())?)
}
fn optional(s: &mut NativeSession<'_>, value: &JsValue, name: &str) -> Result<JsValue> {
    if value.is_null_or_undefined() {
        Ok(JsValue::undefined())
    } else {
        property(s, value, name)
    }
}
fn object(value: JsValue, label: &str) -> Result<JsObject> {
    value
        .as_object()
        .ok_or_else(|| invalid(format!("{label} must be an object")))
}
fn values(
    s: &mut NativeSession<'_>,
    value: JsValue,
    remaining: &mut usize,
) -> Result<Vec<JsValue>> {
    let values = OrdinaryObject::values(&JsValue::undefined(), &[value], s.context())?;
    let values = JsArray::from_object(object(values, "Object.values result")?)?;
    let len = usize::try_from(values.length(s.context())?)
        .map_err(|_| invalid("Remote exports exceed bounds"))?;
    if len > *remaining {
        return Err(invalid("Remote exports exceed bounds"));
    }
    *remaining -= len;
    (0..len)
        .map(|i| Ok(values.get(i as u64, s.context())?))
        .collect()
}
/// Capture before evaluating application modules. Native Object.values is used
/// directly, while encodeURI and Error identity are retained from this realm.
#[derive(Clone)]
pub struct RemotePrerenderIntrinsics {
    realm: Realm,
    encode_uri: JsValue,
    error_constructor: JsValue,
}
impl RemotePrerenderIntrinsics {
    pub fn capture(s: &mut NativeSession<'_>) -> Result<Self> {
        let global = s.context().global_object();
        let encode_uri = global.get(JsString::from("encodeURI"), s.context())?;
        if !encode_uri.is_callable() {
            return Err(invalid("Native encodeURI is unavailable"));
        }
        let error_constructor = s
            .context()
            .intrinsics()
            .constructors()
            .error()
            .constructor()
            .into();
        Ok(Self {
            realm: s.context().realm().clone(),
            encode_uri,
            error_constructor,
        })
    }
    fn check(&self, s: &mut NativeSession<'_>) -> Result<()> {
        if &self.realm != s.context().realm() {
            Err(invalid("Remote prerender handle belongs to another realm"))
        } else {
            Ok(())
        }
    }
    pub fn discover(
        &self,
        s: &mut NativeSession<'_>,
        manifest: &JsObject,
        profile: MetadataProfile,
    ) -> Result<RemotePrerenders> {
        self.check(s)?;
        let manifest: JsValue = manifest.clone().into();
        let container = if profile.is_kit3() {
            manifest
        } else {
            property(s, &manifest, "_")?
        };
        let remotes = property(s, &container, "remotes")?;
        let mut remaining = MAX_EXPORTS;
        let loaders = values(s, remotes, &mut remaining)?;
        let mut functions = vec![];
        for loader in loaders {
            let pending = s.call(&loader, &JsValue::undefined(), &[])?;
            let module = s.settle(pending)?;
            let exports = property(s, &module, "default")?;
            for function in values(s, exports, &mut remaining)? {
                let internals = optional(s, &function, "__")?;
                let kind = optional(s, &internals, "type")?;
                if kind.as_string().is_some_and(|kind| kind == "prerender") {
                    if functions.len() >= MAX_FUNCTIONS {
                        return Err(invalid("Remote prerender function limit exceeded"));
                    }
                    // Upstream tests fn?.__?.type and then pushes fn.__: retain
                    // that second property read instead of caching the first.
                    functions.push(property(s, &function, "__")?);
                }
            }
        }
        Ok(RemotePrerenders {
            intrinsics: self.clone(),
            profile,
            functions,
        })
    }
}
/// Functions from an explicit module namespace, evaluated inside this runtime.
/// Kit 2 transport is the actual (await internal.get_hooks()).transport ?? {}.
/// Kit 3 formatter is the snapshot/host's real fix_stack_trace implementation;
/// it receives an actual thrown Error, not a JSON approximation.
pub enum RemoteArgumentEncoding {
    Kit2 {
        stringify: JsValue,
        transport: JsValue,
    },
    Kit3 {
        stringify: JsValue,
        fix_stack_trace: JsValue,
    },
}
pub struct RemotePrerenders {
    intrinsics: RemotePrerenderIntrinsics,
    profile: MetadataProfile,
    functions: Vec<JsValue>,
}
#[derive(Clone, Debug)]
pub struct RemotePrerenderRequest {
    /// Discovery order, including duplicate functions. No deduplication occurs.
    pub function_index: usize,
    /// Actual metadata reads and input value, still rooted engine values.
    pub id: JsValue,
    pub has_arg: JsValue,
    /// None for no-argument functions; Some(undefined) is a real supplied input.
    pub argument: Option<JsValue>,
    pub decoded_path: String,
    pub encoded_path: String,
}
impl RemotePrerenders {
    pub fn len(&self) -> usize {
        self.functions.len()
    }
    pub fn is_empty(&self) -> bool {
        self.functions.is_empty()
    }
    /// Consumes the discovery receipt once. Inputs are evaluated in function
    /// order, then iterated synchronously using the actual Symbol.iterator.
    /// Call after Renderer.start; this method neither starts nor invokes handlers.
    pub fn into_requests(
        self,
        s: &mut NativeSession<'_>,
        remote_prefix: &str,
        encoding: RemoteArgumentEncoding,
    ) -> Result<Vec<RemotePrerenderRequest>> {
        self.intrinsics.check(s)?;
        if remote_prefix.len() > MAX_PATH
            || !remote_prefix.starts_with('/')
            || !remote_prefix.ends_with('/')
        {
            return Err(invalid(
                "Explicit remote URL prefix must start and end with slash",
            ));
        }
        let stringify = match &encoding {
            RemoteArgumentEncoding::Kit2 { stringify, .. } if !self.profile.is_kit3() => stringify,
            RemoteArgumentEncoding::Kit3 {
                stringify,
                fix_stack_trace,
            } if self.profile.is_kit3() => {
                if !fix_stack_trace.is_callable() {
                    return Err(invalid("Kit3 fix_stack_trace must be callable"));
                }
                stringify
            }
            _ => return Err(invalid("Remote argument encoding profile mismatch")),
        };
        if !stringify.is_callable() {
            return Err(invalid("Explicit stringify_remote_arg must be callable"));
        }
        let mut output = vec![];
        let mut remaining = MAX_PATH_BYTES;
        for (function_index, internals) in self.functions.into_iter().enumerate() {
            // Kit3 invokes inputs even for no-argument functions, before reading
            // has_arg. Kit2 reads has_arg first and skips inputs for falsy values.
            let (has_arg, inputs) = if self.profile.is_kit3() {
                let RemoteArgumentEncoding::Kit3 {
                    fix_stack_trace, ..
                } = &encoding
                else {
                    unreachable!()
                };
                let inputs = kit3_inputs(
                    s,
                    &internals,
                    &self.intrinsics.error_constructor,
                    fix_stack_trace,
                )?;
                (property(s, &internals, "has_arg")?, inputs)
            } else {
                let has_arg = property(s, &internals, "has_arg")?;
                let inputs = if has_arg.to_boolean() {
                    inputs(s, &internals)?
                } else {
                    JsValue::undefined()
                };
                (has_arg, inputs)
            };
            if !has_arg.to_boolean() {
                output.push(request(
                    s,
                    &self.intrinsics,
                    remote_prefix,
                    &internals,
                    function_index,
                    &has_arg,
                    None,
                    &encoding,
                    &mut remaining,
                )?);
                if output.len() > MAX_REQUESTS {
                    return Err(invalid("Remote request count exceeds bounds"));
                }
                continue;
            }
            if inputs.is_null_or_undefined() {
                continue;
            }
            let iterator_method = inputs
                .to_object(s.context())?
                .get(JsSymbol::iterator(), s.context())?;
            let iterator = object(
                s.call(&iterator_method, &inputs, &[])?,
                "Remote input iterator",
            )?;
            let next = iterator.get(JsString::from("next"), s.context())?;
            loop {
                // Abrupt iterator next/done/value operations do not close the
                // iterator; abrupt loop-body operations do (ECMAScript for-of).
                let step = object(
                    s.call(&next, &iterator.clone().into(), &[])?,
                    "Iterator result",
                )?;
                if step.get(JsString::from("done"), s.context())?.to_boolean() {
                    break;
                }
                let argument = step.get(JsString::from("value"), s.context())?;
                let item = if output.len() >= MAX_REQUESTS {
                    Err(invalid("Remote request count exceeds bounds"))
                } else {
                    request(
                        s,
                        &self.intrinsics,
                        remote_prefix,
                        &internals,
                        function_index,
                        &has_arg,
                        Some(argument),
                        &encoding,
                        &mut remaining,
                    )
                };
                match item {
                    Ok(item) => output.push(item),
                    Err(error) => {
                        close_after_throw(s, &iterator);
                        return Err(error);
                    }
                }
            }
        }
        Ok(output)
    }
}
fn inputs(s: &mut NativeSession<'_>, internals: &JsValue) -> Result<JsValue> {
    let function = property(s, internals, "inputs")?;
    let pending = if function.is_null_or_undefined() {
        JsValue::undefined()
    } else {
        s.call(&function, internals, &[])?
    };
    s.settle(pending)
}
#[derive(Trace, Finalize)]
struct StackCapture {
    error_constructor: JsValue,
    fix: JsValue,
}
fn format_error(
    reason: JsValue,
    capture: &StackCapture,
    context: &mut Context,
) -> boa_engine::JsResult<JsValue> {
    if reason.instance_of(&capture.error_constructor, context)? {
        capture
            .fix
            .as_callable()
            .ok_or_else(|| JsNativeError::typ().with_message("Invalid stack formatter"))?
            .call(
                &JsValue::undefined(),
                std::slice::from_ref(&reason),
                context,
            )?;
    }
    Err(JsError::from_opaque(reason))
}
fn kit3_inputs(
    s: &mut NativeSession<'_>,
    internals: &JsValue,
    error_constructor: &JsValue,
    fix: &JsValue,
) -> Result<JsValue> {
    let capture = StackCapture {
        error_constructor: error_constructor.clone(),
        fix: fix.clone(),
    };
    // Keep opaque JS errors until Kit's formatter has seen them. The generic
    // native-session boundary serializes failures only after this operation.
    let pending = (|| -> boa_engine::JsResult<JsValue> {
        let function = internals
            .to_object(s.context())?
            .get(JsString::from("inputs"), s.context())?;
        if function.is_null_or_undefined() {
            return Ok(JsValue::undefined());
        }
        function
            .as_callable()
            .ok_or_else(|| JsNativeError::typ().with_message("Remote inputs must be callable"))?
            .call(internals, &[], s.context())
    })();
    let pending = match pending {
        Ok(pending) => pending,
        Err(error) => {
            let reason = error.into_opaque(s.context())?;
            return Ok(format_error(reason, &capture, s.context())?);
        }
    };
    let pending = JsPromise::resolve(pending, s.context())?;
    let rejected = NativeFunction::from_copy_closure_with_captures(
        |_, args, capture, context| {
            format_error(
                args.first().cloned().unwrap_or_else(JsValue::undefined),
                capture,
                context,
            )
        },
        capture,
    )
    .to_js_function(s.context().realm());
    let pending = pending.then_intrinsic(None, Some(rejected), s.context())?;
    s.settle(pending.into())
}
fn close_after_throw(s: &mut NativeSession<'_>, iterator: &JsObject) {
    // IteratorClose preserves the existing throw over return getter/call errors.
    if let Ok(method) = iterator.get(JsString::from("return"), s.context())
        && !method.is_null_or_undefined()
    {
        let _ = s.call(&method, &iterator.clone().into(), &[]);
    }
}
// Mirrors Kit's request construction inputs one-to-one.
#[allow(clippy::too_many_arguments)]
fn request(
    s: &mut NativeSession<'_>,
    intrinsics: &RemotePrerenderIntrinsics,
    prefix: &str,
    internals: &JsValue,
    index: usize,
    has_arg: &JsValue,
    argument: Option<JsValue>,
    encoding: &RemoteArgumentEncoding,
    remaining: &mut usize,
) -> Result<RemotePrerenderRequest> {
    let id = property(s, internals, "id")?;
    let id_text = id
        .to_primitive(s.context(), PreferredType::Default)?
        .to_string(s.context())?;
    let prefix = JsString::from(prefix);
    let mut decoded = JsString::concat(prefix.as_str(), id_text.as_str());
    if let Some(argument) = &argument {
        // Evaluate the ID coercion before invoking the serializer, as the left
        // operand of Kit's string concatenation does.
        let value = match encoding {
            RemoteArgumentEncoding::Kit2 {
                stringify,
                transport,
            } => s.call(
                stringify,
                &JsValue::undefined(),
                &[argument.clone(), transport.clone()],
            )?,
            RemoteArgumentEncoding::Kit3 { stringify, .. } => s.call(
                stringify,
                &JsValue::undefined(),
                std::slice::from_ref(argument),
            )?,
        };
        let suffix = value
            .to_primitive(s.context(), PreferredType::Default)?
            .to_string(s.context())?;
        let slash = JsString::from("/");
        decoded = JsString::concat_array(&[decoded.as_str(), slash.as_str(), suffix.as_str()]);
    }
    if decoded.len() > MAX_PATH {
        return Err(invalid("Remote request path exceeds bounds"));
    }
    let encoded = s.call(
        &intrinsics.encode_uri,
        &JsValue::undefined(),
        &[decoded.clone().into()],
    )?;
    let encoded = encoded
        .as_string()
        .ok_or_else(|| invalid("encodeURI did not return a string"))?
        .to_std_string()
        .map_err(|_| invalid("Encoded URI contains invalid UTF-16"))?;
    let decoded = decoded
        .to_std_string()
        .map_err(|_| invalid("Remote URI contains invalid UTF-16"))?;
    let size = encoded
        .len()
        .checked_add(decoded.len())
        .ok_or_else(|| invalid("Remote path bytes overflow"))?;
    if encoded.len() > MAX_PATH || size > *remaining {
        return Err(invalid("Remote request paths exceed byte bounds"));
    }
    *remaining -= size;
    Ok(RemotePrerenderRequest {
        function_index: index,
        id,
        has_arg: has_arg.clone(),
        argument,
        decoded_path: decoded,
        encoded_path: encoded,
    })
}

#[cfg(test)]
#[path = "prerender_remotes_tests.rs"]
mod tests;

/// The real helper exports and (Kit2) internal module method/receiver. Hooks are
/// invoked by the producer, not pre-evaluated by its host.
pub enum RemoteProducerEncoding {
    Kit2 {
        stringify: JsValue,
        get_hooks: JsValue,
        receiver: JsValue,
    },
    Kit3 {
        stringify: JsValue,
        fix_stack_trace: JsValue,
    },
}
/// Called synchronously for each encoded request. An observer can start other
/// native response promises using this Context, but must not run/drain jobs.
/// Returning an opaque JS error rejects production and closes an active for-of
/// iterator. Return does not await any response work started by the observer.
pub trait RemotePrerenderObserver {
    fn request(
        &self,
        request: RemotePrerenderRequest,
        context: &mut Context,
    ) -> boa_engine::JsResult<()>;
}

struct Producer {
    key: u64,
    intrinsics: RemotePrerenderIntrinsics,
    kit3: bool,
    functions: Vec<JsValue>,
    index: usize,
    has_arg: JsValue,
    stringify: JsValue,
    transport: JsValue,
    fix: JsValue,
    prefix: String,
    remaining: usize,
    count: usize,
    observer: std::rc::Rc<dyn RemotePrerenderObserver>,
    settle: boa_engine::builtins::promise::ResolvingFunctions,
}
// JsValues held by ordinary Context host data are engine roots. Only integer
// keys/stages are captured by traced native reaction closures, never Rust Rc
// containers hiding JS references from Boa's collector.
#[derive(Default)]
struct Producers {
    next: u64,
    active: usize,
    states: std::collections::BTreeMap<u64, Producer>,
}
#[derive(Clone, Copy)]
enum ProducerStage {
    Hooks,
    Inputs,
}
enum ProducerNext {
    Await(JsPromise, ProducerStage),
    Done,
}
fn producer_error(message: &str) -> JsError {
    JsNativeError::typ().with_message(message.to_owned()).into()
}
fn producer_get(
    value: &JsValue,
    name: &str,
    context: &mut Context,
) -> boa_engine::JsResult<JsValue> {
    value.to_object(context)?.get(JsString::from(name), context)
}
fn producer_call(
    function: &JsValue,
    receiver: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> boa_engine::JsResult<JsValue> {
    function
        .as_callable()
        .ok_or_else(|| producer_error("Remote producer requires a callable value"))?
        .call(receiver, args, context)
}
fn producer_take(key: u64, context: &mut Context) -> boa_engine::JsResult<Producer> {
    let mut registry = context.remove_data::<Producers>().unwrap_or_default();
    let state = registry.states.remove(&key);
    context.insert_data(*registry);
    state.ok_or_else(|| producer_error("Unknown native remote producer"))
}
fn producer_put(state: Producer, context: &mut Context) {
    let mut registry = context.remove_data::<Producers>().unwrap_or_default();
    registry.states.insert(state.key, state);
    context.insert_data(*registry);
}
fn producer_release(context: &mut Context) {
    let mut registry = context.remove_data::<Producers>().unwrap_or_default();
    registry.active -= 1;
    context.insert_data(*registry);
}
fn producer_reject(
    state: Producer,
    reason: JsValue,
    context: &mut Context,
) -> boa_engine::JsResult<()> {
    producer_release(context);
    state
        .settle
        .reject
        .call(&JsValue::undefined(), &[reason], context)?;
    Ok(())
}
fn producer_fail(
    state: Producer,
    error: JsError,
    context: &mut Context,
) -> boa_engine::JsResult<()> {
    match error.into_opaque(context) {
        Ok(reason) => producer_reject(state, reason, context),
        Err(error) => {
            // A failure materializing a native exception must not retain its
            // producer slot. The surrounding runtime owns this fatal error.
            producer_release(context);
            Err(error)
        }
    }
}

impl RemotePrerenders {
    /// Start the actual async producer without settling promises or draining the
    /// realm. Kit2 calls get_hooks immediately and always awaits it. Kit3 starts
    /// the first inputs call immediately, awaiting even an absent inputs value.
    /// Only host contract errors are synchronous; execution failures reject the
    /// returned promise with the original JS reason (or Kit3 formatter failure).
    pub fn start_producer_context(
        self,
        context: &mut Context,
        remote_prefix: &str,
        encoding: RemoteProducerEncoding,
        observer: std::rc::Rc<dyn RemotePrerenderObserver>,
    ) -> boa_engine::JsResult<JsPromise> {
        if &self.intrinsics.realm != context.realm() {
            return Err(producer_error("Remote producer belongs to another realm"));
        }
        if remote_prefix.len() > MAX_PATH
            || !remote_prefix.starts_with('/')
            || !remote_prefix.ends_with('/')
        {
            return Err(producer_error(
                "Explicit remote URL prefix must start and end with slash",
            ));
        }
        let kit3 = self.profile.is_kit3();
        let (stringify, fix, hooks) = match encoding {
            RemoteProducerEncoding::Kit2 {
                stringify,
                get_hooks,
                receiver,
            } if !kit3 => {
                if !get_hooks.is_callable() {
                    return Err(producer_error("Actual Kit2 get_hooks must be callable"));
                }
                (stringify, JsValue::undefined(), Some((get_hooks, receiver)))
            }
            RemoteProducerEncoding::Kit3 {
                stringify,
                fix_stack_trace,
            } if kit3 => {
                if !fix_stack_trace.is_callable() {
                    return Err(producer_error(
                        "Actual Kit3 fix_stack_trace must be callable",
                    ));
                }
                (stringify, fix_stack_trace, None)
            }
            _ => return Err(producer_error("Remote producer encoding profile mismatch")),
        };
        if !stringify.is_callable() {
            return Err(producer_error(
                "Actual stringify_remote_arg must be callable",
            ));
        }
        let mut registry = context.remove_data::<Producers>().unwrap_or_default();
        let key = if registry.active >= 4096 {
            Err(producer_error("Active remote producer limit exceeded"))
        } else {
            registry
                .next
                .checked_add(1)
                .ok_or_else(|| producer_error("Remote producer identity exhausted"))
        };
        if let Ok(key) = key {
            registry.next = key;
            registry.active += 1;
        }
        context.insert_data(*registry);
        let key = key?;
        let (promise, settle) = JsPromise::new_pending(context);
        let mut state = Producer {
            key,
            intrinsics: self.intrinsics,
            kit3,
            functions: self.functions,
            index: 0,
            has_arg: JsValue::undefined(),
            stringify,
            transport: JsValue::undefined(),
            fix,
            prefix: remote_prefix.into(),
            remaining: MAX_PATH_BYTES,
            count: 0,
            observer,
            settle,
        };
        let next = if let Some((get_hooks, receiver)) = hooks {
            producer_call(&get_hooks, &receiver, &[], context)
                .and_then(|value| JsPromise::resolve(value, context))
                .map(|promise| ProducerNext::Await(promise, ProducerStage::Hooks))
        } else {
            producer_functions(&mut state, context)
        };
        producer_continue(state, next, context)?;
        Ok(promise)
    }
}
fn producer_continue(
    state: Producer,
    next: boa_engine::JsResult<ProducerNext>,
    context: &mut Context,
) -> boa_engine::JsResult<()> {
    match next {
        Err(error) => producer_fail(state, error, context),
        Ok(ProducerNext::Done) => {
            producer_release(context);
            state
                .settle
                .resolve
                .call(&JsValue::undefined(), &[JsValue::undefined()], context)?;
            Ok(())
        }
        Ok(ProducerNext::Await(promise, stage)) => {
            producer_schedule(state, promise, stage, context)
        }
    }
}
fn producer_schedule(
    state: Producer,
    promise: JsPromise,
    stage: ProducerStage,
    context: &mut Context,
) -> boa_engine::JsResult<()> {
    let key = state.key;
    let stage = match stage {
        ProducerStage::Hooks => 0_u8,
        ProducerStage::Inputs => 1,
    };
    producer_put(state, context);
    let fulfilled = NativeFunction::from_copy_closure_with_captures(
        |_, args, &(key, stage), context| {
            let mut state = producer_take(key, context)?;
            let value = args.first().cloned().unwrap_or_else(JsValue::undefined);
            let next = (|| {
                if stage == 0 {
                    // Nullish fallback is an actual fresh ordinary object, not
                    // JSON or an object created in another realm.
                    let transport = producer_get(&value, "transport", context)?;
                    state.transport = if transport.is_null_or_undefined() {
                        JsObject::with_object_proto(context.intrinsics()).into()
                    } else {
                        transport
                    };
                } else {
                    if state.kit3 {
                        state.has_arg =
                            producer_get(&state.functions[state.index], "has_arg", context)?;
                    }
                    producer_inputs(&mut state, value, context)?;
                    state.index += 1;
                }
                producer_functions(&mut state, context)
            })();
            producer_continue(state, next, context)?;
            Ok(JsValue::undefined())
        },
        (key, stage),
    )
    .to_js_function(context.realm());
    let rejected = NativeFunction::from_copy_closure_with_captures(
        |_, args, &(key, stage), context| {
            let state = producer_take(key, context)?;
            let reason = args.first().cloned().unwrap_or_else(JsValue::undefined);
            if stage == 1 && state.kit3 {
                let capture = StackCapture {
                    error_constructor: state.intrinsics.error_constructor.clone(),
                    fix: state.fix.clone(),
                };
                let error =
                    format_error(reason, &capture, context).expect_err("formatter always throws");
                producer_fail(state, error, context)?;
            } else {
                producer_reject(state, reason, context)?;
            }
            Ok(JsValue::undefined())
        },
        (key, stage),
    )
    .to_js_function(context.realm());
    if let Err(error) = promise.then_intrinsic(Some(fulfilled), Some(rejected), context) {
        producer_fail(producer_take(key, context)?, error, context)?;
    }
    Ok(())
}
fn producer_functions(
    state: &mut Producer,
    context: &mut Context,
) -> boa_engine::JsResult<ProducerNext> {
    while state.index < state.functions.len() {
        let internals = state.functions[state.index].clone();
        if !state.kit3 {
            state.has_arg = producer_get(&internals, "has_arg", context)?;
            if !state.has_arg.to_boolean() {
                producer_emit(state, None, context)?;
                state.index += 1;
                continue;
            }
        }
        let inputs = (|| {
            let method = producer_get(&internals, "inputs", context)?;
            if method.is_null_or_undefined() {
                Ok(JsValue::undefined())
            } else {
                producer_call(&method, &internals, &[], context)
            }
        })();
        let inputs = match inputs.and_then(|value| JsPromise::resolve(value, context)) {
            Ok(value) => value,
            Err(error) if state.kit3 => {
                let reason = error.into_opaque(context)?;
                let capture = StackCapture {
                    error_constructor: state.intrinsics.error_constructor.clone(),
                    fix: state.fix.clone(),
                };
                return Err(
                    format_error(reason, &capture, context).expect_err("formatter always throws")
                );
            }
            Err(error) => return Err(error),
        };
        return Ok(ProducerNext::Await(inputs, ProducerStage::Inputs));
    }
    Ok(ProducerNext::Done)
}
fn producer_inputs(
    state: &mut Producer,
    inputs: JsValue,
    context: &mut Context,
) -> boa_engine::JsResult<()> {
    if !state.has_arg.to_boolean() {
        return producer_emit(state, None, context);
    }
    let inputs = if inputs.is_null_or_undefined() {
        // `(await inputs?.()) ?? []` is an actual Array. Its inherited iterator
        // remains observable if application code changes Array.prototype.
        JsArray::new(context)?.into()
    } else {
        inputs
    };
    let method = inputs
        .to_object(context)?
        .get(JsSymbol::iterator(), context)?;
    let iterator = producer_call(&method, &inputs, &[], context)?
        .as_object()
        .ok_or_else(|| producer_error("Remote input iterator must be an object"))?;
    let next = iterator.get(JsString::from("next"), context)?;
    loop {
        // Iterator step/value failures do not close; loop-body failures do.
        let step = producer_call(&next, &iterator.clone().into(), &[], context)?
            .as_object()
            .ok_or_else(|| producer_error("Remote input iterator result must be an object"))?;
        if step.get(JsString::from("done"), context)?.to_boolean() {
            return Ok(());
        }
        let argument = step.get(JsString::from("value"), context)?;
        if let Err(error) = producer_emit(state, Some(argument), context) {
            if let Ok(method) = iterator.get(JsString::from("return"), context)
                && !method.is_null_or_undefined()
            {
                let _ = producer_call(&method, &iterator.into(), &[], context);
            }
            return Err(error);
        }
    }
}
fn producer_emit(
    state: &mut Producer,
    argument: Option<JsValue>,
    context: &mut Context,
) -> boa_engine::JsResult<()> {
    if state.count >= MAX_REQUESTS {
        return Err(producer_error("Remote request count exceeds bounds"));
    }
    let id = producer_get(&state.functions[state.index], "id", context)?;
    let id_text = id
        .to_primitive(context, PreferredType::Default)?
        .to_string(context)?;
    let prefix = JsString::from(state.prefix.as_str());
    let mut decoded = JsString::concat(prefix.as_str(), id_text.as_str());
    if let Some(argument) = &argument {
        let args = if state.kit3 {
            vec![argument.clone()]
        } else {
            vec![argument.clone(), state.transport.clone()]
        };
        let suffix = producer_call(&state.stringify, &JsValue::undefined(), &args, context)?
            .to_primitive(context, PreferredType::Default)?
            .to_string(context)?;
        let slash = JsString::from("/");
        decoded = JsString::concat_array(&[decoded.as_str(), slash.as_str(), suffix.as_str()]);
    }
    if decoded.len() > MAX_PATH {
        return Err(producer_error("Remote request path exceeds bounds"));
    }
    let encoded = producer_call(
        &state.intrinsics.encode_uri,
        &JsValue::undefined(),
        &[decoded.clone().into()],
        context,
    )?;
    let encoded = encoded
        .as_string()
        .ok_or_else(|| producer_error("encodeURI did not return a string"))?
        .to_std_string()
        .map_err(|_| producer_error("Encoded URI contains invalid UTF16"))?;
    let decoded = decoded
        .to_std_string()
        .map_err(|_| producer_error("Remote URI contains invalid UTF16"))?;
    let size = encoded
        .len()
        .checked_add(decoded.len())
        .ok_or_else(|| producer_error("Remote path bytes overflow"))?;
    if encoded.len() > MAX_PATH || size > state.remaining {
        return Err(producer_error("Remote request paths exceed byte bounds"));
    }
    state.remaining -= size;
    state.count += 1;
    state.observer.request(
        RemotePrerenderRequest {
            function_index: state.index,
            id,
            has_arg: state.has_arg.clone(),
            argument,
            decoded_path: decoded,
            encoded_path: encoded,
        },
        context,
    )
}

#[cfg(test)]
#[path = "prerender_remote_producer_tests.rs"]
mod producer_tests;
