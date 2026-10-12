//! Compose rune diagnostics through a native TypeScript v3 source map.
//!
//! Source-map points are anchors, not copied spans. We extend them only through
//! identical complete lexical units, bounded by the next map segment, and never
//! interpolate through generated helpers or changed formatting. Ambiguous or
//! partially generated ranges remain visible with an explicit unmapped result.
use oxc_syntax::identifier::{is_identifier_part, is_identifier_start};
use serde::Deserialize;
use serde_json::{Value, json};
use ts_svelte::{ByteRange, Document, Position, Range, Snapshot};

const MAX_DOCUMENT: usize = 8 * 1024 * 1024;
const MAX_MAP: usize = 32 * 1024 * 1024;
const MAX_SEGMENTS: usize = 1_000_000;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SourceMap {
    version: u32,
    sources: Vec<String>,
    sources_content: Option<Vec<Option<String>>>,
    #[serde(default)]
    names: Vec<String>,
    mappings: String,
    #[serde(default)]
    source_root: String,
    sections: Option<Value>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Anchor {
    generated: usize,
    original: Option<usize>,
}

#[derive(Clone, Copy, Debug)]
struct CopySpan {
    generated: ByteRange,
    original: ByteRange,
}

fn base64(byte: u8) -> Option<u8> {
    Some(match byte {
        b'A'..=b'Z' => byte - b'A',
        b'a'..=b'z' => byte - b'a' + 26,
        b'0'..=b'9' => byte - b'0' + 52,
        b'+' => 62,
        b'/' => 63,
        _ => return None,
    })
}

fn vlq(segment: &str) -> Result<Vec<i64>, String> {
    let mut values = Vec::with_capacity(5);
    let mut value = 0_u64;
    let mut shift = 0_u32;
    for byte in segment.bytes() {
        let digit = base64(byte).ok_or("Invalid source-map base64 VLQ character")?;
        let payload = u64::from(digit & 31);
        if shift >= 63 || payload > (u64::MAX >> shift) {
            return Err("Source-map VLQ overflow".into());
        }
        value |= payload << shift;
        if digit & 32 != 0 {
            shift += 5;
            continue;
        }
        let magnitude = i64::try_from(value >> 1).map_err(|_| "Source-map VLQ overflow")?;
        values.push(if value & 1 == 0 {
            magnitude
        } else {
            -magnitude
        });
        if values.len() > 5 {
            return Err("Source-map segment has too many fields".into());
        }
        shift = 0;
        value = 0;
    }
    if shift != 0 {
        return Err("Unterminated source-map VLQ".into());
    }
    if !matches!(values.len(), 1 | 4 | 5) {
        return Err("Source-map segment must have 1, 4 or 5 fields".into());
    }
    Ok(values)
}

fn delta(state: &mut i64, change: i64) -> Result<u32, String> {
    *state = state
        .checked_add(change)
        .ok_or("Source-map delta overflow")?;
    u32::try_from(*state).map_err(|_| "Negative or overflowing source-map coordinate".into())
}

fn normalized_path(value: &str) -> String {
    let value = url::Url::parse(value)
        .ok()
        .map_or_else(|| value.to_owned(), |url| url.path().to_owned());
    let value = percent_encoding::percent_decode_str(&value)
        .decode_utf8_lossy()
        .replace('\\', "/");
    let mut parts = Vec::new();
    for part in value.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            part => parts.push(part),
        }
    }
    parts.join("/")
}

fn source_matches(uri: &str, source_root: &str, source: &str) -> bool {
    let source =
        if source_root.is_empty() || source.starts_with('/') || url::Url::parse(source).is_ok() {
            source.to_owned()
        } else {
            format!("{}/{source}", source_root.trim_end_matches('/'))
        };
    let absolute = source.starts_with('/') || url::Url::parse(&source).is_ok();
    let original = normalized_path(uri);
    let candidate = normalized_path(&source);
    !candidate.is_empty()
        && (original == candidate || (!absolute && original.ends_with(&format!("/{candidate}"))))
}

fn selected_source(map: &SourceMap, original: &Snapshot) -> Result<usize, String> {
    let contents = map
        .sources_content
        .as_ref()
        .ok_or("Source map must include sourcesContent to bind the original snapshot")?;
    if contents.len() != map.sources.len() {
        return Err("sourcesContent length does not match sources".into());
    }
    let matches: Vec<_> = map
        .sources
        .iter()
        .zip(contents)
        .enumerate()
        .filter(|(_, (source, content))| {
            content.as_deref() == Some(original.text.as_str())
                && source_matches(&original.uri, &map.source_root, source)
        })
        .map(|(index, _)| index)
        .collect();
    match matches.as_slice() {
        [index] => Ok(*index),
        [] => Err("Source map does not match the original URI and source contents".into()),
        _ => Err("Source map has ambiguous entries for the original snapshot".into()),
    }
}

fn decode(
    map: &SourceMap,
    selected: usize,
    original: &Document,
    generated: &Document,
) -> Result<Vec<Anchor>, String> {
    let mut anchors: Vec<Anchor> = Vec::new();
    let mut source = 0;
    let mut original_line = 0;
    let mut original_column = 0;
    let mut name = 0;
    for (line, mapping) in map.mappings.split(';').enumerate() {
        let line = u32::try_from(line).map_err(|_| "Too many source-map lines")?;
        let mut column = 0;
        let mut previous_column = 0;
        if mapping.is_empty() {
            continue;
        }
        for segment in mapping.split(',') {
            if anchors.len() >= MAX_SEGMENTS {
                return Err("Source-map segment limit exceeded".into());
            }
            let values = vlq(segment)?;
            let generated_column = delta(&mut column, values[0])?;
            if generated_column < previous_column {
                return Err("Source-map generated columns are not ordered".into());
            }
            previous_column = generated_column;
            let generated_byte = generated
                .offset_at(Position {
                    line,
                    character: generated_column,
                })
                .map_err(|error| format!("Invalid generated source-map position: {error}"))?;
            let original_byte = if values.len() >= 4 {
                let source_index = delta(&mut source, values[1])? as usize;
                if source_index >= map.sources.len() {
                    return Err("Source-map source index is out of bounds".into());
                }
                let line = delta(&mut original_line, values[2])?;
                let character = delta(&mut original_column, values[3])?;
                if values.len() == 5 && delta(&mut name, values[4])? as usize >= map.names.len() {
                    return Err("Source-map name index is out of bounds".into());
                }
                if source_index == selected {
                    Some(
                        original
                            .offset_at(Position { line, character })
                            .map_err(|error| {
                                format!("Invalid original source-map position: {error}")
                            })?,
                    )
                } else {
                    None
                }
            } else {
                None
            };
            let anchor = Anchor {
                generated: generated_byte,
                original: original_byte,
            };
            if let Some(previous) = anchors.last_mut()
                && previous.generated == anchor.generated
            {
                // Equal points may occur in emitted maps. Conflicting points
                // are deliberately unmapped rather than choosing an ordering.
                if previous.original != anchor.original {
                    previous.original = None;
                }
            } else {
                anchors.push(anchor);
            }
        }
    }
    Ok(anchors)
}

fn char_at(text: &str, byte: usize) -> Option<char> {
    text.get(byte..)?.chars().next()
}

fn boundary(text: &str, byte: usize) -> bool {
    let before = text.get(..byte).and_then(|s| s.chars().next_back());
    let after = char_at(text, byte);
    !matches!((before, after), (Some(a), Some(b)) if is_identifier_part(a) && is_identifier_part(b))
}

/// A deliberately conservative lexical unit recognizer. Regex/division and
/// template interpolation can remain unmapped; they are never guessed copied.
fn unit_end(text: &str, start: usize) -> usize {
    let Some(first) = char_at(text, start) else {
        return start;
    };
    let mut end = start + first.len_utf8();
    if first.is_whitespace() {
        while let Some(ch) = char_at(text, end) {
            if !ch.is_whitespace() {
                break;
            }
            end += ch.len_utf8();
        }
    } else if is_identifier_start(first) {
        while let Some(ch) = char_at(text, end) {
            if !is_identifier_part(ch) {
                break;
            }
            end += ch.len_utf8();
        }
    } else if first.is_ascii_digit() {
        let mut previous = first;
        while let Some(ch) = char_at(text, end) {
            if !(ch.is_ascii_alphanumeric()
                || matches!(ch, '.' | '_')
                || (matches!(ch, '+' | '-') && matches!(previous, 'e' | 'E')))
            {
                break;
            }
            end += ch.len_utf8();
            previous = ch;
        }
    } else if matches!(first, '\'' | '"' | '`') {
        let mut escaped = false;
        while let Some(ch) = char_at(text, end) {
            end += ch.len_utf8();
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == first {
                return end;
            }
        }
    } else if first == '/' {
        if char_at(text, end) == Some('/') {
            while let Some(ch) = char_at(text, end) {
                if matches!(ch, '\r' | '\n') {
                    break;
                }
                end += ch.len_utf8();
            }
        } else if char_at(text, end) == Some('*') {
            return text[end + 1..]
                .find("*/")
                .map_or(text.len(), |offset| end + 1 + offset + 2);
        } else {
            // Without a parser's expression context a slash could be division
            // or a regex. It is only mapped by an exact bounded anchor pair.
            return text.len();
        }
    } else if "+-*%=!<>?&|^~.".contains(first) {
        while let Some(ch) = char_at(text, end) {
            if !"+-*%=!<>?&|^~.".contains(ch) {
                break;
            }
            end += ch.len_utf8();
        }
    }
    end
}

fn copied_prefix(
    generated: &str,
    original: &str,
    generated_start: usize,
    original_start: usize,
    generated_bound: usize,
    original_bound: usize,
) -> usize {
    if !boundary(generated, generated_start) || !boundary(original, original_start) {
        return 0;
    }
    let mut copied = 0;
    while generated_start + copied < generated_bound && original_start + copied < original_bound {
        let g = generated_start + copied;
        let o = original_start + copied;
        let ge = unit_end(generated, g);
        let oe = unit_end(original, o);
        if ge > generated_bound || oe > original_bound || generated[g..ge] != original[o..oe] {
            break;
        }
        if ge == g {
            break;
        }
        copied += ge - g;
    }
    copied
}

fn line_end(text: &str, start: usize) -> usize {
    text[start..]
        .find(['\r', '\n'])
        .map_or(text.len(), |offset| start + offset)
}

fn spans(anchors: &[Anchor], original: &str, generated: &str) -> Vec<CopySpan> {
    let mut copies: Vec<CopySpan> = Vec::new();
    for (index, anchor) in anchors.iter().enumerate() {
        let Some(original_start) = anchor.original else {
            continue;
        };
        let next = anchors.get(index + 1);
        let generated_end = next.map_or_else(
            || line_end(generated, anchor.generated),
            |point| point.generated,
        );
        let original_end = next
            .and_then(|point| point.original)
            .filter(|&end| end >= original_start)
            .unwrap_or_else(|| line_end(original, original_start));
        let length = if generated_end >= anchor.generated
            && original_end >= original_start
            && generated[anchor.generated..generated_end] == original[original_start..original_end]
        {
            generated_end - anchor.generated
        } else {
            // A later segment that maps backward or to the same original
            // point marks transformed/repeated code, not a copied interval.
            if next
                .and_then(|point| point.original)
                .is_some_and(|end| end <= original_start)
            {
                continue;
            }
            copied_prefix(
                generated,
                original,
                anchor.generated,
                original_start,
                generated_end,
                original_end,
            )
        };
        if length == 0 {
            continue;
        }
        let copy = CopySpan {
            generated: ByteRange {
                start: anchor.generated,
                end: anchor.generated + length,
            },
            original: ByteRange {
                start: original_start,
                end: original_start + length,
            },
        };
        if let Some(previous) = copies.last_mut()
            && previous.generated.end == copy.generated.start
            && previous.original.end == copy.original.start
        {
            previous.generated.end = copy.generated.end;
            previous.original.end = copy.original.end;
        } else {
            copies.push(copy);
        }
    }
    copies
}

fn map_bytes(bytes: ByteRange, anchors: &[Anchor], spans: &[CopySpan]) -> Option<ByteRange> {
    if bytes.start == bytes.end {
        if let Ok(index) = anchors.binary_search_by_key(&bytes.start, |point| point.generated) {
            return anchors[index].original.map(|position| ByteRange {
                start: position,
                end: position,
            });
        }
    }
    spans
        .iter()
        .find(|span| span.generated.start <= bytes.start && bytes.end <= span.generated.end)
        .map(|span| ByteRange {
            start: span.original.start + bytes.start - span.generated.start,
            end: span.original.start + bytes.end - span.generated.start,
        })
}

/// Map a single compiler `checkModule` diagnostic back to its original snapshot.
///
/// Raw generated URI/bytes/range are retained in `generated`. Valid but
/// unprovable spans return a visible diagnostic with null original coordinates.
/// # Errors
/// Rejects malformed maps, missing/mismatching source contents, invalid UTF-16
/// positions, inconsistent diagnostic coordinates, and resource-limit excess.
pub fn map_diagnostic(
    original: &Snapshot,
    transpiled: &str,
    map_json: &str,
    mut diagnostic: Value,
) -> Result<Value, String> {
    if original.text.len() > MAX_DOCUMENT
        || transpiled.len() > MAX_DOCUMENT
        || map_json.len() > MAX_MAP
    {
        return Err("Transpiler mapping input exceeds size limit".into());
    }
    if !diagnostic.is_object() {
        return Err("Compiler diagnostic must be an object".into());
    }
    let map: SourceMap =
        serde_json::from_str(map_json).map_err(|error| format!("Invalid source map: {error}"))?;
    if map.version != 3 || map.sections.is_some() {
        return Err("Expected a flat version-3 source map".into());
    }
    let selected = selected_source(&map, original)?;
    let generated = Document::new(Snapshot {
        uri: diagnostic["uri"]
            .as_str()
            .unwrap_or(&original.uri)
            .to_owned(),
        version: original.version,
        text: transpiled.to_owned(),
    });
    let document = Document::new(original.clone());
    let anchors = decode(&map, selected, &document, &generated)?;
    let copies = spans(&anchors, &original.text, transpiled);
    let raw_bytes = diagnostic.get("bytes").cloned().unwrap_or(Value::Null);
    let raw_range = diagnostic.get("range").cloned().unwrap_or(Value::Null);
    let bytes = if raw_bytes.is_null() {
        None
    } else {
        let bytes: ByteRange = serde_json::from_value(raw_bytes.clone())
            .map_err(|error| format!("Invalid compiler byte range: {error}"))?;
        generated
            .validate_bytes(bytes)
            .map_err(|error| error.to_string())?;
        Some(bytes)
    };
    let range_bytes = if raw_range.is_null() {
        None
    } else {
        let range: Range = serde_json::from_value(raw_range.clone())
            .map_err(|error| format!("Invalid compiler UTF-16 range: {error}"))?;
        Some(
            generated
                .byte_range(range)
                .map_err(|error| error.to_string())?,
        )
    };
    if bytes.is_some() && range_bytes.is_some() && bytes != range_bytes {
        return Err("Compiler byte and UTF-16 diagnostic ranges disagree".into());
    }
    let mapped = bytes
        .or(range_bytes)
        .and_then(|bytes| map_bytes(bytes, &anchors, &copies))
        .and_then(|bytes| {
            document
                .editor_range(bytes)
                .ok()
                .map(|range| (bytes, range))
        });
    diagnostic["generated"] =
        json!({"uri":generated.snapshot().uri,"bytes":raw_bytes,"range":raw_range});
    diagnostic["uri"] = json!(original.uri);
    diagnostic["version"] = json!(original.version);
    if let Some((bytes, range)) = mapped {
        diagnostic["bytes"] = json!(bytes);
        diagnostic["range"] = json!(range);
        diagnostic["mapping"] = json!("native-ts-transpile-mapped");
        diagnostic
            .as_object_mut()
            .expect("checked object")
            .remove("mappingReason");
    } else {
        diagnostic["bytes"] = Value::Null;
        diagnostic["range"] = Value::Null;
        diagnostic["mapping"] = json!("native-ts-transpile-unmapped");
        diagnostic["mappingReason"] = json!(if bytes.or(range_bytes).is_some() {
            "Diagnostic crosses changed, ambiguous or unanchored generated text"
        } else {
            "Compiler diagnostic has no generated range"
        });
    }
    Ok(diagnostic)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encode(value: i64) -> String {
        let mut value = if value < 0 {
            ((-value) as u64) * 2 + 1
        } else {
            value as u64 * 2
        };
        let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut result = String::new();
        loop {
            let mut digit = (value & 31) as usize;
            value >>= 5;
            if value != 0 {
                digit |= 32;
            }
            result.push(char::from(alphabet[digit]));
            if value == 0 {
                break;
            }
        }
        result
    }

    fn snapshot(source: &str) -> Snapshot {
        Snapshot {
            uri: "file:///work/state.svelte.ts".into(),
            version: 7,
            text: source.into(),
        }
    }

    fn map(source: &Snapshot, generated: &str, points: &[(usize, Option<usize>)]) -> String {
        let original = Document::new(source.clone());
        let document = Document::new(snapshot(generated));
        let mut mappings = String::new();
        let mut line = 0;
        let mut column = 0;
        let mut original_line = 0;
        let mut original_column = 0;
        let mut first = true;
        for &(g, o) in points {
            let g = document.position_at(g).unwrap();
            while line < g.line {
                mappings.push(';');
                line += 1;
                column = 0;
                first = true;
            }
            if !first {
                mappings.push(',');
            }
            first = false;
            mappings.push_str(&encode(i64::from(g.character) - column));
            column = i64::from(g.character);
            if let Some(o) = o {
                let o = original.position_at(o).unwrap();
                mappings.push_str(&encode(0));
                mappings.push_str(&encode(i64::from(o.line) - original_line));
                mappings.push_str(&encode(i64::from(o.character) - original_column));
                original_line = i64::from(o.line);
                original_column = i64::from(o.character);
            }
        }
        json!({"version":3,"sources":["state.svelte.ts"],"sourcesContent":[source.text],"names":[],"mappings":mappings}).to_string()
    }

    fn diagnostic(text: &str, bytes: ByteRange) -> Value {
        json!({"code":"rune_error","message":"Invalid rune","severity":"error","uri":"state.svelte.js","bytes":bytes,"range":Document::new(snapshot(text)).editor_range(bytes).unwrap()})
    }

    #[test]
    fn unicode_rune_after_parameter_property_maps_to_original_utf16() {
        let source = snapshot(
            "class Box { constructor(public name: string) {} }\r\nconst π = $state(\"🙂\");\r\n",
        );
        let generated = "class Box {\n    constructor(name) {\n        this.name = name;\n    }\n}\nconst π = $state(\"🙂\");\n";
        let start = generated.find("$state").unwrap();
        let bytes = ByteRange {
            start,
            end: start + "$state(\"🙂\")".len(),
        };
        let mapping = map(
            &source,
            generated,
            &[
                (generated.find("this.name").unwrap(), None),
                (
                    generated.find("const π").unwrap(),
                    source.text.find("const π"),
                ),
            ],
        );
        let result =
            map_diagnostic(&source, generated, &mapping, diagnostic(generated, bytes)).unwrap();
        assert_eq!(result["mapping"], "native-ts-transpile-mapped");
        assert_eq!(result["range"]["start"], json!({"line":1,"character":10}));
        assert_eq!(result["range"]["end"], json!({"line":1,"character":22}));
        assert_eq!(result["generated"]["bytes"], json!(bytes));
        assert_eq!(result["uri"], source.uri);
    }

    #[test]
    fn generated_helpers_and_partial_identifier_prefixes_remain_unmapped() {
        let source =
            snapshot("class Box { constructor(public value: number) {} }\nconst valueLong = 1;");
        let generated = "class Box { constructor(value) { this.value = value; } }\nconst value = 1;\n__decorate([dec], Box);";
        let helper = generated.find("this.value").unwrap();
        let value = generated.find("const value").unwrap() + "const ".len();
        let decorator = generated.find("__decorate").unwrap();
        let mapping = map(
            &source,
            generated,
            &[
                (helper, source.text.find("public")),
                (value, source.text.find("valueLong")),
                (decorator, None),
            ],
        );
        for bytes in [
            ByteRange {
                start: helper,
                end: helper + 10,
            },
            ByteRange {
                start: value,
                end: value + 5,
            },
            ByteRange {
                start: decorator,
                end: decorator + 10,
            },
        ] {
            let result =
                map_diagnostic(&source, generated, &mapping, diagnostic(generated, bytes)).unwrap();
            assert_eq!(result["mapping"], "native-ts-transpile-unmapped");
            assert!(result["range"].is_null());
            assert_eq!(result["generated"]["bytes"], json!(bytes));
        }
    }

    #[test]
    fn exact_multiline_anchor_interval_and_range_only_diagnostic() {
        let source = snapshot("type X = number;\nconst a = 1;\nconst b = $state(a);\n");
        let generated = "const a = 1;\nconst b = $state(a);\n";
        let end = generated.len() - 1;
        let mapping = map(
            &source,
            generated,
            &[
                (0, source.text.find("const a")),
                (end, Some(source.text.len() - 1)),
            ],
        );
        let mut input = diagnostic(generated, ByteRange { start: 0, end });
        input.as_object_mut().unwrap().remove("bytes");
        let result = map_diagnostic(&source, generated, &mapping, input).unwrap();
        assert_eq!(result["mapping"], "native-ts-transpile-mapped");
        assert_eq!(result["range"]["start"]["line"], 1);
        assert_eq!(result["range"]["end"]["line"], 2);
    }

    #[test]
    fn missing_anchor_and_explicit_unmapped_segment_are_not_interpolated() {
        let source = snapshot("let a = 1; let b = 2;");
        let generated = &source.text;
        let cut = generated.find("let b").unwrap();
        let mapping = map(&source, generated, &[(0, Some(0)), (cut, None)]);
        let result = map_diagnostic(
            &source,
            generated,
            &mapping,
            diagnostic(
                generated,
                ByteRange {
                    start: cut,
                    end: cut + 3,
                },
            ),
        )
        .unwrap();
        assert_eq!(result["mapping"], "native-ts-transpile-unmapped");
        let result = map_diagnostic(
            &source,
            generated,
            &mapping,
            diagnostic(
                generated,
                ByteRange {
                    start: cut,
                    end: cut,
                },
            ),
        )
        .unwrap();
        assert_eq!(result["mapping"], "native-ts-transpile-unmapped");
    }

    #[test]
    fn malformed_maps_stale_content_and_disagreeing_coordinates_fail() {
        let source = snapshot("let x = '🙂';");
        let generated = &source.text;
        let mapping = map(&source, generated, &[(0, Some(0))]);
        let input = diagnostic(generated, ByteRange { start: 0, end: 3 });
        let mut invalid: Value = serde_json::from_str(&mapping).unwrap();
        invalid["sourcesContent"][0] = json!("stale");
        assert!(map_diagnostic(&source, generated, &invalid.to_string(), input.clone()).is_err());
        for mappings in ["g", "?", "AA", "ACAA", "AAAAA", "CAAA,DAAA"] {
            let mut invalid: Value = serde_json::from_str(&mapping).unwrap();
            invalid["mappings"] = json!(mappings);
            assert!(
                map_diagnostic(&source, generated, &invalid.to_string(), input.clone()).is_err(),
                "{mappings}"
            );
        }
        let mut bad = input;
        bad["range"]["end"]["character"] = json!(2);
        assert!(map_diagnostic(&source, generated, &mapping, bad).is_err());
    }

    #[test]
    fn utf16_anchors_after_astral_text_and_split_surrogates() {
        let source = snapshot("const label: string = '🙂'; let count = $state(0);");
        let generated = "const label = '🙂'; let count = $state(0);";
        let g = generated.find("$state").unwrap();
        let o = source.text.find("$state").unwrap();
        let mapping = map(&source, generated, &[(g, Some(o))]);
        let result = map_diagnostic(
            &source,
            generated,
            &mapping,
            diagnostic(
                generated,
                ByteRange {
                    start: g,
                    end: g + 6,
                },
            ),
        )
        .unwrap();
        assert_eq!(result["bytes"], json!({"start":o,"end":o + 6}));
        assert_eq!(
            result["range"]["start"]["character"],
            source.text[..o].encode_utf16().count()
        );
        let character = generated[..generated.find('🙂').unwrap()]
            .encode_utf16()
            .count()
            + 1;
        let invalid = json!({"version":3,"sources":["state.svelte.ts"],"sourcesContent":[source.text],"names":[],"mappings":format!("{}AAA", encode(i64::try_from(character).unwrap()))});
        assert!(
            map_diagnostic(
                &source,
                generated,
                &invalid.to_string(),
                diagnostic(
                    generated,
                    ByteRange {
                        start: g,
                        end: g + 6
                    }
                )
            )
            .is_err()
        );
    }
}
