//! Persistent stdio LSP transport for the shared Rust document session.
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, BufRead, Read, Write};
use ts_svelte_tools::session::Session;

fn read_message(reader: &mut impl BufRead) -> io::Result<Option<Vec<u8>>> {
    let mut length = None;
    let mut header_bytes = 0;
    loop {
        let mut line = String::new();
        let bytes = reader.take(8193).read_line(&mut line)?;
        if bytes == 0 {
            return if header_bytes == 0 {
                Ok(None)
            } else {
                Err(io::Error::other("Truncated header"))
            };
        }
        header_bytes += bytes;
        if header_bytes > 8192 {
            return Err(io::Error::other("Header too large"));
        }
        if line == "\r\n" || line == "\n" {
            break;
        }
        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| io::Error::other("Invalid header"))?;
        if name.eq_ignore_ascii_case("Content-Length") {
            if length.is_some() {
                return Err(io::Error::other("Duplicate Content-Length"));
            }
            length = Some(value.trim().parse::<usize>().map_err(io::Error::other)?);
        }
    }
    let length = length.ok_or_else(|| io::Error::other("Missing Content-Length"))?;
    if length > 16 * 1024 * 1024 {
        return Err(io::Error::other("Message too large"));
    }
    let mut input = vec![0; length];
    reader.read_exact(&mut input)?;
    Ok(Some(input))
}
fn send(writer: &mut impl Write, value: &Value) -> io::Result<()> {
    let text = value.to_string();
    write!(writer, "Content-Length: {}\r\n\r\n{text}", text.len())?;
    writer.flush()
}
// Session snapshots encode signed LSP versions in u32. Restore the wire value
// only at versioned text-document edits, leaving unrelated JSON numbers intact.
fn restore_edit_versions(value: &mut Value, open: &BTreeSet<String>) {
    match value {
        Value::Array(items) => {
            for item in items {
                restore_edit_versions(item, open);
            }
        }
        Value::Object(object) => {
            if let Some(changes) = object
                .get_mut("documentChanges")
                .and_then(Value::as_array_mut)
            {
                for change in changes {
                    if let Some(document) = change
                        .get_mut("textDocument")
                        .and_then(Value::as_object_mut)
                    {
                        if let Some(version) = document.get("version").and_then(Value::as_u64) {
                            let is_open = document
                                .get("uri")
                                .and_then(Value::as_str)
                                .is_some_and(|uri| open.contains(uri));
                            document.insert(
                                "version".into(),
                                if is_open {
                                    json!(version as i64 - 2_147_483_648_i64)
                                } else {
                                    Value::Null
                                },
                            );
                        }
                    }
                }
            }
            for child in object.values_mut() {
                restore_edit_versions(child, open);
            }
        }
        _ => {}
    }
}
fn lsp_diagnostics(result: &Value) -> Vec<Value> {
    result["diagnostics"].as_array().into_iter().flatten().map(|d|json!({
        "range":d["range"].as_object().map_or_else(||json!({"start":{"line":0,"character":0},"end":{"line":0,"character":0}}),|_|d["range"].clone()),
        "severity":if d["severity"]=="error" {1}else{2},"code":d["code"],"source":"ts-rust-svelte","message":d["message"],"data":{"unmapped":d["range"].is_null()}
    })).collect()
}
fn wire_version(value: &Value) -> Value {
    value
        .as_i64()
        .filter(|v| i32::try_from(*v).is_ok())
        .map_or(Value::Null, |v| json!(v + 2_147_483_648_i64))
}
fn publish(writer: &mut impl Write, result: &Value) -> io::Result<()> {
    let mut params = json!({"uri":result["uri"],"diagnostics":lsp_diagnostics(result)});
    if let Some(version) = result["version"].as_i64() {
        params["version"] = json!(version - 2_147_483_648_i64);
    }
    send(
        writer,
        &json!({"jsonrpc":"2.0","method":"textDocument/publishDiagnostics","params":params}),
    )
}
fn session_call(session: &mut Session, request: &Value) -> Result<Value, String> {
    let response = session.request(request.to_string().as_bytes());
    if response["ok"] == true {
        Ok(response["result"].clone())
    } else {
        Err(response["error"]["message"]
            .as_str()
            .unwrap_or("Session failure")
            .into())
    }
}
// Keep transport ordering and its session state together in one dispatch loop.
#[allow(clippy::too_many_lines)]
fn run(default_project: Option<&Value>) -> io::Result<i32> {
    let mut reader = io::stdin().lock();
    let mut writer = io::stdout().lock();
    let mut session = Session::default();
    let mut project: Option<ts_svelte_tools::project_check::Config> = None;
    let mut semantic: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    let mut semantic_project: Option<ts_svelte_tools::editor_project::PersistentProject> = None;
    let mut semantic_settings = json!({});
    let mut project_reload_error: Option<String> = None;
    let mut refresh_capabilities = BTreeSet::new();
    let mut refresh_id = 0_u64;
    let mut initialized = false;
    let mut shutdown = false;
    let mut documents = BTreeSet::new();
    while let Some(input) = read_message(&mut reader)? {
        let message: Value = match serde_json::from_slice(&input) {
            Ok(v) => v,
            Err(e) => {
                send(
                    &mut writer,
                    &json!({"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":e.to_string()}}),
                )?;
                continue;
            }
        };
        // Replies to our refresh requests have no method and require no reply.
        if message.get("method").is_none()
            && (message.get("result").is_some() || message.get("error").is_some())
        {
            continue;
        }
        let id = message.get("id").cloned();
        let method = message["method"].as_str().unwrap_or("");
        let p = &message["params"];
        if method == "exit" {
            return Ok(i32::from(!shutdown));
        }
        let mut error_code = -32602;
        let result: Result<Value, String> = if message["jsonrpc"] != "2.0" {
            error_code = -32600;
            Err("Expected JSON-RPC 2.0".into())
        } else if shutdown {
            Err("Server has shut down".into())
        } else if method == "initialize" {
            if initialized {
                Err("Server already initialized".into())
            } else {
                for (capability, method) in [
                    ("inlayHint", "workspace/inlayHint/refresh"),
                    ("codeLens", "workspace/codeLens/refresh"),
                    ("diagnostics", "workspace/diagnostic/refresh"),
                ] {
                    if p["capabilities"]["workspace"][capability]["refreshSupport"] == true {
                        refresh_capabilities.insert(method.to_owned());
                    }
                }
                let options = &p["initializationOptions"];
                let project_config = options
                    .get("project")
                    .filter(|v| !v.is_null())
                    .or(default_project)
                    .map(|v| {
                        serde_json::from_value::<ts_svelte_tools::project_check::Config>(v.clone())
                            .map_err(|e| e.to_string())
                            .and_then(|mut c| {
                                c.validate()?;
                                Ok(c)
                            })
                    })
                    .transpose();
                let configured = project_config.and_then(|config| {
                    let request = match &config {
                        Some(c) => c.session_configuration(options)?,
                        None => json!({"op":"sessionConfigure","compatibility":options.get("compatibility").cloned().unwrap_or(json!({})),"runes":options["runes"],"lintConfig":options["lintConfig"]}),
                    };
                    let result = session_call(&mut session, &request)?;
                    project = config;
                    Ok(result)
                });
                configured.map(|_| {
                    initialized = true;
                    json!({"capabilities": {
                        "positionEncoding":"utf-16",
                        "textDocumentSync":{"openClose":true,"change":2,"save":{"includeText":false}},
                        "diagnosticProvider":{"identifier":"ts-rust-svelte","interFileDependencies":project.is_some(),"workspaceDiagnostics":false},
                        "hoverProvider":project.is_some(),
                        "completionProvider":if project.is_some(){json!({"resolveProvider":true,"triggerCharacters":[".","\"","'","/","@"]})}else{Value::Null},
                        "definitionProvider":project.is_some(),"typeDefinitionProvider":project.is_some(),"referencesProvider":project.is_some(),
                        "renameProvider":if project.is_some(){json!({"prepareProvider":true})}else{Value::Null},
                        "signatureHelpProvider":if project.is_some(){json!({"triggerCharacters":["(",",","<"],"retriggerCharacters":[")"]})}else{Value::Null},
                        "documentHighlightProvider":project.is_some(),"documentSymbolProvider":true,
                        "documentFormattingProvider":ts_svelte_tools::format_host::available(),
                        "selectionRangeProvider":true,"foldingRangeProvider":true,
                        "semanticTokensProvider":if project.is_some(){json!({"legend":ts_svelte_tools::editor_project::semantic_legend(),"full":true,"range":true})}else{Value::Null},
                        "codeActionProvider":{"codeActionKinds":["quickfix","source.organizeImports","source.fixAll","source.fixAll.svelte"],"resolveProvider":false},
                        "inlayHintProvider":project.is_some(),"linkedEditingRangeProvider":true
                    },"serverInfo":{"name":"ts-rust-svelte","version":"0.1.0"}})
                })
            }
        } else if !initialized {
            error_code = -32002;
            Err("Server not initialized".into())
        } else {
            match method {
                "textDocument/formatting" => (|| {
                    let uri = p["textDocument"]["uri"]
                        .as_str()
                        .ok_or("Missing document URI")?;
                    let snapshots = session.snapshots();
                    let snapshot = snapshots
                        .iter()
                        .find(|snapshot| snapshot.uri == uri)
                        .ok_or("Formatting requires an open document")?;
                    ts_svelte_tools::format_host::format(snapshot, &p["options"])
                })(),
                "initialized" | "$/cancelRequest" | "$/setTrace" => Ok(Value::Null),
                "workspace/didChangeWatchedFiles" => (|| {
                    if !p["changes"].is_array() {
                        return Err("Expected watched file changes".into());
                    }
                    if let Some(config) = &project {
                        let refreshed = config.rediscover().and_then(|next| {
                            if let Some(next) = &next {
                                let request = next.session_configuration(&semantic_settings)?;
                                session_call(&mut session, &request)?;
                            }
                            Ok(next)
                        });
                        match refreshed {
                            Ok(Some(next)) => {
                                semantic_project = None;
                                semantic.clear();
                                project_reload_error = None;
                                project = Some(next);
                            }
                            Ok(None) => {}
                            Err(error) => {
                                semantic_project = None;
                                project_reload_error = Some(error);
                            }
                        }
                    }
                    Ok(Value::Null)
                })(),
                "shutdown" => {
                    semantic_project = None;
                    shutdown = true;
                    Ok(Value::Null)
                }
                "textDocument/didOpen" => {
                    let d = &p["textDocument"];
                    let valid = project.as_ref().map_or(Ok(()), |config| {
                        config.validate_open_uri(
                            d["uri"].as_str().unwrap_or(""),
                            &session.snapshots(),
                        )
                    });
                    let response = valid.and_then(|()| session_call(&mut session,
                        &json!({"op":"sessionOpen","document":{"uri":d["uri"],"version":wire_version(&d["version"]),"text":d["text"]}})));
                    if let Ok(result) = &response {
                        documents.insert(d["uri"].as_str().unwrap_or("").to_owned());
                        if project.is_none() {
                            publish(&mut writer, result)?;
                        }
                    }
                    response
                }
                "textDocument/didChange" => {
                    let d = &p["textDocument"];
                    let edits = p["contentChanges"].as_array().map(|changes| {
                        changes
                            .iter()
                            .map(|c| json!({"range":c["range"],"text":c["text"]}))
                            .collect::<Vec<_>>()
                    });
                    let response = session_call(
                        &mut session,
                        &json!({"op":"sessionChange","uri":d["uri"],"version":wire_version(&d["version"]),"edits":edits}),
                    );
                    if let Ok(result) = &response
                        && project.is_none()
                    {
                        publish(&mut writer, result)?;
                    }
                    response
                }
                "textDocument/didSave" => {
                    let response = session_call(
                        &mut session,
                        &json!({"op":"sessionDiagnostics","uri":p["textDocument"]["uri"]}),
                    );
                    if let Ok(result) = &response
                        && project.is_none()
                    {
                        publish(&mut writer, result)?;
                    }
                    response
                }
                "textDocument/didClose" => {
                    let uri = &p["textDocument"]["uri"];
                    semantic.remove(uri.as_str().unwrap_or(""));
                    let response =
                        session_call(&mut session, &json!({"op":"sessionClose","uri":uri}));
                    if let Ok(result) = &response {
                        documents.remove(uri.as_str().unwrap_or(""));
                        publish(&mut writer, result)?;
                    }
                    response
                }
                "textDocument/diagnostic" => session_call(
                    &mut session,
                    &json!({"op":"sessionDiagnostics","uri":p["textDocument"]["uri"]}),
                )
                .map(|mut r| {
                    let uri = r["uri"].as_str().unwrap_or("").to_owned();
                    if let Some(d) = r["diagnostics"].as_array_mut() {
                        d.extend(semantic.get(&uri).cloned().unwrap_or_default());
                    }
                    json!({"kind":"full","items":lsp_diagnostics(&r)})
                }),
                "workspace/didChangeConfiguration" => {
                    let settings = &p["settings"];
                    semantic_settings = settings.clone();
                    if let Some(host) = semantic_project.as_mut() {
                        match host.configure(semantic_settings.clone()) {
                            Ok(response) => {
                                for method in response["refresh"]
                                    .as_array()
                                    .into_iter()
                                    .flatten()
                                    .filter_map(Value::as_str)
                                {
                                    if refresh_capabilities.contains(method) {
                                        refresh_id += 1;
                                        send(
                                            &mut writer,
                                            &json!({"jsonrpc":"2.0","id":format!("svelte-refresh-{refresh_id}"),"method":method}),
                                        )?;
                                    }
                                }
                            }
                            Err(error) => send(
                                &mut writer,
                                &json!({"jsonrpc":"2.0","method":"window/logMessage","params":{"type":1,"message":error}}),
                            )?,
                        }
                    }
                    let request = match &project {
                        Some(c) => c.session_configuration(settings),
                        None => Ok(
                            json!({"op":"sessionConfigure","compatibility":settings.get("compatibility").cloned().unwrap_or(json!({})),"runes":settings["runes"],"lintConfig":settings["lintConfig"]}),
                        ),
                    };
                    let response = request.and_then(|request| session_call(&mut session, &request));
                    if response.is_ok() && project.is_none() {
                        for uri in &documents {
                            if let Ok(result) = session_call(
                                &mut session,
                                &json!({"op":"sessionDiagnostics","uri":uri}),
                            ) {
                                publish(&mut writer, &result)?;
                            }
                        }
                    }
                    response
                }
                "textDocument/hover"
                | "textDocument/completion"
                | "completionItem/resolve"
                | "textDocument/definition"
                | "textDocument/typeDefinition"
                | "textDocument/references"
                | "textDocument/prepareRename"
                | "textDocument/rename"
                | "textDocument/signatureHelp"
                | "textDocument/documentHighlight"
                | "textDocument/documentSymbol"
                | "textDocument/selectionRange"
                | "textDocument/foldingRange"
                | "textDocument/semanticTokens/full"
                | "textDocument/semanticTokens/range"
                | "textDocument/codeAction"
                | "textDocument/inlayHint"
                | "textDocument/linkedEditingRange" => (|| {
                    if let Some(error) = &project_reload_error {
                        return Err(error.clone());
                    }
                    let native_actions = if method == "textDocument/codeAction" {
                        Some(session_call(
                            &mut session,
                            &json!({"op":"sessionCodeActions","uri":p["textDocument"]["uri"],"params":p}),
                        )?)
                    } else {
                        None
                    };
                    if let Some(actions) = &native_actions {
                        let native_only = p
                            .pointer("/context/only")
                            .and_then(Value::as_array)
                            .is_some_and(|kinds| {
                                !kinds.is_empty()
                                    && kinds.iter().all(|k| k == "source.fixAll.svelte")
                            });
                        if project.is_none() || native_only {
                            return Ok(actions.clone());
                        }
                    }
                    if project.is_none()
                        && matches!(
                            method,
                            "textDocument/linkedEditingRange"
                                | "textDocument/foldingRange"
                                | "textDocument/documentSymbol"
                                | "textDocument/selectionRange"
                        )
                    {
                        return session_call(
                            &mut session,
                            &json!({"op":"sessionSyntax","uri":p["textDocument"]["uri"],"method":method,"params":p}),
                        );
                    }
                    let config = project
                        .as_ref()
                        .ok_or("Semantic editor operations require a configured project")?;
                    let snapshots = session.snapshots();
                    if semantic_project.is_none() {
                        semantic_project =
                            Some(ts_svelte_tools::editor_project::PersistentProject::start(
                                config, &snapshots,
                            )?);
                        semantic_project
                            .as_mut()
                            .ok_or("Missing semantic project")?
                            .configure(semantic_settings.clone())?;
                    }
                    let mut result = semantic_project
                        .as_mut()
                        .ok_or("Missing semantic project")?
                        .query(config, &snapshots, method, p)?;
                    if let Some(actions) = native_actions {
                        if result.is_null() {
                            result = json!([]);
                        }
                        result
                            .as_array_mut()
                            .ok_or("Invalid semantic code actions")?
                            .extend(
                                actions
                                    .as_array()
                                    .ok_or("Invalid native code actions")?
                                    .iter()
                                    .cloned(),
                            );
                    }
                    Ok(result)
                })(),
                "svelte/sessionStats" => session_call(&mut session, &json!({"op":"sessionStats"})),
                _ => {
                    error_code = -32601;
                    Err(format!("Unsupported method: {method}"))
                }
            }
        };
        if result.is_ok()
            && matches!(
                method,
                "textDocument/didOpen"
                    | "textDocument/didChange"
                    | "textDocument/didClose"
                    | "textDocument/didSave"
                    | "workspace/didChangeConfiguration"
                    | "workspace/didChangeWatchedFiles"
            )
            && let Some(config) = &project
        {
            let checked = (|| {
                if let Some(error) = &project_reload_error {
                    return Err(error.clone());
                }
                let snapshots = session.snapshots();
                if semantic_project.is_none() {
                    semantic_project =
                        Some(ts_svelte_tools::editor_project::PersistentProject::start(
                            config, &snapshots,
                        )?);
                    semantic_project
                        .as_mut()
                        .ok_or("Missing semantic project")?
                        .configure(semantic_settings.clone())?;
                }
                semantic_project
                    .as_mut()
                    .ok_or("Missing semantic project")?
                    .check(config, &snapshots)
            })();
            if checked.is_err() {
                semantic_project = None;
            }
            semantic=match &checked {Ok(d)=>d.clone(),Err(message)=>documents.iter().map(|uri|(uri.clone(),vec![json!({"code":"incomplete_project_check","message":message,"severity":"error","range":null})])).collect()};
            for uri in &documents {
                if let Ok(mut combined) =
                    session_call(&mut session, &json!({"op":"sessionDiagnostics","uri":uri}))
                {
                    if let Some(diagnostics) = combined["diagnostics"].as_array_mut() {
                        match &checked {
                                Ok(by_uri)=>diagnostics.extend(by_uri.get(uri).cloned().unwrap_or_default()),
                                Err(message)=>diagnostics.push(json!({"code":"incomplete_project_check","message":message,"severity":"error","range":null})),
                            }
                    }
                    publish(&mut writer, &combined)?;
                }
            }
        }
        if let Some(id) = id {
            send(
                &mut writer,
                &match result {
                    Ok(mut result) => {
                        restore_edit_versions(&mut result, &documents);
                        json!({"jsonrpc":"2.0","id":id,"result":result})
                    }
                    Err(message) => {
                        json!({"jsonrpc":"2.0","id":id,"error":{"code":error_code,"message":message}})
                    }
                },
            )?;
        } else if let Err(message) = result {
            send(
                &mut writer,
                &json!({"jsonrpc":"2.0","method":"window/logMessage","params":{"type":1,"message":message}}),
            )?;
        }
    }
    Ok(i32::from(!shutdown))
}
fn main() -> io::Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("--help") {
        println!(
            "Usage: svelte-lsp\nPersistent stdio LSP: native Svelte compiler and script lint diagnostics, UTF-16 incremental edits. Optional initializationOptions.project enables bounded explicit-project native type checking."
        );
        return Ok(());
    }
    let project = match args.as_slice() {
        [] => None,
        [flag, path] if flag == "--project-config" || flag == "--project-config-delete" => {
            let file = std::fs::File::open(path)?;
            // Only the explicit owned-temporary-file mode consumes its input.
            // Unlink before parsing so malformed configuration is cleaned up too.
            if flag == "--project-config-delete" {
                std::fs::remove_file(path)?;
            }
            let mut bytes = Vec::new();
            std::io::Read::take(file, 4 * 1024 * 1024 + 1).read_to_end(&mut bytes)?;
            if bytes.len() > 4 * 1024 * 1024 {
                return Err(io::Error::other("Project configuration exceeds 4 MiB"));
            }
            Some(serde_json::from_slice(&bytes).map_err(io::Error::other)?)
        }
        _ => {
            return Err(io::Error::other(
                "Usage: svelte-lsp [--project-config FILE]",
            ));
        }
    };
    #[cfg(unix)]
    ts_svelte_tools::project_check::install_termination_handler()?;
    std::process::exit(run(project.as_ref())?);
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn edits_restore_signed_open_versions_and_clear_closed_versions() {
        let mut edit = json!({"edit":{"documentChanges":[
            {"textDocument":{"uri":"file:///open.svelte","version":2147483645_u64},"edits":[]},
            {"textDocument":{"uri":"file:///closed.svelte","version":0},"edits":[]}
        ]},"data":{"textDocument":{"uri":"opaque","version":44}}});
        restore_edit_versions(
            &mut edit,
            &["file:///open.svelte".to_owned()].into_iter().collect(),
        );
        assert_eq!(
            edit["edit"]["documentChanges"][0]["textDocument"]["version"],
            -3
        );
        assert!(edit["edit"]["documentChanges"][1]["textDocument"]["version"].is_null());
        assert_eq!(edit["data"]["textDocument"]["version"], 44);
    }
}
