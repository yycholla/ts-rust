//! Persistent framework operations shared by process and WASM hosts.
use crate::{
    BootstrapRequest, ServerRuntime,
    prerender_session::{PrerenderInput, PrerenderSession},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use ts_js_runtime::{
    HostOptions, RuntimeError,
    protocol::{CryptoOptions, FetchOptions},
};

pub use ts_js_runtime::protocol::MAX_REQUEST_BYTES;
const MAX_SESSIONS: usize = 8;
const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;

#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "camelCase", deny_unknown_fields)]
pub enum Request {
    Capabilities {},
    Create {
        bootstrap: BootstrapRequest,
        #[serde(default, rename = "fetchOptions")]
        fetch_options: FetchOptions,
        #[serde(default, rename = "cryptoOptions")]
        crypto_options: CryptoOptions,
    },
    CreatePrerender {
        input: PrerenderInput,
        #[serde(default, rename = "fetchOptions")]
        fetch_options: FetchOptions,
        #[serde(default, rename = "cryptoOptions")]
        crypto_options: CryptoOptions,
    },
    StartPrerender {
        session: String,
    },
    VisitPrerender {
        session: String,
        path: String,
    },
    BeginPrerenderVisit {
        session: String,
        path: String,
    },
    ReadPrerenderMain {
        session: String,
        token: u64,
    },
    NextPrerenderDependency {
        session: String,
        token: u64,
    },
    ReadPrerenderDependency {
        session: String,
        token: u64,
    },
    PublishPrerender {
        session: String,
        file: String,
        bytes: Vec<u8>,
    },
    LoadNodes {
        session: String,
    },
    AnalyzeMetadata {
        session: String,
        options: crate::metadata::MetadataOptions,
    },
    AnalyzeRemotes {
        session: String,
        chunks: Vec<crate::remotes::RemoteChunk>,
    },
    Dispose {
        session: String,
    },
}

fn error(code: &str, message: impl Into<String>) -> RuntimeError {
    RuntimeError {
        code: code.into(),
        message: message.into(),
    }
}

#[derive(Default)]
pub struct Service {
    sessions: BTreeMap<String, ServerRuntime>,
    rendering: BTreeMap<String, PrerenderSession>,
    next: u64,
}
impl Service {
    pub fn dispatch(&mut self, request: Request) -> Result<Value, RuntimeError> {
        match request {
            Request::Capabilities {} => Ok(json!({
                "engine":"boa", "framework":"sveltekit", "persistent":true,
                "initializationProfiles":["kit2","kit3"], "maxSessions":MAX_SESSIONS,
                "nodeLoads":true, "routeMetadata":true, "remoteMetadata":true,
                "adapterFeatureChecks":false, "prerender":false, "prerenderVisits":true,
                "stagedPrerenderVisits":true,
                "networkTransport":false, "implicitFilesystem":false
            })),
            Request::Create {
                bootstrap,
                fetch_options,
                crypto_options,
            } => {
                if self.sessions.len() + self.rendering.len() >= MAX_SESSIONS {
                    return Err(error(
                        "session_limit",
                        "Dispose a session before creating another",
                    ));
                }
                let next = self
                    .next
                    .checked_add(1)
                    .ok_or_else(|| error("session_limit", "Session identity exhausted"))?;
                let mut options = HostOptions {
                    fetch: fetch_options.into_native()?,
                    ..Default::default()
                };
                crypto_options.apply(&mut options)?;
                let runtime = ServerRuntime::initialize(bootstrap, options)?;
                let build = runtime.server_build_id().to_owned();
                let id = next.to_string();
                self.sessions.insert(id.clone(), runtime);
                self.next = next;
                Ok(json!({"session":id, "serverBuildId":build}))
            }
            Request::CreatePrerender {
                input,
                fetch_options,
                crypto_options,
            } => {
                if self.sessions.len() + self.rendering.len() >= MAX_SESSIONS {
                    return Err(error(
                        "session_limit",
                        "Dispose a session before creating another",
                    ));
                }
                let next = self
                    .next
                    .checked_add(1)
                    .ok_or_else(|| error("session_limit", "Session identity exhausted"))?;
                let mut options = HostOptions {
                    fetch: fetch_options.into_native()?,
                    ..Default::default()
                };
                crypto_options.apply(&mut options)?;
                let runtime = PrerenderSession::configure(input, options)?;
                let build = runtime.server_build_id().to_owned();
                let id = next.to_string();
                self.rendering.insert(id.clone(), runtime);
                self.next = next;
                Ok(json!({"session":id, "serverBuildId":build}))
            }
            Request::StartPrerender { session } => {
                self.rendering
                    .get_mut(&session)
                    .ok_or_else(|| {
                        error("unknown_session", "Unknown or disposed rendering session")
                    })?
                    .start()?;
                Ok(json!({"started":true}))
            }
            Request::VisitPrerender { session, path } => {
                let result = self
                    .rendering
                    .get_mut(&session)
                    .ok_or_else(|| {
                        error("unknown_session", "Unknown or disposed rendering session")
                    })?
                    .visit(&path)?;
                serde_json::to_value(result)
                    .map_err(|e| error("response_serialization", e.to_string()))
            }
            Request::BeginPrerenderVisit { session, path } => {
                let runtime = self
                    .rendering
                    .get_mut(&session)
                    .ok_or_else(|| error("unknown_session", "Unknown prerender session"))?;
                serde_json::to_value(runtime.begin_visit(&path)?)
                    .map_err(|e| error("invalid_output", e.to_string()))
            }
            Request::ReadPrerenderMain { session, token } => {
                let runtime = self
                    .rendering
                    .get_mut(&session)
                    .ok_or_else(|| error("unknown_session", "Unknown prerender session"))?;
                serde_json::to_value(runtime.read_main(token)?)
                    .map_err(|e| error("invalid_output", e.to_string()))
            }
            Request::NextPrerenderDependency { session, token } => {
                let runtime = self
                    .rendering
                    .get_mut(&session)
                    .ok_or_else(|| error("unknown_session", "Unknown prerender session"))?;
                serde_json::to_value(runtime.next_dependency(token)?)
                    .map_err(|e| error("invalid_output", e.to_string()))
            }
            Request::ReadPrerenderDependency { session, token } => {
                let runtime = self
                    .rendering
                    .get_mut(&session)
                    .ok_or_else(|| error("unknown_session", "Unknown prerender session"))?;
                serde_json::to_value(runtime.read_dependency(token)?)
                    .map_err(|e| error("invalid_output", e.to_string()))
            }
            Request::PublishPrerender {
                session,
                file,
                bytes,
            } => {
                self.rendering
                    .get_mut(&session)
                    .ok_or_else(|| {
                        error("unknown_session", "Unknown or disposed rendering session")
                    })?
                    .publish(file, bytes)?;
                Ok(json!({"published":true}))
            }
            Request::LoadNodes { session } => {
                let runtime = self
                    .sessions
                    .get_mut(&session)
                    .ok_or_else(|| error("unknown_session", "Unknown or disposed session"))?;
                Ok(
                    json!({"serverBuildId":runtime.server_build_id().to_owned(), "nodes":runtime.load_nodes()?}),
                )
            }
            Request::AnalyzeMetadata { session, options } => self
                .sessions
                .get_mut(&session)
                .ok_or_else(|| error("unknown_session", "Unknown or disposed session"))?
                .analyze_metadata(options),
            Request::AnalyzeRemotes { session, chunks } => self
                .sessions
                .get_mut(&session)
                .ok_or_else(|| error("unknown_session", "Unknown or disposed session"))?
                .analyze_remotes(chunks),
            Request::Dispose { session } => {
                let metadata = self.sessions.remove(&session).is_some();
                let rendering = self.rendering.remove(&session).is_some();
                Ok(json!({"disposed":metadata || rendering}))
            }
        }
    }
    pub fn request_json(&mut self, input: &str) -> String {
        let result = if input.len() > MAX_REQUEST_BYTES {
            Err(error("request_limit", "Runtime request exceeds 96 MiB"))
        } else {
            serde_json::from_str::<Request>(input)
                .map_err(|e| error("invalid_request", e.to_string()))
                .and_then(|request| self.dispatch(request))
        };
        let response = match result {
            Ok(result) => json!({"ok":true,"result":result}),
            Err(error) => json!({"ok":false,"error":error}),
        }
        .to_string();
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
    fn bootstrap(failing: bool) -> Value {
        let source = if failing {
            "export const manifest={nodes:[()=>{throw Error('node failure')}]}; export function configure(){}"
        } else {
            "let initialized=false,count=0;export const manifest={nodes:[async()=>{await 0;if(!initialized||++count!==1)throw Error('invalid lifecycle');return {index:0,server:{load(){}}}}]}; export async function configure(){await 0;initialized=true}"
        };
        json!({"serverBuildId":"fixture-build","snapshot":{"modules":{"server":{"source":source,"importMetaUrl":"file:///server.mjs"}},"resolutions":{}},"manifestEntry":"server","initialization":{"profile":"kit3","server":"server"},"environment":{},"assets":{}})
    }
    #[test]
    fn rendering_and_metadata_share_session_limit_and_disposal() {
        let mut service = Service::default();
        let mut render = bootstrap(false);
        render["snapshot"]["modules"]["server"]["source"] = json!(
            "export const manifest={}; export function configure(){ return {init(){}, respond(){return new Response('ready')}}; }"
        );
        let input = json!({"bootstrap":render,"serverEntry":"server","origin":"https://example.test","base":"","appDir":"_app","staticAssets":{}});
        let created = call(&mut service, json!({"op":"createPrerender","input":input}));
        assert_eq!(created["ok"], true, "{created}");
        let session = created["result"]["session"].clone();
        assert_eq!(
            call(&mut service, json!({"op":"loadNodes","session":session}))["error"]["code"],
            "unknown_session"
        );
        for _ in 0..7 {
            assert_eq!(
                call(
                    &mut service,
                    json!({"op":"create","bootstrap":bootstrap(false)})
                )["ok"],
                true
            );
        }
        assert_eq!(
            call(
                &mut service,
                json!({"op":"create","bootstrap":bootstrap(false)})
            )["error"]["code"],
            "session_limit"
        );
        assert_eq!(
            call(&mut service, json!({"op":"createPrerender","input":input}))["error"]["code"],
            "session_limit"
        );
        assert_eq!(
            call(
                &mut service,
                json!({"op":"startPrerender","session":session})
            )["result"]["started"],
            true
        );
        let visited = call(
            &mut service,
            json!({"op":"visitPrerender","session":session,"path":"/"}),
        );
        assert_eq!(visited["ok"], true, "{visited}");
        assert_eq!(
            visited["result"]["response"]["body"],
            json!(b"ready".to_vec())
        );
        assert_eq!(
            call(&mut service, json!({"op":"dispose","session":session}))["result"]["disposed"],
            true
        );
        assert_eq!(
            call(&mut service, json!({"op":"dispose","session":session}))["result"]["disposed"],
            false
        );
        assert_eq!(
            call(
                &mut service,
                json!({"op":"startPrerender","session":session})
            )["error"]["code"],
            "unknown_session"
        );
        assert_eq!(
            call(&mut service, json!({"op":"createPrerender","input":input}))["ok"],
            true
        );
    }
    #[test]
    fn runtime_metadata_executes_entries_once_and_preserves_undefined() {
        let mut service = Service::default();
        let mut input = bootstrap(false);
        input["snapshot"]["modules"]["server"]["source"] = json!(
            r#"
            let entries=0;
            export function configure(){}
            export const manifest={
                nodes:[async()=>({index:0,universal:{ssr:false,entries:async()=>{
                    await 0;if(++entries!==1)throw Error('entries repeated');return [{slug:'native'}];
                }}})],routes:[{id:'/[slug]',page:{layouts:[],leaf:0}},{id:'/api',endpoint:()=>({GET(){}})}]
            };
        "#
        );
        let created = call(&mut service, json!({"op":"create","bootstrap":input}));
        assert_eq!(created["ok"], true, "{created}");
        let session = &created["result"]["session"];
        let request =
            json!({"op":"analyzeMetadata","session":session,"options":{"profile":"3.0.1"}});
        let result = call(&mut service, request.clone());
        assert_eq!(result["ok"], true, "{result}");
        let route = &result["result"]["routes"][0];
        assert_eq!(
            result["result"]["routes"][1]["entries"],
            json!({"kind":"undefined"})
        );
        assert_eq!(route["entries"], json!({"kind":"json","value":["/native"]}));
        assert_eq!(route["page"]["ssr"], json!({"kind":"json","value":false}));
        assert_eq!(result, call(&mut service, request));
        assert_eq!(
            call(
                &mut service,
                json!({"op":"analyzeMetadata","session":session,"options":{"profile":"2.69.2"}})
            )["error"]["code"],
            "kit_runtime_input"
        );
    }
    #[test]
    fn persistent_initialization_and_cached_nodes_cross_json_boundary() {
        let mut service = Service::default();
        let created = call(
            &mut service,
            json!({"op":"create","bootstrap":bootstrap(false)}),
        );
        assert_eq!(created["ok"], true, "{created}");
        let session = &created["result"]["session"];
        let first = call(&mut service, json!({"op":"loadNodes","session":session}));
        assert_eq!(first["ok"], true, "{first}");
        assert_eq!(first["result"]["serverBuildId"], "fixture-build");
        assert_eq!(first["result"]["nodes"][0]["hasServerLoad"], true);
        assert_eq!(
            call(&mut service, json!({"op":"loadNodes","session":session})),
            first
        );
        assert_eq!(
            call(&mut service, json!({"op":"dispose","session":session}))["result"]["disposed"],
            true
        );
        assert_eq!(
            call(&mut service, json!({"op":"loadNodes","session":session}))["error"]["code"],
            "unknown_session"
        );
    }
    #[test]
    fn failed_execution_is_poisoned_and_sessions_are_bounded() {
        let mut service = Service::default();
        assert_eq!(call(&mut service, json!({"op":"capabilities"}))["ok"], true);
        assert_eq!(
            call(
                &mut service,
                json!({"op":"capabilities", "unexpected":true})
            )["error"]["code"],
            "invalid_request"
        );
        for _ in 0..MAX_SESSIONS {
            assert_eq!(
                call(
                    &mut service,
                    json!({"op":"create","bootstrap":bootstrap(true)})
                )["ok"],
                true
            );
        }
        assert_eq!(
            call(
                &mut service,
                json!({"op":"create","bootstrap":bootstrap(false)})
            )["error"]["code"],
            "session_limit"
        );
        let failure = call(&mut service, json!({"op":"loadNodes","session":"1"}));
        assert_eq!(failure["ok"], false);
        assert!(
            failure["error"]["message"]
                .as_str()
                .unwrap()
                .contains("node failure")
        );
        assert_eq!(
            call(&mut service, json!({"op":"loadNodes","session":"1"}))["error"]["code"],
            "runtime_failed"
        );
        assert_eq!(
            call(
                &mut service,
                json!({"op":"create","bootstrap":bootstrap(false),"unexpected":1})
            )["error"]["code"],
            "invalid_request"
        );
    }
}
