//! Native default and byte readable streams (WHATWG Streams, 2026-08-18).
//! https://streams.spec.whatwg.org/#rs-class
//!
//! No JavaScript polyfill or I/O: queues contain traced JS values, reads are real
//! promises and pull work follows promise jobs. Byte/BYOB controllers transfer
//! backing buffers and preserve intrinsic view types. Native piping integrates
//! writable streams and abort algorithms without looking up public JS methods.
//! Transferable streams remain unsupported. Hosts pump the bounded Boa job queue.
//!
//! Limits: 10,000 queued chunks/pending reads, 64 nested source callbacks, 64 MiB
//! native byte storage/collection. Queue size algorithms remain user-defined.
//! Internal reactions use the minimal vendored Boa `then_intrinsic` extension,
//! preserving the engine's GC/job semantics without invoking Promise species hooks.
use boa_engine::{
    Context, JsArgs, JsData, JsError, JsNativeError, JsObject, JsResult, JsString, JsSymbol,
    JsValue, NativeFunction,
    builtins::{iterable::create_iter_result_object, promise::ResolvingFunctions},
    class::{Class, ClassBuilder},
    object::{
        FunctionObjectBuilder,
        builtins::{JsArrayBuffer, JsPromise, JsUint8Array},
    },
    property::{Attribute, PropertyDescriptor},
};
use boa_gc::{Finalize, Gc, GcRefCell, Trace};
use std::collections::VecDeque;
const MAX_QUEUE: usize = 10_000;
const MAX_BYTES: usize = 64 * 1024 * 1024;
type State = Gc<GcRefCell<StreamData>>;
type ReaderState = Gc<GcRefCell<ReaderData>>;
#[derive(Clone, Copy, PartialEq, Eq)]
enum Status {
    Readable,
    Closed,
    Errored,
}
#[derive(Trace, Finalize)]
struct Chunk {
    value: JsValue,
    size: f64,
}
#[derive(Trace, Finalize)]
struct StreamData {
    #[unsafe_ignore_trace]
    status: Status,
    error: JsValue,
    disturbed: bool,
    reader: Option<ReaderState>,
    controller: Option<JsObject>,
    queue: VecDeque<Chunk>,
    total: f64,
    hwm: f64,
    close_requested: bool,
    started: bool,
    pulling: bool,
    pull_again: bool,
    source: JsValue,
    pull: Option<JsObject>,
    cancel: Option<JsObject>,
    size: Option<JsObject>,
    bytes: Option<byob::ByteData>,
}
#[derive(Trace, Finalize)]
struct ReaderData {
    byob: bool,
    stream: Option<State>,
    closed: JsPromise,
    closed_settle: ResolvingFunctions,
    requests: VecDeque<ResolvingFunctions>,
}
#[derive(Trace, Finalize, JsData)]
struct ReadableStream {
    state: State,
}
#[derive(Trace, Finalize, JsData)]
struct ReadableStreamDefaultReader {
    state: ReaderState,
}
#[derive(Trace, Finalize, JsData)]
struct ReadableStreamDefaultController {
    state: State,
}
/// Traced opaque controller for an incrementally produced native default stream.
#[derive(Clone, Trace, Finalize)]
pub struct ReadableController {
    state: State,
}
struct Registration {
    depth: std::rc::Rc<std::cell::Cell<usize>>,
}
fn typ(message: &str) -> JsError {
    JsNativeError::typ().with_message(message.to_owned()).into()
}
fn limit() -> JsError {
    JsNativeError::range()
        .with_message("Native readable stream resource limit exceeded")
        .into()
}
fn unsupported(message: &str) -> JsError {
    JsNativeError::typ()
        .with_message(format!("Unsupported native stream capability: {message}"))
        .into()
}
fn obj(value: &JsValue) -> JsResult<JsObject> {
    value.as_object().ok_or_else(|| typ("Object required"))
}
fn state(value: &JsValue) -> JsResult<State> {
    obj(value)?
        .downcast_ref::<ReadableStream>()
        .map(|v| v.state.clone())
        .ok_or_else(|| typ("ReadableStream receiver required"))
}
fn reader(value: &JsValue) -> JsResult<ReaderState> {
    obj(value)?
        .downcast_ref::<ReadableStreamDefaultReader>()
        .map(|v| v.state.clone())
        .ok_or_else(|| typ("ReadableStreamDefaultReader receiver required"))
}
fn controller(value: &JsValue) -> JsResult<State> {
    obj(value)?
        .downcast_ref::<ReadableStreamDefaultController>()
        .map(|v| v.state.clone())
        .ok_or_else(|| typ("ReadableStreamDefaultController receiver required"))
}
fn dictionary(value: &JsValue) -> JsResult<Option<JsObject>> {
    if value.is_null_or_undefined() {
        Ok(None)
    } else {
        Ok(Some(obj(value)?))
    }
}
fn field(value: &Option<JsObject>, key: &str, context: &mut Context) -> JsResult<JsValue> {
    value
        .as_ref()
        .map(|o| o.get(JsString::from(key), context))
        .unwrap_or_else(|| Ok(JsValue::undefined()))
}
fn callback(value: JsValue) -> JsResult<Option<JsObject>> {
    if value.is_undefined() {
        Ok(None)
    } else {
        Ok(Some(
            value
                .as_callable()
                .ok_or_else(|| typ("Stream callback must be callable"))?,
        ))
    }
}
fn call(
    function: &JsObject,
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let depth = context
        .get_data::<Registration>()
        .ok_or_else(|| typ("Streams not registered"))?
        .depth
        .clone();
    if depth.get() >= 64 {
        return Err(limit());
    }
    depth.set(depth.get() + 1);
    let result = function.call(this, args, context);
    depth.set(depth.get() - 1);
    result
}
fn settled(value: JsValue, context: &mut Context) -> JsResult<JsPromise> {
    JsPromise::resolve(value, context)
}
fn rejected(error: JsError, context: &mut Context) -> JsResult<JsPromise> {
    JsPromise::reject(error, context)
}
fn promise_method(result: JsResult<JsPromise>, context: &mut Context) -> JsResult<JsValue> {
    match result {
        Ok(p) => Ok(p.into()),
        Err(e) => Ok(rejected(e, context)?.into()),
    }
}
fn resolve(functions: &ResolvingFunctions, value: JsValue, context: &mut Context) -> JsResult<()> {
    functions
        .resolve
        .call(&JsValue::undefined(), &[value], context)?;
    Ok(())
}
fn reject(functions: &ResolvingFunctions, value: JsValue, context: &mut Context) -> JsResult<()> {
    functions
        .reject
        .call(&JsValue::undefined(), &[value], context)?;
    Ok(())
}
fn handled(promise: &JsPromise, context: &mut Context) -> JsResult<()> {
    let ignore = NativeFunction::from_fn_ptr(|_, _, _| Ok(JsValue::undefined()))
        .to_js_function(context.realm());
    drop(promise.then_intrinsic(None, Some(ignore), context)?);
    Ok(())
}
fn clear(data: &mut StreamData) {
    data.source = JsValue::undefined();
    data.pull = None;
    data.cancel = None;
    data.size = None;
}
fn make_state(hwm: f64) -> State {
    Gc::new(GcRefCell::new(StreamData {
        status: Status::Readable,
        error: JsValue::undefined(),
        disturbed: false,
        reader: None,
        controller: None,
        queue: VecDeque::new(),
        total: 0.0,
        hwm,
        close_requested: false,
        started: false,
        pulling: false,
        pull_again: false,
        source: JsValue::undefined(),
        pull: None,
        cancel: None,
        size: None,
        bytes: None,
    }))
}
fn attach_controller(state: &State, context: &mut Context) -> JsResult<()> {
    if state.borrow().bytes.is_some() {
        return byob::attach(state, context);
    }
    let object = ReadableStreamDefaultController::from_data(
        ReadableStreamDefaultController {
            state: state.clone(),
        },
        context,
    )?;
    state.borrow_mut().controller = Some(object);
    Ok(())
}
fn close_state(state: &State, context: &mut Context) -> JsResult<()> {
    let reader = {
        let mut data = state.borrow_mut();
        if data.status != Status::Readable {
            return Ok(());
        }
        data.status = Status::Closed;
        data.reader.clone()
    };
    if let Some(reader) = reader {
        let (closed, requests) = {
            let mut r = reader.borrow_mut();
            (
                r.closed_settle.clone(),
                if r.byob {
                    VecDeque::new()
                } else {
                    std::mem::take(&mut r.requests)
                },
            )
        };
        resolve(&closed, JsValue::undefined(), context)?;
        for request in requests {
            let result = create_iter_result_object(JsValue::undefined(), true, context);
            resolve(&request, result, context)?;
        }
    }
    Ok(())
}
fn error_state(state: &State, error: JsValue, context: &mut Context) -> JsResult<()> {
    if state.borrow().status == Status::Readable {
        byob::clear_pending(state);
    }
    let reader = {
        let mut data = state.borrow_mut();
        if data.status != Status::Readable {
            return Ok(());
        }
        data.status = Status::Errored;
        data.error = error.clone();
        data.queue.clear();
        data.total = 0.0;
        clear(&mut data);
        data.reader.clone()
    };
    if let Some(reader) = reader {
        let (closed, promise, requests) = {
            let mut r = reader.borrow_mut();
            (
                r.closed_settle.clone(),
                r.closed.clone(),
                std::mem::take(&mut r.requests),
            )
        };
        reject(&closed, error.clone(), context)?;
        handled(&promise, context)?;
        for request in requests {
            reject(&request, error.clone(), context)?;
        }
    }
    Ok(())
}
fn can_enqueue(state: &State) -> bool {
    let d = state.borrow();
    d.status == Status::Readable && !d.close_requested
}
fn close_controller(state: &State, context: &mut Context) -> JsResult<()> {
    if state.borrow().bytes.is_some() {
        return byob::close(state, context);
    }
    if !can_enqueue(state) {
        return Err(typ("Cannot close this stream"));
    }
    let close = {
        let mut d = state.borrow_mut();
        d.close_requested = true;
        if d.queue.is_empty() {
            clear(&mut d);
            true
        } else {
            false
        }
    };
    if close {
        close_state(state, context)?;
    }
    Ok(())
}
fn desired_size(state: &State) -> JsValue {
    let d = state.borrow();
    match d.status {
        Status::Errored => JsValue::null(),
        Status::Closed => 0.into(),
        Status::Readable => (d.hwm - d.total).into(),
    }
}
fn should_pull(state: &State) -> bool {
    let d = state.borrow();
    d.status == Status::Readable
        && !d.close_requested
        && d.started
        && (d
            .reader
            .as_ref()
            .is_some_and(|r| !r.borrow().requests.is_empty())
            || d.hwm - d.total > 0.0)
}
fn pull_if_needed(state: &State, context: &mut Context) -> JsResult<()> {
    if !should_pull(state) {
        return Ok(());
    }
    let (pull, source, controller) = {
        let mut d = state.borrow_mut();
        if d.pulling {
            d.pull_again = true;
            return Ok(());
        }
        d.pulling = true;
        (
            d.pull.clone(),
            d.source.clone(),
            d.controller.clone().unwrap(),
        )
    };
    let result = match pull {
        Some(pull) => call(&pull, &source, &[controller.into()], context),
        None => Ok(JsValue::undefined()),
    };
    let promise = match result {
        Ok(value) => settled(value, context)?,
        Err(error) => rejected(error, context)?,
    };
    let fulfilled = NativeFunction::from_copy_closure_with_captures(
        |_, _, state, context| {
            let again = {
                let mut d = state.borrow_mut();
                d.pulling = false;
                std::mem::take(&mut d.pull_again)
            };
            if again {
                pull_if_needed(state, context)?;
            }
            Ok(JsValue::undefined())
        },
        state.clone(),
    )
    .to_js_function(context.realm());
    let failed = NativeFunction::from_copy_closure_with_captures(
        |_, args, state, context| {
            error_state(state, args.get_or_undefined(0).clone(), context)?;
            Ok(JsValue::undefined())
        },
        state.clone(),
    )
    .to_js_function(context.realm());
    drop(promise.then_intrinsic(Some(fulfilled), Some(failed), context)?);
    Ok(())
}
fn start(state: &State, result: JsValue, context: &mut Context) -> JsResult<()> {
    let promise = settled(result, context)?;
    let fulfilled = NativeFunction::from_copy_closure_with_captures(
        |_, _, state, context| {
            state.borrow_mut().started = true;
            pull_if_needed(state, context)?;
            Ok(JsValue::undefined())
        },
        state.clone(),
    )
    .to_js_function(context.realm());
    let failed = NativeFunction::from_copy_closure_with_captures(
        |_, args, state, context| {
            error_state(state, args.get_or_undefined(0).clone(), context)?;
            Ok(JsValue::undefined())
        },
        state.clone(),
    )
    .to_js_function(context.realm());
    drop(promise.then_intrinsic(Some(fulfilled), Some(failed), context)?);
    Ok(())
}
fn enqueue(state: &State, value: JsValue, context: &mut Context) -> JsResult<()> {
    if state.borrow().bytes.is_some() {
        return byob::enqueue(state, &value, context);
    }
    if !can_enqueue(state) {
        return Err(typ("Cannot enqueue into this stream"));
    }
    let reader = state.borrow().reader.clone();
    let pending = reader.and_then(|r| r.borrow_mut().requests.pop_front());
    if let Some(request) = pending {
        let result = create_iter_result_object(value, false, context);
        resolve(&request, result, context)?;
    } else {
        let algorithm = state.borrow().size.clone();
        let size: JsResult<f64> = (|| {
            let value = if let Some(algorithm) = algorithm {
                call(
                    &algorithm,
                    &JsValue::undefined(),
                    std::slice::from_ref(&value),
                    context,
                )?
            } else {
                1.into()
            };
            let size = value.to_number(context)?;
            if !size.is_finite() || size < 0.0 {
                return Err(JsNativeError::range()
                    .with_message("Invalid stream chunk size")
                    .into());
            }
            Ok(size)
        })();
        let size = match size {
            Ok(value) => value,
            Err(error) => {
                let reason = error.into_opaque(context)?;
                error_state(state, reason.clone(), context)?;
                return Err(JsError::from_opaque(reason));
            }
        };
        if state.borrow().queue.len() >= MAX_QUEUE {
            let reason = limit().into_opaque(context)?;
            error_state(state, reason.clone(), context)?;
            return Err(JsError::from_opaque(reason));
        }
        // A size callback can reenter and close/error the stream. The standard
        // still performs EnqueueValueWithSize after that callback completes.
        let mut data = state.borrow_mut();
        data.total += size;
        data.queue.push_back(Chunk { value, size });
    }
    pull_if_needed(state, context)
}
fn acquire(state: &State, context: &mut Context) -> JsResult<ReaderState> {
    if state.borrow().reader.is_some() {
        return Err(typ("ReadableStream is locked"));
    }
    let (closed, closed_settle) = JsPromise::new_pending(context);
    let reader = Gc::new(GcRefCell::new(ReaderData {
        byob: false,
        stream: Some(state.clone()),
        closed: closed.clone(),
        closed_settle: closed_settle.clone(),
        requests: VecDeque::new(),
    }));
    state.borrow_mut().reader = Some(reader.clone());
    let (status, error) = {
        let data = state.borrow();
        (data.status, data.error.clone())
    };
    match status {
        Status::Closed => resolve(&closed_settle, JsValue::undefined(), context)?,
        Status::Errored => {
            reject(&closed_settle, error, context)?;
            handled(&closed, context)?;
        }
        Status::Readable => {}
    }
    Ok(reader)
}
fn read_state(reader: &ReaderState, context: &mut Context) -> JsResult<JsPromise> {
    let (promise, request) = JsPromise::new_pending(context);
    read_request(reader, request, context)?;
    Ok(promise)
}
// Native ReadRequest callbacks bypass Promise resolution of iterator-result
// objects. PipeTo must preserve raw chunks, including thenables, without invoking
// Object.prototype.then or user-visible reader methods.
fn read_request(
    reader: &ReaderState,
    request: ResolvingFunctions,
    context: &mut Context,
) -> JsResult<()> {
    let state = reader
        .borrow()
        .stream
        .clone()
        .ok_or_else(|| typ("Reader lock was released"))?;
    state.borrow_mut().disturbed = true;
    let (status, error) = {
        let d = state.borrow();
        (d.status, d.error.clone())
    };
    if status == Status::Closed {
        let value = create_iter_result_object(JsValue::undefined(), true, context);
        return resolve(&request, value, context);
    }
    if status == Status::Errored {
        return reject(&request, error, context);
    }
    if state.borrow().bytes.is_some() {
        byob::read_default(&state, reader, request, context)?;
        return Ok(());
    }
    let chunk = state.borrow_mut().queue.pop_front();
    if let Some(chunk) = chunk {
        let close = {
            let mut d = state.borrow_mut();
            d.total = (d.total - chunk.size).max(0.0);
            if d.close_requested && d.queue.is_empty() {
                clear(&mut d);
                true
            } else {
                false
            }
        };
        if close {
            close_state(&state, context)?;
        } else {
            pull_if_needed(&state, context)?;
        }
        let result = create_iter_result_object(chunk.value.clone(), false, context);
        resolve(&request, result, context)?;
    } else {
        if reader.borrow().requests.len() >= MAX_QUEUE {
            return Err(limit());
        }
        reader.borrow_mut().requests.push_back(request);
        pull_if_needed(&state, context)?;
    }
    Ok(())
}
fn cancel_state(state: &State, reason: JsValue, context: &mut Context) -> JsResult<JsPromise> {
    state.borrow_mut().disturbed = true;
    let (status, error) = {
        let d = state.borrow();
        (d.status, d.error.clone())
    };
    if status == Status::Closed {
        return settled(JsValue::undefined(), context);
    }
    if status == Status::Errored {
        return rejected(JsError::from_opaque(error), context);
    }
    close_state(state, context)?;
    byob::cancel_pending(state, context)?;
    let (cancel, source) = {
        let mut d = state.borrow_mut();
        d.queue.clear();
        d.total = 0.0;
        let value = (d.cancel.clone(), d.source.clone());
        clear(&mut d);
        value
    };
    let result = match cancel {
        Some(cancel) => call(&cancel, &source, &[reason], context),
        None => Ok(JsValue::undefined()),
    };
    let promise = match result {
        Ok(value) => settled(value, context)?,
        Err(error) => rejected(error, context)?,
    };
    let ignore = NativeFunction::from_fn_ptr(|_, _, _| Ok(JsValue::undefined()))
        .to_js_function(context.realm());
    promise.then_intrinsic(Some(ignore), None, context)
}
fn release(reader: &ReaderState, context: &mut Context) -> JsResult<()> {
    let Some(state) = reader.borrow().stream.clone() else {
        return Ok(());
    };
    let error = typ("Reader lock was released").into_opaque(context)?;
    let status = state.borrow().status;
    let (closed, promise, requests) = {
        let mut r = reader.borrow_mut();
        if status != Status::Readable {
            let (p, f) = JsPromise::new_pending(context);
            r.closed = p;
            r.closed_settle = f;
        }
        (
            r.closed_settle.clone(),
            r.closed.clone(),
            std::mem::take(&mut r.requests),
        )
    };
    byob::release_pending(&state);
    state.borrow_mut().reader = None;
    reader.borrow_mut().stream = None;
    reject(&closed, error.clone(), context)?;
    handled(&promise, context)?;
    for request in requests {
        reject(&request, error.clone(), context)?;
    }
    Ok(())
}
/// Resolved default-source algorithms for native composition (e.g. TransformStream).
/// Callbacks are GC traced. start/pull receive the readable controller; cancel
/// receives its reason. size runs with undefined this. No JS property lookups.
#[derive(Clone, Trace, Finalize)]
pub struct ReadableAlgorithms {
    pub this: JsValue,
    pub start: Option<JsObject>,
    pub pull: Option<JsObject>,
    pub cancel: Option<JsObject>,
    pub size: Option<JsObject>,
    pub high_water_mark: f64,
}
impl Default for ReadableAlgorithms {
    fn default() -> Self {
        Self {
            this: JsValue::undefined(),
            start: None,
            pull: None,
            cancel: None,
            size: None,
            high_water_mark: 1.0,
        }
    }
}
/// Create a real default stream from already resolved native algorithms. start
/// throws synchronously; its returned promise gates pulls and queued reads.
pub fn new_readable_with_algorithms(
    algorithms: ReadableAlgorithms,
    context: &mut Context,
) -> JsResult<(JsObject, ReadableController)> {
    if algorithms.high_water_mark.is_nan() || algorithms.high_water_mark < 0.0 {
        return Err(JsNativeError::range()
            .with_message("Invalid highWaterMark")
            .into());
    }
    for function in [
        &algorithms.start,
        &algorithms.pull,
        &algorithms.cancel,
        &algorithms.size,
    ]
    .into_iter()
    .flatten()
    {
        if !function.is_callable() {
            return Err(typ("Readable algorithm must be callable"));
        }
    }
    let state = make_state(algorithms.high_water_mark);
    attach_controller(&state, context)?;
    {
        let mut d = state.borrow_mut();
        d.source = algorithms.this.clone();
        d.pull = algorithms.pull.clone();
        d.cancel = algorithms.cancel.clone();
        d.size = algorithms.size.clone();
    }
    let result = if let Some(function) = &algorithms.start {
        let controller = state.borrow().controller.clone().unwrap();
        call(function, &algorithms.this, &[controller.into()], context)?
    } else {
        JsValue::undefined()
    };
    start(&state, result, context)?;
    let object = ReadableStream::from_data(
        ReadableStream {
            state: state.clone(),
        },
        context,
    )?;
    Ok((object, ReadableController { state }))
}
/// Create a host-driven stream, initially readable with no producer callbacks.
/// Enqueue only in response to host data; desired_size exposes backpressure.
pub fn new_readable(
    high_water_mark: f64,
    context: &mut Context,
) -> JsResult<(JsObject, ReadableController)> {
    if high_water_mark.is_nan() || high_water_mark < 0.0 {
        return Err(JsNativeError::range()
            .with_message("Invalid highWaterMark")
            .into());
    }
    let state = make_state(high_water_mark);
    attach_controller(&state, context)?;
    start(&state, JsValue::undefined(), context)?;
    let object = ReadableStream::from_data(
        ReadableStream {
            state: state.clone(),
        },
        context,
    )?;
    Ok((object, ReadableController { state }))
}
impl ReadableController {
    /// Internal default-controller state, without public JS getter calls.
    pub fn can_close_or_enqueue(&self) -> bool {
        can_enqueue(&self.state)
    }
    /// Includes pending read demand and the start gate, not only queue size.
    pub fn has_backpressure(&self) -> bool {
        !should_pull(&self.state)
    }
    pub fn is_errored(&self) -> bool {
        self.state.borrow().status == Status::Errored
    }
    pub fn stored_error(&self) -> JsValue {
        self.state.borrow().error.clone()
    }
    pub fn enqueue(&self, value: JsValue, context: &mut Context) -> JsResult<()> {
        enqueue(&self.state, value, context)
    }
    pub fn close(&self, context: &mut Context) -> JsResult<()> {
        if self.state.borrow().bytes.is_some() {
            byob::close_and_respond(&self.state, context)
        } else {
            close_controller(&self.state, context)
        }
    }
    pub fn error(&self, error: JsValue, context: &mut Context) -> JsResult<()> {
        error_state(&self.state, error, context)
    }
    pub fn desired_size(&self) -> Option<f64> {
        desired_size(&self.state).as_number()
    }
}
pub fn is_locked(value: &JsValue) -> JsResult<bool> {
    Ok(state(value)?.borrow().reader.is_some())
}
pub fn is_disturbed(value: &JsValue) -> JsResult<bool> {
    Ok(state(value)?.borrow().disturbed)
}
pub fn get_reader(value: &JsValue, context: &mut Context) -> JsResult<JsObject> {
    let state = acquire(&state(value)?, context)?;
    ReadableStreamDefaultReader::from_data(ReadableStreamDefaultReader { state }, context)
}
pub fn read(value: &JsValue, context: &mut Context) -> JsResult<JsPromise> {
    let result = reader(value).and_then(|r| read_state(&r, context));
    match result {
        Ok(p) => Ok(p),
        Err(e) => rejected(e, context),
    }
}
pub fn cancel_reader(
    value: &JsValue,
    reason: JsValue,
    context: &mut Context,
) -> JsResult<JsPromise> {
    let result = (|| {
        let r = reader(value)?;
        let s = r
            .borrow()
            .stream
            .clone()
            .ok_or_else(|| typ("Reader lock was released"))?;
        cancel_state(&s, reason, context)
    })();
    match result {
        Ok(p) => Ok(p),
        Err(e) => rejected(e, context),
    }
}
pub fn release_reader(value: &JsValue, context: &mut Context) -> JsResult<()> {
    release(&reader(value)?, context)
}
impl Class for ReadableStream {
    const NAME: &'static str = "ReadableStream";
    const ATTRIBUTES: Attribute = Attribute::WRITABLE.union(Attribute::CONFIGURABLE);
    fn data_constructor(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<Self> {
        // The strategy argument's Web IDL dictionary conversion precedes the
        // constructor's explicit conversion of the `any` underlying source.
        let strategy = dictionary(args.get_or_undefined(1))?;
        let hwm = field(&strategy, "highWaterMark", context)?;
        let default_hwm = hwm.is_undefined();
        let mut hwm = if default_hwm {
            1.0
        } else {
            hwm.to_number(context)?
        };
        let size = callback(field(&strategy, "size", context)?)?;
        let source = dictionary(args.get_or_undefined(0))?;
        let auto = field(&source, "autoAllocateChunkSize", context)?;
        let auto = if !auto.is_undefined() {
            let v = auto.to_number(context)?;
            if !v.is_finite() || v.trunc() < 0.0 || v.trunc() >= 18446744073709551616.0 {
                return Err(typ("Invalid autoAllocateChunkSize"));
            }
            Some(v.trunc())
        } else {
            None
        };
        let cancel = callback(field(&source, "cancel", context)?)?;
        let pull = callback(field(&source, "pull", context)?)?;
        let start_method = callback(field(&source, "start", context)?)?;
        let kind = field(&source, "type", context)?;
        let byte_stream = !kind.is_undefined();
        if byte_stream {
            let kind = kind.to_string(context)?;
            if kind != "bytes" {
                return Err(typ("Invalid underlying source type"));
            }
            if size.is_some() {
                return Err(JsNativeError::range()
                    .with_message("Byte stream strategy cannot have size")
                    .into());
            }
            if default_hwm {
                hwm = 0.0;
            }
        }
        if hwm.is_nan() || hwm < 0.0 {
            return Err(JsNativeError::range()
                .with_message("Invalid highWaterMark")
                .into());
        }
        let state = make_state(hwm);
        if byte_stream {
            let auto = if let Some(n) = auto {
                if n == 0.0 {
                    return Err(typ("autoAllocateChunkSize must be positive"));
                }
                if n > MAX_BYTES as f64 {
                    return Err(limit());
                }
                Some(n as usize)
            } else {
                None
            };
            byob::initialize(&state, auto);
        }
        attach_controller(&state, context)?;
        let this = source.map(JsValue::from).unwrap_or_else(JsValue::null);
        {
            let mut d = state.borrow_mut();
            d.source = this.clone();
            d.pull = pull;
            d.cancel = cancel;
            d.size = size;
        }
        let result = if let Some(method) = start_method {
            let controller = state.borrow().controller.clone().unwrap();
            call(&method, &this, &[controller.into()], context)?
        } else {
            JsValue::undefined()
        };
        start(&state, result, context)?;
        Ok(Self { state })
    }
    fn init(class: &mut ClassBuilder) -> JsResult<()> {
        let getter = FunctionObjectBuilder::new(
            class.context().realm(),
            NativeFunction::from_fn_ptr(|this, _, _| is_locked(this).map(JsValue::from)),
        )
        .build();
        class.accessor(
            JsString::from("locked"),
            Some(getter),
            None,
            Attribute::ENUMERABLE.union(Attribute::CONFIGURABLE),
        );
        class.method(
            JsString::from("getReader"),
            0,
            NativeFunction::from_fn_ptr(|this, args, context| {
                let state = state(this)?;
                let options = dictionary(args.get_or_undefined(0))?;
                let mode = field(&options, "mode", context)?;
                if !mode.is_undefined() {
                    let mode = mode.to_string(context)?;
                    if mode != "byob" {
                        return Err(typ("Invalid reader mode"));
                    }
                    return byob::get_reader(&state, context).map(JsValue::from);
                }
                let reader = acquire(&state, context)?;
                Ok(ReadableStreamDefaultReader::from_data(
                    ReadableStreamDefaultReader { state: reader },
                    context,
                )?
                .into())
            }),
        );
        class.method(
            JsString::from("cancel"),
            0,
            NativeFunction::from_fn_ptr(|this, args, context| {
                let result = (|| {
                    let state = state(this)?;
                    if state.borrow().reader.is_some() {
                        return Err(typ("ReadableStream is locked"));
                    }
                    cancel_state(&state, args.get_or_undefined(0).clone(), context)
                })();
                promise_method(result, context)
            }),
        );
        class.method(
            JsString::from("values"),
            0,
            NativeFunction::from_fn_ptr(iteration::values),
        );
        class.method(
            JsString::from("tee"),
            0,
            NativeFunction::from_fn_ptr(|this, _, context| tee::tee(this, context)),
        );
        class.method(
            JsString::from("pipeTo"),
            1,
            NativeFunction::from_fn_ptr(pipe::pipe_to_method),
        );
        class.method(
            JsString::from("pipeThrough"),
            1,
            NativeFunction::from_fn_ptr(pipe::pipe_through_method),
        );
        class.static_method(
            JsString::from("from"),
            1,
            NativeFunction::from_fn_ptr(|_, _, _| {
                Err(unsupported("ReadableStream.from iterable source"))
            }),
        );
        class.property(
            JsSymbol::to_string_tag(),
            JsString::from("ReadableStream"),
            Attribute::CONFIGURABLE,
        );
        Ok(())
    }
}
impl Class for ReadableStreamDefaultController {
    const NAME: &'static str = "ReadableStreamDefaultController";
    const ATTRIBUTES: Attribute = Attribute::WRITABLE.union(Attribute::CONFIGURABLE);
    fn data_constructor(_: &JsValue, _: &[JsValue], _: &mut Context) -> JsResult<Self> {
        Err(typ("Illegal controller constructor"))
    }
    fn init(class: &mut ClassBuilder) -> JsResult<()> {
        let getter = FunctionObjectBuilder::new(
            class.context().realm(),
            NativeFunction::from_fn_ptr(|this, _, _| Ok(desired_size(&controller(this)?))),
        )
        .build();
        class.accessor(
            JsString::from("desiredSize"),
            Some(getter),
            None,
            Attribute::ENUMERABLE.union(Attribute::CONFIGURABLE),
        );
        class.method(
            JsString::from("enqueue"),
            0,
            NativeFunction::from_fn_ptr(|this, args, context| {
                enqueue(
                    &controller(this)?,
                    args.get_or_undefined(0).clone(),
                    context,
                )?;
                Ok(JsValue::undefined())
            }),
        );
        class.method(
            JsString::from("close"),
            0,
            NativeFunction::from_fn_ptr(|this, _, context| {
                close_controller(&controller(this)?, context)?;
                Ok(JsValue::undefined())
            }),
        );
        class.method(
            JsString::from("error"),
            0,
            NativeFunction::from_fn_ptr(|this, args, context| {
                error_state(
                    &controller(this)?,
                    args.get_or_undefined(0).clone(),
                    context,
                )?;
                Ok(JsValue::undefined())
            }),
        );
        class.property(
            JsSymbol::to_string_tag(),
            JsString::from("ReadableStreamDefaultController"),
            Attribute::CONFIGURABLE,
        );
        Ok(())
    }
}
impl Class for ReadableStreamDefaultReader {
    const NAME: &'static str = "ReadableStreamDefaultReader";
    const LENGTH: usize = 1;
    const ATTRIBUTES: Attribute = Attribute::WRITABLE.union(Attribute::CONFIGURABLE);
    fn data_constructor(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<Self> {
        Ok(Self {
            state: acquire(&state(args.get_or_undefined(0))?, context)?,
        })
    }
    fn init(class: &mut ClassBuilder) -> JsResult<()> {
        let getter = FunctionObjectBuilder::new(
            class.context().realm(),
            NativeFunction::from_fn_ptr(|this, _, context| {
                let result = reader(this).map(|r| r.borrow().closed.clone());
                promise_method(result, context)
            }),
        )
        .build();
        class.accessor(
            JsString::from("closed"),
            Some(getter),
            None,
            Attribute::ENUMERABLE.union(Attribute::CONFIGURABLE),
        );
        class.method(
            JsString::from("read"),
            0,
            NativeFunction::from_fn_ptr(|this, _, context| read(this, context).map(JsValue::from)),
        );
        class.method(
            JsString::from("cancel"),
            0,
            NativeFunction::from_fn_ptr(|this, args, context| {
                cancel_reader(this, args.get_or_undefined(0).clone(), context).map(JsValue::from)
            }),
        );
        class.method(
            JsString::from("releaseLock"),
            0,
            NativeFunction::from_fn_ptr(|this, _, context| {
                release_reader(this, context)?;
                Ok(JsValue::undefined())
            }),
        );
        class.property(
            JsSymbol::to_string_tag(),
            JsString::from("ReadableStreamDefaultReader"),
            Attribute::CONFIGURABLE,
        );
        Ok(())
    }
}
/// Register classes once. No I/O, text, Blob, or Abort capability dependency.
pub fn register(context: &mut Context) -> JsResult<()> {
    if context.get_data::<Registration>().is_some() {
        return Ok(());
    }
    context.insert_data(Registration {
        depth: std::rc::Rc::new(std::cell::Cell::new(0)),
    });
    context.register_global_class::<ReadableStream>()?;
    context.register_global_class::<ReadableStreamDefaultReader>()?;
    context.register_global_class::<ReadableStreamDefaultController>()?;
    iteration::register(context)?;
    byob::register(context)?;
    let prototype = context
        .get_global_class::<ReadableStream>()
        .unwrap()
        .prototype();
    let values = prototype.get(JsString::from("values"), context)?;
    prototype.define_property_or_throw(
        JsSymbol::async_iterator(),
        PropertyDescriptor::builder()
            .value(values)
            .writable(true)
            .configurable(true),
        context,
    )?;
    for (prototype, names) in [
        (
            prototype,
            &[
                "getReader",
                "cancel",
                "values",
                "tee",
                "pipeTo",
                "pipeThrough",
            ][..],
        ),
        (
            context
                .get_global_class::<ReadableStreamDefaultReader>()
                .unwrap()
                .prototype(),
            &["read", "cancel", "releaseLock"][..],
        ),
        (
            context
                .get_global_class::<ReadableStreamDefaultController>()
                .unwrap()
                .prototype(),
            &["enqueue", "close", "error"][..],
        ),
        (
            context
                .get_global_class::<ReadableStream>()
                .unwrap()
                .constructor(),
            &["from"][..],
        ),
    ] {
        for name in names {
            prototype.define_property_or_throw(
                JsString::from(*name),
                PropertyDescriptor::builder().enumerable(true),
                context,
            )?;
        }
    }
    Ok(())
}
#[path = "web_streams_bytes.rs"]
mod bytes;
#[path = "web_streams_iteration.rs"]
mod iteration;
#[path = "web_streams_tee.rs"]
mod tee;
pub use bytes::{consume_bytes, copy_uint8_chunk, from_bytes, fully_read_bytes};
#[cfg(test)]
#[path = "web_streams_tests.rs"]
mod tests;

#[path = "web_streams_byob.rs"]
mod byob;
pub use byob::{get_byob_reader, new_readable_bytes, read_into};

#[path = "web_streams_byte_tee.rs"]
mod byte_tee;

/// Fetch body tee: byte streams use the byte tee algorithm; default streams
/// clone branch-two BufferSource/primitive chunks. Unsupported structured object
/// graphs error both branches explicitly instead of sharing object identity.
pub fn tee_for_fetch(value: &JsValue, context: &mut Context) -> JsResult<(JsObject, JsObject)> {
    let pair = obj(&tee::tee_with_clone(value, true, context)?)?;
    Ok((obj(&pair.get(0, context)?)?, obj(&pair.get(1, context)?)?))
}
#[path = "web_streams_proxy.rs"]
mod proxy;
pub use proxy::identity_proxy;

#[path = "web_streams_pipe.rs"]
mod pipe;
pub use pipe::{PipeOptions, pipe_to};
