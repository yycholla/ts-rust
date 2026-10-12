//! Balanced token operations and arbitrary-value decoding from Tailwind 4.2.1.
use serde::{Deserialize, Serialize};

pub fn segment(input: &str, separator: char) -> Vec<&str> {
    let mut result = Vec::new();
    let mut stack = Vec::new();
    let mut quote = None;
    let mut escaped = false;
    let mut start = 0;
    for (i, c) in input.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if c == '\\' {
            escaped = true;
            continue;
        }
        if let Some(q) = quote {
            if c == q {
                quote = None;
            }
            continue;
        }
        if matches!(c, '\'' | '"') {
            quote = Some(c);
            continue;
        }
        if c == separator && stack.is_empty() {
            result.push(&input[start..i]);
            start = i + c.len_utf8();
            continue;
        }
        match c {
            '(' => stack.push(')'),
            '[' => stack.push(']'),
            '{' => stack.push('}'),
            ')' | ']' | '}' if stack.last() == Some(&c) => {
                stack.pop();
            }
            _ => {}
        }
    }
    result.push(&input[start..]);
    result
}

/// Matches the pinned candidate validator, including its intentionally permissive
/// treatment of unmatched opening parentheses. This is not a CSS validity check.
pub fn is_valid_arbitrary(input: &str) -> bool {
    let mut stack = Vec::new();
    let mut quote = None;
    let mut escaped = false;
    for c in input.chars() {
        if escaped {
            escaped = false;
            continue;
        }
        if c == '\\' {
            escaped = true;
            continue;
        }
        if let Some(q) = quote {
            if c == q {
                quote = None;
            }
            continue;
        }
        match c {
            '\'' | '"' => quote = Some(c),
            '(' => stack.push(')'),
            '[' => stack.push(']'),
            ')' | ']' | '}' => {
                if stack.is_empty() {
                    return false;
                }
                if stack.last() == Some(&c) {
                    stack.pop();
                }
            }
            ';' if stack.is_empty() => return false,
            _ => {}
        }
    }
    true
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum ValueNode {
    Word {
        value: String,
    },
    Separator {
        value: String,
    },
    Function {
        value: String,
        nodes: Vec<ValueNode>,
    },
}
impl ValueNode {
    fn value_mut(&mut self) -> &mut String {
        match self {
            Self::Word { value } | Self::Separator { value } | Self::Function { value, .. } => {
                value
            }
        }
    }
}

pub fn parse_value(input: &str) -> Result<Vec<ValueNode>, String> {
    let text = input.replace("\r\n", "\n");
    let mut cursor = 0;
    fn parse(s: &str, i: &mut usize, depth: usize) -> Result<Vec<ValueNode>, String> {
        if depth > 256 {
            return Err("Value nesting limit exceeded".into());
        }
        let mut out = Vec::new();
        let mut buffer = String::new();
        fn flush(out: &mut Vec<ValueNode>, buffer: &mut String) {
            if !buffer.is_empty() {
                out.push(ValueNode::Word {
                    value: std::mem::take(buffer),
                });
            }
        }
        while *i < s.len() {
            let c = s[*i..].chars().next().unwrap();
            *i += c.len_utf8();
            match c {
                '\\' => {
                    buffer.push(c);
                    if let Some(next) = s[*i..].chars().next() {
                        buffer.push(next);
                        *i += next.len_utf8();
                    }
                }
                '\'' | '"' => {
                    buffer.push(c);
                    let mut escaped = false;
                    let mut closed = false;
                    while *i < s.len() {
                        let next = s[*i..].chars().next().unwrap();
                        *i += next.len_utf8();
                        buffer.push(next);
                        if escaped {
                            escaped = false;
                        } else if next == '\\' {
                            escaped = true;
                        } else if next == c {
                            closed = true;
                            break;
                        }
                    }
                    if !closed {
                        return Err("Unterminated value string".into());
                    }
                }
                '(' => {
                    let name = std::mem::take(&mut buffer);
                    let nodes = parse(s, i, depth + 1)?;
                    out.push(ValueNode::Function { value: name, nodes });
                }
                ')' => {
                    if depth == 0 {
                        return Err("Unexpected closing parenthesis".into());
                    }
                    flush(&mut out, &mut buffer);
                    return Ok(out);
                }
                '/' => {
                    flush(&mut out, &mut buffer);
                    out.push(ValueNode::Word { value: "/".into() });
                }
                ':' | ',' | '=' | '>' | '<' | '\n' | ' ' | '\t' => {
                    flush(&mut out, &mut buffer);
                    let mut value = c.to_string();
                    while let Some(next) = s[*i..].chars().next() {
                        if !matches!(next, ':' | ',' | '=' | '>' | '<' | '\n' | ' ' | '\t') {
                            break;
                        }
                        value.push(next);
                        *i += next.len_utf8();
                    }
                    out.push(ValueNode::Separator { value });
                }
                _ => buffer.push(c),
            }
        }
        if depth != 0 {
            return Err("Unterminated value function".into());
        }
        flush(&mut out, &mut buffer);
        Ok(out)
    }
    parse(&text, &mut cursor, 0)
}
pub fn value_to_css(nodes: &[ValueNode]) -> String {
    let mut s = String::new();
    for n in nodes {
        match n {
            ValueNode::Word { value } | ValueNode::Separator { value } => s.push_str(value),
            ValueNode::Function { value, nodes } => {
                s.push_str(value);
                s.push('(');
                s.push_str(&value_to_css(nodes));
                s.push(')');
            }
        }
    }
    s
}
fn underscores(input: &str, keep: bool) -> String {
    let mut s = String::new();
    let mut it = input.chars().peekable();
    while let Some(c) = it.next() {
        if c == '\\' && it.peek() == Some(&'_') {
            it.next();
            s.push('_');
        } else if c == '_' && !keep {
            s.push(' ');
        } else {
            s.push(c);
        }
    }
    s
}
pub fn decode_arbitrary_value(input: &str) -> Result<String, String> {
    if !input.contains('(') {
        return Ok(underscores(input, false));
    }
    fn decode(nodes: &mut [ValueNode]) {
        for n in nodes {
            match n {
                ValueNode::Function { value, nodes } => {
                    let url = value == "url" || value.ends_with("_url");
                    let preserve = value == "var"
                        || value.ends_with("_var")
                        || value == "theme"
                        || value.ends_with("_theme");
                    *value = underscores(value, false);
                    if url {
                        continue;
                    }
                    for (i, n) in nodes.iter_mut().enumerate() {
                        if preserve && i == 0 && matches!(n, ValueNode::Word { .. }) {
                            let v = n.value_mut();
                            *v = underscores(v, true);
                        } else {
                            decode(std::slice::from_mut(n));
                        }
                    }
                }
                _ => {
                    let v = n.value_mut();
                    *v = underscores(v, false);
                }
            }
        }
    }
    let mut ast = parse_value(input)?;
    decode(&mut ast);
    Ok(space_math(&value_to_css(&ast)))
}
const MATH: &[&str] = &[
    "calc", "min", "max", "clamp", "mod", "rem", "sin", "cos", "tan", "asin", "acos", "atan",
    "atan2", "pow", "sqrt", "hypot", "log", "exp", "round",
];
pub fn space_math(input: &str) -> String {
    if !MATH.iter().any(|f| input.contains(f)) {
        return input.into();
    }
    let chars: Vec<char> = input.chars().collect();
    let mut result = String::new();
    let mut stack = Vec::new();
    let mut value_pos = None;
    let mut last_value = None;
    for (i, &c) in chars.iter().enumerate() {
        if c.is_ascii_digit() || (value_pos.is_some() && (c == '%' || c.is_ascii_alphabetic())) {
            value_pos = Some(i);
        } else {
            last_value = value_pos;
            value_pos = None;
        }
        let active = stack.last().copied().unwrap_or(false);
        if c == '(' {
            let mut start = i;
            while start > 0
                && (chars[start - 1].is_ascii_lowercase() || chars[start - 1].is_ascii_digit())
            {
                start -= 1;
            }
            let name: String = chars[start..i].iter().collect();
            stack.push(MATH.contains(&name.as_str()) || (active && name.is_empty()));
            result.push(c);
        } else if c == ')' {
            result.push(c);
            stack.pop();
        } else if c == ',' && active {
            result.push_str(", ");
        } else if c == ' ' && active && result.ends_with(' ') {
            continue;
        } else if matches!(c, '+' | '-' | '*' | '/') && active {
            let prevs: Vec<_> = result.trim_end().chars().rev().take(2).collect();
            let prev = prevs.first().copied().unwrap_or('\0');
            let prevprev = prevs.get(1).copied().unwrap_or('\0');
            let next = chars.get(i + 1).copied().unwrap_or('\0');
            if (matches!(prev, 'e' | 'E') && prevprev.is_ascii_digit())
                || matches!(prev, '+' | '-' | '*' | '/' | '(' | ',')
            {
                result.push(c);
            } else if i > 0 && chars[i - 1] == ' ' {
                result.push(c);
                result.push(' ');
            } else if prev.is_ascii_digit()
                || next.is_ascii_digit()
                || prev == ')'
                || matches!(next, '(' | '+' | '-' | '*' | '/')
                || last_value == i.checked_sub(1)
            {
                result.push(' ');
                result.push(c);
                result.push(' ');
            } else {
                result.push(c);
            }
        } else {
            result.push(c);
        }
    }
    result
}

pub fn escape_identifier(input: &str) -> String {
    if input == "-" {
        return "\\-".into();
    }
    let mut out = String::new();
    for (i, c) in input.chars().enumerate() {
        if c == '\0' {
            out.push('\u{fffd}');
        } else if (c as u32) <= 31
            || c == '\u{7f}'
            || (c.is_ascii_digit() && (i == 0 || (i == 1 && input.starts_with('-'))))
        {
            out.push_str(&format!("\\{:x} ", c as u32));
        } else if !c.is_ascii() || c.is_ascii_alphanumeric() || matches!(c, '-' | '_') {
            out.push(c);
        } else {
            out.push('\\');
            out.push(c);
        }
    }
    out
}
pub fn unescape_identifier(input: &str) -> String {
    let mut out = String::new();
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        let mut hex = String::new();
        while hex.len() < 6 && chars.peek().is_some_and(|c| c.is_ascii_hexdigit()) {
            hex.push(chars.next().unwrap());
        }
        if hex.is_empty() {
            if let Some(c) = chars.next() {
                out.push(c);
            } else {
                out.push('\\');
            }
        } else {
            if chars
                .peek()
                .is_some_and(|c| matches!(c, '\t' | '\n' | '\u{c}' | '\r' | ' '))
            {
                chars.next();
            }
            out.push(char::from_u32(u32::from_str_radix(&hex, 16).unwrap()).unwrap_or('\u{fffd}'));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn nested_segmentation() {
        assert_eq!(
            segment("hover:[&:is(a,b)]:bg-[url('a:b')]", ':'),
            ["hover", "[&:is(a,b)]", "bg-[url('a:b')]"]
        );
        assert_eq!(segment("a\\:b:c", ':'), ["a\\:b", "c"]);
    }
    #[test]
    fn arbitrary_decoding() {
        for (input, want) in [
            ("hello_world\\_x", "hello world_x"),
            ("url(/hello_world.png)", "url(/hello_world.png)"),
            ("var(--my_token,_red)", "var(--my_token, red)"),
            ("calc(100%-2rem)", "calc(100% - 2rem)"),
            ("calc(1e-3*2)", "calc(1e-3 * 2)"),
            ("min(1px,2px)", "min(1px, 2px)"),
        ] {
            assert_eq!(decode_arbitrary_value(input).unwrap(), want, "{input}");
        }
    }
    #[test]
    fn escapes_and_injection() {
        assert_eq!(escape_identifier("-1.😀"), "-\\31 \\.😀");
        assert_eq!(unescape_identifier("--color-1\\.5"), "--color-1.5");
        assert!(!is_valid_arbitrary("red;display:none"));
        assert!(!is_valid_arbitrary("&{color:red}"));
        assert!(is_valid_arbitrary("url('a;b')"));
    }
}
