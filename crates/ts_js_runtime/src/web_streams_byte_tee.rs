//! WHATWG ReadableByteStreamTee: BYOB demand is forwarded through a source
//! BYOB reader, and both live branches receive independently owned buffers.
use super::*;
use boa_engine::object::builtins::JsArray;
#[derive(Trace, Finalize)]
struct Tee {
    source: State,
    reader: ReaderState,
    branches: Vec<State>,
    reading: bool,
    again: [bool; 2],
    canceled: [bool; 2],
    reasons: [JsValue; 2],
    cancel_promise: JsPromise,
    cancel_settle: ResolvingFunctions,
}
type Shared = Gc<GcRefCell<Tee>>;
fn fail(tee: &Shared, reason: JsValue, cancel: bool, c: &mut Context) -> JsResult<()> {
    let (branches, source, settle, resolve_cancel) = {
        let mut t = tee.borrow_mut();
        t.reading = false;
        (
            t.branches.clone(),
            t.source.clone(),
            t.cancel_settle.clone(),
            !t.canceled[0] || !t.canceled[1],
        )
    };
    for branch in branches {
        error_state(&branch, reason.clone(), c)?;
    }
    if cancel {
        let promise = cancel_state(&source, reason, c)?;
        resolve(&settle, promise.into(), c)?;
    } else if resolve_cancel {
        resolve(&settle, JsValue::undefined(), c)?;
    }
    Ok(())
}
fn forward(tee: &Shared, reader: &ReaderState, c: &mut Context) -> JsResult<()> {
    let failed = NativeFunction::from_copy_closure_with_captures(
        |_, args, (tee, reader), c| {
            if Gc::ptr_eq(&tee.borrow().reader, reader) {
                fail(tee, args.get_or_undefined(0).clone(), false, c)?;
            }
            Ok(JsValue::undefined())
        },
        (tee.clone(), reader.clone()),
    )
    .to_js_function(c.realm());
    let closed = reader.borrow().closed.clone();
    drop(closed.then_intrinsic(None, Some(failed), c)?);
    Ok(())
}
fn pull(tee: &Shared, index: usize, c: &mut Context) -> JsResult<JsValue> {
    {
        let mut t = tee.borrow_mut();
        if t.reading {
            t.again[index] = true;
            return Ok(JsValue::undefined());
        }
        t.reading = true;
    }
    let branch = tee.borrow().branches[index].clone();
    let view = byob::request_view(&branch, c)?;
    let byob = view.is_some();
    let mut reader = tee.borrow().reader.clone();
    if reader.borrow().byob != byob {
        release(&reader, c)?;
        reader = acquire(&tee.borrow().source, c)?;
        reader.borrow_mut().byob = byob;
        tee.borrow_mut().reader = reader.clone();
        forward(tee, &reader, c)?;
    }
    let promise = if let Some(view) = view {
        byob::read_internal(&reader, &view, c)?
    } else {
        read_state(&reader, c)?
    };
    let success = NativeFunction::from_copy_closure_with_captures(
        move |_, args, tee, c| {
            let operation: JsResult<JsValue> = (|| {
                let result = obj(args.get_or_undefined(0))?;
                let done = result.get(JsString::from("done"), c)?.to_boolean();
                let value = result.get(JsString::from("value"), c)?;
                let (branches, canceled, settle) = {
                    let mut t = tee.borrow_mut();
                    t.again = [false; 2];
                    (t.branches.clone(), t.canceled, t.cancel_settle.clone())
                };
                if done {
                    tee.borrow_mut().reading = false;
                    if byob {
                        for (i, branch) in branches.iter().enumerate() {
                            if !canceled[i] && can_enqueue(branch) {
                                byob::close(branch, c)?;
                            }
                        }
                        if !value.is_undefined() {
                            if !canceled[index] {
                                byob::respond_view(&branches[index], &value, c)?;
                            }
                            if !canceled[1 - index] {
                                byob::close_and_respond(&branches[1 - index], c)?;
                            }
                        }
                    } else {
                        for (i, branch) in branches.iter().enumerate() {
                            if !canceled[i] {
                                byob::close_and_respond(branch, c)?;
                            }
                        }
                    }
                    if !canceled[0] || !canceled[1] {
                        resolve(&settle, JsValue::undefined(), c)?;
                    }
                } else {
                    if byob {
                        let other = 1 - index;
                        let cloned = if !canceled[other] {
                            Some(JsUint8Array::from_iter(copy_uint8_chunk(&value, c)?, c)?.into())
                        } else {
                            None
                        };
                        if !canceled[index] {
                            byob::respond_view(&branches[index], &value, c)?;
                        }
                        if let Some(cloned) = cloned {
                            enqueue(&branches[other], cloned, c)?;
                        }
                    } else {
                        let second = if !canceled[0] && !canceled[1] {
                            JsUint8Array::from_iter(copy_uint8_chunk(&value, c)?, c)?.into()
                        } else {
                            value.clone()
                        };
                        if !canceled[0] {
                            enqueue(&branches[0], value, c)?;
                        }
                        if !canceled[1] {
                            enqueue(&branches[1], second, c)?;
                        }
                    }
                    let again = {
                        let mut t = tee.borrow_mut();
                        t.reading = false;
                        t.again
                    };
                    if again[0] {
                        pull(tee, 0, c)?;
                    } else if again[1] {
                        pull(tee, 1, c)?;
                    }
                }
                Ok(JsValue::undefined())
            })();
            if let Err(error) = operation {
                fail(tee, error.into_opaque(c)?, true, c)?;
            }
            Ok(JsValue::undefined())
        },
        tee.clone(),
    )
    .to_js_function(c.realm());
    let failure = NativeFunction::from_copy_closure_with_captures(
        |_, args, tee, c| {
            fail(tee, args.get_or_undefined(0).clone(), false, c)?;
            Ok(JsValue::undefined())
        },
        tee.clone(),
    )
    .to_js_function(c.realm());
    drop(promise.then_intrinsic(Some(success), Some(failure), c)?);
    Ok(JsValue::undefined())
}
pub(super) fn tee(source: &State, c: &mut Context) -> JsResult<JsValue> {
    let reader = acquire(source, c)?;
    let (cancel_promise, cancel_settle) = JsPromise::new_pending(c);
    let tee = Gc::new(GcRefCell::new(Tee {
        source: source.clone(),
        reader: reader.clone(),
        branches: vec![],
        reading: false,
        again: [false; 2],
        canceled: [false; 2],
        reasons: [JsValue::undefined(), JsValue::undefined()],
        cancel_promise,
        cancel_settle,
    }));
    let mut objects = vec![];
    for index in 0..2 {
        let branch = make_state(0.0);
        byob::initialize(&branch, None);
        attach_controller(&branch, c)?;
        let pull = NativeFunction::from_copy_closure_with_captures(
            move |_, _, tee, c| self::pull(tee, index, c),
            tee.clone(),
        )
        .to_js_function(c.realm());
        let cancel = NativeFunction::from_copy_closure_with_captures(
            move |_, args, tee, c| {
                let (both, reasons, source, settle, promise) = {
                    let mut t = tee.borrow_mut();
                    t.canceled[index] = true;
                    t.reasons[index] = args.get_or_undefined(0).clone();
                    (
                        t.canceled[0] && t.canceled[1],
                        t.reasons.clone(),
                        t.source.clone(),
                        t.cancel_settle.clone(),
                        t.cancel_promise.clone(),
                    )
                };
                if both {
                    let reasons = JsArray::from_iter(reasons, c).into();
                    let cancel = cancel_state(&source, reasons, c)?;
                    resolve(&settle, cancel.into(), c)?;
                }
                Ok(promise.into())
            },
            tee.clone(),
        )
        .to_js_function(c.realm());
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
                c,
            )?
            .into(),
        );
        tee.borrow_mut().branches.push(branch.clone());
        start(&branch, JsValue::undefined(), c)?;
    }
    forward(&tee, &reader, c)?;
    Ok(JsArray::from_iter(objects, c).into())
}
