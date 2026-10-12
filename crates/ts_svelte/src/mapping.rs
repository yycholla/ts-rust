//! Exact copied-span mappings; generated helpers intentionally have no source.

use serde::{Deserialize, Serialize};

use crate::{ByteRange, Document, Error};

/// Bounds expansion when a transform repeats the same source many times.
pub const MAX_PROJECTION_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub enum Chunk {
    Copy(ByteRange),
    Generated(String),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct Mapping {
    pub generated: ByteRange,
    pub source: ByteRange,
}

#[derive(Debug)]
pub struct Projection<'a> {
    source: &'a Document,
    text: String,
    mappings: Vec<Mapping>,
}

impl<'a> Projection<'a> {
    /// Creates a projection tied to an immutable source snapshot. Adjacent
    /// contiguous copies coalesce; repeated/reordered copies remain distinct.
    /// # Errors
    /// Returns an error for an invalid copied source range.
    pub fn new(source: &'a Document, chunks: &[Chunk]) -> Result<Self, Error> {
        let mut projection = Self {
            source,
            text: String::new(),
            mappings: Vec::new(),
        };
        for chunk in chunks {
            let len = match chunk {
                Chunk::Generated(text) => text.len(),
                Chunk::Copy(range) => {
                    source.validate_bytes(*range)?;
                    range.end - range.start
                }
            };
            if len > MAX_PROJECTION_BYTES - projection.text.len() {
                return Err(Error::new("size_limit", "projection exceeds 16 MiB"));
            }
            match chunk {
                Chunk::Generated(text) => projection.text.push_str(text),
                Chunk::Copy(range) => {
                    source.validate_bytes(*range)?;
                    if range.start == range.end {
                        continue;
                    }
                    let start = projection.text.len();
                    projection
                        .text
                        .push_str(&source.text()[range.start..range.end]);
                    let end = projection.text.len();
                    if let Some(last) = projection.mappings.last_mut()
                        && last.generated.end == start
                        && last.source.end == range.start
                    {
                        last.generated.end = end;
                        last.source.end = range.end;
                    } else {
                        projection.mappings.push(Mapping {
                            generated: ByteRange { start, end },
                            source: *range,
                        });
                    }
                }
            }
        }
        Ok(projection)
    }

    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    #[must_use]
    pub fn source(&self) -> &Document {
        self.source
    }

    #[must_use]
    pub fn mappings(&self) -> &[Mapping] {
        &self.mappings
    }

    /// Find every exact generated occurrence of an original range, in generated
    /// order. Repeated template expressions may have several occurrences.
    /// Cursor points include copied-span boundaries; callers choose which
    /// generated occurrences to query. This does not weaken `to_source`'s
    /// stricter boundary policy for diagnostics or edits.
    /// # Errors
    /// Returns an error for an invalid original UTF-8 range.
    pub fn to_generated(&self, range: ByteRange) -> Result<Vec<ByteRange>, Error> {
        self.source.validate_bytes(range)?;
        let mut ranges = Vec::new();
        for mapping in &self.mappings {
            if range.start >= mapping.source.start && range.end <= mapping.source.end {
                let start = mapping.generated.start + range.start - mapping.source.start;
                let mapped = ByteRange {
                    start,
                    end: start + range.end - range.start,
                };
                if ranges.last() != Some(&mapped) {
                    ranges.push(mapped);
                }
            }
        }
        Ok(ranges)
    }

    /// Maps a whole generated range only when every byte is from one contiguous
    /// original range. A point at a boundary is mapped only if all touching
    /// copied spans agree. Points next to generated helpers are left unmapped.
    /// # Errors
    /// Returns an error for an invalid generated range.
    pub fn to_source(&self, range: ByteRange) -> Result<Option<ByteRange>, Error> {
        if range.start > range.end
            || !self.text.is_char_boundary(range.start)
            || !self.text.is_char_boundary(range.end)
        {
            return Err(Error::new(
                "invalid_range",
                "generated range is invalid or splits UTF-8",
            ));
        }
        if range.start == range.end {
            return Ok(self.map_point(range.start).map(|offset| ByteRange {
                start: offset,
                end: offset,
            }));
        }
        // Mappings are sorted by generated start and never overlap.
        let index = self
            .mappings
            .partition_point(|mapping| mapping.generated.start <= range.start);
        let Some(mapping) = index.checked_sub(1).and_then(|i| self.mappings.get(i)) else {
            return Ok(None);
        };
        if range.end > mapping.generated.end {
            return Ok(None);
        }
        let start = mapping.source.start + range.start - mapping.generated.start;
        Ok(Some(ByteRange {
            start,
            end: start + range.end - range.start,
        }))
    }

    fn map_point(&self, offset: usize) -> Option<usize> {
        let index = self
            .mappings
            .partition_point(|mapping| mapping.generated.start < offset);
        let left = index
            .checked_sub(1)
            .and_then(|i| self.mappings.get(i))
            .filter(|mapping| mapping.generated.end >= offset);
        let right = self
            .mappings
            .get(index)
            .filter(|mapping| mapping.generated.start == offset);
        let mapped = |mapping: &Mapping| mapping.source.start + offset - mapping.generated.start;
        match (left, right) {
            (Some(left), Some(right)) if mapped(left) == mapped(right) => Some(mapped(left)),
            (Some(left), None) if offset < left.generated.end || offset == self.text.len() => {
                Some(mapped(left))
            }
            (None, Some(right)) if offset == 0 => Some(mapped(right)),
            _ => None,
        }
    }
}

#[cfg(test)]
mod reverse_mapping_tests {
    use super::*;
    use crate::Snapshot;

    #[test]
    fn original_queries_find_repeated_unicode_occurrences() {
        let source = Document::new(Snapshot {
            uri: "test.svelte".into(),
            version: 1,
            text: "😀name".into(),
        });
        let projection = Projection::new(
            &source,
            &[
                Chunk::Generated("let ".into()),
                Chunk::Copy(ByteRange { start: 4, end: 8 }),
                Chunk::Generated("; ".into()),
                Chunk::Copy(ByteRange { start: 4, end: 8 }),
            ],
        )
        .unwrap();
        assert_eq!(
            projection
                .to_generated(ByteRange { start: 4, end: 8 })
                .unwrap(),
            vec![
                ByteRange { start: 4, end: 8 },
                ByteRange { start: 10, end: 14 }
            ]
        );
        assert_eq!(
            projection
                .to_generated(ByteRange { start: 8, end: 8 })
                .unwrap(),
            vec![
                ByteRange { start: 8, end: 8 },
                ByteRange { start: 14, end: 14 }
            ]
        );
        assert!(
            projection
                .to_generated(ByteRange { start: 0, end: 4 })
                .unwrap()
                .is_empty()
        );
        assert!(
            projection
                .to_generated(ByteRange { start: 1, end: 4 })
                .is_err()
        );
    }
}
