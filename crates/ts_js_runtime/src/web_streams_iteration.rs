//! Serialized native asynchronous iteration for default readable streams.
use super::*;
use boa_engine::builtins::object::OrdinaryObject;
#[derive(Trace, Finalize, JsData)]
struct Iterator {
    state: Gc<GcRefCell<IteratorData>>,
}
#[derive(Trace, Finalize)]
struct IteratorData {
    reader: ReaderState,
    prevent_cancel: bool,
    done: bool,
    ongoing: Option<JsPromise>,
    pending: usize,
}
struct IteratorRegistration {
    prototype: JsObject,
}
#[derive(Clone, Trace, Finalize)]
struct Operation {
    state: Gc<GcRefCell<IteratorData>>,
    returning: bool,
    value: JsValue,
}
fn finish(state: &Gc<GcRefCell<IteratorData>>, context: &mut Context) -> JsResult<()> {
    state.borrow_mut().done = true;
    let reader = state.borrow().reader.clone();
    release(&reader, context)
}
fn operation(operation: &Operation, context: &mut Context) -> JsResult<JsPromise> {
    let state = &operation.state;
    let (done, reader, prevent_cancel) = {
        let d = state.borrow();
        (d.done, d.reader.clone(), d.prevent_cancel)
    };
    if done {
        let value = if operation.returning {
            operation.value.clone()
        } else {
            JsValue::undefined()
        };
        let result = create_iter_result_object(value, true, context);
        return settled(result, context);
    }
    if operation.returning {
        state.borrow_mut().done = true;
        let promise = if prevent_cancel {
            settled(JsValue::undefined(), context)?
        } else {
            let stream = reader
                .borrow()
                .stream
                .clone()
                .ok_or_else(|| typ("Async iterator reader released"))?;
            cancel_state(&stream, operation.value.clone(), context)?
        };
        release(&reader, context)?;
        let fulfilled = NativeFunction::from_copy_closure_with_captures(
            |_, _, operation, context| {
                Ok(create_iter_result_object(
                    operation.value.clone(),
                    true,
                    context,
                ))
            },
            operation.clone(),
        )
        .to_js_function(context.realm());
        return promise.then_intrinsic(Some(fulfilled), None, context);
    }
    let promise = read_state(&reader, context)?;
    let fulfilled = NativeFunction::from_copy_closure_with_captures(
        |_, args, state, context| {
            let value = args.get_or_undefined(0).clone();
            if obj(&value)?
                .get(JsString::from("done"), context)?
                .to_boolean()
            {
                finish(state, context)?;
            }
            Ok(value)
        },
        state.clone(),
    )
    .to_js_function(context.realm());
    let failed = NativeFunction::from_copy_closure_with_captures(
        |_, args, state, context| {
            finish(state, context)?;
            Err(JsError::from_opaque(args.get_or_undefined(0).clone()))
        },
        state.clone(),
    )
    .to_js_function(context.realm());
    promise.then_intrinsic(Some(fulfilled), Some(failed), context)
}
fn invoke(
    this: &JsValue,
    args: &[JsValue],
    returning: bool,
    context: &mut Context,
) -> JsResult<JsValue> {
    let result = (|| {
        let state = obj(this)?
            .downcast_ref::<Iterator>()
            .map(|v| v.state.clone())
            .ok_or_else(|| typ("ReadableStream iterator receiver required"))?;
        if state.borrow().pending >= MAX_QUEUE {
            return Err(limit());
        }
        let operation = Operation {
            state: state.clone(),
            returning,
            value: args.get_or_undefined(0).clone(),
        };
        let previous = state.borrow().ongoing.clone();
        let promise = if let Some(previous) = previous {
            let next = NativeFunction::from_copy_closure_with_captures(
                |_, _, operation, context| operation_result(operation, context).map(JsValue::from),
                operation,
            )
            .to_js_function(context.realm());
            previous.then_intrinsic(Some(next.clone()), Some(next), context)?
        } else {
            operation_result(&operation, context)?
        };
        state.borrow_mut().pending += 1;
        let success = NativeFunction::from_copy_closure_with_captures(
            |_, args, state, _| {
                state.borrow_mut().pending -= 1;
                Ok(args.get_or_undefined(0).clone())
            },
            state.clone(),
        )
        .to_js_function(context.realm());
        let failure = NativeFunction::from_copy_closure_with_captures(
            |_, args, state, _| {
                state.borrow_mut().pending -= 1;
                Err(JsError::from_opaque(args.get_or_undefined(0).clone()))
            },
            state.clone(),
        )
        .to_js_function(context.realm());
        let promise = promise.then_intrinsic(Some(success), Some(failure), context)?;
        state.borrow_mut().ongoing = Some(promise.clone());
        Ok(promise)
    })();
    promise_method(result, context)
}
fn operation_result(operation: &Operation, context: &mut Context) -> JsResult<JsPromise> {
    match self::operation(operation, context) {
        Ok(p) => Ok(p),
        Err(e) => rejected(e, context),
    }
}
pub(super) fn values(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let stream = state(this)?;
    let options = dictionary(args.get_or_undefined(0))?;
    let prevent_cancel = field(&options, "preventCancel", context)?.to_boolean();
    let reader = acquire(&stream, context)?;
    let prototype = context
        .get_data::<IteratorRegistration>()
        .ok_or_else(|| typ("Stream iterator not registered"))?
        .prototype
        .clone();
    Ok(JsObject::from_proto_and_data(
        prototype,
        Iterator {
            state: Gc::new(GcRefCell::new(IteratorData {
                reader,
                prevent_cancel,
                done: false,
                ongoing: None,
                pending: 0,
            })),
        },
    )
    .into())
}
pub(super) fn register(context: &mut Context) -> JsResult<()> {
    let parent = context
        .intrinsics()
        .objects()
        .iterator_prototypes()
        .async_iterator();
    let prototype = JsObject::from_proto_and_data(parent, OrdinaryObject);
    for (name, returning) in [("next", false), ("return", true)] {
        let function = FunctionObjectBuilder::new(
            context.realm(),
            NativeFunction::from_copy_closure(move |this, args, context| {
                invoke(this, args, returning, context)
            }),
        )
        .name(JsString::from(name))
        .length(usize::from(returning))
        .build();
        prototype.define_property_or_throw(
            JsString::from(name),
            PropertyDescriptor::builder()
                .value(function)
                .writable(true)
                .configurable(true)
                .enumerable(true),
            context,
        )?;
    }
    prototype.define_property_or_throw(
        JsSymbol::to_string_tag(),
        PropertyDescriptor::builder()
            .value(JsString::from("ReadableStream AsyncIterator"))
            .configurable(true),
        context,
    )?;
    context.insert_data(IteratorRegistration { prototype });
    Ok(())
}
