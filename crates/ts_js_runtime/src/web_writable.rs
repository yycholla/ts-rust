//! Native WHATWG WritableStream, default writer and controller.
//!
//! Algorithm source: https://streams.spec.whatwg.org/#ws-abstract-ops (living
//! standard, 18 August 2026). Uses Boa's intrinsic promise reactions: no Promise
//! species, JavaScript shim, I/O, or implicit sink. Hosts must drain Boa jobs.
//! Register Text and Abort first. Native callbacks and state are GC traced;
//! callbacks run outside all state borrows. Queued writes are bounded to 10,000
//! and nested native sink callbacks to 64. Chunk sizes are strategy units, not
//! bytes; allocation/instruction budgets remain the embedding's responsibility.
//! TransformStream, pipeTo and transferable-stream protocols are separate layers.
use crate::web_abort;
use boa_engine::{
    Context, JsArgs, JsData, JsError, JsNativeError, JsObject, JsResult, JsString, JsSymbol,
    JsValue, NativeFunction,
    builtins::promise::{PromiseState, ResolvingFunctions},
    class::{Class, ClassBuilder},
    object::{FunctionObjectBuilder, builtins::JsPromise},
    property::{Attribute, PropertyDescriptor},
};
use boa_gc::{Finalize, Gc, GcRefCell, Trace};
use std::{cell::Cell, collections::VecDeque, rc::Rc};
const MAX_QUEUE: usize = 10_000;
type State = Gc<GcRefCell<Data>>;
type WriterState = Gc<GcRefCell<WriterData>>;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WritableStatus {
    Writable,
    Erroring,
    Errored,
    Closed,
}
#[derive(Clone, Trace, Finalize)]
struct Pending {
    promise: JsPromise,
    settle: ResolvingFunctions,
}
impl Pending {
    fn new(c: &mut Context) -> Self {
        let (promise, settle) = JsPromise::new_pending(c);
        Self { promise, settle }
    }
    fn resolve(&self, c: &mut Context) -> JsResult<()> {
        self.settle
            .resolve
            .call(&JsValue::undefined(), &[JsValue::undefined()], c)?;
        Ok(())
    }
    fn reject(&self, error: JsValue, c: &mut Context) -> JsResult<()> {
        self.settle
            .reject
            .call(&JsValue::undefined(), &[error], c)?;
        Ok(())
    }
}
#[derive(Trace, Finalize)]
struct Chunk {
    value: JsValue,
    size: f64,
}
#[derive(Trace, Finalize)]
struct AbortRequest {
    pending: Pending,
    reason: JsValue,
    already_erroring: bool,
}
#[derive(Trace, Finalize)]
struct Data {
    #[unsafe_ignore_trace]
    status: WritableStatus,
    error: JsValue,
    writer: Option<WriterState>,
    controller: Option<JsObject>,
    signal: JsObject,
    started: bool,
    backpressure: bool,
    hwm: f64,
    total: f64,
    queue: VecDeque<Chunk>,
    requests: VecDeque<Pending>,
    in_write: Option<Pending>,
    close: Option<Pending>,
    in_close: Option<Pending>,
    abort: Option<AbortRequest>,
    sink: JsValue,
    write: Option<JsObject>,
    close_algorithm: Option<JsObject>,
    abort_algorithm: Option<JsObject>,
    size: Option<JsObject>,
}
#[derive(Trace, Finalize)]
struct WriterData {
    stream: Option<State>,
    ready: Pending,
    closed: Pending,
}
#[derive(Trace, Finalize, JsData)]
struct WritableStream {
    state: State,
}
#[derive(Trace, Finalize, JsData)]
struct WritableStreamDefaultWriter {
    state: WriterState,
}
#[derive(Trace, Finalize, JsData)]
struct WritableStreamDefaultController {
    state: State,
}
/// Opaque traced native controller; no public-method lookup is performed.
#[derive(Clone, Trace, Finalize)]
pub struct WritableController {
    state: State,
}
/// Already-resolved native algorithms. Functions may be native traced functions
/// or JS callables. start/write receive the controller; size receives the chunk
/// with undefined this. write/close/abort assimilate returned promises.
#[derive(Clone, Trace, Finalize)]
pub struct WritableAlgorithms {
    pub this: JsValue,
    pub start: Option<JsObject>,
    pub write: Option<JsObject>,
    pub close: Option<JsObject>,
    pub abort: Option<JsObject>,
    pub size: Option<JsObject>,
    pub high_water_mark: f64,
}
impl Default for WritableAlgorithms {
    fn default() -> Self {
        Self {
            this: JsValue::undefined(),
            start: None,
            write: None,
            close: None,
            abort: None,
            size: None,
            high_water_mark: 1.0,
        }
    }
}
struct Registration {
    depth: Rc<Cell<usize>>,
}
struct Depth(Rc<Cell<usize>>);
impl Drop for Depth {
    fn drop(&mut self) {
        self.0.set(self.0.get() - 1);
    }
}
fn typ(message: &str) -> JsError {
    JsNativeError::typ().with_message(message.to_owned()).into()
}
fn range(message: &str) -> JsError {
    JsNativeError::range()
        .with_message(message.to_owned())
        .into()
}
fn call(f: &JsObject, this: &JsValue, args: &[JsValue], c: &mut Context) -> JsResult<JsValue> {
    let depth = c
        .get_data::<Registration>()
        .ok_or_else(|| typ("Writable streams not registered"))?
        .depth
        .clone();
    if depth.get() >= 64 {
        return Err(range("Writable sink callback nesting limit exceeded"));
    }
    depth.set(depth.get() + 1);
    let _depth = Depth(depth);
    f.call(this, args, c)
}
fn opaque(error: JsError, c: &mut Context) -> JsResult<JsValue> {
    error.into_opaque(c)
}
fn state(value: &JsValue) -> JsResult<State> {
    value
        .as_object()
        .and_then(|o| o.downcast_ref::<WritableStream>().map(|s| s.state.clone()))
        .ok_or_else(|| typ("WritableStream receiver required"))
}
fn writer(value: &JsValue) -> JsResult<WriterState> {
    value
        .as_object()
        .and_then(|o| {
            o.downcast_ref::<WritableStreamDefaultWriter>()
                .map(|s| s.state.clone())
        })
        .ok_or_else(|| typ("WritableStreamDefaultWriter receiver required"))
}
fn controller(value: &JsValue) -> JsResult<State> {
    value
        .as_object()
        .and_then(|o| {
            o.downcast_ref::<WritableStreamDefaultController>()
                .map(|s| s.state.clone())
        })
        .ok_or_else(|| typ("WritableStreamDefaultController receiver required"))
}
fn writer_stream(w: &WriterState) -> JsResult<State> {
    w.borrow()
        .stream
        .clone()
        .ok_or_else(|| typ("Writer has been released"))
}
fn closing(d: &Data) -> bool {
    d.close.is_some() || d.in_close.is_some()
}
fn resolved(c: &mut Context) -> JsResult<JsPromise> {
    JsPromise::resolve(JsValue::undefined(), c)
}
fn rejected(reason: JsValue, c: &mut Context) -> JsResult<JsPromise> {
    JsPromise::reject(JsError::from_opaque(reason), c)
}
fn promise_result(result: JsResult<JsPromise>, c: &mut Context) -> JsResult<JsPromise> {
    match result {
        Ok(p) => Ok(p),
        Err(e) => JsPromise::reject(e, c),
    }
}
fn handled(p: &JsPromise, c: &mut Context) -> JsResult<()> {
    let f =
        NativeFunction::from_fn_ptr(|_, _, _| Ok(JsValue::undefined())).to_js_function(c.realm());
    drop(p.then_intrinsic(None, Some(f), c)?);
    Ok(())
}
fn reject_writer(w: &WriterState, ready: bool, error: JsValue, c: &mut Context) -> JsResult<()> {
    let old = {
        let d = w.borrow();
        if ready {
            d.ready.clone()
        } else {
            d.closed.clone()
        }
    };
    let pending = if matches!(old.promise.state(), PromiseState::Pending) {
        old
    } else {
        let p = Pending::new(c);
        if ready {
            w.borrow_mut().ready = p.clone()
        } else {
            w.borrow_mut().closed = p.clone()
        }
        p
    };
    pending.reject(error, c)?;
    handled(&pending.promise, c)
}
fn clear(d: &mut Data) {
    d.write = None;
    d.close_algorithm = None;
    d.abort_algorithm = None;
    d.size = None;
    d.sink = JsValue::undefined();
}
fn backpressure(s: &State, c: &mut Context) -> JsResult<()> {
    let (changed, pressure, writer) = {
        let mut d = s.borrow_mut();
        let pressure = d.hwm - d.total <= 0.0;
        let changed = pressure != d.backpressure;
        d.backpressure = pressure;
        (changed, pressure, d.writer.clone())
    };
    if changed && let Some(w) = writer {
        if pressure {
            w.borrow_mut().ready = Pending::new(c)
        } else {
            let ready = w.borrow().ready.clone();
            ready.resolve(c)?
        }
    }
    Ok(())
}
fn reject_closed(s: &State, c: &mut Context) -> JsResult<()> {
    let (close, writer, error) = {
        let mut d = s.borrow_mut();
        (d.close.take(), d.writer.clone(), d.error.clone())
    };
    if let Some(p) = close {
        p.reject(error.clone(), c)?
    }
    if let Some(w) = writer {
        reject_writer(&w, false, error, c)?
    }
    Ok(())
}
fn start_erroring(s: &State, error: JsValue, c: &mut Context) -> JsResult<()> {
    let w = {
        let mut d = s.borrow_mut();
        if d.status != WritableStatus::Writable {
            return Ok(());
        }
        d.status = WritableStatus::Erroring;
        d.error = error.clone();
        d.writer.clone()
    };
    if let Some(w) = w {
        reject_writer(&w, true, error, c)?
    }
    finish_erroring(s, c)
}
fn deal_rejection(s: &State, error: JsValue, c: &mut Context) -> JsResult<()> {
    if s.borrow().status == WritableStatus::Writable {
        start_erroring(s, error, c)
    } else {
        finish_erroring(s, c)
    }
}
fn error_controller(s: &State, error: JsValue, c: &mut Context) -> JsResult<()> {
    if s.borrow().status == WritableStatus::Writable {
        clear(&mut s.borrow_mut());
        start_erroring(s, error, c)?
    }
    Ok(())
}
fn algorithm(
    f: Option<JsObject>,
    this: JsValue,
    args: &[JsValue],
    c: &mut Context,
) -> JsResult<JsPromise> {
    let result = match f {
        Some(f) => call(&f, &this, args, c),
        None => Ok(JsValue::undefined()),
    };
    let result = match result {
        Ok(v) => JsPromise::resolve(v, c),
        Err(e) => Err(e),
    };
    promise_result(result, c)
}
fn finish_erroring(s: &State, c: &mut Context) -> JsResult<()> {
    let (requests, abort, error) = {
        let mut d = s.borrow_mut();
        if d.status != WritableStatus::Erroring
            || !d.started
            || d.in_write.is_some()
            || d.in_close.is_some()
        {
            return Ok(());
        }
        d.status = WritableStatus::Errored;
        d.queue.clear();
        d.total = 0.0;
        (
            std::mem::take(&mut d.requests),
            d.abort.take(),
            d.error.clone(),
        )
    };
    for p in requests {
        p.reject(error.clone(), c)?
    }
    let Some(abort) = abort else {
        return reject_closed(s, c);
    };
    if abort.already_erroring {
        abort.pending.reject(error, c)?;
        return reject_closed(s, c);
    }
    let (function, this) = {
        let d = s.borrow();
        (d.abort_algorithm.clone(), d.sink.clone())
    };
    let promise = algorithm(function, this, std::slice::from_ref(&abort.reason), c)?;
    clear(&mut s.borrow_mut());
    let success = NativeFunction::from_copy_closure_with_captures(
        |_, _, capture, c| {
            capture.1.resolve(c)?;
            reject_closed(&capture.0, c)?;
            Ok(JsValue::undefined())
        },
        (s.clone(), abort.pending.clone()),
    )
    .to_js_function(c.realm());
    let failure = NativeFunction::from_copy_closure_with_captures(
        |_, args, capture, c| {
            capture.1.reject(args.get_or_undefined(0).clone(), c)?;
            reject_closed(&capture.0, c)?;
            Ok(JsValue::undefined())
        },
        (s.clone(), abort.pending.clone()),
    )
    .to_js_function(c.realm());
    drop(promise.then_intrinsic(Some(success), Some(failure), c)?);
    Ok(())
}
fn advance(s: &State, c: &mut Context) -> JsResult<()> {
    {
        let d = s.borrow();
        if !d.started
            || d.in_write.is_some()
            || d.in_close.is_some()
            || matches!(d.status, WritableStatus::Closed | WritableStatus::Errored)
        {
            return Ok(());
        }
    }
    if s.borrow().status == WritableStatus::Erroring {
        return finish_erroring(s, c);
    }
    let operation = {
        let mut d = s.borrow_mut();
        if let Some(chunk) = d.queue.front() {
            let value = chunk.value.clone();
            d.in_write = d.requests.pop_front();
            Some((
                false,
                d.write.clone(),
                d.sink.clone(),
                vec![value, d.controller.clone().unwrap().into()],
            ))
        } else if d.close.is_some() {
            d.in_close = d.close.take();
            Some((true, d.close_algorithm.clone(), d.sink.clone(), vec![]))
        } else {
            None
        }
    };
    let Some((is_close, function, this, args)) = operation else {
        return Ok(());
    };
    let promise = algorithm(function, this, &args, c)?;
    if is_close {
        clear(&mut s.borrow_mut())
    }
    let success = NativeFunction::from_copy_closure_with_captures(
        move |_, _, s, c| {
            finish_operation(s, is_close, None, c)?;
            Ok(JsValue::undefined())
        },
        s.clone(),
    )
    .to_js_function(c.realm());
    let failure = NativeFunction::from_copy_closure_with_captures(
        move |_, args, s, c| {
            finish_operation(s, is_close, Some(args.get_or_undefined(0).clone()), c)?;
            Ok(JsValue::undefined())
        },
        s.clone(),
    )
    .to_js_function(c.realm());
    drop(promise.then_intrinsic(Some(success), Some(failure), c)?);
    Ok(())
}
fn finish_operation(
    s: &State,
    is_close: bool,
    error: Option<JsValue>,
    c: &mut Context,
) -> JsResult<()> {
    let pending = {
        let mut d = s.borrow_mut();
        if is_close {
            d.in_close.take()
        } else {
            d.in_write.take()
        }
    }
    .ok_or_else(|| typ("Missing in-flight writable operation"))?;
    if let Some(error) = error {
        if !is_close && s.borrow().status == WritableStatus::Writable {
            clear(&mut s.borrow_mut())
        }
        pending.reject(error.clone(), c)?;
        if is_close {
            let abort = s.borrow_mut().abort.take();
            if let Some(a) = abort {
                a.pending.reject(error.clone(), c)?
            }
        }
        return deal_rejection(s, error, c);
    }
    pending.resolve(c)?;
    if is_close {
        let (abort, writer) = {
            let mut d = s.borrow_mut();
            d.error = JsValue::undefined();
            d.status = WritableStatus::Closed;
            (d.abort.take(), d.writer.clone())
        };
        if let Some(a) = abort {
            a.pending.resolve(c)?
        }
        if let Some(w) = writer {
            let closed = w.borrow().closed.clone();
            closed.resolve(c)?
        }
        return Ok(());
    }
    let update = {
        let mut d = s.borrow_mut();
        if let Some(chunk) = d.queue.pop_front() {
            d.total = (d.total - chunk.size).max(0.0)
        }
        !closing(&d) && d.status == WritableStatus::Writable
    };
    if update {
        backpressure(s, c)?
    }
    advance(s, c)
}
fn start(s: &State, value: JsValue, c: &mut Context) -> JsResult<()> {
    let promise = JsPromise::resolve(value, c)?;
    let success = NativeFunction::from_copy_closure_with_captures(
        |_, _, s, c| {
            s.borrow_mut().started = true;
            advance(s, c)?;
            Ok(JsValue::undefined())
        },
        s.clone(),
    )
    .to_js_function(c.realm());
    let failure = NativeFunction::from_copy_closure_with_captures(
        |_, args, s, c| {
            s.borrow_mut().started = true;
            deal_rejection(s, args.get_or_undefined(0).clone(), c)?;
            Ok(JsValue::undefined())
        },
        s.clone(),
    )
    .to_js_function(c.realm());
    drop(promise.then_intrinsic(Some(success), Some(failure), c)?);
    Ok(())
}
fn make_state(algorithms: WritableAlgorithms, c: &mut Context) -> JsResult<State> {
    if algorithms.high_water_mark.is_nan() || algorithms.high_water_mark < 0.0 {
        return Err(range("Invalid writable highWaterMark"));
    }
    for f in [
        &algorithms.start,
        &algorithms.write,
        &algorithms.close,
        &algorithms.abort,
        &algorithms.size,
    ]
    .into_iter()
    .flatten()
    {
        if !f.is_callable() {
            return Err(typ("Writable algorithm must be callable"));
        }
    }
    let signal = web_abort::new_signal(c)?;
    let s = Gc::new(GcRefCell::new(Data {
        status: WritableStatus::Writable,
        error: JsValue::undefined(),
        writer: None,
        controller: None,
        signal,
        started: false,
        backpressure: algorithms.high_water_mark <= 0.0,
        hwm: algorithms.high_water_mark,
        total: 0.0,
        queue: VecDeque::new(),
        requests: VecDeque::new(),
        in_write: None,
        close: None,
        in_close: None,
        abort: None,
        sink: algorithms.this.clone(),
        write: algorithms.write.clone(),
        close_algorithm: algorithms.close.clone(),
        abort_algorithm: algorithms.abort.clone(),
        size: algorithms.size.clone(),
    }));
    let controller = WritableStreamDefaultController::from_data(
        WritableStreamDefaultController { state: s.clone() },
        c,
    )?;
    s.borrow_mut().controller = Some(controller.clone());
    let value = match &algorithms.start {
        Some(f) => call(f, &algorithms.this, &[controller.into()], c)?,
        None => JsValue::undefined(),
    };
    start(&s, value, c)?;
    Ok(s)
}
/// Native creation bypasses public constructors and property lookups.
pub fn new_writable(
    algorithms: WritableAlgorithms,
    c: &mut Context,
) -> JsResult<(JsObject, WritableController)> {
    let state = make_state(algorithms, c)?;
    let object = WritableStream::from_data(
        WritableStream {
            state: state.clone(),
        },
        c,
    )?;
    Ok((object, WritableController { state }))
}
impl WritableController {
    pub fn error(&self, reason: JsValue, c: &mut Context) -> JsResult<()> {
        error_controller(&self.state, reason, c)
    }
    pub fn signal(&self) -> JsObject {
        self.state.borrow().signal.clone()
    }
    pub fn status(&self) -> WritableStatus {
        self.state.borrow().status
    }
}
pub fn is_writable(value: &JsValue) -> bool {
    state(value).is_ok()
}
pub fn is_locked(value: &JsValue) -> JsResult<bool> {
    Ok(state(value)?.borrow().writer.is_some())
}
pub fn status(value: &JsValue) -> JsResult<WritableStatus> {
    Ok(state(value)?.borrow().status)
}
pub fn stored_error(value: &JsValue) -> JsResult<JsValue> {
    Ok(state(value)?.borrow().error.clone())
}
pub fn is_closing(value: &JsValue) -> JsResult<bool> {
    Ok(closing(&state(value)?.borrow()))
}
fn acquire(s: &State, c: &mut Context) -> JsResult<WriterState> {
    if s.borrow().writer.is_some() {
        return Err(typ("Writable stream is locked"));
    }
    let w = Gc::new(GcRefCell::new(WriterData {
        stream: Some(s.clone()),
        ready: Pending::new(c),
        closed: Pending::new(c),
    }));
    s.borrow_mut().writer = Some(w.clone());
    let (status, error, pressure, closing) = {
        let d = s.borrow();
        (d.status, d.error.clone(), d.backpressure, closing(&d))
    };
    match status {
        WritableStatus::Writable => {
            if !pressure || closing {
                let p = w.borrow().ready.clone();
                p.resolve(c)?
            }
        }
        WritableStatus::Erroring => reject_writer(&w, true, error, c)?,
        WritableStatus::Errored => {
            reject_writer(&w, true, error.clone(), c)?;
            reject_writer(&w, false, error, c)?
        }
        WritableStatus::Closed => {
            let (ready, closed) = {
                let d = w.borrow();
                (d.ready.clone(), d.closed.clone())
            };
            ready.resolve(c)?;
            closed.resolve(c)?
        }
    }
    Ok(w)
}
pub fn acquire_writer(value: &JsValue, c: &mut Context) -> JsResult<JsObject> {
    let state = acquire(&state(value)?, c)?;
    WritableStreamDefaultWriter::from_data(WritableStreamDefaultWriter { state }, c)
}
pub fn ready(value: &JsValue) -> JsResult<JsPromise> {
    Ok(writer(value)?.borrow().ready.promise.clone())
}
pub fn closed(value: &JsValue) -> JsResult<JsPromise> {
    Ok(writer(value)?.borrow().closed.promise.clone())
}
pub fn desired_size(value: &JsValue) -> JsResult<Option<f64>> {
    let s = writer_stream(&writer(value)?)?;
    let d = s.borrow();
    Ok(match d.status {
        WritableStatus::Erroring | WritableStatus::Errored => None,
        WritableStatus::Closed => Some(0.0),
        WritableStatus::Writable => Some(d.hwm - d.total),
    })
}
pub fn release_writer(value: &JsValue, c: &mut Context) -> JsResult<()> {
    let w = writer(value)?;
    let Some(s) = w.borrow().stream.clone() else {
        return Ok(());
    };
    let error = opaque(typ("Writer has been released"), c)?;
    reject_writer(&w, true, error.clone(), c)?;
    reject_writer(&w, false, error, c)?;
    s.borrow_mut().writer = None;
    w.borrow_mut().stream = None;
    Ok(())
}
fn write_inner(value: &JsValue, chunk: JsValue, c: &mut Context) -> JsResult<JsPromise> {
    let w = writer(value)?;
    let s = writer_stream(&w)?;
    let size = s.borrow().size.clone();
    let size = match size {
        Some(size) => match call(
            &size,
            &JsValue::undefined(),
            std::slice::from_ref(&chunk),
            c,
        )
        .and_then(|v| v.to_number(c))
        {
            Ok(v) => v,
            Err(e) => {
                let error = opaque(e, c)?;
                error_controller(&s, error, c)?;
                1.0
            }
        },
        None => 1.0,
    };
    if !w
        .borrow()
        .stream
        .as_ref()
        .is_some_and(|current| Gc::ptr_eq(current, &s))
    {
        return Err(typ("Writer was released during size calculation"));
    }
    {
        let d = s.borrow();
        if d.status == WritableStatus::Errored {
            return rejected(d.error.clone(), c);
        }
        if closing(&d) || d.status == WritableStatus::Closed {
            return Err(typ("Writable stream is closing or closed"));
        }
        if d.status == WritableStatus::Erroring {
            return rejected(d.error.clone(), c);
        }
    }
    if s.borrow().requests.len() + usize::from(s.borrow().in_write.is_some()) >= MAX_QUEUE {
        let error = opaque(range("Writable queued write limit exceeded"), c)?;
        error_controller(&s, error.clone(), c)?;
        return rejected(error, c);
    }
    let pending = Pending::new(c);
    s.borrow_mut().requests.push_back(pending.clone());
    if !size.is_finite() || size < 0.0 {
        let error = opaque(range("Invalid writable chunk size"), c)?;
        error_controller(&s, error, c)?;
        return Ok(pending.promise.clone());
    }
    let update = {
        let mut d = s.borrow_mut();
        d.queue.push_back(Chunk { value: chunk, size });
        d.total += size;
        !closing(&d) && d.status == WritableStatus::Writable
    };
    if update {
        backpressure(&s, c)?
    }
    advance(&s, c)?;
    Ok(pending.promise.clone())
}
/// Native writer operation. Brand/release failures become rejected promises.
pub fn write(value: &JsValue, chunk: JsValue, c: &mut Context) -> JsResult<JsPromise> {
    let result = write_inner(value, chunk, c);
    promise_result(result, c)
}
fn close_state(s: &State, c: &mut Context) -> JsResult<JsPromise> {
    {
        let d = s.borrow();
        if matches!(d.status, WritableStatus::Closed | WritableStatus::Errored) || closing(&d) {
            return Err(typ("Writable stream is closing, closed or errored"));
        }
    }
    let p = Pending::new(c);
    let writer = {
        let mut d = s.borrow_mut();
        d.close = Some(p.clone());
        if d.backpressure && d.status == WritableStatus::Writable {
            d.writer.clone()
        } else {
            None
        }
    };
    if let Some(w) = writer {
        let ready = w.borrow().ready.clone();
        ready.resolve(c)?
    }
    advance(s, c)?;
    Ok(p.promise.clone())
}
pub fn close_writer(value: &JsValue, c: &mut Context) -> JsResult<JsPromise> {
    let result = (|| close_state(&writer_stream(&writer(value)?)?, c))();
    promise_result(result, c)
}
/// WHATWG close-with-error-propagation operation for a later pipeTo layer.
/// Already-closing/closed succeeds; errored rejects with the stored reason.
pub fn close_with_error_propagation(value: &JsValue, c: &mut Context) -> JsResult<JsPromise> {
    let result = (|| {
        let s = writer_stream(&writer(value)?)?;
        let (status, is_closing, error) = {
            let d = s.borrow();
            (d.status, closing(&d), d.error.clone())
        };
        if is_closing || status == WritableStatus::Closed {
            return resolved(c);
        }
        if status == WritableStatus::Errored {
            return rejected(error, c);
        }
        close_state(&s, c)
    })();
    promise_result(result, c)
}
/// Abstract close usable by pipeTo while its writer owns the lock.
pub fn close_stream(value: &JsValue, c: &mut Context) -> JsResult<JsPromise> {
    let result = (|| close_state(&state(value)?, c))();
    promise_result(result, c)
}
fn abort_state(s: &State, reason: JsValue, c: &mut Context) -> JsResult<JsPromise> {
    if matches!(
        s.borrow().status,
        WritableStatus::Closed | WritableStatus::Errored
    ) {
        return resolved(c);
    }
    let signal = s.borrow().signal.clone();
    web_abort::abort_signal(&signal, reason.clone(), c)?;
    if matches!(
        s.borrow().status,
        WritableStatus::Closed | WritableStatus::Errored
    ) {
        return resolved(c);
    }
    if let Some(abort) = &s.borrow().abort {
        return Ok(abort.pending.promise.clone());
    }
    let already_erroring = s.borrow().status == WritableStatus::Erroring;
    let p = Pending::new(c);
    s.borrow_mut().abort = Some(AbortRequest {
        pending: p.clone(),
        reason: if already_erroring {
            JsValue::undefined()
        } else {
            reason.clone()
        },
        already_erroring,
    });
    if !already_erroring {
        start_erroring(s, reason, c)?
    }
    Ok(p.promise.clone())
}
pub fn abort_writer(value: &JsValue, reason: JsValue, c: &mut Context) -> JsResult<JsPromise> {
    let result = (|| abort_state(&writer_stream(&writer(value)?)?, reason, c))();
    promise_result(result, c)
}
/// Abstract abort usable by pipeTo while its writer owns the lock.
pub fn abort_stream(value: &JsValue, reason: JsValue, c: &mut Context) -> JsResult<JsPromise> {
    let result = (|| abort_state(&state(value)?, reason, c))();
    promise_result(result, c)
}
fn dict(value: &JsValue) -> JsResult<Option<JsObject>> {
    if value.is_null_or_undefined() {
        Ok(None)
    } else {
        value
            .as_object()
            .map(Some)
            .ok_or_else(|| typ("Writable dictionary must be an object"))
    }
}
fn get(dict: &Option<JsObject>, key: &str, c: &mut Context) -> JsResult<JsValue> {
    match dict {
        Some(d) => d.get(JsString::from(key), c),
        None => Ok(JsValue::undefined()),
    }
}
fn callback(value: JsValue) -> JsResult<Option<JsObject>> {
    if value.is_undefined() {
        Ok(None)
    } else {
        value
            .as_callable()
            .map(Some)
            .ok_or_else(|| typ("Writable callback must be callable"))
    }
}
fn constructor(args: &[JsValue], c: &mut Context) -> JsResult<State> {
    // strategy is an IDL dictionary parameter; it converts before the body of
    // the constructor converts the `any` underlyingSink argument.
    let strategy = dict(args.get_or_undefined(1))?;
    let hwm = get(&strategy, "highWaterMark", c)?;
    let hwm = if hwm.is_undefined() {
        1.0
    } else {
        hwm.to_number(c)?
    };
    let size = callback(get(&strategy, "size", c)?)?;
    let sink = dict(args.get_or_undefined(0))?;
    let abort = callback(get(&sink, "abort", c)?)?;
    let close = callback(get(&sink, "close", c)?)?;
    let start = callback(get(&sink, "start", c)?)?;
    let kind = get(&sink, "type", c)?;
    let write = callback(get(&sink, "write", c)?)?;
    if !kind.is_undefined() {
        return Err(range("Writable stream type must be undefined"));
    }
    make_state(
        WritableAlgorithms {
            this: sink.map(JsValue::from).unwrap_or_else(JsValue::null),
            start,
            write,
            close,
            abort,
            size,
            high_water_mark: hwm,
        },
        c,
    )
}
fn accessor(class: &mut ClassBuilder, key: &str, f: NativeFunction) {
    let getter = FunctionObjectBuilder::new(class.context().realm(), f).build();
    class.accessor(
        JsString::from(key),
        Some(getter),
        None,
        Attribute::ENUMERABLE.union(Attribute::CONFIGURABLE),
    );
}
fn tag(class: &mut ClassBuilder, name: &str) {
    class.property(
        JsSymbol::to_string_tag(),
        JsString::from(name),
        Attribute::CONFIGURABLE,
    );
}
impl Class for WritableStream {
    const NAME: &'static str = "WritableStream";
    const ATTRIBUTES: Attribute = Attribute::WRITABLE.union(Attribute::CONFIGURABLE);
    fn data_constructor(_: &JsValue, args: &[JsValue], c: &mut Context) -> JsResult<Self> {
        Ok(Self {
            state: constructor(args, c)?,
        })
    }
    fn init(class: &mut ClassBuilder) -> JsResult<()> {
        accessor(
            class,
            "locked",
            NativeFunction::from_fn_ptr(|this, _, _| is_locked(this).map(JsValue::from)),
        );
        class.method(
            JsString::from("getWriter"),
            0,
            NativeFunction::from_fn_ptr(|this, _, c| acquire_writer(this, c).map(JsValue::from)),
        );
        for name in ["close", "abort"] {
            class.method(
                JsString::from(name),
                0,
                NativeFunction::from_copy_closure(move |this, args, c| {
                    let result = (|| {
                        let s = state(this)?;
                        if s.borrow().writer.is_some() {
                            return Err(typ("Writable stream is locked"));
                        }
                        if name == "close" {
                            close_state(&s, c)
                        } else {
                            abort_state(&s, args.get_or_undefined(0).clone(), c)
                        }
                    })();
                    promise_result(result, c).map(JsValue::from)
                }),
            );
        }
        tag(class, Self::NAME);
        Ok(())
    }
}
impl Class for WritableStreamDefaultWriter {
    const NAME: &'static str = "WritableStreamDefaultWriter";
    const LENGTH: usize = 1;
    const ATTRIBUTES: Attribute = Attribute::WRITABLE.union(Attribute::CONFIGURABLE);
    fn data_constructor(_: &JsValue, args: &[JsValue], c: &mut Context) -> JsResult<Self> {
        Ok(Self {
            state: acquire(&state(args.get_or_undefined(0))?, c)?,
        })
    }
    fn init(class: &mut ClassBuilder) -> JsResult<()> {
        accessor(
            class,
            "desiredSize",
            NativeFunction::from_fn_ptr(|this, _, _| {
                Ok(desired_size(this)?
                    .map(JsValue::from)
                    .unwrap_or_else(JsValue::null))
            }),
        );
        accessor(
            class,
            "ready",
            NativeFunction::from_fn_ptr(|this, _, c| {
                promise_result(ready(this), c).map(JsValue::from)
            }),
        );
        accessor(
            class,
            "closed",
            NativeFunction::from_fn_ptr(|this, _, c| {
                promise_result(closed(this), c).map(JsValue::from)
            }),
        );
        class.method(
            JsString::from("write"),
            0,
            NativeFunction::from_fn_ptr(|this, args, c| {
                write(this, args.get_or_undefined(0).clone(), c).map(JsValue::from)
            }),
        );
        class.method(
            JsString::from("close"),
            0,
            NativeFunction::from_fn_ptr(|this, _, c| close_writer(this, c).map(JsValue::from)),
        );
        class.method(
            JsString::from("abort"),
            0,
            NativeFunction::from_fn_ptr(|this, args, c| {
                abort_writer(this, args.get_or_undefined(0).clone(), c).map(JsValue::from)
            }),
        );
        class.method(
            JsString::from("releaseLock"),
            0,
            NativeFunction::from_fn_ptr(|this, _, c| {
                release_writer(this, c)?;
                Ok(JsValue::undefined())
            }),
        );
        tag(class, Self::NAME);
        Ok(())
    }
}
impl Class for WritableStreamDefaultController {
    const NAME: &'static str = "WritableStreamDefaultController";
    const ATTRIBUTES: Attribute = Attribute::WRITABLE.union(Attribute::CONFIGURABLE);
    fn data_constructor(_: &JsValue, _: &[JsValue], _: &mut Context) -> JsResult<Self> {
        Err(typ("Illegal WritableStreamDefaultController constructor"))
    }
    fn init(class: &mut ClassBuilder) -> JsResult<()> {
        accessor(
            class,
            "signal",
            NativeFunction::from_fn_ptr(|this, _, _| {
                Ok(controller(this)?.borrow().signal.clone().into())
            }),
        );
        class.method(
            JsString::from("error"),
            0,
            NativeFunction::from_fn_ptr(|this, args, c| {
                error_controller(&controller(this)?, args.get_or_undefined(0).clone(), c)?;
                Ok(JsValue::undefined())
            }),
        );
        tag(class, Self::NAME);
        Ok(())
    }
}
/// Register after Text + Abort. No global stream class is consulted internally.
pub fn register(c: &mut Context) -> JsResult<()> {
    if c.has_data::<Registration>() {
        return Ok(());
    }
    c.insert_data(Registration {
        depth: Rc::new(Cell::new(0)),
    });
    c.register_global_class::<WritableStream>()?;
    c.register_global_class::<WritableStreamDefaultWriter>()?;
    c.register_global_class::<WritableStreamDefaultController>()?;
    for (prototype, names) in [
        (
            c.get_global_class::<WritableStream>().unwrap().prototype(),
            &["getWriter", "close", "abort"][..],
        ),
        (
            c.get_global_class::<WritableStreamDefaultWriter>()
                .unwrap()
                .prototype(),
            &["write", "close", "abort", "releaseLock"][..],
        ),
        (
            c.get_global_class::<WritableStreamDefaultController>()
                .unwrap()
                .prototype(),
            &["error"][..],
        ),
    ] {
        for name in names {
            prototype.define_property_or_throw(
                JsString::from(*name),
                PropertyDescriptor::builder().enumerable(true),
                c,
            )?;
        }
    }
    Ok(())
}
#[cfg(test)]
#[path = "web_writable_tests.rs"]
mod tests;
