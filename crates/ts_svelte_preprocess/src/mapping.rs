use serde::Serialize;
use serde_json::{Value, json};
use std::ops::Range;

/// A byte-for-byte unchanged region. Only these regions promise exact mapping.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CopySpan {
    pub generated: Range<usize>,
    pub original: Range<usize>,
}

pub(crate) struct Output<'a> {
    source: &'a str,
    pub code: String,
    pub copies: Vec<CopySpan>,
}

impl<'a> Output<'a> {
    pub fn new(source: &'a str) -> Self {
        Self {
            source,
            code: String::new(),
            copies: Vec::new(),
        }
    }

    pub fn generated(&mut self, text: &str) {
        self.code.push_str(text);
    }

    pub fn copy(&mut self, range: Range<usize>) {
        if range.is_empty() {
            return;
        }
        let start = self.code.len();
        self.code.push_str(&self.source[range.clone()]);
        self.copies.push(CopySpan {
            generated: start..self.code.len(),
            original: range,
        });
    }

    pub fn map(&self, filename: &str) -> Value {
        let source_lines = lines(self.source);
        let mut columns = vec![0u32; self.source.len() + 1];
        let mut column = 0;
        for (offset, character) in self.source.char_indices() {
            columns[offset] = column;
            column = if character == '\n' {
                0
            } else {
                column + character.len_utf16() as u32
            };
        }
        columns[self.source.len()] = column;
        let mut points = Vec::new();
        for span in &self.copies {
            for (offset, _) in self.source[span.original.clone()].char_indices() {
                points.push((
                    span.generated.start + offset,
                    Some(span.original.start + offset),
                ));
            }
            points.push((span.generated.end, None));
        }
        points.sort_by_key(|(generated, _)| *generated);
        let mut mappings = String::new();
        let mut generated_column = 0i64;
        let mut previous_column = 0i64;
        let mut original_line = 0i64;
        let mut original_column = 0i64;
        let mut cursor = 0;
        let mut first = true;
        for (index, &(offset, original)) in points.iter().enumerate() {
            // At a copied-run boundary the following run takes precedence.
            if points.get(index + 1).is_some_and(|point| point.0 == offset) {
                continue;
            }
            for character in self.code[cursor..offset].chars() {
                if character == '\n' {
                    mappings.push(';');
                    generated_column = 0;
                    previous_column = 0;
                    first = true;
                } else {
                    generated_column += character.len_utf16() as i64;
                }
            }
            cursor = offset;
            if !first {
                mappings.push(',');
            }
            first = false;
            vlq(generated_column - previous_column, &mut mappings);
            previous_column = generated_column;
            if let Some(original) = original {
                let line = source_lines.partition_point(|&start| start <= original) - 1;
                let column = i64::from(columns[original]);
                vlq(0, &mut mappings);
                vlq(line as i64 - original_line, &mut mappings);
                vlq(column - original_column, &mut mappings);
                original_line = line as i64;
                original_column = column;
            }
        }
        json!({"version":3,"file":filename,"sources":[filename],"sourcesContent":[self.source],"names":[],"mappings":mappings})
    }
}

pub(crate) fn lines(source: &str) -> Vec<usize> {
    std::iter::once(0)
        .chain(source.match_indices('\n').map(|(offset, _)| offset + 1))
        .collect()
}

fn vlq(value: i64, output: &mut String) {
    const BASE64: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut value = (value.unsigned_abs() << 1) | u64::from(value < 0);
    loop {
        let mut digit = (value & 31) as usize;
        value >>= 5;
        if value != 0 {
            digit |= 32;
        }
        output.push(BASE64[digit] as char);
        if value == 0 {
            break;
        }
    }
}
