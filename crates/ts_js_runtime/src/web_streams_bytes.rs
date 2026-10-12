//! Incremental byte-source adapter and an explicitly bounded consuming operation.
//! The source adapter is a byte stream with zero high-water mark and supports
//! both default and BYOB readers without reading ahead.
use super::*;
#[derive(Trace, Finalize)]
struct Bytes {
    bytes: Vec<u8>,
    offset: usize,
    controller: State,
}
/// Create a byte stream of fresh Uint8Array chunks (64 KiB), pulled on demand.
/// The caller transfers its native byte snapshot; no application source is read.
pub fn from_bytes(bytes: Vec<u8>, context: &mut Context) -> JsResult<JsObject> {
    if bytes.len() > MAX_BYTES {
        return Err(limit());
    }
    let state = make_state(0.0);
    byob::initialize(&state, None);
    attach_controller(&state, context)?;
    let bytes = Gc::new(GcRefCell::new(Bytes {
        bytes,
        offset: 0,
        controller: state.clone(),
    }));
    let pull = NativeFunction::from_copy_closure_with_captures(
        |_, _, bytes, context| {
            let (chunk, done, controller) = {
                let mut data = bytes.borrow_mut();
                let end = data.offset.saturating_add(64 * 1024).min(data.bytes.len());
                let chunk = data.bytes[data.offset..end].to_vec();
                data.offset = end;
                let done = end == data.bytes.len();
                if done {
                    data.bytes.clear();
                    data.offset = 0;
                }
                (chunk, done, data.controller.clone())
            };
            if !chunk.is_empty() {
                let value = JsUint8Array::from_iter(chunk, context)?.into();
                enqueue(&controller, value, context)?;
            }
            if done && can_enqueue(&controller) {
                byob::close_and_respond(&controller, context)?;
            }
            Ok(JsValue::undefined())
        },
        bytes.clone(),
    )
    .to_js_function(context.realm());
    let cancel = NativeFunction::from_copy_closure_with_captures(
        |_, _, bytes, _| {
            let mut data = bytes.borrow_mut();
            data.bytes.clear();
            data.offset = 0;
            Ok(JsValue::undefined())
        },
        bytes,
    )
    .to_js_function(context.realm());
    {
        let mut data = state.borrow_mut();
        data.pull = Some(pull.into());
        data.cancel = Some(cancel.into());
    }
    start(&state, JsValue::undefined(), context)?;
    ReadableStream::from_data(ReadableStream { state }, context)
}
/// Copy a real Uint8Array chunk using internal buffer/view slots. Reject shared
/// or detached buffers explicitly; no array-like/property-coercion substitution.
pub fn copy_uint8_chunk(value: &JsValue, context: &mut Context) -> JsResult<Vec<u8>> {
    let view = JsUint8Array::from_object(obj(value)?)?;
    let buffer = JsArrayBuffer::from_object(obj(&view.buffer(context)?)?)?;
    let offset = view.byte_offset(context)?;
    let length = view.byte_length(context)?;
    if length > MAX_BYTES {
        return Err(limit());
    }
    let bytes = buffer
        .data()
        .ok_or_else(|| typ("Detached stream chunk buffer"))?;
    Ok(bytes
        .get(offset..offset.checked_add(length).ok_or_else(limit)?)
        .ok_or_else(|| typ("Invalid stream chunk range"))?
        .to_vec())
}
#[derive(Trace, Finalize)]
struct Consumer {
    reader: ReaderState,
    bytes: Vec<u8>,
    chunks: usize,
    settle: ResolvingFunctions,
    cleanup: bool,
}
type ConsumerState = Gc<GcRefCell<Consumer>>;
fn fail(state: &ConsumerState, reason: JsValue, context: &mut Context) -> JsResult<()> {
    let (reader, settle) = {
        let mut data = state.borrow_mut();
        data.bytes.clear();
        (data.reader.clone(), data.settle.clone())
    };
    if !state.borrow().cleanup {
        return reject(&settle, reason, context);
    }
    // This bounded host helper stops its producer on type/allocation failure.
    let stream = reader.borrow().stream.clone();
    if let Some(stream) = stream {
        let cancellation = cancel_state(&stream, reason.clone(), context)?;
        handled(&cancellation, context)?;
    }
    release(&reader, context)?;
    reject(&settle, reason, context)
}
fn pump(state: &ConsumerState, context: &mut Context) -> JsResult<()> {
    let reader = state.borrow().reader.clone();
    let promise = read_state(&reader, context)?;
    let success = NativeFunction::from_copy_closure_with_captures(
        |_, args, state, context| {
            let result = (|| {
                let result = obj(args.get_or_undefined(0))?;
                if result.get(JsString::from("done"), context)?.to_boolean() {
                    let (bytes, reader, settle) = {
                        let mut d = state.borrow_mut();
                        (
                            std::mem::take(&mut d.bytes),
                            d.reader.clone(),
                            d.settle.clone(),
                        )
                    };
                    if state.borrow().cleanup {
                        release(&reader, context)?;
                    }
                    let value = JsUint8Array::from_iter(bytes, context)?.into();
                    resolve(&settle, value, context)?;
                } else {
                    let value = result.get(JsString::from("value"), context)?;
                    let bytes = copy_uint8_chunk(&value, context)?;
                    {
                        let mut d = state.borrow_mut();
                        if d.chunks >= MAX_QUEUE || bytes.len() > MAX_BYTES - d.bytes.len() {
                            return Err(limit());
                        }
                        d.chunks += 1;
                        d.bytes.extend_from_slice(&bytes);
                    }
                    pump(state, context)?;
                }
                Ok(JsValue::undefined())
            })();
            if let Err(error) = result {
                fail(state, error.into_opaque(context)?, context)?;
            }
            Ok(JsValue::undefined())
        },
        state.clone(),
    )
    .to_js_function(context.realm());
    let failure = NativeFunction::from_copy_closure_with_captures(
        |_, args, state, context| {
            fail(state, args.get_or_undefined(0).clone(), context)?;
            Ok(JsValue::undefined())
        },
        state.clone(),
    )
    .to_js_function(context.realm());
    drop(promise.then_intrinsic(Some(success), Some(failure), context)?);
    Ok(())
}
/// Incrementally consume Uint8Array chunks, returning a Promise<Uint8Array>.
/// This is an explicit collecting operation, capped at 64 MiB and 10,000 chunks;
/// it releases its lock on completion and cancels its producer on failure.
pub fn consume_bytes(value: &JsValue, context: &mut Context) -> JsResult<JsPromise> {
    collect(value, true, context)
}
/// Fetch/Streams read-all-bytes: leaves its acquired reader locked on success
/// and failure, and does not cancel a producer when a chunk is invalid. The
/// promise resolves to an actual Uint8Array; collection bounds remain explicit.
pub fn fully_read_bytes(value: &JsValue, context: &mut Context) -> JsResult<JsPromise> {
    collect(value, false, context)
}
fn collect(value: &JsValue, cleanup: bool, context: &mut Context) -> JsResult<JsPromise> {
    let result = (|| {
        let reader = acquire(&state(value)?, context)?;
        let (promise, settle) = JsPromise::new_pending(context);
        let state = Gc::new(GcRefCell::new(Consumer {
            reader,
            bytes: vec![],
            chunks: 0,
            settle,
            cleanup,
        }));
        pump(&state, context)?;
        Ok(promise)
    })();
    match result {
        Ok(p) => Ok(p),
        Err(error) => rejected(error, context),
    }
}
