//! Strict UTF-8 byte / UTF-16 editor coordinates. Lines end at LF, CRLF or CR.
//! Columns past line end and positions inside surrogate pairs are errors.
//! Byte positions inside a CRLF terminator are also errors: there is no unique
//! editor position for that byte. The start of a terminator is the line end.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Position {
    pub line: u32,
    pub character: u32,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Range {
    pub start: Position,
    pub end: Position,
}

/// Half-open UTF-8 byte range, never a JavaScript string index.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ByteRange {
    #[serde(deserialize_with = "wire_offset")]
    pub start: usize,
    #[serde(deserialize_with = "wire_offset")]
    pub end: usize,
}

// The wire format has fixed-width integers on native and wasm32 alike.
fn wire_offset<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<usize, D::Error> {
    u32::deserialize(deserializer).map(|offset| offset as usize)
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    pub uri: String,
    pub version: u32,
    pub text: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Edit {
    /// None replaces the entire document, as in an LSP full-content change.
    pub range: Option<Range>,
    pub text: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Error {
    pub code: &'static str,
    pub message: String,
}

impl Error {
    pub(crate) fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for Error {}

#[derive(Clone, Debug)]
struct Line {
    start: usize,
    end: usize,
}

/// Immutable source snapshot. Edits create a new document; existing projections
/// keep borrowing the original, so a map cannot silently follow a newer version.
#[derive(Clone, Debug)]
pub struct Document {
    snapshot: Snapshot,
    lines: Vec<Line>,
}

impl Document {
    #[must_use]
    pub fn new(snapshot: Snapshot) -> Self {
        let bytes = snapshot.text.as_bytes();
        let mut lines = Vec::new();
        let mut start = 0;
        let mut i = 0;
        while i < bytes.len() {
            if matches!(bytes[i], b'\r' | b'\n') {
                lines.push(Line { start, end: i });
                i += if bytes[i] == b'\r' && bytes.get(i + 1) == Some(&b'\n') {
                    2
                } else {
                    1
                };
                start = i;
            } else {
                i += 1;
            }
        }
        lines.push(Line {
            start,
            end: bytes.len(),
        });
        Self { snapshot, lines }
    }

    #[must_use]
    pub fn snapshot(&self) -> &Snapshot {
        &self.snapshot
    }

    #[must_use]
    pub fn text(&self) -> &str {
        &self.snapshot.text
    }

    #[must_use]
    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    /// # Errors
    /// Returns an error for an invalid line, column or split surrogate pair.
    pub fn offset_at(&self, position: Position) -> Result<usize, Error> {
        let line = self
            .lines
            .get(position.line as usize)
            .ok_or_else(|| Error::new("invalid_position", "line is outside the document"))?;
        let mut column = 0_u64;
        for (relative, ch) in self.text()[line.start..line.end].char_indices() {
            if column == u64::from(position.character) {
                return Ok(line.start + relative);
            }
            column += ch.len_utf16() as u64;
            if column > u64::from(position.character) {
                return Err(Error::new(
                    "invalid_position",
                    "column splits a UTF-16 surrogate pair",
                ));
            }
        }
        if column == u64::from(position.character) {
            Ok(line.end)
        } else {
            Err(Error::new(
                "invalid_position",
                "column is past the end of the line",
            ))
        }
    }

    /// # Errors
    /// Returns an error for an out-of-bounds byte, a split codepoint/CRLF, or a
    /// coordinate too large for the protocol's u32 fields.
    pub fn position_at(&self, offset: usize) -> Result<Position, Error> {
        if !self.text().is_char_boundary(offset) {
            return Err(Error::new(
                "invalid_offset",
                "offset is outside the document or splits UTF-8",
            ));
        }
        let line_index = self.lines.partition_point(|line| line.start <= offset) - 1;
        let line = &self.lines[line_index];
        if offset > line.end {
            return Err(Error::new(
                "invalid_offset",
                "offset splits a CRLF line ending",
            ));
        }
        let character = self.text()[line.start..offset].encode_utf16().count();
        Ok(Position {
            line: u32::try_from(line_index)
                .map_err(|_| Error::new("size_limit", "too many lines"))?,
            character: u32::try_from(character)
                .map_err(|_| Error::new("size_limit", "line is too long"))?,
        })
    }

    /// Validates UTF-8 boundaries. CRLF can be copied as raw bytes; conversion
    /// to editor positions remains strict.
    /// # Errors
    /// Returns an error for a reversed, out-of-bounds or split-codepoint range.
    pub fn validate_bytes(&self, range: ByteRange) -> Result<(), Error> {
        if range.start > range.end
            || !self.text().is_char_boundary(range.start)
            || !self.text().is_char_boundary(range.end)
        {
            return Err(Error::new(
                "invalid_range",
                "range is reversed, outside the document, or splits UTF-8",
            ));
        }
        Ok(())
    }

    /// # Errors
    /// Returns an error when either endpoint or the range order is invalid.
    pub fn byte_range(&self, range: Range) -> Result<ByteRange, Error> {
        let bytes = ByteRange {
            start: self.offset_at(range.start)?,
            end: self.offset_at(range.end)?,
        };
        self.validate_bytes(bytes)?;
        Ok(bytes)
    }

    /// # Errors
    /// Returns an error when either byte endpoint cannot be represented exactly.
    pub fn editor_range(&self, range: ByteRange) -> Result<Range, Error> {
        self.validate_bytes(range)?;
        Ok(Range {
            start: self.position_at(range.start)?,
            end: self.position_at(range.end)?,
        })
    }

    /// Applies changes sequentially (LSP order) to a private copy, rebuilding
    /// the line index after each change. Failure leaves this snapshot unchanged.
    /// # Errors
    /// Rejects stale versions and invalid edit ranges.
    pub fn edited(&self, version: u32, edits: &[Edit]) -> Result<Self, Error> {
        if version <= self.snapshot.version {
            return Err(Error::new(
                "stale_version",
                "new version must be greater than the source version",
            ));
        }
        let mut next = self.clone();
        for edit in edits {
            let range = match edit.range {
                Some(range) => next.byte_range(range)?,
                None => ByteRange {
                    start: 0,
                    end: next.text().len(),
                },
            };
            let mut snapshot = next.snapshot;
            snapshot
                .text
                .replace_range(range.start..range.end, &edit.text);
            next = Self::new(snapshot);
        }
        next.snapshot.version = version;
        Ok(next)
    }
}
