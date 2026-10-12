//! Persistent native semantic project behind the framework-neutral editor host.
use crate::editor_map::EditorMap;
use crate::project_check::{Config, Workspace};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::Receiver;
use std::time::Duration;
use ts_svelte::{ByteRange, Mapping, Position, Snapshot};

/// Fixed full engine legend; initialization verifies the child agrees.
pub fn semantic_legend() -> Value {
    json!({"tokenTypes":["namespace","class","enum","interface","struct","typeParameter","type","parameter","variable","property","enumMember","decorator","event","function","method","macro","label","comment","string","keyword","number","regexp","operator"],"tokenModifiers":["declaration","definition","readonly","static","deprecated","abstract","async","modification","documentation","defaultLibrary","local"]})
}
const LIMIT: u64 = 64 * 1024 * 1024;
struct Client {
    child: Child,
    input: ChildStdin,
    output: Receiver<Result<Value, String>>,
    next_id: u64,
}
impl Client {
    fn start() -> Result<Self, String> {
        let binary = std::env::current_exe()
            .map_err(|e| e.to_string())?
            .with_file_name(format!("svelte-ts-service{}", std::env::consts::EXE_SUFFIX));
        let mut command = Command::new(binary);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        #[cfg(unix)]
        let mut registration = crate::project_check::ACTIVE_CHECK
            .lock()
            .map_err(|_| "Poisoned process registry")?;
        let mut child = command.spawn().map_err(|e| e.to_string())?;
        #[cfg(unix)]
        {
            registration.as_mut().ok_or("Missing editor workspace")?.pid = Some(child.id());
        }
        let input = child.stdin.take().ok_or("Missing semantic input")?;
        let stdout = child.stdout.take().ok_or("Missing semantic output")?;
        let (tx, output) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                let mut line = Vec::new();
                let read = reader.by_ref().take(LIMIT + 1).read_until(b'\n', &mut line);
                let result = match read {
                    Ok(0) => break,
                    Ok(_) if line.len() as u64 > LIMIT => {
                        Err("Semantic response exceeds 64 MiB".into())
                    }
                    Ok(_) => serde_json::from_slice(&line).map_err(|e| e.to_string()),
                    Err(e) => Err(e.to_string()),
                };
                let failed = result.is_err();
                if tx.send(result).is_err() || failed {
                    break;
                }
            }
        });
        Ok(Self {
            child,
            input,
            output,
            next_id: 0,
        })
    }
    fn request(&mut self, op: &str, params: Value) -> Result<Value, String> {
        self.next_id += 1;
        let message = json!({"id":self.next_id,"op":op,"params":params});
        let data = serde_json::to_vec(&message).map_err(|e| e.to_string())?;
        if data.len() as u64 > LIMIT {
            return Err("Semantic request exceeds 64 MiB".into());
        }
        self.input
            .write_all(&data)
            .and_then(|()| self.input.write_all(b"\n"))
            .and_then(|()| self.input.flush())
            .map_err(|e| e.to_string())?;
        let response = self
            .output
            .recv_timeout(Duration::from_secs(60))
            .map_err(|e| format!("Persistent semantic service unavailable: {e}"))??;
        if response["id"] != self.next_id {
            return Err("Semantic response ID mismatch".into());
        }
        if response["ok"] != true {
            return Err(format!(
                "Semantic operation {op} failed: {}",
                response["error"]
            ));
        }
        Ok(response["result"].clone())
    }
}
impl Drop for Client {
    fn drop(&mut self) {
        #[cfg(unix)]
        let mut guard = crate::project_check::ACTIVE_CHECK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _ = self.child.kill();
        let _ = self.child.wait();
        #[cfg(unix)]
        if let Some(active) = guard.as_mut() {
            active.pid = None;
        }
    }
}

pub struct PersistentProject {
    // Child must stop before the backing workspace is deleted.
    client: Client,
    _workspace: Workspace,
    directory: PathBuf,
    mappings: EditorMap,
    projections: BTreeMap<String, Value>,
    sources: BTreeMap<String, Snapshot>,
    virtual_uris: BTreeMap<String, String>,
    versions: BTreeMap<String, i32>,
    compatibility: Value,
    module_diagnostics: BTreeMap<String, Vec<Value>>,
    pub suppressed_diagnostics: Vec<Value>,
    completions: BTreeMap<u64, (String, Value)>,
    next_completion: u64,
}
fn uri(path: &Path) -> Result<String, String> {
    url::Url::from_file_path(path)
        .map(String::from)
        .map_err(|()| "Invalid file URI".into())
}
fn register(map: &mut EditorMap, projection: &Value, replace: bool) -> Result<(), String> {
    let document: Snapshot =
        serde_json::from_value(projection["document"].clone()).map_err(|e| e.to_string())?;
    let mappings = projection["mappings"]
        .as_array()
        .ok_or("Missing mappings")?
        .iter()
        .map(|m| {
            Ok(Mapping {
                source: serde_json::from_value::<ByteRange>(m["source"].clone())
                    .map_err(|e| e.to_string())?,
                generated: serde_json::from_value::<ByteRange>(m["generated"].clone())
                    .map_err(|e| e.to_string())?,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    let generated = uri(Path::new(
        projection["fileName"]
            .as_str()
            .ok_or("Missing projection path")?,
    ))?;
    let code = projection["code"]
        .as_str()
        .ok_or("Missing projection code")?
        .to_owned();
    if replace {
        map.replace(document, generated, code, mappings)
    } else {
        map.register(document, generated, code, mappings)
    }
    .map_err(|e| e.to_string())
}
impl PersistentProject {
    pub fn start(config: &Config, documents: &[Snapshot]) -> Result<Self, String> {
        let (workspace, prepared) = config.prepare(documents)?;
        let directory = PathBuf::from(
            prepared["workspace"]
                .as_str()
                .ok_or("Missing project workspace")?,
        );
        let mut mappings = EditorMap::new();
        let mut projections = BTreeMap::new();
        let mut sources = BTreeMap::new();
        let mut virtual_uris = BTreeMap::new();
        for p in prepared["projections"]
            .as_array()
            .ok_or("Missing projections")?
        {
            register(&mut mappings, p, false)?;
            let d: Snapshot =
                serde_json::from_value(p["document"].clone()).map_err(|e| e.to_string())?;
            virtual_uris.insert(
                d.uri.clone(),
                uri(Path::new(
                    p["fileName"]
                        .as_str()
                        .ok_or("Missing projection filename")?,
                ))?,
            );
            projections.insert(d.uri.clone(), p.clone());
            sources.insert(d.uri.clone(), d);
        }
        for path in config.files.iter().chain(&config.auxiliary_files) {
            let d = config.source(path, documents)?;
            let generated = uri(&directory.join(path))?;
            mappings
                .register_identity(d.clone(), generated.clone())
                .map_err(|e| e.to_string())?;
            virtual_uris.insert(d.uri.clone(), generated);
            sources.insert(d.uri.clone(), d);
        }
        let mut client = Client::start()?;
        let initialized = client.request(
            "initialize",
            json!({"root":directory,"project":prepared["project"]}),
        )?;
        if initialized["semanticTokensLegend"] != semantic_legend() {
            return Err("Semantic token legend differs from host contract".into());
        }
        Ok(Self {
            client,
            _workspace: workspace,
            directory,
            mappings,
            projections,
            sources,
            virtual_uris,
            versions: BTreeMap::new(),
            compatibility: config.session_configuration(&json!({}))?["compatibility"].clone(),
            module_diagnostics: BTreeMap::new(),
            suppressed_diagnostics: Vec::new(),
            completions: BTreeMap::new(),
            next_completion: 0,
        })
    }
    pub fn update(&mut self, config: &Config, documents: &[Snapshot]) -> Result<(), String> {
        config.validate_snapshot(documents)?;
        let mut changed = Vec::new();
        for (original, old) in &self.sources {
            let path = url::Url::parse(original)
                .map_err(|e| e.to_string())?
                .to_file_path()
                .map_err(|()| "Invalid original file URI")?;
            let relative = path.strip_prefix(&config.root).map_err(|e| e.to_string())?;
            let d = config.source(relative.to_str().ok_or("Non-UTF8 source")?, documents)?;
            if d.uri != old.uri || d.text != old.text || d.version != old.version {
                changed.push((original.clone(), d));
            }
        }
        if !changed.is_empty() {
            self.completions.clear();
        }
        for (previous_uri, d) in changed {
            let original = d.uri.clone();
            self.module_diagnostics.remove(&previous_uri);
            let generated = self
                .virtual_uris
                .get(&previous_uri)
                .ok_or("Missing virtual URI")?
                .clone();
            let code = if let Some(previous) = self.projections.get(&previous_uri) {
                let typescript = previous["fileName"]
                    .as_str()
                    .is_some_and(|s| s.ends_with(".tsx"));
                let response=ts_svelte_compiler::dispatch(json!({"op":"project","document":d,"typescript":typescript,"runes":config.runes,"compatibility":self.compatibility}).to_string().as_bytes());
                if response["ok"] != true {
                    return Err(format!("Cannot update checking projection: {response}"));
                }
                let mut projection = previous.clone();
                projection["document"] = json!(d);
                projection["code"] = response["result"]["code"].clone();
                projection["mappings"] = response["result"]["mappings"].clone();
                let mut code = projection["code"]
                    .as_str()
                    .ok_or("Projection missing code")?
                    .to_owned();
                for directive in [
                    "///<reference types=\"svelte\" />",
                    "/// <reference types=\"svelte\" />",
                ] {
                    if code.starts_with(directive) {
                        code.replace_range(..directive.len(), &" ".repeat(directive.len()));
                    }
                }
                projection["code"] = json!(code);
                register(&mut self.mappings, &projection, true)?;
                self.projections.remove(&previous_uri);
                self.projections.insert(original.clone(), projection);
                code
            } else {
                self.mappings
                    .replace_identity(d.clone(), generated.clone())
                    .map_err(|e| e.to_string())?;
                d.text.clone()
            };
            let next = self.versions.get(&generated).copied().unwrap_or(0) + 1;
            if self.versions.contains_key(&generated) {
                self.client.request("change",json!({"textDocument":{"uri":generated,"version":next},"contentChanges":[{"text":code}]}))?;
            } else {
                self.client.request("open",json!({"textDocument":{"uri":generated,"version":next,"languageId":if generated.ends_with(".jsx"){"javascriptreact"}else{"typescript"},"text":code}}))?;
            }
            self.mappings
                .set_generated_version(&generated, next as u32)
                .map_err(|e| e.to_string())?;
            self.versions.insert(generated.clone(), next);
            self.virtual_uris.remove(&previous_uri);
            self.virtual_uris.insert(original.clone(), generated);
            self.sources.remove(&previous_uri);
            self.sources.insert(original, d);
        }
        Ok(())
    }
    pub fn check(
        &mut self,
        config: &Config,
        documents: &[Snapshot],
    ) -> Result<BTreeMap<String, Vec<Value>>, String> {
        self.update(config, documents)?;
        let first = self
            .virtual_uris
            .values()
            .next()
            .ok_or("Empty semantic project")?;
        let response = self.client.request(
            "projectDiagnostics",
            json!({"uri":first,"projections":self.projections.values().collect::<Vec<_>>()}),
        )?;
        self.suppressed_diagnostics = response["suppressedDiagnostics"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let mut result: BTreeMap<String, Vec<Value>> = BTreeMap::new();
        for diagnostic in response["diagnostics"]
            .as_array()
            .ok_or("Missing project diagnostics")?
        {
            let original = &diagnostic["original"];
            let source_uri = original["uri"].as_str().map(str::to_owned).or_else(|| {
                diagnostic["fileName"]
                    .as_str()
                    .and_then(|p| Path::new(p).strip_prefix(&self.directory).ok())
                    .and_then(|p| uri(&config.root.join(p)).ok())
            });
            let source_uri =
                source_uri.ok_or_else(|| format!("Unattached project diagnostic: {diagnostic}"))?;
            let range = if original["status"] == "mapped" {
                original["range"].clone()
            } else if original.is_null() {
                json!({"start":diagnostic["startPosition"],"end":diagnostic["endPosition"]})
            } else {
                Value::Null
            };
            result.entry(source_uri.clone()).or_default().push(json!({"uri":source_uri,"range":range,"code":diagnostic["code"],"severity":if diagnostic["category"]==1{"error"}else{"warning"},"message":diagnostic["text"],"checker":diagnostic}));
        }
        if response["projectDiagnostics"]
            .as_array()
            .is_some_and(|v| !v.is_empty())
        {
            return Err(format!(
                "Project configuration/global diagnostics: {}",
                response["projectDiagnostics"]
            ));
        }
        let loaded = response["loadedFiles"]
            .as_array()
            .ok_or("Missing loaded files")?;
        for (original, document) in &self.sources {
            if !(original.ends_with(".svelte.ts") && !original.ends_with(".d.svelte.ts")
                || original.ends_with(".svelte.js"))
            {
                continue;
            }
            let virtual_uri = &self.virtual_uris[original];
            let path = url::Url::parse(virtual_uri)
                .map_err(|e| e.to_string())?
                .to_file_path()
                .map_err(|()| "Invalid virtual module URI")?;
            if !loaded.contains(&json!(path)) {
                continue;
            }
            if !self.module_diagnostics.contains_key(original) {
                let mut module=ts_svelte_compiler::dispatch(json!({"op":"checkModule","document":document,"typescript":original.ends_with(".ts"),"compatibility":self.compatibility}).to_string().as_bytes());
                let mut transpilation = None;
                if module["error"]["code"] == "unsupported_module_typescript" {
                    let transpiled = self.client.request(
                        "transpileModule",
                        json!({"fileName":path,"source":document.text}),
                    )?;
                    if transpiled["ok"] != true {
                        return Err(format!("Rune module transpilation failed: {transpiled}"));
                    }
                    let code = transpiled["code"]
                        .as_str()
                        .ok_or("Missing transpiled module")?
                        .to_owned();
                    module=ts_svelte_compiler::dispatch(json!({"op":"checkModule","document":{"uri":original,"version":document.version,"text":code},"typescript":false,"compatibility":self.compatibility}).to_string().as_bytes());
                    transpilation = Some((
                        code,
                        transpiled["map"]
                            .as_str()
                            .ok_or("Missing transpile map")?
                            .to_owned(),
                    ));
                }
                let body = if module["ok"] == true {
                    &module["result"]
                } else {
                    &module
                };
                let mut diagnostics = body["diagnostics"].as_array().cloned().unwrap_or_default();
                if module["ok"] != true && diagnostics.is_empty() {
                    return Err(format!("Rune module analysis incomplete: {module}"));
                }
                if let Some((code, map)) = transpilation {
                    diagnostics = diagnostics
                        .into_iter()
                        .map(|d| ts_svelte_mapping::map_diagnostic(document, &code, &map, d))
                        .collect::<Result<Vec<_>, _>>()?;
                }
                self.module_diagnostics
                    .insert(original.clone(), diagnostics);
            }
            result
                .entry(original.clone())
                .or_default()
                .extend(self.module_diagnostics[original].clone());
        }
        Ok(result)
    }
    pub fn configure(&mut self, settings: Value) -> Result<Value, String> {
        self.client.request("configure", settings)
    }
    pub fn query(
        &mut self,
        config: &Config,
        documents: &[Snapshot],
        method: &str,
        params: &Value,
    ) -> Result<Value, String> {
        self.update(config, documents)?;
        if method == "completionItem/resolve" {
            let key = params["data"]["tsRustCompletion"]
                .as_u64()
                .ok_or("Unknown completion resolution token")?;
            let (generated, item) = self
                .completions
                .get(&key)
                .cloned()
                .ok_or("Completion snapshot is stale; request completions again")?;
            let response = self.client.request("completionResolve", item)?;
            let mut mapped = self
                .mappings
                .map_completions(&generated, response)
                .map_err(|e| e.to_string())?;
            mapped["data"] = json!({"tsRustCompletion":key});
            return Ok(mapped);
        }
        let original = params["textDocument"]["uri"]
            .as_str()
            .ok_or("Missing document URI")?;
        let syntax = if original.ends_with(".svelte") {
            let profile = ts_svelte::compatibility::resolve(
                &serde_json::from_value(self.compatibility.clone()).map_err(|e| e.to_string())?,
            )?;
            crate::syntax_editor::query(
                self.sources
                    .get(original)
                    .ok_or("Missing original snapshot")?
                    .clone(),
                &profile,
                method,
                params,
            )?
        } else {
            None
        };
        if method == "textDocument/linkedEditingRange" && syntax.is_some() {
            return Ok(syntax.unwrap());
        }
        let mut p = params.clone();
        let generated_uri = self
            .virtual_uris
            .get(original)
            .ok_or("Document is not in the project")?
            .clone();
        p["textDocument"]["uri"] = json!(generated_uri);
        if let Some(position) = params.get("position") {
            let position: Position =
                serde_json::from_value(position.clone()).map_err(|e| e.to_string())?;
            let choices = self
                .mappings
                .generated_positions(original, position)
                .map_err(|e| e.to_string())?;
            let choice = choices
                .iter()
                .find(|p| p.uri == generated_uri)
                .ok_or("Position has no checking projection")?;
            p["position"] = json!(choice.position);
        }
        if let Some(range) = params.get("range") {
            let range = serde_json::from_value(range.clone()).map_err(|e| e.to_string())?;
            let choices = self
                .mappings
                .generated_ranges(original, range)
                .map_err(|e| e.to_string())?;
            let choice = choices
                .iter()
                .find(|r| r.uri == generated_uri)
                .ok_or("Range has no contiguous checking projection")?;
            p["range"] = json!(choice.range);
        }
        if let Some(positions) = params.get("positions").and_then(Value::as_array) {
            let mut mapped = Vec::new();
            for position in positions {
                let position =
                    serde_json::from_value(position.clone()).map_err(|e| e.to_string())?;
                let choices = self
                    .mappings
                    .generated_positions(original, position)
                    .map_err(|e| e.to_string())?;
                if choices.is_empty()
                    && let Some(syntax) = &syntax
                {
                    return Ok(syntax.clone());
                }
                mapped.push(json!(
                    choices
                        .iter()
                        .find(|p| p.uri == generated_uri)
                        .ok_or("Selection position has no checking projection")?
                        .position
                ));
            }
            p["positions"] = json!(mapped);
        }
        if let Some(diagnostics) = p["context"]["diagnostics"].as_array_mut() {
            for diagnostic in diagnostics {
                let range = serde_json::from_value(diagnostic["range"].clone())
                    .map_err(|e| e.to_string())?;
                let choices = self
                    .mappings
                    .generated_ranges(original, range)
                    .map_err(|e| e.to_string())?;
                diagnostic["range"] = json!(
                    choices
                        .iter()
                        .find(|r| r.uri == generated_uri)
                        .ok_or("Diagnostic range cannot map for code action")?
                        .range
                );
            }
        }
        let op = match method {
            "textDocument/hover" => "hover",
            "textDocument/completion" => "completion",
            "textDocument/definition" => "definition",
            "textDocument/typeDefinition" => "typeDefinition",
            "textDocument/references" => "references",
            "textDocument/prepareRename" => "prepareRename",
            "textDocument/rename" => "rename",
            "textDocument/signatureHelp" => "signatureHelp",
            "textDocument/documentHighlight" => "documentHighlight",
            "textDocument/documentSymbol" => "documentSymbols",
            "textDocument/selectionRange" => "selectionRange",
            "textDocument/foldingRange" => "foldingRange",
            "textDocument/semanticTokens/full" => "semanticTokens",
            "textDocument/semanticTokens/range" => "semanticTokensRange",
            "textDocument/codeAction" => "codeAction",
            "textDocument/inlayHint" => "inlayHint",
            "textDocument/linkedEditingRange" => "linkedEditingRange",
            _ => return Err("Unsupported semantic operation".into()),
        };
        let response = self.client.request(op, p)?;
        if op == "completion" {
            let mut mapped = self
                .mappings
                .map_completions(&generated_uri, response.clone())
                .map_err(|e| e.to_string())?;
            let raw_items = if let Some(items) = response.as_array() {
                items.clone()
            } else {
                response["items"].as_array().cloned().unwrap_or_default()
            };
            if self.completions.len() + raw_items.len() > 10_000 {
                self.completions.clear();
            }
            let items = if mapped.is_array() {
                mapped.as_array_mut()
            } else {
                mapped["items"].as_array_mut()
            };
            if let Some(items) = items {
                for (item, raw) in items.iter_mut().zip(raw_items) {
                    self.next_completion += 1;
                    self.completions
                        .insert(self.next_completion, (generated_uri.clone(), raw));
                    item["data"] = json!({"tsRustCompletion":self.next_completion});
                }
            }
            return Ok(mapped);
        }
        let mut mapped = match op {
            "hover" => self.mappings.map_hover(&generated_uri, response),
            "completion" => self.mappings.map_completions(&generated_uri, response),
            "rename" => self.mappings.map_workspace_edit(response),
            "prepareRename" => self.mappings.map_prepare_rename(&generated_uri, response),
            "signatureHelp" => self.mappings.map_signature_help(response),
            "documentHighlight" => self.mappings.map_highlights(&generated_uri, response),
            "documentSymbols" => self.mappings.map_document_symbols(&generated_uri, response),
            "selectionRange" => self.mappings.map_selection_ranges(&generated_uri, response),
            "foldingRange" => self.mappings.map_folding_ranges(&generated_uri, response),
            "semanticTokens" | "semanticTokensRange" => {
                self.mappings.map_semantic_tokens(&generated_uri, response)
            }
            "codeAction" => self.mappings.map_code_actions(&generated_uri, response),
            "inlayHint" => self.mappings.map_inlay_hints(&generated_uri, response),
            "linkedEditingRange" => self
                .mappings
                .map_linked_editing_ranges(&generated_uri, response),
            _ => self.mappings.map_locations(&generated_uri, response),
        }
        .map_err(|e| e.to_string())?;
        if matches!(op, "foldingRange" | "documentSymbols")
            && let Some(syntax) = syntax
        {
            if mapped.is_null() {
                mapped = json!([]);
            }
            if let (Some(items), Some(additions)) = (mapped.as_array_mut(), syntax.as_array()) {
                for item in additions {
                    if !items.contains(item) {
                        items.push(item.clone());
                    }
                }
            }
        }
        Ok(mapped)
    }
}
