use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;
use ts_goport::api::proto::new_diagnostic_response;
use ts_goport::frontend::bundled;
use ts_goport::frontend::core_context::{self, CheckerLifetime};
use ts_goport::frontend::json::{MarshalerTo, UnmarshalerFrom, json_marshal, json_unmarshal};
use ts_goport::gostd::{Context, context};
use ts_goport::ls;
use ts_goport::ls::lsconv;
use ts_goport::ls::program_view::ProgramView;
use ts_goport::lsp::lsproto;
use ts_goport::program::ls_program;
use ts_goport::project::{self, Session, SessionInit, SessionOptions};

use crate::{
    mapping,
    memory::{MemoryFs, checked_path},
};
use ts_goport::frontend::vfs::Fs;

fn decode<T: UnmarshalerFrom + Default>(value: &Value) -> Result<T, String> {
    let mut result = T::default();
    json_unmarshal(value.to_string().as_bytes(), &mut result, &[])
        .map_err(|error| format!("Invalid protocol params: {error:?}"))?;
    Ok(result)
}

pub(crate) fn encode<T: MarshalerTo>(value: &T) -> Result<Value, String> {
    let text = json_marshal(value, &[]).map_err(|error| format!("{error:?}"))?;
    serde_json::from_str(&text).map_err(|error| error.to_string())
}

fn answer<T: MarshalerTo>(value: Result<T, ts_goport::gostd::GoError>) -> Result<Value, String> {
    encode(&value.map_err(|error| error.error())?)
}

// Pull-mode host: the outer Svelte server owns watches and publications. Refresh
// requests are returned to that host rather than sent over the private JSONL pipe.
struct PullClient {
    locale: std::cell::RefCell<ts_goport::locale::Locale>,
    refresh: std::cell::RefCell<Vec<&'static str>>,
}
impl PullClient {
    fn new() -> Self {
        Self {
            locale: std::cell::RefCell::new(ts_goport::locale::DEFAULT),
            refresh: Default::default(),
        }
    }
}
impl project::Client for PullClient {
    fn watch_files(
        &self,
        _: &Context,
        _: project::WatcherID,
        _: &[lsproto::FileSystemWatcher],
    ) -> Result<(), ts_goport::gostd::GoError> {
        Err(ts_goport::gostd::errors::new(
            "Watch registration is owned by the Svelte host",
        ))
    }
    fn unwatch_files(
        &self,
        _: &Context,
        _: project::WatcherID,
    ) -> Result<(), ts_goport::gostd::GoError> {
        Ok(())
    }
    fn register_content_mapper_extensions(
        &self,
        _: &Context,
        _: &[String],
    ) -> Result<(), ts_goport::gostd::GoError> {
        Err(ts_goport::gostd::errors::new(
            "Content projection is owned by the Svelte host",
        ))
    }
    fn refresh_diagnostics(&self, _: &Context) -> Result<(), ts_goport::gostd::GoError> {
        self.refresh
            .borrow_mut()
            .push("workspace/diagnostic/refresh");
        Ok(())
    }
    fn publish_diagnostics(
        &self,
        _: &Context,
        _: lsproto::PublishDiagnosticsParams,
    ) -> Result<(), ts_goport::gostd::GoError> {
        Err(ts_goport::gostd::errors::new(
            "This service uses pull diagnostics",
        ))
    }
    fn refresh_inlay_hints(&self, _: &Context) -> Result<(), ts_goport::gostd::GoError> {
        self.refresh
            .borrow_mut()
            .push("workspace/inlayHint/refresh");
        Ok(())
    }
    fn refresh_code_lens(&self, _: &Context) -> Result<(), ts_goport::gostd::GoError> {
        self.refresh.borrow_mut().push("workspace/codeLens/refresh");
        Ok(())
    }
    fn progress_start(&self, _: &'static ts_goport::diagnostics::Message, _: Vec<String>) {}
    fn progress_finish(&self, _: &'static ts_goport::diagnostics::Message, _: Vec<String>) {}
    fn send_telemetry(
        &self,
        _: &Context,
        _: lsproto::TelemetryEvent,
    ) -> Result<(), ts_goport::gostd::GoError> {
        Ok(())
    }
    fn is_active(&self) -> bool {
        true
    }
    fn set_locale(&self, locale: &str) {
        *self.locale.borrow_mut() = ts_goport::locale::parse(locale).0;
    }
    fn get_locale(&self) -> ts_goport::locale::Locale {
        self.locale.borrow().clone()
    }
}

pub(crate) struct Service {
    _occupancy: crate::Occupancy,
    pub(crate) fs: Rc<MemoryFs>,
    texts: HashMap<String, String>,
    root: PathBuf,
    project: PathBuf,
    session: Rc<Session>,
    client: Rc<PullClient>,
    pub(crate) ctx: Context,
    opened: HashMap<String, i32>,
}

impl Drop for Service {
    fn drop(&mut self) {
        self.session.close();
        ts_goport::gostd::local::run_pending();
        while ts_goport::gostd::local::run_idle() {}
        self.session.snapshot().deref();
    }
}

impl Service {
    pub(crate) fn new(params: &Value) -> Result<Self, String> {
        let occupancy = crate::Occupancy::acquire()?;
        let root = PathBuf::from(checked_path(
            params["root"].as_str().ok_or("Missing root")?,
        )?);
        let project = PathBuf::from(checked_path(
            params["project"].as_str().ok_or("Missing project")?,
        )?);
        let fs = Rc::new(MemoryFs::new(&params["files"])?);
        if !fs.directory_exists(root.to_str().ok_or("Invalid root")?)
            || !fs.file_exists(project.to_str().ok_or("Invalid project")?)
            || !project.starts_with(&root)
            || project
                .file_name()
                .is_none_or(|name| name != "tsconfig.json")
        {
            return Err("Expected an existing tsconfig.json inside root".into());
        }
        let capabilities = params.get("capabilities").cloned().unwrap_or_else(|| json!({
            "textDocument": {
                "hover": {"contentFormat":["markdown","plaintext"]},
                "signatureHelp": {"signatureInformation":{"documentationFormat":["markdown","plaintext"],"parameterInformation":{"labelOffsetSupport":true}}},
                "documentSymbol": {"hierarchicalDocumentSymbolSupport":true},
                "semanticTokens": {
                    "requests":{"full":true,"range":true},"formats":["relative"],
                    "tokenTypes":["namespace","class","enum","interface","struct","typeParameter","type","parameter","variable","property","enumMember","decorator","event","function","method","macro","label","comment","string","keyword","number","regexp","operator"],
                    "tokenModifiers":["declaration","definition","readonly","static","deprecated","abstract","async","modification","documentation","defaultLibrary","local"]
                }
            }
        }));
        let capabilities: lsproto::ClientCapabilities = decode(&capabilities)?;
        let ctx = lsproto::with_client_capabilities(
            &context::background(),
            Arc::new(lsproto::ClientCapabilities::resolve(Some(&capabilities))),
        );
        let ctx = core_context::with_checker_lifetime(&ctx, CheckerLifetime::API);
        let client = Rc::new(PullClient::new());
        let session = project::new_session(&SessionInit {
            background_ctx: ctx.clone(),
            options: Rc::new(SessionOptions {
                current_directory: root.to_str().ok_or("Root is not UTF-8")?.to_owned(),
                default_library_path: bundled::lib_path(),
                typings_location: String::new(),
                position_encoding: lsproto::PositionEncodingKind::UTF16,
                watch_enabled: false,
                logging_enabled: false,
                telemetry_enabled: false,
                push_diagnostics_enabled: false,
                run_external_code: false,
                debounce_delay: Duration::ZERO,
                checker_pool_options: Default::default(),
            }),
            fs: bundled::wrap_fs(fs.clone()),
            client: Some(client.clone()),
            logger: None,
            npm_executor: None,
            spawner: None,
            content_mapper_logger: None,
            parse_cache: None,
            content_mapped_parse_cache: None,
        });
        Ok(Self {
            _occupancy: occupancy,
            fs,
            texts: HashMap::new(),
            root,
            project,
            session,
            client,
            ctx,
            opened: HashMap::new(),
        })
    }

    fn validate_uri(&self, uri: &lsproto::DocumentUri) -> Result<(), String> {
        if !uri.0.starts_with("file:///") {
            return Err("Expected an absolute file URI".into());
        }
        let parsed = ts_goport::gostd::url::parse(&uri.0).map_err(|error| error.to_string())?;
        if !parsed.host.is_empty() || !parsed.raw_query.is_empty() || !parsed.fragment.is_empty() {
            return Err("Document URI cannot have an authority, query or fragment".into());
        }
        let name = uri.file_name();
        checked_path(&name)?;
        let path = Path::new(&name);
        if !path.is_absolute()
            || path
                .components()
                .any(|part| matches!(part, std::path::Component::ParentDir))
            || !path.starts_with(&self.root)
        {
            return Err("Document URI must resolve inside the project root".into());
        }
        Ok(())
    }

    fn language_service(
        &self,
        ctx: &Context,
        uri: &lsproto::DocumentUri,
    ) -> Result<ls::LanguageService, String> {
        self.validate_uri(uri)?;
        let (_, project, service) = self
            .session
            .get_snapshot_and_default_project(ctx, uri, false)
            .map_err(|error| error.error())?;
        let selected = PathBuf::from(&project.borrow().config_file_name);
        if selected != self.project {
            return Err(format!(
                "Document selected a different/inferred project: {}",
                selected.display()
            ));
        }
        Ok(service)
    }

    fn enqueue(&self, change: project::FileChange) {
        self.session.cancel_warm_auto_import_cache();
        self.session.pending_file_changes.borrow_mut().push(change);
    }

    pub(crate) fn drain(&self) {
        ts_goport::gostd::local::run_pending();
        // Warming is an optimization; completion explicitly requests auto-imports
        // on demand. Drop cancelled idle work so it cannot retain old snapshots.
        self.session.cancel_warm_auto_import_cache();
        while ts_goport::gostd::local::run_idle() {}
        ts_goport::gostd::local::run_pending();
    }

    pub(crate) fn take_refresh(&self) -> Vec<&'static str> {
        self.client.refresh.take()
    }

    fn validate_open_size(&self, key: &str, text: &str) -> Result<(), String> {
        let total: usize = self
            .texts
            .iter()
            .filter(|(path, _)| path.as_str() != key)
            .map(|(_, text)| text.len())
            .sum();
        if text.len() > crate::memory::MAX_FILE
            || total + text.len() > crate::memory::MAX_TOTAL
            || (!self.texts.contains_key(key) && self.texts.len() >= crate::memory::MAX_FILES)
        {
            return Err("Open document storage limit exceeded".into());
        }
        Ok(())
    }

    pub(crate) fn files(&mut self, params: &Value, ctx: &Context) -> Result<Value, String> {
        let changes = self.fs.prepare(&params["files"])?;
        for (path, _) in &changes {
            self.validate_uri(&lsconv::file_name_to_document_uri(path))?;
        }
        let events: Vec<Value> = changes.iter().map(|(path, text)| json!({
            "uri":lsconv::file_name_to_document_uri(path).0,
            "type": if text.is_none() { 3 } else if self.fs.file_exists(path) { 2 } else { 1 }
        })).collect();
        self.fs.apply(changes);
        self.request("watchedFiles", &json!({"changes":events}), ctx)
    }

    #[allow(clippy::too_many_lines)]
    pub(crate) fn request(
        &mut self,
        op: &str,
        params: &Value,
        ctx: &Context,
    ) -> Result<Value, String> {
        match op {
            "configure" => {
                if !params.is_object() {
                    return Err("Preferences must be an object".into());
                }
                let preferences: ls::lsutil::UserPreferences = decode(params)?;
                self.session.configure(preferences.clone());
                self.session
                    .get_snapshot(ctx, project::ResourceRequest::default(), false);
                Ok(
                    json!({"preferences":encode(&preferences)?,"refresh":self.client.refresh.take()}),
                )
            }
            "transpileModule" => {
                #[derive(serde::Deserialize)]
                #[serde(rename_all = "camelCase", deny_unknown_fields)]
                struct TranspileRequest {
                    file_name: String,
                    source: String,
                }
                if params.to_string().len() > 2 * 1024 * 1024 {
                    return Err("Transpilation request exceeds 2 MiB".into());
                }
                let request: TranspileRequest =
                    serde_json::from_value(params.clone()).map_err(|error| error.to_string())?;
                if request.source.len() > 1024 * 1024 {
                    return Err("Source exceeds 1 MiB".into());
                }
                let output = ts_goport::transpile::transpile_module(
                    ctx,
                    &request.source,
                    ts_goport::transpile::Options {
                        file_name: request.file_name,
                        report_diagnostics: true,
                        compiler_options: Some(ts_goport::options::CompilerOptions {
                            target: ts_goport::flags::ScriptTarget::ES_NEXT,
                            module: ts_goport::flags::ModuleKind::ES_NEXT,
                            source_map: ts_goport::options::Tristate::True,
                            inline_sources: ts_goport::options::Tristate::True,
                            verbatim_module_syntax: ts_goport::options::Tristate::True,
                            ..Default::default()
                        }),
                    },
                )
                .ok_or("Transpilation cancelled")?;
                let diagnostics: Vec<Value> = output
                    .diagnostics
                    .iter()
                    .map(|diagnostic| encode(&new_diagnostic_response(diagnostic)))
                    .collect::<Result<_, _>>()?;
                let unported = ts_goport::prelude::unported_report();
                Ok(
                    json!({"ok":diagnostics.is_empty() && unported.is_empty(),"code":output.output_text,"map":output.source_map_text,"diagnostics":diagnostics,"unported":unported}),
                )
            }
            "open" => {
                let p: lsproto::DidOpenTextDocumentParams = decode(params)?;
                let d = p.text_document.ok_or("Missing textDocument")?;
                self.validate_uri(&d.uri)?;
                if d.text.len() > crate::memory::MAX_FILE {
                    return Err("Document exceeds 8 MiB".into());
                }
                let key = d.uri.file_name();
                self.validate_open_size(&key, &d.text)?;
                if self.opened.contains_key(&key) {
                    return Err("Document is already open".into());
                }
                self.enqueue(project::FileChange {
                    kind: project::FileChangeKind::OPEN,
                    uri: d.uri.clone(),
                    version: d.version,
                    content: d.text.clone(),
                    language_kind: d.language_id,
                    ..Default::default()
                });
                self.texts.insert(key.clone(), d.text);
                self.opened.insert(key, d.version);
                self.language_service(ctx, &d.uri)?;
                Ok(Value::Null)
            }
            "change" => {
                let p: lsproto::DidChangeTextDocumentParams = decode(params)?;
                self.validate_uri(&p.text_document.uri)?;
                let key = p.text_document.uri.file_name();
                let previous = self.opened.get(&key).ok_or("Document is not open")?;
                if p.text_document.version <= *previous {
                    return Err("Document version must increase".into());
                }
                let text = crate::changes::apply(
                    self.texts.get(&key).ok_or("Missing open text")?,
                    &params["contentChanges"],
                )?;
                self.validate_open_size(&key, &text)?;
                self.enqueue(project::FileChange {
                    kind: project::FileChangeKind::CHANGE,
                    uri: p.text_document.uri.clone(),
                    version: p.text_document.version,
                    changes: p.content_changes,
                    ..Default::default()
                });
                self.texts.insert(key.clone(), text);
                self.opened.insert(key, p.text_document.version);
                self.language_service(ctx, &p.text_document.uri)?;
                Ok(Value::Null)
            }
            "close" => {
                let p: lsproto::DidCloseTextDocumentParams = decode(params)?;
                self.validate_uri(&p.text_document.uri)?;
                if self
                    .opened
                    .remove(&p.text_document.uri.file_name())
                    .is_none()
                {
                    return Err("Document is not open".into());
                }
                self.texts.remove(&p.text_document.uri.file_name());
                self.enqueue(project::FileChange {
                    kind: project::FileChangeKind::CLOSE,
                    uri: p.text_document.uri,
                    ..Default::default()
                });
                self.session
                    .get_snapshot(ctx, project::ResourceRequest::default(), false);
                Ok(Value::Null)
            }
            "watchedFiles" => {
                let p: lsproto::DidChangeWatchedFilesParams = decode(params)?;
                for file in &p.changes {
                    let file = file.as_ref().ok_or("Null file event")?;
                    self.validate_uri(&file.uri)?;
                    if !matches!(
                        file.type_,
                        lsproto::FileChangeType::CREATED
                            | lsproto::FileChangeType::CHANGED
                            | lsproto::FileChangeType::DELETED
                    ) {
                        return Err("Invalid file change type".into());
                    }
                }
                for file in p.changes.into_iter().flatten() {
                    let kind = match file.type_ {
                        lsproto::FileChangeType::CREATED => project::FileChangeKind::WATCH_CREATE,
                        lsproto::FileChangeType::DELETED => project::FileChangeKind::WATCH_DELETE,
                        _ => project::FileChangeKind::WATCH_CHANGE,
                    };
                    self.enqueue(project::FileChange {
                        kind,
                        uri: file.uri,
                        ..Default::default()
                    });
                }
                self.session
                    .get_snapshot(ctx, project::ResourceRequest::default(), false);
                Ok(Value::Null)
            }
            "projectDiagnostics" => {
                // `uri` selects the configured project; optional `uris` limits
                // files checked. Without it, check every loaded source including
                // imported auxiliary files. Preserve raw API diagnostics for the
                // caller's exact source mapping and generated-code filtering.
                let uri: lsproto::DocumentUri = decode(&params["uri"])?;
                let ctx = core_context::with_checker_lifetime(ctx, CheckerLifetime::API);
                let service = self.language_service(&ctx, &uri)?;
                let program = service.get_program();
                let projections: Vec<mapping::ProjectionInput> = serde_json::from_value(
                    params
                        .get("projections")
                        .cloned()
                        .unwrap_or_else(|| json!([])),
                )
                .map_err(|error| format!("Invalid projections: {error}"))?;
                mapping::map_diagnostics(&mut [], &projections)?;
                for projection in &projections {
                    let file = program.source_file_root(&projection.file_name);
                    if file == ts_goport::prelude::Node::NIL && !projection.required {
                        continue;
                    }
                    if file == ts_goport::prelude::Node::NIL
                        || &*ts_goport::ast::source_file_text(file) != projection.code.as_str()
                    {
                        return Err(format!(
                            "Projection does not match loaded checker source: {}",
                            projection.file_name
                        ));
                    }
                }
                let loaded: Vec<String> = program
                    .source_files()
                    .iter()
                    .map(|file| file.file_name().to_owned())
                    .collect();
                let files: Vec<lsproto::DocumentUri> = if let Some(uris) = params.get("uris") {
                    let uris = uris.as_array().ok_or("uris must be an array")?;
                    uris.iter().map(decode).collect::<Result<_, _>>()?
                } else {
                    loaded
                        .iter()
                        .map(|file| lsconv::file_name_to_document_uri(file))
                        .collect()
                };
                let mut documents = Vec::with_capacity(files.len());
                let mut raw = Vec::new();
                for file_uri in files {
                    let file = program.source_file_root(&file_uri.file_name());
                    if file == ts_goport::prelude::Node::NIL {
                        return Err(format!(
                            "Requested diagnostic file is not loaded: {}",
                            file_uri.file_name()
                        ));
                    }
                    let mut diagnostics =
                        ls_program::get_syntactic_diagnostics(program, &ctx, file);
                    diagnostics.extend(ls_program::get_semantic_diagnostics(program, &ctx, file));
                    let mapped = service.to_lsp_diagnostics(&ctx, &[diagnostics.as_slice()]);
                    for diagnostic in &diagnostics {
                        raw.push(encode(&new_diagnostic_response(diagnostic))?);
                    }
                    documents
                        .push(json!({"uri":encode(&file_uri)?,"diagnostics":encode(&mapped)?}));
                }
                let snapshot = self.session.snapshot();
                let project = snapshot
                    .get_default_project(&uri)
                    .ok_or("Project disappeared during diagnostics")?;
                let project_diagnostics = project.borrow().get_project_diagnostics(&ctx);
                let global: Vec<Value> = project_diagnostics
                    .iter()
                    .map(|diagnostic| encode(&new_diagnostic_response(diagnostic)))
                    .collect::<Result<_, _>>()?;
                raw.extend(global.iter().cloned());
                mapping::map_diagnostics(&mut raw, &projections)?;
                let suppressed = mapping::take_generated_diagnostics(&mut raw, &projections);
                Ok(
                    json!({"documents":documents,"diagnostics":raw,"projectDiagnostics":global,"loadedFiles":loaded,"suppressedDiagnostics":suppressed,"projectionsVerified":true,"unported":ts_goport::prelude::unported_report()}),
                )
            }
            "diagnostics" => {
                let p: lsproto::DocumentDiagnosticParams = decode(params)?;
                let ctx = core_context::with_checker_lifetime(ctx, CheckerLifetime::API);
                let service = self.language_service(&ctx, &p.text_document.uri)?;
                answer(service.provide_diagnostics(&ctx, &p.text_document.uri))
            }
            "hover" => {
                let p: lsproto::HoverParams = decode(params)?;
                answer(
                    self.language_service(ctx, &p.text_document.uri)?
                        .provide_hover(ctx, &p),
                )
            }
            "signatureHelp" => {
                let p: lsproto::SignatureHelpParams = decode(params)?;
                answer(
                    self.language_service(ctx, &p.text_document.uri)?
                        .provide_signature_help(
                            ctx,
                            &p.text_document.uri,
                            p.position,
                            p.context.as_ref(),
                        ),
                )
            }
            "documentHighlight" => {
                let p: lsproto::DocumentHighlightParams = decode(params)?;
                answer(
                    self.language_service(ctx, &p.text_document.uri)?
                        .provide_document_highlights(ctx, &p.text_document.uri, p.position),
                )
            }
            "documentSymbols" => {
                let p: lsproto::DocumentSymbolParams = decode(params)?;
                answer(
                    self.language_service(ctx, &p.text_document.uri)?
                        .provide_document_symbols(ctx, &p.text_document.uri),
                )
            }
            "selectionRange" => {
                let p: lsproto::SelectionRangeParams = decode(params)?;
                answer(
                    self.language_service(ctx, &p.text_document.uri)?
                        .provide_selection_ranges(ctx, &p),
                )
            }
            "foldingRange" => {
                let p: lsproto::FoldingRangeParams = decode(params)?;
                answer(
                    self.language_service(ctx, &p.text_document.uri)?
                        .provide_folding_range(ctx, &p.text_document.uri),
                )
            }
            "semanticTokens" => {
                let p: lsproto::SemanticTokensParams = decode(params)?;
                answer(
                    self.language_service(ctx, &p.text_document.uri)?
                        .provide_semantic_tokens(ctx, &p.text_document.uri),
                )
            }
            "semanticTokensRange" => {
                let p: lsproto::SemanticTokensRangeParams = decode(params)?;
                answer(
                    self.language_service(ctx, &p.text_document.uri)?
                        .provide_semantic_tokens_range(ctx, &p.text_document.uri, p.range),
                )
            }
            "codeAction" => {
                let p: lsproto::CodeActionParams = decode(params)?;
                if p.context
                    .as_ref()
                    .is_some_and(|context| context.diagnostics.iter().any(Option::is_none))
                {
                    return Err("Null code-action diagnostic".into());
                }
                answer(
                    self.language_service(ctx, &p.text_document.uri)?
                        .provide_code_actions(ctx, &p),
                )
            }
            "inlayHint" => {
                let p: lsproto::InlayHintParams = decode(params)?;
                answer(
                    self.language_service(ctx, &p.text_document.uri)?
                        .provide_inlay_hint(ctx, &p),
                )
            }
            "linkedEditingRange" => {
                let p: lsproto::LinkedEditingRangeParams = decode(params)?;
                answer(
                    self.language_service(ctx, &p.text_document.uri)?
                        .provide_linked_editing_range(ctx, &p),
                )
            }
            "completion" => {
                let p: lsproto::CompletionParams = decode(params)?;
                let service = self.language_service(ctx, &p.text_document.uri)?;
                let result = service.provide_completion(
                    ctx,
                    &p.text_document.uri,
                    p.position,
                    p.context.as_ref(),
                );
                if result.as_ref().is_err_and(|error| {
                    ts_goport::gostd::errors::is(error, &ls::ERR_NEEDS_AUTO_IMPORTS)
                }) {
                    drop(service);
                    let snapshot = self.session.get_snapshot(
                        ctx,
                        project::ResourceRequest {
                            documents: vec![p.text_document.uri.clone()],
                            ..Default::default()
                        },
                        true,
                    );
                    let result = self.session.get_language_service_with_auto_imports(
                        ctx,
                        &snapshot,
                        &p.text_document.uri,
                    );
                    snapshot.deref();
                    let service = result.map_err(|error| error.error())?;
                    answer(service.provide_completion(
                        ctx,
                        &p.text_document.uri,
                        p.position,
                        p.context.as_ref(),
                    ))
                } else {
                    answer(result)
                }
            }
            "completionResolve" => {
                let p: lsproto::CompletionItem = decode(params)?;
                let data = p.data.clone().ok_or("Completion item has no data")?;
                let uri = lsconv::file_name_to_document_uri(&data.file_name);
                answer(self.language_service(ctx, &uri)?.resolve_completion_item(
                    ctx,
                    p,
                    Some(data),
                ))
            }
            "definition" | "typeDefinition" => {
                let p: lsproto::TextDocumentPositionParams = decode(params)?;
                let service = self.language_service(ctx, &p.text_document.uri)?;
                if op == "definition" {
                    answer(service.provide_definition(ctx, &p.text_document.uri, p.position))
                } else {
                    answer(service.provide_type_definition(ctx, &p.text_document.uri, p.position))
                }
            }
            "references" => {
                let p: lsproto::ReferenceParams = decode(params)?;
                answer(
                    self.language_service(ctx, &p.text_document.uri)?
                        .provide_references(ctx, &p, None),
                )
            }
            "prepareRename" => {
                let p: lsproto::PrepareRenameParams = decode(params)?;
                let info = self
                    .language_service(ctx, &p.text_document.uri)?
                    .get_rename_info(ctx, "", &p.text_document.uri, p.position);
                if !info.can_rename {
                    return Err(info.localized_error_message);
                }
                if !info.file_to_rename.is_empty() {
                    return Err("File rename requires separate file-operation handling".into());
                }
                Ok(json!({"range":encode(&info.trigger_span)?,"placeholder":info.display_name}))
            }
            "rename" => {
                let p: lsproto::RenameParams = decode(params)?;
                let service = self.language_service(ctx, &p.text_document.uri)?;
                let info =
                    service.get_rename_info(ctx, &p.new_name, &p.text_document.uri, p.position);
                if !info.can_rename {
                    return Err(info.localized_error_message);
                }
                if !info.file_to_rename.is_empty() {
                    return Err("File rename requires separate file-operation handling".into());
                }
                answer(service.provide_rename(ctx, &p, None))
            }
            _ => Err(format!("Unknown operation: {op}")),
        }
    }
}
