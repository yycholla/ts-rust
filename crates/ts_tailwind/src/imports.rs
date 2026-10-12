//! Explicit-snapshot CSS import expansion, ported from Tailwind CSS v4.2.1
//! `packages/tailwindcss/src/at-import.ts` (MIT, copyright Tailwind Labs).
//! Parameter parsing derives from postcss-import (MIT, copyright Maxime Thirouin,
//! Jason Campbell & Kevin Mårtensson). Pinned sources live in reference/imports.
//!
//! `expand_imports` consumes only supplied sources, exact resolutions and bases.
//! It preserves source identities/ranges and returns the intermediate Context
//! tree expected by Tailwind's directive pass. In particular `theme(...)`,
//! `reference`, `source(...)`, and `prefix(...)` remain media parameters for that
//! pass; this stage neither interprets nor silently drops them. HTTP/data and
//! url(...) imports remain in the tree, exactly as in the upstream import pass.
//! Resolved-identity cycles fail immediately (upstream has only a depth guard).
use crate::{
    ast::{AstNode, ContextValue, NodeKind, SourceLocation},
    css,
    snapshot::ProjectSnapshot,
    value::{ValueNode, parse_value, value_to_css},
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ImportOptions {
    /// Host-resolved directories, keyed by opaque stylesheet identity. Required
    /// for the entry and every actually imported stylesheet; never inferred.
    pub bases: BTreeMap<String, String>,
    #[serde(default)]
    pub limits: ImportLimits,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct ImportLimits {
    pub depth: usize,
    pub expansions: usize,
    pub output_nodes: usize,
    pub output_text_bytes: usize,
}
impl Default for ImportLimits {
    fn default() -> Self {
        Self {
            depth: 101,
            expansions: 16_384,
            output_nodes: 1_000_000,
            output_text_bytes: 64 * 1024 * 1024,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportedStylesheet {
    pub id: String,
    pub version: i64,
    pub base: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportExpansion {
    pub from: String,
    pub specifier: String,
    pub target: String,
    pub source: Option<SourceLocation>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExpandedImports {
    pub ast: Vec<AstNode>,
    pub entry_base: String,
    /// Unique identities, in first depth-first encounter order. Repeated imports
    /// still expand repeatedly and appear individually in `imports`.
    pub imported_stylesheets: Vec<ImportedStylesheet>,
    pub imports: Vec<ImportExpansion>,
    /// True for parsed imports including remote/data imports; false for skipped
    /// url(...) or syntactically unrecognized import parameters.
    pub has_imports: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportError {
    pub message: String,
    pub source: Option<SourceLocation>,
    pub chain: Vec<String>,
}
impl std::fmt::Display for ImportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(source) = &self.source {
            write!(f, "{}:{}: ", source.file, source.start)?;
        }
        write!(f, "{}", self.message)?;
        if !self.chain.is_empty() {
            write!(f, " [{}]", self.chain.join(" -> "))?;
        }
        Ok(())
    }
}
impl std::error::Error for ImportError {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportParams {
    pub uri: String,
    pub layer: Option<String>,
    pub media: Option<String>,
    pub supports: Option<String>,
}
/// None means preserve the original rule, matching the upstream parser contract.
/// CSS string escapes intentionally remain unchanged for the exact host resolver.
pub fn parse_import_params(params: &str) -> Result<Option<ImportParams>, String> {
    let nodes = parse_value(params)?;
    let mut parsed = ImportParams {
        uri: String::new(),
        layer: None,
        media: None,
        supports: None,
    };
    for (i, node) in nodes.iter().enumerate() {
        let value = match node {
            ValueNode::Separator { .. } => continue,
            ValueNode::Word { value } => {
                if parsed.uri.is_empty() {
                    if !value.starts_with(['\'', '"']) || value.len() < 2 {
                        return Ok(None);
                    }
                    parsed.uri = value
                        .get(1..value.len() - 1)
                        .ok_or("Malformed quoted import URI ends inside a Unicode character")?
                        .into();
                    continue;
                }
                value
            }
            ValueNode::Function { value, .. } => {
                if value.eq_ignore_ascii_case("url") {
                    return Ok(None);
                }
                value
            }
        };
        if parsed.uri.is_empty() {
            return Ok(None);
        }
        if value.eq_ignore_ascii_case("layer") {
            // Upstream tests truthiness, so an empty anonymous layer may be
            // followed by a named layer. Preserve that distinction from Some.
            if parsed.layer.as_ref().is_some_and(|s| !s.is_empty()) {
                return Ok(None);
            }
            if parsed.supports.as_ref().is_some_and(|s| !s.is_empty()) {
                return Err("`layer(…)` in an `@import` should come before any other functions or conditions".into());
            }
            parsed.layer = Some(match node {
                ValueNode::Function { nodes, .. } => value_to_css(nodes),
                _ => String::new(),
            });
            continue;
        }
        if let ValueNode::Function { nodes, .. } = node {
            if value.eq_ignore_ascii_case("supports") {
                if parsed.supports.as_ref().is_some_and(|s| !s.is_empty()) {
                    return Ok(None);
                }
                parsed.supports = Some(value_to_css(nodes));
                continue;
            }
        }
        parsed.media = Some(value_to_css(&nodes[i..]));
        break;
    }
    Ok((!parsed.uri.is_empty()).then_some(parsed))
}

pub fn expand_imports(
    snapshot: &ProjectSnapshot,
    options: &ImportOptions,
) -> Result<ExpandedImports, ImportError> {
    let fail = |message: String| ImportError {
        message,
        source: None,
        chain: vec![],
    };
    snapshot.validate().map_err(fail)?;
    let limits = &options.limits;
    if limits.depth == 0
        || limits.depth > 101
        || limits.expansions == 0
        || limits.expansions > 16_384
        || limits.output_nodes == 0
        || limits.output_nodes > 1_000_000
        || limits.output_text_bytes == 0
        || limits.output_text_bytes > 64 * 1024 * 1024
    {
        return Err(fail(
            "Import limits must be positive and no larger than the default limits".into(),
        ));
    }
    for (id, base) in &options.bases {
        if base.is_empty() || base.len() > 16_384 || base.contains('\0') {
            return Err(fail(format!("Invalid explicit stylesheet base for {id}")));
        }
    }
    let entry_base = options.bases.get(&snapshot.entry).cloned().ok_or_else(|| {
        fail(format!(
            "Missing explicit stylesheet base for {}",
            snapshot.entry
        ))
    })?;
    let mut state = ExpansionState {
        snapshot,
        options,
        stack: vec![snapshot.entry.clone()],
        seen: BTreeSet::new(),
        output: ExpandedImports {
            ast: vec![],
            entry_base,
            imported_stylesheets: vec![],
            imports: vec![],
            has_imports: false,
        },
        nodes: 0,
        bytes: 0,
    };
    let entry = snapshot
        .stylesheets
        .iter()
        .find(|s| s.id == snapshot.entry)
        .expect("validated entry");
    let ast =
        css::parse(&entry.css, &entry.id).map_err(|e| state.error(e.message, Some(e.location)))?;
    state.output.ast = state.expand(ast, &entry.id, 0)?;
    Ok(state.output)
}
struct ExpansionState<'a> {
    snapshot: &'a ProjectSnapshot,
    options: &'a ImportOptions,
    stack: Vec<String>,
    seen: BTreeSet<String>,
    output: ExpandedImports,
    nodes: usize,
    bytes: usize,
}
impl ExpansionState<'_> {
    fn error(&self, message: String, source: Option<SourceLocation>) -> ImportError {
        ImportError {
            message,
            source,
            chain: self.stack.clone(),
        }
    }
    fn charge(&mut self, node: &AstNode) -> Result<(), ImportError> {
        self.nodes += 1;
        self.bytes += match &node.kind {
            NodeKind::Rule { selector, .. } => selector.len(),
            NodeKind::AtRule { name, params, .. } => name.len() + params.len(),
            NodeKind::Declaration {
                property, value, ..
            } => property.len() + value.as_ref().map_or(0, String::len),
            NodeKind::Comment { value } => value.len(),
            NodeKind::Context { context, .. } => context
                .iter()
                .map(|(key, value)| {
                    key.len()
                        + match value {
                            ContextValue::String(s) => s.len(),
                            ContextValue::Bool(_) => 1,
                        }
                })
                .sum(),
            NodeKind::AtRoot { .. } => 0,
        };
        if let Some(src) = &node.src {
            self.bytes += src.file.len();
        }
        if self.nodes > self.options.limits.output_nodes
            || self.bytes > self.options.limits.output_text_bytes
        {
            return Err(self.error(
                "Expanded CSS exceeds output bounds".into(),
                node.src.clone(),
            ));
        }
        Ok(())
    }
    fn expand(
        &mut self,
        ast: Vec<AstNode>,
        from: &str,
        depth: usize,
    ) -> Result<Vec<AstNode>, ImportError> {
        if depth > 256 {
            return Err(self.error("Expanded CSS nesting exceeds 256".into(), None));
        }
        let mut result = Vec::new();
        for mut node in ast {
            if let NodeKind::AtRule { name, params, .. } = &node.kind {
                if name == "@import" || name == "@reference" {
                    let parsed =
                        parse_import_params(params).map_err(|e| self.error(e, node.src.clone()))?;
                    if let Some(mut parsed) = parsed {
                        if name == "@reference" {
                            parsed.media = Some("reference".into());
                        }
                        self.output.has_imports = true;
                        if !parsed.uri.starts_with("data:")
                            && !parsed.uri.starts_with("http://")
                            && !parsed.uri.starts_with("https://")
                        {
                            if self.stack.len() > self.options.limits.depth
                                || self.output.imports.len() >= self.options.limits.expansions
                            {
                                return Err(self.error(
                                    "CSS import expansion limit exceeded".into(),
                                    node.src.clone(),
                                ));
                            }
                            let snapshot = self.snapshot;
                            let target =
                                snapshot.resolve_import(from, &parsed.uri).ok_or_else(|| {
                                    self.error(
                                        format!(
                                            "Missing CSS import resolution for {:?} from {from}",
                                            parsed.uri
                                        ),
                                        node.src.clone(),
                                    )
                                })?;
                            if self.stack.contains(&target.id) {
                                let mut error =
                                    self.error("Circular CSS import".into(), node.src.clone());
                                error.chain.push(target.id.clone());
                                return Err(error);
                            }
                            let base =
                                self.options.bases.get(&target.id).cloned().ok_or_else(|| {
                                    self.error(
                                        format!(
                                            "Missing explicit stylesheet base for {}",
                                            target.id
                                        ),
                                        node.src.clone(),
                                    )
                                })?;
                            self.output.imports.push(ImportExpansion {
                                from: from.into(),
                                specifier: parsed.uri,
                                target: target.id.clone(),
                                source: node.src.clone(),
                            });
                            if self.seen.insert(target.id.clone()) {
                                self.output.imported_stylesheets.push(ImportedStylesheet {
                                    id: target.id.clone(),
                                    version: target.version,
                                    base: base.clone(),
                                });
                            }
                            self.stack.push(target.id.clone());
                            let imported = css::parse(&target.css, &target.id)
                                .map_err(|e| self.error(e.message, Some(e.location)))?;
                            let added_depth = 2
                                + usize::from(parsed.layer.is_some())
                                + usize::from(parsed.media.is_some())
                                + usize::from(parsed.supports.is_some());
                            let imported =
                                self.expand(imported, &target.id, depth + added_depth)?;
                            self.stack.pop();
                            let context = AstNode::new(NodeKind::Context {
                                context: BTreeMap::from([(
                                    "base".into(),
                                    ContextValue::String(base),
                                )]),
                                nodes: imported,
                            });
                            self.charge(&context)?;
                            let mut children = vec![context];
                            for (name, params) in [
                                ("@layer", parsed.layer),
                                ("@media", parsed.media),
                                (
                                    "@supports",
                                    parsed.supports.map(|s| {
                                        if s.starts_with('(') {
                                            s
                                        } else {
                                            format!("({s})")
                                        }
                                    }),
                                ),
                            ] {
                                if let Some(params) = params {
                                    let mut wrapper = AstNode::new(NodeKind::AtRule {
                                        name: name.into(),
                                        params,
                                        nodes: children,
                                    });
                                    wrapper.src = node.src.clone();
                                    self.charge(&wrapper)?;
                                    children = vec![wrapper];
                                }
                            }
                            let context = AstNode::new(NodeKind::Context {
                                context: BTreeMap::new(),
                                nodes: children,
                            });
                            self.charge(&context)?;
                            result.push(context);
                            continue;
                        }
                    }
                }
            }
            self.charge(&node)?;
            if let Some(children) = node.children_mut() {
                *children = self.expand(std::mem::take(children), from, depth + 1)?;
            }
            result.push(node);
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        TAILWIND_VERSION,
        snapshot::{ResolvedImport, StylesheetSnapshot},
    };
    fn fixture(
        css: &[(&str, &str)],
        resolutions: &[(&str, &str, &str)],
    ) -> (ProjectSnapshot, ImportOptions) {
        let snapshot = ProjectSnapshot {
            tailwind_version: TAILWIND_VERSION.into(),
            entry: css[0].0.into(),
            stylesheets: css
                .iter()
                .enumerate()
                .map(|(i, (id, css))| StylesheetSnapshot {
                    id: (*id).into(),
                    version: i as i64,
                    css: (*css).into(),
                })
                .collect(),
            imports: resolutions
                .iter()
                .map(|(from, specifier, target)| ResolvedImport {
                    from: (*from).into(),
                    specifier: (*specifier).into(),
                    target: (*target).into(),
                })
                .collect(),
            sources: vec![],
            plugins: vec![],
        };
        let options = ImportOptions {
            bases: css
                .iter()
                .map(|(id, _)| ((*id).into(), format!("/resolved/{id}")))
                .collect(),
            limits: ImportLimits::default(),
        };
        (snapshot, options)
    }
    fn wrappers(ast: &[AstNode], out: &mut Vec<(String, String)>) {
        for node in ast {
            if let NodeKind::AtRule { name, params, .. } = &node.kind {
                out.push((name.clone(), params.clone()));
            }
            if let Some(children) = node.children() {
                wrappers(children, out);
            }
        }
    }
    #[test]
    fn nested_conditions_and_original_provenance() {
        let (snapshot, options) = fixture(
            &[
                (
                    "app",
                    "@import 'theme' layer(utilities) supports(display: grid) screen and (width > 1px);",
                ),
                ("theme", ".a { color: red; }"),
            ],
            &[("app", "theme", "theme")],
        );
        let output = expand_imports(&snapshot, &options).unwrap();
        let mut rules = vec![];
        wrappers(&output.ast, &mut rules);
        assert_eq!(
            rules,
            [
                ("@supports".into(), "(display: grid)".into()),
                ("@media".into(), "screen and (width > 1px)".into()),
                ("@layer".into(), "utilities".into())
            ]
        );
        let mut node = &output.ast[0];
        for _ in 0..4 {
            node = &node.children().unwrap()[0];
        }
        assert!(
            matches!(&node.kind, NodeKind::Context { context, .. } if context.get("base") == Some(&ContextValue::String("/resolved/theme".into())))
        );
        assert_eq!(
            node.children().unwrap()[0].src.as_ref().unwrap().file,
            "theme"
        );
        assert_eq!(output.imports[0].source.as_ref().unwrap().file, "app");
        assert_eq!(output.imported_stylesheets[0].version, 1);
    }
    #[test]
    fn reference_theme_and_other_directive_parameters_survive() {
        let (snapshot, options) = fixture(
            &[
                (
                    "app",
                    "@reference 'theme' layer(base) print; @import 'theme' theme(reference inline) prefix(tw) source(none);",
                ),
                ("theme", "@theme { --color-a: red; }"),
            ],
            &[("app", "theme", "theme")],
        );
        let output = expand_imports(&snapshot, &options).unwrap();
        let mut rules = vec![];
        wrappers(&output.ast, &mut rules);
        assert_eq!(rules[0], ("@media".into(), "reference".into()));
        assert_eq!(rules[1], ("@layer".into(), "base".into()));
        assert!(rules.contains(&(
            "@media".into(),
            "theme(reference inline) prefix(tw) source(none)".into()
        )));
        assert_eq!(output.imports.len(), 2);
        assert_eq!(output.imported_stylesheets.len(), 1);
    }
    #[test]
    fn remote_data_url_and_unrecognized_imports_remain_unchanged() {
        let source = "@import 'https://example.test/a.css'; @reference 'http://example.test/b.css'; @import 'data:text/css,.a{}'; @import URL('local.css'); @import unquoted;";
        let (snapshot, options) = fixture(&[("app", source)], &[]);
        let output = expand_imports(&snapshot, &options).unwrap();
        assert_eq!(output.ast, css::parse(source, "app").unwrap());
        assert!(output.has_imports);
        assert!(output.imports.is_empty());
        assert_eq!(parse_import_params("url(a.css)").unwrap(), None);
    }
    #[test]
    fn parameter_order_and_nested_tokens_follow_upstream() {
        assert!(parse_import_params("'x'😀").is_err());
        assert_eq!(
            parse_import_params("'😀.css'").unwrap().unwrap().uri,
            "😀.css"
        );
        assert!(
            parse_import_params("'x' supports(display: grid) layer(foo)")
                .unwrap_err()
                .contains("before")
        );
        assert_eq!(parse_import_params("'x' layer(a) layer(b)").unwrap(), None);
        assert_eq!(
            parse_import_params("'x' supports(a) supports(b)").unwrap(),
            None
        );
        let parsed = parse_import_params("'x' LaYeR SUPPORTS((font-tech(color-COLRv1))) print")
            .unwrap()
            .unwrap();
        assert_eq!(parsed.layer.as_deref(), Some(""));
        assert_eq!(
            parsed.supports.as_deref(),
            Some("(font-tech(color-COLRv1))")
        );
        assert_eq!(parsed.media.as_deref(), Some("print"));
        assert_eq!(
            parse_import_params("'x' layer layer(named)")
                .unwrap()
                .unwrap()
                .layer
                .as_deref(),
            Some("named")
        );
    }
    #[test]
    fn imports_preserve_order_and_expand_repeated_edges() {
        let (snapshot, options) = fixture(
            &[
                ("app", "@import 'a'; @import 'a';"),
                ("a", "@import 'b'; .a { x: a; }"),
                ("b", ".b { x: b; }"),
            ],
            &[("app", "a", "a"), ("a", "b", "b")],
        );
        let output = expand_imports(&snapshot, &options).unwrap();
        assert_eq!(
            output
                .imports
                .iter()
                .map(|i| i.target.as_str())
                .collect::<Vec<_>>(),
            ["a", "b", "a", "b"]
        );
        assert_eq!(
            output
                .imported_stylesheets
                .iter()
                .map(|i| i.id.as_str())
                .collect::<Vec<_>>(),
            ["a", "b"]
        );
        let first = &output.ast[0].children().unwrap()[0].children().unwrap()[0];
        assert!(matches!(first.kind, NodeKind::Context { .. }));
    }
    #[test]
    fn missing_resolution_base_and_imported_syntax_have_locations() {
        let (mut snapshot, mut options) =
            fixture(&[("app", "@import 'x';"), ("x", ".a { color: red; }")], &[]);
        let error = expand_imports(&snapshot, &options).unwrap_err();
        assert!(error.message.contains("Missing CSS import resolution"));
        assert_eq!(error.source.unwrap().file, "app");
        snapshot.imports.push(ResolvedImport {
            from: "app".into(),
            specifier: "x".into(),
            target: "x".into(),
        });
        options.bases.remove("x");
        assert!(
            expand_imports(&snapshot, &options)
                .unwrap_err()
                .message
                .contains("base")
        );
        options.bases.insert("x".into(), "/x".into());
        snapshot.stylesheets[1].css = "a {".into();
        let error = expand_imports(&snapshot, &options).unwrap_err();
        assert_eq!(error.source.unwrap().file, "x");
        assert_eq!(error.chain, ["app", "x"]);
    }
    #[test]
    fn cycles_and_expansion_bounds_fail_without_partial_output() {
        let (snapshot, mut options) = fixture(
            &[("app", "@import 'a';"), ("a", "@import 'app';")],
            &[("app", "a", "a"), ("a", "app", "app")],
        );
        let error = expand_imports(&snapshot, &options).unwrap_err();
        assert_eq!(error.message, "Circular CSS import");
        assert_eq!(error.chain, ["app", "a", "app"]);
        options.limits.depth = 1;
        assert!(
            expand_imports(&snapshot, &options)
                .unwrap_err()
                .message
                .contains("limit")
        );
        let (snapshot, mut options) = fixture(
            &[
                ("app", "@import 'a'; @import 'a';"),
                ("a", ".a { color: red; }"),
            ],
            &[("app", "a", "a")],
        );
        options.limits.expansions = 1;
        assert!(
            expand_imports(&snapshot, &options)
                .unwrap_err()
                .message
                .contains("limit")
        );
        options.limits = ImportLimits::default();
        options.limits.output_nodes = 1;
        assert!(
            expand_imports(&snapshot, &options)
                .unwrap_err()
                .message
                .contains("bounds")
        );
    }
}
