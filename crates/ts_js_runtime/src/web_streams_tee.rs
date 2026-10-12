//! Default-stream tee. Both branches share each JS chunk identity, as mandated
//! for public ReadableStream.tee(). Structured cloning is not approximated here.
use super::*;
use boa_engine::object::builtins::JsArray;
#[derive(Trace, Finalize)]
struct Tee {
    reader: ReaderState,
    clone_second: bool,
    branches: Vec<State>,
    reading: bool,
    read_again: bool,
    canceled: [bool; 2],
    reasons: [JsValue; 2],
    cancel_promise: JsPromise,
    cancel_settle: ResolvingFunctions,
}
type TeeState = Gc<GcRefCell<Tee>>;
fn fail(tee: &TeeState, reason: JsValue, context: &mut Context) -> JsResult<()> {
    let (branches, resolve_cancel, settle) = {
        let mut d = tee.borrow_mut();
        d.reading = false;
        (
            d.branches.clone(),
            !d.canceled[0] || !d.canceled[1],
            d.cancel_settle.clone(),
        )
    };
    for branch in branches {
        error_state(&branch, reason.clone(), context)?;
    }
    if resolve_cancel {
        resolve(&settle, JsValue::undefined(), context)?;
    }
    Ok(())
}
fn pull(tee: &TeeState, context: &mut Context) -> JsResult<JsValue> {
    {
        let mut d = tee.borrow_mut();
        if d.reading {
            d.read_again = true;
            return Ok(JsValue::undefined());
        }
        d.reading = true;
    }
    let reader = tee.borrow().reader.clone();
    let promise = read_state(&reader, context)?;
    let fulfilled = NativeFunction::from_copy_closure_with_captures(
        |_, args, tee, context| {
            let result = obj(args.get_or_undefined(0))?;
            let done = result.get(JsString::from("done"), context)?.to_boolean();
            let value = result.get(JsString::from("value"), context)?;
            let (branches, canceled, settle) = {
                let mut d = tee.borrow_mut();
                d.read_again = false;
                (d.branches.clone(), d.canceled, d.cancel_settle.clone())
            };
            if done {
                tee.borrow_mut().reading = false;
                for (index, branch) in branches.iter().enumerate() {
                    if !canceled[index] && can_enqueue(branch) {
                        close_controller(branch, context)?;
                    }
                }
                if !canceled[0] || !canceled[1] {
                    resolve(&settle, JsValue::undefined(), context)?;
                }
            } else {
                let second = if tee.borrow().clone_second && !canceled[1] {
                    match byob::clone_chunk(&value, context) {
                        Ok(value) => value,
                        Err(error) => {
                            let reason = error.into_opaque(context)?;
                            for branch in &branches {
                                error_state(branch, reason.clone(), context)?;
                            }
                            let source = tee.borrow().reader.borrow().stream.clone();
                            if let Some(source) = source {
                                let promise = cancel_state(&source, reason, context)?;
                                resolve(&settle, promise.into(), context)?;
                            }
                            tee.borrow_mut().reading = false;
                            return Ok(JsValue::undefined());
                        }
                    }
                } else {
                    value.clone()
                };
                for (index, branch) in branches.iter().enumerate() {
                    if !canceled[index]
                        && can_enqueue(branch)
                        && let Err(error) = enqueue(
                            branch,
                            if index == 0 {
                                value.clone()
                            } else {
                                second.clone()
                            },
                            context,
                        )
                    {
                        let reason = error.into_opaque(context)?;
                        tee.borrow_mut().reading = false;
                        for other in &branches {
                            error_state(other, reason.clone(), context)?;
                        }
                        let source = tee.borrow().reader.borrow().stream.clone();
                        if let Some(source) = source {
                            let promise = cancel_state(&source, reason, context)?;
                            resolve(&settle, promise.into(), context)?;
                        }
                        return Ok(JsValue::undefined());
                    }
                }
                let again = {
                    let mut d = tee.borrow_mut();
                    d.reading = false;
                    d.read_again
                };
                if again {
                    pull(tee, context)?;
                }
            }
            Ok(JsValue::undefined())
        },
        tee.clone(),
    )
    .to_js_function(context.realm());
    let failed = NativeFunction::from_copy_closure_with_captures(
        |_, args, tee, context| {
            fail(tee, args.get_or_undefined(0).clone(), context)?;
            Ok(JsValue::undefined())
        },
        tee.clone(),
    )
    .to_js_function(context.realm());
    drop(promise.then_intrinsic(Some(fulfilled), Some(failed), context)?);
    Ok(JsValue::undefined())
}
pub(super) fn tee(this: &JsValue, context: &mut Context) -> JsResult<JsValue> {
    tee_with_clone(this, false, context)
}
pub(super) fn tee_with_clone(
    this: &JsValue,
    clone_second: bool,
    context: &mut Context,
) -> JsResult<JsValue> {
    let stream = state(this)?;
    if stream.borrow().bytes.is_some() {
        return byte_tee::tee(&stream, context);
    }
    let reader = acquire(&stream, context)?;
    let (cancel_promise, cancel_settle) = JsPromise::new_pending(context);
    let tee = Gc::new(GcRefCell::new(Tee {
        reader: reader.clone(),
        clone_second,
        branches: vec![],
        reading: false,
        read_again: false,
        canceled: [false; 2],
        reasons: [JsValue::undefined(), JsValue::undefined()],
        cancel_promise,
        cancel_settle,
    }));
    let mut objects = vec![];
    for index in 0..2 {
        let branch = make_state(1.0);
        attach_controller(&branch, context)?;
        let pull = NativeFunction::from_copy_closure_with_captures(
            |_, _, tee, context| self::pull(tee, context),
            tee.clone(),
        )
        .to_js_function(context.realm());
        let cancel = NativeFunction::from_copy_closure_with_captures(
            move |_, args, tee, context| {
                let (both, reasons, promise, settle, reader) = {
                    let mut d = tee.borrow_mut();
                    d.canceled[index] = true;
                    d.reasons[index] = args.get_or_undefined(0).clone();
                    (
                        d.canceled[0] && d.canceled[1],
                        d.reasons.clone(),
                        d.cancel_promise.clone(),
                        d.cancel_settle.clone(),
                        d.reader.clone(),
                    )
                };
                if both {
                    let stream = reader
                        .borrow()
                        .stream
                        .clone()
                        .ok_or_else(|| typ("Tee source reader released"))?;
                    let reason = JsArray::from_iter(reasons, context).into();
                    let cancel = cancel_state(&stream, reason, context)?;
                    resolve(&settle, cancel.into(), context)?;
                }
                Ok(promise.into())
            },
            tee.clone(),
        )
        .to_js_function(context.realm());
        {
            let mut d = branch.borrow_mut();
            d.pull = Some(pull.into());
            d.cancel = Some(cancel.into());
        }
        objects.push(
            ReadableStream::from_data(
                ReadableStream {
                    state: branch.clone(),
                },
                context,
            )?
            .into(),
        );
        tee.borrow_mut().branches.push(branch.clone());
        start(&branch, JsValue::undefined(), context)?;
    }
    let failed = NativeFunction::from_copy_closure_with_captures(
        |_, args, tee, context| {
            fail(tee, args.get_or_undefined(0).clone(), context)?;
            Ok(JsValue::undefined())
        },
        tee,
    )
    .to_js_function(context.realm());
    let closed = reader.borrow().closed.clone();
    drop(closed.then_intrinsic(None, Some(failed), context)?);
    Ok(JsArray::from_iter(objects, context).into())
}
