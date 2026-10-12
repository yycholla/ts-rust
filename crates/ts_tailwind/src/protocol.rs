//! Persistent native/WASM stylesheet compiler. Prepared state is immutable;
//! each build replaces its candidate set, including removal after edits.
use crate::{
    snapshot::ProjectSnapshot,
    stylesheet::{PreparedStylesheet, StylesheetOptions, prepare_stylesheet},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
pub const MAX_REQUEST_BYTES: usize = 16 * 1024 * 1024;
const MAX_SESSIONS: usize = 8;

#[derive(Deserialize)]
#[serde(
    tag = "op",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum Request {
    Capabilities,
    Prepare {
        snapshot: ProjectSnapshot,
        options: StylesheetOptions,
    },
    Build {
        session: String,
        candidates: Vec<String>,
    },
    BuildProject {
        session: String,
        sources: Vec<crate::snapshot::SourceSnapshot>,
        inventory: crate::source_selection::SourceInventory,
        #[serde(default)]
        scan_options: ts_tailwind_scan::ScanOptions,
    },
    Scan {
        sources: Vec<ts_tailwind_scan::SourceSnapshot>,
        #[serde(default)]
        options: ts_tailwind_scan::ScanOptions,
    },
    Dispose {
        session: String,
    },
}
#[derive(Default)]
pub struct Service {
    sessions: BTreeMap<String, PreparedStylesheet>,
    next: u64,
}
impl Service {
    pub fn dispatch(&mut self, request: Request) -> Result<Value, String> {
        match request {
            Request::Capabilities => Ok(
                json!({"tailwindVersion":crate::TAILWIND_VERSION,"persistent":true,"maxSessions":MAX_SESSIONS,"candidateReplacement":true,"nativePlugins":{"@tailwindcss/typography":crate::typography::TYPOGRAPHY_VERSION}}),
            ),
            Request::Prepare { snapshot, options } => {
                if self.sessions.len() >= MAX_SESSIONS {
                    return Err("Dispose a stylesheet session before preparing another".into());
                }
                let next = self
                    .next
                    .checked_add(1)
                    .ok_or("Stylesheet session identity exhausted")?;
                let prepared =
                    prepare_stylesheet(&snapshot, &options).map_err(|error| error.to_string())?;
                let id = next.to_string();
                let result = json!({"session":id,"metadata":prepared.metadata()});
                self.sessions.insert(id, prepared);
                self.next = next;
                Ok(result)
            }
            Request::Build {
                session,
                candidates,
            } => {
                let prepared = self
                    .sessions
                    .get(&session)
                    .ok_or("Unknown or disposed stylesheet session")?;
                serde_json::to_value(
                    prepared
                        .build(&candidates)
                        .map_err(|error| error.to_string())?,
                )
                .map_err(|error| error.to_string())
            }
            Request::Scan { sources, options } => serde_json::to_value(
                ts_tailwind_scan::scan_sources(&sources, &options)
                    .map_err(|error| error.to_string())?,
            )
            .map_err(|error| error.to_string()),
            Request::BuildProject {
                session,
                sources,
                inventory,
                scan_options,
            } => {
                let prepared = self
                    .sessions
                    .get(&session)
                    .ok_or("Unknown or disposed stylesheet session")?;
                serde_json::to_value(crate::project::build_prepared(
                    prepared,
                    &sources,
                    &inventory,
                    &scan_options,
                )?)
                .map_err(|error| error.to_string())
            }
            Request::Dispose { session } => {
                Ok(json!({"disposed":self.sessions.remove(&session).is_some()}))
            }
        }
    }
    pub fn request_json(&mut self, request: &str) -> String {
        let result = if request.len() > MAX_REQUEST_BYTES {
            Err("Tailwind request exceeds 16 MiB".into())
        } else {
            serde_json::from_str(request)
                .map_err(|error| error.to_string())
                .and_then(|request| self.dispatch(request))
        };
        let response = match result {
            Ok(result) => json!({"ok":true,"result":result}),
            Err(error) => json!({"ok":false,"error":error}),
        }
        .to_string();
        if response.len() > MAX_REQUEST_BYTES {
            "{\"ok\":false,\"error\":\"Tailwind response exceeds 16 MiB\"}".into()
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
    fn prepared_stylesheets_replace_candidates_and_preserve_state_after_errors() {
        let mut service = Service::default();
        let prepared = call(
            &mut service,
            json!({"op":"prepare","snapshot":{"tailwindVersion":"4.2.1","entry":"app.css","stylesheets":[{"id":"app.css","version":1,"css":"@tailwind utilities;"}]},"options":{"imports":{"bases":{"app.css":"/app"}},"licenseComment":false}}),
        );
        assert_eq!(prepared["ok"], true, "{prepared}");
        let id = prepared["result"]["session"].clone();
        let first = call(
            &mut service,
            json!({"op":"build","session":id,"candidates":["flex"]}),
        );
        assert!(
            first["result"]["css"]
                .as_str()
                .unwrap()
                .contains("display: flex")
        );
        assert_eq!(
            call(&mut service, json!({"op":"prepare","snapshot":{}}))["ok"],
            false
        );
        let second = call(
            &mut service,
            json!({"op":"build","session":id,"candidates":["block"]}),
        );
        let css = second["result"]["css"].as_str().unwrap();
        assert!(css.contains("display: block"));
        assert!(!css.contains("flex"));
        assert_eq!(
            call(&mut service, json!({"op":"dispose","session":id}))["result"]["disposed"],
            true
        );
        assert_eq!(
            call(
                &mut service,
                json!({"op":"build","session":id,"candidates":[]})
            )["ok"],
            false
        );
    }
    #[test]
    fn scan_runs_real_oxide_svelte_extraction() {
        let mut service = Service::default();
        let result = call(
            &mut service,
            json!({"op":"scan","sources":[{"id":"/app/A.svelte","version":3,"extension":"svelte","text":"<div class:underline={enabled} class=\"hover:flex\">test</div>"}]}),
        );
        assert_eq!(result["ok"], true, "{result}");
        let candidates = result["result"]["candidates"].as_array().unwrap();
        assert!(candidates.contains(&json!("underline")));
        assert!(candidates.contains(&json!("hover:flex")));
    }
}
