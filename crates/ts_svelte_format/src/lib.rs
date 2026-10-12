//! Pure native/WASM formatting with the pinned Rust Svelte/OXC formatter.
//! No configuration evaluation, filesystem access, subprocesses or JavaScript
//! execution occurs in the library. Hosts own discovery and persistence.
use rsvelte_formatter::{CssDialect, JsonVariant};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use ts_svelte::{ByteRange, Document, Range, Snapshot};

mod options;
pub use options::{ArrowParens, EndOfLine, QuoteProps, ResolvedOptions, TrailingComma};

pub const BACKEND_REVISION: &str = "5ed8ea3a3401b9fbbe780d3b18d6f90073291d6b";
pub const OXC_REVISION: &str = "60fa13878c3808413268c462ddadbceaf71c38bb";
pub const MAX_SOURCE_BYTES: usize = 1024 * 1024;
pub const MAX_REQUEST_BYTES: usize = MAX_SOURCE_BYTES * 6 + 65536;

#[derive(Clone, Debug, Serialize)]
pub struct FormatError {
    pub code: &'static str,
    pub message: String,
}
impl FormatError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}
impl std::fmt::Display for FormatError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for FormatError {}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplacementEdit {
    pub range: Range,
    pub bytes: ByteRange,
    pub new_text: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FormattedDocument {
    pub uri: String,
    pub version: u32,
    pub filename: String,
    pub language: &'static str,
    pub code: String,
    pub changed: bool,
    /// Empty for unchanged source; otherwise one atomic whole-document edit.
    pub edits: Vec<ReplacementEdit>,
}

/// Format a source string. Returned replacement coordinates address the exact
/// input, including BOM, UTF-16 surrogate pairs, CRLF and trailing line breaks.
pub fn format(
    source: &str,
    filename: &str,
    options: &ResolvedOptions,
) -> Result<FormattedDocument, FormatError> {
    format_document(
        Snapshot {
            uri: filename.into(),
            version: 0,
            text: source.into(),
        },
        filename,
        options,
    )
}

pub fn format_document(
    snapshot: Snapshot,
    filename: &str,
    options: &ResolvedOptions,
) -> Result<FormattedDocument, FormatError> {
    if snapshot.text.len() > MAX_SOURCE_BYTES {
        return Err(FormatError::new("size_limit", "Source exceeds 1 MiB"));
    }
    if filename.is_empty() || filename.contains(['\0', '?', '#']) {
        return Err(FormatError::new(
            "invalid_filename",
            "Expected a filename or URI path without query or fragment",
        ));
    }
    let extension = filename
        .rsplit('/')
        .next()
        .unwrap_or(filename)
        .rsplit_once('.')
        .map(|(_, extension)| extension)
        .ok_or_else(|| {
            FormatError::new(
                "unsupported_language",
                "Filename must have a supported extension",
            )
        })?;
    let backend = options.backend()?;
    let original = &snapshot.text;
    let line_ending = options.line_ending(original);
    let had_bom = original.starts_with('\u{feff}');
    let source = original
        .strip_prefix('\u{feff}')
        .unwrap_or(original)
        .replace("\r\n", "\n")
        .replace('\r', "\n");
    let (language, formatted) = match extension {
        "svelte" => {
            validate_component_styles(&source)?;
            (
                "svelte",
                rsvelte_formatter::format(&source, &backend.svelte),
            )
        }
        "js" | "jsx" | "mjs" | "cjs" | "ts" | "tsx" | "mts" | "cts" => (
            if matches!(extension, "ts" | "tsx" | "mts" | "cts") {
                "typescript"
            } else {
                "javascript"
            },
            rsvelte_formatter::format_js_source(&source, extension, &backend.svelte),
        ),
        "css" | "scss" | "less" | "pcss" | "postcss" => (
            "css",
            rsvelte_formatter::format_css_source(&source, css_dialect(extension)?, &backend.css),
        ),
        "json" | "jsonc" | "json5" => (
            "json",
            rsvelte_formatter::format_json_source(
                &source,
                match extension {
                    "jsonc" => JsonVariant::Jsonc,
                    "json5" => JsonVariant::Json5,
                    _ => JsonVariant::Json,
                },
                &backend.json,
            ),
        ),
        _ => {
            return Err(FormatError::new(
                "unsupported_language",
                format!("No native formatter for .{extension}"),
            ));
        }
    };
    let formatted =
        formatted.map_err(|error| FormatError::new("format_error", error.to_string()))?;
    let mut code = if line_ending == "\n" {
        formatted
    } else {
        formatted.replace('\n', line_ending)
    };
    if had_bom {
        code.insert(0, '\u{feff}');
    }
    let changed = code != *original;
    let document = Document::new(snapshot);
    let edits = if changed {
        let bytes = ByteRange {
            start: 0,
            end: document.text().len(),
        };
        vec![ReplacementEdit {
            range: document
                .editor_range(bytes)
                .map_err(|error| FormatError::new("invalid_document", error.to_string()))?,
            bytes,
            new_text: code.clone(),
        }]
    } else {
        Vec::new()
    };
    Ok(FormattedDocument {
        uri: document.snapshot().uri.clone(),
        version: document.snapshot().version,
        filename: filename.into(),
        language,
        code,
        changed,
        edits,
    })
}

fn css_dialect(language: &str) -> Result<CssDialect, FormatError> {
    match language {
        "css" | "postcss" | "pcss" => Ok(CssDialect::Css),
        "scss" => Ok(CssDialect::Scss),
        "less" => Ok(CssDialect::Less),
        _ => Err(FormatError::new(
            "unsupported_style_language",
            format!("No native style formatter for {language}"),
        )),
    }
}

// The dependency deliberately leaves indented Sass/Stylus bodies untouched.
// Detect unsupported top-level style languages before that silent bypass.
fn validate_component_styles(source: &str) -> Result<(), FormatError> {
    let arena = rsvelte_core::Allocator::default();
    let root = rsvelte_core::parse(
        source,
        &arena,
        rsvelte_core::ParseOptions {
            skip_non_css_lang_style: true,
            skip_expression_loc: true,
            defer_script_parse: true,
            force_typescript: true,
            ..Default::default()
        },
    )
    .map_err(|error| FormatError::new("format_error", error.to_string()))?;
    if let Some(css) = &root.css {
        for attribute in &css.attributes {
            if attribute["name"] != "lang" {
                continue;
            }
            let value = &attribute["value"];
            let language = if let Some(language) = value.as_str() {
                language.to_owned()
            } else if let Some(parts) = value.as_array() {
                let mut language = String::new();
                for part in parts {
                    if part["type"] != "Text" {
                        return Err(FormatError::new(
                            "unsupported_style_language",
                            "Style language must be a static string",
                        ));
                    }
                    language.push_str(
                        part["data"]
                            .as_str()
                            .or_else(|| part["raw"].as_str())
                            .ok_or_else(|| {
                                FormatError::new(
                                    "unsupported_style_language",
                                    "Style language must be a static string",
                                )
                            })?,
                    );
                }
                language
            } else {
                return Err(FormatError::new(
                    "unsupported_style_language",
                    "Style language must be a static string",
                ));
            };
            css_dialect(&language.to_ascii_lowercase())?;
        }
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "camelCase", deny_unknown_fields)]
enum Request {
    Capabilities {},
    Format {
        document: Snapshot,
        #[serde(default)]
        filename: Option<String>,
        #[serde(default)]
        options: ResolvedOptions,
    },
}

/// Shared bounded JSON protocol for the CLI, JavaScript and WebAssembly hosts.
pub fn dispatch(input: &[u8]) -> Value {
    if input.len() > MAX_REQUEST_BYTES {
        return json!({"ok":false,"error":FormatError::new("size_limit","Request is too large")});
    }
    let request: Request = match serde_json::from_slice(input) {
        Ok(request) => request,
        Err(error) => {
            return json!({"ok":false,"error":FormatError::new("invalid_request",error.to_string())});
        }
    };
    match request {
        Request::Capabilities {} => {
            json!({"ok":true,"result":{"native":true,"wasm":true,"javascriptInterface":"JSON","backend":{"name":"rsvelte_formatter","revision":BACKEND_REVISION,"oxcRevision":OXC_REVISION},"extensions":["svelte","js","jsx","mjs","cjs","ts","tsx","mts","cts","css","scss","less","pcss","postcss","json","jsonc","json5"],"edits":"atomic whole-document replacement","limitations":["Pinned Svelte 5 formatter; full Prettier compatibility is not established","No Tailwind class sorting or package.json key sorting","No range formatting, config evaluation, plugin loading or filesystem discovery","Indented Sass/Stylus and unknown style languages are unsupported"]}})
        }
        Request::Format {
            document,
            filename,
            options,
        } => {
            let filename = filename.unwrap_or_else(|| document.uri.clone());
            match format_document(document, &filename, &options) {
                Ok(result) => json!({"ok":true,"result":result}),
                Err(error) => json!({"ok":false,"error":error}),
            }
        }
    }
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen(js_name = formatRequest)]
pub fn format_request(input: &str) -> String {
    dispatch(input.as_bytes()).to_string()
}

#[cfg(test)]
mod tests;
