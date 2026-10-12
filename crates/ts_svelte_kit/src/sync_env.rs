//! Environment-name discovery for declaration generation. Values are neither
//! returned nor recorded. Runtime value loading is a separate build concern.
use std::collections::BTreeSet;
use std::path::Path;

pub fn names_in_dotenv(source: &str) -> BTreeSet<String> {
    let b = source.as_bytes();
    let mut i = 0;
    let mut names = BTreeSet::new();
    while i < b.len() {
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
        }
        if b.get(i) == Some(&b'#') {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if source[i..].starts_with("export") && b.get(i + 6).is_some_and(u8::is_ascii_whitespace) {
            i += 6;
            while b.get(i).is_some_and(|c| *c == b' ' || *c == b'\t') {
                i += 1;
            }
        }
        let start = i;
        while b
            .get(i)
            .is_some_and(|c| c.is_ascii_alphanumeric() || matches!(*c, b'_' | b'.' | b'-'))
        {
            i += 1;
        }
        let end = i;
        while b.get(i).is_some_and(|c| *c == b' ' || *c == b'\t') {
            i += 1;
        }
        let separator = b.get(i).copied();
        let valid = end > start
            && (separator == Some(b'=')
                || (separator == Some(b':') && b.get(i + 1).is_some_and(u8::is_ascii_whitespace)));
        if !valid {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        names.insert(source[start..end].into());
        i += 1;
        while b.get(i).is_some_and(|c| *c == b' ' || *c == b'\t') {
            i += 1;
        }
        if let Some(quote @ (b'\'' | b'"' | b'`')) = b.get(i).copied() {
            i += 1;
            while i < b.len() {
                if b[i] == b'\\' && i + 1 < b.len() {
                    i += 2;
                } else if b[i] == quote {
                    i += 1;
                    break;
                } else {
                    i += 1;
                }
            }
        }
        while i < b.len() && b[i] != b'\n' {
            i += 1;
        }
    }
    names
}

pub fn discover_names(root: &Path, directory: &str, mode: &str) -> Result<Vec<String>, String> {
    if mode.is_empty()
        || mode == "local"
        || mode.contains('/')
        || mode.contains('\\')
        || mode.chars().any(char::is_control)
    {
        return Err("Invalid environment mode".into());
    }
    let mut names = std::env::vars_os()
        .filter_map(|(name, _)| name.into_string().ok())
        .collect::<BTreeSet<_>>();
    let root = root.canonicalize().map_err(|e| e.to_string())?;
    for name in [
        ".env".to_owned(),
        ".env.local".to_owned(),
        format!(".env.{mode}"),
        format!(".env.{mode}.local"),
    ] {
        let path = root.join(directory).join(name);
        if !path.exists() {
            continue;
        }
        if !path
            .canonicalize()
            .map_err(|e| e.to_string())?
            .starts_with(&root)
        {
            return Err("Environment file escapes project root".into());
        }
        if path.metadata().map_err(|e| e.to_string())?.len() > 2 * 1024 * 1024 {
            return Err("Environment file exceeds 2 MiB".into());
        }
        let source = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
        names.extend(names_in_dotenv(&source));
    }
    Ok(names.into_iter().collect())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn multiline_values_never_become_declarations() {
        let text = "export PUBLIC_NAME='hello'\nSECRET=\"first\nFAKE=inside\nlast\"\nANOTHER=`x\nNOT_A_KEY=z`\nEMPTY=\n#COMMENT=no\nALIAS: ok\n";
        assert_eq!(
            names_in_dotenv(text),
            ["PUBLIC_NAME", "SECRET", "ANOTHER", "EMPTY", "ALIAS"]
                .map(str::to_owned)
                .into_iter()
                .collect()
        );
    }
    #[test]
    fn invalid_lines_and_escaped_quotes() {
        assert_eq!(
            names_in_dotenv("BAD LINE=x\nREAL=\"hello\\\"\nSTILL_VALUE=yes\"\nNEXT='ok'\n"),
            ["REAL", "NEXT"].map(str::to_owned).into_iter().collect()
        );
    }
}
