//! Persistent immutable document snapshots, diagnostics caches and atomic edits.
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use ts_svelte::compatibility::{self, Versions};
use ts_svelte::{Document, Edit, Snapshot};

#[derive(Default)]
pub struct Session {
    documents: BTreeMap<String, Document>,
    diagnostics: BTreeMap<String, Value>,
    versions: Versions,
    runes: Option<bool>,
    lint_config: Value,
}
#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "camelCase", deny_unknown_fields)]
enum Request {
    #[serde(rename = "sessionConfigure")]
    Configure {
        #[serde(default)]
        compatibility: Versions,
        #[serde(default)]
        runes: Option<bool>,
        #[serde(default, rename = "lintConfig")]
        lint_config: Value,
    },
    #[serde(rename = "sessionOpen")]
    Open { document: Snapshot },
    #[serde(rename = "sessionChange")]
    Change {
        uri: String,
        version: u32,
        edits: Vec<Edit>,
    },
    #[serde(rename = "sessionClose")]
    Close { uri: String },
    #[serde(rename = "sessionDiagnostics")]
    Diagnostics { uri: String },
    #[serde(rename = "sessionCodeActions")]
    CodeActions { uri: String, params: Value },
    #[serde(rename = "sessionSyntax")]
    Syntax {
        uri: String,
        method: String,
        #[serde(default)]
        params: Value,
    },
    #[serde(rename = "sessionStats")]
    Stats {},
}
impl Session {
    #[must_use]
    pub fn snapshots(&self) -> Vec<Snapshot> {
        self.documents
            .values()
            .map(|d| d.snapshot().clone())
            .collect()
    }
    #[must_use]
    pub fn request(&mut self, input: &[u8]) -> Value {
        if input.len() > ts_svelte::protocol::MAX_REQUEST_BYTES {
            return super::failure("size_limit", "Request exceeds 16 MiB");
        }
        let request: Request = match serde_json::from_slice(input) {
            Ok(r) => r,
            Err(e) => return super::failure("invalid_request", &e.to_string()),
        };
        match self.execute(request) {
            Ok(v) => json!({"ok":true,"result":v}),
            Err(e) => super::failure("session_error", &e),
        }
    }
    fn execute(&mut self, request: Request) -> Result<Value, String> {
        match request {
            Request::Configure {
                compatibility,
                runes,
                lint_config,
            } => {
                compatibility::resolve(&compatibility)?;
                crate::lint::validate_config(&lint_config)?;
                self.versions = compatibility;
                self.lint_config = lint_config;
                self.runes = runes;
                self.diagnostics.clear();
                Ok(json!({"invalidated":self.documents.keys().collect::<Vec<_>>()}))
            }
            Request::Open { document } => {
                if self.documents.contains_key(&document.uri) {
                    return Err("Document is already open".into());
                }
                if self.documents.len() >= 4096 {
                    return Err("Too many open documents".into());
                }
                if document.text.len() > ts_svelte_compiler::MAX_SOURCE_BYTES
                    || self.total_bytes() + document.text.len() > 32 * 1024 * 1024
                {
                    return Err("Session source limit exceeded".into());
                }
                let uri = document.uri.clone();
                self.documents.insert(uri.clone(), Document::new(document));
                self.diagnostics(&uri)
            }
            Request::Change {
                uri,
                version,
                edits,
            } => {
                let old = self.documents.get(&uri).ok_or("Document is not open")?;
                let next = old.edited(version, &edits).map_err(|e| e.to_string())?;
                if next.text().len() > ts_svelte_compiler::MAX_SOURCE_BYTES
                    || self.total_bytes() - old.text().len() + next.text().len() > 32 * 1024 * 1024
                {
                    return Err("Session source limit exceeded".into());
                }
                self.documents.insert(uri.clone(), next);
                self.diagnostics.remove(&uri);
                self.diagnostics(&uri)
            }
            Request::Close { uri } => {
                self.documents.remove(&uri);
                self.diagnostics.remove(&uri);
                Ok(json!({"uri":uri,"diagnostics":[]}))
            }
            Request::Diagnostics { uri } => self.diagnostics(&uri),
            Request::CodeActions { uri, params } => {
                let diagnostics = self.diagnostics(&uri)?;
                let document = self.documents.get(&uri).ok_or("Document is not open")?;
                crate::lint_actions::code_actions(document.snapshot(), &diagnostics, &params)
                    .map(|actions| json!(actions))
            }
            Request::Syntax {
                uri,
                method,
                params,
            } => {
                let document = self.documents.get(&uri).ok_or("Document is not open")?;
                let profile = compatibility::resolve(&self.versions)?;
                crate::syntax_editor::query(document.snapshot().clone(), &profile, &method, &params)
                    .map(|r| r.unwrap_or(Value::Null))
            }
            Request::Stats {} => Ok(
                json!({"documents":self.documents.len(),"sourceBytes":self.total_bytes(),"cachedDiagnostics":self.diagnostics.len(),"typeChecking":false}),
            ),
        }
    }
    fn total_bytes(&self) -> usize {
        self.documents.values().map(|d| d.text().len()).sum()
    }
    fn diagnostics(&mut self, uri: &str) -> Result<Value, String> {
        if let Some(cached) = self.diagnostics.get(uri) {
            return Ok(cached.clone());
        }
        let document = self.documents.get(uri).ok_or("Document is not open")?;
        let profile = compatibility::resolve(&self.versions)?;
        let response=super::dispatch(json!({"op":"lint","document":document.snapshot(),"runes":self.runes,"compatibility":self.versions,"lintConfig":self.lint_config}).to_string().as_bytes());
        let body = if response["ok"] == true {
            &response["result"]
        } else {
            &response
        };
        let mut diagnostics = body["diagnostics"].as_array().cloned().unwrap_or_default();
        if response["ok"] != true && diagnostics.is_empty() {
            diagnostics.push(json!({"code":response["error"]["code"],"message":response["error"]["message"],"severity":"error","uri":uri,"range":null}));
        }
        let result = json!({"uri":uri,"version":document.snapshot().version,"diagnostics":diagnostics,"compatibility":profile,"typeChecking":false});
        self.diagnostics.insert(uri.to_owned(), result.clone());
        Ok(result)
    }
}
