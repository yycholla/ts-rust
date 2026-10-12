//! Vite 7.3.1 `loadEnv` (MIT, Vite contributors) as SvelteKit's `get_env` calls
//! it: `loadEnv(mode, kit.env.dir, '')` from the `pre` config hook of a build.
//! Bundled dotenv 16 `parse` and patched dotenv-expand 12.0.3 `expand` are
//! ported with their original ECMAScript regular expressions (regress), JS
//! `String.prototype.replace` substitution patterns and object key order.
//!
//! Only the four mode files in the env directory are read. The process
//! environment is an explicit input; this module never reads the real one.
//! Like Vite, loading mutates that input (`VITE_USER_NODE_ENV`, `BROWSER`,
//! `BROWSER_ARGS`), and every process key overrides file values when the
//! prefix list contains `''`.
use regress::Regex;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// dotenv 16 `LINE`, flags `gm`.
const LINE: &str = r#"(?:^|^)\s*(?:export\s+)?([\w.-]+)(?:\s*=\s*?|:\s+?)(\s*'(?:\\'|[^'])*'|\s*"(?:\\"|[^"])*"|\s*`(?:\\`|[^`])*`|[^#\r\n]+)?\s*(?:#.*)?(?:$|$)"#;
/// dotenv 16 surrounding-quote removal, flags `gm`, replaced with `$2`.
const QUOTES: &str = r#"^(['"`])([\s\S]*)\1$"#;
/// dotenv-expand 12.0.3 interpolation, flag `g`.
const INTERPOLATION: &str = r"(?<!\\)\${([^{}]+)}|(?<!\\)\$([A-Za-z_][A-Za-z0-9_]*)";
/// Bounds the expansion loop. dotenv-expand can loop forever on self-referential
/// defaults, or grow until V8 throws `RangeError: Invalid string length` when a
/// value reinserts its own template (e.g. through `$&`). Both fail here instead.
const MAX_EXPANSION_STEPS: usize = 100_000;
const MAX_EXPANDED_BYTES: usize = 1 << 20;

fn regex(cell: &'static OnceLock<Regex>, pattern: &str, flags: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::with_flags(pattern, flags).expect("pinned dotenv pattern"))
}
fn line() -> &'static Regex {
    static CELL: OnceLock<Regex> = OnceLock::new();
    regex(&CELL, LINE, "gm")
}
fn quotes() -> &'static Regex {
    static CELL: OnceLock<Regex> = OnceLock::new();
    regex(&CELL, QUOTES, "gm")
}
fn interpolation() -> &'static Regex {
    static CELL: OnceLock<Regex> = OnceLock::new();
    regex(&CELL, INTERPOLATION, "g")
}

/// A JavaScript ordinary object restricted to string values: array-index keys
/// enumerate first in ascending numeric order, then other keys by insertion.
#[derive(Debug, Clone, Default)]
pub struct JsStringObject {
    entries: Vec<(String, String)>,
    index: HashMap<String, usize>,
}
impl JsStringObject {
    pub fn get(&self, key: &str) -> Option<&str> {
        self.index.get(key).map(|&i| self.entries[i].1.as_str())
    }
    pub fn set(&mut self, key: &str, value: String) {
        if let Some(&i) = self.index.get(key) {
            self.entries[i].1 = value;
        } else {
            self.index.insert(key.into(), self.entries.len());
            self.entries.push((key.into(), value));
        }
    }
    /// Keys in `for...in` / `Object.entries` order.
    pub fn keys(&self) -> Vec<String> {
        let mut indices: Vec<(u32, &str)> = vec![];
        let mut others = vec![];
        for (key, _) in &self.entries {
            match array_index(key) {
                Some(n) => indices.push((n, key)),
                None => others.push(key.as_str()),
            }
        }
        indices.sort_unstable_by_key(|&(n, _)| n);
        indices
            .into_iter()
            .map(|(_, key)| key)
            .chain(others)
            .map(str::to_owned)
            .collect()
    }
}
fn array_index(key: &str) -> Option<u32> {
    if key.is_empty() || (key.len() > 1 && key.starts_with('0')) {
        return None;
    }
    let n: u32 = key.parse().ok()?;
    (n != u32::MAX && n.to_string() == key).then_some(n)
}

/// ECMAScript WhiteSpace and LineTerminator, as removed by `String.prototype.trim`.
fn js_space(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n' | '\u{b}' | '\u{c}' | '\r' | ' ' | '\u{a0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200a}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202f}'
                | '\u{205f}'
                | '\u{3000}'
                | '\u{feff}'
    )
}

/// dotenv 16 `parse(src)` for an already decoded file.
pub fn parse(src: &str) -> JsStringObject {
    // lines.replace(/\r\n?/gm, '\n')
    let lines = src.replace("\r\n", "\n").replace('\r', "\n");
    let mut out = JsStringObject::default();
    let mut last = 0;
    // RegExp.prototype.exec with lastIndex; every match is nonempty.
    while let Some(found) = line().find_from(&lines, last).next() {
        last = found.end();
        let key = &lines[found.group(1).expect("LINE key group")];
        let value = found.group(2).map_or("", |range| &lines[range]);
        let value = value.trim_matches(js_space);
        let maybe_quote = value.chars().next();
        let mut value = replace_all_groups(quotes(), value);
        if maybe_quote == Some('"') {
            value = value.replace("\\n", "\n").replace("\\r", "\r");
        }
        out.set(key, value);
    }
    out
}

/// `text.replace(regex /g, '$2')` for the quote pattern: matches are nonempty.
fn replace_all_groups(regex: &Regex, text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for found in regex.find_iter(text) {
        out.push_str(&text[last..found.start()]);
        if let Some(inner) = found.group(2) {
            out.push_str(&text[inner]);
        }
        last = found.end();
    }
    out.push_str(&text[last..]);
    out
}

/// `subject.replace(pattern: string, replacement: string)`: replaces the first
/// occurrence and applies GetSubstitution without captures.
fn replace_first(subject: &str, pattern: &str, replacement: &str) -> String {
    let Some(position) = subject.find(pattern) else {
        return subject.into();
    };
    let tail = position + pattern.len();
    let mut out = String::with_capacity(subject.len() + replacement.len());
    out.push_str(&subject[..position]);
    let mut chars = replacement.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '$' {
            out.push(c);
            continue;
        }
        match chars.peek() {
            Some('$') => {
                chars.next();
                out.push('$');
            }
            Some('&') => {
                chars.next();
                out.push_str(pattern);
            }
            Some('`') => {
                chars.next();
                out.push_str(&subject[..position]);
            }
            Some('\'') => {
                chars.next();
                out.push_str(&subject[tail..]);
            }
            // `$n`, `$nn` and `$<` stay literal: a string pattern has no
            // captures and no named groups.
            _ => out.push('$'),
        }
    }
    out.push_str(&subject[tail..]);
    out
}

/// `expression.match(/(:\+|\+|:-|-)/)`: the leftmost operator, preferring the
/// two-character forms at the same position.
fn operator(expression: &str) -> Option<&'static str> {
    let bytes = expression.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        match b {
            b':' if bytes.get(i + 1) == Some(&b'+') => return Some(":+"),
            b':' if bytes.get(i + 1) == Some(&b'-') => return Some(":-"),
            b'+' => return Some("+"),
            b'-' => return Some("-"),
            _ => {}
        }
    }
    None
}

fn expand_value(
    value: &str,
    process_env: &BTreeMap<String, String>,
    running: &JsStringObject,
) -> Result<String, String> {
    // const env = { ...runningParsed, ...processEnv }, captured once per call.
    let lookup = |key: &str| -> Option<String> {
        process_env
            .get(key)
            .cloned()
            .or_else(|| running.get(key).map(str::to_owned))
    };
    let mut result = value.to_owned();
    let mut seen = std::collections::HashSet::new();
    let mut last = 0;
    for _ in 0..MAX_EXPANSION_STEPS {
        let Some(found) = interpolation().find_from(&result, last).next() else {
            return Ok(result);
        };
        seen.insert(result.clone());
        let template = result[found.range()].to_owned();
        let expression = found
            .group(1)
            .or_else(|| found.group(2))
            .map(|range| result[range].to_owned())
            .expect("one interpolation group matches");
        // split(null) and join(null) use the string "null".
        let splitter = operator(&expression).unwrap_or("null");
        let mut parts = expression.split(splitter);
        let key = parts.next().unwrap_or("").to_owned();
        let rest = parts.collect::<Vec<_>>().join(splitter);
        let truthy = |v: &Option<String>| v.as_deref().is_some_and(|s| !s.is_empty());
        let (default, current) = if matches!(splitter, ":+" | "+") {
            (
                if truthy(&lookup(&key)) {
                    rest
                } else {
                    String::new()
                },
                None,
            )
        } else {
            (rest, lookup(&key))
        };
        result = match current.filter(|v| !v.is_empty()) {
            Some(current) if seen.contains(&current) => replace_first(&result, &template, &default),
            Some(current) => replace_first(&result, &template, &current),
            None => replace_first(&result, &template, &default),
        };
        if running.get(&key) == Some(result.as_str()) {
            return Ok(result);
        }
        if result.len() > MAX_EXPANDED_BYTES {
            return Err(format!(
                "Environment variable expansion exceeded {MAX_EXPANDED_BYTES} bytes"
            ));
        }
        last = 0;
    }
    Err(format!(
        "Environment variable expansion did not terminate after {MAX_EXPANSION_STEPS} steps"
    ))
}

/// Patched dotenv-expand 12.0.3 `expand({ parsed, processEnv })`. Mutates both.
pub fn expand(
    parsed: &mut JsStringObject,
    process_env: &mut BTreeMap<String, String>,
) -> Result<(), String> {
    for key in parsed.keys() {
        let value = parsed.get(&key).unwrap_or_default().to_owned();
        let value = match process_env.get(&key) {
            Some(process) if !process.is_empty() && *process != value => process.clone(),
            _ => expand_value(&value, process_env, parsed)?,
        };
        parsed.set(&key, value.replace("\\$", "$"));
    }
    for key in parsed.keys() {
        process_env.insert(key.clone(), parsed.get(&key).unwrap_or_default().into());
    }
    Ok(())
}

pub fn env_files(mode: &str, env_dir: &Path) -> Vec<PathBuf> {
    [
        ".env".to_owned(),
        ".env.local".to_owned(),
        format!(".env.{mode}"),
        format!(".env.{mode}.local"),
    ]
    .into_iter()
    .map(|file| env_dir.join(file))
    .collect()
}

/// Vite `loadEnv(mode, envDir, prefixes)`. `process_env` is the process
/// environment at that moment; it receives the same mutations as Node's.
pub fn load_env(
    mode: &str,
    env_dir: &Path,
    prefixes: &[&str],
    process_env: &mut BTreeMap<String, String>,
) -> Result<BTreeMap<String, String>, String> {
    if mode == "local" {
        return Err("\"local\" cannot be used as a mode name because it conflicts with the .local postfix for .env files.".into());
    }
    let mut parsed = JsStringObject::default();
    for file in env_files(mode, env_dir) {
        // tryStatSync(filePath)?.isFile() follows symlinks.
        if !std::fs::metadata(&file).is_ok_and(|m| m.is_file()) {
            continue;
        }
        let bytes = std::fs::read(&file).map_err(|e| format!("{}: {e}", file.display()))?;
        // Buffer#toString(): WHATWG UTF-8 decoding with replacement.
        let entries = parse(&String::from_utf8_lossy(&bytes));
        for key in entries.keys() {
            parsed.set(&key, entries.get(&key).unwrap_or_default().into());
        }
    }
    for (name, target) in [
        ("NODE_ENV", "VITE_USER_NODE_ENV"),
        ("BROWSER", "BROWSER"),
        ("BROWSER_ARGS", "BROWSER_ARGS"),
    ] {
        if let Some(value) = parsed.get(name).filter(|v| !v.is_empty()) {
            if !process_env.contains_key(target) {
                process_env.insert(target.into(), value.into());
            }
        }
    }
    // expand() receives a copy: { ...process.env }.
    let mut copy = process_env.clone();
    expand(&mut parsed, &mut copy)?;
    let mut env = BTreeMap::new();
    for key in parsed.keys() {
        if prefixes.iter().any(|prefix| key.starts_with(prefix)) {
            env.insert(key.clone(), parsed.get(&key).unwrap_or_default().into());
        }
    }
    for (key, value) in process_env.iter() {
        if prefixes.iter().any(|prefix| key.starts_with(prefix)) {
            env.insert(key.clone(), value.clone());
        }
    }
    Ok(env)
}

/// SvelteKit 2/3 build: Vite's `resolveConfig` first defaults an unset or empty
/// `NODE_ENV` to `production`, then Kit's `pre` config hook calls
/// `loadEnv(mode, kit.env.dir, '')` relative to the project root.
pub fn kit_build_env(
    project_root: &Path,
    kit_env_dir: &str,
    mode: &str,
    process_env: &BTreeMap<String, String>,
) -> Result<BTreeMap<String, String>, String> {
    let mut process_env = process_env.clone();
    if process_env.get("NODE_ENV").is_none_or(String::is_empty) {
        process_env.insert("NODE_ENV".into(), "production".into());
    }
    load_env(
        mode,
        &project_root.join(kit_env_dir),
        &[""],
        &mut process_env,
    )
}

#[cfg(test)]
#[path = "vite_env_tests.rs"]
mod tests;
