//! Snapshot-only Tailwind CSS 4.2.1 Oxide extraction. No filesystem discovery,
//! environment, network, JavaScript execution, threads, or host callbacks.
//! Official Rust machines and preprocessors are included unchanged from vendor/.
//! Extraction finds candidate-looking tokens, not proven valid CSS utilities.
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[path = "../vendor/crates/oxide/src/cursor.rs"]
mod cursor;
#[path = "../vendor/crates/oxide/src/extractor/mod.rs"]
mod extractor;
// Retain the exact upstream helper and its tests; current extractor does not call it.
#[allow(dead_code)]
#[path = "../vendor/crates/oxide/src/fast_skip.rs"]
mod fast_skip;
mod scanner;
#[cfg(test)]
#[path = "../vendor/crates/oxide/src/throughput.rs"]
mod throughput;

pub const TAILWIND_VERSION: &str = "4.2.1";
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SourceSnapshot {
    pub id: String,
    pub version: i64,
    /// Explicit, case-sensitive extension without a dot, as in Oxide dispatch.
    /// Unknown extensions use the unmodified text extraction path upstream.
    pub extension: String,
    pub text: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct ScanOptions {
    pub mode: ExtractionMode,
    pub limits: ScanLimits,
}
impl Default for ScanOptions {
    fn default() -> Self {
        Self {
            mode: ExtractionMode::ByExtension,
            limits: ScanLimits::default(),
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ExtractionMode {
    /// CSS source files contribute only custom properties used in var(...).
    ByExtension,
    /// Equivalent to Oxide's explicit content candidate extraction operation.
    Candidates,
    CssVariableUses,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct ScanLimits {
    pub sources: usize,
    pub source_bytes: usize,
    pub total_bytes: usize,
    pub line_bytes: usize,
    pub occurrences: usize,
    pub extracted_bytes: usize,
}
impl Default for ScanLimits {
    fn default() -> Self {
        Self {
            sources: 100_000,
            source_bytes: 4 * 1024 * 1024,
            total_bytes: 64 * 1024 * 1024,
            line_bytes: 1024 * 1024,
            occurrences: 1_000_000,
            extracted_bytes: 64 * 1024 * 1024,
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ExtractedKind {
    Candidate,
    CustomProperty,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ByteRange {
    pub start: usize,
    pub end: usize,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MappingStatus {
    Exact,
    /// Bytes were rewritten at this range, so this token has no exact spelling
    /// span in the original source. Processed coordinates are still provided.
    Rewritten,
    /// This preprocessor has no supplied provenance mapping. Even equal-length
    /// output is insufficient to prove a correspondence for individual tokens.
    Unavailable,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Occurrence {
    pub kind: ExtractedKind,
    pub value: String,
    pub processed_range: ByteRange,
    pub original_range: Option<ByteRange>,
    pub mapping: MappingStatus,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceExtraction {
    pub id: String,
    pub version: i64,
    pub extension: String,
    pub preprocessed: bool,
    pub processed_bytes: usize,
    /// Source byte order, then kind/value for deterministic ties. Duplicate
    /// occurrences at distinct source positions remain distinct.
    pub occurrences: Vec<Occurrence>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanOutput {
    pub tailwind_version: String,
    /// Same order as the supplied source snapshots; repeated IDs are rejected.
    pub sources: Vec<SourceExtraction>,
    /// Deduplicated across all source snapshots, sorted by UTF-8 bytes.
    pub candidates: Vec<String>,
    pub custom_properties: Vec<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanError {
    pub code: String,
    pub message: String,
    pub source_id: Option<String>,
}
impl std::fmt::Display for ScanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(id) = &self.source_id {
            write!(f, "{id}: ")?;
        }
        write!(f, "{}", self.message)
    }
}
impl std::error::Error for ScanError {}
fn error(code: &str, message: impl Into<String>, source: Option<&str>) -> ScanError {
    ScanError {
        code: code.into(),
        message: message.into(),
        source_id: source.map(str::to_string),
    }
}
fn validate(sources: &[SourceSnapshot], limits: &ScanLimits) -> Result<(), ScanError> {
    let max = ScanLimits::default();
    for (actual, maximum) in [
        (limits.sources, max.sources),
        (limits.source_bytes, max.source_bytes),
        (limits.total_bytes, max.total_bytes),
        (limits.line_bytes, max.line_bytes),
        (limits.occurrences, max.occurrences),
        (limits.extracted_bytes, max.extracted_bytes),
    ] {
        if actual == 0 || actual > maximum {
            return Err(error(
                "invalid_limits",
                "Limits must be positive and no larger than the default limits",
                None,
            ));
        }
    }
    if sources.len() > limits.sources {
        return Err(error("source_limit", "Too many source snapshots", None));
    }
    let mut ids = BTreeSet::new();
    let mut total = 0;
    for source in sources {
        let id = Some(source.id.as_str());
        if source.id.is_empty()
            || source.id.len() > 16_384
            || source.id.contains('\0')
            || !ids.insert(&source.id)
        {
            return Err(error(
                "invalid_source",
                "Empty, duplicate or invalid source identity",
                id,
            ));
        }
        if source.extension.len() > 128 || source.extension.contains(['\0', '/', '\\', '.']) {
            return Err(error(
                "invalid_extension",
                "Extension must be explicit without a leading dot or path",
                id,
            ));
        }
        total += source.text.len();
        if source.text.len() > limits.source_bytes || total > limits.total_bytes {
            return Err(error("source_limit", "Source byte limit exceeded", id));
        }
        if source
            .text
            .as_bytes()
            .split(|b| *b == b'\n')
            .any(|line| line.len() > limits.line_bytes)
        {
            return Err(error(
                "line_limit",
                "Source line exceeds extraction bounds",
                id,
            ));
        }
    }
    Ok(())
}

pub fn scan_sources(
    sources: &[SourceSnapshot],
    options: &ScanOptions,
) -> Result<ScanOutput, ScanError> {
    validate(sources, &options.limits)?;
    let mut output = ScanOutput {
        tailwind_version: TAILWIND_VERSION.into(),
        sources: vec![],
        candidates: vec![],
        custom_properties: vec![],
    };
    let mut candidates = BTreeSet::new();
    let mut variables = BTreeSet::new();
    let mut occurrence_count = 0;
    let mut extracted_bytes = 0;
    for source in sources {
        let original = source.text.as_bytes();
        let processed = scanner::bounded_pre_process(original.to_vec(), &source.extension)
            .map_err(|message| error("preprocess_limit", message, Some(&source.id)))?;
        if processed.len() > options.limits.source_bytes {
            return Err(error(
                "source_limit",
                "Preprocessed source exceeds byte bounds",
                Some(&source.id),
            ));
        }
        let changed = processed != original;
        // These three pinned preprocessors only replace bytes in place, or use
        // equal-length replacements. Others remain conservatively unmapped.
        let offset_preserved = !changed
            || (processed.len() == original.len()
                && matches!(source.extension.as_str(), "svelte" | "md" | "mdx" | "json"));
        let css_only = matches!(options.mode, ExtractionMode::CssVariableUses)
            || (matches!(options.mode, ExtractionMode::ByExtension) && source.extension == "css");
        let mut occurrences = BTreeMap::new();
        let mut line_start = 0;
        for line in processed.split(|b| *b == b'\n') {
            if line.len() > options.limits.line_bytes {
                return Err(error(
                    "line_limit",
                    "Preprocessed line exceeds extraction bounds",
                    Some(&source.id),
                ));
            }
            let mut extractor = extractor::Extractor::new(line);
            let extracted = if css_only {
                extractor.extract_variables_from_css()
            } else {
                extractor.extract()
            };
            for extracted in extracted {
                let (kind, bytes) = match extracted {
                    extractor::Extracted::Candidate(bytes) => (ExtractedKind::Candidate, bytes),
                    extractor::Extracted::CssVariable(bytes) => {
                        (ExtractedKind::CustomProperty, bytes)
                    }
                };
                let value = std::str::from_utf8(bytes).map_err(|_| {
                    error(
                        "invalid_extracted_utf8",
                        "Upstream extractor returned a non-UTF-8 token",
                        Some(&source.id),
                    )
                })?;
                // Every returned slice borrows this line; integer address
                // subtraction recovers its offset without pointer dereference.
                let relative = (bytes.as_ptr() as usize)
                    .checked_sub(line.as_ptr() as usize)
                    .ok_or_else(|| {
                        error(
                            "invalid_extracted_range",
                            "Extractor returned a foreign slice",
                            Some(&source.id),
                        )
                    })?;
                if relative.saturating_add(bytes.len()) > line.len() {
                    return Err(error(
                        "invalid_extracted_range",
                        "Extractor returned an out-of-bounds slice",
                        Some(&source.id),
                    ));
                }
                let range = ByteRange {
                    start: line_start + relative,
                    end: line_start + relative + bytes.len(),
                };
                let exact = offset_preserved
                    && original.get(range.start..range.end) == Some(bytes)
                    && source.text.is_char_boundary(range.start)
                    && source.text.is_char_boundary(range.end);
                let mapping = if exact {
                    MappingStatus::Exact
                } else if offset_preserved {
                    MappingStatus::Rewritten
                } else {
                    MappingStatus::Unavailable
                };
                occurrence_count += 1;
                extracted_bytes += bytes.len();
                if occurrence_count > options.limits.occurrences
                    || extracted_bytes > options.limits.extracted_bytes
                {
                    return Err(error(
                        "output_limit",
                        "Extracted output exceeds bounds",
                        Some(&source.id),
                    ));
                }
                match kind {
                    ExtractedKind::Candidate => {
                        candidates.insert(value.to_string());
                    }
                    ExtractedKind::CustomProperty => {
                        variables.insert(value.to_string());
                    }
                }
                occurrences
                    .entry((range, kind, value.to_string()))
                    .or_insert_with(|| Occurrence {
                        kind,
                        value: value.into(),
                        processed_range: range,
                        original_range: exact.then_some(range),
                        mapping,
                    });
            }
            line_start += line.len() + 1;
        }
        output.sources.push(SourceExtraction {
            id: source.id.clone(),
            version: source.version,
            extension: source.extension.clone(),
            preprocessed: changed,
            processed_bytes: processed.len(),
            occurrences: occurrences.into_values().collect(),
        });
    }
    output.candidates = candidates.into_iter().collect();
    output.custom_properties = variables.into_iter().collect();
    Ok(output)
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ScanRequest {
    pub protocol: u32,
    pub sources: Vec<SourceSnapshot>,
    #[serde(default)]
    pub options: ScanOptions,
}
/// Shared serialization boundary for native/JavaScript/WASM hosts. No OS or
/// wasm-specific APIs are required by the implementation itself.
pub fn scan_json(input: &str) -> Result<String, ScanError> {
    if input.len() > 80 * 1024 * 1024 {
        return Err(error("request_limit", "JSON request exceeds bounds", None));
    }
    let request: ScanRequest =
        serde_json::from_str(input).map_err(|e| error("invalid_json", e.to_string(), None))?;
    if request.protocol != 1 {
        return Err(error(
            "unsupported_protocol",
            "Scanner protocol must be 1",
            None,
        ));
    }
    let output = scan_sources(&request.sources, &request.options)?;
    serde_json::to_string(&output).map_err(|e| error("serialization", e.to_string(), None))
}

#[cfg(test)]
mod tests;
