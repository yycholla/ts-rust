//! One native TypeScript project check per process, with complete JSON diagnostics.
//! Svelte projection/overlay construction is a separate operation.
use std::cell::{Cell, RefCell};
use std::io::{Read, Write};
use std::rc::Rc;

use serde::Deserialize;
use serde_json::{Value, json};
use ts_goport::api::proto::new_diagnostic_response;
use ts_goport::execute::execute_tsc::{TscCompilationHooks, command_line};
use ts_goport::execute::tsc::{DiagnosticReporter, System, new_os_system};
use ts_goport::frontend::json::json_marshal;
use ts_goport::gostd::context;

mod mapping;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    project: String,
    #[serde(default)]
    projections: Vec<mapping::ProjectionInput>,
    #[serde(default)]
    tracked_files: Vec<String>,
}

struct Hooks<'a> {
    projections: &'a [mapping::ProjectionInput],
    projections_verified: Cell<bool>,
    diagnostics: Rc<RefCell<Vec<Value>>>,
    failures: Rc<RefCell<Vec<String>>>,
    tracked_files: &'a [String],
    loaded_files: RefCell<Vec<String>>,
}

impl TscCompilationHooks for Hooks<'_> {
    fn program_created(&self) -> Result<(), ts_goport::execute::tsc::ExitStatus> {
        for projection in self.projections {
            let file = ts_goport::program::get_source_file(&projection.file_name);
            if file == ts_goport::prelude::Node::NIL && !projection.required {
                continue;
            }
            if file == ts_goport::prelude::Node::NIL
                || &*ts_goport::ast::source_file_text(file) != projection.code.as_str()
            {
                self.failures.borrow_mut().push(format!(
                    "Projection does not match loaded checker source: {}",
                    projection.file_name
                ));
                return Err(ts_goport::execute::tsc::ExitStatus::InvalidProjectOutputsSkipped);
            }
        }
        self.projections_verified.set(true);
        for path in self.tracked_files {
            if ts_goport::program::get_source_file(path) != ts_goport::prelude::Node::NIL {
                self.loaded_files.borrow_mut().push(path.clone());
            }
        }
        Ok(())
    }

    fn write_file(&self) -> Option<ts_goport::emitter::program_emit::WriteFile> {
        Some(std::sync::Arc::new(|_, _, _| {
            Err("The Svelte checker does not permit file emission".to_owned())
        }))
    }

    fn diagnostic_reporter(&self, _reporter: DiagnosticReporter) -> DiagnosticReporter {
        let diagnostics = self.diagnostics.clone();
        let failures = self.failures.clone();
        Rc::new(
            move |diagnostic| match json_marshal(&new_diagnostic_response(diagnostic), &[]) {
                Ok(text) => match serde_json::from_str(&text) {
                    Ok(value) => diagnostics.borrow_mut().push(value),
                    Err(error) => failures.borrow_mut().push(error.to_string()),
                },
                Err(error) => failures.borrow_mut().push(format!("{error:?}")),
            },
        )
    }
}

fn check(request: &Request) -> Value {
    if let Err(message) = mapping::map_diagnostics(&mut [], &request.projections) {
        return json!({"ok":false,"error":{"code":"invalid_projection","message":message}});
    }
    ts_goport::execute::tsc::stdio::init();
    let output = Rc::new(RefCell::new(Vec::<u8>::new()));
    let system: Rc<dyn System> = match new_os_system() {
        Ok(system) => Rc::new(system.with_writer(output.clone())),
        Err(status) => {
            return json!({"ok":false,"error":{"code":"system","message":"Cannot create checker filesystem"},"status":status.code()});
        }
    };
    let hooks = Hooks {
        projections: &request.projections,
        projections_verified: Cell::new(false),
        diagnostics: Rc::default(),
        failures: Rc::default(),
        tracked_files: &request.tracked_files,
        loaded_files: RefCell::default(),
    };
    let args = [
        "-p",
        &request.project,
        "--noEmit",
        "--noCheck",
        "false",
        "--incremental",
        "false",
        "--pretty",
        "false",
    ]
    .map(str::to_owned);
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        command_line(&context::background(), system, &args, &hooks)
            .status
            .code()
    }));
    let unported = ts_goport::prelude::unported_report();
    let status = outcome.ok();
    let mut diagnostics = hooks.diagnostics.take();
    let verified = hooks.projections_verified.get();
    if hooks.failures.borrow().is_empty()
        && let Err(message) = mapping::map_diagnostics(
            &mut diagnostics,
            if verified { &request.projections } else { &[] },
        )
    {
        hooks.failures.borrow_mut().push(message);
    }
    let completed = status.is_some_and(|code| (0..=2).contains(&code))
        && unported.is_empty()
        && hooks.failures.borrow().is_empty();
    let suppressed = if completed && verified {
        mapping::take_generated_diagnostics(&mut diagnostics, &request.projections)
    } else {
        Vec::new()
    };
    json!({
        "ok":completed,"status":status,
        "projectionsVerified":verified,
        "diagnostics":diagnostics,
        "suppressedDiagnostics":suppressed,
        "loadedFiles":hooks.loaded_files.take(),
        "output":String::from_utf8_lossy(&output.borrow()),
        "serializationFailures":hooks.failures.take(),"unported":unported,
        "error":if completed { Value::Null } else { json!({"code":"incomplete_check","message":"Checker did not complete normally; diagnostics may be partial"}) }
    })
}

fn main() -> std::io::Result<()> {
    if std::env::args().nth(1).as_deref() == Some("--help") {
        println!(
            "Usage: svelte-ts-check < request.json\nRequest: {{\"project\":\"/absolute/overlay/tsconfig.json\"}}\nOne project per process; no emit. Full diagnostic JSON with UTF-16 offsets."
        );
        return Ok(());
    }
    let mut input = Vec::new();
    std::io::stdin()
        .take(64 * 1024 * 1024 + 1)
        .read_to_end(&mut input)?;
    let request = if input.len() > 64 * 1024 * 1024 {
        Err("request exceeds 64 MiB".to_owned())
    } else {
        serde_json::from_slice::<Request>(&input)
            .map_err(|error| error.to_string())
            .and_then(|mut request| {
                let project =
                    std::fs::canonicalize(&request.project).map_err(|error| error.to_string())?;
                if !project.is_file() {
                    return Err("project must name an existing tsconfig file".to_owned());
                }
                let project = project
                    .into_os_string()
                    .into_string()
                    .map_err(|_| "project path is not UTF-8".to_owned())?;
                for projection in &mut request.projections {
                    projection.file_name = std::fs::canonicalize(&projection.file_name)
                        .map_err(|error| error.to_string())?
                        .into_os_string()
                        .into_string()
                        .map_err(|_| "projected path is not UTF-8".to_owned())?;
                }
                Ok(Request {
                    project,
                    projections: request.projections,
                    tracked_files: request.tracked_files,
                })
            })
    };
    let response = match request {
        Err(message) => json!({"ok":false,"error":{"code":"invalid_request","message":message}}),
        Ok(request) => {
            std::thread::Builder::new().name("svelte-ts-check".into())
                .stack_size(ts_goport::gostd::stack::max_stack_size())
                .spawn(move || check(&request))?.join()
                .unwrap_or_else(|_| json!({"ok":false,"error":{"code":"checker_panic","message":"Checker thread failed"}}))
        }
    };
    let mut stdout = std::io::stdout().lock();
    writeln!(stdout, "{response}")?;
    stdout.flush()?;
    if response["ok"] != true || response["status"] != 0 {
        std::process::exit(1);
    }
    Ok(())
}
