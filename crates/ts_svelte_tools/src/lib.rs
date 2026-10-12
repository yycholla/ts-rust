//! Framework-independent Svelte lint and persistent document services.
use serde::Deserialize;
use serde_json::{Value, json};
use ts_svelte::{
    Snapshot,
    compatibility::{self, Versions},
};
pub mod editor_map;
#[cfg(not(target_arch = "wasm32"))]
pub mod editor_project;
#[cfg(not(target_arch = "wasm32"))]
pub mod format_host;
mod lint;
pub mod lint_actions;
#[cfg(not(target_arch = "wasm32"))]
pub mod project_check;
pub mod project_host;
#[cfg(not(target_arch = "wasm32"))]
pub mod project_provider;
pub mod session;
pub mod syntax_editor;
pub mod wasm_project;
#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "camelCase", deny_unknown_fields)]
enum Request {
    Lint {
        document: Snapshot,
        #[serde(default)]
        runes: Option<bool>,
        #[serde(default)]
        compatibility: Versions,
        #[serde(default, rename = "lintConfig")]
        lint_config: Value,
    },
}
fn failure(code: &str, message: &str) -> Value {
    json!({"ok":false,"error":{"code":code,"message":message}})
}
#[must_use]
pub fn dispatch(input: &[u8]) -> Value {
    if input.len() > ts_svelte::protocol::MAX_REQUEST_BYTES {
        return failure("size_limit", "Request exceeds 16 MiB");
    }
    match serde_json::from_slice::<Request>(input) {
        Ok(Request::Lint {
            document,
            runes,
            compatibility,
            lint_config,
        }) => match compatibility::resolve(&compatibility) {
            Ok(profile) => std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                lint::lint_configured(document, runes, &profile, lint_config)
            }))
            .unwrap_or_else(|_| {
                failure(
                    "backend_panic",
                    "Rust lint backend failed; diagnostics are incomplete",
                )
            }),
            Err(message) => failure("unsupported_version", &message),
        },
        Err(error) => failure("invalid_request", &error.to_string()),
    }
}
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen]
#[must_use]
pub fn tooling_request_json(input: &str) -> String {
    thread_local! {static SESSION:std::cell::RefCell<session::Session>=std::cell::RefCell::default();}
    thread_local! {static PROJECT:std::cell::RefCell<project_host::ProjectHost>=std::cell::RefCell::default();}
    if serde_json::from_str::<Value>(input)
        .ok()
        .is_some_and(|v| v["op"].as_str().is_some_and(|op| op.starts_with("project")))
    {
        return PROJECT
            .with(|host| host.borrow_mut().request(input.as_bytes()))
            .to_string();
    }
    let is_session = serde_json::from_str::<Value>(input)
        .ok()
        .and_then(|v| v["op"].as_str().map(|op| op.starts_with("session")))
        .unwrap_or(false);
    if is_session {
        SESSION
            .with(|s| s.borrow_mut().request(input.as_bytes()))
            .to_string()
    } else {
        dispatch(input.as_bytes()).to_string()
    }
}
