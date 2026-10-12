use serde_json::Value;
use ts_svelte::{Document, Range, Snapshot};

/// Validate sequential LSP edits before the core can clamp an invalid coordinate.
pub(crate) fn apply(source: &str, changes: &Value) -> Result<String, String> {
    let changes = changes
        .as_array()
        .filter(|v| !v.is_empty())
        .ok_or("contentChanges must be nonempty")?;
    let mut text = source.to_owned();
    for change in changes {
        let replacement = change["text"]
            .as_str()
            .ok_or("Change text must be a string")?;
        if let Some(range) = change.get("range") {
            let range: Range = serde_json::from_value(range.clone()).map_err(|e| e.to_string())?;
            let document = Document::new(Snapshot {
                uri: "memory:///edit".into(),
                version: 0,
                text,
            });
            let bytes = document.byte_range(range).map_err(|e| e.to_string())?;
            if let Some(length) = change.get("rangeLength") {
                let actual = document.text()[bytes.start..bytes.end]
                    .encode_utf16()
                    .count() as u64;
                if length.as_u64() != Some(actual) {
                    return Err("rangeLength does not match the current source".into());
                }
            }
            text = document.text().to_owned();
            text.replace_range(bytes.start..bytes.end, replacement);
        } else {
            if change.get("rangeLength").is_some() {
                return Err("rangeLength requires range".into());
            }
            text = replacement.to_owned();
        }
        if text.len() > crate::memory::MAX_FILE {
            return Err("Document exceeds 8 MiB".into());
        }
    }
    Ok(text)
}
