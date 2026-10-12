use crate::mapping::lines;
use comrak::nodes::{NodeValue, Sourcepos};
use comrak::{Arena, Options, parse_document};
use std::ops::Range;

#[derive(Debug, Clone)]
pub(crate) struct Island {
    pub token: String,
    pub original: Range<usize>,
    pub module_open: Option<usize>,
}

pub(crate) struct Protected {
    pub text: String,
    pub islands: Vec<Island>,
    pub copies: Vec<(Range<usize>, Range<usize>)>,
}

pub(crate) fn source_range(pos: Sourcepos, text: &str, starts: &[usize]) -> Option<Range<usize>> {
    let start = *starts.get(pos.start.line.checked_sub(1)?)? + pos.start.column.checked_sub(1)?;
    let end = *starts.get(pos.end.line.checked_sub(1)?)? + pos.end.column;
    (start <= end && text.is_char_boundary(start) && text.is_char_boundary(end))
        .then_some(start..end)
}

impl Protected {
    pub fn original(&self, range: Range<usize>) -> Option<Range<usize>> {
        let index = self
            .copies
            .partition_point(|(generated, _)| generated.start <= range.start)
            .checked_sub(1)?;
        self.copies.get(index).and_then(|(generated, original)| {
            (generated.start <= range.start && range.end <= generated.end).then(|| {
                original.start + range.start - generated.start
                    ..original.start + range.end - generated.start
            })
        })
    }
}

pub(crate) fn protect(source: &str, start: usize, prefix: &str) -> Result<Protected, String> {
    let tail = &source[start..];
    let arena = Arena::new();
    let options = Options::default();
    let root = parse_document(&arena, tail, &options);
    let starts = lines(tail);
    let mut code = root
        .descendants()
        .filter_map(|node| {
            let data = node.data.borrow();
            matches!(data.value, NodeValue::Code(_) | NodeValue::CodeBlock(_))
                .then(|| source_range(data.sourcepos, tail, &starts))
                .flatten()
        })
        .collect::<Vec<_>>();
    code.sort_by_key(|range| range.start);
    let mut result = Protected {
        text: String::new(),
        islands: Vec::new(),
        copies: Vec::new(),
    };
    let mut cursor = 0;
    let mut copied = 0;
    let mut code_index = 0;
    while cursor < tail.len() {
        while code
            .get(code_index)
            .is_some_and(|range| range.end <= cursor)
        {
            code_index += 1;
        }
        if let Some(range) = code
            .get(code_index)
            .filter(|range| range.start <= cursor && cursor < range.end)
        {
            cursor = range.end;
            continue;
        }
        if tail.as_bytes()[cursor] == b'\\' {
            cursor += 1;
            if cursor < tail.len() {
                cursor += tail[cursor..].chars().next().unwrap().len_utf8();
            }
            continue;
        }
        let mut module_open = None;
        let (end, block) = if tail[cursor..].starts_with("<!--") {
            let end = tail[cursor + 4..]
                .find("-->")
                .ok_or("Unclosed HTML comment")?
                + cursor
                + 7;
            (end, true)
        } else if tail[cursor..].starts_with("<script") || tail[cursor..].starts_with("<style") {
            let name = if tail[cursor..].starts_with("<script") {
                "script"
            } else {
                "style"
            };
            let after_name = cursor + 1 + name.len();
            if !tail
                .as_bytes()
                .get(after_name)
                .is_some_and(|c| c.is_ascii_whitespace() || *c == b'>')
            {
                cursor += 1;
                continue;
            }
            let open = tag_end(tail, cursor)?;
            let closing = format!("</{name}>");
            let close = tail[open..]
                .find(&closing)
                .ok_or_else(|| format!("Unclosed {name} element"))?
                + open
                + closing.len();
            if name == "script" && module(&tail[after_name..open - 1])? {
                module_open = Some(start + open);
            }
            (close, true)
        } else if tail.as_bytes()[cursor] == b'{' {
            let block = tail
                .as_bytes()
                .get(cursor + 1)
                .is_some_and(|c| [b'#', b'/', b':', b'@'].contains(c));
            (expression_end(tail, cursor)?, block)
        } else {
            cursor += tail[cursor..].chars().next().unwrap().len_utf8();
            continue;
        };
        let begin = result.text.len();
        result.text.push_str(&tail[copied..cursor]);
        result
            .copies
            .push((begin..result.text.len(), start + copied..start + cursor));
        let token = if block {
            format!("<!--{prefix}{}END-->", result.islands.len())
        } else {
            format!("{prefix}{}END", result.islands.len())
        };
        result.text.push_str(&token);
        result.islands.push(Island {
            token,
            original: start + cursor..start + end,
            module_open,
        });
        cursor = end;
        copied = end;
    }
    let begin = result.text.len();
    result.text.push_str(&tail[copied..]);
    result
        .copies
        .push((begin..result.text.len(), start + copied..source.len()));
    Ok(result)
}

fn tag_end(source: &str, start: usize) -> Result<usize, String> {
    let mut quote = None;
    for (relative, character) in source[start..].char_indices() {
        if let Some(expected) = quote {
            if character == expected {
                quote = None;
            }
        } else if character == '"' || character == '\'' {
            quote = Some(character);
        } else if character == '>' {
            return Ok(start + relative + 1);
        }
    }
    Err("Unclosed Svelte tag".into())
}

fn module(attributes: &str) -> Result<bool, String> {
    let mut attributes = attributes.trim();
    let mut found = false;
    while !attributes.is_empty() {
        let end = attributes
            .find(|c: char| c.is_whitespace() || c == '=')
            .unwrap_or(attributes.len());
        if end == 0 {
            return Err("Unsupported script attribute syntax".into());
        }
        let name = &attributes[..end];
        attributes = attributes[end..].trim_start();
        let mut value = None;
        if let Some(rest) = attributes.strip_prefix('=') {
            attributes = rest.trim_start();
            if let Some(quote) = attributes
                .chars()
                .next()
                .filter(|c| *c == '"' || *c == '\'')
            {
                let end = attributes[1..]
                    .find(quote)
                    .ok_or("Unclosed script attribute")?
                    + 1;
                value = Some(&attributes[1..end]);
                attributes = attributes[end + 1..].trim_start();
            } else {
                let end = attributes
                    .find(char::is_whitespace)
                    .unwrap_or(attributes.len());
                value = Some(&attributes[..end]);
                attributes = attributes[end..].trim_start();
            }
        }
        found |=
            name == "module" && value.is_none() || name == "context" && value == Some("module");
    }
    Ok(found)
}

fn expression_end(source: &str, start: usize) -> Result<usize, String> {
    let bytes = source.as_bytes();
    let mut cursor = start + 1;
    // Closing/else block names are not JavaScript regex literals.
    if bytes.get(cursor) == Some(&b'/') {
        cursor += 1;
    }
    let mut depth = 1usize;
    let mut operand = true;
    while cursor < bytes.len() {
        match bytes[cursor] {
            b'\'' | b'"' | b'`' => {
                let quote = bytes[cursor];
                cursor += 1;
                let mut closed = false;
                while cursor < bytes.len() {
                    if bytes[cursor] == b'\\' {
                        cursor += 2;
                        continue;
                    }
                    if quote == b'`' && bytes[cursor..].starts_with(b"${") {
                        cursor = expression_end(source, cursor + 1)?;
                        continue;
                    }
                    if bytes[cursor] == quote {
                        cursor += 1;
                        closed = true;
                        break;
                    }
                    cursor += 1;
                }
                if !closed {
                    return Err("Unclosed string in Svelte expression".into());
                }
                operand = false;
            }
            b'/' if bytes.get(cursor + 1) == Some(&b'/') => {
                cursor = source[cursor..]
                    .find('\n')
                    .map(|n| n + cursor)
                    .ok_or("Unclosed Svelte expression after line comment")?;
            }
            b'/' if bytes.get(cursor + 1) == Some(&b'*') => {
                cursor = source[cursor + 2..]
                    .find("*/")
                    .map(|n| n + cursor + 4)
                    .ok_or("Unclosed expression comment")?;
            }
            b'/' if operand => {
                cursor += 1;
                let mut class = false;
                let mut closed = false;
                while cursor < bytes.len() {
                    if bytes[cursor] == b'\\' {
                        cursor += 2;
                        continue;
                    }
                    if bytes[cursor] == b'[' {
                        class = true;
                    }
                    if bytes[cursor] == b']' {
                        class = false;
                    }
                    if bytes[cursor] == b'/' && !class {
                        cursor += 1;
                        closed = true;
                        break;
                    }
                    cursor += 1;
                }
                if !closed {
                    return Err("Unclosed regular expression".into());
                }
                operand = false;
            }
            b'{' => {
                depth += 1;
                cursor += 1;
                operand = true;
            }
            b'}' => {
                depth -= 1;
                cursor += 1;
                if depth == 0 {
                    return Ok(cursor);
                }
                operand = false;
            }
            b'(' | b'[' | b'=' | b':' | b',' | b'!' | b'?' | b'+' | b'-' | b'*' | b'/' | b'&'
            | b'|' => {
                operand = true;
                cursor += 1;
            }
            byte if byte.is_ascii_whitespace() => cursor += 1,
            _ => {
                operand = false;
                cursor += 1;
            }
        }
    }
    Err("Unclosed Svelte expression".into())
}
