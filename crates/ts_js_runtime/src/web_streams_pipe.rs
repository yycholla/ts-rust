//! WHATWG ReadableStreamPipeTo and the pipeTo/pipeThrough WebIDL surface.
//! https://streams.spec.whatwg.org/#readable-stream-pipe-to
//!
//! Native ReadRequest callbacks preserve raw chunks (including thenables). The
//! pump observes native reader/writer state and intrinsic promise reactions; it
//! never calls overridable JS stream methods. Abort hooks are actual DOM abort
//! algorithms, independent of event propagation. Shutdown drains already-read
//! writes before invoking its action whenever the destination remains writable.
use super::*;
use crate::{web_abort, web_writable as writable};
use writable::WritableStatus;

#[derive(Clone, Default, Trace, Finalize)]
pub struct PipeOptions {
    pub prevent_close: bool,
    pub prevent_abort: bool,
    pub prevent_cancel: bool,
    pub signal: Option<JsObject>,
}
#[derive(Clone, Copy)]
enum Action {
    None,
    Close,
    Abort,
    Cancel,
    Signal,
}
type Pipe = Gc<GcRefCell<PipeData>>;
#[derive(Trace, Finalize)]
struct PipeData {
    source: State,
    destination: JsValue,
    reader: ReaderState,
    writer: JsValue,
    options: PipeOptions,
    result: ResolvingFunctions,
    abort_token: u64,
    scheduled: bool,
    reading: bool,
    waiting_ready: bool,
    buffered: Option<JsValue>,
    pending_writes: usize,
    shutting_down: bool,
    ended: bool,
    wait_for_writes: bool,
    action_started: bool,
    pending_actions: usize,
    #[unsafe_ignore_trace]
    action: Action,
    error: Option<JsValue>,
}
fn options(value: &JsValue, c: &mut Context) -> JsResult<PipeOptions> {
    if value.is_null_or_undefined() {
        return Ok(PipeOptions::default());
    }
    let value = obj(value)?;
    // WebIDL dictionary conversion is lexicographic, before lock checks.
    let prevent_abort = value.get(JsString::from("preventAbort"), c)?.to_boolean();
    let prevent_cancel = value.get(JsString::from("preventCancel"), c)?.to_boolean();
    let prevent_close = value.get(JsString::from("preventClose"), c)?.to_boolean();
    let signal = value.get(JsString::from("signal"), c)?;
    let signal = if signal.is_undefined() {
        None
    } else {
        Some(web_abort::validate_signal(&signal)?)
    };
    Ok(PipeOptions {
        prevent_close,
        prevent_abort,
        prevent_cancel,
        signal,
    })
}
pub(super) fn pipe_to_method(
    this: &JsValue,
    args: &[JsValue],
    c: &mut Context,
) -> JsResult<JsValue> {
    let result = (|| {
        state(this)?;
        let destination = args.get_or_undefined(0);
        if !writable::is_writable(destination) {
            return Err(typ("pipeTo destination must be a WritableStream"));
        }
        let options = options(args.get_or_undefined(1), c)?;
        pipe_to(this, destination, options, c)
    })();
    promise_method(result, c)
}
pub(super) fn pipe_through_method(
    this: &JsValue,
    args: &[JsValue],
    c: &mut Context,
) -> JsResult<JsValue> {
    state(this)?;
    let pair = obj(args.get_or_undefined(0))?;
    let readable = pair.get(JsString::from("readable"), c)?;
    state(&readable)?;
    let writable = pair.get(JsString::from("writable"), c)?;
    if !writable::is_writable(&writable) {
        return Err(typ("pipeThrough writable must be a WritableStream"));
    }
    let options = options(args.get_or_undefined(1), c)?;
    let promise = pipe_to(this, &writable, options, c)?;
    handled(&promise, c)?;
    Ok(readable)
}

/// Native PipeTo abstract operation. Argument/lock failures are Rust errors;
/// once both locks are acquired, propagation and shutdown settle the returned
/// promise. The host must pump Boa jobs. No chunk cloning or byte conversion.
pub fn pipe_to(
    source: &JsValue,
    destination: &JsValue,
    options: PipeOptions,
    c: &mut Context,
) -> JsResult<JsPromise> {
    let source = state(source)?;
    if source.borrow().reader.is_some() {
        return Err(typ("ReadableStream is locked"));
    }
    if writable::is_locked(destination)? {
        return Err(typ("WritableStream is locked"));
    }
    if let Some(signal) = &options.signal {
        web_abort::validate_signal(&signal.clone().into())?;
    }
    let reader = acquire(&source, c)?;
    let writer = match writable::acquire_writer(destination, c) {
        Ok(writer) => writer,
        Err(error) => {
            release(&reader, c)?;
            return Err(error);
        }
    };
    source.borrow_mut().disturbed = true;
    let (promise, result) = JsPromise::new_pending(c);
    let pipe = Gc::new(GcRefCell::new(PipeData {
        source,
        destination: destination.clone(),
        reader,
        writer: writer.into(),
        options,
        result,
        abort_token: 0,
        scheduled: false,
        reading: false,
        waiting_ready: false,
        buffered: None,
        pending_writes: 0,
        shutting_down: false,
        ended: false,
        wait_for_writes: false,
        action_started: false,
        pending_actions: 0,
        action: Action::None,
        error: None,
    }));
    let setup = (|| {
        let signal = pipe.borrow().options.signal.clone();
        if let Some(signal) = signal {
            if let Some(reason) = web_abort::abort_reason(&signal.clone().into())? {
                shutdown(&pipe, Action::Signal, Some(reason), c)?;
                return Ok(());
            }
            let callback = NativeFunction::from_copy_closure_with_captures(
                |_, args, pipe, c| {
                    shutdown(
                        pipe,
                        Action::Signal,
                        Some(args.get_or_undefined(0).clone()),
                        c,
                    )?;
                    Ok(JsValue::undefined())
                },
                pipe.clone(),
            )
            .to_js_function(c.realm());
            let token = web_abort::add_abort_algorithm(&signal, callback.into(), c)?;
            pipe.borrow_mut().abort_token = token;
        }
        let closed = pipe.borrow().reader.borrow().closed.clone();
        watch(&pipe, &closed, Watch::State, c)?;
        let writer = pipe.borrow().writer.clone();
        watch(&pipe, &writable::closed(&writer)?, Watch::State, c)?;
        // Initial conditions are checked synchronously, before the first read.
        drive(&pipe, c)
    })();
    if let Err(error) = setup {
        fail(&pipe, error, c)?;
    }
    Ok(promise)
}
#[derive(Clone, Copy)]
enum Watch {
    State,
    Ready,
    Write,
    Action,
}
#[derive(Trace, Finalize)]
struct Reaction {
    pipe: Pipe,
    #[unsafe_ignore_trace]
    kind: Watch,
    rejected: bool,
}
fn watch(pipe: &Pipe, promise: &JsPromise, kind: Watch, c: &mut Context) -> JsResult<()> {
    let callback = |rejected| {
        NativeFunction::from_copy_closure_with_captures(
            |_, args, reaction, c| {
                let pipe = &reaction.pipe;
                if pipe.borrow().ended {
                    return Ok(JsValue::undefined());
                }
                match reaction.kind {
                    Watch::State => {}
                    Watch::Ready => {
                        pipe.borrow_mut().waiting_ready = false;
                    }
                    Watch::Write => {
                        let mut data = pipe.borrow_mut();
                        data.pending_writes = data.pending_writes.saturating_sub(1);
                    }
                    Watch::Action => {
                        if reaction.rejected {
                            finalize(pipe, Some(args.get_or_undefined(0).clone()), c)?;
                            return Ok(JsValue::undefined());
                        }
                        let done = {
                            let mut data = pipe.borrow_mut();
                            data.pending_actions -= 1;
                            data.pending_actions == 0
                        };
                        if done {
                            let error = pipe.borrow().error.clone();
                            finalize(pipe, error, c)?;
                        }
                        return Ok(JsValue::undefined());
                    }
                }
                schedule(pipe, c)?;
                Ok(JsValue::undefined())
            },
            Reaction {
                pipe: pipe.clone(),
                kind,
                rejected,
            },
        )
    };
    let fulfilled = callback(false).to_js_function(c.realm());
    let rejected = callback(true).to_js_function(c.realm());
    promise.then_intrinsic(Some(fulfilled), Some(rejected), c)?;
    Ok(())
}
fn schedule(pipe: &Pipe, c: &mut Context) -> JsResult<()> {
    {
        let mut data = pipe.borrow_mut();
        if data.ended || data.scheduled {
            return Ok(());
        }
        data.scheduled = true;
    }
    let callback = NativeFunction::from_copy_closure_with_captures(
        |_, _, pipe, c| {
            pipe.borrow_mut().scheduled = false;
            if let Err(error) = drive(pipe, c) {
                fail(pipe, error, c)?;
            }
            Ok(JsValue::undefined())
        },
        pipe.clone(),
    )
    .to_js_function(c.realm());
    settled(JsValue::undefined(), c)?.then_intrinsic(Some(callback), None, c)?;
    Ok(())
}
fn fail(pipe: &Pipe, error: JsError, c: &mut Context) -> JsResult<()> {
    let error = error.into_opaque(c)?;
    if pipe.borrow().shutting_down {
        finalize(pipe, Some(error), c)
    } else {
        shutdown(pipe, Action::None, Some(error), c)
    }
}
fn drive(pipe: &Pipe, c: &mut Context) -> JsResult<()> {
    if pipe.borrow().ended {
        return Ok(());
    }
    if pipe.borrow().shutting_down {
        return advance_shutdown(pipe, c);
    }
    let (source, destination, writer, options) = {
        let data = pipe.borrow();
        (
            data.source.clone(),
            data.destination.clone(),
            data.writer.clone(),
            data.options.clone(),
        )
    };
    let (source_status, source_error) = {
        let source = source.borrow();
        (source.status, source.error.clone())
    };
    let destination_status = writable::status(&destination)?;
    // The specification orders these initial/observed conditions deliberately.
    if source_status == Status::Errored {
        return shutdown(
            pipe,
            if options.prevent_abort {
                Action::None
            } else {
                Action::Abort
            },
            Some(source_error),
            c,
        );
    }
    if destination_status == WritableStatus::Errored {
        return shutdown(
            pipe,
            if options.prevent_cancel {
                Action::None
            } else {
                Action::Cancel
            },
            Some(writable::stored_error(&destination)?),
            c,
        );
    }
    if source_status == Status::Closed {
        return shutdown(
            pipe,
            if options.prevent_close {
                Action::None
            } else {
                Action::Close
            },
            None,
            c,
        );
    }
    if writable::is_closing(&destination)? || destination_status == WritableStatus::Closed {
        let error = typ("Cannot pipe to a closing or closed WritableStream").into_opaque(c)?;
        return shutdown(
            pipe,
            if options.prevent_cancel {
                Action::None
            } else {
                Action::Cancel
            },
            Some(error),
            c,
        );
    }
    if pipe.borrow().buffered.is_some() {
        write_buffered(pipe, c)?;
        return schedule(pipe, c);
    }
    if pipe.borrow().reading || destination_status == WritableStatus::Erroring {
        return Ok(());
    }
    if writable::desired_size(&writer)?.is_none_or(|size| size <= 0.0) {
        if !pipe.borrow().waiting_ready {
            pipe.borrow_mut().waiting_ready = true;
            watch(pipe, &writable::ready(&writer)?, Watch::Ready, c)?;
        }
        return Ok(());
    }
    pipe.borrow_mut().reading = true;
    let fulfilled = NativeFunction::from_copy_closure_with_captures(
        |_, args, pipe, c| {
            if pipe.borrow().ended {
                return Ok(JsValue::undefined());
            }
            let result = obj(args.get_or_undefined(0))?;
            let done = result.get(JsString::from("done"), c)?.to_boolean();
            let value = result.get(JsString::from("value"), c)?;
            {
                let mut data = pipe.borrow_mut();
                data.reading = false;
                if !done {
                    data.buffered = Some(value);
                }
            }
            schedule(pipe, c)?;
            Ok(JsValue::undefined())
        },
        pipe.clone(),
    )
    .to_js_function(c.realm());
    let rejected = NativeFunction::from_copy_closure_with_captures(
        |_, _, pipe, c| {
            pipe.borrow_mut().reading = false;
            schedule(pipe, c)?;
            Ok(JsValue::undefined())
        },
        pipe.clone(),
    )
    .to_js_function(c.realm());
    let reader = pipe.borrow().reader.clone();
    read_request(
        &reader,
        ResolvingFunctions {
            resolve: fulfilled,
            reject: rejected,
        },
        c,
    )
}
fn write_buffered(pipe: &Pipe, c: &mut Context) -> JsResult<()> {
    let (chunk, writer) = {
        let mut data = pipe.borrow_mut();
        let Some(chunk) = data.buffered.take() else {
            return Ok(());
        };
        data.pending_writes += 1;
        (chunk, data.writer.clone())
    };
    let promise = match writable::write(&writer, chunk, c) {
        Ok(promise) => promise,
        Err(error) => rejected(error, c)?,
    };
    watch(pipe, &promise, Watch::Write, c)
}
fn shutdown(pipe: &Pipe, action: Action, error: Option<JsValue>, c: &mut Context) -> JsResult<()> {
    if pipe.borrow().shutting_down || pipe.borrow().ended {
        return Ok(());
    }
    let destination = pipe.borrow().destination.clone();
    let wait = writable::status(&destination)? == WritableStatus::Writable
        && !writable::is_closing(&destination)?;
    {
        let mut data = pipe.borrow_mut();
        data.shutting_down = true;
        data.wait_for_writes = wait;
        data.action = action;
        data.error = error;
    }
    advance_shutdown(pipe, c)
}
fn advance_shutdown(pipe: &Pipe, c: &mut Context) -> JsResult<()> {
    if pipe.borrow().ended || pipe.borrow().action_started {
        return Ok(());
    }
    if pipe.borrow().wait_for_writes {
        write_buffered(pipe, c)?;
        if pipe.borrow().pending_writes != 0 {
            return Ok(());
        }
    }
    let (action, error, destination, source, writer, options) = {
        let mut data = pipe.borrow_mut();
        data.action_started = true;
        (
            data.action,
            data.error.clone(),
            data.destination.clone(),
            data.source.clone(),
            data.writer.clone(),
            data.options.clone(),
        )
    };
    let reason = error.clone().unwrap_or_else(JsValue::undefined);
    let mut actions = Vec::new();
    // Signal shutdown starts both actions before awaiting either, destination
    // first. Ordinary forward/backward propagation always calls its algorithm.
    match action {
        Action::None => {}
        Action::Close => actions.push(writable::close_with_error_propagation(&writer, c)),
        Action::Abort => actions.push(writable::abort_stream(&destination, reason, c)),
        Action::Cancel => actions.push(cancel_state(&source, reason, c)),
        Action::Signal => {
            if !options.prevent_abort && writable::status(&destination)? == WritableStatus::Writable
            {
                actions.push(writable::abort_stream(&destination, reason.clone(), c));
            }
            if !options.prevent_cancel && source.borrow().status == Status::Readable {
                actions.push(cancel_state(&source, reason, c));
            }
        }
    }
    if actions.is_empty() {
        return finalize(pipe, error, c);
    }
    pipe.borrow_mut().pending_actions = actions.len();
    for action in actions {
        let promise = match action {
            Ok(promise) => promise,
            Err(error) => rejected(error, c)?,
        };
        watch(pipe, &promise, Watch::Action, c)?;
    }
    Ok(())
}
fn finalize(pipe: &Pipe, error: Option<JsValue>, c: &mut Context) -> JsResult<()> {
    let (writer, reader, signal, token, result) = {
        let mut data = pipe.borrow_mut();
        if data.ended {
            return Ok(());
        }
        data.ended = true;
        data.buffered = None;
        (
            data.writer.clone(),
            data.reader.clone(),
            data.options.signal.clone(),
            data.abort_token,
            data.result.clone(),
        )
    };
    writable::release_writer(&writer, c)?;
    release(&reader, c)?;
    if let Some(signal) = signal {
        web_abort::remove_abort_algorithm(&signal, token)?;
    }
    match error {
        Some(error) => reject(&result, error, c),
        None => resolve(&result, JsValue::undefined(), c),
    }
}
#[cfg(test)]
#[path = "web_streams_pipe_tests.rs"]
mod tests;
