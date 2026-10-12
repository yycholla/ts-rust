//! Native Tailwind 4.2.1 stylesheet compilation over explicit source snapshots.
pub mod apply;
pub mod ast;
pub mod candidate;
pub mod color_mix;
pub mod compile;
pub mod css;
pub mod custom_utility;
pub mod design_system;
pub mod functions;
pub mod imports;
pub mod plugins;
pub mod project;
pub mod protocol;
pub mod snapshot;
pub mod source_map;
pub mod source_selection;
pub mod stylesheet;
pub mod theme;
pub mod typography;
pub mod utilities;
pub mod value;
pub mod variants;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
pub const TAILWIND_VERSION: &str = "4.2.1";
const MAX_JSON: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "camelCase", deny_unknown_fields)]
pub enum Request {
    CompileStylesheet {
        snapshot: snapshot::ProjectSnapshot,
        options: stylesheet::StylesheetOptions,
        candidates: Vec<String>,
    },
    ScanSources {
        sources: Vec<ts_tailwind_scan::SourceSnapshot>,
        #[serde(default)]
        options: ts_tailwind_scan::ScanOptions,
    },
    ParseCss {
        css: String,
        file: String,
    },
    ParseCandidates {
        candidates: Vec<String>,
        registry: candidate::CandidateRegistry,
    },
    ParseValue {
        value: String,
    },
    ParseSnapshot {
        snapshot: snapshot::ProjectSnapshot,
    },
    ResolveTheme {
        entries: Vec<ThemeEntry>,
        #[serde(default)]
        prefix: Option<String>,
        queries: Vec<ThemeQuery>,
    },
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThemeEntry {
    pub key: String,
    pub value: String,
    #[serde(default)]
    pub options: theme::ThemeOptions,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThemeQuery {
    pub candidate: Option<String>,
    pub namespaces: Vec<String>,
    #[serde(default)]
    pub options: theme::ThemeOptions,
}

pub fn dispatch(request: Request) -> Result<Value, String> {
    match request {
        Request::CompileStylesheet {
            snapshot,
            options,
            candidates,
        } => {
            let prepared = stylesheet::prepare_stylesheet(&snapshot, &options)
                .map_err(|error| error.to_string())?;
            let build = prepared
                .build(&candidates)
                .map_err(|error| error.to_string())?;
            Ok(json!({"build": build,"stage":"compiled","tailwindVersion":TAILWIND_VERSION}))
        }
        Request::ScanSources { sources, options } => {
            let output = ts_tailwind_scan::scan_sources(&sources, &options)
                .map_err(|error| error.to_string())?;
            serde_json::to_value(output).map_err(|error| error.to_string())
        }
        Request::ParseCss { css, file } => {
            let mut ast = css::parse(&css, &file)
                .map_err(|e| serde_json::to_string(&e).unwrap_or_else(|_| e.to_string()))?;
            let printed = ast::to_css(&mut ast, "<printed>")?;
            Ok(json!({"ast":ast,"css":printed,"stage":"parsed","tailwindVersion":TAILWIND_VERSION}))
        }
        Request::ParseCandidates {
            candidates,
            registry,
        } => {
            if candidates.len() > 100_000 {
                return Err("Too many candidates".into());
            }
            Ok(
                json!({"candidates":candidates.iter().map(|c|candidate::parse_candidate(c,&registry)).collect::<Vec<_>>(),"stage":"parsed"}),
            )
        }
        Request::ParseValue { value } => {
            if value.len() > MAX_JSON {
                return Err("Value input exceeds 16 MiB".into());
            }
            let ast = value::parse_value(&value)?;
            Ok(json!({"ast":ast,"css":value::value_to_css(&ast)}))
        }
        Request::ParseSnapshot { snapshot } => Ok(
            json!({"stylesheets":snapshot.parse_stylesheets()?,"stage":"parsed","tailwindVersion":TAILWIND_VERSION}),
        ),
        Request::ResolveTheme {
            entries,
            prefix,
            queries,
        } => {
            if entries.len() > 100_000 || queries.len() > 100_000 {
                return Err("Too many theme entries or queries".into());
            }
            let mut theme = theme::Theme::default();
            theme.prefix = prefix;
            for entry in entries {
                theme.add(&entry.key, &entry.value, entry.options, None)?;
            }
            let results: Vec<_> = queries
                .iter()
                .map(|q| {
                    theme.resolve(
                        q.candidate.as_deref(),
                        &q.namespaces.iter().map(String::as_str).collect::<Vec<_>>(),
                        q.options,
                    )
                })
                .collect();
            Ok(json!({"values":results,"theme":theme}))
        }
    }
}
pub fn request_json(request: &str) -> String {
    let result = if request.len() > MAX_JSON {
        Err("Request exceeds 16 MiB".into())
    } else {
        serde_json::from_str::<Request>(request)
            .map_err(|e| e.to_string())
            .and_then(dispatch)
    };
    let value = match result {
        Ok(value) => json!({"ok":true,"result":value}),
        Err(error) => json!({"ok":false,"error":error}),
    };
    let output = serde_json::to_string(&value)
        .unwrap_or_else(|_| "{\"ok\":false,\"error\":\"Serialization failed\"}".into());
    if output.len() > MAX_JSON {
        "{\"ok\":false,\"error\":\"Response exceeds 16 MiB\"}".into()
    } else {
        output
    }
}
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn tailwind_foundation_json(request: &str) -> String {
    request_json(request)
}
#[cfg(target_arch = "wasm32")]
thread_local! {
    static SERVICE: std::cell::RefCell<protocol::Service> = std::cell::RefCell::new(protocol::Service::default());
}
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn tailwind_json(request: &str) -> String {
    SERVICE.with(|service| service.borrow_mut().request_json(request))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn json_reports_real_phase_and_rejects_compile() {
        let result: Value = serde_json::from_str(&request_json(
            r#"{"operation":"parseCss","css":"@theme { --color-red: red; }","file":"app.css"}"#,
        ))
        .unwrap();
        assert_eq!(result["ok"], true);
        assert_eq!(result["result"]["stage"], "parsed");
        let result: Value =
            serde_json::from_str(&request_json(r#"{"operation":"compile","css":""}"#)).unwrap();
        assert_eq!(result["ok"], false);
    }
}
