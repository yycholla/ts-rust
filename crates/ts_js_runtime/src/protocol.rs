//! Stateful JSON host interface, shared by native process and worker hosts.
use crate::{HostCapability, HostSnapshot, Runtime, RuntimeError, RuntimeLimits};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

pub const MAX_REQUEST_BYTES: usize = 96 * 1024 * 1024;
const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
const MAX_SESSIONS: usize = 8;

#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "camelCase", deny_unknown_fields)]
pub enum Request {
    Capabilities,
    Create {
        snapshot: HostSnapshot,
        limits: Option<RuntimeLimits>,
        #[serde(default)]
        capabilities: BTreeSet<HostCapability>,
        #[serde(default, rename = "fetchOptions")]
        fetch_options: Option<FetchOptions>,
        #[serde(default, rename = "cryptoOptions")]
        crypto_options: Option<CryptoOptions>,
    },
    Evaluate {
        session: String,
        module: String,
    },
    ReadExport {
        session: String,
        module: String,
        name: String,
    },
    InvokeExport {
        session: String,
        module: String,
        name: String,
        arguments: Vec<Value>,
    },
    Dispose {
        session: String,
    },
}

/// Entropy is supplied by the embedding, never generated from a seed here.
#[derive(Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct CryptoOptions {
    pub secure_context: bool,
    pub entropy: Vec<u8>,
}
impl Default for CryptoOptions {
    fn default() -> Self {
        Self {
            secure_context: true,
            entropy: vec![],
        }
    }
}
impl CryptoOptions {
    pub fn apply(self, host: &mut crate::HostOptions) -> Result<(), RuntimeError> {
        if self.entropy.len() > 1024 * 1024 {
            return Err(crate::error(
                "host_input_limit",
                "Crypto entropy pool exceeds 1 MiB",
            ));
        }
        host.crypto.secure_context = self.secure_context;
        if !self.entropy.is_empty() {
            let pool = std::sync::Mutex::new(std::collections::VecDeque::from(self.entropy));
            host.entropy = Some(std::sync::Arc::new(move |output: &mut [u8]| {
                let mut pool = pool
                    .lock()
                    .map_err(|_| "Crypto entropy pool lock poisoned".to_owned())?;
                if pool.len() < output.len() {
                    return Err("Host crypto entropy pool exhausted".into());
                }
                for byte in output {
                    *byte = pool.pop_front().expect("Pool length checked");
                }
                Ok(())
            }));
        }
        Ok(())
    }
}

/// Host-provided web environment. Multipart bytes must come from the host's
/// entropy source; this protocol neither manufactures nor certifies randomness.
#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct FetchOptions {
    pub base_url: Option<String>,
    pub origin: Option<String>,
    pub multipart_entropy: Vec<[u8; 16]>,
}
impl FetchOptions {
    pub fn into_native(self) -> Result<crate::web_fetch::FetchOptions, RuntimeError> {
        if self.multipart_entropy.len() > 4096 {
            return Err(crate::error(
                "host_input_limit",
                "Multipart entropy pool exceeds 4096 boundaries",
            ));
        }
        let multipart_entropy = if self.multipart_entropy.is_empty() {
            None
        } else {
            let pool =
                std::cell::RefCell::new(std::collections::VecDeque::from(self.multipart_entropy));
            let callback: crate::web_fetch::MultipartEntropy = std::rc::Rc::new(move |output| {
                if output.len() != 16 {
                    return Err("Expected a 16-byte multipart entropy request".into());
                }
                let value = pool
                    .borrow_mut()
                    .pop_front()
                    .ok_or("Host multipart entropy pool exhausted")?;
                output.copy_from_slice(&value);
                Ok(())
            });
            Some(callback)
        };
        Ok(crate::web_fetch::FetchOptions {
            base_url: self.base_url,
            origin: self.origin,
            multipart_entropy,
        })
    }
}

#[derive(Default)]
pub struct Service {
    sessions: BTreeMap<String, Runtime>,
    next: u64,
}
impl Service {
    fn runtime(&mut self, id: &str) -> Result<&mut Runtime, RuntimeError> {
        self.sessions
            .get_mut(id)
            .ok_or_else(|| crate::error("unknown_session", "Unknown or disposed session"))
    }
    pub fn dispatch(&mut self, request: Request) -> Result<Value, RuntimeError> {
        match request {
            Request::Capabilities => Ok(
                json!({"engine":"boa","version":"0.22.0","esm":true,"persistent":true,"hostCapabilities":["url","text","headers","blob","formData","abort","streams","requestResponse","nodeAsyncHooks","crypto"],"networkTransport":false,"defaultHostCapabilities":[],"capabilityDependencies":{"formData":["blob"],"blob":["streams"],"streams":["abort","text"],"abort":["text"],"crypto":["text"],"requestResponse":["url","text","headers","formData","abort"]},"maxSessions":MAX_SESSIONS}),
            ),
            Request::Create {
                snapshot,
                limits,
                capabilities,
                fetch_options,
                crypto_options,
            } => {
                if crypto_options.is_some() && !capabilities.contains(&HostCapability::Crypto) {
                    return Err(crate::error(
                        "host_configuration",
                        "cryptoOptions requires crypto capability",
                    ));
                }
                if fetch_options.is_some()
                    && !capabilities.contains(&HostCapability::RequestResponse)
                {
                    return Err(crate::error(
                        "host_configuration",
                        "fetchOptions requires requestResponse capability",
                    ));
                }
                if self.sessions.len() == MAX_SESSIONS {
                    return Err(crate::error(
                        "session_limit",
                        "Dispose a session before creating another",
                    ));
                }
                let next = self
                    .next
                    .checked_add(1)
                    .ok_or_else(|| crate::error("session_limit", "Session identity exhausted"))?;
                let mut host = crate::HostOptions {
                    fetch: fetch_options.unwrap_or_default().into_native()?,
                    ..Default::default()
                };
                crypto_options.unwrap_or_default().apply(&mut host)?;
                let runtime = Runtime::with_host_options(
                    snapshot,
                    limits.unwrap_or_default(),
                    &capabilities,
                    host,
                )?;
                let id = next.to_string();
                self.sessions.insert(id.clone(), runtime);
                self.next = next;
                Ok(json!({"session":id}))
            }
            Request::Dispose { session } => {
                Ok(json!({"disposed":self.sessions.remove(&session).is_some()}))
            }
            Request::Evaluate { session, module } => {
                let runtime = self.runtime(&session)?;
                let value = runtime.evaluate(&module)?;
                Ok(
                    json!({"value":value,"instructionsRemaining":runtime.instructions_remaining(),"jobsRemaining":runtime.jobs_remaining(),"reportedErrors":runtime.take_reported_errors()}),
                )
            }
            Request::ReadExport {
                session,
                module,
                name,
            } => {
                let runtime = self.runtime(&session)?;
                let value = runtime.read_export(&module, &name)?;
                Ok(
                    json!({"value":value,"instructionsRemaining":runtime.instructions_remaining(),"jobsRemaining":runtime.jobs_remaining(),"reportedErrors":runtime.take_reported_errors()}),
                )
            }
            Request::InvokeExport {
                session,
                module,
                name,
                arguments,
            } => {
                let runtime = self.runtime(&session)?;
                let value = runtime.invoke_export(&module, &name, arguments)?;
                Ok(
                    json!({"value":value,"instructionsRemaining":runtime.instructions_remaining(),"jobsRemaining":runtime.jobs_remaining(),"reportedErrors":runtime.take_reported_errors()}),
                )
            }
        }
    }
    pub fn request_json(&mut self, input: &str) -> String {
        let result = if input.len() > MAX_REQUEST_BYTES {
            Err(crate::error(
                "request_limit",
                "Runtime request exceeds 96 MiB",
            ))
        } else {
            serde_json::from_str::<Request>(input)
                .map_err(|error| crate::error("invalid_request", error.to_string()))
                .and_then(|request| self.dispatch(request))
        };
        let response = match result {
            Ok(result) => json!({"ok":true,"result":result}),
            Err(error) => json!({"ok":false,"error":error}),
        };
        let response = response.to_string();
        if response.len() > MAX_RESPONSE_BYTES {
            json!({"ok":false,"error":{"code":"response_limit","message":"Runtime response exceeds 8 MiB"}}).to_string()
        } else {
            response
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn call(service: &mut Service, request: Value) -> Value {
        serde_json::from_str(&service.request_json(&request.to_string())).unwrap()
    }
    #[test]
    fn explicit_builtin_context_survives_concurrent_awaits() {
        let mut service = Service::default();
        let snapshot = json!({"modules":{"app":{"importMetaUrl":"file:///app.mjs","source":r#"
            import {AsyncLocalStorage} from 'node:async_hooks';
            const storage=new AsyncLocalStorage();
            export async function run(){
                const alias=await import('async_hooks');
                const values=await Promise.all(['first','second'].map(value=>storage.run(value,async()=>{
                    await Promise.resolve(); const before=storage.getStore();
                    await Promise.resolve(); return [before,storage.getStore()];
                })));
                return [values,storage.getStore()===undefined,alias.AsyncLocalStorage===AsyncLocalStorage,typeof globalThis.AsyncLocalStorage];
            }
        "#}},"resolutions":{"app":{"node:async_hooks":{"kind":"builtin","name":"node:async_hooks"},"async_hooks":{"kind":"builtin","name":"async_hooks"}}}});
        let denied = call(&mut service, json!({"op":"create","snapshot":snapshot}));
        assert_eq!(denied["error"]["code"], "invalid_snapshot");
        let created = call(
            &mut service,
            json!({"op":"create","capabilities":["nodeAsyncHooks"],"snapshot":snapshot}),
        );
        assert_eq!(created["ok"], true, "{created}");
        for _ in 0..2 {
            let response = call(
                &mut service,
                json!({"op":"invokeExport","session":created["result"]["session"],"module":"app","name":"run","arguments":[]}),
            );
            assert_eq!(response["ok"], true, "{response}");
            assert_eq!(
                response["result"]["value"],
                json!({"kind":"json","value":[[["first","first"],["second","second"]],true,true,"undefined"]})
            );
        }
    }
    #[test]
    fn native_capabilities_work_together_and_report_listener_exceptions() {
        let mut service = Service::default();
        let created = call(
            &mut service,
            json!({"op":"create","capabilities":["url","text","headers","formData","abort"],"snapshot":{"modules":{"app":{"importMetaUrl":"file:///app.mjs","source":r#"
            export async function run() {
                const target=new EventTarget();
                target.addEventListener('test',()=>{throw new Error('listener failure')});
                const dispatched=target.dispatchEvent(new Event('test'));
                const form=new FormData(); form.append('file',new Blob(['hello']), 'hello.txt');
                const controller=new AbortController(); controller.abort('cancelled');
                return {
                    path:new URL('../path','https://example.test/base/').pathname,
                    text:new TextDecoder().decode(new TextEncoder().encode('🙂')),
                    header:new Headers([['A','one'],['a','two']]).get('a'),
                    file:await form.get('file').text(),
                    name:form.get('file').name,
                    reason:controller.signal.reason,
                    dispatched
                };
            }
        "#}},"resolutions":{}}}),
        );
        assert_eq!(created["ok"], true, "{created}");
        let response = call(
            &mut service,
            json!({"op":"invokeExport","session":created["result"]["session"],"module":"app","name":"run","arguments":[]}),
        );
        assert_eq!(response["ok"], true, "{response}");
        assert_eq!(
            response["result"]["value"],
            json!({"kind":"json","value":{"path":"/path","text":"🙂","header":"one, two","file":"hello","name":"hello.txt","reason":"cancelled","dispatched":true}})
        );
        let errors = response["result"]["reportedErrors"].as_array().unwrap();
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0]["code"], "event_listener_exception");
        assert!(
            errors[0]["message"]
                .as_str()
                .unwrap()
                .contains("listener failure")
        );
    }
    #[test]
    fn persistent_module_state_disposal_and_errors_are_observable() {
        let mut service = Service::default();
        let created = call(
            &mut service,
            json!({"op":"create","snapshot":{"modules":{"app":{"source":"let count=0; export async function next(){ await 0; return ++count; } export const empty=undefined;","importMetaUrl":"file:///app.js"}},"resolutions":{}}}),
        );
        let session = created["result"]["session"].as_str().unwrap().to_owned();
        for expected in [1, 2] {
            let response = call(
                &mut service,
                json!({"op":"invokeExport","session":session,"module":"app","name":"next","arguments":[]}),
            );
            assert_eq!(
                response["result"]["value"],
                json!({"kind":"json","value":expected})
            );
        }
        assert_eq!(
            call(
                &mut service,
                json!({"op":"readExport","session":session,"module":"app","name":"empty"})
            )["result"]["value"]["kind"],
            "undefined"
        );
        assert_eq!(
            call(&mut service, json!({"op":"dispose","session":session}))["result"]["disposed"],
            true
        );
        assert_eq!(
            call(
                &mut service,
                json!({"op":"evaluate","session":session,"module":"app"})
            )["error"]["code"],
            "unknown_session"
        );
    }
    #[test]
    fn malformed_requests_do_not_destroy_existing_sessions() {
        let mut service = Service::default();
        assert_eq!(
            call(
                &mut service,
                json!({"op":"create","snapshot":{"modules":{},"resolutions":{}}})
            )["result"]["session"],
            "1"
        );
        assert_eq!(
            call(
                &mut service,
                json!({"op":"dispose","session":"1","unexpected":true})
            )["ok"],
            false
        );
        assert_eq!(
            call(&mut service, json!({"op":"dispose","session":"1"}))["result"]["disposed"],
            true
        );
    }
}
