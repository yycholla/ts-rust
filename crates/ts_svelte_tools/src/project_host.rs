//! Transactional JSON boundary shared by native tests and the WASM worker.
use crate::wasm_project::WasmProject;
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Default)]
pub struct ProjectHost {
    active: Option<WasmProject>,
    pending: Option<WasmProject>,
}

#[cfg(test)]
mod tests {
    use super::*;
    fn input(version: i32, text: &str) -> Value {
        json!({"root":"/project","files":{"/project/main.ts":text},
            "rootFiles":["main.ts"],"documents":[{"uri":"file:///project/main.ts","version":version,"text":text}]})
    }
    fn call(host: &mut ProjectHost, request: Value) -> Value {
        host.request(request.to_string().as_bytes())
    }
    #[test]
    fn replacement_cannot_publish_until_matching_commit_and_discard_preserves_active() {
        let mut host = ProjectHost::default();
        assert_eq!(
            call(
                &mut host,
                json!({"op":"projectCreate","snapshot":input(-7,"let value = 1;")})
            )["ok"],
            true
        );
        let plan = call(
            &mut host,
            json!({"op":"projectReplace","snapshot":input(-6,"let value = 2;")}),
        );
        assert_eq!(plan["result"]["prepared"]["revision"], 2);
        assert_eq!(
            call(&mut host, json!({"op":"projectPrepared"}))["ok"],
            false
        );
        assert_eq!(
            call(&mut host, json!({"op":"projectCommit","revision":3}))["ok"],
            false
        );
        assert_eq!(call(&mut host, json!({"op":"projectDiscard"}))["ok"], true);
        assert_eq!(
            call(&mut host, json!({"op":"projectPrepared"}))["result"]["revision"],
            1
        );
        assert_eq!(
            call(
                &mut host,
                json!({"op":"projectReplace","snapshot":input(-6,"let value = 2;")})
            )["ok"],
            true
        );
        assert_eq!(
            call(&mut host, json!({"op":"projectCommit","revision":2}))["result"]["revision"],
            2
        );
        assert_eq!(
            call(
                &mut host,
                json!({"op":"projectReplace","snapshot":input(-7,"let value = 1;")})
            )["ok"],
            false
        );
        assert_eq!(
            call(&mut host, json!({"op":"projectPrepared"}))["result"]["revision"],
            2
        );
    }
}
#[derive(Deserialize)]
#[serde(tag = "op", deny_unknown_fields)]
enum Request {
    #[serde(rename = "projectCreate")]
    Create { snapshot: Value },
    #[serde(rename = "projectPrepared")]
    Prepared {},
    #[serde(rename = "projectQuery")]
    Query { method: String, params: Value },
    #[serde(rename = "projectRespond")]
    Respond { ticket: u64, response: Value },
    #[serde(rename = "projectReplace")]
    Replace { snapshot: Value },
    #[serde(rename = "projectCommit")]
    Commit { revision: u32 },
    #[serde(rename = "projectModuleComplete")]
    ModuleComplete {
        uri: String,
        result: Value,
        #[serde(default)]
        candidate: bool,
    },
    #[serde(rename = "projectDiscard")]
    Discard {},
    #[serde(rename = "projectDispose")]
    Dispose {},
}
impl ProjectHost {
    fn active(&mut self) -> Result<&mut WasmProject, String> {
        if self.pending.is_some() {
            return Err("Project replacement is pending; semantic queries are paused".into());
        }
        self.active
            .as_mut()
            .ok_or_else(|| "Project is not initialized".into())
    }
    pub fn request(&mut self, bytes: &[u8]) -> Value {
        let result = (|| -> Result<Value, String> {
            if bytes.len() > ts_svelte::protocol::MAX_REQUEST_BYTES {
                return Err("Project request exceeds 16 MiB".into());
            }
            let request: Request = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
            match request {
                Request::Create { snapshot } => {
                    if self.active.is_some() {
                        return Err("Project is already initialized".into());
                    }
                    let project = WasmProject::new(snapshot)?;
                    let prepared = project.prepared();
                    self.active = Some(project);
                    Ok(prepared)
                }
                Request::Prepared {} => Ok(self.active()?.prepared()),
                Request::Query { method, params } => self.active()?.query(&method, &params),
                Request::Respond { ticket, response } => self.active()?.respond(ticket, response),
                Request::Replace { snapshot } => {
                    let (candidate, sync) = self.active()?.replacement(snapshot)?;
                    let plan = json!({"sync":sync,"prepared":candidate.prepared()});
                    self.pending = Some(candidate);
                    Ok(plan)
                }
                Request::Commit { revision } => {
                    let candidate = self.pending.as_ref().ok_or("No pending replacement")?;
                    if candidate.prepared()["revision"] != revision {
                        return Err("Pending revision does not match commit".into());
                    }
                    self.active = self.pending.take();
                    Ok(self.active()?.prepared())
                }
                Request::ModuleComplete {
                    uri,
                    result,
                    candidate,
                } => {
                    if candidate {
                        self.pending
                            .as_mut()
                            .ok_or("No pending replacement")?
                            .complete_module(&uri, &result)
                    } else {
                        self.active()?.complete_module(&uri, &result)
                    }
                }
                Request::Discard {} => {
                    self.pending = None;
                    Ok(Value::Null)
                }
                Request::Dispose {} => {
                    self.active = None;
                    self.pending = None;
                    Ok(Value::Null)
                }
            }
        })();
        match result {
            Ok(result) => json!({"ok":true,"result":result}),
            Err(message) => json!({"ok":false,"error":{"code":"project_error","message":message}}),
        }
    }
}
