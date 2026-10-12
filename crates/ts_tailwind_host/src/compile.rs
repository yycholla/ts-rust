//! Native host composition: discovery, extraction, Tailwind compilation, and
//! source-map composition. Relative CSS URLs are bound to their originating
//! stylesheet before imports/theme/functions can move their declarations.
use crate::{
    HostError, HostProject, HostRequest, HostSnapshots, canonical, environment_file, fail,
    load_project_with_snapshots, path_string,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::Read,
    path::{Path, PathBuf},
};
use ts_tailwind::{
    ast::{AstNode, SourceLocation},
    project::{ProjectBuild, build_prepared},
    snapshot::ProjectSnapshot,
    source_map,
    stylesheet::{StylesheetSourceReceipt, prepare_stylesheet},
};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct CompileOptions {
    pub scan_options: ts_tailwind_scan::ScanOptions,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Replacement {
    pub original_start: usize,
    pub original_end: usize,
    pub generated_start: usize,
    pub generated_end: usize,
    pub asset_token: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StylesheetPreprocess {
    pub id: String,
    pub original_version: i64,
    pub generated_version: i64,
    pub original_sha256: String,
    pub generated_sha256: String,
    /// Only unchanged spans and replacement endpoints have exact correspondence.
    /// Replacement interiors are generated-only; all offsets are UTF-8 bytes.
    pub replacements: Vec<Replacement>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetReference {
    pub token: String,
    pub path: String,
    pub byte_length: u64,
    pub sha256: String,
    pub query: Option<String>,
    pub fragment: Option<String>,
    pub original_url: String,
    pub occurrences: Vec<SourceLocation>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompileDiagnostic {
    pub severity: String,
    pub code: String,
    pub message: String,
    pub candidate: Option<String>,
    pub source: Option<SourceLocation>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompiledProject {
    /// Original inputs and normalized discovery facts; includes asset watches.
    pub project: HostProject,
    pub preprocessed_snapshot: ProjectSnapshot,
    pub preprocess: Vec<StylesheetPreprocess>,
    /// Real compiled intermediate CSS. Asset tokens require the declared linker
    /// stage before publication; this API never writes output or asset files.
    pub build: ProjectBuild,
    pub map: Value,
    pub assets: Vec<AssetReference>,
    pub requires_asset_linking: bool,
    pub diagnostics: Vec<CompileDiagnostic>,
}
struct PendingAsset {
    token: String,
    path: PathBuf,
    query: Option<String>,
    fragment: Option<String>,
    url: String,
    occurrences: Vec<SourceLocation>,
}
#[derive(Debug)]
struct UrlToken {
    start: usize,
    end: usize,
    url: String,
}

pub fn compile_project(
    request: &HostRequest,
    options: &CompileOptions,
) -> Result<CompiledProject, HostError> {
    compile_project_with_snapshots(request, options, &HostSnapshots::default())
}

pub fn compile_project_with_snapshots(
    request: &HostRequest,
    options: &CompileOptions,
    snapshots: &HostSnapshots,
) -> Result<CompiledProject, HostError> {
    let mut project = load_project_with_snapshots(request, snapshots)?;
    let overlay = crate::snapshot_fs::SnapshotFs::prepare(snapshots, &request.limits)?;
    let operation = (|| {
        let (snapshot, preprocess, pending) =
            rewrite_urls(&project.snapshot, &project.options.imports.bases)?;
        let prepared = prepare_stylesheet(&snapshot, &project.options)
            .map_err(|e| fail("stylesheet", &request.entry, e))?
            .with_source_discovery(
                project.source_metadata.root.clone(),
                project.source_metadata.sources.clone(),
            );
        let mut build = build_prepared(
            &prepared,
            &snapshot.sources,
            &project.inventory,
            &options.scan_options,
        )
        .map_err(|e| fail("compile", &request.entry, e))?;
        // Validate receipts/ranges against the actual rewritten inputs before
        // composing provenance back through the preprocessing operation.
        source_map::generate(&build.stylesheet, &snapshot)
            .map_err(|e| fail("sourceMap", &request.entry, e))?;
        let maps = preprocess
            .iter()
            .map(|p| (p.id.as_str(), p))
            .collect::<BTreeMap<_, _>>();
        remap_ast(&mut build.stylesheet.ast, &maps);
        for warning in &mut build.stylesheet.warnings {
            warning.source = warning.source.as_ref().and_then(|s| remap_range(s, &maps));
        }
        for imported in &mut build.stylesheet.metadata.imported_stylesheets {
            if let Some(map) = maps.get(imported.id.as_str()) {
                imported.version = map.original_version;
            }
        }
        for import in &mut build.stylesheet.metadata.imports {
            import.source = import.source.as_ref().and_then(|s| remap_range(s, &maps));
        }
        build.stylesheet.source_receipts = project
            .snapshot
            .stylesheets
            .iter()
            .map(|s| {
                (
                    s.id.clone(),
                    StylesheetSourceReceipt {
                        version: s.version,
                        sha256: hash(s.css.as_bytes()),
                    },
                )
            })
            .collect();
        let mut map = source_map::generate(&build.stylesheet, &project.snapshot)
            .map_err(|e| fail("sourceMap", &request.entry, e))?;
        preserve_source_documents(&mut map, &project.snapshot)
            .map_err(|e| fail("sourceMap", &request.entry, e))?;
        let output_urls =
            urls(&build.stylesheet.css).map_err(|e| fail("cssUrl", &request.entry, e))?;
        let mut used_urls: BTreeMap<&str, Vec<&UrlToken>> = BTreeMap::new();
        for url in &output_urls {
            used_urls
                .entry(split_url(&url.url).0)
                .or_default()
                .push(url);
        }
        let mut assets = Vec::new();
        let mut generated_only = Vec::new();
        let mut asset_bytes = project
            .snapshot
            .stylesheets
            .iter()
            .map(|s| s.css.len())
            .sum::<usize>()
            + project
                .snapshot
                .sources
                .iter()
                .map(|s| s.text.len())
                .sum::<usize>();
        for (_, pending) in pending {
            let Some(used) = used_urls.get(pending.token.as_str()) else {
                continue;
            };
            if overlay.get(&pending.path).is_some() {
                return Err(fail(
                    "unsupported",
                    &pending.path,
                    "CSS asset has an authoritative text snapshot; binary asset snapshots require a typed asset provider, disk contents cannot be substituted",
                ));
            }
            let path = match canonical(&pending.path) {
                Ok(path) => path,
                Err(error) => {
                    project.watch.missing.insert(path_string(&pending.path)?);
                    return Err(error);
                }
            };
            if environment_file(&path) {
                return Err(fail(
                    "protectedInput",
                    &path,
                    "CSS assets cannot read environment files",
                ));
            }
            let file = fs::File::open(&path).map_err(|e| fail("asset", &path, e))?;
            let before = file.metadata().map_err(|e| fail("asset", &path, e))?;
            if !before.is_file() || before.len() > request.limits.file_bytes as u64 {
                return Err(fail(
                    "limit",
                    &path,
                    "CSS asset is not a regular bounded file",
                ));
            }
            let mut bytes = Vec::new();
            file.take(request.limits.file_bytes as u64 + 1)
                .read_to_end(&mut bytes)
                .map_err(|e| fail("asset", &path, e))?;
            asset_bytes = asset_bytes.saturating_add(bytes.len());
            if bytes.len() > request.limits.file_bytes || asset_bytes > request.limits.total_bytes {
                return Err(fail("limit", &path, "CSS assets exceed byte limits"));
            }
            let after = fs::metadata(&path).map_err(|e| fail("asset", &path, e))?;
            if before.len() != after.len() || before.modified().ok() != after.modified().ok() {
                return Err(fail(
                    "changedInput",
                    &path,
                    "CSS asset changed while reading; retry",
                ));
            }
            let id = path_string(&path)?;
            let digest = hash(&bytes);
            project.watch.files.insert(id.clone());
            project.watch.sha256.insert(id.clone(), digest.clone());
            for url in used {
                generated_only.push((url.start, url.end));
            }
            assets.push(AssetReference {
                token: pending.token,
                path: id,
                byte_length: bytes.len() as u64,
                sha256: digest,
                query: pending.query,
                fragment: pending.fragment,
                original_url: pending.url,
                occurrences: pending.occurrences,
            });
        }
        mask_generated_urls(&mut map, &build.stylesheet.css, &generated_only)
            .map_err(|e| fail("sourceMap", &request.entry, e))?;
        map = crate::snapshot_fs::compose_maps(&map, &project.input_maps)
            .map_err(|e| fail("sourceMap", &request.entry, e))?;
        let mut diagnostics = build
            .stylesheet
            .unsupported
            .iter()
            .map(|u| CompileDiagnostic {
                severity: "error".into(),
                code: "unsupportedCandidate".into(),
                message: u.reasons.join("; "),
                candidate: Some(u.candidate.clone()),
                source: None,
            })
            .collect::<Vec<_>>();
        diagnostics.extend(build.stylesheet.warnings.iter().map(|w| CompileDiagnostic {
            severity: "warning".into(),
            code: "utilityWarning".into(),
            message: w.message.clone(),
            candidate: None,
            source: w.source.clone(),
        }));
        if !build.stylesheet.remaining_passes.is_empty() {
            diagnostics.push(CompileDiagnostic {
                severity: "error".into(),
                code: "remainingPasses".into(),
                message: format!(
                    "Required CSS passes remain: {:?}",
                    build.stylesheet.remaining_passes
                ),
                candidate: None,
                source: None,
            });
        }
        Ok(CompiledProject {
            project: project.clone(),
            preprocessed_snapshot: snapshot,
            preprocess,
            build,
            map,
            requires_asset_linking: !assets.is_empty(),
            assets,
            diagnostics,
        })
    })();
    operation.map_err(|mut error: HostError| {
        error.watch = project.watch;
        error
    })
}
/// Inline theme declarations may generate utility bodies with no source range.
/// Retain their original documents for asset provenance without assigning a
/// fabricated source anchor to generated CSS. Appending sources preserves every
/// existing source index in the v3 mappings.
fn preserve_source_documents(map: &mut Value, snapshot: &ProjectSnapshot) -> Result<(), String> {
    let mut sources = map["sources"]
        .as_array()
        .ok_or("Missing map sources")?
        .clone();
    let mut contents = map["sourcesContent"]
        .as_array()
        .ok_or("Missing map sourcesContent")?
        .clone();
    if sources.len() != contents.len() {
        return Err("Mismatched map sourcesContent length".into());
    }
    let mut document_only = Vec::new();
    for sheet in &snapshot.stylesheets {
        if let Some(index) = sources
            .iter()
            .position(|s| s.as_str() == Some(sheet.id.as_str()))
        {
            if contents[index].as_str() != Some(sheet.css.as_str()) {
                return Err("Conflicting original map source content".into());
            }
        } else {
            sources.push(Value::String(sheet.id.clone()));
            contents.push(Value::String(sheet.css.clone()));
            document_only.push(sheet.id.clone());
        }
    }
    map["sources"] = Value::Array(sources);
    map["sourcesContent"] = Value::Array(contents);
    map["x_ts_rust_documentOnlySources"] = json!(document_only);
    Ok(())
}
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn version(bytes: &[u8]) -> i64 {
    (u64::from_be_bytes(Sha256::digest(bytes)[..8].try_into().unwrap()) & ((1_u64 << 53) - 1))
        as i64
}
fn rewrite_urls(
    snapshot: &ProjectSnapshot,
    bases: &BTreeMap<String, String>,
) -> Result<
    (
        ProjectSnapshot,
        Vec<StylesheetPreprocess>,
        BTreeMap<String, PendingAsset>,
    ),
    HostError,
> {
    let mut rewritten = snapshot.clone();
    let mut maps = Vec::new();
    let mut assets: BTreeMap<String, PendingAsset> = BTreeMap::new();
    let mut total_bytes = 0usize;
    for sheet in &mut rewritten.stylesheets {
        let path = Path::new(&sheet.id);
        let base = bases
            .get(&sheet.id)
            .ok_or_else(|| fail("cssUrl", path, "Missing original stylesheet URL base"))?;
        let tokens = urls(&sheet.css).map_err(|e| fail("cssUrl", path, e))?;
        let mut output = String::new();
        let mut cursor = 0;
        let mut replacements = Vec::new();
        for url in tokens {
            if passthrough(&url.url) {
                continue;
            }
            let (pathname, query, fragment) = split_url(&url.url);
            let decoded = decode_path(pathname).map_err(|e| fail("cssUrl", path, e))?;
            if decoded.is_empty() {
                return Err(fail(
                    "cssUrl",
                    path,
                    "A query-only CSS URL requires an explicit stylesheet-resource loader",
                ));
            }
            let target = Path::new(base).join(decoded);
            let identity = format!("{}\0{}", sheet.id, url.url);
            let token = format!("__ts_tailwind_asset_{}__", hash(identity.as_bytes()));
            if sheet.css.contains(&token) {
                return Err(fail(
                    "cssUrl",
                    path,
                    "Asset token collides with original source text",
                ));
            }
            assets
                .entry(token.clone())
                .or_insert_with(|| PendingAsset {
                    token: token.clone(),
                    path: target,
                    query: query.clone(),
                    fragment: fragment.clone(),
                    url: url.url.clone(),
                    occurrences: Vec::new(),
                })
                .occurrences
                .push(SourceLocation {
                    file: sheet.id.clone(),
                    start: url.start,
                    end: url.end,
                });
            if assets.len() > 100_000 {
                return Err(fail(
                    "limit",
                    path,
                    "CSS asset reference count exceeds bounds",
                ));
            }
            output.push_str(&sheet.css[cursor..url.start]);
            let start = output.len();
            let mut emitted = token.clone();
            if let Some(q) = query {
                emitted.push('?');
                emitted.push_str(&q);
            }
            if let Some(f) = fragment {
                emitted.push('#');
                emitted.push_str(&f);
            }
            output.push_str("url(\"");
            output.push_str(&css_quote(&emitted));
            output.push_str("\")");
            if output.len().saturating_add(total_bytes) > 64 * 1024 * 1024 {
                return Err(fail(
                    "limit",
                    path,
                    "URL preprocessing exceeds snapshot byte limit",
                ));
            }
            replacements.push(Replacement {
                original_start: url.start,
                original_end: url.end,
                generated_start: start,
                generated_end: output.len(),
                asset_token: token,
            });
            cursor = url.end;
        }
        output.push_str(&sheet.css[cursor..]);
        total_bytes = total_bytes.saturating_add(output.len());
        if total_bytes > 64 * 1024 * 1024 {
            return Err(fail(
                "limit",
                path,
                "URL preprocessing exceeds snapshot byte limit",
            ));
        }
        let generated_version = if replacements.is_empty() {
            sheet.version
        } else {
            version(output.as_bytes())
        };
        maps.push(StylesheetPreprocess {
            id: sheet.id.clone(),
            original_version: sheet.version,
            generated_version,
            original_sha256: hash(sheet.css.as_bytes()),
            generated_sha256: hash(output.as_bytes()),
            replacements,
        });
        sheet.css = output;
        sheet.version = generated_version;
    }
    rewritten
        .validate()
        .map_err(|e| fail("snapshot", Path::new(&snapshot.entry), e))?;
    Ok((rewritten, maps, assets))
}
fn remap_range(
    range: &SourceLocation,
    maps: &BTreeMap<&str, &StylesheetPreprocess>,
) -> Option<SourceLocation> {
    let map = maps.get(range.file.as_str())?;
    Some(SourceLocation {
        file: range.file.clone(),
        start: original_offset(range.start, &map.replacements)?,
        end: original_offset(range.end, &map.replacements)?,
    })
}
fn original_offset(offset: usize, replacements: &[Replacement]) -> Option<usize> {
    let mut generated = 0;
    let mut original = 0;
    for edit in replacements {
        if offset < edit.generated_start {
            break;
        }
        if offset == edit.generated_start {
            return Some(edit.original_start);
        }
        if offset < edit.generated_end {
            return None;
        }
        generated = edit.generated_end;
        original = edit.original_end;
    }
    original.checked_add(offset.checked_sub(generated)?)
}
fn remap_ast(nodes: &mut [AstNode], maps: &BTreeMap<&str, &StylesheetPreprocess>) {
    for node in nodes {
        node.src = node.src.as_ref().and_then(|src| remap_range(src, maps));
        if let Some(children) = node.children_mut() {
            remap_ast(children, maps);
        }
    }
}

fn split_url(url: &str) -> (&str, Option<String>, Option<String>) {
    let (before, fragment) = url
        .split_once('#')
        .map_or((url, None), |(a, b)| (a, Some(b.to_owned())));
    let (path, query) = before
        .split_once('?')
        .map_or((before, None), |(a, b)| (a, Some(b.to_owned())));
    (path, query, fragment)
}
fn passthrough(url: &str) -> bool {
    if url.is_empty() || url.starts_with(['#', '/']) {
        return true;
    }
    let Some((scheme, _)) = url.split_once(':') else {
        return false;
    };
    scheme
        .as_bytes()
        .first()
        .is_some_and(u8::is_ascii_alphabetic)
        && scheme
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'.'))
}
fn decode_path(value: &str) -> Result<String, String> {
    let mut bytes = Vec::new();
    let input = value.as_bytes();
    let mut i = 0;
    while i < input.len() {
        if input[i] == b'%' {
            let digits = input
                .get(i + 1..i + 3)
                .ok_or("Incomplete CSS asset percent escape")?;
            let high = (digits[0] as char)
                .to_digit(16)
                .ok_or("Invalid CSS asset percent escape")?;
            let low = (digits[1] as char)
                .to_digit(16)
                .ok_or("Invalid CSS asset percent escape")?;
            let byte = (high * 16 + low) as u8;
            if matches!(byte, 0 | b'/' | b'\\') {
                return Err(
                    "Encoded separators/NUL in CSS asset paths require an explicit URL loader"
                        .into(),
                );
            }
            bytes.push(byte);
            i += 3;
        } else {
            bytes.push(input[i]);
            i += 1;
        }
    }
    let decoded = String::from_utf8(bytes).map_err(|_| "CSS asset filename is not UTF-8")?;
    if decoded.contains(['\0', '\\']) {
        return Err("Ambiguous backslash/NUL CSS asset path".into());
    }
    Ok(decoded)
}
fn css_quote(value: &str) -> String {
    let mut out = String::new();
    for ch in value.chars() {
        match ch {
            '"' | '\\' => {
                out.push('\\');
                out.push(ch);
            }
            '\n' | '\r' | '\u{c}' => {
                out.push_str(&format!("\\{:x} ", ch as u32));
            }
            _ => out.push(ch),
        }
    }
    out
}
fn whitespace(byte: u8) -> bool {
    matches!(byte, b' ' | b'\n' | b'\r' | b'\t' | 0x0c)
}
fn escape(text: &str, cursor: &mut usize, string: bool) -> Result<Option<char>, String> {
    *cursor += 1;
    let Some(&byte) = text.as_bytes().get(*cursor) else {
        return Err("Trailing CSS escape".into());
    };
    if matches!(byte, b'\n' | b'\r' | 0x0c) {
        if !string {
            return Err("Newline in CSS identifier escape".into());
        }
        *cursor += 1;
        if byte == b'\r' && text.as_bytes().get(*cursor) == Some(&b'\n') {
            *cursor += 1;
        }
        return Ok(None);
    }
    if byte.is_ascii_hexdigit() {
        let mut value = 0;
        let mut digits = 0;
        while digits < 6
            && text
                .as_bytes()
                .get(*cursor)
                .is_some_and(u8::is_ascii_hexdigit)
        {
            value = value * 16 + (text.as_bytes()[*cursor] as char).to_digit(16).unwrap();
            *cursor += 1;
            digits += 1;
        }
        if text.as_bytes().get(*cursor).is_some_and(|b| whitespace(*b)) {
            let cr = text.as_bytes()[*cursor] == b'\r';
            *cursor += 1;
            if cr && text.as_bytes().get(*cursor) == Some(&b'\n') {
                *cursor += 1;
            }
        }
        return Ok(Some(
            char::from_u32(value)
                .filter(|c| *c != '\0')
                .unwrap_or('\u{fffd}'),
        ));
    }
    let ch = text[*cursor..].chars().next().unwrap();
    *cursor += ch.len_utf8();
    Ok(Some(ch))
}
fn quoted(text: &str, cursor: &mut usize) -> Result<String, String> {
    let quote = text.as_bytes()[*cursor];
    *cursor += 1;
    let mut out = String::new();
    while *cursor < text.len() {
        let byte = text.as_bytes()[*cursor];
        if byte == quote {
            *cursor += 1;
            return Ok(out);
        }
        if byte == b'\\' {
            if let Some(ch) = escape(text, cursor, true)? {
                out.push(ch);
            }
            continue;
        }
        if matches!(byte, b'\n' | b'\r' | 0x0c) {
            return Err("Unescaped newline in CSS string".into());
        }
        let ch = text[*cursor..].chars().next().unwrap();
        out.push(ch);
        *cursor += ch.len_utf8();
    }
    Err("Unterminated CSS string".into())
}
/// CSS token-aware traversal. Strings/comments never expose fake URL functions;
/// escaped identifiers and quoted/unquoted URL contents follow CSS escape rules.
fn urls(text: &str) -> Result<Vec<UrlToken>, String> {
    let mut result = Vec::new();
    let mut i = 0;
    while i < text.len() {
        let byte = text.as_bytes()[i];
        if text[i..].starts_with("/*") {
            i += 2;
            let end = text[i..].find("*/").ok_or("Unterminated CSS comment")?;
            i += end + 2;
            continue;
        }
        if matches!(byte, b'\'' | b'"') {
            quoted(text, &mut i)?;
            continue;
        }
        if byte.is_ascii_alphabetic() || matches!(byte, b'_' | b'-' | b'\\') || byte >= 0x80 {
            let start = i;
            let mut name = String::new();
            while i < text.len() {
                let b = text.as_bytes()[i];
                if b == b'\\' {
                    if let Some(ch) = escape(text, &mut i, false)? {
                        name.push(ch);
                    }
                } else if b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-') || b >= 0x80 {
                    let ch = text[i..].chars().next().unwrap();
                    name.push(ch);
                    i += ch.len_utf8();
                } else {
                    break;
                }
            }
            if !name.eq_ignore_ascii_case("url") || text.as_bytes().get(i) != Some(&b'(') {
                continue;
            }
            i += 1;
            while text.as_bytes().get(i).is_some_and(|b| whitespace(*b)) {
                i += 1;
            }
            let url = if text
                .as_bytes()
                .get(i)
                .is_some_and(|b| matches!(b, b'\'' | b'"'))
            {
                let value = quoted(text, &mut i)?;
                while text.as_bytes().get(i).is_some_and(|b| whitespace(*b)) {
                    i += 1;
                }
                value
            } else {
                let mut value = String::new();
                while i < text.len() && text.as_bytes()[i] != b')' {
                    let b = text.as_bytes()[i];
                    if b == b'\\' {
                        if let Some(ch) = escape(text, &mut i, false)? {
                            value.push(ch);
                        }
                        continue;
                    }
                    if whitespace(b) {
                        while text.as_bytes().get(i).is_some_and(|b| whitespace(*b)) {
                            i += 1;
                        }
                        break;
                    }
                    if matches!(b, b'\'' | b'"' | b'(' | 0x7f) || b < 0x20 {
                        return Err("Invalid/dynamic unquoted CSS URL".into());
                    }
                    let ch = text[i..].chars().next().unwrap();
                    value.push(ch);
                    i += ch.len_utf8();
                }
                value
            };
            if text.as_bytes().get(i) != Some(&b')') {
                return Err("Unterminated or invalid CSS URL".into());
            }
            i += 1;
            result.push(UrlToken { start, end: i, url });
            if result.len() > 100_000 {
                return Err("CSS URL count limit exceeded".into());
            }
        } else {
            i += text[i..].chars().next().unwrap().len_utf8();
        }
    }
    Ok(result)
}

// Source-map event composition. Existing maps are provenance anchors, not
// per-character assertions. URL replacement interiors explicitly reset mapping.
#[derive(Clone)]
struct Event {
    line: usize,
    column: i64,
    original: Option<[i64; 3]>,
}
fn mask_generated_urls(
    map: &mut Value,
    css: &str,
    ranges: &[(usize, usize)],
) -> Result<(), String> {
    let mut events = decode_mappings(
        map["mappings"]
            .as_str()
            .ok_or("Missing source map mappings")?,
    )?;
    let mut wanted = ranges.iter().flat_map(|&(a, b)| [a, b]).collect::<Vec<_>>();
    wanted.sort_unstable();
    wanted.dedup();
    let mut positions = BTreeMap::new();
    let mut next = 0;
    let mut line = 0;
    let mut column = 0i64;
    for (offset, ch) in css.char_indices() {
        if wanted.get(next) == Some(&offset) {
            positions.insert(offset, (line, column));
            next += 1;
        }
        if ch == '\n' {
            line += 1;
            column = 0;
        } else {
            column += ch.len_utf16() as i64;
        }
    }
    if wanted.get(next) == Some(&css.len()) {
        positions.insert(css.len(), (line, column));
        next += 1;
    }
    if next != wanted.len() {
        return Err("Asset URL ranges split UTF-8 or exceed generated CSS".into());
    }
    let mut spans = Vec::new();
    let mut intervals = Vec::new();
    for &(start, end) in ranges {
        intervals.push((positions[&start], positions[&end]));
        spans.push(json!({"start":start,"end":end,"mapping":"generatedOnly"}));
    }
    intervals.sort_unstable();
    intervals.dedup();
    let mut interval = 0;
    events.retain(|event| {
        let position = (event.line, event.column);
        while intervals
            .get(interval)
            .is_some_and(|(_, end)| position >= *end)
        {
            interval += 1;
        }
        intervals
            .get(interval)
            .is_none_or(|(start, _)| position < *start)
    });
    events.extend(intervals.iter().map(|&(start, _)| Event {
        line: start.0,
        column: start.1,
        original: None,
    }));
    events.sort_by_key(|e| (e.line, e.column));
    map["mappings"] = Value::String(encode_mappings(
        &events,
        css.bytes().filter(|b| *b == b'\n').count() + 1,
    ));
    map["x_ts_rust_assetRegions"] = Value::Array(spans);
    map["x_ts_rust_preprocessing"] = json!(
        "relative URLs bound before Tailwind flattening; original provenance anchors composed through exact unchanged spans; rewritten URL regions unmapped"
    );
    Ok(())
}
const BASE64: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
fn read_vlq(bytes: &[u8], cursor: &mut usize) -> Result<i64, String> {
    let mut n = 0u64;
    let mut shift = 0;
    loop {
        let byte = *bytes.get(*cursor).ok_or("Truncated source-map VLQ")?;
        *cursor += 1;
        let digit = BASE64
            .iter()
            .position(|v| *v == byte)
            .ok_or("Invalid source-map base64")? as u64;
        n |= (digit & 31) << shift;
        if digit & 32 == 0 {
            break;
        }
        shift += 5;
        if shift > 55 {
            return Err("Source-map VLQ exceeds bounds".into());
        }
    }
    Ok(if n & 1 == 1 {
        -((n >> 1) as i64)
    } else {
        (n >> 1) as i64
    })
}
fn vlq(value: i64, out: &mut String) {
    let mut n = (value.unsigned_abs() << 1) | u64::from(value < 0);
    loop {
        let mut digit = (n & 31) as usize;
        n >>= 5;
        if n != 0 {
            digit |= 32;
        }
        out.push(BASE64[digit] as char);
        if n == 0 {
            break;
        }
    }
}
fn decode_mappings(text: &str) -> Result<Vec<Event>, String> {
    let mut result = Vec::new();
    let mut original = [0i64; 3];
    for (line, text) in text.split(';').enumerate() {
        let mut column = 0;
        for segment in text.split(',').filter(|s| !s.is_empty()) {
            let mut cursor = 0;
            column += read_vlq(segment.as_bytes(), &mut cursor)?;
            let origin = if cursor == segment.len() {
                None
            } else {
                for field in &mut original {
                    *field += read_vlq(segment.as_bytes(), &mut cursor)?;
                }
                if cursor != segment.len() {
                    return Err("Unexpected named source-map segment".into());
                }
                Some(original)
            };
            if column < 0 || origin.is_some_and(|o| o.iter().any(|v| *v < 0)) {
                return Err("Negative source-map coordinate".into());
            }
            result.push(Event {
                line,
                column,
                original: origin,
            });
        }
    }
    Ok(result)
}
fn encode_mappings(events: &[Event], lines: usize) -> String {
    let mut out = String::new();
    let mut original = [0i64; 3];
    let mut at = 0;
    for line in 0..lines {
        if line > 0 {
            out.push(';');
        }
        let mut column = 0;
        let mut count = 0;
        while let Some(event) = events.get(at).filter(|e| e.line == line) {
            if count > 0 {
                out.push(',');
            }
            vlq(event.column - column, &mut out);
            column = event.column;
            if let Some(origin) = event.original {
                for i in 0..3 {
                    vlq(origin[i] - original[i], &mut out);
                    original[i] = origin[i];
                }
            }
            at += 1;
            count += 1;
        }
    }
    out
}

#[cfg(test)]
#[path = "compile_tests.rs"]
mod tests;
