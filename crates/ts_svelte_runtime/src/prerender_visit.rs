//! Native promise-continuation visitor for SvelteKit's MIT postbuild/prerender.js
//! (2.69.2 and 3.0.1). Queue policy, saving and crawling belong to the observer.
//! This module never drains jobs, evaluates driver JavaScript or performs I/O.
use crate::{
    prerender_session::{DependencyHead, DependencyRecord, ResponseHead, ResponseRecord},
    server_dispatch::{PendingDispatch, Renderer},
};
use boa_engine::{
    Context, JsError, JsNativeError, JsObject, JsResult, JsString, JsValue, NativeFunction,
    builtins::{object::OrdinaryObject, promise::ResolvingFunctions},
    object::builtins::{JsArray, JsArrayBuffer, JsMap, JsMapIterator, JsPromise},
    realm::Realm,
};
use std::{collections::BTreeMap, rc::Rc};
use ts_js_runtime::{web_fetch, web_headers::HeaderList, web_streams};

const MAX_PATH: usize = 1024 * 1024;
const MAX_ACTIVE: usize = 4096;
fn error(message: impl Into<String>) -> JsError {
    JsNativeError::typ().with_message(message.into()).into()
}
fn object(value: JsValue, label: &str) -> JsResult<JsObject> {
    value
        .as_object()
        .ok_or_else(|| error(format!("{label} must be an object")))
}
fn string(value: JsValue, label: &str) -> JsResult<String> {
    let text = value
        .as_string()
        .ok_or_else(|| error(format!("{label} must be a string")))?;
    if text.len() > MAX_PATH {
        return Err(error(format!("{label} exceeds path limit")));
    }
    let text = text
        .to_std_string()
        .map_err(|_| error(format!("{label} contains invalid UTF-16")))?;
    if text.len() > MAX_PATH {
        return Err(error(format!("{label} exceeds path limit")));
    }
    Ok(text)
}
fn get(object: &JsObject, name: &str, context: &mut Context) -> JsResult<JsValue> {
    object.get(JsString::from(name), context)
}
/// Capture in this realm before application modules can overwrite URL/decodeURI.
#[derive(Clone)]
pub struct VisitIntrinsics {
    realm: Realm,
    url: JsObject,
    pathname: JsValue,
    decode_uri: JsValue,
    array_buffer: JsValue,
}
impl VisitIntrinsics {
    pub fn capture(context: &mut Context) -> JsResult<Self> {
        let global = context.global_object();
        let url = object(get(&global, "URL", context)?, "URL")?;
        let prototype = object(get(&url, "prototype", context)?, "URL prototype")?;
        let descriptor = OrdinaryObject::get_own_property_descriptor(
            &JsValue::undefined(),
            &[prototype.into(), JsString::from("pathname").into()],
            context,
        )?;
        let pathname = get(
            &object(descriptor, "URL pathname descriptor")?,
            "get",
            context,
        )?;
        let decode_uri = get(&global, "decodeURI", context)?;
        let response = object(get(&global, "Response", context)?, "Response")?;
        let response_prototype =
            object(get(&response, "prototype", context)?, "Response prototype")?;
        let array_buffer = get(&response_prototype, "arrayBuffer", context)?;
        if !url.is_constructor()
            || !pathname.is_callable()
            || !decode_uri.is_callable()
            || !array_buffer.is_callable()
        {
            return Err(error("Native URL/decodeURI intrinsics unavailable"));
        }
        Ok(Self {
            realm: context.realm().clone(),
            url,
            pathname,
            decode_uri,
            array_buffer,
        })
    }
    fn decode(&self, value: &str, context: &mut Context) -> JsResult<String> {
        string(
            self.decode_uri.as_callable().unwrap().call(
                &JsValue::undefined(),
                &[JsString::from(value).into()],
                context,
            )?,
            "Decoded URI",
        )
    }
    fn pathname(&self, value: &str, context: &mut Context) -> JsResult<String> {
        let url = self.url.construct(
            &[
                JsString::from(value).into(),
                JsString::from("http://localhost").into(),
            ],
            None,
            context,
        )?;
        string(
            self.pathname
                .as_callable()
                .unwrap()
                .call(&url.into(), &[], context)?,
            "URL pathname",
        )
    }
}
#[derive(Clone, Copy, Debug)]
pub struct VisitLimits {
    pub response_bytes: usize,
    pub visit_bytes: usize,
    pub dependencies: usize,
}
impl Default for VisitLimits {
    fn default() -> Self {
        Self {
            response_bytes: 64 * 1024 * 1024,
            visit_bytes: 64 * 1024 * 1024,
            dependencies: 10_000,
        }
    }
}
impl VisitLimits {
    fn validate(self) -> JsResult<()> {
        let max = Self::default();
        if self.response_bytes == 0
            || self.response_bytes > max.response_bytes
            || self.visit_bytes == 0
            || self.visit_bytes > max.visit_bytes
            || self.dependencies == 0
            || self.dependencies > max.dependencies
        {
            return Err(error(
                "Visit limits must be positive and within native caps",
            ));
        }
        Ok(())
    }
}
/// Each phase retains its actual branded Response and live dependency Map.
/// Body records contain fresh headers after consumption, suitable for saving.
pub enum VisitEvent {
    MainHead {
        response: JsObject,
        dependencies: JsMap,
        head: ResponseHead,
    },
    MainBody {
        response: JsObject,
        dependencies: JsMap,
        body: ResponseRecord,
    },
    DependencyHead {
        response: JsObject,
        dependencies: JsMap,
        head: DependencyHead,
    },
    DependencyBody {
        response: JsObject,
        dependencies: JsMap,
        body: DependencyRecord,
    },
    /// Fresh original-main headers after all dependency callbacks. No second
    /// main route-ID decoding is performed just for the crawl phase.
    CrawlHead {
        response: JsObject,
        dependencies: JsMap,
        head: ResponseHead,
    },
}
/// Called synchronously, without any registry borrow held. Implementations may
/// publish bytes or start another visit using this Context. Do not drain jobs.
/// Return an actual JsError to reject this visit with its original opaque value.
pub trait VisitObserver {
    fn event(&self, event: VisitEvent, context: &mut Context) -> JsResult<()>;
}
struct Dependency {
    record: JsObject,
    path: String,
    encoded: String,
    decoded: String,
}
struct State {
    id: u64,
    pending: Option<PendingDispatch>,
    response: Option<JsObject>,
    dependencies: JsMap,
    iterator: Option<JsMapIterator>,
    dependency: Option<Dependency>,
    intrinsics: VisitIntrinsics,
    limits: VisitLimits,
    remaining: usize,
    count: usize,
    observer: Rc<dyn VisitObserver>,
    settle: ResolvingFunctions,
}
// Context host data retains ordinary Rust roots. Native callbacks capture only
// integer IDs; no JS references are hidden inside untraced callback closures.
#[derive(Default)]
struct Visits {
    next: u64,
    active: usize,
    states: BTreeMap<u64, State>,
}
fn take(id: u64, context: &mut Context) -> JsResult<State> {
    let mut registry = context.remove_data::<Visits>().unwrap_or_default();
    let state = registry.states.remove(&id);
    context.insert_data(*registry);
    state.ok_or_else(|| error("Unknown pending native visit"))
}
fn put(state: State, context: &mut Context) {
    let mut registry = context.remove_data::<Visits>().unwrap_or_default();
    registry.states.insert(state.id, state);
    context.insert_data(*registry);
}
fn release(context: &mut Context) {
    let mut registry = context.remove_data::<Visits>().unwrap_or_default();
    registry.active -= 1;
    context.insert_data(*registry);
}
#[derive(Clone, Copy)]
enum Stage {
    Response,
    MainBody,
    DependencyBody,
}
enum Next {
    Await(JsPromise, Stage),
    Done,
}

/// Start respond immediately and return this visit's completion promise. Every
/// await is a native reaction; callers can start other visits before draining.
/// Validation errors are synchronous. Response/observer/body errors reject the
/// completion promise. Completion resolves undefined and releases visit state.
pub fn start(
    renderer: &Renderer,
    context: &mut Context,
    request: JsValue,
    options: JsObject,
    intrinsics: VisitIntrinsics,
    limits: VisitLimits,
    observer: Rc<dyn VisitObserver>,
) -> JsResult<JsPromise> {
    limits.validate()?;
    if &intrinsics.realm != context.realm() {
        return Err(error("Visitor intrinsics belong to a different realm"));
    }
    let mut registry = context.remove_data::<Visits>().unwrap_or_default();
    let reservation = if registry.active >= MAX_ACTIVE {
        Err(error("Active native visit limit exceeded"))
    } else {
        registry
            .next
            .checked_add(1)
            .ok_or_else(|| error("Native visit identity exhausted"))
    };
    if let Ok(id) = reservation {
        registry.next = id;
        registry.active += 1;
    }
    context.insert_data(*registry);
    let id = reservation?;
    let pending = match renderer.dispatch_pending_context(context, request, options) {
        Ok(pending) => pending,
        Err(error) => {
            release(context);
            return Err(error);
        }
    };
    let (completion, settle) = JsPromise::new_pending(context);
    let promise = pending.response.clone();
    let state = State {
        id,
        dependencies: pending.dependencies.clone(),
        pending: Some(pending),
        response: None,
        iterator: None,
        dependency: None,
        intrinsics,
        limits,
        remaining: limits.visit_bytes,
        count: 0,
        observer,
        settle,
    };
    schedule(state, promise, Stage::Response, context)?;
    Ok(completion)
}
fn reject(state: State, reason: JsValue, context: &mut Context) -> JsResult<()> {
    release(context);
    state
        .settle
        .reject
        .call(&JsValue::undefined(), &[reason], context)?;
    Ok(())
}
fn schedule(state: State, promise: JsPromise, stage: Stage, context: &mut Context) -> JsResult<()> {
    let id = state.id;
    put(state, context);
    let stage_id = match stage {
        Stage::Response => 0_u8,
        Stage::MainBody => 1,
        Stage::DependencyBody => 2,
    };
    let fulfilled = NativeFunction::from_copy_closure_with_captures(
        |_, args, &(id, stage_id), context| {
            let mut state = take(id, context)?;
            let value = args.first().cloned().unwrap_or_else(JsValue::undefined);
            let stage = match stage_id {
                0 => Stage::Response,
                1 => Stage::MainBody,
                _ => Stage::DependencyBody,
            };
            match advance(&mut state, stage, value, context) {
                Ok(Next::Await(promise, stage)) => schedule(state, promise, stage, context)?,
                Ok(Next::Done) => {
                    release(context);
                    state.settle.resolve.call(
                        &JsValue::undefined(),
                        &[JsValue::undefined()],
                        context,
                    )?;
                }
                Err(error) => {
                    let reason = error.into_opaque(context)?;
                    reject(state, reason, context)?;
                }
            }
            Ok(JsValue::undefined())
        },
        (id, stage_id),
    )
    .to_js_function(context.realm());
    let rejected = NativeFunction::from_copy_closure_with_captures(
        |_, args, id, context| {
            reject(
                take(*id, context)?,
                args.first().cloned().unwrap_or_else(JsValue::undefined),
                context,
            )?;
            Ok(JsValue::undefined())
        },
        id,
    )
    .to_js_function(context.realm());
    if let Err(error) = promise.then_intrinsic(Some(fulfilled), Some(rejected), context) {
        let reason = error.into_opaque(context)?;
        reject(take(id, context)?, reason, context)?;
    }
    Ok(())
}
fn route_id(headers: &HeaderList) -> Option<String> {
    let parts = headers
        .iter()
        .filter(|(name, _)| name == b"x-sveltekit-routeid")
        .map(|(_, value)| value.iter().copied().map(char::from).collect::<String>())
        .collect::<Vec<_>>();
    (!parts.is_empty()).then(|| parts.join(", "))
}
fn head(
    state: &State,
    response: &JsObject,
    decode: bool,
    context: &mut Context,
) -> JsResult<ResponseHead> {
    let snapshot = web_fetch::response_snapshot(&response.clone().into())?;
    let route_id = if decode {
        route_id(&snapshot.headers)
            .map(|id| state.intrinsics.decode(&id, context))
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
fn read(state: &State, response: &JsObject, context: &mut Context) -> JsResult<JsPromise> {
    // Use the actual native Body consumption algorithm, including its convert
    // reaction. Awaiting fully_read_bytes directly skips a microtask and changes
    // interleaving with sibling visits and the remote input producer.
    let value = state.intrinsics.array_buffer.as_callable().unwrap().call(
        &response.clone().into(),
        &[],
        context,
    )?;
    JsPromise::from_object(object(value, "Response arrayBuffer promise")?)
}
fn buffer_bytes(value: JsValue) -> JsResult<Vec<u8>> {
    let buffer = JsArrayBuffer::from_object(object(value, "Response ArrayBuffer")?)?;
    let bytes = buffer
        .data()
        .ok_or_else(|| error("Response ArrayBuffer was detached"))?;
    Ok(bytes.to_vec())
}
fn record(
    state: &mut State,
    response: &JsObject,
    bytes: Vec<u8>,
    context: &mut Context,
) -> JsResult<ResponseRecord> {
    if bytes.len() > state.limits.response_bytes || bytes.len() > state.remaining {
        return Err(error("Prerender response bytes exceed visit limit"));
    }
    state.remaining -= bytes.len();
    let head = head(state, response, true, context)?;
    Ok(ResponseRecord {
        status: head.status,
        status_text: head.status_text,
        headers: head.headers,
        body: bytes,
        route_id: head.route_id,
    })
}
fn advance(
    state: &mut State,
    stage: Stage,
    value: JsValue,
    context: &mut Context,
) -> JsResult<Next> {
    match stage {
        Stage::Response => {
            let result = state.pending.take().unwrap().finish_context(context)?;
            let response = result.response;
            state.response = Some(response.clone());
            let head = head(state, &response, true, context)?;
            state.observer.event(
                VisitEvent::MainHead {
                    response: response.clone(),
                    dependencies: state.dependencies.clone(),
                    head,
                },
                context,
            )?;
            return Ok(Next::Await(
                read(state, &response, context)?,
                Stage::MainBody,
            ));
        }
        Stage::MainBody => {
            let response = state.response.as_ref().unwrap().clone();
            let bytes = buffer_bytes(value)?;
            let body = record(state, &response, bytes, context)?;
            state.observer.event(
                VisitEvent::MainBody {
                    response,
                    dependencies: state.dependencies.clone(),
                    body,
                },
                context,
            )?;
            // Create the live iterator only after the main save/publication.
            state.iterator = Some(state.dependencies.entries(context)?);
        }
        Stage::DependencyBody => {
            let bytes = buffer_bytes(value)?;
            save_dependency(state, bytes, context)?;
        }
    }
    dependencies(state, context)
}
fn save_dependency(state: &mut State, bytes: Vec<u8>, context: &mut Context) -> JsResult<()> {
    let dependency = state.dependency.take().unwrap();
    // Upstream save receives result.response again, after reading the body.
    let response = object(
        get(&dependency.record, "response", context)?,
        "Dependency response",
    )?;
    let body = record(state, &response, bytes, context)?;
    state.observer.event(
        VisitEvent::DependencyBody {
            response,
            dependencies: state.dependencies.clone(),
            body: DependencyRecord {
                path: dependency.path,
                encoded_path: dependency.encoded,
                decoded_path: dependency.decoded,
                response: body,
            },
        },
        context,
    )
}
fn dependencies(state: &mut State, context: &mut Context) -> JsResult<Next> {
    loop {
        let step = object(
            state.iterator.as_ref().unwrap().next(context)?,
            "Dependency iterator result",
        )?;
        if get(&step, "done", context)?.to_boolean() {
            let response = state.response.as_ref().unwrap().clone();
            let head = head(state, &response, false, context)?;
            state.observer.event(
                VisitEvent::CrawlHead {
                    response,
                    dependencies: state.dependencies.clone(),
                    head,
                },
                context,
            )?;
            return Ok(Next::Done);
        }
        if state.count >= state.limits.dependencies {
            return Err(error("Prerender dependency limit exceeded"));
        }
        state.count += 1;
        let pair = JsArray::from_object(object(get(&step, "value", context)?, "Dependency pair")?)?;
        let path = string(pair.get(0, context)?, "Dependency path")?;
        let record = object(pair.get(1, context)?, "Dependency record")?;
        let encoded = state.intrinsics.pathname(&path, context)?;
        let decoded = state.intrinsics.decode(&encoded, context)?;
        let response = object(get(&record, "response", context)?, "Dependency response")?;
        let head = head(state, &response, false, context)?;
        state.dependency = Some(Dependency {
            record: record.clone(),
            path: path.clone(),
            encoded: encoded.clone(),
            decoded: decoded.clone(),
        });
        state.observer.event(
            VisitEvent::DependencyHead {
                response,
                dependencies: state.dependencies.clone(),
                head: DependencyHead {
                    path,
                    encoded_path: encoded,
                    decoded_path: decoded,
                    response: head,
                },
            },
            context,
        )?;
        // A non-nullish override is consumed synchronously, without adding an
        // await checkpoint. Empty strings and zero-length views are real bodies.
        let override_body = get(&record, "body", context)?;
        if !override_body.is_null_or_undefined() {
            let bytes = if let Some(text) = override_body.as_string() {
                if text.len() > state.limits.response_bytes {
                    return Err(error("Dependency string exceeds response limit"));
                }
                text.to_std_string_lossy().into_bytes()
            } else {
                web_streams::copy_uint8_chunk(&override_body, context)?
            };
            save_dependency(state, bytes, context)?;
        } else {
            let response = object(
                get(&record, "response", context)?,
                "Dependency body response",
            )?;
            return Ok(Next::Await(
                read(state, &response, context)?,
                Stage::DependencyBody,
            ));
        }
    }
}

#[cfg(test)]
#[path = "prerender_visit_tests.rs"]
mod tests;
