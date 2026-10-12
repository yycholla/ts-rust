//! Host-resolved inputs. These types never read files, execute plugins, or infer
//! package versions from an ambient installation.
use crate::{TAILWIND_VERSION, ast::AstNode, css};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StylesheetSnapshot {
    pub id: String,
    pub version: i64,
    pub css: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SourceSnapshot {
    pub id: String,
    pub version: i64,
    pub extension: String,
    pub text: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResolvedImport {
    pub from: String,
    pub specifier: String,
    pub target: String,
}
/// Native implementation identity, not JavaScript source or an executable module.
/// The future directive engine must look this up in its native plugin registry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PluginRegistration {
    pub specifier: String,
    pub implementation: String,
    pub version: String,
    #[serde(default)]
    pub options: serde_json::Value,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProjectSnapshot {
    pub tailwind_version: String,
    pub entry: String,
    pub stylesheets: Vec<StylesheetSnapshot>,
    #[serde(default)]
    pub sources: Vec<SourceSnapshot>,
    #[serde(default)]
    pub imports: Vec<ResolvedImport>,
    #[serde(default)]
    pub plugins: Vec<PluginRegistration>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ParsedStylesheet {
    pub id: String,
    pub version: i64,
    pub ast: Vec<AstNode>,
}

impl ProjectSnapshot {
    pub fn validate(&self) -> Result<(), String> {
        if self.tailwind_version != TAILWIND_VERSION {
            return Err(format!(
                "Unsupported Tailwind version {}; this foundation is pinned to {TAILWIND_VERSION}",
                self.tailwind_version
            ));
        }
        if self.stylesheets.len() > 4096
            || self.sources.len() > 100_000
            || self.imports.len() > 16_384
            || self.plugins.len() > 256
        {
            return Err("Snapshot item limit exceeded".into());
        }
        let mut total = 0usize;
        let mut ids = BTreeSet::new();
        for sheet in &self.stylesheets {
            validate_id(&sheet.id)?;
            if !ids.insert(sheet.id.as_str()) {
                return Err(format!("Duplicate stylesheet {}", sheet.id));
            }
            total = total
                .checked_add(sheet.css.len())
                .ok_or("Snapshot size overflow")?;
        }
        if !ids.contains(self.entry.as_str()) {
            return Err("Entry stylesheet is absent from the snapshot".into());
        }
        let mut source_ids = BTreeSet::new();
        for source in &self.sources {
            validate_id(&source.id)?;
            if !source_ids.insert(source.id.as_str()) {
                return Err(format!("Duplicate source {}", source.id));
            }
            total = total
                .checked_add(source.text.len())
                .ok_or("Snapshot size overflow")?;
        }
        if total > 64 * 1024 * 1024 {
            return Err("Snapshot content exceeds 64 MiB".into());
        }
        let mut imports = BTreeSet::new();
        for import in &self.imports {
            if !ids.contains(import.from.as_str()) || !ids.contains(import.target.as_str()) {
                return Err("Resolved CSS import refers to an absent stylesheet".into());
            }
            if import.specifier.is_empty() || !imports.insert((&import.from, &import.specifier)) {
                return Err("Empty or duplicate CSS import resolution".into());
            }
        }
        let mut plugins = BTreeSet::new();
        for plugin in &self.plugins {
            if plugin.specifier.is_empty()
                || plugin.implementation.is_empty()
                || plugin.version.is_empty()
                || !plugins.insert(&plugin.specifier)
            {
                return Err("Invalid or duplicate native plugin registration".into());
            }
            if !plugin.options.is_null() && !plugin.options.is_object() {
                return Err("Native plugin options must be an object or null".into());
            }
        }
        Ok(())
    }
    /// Parses supplied stylesheets only. Imports and plugin registrations remain
    /// explicit descriptors; no directive expansion or utility compilation occurs.
    pub fn parse_stylesheets(&self) -> Result<Vec<ParsedStylesheet>, String> {
        self.validate()?;
        self.stylesheets
            .iter()
            .map(|s| {
                Ok(ParsedStylesheet {
                    id: s.id.clone(),
                    version: s.version,
                    ast: css::parse(&s.css, &s.id).map_err(|e| e.to_string())?,
                })
            })
            .collect()
    }
    pub fn resolve_import(&self, from: &str, specifier: &str) -> Option<&StylesheetSnapshot> {
        let target = &self
            .imports
            .iter()
            .find(|i| i.from == from && i.specifier == specifier)?
            .target;
        self.stylesheets.iter().find(|s| &s.id == target)
    }
}
fn validate_id(id: &str) -> Result<(), String> {
    if id.is_empty() || id.len() > 16 * 1024 || id.contains('\0') {
        return Err("Invalid snapshot identifier".into());
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Position {
    pub line: usize,
    pub character: usize,
}
/// Convert an exact Rust source byte boundary to a zero-based UTF-16 position.
pub fn position(text: &str, byte: usize) -> Result<Position, String> {
    if !text.is_char_boundary(byte) {
        return Err("Source offset is outside the text or splits UTF-8".into());
    }
    let prefix = &text[..byte];
    let line = prefix.bytes().filter(|b| *b == b'\n').count();
    let line_start = prefix.rfind('\n').map_or(0, |i| i + 1);
    Ok(Position {
        line,
        character: prefix[line_start..].encode_utf16().count(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn snapshots_are_resolved_and_versioned() {
        let mut snapshot = ProjectSnapshot {
            tailwind_version: TAILWIND_VERSION.into(),
            entry: "app.css".into(),
            stylesheets: vec![
                StylesheetSnapshot {
                    id: "app.css".into(),
                    version: -9,
                    css: "@import 'theme';".into(),
                },
                StylesheetSnapshot {
                    id: "theme.css".into(),
                    version: 4,
                    css: "@theme { --color-red: red; }".into(),
                },
            ],
            sources: Vec::new(),
            imports: vec![ResolvedImport {
                from: "app.css".into(),
                specifier: "theme".into(),
                target: "theme.css".into(),
            }],
            plugins: Vec::new(),
        };
        assert_eq!(snapshot.parse_stylesheets().unwrap()[0].version, -9);
        assert_eq!(
            snapshot.resolve_import("app.css", "theme").unwrap().id,
            "theme.css"
        );
        snapshot.imports[0].target = "missing.css".into();
        assert!(snapshot.validate().is_err());
    }
    #[test]
    fn utf16_boundary_conversion() {
        let text = "😀é\r\n.b";
        assert_eq!(
            position(text, 6).unwrap(),
            Position {
                line: 0,
                character: 3
            }
        );
        assert_eq!(
            position(text, 9).unwrap(),
            Position {
                line: 1,
                character: 1
            }
        );
        assert!(position(text, 1).is_err());
        assert!(position(text, 100).is_err());
    }
}
