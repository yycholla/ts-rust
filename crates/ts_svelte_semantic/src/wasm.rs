//! Buffer ownership stays in Rust. Pointers expire at the next allocating call.
use crate::{Host, MAX_FRAME};
use std::cell::RefCell;

#[derive(Default)]
struct State {
    host: Host,
    input: Vec<u8>,
    output: Vec<u8>,
    prepared: bool,
}
thread_local! { static STATE: RefCell<State> = RefCell::new(State::default()); }

/// Allocate a zeroed UTF-8 JSON request buffer; zero means size/state rejected.
#[unsafe(no_mangle)]
pub extern "C" fn semantic_input(len: u32) -> *mut u8 {
    STATE.with(|state| {
        let Ok(mut state) = state.try_borrow_mut() else {
            return std::ptr::null_mut();
        };
        state.prepared = false;
        if len == 0 || len as usize > MAX_FRAME {
            return std::ptr::null_mut();
        }
        state.input.clear();
        state.input.resize(len as usize, 0);
        state.prepared = true;
        state.input.as_mut_ptr()
    })
}

/// Dispatch exactly the last prepared buffer. Return 0 for a JSON reply, 1 for
/// rejected ABI usage. A trap/proc_exit requires discarding the entire instance.
#[unsafe(no_mangle)]
pub extern "C" fn semantic_request() -> u32 {
    STATE.with(|state| {
        let Ok(mut state) = state.try_borrow_mut() else { return 1; };
        if !std::mem::take(&mut state.prepared) { return 1; }
        let input = std::mem::take(&mut state.input);
        state.output = match std::str::from_utf8(&input) {
            Ok(input) => state.host.request_json(input).into_bytes(),
            Err(_) => b"{\"ok\":false,\"error\":{\"code\":\"invalid_utf8\",\"message\":\"Request must be UTF-8\"}}".to_vec(),
        };
        0
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn semantic_output() -> *const u8 {
    STATE.with(|state| {
        state
            .try_borrow()
            .map_or(std::ptr::null(), |s| s.output.as_ptr())
    })
}
#[unsafe(no_mangle)]
pub extern "C" fn semantic_output_len() -> u32 {
    STATE.with(|state| state.try_borrow().map_or(0, |s| s.output.len() as u32))
}
