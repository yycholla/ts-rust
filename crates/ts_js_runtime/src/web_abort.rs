//! Native standalone DOM EventTarget and abort primitives.
//!
//! Algorithms follow https://dom.spec.whatwg.org/#events and #aborting-ongoing-activities
//! (WHATWG DOM, inspected 2026-10-10). There is no DOM tree, window, timer, or I/O.
//! Register after `web_text::register` (native DOMException). Hosts MUST drain
//! `take_reported_errors` after JS work: listener exceptions are reported there,
//! not thrown by dispatchEvent. `AbortSignal.timeout` explicitly throws until a
//! real timer capability is available. Clock timestamps use the context clock,
//! relative to registration. All callback/reason/dependency references are traced.
//!
//! Resource policy: 10,000 listeners/dependencies per object, 100,000 listener
//! visits per outer dispatch, nesting 64, 1 Mi UTF-16 event type units. Exceeding
//! policy is an explicit RangeError. A collected native state cycle is reclaimable;
//! live sources retain their dependent signals until abortion.
use boa_engine::{
    Context, JsArgs, JsData, JsError, JsNativeError, JsObject, JsResult, JsString, JsSymbol,
    JsValue, NativeFunction,
    class::{Class, ClassBuilder},
    object::{FunctionObjectBuilder, builtins::JsArray},
    property::{Attribute, PropertyDescriptor},
};
use boa_gc::{Finalize, Gc, GcRefCell, Trace};
use std::collections::VecDeque;

const MAX_ITEMS: usize = 10_000;
const MAX_WORK: usize = 100_000;
const MAX_DEPTH: usize = 64;
const MAX_TYPE: usize = 1024 * 1024;
type TargetState = Gc<GcRefCell<TargetData>>;
type SignalState = Gc<GcRefCell<SignalData>>;
#[derive(Clone, Trace, Finalize)]
struct Listener {
    id: u64,
    kind: JsString,
    callback: JsObject,
    capture: bool,
    once: bool,
    passive: bool,
    handler: bool,
    signal: Option<SignalState>,
}
#[derive(Default, Trace, Finalize)]
struct TargetData {
    listeners: Vec<Listener>,
    next_id: u64,
}
#[derive(Trace, Finalize)]
struct Removal {
    target: TargetState,
    id: u64,
}
#[derive(Trace, Finalize)]
enum AbortAlgorithm {
    Removal(Removal),
    Callback { token: u64, callback: JsObject },
}
#[derive(Trace, Finalize)]
struct SignalData {
    target: TargetState,
    reason: JsValue,
    sources: Vec<JsObject>,
    dependents: Vec<JsObject>,
    algorithms: VecDeque<AbortAlgorithm>,
    next_algorithm_token: u64,
    dependent: bool,
}
#[derive(Trace, Finalize, JsData)]
struct EventTarget {
    state: TargetState,
}
#[derive(Trace, Finalize, JsData)]
struct AbortSignal {
    state: SignalState,
}
#[derive(Trace, Finalize, JsData)]
struct AbortController {
    signal: JsObject,
}
#[derive(Trace, Finalize, JsData)]
struct Event {
    kind: JsString,
    target: JsValue,
    current: JsValue,
    bubbles: bool,
    cancelable: bool,
    composed: bool,
    cancelled: bool,
    stopped: bool,
    immediate: bool,
    passive: bool,
    dispatching: bool,
    trusted: bool,
    timestamp: f64,
}
#[derive(Default, Trace, Finalize)]
struct HostState {
    errors: Vec<JsValue>,
    depth: usize,
    work: usize,
}
struct Registration {
    host: Gc<GcRefCell<HostState>>,
    origin: u64,
}
fn typ(message: &str) -> JsError {
    JsNativeError::typ().with_message(message.to_owned()).into()
}
fn limit() -> JsError {
    JsNativeError::range()
        .with_message("Native event/abort resource limit exceeded")
        .into()
}
fn required(args: &[JsValue], count: usize) -> JsResult<()> {
    if args.len() < count {
        Err(typ("Insufficient event/abort arguments"))
    } else {
        Ok(())
    }
}
fn string(value: &JsValue, context: &mut Context) -> JsResult<JsString> {
    let value = value.to_string(context)?;
    if value.len() > MAX_TYPE {
        Err(limit())
    } else {
        Ok(value)
    }
}
fn object(this: &JsValue) -> JsResult<JsObject> {
    this.as_object()
        .ok_or_else(|| typ("Invalid native event receiver"))
}
fn target(this: &JsValue) -> JsResult<TargetState> {
    let object = object(this)?;
    if let Some(data) = object.downcast_ref::<EventTarget>() {
        return Ok(data.state.clone());
    }
    if let Some(data) = object.downcast_ref::<AbortSignal>() {
        return Ok(data.state.borrow().target.clone());
    }
    Err(typ("EventTarget receiver required"))
}
fn signal(this: &JsValue) -> JsResult<SignalState> {
    object(this)?
        .downcast_ref::<AbortSignal>()
        .map(|data| data.state.clone())
        .ok_or_else(|| typ("AbortSignal receiver required"))
}
fn event(this: &JsValue) -> JsResult<JsObject> {
    let object = object(this)?;
    if !object.is::<Event>() {
        return Err(typ("Event receiver required"));
    }
    Ok(object)
}
fn dict(value: &JsValue) -> JsResult<Option<JsObject>> {
    if value.is_null_or_undefined() {
        Ok(None)
    } else {
        Ok(Some(object(value)?))
    }
}
fn member(dict: &Option<JsObject>, name: &str, context: &mut Context) -> JsResult<JsValue> {
    match dict {
        Some(object) => object.get(JsString::from(name), context),
        None => Ok(JsValue::undefined()),
    }
}
fn exception(name: &str, message: &str, context: &mut Context) -> JsResult<JsError> {
    Ok(JsError::from_opaque(
        crate::web_text::dom_exception(message, name, context)?.into(),
    ))
}
fn host(context: &Context) -> JsResult<Gc<GcRefCell<HostState>>> {
    context
        .get_data::<Registration>()
        .map(|data| data.host.clone())
        .ok_or_else(|| typ("Event capability is not registered"))
}
/// Drain DOM listener exceptions without changing dispatchEvent's return behavior.
pub fn take_reported_errors(context: &mut Context) -> Vec<JsValue> {
    context
        .get_data::<Registration>()
        .map(|data| std::mem::take(&mut data.host.borrow_mut().errors))
        .unwrap_or_default()
}
fn timestamp(context: &Context) -> f64 {
    let origin = context
        .get_data::<Registration>()
        .map_or(0, |data| data.origin);
    context
        .clock()
        .now()
        .millis_since_epoch()
        .saturating_sub(origin) as f64
}
impl Event {
    fn new(kind: JsString, context: &Context) -> Self {
        Self {
            kind,
            target: JsValue::null(),
            current: JsValue::null(),
            bubbles: false,
            cancelable: false,
            composed: false,
            cancelled: false,
            stopped: false,
            immediate: false,
            passive: false,
            dispatching: false,
            trusted: false,
            timestamp: timestamp(context),
        }
    }
}
impl Class for Event {
    const NAME: &'static str = "Event";
    const LENGTH: usize = 1;
    const ATTRIBUTES: Attribute = Attribute::WRITABLE.union(Attribute::CONFIGURABLE);
    fn data_constructor(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<Self> {
        required(args, 1)?;
        let kind = string(&args[0], context)?;
        let options = dict(args.get_or_undefined(1))?;
        let mut data = Self::new(kind, context);
        data.bubbles = member(&options, "bubbles", context)?.to_boolean();
        data.cancelable = member(&options, "cancelable", context)?.to_boolean();
        data.composed = member(&options, "composed", context)?.to_boolean();
        Ok(data)
    }
    fn object_constructor(
        instance: &JsObject<Self>,
        _: &[JsValue],
        context: &mut Context,
    ) -> JsResult<()> {
        let getter = FunctionObjectBuilder::new(
            context.realm(),
            NativeFunction::from_fn_ptr(|this, _, _| {
                let object = event(this)?;
                let value = object.downcast_ref::<Event>().unwrap().trusted;
                Ok(value.into())
            }),
        )
        .build();
        instance.clone().upcast().define_property_or_throw(
            JsString::from("isTrusted"),
            PropertyDescriptor::builder()
                .get(getter)
                .enumerable(true)
                .configurable(false),
            context,
        )?;
        Ok(())
    }
    fn init(class: &mut ClassBuilder) -> JsResult<()> {
        for name in [
            "type",
            "target",
            "srcElement",
            "currentTarget",
            "eventPhase",
            "bubbles",
            "cancelable",
            "composed",
            "defaultPrevented",
            "timeStamp",
            "cancelBubble",
            "returnValue",
        ] {
            let getter = FunctionObjectBuilder::new(
                class.context().realm(),
                NativeFunction::from_copy_closure(move |this, _, _| {
                    let object = event(this)?;
                    let data = object.downcast_ref::<Event>().unwrap();
                    Ok(match name {
                        "type" => data.kind.clone().into(),
                        "target" | "srcElement" => data.target.clone(),
                        "currentTarget" => data.current.clone(),
                        "eventPhase" => (if data.dispatching { 2 } else { 0 }).into(),
                        "bubbles" => data.bubbles.into(),
                        "cancelable" => data.cancelable.into(),
                        "composed" => data.composed.into(),
                        "defaultPrevented" => data.cancelled.into(),
                        "timeStamp" => data.timestamp.into(),
                        "cancelBubble" => data.stopped.into(),
                        _ => (!data.cancelled).into(),
                    })
                }),
            )
            .build();
            let setter = if matches!(name, "cancelBubble" | "returnValue") {
                Some(
                    FunctionObjectBuilder::new(
                        class.context().realm(),
                        NativeFunction::from_copy_closure(move |this, args, _| {
                            let object = event(this)?;
                            let mut data = object.downcast_mut::<Event>().unwrap();
                            let value = args.get_or_undefined(0).to_boolean();
                            if name == "cancelBubble" {
                                if value {
                                    data.stopped = true;
                                }
                            } else if !value && data.cancelable && !data.passive {
                                data.cancelled = true;
                            }
                            Ok(JsValue::undefined())
                        }),
                    )
                    .build(),
                )
            } else {
                None
            };
            class.accessor(
                JsString::from(name),
                Some(getter),
                setter,
                Attribute::ENUMERABLE.union(Attribute::CONFIGURABLE),
            );
        }
        for name in [
            "stopPropagation",
            "stopImmediatePropagation",
            "preventDefault",
        ] {
            class.method(
                JsString::from(name),
                0,
                NativeFunction::from_copy_closure(move |this, _, _| {
                    let object = event(this)?;
                    let mut data = object.downcast_mut::<Event>().unwrap();
                    match name {
                        "stopPropagation" => data.stopped = true,
                        "stopImmediatePropagation" => {
                            data.stopped = true;
                            data.immediate = true;
                        }
                        _ => {
                            if data.cancelable && !data.passive {
                                data.cancelled = true;
                            }
                        }
                    }
                    Ok(JsValue::undefined())
                }),
            );
        }
        class.method(
            JsString::from("composedPath"),
            0,
            NativeFunction::from_fn_ptr(|this, _, context| {
                let object = event(this)?;
                let data = object.downcast_ref::<Event>().unwrap();
                let values = if data.dispatching {
                    vec![data.target.clone()]
                } else {
                    vec![]
                };
                drop(data);
                Ok(JsArray::from_iter(values, context).into())
            }),
        );
        class.method(
            JsString::from("initEvent"),
            1,
            NativeFunction::from_fn_ptr(|this, args, context| {
                let object = event(this)?;
                required(args, 1)?;
                let kind = string(&args[0], context)?;
                let mut data = object.downcast_mut::<Event>().unwrap();
                if !data.dispatching {
                    data.kind = kind;
                    data.bubbles = args.get_or_undefined(1).to_boolean();
                    data.cancelable = args.get_or_undefined(2).to_boolean();
                    data.cancelled = false;
                    data.stopped = false;
                    data.immediate = false;
                    data.target = JsValue::null();
                    data.trusted = false;
                }
                Ok(JsValue::undefined())
            }),
        );
        for (name, value) in [
            ("NONE", 0),
            ("CAPTURING_PHASE", 1),
            ("AT_TARGET", 2),
            ("BUBBLING_PHASE", 3),
        ] {
            class.property(JsString::from(name), value, Attribute::ENUMERABLE);
            class.static_property(JsString::from(name), value, Attribute::ENUMERABLE);
        }
        class.property(
            JsSymbol::to_string_tag(),
            JsString::from("Event"),
            Attribute::CONFIGURABLE,
        );
        Ok(())
    }
}
fn remove(target: &TargetState, id: u64) {
    let removed = {
        let mut data = target.borrow_mut();
        data.listeners
            .iter()
            .position(|item| item.id == id)
            .map(|index| data.listeners.remove(index))
    };
    if let Some(Listener {
        signal: Some(ref signal),
        ..
    }) = removed
    {
        signal.borrow_mut().algorithms.retain(|item| {
            !matches!(item, AbortAlgorithm::Removal(item)
                if Gc::ptr_eq(&item.target, target) && item.id == id)
        });
    }
}
fn add_listener(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let target = target(this)?;
    required(args, 2)?;
    let kind = string(&args[0], context)?;
    let callback = if args[1].is_null_or_undefined() {
        None
    } else {
        Some(object(&args[1])?)
    };
    let options = args.get_or_undefined(2);
    let (capture, once, passive, abort) = if options.is_object() {
        let options = dict(options)?;
        // Web IDL dictionary member conversion is lexicographic.
        let capture = member(&options, "capture", context)?.to_boolean();
        let once = member(&options, "once", context)?.to_boolean();
        let passive = member(&options, "passive", context)?.to_boolean();
        let abort = member(&options, "signal", context)?;
        let abort = if abort.is_undefined() {
            None
        } else {
            Some(signal(&abort)?)
        };
        (capture, once, passive, abort)
    } else {
        (options.to_boolean(), false, false, None)
    };
    if abort
        .as_ref()
        .is_some_and(|signal| !signal.borrow().reason.is_undefined())
    {
        return Ok(JsValue::undefined());
    }
    let Some(callback) = callback else {
        return Ok(JsValue::undefined());
    };
    let mut data = target.borrow_mut();
    if data.listeners.iter().any(|item| {
        item.kind == kind
            && item.capture == capture
            && !item.handler
            && JsObject::equals(&item.callback, &callback)
    }) {
        return Ok(JsValue::undefined());
    }
    if data.listeners.len() >= MAX_ITEMS
        || data.next_id == u64::MAX
        || abort
            .as_ref()
            .is_some_and(|s| s.borrow().algorithms.len() >= MAX_ITEMS)
    {
        return Err(limit());
    }
    let id = data.next_id;
    data.next_id += 1;
    data.listeners.push(Listener {
        id,
        kind,
        callback,
        capture,
        once,
        passive,
        handler: false,
        signal: abort.clone(),
    });
    drop(data);
    if let Some(signal) = abort {
        signal
            .borrow_mut()
            .algorithms
            .push_back(AbortAlgorithm::Removal(Removal { target, id }));
    }
    Ok(JsValue::undefined())
}
fn remove_listener(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let target = target(this)?;
    required(args, 2)?;
    let kind = string(&args[0], context)?;
    let callback = if args[1].is_null_or_undefined() {
        None
    } else {
        Some(object(&args[1])?)
    };
    let options = args.get_or_undefined(2);
    let capture = if options.is_object() {
        member(&dict(options)?, "capture", context)?.to_boolean()
    } else {
        options.to_boolean()
    };
    if let Some(callback) = callback {
        let id = target
            .borrow()
            .listeners
            .iter()
            .find(|item| {
                item.kind == kind
                    && item.capture == capture
                    && !item.handler
                    && JsObject::equals(&item.callback, &callback)
            })
            .map(|item| item.id);
        if let Some(id) = id {
            remove(&target, id);
        }
    }
    Ok(JsValue::undefined())
}
impl Class for EventTarget {
    const NAME: &'static str = "EventTarget";
    const ATTRIBUTES: Attribute = Attribute::WRITABLE.union(Attribute::CONFIGURABLE);
    fn data_constructor(_: &JsValue, _: &[JsValue], _: &mut Context) -> JsResult<Self> {
        Ok(Self {
            state: Gc::new(GcRefCell::new(TargetData::default())),
        })
    }
    fn init(class: &mut ClassBuilder) -> JsResult<()> {
        class.method(
            JsString::from("addEventListener"),
            2,
            NativeFunction::from_fn_ptr(add_listener),
        );
        class.method(
            JsString::from("removeEventListener"),
            2,
            NativeFunction::from_fn_ptr(remove_listener),
        );
        class.method(
            JsString::from("dispatchEvent"),
            1,
            NativeFunction::from_fn_ptr(|this, args, context| {
                target(this)?;
                required(args, 1)?;
                dispatch_event(&object(this)?, &event(&args[0])?, false, context).map(JsValue::from)
            }),
        );
        class.property(
            JsSymbol::to_string_tag(),
            JsString::from("EventTarget"),
            Attribute::CONFIGURABLE,
        );
        Ok(())
    }
}
/// Dispatch a native Event at a standalone target. Host-created events can be trusted.
pub fn dispatch_event(
    target_object: &JsObject,
    event_object: &JsObject,
    trusted: bool,
    context: &mut Context,
) -> JsResult<bool> {
    let state = target(&target_object.clone().into())?;
    event(&event_object.clone().into())?;
    if event_object.downcast_ref::<Event>().unwrap().dispatching {
        return Err(exception(
            "InvalidStateError",
            "Event is already being dispatched",
            context,
        )?);
    }
    let host = host(context)?;
    {
        let mut h = host.borrow_mut();
        if h.depth >= MAX_DEPTH {
            return Err(limit());
        }
        if h.depth == 0 {
            h.work = 0;
        }
        h.depth += 1;
    }
    {
        let mut e = event_object.downcast_mut::<Event>().unwrap();
        e.dispatching = true;
        e.trusted = trusted;
        e.target = target_object.clone().into();
    }
    let result = (|| {
        for capture in [true, false] {
            if event_object.downcast_ref::<Event>().unwrap().stopped {
                break;
            }
            event_object.downcast_mut::<Event>().unwrap().current = target_object.clone().into();
            let ids: Vec<_> = state
                .borrow()
                .listeners
                .iter()
                .map(|listener| listener.id)
                .collect();
            for id in ids {
                {
                    let mut h = host.borrow_mut();
                    h.work += 1;
                    if h.work > MAX_WORK {
                        return Err(limit());
                    }
                }
                let listener = state
                    .borrow()
                    .listeners
                    .iter()
                    .find(|item| item.id == id)
                    .cloned();
                let Some(listener) = listener else { continue };
                if listener.capture != capture
                    || listener.kind != event_object.downcast_ref::<Event>().unwrap().kind
                {
                    continue;
                }
                if listener.once {
                    remove(&state, id);
                }
                event_object.downcast_mut::<Event>().unwrap().passive = listener.passive;
                let callback_result = (|| {
                    let (callback, this) = if listener.callback.is_callable() {
                        (listener.callback.clone(), target_object.clone().into())
                    } else {
                        if listener.handler {
                            return Err(typ("Event handler is not callable"));
                        }
                        let method = listener
                            .callback
                            .get(JsString::from("handleEvent"), context)?
                            .as_callable()
                            .ok_or_else(|| typ("Listener handleEvent is not callable"))?;
                        (method, listener.callback.clone().into())
                    };
                    callback.call(&this, &[event_object.clone().into()], context)
                })();
                event_object.downcast_mut::<Event>().unwrap().passive = false;
                match callback_result {
                    Ok(value) if listener.handler && value.as_boolean() == Some(false) => {
                        let mut e = event_object.downcast_mut::<Event>().unwrap();
                        if e.cancelable {
                            e.cancelled = true;
                        }
                    }
                    Err(error) => {
                        let error = error.into_opaque(context)?;
                        let mut h = host.borrow_mut();
                        if h.errors.len() >= MAX_ITEMS {
                            return Err(limit());
                        }
                        h.errors.push(error);
                    }
                    _ => {}
                }
                if event_object.downcast_ref::<Event>().unwrap().immediate {
                    break;
                }
            }
        }
        Ok(!event_object.downcast_ref::<Event>().unwrap().cancelled)
    })();
    {
        let mut e = event_object.downcast_mut::<Event>().unwrap();
        e.current = JsValue::null();
        e.dispatching = false;
        e.stopped = false;
        e.immediate = false;
        e.passive = false;
    }
    host.borrow_mut().depth -= 1;
    result
}
/// Create an un-aborted signal for a host operation; no public constructor exists.
pub fn new_signal(context: &mut Context) -> JsResult<JsObject> {
    AbortSignal::from_data(
        AbortSignal {
            state: Gc::new(GcRefCell::new(SignalData {
                target: Gc::new(GcRefCell::new(TargetData::default())),
                reason: JsValue::undefined(),
                sources: vec![],
                dependents: vec![],
                algorithms: VecDeque::new(),
                next_algorithm_token: 1,
                dependent: false,
            })),
        },
        context,
    )
}
/// Register a native abort step, before the signal's abort event. The callable
/// receives the exact abort reason with an undefined `this` argument. References
/// are traced, and exceptions use `take_reported_errors`, as event listeners do.
/// Token zero means the signal was already aborted and nothing was registered.
pub fn add_abort_algorithm(
    object: &JsObject,
    callback: JsObject,
    context: &mut Context,
) -> JsResult<u64> {
    let state = signal(&object.clone().into())?;
    if !callback.is_callable() {
        return Err(typ("Abort algorithm callback must be callable"));
    }
    host(context)?;
    let mut state = state.borrow_mut();
    if !state.reason.is_undefined() {
        return Ok(0);
    }
    if state.algorithms.len() >= MAX_ITEMS || state.next_algorithm_token == u64::MAX {
        return Err(limit());
    }
    let token = state.next_algorithm_token;
    state.next_algorithm_token += 1;
    state
        .algorithms
        .push_back(AbortAlgorithm::Callback { token, callback });
    Ok(token)
}
/// Remove a registered native step. Unknown tokens (including zero) are no-ops.
/// Removing a future step during abortion prevents its invocation.
pub fn remove_abort_algorithm(object: &JsObject, token: u64) -> JsResult<()> {
    signal(&object.clone().into())?
        .borrow_mut()
        .algorithms
        .retain(|item| !matches!(item, AbortAlgorithm::Callback { token: id, .. } if *id == token));
    Ok(())
}
/// `None` means not aborted; `Some(undefined)` cannot occur (default reason is native AbortError).
pub fn abort_reason(value: &JsValue) -> JsResult<Option<JsValue>> {
    let state = signal(value)?;
    let reason = state.borrow().reason.clone();
    Ok((!reason.is_undefined()).then_some(reason))
}
fn resolved_reason(reason: JsValue, context: &mut Context) -> JsResult<JsValue> {
    if reason.is_undefined() {
        Ok(
            crate::web_text::dom_exception("This operation was aborted", "AbortError", context)?
                .into(),
        )
    } else {
        Ok(reason)
    }
}
/// Abort once, preserving the exact JavaScript reason object. Marks all dependent
/// signals before firing any callback. Native callbacks and listener removals
/// run in their shared registration order, before each signal's abort event.
pub fn abort_signal(object: &JsObject, reason: JsValue, context: &mut Context) -> JsResult<()> {
    let state = signal(&object.clone().into())?;
    if !state.borrow().reason.is_undefined() {
        return Ok(());
    }
    let reason = resolved_reason(reason, context)?;
    let dependents = state.borrow().dependents.clone();
    let mut pending = vec![object.clone()];
    for dependent in dependents {
        if signal(&dependent.clone().into())?
            .borrow()
            .reason
            .is_undefined()
        {
            pending.push(dependent);
        }
    }
    for object in &pending {
        signal(&object.clone().into())?.borrow_mut().reason = reason.clone();
    }
    let report = host(context)?;
    let mut failure = None;
    for object in pending {
        let state = signal(&object.clone().into())?;
        let sources = {
            let mut s = state.borrow_mut();
            s.dependents.clear();
            std::mem::take(&mut s.sources)
        };
        for source in sources {
            signal(&source.into())?
                .borrow_mut()
                .dependents
                .retain(|item| !JsObject::equals(item, &object));
        }
        loop {
            // Pop only the current step. Taking the entire list here would make
            // removing a later algorithm during a callback ineffective. Never
            // retain a native-state borrow while calling user code.
            let algorithm = state.borrow_mut().algorithms.pop_front();
            match &algorithm {
                None => break,
                Some(AbortAlgorithm::Removal(removal)) => remove(&removal.target, removal.id),
                Some(AbortAlgorithm::Callback { callback, .. }) => {
                    if let Err(error) = callback.call(
                        &JsValue::undefined(),
                        std::slice::from_ref(&reason),
                        context,
                    ) {
                        match error.into_opaque(context) {
                            Ok(error) => {
                                let mut report = report.borrow_mut();
                                if report.errors.len() < MAX_ITEMS {
                                    report.errors.push(error);
                                } else if failure.is_none() {
                                    failure = Some(limit());
                                }
                            }
                            Err(error) => {
                                if failure.is_none() {
                                    failure = Some(error);
                                }
                            }
                        }
                    }
                }
            }
        }
        let result = (|| {
            let mut event = Event::new(JsString::from("abort"), context);
            event.trusted = true;
            let event = Event::from_data(event, context)?;
            dispatch_event(&object, &event, true, context)
        })();
        if let Err(error) = result
            && failure.is_none()
        {
            failure = Some(error);
        }
    }
    failure.map_or(Ok(()), Err)
}
fn any(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    required(args, 1)?;
    let iterable = object(&args[0])?;
    let method = iterable
        .get(JsSymbol::iterator(), context)?
        .as_callable()
        .ok_or_else(|| typ("AbortSignal.any requires an iterable"))?;
    let iterator = method
        .call(&args[0], &[], context)?
        .as_object()
        .ok_or_else(|| typ("Iterator must be an object"))?;
    let next = iterator
        .get(JsString::from("next"), context)?
        .as_callable()
        .ok_or_else(|| typ("Iterator next must be callable"))?;
    let mut inputs = vec![];
    loop {
        let step = next
            .call(&iterator.clone().into(), &[], context)?
            .as_object()
            .ok_or_else(|| typ("Iterator result must be an object"))?;
        if step.get(JsString::from("done"), context)?.to_boolean() {
            break;
        }
        if inputs.len() >= MAX_ITEMS {
            if let Ok(value) = iterator.get(JsString::from("return"), context)
                && let Some(method) = value.as_callable()
            {
                let _ = method.call(&iterator.clone().into(), &[], context);
            }
            return Err(limit());
        }
        let value = step.get(JsString::from("value"), context)?;
        signal(&value)?;
        inputs.push(object(&value)?);
    }
    dependent_signal(&inputs, context).map(JsValue::from)
}
/// Validate an AbortSignal by native brand, ignoring mutable JS properties.
/// Used during RequestInit's Web IDL conversion without creating a dependency.
pub fn validate_signal(value: &JsValue) -> JsResult<JsObject> {
    signal(value)?;
    object(value)
}
/// Construct a distinct dependent signal from already converted native inputs.
/// No global constructors, iterators, `aborted`/`reason` getters or public event
/// methods are invoked. Duplicates and nested dependents flatten to source
/// identity; all reasons are marked before any abort callback runs.
pub fn dependent_signal(inputs: &[JsObject], context: &mut Context) -> JsResult<JsObject> {
    if inputs.len() > MAX_ITEMS {
        return Err(limit());
    }
    // Validate every input before inspecting abort state, matching completed
    // sequence conversion even when an earlier source is already aborted.
    for input in inputs {
        signal(&input.clone().into())?;
    }
    let result = new_signal(context)?;
    let result_state = signal(&result.clone().into())?;
    for input in inputs {
        let state = signal(&input.clone().into())?;
        if !state.borrow().reason.is_undefined() {
            result_state.borrow_mut().reason = state.borrow().reason.clone();
            return Ok(result);
        }
    }
    let mut sources = vec![];
    for input in inputs {
        let state = signal(&input.clone().into())?;
        let items = if state.borrow().dependent {
            state.borrow().sources.clone()
        } else {
            vec![input.clone()]
        };
        for item in items {
            if !sources
                .iter()
                .any(|existing| JsObject::equals(existing, &item))
            {
                if sources.len() >= MAX_ITEMS
                    || signal(&item.clone().into())?.borrow().dependents.len() >= MAX_ITEMS
                {
                    return Err(limit());
                }
                sources.push(item);
            }
        }
    }
    {
        let mut data = result_state.borrow_mut();
        data.dependent = true;
        data.sources = sources.clone();
    }
    for source in sources {
        signal(&source.into())?
            .borrow_mut()
            .dependents
            .push(result.clone());
    }
    Ok(result)
}
impl Class for AbortSignal {
    const NAME: &'static str = "AbortSignal";
    const ATTRIBUTES: Attribute = Attribute::WRITABLE.union(Attribute::CONFIGURABLE);
    fn data_constructor(_: &JsValue, _: &[JsValue], _: &mut Context) -> JsResult<Self> {
        Err(typ("Illegal AbortSignal constructor"))
    }
    fn init(class: &mut ClassBuilder) -> JsResult<()> {
        for name in ["aborted", "reason", "onabort"] {
            let getter = FunctionObjectBuilder::new(
                class.context().realm(),
                NativeFunction::from_copy_closure(move |this, _, _| {
                    let state = signal(this)?;
                    let data = state.borrow();
                    Ok(match name {
                        "aborted" => (!data.reason.is_undefined()).into(),
                        "reason" => data.reason.clone(),
                        _ => data
                            .target
                            .borrow()
                            .listeners
                            .iter()
                            .find(|item| item.handler)
                            .map(|item| item.callback.clone().into())
                            .unwrap_or_else(JsValue::null),
                    })
                }),
            )
            .build();
            let setter = if name == "onabort" {
                Some(
                    FunctionObjectBuilder::new(
                        class.context().realm(),
                        NativeFunction::from_fn_ptr(|this, args, _| {
                            let state = signal(this)?.borrow().target.clone();
                            let callback = args.get_or_undefined(0).as_object();
                            let existing = state
                                .borrow()
                                .listeners
                                .iter()
                                .position(|item| item.handler);
                            let mut data = state.borrow_mut();
                            match (existing, callback) {
                                (Some(index), Some(callback)) => {
                                    data.listeners[index].callback = callback
                                }
                                (Some(index), None) => {
                                    data.listeners.remove(index);
                                }
                                (None, Some(callback)) => {
                                    if data.listeners.len() >= MAX_ITEMS || data.next_id == u64::MAX
                                    {
                                        return Err(limit());
                                    }
                                    let id = data.next_id;
                                    data.next_id += 1;
                                    data.listeners.push(Listener {
                                        id,
                                        kind: JsString::from("abort"),
                                        callback,
                                        capture: false,
                                        once: false,
                                        passive: false,
                                        handler: true,
                                        signal: None,
                                    });
                                }
                                _ => {}
                            }
                            Ok(JsValue::undefined())
                        }),
                    )
                    .build(),
                )
            } else {
                None
            };
            class.accessor(
                JsString::from(name),
                Some(getter),
                setter,
                Attribute::ENUMERABLE.union(Attribute::CONFIGURABLE),
            );
        }
        class.method(
            JsString::from("throwIfAborted"),
            0,
            NativeFunction::from_fn_ptr(|this, _, _| {
                if let Some(reason) = abort_reason(this)? {
                    Err(JsError::from_opaque(reason))
                } else {
                    Ok(JsValue::undefined())
                }
            }),
        );
        class.static_method(
            JsString::from("abort"),
            0,
            NativeFunction::from_fn_ptr(|_, args, context| {
                let reason = resolved_reason(args.get_or_undefined(0).clone(), context)?;
                let result = new_signal(context)?;
                signal(&result.clone().into())?.borrow_mut().reason = reason;
                Ok(result.into())
            }),
        );
        class.static_method(JsString::from("any"), 1, NativeFunction::from_fn_ptr(any));
        class.static_method(
            JsString::from("timeout"),
            1,
            NativeFunction::from_fn_ptr(|_, args, context| {
                required(args, 1)?;
                let value = args[0].to_number(context)?;
                if !value.is_finite()
                    || value.trunc() < 0.0
                    || value.trunc() >= 18446744073709551616.0
                {
                    return Err(typ("Timeout must be an unsigned 64-bit integer"));
                }
                Err(exception(
                    "NotSupportedError",
                    "AbortSignal.timeout requires a timer host",
                    context,
                )?)
            }),
        );
        class.property(
            JsSymbol::to_string_tag(),
            JsString::from("AbortSignal"),
            Attribute::CONFIGURABLE,
        );
        Ok(())
    }
}
impl Class for AbortController {
    const NAME: &'static str = "AbortController";
    const ATTRIBUTES: Attribute = Attribute::WRITABLE.union(Attribute::CONFIGURABLE);
    fn data_constructor(_: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<Self> {
        Ok(Self {
            signal: new_signal(context)?,
        })
    }
    fn init(class: &mut ClassBuilder) -> JsResult<()> {
        let getter = FunctionObjectBuilder::new(
            class.context().realm(),
            NativeFunction::from_fn_ptr(|this, _, _| {
                Ok(object(this)?
                    .downcast_ref::<AbortController>()
                    .ok_or_else(|| typ("AbortController receiver required"))?
                    .signal
                    .clone()
                    .into())
            }),
        )
        .build();
        class.accessor(
            JsString::from("signal"),
            Some(getter),
            None,
            Attribute::ENUMERABLE.union(Attribute::CONFIGURABLE),
        );
        class.method(
            JsString::from("abort"),
            0,
            NativeFunction::from_fn_ptr(|this, args, context| {
                let signal = object(this)?
                    .downcast_ref::<AbortController>()
                    .ok_or_else(|| typ("AbortController receiver required"))?
                    .signal
                    .clone();
                abort_signal(&signal, args.get_or_undefined(0).clone(), context)?;
                Ok(JsValue::undefined())
            }),
        );
        class.property(
            JsSymbol::to_string_tag(),
            JsString::from("AbortController"),
            Attribute::CONFIGURABLE,
        );
        Ok(())
    }
}
/// Registers native Event, EventTarget, AbortSignal and AbortController once.
/// Requires the native DOMException capability already registered by web_text.
pub fn register(context: &mut Context) -> JsResult<()> {
    if context.get_data::<Registration>().is_some() {
        return Ok(());
    }
    let origin = context.clock().now().millis_since_epoch();
    context.insert_data(Registration {
        host: Gc::new(GcRefCell::new(HostState::default())),
        origin,
    });
    context.register_global_class::<Event>()?;
    context.register_global_class::<EventTarget>()?;
    context.register_global_class::<AbortSignal>()?;
    context.register_global_class::<AbortController>()?;
    let parent = context.get_global_class::<EventTarget>().unwrap();
    let child = context.get_global_class::<AbortSignal>().unwrap();
    child.prototype().set_prototype(Some(parent.prototype()));
    child
        .constructor()
        .set_prototype(Some(parent.constructor()));
    // Boa's ClassBuilder intentionally defaults to ECMAScript class method
    // descriptors. Web IDL operations are enumerable.
    for (prototype, names) in [
        (
            context.get_global_class::<Event>().unwrap().prototype(),
            &[
                "stopPropagation",
                "stopImmediatePropagation",
                "preventDefault",
                "composedPath",
                "initEvent",
            ][..],
        ),
        (
            parent.prototype(),
            &["addEventListener", "removeEventListener", "dispatchEvent"][..],
        ),
        (child.prototype(), &["throwIfAborted"][..]),
        (child.constructor(), &["abort", "any", "timeout"][..]),
        (
            context
                .get_global_class::<AbortController>()
                .unwrap()
                .prototype(),
            &["abort"][..],
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
#[cfg(test)]
#[path = "web_abort_tests.rs"]
mod tests;
