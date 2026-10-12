//! One isolated native TypeScript transpilation for rune-module validation.
use serde::Deserialize;
use serde_json::json;
use std::io::Read;
use ts_goport::flags::{ModuleKind, ScriptTarget};
use ts_goport::options::{CompilerOptions, Tristate};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Request {
    file_name: String,
    source: String,
}

fn main() {
    if std::env::args().nth(1).as_deref() == Some("--help") {
        println!(
            "Usage: svelte-ts-transpile < request.json\nOne native TypeScript transpilation per process: fileName, source. ESNext JavaScript and source map; no type-check claim."
        );
        return;
    }
    let result = (|| -> Result<serde_json::Value, String> {
        let mut input = Vec::new();
        std::io::stdin()
            .take(2 * 1024 * 1024 + 1)
            .read_to_end(&mut input)
            .map_err(|e| e.to_string())?;
        if input.len() > 2 * 1024 * 1024 {
            return Err("Request exceeds 2 MiB".into());
        }
        let request: Request = serde_json::from_slice(&input).map_err(|e| e.to_string())?;
        if request.source.len() > 1024 * 1024 {
            return Err("Source exceeds 1 MiB".into());
        }
        ts_goport::execute::tsc::stdio::init();
        let output = ts_goport::transpile::transpile_module(
            &ts_goport::gostd::context::background(),
            &request.source,
            ts_goport::transpile::Options {
                file_name: request.file_name,
                report_diagnostics: true,
                compiler_options: Some(CompilerOptions {
                    target: ScriptTarget::ES_NEXT,
                    module: ModuleKind::ES_NEXT,
                    source_map: Tristate::True,
                    inline_sources: Tristate::True,
                    verbatim_module_syntax: Tristate::True,
                    ..Default::default()
                }),
            },
        )
        .ok_or("Transpilation cancelled")?;
        let diagnostics = output
            .diagnostics
            .iter()
            .map(|d| {
                let value = ts_goport::api::proto::new_diagnostic_response(d);
                let text = ts_goport::frontend::json::json_marshal(&value, &[])
                    .map_err(|e| format!("{e:?}"))?;
                serde_json::from_str::<serde_json::Value>(&text).map_err(|e| e.to_string())
            })
            .collect::<Result<Vec<_>, String>>()?;
        let unported = ts_goport::prelude::unported_report();
        Ok(
            json!({"ok":diagnostics.is_empty() && unported.is_empty(),"code":output.output_text,"map":output.source_map_text,"diagnostics":diagnostics,"unported":unported}),
        )
    })();
    let value = result.unwrap_or_else(
        |message| json!({"ok":false,"error":{"code":"transpile_failed","message":message}}),
    );
    println!("{value}");
    if value["ok"] != true {
        std::process::exit(1);
    }
}
