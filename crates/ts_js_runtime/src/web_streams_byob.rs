//! Native byte-controller and pull-into algorithms, following WHATWG Streams
//! §§4.5, 4.7–4.9.5 (2026-08-18). Buffers are transferred, not merely copied
//! while retaining caller ownership. Source callbacks never run under a borrow.
use super::*;
use boa_engine::{
    builtins::typed_array::TypedArrayKind,
    object::builtins::{JsDataView, JsTypedArray},
};

#[derive(Clone, Copy, PartialEq, Eq, Trace, Finalize)]
#[boa_gc(empty_trace)]
enum ReadKind {
    Default,
    Byob,
    None,
}
#[derive(Trace, Finalize)]
struct PullInto {
    buffer: JsArrayBuffer,
    buffer_length: usize,
    offset: usize,
    length: usize,
    filled: usize,
    minimum: usize,
    element: usize,
    constructor: JsObject,
    kind: ReadKind,
}
#[derive(Trace, Finalize)]
pub(super) struct ByteData {
    pending: VecDeque<PullInto>,
    request: Option<JsObject>,
    auto: Option<usize>,
}
#[derive(Trace, Finalize, JsData)]
struct ByteController {
    state: State,
}
#[derive(Trace, Finalize, JsData)]
struct ByobReader {
    state: ReaderState,
}
#[derive(Trace, Finalize)]
struct RequestData {
    state: Option<State>,
    view: JsValue,
}
#[derive(Trace, Finalize, JsData)]
struct ByobRequest {
    data: Gc<GcRefCell<RequestData>>,
}

fn range(message: &str) -> JsError {
    JsNativeError::range()
        .with_message(message.to_owned())
        .into()
}
fn byte_controller(value: &JsValue) -> JsResult<State> {
    obj(value)?
        .downcast_ref::<ByteController>()
        .map(|v| v.state.clone())
        .ok_or_else(|| typ("Invalid byte controller receiver"))
}
fn byob_reader(value: &JsValue) -> JsResult<ReaderState> {
    obj(value)?
        .downcast_ref::<ByobReader>()
        .map(|v| v.state.clone())
        .ok_or_else(|| typ("Invalid BYOB reader receiver"))
}
fn request_data(value: &JsValue) -> JsResult<Gc<GcRefCell<RequestData>>> {
    obj(value)?
        .downcast_ref::<ByobRequest>()
        .map(|v| v.data.clone())
        .ok_or_else(|| typ("Invalid BYOB request receiver"))
}
fn transfer(buffer: &JsArrayBuffer, context: &mut Context) -> JsResult<JsArrayBuffer> {
    if buffer.byte_length() > MAX_BYTES {
        return Err(limit());
    }
    // Streams TransferArrayBuffer creates a fixed-length buffer, regardless of
    // the original buffer's resizability, and checks its detach key.
    JsArrayBuffer::from_byte_block(buffer.detach(&JsValue::undefined())?, context)
}
fn view_info(value: &JsValue, context: &mut Context) -> JsResult<PullInto> {
    let object = obj(value)?;
    let (buffer, offset, length, element, constructor) =
        if let Ok(view) = JsTypedArray::from_object(object.clone()) {
            let buffer = JsArrayBuffer::from_object(obj(&view.buffer(context)?)?)?;
            let offset = view.byte_offset(context)?;
            let length = view.byte_length(context)?;
            let c = context.intrinsics().constructors();
            let (size, ctor) = match view.kind().ok_or_else(|| typ("Invalid typed array"))? {
                TypedArrayKind::Int8 => (1, c.typed_int8_array()),
                TypedArrayKind::Uint8 => (1, c.typed_uint8_array()),
                TypedArrayKind::Uint8Clamped => (1, c.typed_uint8clamped_array()),
                TypedArrayKind::Int16 => (2, c.typed_int16_array()),
                TypedArrayKind::Uint16 => (2, c.typed_uint16_array()),
                TypedArrayKind::Int32 => (4, c.typed_int32_array()),
                TypedArrayKind::Uint32 => (4, c.typed_uint32_array()),
                TypedArrayKind::BigInt64 => (8, c.typed_bigint64_array()),
                TypedArrayKind::BigUint64 => (8, c.typed_biguint64_array()),
                TypedArrayKind::Float32 => (4, c.typed_float32_array()),
                TypedArrayKind::Float64 => (8, c.typed_float64_array()),
            };
            (buffer, offset, length, size, ctor.constructor())
        } else {
            let view = JsDataView::from_object(object)?;
            (
                JsArrayBuffer::from_object(obj(&view.buffer(context)?)?)?,
                view.byte_offset(context)? as usize,
                view.byte_length(context)? as usize,
                1,
                context
                    .intrinsics()
                    .constructors()
                    .data_view()
                    .constructor(),
            )
        };
    if buffer.data().is_none() {
        return Err(typ("Detached byte-stream buffer"));
    }
    if buffer.byte_length() > MAX_BYTES {
        return Err(limit());
    }
    let buffer_length = buffer.byte_length();
    Ok(PullInto {
        buffer,
        buffer_length,
        offset,
        length,
        filled: 0,
        minimum: element,
        element,
        constructor,
        kind: ReadKind::Byob,
    })
}
fn make_view(
    buffer: JsArrayBuffer,
    offset: usize,
    length: usize,
    ctor: &JsObject,
    context: &mut Context,
) -> JsResult<JsValue> {
    Ok(ctor
        .construct(
            &[
                buffer.into(),
                (offset as f64).into(),
                (length as f64).into(),
            ],
            None,
            context,
        )?
        .into())
}
fn uint8_view(
    buffer: JsArrayBuffer,
    offset: usize,
    length: usize,
    context: &mut Context,
) -> JsResult<JsValue> {
    let ctor = context
        .intrinsics()
        .constructors()
        .typed_uint8_array()
        .constructor();
    make_view(buffer, offset, length, &ctor, context)
}
pub(super) fn initialize(state: &State, auto: Option<usize>) {
    state.borrow_mut().bytes = Some(ByteData {
        pending: VecDeque::new(),
        request: None,
        auto,
    });
}
pub(super) fn attach(state: &State, context: &mut Context) -> JsResult<()> {
    state.borrow_mut().controller = Some(ByteController::from_data(
        ByteController {
            state: state.clone(),
        },
        context,
    )?);
    Ok(())
}
fn invalidate(state: &State) {
    let request = state
        .borrow_mut()
        .bytes
        .as_mut()
        .and_then(|b| b.request.take());
    if let Some(request) = request {
        let request = request.downcast_ref::<ByobRequest>().unwrap();
        let mut data = request.data.borrow_mut();
        data.state = None;
        data.view = JsValue::null();
    }
}
pub(super) fn clear_pending(state: &State) {
    invalidate(state);
    if let Some(b) = state.borrow_mut().bytes.as_mut() {
        b.pending.clear();
    }
}
pub(super) fn release_pending(state: &State) {
    if let Some(b) = state.borrow_mut().bytes.as_mut() {
        if let Some(first) = b.pending.front_mut() {
            first.kind = ReadKind::None;
        }
        b.pending.truncate(1);
    }
}
pub(super) fn cancel_pending(state: &State, context: &mut Context) -> JsResult<()> {
    clear_pending(state);
    let reader = state.borrow().reader.clone();
    if let Some(reader) = reader
        && reader.borrow().byob
    {
        let requests = std::mem::take(&mut reader.borrow_mut().requests);
        for request in requests {
            let result = create_iter_result_object(JsValue::undefined(), true, context);
            resolve(&request, result, context)?;
        }
    }
    Ok(())
}
pub(super) fn close(state: &State, context: &mut Context) -> JsResult<()> {
    if !can_enqueue(state) {
        return Err(typ("Cannot close this byte stream"));
    }
    if state.borrow().total > 0.0 {
        state.borrow_mut().close_requested = true;
        return Ok(());
    }
    let incomplete = state
        .borrow()
        .bytes
        .as_ref()
        .unwrap()
        .pending
        .front()
        .is_some_and(|p| p.filled % p.element != 0);
    if incomplete {
        let error = typ("Byte stream closed with an incomplete typed-array element");
        error_state(state, error.clone().into_opaque(context)?, context)?;
        return Err(error);
    }
    clear(&mut state.borrow_mut());
    close_state(state, context)
}
/// Host/spec close additionally answers outstanding BYOB reads; the public
/// controller.close() alone intentionally leaves them awaiting respond(0).
pub(super) fn close_and_respond(state: &State, context: &mut Context) -> JsResult<()> {
    if can_enqueue(state) {
        close(state, context)?;
    }
    let pending = state
        .borrow()
        .bytes
        .as_ref()
        .is_some_and(|b| !b.pending.is_empty());
    if state.borrow().status == Status::Closed && pending {
        respond(state, 0, None, context)?;
    }
    Ok(())
}
fn drain(state: &State, context: &mut Context) -> JsResult<()> {
    if state.borrow().total == 0.0 && state.borrow().close_requested {
        // close()'s guard is public; the internal queue-drain operation closes.
        state.borrow_mut().close_requested = false;
        close(state, context)
    } else {
        pull_if_needed(state, context)
    }
}
fn queue(state: &State, value: JsValue, length: usize, front: bool) -> JsResult<()> {
    if length == 0 {
        return Ok(());
    }
    let mut d = state.borrow_mut();
    if d.queue.len() >= MAX_QUEUE || length as f64 > MAX_BYTES as f64 - d.total {
        return Err(limit());
    }
    d.total += length as f64;
    let chunk = Chunk {
        value,
        size: length as f64,
    };
    if front {
        d.queue.push_front(chunk);
    } else {
        d.queue.push_back(chunk);
    }
    Ok(())
}
fn fulfill(state: &State, descriptor: PullInto, done: bool, context: &mut Context) -> JsResult<()> {
    if descriptor.kind == ReadKind::None {
        return Ok(());
    }
    let request = state
        .borrow()
        .reader
        .clone()
        .and_then(|r| r.borrow_mut().requests.pop_front());
    if let Some(request) = request {
        let buffer = transfer(&descriptor.buffer, context)?;
        let value = make_view(
            buffer,
            descriptor.offset,
            descriptor.filled / descriptor.element,
            &descriptor.constructor,
            context,
        )?;
        let result = create_iter_result_object(value, done, context);
        resolve(&request, result, context)?;
    }
    Ok(())
}
// Fill as many complete elements as possible once minimumFill can be reached.
// An unaligned remainder stays in the byte queue for the next descriptor.
fn fill(state: &State, descriptor: &mut PullInto, context: &mut Context) -> JsResult<bool> {
    let max = (state.borrow().total as usize).min(descriptor.length - descriptor.filled);
    let aligned = (descriptor.filled + max) / descriptor.element * descriptor.element;
    let ready = aligned >= descriptor.minimum;
    let mut remaining = if ready {
        aligned - descriptor.filled
    } else {
        max
    };
    while remaining > 0 {
        let chunk = state
            .borrow_mut()
            .queue
            .pop_front()
            .ok_or_else(|| typ("Invalid byte queue"))?;
        let view = view_info(&chunk.value, context)?;
        let count = remaining.min(view.length);
        {
            let source = view
                .buffer
                .data()
                .ok_or_else(|| typ("Detached queued byte buffer"))?;
            let mut target = descriptor
                .buffer
                .data_mut()
                .ok_or_else(|| typ("Detached BYOB buffer"))?;
            let at = descriptor.offset + descriptor.filled;
            target[at..at + count].copy_from_slice(&source[view.offset..view.offset + count]);
        }
        descriptor.filled += count;
        remaining -= count;
        state.borrow_mut().total -= count as f64;
        if count < view.length {
            let value = uint8_view(
                view.buffer.clone(),
                view.offset + count,
                view.length - count,
                context,
            )?;
            state.borrow_mut().queue.push_front(Chunk {
                value,
                size: (view.length - count) as f64,
            });
        }
    }
    Ok(ready)
}
fn process(state: &State, context: &mut Context) -> JsResult<()> {
    loop {
        let reader = state.borrow().reader.clone();
        let Some(reader) = reader else { break };
        if reader.borrow().requests.is_empty() || state.borrow().queue.is_empty() {
            break;
        }
        if !reader.borrow().byob {
            let auto_descriptor = state
                .borrow()
                .bytes
                .as_ref()
                .unwrap()
                .pending
                .front()
                .is_some_and(|p| p.kind == ReadKind::Default);
            if auto_descriptor {
                invalidate(state);
                state
                    .borrow_mut()
                    .bytes
                    .as_mut()
                    .unwrap()
                    .pending
                    .pop_front();
            }
            let chunk = state.borrow_mut().queue.pop_front().unwrap();
            state.borrow_mut().total -= chunk.size;
            let request = reader.borrow_mut().requests.pop_front().unwrap();
            let result = create_iter_result_object(chunk.value.clone(), false, context);
            resolve(&request, result, context)?;
        } else {
            let descriptor = state
                .borrow_mut()
                .bytes
                .as_mut()
                .unwrap()
                .pending
                .pop_front();
            let Some(mut descriptor) = descriptor else {
                break;
            };
            if descriptor.kind == ReadKind::None {
                state
                    .borrow_mut()
                    .bytes
                    .as_mut()
                    .unwrap()
                    .pending
                    .push_front(descriptor);
                break;
            }
            let ready = fill(state, &mut descriptor, context)?;
            if ready {
                invalidate(state);
                fulfill(state, descriptor, false, context)?;
            } else {
                state
                    .borrow_mut()
                    .bytes
                    .as_mut()
                    .unwrap()
                    .pending
                    .push_front(descriptor);
                break;
            }
        }
    }
    Ok(())
}
fn abandon(state: &State, descriptor: PullInto, context: &mut Context) -> JsResult<()> {
    if descriptor.filled > 0 {
        let bytes = {
            let data = descriptor
                .buffer
                .data()
                .ok_or_else(|| typ("Detached abandoned BYOB buffer"))?;
            data[descriptor.offset..descriptor.offset + descriptor.filled].to_vec()
        };
        let value = JsUint8Array::from_iter(bytes, context)?.into();
        queue(state, value, descriptor.filled, false)?;
    }
    Ok(())
}
pub(super) fn enqueue(state: &State, value: &JsValue, context: &mut Context) -> JsResult<()> {
    let view = view_info(value, context)?;
    if view.length == 0 {
        return Err(typ("Cannot enqueue an empty byte view"));
    }
    if !can_enqueue(state) {
        return Err(typ("Cannot enqueue into this byte stream"));
    }
    if view.length as f64 > MAX_BYTES as f64 - state.borrow().total {
        return Err(limit());
    }
    let buffer = transfer(&view.buffer, context)?;
    let first = state
        .borrow_mut()
        .bytes
        .as_mut()
        .unwrap()
        .pending
        .pop_front();
    if let Some(mut first) = first {
        match transfer(&first.buffer, context) {
            Ok(buffer) => first.buffer = buffer,
            Err(error) => {
                state
                    .borrow_mut()
                    .bytes
                    .as_mut()
                    .unwrap()
                    .pending
                    .push_front(first);
                return Err(error);
            }
        }
        invalidate(state);
        if first.kind == ReadKind::None {
            abandon(state, first, context)?;
        } else {
            state
                .borrow_mut()
                .bytes
                .as_mut()
                .unwrap()
                .pending
                .push_front(first);
        }
    }
    process(state, context)?;
    let reader = state.borrow().reader.clone();
    let default = reader
        .as_ref()
        .is_some_and(|r| !r.borrow().byob && !r.borrow().requests.is_empty());
    let value = uint8_view(buffer, view.offset, view.length, context)?;
    if default {
        state
            .borrow_mut()
            .bytes
            .as_mut()
            .unwrap()
            .pending
            .pop_front();
        let request = reader.unwrap().borrow_mut().requests.pop_front().unwrap();
        let result = create_iter_result_object(value, false, context);
        resolve(&request, result, context)?;
    } else {
        queue(state, value, view.length, false)?;
        process(state, context)?;
    }
    pull_if_needed(state, context)
}
pub(super) fn read_default(
    state: &State,
    reader: &ReaderState,
    request: ResolvingFunctions,
    context: &mut Context,
) -> JsResult<()> {
    if reader.borrow().requests.len() >= MAX_QUEUE {
        return Err(limit());
    }
    reader.borrow_mut().requests.push_back(request);
    if !state.borrow().queue.is_empty() {
        process(state, context)?;
        return drain(state, context);
    }
    let auto = state.borrow().bytes.as_ref().unwrap().auto;
    if let Some(length) = auto {
        let buffer = JsArrayBuffer::new(length, context)?;
        let constructor = context
            .intrinsics()
            .constructors()
            .typed_uint8_array()
            .constructor();
        state
            .borrow_mut()
            .bytes
            .as_mut()
            .unwrap()
            .pending
            .push_back(PullInto {
                buffer,
                buffer_length: length,
                offset: 0,
                length,
                filled: 0,
                minimum: 1,
                element: 1,
                constructor,
                kind: ReadKind::Default,
            });
    }
    pull_if_needed(state, context)
}
pub(super) fn get_reader(state: &State, context: &mut Context) -> JsResult<JsObject> {
    if state.borrow().bytes.is_none() {
        return Err(typ("BYOB reader requires a byte stream"));
    }
    let reader = acquire(state, context)?;
    reader.borrow_mut().byob = true;
    ByobReader::from_data(ByobReader { state: reader }, context)
}
/// Acquire the actual native BYOB reader; fails for default streams or locks.
pub fn get_byob_reader(value: &JsValue, context: &mut Context) -> JsResult<JsObject> {
    get_reader(&state(value)?, context)
}
/// Read at least `min` elements (bytes for DataView), transferring the view's
/// entire backing ArrayBuffer synchronously. Fulfillment uses its intrinsic type.
pub fn read_into(
    value: &JsValue,
    view: &JsValue,
    min: usize,
    context: &mut Context,
) -> JsResult<JsPromise> {
    let result = (|| {
        let reader = byob_reader(value)?;
        let mut descriptor = view_info(view, context)?;
        if descriptor.length == 0 {
            return Err(typ("BYOB read requires a nonempty view"));
        }
        if min == 0 || min > descriptor.length / descriptor.element {
            return Err(range("Invalid BYOB minimum"));
        }
        let state = reader
            .borrow()
            .stream
            .clone()
            .ok_or_else(|| typ("Reader lock was released"))?;
        state.borrow_mut().disturbed = true;
        if state.borrow().status == Status::Errored {
            return rejected(JsError::from_opaque(state.borrow().error.clone()), context);
        }
        if reader.borrow().requests.len() >= MAX_QUEUE {
            return Err(limit());
        }
        descriptor.minimum = min * descriptor.element;
        descriptor.buffer = transfer(&descriptor.buffer, context)?;
        let (promise, settle) = JsPromise::new_pending(context);
        if state.borrow().bytes.as_ref().unwrap().pending.is_empty()
            && state.borrow().status == Status::Closed
        {
            let value = make_view(
                descriptor.buffer.clone(),
                descriptor.offset,
                0,
                &descriptor.constructor,
                context,
            )?;
            let result = create_iter_result_object(value, true, context);
            resolve(&settle, result, context)?;
            return Ok(promise);
        }
        reader.borrow_mut().requests.push_back(settle);
        state
            .borrow_mut()
            .bytes
            .as_mut()
            .unwrap()
            .pending
            .push_back(descriptor);
        process(&state, context)?;
        let incomplete = state.borrow().close_requested
            && state.borrow().total == 0.0
            && !state.borrow().bytes.as_ref().unwrap().pending.is_empty();
        if incomplete {
            let error =
                typ("Insufficient bytes for BYOB minimum at end of stream").into_opaque(context)?;
            error_state(&state, error, context)?;
        } else {
            drain(&state, context)?;
        }
        Ok(promise)
    })();
    match result {
        Ok(p) => Ok(p),
        Err(e) => rejected(e, context),
    }
}
pub(super) fn byob_request(state: &State, context: &mut Context) -> JsResult<JsValue> {
    if let Some(request) = state.borrow().bytes.as_ref().unwrap().request.clone() {
        return Ok(request.into());
    }
    let info = {
        let d = state.borrow();
        d.bytes
            .as_ref()
            .unwrap()
            .pending
            .front()
            .map(|p| (p.buffer.clone(), p.offset + p.filled, p.length - p.filled))
    };
    let Some((buffer, offset, length)) = info else {
        return Ok(JsValue::null());
    };
    let view = uint8_view(buffer, offset, length, context)?;
    let request = ByobRequest::from_data(
        ByobRequest {
            data: Gc::new(GcRefCell::new(RequestData {
                state: Some(state.clone()),
                view,
            })),
        },
        context,
    )?;
    state.borrow_mut().bytes.as_mut().unwrap().request = Some(request.clone());
    Ok(request.into())
}
fn respond(
    state: &State,
    written: usize,
    replacement: Option<PullInto>,
    context: &mut Context,
) -> JsResult<()> {
    let status = state.borrow().status;
    {
        let data = state.borrow();
        let first = data
            .bytes
            .as_ref()
            .unwrap()
            .pending
            .front()
            .ok_or_else(|| typ("No outstanding BYOB request"))?;
        if status == Status::Closed {
            if written != 0 {
                return Err(typ("Closed BYOB request requires zero bytes"));
            }
        } else if status != Status::Readable || written == 0 {
            return Err(typ("Readable BYOB request requires positive bytes"));
        }
        if written > first.length - first.filled {
            return Err(range("BYOB response exceeds view capacity"));
        }
        if let Some(view) = &replacement {
            if view.buffer_length != first.buffer_length
                || view.offset != first.offset + first.filled
            {
                return Err(range(
                    "Replacement BYOB view has different backing size or offset",
                ));
            }
        } else if first.buffer.data().is_none() {
            return Err(typ("Detached BYOB request buffer"));
        }
    }
    let mut first = state
        .borrow_mut()
        .bytes
        .as_mut()
        .unwrap()
        .pending
        .pop_front()
        .unwrap();
    let buffer = if let Some(view) = replacement {
        view.buffer.clone()
    } else {
        first.buffer.clone()
    };
    match transfer(&buffer, context) {
        Ok(buffer) => first.buffer = buffer,
        Err(error) => {
            state
                .borrow_mut()
                .bytes
                .as_mut()
                .unwrap()
                .pending
                .push_front(first);
            return Err(error);
        }
    }
    invalidate(state);
    if status == Status::Closed {
        if first.kind != ReadKind::None {
            state
                .borrow_mut()
                .bytes
                .as_mut()
                .unwrap()
                .pending
                .push_front(first);
        }
        let is_byob = state
            .borrow()
            .reader
            .as_ref()
            .is_some_and(|r| r.borrow().byob);
        if is_byob {
            let pending = std::mem::take(&mut state.borrow_mut().bytes.as_mut().unwrap().pending);
            for descriptor in pending {
                fulfill(state, descriptor, true, context)?;
            }
        }
    } else {
        first.filled += written;
        if first.kind == ReadKind::None {
            abandon(state, first, context)?;
        } else if first.filled >= first.minimum {
            let remainder = first.filled % first.element;
            if remainder > 0 {
                let bytes = {
                    let data = first.buffer.data().unwrap();
                    let end = first.offset + first.filled;
                    data[end - remainder..end].to_vec()
                };
                let value = JsUint8Array::from_iter(bytes, context)?.into();
                queue(state, value, remainder, false)?;
                first.filled -= remainder;
            }
            fulfill(state, first, false, context)?;
        } else {
            state
                .borrow_mut()
                .bytes
                .as_mut()
                .unwrap()
                .pending
                .push_front(first);
        }
        process(state, context)?;
    }
    pull_if_needed(state, context)
}
/// Create a host-driven byte stream with HWM 0. The same controller wrapper is
/// usable for enqueue/close/error; enqueue requires and transfers a BufferView.
pub fn new_readable_bytes(context: &mut Context) -> JsResult<(JsObject, ReadableController)> {
    let state = make_state(0.0);
    initialize(&state, None);
    attach(&state, context)?;
    start(&state, JsValue::undefined(), context)?;
    let object = ReadableStream::from_data(
        ReadableStream {
            state: state.clone(),
        },
        context,
    )?;
    Ok((object, ReadableController { state }))
}
fn unsigned(value: &JsValue, context: &mut Context) -> JsResult<usize> {
    let n = value.to_number(context)?;
    if !n.is_finite() || n.trunc() < 0.0 || n.trunc() > 9_007_199_254_740_991.0 {
        return Err(typ("Invalid unsigned stream count"));
    }
    if n.trunc() > usize::MAX as f64 {
        return Err(limit());
    }
    Ok(n.trunc() as usize)
}
impl Class for ByteController {
    const NAME: &'static str = "ReadableByteStreamController";
    const ATTRIBUTES: Attribute = Attribute::WRITABLE.union(Attribute::CONFIGURABLE);
    fn data_constructor(_: &JsValue, _: &[JsValue], _: &mut Context) -> JsResult<Self> {
        Err(typ("Illegal byte controller constructor"))
    }
    fn init(class: &mut ClassBuilder) -> JsResult<()> {
        for (name, getter) in [
            (
                "desiredSize",
                NativeFunction::from_fn_ptr(|this, _, _| Ok(desired_size(&byte_controller(this)?))),
            ),
            (
                "byobRequest",
                NativeFunction::from_fn_ptr(|this, _, c| byob_request(&byte_controller(this)?, c)),
            ),
        ] {
            let getter = FunctionObjectBuilder::new(class.context().realm(), getter).build();
            class.accessor(
                JsString::from(name),
                Some(getter),
                None,
                Attribute::ENUMERABLE.union(Attribute::CONFIGURABLE),
            );
        }
        class.method(
            JsString::from("close"),
            0,
            NativeFunction::from_fn_ptr(|this, _, c| {
                close(&byte_controller(this)?, c)?;
                Ok(JsValue::undefined())
            }),
        );
        class.method(
            JsString::from("enqueue"),
            1,
            NativeFunction::from_fn_ptr(|this, args, c| {
                enqueue(&byte_controller(this)?, args.get_or_undefined(0), c)?;
                Ok(JsValue::undefined())
            }),
        );
        class.method(
            JsString::from("error"),
            0,
            NativeFunction::from_fn_ptr(|this, args, c| {
                error_state(&byte_controller(this)?, args.get_or_undefined(0).clone(), c)?;
                Ok(JsValue::undefined())
            }),
        );
        class.property(
            JsSymbol::to_string_tag(),
            JsString::from(Self::NAME),
            Attribute::CONFIGURABLE,
        );
        Ok(())
    }
}
impl Class for ByobReader {
    const NAME: &'static str = "ReadableStreamBYOBReader";
    const LENGTH: usize = 1;
    const ATTRIBUTES: Attribute = Attribute::WRITABLE.union(Attribute::CONFIGURABLE);
    fn data_constructor(_: &JsValue, args: &[JsValue], c: &mut Context) -> JsResult<Self> {
        let state = state(args.get_or_undefined(0))?;
        if state.borrow().bytes.is_none() {
            return Err(typ("BYOB reader requires a byte stream"));
        }
        let reader = acquire(&state, c)?;
        reader.borrow_mut().byob = true;
        Ok(Self { state: reader })
    }
    fn init(class: &mut ClassBuilder) -> JsResult<()> {
        let getter = FunctionObjectBuilder::new(
            class.context().realm(),
            NativeFunction::from_fn_ptr(|this, _, c| {
                let result = byob_reader(this).map(|r| r.borrow().closed.clone());
                promise_method(result, c)
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
            1,
            NativeFunction::from_fn_ptr(|this, args, c| {
                let result = (|| {
                    let options = dictionary(args.get_or_undefined(1))?;
                    let min = field(&options, "min", c)?;
                    let min = if min.is_undefined() {
                        1
                    } else {
                        unsigned(&min, c)?
                    };
                    read_into(this, args.get_or_undefined(0), min, c)
                })();
                promise_method(result, c)
            }),
        );
        class.method(
            JsString::from("releaseLock"),
            0,
            NativeFunction::from_fn_ptr(|this, _, c| {
                release(&byob_reader(this)?, c)?;
                Ok(JsValue::undefined())
            }),
        );
        class.method(
            JsString::from("cancel"),
            0,
            NativeFunction::from_fn_ptr(|this, args, c| {
                let result = (|| {
                    let r = byob_reader(this)?;
                    let state = r
                        .borrow()
                        .stream
                        .clone()
                        .ok_or_else(|| typ("Reader lock was released"))?;
                    cancel_state(&state, args.get_or_undefined(0).clone(), c)
                })();
                promise_method(result, c)
            }),
        );
        class.property(
            JsSymbol::to_string_tag(),
            JsString::from(Self::NAME),
            Attribute::CONFIGURABLE,
        );
        Ok(())
    }
}
impl Class for ByobRequest {
    const NAME: &'static str = "ReadableStreamBYOBRequest";
    const ATTRIBUTES: Attribute = Attribute::WRITABLE.union(Attribute::CONFIGURABLE);
    fn data_constructor(_: &JsValue, _: &[JsValue], _: &mut Context) -> JsResult<Self> {
        Err(typ("Illegal BYOB request constructor"))
    }
    fn init(class: &mut ClassBuilder) -> JsResult<()> {
        let getter = FunctionObjectBuilder::new(
            class.context().realm(),
            NativeFunction::from_fn_ptr(|this, _, _| Ok(request_data(this)?.borrow().view.clone())),
        )
        .build();
        class.accessor(
            JsString::from("view"),
            Some(getter),
            None,
            Attribute::ENUMERABLE.union(Attribute::CONFIGURABLE),
        );
        class.method(
            JsString::from("respond"),
            1,
            NativeFunction::from_fn_ptr(|this, args, c| {
                let data = request_data(this)?;
                let count = unsigned(args.get_or_undefined(0), c)?;
                let state = data
                    .borrow()
                    .state
                    .clone()
                    .ok_or_else(|| typ("BYOB request is invalidated"))?;
                respond(&state, count, None, c)?;
                Ok(JsValue::undefined())
            }),
        );
        class.method(
            JsString::from("respondWithNewView"),
            1,
            NativeFunction::from_fn_ptr(|this, args, c| {
                let data = request_data(this)?;
                let view = view_info(args.get_or_undefined(0), c)?;
                let state = data
                    .borrow()
                    .state
                    .clone()
                    .ok_or_else(|| typ("BYOB request is invalidated"))?;
                respond(&state, view.length, Some(view), c)?;
                Ok(JsValue::undefined())
            }),
        );
        class.property(
            JsSymbol::to_string_tag(),
            JsString::from(Self::NAME),
            Attribute::CONFIGURABLE,
        );
        Ok(())
    }
}
pub(super) fn register(context: &mut Context) -> JsResult<()> {
    context.register_global_class::<ByteController>()?;
    context.register_global_class::<ByobReader>()?;
    context.register_global_class::<ByobRequest>()?;
    for (prototype, names) in [
        (
            context
                .get_global_class::<ByteController>()
                .unwrap()
                .prototype(),
            &["close", "enqueue", "error"][..],
        ),
        (
            context
                .get_global_class::<ByobReader>()
                .unwrap()
                .prototype(),
            &["read", "cancel", "releaseLock"][..],
        ),
        (
            context
                .get_global_class::<ByobRequest>()
                .unwrap()
                .prototype(),
            &["respond", "respondWithNewView"][..],
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

pub(super) fn request_view(state: &State, context: &mut Context) -> JsResult<Option<JsValue>> {
    let value = byob_request(state, context)?;
    if value.is_null() {
        Ok(None)
    } else {
        Ok(Some(request_data(&value)?.borrow().view.clone()))
    }
}
pub(super) fn read_internal(
    reader: &ReaderState,
    view: &JsValue,
    context: &mut Context,
) -> JsResult<JsPromise> {
    let object = ByobReader::from_data(
        ByobReader {
            state: reader.clone(),
        },
        context,
    )?;
    read_into(&object.into(), view, 1, context)
}
pub(super) fn respond_view(state: &State, view: &JsValue, context: &mut Context) -> JsResult<()> {
    let view = view_info(view, context)?;
    respond(state, view.length, Some(view), context)
}

// Fetch's default-stream tee requests structured cloning. Native BufferSource
// chunks preserve their entire backing buffer, offset and intrinsic view type.
// Other structured object graphs are an explicit unsupported capability; they
// are never shared while claiming a clone. Primitive values clone by value.
pub(super) fn clone_chunk(value: &JsValue, context: &mut Context) -> JsResult<JsValue> {
    let Some(object) = value.as_object() else {
        if value.is_symbol() {
            return Err(typ("Cannot structured-clone a Symbol"));
        }
        return Ok(value.clone());
    };
    if let Ok(buffer) = JsArrayBuffer::from_object(object.clone()) {
        let bytes = buffer
            .to_vec()
            .ok_or_else(|| typ("Cannot clone a detached buffer"))?;
        if bytes.len() > MAX_BYTES {
            return Err(limit());
        }
        let clone = JsArrayBuffer::new(bytes.len(), context)?;
        clone.data_mut().unwrap().copy_from_slice(&bytes);
        return Ok(clone.into());
    }
    if JsTypedArray::from_object(object.clone()).is_err()
        && JsDataView::from_object(object).is_err()
    {
        return Err(unsupported(
            "Structured cloning of non-BufferSource object chunks",
        ));
    }
    let view = view_info(value, context)?;
    let clone = JsArrayBuffer::new(view.buffer_length, context)?;
    {
        let data = view
            .buffer
            .data()
            .ok_or_else(|| typ("Cannot clone a detached buffer"))?;
        clone.data_mut().unwrap().copy_from_slice(&data);
    }
    make_view(
        clone,
        view.offset,
        view.length / view.element,
        &view.constructor,
        context,
    )
}
