//! Native async context for the Node 22 AsyncLocalStorage contract.
//!
//! Install `Rc<AsyncContextHooks>` on the Context builder, then call `register`
//! with that same Rc. Resolve `node:async_hooks` and `async_hooks` to its returned
//! synthetic module. Nothing is installed on the JS global object. Other Node
//! async_hooks exports (AsyncResource, IDs, createHook) are not implemented.
//!
//! Boa 0.22's Await opcode and async-generator AwaitReturn both use
//! PerformPromiseThen, whose callbacks pass through these HostHooks. No JS
//! Promise patching is used. Native timers/I/O callbacks must capture() when
//! scheduled and call snapshot.run() when dispatched; capturing at dispatch
//! would be incorrect. This module provides no timers or implicit I/O.
//!
//! References: https://nodejs.org/download/release/v22.15.0/docs/api/async_context.html
//! Stores and callbacks are traced, snapshots are immutable, and at most 4096
//! simultaneously active storage instances are permitted per context frame.
use boa_engine::{
    Context, JsArgs, JsData, JsError, JsNativeError, JsObject, JsResult, JsString, JsValue, Module,
    NativeFunction,
    class::{Class, ClassBuilder},
    context::HostHooks,
    job::JobCallback,
    module::SyntheticModuleInitializer,
    object::{FunctionObjectBuilder, ObjectInitializer, builtins::JsFunction},
    property::{Attribute, PropertyDescriptor},
};
use boa_gc::{Finalize, Gc, GcRefCell, Trace};
use std::{cell::Cell, rc::Rc};

const MAX_STORES: usize = 4096;
#[derive(Trace, Finalize)]
struct Storage {
    enabled: Cell<bool>,
}
#[derive(Clone, Trace, Finalize)]
struct Entry {
    storage: Gc<Storage>,
    value: JsValue,
}
#[derive(Default, Trace, Finalize)]
struct Frame {
    entries: Vec<Entry>,
}
#[derive(Default, Trace, Finalize)]
struct State {
    current: GcRefCell<Gc<Frame>>,
}
struct Registration {
    state: Gc<State>,
    module: Module,
}

/// A snapshot retains real JS store identity and owns traced references.
#[derive(Clone, Trace, Finalize, JsData)]
pub struct AsyncContextSnapshot {
    state: Gc<State>,
    frame: Gc<Frame>,
}
impl AsyncContextSnapshot {
    /// Restore this scheduling context for one host callback. The previous frame
    /// is restored on both normal return and a JavaScript exception.
    pub fn run<T>(
        &self,
        context: &mut Context,
        operation: impl FnOnce(&mut Context) -> JsResult<T>,
    ) -> JsResult<T> {
        let state = state(context)?;
        if !Gc::ptr_eq(&state, &self.state) {
            return Err(typ("Async context snapshot belongs to another Context"));
        }
        let previous = std::mem::replace(&mut *self.state.current.borrow_mut(), self.frame.clone());
        let result = operation(context);
        *self.state.current.borrow_mut() = previous;
        result
    }
}
/// Capture now, not when the asynchronous operation completes.
pub fn capture(context: &Context) -> JsResult<AsyncContextSnapshot> {
    let state = state(context)?;
    let frame = Gc::new(Frame {
        entries: state
            .current
            .borrow()
            .entries
            .iter()
            .filter(|entry| entry.storage.enabled.get())
            .cloned()
            .collect(),
    });
    Ok(AsyncContextSnapshot { state, frame })
}
fn state(context: &Context) -> JsResult<Gc<State>> {
    context
        .get_data::<Registration>()
        .map(|registration| registration.state.clone())
        .ok_or_else(|| typ("Native async context is not registered"))
}
fn typ(message: &str) -> JsError {
    JsNativeError::typ().with_message(message.to_owned()).into()
}
fn callable(value: &JsValue) -> JsResult<JsObject> {
    value
        .as_callable()
        .ok_or_else(|| typ("AsyncLocalStorage callback must be callable"))
}

/// Install before any script/module evaluation. Existing unrelated HostHooks
/// overrides must delegate these two methods instead of replacing this behavior.
#[derive(Default)]
pub struct AsyncContextHooks;
impl HostHooks for AsyncContextHooks {
    fn make_job_callback(&self, callback: JsFunction, context: &mut Context) -> JobCallback {
        // Before explicit capability registration no stores can exist.
        JobCallback::new(callback, CallbackContext(capture(context).ok()))
    }
    fn call_job_callback(
        &self,
        job: &JobCallback,
        this: &JsValue,
        args: &[JsValue],
        context: &mut Context,
    ) -> JsResult<JsValue> {
        let snapshot = job
            .host_defined()
            .downcast_ref::<CallbackContext>()
            .and_then(|data| data.0.as_ref());
        match snapshot {
            Some(snapshot) => {
                snapshot.run(context, |context| job.callback().call(this, args, context))
            }
            None => job.callback().call(this, args, context),
        }
    }
}
#[derive(Trace, Finalize, JsData)]
struct CallbackContext(Option<AsyncContextSnapshot>);
#[derive(Trace, Finalize, JsData)]
struct AsyncLocalStorage {
    state: Gc<State>,
    storage: Gc<Storage>,
}
fn receiver(value: &JsValue) -> JsResult<(Gc<State>, Gc<Storage>)> {
    value
        .as_object()
        .and_then(|object| {
            object
                .downcast_ref::<AsyncLocalStorage>()
                .map(|value| (value.state.clone(), value.storage.clone()))
        })
        .ok_or_else(|| typ("AsyncLocalStorage receiver required"))
}
fn lookup(state: &State, storage: &Gc<Storage>) -> Option<JsValue> {
    state
        .current
        .borrow()
        .entries
        .iter()
        .find(|entry| Gc::ptr_eq(&entry.storage, storage))
        .map(|entry| entry.value.clone())
}
fn replace(state: &State, storage: &Gc<Storage>, value: Option<JsValue>) -> JsResult<()> {
    let mut entries = state.current.borrow().entries.clone();
    if let Some(index) = entries
        .iter()
        .position(|entry| Gc::ptr_eq(&entry.storage, storage))
    {
        entries.remove(index);
    }
    // Disabled instances do not consume active-frame capacity.
    entries.retain(|entry| entry.storage.enabled.get());
    if let Some(value) = value {
        if entries.len() >= MAX_STORES {
            return Err(JsNativeError::range()
                .with_message("Native async context store limit exceeded")
                .into());
        }
        entries.push(Entry {
            storage: storage.clone(),
            value,
        });
    }
    *state.current.borrow_mut() = Gc::new(Frame { entries });
    Ok(())
}
impl Class for AsyncLocalStorage {
    const NAME: &'static str = "AsyncLocalStorage";
    fn data_constructor(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<Self> {
        // Node 22.15 has no constructor options. Avoid accepting a later profile's
        // defaultValue/name and silently providing the older behavior.
        if !args.get_or_undefined(0).is_undefined() {
            return Err(typ(
                "AsyncLocalStorage constructor options require a newer Node profile; this capability implements Node 22.15",
            ));
        }
        Ok(Self {
            state: state(context)?,
            storage: Gc::new(Storage {
                enabled: Cell::new(false),
            }),
        })
    }
    fn init(class: &mut ClassBuilder) -> JsResult<()> {
        class.method(
            JsString::from("getStore"),
            0,
            NativeFunction::from_fn_ptr(|this, _, _| {
                let (state, storage) = receiver(this)?;
                Ok(if storage.enabled.get() {
                    lookup(&state, &storage).unwrap_or_default()
                } else {
                    JsValue::undefined()
                })
            }),
        );
        class.method(
            JsString::from("enterWith"),
            1,
            NativeFunction::from_fn_ptr(|this, args, _| {
                let (state, storage) = receiver(this)?;
                replace(&state, &storage, Some(args.get_or_undefined(0).clone()))?;
                storage.enabled.set(true);
                Ok(JsValue::undefined())
            }),
        );
        class.method(
            JsString::from("disable"),
            0,
            NativeFunction::from_fn_ptr(|this, _, _| {
                let (state, storage) = receiver(this)?;
                storage.enabled.set(false);
                replace(&state, &storage, None)?;
                Ok(JsValue::undefined())
            }),
        );
        class.method(JsString::from("run"), 2, NativeFunction::from_fn_ptr(run));
        class.method(JsString::from("exit"), 1, NativeFunction::from_fn_ptr(exit));
        class.static_method(JsString::from("bind"), 1, NativeFunction::from_fn_ptr(bind));
        class.static_method(
            JsString::from("snapshot"),
            0,
            NativeFunction::from_fn_ptr(snapshot),
        );
        Ok(())
    }
}
fn run(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let (state, storage) = receiver(this)?;
    let callback = callable(args.get_or_undefined(1))?;
    let previous = lookup(&state, &storage);
    replace(&state, &storage, Some(args.get_or_undefined(0).clone()))?;
    storage.enabled.set(true);
    let result = callback.call(
        &JsValue::undefined(),
        args.get(2..).unwrap_or_default(),
        context,
    );
    replace(&state, &storage, previous)?;
    result
}
fn exit(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let (state, storage) = receiver(this)?;
    let callback = callable(args.get_or_undefined(0))?;
    let previous = lookup(&state, &storage);
    let enabled = storage.enabled.replace(false);
    let result = callback.call(
        &JsValue::undefined(),
        args.get(1..).unwrap_or_default(),
        context,
    );
    storage.enabled.set(enabled);
    replace(&state, &storage, previous)?;
    result
}
#[derive(Trace, Finalize)]
struct Bound {
    snapshot: AsyncContextSnapshot,
    callback: JsObject,
}
fn bind(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let callback = callable(args.get_or_undefined(0))?;
    let snapshot = capture(context)?;
    let length = callback.get(JsString::from("length"), context)?;
    let function = FunctionObjectBuilder::new(
        context.realm(),
        NativeFunction::from_copy_closure_with_captures(
            |this, args, bound: &Bound, context| {
                bound
                    .snapshot
                    .run(context, |context| bound.callback.call(this, args, context))
            },
            Bound { snapshot, callback },
        ),
    )
    .name(JsString::from("bound"))
    .build();
    function.define_property_or_throw(
        JsString::from("length"),
        PropertyDescriptor::builder()
            .value(length)
            .configurable(true),
        context,
    )?;
    Ok(function.into())
}
fn snapshot(_: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let snapshot = capture(context)?;
    Ok(FunctionObjectBuilder::new(
        context.realm(),
        NativeFunction::from_copy_closure_with_captures(
            |_, args, snapshot: &AsyncContextSnapshot, context| {
                let callback = callable(args.get_or_undefined(0))?;
                snapshot.run(context, |context| {
                    callback.call(
                        &JsValue::undefined(),
                        args.get(1..).unwrap_or_default(),
                        context,
                    )
                })
            },
            snapshot,
        ),
    )
    .length(1)
    .build()
    .into())
}
/// Register only the explicit native module, with a stable per-context identity.
/// The default export is the same object whose AsyncLocalStorage property is
/// the named export. Unsupported named imports fail module linking normally.
pub fn register(context: &mut Context, hooks: Rc<AsyncContextHooks>) -> JsResult<Module> {
    let expected: Rc<dyn HostHooks> = hooks;
    if !Rc::ptr_eq(&context.host_hooks(), &expected) {
        return Err(typ(
            "Install the supplied AsyncContextHooks on Context::builder before registration",
        ));
    }
    if let Some(registration) = context.get_data::<Registration>() {
        return Ok(registration.module.clone());
    }
    let mut builder = ClassBuilder::new::<AsyncLocalStorage>(context);
    AsyncLocalStorage::init(&mut builder)?;
    let class = builder.build();
    let constructor = class.constructor();
    context.realm().register_class::<AsyncLocalStorage>(class);
    let default = ObjectInitializer::new(context)
        .property(
            JsString::from("AsyncLocalStorage"),
            constructor.clone(),
            Attribute::all(),
        )
        .build();
    let module = Module::synthetic(
        &[
            JsString::from("AsyncLocalStorage"),
            JsString::from("default"),
        ],
        SyntheticModuleInitializer::from_copy_closure_with_captures(
            |module, captures: &(JsObject, JsObject), _| {
                module.set_export(
                    &JsString::from("AsyncLocalStorage"),
                    captures.0.clone().into(),
                )?;
                module.set_export(&JsString::from("default"), captures.1.clone().into())?;
                Ok(())
            },
            (constructor, default),
        ),
        None,
        None,
        context,
    );
    context.insert_data(Registration {
        state: Gc::new(State::default()),
        module: module.clone(),
    });
    Ok(module)
}
#[cfg(test)]
#[path = "node_async_hooks_tests.rs"]
mod tests;
