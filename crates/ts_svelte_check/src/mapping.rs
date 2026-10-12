//! Attach source locations without losing the original checker diagnostic tree.
use std::collections::HashMap;

use serde::Deserialize;
use serde_json::{Value, json};
use ts_svelte::{ByteRange, Chunk, Document, Projection, Snapshot};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProjectionInput {
    pub file_name: String,
    #[serde(default = "required_projection")]
    pub required: bool,
    document: Snapshot,
    pub code: String,
    mappings: Vec<MappingInput>,
}
const fn required_projection() -> bool {
    true
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MappingInput {
    source: ByteRange,
    generated: ByteRange,
}

fn chunks(input: &ProjectionInput) -> Result<Vec<Chunk>, String> {
    let mut chunks = Vec::new();
    let mut end = 0;
    for mapping in &input.mappings {
        let gap = input
            .code
            .get(end..mapping.generated.start)
            .ok_or("unordered or invalid generated mapping")?;
        let generated = input
            .code
            .get(mapping.generated.start..mapping.generated.end)
            .ok_or("invalid generated range")?;
        let source = input
            .document
            .text
            .get(mapping.source.start..mapping.source.end)
            .ok_or("invalid source range")?;
        if source != generated {
            return Err("mapping is not an exact copied span".into());
        }
        chunks.push(Chunk::Generated(gap.to_owned()));
        chunks.push(Chunk::Copy(mapping.source));
        end = mapping.generated.end;
    }
    chunks.push(Chunk::Generated(
        input
            .code
            .get(end..)
            .ok_or("invalid mapping end")?
            .to_owned(),
    ));
    Ok(chunks)
}

fn utf16_byte(text: &str, wanted: u64) -> Option<usize> {
    let mut offset = 0;
    for (byte, ch) in text.char_indices() {
        if offset == wanted {
            return Some(byte);
        }
        offset += ch.len_utf16() as u64;
        if offset > wanted {
            return None;
        }
    }
    (offset == wanted).then_some(text.len())
}

fn map_node(node: &mut Value, projections: &HashMap<&str, Projection<'_>>) {
    if let Some(filename) = node["fileName"].as_str() {
        if let Some(projection) = projections.get(filename) {
            let bytes = node["pos"]
                .as_u64()
                .zip(node["end"].as_u64())
                .and_then(|(start, end)| {
                    Some(ByteRange {
                        start: utf16_byte(projection.text(), start)?,
                        end: utf16_byte(projection.text(), end)?,
                    })
                });
            let mapped = bytes
                .and_then(|bytes| projection.to_source(bytes).ok().flatten())
                .and_then(|bytes| {
                    projection
                        .source()
                        .editor_range(bytes)
                        .ok()
                        .map(|range| (bytes, range))
                });
            node["original"] = mapped.map_or_else(
                || json!({"status":"unmapped","uri":projection.source().snapshot().uri,"version":projection.source().snapshot().version}),
                |(bytes, range)| json!({"status":"mapped","uri":projection.source().snapshot().uri,"version":projection.source().snapshot().version,"bytes":bytes,"range":range}),
            );
        } else {
            node["original"] = json!({"status":"unprojected","uri":filename});
        }
    } else {
        node["original"] = json!({"status":"project"});
    }
    for field in ["messageChain", "relatedInformation"] {
        if let Some(children) = node.get_mut(field).and_then(Value::as_array_mut) {
            for child in children {
                map_node(child, projections);
            }
        }
    }
}

/// Validates every copied span before attaching mapped locations recursively.
pub fn map_diagnostics(
    diagnostics: &mut [Value],
    inputs: &[ProjectionInput],
) -> Result<(), String> {
    let documents: Vec<_> = inputs
        .iter()
        .map(|input| Document::new(input.document.clone()))
        .collect();
    let mut projections = HashMap::new();
    for (input, document) in inputs.iter().zip(&documents) {
        let projection =
            Projection::new(document, &chunks(input)?).map_err(|error| error.to_string())?;
        if projection.text() != input.code {
            return Err("reconstructed projection differs".into());
        }
        if projections
            .insert(input.file_name.as_str(), projection)
            .is_some()
        {
            return Err("duplicate projected filename".into());
        }
    }
    for diagnostic in diagnostics {
        map_node(diagnostic, &projections);
    }
    Ok(())
}

/// Keep synthetic-helper diagnostics as evidence, separate from editor output.
/// Both markers and the enclosed diagnostic must be wholly generated: literal
/// marker comments copied from a user's script cannot hide source errors.
pub fn take_generated_diagnostics(
    diagnostics: &mut Vec<Value>,
    inputs: &[ProjectionInput],
) -> Vec<Value> {
    const START: &str = "/*Ωignore_startΩ*/";
    const END: &str = "/*Ωignore_endΩ*/";
    let mut suppressed = Vec::new();
    diagnostics.retain(|diagnostic| {
        let ignored = inputs.iter().find(|input| diagnostic["fileName"] == input.file_name)
            .and_then(|input| {
                let start = utf16_byte(&input.code, diagnostic["pos"].as_u64()?)?;
                let end = utf16_byte(&input.code, diagnostic["end"].as_u64()?)?;
                if end < start { return None; }
                let marker = input.code.get(..start)?.rfind(START)?;
                let close = marker + START.len() + input.code.get(marker + START.len()..)?.find(END)?;
                let region_end = close + END.len();
                if start < marker + START.len() || end > close || input.mappings.iter().any(|mapping| {
                    mapping.generated.start < region_end && mapping.generated.end > marker
                }) { return None; }
                Some(json!({"reason":"svelte-generated-ignore-span","bytes":{"start":marker,"end":region_end}}))
            });
        if let Some(reason) = ignored {
            let mut evidence = diagnostic.clone();
            evidence["suppression"] = reason;
            suppressed.push(evidence);
            false
        } else { true }
    });
    suppressed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ignored_helpers_preserve_raw_evidence_and_adjacent_source_errors() {
        let code = "/*Ωignore_startΩ*/helper/*Ωignore_endΩ*/source";
        let start = code.find("helper").unwrap();
        let end = start + "helper".len();
        let source = code.find("source").unwrap();
        let input: ProjectionInput = serde_json::from_value(json!({
            "fileName":"c.tsx", "document":{"uri":"file:///c.svelte","version":1,"text":"source"},
            "code":code,"mappings":[{"source":{"start":0,"end":6},"generated":{"start":source,"end":source+6}}]
        })).unwrap();
        let offset = |byte| code[..byte].encode_utf16().count();
        let raw = json!({"fileName":"c.tsx","pos":offset(start),"end":offset(end),"code":2558});
        let mut diagnostics = vec![
            raw.clone(),
            json!({"fileName":"c.tsx","pos":offset(source),"end":offset(source+6),"code":2304}),
            json!({"fileName":"c.tsx","pos":offset(start),"end":offset(source+1),"code":2322}),
        ];
        let suppressed = take_generated_diagnostics(&mut diagnostics, &[input]);
        assert_eq!(diagnostics.len(), 2);
        assert_eq!(suppressed.len(), 1);
        assert_eq!(suppressed[0]["code"], raw["code"]);
        assert_eq!(suppressed[0]["pos"], raw["pos"]);
        // User-authored marker comments may not suppress a copied source error.
        let copied: ProjectionInput = serde_json::from_value(json!({
            "fileName":"c.tsx", "document":{"uri":"file:///c.svelte","version":1,"text":code},
            "code":code,"mappings":[{"source":{"start":0,"end":code.len()},"generated":{"start":0,"end":code.len()}}]
        })).unwrap();
        let mut diagnostics = vec![raw];
        assert!(take_generated_diagnostics(&mut diagnostics, &[copied]).is_empty());
        assert_eq!(diagnostics.len(), 1);
    }

    fn input() -> ProjectionInput {
        serde_json::from_value(json!({
            "fileName":"/virtual/Component.svelte.tsx",
            "document":{"uri":"file:///Component.svelte","version":7,"text":"😀\r\nvalue"},
            "code":"helper();😀\r\nvalue",
            "mappings":[{"source":{"start":0,"end":11},"generated":{"start":9,"end":20}}]
        }))
        .unwrap()
    }

    #[test]
    fn recursive_diagnostics_keep_generated_details_and_map_original_ranges() {
        let mut diagnostics = vec![
            json!({"fileName":"/virtual/Component.svelte.tsx","pos":13,"end":18,"code":2304,"text":"Unknown value", "relatedInformation":[
                {"fileName":"/virtual/Component.svelte.tsx","pos":9,"end":11,"text":"emoji"}
            ]}),
        ];
        map_diagnostics(&mut diagnostics, &[input()]).unwrap();
        let diagnostic = &diagnostics[0];
        assert_eq!(diagnostic["pos"], 13);
        assert_eq!(diagnostic["code"], 2304);
        assert_eq!(diagnostic["original"]["bytes"], json!({"start":6,"end":11}));
        assert_eq!(
            diagnostic["original"]["range"],
            json!({"start":{"line":1,"character":0},"end":{"line":1,"character":5}})
        );
        assert_eq!(
            diagnostic["relatedInformation"][0]["original"]["bytes"],
            json!({"start":0,"end":4})
        );
    }

    #[test]
    fn generated_and_invalid_surrogate_locations_remain_unmapped() {
        let mut diagnostics = vec![
            json!({"fileName":"/virtual/Component.svelte.tsx","pos":0,"end":6}),
            json!({"fileName":"/virtual/Component.svelte.tsx","pos":10,"end":11}),
            json!({"fileName":"/virtual/Component.svelte.tsx","pos":9,"end":9}),
        ];
        map_diagnostics(&mut diagnostics, &[input()]).unwrap();
        for diagnostic in diagnostics {
            assert_eq!(diagnostic["original"]["status"], "unmapped");
        }
    }

    #[test]
    fn bad_copied_spans_and_duplicate_files_are_rejected() {
        let mut bad = input();
        bad.code.replace_range(9..13, "oops");
        assert!(map_diagnostics(&mut [], &[bad]).is_err());
        assert!(map_diagnostics(&mut [], &[input(), input()]).is_err());
    }

    #[test]
    fn fileless_and_unprojected_messages_are_preserved() {
        let mut diagnostics =
            vec![json!({"code":123,"messageChain":[{"fileName":"other.ts","pos":0,"end":1}]})];
        map_diagnostics(&mut diagnostics, &[]).unwrap();
        assert_eq!(diagnostics[0]["original"]["status"], "project");
        assert_eq!(
            diagnostics[0]["messageChain"][0]["original"]["status"],
            "unprojected"
        );
    }
}
