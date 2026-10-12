//! Native WHATWG TransformStream and default controller, composed from the
//! native default readable and writable engines. Source algorithms:
//! https://streams.spec.whatwg.org/#ts-abstract-ops (18 August 2026).
//!
//! The shared start promise gates both sides. Real read demand clears
//! backpressure; writes never fabricate reads. Flush/cancel share one finish
//! promise, and all reactions use Boa's native intrinsic promise operation.
//! No I/O, JS polyfill, implicit byte transform or transferable-stream protocol.
//! Register after Text/Abort/Readable/Writable. The underlying queue limits apply;
//! native transformer callbacks additionally have a 64-deep nesting bound.
use crate::{
    web_streams::{self, ReadableAlgorithms, ReadableController},
    web_writable::{self, WritableAlgorithms, WritableController, WritableStatus},
};
use boa_engine::{
    Context, JsArgs, JsData, JsError, JsNativeError, JsObject, JsResult, JsString, JsSymbol,
    JsValue, NativeFunction,
    builtins::promise::ResolvingFunctions,
    class::{Class, ClassBuilder},
    object::{FunctionObjectBuilder, builtins::JsPromise},
    property::{Attribute, PropertyDescriptor},
};
use boa_gc::{Finalize, Gc, GcRefCell, Trace};
use std::{cell::Cell, rc::Rc};
type State = Gc<GcRefCell<Data>>;
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
    fn resolve(&self, value: JsValue, c: &mut Context) -> JsResult<()> {
        self.settle
            .resolve
            .call(&JsValue::undefined(), &[value], c)?;
        Ok(())
    }
    fn reject(&self, value: JsValue, c: &mut Context) -> JsResult<()> {
        self.settle
            .reject
            .call(&JsValue::undefined(), &[value], c)?;
        Ok(())
    }
}
#[derive(Trace, Finalize)]
struct Data {
    readable: Option<JsObject>,
    readable_controller: Option<ReadableController>,
    writable: Option<JsObject>,
    writable_controller: Option<WritableController>,
    controller: Option<JsObject>,
    backpressure: bool,
    change: Pending,
    finish: Option<Pending>,
    transformer: JsValue,
    transform: Option<JsObject>,
    flush: Option<JsObject>,
    cancel: Option<JsObject>,
}
#[derive(Trace, Finalize, JsData)]
struct TransformStream {
    state: State,
}
#[derive(Trace, Finalize, JsData)]
struct TransformStreamDefaultController {
    state: State,
}
/// Traced native controller for future TextDecoderStream and other transforms.
#[derive(Clone, Trace, Finalize)]
pub struct TransformController {
    state: State,
}
/// Resolved callbacks: transform(chunk,controller), flush(controller),
/// cancel(reason), start(controller), each with `this` as receiver. None for
/// transform means identity. Size functions use undefined this.
#[derive(Clone, Trace, Finalize)]
pub struct TransformAlgorithms {
    pub this: JsValue,
    pub start: Option<JsObject>,
    pub transform: Option<JsObject>,
    pub flush: Option<JsObject>,
    pub cancel: Option<JsObject>,
    pub writable_high_water_mark: f64,
    pub writable_size: Option<JsObject>,
    pub readable_high_water_mark: f64,
    pub readable_size: Option<JsObject>,
}
impl Default for TransformAlgorithms {
    fn default() -> Self {
        Self {
            this: JsValue::undefined(),
            start: None,
            transform: None,
            flush: None,
            cancel: None,
            writable_high_water_mark: 1.0,
            writable_size: None,
            readable_high_water_mark: 0.0,
            readable_size: None,
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
fn state(v: &JsValue) -> JsResult<State> {
    v.as_object()
        .and_then(|o| o.downcast_ref::<TransformStream>().map(|s| s.state.clone()))
        .ok_or_else(|| typ("TransformStream receiver required"))
}
fn controller(v: &JsValue) -> JsResult<State> {
    v.as_object()
        .and_then(|o| {
            o.downcast_ref::<TransformStreamDefaultController>()
                .map(|s| s.state.clone())
        })
        .ok_or_else(|| typ("TransformStreamDefaultController receiver required"))
}
fn rc(s: &State) -> ReadableController {
    s.borrow().readable_controller.as_ref().unwrap().clone()
}
fn wc(s: &State) -> WritableController {
    s.borrow().writable_controller.as_ref().unwrap().clone()
}
fn writable_value(s: &State) -> JsValue {
    s.borrow().writable.clone().unwrap().into()
}
fn clear(s: &State) {
    let mut d = s.borrow_mut();
    d.transform = None;
    d.flush = None;
    d.cancel = None;
    d.transformer = JsValue::undefined();
}
fn call(f: &JsObject, this: &JsValue, args: &[JsValue], c: &mut Context) -> JsResult<JsValue> {
    let depth = c
        .get_data::<Registration>()
        .ok_or_else(|| typ("Transform streams not registered"))?
        .depth
        .clone();
    if depth.get() >= 64 {
        return Err(range("Transform callback nesting limit exceeded"));
    }
    depth.set(depth.get() + 1);
    let _depth = Depth(depth);
    f.call(this, args, c)
}
fn promise(result: JsResult<JsValue>, c: &mut Context) -> JsResult<JsPromise> {
    // Web IDL Promise<T> conversion creates a fresh capability, unlike
    // Promise.resolve's same-constructor fast path. Assimilation can legitimately
    // invoke a returned promise's .then; only internal reactions bypass species.
    let pending = Pending::new(c);
    match result {
        Ok(value) => pending.resolve(value, c)?,
        Err(error) => pending.reject(error.into_opaque(c)?, c)?,
    }
    Ok(pending.promise.clone())
}

fn set_backpressure(s: &State, value: bool, c: &mut Context) -> JsResult<()> {
    let old = s.borrow().change.clone();
    old.resolve(JsValue::undefined(), c)?;
    let change = Pending::new(c);
    let mut d = s.borrow_mut();
    d.change = change;
    d.backpressure = value;
    Ok(())
}
fn unblock(s: &State, c: &mut Context) -> JsResult<()> {
    if s.borrow().backpressure {
        set_backpressure(s, false, c)?
    }
    Ok(())
}
fn error_writable(s: &State, reason: JsValue, c: &mut Context) -> JsResult<()> {
    clear(s);
    wc(s).error(reason, c)?;
    unblock(s, c)
}
fn error(s: &State, reason: JsValue, c: &mut Context) -> JsResult<()> {
    rc(s).error(reason.clone(), c)?;
    error_writable(s, reason, c)
}
fn enqueue(s: &State, value: JsValue, c: &mut Context) -> JsResult<()> {
    let readable = rc(s);
    if !readable.can_close_or_enqueue() {
        return Err(typ("Cannot enqueue into this transform"));
    }
    if let Err(error) = readable.enqueue(value, c) {
        let reason = error.into_opaque(c)?;
        error_writable(s, reason, c)?;
        return Err(JsError::from_opaque(readable.stored_error()));
    }
    let pressure = readable.has_backpressure();
    if pressure != s.borrow().backpressure {
        set_backpressure(s, pressure, c)?
    }
    Ok(())
}
fn terminate(s: &State, c: &mut Context) -> JsResult<()> {
    let readable = rc(s);
    if readable.can_close_or_enqueue() {
        readable.close(c)?
    }
    let reason = typ("Transform stream was terminated").into_opaque(c)?;
    error_writable(s, reason, c)
}
fn perform_transform(s: &State, value: JsValue, c: &mut Context) -> JsResult<JsPromise> {
    let (function, this, controller) = {
        let d = s.borrow();
        (
            d.transform.clone(),
            d.transformer.clone(),
            d.controller.clone().unwrap(),
        )
    };
    let result = match function {
        Some(f) => call(&f, &this, &[value, controller.into()], c),
        None => enqueue(s, value, c).map(|_| JsValue::undefined()),
    };
    let p = promise(result, c)?;
    let reject = NativeFunction::from_copy_closure_with_captures(
        |_, args, s, c| {
            let reason = args.get_or_undefined(0).clone();
            error(s, reason.clone(), c)?;
            Err(JsError::from_opaque(reason))
        },
        s.clone(),
    )
    .to_js_function(c.realm());
    // Promise<undefined> fulfillment converts its value to undefined.
    let fulfilled =
        NativeFunction::from_fn_ptr(|_, _, _| Ok(JsValue::undefined())).to_js_function(c.realm());
    p.then_intrinsic(Some(fulfilled), Some(reject), c)
}
fn sink_write(s: &State, value: JsValue, c: &mut Context) -> JsResult<JsPromise> {
    let waiting = {
        let d = s.borrow();
        if d.backpressure {
            Some(d.change.promise.clone())
        } else {
            None
        }
    };
    if let Some(p) = waiting {
        let resume = NativeFunction::from_copy_closure_with_captures(
            |_, _, capture, c| {
                let writable = writable_value(&capture.0);
                if matches!(
                    web_writable::status(&writable)?,
                    WritableStatus::Erroring | WritableStatus::Errored
                ) {
                    return Err(JsError::from_opaque(web_writable::stored_error(&writable)?));
                }
                perform_transform(&capture.0, capture.1.clone(), c).map(JsValue::from)
            },
            (s.clone(), value),
        )
        .to_js_function(c.realm());
        p.then_intrinsic(Some(resume), None, c)
    } else {
        perform_transform(s, value, c)
    }
}
fn source_pull(s: &State, c: &mut Context) -> JsResult<JsPromise> {
    set_backpressure(s, false, c)?;
    Ok(s.borrow().change.promise.clone())
}
#[derive(Clone, Copy)]
enum FinishKind {
    Close,
    Abort,
    Cancel,
}
fn finish(s: &State, kind: FinishKind, reason: JsValue, c: &mut Context) -> JsResult<JsPromise> {
    if let Some(p) = &s.borrow().finish {
        return Ok(p.promise.clone());
    }
    let pending = Pending::new(c);
    s.borrow_mut().finish = Some(pending.clone());
    let (function, this, controller) = {
        let d = s.borrow();
        (
            if matches!(kind, FinishKind::Close) {
                d.flush.clone()
            } else {
                d.cancel.clone()
            },
            d.transformer.clone(),
            d.controller.clone().unwrap(),
        )
    };
    let argument = if matches!(kind, FinishKind::Close) {
        controller.into()
    } else {
        reason.clone()
    };
    let result = match function {
        Some(f) => call(&f, &this, &[argument], c),
        None => Ok(JsValue::undefined()),
    };
    let operation = promise(result, c)?;
    clear(s);
    let success = NativeFunction::from_copy_closure_with_captures(
        move |_, _, capture, c| {
            let (s, pending, reason) = capture;
            match kind {
                FinishKind::Cancel => {
                    let writable = writable_value(s);
                    if web_writable::status(&writable)? == WritableStatus::Errored {
                        pending.reject(web_writable::stored_error(&writable)?, c)?
                    } else {
                        wc(s).error(reason.clone(), c)?;
                        unblock(s, c)?;
                        pending.resolve(JsValue::undefined(), c)?
                    }
                }
                FinishKind::Close | FinishKind::Abort => {
                    let readable = rc(s);
                    if readable.is_errored() {
                        pending.reject(readable.stored_error(), c)?
                    } else {
                        if matches!(kind, FinishKind::Close) {
                            if readable.can_close_or_enqueue() {
                                readable.close(c)?
                            }
                        } else {
                            readable.error(reason.clone(), c)?
                        }
                        pending.resolve(JsValue::undefined(), c)?;
                    }
                }
            }
            Ok(JsValue::undefined())
        },
        (s.clone(), pending.clone(), reason),
    )
    .to_js_function(c.realm());
    let failure = NativeFunction::from_copy_closure_with_captures(
        move |_, args, capture, c| {
            let reason = args.get_or_undefined(0).clone();
            if matches!(kind, FinishKind::Cancel) {
                wc(&capture.0).error(reason.clone(), c)?;
                unblock(&capture.0, c)?
            } else {
                rc(&capture.0).error(reason.clone(), c)?
            }
            capture.1.reject(reason, c)?;
            Ok(JsValue::undefined())
        },
        (s.clone(), pending.clone()),
    )
    .to_js_function(c.realm());
    drop(operation.then_intrinsic(Some(success), Some(failure), c)?);
    Ok(pending.promise.clone())
}
fn make_state(algorithms: TransformAlgorithms, c: &mut Context) -> JsResult<State> {
    for hwm in [
        algorithms.readable_high_water_mark,
        algorithms.writable_high_water_mark,
    ] {
        if hwm.is_nan() || hwm < 0.0 {
            return Err(range("Invalid transform highWaterMark"));
        }
    }
    for f in [
        &algorithms.start,
        &algorithms.transform,
        &algorithms.flush,
        &algorithms.cancel,
        &algorithms.readable_size,
        &algorithms.writable_size,
    ]
    .into_iter()
    .flatten()
    {
        if !f.is_callable() {
            return Err(typ("Transform callback must be callable"));
        }
    }
    let start = Pending::new(c);
    let s = Gc::new(GcRefCell::new(Data {
        readable: None,
        readable_controller: None,
        writable: None,
        writable_controller: None,
        controller: None,
        backpressure: true,
        change: Pending::new(c),
        finish: None,
        transformer: algorithms.this.clone(),
        transform: algorithms.transform.clone(),
        flush: algorithms.flush.clone(),
        cancel: algorithms.cancel.clone(),
    }));
    let start_callback = NativeFunction::from_copy_closure_with_captures(
        |_, _, start, _| Ok(start.promise.clone().into()),
        start.clone(),
    )
    .to_js_function(c.realm());
    let write_callback = NativeFunction::from_copy_closure_with_captures(
        |_, args, s, c| sink_write(s, args.get_or_undefined(0).clone(), c).map(JsValue::from),
        s.clone(),
    )
    .to_js_function(c.realm());
    let close_callback = NativeFunction::from_copy_closure_with_captures(
        |_, _, s, c| finish(s, FinishKind::Close, JsValue::undefined(), c).map(JsValue::from),
        s.clone(),
    )
    .to_js_function(c.realm());
    let abort_callback = NativeFunction::from_copy_closure_with_captures(
        |_, args, s, c| {
            finish(s, FinishKind::Abort, args.get_or_undefined(0).clone(), c).map(JsValue::from)
        },
        s.clone(),
    )
    .to_js_function(c.realm());
    let writable_algorithms = WritableAlgorithms {
        this: JsValue::undefined(),
        start: Some(start_callback.clone().into()),
        write: Some(write_callback.into()),
        close: Some(close_callback.into()),
        abort: Some(abort_callback.into()),
        size: algorithms.writable_size.clone(),
        high_water_mark: algorithms.writable_high_water_mark,
    };
    let (writable, writable_controller) = web_writable::new_writable(writable_algorithms, c)?;
    {
        let mut d = s.borrow_mut();
        d.writable = Some(writable);
        d.writable_controller = Some(writable_controller);
    }
    let pull_callback = NativeFunction::from_copy_closure_with_captures(
        |_, _, s, c| source_pull(s, c).map(JsValue::from),
        s.clone(),
    )
    .to_js_function(c.realm());
    let cancel_callback = NativeFunction::from_copy_closure_with_captures(
        |_, args, s, c| {
            finish(s, FinishKind::Cancel, args.get_or_undefined(0).clone(), c).map(JsValue::from)
        },
        s.clone(),
    )
    .to_js_function(c.realm());
    let readable_algorithms = ReadableAlgorithms {
        this: JsValue::undefined(),
        start: Some(start_callback.into()),
        pull: Some(pull_callback.into()),
        cancel: Some(cancel_callback.into()),
        size: algorithms.readable_size.clone(),
        high_water_mark: algorithms.readable_high_water_mark,
    };
    let (readable, readable_controller) =
        web_streams::new_readable_with_algorithms(readable_algorithms, c)?;
    {
        let mut d = s.borrow_mut();
        d.readable = Some(readable);
        d.readable_controller = Some(readable_controller);
    }
    let controller = TransformStreamDefaultController::from_data(
        TransformStreamDefaultController { state: s.clone() },
        c,
    )?;
    s.borrow_mut().controller = Some(controller.clone());
    let result = match &algorithms.start {
        Some(f) => call(f, &algorithms.this, &[controller.into()], c)?,
        None => JsValue::undefined(),
    };
    start.resolve(result, c)?;
    Ok(s)
}
/// Native construction with resolved algorithms; neither public constructors nor
/// callback properties are looked up. Returns stream and traced controller.
pub fn new_transform(
    algorithms: TransformAlgorithms,
    c: &mut Context,
) -> JsResult<(JsObject, TransformController)> {
    let state = make_state(algorithms, c)?;
    let stream = TransformStream::from_data(
        TransformStream {
            state: state.clone(),
        },
        c,
    )?;
    Ok((stream, TransformController { state }))
}
impl TransformController {
    pub fn desired_size(&self) -> Option<f64> {
        rc(&self.state).desired_size()
    }
    pub fn enqueue(&self, chunk: JsValue, c: &mut Context) -> JsResult<()> {
        enqueue(&self.state, chunk, c)
    }
    pub fn error(&self, reason: JsValue, c: &mut Context) -> JsResult<()> {
        error(&self.state, reason, c)
    }
    pub fn terminate(&self, c: &mut Context) -> JsResult<()> {
        terminate(&self.state, c)
    }
}
pub fn is_transform(value: &JsValue) -> bool {
    state(value).is_ok()
}
pub fn readable(value: &JsValue) -> JsResult<JsObject> {
    Ok(state(value)?.borrow().readable.clone().unwrap())
}
pub fn writable(value: &JsValue) -> JsResult<JsObject> {
    Ok(state(value)?.borrow().writable.clone().unwrap())
}
fn dict(v: &JsValue) -> JsResult<Option<JsObject>> {
    if v.is_null_or_undefined() {
        Ok(None)
    } else {
        v.as_object()
            .map(Some)
            .ok_or_else(|| typ("Transform dictionary must be an object"))
    }
}
fn get(d: &Option<JsObject>, key: &str, c: &mut Context) -> JsResult<JsValue> {
    match d {
        Some(o) => o.get(JsString::from(key), c),
        None => Ok(JsValue::undefined()),
    }
}
fn callback(v: JsValue) -> JsResult<Option<JsObject>> {
    if v.is_undefined() {
        Ok(None)
    } else {
        v.as_callable()
            .map(Some)
            .ok_or_else(|| typ("Transform callback must be callable"))
    }
}
fn strategy(v: &JsValue, default: f64, c: &mut Context) -> JsResult<(f64, Option<JsObject>)> {
    let d = dict(v)?;
    let hwm = get(&d, "highWaterMark", c)?;
    let hwm = if hwm.is_undefined() {
        default
    } else {
        hwm.to_number(c)?
    };
    let size = callback(get(&d, "size", c)?)?;
    Ok((hwm, size))
}
fn constructor(args: &[JsValue], c: &mut Context) -> JsResult<State> {
    let (writable_high_water_mark, writable_size) = strategy(args.get_or_undefined(1), 1.0, c)?;
    let (readable_high_water_mark, readable_size) = strategy(args.get_or_undefined(2), 0.0, c)?;
    let d = dict(args.get_or_undefined(0))?;
    let cancel = callback(get(&d, "cancel", c)?)?;
    let flush = callback(get(&d, "flush", c)?)?;
    let readable_type = get(&d, "readableType", c)?;
    let start = callback(get(&d, "start", c)?)?;
    let transform = callback(get(&d, "transform", c)?)?;
    let writable_type = get(&d, "writableType", c)?;
    if !readable_type.is_undefined() {
        return Err(range("Transform readableType must be undefined"));
    }
    if !writable_type.is_undefined() {
        return Err(range("Transform writableType must be undefined"));
    }
    make_state(
        TransformAlgorithms {
            this: d.map(JsValue::from).unwrap_or_else(JsValue::null),
            start,
            transform,
            flush,
            cancel,
            writable_high_water_mark,
            writable_size,
            readable_high_water_mark,
            readable_size,
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
impl Class for TransformStream {
    const NAME: &'static str = "TransformStream";
    const ATTRIBUTES: Attribute = Attribute::WRITABLE.union(Attribute::CONFIGURABLE);
    fn data_constructor(_: &JsValue, args: &[JsValue], c: &mut Context) -> JsResult<Self> {
        Ok(Self {
            state: constructor(args, c)?,
        })
    }
    fn init(class: &mut ClassBuilder) -> JsResult<()> {
        accessor(
            class,
            "readable",
            NativeFunction::from_fn_ptr(|this, _, _| readable(this).map(JsValue::from)),
        );
        accessor(
            class,
            "writable",
            NativeFunction::from_fn_ptr(|this, _, _| writable(this).map(JsValue::from)),
        );
        tag(class, Self::NAME);
        Ok(())
    }
}
impl Class for TransformStreamDefaultController {
    const NAME: &'static str = "TransformStreamDefaultController";
    const ATTRIBUTES: Attribute = Attribute::WRITABLE.union(Attribute::CONFIGURABLE);
    fn data_constructor(_: &JsValue, _: &[JsValue], _: &mut Context) -> JsResult<Self> {
        Err(typ("Illegal TransformStreamDefaultController constructor"))
    }
    fn init(class: &mut ClassBuilder) -> JsResult<()> {
        accessor(
            class,
            "desiredSize",
            NativeFunction::from_fn_ptr(|this, _, _| {
                Ok(rc(&controller(this)?)
                    .desired_size()
                    .map(JsValue::from)
                    .unwrap_or_else(JsValue::null))
            }),
        );
        class.method(
            JsString::from("enqueue"),
            0,
            NativeFunction::from_fn_ptr(|this, args, c| {
                enqueue(&controller(this)?, args.get_or_undefined(0).clone(), c)?;
                Ok(JsValue::undefined())
            }),
        );
        class.method(
            JsString::from("error"),
            0,
            NativeFunction::from_fn_ptr(|this, args, c| {
                error(&controller(this)?, args.get_or_undefined(0).clone(), c)?;
                Ok(JsValue::undefined())
            }),
        );
        class.method(
            JsString::from("terminate"),
            0,
            NativeFunction::from_fn_ptr(|this, _, c| {
                terminate(&controller(this)?, c)?;
                Ok(JsValue::undefined())
            }),
        );
        tag(class, Self::NAME);
        Ok(())
    }
}
/// Register after native readable and writable capabilities. No I/O authority.
pub fn register(c: &mut Context) -> JsResult<()> {
    if c.has_data::<Registration>() {
        return Ok(());
    }
    c.insert_data(Registration {
        depth: Rc::new(Cell::new(0)),
    });
    c.register_global_class::<TransformStream>()?;
    c.register_global_class::<TransformStreamDefaultController>()?;
    let prototype = c
        .get_global_class::<TransformStreamDefaultController>()
        .unwrap()
        .prototype();
    for name in ["enqueue", "error", "terminate"] {
        prototype.define_property_or_throw(
            JsString::from(name),
            PropertyDescriptor::builder().enumerable(true),
            c,
        )?;
    }
    Ok(())
}
#[cfg(test)]
#[path = "web_transform_tests.rs"]
mod tests;
