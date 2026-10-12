//! Source map v3 generation from the printed Tailwind tree and original snapshots.
//!
//! Provenance-anchor algorithm follows Tailwind CSS v4.2.1 (MIT, copyright
//! Tailwind Labs), packages/tailwindcss/src/source-maps/{source-map,line-table}.ts.
//! Unlike a range translation, anchors do not assert character correspondence
//! inside transformed nodes. The extension x_ts_rust_mappingMode records this.
//! Generated-only line starts/nodes and positions after node-end anchors reset
//! provenance, preventing closing braces and compiler helpers inheriting a map.
//! All offsets are validated UTF-8 bytes; emitted columns count UTF-16 units.
use crate::{
    ast::{AstNode, NodeKind, SourceLocation, to_css},
    snapshot::ProjectSnapshot,
    stylesheet::StylesheetBuild,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

const MAX_BYTES: usize = 64 * 1024 * 1024;
const MAX_SCAN: usize = 128 * 1024 * 1024;
const MAX_NODES: usize = 1_000_000;
const MAX_EVENTS: usize = 4_000_000;
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Position {
    line: u32,
    column: u32,
}
struct LineTable<'a> {
    text: &'a str,
    checkpoints: Vec<(usize, Position)>,
}
impl<'a> LineTable<'a> {
    fn new(text: &'a str) -> Self {
        let mut checkpoints = vec![(0, Position { line: 0, column: 0 })];
        let mut position = Position { line: 0, column: 0 };
        for (count, (offset, ch)) in text.char_indices().enumerate() {
            if count > 0 && count % 128 == 0 {
                checkpoints.push((offset, position));
            }
            advance(&mut position, ch);
        }
        Self { text, checkpoints }
    }
    fn position(&self, offset: usize) -> Result<Position, String> {
        if offset > self.text.len() || !self.text.is_char_boundary(offset) {
            return Err("Source-map offset is not a valid UTF-8 boundary".into());
        }
        let index = self
            .checkpoints
            .partition_point(|(byte, _)| *byte <= offset)
            - 1;
        let (start, mut position) = self.checkpoints[index];
        for ch in self.text[start..offset].chars() {
            advance(&mut position, ch);
        }
        Ok(position)
    }
}
fn advance(position: &mut Position, ch: char) {
    // Pinned Tailwind uses LF line breaks. CRLF counts as one line break; the
    // CR has a UTF-16 column before LF. Bare CR does not invent a new line.
    if ch == '\n' {
        position.line += 1;
        position.column = 0;
    } else {
        position.column += ch.len_utf16() as u32;
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Original {
    source: usize,
    position: Position,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Mapping {
    generated: Position,
    original: Option<Original>,
}
fn validate_range(range: &SourceLocation, text: &str) -> Result<(), String> {
    if range.start > range.end
        || range.end > text.len()
        || !text.is_char_boundary(range.start)
        || !text.is_char_boundary(range.end)
    {
        return Err(format!(
            "Invalid source-map byte range {}:{}..{}",
            range.file, range.start, range.end
        ));
    }
    Ok(())
}
fn tree<'a>(ast: &'a [AstNode]) -> Result<Vec<&'a AstNode>, String> {
    let mut stack = ast
        .iter()
        .rev()
        .map(|node| (node, 0usize))
        .collect::<Vec<_>>();
    let mut nodes = Vec::new();
    let mut printed = 0usize;
    while let Some((node, depth)) = stack.pop() {
        if depth > 256 || nodes.len() == MAX_NODES {
            return Err("Source-map AST nesting/node limit exceeded".into());
        }
        let (payload, closing) = match &node.kind {
            NodeKind::Declaration {
                property,
                value,
                important,
            } => {
                let value = value
                    .as_ref()
                    .ok_or("Source maps require a finalized declaration value")?;
                (
                    property
                        .len()
                        .saturating_add(value.len())
                        .saturating_add(if *important { 15 } else { 4 }),
                    false,
                )
            }
            NodeKind::Rule { selector, .. } => (selector.len().saturating_add(3), true),
            NodeKind::AtRule {
                name,
                params,
                nodes,
            } => (
                name.len().saturating_add(params.len()).saturating_add(4),
                !nodes.is_empty(),
            ),
            NodeKind::Comment { value } => (value.len().saturating_add(5), false),
            NodeKind::Context { .. } | NodeKind::AtRoot { .. } => {
                return Err("Source maps require lowered context/at-root nodes".into());
            }
        };
        printed = printed.saturating_add(depth * 2).saturating_add(payload);
        if closing {
            printed = printed.saturating_add(depth * 2 + 2);
        }
        if printed > MAX_BYTES {
            return Err("Source-map printed AST exceeds 64 MiB".into());
        }
        nodes.push(node);
        if let Some(children) = node.children() {
            stack.extend(children.iter().rev().map(|node| (node, depth + 1)));
        }
    }
    Ok(nodes)
}
fn push(mappings: &mut Vec<Mapping>, mapping: Mapping) -> Result<(), String> {
    if mappings.len() == MAX_EVENTS {
        return Err("Source map exceeds four million anchor/boundary events".into());
    }
    mappings.push(mapping);
    Ok(())
}
/// Generate a self-contained standard v3 map. Source identities remain exactly
/// the snapshot's opaque IDs; no path resolution, filesystem, or host execution.
/// Requires the immutable build receipts and rejects absent, changed, or stale
/// sources and any mismatch between the AST printer ranges and generated CSS.
pub fn generate(build: &StylesheetBuild, snapshot: &ProjectSnapshot) -> Result<Value, String> {
    snapshot.validate()?;
    if build.metadata.entry != snapshot.entry {
        return Err("Source-map entry does not match the build".into());
    }
    if build.css.len() > MAX_BYTES {
        return Err("Generated source-map CSS exceeds 64 MiB".into());
    }
    let sheets = snapshot
        .stylesheets
        .iter()
        .map(|sheet| (sheet.id.as_str(), sheet))
        .collect::<BTreeMap<_, _>>();
    let ids = sheets.keys().copied().collect::<BTreeSet<_>>();
    if build
        .source_receipts
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>()
        != ids
    {
        return Err("Source-map build receipts do not match the stylesheet snapshot set".into());
    }
    for (id, receipt) in &build.source_receipts {
        let sheet = sheets
            .get(id.as_str())
            .ok_or_else(|| format!("Missing source-map stylesheet {id}"))?;
        if sheet.version != receipt.version
            || format!("{:x}", Sha256::digest(sheet.css.as_bytes())) != receipt.sha256
        {
            return Err(format!("Stale source-map stylesheet receipt for {id}"));
        }
    }
    for imported in &build.metadata.imported_stylesheets {
        let sheet = sheets
            .get(imported.id.as_str())
            .ok_or_else(|| format!("Missing imported source-map stylesheet {}", imported.id))?;
        if imported.version != sheet.version {
            return Err(format!(
                "Stale imported stylesheet version for {}",
                imported.id
            ));
        }
    }
    let nodes = tree(&build.ast)?;
    let mut reproduced = build.ast.clone();
    if to_css(&mut reproduced, &build.metadata.entry)? != build.css {
        return Err("Generated CSS does not match the source-map AST printer".into());
    }
    let reproduced_nodes = tree(&reproduced)?;
    let mut referenced = BTreeSet::new();
    for (node, printed) in nodes.iter().zip(reproduced_nodes) {
        if node.dst != printed.dst {
            return Err("Stale or missing generated source-map AST ranges".into());
        }
        let dst = node
            .dst
            .as_ref()
            .ok_or("Missing generated source-map range")?;
        if dst.file != build.metadata.entry {
            return Err("Generated source-map range has an unexpected file identity".into());
        }
        validate_range(dst, &build.css)?;
        if let Some(src) = &node.src {
            let sheet = sheets
                .get(src.file.as_str())
                .ok_or_else(|| format!("Missing original source-map stylesheet {}", src.file))?;
            validate_range(src, &sheet.css)?;
            referenced.insert(src.file.as_str());
        }
    }
    let sources = referenced.into_iter().collect::<Vec<_>>();
    let indices = sources
        .iter()
        .enumerate()
        .map(|(index, id)| (*id, index))
        .collect::<BTreeMap<_, _>>();
    let tables = sources
        .iter()
        .map(|id| LineTable::new(&sheets[id].css))
        .collect::<Vec<_>>();
    let generated = LineTable::new(&build.css);
    let mut mappings = Vec::new();
    // Every line starts unmapped, including closing braces, blank lines, and
    // compiler-created rules without source locations. A real anchor at column
    // zero takes precedence over this reset during normalization below.
    push(
        &mut mappings,
        Mapping {
            generated: Position { line: 0, column: 0 },
            original: None,
        },
    )?;
    let mut line = 0;
    for byte in build.css.bytes() {
        if byte == b'\n' {
            line += 1;
            push(
                &mut mappings,
                Mapping {
                    generated: Position { line, column: 0 },
                    original: None,
                },
            )?;
        }
    }
    let mut scanned = 0usize;
    for node in nodes {
        let dst = node
            .dst
            .as_ref()
            .ok_or("Missing printed source-map range")?;
        let start = generated.position(dst.start)?;
        let end = generated.position(dst.end)?;
        if let Some(src) = &node.src {
            let source = indices[src.file.as_str()];
            let table = &tables[source];
            let original_slice = &table.text[src.start..src.end];
            scanned = scanned.saturating_add(original_slice.len());
            if scanned > MAX_SCAN {
                return Err("Source-map original range scanning exceeds 128 MiB".into());
            }
            // Upstream emits each nonempty original line at the node's generated
            // start. This is intentionally provenance, not an exact range map.
            let mut offset = 0;
            for text in original_slice.split('\n') {
                if !text
                    .trim_matches(|ch: char| ch.is_whitespace() || ch == '\u{feff}')
                    .is_empty()
                {
                    push(
                        &mut mappings,
                        Mapping {
                            generated: start,
                            original: Some(Original {
                                source,
                                position: table.position(src.start + offset)?,
                            }),
                        },
                    )?;
                }
                offset += text.len() + 1;
            }
            push(
                &mut mappings,
                Mapping {
                    generated: end,
                    original: Some(Original {
                        source,
                        position: table.position(src.end)?,
                    }),
                },
            )?;
            // A point at dst.end is the upstream end anchor. Do not let it
            // extend through later generated punctuation/text on the same line.
            if let Some(ch) = build.css[dst.end..].chars().next() {
                push(
                    &mut mappings,
                    Mapping {
                        generated: generated.position(dst.end + ch.len_utf8())?,
                        original: None,
                    },
                )?;
            }
        } else {
            push(
                &mut mappings,
                Mapping {
                    generated: start,
                    original: None,
                },
            )?;
            push(
                &mut mappings,
                Mapping {
                    generated: end,
                    original: None,
                },
            )?;
        }
    }
    mappings.sort_by(|a, b| {
        a.generated
            .cmp(&b.generated)
            .then_with(|| match (a.original, b.original) {
                (None, None) => std::cmp::Ordering::Equal,
                (None, Some(_)) => std::cmp::Ordering::Less,
                (Some(_), None) => std::cmp::Ordering::Greater,
                (Some(a), Some(b)) => a.position.cmp(&b.position),
            })
    });
    let mut normalized: Vec<Mapping> = Vec::with_capacity(mappings.len());
    for mapping in mappings {
        if let Some(last) = normalized.last() {
            if *last == mapping {
                continue;
            }
            let last: &Mapping = last;
            if last.generated == mapping.generated
                && last.original.is_none()
                && mapping.original.is_some()
            {
                normalized.pop();
            }
        }
        normalized.push(mapping);
    }
    Ok(json!({
        "version":3,"file":build.metadata.entry,"sources":sources,
        "sourcesContent":sources.iter().map(|id|sheets[id].css.as_str()).collect::<Vec<_>>(),
        "names":[],"mappings":encode(&normalized),
        "x_ts_rust_mappingMode":"provenance-anchors",
    }))
}
fn vlq(value: i64, output: &mut String) {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut value = (value.unsigned_abs() << 1) | u64::from(value < 0);
    loop {
        let mut digit = (value & 31) as usize;
        value >>= 5;
        if value != 0 {
            digit |= 32;
        }
        output.push(ALPHABET[digit] as char);
        if value == 0 {
            break;
        }
    }
}
fn encode(mappings: &[Mapping]) -> String {
    let mut result = String::new();
    let mut line = 0;
    let mut generated_column = 0i64;
    let mut source = 0i64;
    let mut original_line = 0i64;
    let mut original_column = 0i64;
    let mut first = true;
    for mapping in mappings {
        while line < mapping.generated.line {
            result.push(';');
            line += 1;
            generated_column = 0;
            first = true;
        }
        if !first {
            result.push(',');
        }
        first = false;
        vlq(
            i64::from(mapping.generated.column) - generated_column,
            &mut result,
        );
        generated_column = i64::from(mapping.generated.column);
        if let Some(original) = mapping.original {
            vlq(original.source as i64 - source, &mut result);
            source = original.source as i64;
            vlq(
                i64::from(original.position.line) - original_line,
                &mut result,
            );
            original_line = i64::from(original.position.line);
            vlq(
                i64::from(original.position.column) - original_column,
                &mut result,
            );
            original_column = i64::from(original.position.column);
        }
    }
    result
}
#[cfg(test)]
#[path = "source_map_tests.rs"]
mod tests;
