//! Native preprocessing and source-map composition. Single-source maps are
//! composed with same-line lookups; unmapped regions remain unmapped instead of
//! borrowing a preceding line's location. Diagnostic approximation is explicit.
use crate::BundleRequest;
use anyhow::{Context, ensure};
use rolldown_sourcemap::{SourceMap, SourceMapBuilder};
use serde_json::{Value, json};

pub(crate) struct Prepared {
    pub code: String,
    pub map: Option<SourceMap>,
    pub diagnostics: Vec<Value>,
    pub metadata: Option<Value>,
}
#[derive(Debug)]
pub(crate) struct Failure(pub Value);
impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}",
            self.0["error"]["message"]
                .as_str()
                .unwrap_or("Native preprocessing failed")
        )
    }
}
impl std::error::Error for Failure {}
fn failure(message: impl ToString, diagnostics: Vec<Value>) -> Failure {
    Failure(
        json!({"ok":false,"error":{"code":"native_preprocess_failed","message":message.to_string()},"diagnostics":diagnostics}),
    )
}
pub(crate) fn map(value: &Value) -> anyhow::Result<SourceMap> {
    Ok(SourceMap::from_json(serde_json::from_value(
        value.clone(),
    )?)?)
}
pub(crate) fn compose(first: &SourceMap, last: &SourceMap) -> anyhow::Result<SourceMap> {
    ensure!(
        first.get_sources().len() == 1 && last.get_sources().len() <= 1,
        "Preprocessor maps must describe a single source"
    );
    let mut output = SourceMapBuilder::default();
    let source = output.set_source_and_content(
        first.get_source(0).context("Missing source")?,
        first
            .get_source_content(0)
            .context("Missing original source content")?,
    );
    let lookup = first.generate_lookup_table();
    for token in last.get_source_view_tokens() {
        let original = token.get_source_id().and_then(|_| {
            first.lookup_source_view_token(&lookup, token.get_src_line(), token.get_src_col())
        });
        // OXC 9's non-approximate lookup searches only lookup_table[line].
        // Keep this guard explicit so a future API change cannot bridge lines.
        if let Some(original) = original
            .filter(|p| p.get_source_id().is_some() && p.get_dst_line() == token.get_src_line())
        {
            let name = original
                .get_name()
                .or_else(|| token.get_name())
                .map(|name| output.add_name(name));
            output.add_token(
                token.get_dst_line(),
                token.get_dst_col(),
                original.get_src_line(),
                original.get_src_col(),
                Some(source),
                name,
            );
        } else {
            output.add_token(token.get_dst_line(), token.get_dst_col(), 0, 0, None, None);
        }
    }
    Ok(output.into_owned_sourcemap().into())
}
fn position(text: &str, byte: usize) -> Option<(u32, u32)> {
    let prefix = text.get(..byte)?;
    let line = prefix.bytes().filter(|b| *b == b'\n').count() as u32;
    let column = prefix.rsplit('\n').next()?.encode_utf16().count() as u32;
    Some((line, column))
}
fn byte(text: &str, line: u32, column: u32) -> Option<usize> {
    let start = if line == 0 {
        0
    } else {
        text.match_indices('\n').nth(line as usize - 1)?.0 + 1
    };
    let mut col = 0;
    for (index, character) in text[start..].char_indices() {
        if col == column {
            return Some(start + index);
        }
        if character == '\n' {
            return None;
        }
        col += character.len_utf16() as u32;
    }
    (col == column).then_some(text.len())
}
fn shift(base: (u32, u32), point: (u32, u32)) -> (u32, u32) {
    (
        base.0 + point.0,
        point.1 + if point.0 == 0 { base.1 } else { 0 },
    )
}

/// Retains generated location metadata when an exact original location is not
/// available. Never presents a generated Markdown wrapper as an original span.
pub(crate) fn remap_diagnostic(
    value: &mut Value,
    map: &SourceMap,
    generated: &str,
    original: &str,
    id: &str,
) {
    if let Some(labels) = value["labels"].as_array_mut() {
        for label in labels {
            let mut location = json!({"bytes":{"start":label["start"],"end":label["end"]}});
            remap_diagnostic(&mut location, map, generated, original, id);
            label["generatedLocation"] = location["generatedLocation"].clone();
            label["start"] = location["bytes"]["start"].clone();
            label["end"] = location["bytes"]["end"].clone();
            label["mapping"] = location["mapping"].clone();
        }
    }
    let Some(bytes) = value.get("bytes").cloned() else {
        return;
    };
    let Some(start) = bytes["start"]
        .as_u64()
        .and_then(|n| position(generated, n as usize))
    else {
        return;
    };
    let Some(end) = bytes["end"]
        .as_u64()
        .and_then(|n| position(generated, n as usize))
    else {
        return;
    };
    let lookup = map.generate_lookup_table();
    let mapped = |(line, col)| {
        let token = map.lookup_source_view_token(&lookup, line, col)?;
        if token.get_dst_line() != line {
            return None;
        }
        token.get_source_id()?;
        Some((
            byte(original, token.get_src_line(), token.get_src_col())?,
            token.get_dst_col() == col,
        ))
    };
    value["generatedLocation"] = json!({"bytes":bytes,"range":value["range"]});
    value["uri"] = json!(id);
    let mapped_end = mapped(end).or_else(|| {
        // Half-open ranges often end exactly where a copied Markdown island
        // stops. The boundary token is deliberately unmapped; the preceding
        // identical character still establishes its exact original end.
        let end_byte = bytes["end"].as_u64()? as usize;
        let (previous, character) = generated.get(..end_byte)?.char_indices().next_back()?;
        let (original_byte, exact) = mapped(position(generated, previous)?)?;
        (exact
            && original.get(original_byte..original_byte + character.len_utf8())
                == Some(&generated[previous..end_byte]))
        .then_some((original_byte + character.len_utf8(), true))
    });
    if let (Some((start, exact_start)), Some((end, exact_end))) = (mapped(start), mapped_end) {
        if start <= end {
            let document = ts_svelte::Document::new(ts_svelte::Snapshot {
                uri: id.into(),
                version: 0,
                text: original.into(),
            });
            value["bytes"] = json!({"start":start,"end":end});
            value["range"] = json!(
                document
                    .editor_range(ts_svelte::ByteRange { start, end })
                    .ok()
            );
            // Source-map names are metadata, never evidence for interpolation.
            // Exact ranges require byte-identical text and an exact mapped
            // anchor for every character, as provided by copied Markdown runs.
            let generated_start = bytes["start"].as_u64().unwrap() as usize;
            let generated_end = bytes["end"].as_u64().unwrap() as usize;
            let copied_range = generated
                .get(generated_start..generated_end)
                .zip(original.get(start..end))
                .is_some_and(|(generated_range, original_range)| {
                    let mut point =
                        position(generated, generated_start).expect("validated byte boundary");
                    generated_range == original_range
                        && generated_range.char_indices().all(|(offset, character)| {
                            let exact = mapped(point) == Some((start + offset, true));
                            if character == '\n' {
                                point = (point.0 + 1, 0);
                            } else {
                                point.1 += character.len_utf16() as u32;
                            }
                            exact
                        })
                });
            value["mapping"] = json!(if exact_start && exact_end && copied_range {
                "exact"
            } else {
                "approximate"
            });
            return;
        }
    }
    value["bytes"] = Value::Null;
    value["range"] = Value::Null;
    value["mapping"] = json!("unmapped");
}
fn ts_diagnostic(
    diagnostic: &ts_svelte_transpile::Diagnostic,
    id: &str,
    text: &str,
    offset: usize,
) -> Value {
    let mut value = serde_json::to_value(diagnostic).expect("diagnostic serialization");
    value["uri"] = json!(id);
    value["filename"] = json!(id);
    value["source"] = json!("oxc-typescript-preprocess");
    if let Some(labels) = value["labels"].as_array_mut() {
        for label in labels {
            for key in ["start", "end"] {
                if let Some(at) = label[key].as_u64() {
                    label[key] = json!(at + offset as u64);
                }
            }
        }
    }
    if let Some(label) = diagnostic
        .labels
        .iter()
        .find(|l| l.primary)
        .or_else(|| diagnostic.labels.first())
    {
        let bytes = ts_svelte::ByteRange {
            start: offset + label.start as usize,
            end: offset + label.end as usize,
        };
        let document = ts_svelte::Document::new(ts_svelte::Snapshot {
            uri: id.into(),
            version: 0,
            text: text.into(),
        });
        value["bytes"] = json!(bytes);
        value["range"] = json!(document.editor_range(bytes).ok());
    }
    value
}
fn transpile(
    source: &str,
    filename: &str,
    options: &ts_svelte_transpile::TransformOptions,
) -> Result<ts_svelte_transpile::TransformOutput, ts_svelte_transpile::TransformFailure> {
    let mut options = options.clone();
    // Internal maps are mandatory even when final bundle maps are disabled:
    // they also carry compiler diagnostics back to the original source.
    options.source_map = true;
    ts_svelte_transpile::transform(source, filename, &options)
}

struct Edit {
    start: usize,
    end: usize,
    code: String,
    map: SourceMap,
}
fn component_scripts(
    source: &str,
    id: &str,
    options: &ts_svelte_transpile::TransformOptions,
) -> Result<Prepared, Failure> {
    let allocator = oxc_allocator::Allocator::default();
    let parsed = rsvelte_core::parse(
        source,
        &allocator,
        rsvelte_core::ParseOptions {
            defer_script_parse: true,
            skip_expression_loc: true,
            skip_non_css_lang_style: true,
            ..Default::default()
        },
    );
    // Svelte itself produces the authoritative markup/parser failure afterward.
    let Ok(ast) = parsed else {
        return Ok(Prepared {
            code: source.into(),
            map: None,
            diagnostics: vec![],
            metadata: None,
        });
    };
    let mut edits = Vec::new();
    let mut diagnostics = Vec::new();
    for (index, script) in ast.instance.iter().chain(ast.module.iter()).enumerate() {
        if !script.is_typescript {
            continue;
        }
        let start = script.content_offset as usize;
        let end = start + script.raw_content.len();
        let transformed = transpile(script.raw_content, &format!("{id}.{index}.ts"), options)
            .map_err(|error| {
                let diagnostics = error
                    .diagnostics
                    .iter()
                    .map(|d| ts_diagnostic(d, id, source, start))
                    .collect();
                failure(error.to_string(), diagnostics)
            })?;
        diagnostics.extend(
            transformed
                .diagnostics
                .iter()
                .map(|d| ts_diagnostic(d, id, source, start)),
        );
        let map = map(transformed.map.as_ref().expect("requested transpiler map"))
            .map_err(|e| failure(e, vec![]))?;
        edits.push(Edit {
            start,
            end,
            code: transformed.code,
            map,
        });
    }
    if edits.is_empty() {
        return Ok(Prepared {
            code: source.into(),
            map: None,
            diagnostics,
            metadata: None,
        });
    }
    edits.sort_by_key(|edit| edit.start);
    let mut output = String::new();
    let mut builder = SourceMapBuilder::default();
    let source_id = builder.set_source_and_content(id, source);
    let mut cursor = 0;
    for edit in &edits {
        copy(
            source,
            cursor,
            edit.start,
            &mut output,
            &mut builder,
            source_id,
        );
        let destination = position(&output, output.len()).expect("output boundary");
        let original = position(source, edit.start).expect("script boundary");
        builder.add_token(destination.0, destination.1, 0, 0, None, None);
        for token in edit.map.get_source_view_tokens() {
            let dst = shift(destination, (token.get_dst_line(), token.get_dst_col()));
            let src = shift(original, (token.get_src_line(), token.get_src_col()));
            // Script names are optional map metadata, not source identities.
            builder.add_token(
                dst.0,
                dst.1,
                src.0,
                src.1,
                token.get_source_id().map(|_| source_id),
                None,
            );
        }
        output.push_str(&edit.code);
        cursor = edit.end;
    }
    copy(
        source,
        cursor,
        source.len(),
        &mut output,
        &mut builder,
        source_id,
    );
    Ok(Prepared {
        code: output,
        map: Some(builder.into_owned_sourcemap().into()),
        diagnostics,
        metadata: None,
    })
}
fn copy(
    source: &str,
    start: usize,
    end: usize,
    output: &mut String,
    builder: &mut SourceMapBuilder<'_>,
    source_id: u32,
) {
    let mut dst = position(output, output.len()).expect("output boundary");
    let mut src = position(source, start).expect("copy boundary");
    for character in source[start..end].chars() {
        builder.add_token(dst.0, dst.1, src.0, src.1, Some(source_id), None);
        if character == '\n' {
            dst = (dst.0 + 1, 0);
            src = (src.0 + 1, 0);
        } else {
            dst.1 += character.len_utf16() as u32;
            src.1 += character.len_utf16() as u32;
        }
    }
    builder.add_token(dst.0, dst.1, src.0, src.1, Some(source_id), None);
    output.push_str(&source[start..end]);
}

pub(crate) fn prepare(
    source: &str,
    id: &str,
    request: &BundleRequest,
    markdown: bool,
    module_ts: bool,
) -> Result<Prepared, Failure> {
    if module_ts {
        let result = transpile(source, id, &request.typescript).map_err(|e| {
            let diagnostics = e
                .diagnostics
                .iter()
                .map(|d| ts_diagnostic(d, id, source, 0))
                .collect();
            failure(e.to_string(), diagnostics)
        })?;
        return Ok(Prepared {
            map: Some(
                map(result.map.as_ref().expect("requested map")).map_err(|e| failure(e, vec![]))?,
            ),
            diagnostics: result
                .diagnostics
                .iter()
                .map(|d| ts_diagnostic(d, id, source, 0))
                .collect(),
            code: result.code,
            metadata: None,
        });
    }
    let markdown = if markdown {
        Some(
            ts_svelte_preprocess::preprocess(
                source,
                id,
                request.mdsvex.as_ref().expect("configured markdown"),
            )
            .map_err(|e| {
                let mut error = failure(e.to_string(), vec![]);
                error.0["preprocessorError"] =
                    serde_json::to_value(e).expect("preprocessor error serialization");
                error
            })?
            .ok_or_else(|| failure("Markdown extension was not accepted", vec![]))?,
        )
    } else {
        None
    };
    let markup = markdown.as_ref().map_or(source, |p| p.code.as_str());
    let original_map = markdown
        .as_ref()
        .map(|p| map(&p.map))
        .transpose()
        .map_err(|e| failure(e, vec![]))?;
    let mut result = component_scripts(markup, id, &request.typescript).map_err(|mut error| {
        if let Some(map) = &original_map {
            if let Some(items) = error.0["diagnostics"].as_array_mut() {
                for diagnostic in items {
                    remap_diagnostic(diagnostic, map, markup, source, id);
                }
            }
        }
        error
    })?;
    if let Some(first) = original_map {
        for diagnostic in &mut result.diagnostics {
            remap_diagnostic(diagnostic, &first, markup, source, id);
        }
        result.map = Some(if let Some(last) = result.map {
            compose(&first, &last).map_err(|e| failure(e, vec![]))?
        } else {
            first
        });
    }
    result.metadata = markdown.and_then(|p| p.metadata);
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn composition_preserves_generated_islands_as_unmapped() {
        let mut first = SourceMapBuilder::default();
        let source = first.set_source_and_content("original.md", "hello");
        first.add_token(0, 0, 0, 0, None, None);
        first.add_token(0, 3, 0, 0, Some(source), None);
        first.add_token(0, 8, 0, 0, None, None);
        let first: SourceMap = first.into_owned_sourcemap().into();
        let mut last = SourceMapBuilder::default();
        let source = last.set_source_and_content("generated.svelte", "<p>hello</p>");
        last.add_token(0, 0, 0, 0, Some(source), None);
        last.add_token(0, 5, 0, 3, Some(source), None);
        last.add_token(0, 10, 0, 8, Some(source), None);
        last.add_token(1, 0, 1, 0, Some(source), None);
        let result = compose(&first, &last.into_owned_sourcemap().into()).unwrap();
        let tokens = result.get_source_view_tokens().collect::<Vec<_>>();
        assert_eq!(tokens.len(), 4);
        assert_eq!(tokens[0].get_source_id(), None);
        assert_eq!(tokens[1].get_source_id(), Some(0));
        assert_eq!(tokens[2].get_source_id(), None);
        assert_eq!(tokens[3].get_source_id(), None);
        assert_eq!(result.get_source_content(0), Some("hello"));
    }
    #[test]
    fn copied_markdown_range_end_maps_without_mapping_generated_wrappers() {
        let original = "<img>";
        let generated = "<p><img></p>";
        let mut builder = SourceMapBuilder::default();
        let source = builder.set_source_and_content("x.md", original);
        builder.add_token(0, 0, 0, 0, None, None);
        for i in 0..5 {
            builder.add_token(0, 3 + i, 0, i, Some(source), None);
        }
        builder.add_token(0, 8, 0, 0, None, None);
        let map: SourceMap = builder.into_owned_sourcemap().into();
        let mut diagnostic = json!({"bytes":{"start":3,"end":8}});
        remap_diagnostic(&mut diagnostic, &map, generated, original, "x.md");
        assert_eq!(diagnostic["bytes"], json!({"start":0,"end":5}));
        assert_eq!(diagnostic["mapping"], "exact");
        let mut wrapper = json!({"bytes":{"start":0,"end":3}});
        remap_diagnostic(&mut wrapper, &map, generated, original, "x.md");
        assert_eq!(wrapper["mapping"], "unmapped");
        assert!(wrapper["bytes"].is_null());
    }
    #[test]
    fn lookups_never_cross_empty_lines_or_extrapolate_before_first_column() {
        let mut first = SourceMapBuilder::default();
        let source = first.set_source_and_content("original.ts", "name\n\nnext");
        let name = first.add_name("name");
        first.add_token(0, 0, 0, 0, Some(source), Some(name));
        first.add_token(2, 4, 2, 0, Some(source), None);
        let first: SourceMap = first.into_owned_sourcemap().into();
        let mut last = SourceMapBuilder::default();
        let source = last.set_source_and_content("generated.js", "renamed\nunknown\n    next");
        last.add_token(0, 0, 1, 3, Some(source), None);
        last.add_token(0, 5, 2, 2, Some(source), None);
        last.add_token(0, 10, 2, 4, Some(source), None);
        let composed = compose(&first, &last.into_owned_sourcemap().into()).unwrap();
        let tokens = composed.get_source_view_tokens().collect::<Vec<_>>();
        assert_eq!(tokens[0].get_source_id(), None);
        assert_eq!(tokens[1].get_source_id(), None);
        assert_eq!(tokens[2].get_source_id(), Some(0));
        let mut diagnostic = json!({"bytes":{"start":9,"end":10}});
        remap_diagnostic(
            &mut diagnostic,
            &first,
            "renamed\nunknown\n    next",
            "name\n\nnext",
            "original.ts",
        );
        assert_eq!(diagnostic["mapping"], "unmapped");
        let mut rewritten = json!({"bytes":{"start":1,"end":3}});
        remap_diagnostic(
            &mut rewritten,
            &first,
            "renamed\nunknown\n    next",
            "name\n\nnext",
            "original.ts",
        );
        assert_eq!(rewritten["mapping"], "approximate");
        assert_eq!(rewritten["bytes"], json!({"start":0,"end":0}));
    }
}
