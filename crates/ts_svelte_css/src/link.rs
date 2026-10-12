use super::{BundleOutput, CssError, MAX_TOTAL_BYTES};
use cssparser::{Parser, ParserInput, Token};
use parcel_sourcemap::{OriginalLocation, SourceMap};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use ts_svelte::{Document, Position, Snapshot};

/// A host-selected output URL for one original CSS URL dependency. `emittedUrl`
/// is already relative to final CSS or absolute to its public base. It excludes
/// query/fragment, which are preserved from the original URL.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AssetLink {
    pub from: String,
    pub url: String,
    pub emitted_url: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkedCss {
    pub code: String,
    pub map: Option<Value>,
    pub exports: Value,
    pub references: Value,
    pub dependencies: Vec<Value>,
    pub requires_asset_linking: bool,
    pub consumed_links: Vec<AssetLink>,
    pub replacement_count: usize,
}

struct Plan {
    url: String,
    expected: usize,
    seen: usize,
}
struct Replacement {
    start: usize,
    end: usize,
    text: String,
}
fn fail(message: impl Into<String>) -> CssError {
    CssError::new("css_link_error", message)
}
fn field<'a>(value: &'a Value, key: &str) -> Result<&'a str, CssError> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| fail(format!("Dependency has no string {key}")))
}
fn suffix(url: &str) -> &str {
    url.find(['?', '#']).map_or("", |index| &url[index..])
}

/// Link all generated URL/import tokens atomically. The result map addresses
/// final CSS directly against original CSS sources. Source files are not read.
pub fn link(bundle: &BundleOutput, links: &[AssetLink]) -> Result<LinkedCss, CssError> {
    if bundle.code.len() > MAX_TOTAL_BYTES
        || links.len() > 32768
        || bundle.dependencies.len() > 131072
    {
        return Err(fail("CSS/link graph exceeds the supported size"));
    }
    if bundle.mapping_encoding != "utf16-lightningcss-alpha72-patched" {
        return Err(fail(
            "Bundle must originate from the UTF-16-correct patched CSS printer",
        ));
    }
    let mut supplied = BTreeMap::new();
    for item in links {
        super::validate_path(&item.from)?;
        if item.emitted_url.is_empty() || item.emitted_url.contains(['\0', '?', '#']) {
            return Err(fail(
                "emittedUrl must be nonempty and exclude NUL, query and fragment",
            ));
        }
        if supplied
            .insert((item.from.as_str(), item.url.as_str()), item)
            .is_some()
        {
            return Err(fail("Duplicate asset link identity"));
        }
    }
    let mut used = BTreeSet::new();
    let mut plans = BTreeMap::<(String, String), Plan>::new();
    let mut dependencies = Vec::new();
    for dependency in &bundle.dependencies {
        let placeholder = field(dependency, "placeholder")?;
        if placeholder.is_empty() {
            return Err(fail("Empty dependency placeholder"));
        }
        let kind = field(dependency, "type")?;
        if !matches!(kind, "url" | "import") {
            return Err(fail("Unsupported dependency type"));
        }
        let origin = field(
            dependency
                .get("loc")
                .ok_or_else(|| fail("Dependency has no origin"))?,
            "filePath",
        )?;
        let url = field(dependency, "url")?;
        let final_url = if field(dependency, "resolutionStatus")? == "external" {
            field(dependency, "resolvedUrl")?.to_owned()
        } else {
            if kind != "url" {
                return Err(fail("Unresolved stylesheet import survived CSS bundling"));
            }
            let key = (origin, url);
            let item = supplied
                .get(&key)
                .ok_or_else(|| fail(format!("Missing asset link for {url:?} from {origin}")))?;
            used.insert(key);
            format!("{}{}", item.emitted_url, suffix(url))
        };
        if final_url.contains('\0') {
            return Err(fail("Final URL contains NUL"));
        }
        if let Some(plan) = plans.get_mut(&(kind.into(), placeholder.into())) {
            if plan.url != final_url {
                return Err(fail("Conflicting placeholder targets"));
            }
            plan.expected += 1;
        } else {
            plans.insert(
                (kind.into(), placeholder.into()),
                Plan {
                    url: final_url.clone(),
                    expected: 1,
                    seen: 0,
                },
            );
        }
        let mut dependency = dependency.clone();
        dependency["linkedUrl"] = json!(final_url);
        dependency["resolutionStatus"] = json!("linked");
        dependencies.push(dependency);
    }
    if used.len() != supplied.len() {
        return Err(fail(
            "Asset link does not identify a relative URL dependency",
        ));
    }
    let mut replacements = Vec::new();
    let mut parser_input = ParserInput::new(&bundle.code);
    scan(
        &mut Parser::new(&mut parser_input),
        None,
        0,
        &mut plans,
        &mut replacements,
    )
    .map_err(|error| fail(format!("Invalid placeholder CSS: {error:?}")))?;
    if plans.values().any(|plan| plan.seen != plan.expected) {
        return Err(fail(
            "Dependency metadata does not match every generated URL/import occurrence",
        ));
    }
    replacements.sort_by_key(|edit| edit.start);
    if replacements
        .windows(2)
        .any(|pair| pair[0].end > pair[1].start)
    {
        return Err(fail("Overlapping URL replacements"));
    }
    let mut code = String::new();
    let mut cursor = 0;
    for edit in &replacements {
        let size = code
            .len()
            .checked_add(edit.start - cursor)
            .and_then(|size| size.checked_add(edit.text.len()))
            .ok_or_else(|| fail("CSS output size overflow"))?;
        if size > MAX_TOTAL_BYTES {
            return Err(fail("Linked CSS exceeds 32 MiB"));
        }
        code.push_str(&bundle.code[cursor..edit.start]);
        code.push_str(&edit.text);
        cursor = edit.end;
    }
    code.push_str(&bundle.code[cursor..]);
    if code.len() > MAX_TOTAL_BYTES {
        return Err(fail("Linked CSS exceeds 32 MiB"));
    }
    let map = bundle
        .map
        .as_ref()
        .map(|map| remap(map, &bundle.code, &code, &replacements))
        .transpose()?;
    Ok(LinkedCss {
        code,
        map,
        exports: bundle.exports.clone(),
        references: bundle.references.clone(),
        dependencies,
        requires_asset_linking: false,
        consumed_links: used
            .into_iter()
            .map(|key| (*supplied[&key]).clone())
            .collect(),
        replacement_count: replacements.len(),
    })
}

fn replace<'i>(
    parser: &Parser<'i, '_>,
    value: &str,
    kind: &str,
    start: usize,
    end: usize,
    whole_url: bool,
    plans: &mut BTreeMap<(String, String), Plan>,
    edits: &mut Vec<Replacement>,
) -> Result<(), cssparser::ParseError<'i, String>> {
    let plan = plans
        .get_mut(&(kind.into(), value.into()))
        .ok_or_else(|| parser.new_custom_error(format!("Untracked generated {kind} URL token")))?;
    plan.seen += 1;
    let mut text = String::new();
    if whole_url {
        text.push_str("url(");
    }
    cssparser::serialize_string(&plan.url, &mut text)
        .map_err(|_| parser.new_custom_error("Could not escape CSS URL".to_owned()))?;
    if whole_url {
        text.push(')');
    }
    edits.push(Replacement { start, end, text });
    Ok(())
}

fn scan<'i, 't>(
    parser: &mut Parser<'i, 't>,
    context: Option<&str>,
    depth: usize,
    plans: &mut BTreeMap<(String, String), Plan>,
    edits: &mut Vec<Replacement>,
) -> Result<(), cssparser::ParseError<'i, String>> {
    if depth > 256 {
        return Err(parser.new_custom_error("CSS nesting exceeds 256".to_owned()));
    }
    let mut import = false;
    let mut argument_seen = false;
    while !parser.is_exhausted() {
        let start = parser.position().byte_index();
        let token = parser.next_including_whitespace_and_comments()?.clone();
        let end = parser.position().byte_index();
        match token {
            Token::WhiteSpace(_) | Token::Comment(_) => continue,
            Token::AtKeyword(name) if name.eq_ignore_ascii_case("import") => {
                import = true;
                continue;
            }
            Token::QuotedString(value) if import || context.is_some() => {
                if argument_seen {
                    return Err(
                        parser.new_custom_error("Multiple strings in generated URL".to_owned())
                    );
                }
                replace(
                    parser,
                    &value,
                    if import { "import" } else { context.unwrap() },
                    start,
                    end,
                    false,
                    plans,
                    edits,
                )?;
                argument_seen = context.is_some();
            }
            Token::UnquotedUrl(value) => {
                replace(
                    parser,
                    &value,
                    if import { "import" } else { "url" },
                    start,
                    end,
                    true,
                    plans,
                    edits,
                )?;
            }
            Token::Function(name) if name.eq_ignore_ascii_case("url") => {
                parser.parse_nested_block(|nested| {
                    scan(
                        nested,
                        Some(if import { "import" } else { "url" }),
                        depth + 1,
                        plans,
                        edits,
                    )
                })?;
            }
            Token::Function(_)
            | Token::ParenthesisBlock
            | Token::SquareBracketBlock
            | Token::CurlyBracketBlock => {
                if context.is_some() {
                    return Err(
                        parser.new_custom_error("Unexpected nested URL argument".to_owned())
                    );
                }
                parser.parse_nested_block(|nested| scan(nested, None, depth + 1, plans, edits))?;
            }
            _ if context.is_some() => {
                return Err(parser.new_custom_error("Invalid generated URL argument".to_owned()));
            }
            _ => {}
        }
        import = false;
    }
    if context.is_some() && !argument_seen {
        return Err(parser.new_custom_error("Empty generated URL argument".to_owned()));
    }
    Ok(())
}

fn doc(text: &str) -> Document {
    Document::new(Snapshot {
        uri: "css-output".into(),
        version: 0,
        text: text.into(),
    })
}
/// UTF-16 conversions for ascending queries. Each continues from the previous
/// answer on the same line instead of rescanning from the line start (a
/// minified stylesheet is one long line); anything else asks the document.
struct Sequential<'a> {
    doc: &'a Document,
    last: Option<(Position, usize)>,
}
impl<'a> Sequential<'a> {
    fn new(doc: &'a Document) -> Self {
        Self { doc, last: None }
    }
    fn offset_at(&mut self, position: Position) -> Result<usize, ts_svelte::Error> {
        if let Some((last, mut offset)) = self.last
            && last.line == position.line
            && last.character <= position.character
        {
            let mut column = u64::from(last.character);
            let target = u64::from(position.character);
            for ch in self.doc.text()[offset..].chars() {
                if column >= target || matches!(ch, '\r' | '\n') {
                    break;
                }
                column += ch.len_utf16() as u64;
                offset += ch.len_utf8();
            }
            if column == target {
                self.last = Some((position, offset));
                return Ok(offset);
            }
        }
        let offset = self.doc.offset_at(position)?;
        self.last = Some((position, offset));
        Ok(offset)
    }
    fn position_at(&mut self, offset: usize) -> Result<Position, ts_svelte::Error> {
        if let Some((last, from)) = self.last
            && from <= offset
            && let Some(between) = self.doc.text().get(from..offset)
            && !between.contains(['\r', '\n'])
        {
            let position = Position {
                line: last.line,
                character: last.character + between.encode_utf16().count() as u32,
            };
            self.last = Some((position, offset));
            return Ok(position);
        }
        let position = self.doc.position_at(offset)?;
        self.last = Some((position, offset));
        Ok(position)
    }
}
fn key(position: Position) -> (u32, u32) {
    (position.line, position.character)
}

fn remap(map: &Value, before: &str, after: &str, edits: &[Replacement]) -> Result<Value, CssError> {
    if map["version"] != 3 || map.get("sections").is_some() || map.get("sourceRoot").is_some() {
        return Err(fail(
            "Expected flat generated source map without sourceRoot",
        ));
    }
    let parsed = SourceMap::from_json("/", &map.to_string())
        .map_err(|error| fail(format!("Invalid CSS source map: {error}")))?;
    if json!(parsed.get_sources()) != map["sources"] || json!(parsed.get_names()) != map["names"] {
        return Err(fail("Source-map table identities changed while parsing"));
    }
    let before = doc(before);
    let after = doc(after);
    let mut before_sequential = Sequential::new(&before);
    let mut original = BTreeMap::new();
    for mapping in parsed.get_mappings() {
        let position = Position {
            line: mapping.generated_line,
            character: mapping.generated_column,
        };
        if mapping.original.is_some_and(|location| {
            location.source as usize >= parsed.get_sources().len()
                || location
                    .name
                    .is_some_and(|name| name as usize >= parsed.get_names().len())
        }) {
            return Err(fail("Source-map index is out of bounds"));
        }
        before_sequential
            .offset_at(position)
            .map_err(|error| fail(format!("Invalid generated UTF-16 mapping: {error}")))?;
        original.insert(key(position), mapping.original);
    }
    let mut shifts = vec![0i64];
    for edit in edits {
        shifts.push(
            shifts.last().copied().unwrap() + edit.text.len() as i64
                - (edit.end - edit.start) as i64,
        );
    }
    let moved = |offset: usize| -> Result<usize, CssError> {
        let count = edits.partition_point(|edit| edit.end <= offset);
        usize::try_from(offset as i64 + shifts[count])
            .map_err(|_| fail("Generated mapping underflow"))
    };
    let mut generated = BTreeMap::<(u32, u32), Option<OriginalLocation>>::new();
    let mut before_sequential = Sequential::new(&before);
    let mut after_sequential = Sequential::new(&after);
    for (&(line, column), &location) in &original {
        let offset = before_sequential
            .offset_at(Position {
                line,
                character: column,
            })
            .map_err(|error| fail(error.to_string()))?;
        let next = edits.partition_point(|edit| edit.end <= offset);
        if edits.get(next).is_some_and(|edit| edit.start <= offset) {
            continue;
        }
        let position = after_sequential
            .position_at(moved(offset)?)
            .map_err(|error| fail(error.to_string()))?;
        generated.insert(key(position), location);
    }
    for edit in edits {
        let start = after
            .position_at(moved(edit.start)?)
            .map_err(|error| fail(error.to_string()))?;
        generated.insert(key(start), None);
        let old_end = before
            .position_at(edit.end)
            .map_err(|error| fail(error.to_string()))?;
        let end = after
            .position_at(moved(edit.end)?)
            .map_err(|error| fail(error.to_string()))?;
        // Same-line greatest-lower-bound lookup; parcel's lookup falls back to
        // the FIRST mapping when a requested column is after the last mapping.
        let restored = original
            .range((old_end.line, 0)..=key(old_end))
            .next_back()
            .and_then(|(_, original)| *original);
        generated.entry(key(end)).or_insert(restored);
    }
    let mut output = SourceMap::new("/");
    output.add_sources(parsed.get_sources().clone());
    output.add_names(parsed.get_names().clone());
    for ((line, column), location) in generated {
        output.add_mapping(line, column, location);
    }
    let mut mappings = Vec::new();
    output
        .write_vlq(&mut mappings)
        .map_err(|error| fail(error.to_string()))?;
    let mut result = map.clone();
    result["mappings"] =
        json!(String::from_utf8(mappings).map_err(|error| fail(error.to_string()))?);
    Ok(result)
}

#[cfg(test)]
mod sequential_tests {
    use super::*;

    #[test]
    fn sequential_conversions_match_the_document() {
        let text = "a{content:\"é😀\"}\r\nb{x:y}\rc{}\n\n😀z";
        let document = doc(text);
        let mut positions = vec![];
        for line in 0..6u32 {
            for character in 0..12u32 {
                positions.push(Position { line, character });
            }
        }
        // Ascending, then the same queries descending (fallback path).
        let reversed: Vec<_> = positions.iter().rev().copied().collect();
        for order in [&positions, &reversed] {
            let mut sequential = Sequential::new(&document);
            for position in order.iter() {
                assert_eq!(
                    sequential.offset_at(*position).map_err(|e| e.to_string()),
                    document.offset_at(*position).map_err(|e| e.to_string()),
                    "{position:?}"
                );
            }
        }
        for order in [
            (0..=text.len()).collect::<Vec<_>>(),
            (0..=text.len()).rev().collect(),
        ] {
            let mut sequential = Sequential::new(&document);
            for offset in order {
                assert_eq!(
                    sequential.position_at(offset).map_err(|e| e.to_string()),
                    document.position_at(offset).map_err(|e| e.to_string()),
                    "{offset}"
                );
            }
        }
    }
}
