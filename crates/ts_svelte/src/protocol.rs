//! Version-1 stateless JSON protocol shared by the native CLI and WASM.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{ByteRange, Chunk, Document, Edit, Error, Position, Projection, Snapshot};

pub const MAX_REQUEST_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_OPERATIONS: usize = 4096;
pub const MAX_WORK_BYTES: u64 = 128 * 1024 * 1024;

#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "camelCase", deny_unknown_fields)]
pub enum Request {
    Capabilities {},
    Locate {
        document: Snapshot,
        positions: Vec<Position>,
        offsets: Vec<u32>,
    },
    Edit {
        document: Snapshot,
        version: u32,
        edits: Vec<Edit>,
    },
    Project {
        document: Snapshot,
        chunks: Vec<Chunk>,
        ranges: Vec<ByteRange>,
    },
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum Response {
    Success { ok: bool, result: Value },
    Failure { ok: bool, error: Error },
}

fn execute(request: Request) -> Result<Value, Error> {
    match request {
        Request::Capabilities {} => Ok(json!({
            "protocolVersion": 1,
            "stage": "source-foundation",
            "coordinates": {"offsets": "utf8-bytes", "positions": "utf16", "ranges": "half-open"},
            "operations": ["capabilities", "locate", "edit", "project"],
            "svelteParsing": false, "typeChecking": false, "compilation": false, "lsp": false,
            "limits": {"requestBytes": MAX_REQUEST_BYTES, "operations": MAX_OPERATIONS, "workBytes": MAX_WORK_BYTES,
                "projectionBytes": crate::mapping::MAX_PROJECTION_BYTES}
        })),
        Request::Locate {
            document,
            positions,
            offsets,
        } => {
            budget(&document, positions.len() + offsets.len(), 1)?;
            let document = Document::new(document);
            let byte_offsets = positions
                .into_iter()
                .map(|p| document.offset_at(p))
                .collect::<Result<Vec<_>, _>>()?;
            let positions = offsets
                .into_iter()
                .map(|offset| document.position_at(offset as usize))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(
                json!({"uri": document.snapshot().uri, "version": document.snapshot().version,
                "byteOffsets": byte_offsets, "positions": positions, "lineCount": document.line_count()}),
            )
        }
        Request::Edit {
            document,
            version,
            edits,
        } => {
            budget(&document, edits.len(), 1)?;
            // Full replacements can make subsequent edits much more expensive.
            // Bound using the largest possible intermediate document size.
            let maximum_bytes = edits
                .iter()
                .try_fold(document.text.len(), |size, edit| {
                    size.checked_add(edit.text.len())
                })
                .ok_or_else(|| Error::new("size_limit", "edit batch is too large"))?;
            work_budget(maximum_bytes, edits.len(), 1)?;
            let document = Document::new(document).edited(version, &edits)?;
            Ok(json!({"document": document.snapshot(), "lineCount": document.line_count()}))
        }
        Request::Project {
            document,
            chunks,
            ranges,
        } => {
            budget(&document, chunks.len() + ranges.len(), 2)?;
            let document = Document::new(document);
            let projection = Projection::new(&document, &chunks)?;
            let mapped = ranges
                .into_iter()
                .map(|range| match projection.to_source(range)? {
                    Some(bytes) => {
                        Ok(json!({"bytes": bytes, "range": document.editor_range(bytes)?}))
                    }
                    None => Ok(Value::Null),
                })
                .collect::<Result<Vec<_>, Error>>()?;
            Ok(
                json!({"uri": document.snapshot().uri, "version": document.snapshot().version,
                "text": projection.text(), "mappings": projection.mappings(), "ranges": mapped}),
            )
        }
    }
}

fn budget(document: &Snapshot, count: usize, scans: u64) -> Result<(), Error> {
    if count > MAX_OPERATIONS {
        return Err(Error::new("size_limit", "request exceeds 4096 operations"));
    }
    work_budget(document.text.len(), count, scans)
}

fn work_budget(bytes: usize, count: usize, scans: u64) -> Result<(), Error> {
    let work = (bytes as u64)
        .saturating_mul(count as u64 + 1)
        .saturating_mul(scans);
    if work > MAX_WORK_BYTES {
        return Err(Error::new(
            "size_limit",
            "request exceeds the 128 MiB scan-work budget",
        ));
    }
    Ok(())
}

/// Handles one UTF-8 JSON request. Protocol errors are structured responses,
/// including invalid UTF-8, unknown fields/operations and invalid coordinates.
#[must_use]
pub fn dispatch(input: &[u8]) -> Response {
    let result = if input.len() > MAX_REQUEST_BYTES {
        Err(Error::new("size_limit", "request exceeds 16 MiB"))
    } else {
        serde_json::from_slice::<Request>(input)
            .map_err(|error| Error::new("invalid_request", error.to_string()))
            .and_then(execute)
    };
    match result {
        Ok(result) => Response::Success { ok: true, result },
        Err(error) => Response::Failure { ok: false, error },
    }
}

/// Encodes a response using only JSON-representable data.
#[must_use]
pub fn dispatch_json(input: &[u8]) -> Vec<u8> {
    // All values in Response are JSON values, strings and booleans.
    serde_json::to_vec(&dispatch(input)).unwrap_or_else(|_| {
        br#"{"ok":false,"error":{"code":"internal_error","message":"response serialization failed"}}"#.to_vec()
    })
}
