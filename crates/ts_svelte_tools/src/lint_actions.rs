//! LSP actions from server-owned, current native lint findings. Client-provided
//! diagnostics and their `data` fields never supply edits or authorization.
use serde_json::{Value, json};
use std::collections::BTreeSet;
use ts_svelte::{ByteRange, Document, Range, Snapshot};

const FIX_ALL: &str = "source.fixAll.svelte";

#[derive(Clone, Debug, PartialEq, Eq)]
struct Edit {
    bytes: ByteRange,
    range: Range,
    text: String,
}

#[derive(Clone)]
struct Fix {
    title: String,
    edits: Vec<Edit>,
    diagnostic: Value,
}

/// Convert current native lint fixes into LSP actions. The caller must obtain
/// `lint_result` from the server's lint engine for this exact snapshot, never
/// from request parameters or client diagnostic data.
///
/// Successful lint envelopes and result bodies carry document identity. Native
/// diagnostic arrays are also accepted when each finding carries URI/version.
/// All edits require their own URI/version and both byte and UTF-16 ranges.
/// Invalid or stale server fixes reject the entire operation. Overlapping
/// automatic fix groups are omitted atomically from fix-all; their individual
/// quickfixes remain available. Suggestions never participate in fix-all.
pub fn code_actions(
    snapshot: &Snapshot,
    lint_result: &Value,
    params: &Value,
) -> Result<Vec<Value>, String> {
    let document = Document::new(snapshot.clone());
    let request = params
        .as_object()
        .ok_or("Code action parameters must be an object")?;
    let requested_document = request.get("textDocument").ok_or("Missing textDocument")?;
    if requested_document["uri"].as_str() != Some(snapshot.uri.as_str()) {
        return Err("Code action URI does not match the current document".into());
    }
    if let Some(version) = requested_document.get("version")
        && version.as_u64() != Some(u64::from(snapshot.version))
    {
        return Err("Code action request is for a stale document version".into());
    }
    let range: Range = serde_json::from_value(
        request
            .get("range")
            .cloned()
            .ok_or("Missing code action range")?,
    )
    .map_err(|error| format!("Invalid code action range: {error}"))?;
    let requested_range = document
        .byte_range(range)
        .map_err(|error| error.to_string())?;
    let context = request
        .get("context")
        .and_then(Value::as_object)
        .ok_or("Missing code action context")?;
    let only = context
        .get("only")
        .map(|value| {
            value
                .as_array()
                .ok_or("context.only must be an array")?
                .iter()
                .map(|value| value.as_str().ok_or("context.only entries must be strings"))
                .collect::<Result<Vec<_>, _>>()
        })
        .transpose()?;
    let wanted = |kind: &str| {
        only.as_ref().is_none_or(|only| {
            only.iter().any(|requested| {
                requested.is_empty()
                    || kind == *requested
                    || kind
                        .strip_prefix(*requested)
                        .is_some_and(|suffix| suffix.starts_with('.'))
            })
        })
    };

    let (diagnostics, document_bound) = findings(snapshot, lint_result)?;
    let mut automatic = Vec::new();
    let mut quickfixes = Vec::new();
    for diagnostic in diagnostics {
        if !document_bound {
            identity(snapshot, diagnostic, "Lint diagnostic")?;
        } else {
            if let Some(uri) = diagnostic.get("uri")
                && uri.as_str() != Some(snapshot.uri.as_str())
            {
                return Err("Lint diagnostic URI differs from its document result".into());
            }
            if let Some(version) = diagnostic.get("version")
                && version.as_u64() != Some(u64::from(snapshot.version))
            {
                return Err("Lint diagnostic version differs from its document result".into());
            }
        }
        // Script/compiler diagnostics without fixes need no action-specific
        // coordinate requirements; they can legitimately be unmapped.
        let has_fix = diagnostic.get("fix").is_some_and(|fix| !fix.is_null());
        let suggestions = diagnostic
            .get("suggestions")
            .filter(|value| !value.is_null())
            .map(|value| {
                value
                    .as_array()
                    .ok_or("Diagnostic suggestions must be an array")
            })
            .transpose()?;
        if !has_fix && suggestions.is_none_or(Vec::is_empty) {
            continue;
        }
        let (bytes, diagnostic_range) = coordinates(&document, diagnostic, "Lint diagnostic")?;
        let lsp_diagnostic = lsp_diagnostic(diagnostic, diagnostic_range)?;
        let intersects = intersects(requested_range, bytes);
        let code = diagnostic["code"].as_str().unwrap_or("lint");
        if has_fix {
            let fallback = format!("Fix {code}");
            let fix = read_fix(&document, &diagnostic["fix"], &fallback, &lsp_diagnostic)?;
            if !fix.edits.is_empty() {
                if intersects && wanted("quickfix") {
                    quickfixes.push(action(snapshot, &fix, "quickfix", true));
                }
                automatic.push(fix);
            }
        }
        for suggestion in suggestions.into_iter().flatten() {
            let description = suggestion["description"]
                .as_str()
                .filter(|text| !text.is_empty())
                .ok_or("Suggestion requires a description")?;
            let mut fix = read_fix(&document, &suggestion["fix"], description, &lsp_diagnostic)?;
            // The suggestion describes the choice; an internal edit-group
            // message may describe only one operation within that choice.
            fix.title = description.into();
            if intersects && wanted("quickfix") && !fix.edits.is_empty() {
                quickfixes.push(action(snapshot, &fix, "quickfix", false));
            }
        }
    }
    let mut actions = Vec::new();
    let mut seen = BTreeSet::new();
    for action in quickfixes {
        if seen.insert(action.to_string()) {
            actions.push(action);
        }
    }
    if wanted(FIX_ALL) && !automatic.is_empty() {
        let mut blocked = vec![false; automatic.len()];
        for left in 0..automatic.len() {
            for right in left + 1..automatic.len() {
                if automatic[left].edits.iter().any(|a| {
                    automatic[right]
                        .edits
                        .iter()
                        .any(|b| a != b && conflict(a.bytes, b.bytes))
                }) {
                    blocked[left] = true;
                    blocked[right] = true;
                }
            }
        }
        let conflict_count = blocked.iter().filter(|blocked| **blocked).count();
        let mut edits = Vec::new();
        let mut covered = Vec::new();
        for (fix, blocked) in automatic.iter().zip(&blocked) {
            if *blocked {
                continue;
            }
            for edit in &fix.edits {
                if !edits.contains(edit) {
                    edits.push(edit.clone());
                }
            }
            if !covered.contains(&fix.diagnostic) {
                covered.push(fix.diagnostic.clone());
            }
        }
        edits.sort_by_key(|edit| (edit.bytes.start, edit.bytes.end));
        if edits.is_empty() {
            actions.push(json!({"title":"Fix all auto-fixable Svelte lint issues","kind":FIX_ALL,"disabled":{"reason":"Automatic fixes overlap. Apply an individual quickfix and request actions again."}}));
        } else {
            let title = if conflict_count == 0 {
                "Fix all auto-fixable Svelte lint issues".to_owned()
            } else {
                format!(
                    "Fix non-conflicting Svelte lint issues ({conflict_count} conflicting fixes skipped)"
                )
            };
            actions.push(json!({"title":title,"kind":FIX_ALL,"diagnostics":covered,"edit":workspace_edit(snapshot,&edits)}));
        }
    }
    Ok(actions)
}

fn findings<'a>(snapshot: &Snapshot, result: &'a Value) -> Result<(&'a Vec<Value>, bool), String> {
    if let Some(diagnostics) = result.as_array() {
        return Ok((diagnostics, false));
    }
    let body = if result.get("ok").is_some() {
        if result["ok"] != true {
            return Err("Code actions require a complete successful lint result".into());
        }
        result
            .get("result")
            .ok_or("Lint response is missing its result")?
    } else {
        result
    };
    identity(snapshot, body, "Lint result")?;
    Ok((
        body["diagnostics"]
            .as_array()
            .ok_or("Lint result has no diagnostics array")?,
        true,
    ))
}

fn identity(snapshot: &Snapshot, value: &Value, label: &str) -> Result<(), String> {
    if value["uri"].as_str() != Some(snapshot.uri.as_str())
        || value["version"].as_u64() != Some(u64::from(snapshot.version))
    {
        return Err(format!(
            "{label} does not belong to the current document URI and version"
        ));
    }
    Ok(())
}

fn coordinates(
    document: &Document,
    value: &Value,
    label: &str,
) -> Result<(ByteRange, Range), String> {
    let bytes: ByteRange = serde_json::from_value(value["bytes"].clone())
        .map_err(|error| format!("{label} has invalid bytes: {error}"))?;
    let range: Range = serde_json::from_value(value["range"].clone())
        .map_err(|error| format!("{label} has invalid editor range: {error}"))?;
    if document
        .editor_range(bytes)
        .map_err(|error| format!("{label}: {error}"))?
        != range
        || document
            .byte_range(range)
            .map_err(|error| format!("{label}: {error}"))?
            != bytes
    {
        return Err(format!("{label} byte and UTF-16 ranges disagree"));
    }
    Ok((bytes, range))
}

fn read_fix(
    document: &Document,
    value: &Value,
    fallback: &str,
    diagnostic: &Value,
) -> Result<Fix, String> {
    identity(document.snapshot(), value, "Lint fix")?;
    let raw_edits = value["edits"]
        .as_array()
        .ok_or("Lint fix requires an edits array")?;
    let mut edits = Vec::new();
    for edit in raw_edits {
        let (bytes, range) = coordinates(document, edit, "Lint fix edit")?;
        let text = edit["newText"]
            .as_str()
            .ok_or("Lint edit requires newText")?;
        let original = &document.text()[bytes.start..bytes.end];
        if let Some(expected) = edit.get("expectedText")
            && expected.as_str() != Some(original)
        {
            return Err("Lint edit expectedText differs from the current source".into());
        }
        // Check conflicts before dropping no-ops: invalid producer output must
        // not become valid by selectively removing one member of an edit group.
        edits.push(Edit {
            bytes,
            range,
            text: text.into(),
        });
    }
    edits.sort_by_key(|edit| (edit.bytes.start, edit.bytes.end));
    for pair in edits.windows(2) {
        if conflict(pair[0].bytes, pair[1].bytes) {
            return Err(
                "Lint fix contains overlapping edits; its complete edit group was rejected".into(),
            );
        }
    }
    edits.retain(|edit| document.text()[edit.bytes.start..edit.bytes.end] != edit.text);
    let title = value["message"]
        .as_str()
        .filter(|message| !message.is_empty())
        .unwrap_or(fallback)
        .into();
    Ok(Fix {
        title,
        edits,
        diagnostic: diagnostic.clone(),
    })
}

fn conflict(left: ByteRange, right: ByteRange) -> bool {
    left.start == right.start || (left.start < right.end && right.start < left.end)
}

fn intersects(requested: ByteRange, diagnostic: ByteRange) -> bool {
    if requested.start == requested.end {
        diagnostic.start <= requested.start && requested.start <= diagnostic.end
    } else if diagnostic.start == diagnostic.end {
        requested.start <= diagnostic.start && diagnostic.start < requested.end
    } else {
        requested.start < diagnostic.end && diagnostic.start < requested.end
    }
}

fn lsp_diagnostic(diagnostic: &Value, range: Range) -> Result<Value, String> {
    let message = diagnostic["message"]
        .as_str()
        .ok_or("Lint diagnostic requires a message")?;
    let severity = match diagnostic["severity"].as_str() {
        Some("error") => 1,
        Some("warning") | Some("warn") => 2,
        Some("information") | Some("info") => 3,
        Some("hint") => 4,
        _ => return Err("Lint diagnostic has an unsupported severity".into()),
    };
    let mut output =
        json!({"range":range,"message":message,"severity":severity,"source":"svelte-native-lint"});
    if diagnostic["code"].is_string() || diagnostic["code"].is_i64() {
        output["code"] = diagnostic["code"].clone();
    }
    Ok(output)
}

fn workspace_edit(snapshot: &Snapshot, edits: &[Edit]) -> Value {
    json!({"documentChanges":[{"textDocument":{"uri":snapshot.uri,"version":snapshot.version},"edits":edits.iter().map(|edit|json!({"range":edit.range,"newText":edit.text})).collect::<Vec<_>>()}]})
}

fn action(snapshot: &Snapshot, fix: &Fix, kind: &str, preferred: bool) -> Value {
    json!({"title":fix.title,"kind":kind,"diagnostics":[fix.diagnostic],"isPreferred":preferred,"edit":workspace_edit(snapshot,&fix.edits)})
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot() -> Snapshot {
        Snapshot {
            uri: "file:///Component.svelte".into(),
            version: 7,
            text: "🤖abcdef\r\nghij".into(),
        }
    }
    fn range(snapshot: &Snapshot, start: usize, end: usize) -> Value {
        json!(
            Document::new(snapshot.clone())
                .editor_range(ByteRange { start, end })
                .unwrap()
        )
    }
    fn edit(snapshot: &Snapshot, start: usize, end: usize, text: &str) -> Value {
        json!({"bytes":{"start":start,"end":end},"range":range(snapshot,start,end),"newText":text})
    }
    fn fix(snapshot: &Snapshot, edits: Vec<Value>) -> Value {
        json!({"uri":snapshot.uri,"version":snapshot.version,"message":"Apply native fix","edits":edits})
    }
    fn diagnostic(snapshot: &Snapshot, start: usize, end: usize, edits: Vec<Value>) -> Value {
        json!({"uri":snapshot.uri,"version":snapshot.version,"code":"svelte/test-rule","message":"Native diagnostic","severity":"warning","bytes":{"start":start,"end":end},"range":range(snapshot,start,end),"fix":fix(snapshot,edits)})
    }
    fn result(snapshot: &Snapshot, diagnostics: Vec<Value>) -> Value {
        json!({"ok":true,"result":{"uri":snapshot.uri,"version":snapshot.version,"diagnostics":diagnostics}})
    }
    fn params(snapshot: &Snapshot, start: usize, end: usize, only: Value) -> Value {
        let mut value = json!({"textDocument":{"uri":snapshot.uri},"range":range(snapshot,start,end),"context":{"diagnostics":[]}});
        if !only.is_null() {
            value["context"]["only"] = only;
        }
        value
    }

    #[test]
    fn quickfix_and_fix_all_use_current_version_and_utf16_coordinates() {
        let snapshot = snapshot();
        let lint = result(
            &snapshot,
            vec![diagnostic(
                &snapshot,
                4,
                5,
                vec![edit(&snapshot, 4, 5, "A")],
            )],
        );
        let actions =
            code_actions(&snapshot, &lint, &params(&snapshot, 4, 5, Value::Null)).unwrap();
        assert_eq!(actions.len(), 2);
        let change = &actions[0]["edit"]["documentChanges"][0];
        assert_eq!(change["textDocument"]["version"], 7);
        assert_eq!(change["edits"][0]["range"]["start"]["character"], 2);
        assert_eq!(actions[0]["diagnostics"][0]["severity"], 2);
        assert_eq!(actions[1]["kind"], FIX_ALL);
    }

    #[test]
    fn client_diagnostic_edits_are_ignored_and_only_hierarchy_is_respected() {
        let snapshot = snapshot();
        let lint = result(
            &snapshot,
            vec![diagnostic(
                &snapshot,
                4,
                5,
                vec![edit(&snapshot, 4, 5, "A")],
            )],
        );
        let mut request = params(&snapshot, 4, 5, json!(["quickfix"]));
        request["context"]["diagnostics"] =
            json!([{"data":{"fix":{"uri":"file:///other","edits":[{"newText":"attacker"}]}}}]);
        let actions = code_actions(&snapshot, &lint, &request).unwrap();
        assert_eq!(actions.len(), 1);
        assert!(!actions[0].to_string().contains("attacker"));
        assert_eq!(
            code_actions(
                &snapshot,
                &lint,
                &params(&snapshot, 4, 5, json!(["source"]))
            )
            .unwrap()[0]["kind"],
            FIX_ALL
        );
        assert!(
            code_actions(&snapshot, &lint, &params(&snapshot, 4, 5, json!([])))
                .unwrap()
                .is_empty()
        );
        assert!(
            code_actions(
                &snapshot,
                &lint,
                &params(&snapshot, 4, 5, json!(["refactor"]))
            )
            .unwrap()
            .is_empty()
        );
    }

    #[test]
    fn stale_versions_and_invalid_edit_ranges_fail_atomically() {
        let snapshot = snapshot();
        let lint = result(
            &snapshot,
            vec![diagnostic(
                &snapshot,
                4,
                5,
                vec![edit(&snapshot, 4, 5, "A")],
            )],
        );
        for mutate in 0..5 {
            let mut lint = lint.clone();
            match mutate {
                0 => lint["result"]["version"] = json!(6),
                1 => lint["result"]["diagnostics"][0]["fix"]["version"] = json!(6),
                2 => {
                    lint["result"]["diagnostics"][0]["fix"]["edits"][0]["bytes"]["start"] = json!(1)
                }
                3 => {
                    lint["result"]["diagnostics"][0]["fix"]["edits"][0]["range"]["end"]["character"] =
                        json!(99)
                }
                _ => {
                    lint["result"]["diagnostics"][0]["fix"]["edits"][0]["expectedText"] =
                        json!("stale")
                }
            }
            assert!(code_actions(&snapshot, &lint, &params(&snapshot, 4, 5, Value::Null)).is_err());
        }
    }

    #[test]
    fn suggestions_are_individual_choices_and_never_fix_all() {
        let snapshot = snapshot();
        let mut finding = diagnostic(&snapshot, 4, 5, Vec::new());
        finding["fix"] = Value::Null;
        finding["suggestions"] = json!([{"description":"Choose another spelling","fix":fix(&snapshot,vec![edit(&snapshot,4,5,"X")])}]);
        let actions = code_actions(
            &snapshot,
            &result(&snapshot, vec![finding]),
            &params(&snapshot, 4, 4, Value::Null),
        )
        .unwrap();
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0]["title"], "Choose another spelling");
        assert_eq!(actions[0]["isPreferred"], false);
    }

    #[test]
    fn fix_all_skips_entire_conflicting_groups_and_deduplicates_shared_edits() {
        let snapshot = snapshot();
        let a = diagnostic(
            &snapshot,
            4,
            5,
            vec![edit(&snapshot, 4, 5, "A"), edit(&snapshot, 8, 9, "E")],
        );
        let b = diagnostic(&snapshot, 4, 6, vec![edit(&snapshot, 4, 6, "AB")]);
        let c = diagnostic(&snapshot, 12, 13, vec![edit(&snapshot, 12, 13, "G")]);
        let actions = code_actions(
            &snapshot,
            &result(&snapshot, vec![a, b, c.clone(), c]),
            &params(&snapshot, 0, 0, json!(["source.fixAll"])),
        )
        .unwrap();
        let edits = actions[0]["edit"]["documentChanges"][0]["edits"]
            .as_array()
            .unwrap();
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0]["newText"], "G");
        assert!(
            actions[0]["title"]
                .as_str()
                .unwrap()
                .contains("2 conflicting fixes skipped")
        );
    }

    #[test]
    fn overlapping_edits_inside_a_single_fix_reject_all_actions() {
        let snapshot = snapshot();
        let lint = result(
            &snapshot,
            vec![diagnostic(
                &snapshot,
                4,
                5,
                vec![edit(&snapshot, 4, 6, "AB"), edit(&snapshot, 5, 7, "BC")],
            )],
        );
        assert!(code_actions(&snapshot, &lint, &params(&snapshot, 4, 5, Value::Null)).is_err());
    }

    #[test]
    fn quickfix_range_is_local_but_fix_all_is_document_wide() {
        let snapshot = snapshot();
        let lint = result(
            &snapshot,
            vec![diagnostic(
                &snapshot,
                12,
                13,
                vec![edit(&snapshot, 12, 13, "G")],
            )],
        );
        assert!(
            code_actions(
                &snapshot,
                &lint,
                &params(&snapshot, 4, 5, json!(["quickfix"]))
            )
            .unwrap()
            .is_empty()
        );
        let actions = code_actions(
            &snapshot,
            &lint,
            &params(&snapshot, 4, 5, json!(["source.fixAll.svelte"])),
        )
        .unwrap();
        assert_eq!(actions.len(), 1);
        assert!(actions[0].get("edit").is_some());
    }

    #[test]
    fn unfixable_script_diagnostics_inherit_result_version_without_client_data() {
        let snapshot = snapshot();
        let lint = result(
            &snapshot,
            vec![
                json!({"uri":snapshot.uri,"code":"no-debugger","message":"Unexpected debugger statement.","severity":"warning","range":null,"bytes":null}),
                diagnostic(&snapshot, 4, 5, vec![edit(&snapshot, 4, 5, "A")]),
            ],
        );
        let actions = code_actions(
            &snapshot,
            &lint,
            &params(&snapshot, 4, 5, json!(["quickfix"])),
        )
        .unwrap();
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0]["diagnostics"][0]["code"], "svelte/test-rule");
    }

    #[test]
    fn conflicting_insertions_disable_fix_all_without_exposing_partial_edits() {
        let snapshot = snapshot();
        let lint = result(
            &snapshot,
            vec![
                diagnostic(&snapshot, 4, 4, vec![edit(&snapshot, 4, 4, "first")]),
                diagnostic(&snapshot, 4, 4, vec![edit(&snapshot, 4, 4, "second")]),
            ],
        );
        let actions =
            code_actions(&snapshot, &lint, &params(&snapshot, 4, 4, Value::Null)).unwrap();
        assert_eq!(actions.len(), 3);
        assert!(actions[2].get("disabled").is_some());
        assert!(actions[2].get("edit").is_none());
    }
}
