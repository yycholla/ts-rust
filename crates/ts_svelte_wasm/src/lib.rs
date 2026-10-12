//! Import-free wasm32-unknown-unknown ABI. The host writes to the owned input
//! buffer then calls run; output is valid until the next input/run/reset call.
//! Calls are synchronous and serialized per instance. No raw host pointers are
//! dereferenced by Rust. Panics trap; the JS wrapper retires a trapped instance.

#[cfg(any(target_arch = "wasm32", test))]
mod exports {
    use std::cell::RefCell;
    use ts_svelte::protocol::{MAX_REQUEST_BYTES, dispatch_json};

    thread_local! {
        static INPUT: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
        static OUTPUT: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
        static READY: RefCell<bool> = const { RefCell::new(false) };
    }

    #[allow(unsafe_code)]
    #[unsafe(no_mangle)]
    pub extern "C" fn svelte_input(len: usize) -> *mut u8 {
        OUTPUT.with(|output| output.borrow_mut().clear());
        READY.with(|ready| *ready.borrow_mut() = false);
        INPUT.with(|input| {
            let mut input = input.borrow_mut();
            input.clear();
            if len > MAX_REQUEST_BYTES {
                return std::ptr::null_mut();
            }
            input.resize(len, 0);
            READY.with(|ready| *ready.borrow_mut() = true);
            input.as_mut_ptr()
        })
    }

    #[allow(unsafe_code)]
    #[unsafe(no_mangle)]
    pub extern "C" fn svelte_run() {
        let ready = READY.with(|ready| ready.replace(false));
        let response = if ready {
            INPUT.with(|input| dispatch_json(&input.borrow()))
        } else {
            br#"{"ok":false,"error":{"code":"invalid_request","message":"call svelte_input before each run"}}"#.to_vec()
        };
        OUTPUT.with(|output| *output.borrow_mut() = response);
    }

    #[allow(unsafe_code)]
    #[unsafe(no_mangle)]
    pub extern "C" fn svelte_output() -> *const u8 {
        OUTPUT.with(|output| output.borrow().as_ptr())
    }

    #[allow(unsafe_code)]
    #[unsafe(no_mangle)]
    pub extern "C" fn svelte_output_len() -> usize {
        OUTPUT.with(|output| output.borrow().len())
    }

    #[allow(unsafe_code)]
    #[unsafe(no_mangle)]
    pub extern "C" fn svelte_reset() {
        INPUT.with(|input| *input.borrow_mut() = Vec::new());
        OUTPUT.with(|output| *output.borrow_mut() = Vec::new());
        READY.with(|ready| *ready.borrow_mut() = false);
    }
}
