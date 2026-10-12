//! Candidate compilation, ported from Tailwind CSS v4.2.1 `compile.ts`,
//! `utils/compare.ts`, `utils/variables.ts`, and the structural passes in `ast.ts`
//! (MIT, copyright Tailwind Labs). This is not a stylesheet/directive compiler.
//!
//! References: https://github.com/tailwindlabs/tailwindcss/tree/v4.2.1/packages/tailwindcss/src
//! Theme declarations/keyframes are separate artifacts: a stylesheet host must
//! place them at its resolved @theme location. Color-mix compatibility lowering
//! remains a reported downstream pass; native CSS nesting is preserved.
use crate::{
    ast::{AstNode, ContextValue, NodeKind},
    candidate::{Candidate, Variant, parse_candidate, parse_variant},
    design_system::{CandidateTransformErrorKind, DesignSystem},
    theme::{Theme, ThemeOptions},
    value::{ValueNode, escape_identifier, parse_value, segment},
    variants::VariantErrorKind,
};
use serde::{Deserialize, Serialize};
use std::{
    cmp::Ordering,
    collections::{BTreeMap, BTreeSet},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct CompileOptions {
    pub important: bool,
    /// Disable stylesheet-level important for @apply, while retaining `p-4!`.
    pub respect_important: bool,
    pub property_fallbacks: bool,
}
impl Default for CompileOptions {
    fn default() -> Self {
        Self {
            important: false,
            respect_important: true,
            property_fallbacks: true,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompiledRule {
    pub candidate: String,
    /// Class wrapper and variant body before structural lowering.
    pub node: AstNode,
    pub property_order: Vec<usize>,
    pub declaration_count: usize,
    /// Set-bit positions, ascending. Unlike a u64 mask this has no variant cap.
    pub variant_order: Vec<usize>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnsupportedCandidate {
    pub candidate: String,
    pub reasons: Vec<String>,
}
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThemeArtifacts {
    /// Escaped, prefixed declarations, in theme insertion order.
    pub declarations: Vec<AstNode>,
    pub keyframes: Vec<AstNode>,
    /// Referenced variable spellings; includes external CSS variables.
    pub used_variables: Vec<String>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CandidateCompilation {
    pub rules: Vec<CompiledRule>,
    /// Printable candidate rules and support rules, with native CSS nesting.
    pub ast: Vec<AstNode>,
    pub generated: Vec<String>,
    pub invalid: Vec<String>,
    pub unsupported: Vec<UnsupportedCandidate>,
    pub theme: ThemeArtifacts,
    pub remaining_passes: Vec<RemainingPass>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RemainingPass {
    ColorMixCompatibility,
}

/// Compile distinct raw candidates, retaining first input order for diagnostics
/// and upstream's stable rule ordering for successful interpretations.
pub fn compile_candidates(
    raw: &[String],
    theme: &Theme,
    options: &CompileOptions,
) -> Result<CandidateCompilation, String> {
    compile_with_design_system(raw, &DesignSystem::new(theme.clone()), options)
}

/// Compile against authoritative custom and built-in registrations. Custom
/// definitions are observed on every call, including replacements after @apply.
pub fn compile_with_design_system(
    raw: &[String],
    design_system: &DesignSystem,
    options: &CompileOptions,
) -> Result<CandidateCompilation, String> {
    let theme = design_system.theme();
    if raw.len() > 100_000 || raw.iter().map(String::len).sum::<usize>() > 16 * 1024 * 1024 {
        return Err("Candidate input exceeds compilation bounds".into());
    }
    let registry = design_system.registry();
    let mut seen = BTreeSet::new();
    let mut parsed: Vec<(&String, Vec<Candidate>)> = Vec::new();
    for candidate in raw {
        if seen.insert(candidate) {
            parsed.push((
                candidate,
                if design_system.is_excluded(candidate) {
                    Vec::new()
                } else {
                    parse_candidate(candidate, &registry)
                },
            ));
        }
    }
    fn collect_variant(variant: Variant, unique: &mut BTreeMap<String, Variant>) {
        if let Variant::Compound { variant, .. } = &variant {
            collect_variant((**variant).clone(), unique);
        }
        unique.entry(variant_key(&variant)).or_insert(variant);
    }
    let mut unique_variants = BTreeMap::new();
    // Parsing variants precedes utility matching upstream, so valid variants on
    // invalid utilities also participate in the cache's ordering groups.
    for (raw, _) in &parsed {
        let mut parts = segment(raw, ':');
        if let Some(prefix) = registry.prefix.as_ref().filter(|p| !p.is_empty()) {
            if parts.len() == 1 || parts[0] != prefix {
                continue;
            }
            parts.remove(0);
        }
        parts.pop();
        if parts.len() > 256 || raw.len() > 64 * 1024 {
            continue;
        }
        for part in parts.into_iter().rev() {
            if let Some(variant) = parse_variant(part, &registry) {
                collect_variant(variant, &mut unique_variants);
            } else {
                break;
            }
        }
    }
    let mut all_variants: Vec<Variant> = unique_variants.into_values().collect();
    let mut comparison_error = None;
    all_variants.sort_by(|a, b| {
        design_system
            .compare_variants(Some(a), Some(b))
            .unwrap_or_else(|e| {
                comparison_error = Some(e.message);
                Ordering::Equal
            })
    });
    if let Some(error) = comparison_error {
        return Err(format!("Cannot order registered variants: {error}"));
    }
    let mut groups: Vec<(Variant, usize)> = Vec::new();
    let mut group = 0;
    for variant in all_variants {
        if let Some((previous, _)) = groups.last() {
            if design_system
                .compare_variants(Some(previous), Some(&variant))
                .map_err(|e| e.message)?
                != Ordering::Equal
            {
                group += 1;
            }
        }
        groups.push((variant, group));
    }
    let order_map: BTreeMap<_, _> = groups
        .iter()
        .map(|(v, index)| (variant_key(v), *index))
        .collect();
    let mut rules = Vec::new();
    let mut generated = Vec::new();
    let mut invalid = Vec::new();
    let mut unsupported = Vec::new();
    for (raw, candidates) in parsed {
        let start = rules.len();
        let mut reasons = BTreeSet::new();
        for candidate in candidates {
            let base = design_system.compile_base(&candidate)?;
            reasons.extend(base.unsupported);
            for utility in base.utilities {
                let mut nodes = utility.nodes;
                if candidate.important || (options.important && options.respect_important) {
                    apply_important(&mut nodes);
                }
                let mut failed = false;
                for variant in &candidate.variants {
                    match design_system.apply_variant(variant, std::mem::take(&mut nodes)) {
                        Ok(applied) => nodes = applied,
                        Err(error) => {
                            if error.kind == VariantErrorKind::Unsupported {
                                reasons.insert(error.message);
                            }
                            failed = true;
                            break;
                        }
                    }
                }
                if failed {
                    continue;
                }
                nodes = match design_system.transform_candidate(nodes) {
                    Ok(nodes) => nodes,
                    Err(error) => {
                        match error.kind {
                            CandidateTransformErrorKind::Unsupported => {
                                reasons.insert(error.message);
                            }
                            CandidateTransformErrorKind::Limit => return Err(error.message),
                            CandidateTransformErrorKind::Invalid => {}
                        }
                        continue;
                    }
                };
                match unresolved_functions(&nodes) {
                    Ok(Some(reason)) => {
                        reasons.insert(reason);
                        continue;
                    }
                    Err(_) => continue,
                    Ok(None) => {}
                }
                let variant_order: BTreeSet<_> = candidate
                    .variants
                    .iter()
                    .filter_map(|v| order_map.get(&variant_key(v)).copied())
                    .collect();
                rules.push(CompiledRule {
                    candidate: raw.clone(),
                    node: rule(&format!(".{}", escape_identifier(raw)), nodes),
                    property_order: utility.property_order,
                    declaration_count: utility.declaration_count,
                    variant_order: variant_order.into_iter().collect(),
                });
            }
        }
        // Never publish an incomplete candidate if one interpretation requires
        // an unavailable generator/substitution stage.
        if !reasons.is_empty() {
            rules.truncate(start);
            unsupported.push(UnsupportedCandidate {
                candidate: raw.clone(),
                reasons: reasons.into_iter().collect(),
            });
        } else if rules.len() == start {
            invalid.push(raw.clone());
        } else {
            generated.push(raw.clone());
        }
    }
    rules.sort_by(compare_rules);
    let lowered = lower_ast(
        rules.iter().map(|r| r.node.clone()).collect(),
        theme,
        options.property_fallbacks,
    )?;
    Ok(CandidateCompilation {
        rules,
        ast: lowered.ast,
        generated,
        invalid,
        unsupported,
        theme: lowered.theme,
        remaining_passes: lowered.remaining_passes,
    })
}

fn variant_key(variant: &Variant) -> String {
    serde_json::to_string(variant).expect("Variant contains only serializable strings and enums")
}

/// Exact pinned alphanumeric compare: UTF-16 indexing, IEEE754 Number runs,
/// lexical fallback for equal numeric values with different representations.
pub fn compare_candidates(a: &str, b: &str) -> Ordering {
    let a: Vec<_> = a.encode_utf16().collect();
    let b: Vec<_> = b.encode_utf16().collect();
    let digit = |c: u16| (48..=57).contains(&c);
    for i in 0..a.len().min(b.len()) {
        if digit(a[i]) && digit(b[i]) {
            let end = |v: &[u16]| {
                let mut j = i + 1;
                while j < v.len() && digit(v[j]) {
                    j += 1;
                }
                j
            };
            let an = &a[i..end(&a)];
            let bn = &b[i..end(&b)];
            let number = |v: &[u16]| {
                String::from_utf16(v)
                    .unwrap()
                    .parse::<f64>()
                    .unwrap_or(f64::INFINITY)
            };
            let diff = number(an) - number(bn);
            if diff < 0.0 {
                return Ordering::Less;
            }
            if diff > 0.0 {
                return Ordering::Greater;
            }
            let order = an.cmp(bn);
            if order != Ordering::Equal {
                return order;
            }
        } else if a[i] != b[i] {
            return a[i].cmp(&b[i]);
        }
    }
    a.len().cmp(&b.len())
}
fn compare_rules(a: &CompiledRule, b: &CompiledRule) -> Ordering {
    // The highest differing set bit decides bigint order.
    let order = a
        .variant_order
        .iter()
        .rev()
        .cmp(b.variant_order.iter().rev());
    if order != Ordering::Equal {
        return order;
    }
    for i in 0..a.property_order.len().max(b.property_order.len()) {
        let order = a
            .property_order
            .get(i)
            .copied()
            .unwrap_or(usize::MAX)
            .cmp(&b.property_order.get(i).copied().unwrap_or(usize::MAX));
        if order != Ordering::Equal {
            return order;
        }
    }
    b.declaration_count
        .cmp(&a.declaration_count)
        .then_with(|| compare_candidates(&a.candidate, &b.candidate))
}
fn apply_important(nodes: &mut [AstNode]) {
    for node in nodes {
        match &mut node.kind {
            NodeKind::Declaration { important, .. } => *important = true,
            NodeKind::Rule { nodes, .. } | NodeKind::AtRule { nodes, .. } => apply_important(nodes),
            _ => {}
        }
    }
}
fn rule(selector: &str, nodes: Vec<AstNode>) -> AstNode {
    AstNode::new(NodeKind::Rule {
        selector: selector.into(),
        nodes,
    })
}
fn at(name: &str, params: &str, nodes: Vec<AstNode>) -> AstNode {
    AstNode::new(NodeKind::AtRule {
        name: name.into(),
        params: params.into(),
        nodes,
    })
}

fn unresolved_functions(nodes: &[AstNode]) -> Result<Option<String>, String> {
    fn check(nodes: &[ValueNode]) -> Option<&str> {
        for node in nodes {
            if let ValueNode::Function { value, nodes } = node {
                if matches!(
                    value.as_str(),
                    "theme" | "--theme" | "--spacing" | "--alpha" | "--value" | "--modifier"
                ) {
                    return Some(value);
                }
                if let Some(value) = check(nodes) {
                    return Some(value);
                }
            }
        }
        None
    }
    for node in nodes {
        if let NodeKind::AtRule { name, .. } = &node.kind {
            if matches!(name.as_str(), "@apply" | "@variant" | "@slot" | "@utility") {
                return Ok(Some(format!(
                    "{name} requires the stylesheet directive-substitution stage"
                )));
            }
        }
        let value = match &node.kind {
            NodeKind::Declaration {
                value: Some(value), ..
            } => Some(value),
            NodeKind::AtRule { params, .. } => Some(params),
            _ => None,
        };
        if let Some(value) = value {
            let parsed = parse_value(value)?;
            if let Some(function) = check(&parsed) {
                return Ok(Some(format!(
                    "{function}() requires the stylesheet function-substitution stage"
                )));
            }
        }
        if let Some(children) = node.children() {
            if let Some(reason) = unresolved_functions(children)? {
                return Ok(Some(reason));
            }
        }
    }
    Ok(None)
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoweredAst {
    pub ast: Vec<AstNode>,
    pub theme: ThemeArtifacts,
    pub remaining_passes: Vec<RemainingPass>,
}
#[derive(Default)]
struct Usage {
    direct: BTreeSet<String>,
    dependencies: BTreeMap<String, Vec<String>>,
    animation_names: BTreeSet<String>,
    selected: BTreeSet<String>,
    color_mix: bool,
}
fn truthy(context: &BTreeMap<String, ContextValue>, key: &str) -> bool {
    match context.get(key) {
        Some(ContextValue::Bool(v)) => *v,
        Some(ContextValue::String(v)) => !v.is_empty(),
        None => false,
    }
}
fn has_theme_declarations(nodes: &[AstNode], theme_context: bool) -> bool {
    nodes.iter().any(|node| match &node.kind {
        NodeKind::Declaration { property, .. } => theme_context && property.starts_with("--"),
        NodeKind::Context { context, nodes } => {
            if truthy(context, "reference") {
                return false;
            }
            let theme_context = if context.contains_key("theme") {
                truthy(context, "theme")
            } else {
                theme_context
            };
            has_theme_declarations(nodes, theme_context)
        }
        _ => node
            .children()
            .is_some_and(|nodes| has_theme_declarations(nodes, theme_context)),
    })
}
/// Pinned extractUsedVariables walks all words beneath an outermost var().
fn variables(value: &str) -> Result<Vec<String>, String> {
    fn words(nodes: &[ValueNode], out: &mut Vec<String>) {
        for node in nodes {
            match node {
                ValueNode::Word { value } if value.starts_with("--") => out.push(value.clone()),
                ValueNode::Function { nodes, .. } => words(nodes, out),
                _ => {}
            }
        }
    }
    fn walk(nodes: &[ValueNode], out: &mut Vec<String>) {
        for node in nodes {
            if let ValueNode::Function { value, nodes } = node {
                if value == "var" {
                    words(nodes, out);
                } else {
                    walk(nodes, out);
                }
            }
        }
    }
    let mut out = Vec::new();
    if value.contains("var(") {
        walk(&parse_value(value)?, &mut out);
    }
    Ok(out)
}
fn animation_names(value: &str) -> impl Iterator<Item = &str> {
    value
        .split(|c: char| c.is_whitespace() || c == ',')
        .filter(|v| !v.is_empty())
}
impl Usage {
    fn declaration(
        &mut self,
        property: &str,
        value: &str,
        theme: bool,
        keyframes: bool,
        supports_mix: bool,
    ) -> Result<(), String> {
        if property == "--tw-sort" || (theme && property.starts_with("--") && value == "initial") {
            return Ok(());
        }
        if theme && property.starts_with("--") {
            for var in variables(value)? {
                let dependents = self.dependencies.entry(var).or_default();
                if !dependents.iter().any(|v| v == property) {
                    dependents.push(property.into());
                }
            }
        } else {
            self.direct.extend(variables(value)?);
        }
        if property == "animation" {
            self.animation_names
                .extend(animation_names(value).map(str::to_string));
        }
        if value.contains("color-mix(") && !keyframes && !supports_mix {
            self.color_mix = true;
        }
        Ok(())
    }
    fn scan(
        &mut self,
        nodes: &[AstNode],
        context: &BTreeMap<String, ContextValue>,
        depth: usize,
    ) -> Result<(), String> {
        if depth > 256 {
            return Err("AST analysis nesting limit exceeded".into());
        }
        for node in nodes {
            match &node.kind {
                NodeKind::Declaration {
                    property,
                    value: Some(value),
                    ..
                } => self.declaration(
                    property,
                    value,
                    truthy(context, "theme"),
                    truthy(context, "keyframes"),
                    truthy(context, "supportsColorMix"),
                )?,
                NodeKind::Context {
                    context: own,
                    nodes,
                } => {
                    if truthy(own, "reference") {
                        continue;
                    }
                    let mut context = context.clone();
                    context.extend(own.clone());
                    self.scan(nodes, &context, depth + 1)?;
                }
                NodeKind::AtRule {
                    name,
                    params,
                    nodes,
                } => {
                    let mut context = context.clone();
                    if name == "@keyframes" {
                        context.insert("keyframes".into(), ContextValue::Bool(true));
                    }
                    if name == "@supports" && params.contains("color-mix(") {
                        context.insert("supportsColorMix".into(), ContextValue::Bool(true));
                    }
                    self.scan(nodes, &context, depth + 1)?;
                }
                _ => {
                    if let Some(children) = node.children() {
                        self.scan(children, context, depth + 1)?;
                    }
                }
            }
        }
        Ok(())
    }
    fn used(&self, variable: &str, flagged: &BTreeSet<String>) -> bool {
        let mut stack = vec![variable];
        let mut seen = BTreeSet::new();
        while let Some(var) = stack.pop() {
            // Exact upstream cycle behavior retains cyclic theme dependencies.
            if !seen.insert(var) || flagged.contains(var) || self.direct.contains(var) {
                return true;
            }
            if let Some(dependencies) = self.dependencies.get(var) {
                stack.extend(dependencies.iter().rev().map(String::as_str));
            }
        }
        false
    }
}

/// Structural optimization only. Context and AtRoot are eliminated, while
/// unknown ordinary CSS constructs are retained. No filesystem/global state.
pub fn lower_ast(
    nodes: Vec<AstNode>,
    theme: &Theme,
    property_fallbacks: bool,
) -> Result<LoweredAst, String> {
    let mut usage = Usage::default();
    usage.scan(&nodes, &BTreeMap::new(), 0)?;
    let entries = theme.entries();
    let mut flagged = BTreeSet::new();
    for (key, value) in &entries {
        let key = escape_identifier(key);
        if value.options.contains(ThemeOptions::STATIC)
            || value.options.contains(ThemeOptions::USED)
        {
            flagged.insert(key.clone());
        }
        if !value.options.contains(ThemeOptions::REFERENCE) {
            usage.declaration(&key, &value.value, true, false, false)?;
        }
    }
    let keyframe_context = BTreeMap::from([
        ("theme".into(), ContextValue::Bool(true)),
        ("keyframes".into(), ContextValue::Bool(true)),
    ]);
    usage.scan(theme.keyframes(), &keyframe_context, 0)?;
    let mut declarations = Vec::new();
    for (key, value) in &entries {
        let key = escape_identifier(key);
        if usage.used(&key, &flagged) {
            usage.selected.insert(key.clone());
            if !value.options.contains(ThemeOptions::REFERENCE) && value.value != "initial" {
                let mut node = AstNode::declaration(&key, &value.value);
                node.src = value.src.clone();
                declarations.push(node);
                if key.starts_with(&escape_identifier(&theme.prefix_key("--animate-"))) {
                    usage
                        .animation_names
                        .extend(animation_names(&value.value).map(str::to_string));
                }
            }
        }
    }
    // Theme contexts supplied by future stylesheet passes may also contain
    // declarations not present in Theme; only actually used ones survive.
    for key in usage.dependencies.keys().chain(usage.direct.iter()) {
        if usage.used(key, &flagged) {
            usage.selected.insert(key.clone());
        }
    }
    let mut lowerer = Lowerer {
        usage: &usage,
        seen_properties: BTreeSet::new(),
        roots: Vec::new(),
        fallback_root: Vec::new(),
        fallback_universal: Vec::new(),
        property_fallbacks,
    };
    let mut ast = lowerer.nodes(nodes, &BTreeMap::new(), 0)?;
    let mut keyframes = Vec::new();
    for node in theme.keyframes() {
        if matches!(&node.kind, NodeKind::AtRule { name, params, .. } if name == "@keyframes" && usage.animation_names.contains(params))
        {
            keyframes.extend(lowerer.nodes(vec![node.clone()], &keyframe_context, 0)?);
        }
    }
    ast.append(&mut lowerer.roots);
    if usage.color_mix {
        ast = crate::color_mix::color_mix_compatibility(&ast, theme)
            .map_err(|error| error.to_string())?
            .ast;
        declarations = crate::color_mix::color_mix_compatibility(&declarations, theme)
            .map_err(|error| error.to_string())?
            .ast;
    }
    lowerer.fallbacks(&mut ast);
    Ok(LoweredAst {
        ast,
        theme: ThemeArtifacts {
            declarations,
            keyframes,
            used_variables: usage.direct.iter().cloned().collect(),
        },
        remaining_passes: Vec::new(),
    })
}

struct Lowerer<'a> {
    usage: &'a Usage,
    seen_properties: BTreeSet<String>,
    roots: Vec<AstNode>,
    fallback_root: Vec<AstNode>,
    fallback_universal: Vec<AstNode>,
    property_fallbacks: bool,
}
impl Lowerer<'_> {
    fn nodes(
        &mut self,
        nodes: Vec<AstNode>,
        context: &BTreeMap<String, ContextValue>,
        depth: usize,
    ) -> Result<Vec<AstNode>, String> {
        if depth > 256 {
            return Err("AST lowering nesting limit exceeded".into());
        }
        let mut out = Vec::new();
        for mut node in nodes {
            match node.kind {
                NodeKind::Declaration {
                    ref property,
                    ref value,
                    ..
                } => {
                    let Some(value) = value else {
                        continue;
                    };
                    if property == "--tw-sort" {
                        continue;
                    }
                    if truthy(context, "theme")
                        && property.starts_with("--")
                        && (value == "initial"
                            || (!truthy(context, "keyframes")
                                && !self.usage.selected.contains(property)))
                    {
                        continue;
                    }
                    out.push(node);
                }
                NodeKind::Context {
                    context: own,
                    nodes,
                } => {
                    if truthy(&own, "reference") {
                        continue;
                    }
                    let mut context = context.clone();
                    context.extend(own);
                    out.extend(self.nodes(nodes, &context, depth)?);
                }
                NodeKind::AtRoot { nodes } => {
                    for child in nodes {
                        let lowered = self.nodes(vec![child], context, 0)?;
                        self.roots.extend(lowered);
                    }
                }
                NodeKind::Rule { selector, nodes } => {
                    let mut nodes = self.nodes(nodes, context, depth + 1)?;
                    let mut seen = BTreeSet::new();
                    nodes.reverse();
                    nodes.retain(|node| match &node.kind {
                        NodeKind::Declaration {
                            property,
                            value,
                            important,
                        } => seen.insert((property.clone(), value.clone(), *important)),
                        _ => true,
                    });
                    nodes.reverse();
                    if nodes.is_empty() {
                        continue;
                    }
                    if selector == "&" {
                        out.extend(nodes);
                    } else {
                        node.kind = NodeKind::Rule { selector, nodes };
                        out.push(node);
                    }
                }
                NodeKind::AtRule {
                    name,
                    params,
                    nodes,
                } => {
                    let prune_empty_theme_layer = name == "@layer"
                        && has_theme_declarations(&nodes, truthy(context, "theme"));
                    if name == "@property" && depth == 0 {
                        if !self.seen_properties.insert(params.clone()) {
                            continue;
                        }
                        if self.property_fallbacks {
                            let mut initial = "initial".to_string();
                            let mut inherits = false;
                            for child in &nodes {
                                if let NodeKind::Declaration {
                                    property,
                                    value: Some(value),
                                    ..
                                } = &child.kind
                                {
                                    if property == "initial-value" {
                                        initial = value.clone();
                                    }
                                    if property == "inherits" {
                                        inherits = value == "true";
                                    }
                                }
                            }
                            let mut fallback = AstNode::declaration(&params, initial);
                            fallback.src = node.src.clone();
                            if inherits {
                                self.fallback_root.push(fallback);
                            } else {
                                self.fallback_universal.push(fallback);
                            }
                        }
                    }
                    let mut context = context.clone();
                    if name == "@keyframes" {
                        context.insert("keyframes".into(), ContextValue::Bool(true));
                        if truthy(&context, "theme")
                            && !self.usage.animation_names.contains(&params)
                        {
                            continue;
                        }
                    }
                    let nodes = self.nodes(nodes, &context, depth + 1)?;
                    if nodes.is_empty() && prune_empty_theme_layer {
                        continue;
                    }
                    if nodes.is_empty()
                        && !matches!(
                            name.as_str(),
                            "@layer" | "@charset" | "@custom-media" | "@namespace" | "@import"
                        )
                    {
                        continue;
                    }
                    node.kind = NodeKind::AtRule {
                        name,
                        params,
                        nodes,
                    };
                    out.push(node);
                }
                NodeKind::Comment { .. } => out.push(node),
            }
        }
        Ok(out)
    }
    fn fallbacks(&mut self, ast: &mut Vec<AstNode>) {
        let mut nodes = Vec::new();
        for (selector, declarations) in [
            (":root, :host", &mut self.fallback_root),
            (
                "*, ::before, ::after, ::backdrop",
                &mut self.fallback_universal,
            ),
        ] {
            if !declarations.is_empty() {
                let src = declarations[0].src.clone();
                let mut node = rule(selector, std::mem::take(declarations));
                node.src = src;
                nodes.push(node);
            }
        }
        if nodes.is_empty() {
            return;
        }
        let src = nodes[0].src.clone();
        let index = ast.iter().position(|n| !matches!(&n.kind, NodeKind::Comment { .. }) && !matches!(&n.kind, NodeKind::AtRule { name, .. } if name == "@charset" || name == "@import")).unwrap_or(ast.len());
        let mut statement = at("@layer", "properties", Vec::new());
        statement.src = src.clone();
        ast.insert(index, statement);
        let mut supports = at(
            "@supports",
            "((-webkit-hyphens: none) and (not (margin-trim: inline))) or ((-moz-orient: inline) and (not (color:rgb(from red r g b))))",
            nodes,
        );
        supports.src = src.clone();
        let mut block = at("@layer", "properties", vec![supports]);
        block.src = src;
        ast.push(block);
    }
}

#[cfg(test)]
#[path = "compile_tests.rs"]
mod tests;
