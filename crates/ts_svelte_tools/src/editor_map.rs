//! Exact, snapshot-bound mapping of editor requests and LSP result payloads.
//! Workspace edits are mapped transactionally: no partial rename is returned.
use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::{Map, Value, json};
use ts_svelte::{ByteRange, Chunk, Document, Mapping, Position, Projection, Range, Snapshot};

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct MappingError {
    pub code: &'static str,
    pub message: String,
}

impl MappingError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for MappingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for MappingError {}

impl From<ts_svelte::Error> for MappingError {
    fn from(error: ts_svelte::Error) -> Self {
        Self::new(error.code, error.message)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct GeneratedPosition {
    pub uri: String,
    pub position: Position,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct MappedRange {
    pub uri: String,
    pub range: Range,
}

pub type GeneratedRange = MappedRange;

struct Entry {
    original: Document,
    generated: Document,
    mappings: Vec<Mapping>,
}

impl Entry {
    fn to_original(&self, range: ByteRange) -> Option<ByteRange> {
        if range.start == range.end {
            let offset = range.start;
            let index = self
                .mappings
                .partition_point(|m| m.generated.start < offset);
            let left = index
                .checked_sub(1)
                .and_then(|i| self.mappings.get(i))
                .filter(|m| m.generated.end >= offset);
            let right = self
                .mappings
                .get(index)
                .filter(|m| m.generated.start == offset);
            let map = |m: &Mapping| m.source.start + offset - m.generated.start;
            let source = match (left, right) {
                (Some(left), Some(right)) if map(left) == map(right) => map(left),
                (Some(left), None)
                    if offset < left.generated.end || offset == self.generated.text().len() =>
                {
                    map(left)
                }
                (None, Some(right)) if offset == 0 => map(right),
                _ => return None,
            };
            return Some(ByteRange {
                start: source,
                end: source,
            });
        }
        let index = self
            .mappings
            .partition_point(|m| m.generated.start <= range.start);
        let mapping = index.checked_sub(1).and_then(|i| self.mappings.get(i))?;
        if range.end > mapping.generated.end {
            return None;
        }
        let start = mapping.source.start + range.start - mapping.generated.start;
        Some(ByteRange {
            start,
            end: start + range.end - range.start,
        })
    }
}

/// A registry owns both snapshots, so mappings cannot accidentally follow edits.
#[derive(Default)]
pub struct EditorMap {
    entries: BTreeMap<String, Entry>,
}

impl EditorMap {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register copied spans from a compiler projection. Invalid, overlapping,
    /// out-of-order or non-identical copied spans are rejected at registration.
    /// # Errors
    /// Returns a structured error for invalid projections or duplicate URIs.
    pub fn register(
        &mut self,
        original: Snapshot,
        generated_uri: String,
        code: String,
        mappings: Vec<Mapping>,
    ) -> Result<(), MappingError> {
        if self.entries.contains_key(&generated_uri) {
            return Err(MappingError::new(
                "duplicate_projection",
                format!("Already registered {generated_uri}"),
            ));
        }
        for entry in self.entries.values() {
            if entry.original.snapshot().uri == original.uri
                && entry.original.snapshot() != &original
            {
                return Err(MappingError::new(
                    "snapshot_mismatch",
                    "Original URI has multiple different snapshots",
                ));
            }
        }
        let original = Document::new(original);
        let generated = Document::new(Snapshot {
            uri: generated_uri.clone(),
            version: original.snapshot().version,
            text: code,
        });
        let mut chunks = Vec::new();
        let mut end = 0;
        for mapping in mappings {
            original.validate_bytes(mapping.source)?;
            generated.validate_bytes(mapping.generated)?;
            if mapping.generated.start < end
                || mapping.generated.end - mapping.generated.start
                    != mapping.source.end - mapping.source.start
            {
                return Err(MappingError::new(
                    "invalid_projection",
                    "Copied spans overlap, are out of order or have different lengths",
                ));
            }
            if generated.text()[mapping.generated.start..mapping.generated.end]
                != original.text()[mapping.source.start..mapping.source.end]
            {
                return Err(MappingError::new(
                    "invalid_projection",
                    "Copied span differs from the original snapshot",
                ));
            }
            chunks.push(Chunk::Generated(
                generated.text()[end..mapping.generated.start].into(),
            ));
            chunks.push(Chunk::Copy(mapping.source));
            end = mapping.generated.end;
        }
        chunks.push(Chunk::Generated(generated.text()[end..].into()));
        let projection = Projection::new(&original, &chunks)?;
        let mappings = projection.mappings().to_vec();
        self.entries.insert(
            generated_uri,
            Entry {
                original,
                generated,
                mappings,
            },
        );
        Ok(())
    }

    /// Register an ordinary file, including an overlay URI for an original file.
    /// # Errors
    /// Returns the same registration errors as `register`.
    pub fn register_identity(
        &mut self,
        original: Snapshot,
        generated_uri: String,
    ) -> Result<(), MappingError> {
        let length = original.text.len();
        let code = original.text.clone();
        self.register(
            original,
            generated_uri,
            code,
            vec![Mapping {
                source: ByteRange {
                    start: 0,
                    end: length,
                },
                generated: ByteRange {
                    start: 0,
                    end: length,
                },
            }],
        )
    }

    /// Atomically replace one projection after fully validating its new
    /// snapshots. A failed replacement leaves the previous registration intact.
    /// Original versions may reset on close; the host owns lifecycle ordering.
    /// # Errors
    /// Returns registration errors, including mismatched sibling projections.
    pub fn replace(
        &mut self,
        original: Snapshot,
        generated_uri: String,
        code: String,
        mappings: Vec<Mapping>,
    ) -> Result<(), MappingError> {
        let mut candidate = Self::new();
        candidate.register(original, generated_uri.clone(), code, mappings)?;
        let entry = candidate
            .entries
            .remove(&generated_uri)
            .expect("candidate was registered");
        for (uri, sibling) in &self.entries {
            if uri != &generated_uri
                && sibling.original.snapshot().uri == entry.original.snapshot().uri
                && sibling.original.snapshot() != entry.original.snapshot()
            {
                return Err(MappingError::new(
                    "snapshot_mismatch",
                    "Original URI has multiple different snapshots",
                ));
            }
        }
        self.entries.insert(generated_uri, entry);
        Ok(())
    }

    /// # Errors
    /// Returns the same errors as `replace`.
    pub fn replace_identity(
        &mut self,
        original: Snapshot,
        generated_uri: String,
    ) -> Result<(), MappingError> {
        let length = original.text.len();
        let code = original.text.clone();
        self.replace(
            original,
            generated_uri,
            code,
            vec![Mapping {
                source: ByteRange {
                    start: 0,
                    end: length,
                },
                generated: ByteRange {
                    start: 0,
                    end: length,
                },
            }],
        )
    }

    /// Set the TypeScript service's independent virtual document version. This
    /// is the version checked on incoming versioned WorkspaceEdits; outgoing
    /// edits still use the original editor snapshot's version.
    /// # Errors
    /// Rejects unknown generated URIs.
    pub fn set_generated_version(&mut self, uri: &str, version: u32) -> Result<(), MappingError> {
        let entry = self
            .entries
            .get_mut(uri)
            .ok_or_else(|| MappingError::new("unknown_document", uri))?;
        let mut snapshot = entry.generated.snapshot().clone();
        snapshot.version = version;
        entry.generated = Document::new(snapshot);
        Ok(())
    }

    /// Find every projected cursor occurrence in every projection of a document.
    /// # Errors
    /// Rejects an unknown source or invalid UTF-16 position.
    pub fn generated_positions(
        &self,
        original_uri: &str,
        position: Position,
    ) -> Result<Vec<GeneratedPosition>, MappingError> {
        let mut positions = Vec::new();
        let mut found = false;
        for (uri, entry) in &self.entries {
            if entry.original.snapshot().uri != original_uri {
                continue;
            }
            found = true;
            let offset = entry.original.offset_at(position)?;
            for mapping in &entry.mappings {
                if offset >= mapping.source.start && offset <= mapping.source.end {
                    let position = entry
                        .generated
                        .position_at(mapping.generated.start + offset - mapping.source.start)?;
                    let mapped = GeneratedPosition {
                        uri: uri.clone(),
                        position,
                    };
                    if positions.last() != Some(&mapped) {
                        positions.push(mapped);
                    }
                }
            }
            if entry.original.text().is_empty() && entry.generated.text().is_empty() {
                positions.push(GeneratedPosition {
                    uri: uri.clone(),
                    position,
                });
            }
        }
        if !found {
            return Err(MappingError::new("unknown_document", original_uri));
        }
        Ok(positions)
    }

    /// Map a range without changing diagnostic or generated-span policy. `None`
    /// means it cannot be represented as one exact original range.
    /// # Errors
    /// Rejects unregistered documents and invalid UTF-16 ranges.
    pub fn map_range(
        &self,
        generated_uri: &str,
        range: Range,
    ) -> Result<Option<MappedRange>, MappingError> {
        let entry = self.entry(generated_uri)?;
        let bytes = entry.generated.byte_range(range)?;
        let mapped = if bytes.start == 0
            && bytes.end == 0
            && entry.generated.text().is_empty()
            && entry.original.text().is_empty()
        {
            Some(bytes)
        } else {
            entry.to_original(bytes)
        };
        mapped
            .map(|mapped| {
                Ok(MappedRange {
                    uri: entry.original.snapshot().uri.clone(),
                    range: entry.original.editor_range(mapped)?,
                })
            })
            .transpose()
    }

    /// Map a request range only to occurrences wholly contained in a single
    /// copied span, avoiding accidental pairing of different repeated copies.
    /// # Errors
    /// Rejects unknown source documents and invalid original UTF-16 ranges.
    pub fn generated_ranges(
        &self,
        original_uri: &str,
        range: Range,
    ) -> Result<Vec<GeneratedRange>, MappingError> {
        let mut output = Vec::new();
        let mut found = false;
        for (uri, entry) in &self.entries {
            if entry.original.snapshot().uri != original_uri {
                continue;
            }
            found = true;
            let bytes = entry.original.byte_range(range)?;
            for mapping in &entry.mappings {
                if bytes.start >= mapping.source.start && bytes.end <= mapping.source.end {
                    let start = mapping.generated.start + bytes.start - mapping.source.start;
                    let range = entry.generated.editor_range(ByteRange {
                        start,
                        end: start + bytes.end - bytes.start,
                    })?;
                    let mapped = GeneratedRange {
                        uri: uri.clone(),
                        range,
                    };
                    if output.last() != Some(&mapped) {
                        output.push(mapped);
                    }
                }
            }
            if entry.original.text().is_empty() && entry.generated.text().is_empty() {
                output.push(GeneratedRange {
                    uri: uri.clone(),
                    range,
                });
            }
        }
        if !found {
            return Err(MappingError::new("unknown_document", original_uri));
        }
        Ok(output)
    }

    fn entry(&self, uri: &str) -> Result<&Entry, MappingError> {
        self.entries.get(uri).ok_or_else(|| {
            MappingError::new(
                "unknown_document",
                format!("No snapshot registered for {uri}"),
            )
        })
    }

    fn required_range(&self, uri: &str, value: &Value) -> Result<MappedRange, MappingError> {
        self.map_range(uri, parse_range(value)?)?.ok_or_else(|| {
            MappingError::new(
                "unrepresentable_range",
                format!("Range in {uri} includes generated text or ambiguous boundaries"),
            )
        })
    }

    /// # Errors
    /// Rejects malformed hover payloads or ranges crossing generated helpers.
    pub fn map_hover(&self, generated_uri: &str, mut value: Value) -> Result<Value, MappingError> {
        if value.is_null() {
            return Ok(value);
        }
        let object = object_mut(&mut value)?;
        if let Some(range) = object.get("range") {
            let mapped = self.required_range(generated_uri, range)?;
            object.insert("range".into(), json!(mapped.range));
        }
        Ok(value)
    }

    /// Map Location, LocationLink, or arrays returned by definition/reference
    /// requests. Identical original locations from repeated copies are deduped.
    /// External locations not in this registry are preserved.
    /// # Errors
    /// Rejects malformed locations and unrepresentable projected ranges.
    pub fn map_locations(
        &self,
        origin_generated_uri: &str,
        value: Value,
    ) -> Result<Value, MappingError> {
        if value.is_null() {
            return Ok(value);
        }
        if let Value::Array(values) = value {
            let mut mapped = Vec::new();
            for value in values {
                let location = self.map_location(origin_generated_uri, value)?;
                if !mapped.contains(&location) {
                    mapped.push(location);
                }
            }
            Ok(Value::Array(mapped))
        } else {
            self.map_location(origin_generated_uri, value)
        }
    }

    fn map_location(&self, origin: &str, mut value: Value) -> Result<Value, MappingError> {
        let object = object_mut(&mut value)?;
        if object.contains_key("targetUri") {
            let uri = string_field(object, "targetUri")?.to_owned();
            for name in ["targetRange", "targetSelectionRange"] {
                let range = field(object, name)?;
                if self.entries.contains_key(&uri) {
                    let mapped = self.required_range(&uri, range)?;
                    object.insert(name.into(), json!(mapped.range));
                    object.insert("targetUri".into(), json!(mapped.uri));
                } else {
                    parse_range(range)?;
                }
            }
            if let Some(range) = object.get("originSelectionRange") {
                let mapped = self.required_range(origin, range)?;
                object.insert("originSelectionRange".into(), json!(mapped.range));
            }
        } else {
            let uri = string_field(object, "uri")?.to_owned();
            let range = field(object, "range")?;
            if self.entries.contains_key(&uri) {
                let mapped = self.required_range(&uri, range)?;
                object.insert("uri".into(), json!(mapped.uri));
                object.insert("range".into(), json!(mapped.range));
            } else {
                parse_range(range)?;
            }
        }
        Ok(value)
    }

    /// # Errors
    /// Rejects malformed prepare-rename results or unmappable ranges.
    pub fn map_prepare_rename(&self, uri: &str, mut value: Value) -> Result<Value, MappingError> {
        if value.is_null() {
            return Ok(value);
        }
        if value.get("defaultBehavior") == Some(&Value::Bool(true)) {
            return Ok(value);
        }
        if let Some(range) = value.get("range") {
            let mapped = self.required_range(uri, range)?;
            object_mut(&mut value)?.insert("range".into(), json!(mapped.range));
        } else {
            value = json!(self.required_range(uri, &value)?.range);
        }
        Ok(value)
    }

    fn map_text_edit(&self, uri: &str, mut value: Value) -> Result<Value, MappingError> {
        let object = object_mut(&mut value)?;
        string_field(object, "newText")?;
        if object.contains_key("range") {
            let mapped = self.required_range(uri, field(object, "range")?)?;
            object.insert("range".into(), json!(mapped.range));
        } else {
            for name in ["insert", "replace"] {
                let mapped = self.required_range(uri, field(object, name)?)?;
                object.insert(name.into(), json!(mapped.range));
            }
            let insert = parse_range(field(object, "insert")?)?;
            let replace = parse_range(field(object, "replace")?)?;
            if insert.start != replace.start || point(insert.end) > point(replace.end) {
                return Err(MappingError::new(
                    "invalid_edit",
                    "Insert range is not a prefix of replace range",
                ));
            }
        }
        Ok(value)
    }

    /// Map completion lists/items, including defaults and all additional edits.
    /// # Errors
    /// Any unrepresentable or overlapping edit rejects the complete response.
    pub fn map_completions(&self, uri: &str, mut value: Value) -> Result<Value, MappingError> {
        if value.is_null() {
            return Ok(value);
        }
        if value.is_array() {
            let items = value.as_array_mut().expect("array checked");
            for item in items {
                self.map_completion_item(uri, item, None)?;
            }
        } else if value.get("items").is_some() {
            let object = object_mut(&mut value)?;
            let default_edit = object
                .get("itemDefaults")
                .and_then(|d| d.get("editRange"))
                .cloned();
            let items = object
                .get_mut("items")
                .and_then(Value::as_array_mut)
                .ok_or_else(|| {
                    MappingError::new("invalid_response", "Completion items must be an array")
                })?;
            for item in items {
                self.map_completion_item(uri, item, default_edit.as_ref())?;
            }
            if let Some(defaults) = object.get_mut("itemDefaults") {
                let defaults = object_mut(defaults)?;
                if let Some(range) = default_edit {
                    let edit = if range.get("start").is_some() {
                        json!({"range":range,"newText":""})
                    } else {
                        json!({"insert":range.get("insert"),"replace":range.get("replace"),"newText":""})
                    };
                    let mapped = self.map_text_edit(uri, edit)?;
                    let mapped = mapped.get("range").cloned().unwrap_or_else(
                        || json!({"insert":mapped["insert"],"replace":mapped["replace"]}),
                    );
                    defaults.insert("editRange".into(), mapped);
                }
            }
        } else {
            self.map_completion_item(uri, &mut value, None)?;
        }
        Ok(value)
    }

    fn map_completion_item(
        &self,
        uri: &str,
        item: &mut Value,
        default_edit: Option<&Value>,
    ) -> Result<(), MappingError> {
        let object = object_mut(item)?;
        string_field(object, "label")?;
        let mut edits = Vec::new();
        if let Some(edit) = object.get("textEdit") {
            let mapped = self.map_text_edit(uri, edit.clone())?;
            edits.push(as_replace_edit(&mapped));
            object.insert("textEdit".into(), mapped);
        } else if let Some(range) = default_edit {
            let text = object
                .get("textEditText")
                .or_else(|| object.get("insertText"))
                .unwrap_or_else(|| object.get("label").expect("label validated"));
            let edit = if range.get("start").is_some() {
                json!({"range":range,"newText":text})
            } else {
                json!({"insert":range.get("insert"),"replace":range.get("replace"),"newText":text})
            };
            edits.push(as_replace_edit(&self.map_text_edit(uri, edit)?));
        }
        if let Some(additional) = object.get_mut("additionalTextEdits") {
            let array = additional.as_array_mut().ok_or_else(|| {
                MappingError::new("invalid_response", "additionalTextEdits must be an array")
            })?;
            let mut mapped = Vec::new();
            for edit in array.iter() {
                let edit = self.map_text_edit(uri, edit.clone())?;
                if edit.get("range").is_none() {
                    return Err(MappingError::new(
                        "invalid_edit",
                        "Additional edits cannot be insert/replace edits",
                    ));
                }
                // A repeated projection occurrence may resolve to the primary
                // edit itself. Do not ask the client to apply it twice.
                if edits.contains(&edit) {
                    continue;
                }
                if !mapped.contains(&edit) {
                    mapped.push(edit.clone());
                }
                edits.push(edit);
            }
            *array = mapped;
        }
        validated_edits(edits)?;
        Ok(())
    }

    /// Map document symbols recursively. Helper-only symbols are omitted and
    /// their representable children are promoted. A symbol's exact selection
    /// range is used when its larger generated declaration range has no source.
    /// # Errors
    /// Rejects malformed symbols, invalid positions and excessively deep trees.
    pub fn map_document_symbols(&self, uri: &str, value: Value) -> Result<Value, MappingError> {
        if value.is_null() {
            return Ok(value);
        }
        let symbols = value.as_array().ok_or_else(|| {
            MappingError::new("invalid_response", "Document symbols must be an array")
        })?;
        Ok(Value::Array(self.map_symbol_children(uri, symbols, 0)?))
    }

    fn map_symbol_children(
        &self,
        uri: &str,
        symbols: &[Value],
        depth: usize,
    ) -> Result<Vec<Value>, MappingError> {
        if depth > 128 {
            return Err(MappingError::new(
                "size_limit",
                "Symbol tree exceeds 128 levels",
            ));
        }
        let mut output = Vec::new();
        for symbol in symbols {
            let mut symbol = symbol.clone();
            let object = object_mut(&mut symbol)?;
            if let Some(location) = object.get("location") {
                let location_uri =
                    location.get("uri").and_then(Value::as_str).ok_or_else(|| {
                        MappingError::new("invalid_response", "Symbol location requires a URI")
                    })?;
                if self.entries.contains_key(location_uri)
                    && self
                        .map_range(
                            location_uri,
                            parse_range(location.get("range").ok_or_else(|| {
                                MappingError::new(
                                    "invalid_response",
                                    "Symbol location requires a range",
                                )
                            })?)?,
                        )?
                        .is_none()
                {
                    continue;
                }
                object.insert("location".into(), self.map_location(uri, location.clone())?);
            } else {
                let children = if let Some(children) = object.get("children") {
                    self.map_symbol_children(
                        uri,
                        children.as_array().ok_or_else(|| {
                            MappingError::new(
                                "invalid_response",
                                "Symbol children must be an array",
                            )
                        })?,
                        depth + 1,
                    )?
                } else {
                    Vec::new()
                };
                let selection =
                    self.map_range(uri, parse_range(field(object, "selectionRange")?)?)?;
                let Some(selection) = selection else {
                    output.extend(children);
                    continue;
                };
                let full = self.map_range(uri, parse_range(field(object, "range")?)?)?;
                let full_range = full.map_or(selection.range, |range| range.range);
                if point(full_range.start) > point(selection.range.start)
                    || point(full_range.end) < point(selection.range.end)
                {
                    return Err(MappingError::new(
                        "invalid_response",
                        "Symbol selection is outside its declaration range",
                    ));
                }
                object.insert("range".into(), json!(full_range));
                object.insert("selectionRange".into(), json!(selection.range));
                if object.contains_key("children") {
                    object.insert("children".into(), Value::Array(children));
                }
            }
            if !output.contains(&symbol) {
                output.push(symbol);
            }
        }
        Ok(output)
    }

    /// # Errors
    /// Rejects malformed highlights. Generated-only highlights are omitted.
    pub fn map_highlights(&self, uri: &str, value: Value) -> Result<Value, MappingError> {
        if value.is_null() {
            return Ok(value);
        }
        let values = value
            .as_array()
            .ok_or_else(|| MappingError::new("invalid_response", "Highlights must be an array"))?;
        let mut output = Vec::new();
        for value in values {
            let mut value = value.clone();
            let object = object_mut(&mut value)?;
            if let Some(mapped) = self.map_range(uri, parse_range(field(object, "range")?)?)? {
                object.insert("range".into(), json!(mapped.range));
                if !output.contains(&value) {
                    output.push(value);
                }
            }
        }
        Ok(Value::Array(output))
    }

    /// Preserve one selection leaf per requested cursor. Generated helper
    /// parents are skipped; representable parents retain their nesting.
    /// # Errors
    /// Rejects unmappable leaves, malformed chains and non-containing parents.
    pub fn map_selection_ranges(&self, uri: &str, value: Value) -> Result<Value, MappingError> {
        if value.is_null() {
            return Ok(value);
        }
        let values = value.as_array().ok_or_else(|| {
            MappingError::new("invalid_response", "Selection ranges must be an array")
        })?;
        let mut output = Vec::new();
        for leaf in values {
            let mut current = Some(leaf);
            let mut chain: Vec<Range> = Vec::new();
            let mut depth = 0;
            while let Some(value) = current {
                if depth > 128 {
                    return Err(MappingError::new(
                        "size_limit",
                        "Selection chain exceeds 128 levels",
                    ));
                }
                let object = value.as_object().ok_or_else(|| {
                    MappingError::new("invalid_response", "Selection range must be an object")
                })?;
                match self.map_range(uri, parse_range(field(object, "range")?)?)? {
                    Some(mapped) => {
                        if let Some(child) = chain.last() {
                            if point(mapped.range.start) > point(child.start)
                                || point(mapped.range.end) < point(child.end)
                            {
                                return Err(MappingError::new(
                                    "invalid_response",
                                    "Selection parent does not contain its child",
                                ));
                            }
                        }
                        if chain.last() != Some(&mapped.range) {
                            chain.push(mapped.range);
                        }
                    }
                    None if depth == 0 => {
                        return Err(MappingError::new(
                            "unrepresentable_range",
                            "Selection leaf is generated text",
                        ));
                    }
                    None => {}
                }
                current = object.get("parent").filter(|parent| !parent.is_null());
                depth += 1;
            }
            let mut result = None;
            for range in chain.into_iter().rev() {
                let mut value = json!({"range":range});
                if let Some(parent) = result {
                    value["parent"] = parent;
                }
                result = Some(value);
            }
            output.push(result.expect("leaf mapping is required"));
        }
        Ok(Value::Array(output))
    }

    /// # Errors
    /// Rejects malformed folding ranges. Folds crossing helpers are omitted.
    pub fn map_folding_ranges(&self, uri: &str, value: Value) -> Result<Value, MappingError> {
        if value.is_null() {
            return Ok(value);
        }
        let entry = self.entry(uri)?;
        let values = value.as_array().ok_or_else(|| {
            MappingError::new("invalid_response", "Folding ranges must be an array")
        })?;
        let mut output = Vec::new();
        for value in values {
            let mut value = value.clone();
            let object = object_mut(&mut value)?;
            let start_line = integer_field(object, "startLine")?;
            let end_line = integer_field(object, "endLine")?;
            let start_character = optional_integer_field(object, "startCharacter")?.unwrap_or(0);
            let end_character = match optional_integer_field(object, "endCharacter")? {
                Some(character) => character,
                None => line_end_character(&entry.generated, end_line)?,
            };
            let range = Range {
                start: Position {
                    line: start_line,
                    character: start_character,
                },
                end: Position {
                    line: end_line,
                    character: end_character,
                },
            };
            if let Some(mapped) = self.map_range(uri, range)? {
                object.insert("startLine".into(), json!(mapped.range.start.line));
                object.insert("startCharacter".into(), json!(mapped.range.start.character));
                object.insert("endLine".into(), json!(mapped.range.end.line));
                object.insert("endCharacter".into(), json!(mapped.range.end.character));
                if !output.contains(&value) {
                    output.push(value);
                }
            }
        }
        Ok(Value::Array(output))
    }

    /// Signature parameter label offsets index the signature's own label, not
    /// the source document. They and opaque documentation remain untouched.
    /// # Errors
    /// Rejects a non-object, non-null result or a missing signatures array.
    pub fn map_signature_help(&self, value: Value) -> Result<Value, MappingError> {
        if !value.is_null() && value.get("signatures").and_then(Value::as_array).is_none() {
            return Err(MappingError::new(
                "invalid_response",
                "Signature help requires a signatures array",
            ));
        }
        Ok(value)
    }

    /// # Errors
    /// Rejects any unmappable or conflicting edit; returns no partial list.
    pub fn map_text_edits(&self, uri: &str, value: Value) -> Result<Value, MappingError> {
        if value.is_null() {
            return Ok(value);
        }
        let values = value
            .as_array()
            .ok_or_else(|| MappingError::new("invalid_response", "Text edits must be an array"))?;
        let mut mapped = Vec::new();
        for edit in values {
            let edit = self.map_text_edit(uri, edit.clone())?;
            if edit.get("range").is_none() {
                return Err(MappingError::new(
                    "invalid_edit",
                    "Text edits cannot use completion insert/replace ranges",
                ));
            }
            mapped.push(edit);
        }
        Ok(Value::Array(validated_edits(mapped)?))
    }

    /// Map code actions (or a resolved action), preserving opaque `data` and
    /// commands. Command execution remains a separate host responsibility.
    /// # Errors
    /// Rejects the entire result if any action edit is unrepresentable.
    pub fn map_code_actions(&self, uri: &str, value: Value) -> Result<Value, MappingError> {
        if value.is_null() {
            return Ok(value);
        }
        if let Value::Array(values) = value {
            values
                .into_iter()
                .map(|value| self.map_code_action(uri, value))
                .collect::<Result<Vec<_>, _>>()
                .map(Value::Array)
        } else {
            self.map_code_action(uri, value)
        }
    }

    fn map_code_action(&self, uri: &str, mut value: Value) -> Result<Value, MappingError> {
        let object = object_mut(&mut value)?;
        string_field(object, "title")?;
        if let Some(edit) = object.get("edit") {
            object.insert("edit".into(), self.map_workspace_edit(edit.clone())?);
        }
        if let Some(diagnostics) = object.get("diagnostics") {
            let diagnostics = diagnostics.as_array().ok_or_else(|| {
                MappingError::new("invalid_response", "Action diagnostics must be an array")
            })?;
            let mut mapped = Vec::new();
            for diagnostic in diagnostics {
                let mut diagnostic = diagnostic.clone();
                let diagnostic_object = object_mut(&mut diagnostic)?;
                let Some(range) =
                    self.map_range(uri, parse_range(field(diagnostic_object, "range")?)?)?
                else {
                    continue;
                };
                diagnostic_object.insert("range".into(), json!(range.range));
                if let Some(related) = diagnostic_object.get_mut("relatedInformation") {
                    let related = related.as_array_mut().ok_or_else(|| {
                        MappingError::new("invalid_response", "relatedInformation must be an array")
                    })?;
                    for item in related {
                        let item = object_mut(item)?;
                        let location = self.map_location(uri, field(item, "location")?.clone())?;
                        item.insert("location".into(), location);
                    }
                }
                mapped.push(diagnostic);
            }
            object.insert("diagnostics".into(), Value::Array(mapped));
        }
        Ok(value)
    }

    /// Decode full generated semantic tokens, map exact source spans, then sort
    /// and re-encode their UTF-16 deltas. Helper tokens are omitted. Delta edits
    /// must first be expanded by the host against the generated token cache.
    /// # Errors
    /// Rejects malformed streams, conflicting tokens and unexpanded deltas.
    pub fn map_semantic_tokens(&self, uri: &str, mut value: Value) -> Result<Value, MappingError> {
        if value.is_null() {
            return Ok(value);
        }
        let object = object_mut(&mut value)?;
        if object.contains_key("edits") {
            return Err(MappingError::new(
                "unsupported_token_delta",
                "Expand generated token deltas before source mapping",
            ));
        }
        let data = field(object, "data")?.as_array().ok_or_else(|| {
            MappingError::new("invalid_response", "Semantic token data must be an array")
        })?;
        if data.len() % 5 != 0 {
            return Err(MappingError::new(
                "invalid_response",
                "Semantic token stream length must be a multiple of five",
            ));
        }
        let mut line = 0_u32;
        let mut character = 0_u32;
        let mut mapped = Vec::new();
        for token in data.chunks_exact(5) {
            let values = token.iter().map(integer).collect::<Result<Vec<_>, _>>()?;
            line = line.checked_add(values[0]).ok_or_else(|| {
                MappingError::new("invalid_range", "Semantic token line overflow")
            })?;
            character = if values[0] == 0 {
                character.checked_add(values[1]).ok_or_else(|| {
                    MappingError::new("invalid_range", "Semantic token character overflow")
                })?
            } else {
                values[1]
            };
            if values[2] == 0 {
                return Err(MappingError::new(
                    "invalid_range",
                    "Semantic token length must be positive",
                ));
            }
            let end = character
                .checked_add(values[2])
                .ok_or_else(|| MappingError::new("invalid_range", "Semantic token end overflow"))?;
            let range = Range {
                start: Position { line, character },
                end: Position {
                    line,
                    character: end,
                },
            };
            if let Some(mapped_range) = self.map_range(uri, range)? {
                let range = mapped_range.range;
                if range.start.line != range.end.line {
                    return Err(MappingError::new(
                        "unrepresentable_range",
                        "Semantic token maps across original lines",
                    ));
                }
                mapped.push([
                    range.start.line,
                    range.start.character,
                    range.end.character - range.start.character,
                    values[3],
                    values[4],
                ]);
            }
        }
        mapped.sort_unstable();
        mapped.dedup();
        let mut output = Vec::new();
        let mut previous = [0_u32; 5];
        for (index, token) in mapped.into_iter().enumerate() {
            if index > 0 && token[0] == previous[0] && token[1] < previous[1] + previous[2] {
                return Err(MappingError::new(
                    "conflicting_tokens",
                    "Projected semantic tokens overlap in the original document",
                ));
            }
            output.extend([
                token[0] - previous[0],
                if token[0] == previous[0] {
                    token[1] - previous[1]
                } else {
                    token[1]
                },
                token[2],
                token[3],
                token[4],
            ]);
            previous = token;
        }
        object.insert("data".into(), json!(output));
        Ok(value)
    }

    /// Map inlay positions, optional edits and label-part locations. Helper-only
    /// hint positions are omitted; every edit of a retained hint must map.
    /// # Errors
    /// Rejects malformed hints and any retained hint's unrepresentable edits.
    pub fn map_inlay_hints(&self, uri: &str, value: Value) -> Result<Value, MappingError> {
        if value.is_null() {
            return Ok(value);
        }
        let values = value
            .as_array()
            .ok_or_else(|| MappingError::new("invalid_response", "Inlay hints must be an array"))?;
        let mut output = Vec::new();
        for value in values {
            let mut value = value.clone();
            let object = object_mut(&mut value)?;
            let position: Position = serde_json::from_value(field(object, "position")?.clone())
                .map_err(|error| MappingError::new("invalid_position", error.to_string()))?;
            let Some(mapped) = self.map_range(
                uri,
                Range {
                    start: position,
                    end: position,
                },
            )?
            else {
                continue;
            };
            object.insert("position".into(), json!(mapped.range.start));
            if let Some(edits) = object.get("textEdits") {
                object.insert("textEdits".into(), self.map_text_edits(uri, edits.clone())?);
            }
            if let Some(label) = object.get_mut("label") {
                if let Some(parts) = label.as_array_mut() {
                    for part in parts {
                        let part = object_mut(part)?;
                        if let Some(location) = part.get("location") {
                            part.insert(
                                "location".into(),
                                self.map_location(uri, location.clone())?,
                            );
                        }
                    }
                } else if !label.is_string() {
                    return Err(MappingError::new(
                        "invalid_response",
                        "Inlay label must be a string or label-part array",
                    ));
                }
            } else {
                return Err(MappingError::new("invalid_response", "Missing inlay label"));
            }
            if !output.contains(&value) {
                output.push(value);
            }
        }
        Ok(Value::Array(output))
    }

    /// # Errors
    /// Rejects the whole linked-editing group if a member is unrepresentable.
    pub fn map_linked_editing_ranges(
        &self,
        uri: &str,
        mut value: Value,
    ) -> Result<Value, MappingError> {
        if value.is_null() {
            return Ok(value);
        }
        let object = object_mut(&mut value)?;
        let ranges = field(object, "ranges")?.as_array().ok_or_else(|| {
            MappingError::new("invalid_response", "Linked ranges must be an array")
        })?;
        let mut edits = Vec::new();
        for range in ranges {
            edits.push(json!({"range":self.required_range(uri, range)?.range,"newText":""}));
        }
        let ranges = validated_edits(edits)?
            .into_iter()
            .map(|edit| edit["range"].clone())
            .collect::<Vec<_>>();
        object.insert("ranges".into(), Value::Array(ranges));
        Ok(value)
    }

    /// Map a WorkspaceEdit atomically, deduplicating repeated projection edits
    /// and rejecting conflicts, unknown snapshots and generated helper edits.
    /// # Errors
    /// Returns an error instead of a partially applicable rename.
    pub fn map_workspace_edit(&self, value: Value) -> Result<Value, MappingError> {
        if value.is_null() {
            return Ok(value);
        }
        let input = value.as_object().ok_or_else(|| {
            MappingError::new("invalid_response", "WorkspaceEdit must be an object")
        })?;
        let mut grouped: BTreeMap<String, Vec<Value>> = BTreeMap::new();
        let mut versions: BTreeMap<String, Value> = BTreeMap::new();
        if let Some(changes) = input.get("changes") {
            let changes = changes.as_object().ok_or_else(|| {
                MappingError::new("invalid_response", "changes must be an object")
            })?;
            for (uri, edits) in changes {
                self.collect_edits(uri, edits, &mut grouped)?;
            }
        }
        let mut operations = Vec::new();
        if let Some(changes) = input.get("documentChanges") {
            let changes = changes.as_array().ok_or_else(|| {
                MappingError::new("invalid_response", "documentChanges must be an array")
            })?;
            for change in changes {
                if change.get("kind").is_some() {
                    // A rename/create/delete of an overlay is not equivalent to
                    // changing the original Svelte file's resource identity.
                    return Err(MappingError::new(
                        "unsupported_resource_edit",
                        "Resource operations require an explicit host filesystem mapping",
                    ));
                }
                let document = change
                    .get("textDocument")
                    .and_then(Value::as_object)
                    .ok_or_else(|| MappingError::new("invalid_response", "Missing textDocument"))?;
                let uri = string_field(document, "uri")?;
                let entry = self.entry(uri)?;
                let version = field(document, "version")?;
                if !version.is_null()
                    && version.as_u64() != Some(u64::from(entry.generated.snapshot().version))
                {
                    return Err(MappingError::new(
                        "stale_version",
                        format!("Edit version does not match {uri}"),
                    ));
                }
                let mapped_version = if version.is_null() {
                    Value::Null
                } else {
                    json!(entry.original.snapshot().version)
                };
                let original_uri = entry.original.snapshot().uri.clone();
                if let Some(previous) = versions.get(&original_uri) {
                    if previous != &mapped_version
                        && !previous.is_null()
                        && !mapped_version.is_null()
                    {
                        return Err(MappingError::new(
                            "stale_version",
                            "Conflicting original edit versions",
                        ));
                    }
                }
                if !mapped_version.is_null() || !versions.contains_key(&original_uri) {
                    versions.insert(original_uri, mapped_version);
                }
                self.collect_edits(
                    uri,
                    change
                        .get("edits")
                        .ok_or_else(|| MappingError::new("invalid_response", "Missing edits"))?,
                    &mut grouped,
                )?;
            }
        }
        let mut output = input.clone();
        output.remove("changes");
        output.remove("documentChanges");
        let use_document_changes = input.contains_key("documentChanges");
        let mut changes = Map::new();
        for (uri, edits) in grouped {
            let edits = validated_edits(edits)?;
            if use_document_changes {
                operations.push(json!({"textDocument":{"uri":uri,"version":versions.get(&uri).unwrap_or(&Value::Null)},"edits":edits}));
            } else {
                changes.insert(uri, Value::Array(edits));
            }
        }
        if use_document_changes {
            output.insert("documentChanges".into(), Value::Array(operations));
        } else {
            output.insert("changes".into(), Value::Object(changes));
        }
        Ok(Value::Object(output))
    }

    fn collect_edits(
        &self,
        uri: &str,
        edits: &Value,
        grouped: &mut BTreeMap<String, Vec<Value>>,
    ) -> Result<(), MappingError> {
        let entry = self.entry(uri)?;
        let edits = edits
            .as_array()
            .ok_or_else(|| MappingError::new("invalid_response", "Text edits must be an array"))?;
        for edit in edits {
            let mapped = self.map_text_edit(uri, edit.clone())?;
            if mapped.get("range").is_none() {
                return Err(MappingError::new(
                    "invalid_edit",
                    "Workspace edits cannot use completion insert/replace ranges",
                ));
            }
            grouped
                .entry(entry.original.snapshot().uri.clone())
                .or_default()
                .push(mapped);
        }
        Ok(())
    }
}

fn object_mut(value: &mut Value) -> Result<&mut Map<String, Value>, MappingError> {
    value
        .as_object_mut()
        .ok_or_else(|| MappingError::new("invalid_response", "Expected an object"))
}

fn field<'a>(object: &'a Map<String, Value>, name: &str) -> Result<&'a Value, MappingError> {
    object
        .get(name)
        .ok_or_else(|| MappingError::new("invalid_response", format!("Missing {name}")))
}

fn string_field<'a>(object: &'a Map<String, Value>, name: &str) -> Result<&'a str, MappingError> {
    field(object, name)?
        .as_str()
        .ok_or_else(|| MappingError::new("invalid_response", format!("{name} must be a string")))
}

fn parse_range(value: &Value) -> Result<Range, MappingError> {
    let range: Range = serde_json::from_value(value.clone())
        .map_err(|e| MappingError::new("invalid_range", e.to_string()))?;
    if point(range.start) > point(range.end) {
        return Err(MappingError::new("invalid_range", "Range is reversed"));
    }
    Ok(range)
}

fn point(position: Position) -> (u32, u32) {
    (position.line, position.character)
}

fn integer(value: &Value) -> Result<u32, MappingError> {
    value
        .as_u64()
        .and_then(|number| u32::try_from(number).ok())
        .ok_or_else(|| MappingError::new("invalid_response", "Expected an unsigned 32-bit integer"))
}

fn integer_field(object: &Map<String, Value>, name: &str) -> Result<u32, MappingError> {
    integer(field(object, name)?)
}

fn optional_integer_field(
    object: &Map<String, Value>,
    name: &str,
) -> Result<Option<u32>, MappingError> {
    object.get(name).map(integer).transpose()
}

fn line_end_character(document: &Document, line: u32) -> Result<u32, MappingError> {
    let start = document.offset_at(Position { line, character: 0 })?;
    let text = &document.text()[start..];
    let end = text.find(['\n', '\r']).unwrap_or(text.len());
    Ok(document.position_at(start + end)?.character)
}

fn as_replace_edit(edit: &Value) -> Value {
    if edit.get("range").is_some() {
        edit.clone()
    } else {
        json!({"range":edit["replace"],"newText":edit["newText"]})
    }
}

fn validated_edits(edits: Vec<Value>) -> Result<Vec<Value>, MappingError> {
    let mut unique = Vec::new();
    let mut ranges: Vec<Range> = Vec::new();
    for edit in edits {
        if unique.contains(&edit) {
            continue;
        }
        let range = parse_range(
            edit.get("range")
                .ok_or_else(|| MappingError::new("invalid_edit", "Missing edit range"))?,
        )?;
        for existing in &ranges {
            let start = point(range.start);
            let end = point(range.end);
            let other_start = point(existing.start);
            let other_end = point(existing.end);
            let overlap = start < other_end && other_start < end;
            let coincident_insertion = (start == end && start >= other_start && start <= other_end)
                || (other_start == other_end && other_start >= start && other_start <= end);
            if overlap || coincident_insertion {
                return Err(MappingError::new(
                    "conflicting_edits",
                    "Mapped edits overlap or insert different text at the same position",
                ));
            }
        }
        ranges.push(range);
        unique.push(edit);
    }
    Ok(unique)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn range(start: u32, end: u32) -> Value {
        json!({"start":{"line":0,"character":start},"end":{"line":0,"character":end}})
    }

    fn registry() -> EditorMap {
        let mut registry = EditorMap::new();
        registry
            .register(
                Snapshot {
                    uri: "file:///App.svelte".into(),
                    version: 7,
                    text: "😀name".into(),
                },
                "file:///App.svelte.ts".into(),
                "let name;name".into(),
                vec![
                    Mapping {
                        source: ByteRange { start: 4, end: 8 },
                        generated: ByteRange { start: 4, end: 8 },
                    },
                    Mapping {
                        source: ByteRange { start: 4, end: 8 },
                        generated: ByteRange { start: 9, end: 13 },
                    },
                ],
            )
            .unwrap();
        registry
    }

    #[test]
    fn cursor_occurrences_and_utf16_hover() {
        let registry = registry();
        let positions = registry
            .generated_positions(
                "file:///App.svelte",
                Position {
                    line: 0,
                    character: 3,
                },
            )
            .unwrap();
        assert_eq!(
            positions
                .iter()
                .map(|p| p.position.character)
                .collect::<Vec<_>>(),
            vec![5, 10]
        );
        let hover = registry
            .map_hover(
                "file:///App.svelte.ts",
                json!({"contents":"name: string","range":range(4,8)}),
            )
            .unwrap();
        assert_eq!(hover["range"], range(2, 6));
        assert!(
            registry
                .generated_positions(
                    "file:///App.svelte",
                    Position {
                        line: 0,
                        character: 1
                    }
                )
                .is_err()
        );
    }

    #[test]
    fn locations_links_and_references_deduplicate() {
        let registry = registry();
        let result = registry
            .map_locations(
                "file:///App.svelte.ts",
                json!([
                    {"uri":"file:///App.svelte.ts","range":range(4,8)},
                    {"uri":"file:///App.svelte.ts","range":range(9,13)}
                ]),
            )
            .unwrap();
        assert_eq!(result.as_array().unwrap().len(), 1);
        assert_eq!(result[0]["uri"], "file:///App.svelte");
        let result = registry.map_locations("file:///App.svelte.ts", json!({"targetUri":"file:///App.svelte.ts","targetRange":range(9,13),"targetSelectionRange":range(10,12),"originSelectionRange":range(4,8)})).unwrap();
        assert_eq!(result["targetSelectionRange"], range(3, 5));
        assert_eq!(result["originSelectionRange"], range(2, 6));
    }

    #[test]
    fn rename_is_atomic_deduplicated_and_rejects_helpers() {
        let registry = registry();
        let rename = |second: Value| json!({"changes":{"file:///App.svelte.ts":[{"range":range(4,8),"newText":"renamed"},second]}});
        let mapped = registry
            .map_workspace_edit(rename(json!({"range":range(9,13),"newText":"renamed"})))
            .unwrap();
        assert_eq!(
            mapped["changes"]["file:///App.svelte"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            registry
                .map_workspace_edit(rename(json!({"range":range(0,3),"newText":"var"})))
                .unwrap_err()
                .code,
            "unrepresentable_range"
        );
        assert_eq!(
            registry
                .map_workspace_edit(rename(json!({"range":range(9,13),"newText":"conflict"})))
                .unwrap_err()
                .code,
            "conflicting_edits"
        );
    }

    #[test]
    fn completion_defaults_and_additional_edits_are_checked() {
        let registry = registry();
        let completion = json!({"isIncomplete":false,"itemDefaults":{"editRange":{"insert":range(4,6),"replace":range(4,8)}},"items":[{"label":"renamed"}]});
        let mapped = registry
            .map_completions("file:///App.svelte.ts", completion)
            .unwrap();
        assert_eq!(mapped["itemDefaults"]["editRange"]["replace"], range(2, 6));
        let bad = json!([{"label":"renamed","textEdit":{"range":range(4,8),"newText":"renamed"},"additionalTextEdits":[{"range":range(0,3),"newText":"import"}]}]);
        assert_eq!(
            registry
                .map_completions("file:///App.svelte.ts", bad)
                .unwrap_err()
                .code,
            "unrepresentable_range"
        );
    }

    #[test]
    fn stale_and_unknown_edits_are_rejected() {
        let registry = registry();
        let edit = json!({"documentChanges":[{"textDocument":{"uri":"file:///App.svelte.ts","version":6},"edits":[{"range":range(4,8),"newText":"x"}]}]});
        assert_eq!(
            registry.map_workspace_edit(edit).unwrap_err().code,
            "stale_version"
        );
        let edit = json!({"changes":{"file:///unknown.ts":[{"range":range(0,0),"newText":"x"}]}});
        assert_eq!(
            registry.map_workspace_edit(edit).unwrap_err().code,
            "unknown_document"
        );
    }

    #[test]
    fn identity_empty_file_and_projection_validation() {
        let mut registry = EditorMap::new();
        registry
            .register_identity(
                Snapshot {
                    uri: "file:///empty.ts".into(),
                    version: 1,
                    text: String::new(),
                },
                "file:///overlay/empty.ts".into(),
            )
            .unwrap();
        let result = registry.map_workspace_edit(json!({"changes":{"file:///overlay/empty.ts":[{"range":range(0,0),"newText":"let x = 1;"}]}})).unwrap();
        assert_eq!(
            result["changes"]["file:///empty.ts"][0]["newText"],
            "let x = 1;"
        );
        let invalid = registry.register(
            Snapshot {
                uri: "file:///bad.svelte".into(),
                version: 1,
                text: "abc".into(),
            },
            "file:///bad.svelte.ts".into(),
            "xyz".into(),
            vec![Mapping {
                source: ByteRange { start: 0, end: 3 },
                generated: ByteRange { start: 0, end: 3 },
            }],
        );
        assert_eq!(invalid.unwrap_err().code, "invalid_projection");
    }

    #[test]
    fn replacement_is_atomic_and_virtual_versions_are_independent() {
        let mut registry = registry();
        assert!(
            registry
                .replace(
                    Snapshot {
                        uri: "file:///App.svelte".into(),
                        version: 0,
                        text: "changed".into()
                    },
                    "file:///App.svelte.ts".into(),
                    "different".into(),
                    vec![Mapping {
                        source: ByteRange { start: 0, end: 7 },
                        generated: ByteRange { start: 0, end: 7 }
                    }]
                )
                .is_err()
        );
        assert_eq!(
            registry
                .map_hover("file:///App.svelte.ts", json!({"range":range(4,8)}))
                .unwrap()["range"],
            range(2, 6)
        );
        registry
            .set_generated_version("file:///App.svelte.ts", 42)
            .unwrap();
        let result = registry.map_workspace_edit(json!({"documentChanges":[{"textDocument":{"uri":"file:///App.svelte.ts","version":42},"edits":[{"range":range(4,8),"newText":"next"}]}]})).unwrap();
        assert_eq!(result["documentChanges"][0]["textDocument"]["version"], 7);
        registry
            .replace_identity(
                Snapshot {
                    uri: "file:///App.svelte".into(),
                    version: 0,
                    text: "disk".into(),
                },
                "file:///App.svelte.ts".into(),
            )
            .unwrap();
        assert_eq!(
            registry
                .generated_ranges(
                    "file:///App.svelte",
                    serde_json::from_value(range(0, 4)).unwrap()
                )
                .unwrap()[0]
                .range,
            serde_json::from_value(range(0, 4)).unwrap()
        );
    }

    #[test]
    fn symbols_highlights_and_selection_parents_map() {
        let registry = registry();
        let symbols = registry.map_document_symbols("file:///App.svelte.ts", json!([{"name":"helper","kind":12,"range":range(0,13),"selectionRange":range(0,3),"children":[{"name":"name","kind":13,"range":range(0,8),"selectionRange":range(4,8)}]}])).unwrap();
        assert_eq!(symbols[0]["name"], "name");
        assert_eq!(symbols[0]["range"], range(2, 6));
        let highlights = registry.map_highlights("file:///App.svelte.ts", json!([{"range":range(0,3)},{"range":range(4,8),"kind":2},{"range":range(9,13),"kind":2}])).unwrap();
        assert_eq!(highlights.as_array().unwrap().len(), 1);
        let selections = registry.map_selection_ranges("file:///App.svelte.ts", json!([{"range":range(5,6),"parent":{"range":range(4,8),"parent":{"range":range(0,13)}}}])).unwrap();
        assert_eq!(selections[0]["range"], range(3, 4));
        assert_eq!(selections[0]["parent"]["range"], range(2, 6));
        assert!(selections[0]["parent"].get("parent").is_none());
    }

    #[test]
    fn folding_crlf_and_signature_label_offsets() {
        let mut registry = EditorMap::new();
        registry
            .register_identity(
                Snapshot {
                    uri: "file:///original.ts".into(),
                    version: 1,
                    text: "const f = () => {\r\n  return 1;\r\n};".into(),
                },
                "file:///virtual.ts".into(),
            )
            .unwrap();
        let folds = registry
            .map_folding_ranges(
                "file:///virtual.ts",
                json!([{"startLine":0,"endLine":2,"kind":"region"}]),
            )
            .unwrap();
        assert_eq!(folds[0]["endCharacter"], 2);
        let signature = json!({"signatures":[{"label":"f(😀: string)","parameters":[{"label":[2,4]}]}],"activeSignature":0});
        assert_eq!(
            registry.map_signature_help(signature.clone()).unwrap(),
            signature
        );
    }

    #[test]
    fn code_action_edits_map_and_data_stays_opaque() {
        let registry = registry();
        let data = json!({"uri":"file:///App.svelte.ts","range":range(0,13)});
        let actions = registry.map_code_actions("file:///App.svelte.ts", json!([{"title":"Rename","data":data,"edit":{"changes":{"file:///App.svelte.ts":[{"range":range(4,8),"newText":"renamed"}]}},"diagnostics":[{"range":range(4,8),"message":"name"}]}])).unwrap();
        assert_eq!(actions[0]["data"], data);
        assert_eq!(actions[0]["diagnostics"][0]["range"], range(2, 6));
        assert_eq!(
            actions[0]["edit"]["changes"]["file:///App.svelte"][0]["range"],
            range(2, 6)
        );
    }

    #[test]
    fn semantic_tokens_sort_deduplicate_and_reject_delta_and_conflicts() {
        let registry = registry();
        let tokens = registry
            .map_semantic_tokens(
                "file:///App.svelte.ts",
                json!({"resultId":"one","data":[0,0,3,1,0, 0,4,4,2,1, 0,5,4,2,1]}),
            )
            .unwrap();
        assert_eq!(tokens["data"], json!([0, 2, 4, 2, 1]));
        assert_eq!(tokens["resultId"], "one");
        assert_eq!(
            registry
                .map_semantic_tokens("file:///App.svelte.ts", json!({"edits":[]}))
                .unwrap_err()
                .code,
            "unsupported_token_delta"
        );
        assert_eq!(
            registry
                .map_semantic_tokens(
                    "file:///App.svelte.ts",
                    json!({"data":[0,4,4,2,1, 0,5,4,3,1]})
                )
                .unwrap_err()
                .code,
            "conflicting_tokens"
        );
    }

    #[test]
    fn inlay_and_linked_editing_map_all_embedded_ranges() {
        let registry = registry();
        let hints = registry.map_inlay_hints("file:///App.svelte.ts", json!([{"position":{"line":0,"character":5},"label":[{"value":": string","location":{"uri":"file:///App.svelte.ts","range":range(4,8)}}],"textEdits":[{"range":range(5,5),"newText":": string"}],"data":{"preserve":true}}])).unwrap();
        assert_eq!(hints[0]["position"]["character"], 3);
        assert_eq!(hints[0]["label"][0]["location"]["range"], range(2, 6));
        assert_eq!(hints[0]["textEdits"][0]["range"], range(3, 3));
        let linked = registry
            .map_linked_editing_ranges(
                "file:///App.svelte.ts",
                json!({"ranges":[range(4,8),range(9,13)],"wordPattern":"[a-z]+"}),
            )
            .unwrap();
        assert_eq!(linked["ranges"], json!([range(2, 6)]));
        assert_eq!(linked["wordPattern"], "[a-z]+");
    }
}
