//! Internal identity pipe used when transferring a Fetch body. This models the
//! identity transform's one pending write and zero readable HWM without exposing
//! an incomplete public TransformStream or WritableStream implementation.
use super::*;
#[derive(Trace, Finalize)]
struct Proxy {
    reader: ReaderState,
    output: State,
    buffered: Option<JsValue>,
    demand: Option<ResolvingFunctions>,
    reading: bool,
    ended: bool,
}
type Shared = Gc<GcRefCell<Proxy>>;
fn finish(proxy: &Shared, reason: Option<JsValue>, context: &mut Context) -> JsResult<()> {
    let (reader, output, demand) = {
        let mut p = proxy.borrow_mut();
        if p.ended {
            return Ok(());
        }
        p.ended = true;
        p.buffered = None;
        (p.reader.clone(), p.output.clone(), p.demand.take())
    };
    if let Some(reason) = reason {
        error_state(&output, reason, context)?;
    } else if can_enqueue(&output) {
        close_controller(&output, context)?;
    }
    if let Some(demand) = demand {
        resolve(&demand, JsValue::undefined(), context)?;
    }
    release(&reader, context)
}
fn deliver(proxy: &Shared, context: &mut Context) -> JsResult<()> {
    let ready = {
        let p = proxy.borrow();
        p.buffered.is_some() && p.demand.is_some() && !p.ended
    };
    if !ready {
        return Ok(());
    }
    let (value, output, demand) = {
        let mut p = proxy.borrow_mut();
        (
            p.buffered.take().unwrap(),
            p.output.clone(),
            p.demand.take().unwrap(),
        )
    };
    enqueue(&output, value, context)?;
    resolve(&demand, JsValue::undefined(), context)?;
    pump(proxy, context)
}
fn pump(proxy: &Shared, context: &mut Context) -> JsResult<()> {
    let reader = {
        let mut p = proxy.borrow_mut();
        if p.ended || p.reading || p.buffered.is_some() {
            return Ok(());
        }
        p.reading = true;
        p.reader.clone()
    };
    let promise = read_state(&reader, context)?;
    let yes = NativeFunction::from_copy_closure_with_captures(
        |_, args, proxy, c| {
            let operation: JsResult<()> = (|| {
                if proxy.borrow().ended {
                    return Ok(());
                }
                let result = obj(args.get_or_undefined(0))?;
                proxy.borrow_mut().reading = false;
                if result.get(JsString::from("done"), c)?.to_boolean() {
                    finish(proxy, None, c)?;
                } else {
                    let value = result.get(JsString::from("value"), c)?;
                    proxy.borrow_mut().buffered = Some(value);
                    deliver(proxy, c)?;
                }
                Ok(())
            })();
            if let Err(error) = operation {
                finish(proxy, Some(error.into_opaque(c)?), c)?;
            }
            Ok(JsValue::undefined())
        },
        proxy.clone(),
    )
    .to_js_function(context.realm());
    let no = NativeFunction::from_copy_closure_with_captures(
        |_, args, proxy, c| {
            finish(proxy, Some(args.get_or_undefined(0).clone()), c)?;
            Ok(JsValue::undefined())
        },
        proxy.clone(),
    )
    .to_js_function(context.realm());
    drop(promise.then_intrinsic(Some(yes), Some(no), context)?);
    Ok(())
}
/// Transfer through an internal native identity pipe. The input is locked and
/// disturbed synchronously. The output is a default stream with HWM 0, sharing
/// chunk identities; one write may wait for downstream demand. Cancellation
/// propagates and the input lock is released when the pipe finishes.
pub fn identity_proxy(value: &JsValue, context: &mut Context) -> JsResult<JsObject> {
    let source = state(value)?;
    let reader = acquire(&source, context)?;
    source.borrow_mut().disturbed = true;
    let output = make_state(0.0);
    attach_controller(&output, context)?;
    let proxy = Gc::new(GcRefCell::new(Proxy {
        reader: reader.clone(),
        output: output.clone(),
        buffered: None,
        demand: None,
        reading: false,
        ended: false,
    }));
    let pull = NativeFunction::from_copy_closure_with_captures(
        |_, _, proxy, c| {
            let (promise, settle) = JsPromise::new_pending(c);
            proxy.borrow_mut().demand = Some(settle);
            deliver(proxy, c)?;
            pump(proxy, c)?;
            Ok(promise.into())
        },
        proxy.clone(),
    )
    .to_js_function(context.realm());
    let cancel = NativeFunction::from_copy_closure_with_captures(
        |_, args, proxy, c| {
            let reader = proxy.borrow().reader.clone();
            let source = reader.borrow().stream.clone();
            let cancellation = if let Some(source) = source {
                cancel_state(&source, args.get_or_undefined(0).clone(), c)?
            } else {
                settled(JsValue::undefined(), c)?
            };
            {
                let mut p = proxy.borrow_mut();
                p.ended = true;
                p.buffered = None;
            }
            let cleanup = NativeFunction::from_copy_closure_with_captures(
                |_, _, proxy, c| {
                    let (reader, demand) = {
                        let mut p = proxy.borrow_mut();
                        (p.reader.clone(), p.demand.take())
                    };
                    if let Some(demand) = demand {
                        resolve(&demand, JsValue::undefined(), c)?;
                    }
                    release(&reader, c)?;
                    Ok(JsValue::undefined())
                },
                proxy.clone(),
            )
            .to_js_function(c.realm());
            let fail = NativeFunction::from_copy_closure_with_captures(
                |_, args, proxy, c| {
                    let (reader, demand) = {
                        let mut p = proxy.borrow_mut();
                        (p.reader.clone(), p.demand.take())
                    };
                    if let Some(demand) = demand {
                        resolve(&demand, JsValue::undefined(), c)?;
                    }
                    release(&reader, c)?;
                    Err(JsError::from_opaque(args.get_or_undefined(0).clone()))
                },
                proxy.clone(),
            )
            .to_js_function(c.realm());
            Ok(cancellation
                .then_intrinsic(Some(cleanup), Some(fail), c)?
                .into())
        },
        proxy.clone(),
    )
    .to_js_function(context.realm());
    {
        let mut data = output.borrow_mut();
        data.pull = Some(pull.into());
        data.cancel = Some(cancel.into());
    }
    let closed = reader.borrow().closed.clone();
    let failure = NativeFunction::from_copy_closure_with_captures(
        |_, args, proxy, c| {
            finish(proxy, Some(args.get_or_undefined(0).clone()), c)?;
            Ok(JsValue::undefined())
        },
        proxy.clone(),
    )
    .to_js_function(context.realm());
    drop(closed.then_intrinsic(None, Some(failure), context)?);
    start(&output, JsValue::undefined(), context)?;
    pump(&proxy, context)?;
    ReadableStream::from_data(ReadableStream { state: output }, context)
}
