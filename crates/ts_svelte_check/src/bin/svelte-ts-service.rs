//! Persistent native TypeScript service for already projected Svelte projects.
//!
//! JSON lines in/out: `{id,op,params}` -> `{id,ok,result}` or
//! `{id,ok:false,error:{code,message}}`. `initialize` takes absolute `root`
//! and `project` (an existing tsconfig.json). Other params are ordinary LSP
//! params. Operations: open, change, close, watchedFiles, diagnostics, hover,
//! completion, completionResolve, definition, typeDefinition, references,
//! prepareRename, rename, projectDiagnostics, shutdown. All coordinates and URIs
//! are the actual
//! TypeScript project's UTF-16 coordinates; the caller owns Svelte mapping.
//!
//! One configured project per process. Source/config files are read, never
//! written. Open documents use engine overlays and strictly increasing versions.
//! The caller sends disk changes via watchedFiles. Cross-project orchestration
//! is not supplied: references/rename search this configured program only.
//! Additional editor operations: signatureHelp, documentHighlight,
//! documentSymbols, selectionRange, foldingRange, semanticTokens,
//! semanticTokensRange, codeAction, inlayHint, linkedEditingRange.
//! initialize accepts optional LSP capabilities and returns semanticTokensLegend.
//! Code actions are eager; the engine has no code-action resolve/refactor API.
//! `configure` takes UserPreferences JSON (raw camelCase or nested editor
//! settings), replaces preferences over engine defaults, and returns the
//! effective settings. `transpileModule` takes `{fileName,source}` and returns
//! `{ok,code,map,diagnostics,unported}` using ESNext native single-file emit.
//! Transpilation diagnostics make result.ok false; the outer ok means the
//! operation completed. Transpilation does not type-check or write any files.
//! `projectDiagnostics` takes `{uri,uris?}`: uri selects the configured project,
//! and optional uris restricts checking (default: every loaded source). It returns
//! `{documents:[{uri,diagnostics}],diagnostics,projectDiagnostics,loadedFiles,
//! suppressedDiagnostics,projectionsVerified,unported}`. Optional `projections`
//! uses the same validated projection inputs as svelte-ts-check; raw diagnostics
//! then carry exact original locations and generated helper errors are separated.
//! `diagnostics` is raw API DiagnosticResponse JSON (UTF-16 pos/end), including
//! config/global errors; documents contain LSP diagnostics, including empty lists.
//! Engine panics poison the service and terminate it after an error reply.

use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::{self, BufRead, Read, Write};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, mpsc};
use std::time::Duration;
use ts_goport::api::proto::new_diagnostic_response;
use ts_goport::frontend::core_context::{self, CheckerLifetime};
use ts_goport::frontend::json::{MarshalerTo, UnmarshalerFrom, json_marshal, json_unmarshal};
use ts_goport::frontend::{bundled, vfs};
use ts_goport::gostd::{Context, context};
use ts_goport::ls;
use ts_goport::ls::lsconv;
use ts_goport::ls::program_view::ProgramView;
use ts_goport::lsp::lsproto;
use ts_goport::program::ls_program;
use ts_goport::project::{self, Session, SessionInit, SessionOptions};

#[path = "../mapping.rs"]
mod mapping;

const MAX_FRAME: u64 = 64 * 1024 * 1024;

fn decode<T: UnmarshalerFrom + Default>(value: &Value) -> Result<T, String> {
    let mut result = T::default();
    json_unmarshal(value.to_string().as_bytes(), &mut result, &[])
        .map_err(|error| format!("Invalid protocol params: {error:?}"))?;
    Ok(result)
}

fn encode<T: MarshalerTo>(value: &T) -> Result<Value, String> {
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

struct Service {
    root: PathBuf,
    project: PathBuf,
    session: Rc<Session>,
    client: Rc<PullClient>,
    ctx: Context,
    opened: HashMap<String, i32>,
}

impl Drop for Service {
    fn drop(&mut self) {
        self.session.close();
    }
}

impl Service {
    fn new(params: &Value) -> Result<Self, String> {
        let canonical = |key: &str| -> Result<PathBuf, String> {
            let path = Path::new(
                params[key]
                    .as_str()
                    .ok_or_else(|| format!("Missing {key}"))?,
            );
            if !path.is_absolute() {
                return Err(format!("{key} must be absolute"));
            }
            path.canonicalize().map_err(|error| error.to_string())
        };
        let root = canonical("root")?;
        let project = canonical("project")?;
        if !root.is_dir() || !project.is_file() || !project.starts_with(&root) {
            return Err("Expected a project file inside an existing root directory".into());
        }
        if project
            .file_name()
            .is_none_or(|name| name != "tsconfig.json")
        {
            return Err("This service requires a tsconfig.json project filename".into());
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
            fs: bundled::wrap_fs(vfs::osvfs_fs()),
            client: Some(client.clone()),
            logger: None,
            npm_executor: None,
            spawner: None,
            content_mapper_logger: None,
            parse_cache: None,
            content_mapped_parse_cache: None,
        });
        Ok(Self {
            root,
            project,
            session,
            client,
            ctx,
            opened: HashMap::new(),
        })
    }

    fn validate_uri(&self, uri: &lsproto::DocumentUri) -> Result<(), String> {
        let name = uri.file_name();
        let path = Path::new(&name);
        if !path.is_absolute()
            || path
                .components()
                .any(|part| matches!(part, std::path::Component::ParentDir))
            || !path.starts_with(&self.root)
        {
            return Err("Document URI must resolve inside the project root".into());
        }
        // Do not allow a symlink in an existing ancestor to escape the root.
        let mut ancestor = path;
        while !ancestor.exists() {
            ancestor = ancestor
                .parent()
                .ok_or("Document has no existing ancestor")?;
        }
        if !ancestor
            .canonicalize()
            .map_err(|error| error.to_string())?
            .starts_with(&self.root)
        {
            return Err("Document symlink escapes the project root".into());
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
        if selected.canonicalize().ok().as_ref() != Some(&self.project) {
            return Err(format!(
                "Document selected a different/inferred project: {}",
                selected.display()
            ));
        }
        Ok(service)
    }

    #[allow(clippy::too_many_lines)]
    fn request(&mut self, op: &str, params: &Value, ctx: &Context) -> Result<Value, String> {
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
                let key = d.uri.file_name();
                if self.opened.contains_key(&key) {
                    return Err("Document is already open".into());
                }
                self.session
                    .did_open_file(ctx, &d.uri, d.version, &d.text, &d.language_id);
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
                self.session.did_change_file(
                    ctx,
                    &p.text_document.uri,
                    p.text_document.version,
                    &p.content_changes,
                );
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
                self.session.did_close_file(ctx, &p.text_document.uri);
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
                self.session.did_change_watched_files(ctx, &p.changes);
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
                let ctx = core_context::with_checker_lifetime(ctx, CheckerLifetime::DIAGNOSTICS);
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
                let ctx = core_context::with_checker_lifetime(ctx, CheckerLifetime::DIAGNOSTICS);
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

enum Event {
    Frame(Vec<u8>),
    Wake,
    End,
    Error(String),
}

fn read_frames(sender: mpsc::Sender<Event>) {
    let mut input = io::stdin().lock();
    loop {
        let mut bytes = Vec::new();
        match input
            .by_ref()
            .take(MAX_FRAME + 1)
            .read_until(b'\n', &mut bytes)
        {
            Ok(0) => {
                let _ = sender.send(Event::End);
                return;
            }
            Ok(_) if bytes.len() as u64 > MAX_FRAME => {
                let _ = sender.send(Event::Error("JSON frame exceeds 64 MiB".into()));
                return;
            }
            Ok(_) => {
                if sender.send(Event::Frame(bytes)).is_err() {
                    return;
                }
            }
            Err(error) => {
                let _ = sender.send(Event::Error(error.to_string()));
                return;
            }
        }
    }
}

fn main() -> io::Result<()> {
    if std::env::args()
        .nth(1)
        .as_deref()
        .is_some_and(|arg| arg == "--help" || arg == "help")
    {
        println!(
            "Usage: svelte-ts-service < requests.jsonl\nInitialize with {{id,op:\"initialize\",params:{{root,project}}}}; subsequent params follow LSP. UTF-16 coordinates. One configured project, no writes."
        );
        return Ok(());
    }
    let (sender, receiver) = mpsc::channel();
    let wake = sender.clone();
    ts_goport::gostd::local::set_waker(Arc::new(move || {
        let _ = wake.send(Event::Wake);
    }));
    std::thread::spawn(move || read_frames(sender));
    let mut output = io::stdout().lock();
    let mut service: Option<Service> = None;
    while let Ok(event) = receiver.recv() {
        let mut stop = false;
        let response = match event {
            Event::Wake => {
                ts_goport::gostd::local::run_pending();
                continue;
            }
            Event::End => break,
            Event::Error(message) => {
                stop = true;
                json!({"id":null,"ok":false,"error":{"code":"transport","message":message}})
            }
            Event::Frame(bytes) => {
                let request: Value = match serde_json::from_slice(&bytes) {
                    Ok(value) => value,
                    Err(error) => {
                        writeln!(
                            output,
                            "{}",
                            json!({"id":null,"ok":false,"error":{"code":"invalid_json","message":error.to_string()}})
                        )?;
                        output.flush()?;
                        continue;
                    }
                };
                let id = request.get("id").cloned().unwrap_or(Value::Null);
                let op = request["op"].as_str().unwrap_or("");
                let base = service
                    .as_ref()
                    .map_or_else(context::background, |service| service.ctx.clone());
                let (ctx, cancel) =
                    context::with_cancel(&core_context::with_request_id(&base, &id.to_string()));
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match op {
                    "initialize" => {
                        if service.is_some() {
                            return Err("Already initialized".into());
                        }
                        let created = Service::new(&request["params"])?;
                        let caps = lsproto::get_client_capabilities(&created.ctx);
                        let legend = encode(&ls::semantic_tokens_legend(
                            &caps.text_document.semantic_tokens,
                        ))?;
                        service = Some(created);
                        Ok(
                            json!({"positionEncoding":"utf-16","persistent":true,"crossProject":false,"semanticTokensLegend":legend}),
                        )
                    }
                    "shutdown" => {
                        stop = true;
                        service.take();
                        Ok(Value::Null)
                    }
                    _ => service
                        .as_mut()
                        .ok_or("Service is not initialized".to_owned())?
                        .request(op, &request["params"], &ctx),
                }));
                cancel();
                match result {
                    Ok(Ok(result)) => {
                        let unported = ts_goport::prelude::unported_report();
                        if unported.is_empty() {
                            json!({"id":id,"ok":true,"result":result})
                        } else {
                            stop = true;
                            json!({"id":id,"ok":false,"error":{"code":"unported","message":"TypeScript engine encountered unported behavior","details":unported}})
                        }
                    }
                    Ok(Err(message)) => {
                        json!({"id":id,"ok":false,"error":{"code":"request_failed","message":message}})
                    }
                    Err(payload) => {
                        stop = true;
                        let message = payload
                            .downcast_ref::<String>()
                            .cloned()
                            .or_else(|| payload.downcast_ref::<&str>().map(|s| (*s).to_owned()))
                            .unwrap_or_else(|| "TypeScript engine panic".into());
                        json!({"id":id,"ok":false,"error":{"code":"engine_panic","message":message}})
                    }
                }
            }
        };
        writeln!(output, "{response}")?;
        output.flush()?;
        if stop {
            break;
        }
        ts_goport::gostd::local::run_pending();
    }
    drop(service);
    Ok(())
}
