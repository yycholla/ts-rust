//! Pure Kit 3 app-manifest finalization. Registered 3.0.1 and next.32 share the
//! installed `exports/vite/build/index.js` placeholder protocol. Kit 2 passes
//! through unchanged. No application/configuration code or filesystem access.
//!
//! Replacement is syntax-aware, with explicit occurrence counts and receipts
//! for interim sentinel spans. Unrelated strings/comments/property names are
//! never rewritten. This strengthens upstream's replaceAll boundary handling
//! while preserving its values and array order for valid generated modules.
//!
//! Preserve shifts existing source-map anchors through exact byte edits and
//! makes replacement spans unmapped. Sources/names/content/root metadata remain
//! unchanged. Hosts replace returned updated_map_files together with JS output.
//! Inline maps require an explicit mapFile and are externalized, rather than
//! retaining stale embedded bytes. Indexed maps are explicitly unsupported.
//! Drop removes maps; Reject refuses mapped edits. Upstream rewrites code after
//! hashing; callers must use Kit's fixed manifest chunk naming contract.
//!
//! Algorithm derived from SvelteKit, Copyright (c) 2020-present Svelte authors,
//! licensed MIT: permission is hereby granted, free of charge, to any person
//! obtaining a copy of this software and associated documentation files (the
//! "Software"), to deal in the Software without restriction, including without
//! limitation the rights to use, copy, modify, merge, publish, distribute,
//! sublicense, and/or sell copies of the Software, and to permit persons to whom
//! the Software is furnished to do so, subject to the following conditions:
//! The above copyright notice and this permission notice shall be included in
//! all copies or substantial portions of the Software. THE SOFTWARE IS PROVIDED
//! "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT
//! LIMITED TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A PARTICULAR
//! PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT
//! HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN
//! ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION
//! WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.
use crate::sync_versions::KitSyncVersion;
use oxc_allocator::Allocator;
use oxc_ast::ast::{
    AssignmentExpression, BindingIdentifier, IdentifierReference, StringLiteral, UpdateExpression,
};
use oxc_ast_visit::{Visit, walk};
use oxc_parser::Parser;
use oxc_span::{GetSpan, SourceType, Span};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

const MAX_SOURCE: usize = 16 * 1024 * 1024;
const MAX_TOTAL: usize = 128 * 1024 * 1024;
const MAX_OCCURRENCES: usize = 100_000;
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ManifestKey {
    Immutable,
    Assets,
    Prerendered,
    Routes,
}
impl ManifestKey {
    pub fn identifier(self) -> &'static str {
        match self {
            Self::Immutable => "__SVELTEKIT_MANIFEST_IMMUTABLE__",
            Self::Assets => "__SVELTEKIT_MANIFEST_ASSETS__",
            Self::Prerendered => "__SVELTEKIT_MANIFEST_PRERENDERED__",
            Self::Routes => "__SVELTEKIT_MANIFEST_ROUTES__",
        }
    }
    fn sentinel(self) -> Option<&'static str> {
        match self {
            Self::Immutable => Some("__sveltekit_manifest_build__"),
            Self::Prerendered => Some("__sveltekit_manifest_prerendered__"),
            _ => None,
        }
    }
    fn from_identifier(name: &str) -> Option<Self> {
        [
            Self::Immutable,
            Self::Assets,
            Self::Prerendered,
            Self::Routes,
        ]
        .into_iter()
        .find(|key| key.identifier() == name)
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManifestPath {
    pub path: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManifestRoute {
    pub id: String,
    pub page: bool,
    pub endpoint: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EmittedManifestRecord {
    pub file: String,
    #[serde(default)]
    pub css: Vec<String>,
    #[serde(default)]
    pub assets: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResolvedManifestMetadata {
    pub app_dir: String,
    pub base: String,
    /// Preserve Object.keys(manifest) order from the actual emission. None means
    /// not built yet; Some([]) means a completed empty/non-CSR client graph.
    pub client_manifest: Option<Vec<EmittedManifestRecord>>,
    #[serde(default)]
    pub inlined_files: BTreeSet<String>,
    pub assets: Vec<String>,
    pub routes: Vec<ManifestRoute>,
    /// Actual completed prerender results, including an explicitly empty list.
    pub prerendered_paths: Option<Vec<String>>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ManifestValues {
    pub immutable: Option<Vec<ManifestPath>>,
    pub assets: Vec<ManifestPath>,
    pub prerendered: Option<Vec<ManifestPath>>,
    pub routes: Vec<ManifestRoute>,
}
fn relative(file: &str) -> Result<(), String> {
    if file.is_empty()
        || file.contains(['\\', '\0', '?', '#'])
        || file
            .split('/')
            .any(|p| p.is_empty() || p == "." || p == "..")
    {
        return Err(format!(
            "Expected a relative emitted filename, got {file:?}"
        ));
    }
    Ok(())
}
/// Reproduce collect_immutable's insertion order and prefix test, omit deleted
/// inline files, retain static asset order, and filter non-page/non-endpoint IDs.
pub fn values_from_metadata(input: &ResolvedManifestMetadata) -> Result<ManifestValues, String> {
    relative(&input.app_dir)?;
    if !input.base.is_empty()
        && (!input.base.starts_with('/')
            || input.base.ends_with('/')
            || input.base.contains(['?', '#', '\\', '\0']))
    {
        return Err("Invalid resolved base path".into());
    }
    for file in &input.inlined_files {
        relative(file)?;
    }
    let immutable = input
        .client_manifest
        .as_ref()
        .map(|records| {
            let prefix = format!("{}/immutable", input.app_dir);
            let mut seen = BTreeSet::new();
            let mut paths = vec![];
            for record in records {
                for file in std::iter::once(&record.file)
                    .chain(&record.css)
                    .chain(&record.assets)
                {
                    relative(file)?;
                    if file.starts_with(&prefix)
                        && !input.inlined_files.contains(file)
                        && seen.insert(file)
                    {
                        paths.push(ManifestPath { path: file.clone() });
                    }
                }
            }
            Ok::<_, String>(paths)
        })
        .transpose()?;
    let assets = input
        .assets
        .iter()
        .map(|file| {
            relative(file)?;
            Ok(ManifestPath { path: file.clone() })
        })
        .collect::<Result<_, String>>()?;
    let mut ids = BTreeSet::new();
    let mut routes = vec![];
    for route in &input.routes {
        if !route.id.starts_with('/') || route.id.contains('\0') || !ids.insert(&route.id) {
            return Err(format!("Invalid or duplicate route ID {:?}", route.id));
        }
        if route.page || route.endpoint {
            routes.push(route.clone());
        }
    }
    let prerendered = input
        .prerendered_paths
        .as_ref()
        .map(|paths| {
            paths
                .iter()
                .map(|path| {
                    let without_base = path.strip_prefix(&input.base).ok_or_else(|| {
                        format!("Prerendered path {path:?} is outside base {:?}", input.base)
                    })?;
                    let relative = if without_base.is_empty() {
                        ""
                    } else {
                        without_base.strip_prefix('/').ok_or_else(|| {
                            format!("Prerendered path {path:?} must contain '/' after base")
                        })?
                    };
                    if path.contains(['\0', '\\']) {
                        return Err("Invalid prerendered path".into());
                    }
                    Ok(ManifestPath {
                        path: relative.into(),
                    })
                })
                .collect::<Result<Vec<_>, String>>()
        })
        .transpose()?;
    Ok(ManifestValues {
        immutable,
        assets,
        prerendered,
        routes,
    })
}
fn literal<T: Serialize>(value: &T) -> Result<String, String> {
    let literal = serde_json::to_string(value).map_err(|e| e.to_string())?;
    if literal.len() > MAX_SOURCE {
        return Err("Manifest literal exceeds 16 MiB".into());
    }
    Ok(literal)
}
fn reserve_replacement(
    size: &mut usize,
    start: usize,
    end: usize,
    replacement: &str,
) -> Result<(), String> {
    *size = size
        .checked_sub(end.checked_sub(start).ok_or("Invalid replacement span")?)
        .and_then(|size| size.checked_add(replacement.len()))
        .ok_or("Replacement size overflow")?;
    if *size > MAX_SOURCE {
        return Err("Finalized artifact exceeds 16 MiB".into());
    }
    Ok(())
}
fn reserve_output(size: &mut usize, artifact: &JavaScriptArtifact) -> Result<(), String> {
    *size = size
        .checked_add(artifact.code.len())
        .ok_or("Finalized source size overflow")?;
    if *size > MAX_TOTAL {
        return Err("Finalized JavaScript exceeds 128 MiB".into());
    }
    Ok(())
}
impl ManifestValues {
    fn replacement(&self, key: ManifestKey) -> Result<(String, bool), String> {
        match key {
            ManifestKey::Immutable => match &self.immutable {
                Some(value) => Ok((literal(value)?, false)),
                None => Ok((literal(&key.sentinel().unwrap())?, true)),
            },
            ManifestKey::Assets => Ok((literal(&self.assets)?, false)),
            ManifestKey::Prerendered => match &self.prerendered {
                Some(value) => Ok((literal(value)?, false)),
                None => Ok((literal(&key.sentinel().unwrap())?, true)),
            },
            ManifestKey::Routes => Ok((literal(&self.routes)?, false)),
        }
    }
}
/// Real final values for the service worker's compile-time defines. Missing
/// client/prerender results are errors, never fake empty arrays or sentinels.
pub fn final_defines(values: &ManifestValues) -> Result<BTreeMap<String, String>, String> {
    let mut result = BTreeMap::new();
    for key in [
        ManifestKey::Immutable,
        ManifestKey::Assets,
        ManifestKey::Prerendered,
        ManifestKey::Routes,
    ] {
        let (value, pending) = values.replacement(key)?;
        if pending {
            return Err(format!(
                "Final value is unavailable for {}",
                key.identifier()
            ));
        }
        result.insert(key.identifier().into(), value);
    }
    Ok(result)
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SourceMapPolicy {
    Reject,
    Drop,
    Preserve,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PendingPlaceholder {
    pub key: ManifestKey,
    /// UTF-8 bytes in the receipt's exact source.
    pub start: usize,
    pub end: usize,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct JavaScriptArtifact {
    pub code: String,
    pub map: Option<Value>,
    pub map_file: Option<String>,
    /// Only receipts returned by this module may be passed between phases.
    #[serde(default)]
    pub pending: Vec<PendingPlaceholder>,
    pub receipt_sha256: Option<String>,
}
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VariableReplacementInput {
    pub kit_version: String,
    pub artifacts: BTreeMap<String, JavaScriptArtifact>,
    pub values: ManifestValues,
    /// Exact counts for every supplied artifact. Omitted keys mean zero.
    pub expected_counts: BTreeMap<String, BTreeMap<ManifestKey, usize>>,
    pub source_maps: SourceMapPolicy,
}
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SentinelReplacementInput {
    pub kit_version: String,
    pub artifacts: BTreeMap<String, JavaScriptArtifact>,
    pub immutable: Option<Vec<ManifestPath>>,
    pub prerendered: Option<Vec<ManifestPath>>,
    pub require_final: bool,
    pub source_maps: SourceMapPolicy,
}
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplacementOutput {
    pub kit2_noop: bool,
    pub artifacts: BTreeMap<String, JavaScriptArtifact>,
    pub replacements: BTreeMap<String, BTreeMap<ManifestKey, usize>>,
    /// Host must remove these accompanying map artifacts when committing output.
    pub removed_map_files: BTreeSet<String>,
    /// Complete replacement JSON for emitted map files, committed with code.
    /// Artifact.map contains the same updated map; mapFile names stay stable.
    pub updated_map_files: BTreeMap<String, Value>,
    pub removed_mapping_urls: BTreeMap<String, Vec<String>>,
}
#[derive(Default)]
struct Scan {
    references: Vec<(ManifestKey, Span)>,
    bindings: Vec<String>,
    writes: Vec<Span>,
    strings: BTreeMap<(usize, usize), String>,
    map_comments: Vec<(Span, String)>,
}
impl<'a> Visit<'a> for Scan {
    fn visit_identifier_reference(&mut self, node: &IdentifierReference<'a>) {
        if let Some(key) = ManifestKey::from_identifier(node.name.as_str()) {
            self.references.push((key, node.span));
        }
    }
    fn visit_binding_identifier(&mut self, node: &BindingIdentifier<'a>) {
        if ManifestKey::from_identifier(node.name.as_str()).is_some() {
            self.bindings.push(node.name.to_string());
        }
    }
    fn visit_string_literal(&mut self, node: &StringLiteral<'a>) {
        self.strings.insert(
            (node.span.start as usize, node.span.end as usize),
            node.value.to_string(),
        );
    }
    fn visit_assignment_expression(&mut self, node: &AssignmentExpression<'a>) {
        self.writes.push(node.left.span());
        walk::walk_assignment_expression(self, node);
    }
    fn visit_update_expression(&mut self, node: &UpdateExpression<'a>) {
        self.writes.push(node.argument.span());
        walk::walk_update_expression(self, node);
    }
}
fn scan(code: &str) -> Result<Scan, String> {
    if code.len() > MAX_SOURCE {
        return Err("JavaScript artifact exceeds 16 MiB".into());
    }
    let allocator = Allocator::default();
    let parsed = Parser::new(&allocator, code, SourceType::mjs()).parse();
    if !parsed.diagnostics.is_empty() {
        return Err(format!(
            "Invalid emitted JavaScript: {}",
            parsed.diagnostics[0]
        ));
    }
    let mut output = Scan::default();
    output.visit_program(&parsed.program);
    if output.references.len() > MAX_OCCURRENCES {
        return Err("Too many manifest placeholder occurrences".into());
    }
    if !output.bindings.is_empty() {
        return Err(format!(
            "Manifest placeholder is locally bound: {}",
            output.bindings[0]
        ));
    }
    for (key, span) in &output.references {
        if code.get(span.start as usize..span.end as usize) != Some(key.identifier()) {
            return Err("Escaped manifest placeholder identifiers are unsupported".into());
        }
        if output
            .writes
            .iter()
            .any(|write| write.start <= span.start && write.end >= span.end)
        {
            return Err("Manifest placeholder occurs in an assignment/update target".into());
        }
    }
    for comment in &parsed.program.comments {
        let content =
            &code[comment.content_span().start as usize..comment.content_span().end as usize];
        let text = content.trim();
        if let Some(url) = text
            .strip_prefix("# sourceMappingURL=")
            .or_else(|| text.strip_prefix("@ sourceMappingURL="))
        {
            output.map_comments.push((comment.span, url.trim().into()));
        }
    }
    Ok(output)
}
/// Useful before issuing a replacement request: includes only unshadowed exact
/// identifier references, not misleading comments, strings, properties or regex.
pub fn inspect_variable_counts(code: &str) -> Result<BTreeMap<ManifestKey, usize>, String> {
    let mut counts = BTreeMap::new();
    for (key, _) in scan(code)?.references {
        *counts.entry(key).or_default() += 1;
    }
    Ok(counts)
}
fn hash(code: &str) -> String {
    format!("{:x}", Sha256::digest(code.as_bytes()))
}
#[derive(Debug)]
struct Edit {
    start: usize,
    end: usize,
    replacement: String,
    pending: Option<ManifestKey>,
}

fn preserve_mapping_urls(
    name: &str,
    artifact: &JavaScriptArtifact,
    scan: &Scan,
    edits: &mut Vec<Edit>,
) -> Result<(), String> {
    let base = url::Url::parse("https://sourcemap.invalid/").unwrap();
    let mut js_url = base.clone();
    js_url.set_path(&format!("/{name}"));
    for (span, location) in &scan.map_comments {
        let file = artifact
            .map_file
            .as_ref()
            .ok_or("Preserving sourceMappingURL requires an explicit mapFile artifact")?;
        let mut map_url = base.clone();
        map_url.set_path(&format!("/{file}"));
        if location.starts_with("data:") {
            let relative = js_url
                .make_relative(&map_url)
                .ok_or("Cannot relativize mapFile")?;
            if relative.contains("*/") {
                return Err("mapFile cannot terminate a generated mapping comment".into());
            }
            // Block form also works when the original comment precedes code on
            // the same line. The replacement itself receives unmapped anchors.
            edits.push(Edit {
                start: span.start as usize,
                end: span.end as usize,
                replacement: format!("/*# sourceMappingURL={relative} */"),
                pending: None,
            });
        } else {
            let resolved = js_url
                .join(location)
                .map_err(|e| format!("Invalid sourceMappingURL: {e}"))?;
            if resolved != map_url {
                return Err(format!(
                    "sourceMappingURL {location:?} does not identify mapFile {file:?}"
                ));
            }
        }
    }
    Ok(())
}

const MAX_MAP_SEGMENTS: usize = 1_000_000;
const BASE64: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
fn decode_segment(text: &str) -> Result<Vec<i64>, String> {
    let mut fields = vec![];
    let mut number = 0_u64;
    let mut shift = 0_u32;
    for byte in text.bytes() {
        let digit = BASE64
            .iter()
            .position(|c| *c == byte)
            .ok_or("Invalid source-map VLQ digit")? as u64;
        let payload = digit & 31;
        if shift >= 63 || payload > (u64::MAX >> shift) {
            return Err("Source-map VLQ overflow".into());
        }
        number |= payload << shift;
        if digit & 32 != 0 {
            shift += 5;
            continue;
        }
        let magnitude = i64::try_from(number >> 1).map_err(|_| "Source-map VLQ overflow")?;
        fields.push(if number & 1 == 0 {
            magnitude
        } else {
            -magnitude
        });
        if fields.len() > 5 {
            return Err("Too many source-map fields".into());
        }
        number = 0;
        shift = 0;
    }
    if shift != 0 || !matches!(fields.len(), 1 | 4 | 5) {
        return Err("Invalid source-map segment".into());
    }
    Ok(fields)
}
fn encode_delta(value: i64, output: &mut String) {
    let mut value = (value.unsigned_abs() << 1) | u64::from(value < 0);
    loop {
        let digit = (value & 31) as usize;
        value >>= 5;
        output.push(BASE64[digit | if value == 0 { 0 } else { 32 }] as char);
        if value == 0 {
            break;
        }
    }
}

/// Monotonic coordinate cursor avoids quadratic rescans on minified lines.
/// CRLF is one line break; offsets inside CRLF/surrogates are not anchors.
struct MapCursor<'a> {
    source: &'a str,
    byte: usize,
    line: usize,
    column: usize,
}
impl<'a> MapCursor<'a> {
    fn new(source: &'a str) -> Self {
        Self {
            source,
            byte: 0,
            line: 0,
            column: 0,
        }
    }
    fn advance(&mut self) -> Result<(), String> {
        let rest = &self.source[self.byte..];
        let ch = rest
            .chars()
            .next()
            .ok_or("Source-map position exceeds generated source")?;
        self.byte += ch.len_utf8();
        if ch == '\r' && rest.starts_with("\r\n") {
            self.byte += 1;
        }
        if matches!(ch, '\r' | '\n' | '\u{2028}' | '\u{2029}') {
            self.line += 1;
            self.column = 0;
        } else {
            self.column += ch.len_utf16();
        }
        Ok(())
    }
    fn offset(&mut self, line: usize, column: usize) -> Result<usize, String> {
        while (self.line, self.column) < (line, column) {
            self.advance()?;
        }
        if (self.line, self.column) != (line, column) {
            return Err("Source-map anchor is not a UTF-16 character boundary".into());
        }
        Ok(self.byte)
    }
    fn position(&mut self, byte: usize) -> Result<(usize, usize), String> {
        while self.byte < byte {
            self.advance()?;
        }
        if self.byte != byte {
            return Err("Invalid source-map edit boundary".into());
        }
        Ok((self.line, self.column))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct MapAnchor {
    byte: usize,
    /// Absolute source index, original line/column and optional name index.
    original: Option<Vec<i64>>,
}

fn decode_map(map: &Value, source: &str) -> Result<Vec<MapAnchor>, String> {
    if map.get("version").and_then(Value::as_u64) != Some(3) || !map.is_object() {
        return Err("Preserve requires a version 3 source map".into());
    }
    if map.get("sections").is_some() {
        return Err("Indexed source maps require flattening before Preserve".into());
    }
    let sources = map
        .get("sources")
        .and_then(Value::as_array)
        .ok_or("Source map lacks sources")?;
    if !sources.iter().all(Value::is_string) {
        return Err("Invalid source-map sources".into());
    }
    let names = map
        .get("names")
        .and_then(Value::as_array)
        .ok_or("Source map lacks names")?;
    if !names.iter().all(Value::is_string) {
        return Err("Invalid source-map names".into());
    }
    if let Some(contents) = map.get("sourcesContent") {
        let contents = contents.as_array().ok_or("Invalid sourcesContent")?;
        if contents.len() != sources.len() || !contents.iter().all(|v| v.is_null() || v.is_string())
        {
            return Err("Invalid sourcesContent entries".into());
        }
    }
    if map.get("sourceRoot").is_some_and(|root| !root.is_string()) {
        return Err("Invalid sourceRoot".into());
    }
    let mappings = map
        .get("mappings")
        .and_then(Value::as_str)
        .ok_or("Source map lacks mappings")?;
    if mappings.len() > 32 * 1024 * 1024 {
        return Err("Source-map mappings exceed 32 MiB".into());
    }
    let mut previous = [0_i64; 4];
    let mut anchors: Vec<MapAnchor> = vec![];
    let mut cursor = MapCursor::new(source);
    let mut count = 0;
    for (line, mappings) in mappings.split(';').enumerate() {
        let mut column = 0_i64;
        if mappings.is_empty() {
            continue;
        }
        for segment in mappings.split(',') {
            count += 1;
            if count > MAX_MAP_SEGMENTS {
                return Err("Too many source-map segments".into());
            }
            let fields = decode_segment(segment)?;
            if fields[0] < 0 {
                return Err("Source-map generated columns must be ordered".into());
            }
            column = column
                .checked_add(fields[0])
                .ok_or("Source-map column overflow")?;
            let byte = cursor.offset(
                line,
                usize::try_from(column).map_err(|_| "Invalid generated column")?,
            )?;
            let original = if fields.len() == 1 {
                None
            } else {
                for i in 1..fields.len() {
                    previous[i - 1] = previous[i - 1]
                        .checked_add(fields[i])
                        .ok_or("Source-map original position overflow")?;
                    if previous[i - 1] < 0 || previous[i - 1] > u32::MAX as i64 {
                        return Err("Invalid source-map original coordinate".into());
                    }
                }
                if previous[0] as usize >= sources.len()
                    || (fields.len() == 5 && previous[3] as usize >= names.len())
                {
                    return Err("Source-map index exceeds metadata".into());
                }
                Some(previous[..fields.len() - 1].to_vec())
            };
            let anchor = MapAnchor { byte, original };
            if let Some(last) = anchors.last()
                && last.byte == byte
            {
                if last != &anchor {
                    return Err(
                        "Conflicting source-map segments at duplicate generated position".into(),
                    );
                }
                continue;
            }
            anchors.push(anchor);
        }
    }
    Ok(anchors)
}

fn encode_map(anchors: &BTreeMap<usize, Option<Vec<i64>>>, source: &str) -> Result<String, String> {
    let mut result = String::new();
    let mut previous = [0_i64; 4];
    let mut line = 0;
    let mut column = 0_i64;
    let mut line_has_segment = false;
    let mut cursor = MapCursor::new(source);
    for (byte, original) in anchors {
        let (target_line, target_column) = cursor.position(*byte)?;
        while line < target_line {
            result.push(';');
            line += 1;
            column = 0;
            line_has_segment = false;
        }
        if line_has_segment {
            result.push(',');
        }
        encode_delta(target_column as i64 - column, &mut result);
        column = target_column as i64;
        if let Some(original) = original {
            for (i, value) in original.iter().enumerate() {
                encode_delta(*value - previous[i], &mut result);
                previous[i] = *value;
            }
        }
        line_has_segment = true;
    }
    Ok(result)
}

fn transform_map(map: &Value, before: &str, after: &str, edits: &[Edit]) -> Result<Value, String> {
    let original = decode_map(map, before)?;
    let mut shifted = BTreeMap::new();
    let mut edit_index = 0;
    let mut shift = 0_i64;
    for anchor in original {
        while edit_index < edits.len() && edits[edit_index].end <= anchor.byte {
            let edit = &edits[edit_index];
            shift += edit.replacement.len() as i64 - (edit.end - edit.start) as i64;
            edit_index += 1;
        }
        if edit_index < edits.len() && edits[edit_index].start <= anchor.byte {
            continue;
        }
        let target = usize::try_from(anchor.byte as i64 + shift)
            .map_err(|_| "Source-map shift underflow")?;
        shifted.insert(target, anchor.original);
    }
    shift = 0;
    for edit in edits {
        let start =
            usize::try_from(edit.start as i64 + shift).map_err(|_| "Source-map shift underflow")?;
        let end = start + edit.replacement.len();
        // Replacements carry no original provenance. Insertion of explicit
        // unmapped boundaries stops same-line greatest-lower-bound lookups.
        if start != end {
            shifted.insert(start, None);
        }
        // Resume only at an actual original anchor, never an interpolated one.
        shifted.entry(end).or_insert(None);
        shift += edit.replacement.len() as i64 - (edit.end - edit.start) as i64;
    }
    let mut result = map.clone();
    result["mappings"] = Value::String(encode_map(&shifted, after)?);
    Ok(result)
}

fn validate_artifacts(artifacts: &BTreeMap<String, JavaScriptArtifact>) -> Result<(), String> {
    if artifacts.len() > 100_000 {
        return Err("Too many JavaScript artifacts".into());
    }
    let mut total = 0usize;
    let mut map_owners = BTreeMap::new();
    for (name, artifact) in artifacts {
        relative(name)?;
        if !name.ends_with(".js") && !name.ends_with(".mjs") {
            return Err(format!("Expected a JavaScript artifact, got {name}"));
        }
        if let Some(file) = &artifact.map_file {
            relative(file)?;
            if map_owners.insert(file, name).is_some() {
                return Err(
                    "Shared source-map artifacts require external composition instead of dropping"
                        .into(),
                );
            }
            if artifacts.contains_key(file) {
                return Err("Source map filename collides with a JavaScript artifact".into());
            }
        }
        total = total
            .checked_add(artifact.code.len())
            .ok_or("Source budget overflow")?;
        if total > MAX_TOTAL {
            return Err("JavaScript artifacts exceed 128 MiB".into());
        }
    }
    Ok(())
}
fn apply(
    name: &str,
    artifact: &JavaScriptArtifact,
    mut edits: Vec<Edit>,
    retained: &[PendingPlaceholder],
    scan: &Scan,
    policy: SourceMapPolicy,
    out: &mut ReplacementOutput,
) -> Result<JavaScriptArtifact, String> {
    if edits.is_empty() {
        return Ok(artifact.clone());
    }
    let has_map =
        artifact.map.is_some() || artifact.map_file.is_some() || !scan.map_comments.is_empty();
    if has_map && policy == SourceMapPolicy::Reject {
        return Err(format!(
            "Editing {name} would invalidate its source map; explicitly select Preserve or Drop"
        ));
    }
    if policy == SourceMapPolicy::Preserve && has_map {
        if artifact.map.is_none() {
            return Err(format!(
                "Preserving {name} requires the actual source-map JSON"
            ));
        }
        preserve_mapping_urls(name, artifact, scan, &mut edits)?;
    }
    if policy == SourceMapPolicy::Drop
        && artifact.map_file.is_none()
        && scan
            .map_comments
            .iter()
            .any(|(_, url)| !url.starts_with("data:"))
    {
        return Err(format!(
            "Dropping the external source map of {name} requires its explicit mapFile artifact name"
        ));
    }
    if let Some(file) = &artifact.map_file
        && policy == SourceMapPolicy::Drop
    {
        out.removed_map_files.insert(file.clone());
    }
    for (span, url) in &scan.map_comments {
        if policy != SourceMapPolicy::Drop {
            continue;
        }
        edits.push(Edit {
            start: span.start as usize,
            end: span.end as usize,
            // Keep whitespace/line terminators so comment removal cannot join
            // tokens or alter automatic semicolon insertion.
            replacement: artifact.code[span.start as usize..span.end as usize]
                .chars()
                .map(|c| {
                    if matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}') {
                        c
                    } else {
                        ' '
                    }
                })
                .collect(),
            pending: None,
        });
        out.removed_mapping_urls
            .entry(name.into())
            .or_default()
            .push(url.clone());
    }
    edits.sort_by_key(|edit| (edit.start, edit.end));
    let projected = edits
        .iter()
        .try_fold(artifact.code.len() as i128, |size, edit| {
            if edit.end < edit.start {
                return Err("Invalid edit bounds".to_string());
            }
            Ok(size + edit.replacement.len() as i128 - (edit.end - edit.start) as i128)
        })?;
    if projected < 0 || projected > MAX_SOURCE as i128 {
        return Err("Finalized artifact exceeds 16 MiB".into());
    }
    let mut cursor = 0;
    let mut code = String::new();
    let mut pending = vec![];
    for edit in &edits {
        if edit.start < cursor
            || edit.end < edit.start
            || edit.end > artifact.code.len()
            || !artifact.code.is_char_boundary(edit.start)
            || !artifact.code.is_char_boundary(edit.end)
        {
            return Err("Overlapping/invalid manifest edits".into());
        }
        code.push_str(&artifact.code[cursor..edit.start]);
        let start = code.len();
        code.push_str(&edit.replacement);
        if let Some(key) = edit.pending {
            pending.push(PendingPlaceholder {
                key,
                start,
                end: code.len(),
            });
        }
        cursor = edit.end;
    }
    code.push_str(&artifact.code[cursor..]);
    for receipt in retained {
        if edits
            .iter()
            .any(|edit| edit.start < receipt.end && edit.end > receipt.start)
        {
            return Err("Retained sentinel overlaps another edit".into());
        }
        let shift: i64 = edits
            .iter()
            .filter(|edit| edit.end <= receipt.start)
            .map(|edit| edit.replacement.len() as i64 - (edit.end - edit.start) as i64)
            .sum();
        pending.push(PendingPlaceholder {
            key: receipt.key,
            start: (receipt.start as i64 + shift) as usize,
            end: (receipt.end as i64 + shift) as usize,
        });
    }
    pending.sort_by_key(|receipt| receipt.start);
    scan_generated(&code)?;
    let receipt_sha256 = (!pending.is_empty()).then(|| hash(&code));
    let map = if policy == SourceMapPolicy::Preserve {
        artifact
            .map
            .as_ref()
            .map(|map| transform_map(map, &artifact.code, &code, &edits))
            .transpose()?
    } else {
        None
    };
    let map_file = if policy == SourceMapPolicy::Preserve {
        artifact.map_file.clone()
    } else {
        None
    };
    if let (Some(file), Some(map)) = (&map_file, &map) {
        out.updated_map_files.insert(file.clone(), map.clone());
    }
    Ok(JavaScriptArtifact {
        code,
        map,
        map_file,
        pending,
        receipt_sha256,
    })
}
fn scan_generated(code: &str) -> Result<(), String> {
    scan(code).map(|_| ())
}
fn initial_output(kit2_noop: bool) -> ReplacementOutput {
    ReplacementOutput {
        kit2_noop,
        artifacts: BTreeMap::new(),
        replacements: BTreeMap::new(),
        removed_map_files: BTreeSet::new(),
        updated_map_files: BTreeMap::new(),
        removed_mapping_urls: BTreeMap::new(),
    }
}
/// Shared exact generated-text edits for post-tree-shaking Kit transforms.
/// Coordinates address the supplied artifact, never an inferred source span.
/// Tracking runs before manifest receipts; refuse to silently invalidate them.
pub(crate) fn rewrite_generated_artifact(
    name: &str,
    artifact: &JavaScriptArtifact,
    replacements: Vec<(usize, usize, String)>,
    source_maps: SourceMapPolicy,
) -> Result<ReplacementOutput, String> {
    if !artifact.pending.is_empty() || artifact.receipt_sha256.is_some() {
        return Err("Generated transforms must run before manifest receipt phases".into());
    }
    validate_artifacts(&BTreeMap::from([(name.to_owned(), artifact.clone())]))?;
    let scanned = scan(&artifact.code)?;
    let edits = replacements
        .into_iter()
        .map(|(start, end, replacement)| Edit {
            start,
            end,
            replacement,
            pending: None,
        })
        .collect();
    let mut output = initial_output(false);
    let rewritten = apply(
        name,
        artifact,
        edits,
        &[],
        &scanned,
        source_maps,
        &mut output,
    )?;
    output.artifacts.insert(name.to_owned(), rewritten);
    Ok(output)
}
/// First pass, called independently for SSR and client bundle results. Assets
/// and routes are known; unavailable immutable/prerendered values become exact
/// upstream sentinel strings with position receipts for the next pass.
pub fn replace_variables(input: &VariableReplacementInput) -> Result<ReplacementOutput, String> {
    let kit2 = !KitSyncVersion::parse(&input.kit_version)?.is_kit3();
    let mut output = initial_output(kit2);
    if kit2 {
        output.artifacts = input.artifacts.clone();
        return Ok(output);
    }
    validate_artifacts(&input.artifacts)?;
    if input.expected_counts.keys().ne(input.artifacts.keys()) {
        return Err("Expected counts must name every supplied artifact exactly once".into());
    }
    let mut output_size = 0usize;
    for (name, artifact) in &input.artifacts {
        if !artifact.pending.is_empty() || artifact.receipt_sha256.is_some() {
            return Err(
                "Variable replacement cannot consume an interim receipt; use replace_sentinels"
                    .into(),
            );
        }
        let scanned = scan(&artifact.code)?;
        let mut counts = BTreeMap::new();
        let mut edits = vec![];
        let mut planned_size = artifact.code.len();
        for (key, span) in &scanned.references {
            let (replacement, pending) = input.values.replacement(*key)?;
            reserve_replacement(
                &mut planned_size,
                span.start as usize,
                span.end as usize,
                &replacement,
            )?;
            *counts.entry(*key).or_default() += 1;
            edits.push(Edit {
                start: span.start as usize,
                end: span.end as usize,
                replacement,
                pending: pending.then_some(*key),
            });
        }
        let expected = input.expected_counts[name]
            .iter()
            .filter(|(_, count)| **count != 0)
            .map(|(key, count)| (*key, *count))
            .collect::<BTreeMap<_, _>>();
        if expected != counts {
            return Err(format!(
                "Manifest placeholder multiplicity mismatch in {name}: expected {expected:?}, observed {counts:?}"
            ));
        }
        let changed = apply(
            name,
            artifact,
            edits,
            &[],
            &scanned,
            input.source_maps,
            &mut output,
        )?;
        if !scan(&changed.code)?.references.is_empty() {
            return Err("Unresolved manifest identifier after replacement".into());
        }
        reserve_output(&mut output_size, &changed)?;
        output.replacements.insert(name.clone(), counts);
        output.artifacts.insert(name.clone(), changed);
    }
    Ok(output)
}
/// Subsequent pass, called after the client build (immutable) and after actual
/// prerendering (prerendered). Only validated receipts are replaced; arbitrary
/// application strings equal to upstream sentinels retain their original value.
pub fn replace_sentinels(input: &SentinelReplacementInput) -> Result<ReplacementOutput, String> {
    let kit2 = !KitSyncVersion::parse(&input.kit_version)?.is_kit3();
    let mut output = initial_output(kit2);
    if kit2 {
        output.artifacts = input.artifacts.clone();
        return Ok(output);
    }
    validate_artifacts(&input.artifacts)?;
    let mut output_size = 0usize;
    for (name, artifact) in &input.artifacts {
        let scanned = scan(&artifact.code)?;
        if artifact.pending.is_empty() != artifact.receipt_sha256.is_none() {
            return Err(format!(
                "Inconsistent manifest receipt ownership for {name}"
            ));
        }
        if !scanned.references.is_empty() {
            return Err(format!(
                "Bare manifest identifiers remain in {name}; variable pass must run first"
            ));
        }
        if !artifact.pending.is_empty()
            && artifact.receipt_sha256.as_deref() != Some(hash(&artifact.code).as_str())
        {
            return Err(format!("Stale manifest receipt for {name}"));
        }
        if artifact.pending.len() > MAX_OCCURRENCES {
            return Err("Too many pending manifest occurrences".into());
        }
        let mut sorted = artifact.pending.clone();
        sorted.sort_by_key(|receipt| receipt.start);
        let mut last = 0;
        let mut edits = vec![];
        let mut retained = vec![];
        let mut counts = BTreeMap::new();
        let mut planned_size = artifact.code.len();
        for receipt in sorted {
            let sentinel = receipt
                .key
                .sentinel()
                .ok_or("Invalid interim manifest key")?;
            if receipt.start < last
                || receipt.end <= receipt.start
                || scanned
                    .strings
                    .get(&(receipt.start, receipt.end))
                    .map(String::as_str)
                    != Some(sentinel)
                || artifact.code.get(receipt.start..receipt.end)
                    != Some(literal(&sentinel)?.as_str())
            {
                return Err(format!(
                    "Invalid/overlapping interim manifest span in {name}"
                ));
            }
            last = receipt.end;
            let replacement = match receipt.key {
                ManifestKey::Immutable => input.immutable.as_ref().map(literal),
                ManifestKey::Prerendered => input.prerendered.as_ref().map(literal),
                _ => None,
            }
            .transpose()?;
            if let Some(replacement) = replacement {
                reserve_replacement(&mut planned_size, receipt.start, receipt.end, &replacement)?;
                *counts.entry(receipt.key).or_default() += 1;
                edits.push(Edit {
                    start: receipt.start,
                    end: receipt.end,
                    replacement,
                    pending: None,
                });
            } else {
                retained.push(receipt);
            }
        }
        if input.require_final && !retained.is_empty() {
            return Err(format!("Final manifest values are missing in {name}"));
        }
        let changed = apply(
            name,
            artifact,
            edits,
            &retained,
            &scanned,
            input.source_maps,
            &mut output,
        )?;
        reserve_output(&mut output_size, &changed)?;
        output.replacements.insert(name.clone(), counts);
        output.artifacts.insert(name.clone(), changed);
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn paths(values: &[&str]) -> Vec<ManifestPath> {
        values
            .iter()
            .map(|path| ManifestPath {
                path: (*path).into(),
            })
            .collect()
    }
    fn values() -> ManifestValues {
        ManifestValues {
            immutable: None,
            assets: paths(&["favicon.svg"]),
            prerendered: None,
            routes: vec![ManifestRoute {
                id: "/".into(),
                page: true,
                endpoint: false,
            }],
        }
    }
    fn artifact(code: &str) -> JavaScriptArtifact {
        JavaScriptArtifact {
            code: code.into(),
            map: None,
            map_file: None,
            pending: vec![],
            receipt_sha256: None,
        }
    }
    fn variables(code: &str) -> VariableReplacementInput {
        VariableReplacementInput {
            kit_version: "3.0.1".into(),
            artifacts: BTreeMap::from([("chunks/app-manifest.js".into(), artifact(code))]),
            values: values(),
            expected_counts: BTreeMap::from([(
                "chunks/app-manifest.js".into(),
                inspect_variable_counts(code).unwrap(),
            )]),
            source_maps: SourceMapPolicy::Reject,
        }
    }
    #[test]
    fn metadata_matches_registered_kit3_order_prefix_and_prerender_paths() {
        let input = ResolvedManifestMetadata {
            app_dir: "_app".into(),
            base: "/base".into(),
            client_manifest: Some(vec![
                EmittedManifestRecord {
                    file: "_app/immutable/z.js".into(),
                    css: vec!["_app/immutable/a.css".into()],
                    assets: vec![
                        "_app/immutable/z.js".into(),
                        "static.svg".into(),
                        "_app/immutable-ish.js".into(),
                    ],
                },
                EmittedManifestRecord {
                    file: "_app/immutable/inline.js".into(),
                    css: vec![],
                    assets: vec![],
                },
            ]),
            inlined_files: BTreeSet::from(["_app/immutable/inline.js".into()]),
            assets: vec!["z.svg".into(), "a.svg".into()],
            routes: vec![
                ManifestRoute {
                    id: "/layout".into(),
                    page: false,
                    endpoint: false,
                },
                ManifestRoute {
                    id: "/api".into(),
                    page: false,
                    endpoint: true,
                },
            ],
            prerendered_paths: Some(vec!["/base".into(), "/base/".into(), "/base/a%20b".into()]),
        };
        let actual = values_from_metadata(&input).unwrap();
        assert_eq!(
            actual.immutable,
            Some(paths(&[
                "_app/immutable/z.js",
                "_app/immutable/a.css",
                "_app/immutable-ish.js"
            ]))
        );
        assert_eq!(actual.assets, paths(&["z.svg", "a.svg"]));
        assert_eq!(actual.routes.len(), 1);
        assert_eq!(actual.prerendered, Some(paths(&["", "", "a%20b"])));
        assert_eq!(
            final_defines(&actual).unwrap()[ManifestKey::Routes.identifier()],
            r#"[{"id":"/api","page":false,"endpoint":true}]"#
        );
        assert!(final_defines(&values()).is_err());
    }
    #[test]
    fn references_only_preserve_misleading_javascript_text() {
        let code = r#"// __SVELTEKIT_MANIFEST_ASSETS__
const text="__SVELTEKIT_MANIFEST_ASSETS__";
const regexp=/__SVELTEKIT_MANIFEST_ASSETS__/;
const object={__SVELTEKIT_MANIFEST_ASSETS__:1};
const prefixed=__SVELTEKIT_MANIFEST_ASSETS___suffix;
export const assets=__SVELTEKIT_MANIFEST_ASSETS__;
export const again=__SVELTEKIT_MANIFEST_ASSETS__;"#;
        let input = variables(code);
        assert_eq!(
            input.expected_counts["chunks/app-manifest.js"][&ManifestKey::Assets],
            2
        );
        let result = replace_variables(&input).unwrap();
        let code = &result.artifacts["chunks/app-manifest.js"].code;
        assert!(code.contains("// __SVELTEKIT_MANIFEST_ASSETS__"));
        assert!(code.contains("const text=\"__SVELTEKIT_MANIFEST_ASSETS__\""));
        assert!(code.contains("{__SVELTEKIT_MANIFEST_ASSETS__:1}"));
        assert!(code.contains("const assets=[{\"path\":\"favicon.svg\"}]"));
        let mut bad = input.clone();
        bad.expected_counts
            .get_mut("chunks/app-manifest.js")
            .unwrap()
            .insert(ManifestKey::Assets, 1);
        assert!(
            replace_variables(&bad)
                .unwrap_err()
                .contains("multiplicity")
        );
    }
    #[test]
    fn exact_interim_receipts_survive_two_phases_unicode_and_repeated_placeholders() {
        let code = "const emoji='😀'; const application='__sveltekit_manifest_prerendered__'; export const build=[__SVELTEKIT_MANIFEST_IMMUTABLE__,__SVELTEKIT_MANIFEST_IMMUTABLE__]; export const prerendered=__SVELTEKIT_MANIFEST_PRERENDERED__;";
        let first = replace_variables(&variables(code)).unwrap();
        assert_eq!(first.artifacts["chunks/app-manifest.js"].pending.len(), 3);
        let second = replace_sentinels(&SentinelReplacementInput {
            kit_version: "3.0.0-next.32".into(),
            artifacts: first.artifacts,
            immutable: Some(paths(&["_app/immutable/a\"😀.js"])),
            prerendered: None,
            require_final: false,
            source_maps: SourceMapPolicy::Reject,
        })
        .unwrap();
        assert_eq!(second.artifacts["chunks/app-manifest.js"].pending.len(), 1);
        let third = replace_sentinels(&SentinelReplacementInput {
            kit_version: "3.0.1".into(),
            artifacts: second.artifacts,
            immutable: None,
            prerendered: Some(paths(&["x\"\n😀"])),
            require_final: true,
            source_maps: SourceMapPolicy::Reject,
        })
        .unwrap();
        let artifact = &third.artifacts["chunks/app-manifest.js"];
        assert!(artifact.pending.is_empty());
        assert!(artifact.receipt_sha256.is_none());
        assert!(
            artifact
                .code
                .contains("application='__sveltekit_manifest_prerendered__'")
        );
        assert!(
            artifact
                .code
                .contains("prerendered=[{\"path\":\"x\\\"\\n😀\"}]")
        );
    }
    #[test]
    fn stale_receipts_missing_final_values_and_overlaps_are_errors() {
        let first = replace_variables(&variables(
            "export const x=__SVELTEKIT_MANIFEST_IMMUTABLE__;",
        ))
        .unwrap();
        let mut input = SentinelReplacementInput {
            kit_version: "3.0.1".into(),
            artifacts: first.artifacts,
            immutable: None,
            prerendered: None,
            require_final: true,
            source_maps: SourceMapPolicy::Reject,
        };
        assert!(replace_sentinels(&input).unwrap_err().contains("missing"));
        input.immutable = Some(vec![]);
        let saved = input.clone();
        input
            .artifacts
            .get_mut("chunks/app-manifest.js")
            .unwrap()
            .code
            .push(' ');
        assert!(replace_sentinels(&input).unwrap_err().contains("Stale"));
        let mut input = saved;
        let artifact = input.artifacts.get_mut("chunks/app-manifest.js").unwrap();
        artifact.pending.push(artifact.pending[0].clone());
        assert!(
            replace_sentinels(&input)
                .unwrap_err()
                .contains("overlapping")
        );
    }
    #[test]
    fn preserve_maps_shift_unicode_multiline_and_multiple_replacements_exactly() {
        let before = "const pre='🙂';\r\nconst a=AAA; const b=BBB;\nconst end=1;";
        let a = before.find("AAA").unwrap();
        let b = before.find("BBB").unwrap();
        let edits = vec![
            Edit {
                start: a,
                end: a + 3,
                replacement: "[\n'🙂',\n1]".into(),
                pending: None,
            },
            Edit {
                start: b,
                end: b + 3,
                replacement: "[]".into(),
                pending: None,
            },
        ];
        let after = format!(
            "{}{}{}{}{}",
            &before[..a],
            edits[0].replacement,
            &before[a + 3..b],
            edits[1].replacement,
            &before[b + 3..]
        );
        let positions = [
            0,
            a - 1,
            a,
            a + 1,
            a + 3,
            b,
            b + 3,
            before.find("const end").unwrap(),
        ];
        let mut anchors = BTreeMap::new();
        let mut cursor = MapCursor::new(before);
        for byte in positions {
            let (line, column) = cursor.position(byte).unwrap();
            anchors.insert(byte, Some(vec![0, line as i64, column as i64, 0]));
        }
        let map = serde_json::json!({"version":3,"file":"manifest.js","sourceRoot":"../original/","sources":["input.ts"],"sourcesContent":[before],"names":["value"],"x_google_ignoreList":[0],"mappings":encode_map(&anchors,before).unwrap()});
        let transformed = transform_map(&map, before, &after, &edits).unwrap();
        for field in [
            "file",
            "sourceRoot",
            "sources",
            "sourcesContent",
            "names",
            "x_google_ignoreList",
        ] {
            assert_eq!(transformed[field], map[field]);
        }
        let result = decode_map(&transformed, &after)
            .unwrap()
            .into_iter()
            .map(|anchor| (anchor.byte, anchor.original))
            .collect::<BTreeMap<_, _>>();
        let shift_a = edits[0].replacement.len() as isize - 3;
        let shift_b = edits[1].replacement.len() as isize - 3;
        assert_eq!(result[&a], None);
        assert_eq!(result[&((b as isize + shift_a) as usize)], None);
        assert_eq!(
            result[&((a as isize + 3 + shift_a) as usize)],
            anchors[&(a + 3)]
        );
        assert_eq!(
            result[&((b as isize + 3 + shift_a + shift_b) as usize)],
            anchors[&(b + 3)]
        );
        let last = before.find("const end").unwrap();
        assert_eq!(
            result[&((last as isize + shift_a + shift_b) as usize)],
            anchors[&last]
        );
        assert_eq!(result.values().filter(|value| value.is_some()).count(), 5);
    }
    fn mapped_variables(code: &str) -> VariableReplacementInput {
        let mut input = variables(code);
        input.source_maps = SourceMapPolicy::Preserve;
        let artifact = input.artifacts.get_mut("chunks/app-manifest.js").unwrap();
        artifact.map = Some(
            serde_json::json!({"version":3,"file":"app-manifest.js","sources":["original.ts"],"names":[],"sourcesContent":[code],"sourceRoot":"../src","mappings":"AAAA"}),
        );
        artifact.map_file = Some("chunks/app-manifest.js.map".into());
        input
    }
    #[test]
    fn preserve_updates_named_map_files_and_keeps_urls_across_receipt_phases() {
        let input = mapped_variables(
            "export const a=__SVELTEKIT_MANIFEST_IMMUTABLE__;\nexport const b=__SVELTEKIT_MANIFEST_PRERENDERED__;\n//# sourceMappingURL=app-manifest.js.map\n",
        );
        let interim = replace_variables(&input).unwrap();
        assert!(interim.removed_map_files.is_empty());
        assert!(interim.removed_mapping_urls.is_empty());
        let artifact = &interim.artifacts["chunks/app-manifest.js"];
        assert_eq!(
            artifact.map.as_ref().unwrap(),
            &interim.updated_map_files["chunks/app-manifest.js.map"]
        );
        assert!(
            artifact
                .code
                .contains("//# sourceMappingURL=app-manifest.js.map")
        );
        let mut next = SentinelReplacementInput {
            kit_version: "3.0.1".into(),
            artifacts: interim.artifacts,
            immutable: Some(paths(&["🙂.js"])),
            prerendered: Some(paths(&["done"])),
            require_final: true,
            source_maps: SourceMapPolicy::Preserve,
        };
        let complete = replace_sentinels(&next).unwrap();
        let final_artifact = &complete.artifacts["chunks/app-manifest.js"];
        assert!(final_artifact.pending.is_empty());
        assert!(final_artifact.map.is_some());
        assert!(final_artifact.code.contains("🙂.js"));
        decode_map(final_artifact.map.as_ref().unwrap(), &final_artifact.code).unwrap();
        next.artifacts
            .get_mut("chunks/app-manifest.js")
            .unwrap()
            .code
            .push(' ');
        assert!(
            replace_sentinels(&next)
                .unwrap_err()
                .contains("Stale manifest receipt")
        );
    }
    #[test]
    fn preserve_externalizes_inline_urls_and_rejects_ambiguous_map_metadata() {
        let mut input = mapped_variables(
            "export const a=__SVELTEKIT_MANIFEST_ASSETS__;\n//# sourceMappingURL=data:application/json;base64,e30=\n",
        );
        let result = replace_variables(&input).unwrap();
        assert!(
            !result.artifacts["chunks/app-manifest.js"]
                .code
                .contains("data:")
        );
        assert!(
            result.artifacts["chunks/app-manifest.js"]
                .code
                .contains("/*# sourceMappingURL=app-manifest.js.map */")
        );
        assert_eq!(result.updated_map_files.len(), 1);
        input
            .artifacts
            .get_mut("chunks/app-manifest.js")
            .unwrap()
            .map_file = None;
        assert!(
            replace_variables(&input)
                .unwrap_err()
                .contains("explicit mapFile")
        );
        let mut mismatch = mapped_variables(
            "export const a=__SVELTEKIT_MANIFEST_ASSETS__;\n//# sourceMappingURL=wrong.js.map",
        );
        assert!(
            replace_variables(&mismatch)
                .unwrap_err()
                .contains("does not identify mapFile")
        );
        mismatch
            .artifacts
            .get_mut("chunks/app-manifest.js")
            .unwrap()
            .map = None;
        assert!(
            replace_variables(&mismatch)
                .unwrap_err()
                .contains("actual source-map JSON")
        );
    }
    #[test]
    fn duplicate_mapping_positions_are_deduplicated_only_if_identical() {
        let mut map =
            serde_json::json!({"version":3,"sources":["a.ts"],"names":[],"mappings":"AAAA,AAAA"});
        assert_eq!(decode_map(&map, "x").unwrap().len(), 1);
        map["mappings"] = "AAAA,AAAC".into();
        assert!(
            decode_map(&map, "x")
                .unwrap_err()
                .contains("duplicate generated position")
        );
        map["mappings"] = "CAAA".into();
        assert!(
            decode_map(&map, "🙂")
                .unwrap_err()
                .contains("UTF-16 character boundary")
        );
        map["mappings"] = "AAAA".into();
        map["sections"] = serde_json::json!([]);
        assert!(decode_map(&map, "x").unwrap_err().contains("Indexed"));
    }
    #[test]
    fn preserve_does_not_guess_end_anchor_or_carry_mapping_into_replacement() {
        let before = "abc OLD tail";
        let edits = vec![Edit {
            start: 4,
            end: 7,
            replacement: "new\n🙂".into(),
            pending: None,
        }];
        let after = "abc new\n🙂 tail";
        let map = serde_json::json!({"version":3,"sources":["a"],"names":[],"mappings":"AAAA"});
        let transformed = transform_map(&map, before, after, &edits).unwrap();
        let anchors = decode_map(&transformed, after).unwrap();
        assert_eq!(
            anchors,
            vec![
                MapAnchor {
                    byte: 0,
                    original: Some(vec![0, 0, 0])
                },
                MapAnchor {
                    byte: 4,
                    original: None
                },
                MapAnchor {
                    byte: 12,
                    original: None
                }
            ]
        );
    }
    #[test]
    fn changed_maps_are_rejected_or_explicitly_removed_without_stale_urls() {
        let mut input = variables(
            "export const assets=__SVELTEKIT_MANIFEST_ASSETS__;\n//# sourceMappingURL=app-manifest.js.map\n",
        );
        let artifact = input.artifacts.get_mut("chunks/app-manifest.js").unwrap();
        artifact.map = Some(serde_json::json!({"version":3,"mappings":"AAAA"}));
        artifact.map_file = Some("chunks/app-manifest.js.map".into());
        assert!(
            replace_variables(&input)
                .unwrap_err()
                .contains("source map")
        );
        input.source_maps = SourceMapPolicy::Drop;
        let output = replace_variables(&input).unwrap();
        let artifact = &output.artifacts["chunks/app-manifest.js"];
        assert!(artifact.map.is_none());
        assert!(artifact.map_file.is_none());
        assert!(!artifact.code.contains("sourceMappingURL"));
        assert!(
            output
                .removed_map_files
                .contains("chunks/app-manifest.js.map")
        );
        let mut unchanged = variables("export const plain=1;\n//# sourceMappingURL=plain.js.map");
        unchanged
            .artifacts
            .get_mut("chunks/app-manifest.js")
            .unwrap()
            .map = Some(serde_json::json!({"version":3}));
        assert_eq!(
            replace_variables(&unchanged).unwrap().artifacts,
            unchanged.artifacts
        );
    }
    #[test]
    fn placeholders_cannot_be_bindings_assignments_or_invalid_shorthand() {
        for source in [
            "let __SVELTEKIT_MANIFEST_ASSETS__=[]; export const a=__SVELTEKIT_MANIFEST_ASSETS__;",
            "__SVELTEKIT_MANIFEST_ASSETS__ = [];",
            "__SVELTEKIT_MANIFEST_ASSETS__++;",
            r"export const a=\u005f_SVELTEKIT_MANIFEST_ASSETS__;",
        ] {
            assert!(inspect_variable_counts(source).is_err(), "{source}");
        }
        let input = variables("export const object={__SVELTEKIT_MANIFEST_ASSETS__};");
        assert!(replace_variables(&input).is_err());
    }
    #[test]
    fn kit2_has_an_explicit_byte_preserving_noop() {
        let mut input = variables("export const assets=__SVELTEKIT_MANIFEST_ASSETS__;");
        input.kit_version = "2.69.2".into();
        input.expected_counts.clear();
        let output = replace_variables(&input).unwrap();
        assert!(output.kit2_noop);
        assert_eq!(output.artifacts, input.artifacts);
        assert!(output.replacements.is_empty());
        input.kit_version = "3.1.0".into();
        assert!(replace_variables(&input).is_err());
    }
}
