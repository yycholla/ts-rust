//! JSON with TypeScript configuration comments and trailing commas.
pub fn parse(text: &str) -> Result<serde_json::Value, String> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut bytes = text.as_bytes().to_vec();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => {
                i += 1;
                while i < bytes.len() && bytes[i] != b'"' {
                    i += if bytes[i] == b'\\' { 2 } else { 1 };
                }
                i += 1;
            }
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                while i < bytes.len() && !matches!(bytes[i], b'\r' | b'\n') {
                    bytes[i] = b' ';
                    i += 1;
                }
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                bytes[i..i + 2].fill(b' ');
                i += 2;
                loop {
                    if i + 1 >= bytes.len() {
                        return Err("Unterminated JSONC comment".into());
                    }
                    if bytes[i] == b'*' && bytes[i + 1] == b'/' {
                        bytes[i..i + 2].fill(b' ');
                        i += 2;
                        break;
                    }
                    if !matches!(bytes[i], b'\r' | b'\n') {
                        bytes[i] = b' ';
                    }
                    i += 1;
                }
            }
            _ => i += 1,
        }
    }
    i = 0;
    while i < bytes.len() {
        if bytes[i] == b'"' {
            i += 1;
            while i < bytes.len() && bytes[i] != b'"' {
                i += if bytes[i] == b'\\' { 2 } else { 1 };
            }
        } else if bytes[i] == b',' {
            let next = bytes[i + 1..].iter().find(|c| !c.is_ascii_whitespace());
            if matches!(next, Some(b'}' | b']')) {
                bytes[i] = b' ';
            }
        }
        i += 1;
    }
    serde_json::from_slice(&bytes).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    #[test]
    fn comments_strings_unicode_and_trailing_commas() {
        let value =
            super::parse("\u{feff}{/*日本*/\"url\":\"https://x/*hi*/\",//c\n\"a\":[1,],}").unwrap();
        assert_eq!(value["url"], "https://x/*hi*/");
        assert_eq!(value["a"], serde_json::json!([1]));
        assert!(super::parse("{/*unterminated").is_err());
        assert!(super::parse("{\"a\":,}").is_err());
    }
}
