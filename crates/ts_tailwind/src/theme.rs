//! Theme storage and resolution ported from Tailwind CSS v4.2.1, reference/theme.ts.
use crate::ast::{AstNode, SourceLocation};
use crate::value::{escape_identifier, unescape_identifier};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ThemeOptions(pub u8);
impl ThemeOptions {
    pub const NONE: Self = Self(0);
    pub const INLINE: Self = Self(1);
    pub const REFERENCE: Self = Self(2);
    pub const DEFAULT: Self = Self(4);
    pub const STATIC: Self = Self(8);
    pub const USED: Self = Self(16);
    pub fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }
}
impl std::ops::BitOr for ThemeOptions {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThemeValue {
    pub value: String,
    pub options: ThemeOptions,
    pub src: Option<SourceLocation>,
}
/// Vec preserves JavaScript Map insertion order (overwrites keep their slot).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Theme {
    pub prefix: Option<String>,
    values: Vec<(String, ThemeValue)>,
    keyframes: Vec<AstNode>,
}
fn ignored(namespace: &str) -> &'static [&'static str] {
    match namespace {
        "--font" => &["--font-weight", "--font-size"],
        "--inset" => &["--inset-shadow", "--inset-ring"],
        "--text" => &[
            "--text-color",
            "--text-decoration-color",
            "--text-decoration-thickness",
            "--text-indent",
            "--text-shadow",
            "--text-underline-offset",
        ],
        "--grid-column" => &["--grid-column-start", "--grid-column-end"],
        "--grid-row" => &["--grid-row-start", "--grid-row-end"],
        _ => &[],
    }
}
fn is_ignored(key: &str, namespace: &str) -> bool {
    ignored(namespace).iter().any(|s| {
        key == *s
            || key
                .strip_prefix(s)
                .is_some_and(|tail| tail.starts_with('-'))
    })
}
impl Theme {
    pub fn len(&self) -> usize {
        self.values.len()
    }
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }
    pub fn add(
        &mut self,
        key: &str,
        value: &str,
        options: ThemeOptions,
        src: Option<SourceLocation>,
    ) -> Result<(), String> {
        if !key.starts_with("--") {
            return Err(format!("Theme key must be a custom property: {key}"));
        }
        if options.0 & !31 != 0 {
            return Err("Unknown theme option bits".into());
        }
        if key.ends_with("-*") {
            if value != "initial" {
                return Err(format!(
                    "Invalid theme value `{value}` for namespace `{key}`"
                ));
            }
            if key == "--*" {
                self.values.clear();
            } else {
                self.clear_namespace(&key[..key.len() - 2], ThemeOptions::NONE);
            }
        }
        let index = self.values.iter().position(|(k, _)| k == key);
        if options.contains(ThemeOptions::DEFAULT)
            && index.is_some_and(|i| !self.values[i].1.options.contains(ThemeOptions::DEFAULT))
        {
            return Ok(());
        }
        if value == "initial" {
            if let Some(i) = index {
                self.values.remove(i);
            }
        } else {
            let entry = ThemeValue {
                value: value.into(),
                options,
                src,
            };
            if let Some(i) = index {
                self.values[i].1 = entry;
            } else {
                self.values.push((key.into(), entry));
            }
        }
        Ok(())
    }
    pub fn keys_in_namespaces(&self, namespaces: &[&str]) -> Vec<String> {
        let mut result = Vec::new();
        for ns in namespaces {
            let prefix = format!("{ns}-");
            for (key, _) in &self.values {
                if let Some(rest) = key.strip_prefix(&prefix) {
                    if !key[2..].contains("--") && !is_ignored(key, ns) {
                        result.push(rest.into());
                    }
                }
            }
        }
        result
    }
    pub fn get(&self, keys: &[&str]) -> Option<&str> {
        keys.iter()
            .find_map(|k| self.lookup(k).map(|v| v.value.as_str()))
    }
    fn lookup(&self, key: &str) -> Option<&ThemeValue> {
        self.values.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }
    pub fn options(&self, key: &str) -> ThemeOptions {
        self.lookup(&unescape_identifier(&self.unprefix_key(key)))
            .map_or(ThemeOptions::NONE, |v| v.options)
    }
    pub fn has_default(&self, key: &str) -> bool {
        self.options(key).contains(ThemeOptions::DEFAULT)
    }
    pub fn entries(&self) -> Vec<(String, &ThemeValue)> {
        self.values
            .iter()
            .map(|(k, v)| (self.prefix_key(k), v))
            .collect()
    }
    pub fn prefix_key(&self, key: &str) -> String {
        if let Some(prefix) = self.prefix.as_ref().filter(|p| !p.is_empty()) {
            format!("--{prefix}-{}", key.strip_prefix("--").unwrap_or(key))
        } else {
            key.into()
        }
    }
    fn unprefix_key(&self, key: &str) -> String {
        if let Some(prefix) = self.prefix.as_ref().filter(|p| !p.is_empty()) {
            let prefixed = format!("--{prefix}-");
            key.strip_prefix(&prefixed)
                .map_or_else(|| key.into(), |s| format!("--{s}"))
        } else {
            key.into()
        }
    }
    pub fn clear_namespace(&mut self, namespace: &str, options: ThemeOptions) {
        self.values.retain(|(key, value)| {
            !key.starts_with(namespace)
                || (options != ThemeOptions::NONE && !value.options.contains(options))
                || ignored(namespace).iter().any(|s| key.starts_with(s))
        });
    }
    pub fn resolve_key(&self, candidate: Option<&str>, namespaces: &[&str]) -> Option<String> {
        for ns in namespaces {
            let mut key = candidate.map_or_else(|| ns.to_string(), |v| format!("{ns}-{v}"));
            if self.lookup(&key).is_none() {
                if let Some(v) = candidate.filter(|s| s.contains('.')) {
                    key = format!("{ns}-{}", v.replace('.', "_"));
                    if self.lookup(&key).is_none() {
                        continue;
                    }
                } else {
                    continue;
                }
            }
            if !is_ignored(&key, ns) {
                return Some(key);
            }
        }
        None
    }
    fn variable(&self, key: &str) -> Option<String> {
        let v = self.lookup(key)?;
        let fallback = if v.options.contains(ThemeOptions::REFERENCE) && !v.value.is_empty() {
            format!(", {}", v.value)
        } else {
            String::new()
        };
        Some(format!(
            "var({}{fallback})",
            escape_identifier(&self.prefix_key(key))
        ))
    }
    pub fn mark_used_variable(&mut self, key: &str) -> bool {
        let key = unescape_identifier(&self.unprefix_key(key));
        if let Some((_, v)) = self.values.iter_mut().find(|(k, _)| k == &key) {
            let used = v.options.contains(ThemeOptions::USED);
            v.options = v.options | ThemeOptions::USED;
            !used
        } else {
            false
        }
    }
    pub fn resolve(
        &self,
        candidate: Option<&str>,
        namespaces: &[&str],
        options: ThemeOptions,
    ) -> Option<String> {
        let key = self.resolve_key(candidate, namespaces)?;
        let value = self.lookup(&key)?;
        if (options | value.options).contains(ThemeOptions::INLINE) {
            Some(value.value.clone())
        } else {
            self.variable(&key)
        }
    }
    pub fn resolve_value(&self, candidate: Option<&str>, namespaces: &[&str]) -> Option<&str> {
        let key = self.resolve_key(candidate, namespaces)?;
        Some(&self.lookup(&key)?.value)
    }
    pub fn resolve_with(
        &self,
        candidate: &str,
        namespaces: &[&str],
        nested_keys: &[&str],
    ) -> Option<(String, Vec<(String, String)>)> {
        let key = self.resolve_key(Some(candidate), namespaces)?;
        let mut extra = Vec::new();
        for name in nested_keys {
            let nested = format!("{key}{name}");
            if let Some(v) = self.lookup(&nested) {
                let value = if v.options.contains(ThemeOptions::INLINE) {
                    v.value.clone()
                } else {
                    self.variable(&nested)?
                };
                extra.push((name.to_string(), value));
            }
        }
        Some((
            self.resolve(Some(candidate), namespaces, ThemeOptions::NONE)?,
            extra,
        ))
    }
    pub fn namespace(&self, namespace: &str) -> Vec<(Option<String>, String)> {
        let prefix = format!("{namespace}-");
        self.values
            .iter()
            .filter_map(|(key, value)| {
                let name = if key == namespace {
                    None
                } else if key.starts_with(&format!("{prefix}-")) {
                    Some(key[namespace.len()..].into())
                } else if let Some(rest) = key.strip_prefix(&prefix) {
                    Some(rest.into())
                } else {
                    return None;
                };
                Some((name, value.value.clone()))
            })
            .collect()
    }
    /// Unlike JS object identity, Rust callers transfer ownership of each keyframe
    /// registration; repeated equal rules remain distinct registrations.
    pub fn add_keyframes(&mut self, value: AstNode) -> Result<(), String> {
        if !matches!(&value.kind,crate::ast::NodeKind::AtRule{name,..} if name=="@keyframes") {
            return Err("Expected @keyframes rule".into());
        }
        self.keyframes.push(value);
        Ok(())
    }
    pub fn keyframes(&self) -> &[AstNode] {
        &self.keyframes
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn defaults_resets_and_order() {
        let mut t = Theme::default();
        t.add("--color-red", "red", ThemeOptions::NONE, None)
            .unwrap();
        t.add("--color-red", "blue", ThemeOptions::DEFAULT, None)
            .unwrap();
        t.add("--color-blue", "blue", ThemeOptions::DEFAULT, None)
            .unwrap();
        assert_eq!(t.get(&["--color-red"]), Some("red"));
        assert_eq!(t.keys_in_namespaces(&["--color"]), ["red", "blue"]);
        t.add("--color-*", "initial", ThemeOptions::NONE, None)
            .unwrap();
        assert!(t.is_empty());
        assert!(t.add("--color-*", "red", ThemeOptions::NONE, None).is_err());
    }
    #[test]
    fn reference_prefix_inline_and_nested() {
        let mut t = Theme::default();
        t.prefix = Some("tw".into());
        t.add("--text-sm", ".875rem", ThemeOptions::REFERENCE, None)
            .unwrap();
        t.add(
            "--text-sm--line-height",
            "1.25rem",
            ThemeOptions::INLINE,
            None,
        )
        .unwrap();
        assert_eq!(
            t.resolve(Some("sm"), &["--text"], ThemeOptions::NONE)
                .unwrap(),
            "var(--tw-text-sm, .875rem)"
        );
        assert_eq!(
            t.resolve_with("sm", &["--text"], &["--line-height"])
                .unwrap()
                .1,
            [("--line-height".into(), "1.25rem".into())]
        );
        assert!(t.mark_used_variable("--tw-text-sm"));
        assert!(!t.mark_used_variable("--tw-text-sm"));
    }
    #[test]
    fn overlapping_namespaces_and_dots() {
        let mut t = Theme::default();
        for key in ["--font-sans", "--font-weight-bold", "--spacing-1_5"] {
            t.add(key, "x", ThemeOptions::NONE, None).unwrap();
        }
        t.add("--font-*", "initial", ThemeOptions::NONE, None)
            .unwrap();
        assert_eq!(t.get(&["--font-weight-bold"]), Some("x"));
        assert!(t.resolve_key(Some("weight-bold"), &["--font"]).is_none());
        assert_eq!(t.resolve_value(Some("1.5"), &["--spacing"]), Some("x"));
    }
}
