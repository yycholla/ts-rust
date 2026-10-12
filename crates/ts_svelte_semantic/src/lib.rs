//! Persistent Rust TypeScript semantics over an explicitly supplied filesystem.
//! WASM uses WASI preview1 for clocks; no filesystem, timers, threads or child
//! processes are supplied by the host. Each worker owns one serialized Host.
#![deny(unsafe_code)]

#[cfg(all(target_family = "wasm", not(target_os = "wasi")))]
compile_error!("ts_svelte_semantic requires wasm32-wasip1; core Instant needs WASI clocks");

mod changes;
pub mod mapping;
mod memory;
mod service;
#[cfg(test)]
mod tests;
#[cfg(target_family = "wasm")]
#[allow(unsafe_code)]
mod wasm;

use serde_json::{Value, json};
use std::cell::Cell;
use std::sync::atomic::{AtomicU32, Ordering};
use ts_goport::frontend::core_context::{self, CheckerLifetime};

/// Maximum encoded input or output frame. Oversized replies poison the host:
/// the operation may already have changed state, so retrying is not safe.
pub const MAX_FRAME: usize = 64 * 1024 * 1024;
static NEXT_SESSION: AtomicU32 = AtomicU32::new(1);
thread_local! { static ACTIVE: Cell<bool> = const { Cell::new(false) }; }

pub(crate) struct Occupancy;
impl Occupancy {
    pub(crate) fn acquire() -> Result<Self, String> {
        ACTIVE.with(|active| {
            if active.replace(true) {
                Err("Only one semantic session may be active on a thread".into())
            } else {
                Ok(Self)
            }
        })
    }
}
impl Drop for Occupancy {
    fn drop(&mut self) {
        ACTIVE.with(|active| active.set(false));
    }
}

/// A single serialized semantic session. This type is deliberately !Send/!Sync.
/// Shutdown is terminal; a fresh Host may then be initialized on the same thread.
#[derive(Default)]
pub struct Host {
    service: Option<service::Service>,
    session: u32,
    generation: u32,
    terminal: bool,
}

impl Host {
    pub fn new() -> Self {
        Self::default()
    }

    /// JSON envelope: `{id, op, params, session?, generation?}`. Initialize has
    /// no session/generation; every later request must echo both from the latest
    /// reply. Every dispatched request consumes a generation, even on failure.
    pub fn request(&mut self, request: Value) -> Value {
        let id = request.get("id").cloned().unwrap_or(Value::Null);
        if request.to_string().len() > MAX_FRAME {
            return self.error(id, "frame_too_large", "Request exceeds 64 MiB");
        }
        if self.terminal {
            return self.error(
                id,
                "closed",
                "Host is closed or poisoned; create a new instance",
            );
        }
        let Some(op) = request["op"].as_str() else {
            return self.error(id, "invalid_request", "Missing op");
        };
        if !request.is_object() || !request["params"].is_object() {
            return self.error(
                id,
                "invalid_request",
                "Expected an object with object params",
            );
        }
        if op != "initialize"
            && (self.service.is_none()
                || request["session"].as_u64() != Some(u64::from(self.session))
                || request["generation"].as_u64() != Some(u64::from(self.generation)))
        {
            return self.error(
                id,
                "stale_session",
                "Session or generation does not match current state",
            );
        }
        if op == "initialize"
            && (self.service.is_some()
                || request.get("session").is_some()
                || request.get("generation").is_some())
        {
            return self.error(
                id,
                "invalid_request",
                "Initialize requires a new host and no session/generation",
            );
        }
        let Some(next) = self.generation.checked_add(1) else {
            self.close();
            return self.error(id, "exhausted", "Generation exhausted");
        };
        self.generation = next;
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            if op == "initialize" {
                let session = NEXT_SESSION
                    .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
                    .map_err(|_| "Session IDs exhausted".to_owned())?;
                let created = service::Service::new(&request["params"])?;
                let capabilities = ts_goport::lsp::lsproto::get_client_capabilities(&created.ctx);
                let legend = service::encode(&ts_goport::ls::semantic_tokens_legend(
                    &capabilities.text_document.semantic_tokens,
                ))?;
                self.session = session;
                self.service = Some(created);
                return Ok(
                    json!({"positionEncoding":"utf-16","persistent":true,"crossProject":false,
                    "filesystem":"memory-posix-case-sensitive","semanticTokensLegend":legend}),
                );
            }
            if op == "shutdown" {
                self.close();
                return Ok(Value::Null);
            }
            let service = self.service.as_mut().ok_or("Not initialized")?;
            let ctx = core_context::with_checker_lifetime(
                &core_context::with_request_id(&service.ctx, &id.to_string()),
                CheckerLifetime::API,
            );
            let result = if op == "files" {
                service.files(&request["params"], &ctx)
            } else {
                service.request(op, &request["params"], &ctx)
            };
            service.drain();
            result
        }));
        let unported = ts_goport::prelude::unported_report();
        let refresh = self
            .service
            .as_ref()
            .map(service::Service::take_refresh)
            .unwrap_or_default();
        let mut reply = match result {
            Err(_) => {
                // A native unwind may leave core state inconsistent. Do not run
                // arbitrary disposal on it; native caller must discard this thread.
                self.terminal = true;
                if let Some(service) = self.service.take() {
                    std::mem::forget(service);
                }
                self.error(
                    id,
                    "engine_panic",
                    "Engine panicked; discard the worker/thread",
                )
            }
            _ if !unported.is_empty() => {
                self.close();
                let mut reply = self.error(id, "unported", "Engine encountered unported behavior");
                reply["error"]["details"] = json!(unported);
                reply
            }
            Ok(Err(message)) => self.error(id, "request_failed", &message),
            Ok(Ok(result)) => {
                json!({"id":id,"ok":true,"session":self.session,"generation":self.generation,"result":result})
            }
        };
        reply["refresh"] = json!(refresh);
        if reply.to_string().len() > MAX_FRAME {
            self.close();
            return self.error(
                reply["id"].clone(),
                "frame_too_large",
                "Reply exceeds 64 MiB; host closed",
            );
        }
        reply
    }

    pub fn request_json(&mut self, input: &str) -> String {
        let reply = if input.len() > MAX_FRAME {
            self.error(Value::Null, "frame_too_large", "Request exceeds 64 MiB")
        } else {
            match serde_json::from_str(input) {
                Ok(request) => self.request(request),
                Err(error) => self.error(Value::Null, "invalid_json", &error.to_string()),
            }
        };
        reply.to_string()
    }

    pub fn close(&mut self) {
        self.terminal = true;
        self.service.take();
    }

    fn error(&self, id: Value, code: &str, message: &str) -> Value {
        json!({"id":id,"ok":false,"session":self.session,"generation":self.generation,
            "error":{"code":code,"message":message}})
    }
}
