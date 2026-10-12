//! Editor operations on the original Svelte template, shared by native and WASM.
use serde_json::{Value, json};
use ts_svelte::{ByteRange, Document, Position, Snapshot, compatibility::Profile};
use ts_svelte_compiler::syntax::SyntaxNode;

fn flatten<'a>(nodes: &'a [SyntaxNode], out: &mut Vec<&'a SyntaxNode>) {
    for node in nodes {
        out.push(node);
        flatten(&node.children, out);
    }
}
fn contains(range: ByteRange, offset: usize) -> bool {
    range.start <= offset && offset <= range.end
}
fn editor_range(document: &Document, span: ByteRange) -> Result<Value, String> {
    document
        .editor_range(span)
        .map(|r| json!(r))
        .map_err(|e| e.to_string())
}
fn symbol(document: &Document, node: &SyntaxNode) -> Result<Value, String> {
    let name = if node.kind == "block" {
        format!("{{#{}}}", node.name)
    } else {
        node.name.clone()
    };
    Ok(
        json!({"name":name,"kind":if node.kind=="block" { 12 } else { 8 },"range":editor_range(document,node.span)?,"selectionRange":editor_range(document,node.name_span.unwrap_or(ByteRange {start:node.span.start,end:node.span.start}))?,"children":node.children.iter().filter(|n|n.kind!="comment").map(|n|symbol(document,n)).collect::<Result<Vec<_>,_>>()?}),
    )
}

/// `None` means this syntax operation does not apply at the requested position.
/// It is distinct from an empty but successfully computed result.
/// # Errors
/// Rejects malformed request coordinates or a source that cannot be parsed.
pub fn query(
    snapshot: Snapshot,
    profile: &Profile,
    method: &str,
    params: &Value,
) -> Result<Option<Value>, String> {
    if !matches!(
        method,
        "textDocument/linkedEditingRange"
            | "textDocument/foldingRange"
            | "textDocument/documentSymbol"
            | "textDocument/selectionRange"
    ) {
        return Ok(None);
    }
    let filename = if snapshot.uri.contains("://") {
        let uri = url::Url::parse(&snapshot.uri).map_err(|e| e.to_string())?;
        if uri.query().is_some() || uri.fragment().is_some() {
            return Err("Source URI must not contain a query or fragment".into());
        }
        percent_encoding::percent_decode_str(uri.path())
            .decode_utf8()
            .map_err(|e| e.to_string())?
            .into_owned()
    } else {
        snapshot.uri.clone()
    };
    if !filename.ends_with(".svelte") {
        return Ok(None);
    }
    let document = Document::new(snapshot);
    let nodes = ts_svelte_compiler::syntax::facts(document.text(), profile)?;
    let mut all = Vec::new();
    flatten(&nodes, &mut all);
    match method {
        "textDocument/linkedEditingRange" => {
            let position: Position =
                serde_json::from_value(params["position"].clone()).map_err(|e| e.to_string())?;
            let offset = document.offset_at(position).map_err(|e| e.to_string())?;
            for node in all {
                if let (Some(open), Some(close)) = (node.name_span, node.closing_name) {
                    if contains(open, offset) || contains(close, offset) {
                        return Ok(Some(
                            json!({"ranges":[editor_range(&document,open)?,editor_range(&document,close)?],"wordPattern":"[a-zA-Z0-9_$:.\\-]+"}),
                        ));
                    }
                }
            }
            Ok(None)
        }
        "textDocument/foldingRange" => {
            let mut folds = Vec::new();
            for node in all {
                let range = document
                    .editor_range(node.span)
                    .map_err(|e| e.to_string())?;
                if range.start.line < range.end.line {
                    folds.push(json!({"startLine":range.start.line,"startCharacter":range.start.character,"endLine":range.end.line,"endCharacter":range.end.character,"kind":if node.kind=="comment" {"comment"} else {"region"}}));
                }
            }
            Ok(Some(json!(folds)))
        }
        "textDocument/documentSymbol" => Ok(Some(json!(
            nodes
                .iter()
                .filter(|n| n.kind != "comment" && n.kind != "script")
                .map(|n| symbol(&document, n))
                .collect::<Result<Vec<_>, _>>()?
        ))),
        "textDocument/selectionRange" => {
            let positions: Vec<Position> =
                serde_json::from_value(params["positions"].clone()).map_err(|e| e.to_string())?;
            let mut result = Vec::new();
            for position in positions {
                let offset = document.offset_at(position).map_err(|e| e.to_string())?;
                let mut spans = vec![ByteRange {
                    start: 0,
                    end: document.text().len(),
                }];
                for node in &all {
                    if contains(node.span, offset) {
                        spans.push(node.span);
                    }
                    for span in [node.name_span, node.closing_name].into_iter().flatten() {
                        if contains(span, offset) {
                            spans.push(span);
                        }
                    }
                }
                spans.sort_by_key(|r| std::cmp::Reverse(r.end - r.start));
                spans.dedup();
                let mut parent = Value::Null;
                for span in spans {
                    let mut item = json!({"range":editor_range(&document,span)?});
                    if !parent.is_null() {
                        item["parent"] = parent;
                    }
                    parent = item;
                }
                result.push(parent);
            }
            Ok(Some(json!(result)))
        }
        _ => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn source(text: &str) -> Snapshot {
        Snapshot {
            uri: "file:///App.svelte".into(),
            version: 1,
            text: text.into(),
        }
    }
    #[test]
    fn non_component_sources_are_not_parsed_as_markup() {
        let profile = ts_svelte::compatibility::resolve(&Default::default()).unwrap();
        let snapshot = Snapshot {
            uri: "file:///logic.ts".into(),
            version: 1,
            text: "const x = '<div>not a template</div>';".into(),
        };
        assert!(
            query(
                snapshot,
                &profile,
                "textDocument/documentSymbol",
                &json!({})
            )
            .unwrap()
            .is_none()
        );
    }
    #[test]
    fn nested_tags_and_utf16_positions() {
        let text = "😀<section><section>hello</section></section>";
        let snapshot = source(text);
        let doc = Document::new(snapshot.clone());
        let position = doc.position_at(text.find("section>").unwrap() + 1).unwrap();
        let profile = ts_svelte::compatibility::resolve(&Default::default()).unwrap();
        let value = query(
            snapshot,
            &profile,
            "textDocument/linkedEditingRange",
            &json!({"position":position}),
        )
        .unwrap()
        .unwrap();
        let ranges: Vec<ts_svelte::Range> =
            serde_json::from_value(value["ranges"].clone()).unwrap();
        assert_eq!(ranges[0].start.character, 3);
        assert_eq!(
            doc.byte_range(ranges[1]).unwrap().start,
            text.rfind("</section>").unwrap() + 2
        );
    }
    #[test]
    fn folds_and_template_selection_are_source_bound() {
        let snapshot = source("{#if true}\n<section>\n<p>Hello</p>\n</section>\n{/if}");
        let profile = ts_svelte::compatibility::resolve(&Default::default()).unwrap();
        let folds = query(
            snapshot.clone(),
            &profile,
            "textDocument/foldingRange",
            &json!({}),
        )
        .unwrap()
        .unwrap();
        assert_eq!(folds.as_array().unwrap().len(), 2);
        let selection = query(
            snapshot,
            &profile,
            "textDocument/selectionRange",
            &json!({"positions":[{"line":2,"character":2}]}),
        )
        .unwrap()
        .unwrap();
        assert_eq!(selection[0]["range"]["start"]["character"], 1);
        assert!(selection[0]["parent"].is_object());
    }
}
