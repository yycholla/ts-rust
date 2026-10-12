//! CSS parsing for the Tailwind intermediate representation, not a browser CSS validator.
use crate::ast::{AstNode, NodeKind, SourceLocation};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CssSyntaxError {
    pub message: String,
    pub location: SourceLocation,
}
impl std::fmt::Display for CssSyntaxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}:{}: {}",
            self.location.file, self.location.start, self.message
        )
    }
}
impl std::error::Error for CssSyntaxError {}

pub fn parse_at_rule(buffer: &str, nodes: Vec<AstNode>) -> AstNode {
    // Preserve the upstream minimum-name split convention.
    let split = buffer
        .char_indices()
        .find(|(i, c)| *i >= 5 && matches!(c, ' ' | '\t' | '('))
        .map(|(i, _)| i)
        .unwrap_or(buffer.len());
    AstNode::new(NodeKind::AtRule {
        name: buffer[..split].trim().into(),
        params: buffer[split..].trim().into(),
        nodes,
    })
}

pub fn parse(input: &str, file: &str) -> Result<Vec<AstNode>, CssSyntaxError> {
    if input.len() > 16 * 1024 * 1024 {
        return Err(error(file, 0, 0, "CSS input exceeds 16 MiB"));
    }
    let bytes = input.as_bytes();
    let mut i = usize::from(input.starts_with('\u{feff}')) * 3;
    let mut roots = Vec::new();
    let mut license = Vec::new();
    let mut parents: Vec<AstNode> = Vec::new();
    let mut brackets = Vec::new();
    let mut buffer = String::new();
    let mut start = i;
    while i < bytes.len() {
        let c = bytes[i];
        if c == b'\r' && bytes.get(i + 1) == Some(&b'\n') {
            i += 1;
            continue;
        }
        if c == b'\\' {
            if buffer.is_empty() {
                start = i;
            }
            let end = next_char_end(input, i + 1);
            buffer.push_str(&input[i..end]);
            i = end;
            continue;
        }
        if c == b'/' && bytes.get(i + 1) == Some(&b'*') {
            let end = comment_end(input, i, file)?;
            if bytes.get(i + 2) == Some(&b'!') {
                let mut node = AstNode::new(NodeKind::Comment {
                    value: input[i + 2..end - 2].into(),
                });
                node.src = Some(loc(file, i, end));
                license.push(node);
            }
            i = end;
            continue;
        }
        if matches!(c, b'\'' | b'"') {
            if buffer.is_empty() {
                start = i;
            }
            let end = string_end(input, i, file)?;
            buffer.push_str(&input[i..end]);
            i = end;
            continue;
        }
        if matches!(c, b' ' | b'\n' | b'\t')
            && bytes.get(i + 1).is_some_and(|x| {
                matches!(x, b' ' | b'\n' | b'\t')
                    || (*x == b'\r' && bytes.get(i + 2) == Some(&b'\n'))
            })
        {
            i += 1;
            continue;
        }
        if c == b'\n' {
            if !buffer.is_empty() && !buffer.ends_with([' ', '\n', '\t']) {
                buffer.push(' ');
            }
            i += 1;
            continue;
        }
        if buffer.is_empty() && c == b'-' && bytes.get(i + 1) == Some(&b'-') {
            start = i;
            let mut j = i + 2;
            let mut custom_stack = Vec::new();
            while j < bytes.len() {
                match bytes[j] {
                    b'\\' => {
                        j = next_char_end(input, j + 1);
                        continue;
                    }
                    b'\'' | b'"' => {
                        j = string_end(input, j, file)?;
                        continue;
                    }
                    b'/' if bytes.get(j + 1) == Some(&b'*') => {
                        j = comment_end(input, j, file)?;
                        continue;
                    }
                    b';' | b'}' if custom_stack.is_empty() => break,
                    b'(' => custom_stack.push(b')'),
                    b'[' => custom_stack.push(b']'),
                    b'{' => custom_stack.push(b'}'),
                    b')' | b']' | b'}' if custom_stack.last() == Some(&bytes[j]) => {
                        custom_stack.pop();
                    }
                    _ => {}
                }
                if custom_stack.len() > 256 {
                    return Err(error(file, start, j, "CSS nesting limit exceeded"));
                }
                j += 1;
            }
            if !custom_stack.is_empty() {
                return Err(error(file, start, j, "Unterminated custom property value"));
            }
            let node = declaration(&input[start..j], file, start, j)
                .map_err(|_| error(file, start, j, "Invalid custom property, expected a value"))?;
            append(node, &mut roots, &mut parents);
            i = j + usize::from(bytes.get(j) == Some(&b';'));
            continue;
        }
        if c == b';' && (buffer.starts_with('@') || brackets.last() != Some(&b')')) {
            if !buffer.is_empty() {
                let node = finish(&buffer, file, start, i)?;
                append(node, &mut roots, &mut parents);
                buffer.clear();
            }
            i += 1;
            continue;
        }
        if c == b'{' && brackets.last() != Some(&b')') {
            if parents.len() >= 256 {
                return Err(error(file, start, i, "CSS nesting limit exceeded"));
            }
            let mut node = if buffer.trim_start().starts_with('@') {
                parse_at_rule(buffer.trim(), Vec::new())
            } else {
                AstNode::new(NodeKind::Rule {
                    selector: buffer.trim().into(),
                    nodes: Vec::new(),
                })
            };
            node.src = Some(loc(file, start, i));
            parents.push(node);
            brackets.push(b'}');
            buffer.clear();
            i += 1;
            continue;
        }
        if c == b'}' && brackets.last() != Some(&b')') {
            if brackets.pop() != Some(b'}') {
                return Err(error(file, i, i, "Missing opening {"));
            }
            if !buffer.is_empty() {
                let node = finish(&buffer, file, start, i)?;
                append(node, &mut roots, &mut parents);
                buffer.clear();
            }
            let node = parents
                .pop()
                .ok_or_else(|| error(file, i, i, "Missing opening {"))?;
            append(node, &mut roots, &mut parents);
            i += 1;
            continue;
        }
        if c == b'(' {
            brackets.push(b')');
            if brackets.len() > 512 {
                return Err(error(file, start, i, "CSS nesting limit exceeded"));
            }
        }
        if c == b')' && brackets.pop() != Some(b')') {
            return Err(error(file, i, i, "Missing opening ("));
        }
        if buffer.is_empty() && matches!(c, b' ' | b'\t') {
            i += 1;
            continue;
        }
        if buffer.is_empty() {
            start = i;
        }
        let end = next_char_end(input, i);
        buffer.push_str(&input[i..end]);
        i = end;
    }
    if let Some(parent) = parents.last() {
        let name = match &parent.kind {
            NodeKind::Rule { selector, .. } => selector.clone(),
            NodeKind::AtRule { name, params, .. } => format!("{name} {params}"),
            _ => unreachable!(),
        };
        let pos = parent.src.as_ref().map_or(0, |x| x.start);
        return Err(error(
            file,
            pos,
            pos,
            &format!("Missing closing }} at {name}"),
        ));
    }
    if !brackets.is_empty() {
        return Err(error(file, start, input.len(), "Missing closing )"));
    }
    if buffer.starts_with('@') {
        roots.push(finish(&buffer, file, start, input.len())?);
    }
    // Upstream ignores a trailing top-level declaration without a semicolon.
    license.extend(roots);
    Ok(license)
}
fn append(node: AstNode, roots: &mut Vec<AstNode>, parents: &mut [AstNode]) {
    if let Some(p) = parents.last_mut() {
        p.children_mut().expect("rule parent").push(node);
    } else {
        roots.push(node);
    }
}
fn finish(buffer: &str, file: &str, start: usize, end: usize) -> Result<AstNode, CssSyntaxError> {
    if buffer.starts_with('@') {
        let mut n = parse_at_rule(buffer, Vec::new());
        n.src = Some(loc(file, start, end));
        Ok(n)
    } else {
        declaration(buffer, file, start, end)
    }
}
fn declaration(
    buffer: &str,
    file: &str,
    start: usize,
    end: usize,
) -> Result<AstNode, CssSyntaxError> {
    let colon = buffer.find(':').ok_or_else(|| {
        error(
            file,
            start,
            end,
            &format!("Invalid declaration: `{}`", buffer.trim()),
        )
    })?;
    let important = buffer[colon + 1..]
        .find("!important")
        .map(|x| x + colon + 1);
    let mut n = AstNode::new(NodeKind::Declaration {
        property: buffer[..colon].trim().into(),
        value: Some(
            buffer[colon + 1..important.unwrap_or(buffer.len())]
                .trim()
                .into(),
        ),
        important: important.is_some(),
    });
    n.src = Some(loc(file, start, end));
    Ok(n)
}
fn loc(file: &str, start: usize, end: usize) -> SourceLocation {
    SourceLocation {
        file: file.into(),
        start,
        end,
    }
}
fn error(file: &str, start: usize, end: usize, message: &str) -> CssSyntaxError {
    CssSyntaxError {
        message: message.into(),
        location: loc(file, start, end),
    }
}
fn next_char_end(s: &str, i: usize) -> usize {
    s.get(i..)
        .and_then(|s| s.chars().next())
        .map_or(s.len(), |c| i + c.len_utf8())
}
fn comment_end(input: &str, start: usize, file: &str) -> Result<usize, CssSyntaxError> {
    let b = input.as_bytes();
    let mut i = start + 2;
    while i < b.len() {
        if b[i] == b'\\' {
            i = next_char_end(input, i + 1);
            continue;
        }
        if b[i] == b'*' && b.get(i + 1) == Some(&b'/') {
            return Ok(i + 2);
        }
        i += 1;
    }
    Err(error(file, start, input.len(), "Unterminated comment"))
}
fn string_end(input: &str, start: usize, file: &str) -> Result<usize, CssSyntaxError> {
    let b = input.as_bytes();
    let mut i = start + 1;
    while i < b.len() {
        if b[i] == b'\\' {
            i = next_char_end(input, i + 1);
            continue;
        }
        if b[i] == b[start] {
            return Ok(i + 1);
        }
        if b[i] == b'\n' || (b[i] == b'\r' && b.get(i + 1) == Some(&b'\n')) {
            return Err(error(file, start, i, "Unterminated string"));
        }
        i += 1;
    }
    Err(error(file, start, input.len(), "Unterminated string"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn nested_custom_values_and_license() {
        let mut ast=parse("/*x*/ .a { --data: { a: [1;2] }; color: red!important; @media (width > 10px) { &:hover { color: blue } } } /*!keep*/","a.css").unwrap();
        assert!(matches!(&ast[0].kind,NodeKind::Comment{value} if value=="!keep"));
        let css = crate::ast::to_css(&mut ast, "out.css").unwrap();
        assert!(css.contains("--data: { a: [1;2] };"));
        assert!(css.contains("color: red !important;"));
        assert!(css.contains("&:hover {"));
    }
    #[test]
    fn source_bytes_unicode_crlf() {
        let ast = parse("\u{feff}.é {\r\n  content: '😀';\r\n}", "unicode.css").unwrap();
        let child = &ast[0].children().unwrap()[0];
        assert_eq!(child.src.as_ref().unwrap().start, 12);
        assert_eq!(child.src.as_ref().unwrap().end, 27);
    }
    #[test]
    fn errors_and_at_rules() {
        for css in [
            "}",
            "a { color:red",
            "a { bad; }",
            "a { content:'bad\n'; }",
            "/* unfinished",
        ] {
            assert!(parse(css, "bad.css").is_err(), "{css}");
        }
        let mut ast = parse("@import 'x'; @media(width>1px) {} @tailwind utilities", "x").unwrap();
        assert_eq!(
            crate::ast::to_css(&mut ast, "out").unwrap(),
            "@import 'x';\n@media (width>1px);\n@tailwind utilities;\n"
        );
    }
}
