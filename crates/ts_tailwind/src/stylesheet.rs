//! Native Tailwind CSS 4.2.1 stylesheet orchestration from `index.ts` (MIT,
//! Tailwind Labs). Inputs are resolved snapshots; this module does not scan
//! files, execute JavaScript plugins, or provide color-mix compatibility lowering.
use crate::{
    apply::{ApplyOptions, substitute_at_apply},
    ast::{AstNode, ContextValue, NodeKind, SourceLocation, to_css},
    compile::{
        CompileOptions, RemainingPass, UnsupportedCandidate, compile_with_design_system, lower_ast,
    },
    custom_utility::UtilityWarning,
    design_system::DesignSystem,
    functions::{candidate_transform, substitute_functions},
    imports::{ImportExpansion, ImportOptions, ImportedStylesheet, expand_imports},
    snapshot::ProjectSnapshot,
    theme::{Theme, ThemeOptions},
    value::{escape_identifier, segment, unescape_identifier},
    variants::{CustomVariantBody, CustomVariantDefinition},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
#[path = "stylesheet_braces.rs"]
mod braces;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StylesheetOptions {
    pub imports: ImportOptions,
    #[serde(default = "enabled")]
    pub property_fallbacks: bool,
    #[serde(default = "enabled")]
    pub license_comment: bool,
}
fn enabled() -> bool {
    true
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum StylesheetErrorKind {
    Invalid,
    Unsupported,
    Import,
    Apply,
    Limit,
    Compilation,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StylesheetError {
    pub kind: StylesheetErrorKind,
    pub message: String,
    pub source: Option<SourceLocation>,
}
impl std::fmt::Display for StylesheetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for StylesheetError {}
fn fail(
    kind: StylesheetErrorKind,
    message: impl Into<String>,
    source: Option<SourceLocation>,
) -> StylesheetError {
    StylesheetError {
        kind,
        message: message.into(),
        source,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum SourceRoot {
    Auto,
    None,
    Pattern { base: String, pattern: String },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceDirective {
    pub base: String,
    pub pattern: String,
    pub negated: bool,
    pub source: Option<SourceLocation>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StylesheetMetadata {
    pub entry: String,
    pub entry_base: String,
    pub root: SourceRoot,
    pub sources: Vec<SourceDirective>,
    pub inline_candidates: Vec<String>,
    pub excluded_candidates: Vec<String>,
    pub imported_stylesheets: Vec<ImportedStylesheet>,
    pub imports: Vec<ImportExpansion>,
    pub has_utilities: bool,
    pub important: bool,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StylesheetBuild {
    pub css: String,
    pub ast: Vec<AstNode>,
    pub generated: Vec<String>,
    pub invalid: Vec<String>,
    pub unsupported: Vec<UnsupportedCandidate>,
    pub remaining_passes: Vec<RemainingPass>,
    pub metadata: StylesheetMetadata,
    pub warnings: Vec<UtilityWarning>,
    pub source_receipts: BTreeMap<String, StylesheetSourceReceipt>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StylesheetSourceReceipt {
    pub version: i64,
    pub sha256: String,
}
/// Immutable prepared directives and registrations. Every build uses exactly
/// its supplied candidate set (plus inline sources), so removal is supported.
#[derive(Clone)]
pub struct PreparedStylesheet {
    ast: Vec<AstNode>,
    system: DesignSystem,
    metadata: StylesheetMetadata,
    warnings: Vec<UtilityWarning>,
    property_fallbacks: bool,
    active: bool,
    source_receipts: BTreeMap<String, StylesheetSourceReceipt>,
}
impl PreparedStylesheet {
    pub fn metadata(&self) -> &StylesheetMetadata {
        &self.metadata
    }
    pub fn design_system(&self) -> &DesignSystem {
        &self.system
    }
    /// Filesystem hosts may replace automatic source discovery with the exact
    /// resolved working-directory root while keeping stylesheet import bases.
    pub fn with_source_root(&self, root: SourceRoot) -> Self {
        let mut prepared = self.clone();
        prepared.metadata.root = root;
        prepared
    }
    pub fn with_source_discovery(&self, root: SourceRoot, sources: Vec<SourceDirective>) -> Self {
        let mut prepared = self.with_source_root(root);
        prepared.metadata.sources = sources;
        prepared
    }
    pub fn build(&self, candidates: &[String]) -> Result<StylesheetBuild, StylesheetError> {
        if candidates.len() > 100_000
            || candidates.iter().map(String::len).sum::<usize>() > 16 * 1024 * 1024
        {
            return Err(fail(
                StylesheetErrorKind::Limit,
                "Stylesheet candidates exceed bounds",
                None,
            ));
        }
        let mut system = self.system.clone();
        let mut ast = self.ast.clone();
        let mut generated = Vec::new();
        let mut invalid = Vec::new();
        let mut unsupported = Vec::new();
        if self.metadata.has_utilities {
            let mut requested = self.metadata.inline_candidates.clone();
            for candidate in candidates {
                if candidate.starts_with("--") && !system.is_excluded(candidate) {
                    system.mark_used_variable(candidate);
                } else {
                    requested.push(candidate.clone());
                }
            }
            let compiled = compile_with_design_system(
                &requested,
                &system,
                &CompileOptions {
                    important: self.metadata.important,
                    respect_important: true,
                    property_fallbacks: false,
                },
            )
            .map_err(|message| fail(StylesheetErrorKind::Compilation, message, None))?;
            generated = compiled.generated;
            invalid = compiled.invalid;
            unsupported = compiled.unsupported;
            let rules = compiled
                .rules
                .into_iter()
                .map(|r| r.node)
                .collect::<Vec<_>>();
            fill_slot(&mut ast, UTILITIES_SLOT, &rules, true);
        }
        let remaining_passes;
        if self.active {
            let lowered = lower_ast(ast, system.theme(), self.property_fallbacks)
                .map_err(|message| fail(StylesheetErrorKind::Compilation, message, None))?;
            ast = lowered.ast;
            remaining_passes = lowered.remaining_passes;
        } else {
            remaining_passes = Vec::new();
        }
        let css = to_css(&mut ast, &self.metadata.entry)
            .map_err(|message| fail(StylesheetErrorKind::Compilation, message, None))?;
        Ok(StylesheetBuild {
            css,
            ast,
            generated,
            invalid,
            unsupported,
            remaining_passes,
            metadata: self.metadata.clone(),
            warnings: self.warnings.clone(),
            source_receipts: self.source_receipts.clone(),
        })
    }
}
const THEME_SLOT: &str = "tailwindThemeSlot";
const UTILITIES_SLOT: &str = "tailwindUtilitiesSlot";
fn context(key: &str, nodes: Vec<AstNode>) -> AstNode {
    AstNode::new(NodeKind::Context {
        context: BTreeMap::from([(key.into(), ContextValue::Bool(true))]),
        nodes,
    })
}
fn slot(key: &str, source: Option<SourceLocation>) -> AstNode {
    let mut n = context(key, Vec::new());
    n.src = source;
    n
}
fn fill_slot(nodes: &mut [AstNode], key: &str, replacement: &[AstNode], inherit_source: bool) {
    for node in nodes {
        if matches!(&node.kind, NodeKind::Context { context, .. } if context.contains_key(key)) {
            let mut added = replacement.to_vec();
            if inherit_source {
                for child in &mut added {
                    default_source(child, &node.src);
                }
            }
            *node.children_mut().unwrap() = added;
        } else if let Some(children) = node.children_mut() {
            fill_slot(children, key, replacement, inherit_source);
        }
    }
}
fn default_source(node: &mut AstNode, source: &Option<SourceLocation>) {
    if node.src.is_none() {
        node.src = source.clone();
    }
    if let Some(children) = node.children_mut() {
        for child in children {
            default_source(child, source);
        }
    }
}

pub fn prepare_stylesheet(
    snapshot: &ProjectSnapshot,
    options: &StylesheetOptions,
) -> Result<PreparedStylesheet, StylesheetError> {
    let expanded = expand_imports(snapshot, &options.imports)
        .map_err(|e| fail(StylesheetErrorKind::Import, e.message, e.source))?;
    let mut state = Collector {
        theme: Theme::default(),
        custom_variants: Vec::new(),
        plugins: Vec::new(),
        theme_slot: false,
        active: expanded.has_imports,
        metadata: StylesheetMetadata {
            entry: snapshot.entry.clone(),
            entry_base: expanded.entry_base.clone(),
            root: SourceRoot::Auto,
            sources: Vec::new(),
            inline_candidates: Vec::new(),
            excluded_candidates: Vec::new(),
            imported_stylesheets: expanded.imported_stylesheets,
            imports: expanded.imports,
            has_utilities: false,
            important: false,
        },
    };
    let env = Environment {
        base: expanded.entry_base,
        source_base: None,
        reference: false,
    };
    let mut ast = state.collect(expanded.ast, &env, 0, 0)?;
    let mut system = DesignSystem::new(state.theme);
    crate::plugins::register(&mut system, &state.plugins, &snapshot.plugins)
        .map_err(|(message, source)| fail(StylesheetErrorKind::Unsupported, message, source))?;
    system
        .variants_mut()
        .register_custom_variants(&state.custom_variants)
        .map_err(|e| fail(StylesheetErrorKind::Invalid, e.message, None))?;
    for candidate in &state.metadata.excluded_candidates {
        system.exclude(candidate.clone());
    }
    system.set_candidate_transform(candidate_transform);
    if state.theme_slot {
        let declarations = system
            .theme()
            .entries()
            .into_iter()
            .filter(|(_, v)| !v.options.contains(ThemeOptions::REFERENCE))
            .map(|(key, value)| {
                let mut node = AstNode::declaration(escape_identifier(&key), &value.value);
                node.src = value.src.clone();
                node
            })
            .collect();
        let rule = AstNode::new(NodeKind::Rule {
            selector: ":root, :host".into(),
            nodes: vec![context("theme", declarations)],
        });
        fill_slot(&mut ast, THEME_SLOT, &[rule], true);
        for keyframes in system.theme().keyframes() {
            ast.push(context(
                "theme",
                vec![AstNode::new(NodeKind::AtRoot {
                    nodes: vec![keyframes.clone()],
                })],
            ));
        }
    }
    ast = system
        .variants()
        .substitute_at_variants(&ast)
        .map_err(|e| fail(StylesheetErrorKind::Invalid, e.message, None))?;
    let functions = substitute_functions(&ast, system.theme())
        .map_err(|e| fail(StylesheetErrorKind::Compilation, e.message, e.source))?;
    state.active |= functions.has_theme_functions;
    ast = functions.ast;
    register_utilities(&ast, &mut system)?;
    let warnings = system
        .definitions()
        .flat_map(|(_, utility)| utility.warnings().iter().cloned())
        .collect();
    let applied = substitute_at_apply(
        &ast,
        &mut system,
        &ApplyOptions {
            sources: snapshot
                .stylesheets
                .iter()
                .map(|s| (s.id.clone(), s.css.clone()))
                .collect(),
        },
    )
    .map_err(|e| fail(StylesheetErrorKind::Apply, e.message, e.source))?;
    state.active |= applied.has_apply;
    ast = remove_utilities(applied.ast);
    if options.license_comment && state.active {
        ast.insert(
            0,
            AstNode::new(NodeKind::Comment {
                value: format!(
                    "! tailwindcss v{} | MIT License | https://tailwindcss.com ",
                    crate::TAILWIND_VERSION
                ),
            }),
        );
    }
    Ok(PreparedStylesheet {
        ast,
        system,
        metadata: state.metadata,
        warnings,
        property_fallbacks: options.property_fallbacks,
        active: state.active,
        source_receipts: snapshot
            .stylesheets
            .iter()
            .map(|sheet| {
                (
                    sheet.id.clone(),
                    StylesheetSourceReceipt {
                        version: sheet.version,
                        sha256: format!("{:x}", Sha256::digest(sheet.css.as_bytes())),
                    },
                )
            })
            .collect(),
    })
}
fn register_utilities(nodes: &[AstNode], system: &mut DesignSystem) -> Result<(), StylesheetError> {
    for node in nodes {
        if matches!(&node.kind, NodeKind::AtRule { name, .. } if name == "@utility") {
            system
                .register_utility(node.clone())
                .map_err(|message| fail(StylesheetErrorKind::Invalid, message, node.src.clone()))?;
        } else if let Some(children) = node.children() {
            register_utilities(children, system)?;
        }
    }
    Ok(())
}
fn remove_utilities(nodes: Vec<AstNode>) -> Vec<AstNode> {
    nodes
        .into_iter()
        .filter_map(|mut node| {
            if matches!(&node.kind, NodeKind::AtRule { name, .. } if name == "@utility") {
                return None;
            }
            if let Some(children) = node.children_mut() {
                *children = remove_utilities(std::mem::take(children));
            }
            Some(node)
        })
        .collect()
}
#[derive(Clone)]
struct Environment {
    base: String,
    source_base: Option<String>,
    reference: bool,
}
struct Collector {
    theme: Theme,
    custom_variants: Vec<CustomVariantDefinition>,
    plugins: Vec<crate::plugins::PluginDirective>,
    theme_slot: bool,
    active: bool,
    metadata: StylesheetMetadata,
}
impl Collector {
    fn collect(
        &mut self,
        nodes: Vec<AstNode>,
        env: &Environment,
        nesting: usize,
        depth: usize,
    ) -> Result<Vec<AstNode>, StylesheetError> {
        if depth > 256 {
            return Err(fail(
                StylesheetErrorKind::Limit,
                "Stylesheet directive nesting limit exceeded",
                None,
            ));
        }
        let mut output = Vec::new();
        for mut node in nodes {
            if let NodeKind::Context { context, nodes } = &mut node.kind {
                let mut env = env.clone();
                if let Some(ContextValue::String(base)) = context.get("base") {
                    env.base = base.clone();
                }
                if let Some(ContextValue::String(base)) = context.get("sourceBase") {
                    env.source_base = Some(base.clone());
                }
                if let Some(ContextValue::Bool(reference)) = context.get("reference") {
                    env.reference = *reference;
                }
                *nodes = self.collect(std::mem::take(nodes), &env, nesting, depth + 1)?;
                output.push(node);
                continue;
            }
            let source = node.src.clone();
            let invalid =
                |message: String| fail(StylesheetErrorKind::Invalid, message, source.clone());
            if let NodeKind::AtRule {
                name,
                params,
                nodes,
            } = &mut node.kind
            {
                if name == "@plugin" {
                    if nesting != 0 {
                        return Err(invalid("`@plugin` cannot be nested.".into()));
                    }
                    let specifier = quoted(params.trim())
                        .ok_or_else(|| invalid("`@plugin` paths must be quoted.".into()))?;
                    self.plugins.push(
                        crate::plugins::directive(specifier, nodes, source.clone())
                            .map_err(|message| invalid(message))?,
                    );
                    self.active = true;
                    continue;
                }
                if name == "@config" {
                    return Err(fail(
                        StylesheetErrorKind::Unsupported,
                        format!(
                            "{name} {params} requires a native plugin/configuration implementation; JavaScript execution is not available"
                        ),
                        source,
                    ));
                }
                if name == "@media" {
                    let mut unknown = Vec::new();
                    for parameter in segment(params, ' ') {
                        if let Some(value) = parameter
                            .strip_prefix("source(")
                            .and_then(|p| p.strip_suffix(')'))
                        {
                            media_source(nodes, value, &env.base);
                        } else if let Some(value) = parameter
                            .strip_prefix("theme(")
                            .and_then(|p| p.strip_suffix(')'))
                        {
                            media_theme(nodes, value, value.contains("reference"))?;
                        } else if parameter.starts_with("prefix(") && parameter.ends_with(')') {
                            media_theme(nodes, parameter, false)?;
                        } else if parameter == "important" {
                            self.metadata.important = true;
                            self.active = true;
                        } else if parameter == "reference" {
                            *nodes = vec![context("reference", std::mem::take(nodes))];
                            self.active = true;
                        } else if !parameter.is_empty() {
                            unknown.push(parameter.to_string());
                        }
                    }
                    if unknown.is_empty() && !params.is_empty() {
                        output.extend(self.collect(
                            std::mem::take(nodes),
                            env,
                            nesting,
                            depth + 1,
                        )?);
                        continue;
                    }
                    *params = unknown.join(" ");
                }
                if name == "@tailwind" && params.starts_with("utilities") {
                    self.active = true;
                    if self.metadata.has_utilities || env.reference {
                        continue;
                    }
                    for parameter in segment(params, ' ') {
                        if let Some(value) = parameter
                            .strip_prefix("source(")
                            .and_then(|p| p.strip_suffix(')'))
                        {
                            self.metadata.root = if value == "none" {
                                SourceRoot::None
                            } else {
                                SourceRoot::Pattern {
                                    base: env
                                        .source_base
                                        .clone()
                                        .unwrap_or_else(|| env.base.clone()),
                                    pattern: quoted(value)
                                        .ok_or_else(|| {
                                            invalid("`source(…)` paths must be quoted.".into())
                                        })?
                                        .into(),
                                }
                            };
                        }
                    }
                    self.metadata.has_utilities = true;
                    output.push(slot(UTILITIES_SLOT, source));
                    continue;
                }
                if name == "@tailwind" {
                    return Err(fail(
                        StylesheetErrorKind::Unsupported,
                        format!("Unsupported legacy @tailwind directive `{params}`"),
                        source,
                    ));
                }
                if name == "@source" {
                    self.active = true;
                    if nesting != 0 {
                        return Err(invalid("`@source` cannot be nested.".into()));
                    }
                    if !nodes.is_empty() {
                        return Err(invalid("`@source` cannot have a body.".into()));
                    }
                    let (negated, pattern) = params
                        .strip_prefix("not ")
                        .map_or((false, params.as_str()), |s| (true, s));
                    let inline = pattern
                        .strip_prefix("inline(")
                        .and_then(|p| p.strip_suffix(')'));
                    let pattern = quoted(inline.unwrap_or(pattern).trim())
                        .ok_or_else(|| invalid("`@source` paths must be quoted.".into()))?;
                    if inline.is_some() {
                        let destination = if negated {
                            &mut self.metadata.excluded_candidates
                        } else {
                            &mut self.metadata.inline_candidates
                        };
                        for part in segment(pattern, ' ') {
                            destination
                                .extend(braces::expand(part).map_err(|message| invalid(message))?);
                            if destination.len() > 100_000
                                || destination.iter().map(String::len).sum::<usize>()
                                    > 16 * 1024 * 1024
                            {
                                return Err(fail(
                                    StylesheetErrorKind::Limit,
                                    "Inline source candidates exceed bounds",
                                    source,
                                ));
                            }
                        }
                    } else {
                        self.metadata.sources.push(SourceDirective {
                            base: env.base.clone(),
                            pattern: pattern.into(),
                            negated,
                            source,
                        });
                    }
                    continue;
                }
                if name == "@variant"
                    && nesting == 0
                    && (nodes.is_empty() || contains_directive(nodes, "@slot"))
                {
                    *name = "@custom-variant".into();
                }
                if name == "@custom-variant" {
                    self.active = true;
                    if nesting != 0 {
                        return Err(invalid("`@custom-variant` cannot be nested.".into()));
                    }
                    let parts = segment(params, ' ');
                    let variant = parts.first().copied().unwrap_or("");
                    let selector = parts.get(1).copied();
                    if selector.is_some() && !nodes.is_empty() {
                        return Err(invalid(format!(
                            "`@custom-variant {variant}` cannot have both a selector and a body."
                        )));
                    }
                    let body = if nodes.is_empty() {
                        let selector = selector
                            .and_then(|s| s.strip_prefix('('))
                            .and_then(|s| s.strip_suffix(')'))
                            .ok_or_else(|| {
                                invalid(format!(
                                    "`@custom-variant {variant}` has no valid selector or body."
                                ))
                            })?;
                        CustomVariantBody::Selectors(
                            segment(selector, ',')
                                .into_iter()
                                .map(|s| s.trim().to_string())
                                .collect(),
                        )
                    } else {
                        CustomVariantBody::Template(nodes.clone())
                    };
                    self.custom_variants.push(CustomVariantDefinition {
                        name: variant.into(),
                        body,
                    });
                    continue;
                }
                if name == "@utility" {
                    self.active = true;
                    if nesting != 0 {
                        return Err(invalid("`@utility` cannot be nested.".into()));
                    }
                    crate::custom_utility::CustomUtility::from_ast(AstNode {
                        kind: NodeKind::AtRule {
                            name: name.clone(),
                            params: params.clone(),
                            nodes: nodes.clone(),
                        },
                        src: source.clone(),
                        dst: None,
                    })
                    .map_err(&invalid)?;
                }
                if name == "@variant" || name == "@apply" {
                    self.active = true;
                }
                if name == "@theme" {
                    self.active = true;
                    let mut flags = ThemeOptions::NONE;
                    if env.reference {
                        flags.0 |= ThemeOptions::REFERENCE.0;
                    }
                    for option in segment(params, ' ') {
                        flags.0 |= match option {
                            "reference" => ThemeOptions::REFERENCE.0,
                            "inline" => ThemeOptions::INLINE.0,
                            "default" => ThemeOptions::DEFAULT.0,
                            "static" => ThemeOptions::STATIC.0,
                            _ => 0,
                        };
                        if let Some(prefix) = option
                            .strip_prefix("prefix(")
                            .and_then(|p| p.strip_suffix(')'))
                        {
                            if !prefix.is_empty() {
                                if !prefix.bytes().all(|b| b.is_ascii_lowercase()) {
                                    return Err(invalid(format!(
                                        "The prefix \"{prefix}\" is invalid. Prefixes must be lowercase ASCII letters (a-z) only."
                                    )));
                                }
                                self.theme.prefix = Some(prefix.into());
                            }
                        }
                    }
                    for child in nodes {
                        match &child.kind {
                            NodeKind::Comment { .. } => {}
                            NodeKind::Declaration {
                                property, value, ..
                            } if property.starts_with("--") => self
                                .theme
                                .add(
                                    &unescape_identifier(property),
                                    value.as_deref().unwrap_or(""),
                                    flags,
                                    child.src.clone(),
                                )
                                .map_err(|message| {
                                    fail(StylesheetErrorKind::Invalid, message, child.src.clone())
                                })?,
                            NodeKind::AtRule { name, .. } if name == "@keyframes" => {
                                self.theme.add_keyframes(child.clone()).map_err(|message| {
                                    fail(StylesheetErrorKind::Invalid, message, child.src.clone())
                                })?
                            }
                            _ => {
                                return Err(fail(
                                    StylesheetErrorKind::Invalid,
                                    "`@theme` blocks must only contain custom properties or `@keyframes`.",
                                    child.src.clone(),
                                ));
                            }
                        }
                    }
                    if !self.theme_slot {
                        self.theme_slot = true;
                        output.push(slot(THEME_SLOT, source));
                    }
                    continue;
                }
            }
            if let Some(children) = node.children_mut() {
                *children = self.collect(std::mem::take(children), env, nesting + 1, depth + 1)?;
            }
            output.push(node);
        }
        Ok(output)
    }
}
fn quoted(value: &str) -> Option<&str> {
    if value.len() >= 2
        && value.starts_with(['\'', '"'])
        && value.as_bytes().first() == value.as_bytes().last()
    {
        value.get(1..value.len() - 1)
    } else {
        None
    }
}
fn contains_directive(nodes: &[AstNode], directive: &str) -> bool {
    nodes.iter().any(|node| {
        matches!(&node.kind, NodeKind::AtRule { name, .. } if name == directive)
            || node
                .children()
                .is_some_and(|children| contains_directive(children, directive))
    })
}
fn media_theme(
    nodes: &mut [AstNode],
    params: &str,
    reference_only: bool,
) -> Result<(), StylesheetError> {
    for node in nodes {
        match &mut node.kind {
            NodeKind::AtRule {
                name, params: own, ..
            } if name == "@theme" => {
                own.push(' ');
                own.push_str(params);
                continue;
            }
            NodeKind::AtRule { .. } | NodeKind::Context { .. } => {}
            _ if reference_only => {
                return Err(fail(
                    StylesheetErrorKind::Invalid,
                    "Files imported with `theme(reference)` must only contain `@theme` blocks. Use `@reference` instead.",
                    node.src.clone(),
                ));
            }
            _ => {}
        }
        if let Some(children) = node.children_mut() {
            media_theme(children, params, reference_only)?;
        }
    }
    Ok(())
}
fn media_source(nodes: &mut [AstNode], params: &str, base: &str) -> bool {
    for node in nodes {
        if let NodeKind::AtRule {
            name, params: own, ..
        } = &mut node.kind
        {
            if name == "@tailwind" && own == "utilities" {
                own.push_str(&format!(" source({params})"));
                let child = node.clone();
                *node = AstNode::new(NodeKind::Context {
                    context: BTreeMap::from([(
                        "sourceBase".into(),
                        ContextValue::String(base.into()),
                    )]),
                    nodes: vec![child],
                });
                return true;
            }
        }
        if let Some(children) = node.children_mut() {
            if media_source(children, params, base) {
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
#[path = "stylesheet_tests.rs"]
mod tests;
