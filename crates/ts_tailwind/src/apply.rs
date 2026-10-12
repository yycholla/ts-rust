//! Native Tailwind 4.2.1 `apply.ts` substitution (MIT, Tailwind Labs).
//! Definitions must already be registered. Expansion preserves compiler IR;
//! the stylesheet host removes @utility definitions and lowers AtRoot later.
use crate::{
    ast::{AstNode, NodeKind, SourceLocation},
    candidate::{CandidateKind, parse_candidate},
    compile::{CompileOptions, compile_with_design_system},
    custom_utility::CustomUtility,
    design_system::{DesignSystem, UtilityId},
    value::segment,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct ApplyOptions {
    /// Exact immutable inputs indexed by SourceLocation.file. Without a
    /// snapshot the directive range is retained; candidate precision is not guessed.
    pub sources: BTreeMap<String, String>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ApplyErrorKind {
    UnknownCandidate,
    UnsupportedCandidate,
    CircularDependency,
    Keyframes,
    UnregisteredDefinition,
    SourceMismatch,
    Limit,
    Compilation,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplyError {
    pub kind: ApplyErrorKind,
    pub message: String,
    pub candidate: Option<String>,
    pub source: Option<SourceLocation>,
    pub exact_source: bool,
}
impl std::fmt::Display for ApplyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for ApplyError {}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplyExpansion {
    pub candidate: String,
    pub source: Option<SourceLocation>,
    pub exact_source: bool,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplyResult {
    pub ast: Vec<AstNode>,
    pub has_apply: bool,
    /// Dependency-first identities whose registered bodies were replaced.
    pub updated_definitions: Vec<UtilityId>,
    pub expansions: Vec<ApplyExpansion>,
}
fn error(
    kind: ApplyErrorKind,
    message: impl Into<String>,
    source: Option<SourceLocation>,
) -> ApplyError {
    ApplyError {
        kind,
        message: message.into(),
        candidate: None,
        source,
        exact_source: false,
    }
}
fn candidate_error(
    kind: ApplyErrorKind,
    message: impl Into<String>,
    token: &ApplyExpansion,
) -> ApplyError {
    ApplyError {
        kind,
        message: message.into(),
        candidate: Some(token.candidate.clone()),
        source: token.source.clone(),
        exact_source: token.exact_source,
    }
}
const MAX_NODES: usize = 100_000;
const MAX_DEPTH: usize = 256;
const MAX_BYTES: usize = 16 * 1024 * 1024;

/// Transactional across both input trees and registered definitions. Candidate
/// `!` survives @apply; stylesheet-level important is intentionally ignored.
pub fn substitute_at_apply(
    ast: &[AstNode],
    design_system: &mut DesignSystem,
    options: &ApplyOptions,
) -> Result<ApplyResult, ApplyError> {
    let mut count = 0;
    let mut bytes = 0;
    validate_tree(ast, 0, false, &mut count, &mut bytes)?;
    for (_, utility) in design_system.definitions() {
        validate_tree(utility.body(), 0, false, &mut count, &mut bytes)?;
    }
    let mut bindings = BTreeMap::new();
    let mut bound = BTreeSet::new();
    bind_definitions(
        ast,
        &mut Vec::new(),
        design_system,
        &mut bindings,
        &mut bound,
    )?;
    let mut system = design_system.clone();
    let mut dependencies = BTreeMap::new();
    let registry = system.registry();
    for (id, utility) in system.definitions() {
        let mut applies = Vec::new();
        collect_applies(utility.body(), &mut applies);
        let mut edges = Vec::new();
        let mut seen = BTreeSet::new();
        for apply in applies {
            for token in tokens(apply, options)? {
                for candidate in parse_candidate(&token.candidate, &registry) {
                    let root = match candidate.kind {
                        CandidateKind::Static { root } | CandidateKind::Functional { root, .. } => {
                            root
                        }
                        CandidateKind::Arbitrary { .. } => continue,
                    };
                    for (dependency, _) in system.definitions_for_root(&root) {
                        if seen.insert(dependency) {
                            edges.push((dependency, token.clone()));
                        }
                    }
                }
            }
        }
        dependencies.insert(id, edges);
    }
    let mut state = BTreeMap::new();
    let mut order = Vec::new();
    for id in dependencies.keys() {
        visit(*id, &dependencies, &mut state, &mut order, 0)?;
    }
    let mut result = ApplyResult {
        ast: Vec::new(),
        has_apply: false,
        updated_definitions: Vec::new(),
        expansions: Vec::new(),
    };
    let mut expanded_count = 0;
    for id in order {
        let utility = system.utility(id).expect("registered identity");
        let mut applies = Vec::new();
        collect_applies(utility.body(), &mut applies);
        if applies.is_empty() {
            continue;
        }
        let body = expand(
            utility.body(),
            &system,
            options,
            &mut result,
            &mut expanded_count,
            0,
        )?;
        system
            .replace_utility_body(id, body)
            .map_err(|message| error(ApplyErrorKind::Compilation, message, None))?;
        result.updated_definitions.push(id);
    }
    let mut prepared = ast.to_vec();
    restore_definitions(&mut prepared, &mut Vec::new(), &bindings, &system);
    result.ast = expand(
        &prepared,
        &system,
        options,
        &mut result,
        &mut expanded_count,
        0,
    )?;
    let mut count = 0;
    let mut bytes = 0;
    validate_tree(&result.ast, 0, false, &mut count, &mut bytes)?;
    *design_system = system;
    Ok(result)
}

fn validate_tree(
    nodes: &[AstNode],
    depth: usize,
    keyframes: bool,
    count: &mut usize,
    bytes: &mut usize,
) -> Result<(), ApplyError> {
    if depth > MAX_DEPTH {
        return Err(error(
            ApplyErrorKind::Limit,
            "@apply tree nesting limit exceeded",
            None,
        ));
    }
    for node in nodes {
        *count += 1;
        *bytes = bytes.saturating_add(match &node.kind {
            NodeKind::Rule { selector, .. } => selector.len(),
            NodeKind::AtRule { name, params, .. } => name.len().saturating_add(params.len()),
            NodeKind::Declaration {
                property, value, ..
            } => property
                .len()
                .saturating_add(value.as_ref().map_or(0, String::len)),
            NodeKind::Comment { value } => value.len(),
            NodeKind::Context { context, .. } => context
                .iter()
                .map(|(key, value)| {
                    key.len().saturating_add(match value {
                        crate::ast::ContextValue::String(value) => value.len(),
                        crate::ast::ContextValue::Bool(_) => 1,
                    })
                })
                .sum(),
            NodeKind::AtRoot { .. } => 0,
        });
        if *bytes > MAX_BYTES {
            return Err(error(
                ApplyErrorKind::Limit,
                "@apply input text exceeds 16 MiB",
                node.src.clone(),
            ));
        }
        if *count > MAX_NODES {
            return Err(error(
                ApplyErrorKind::Limit,
                "@apply input node limit exceeded",
                node.src.clone(),
            ));
        }
        let is_keyframes =
            matches!(&node.kind, NodeKind::AtRule { name, .. } if name == "@keyframes");
        if keyframes && matches!(&node.kind, NodeKind::AtRule { name, .. } if name == "@apply") {
            return Err(error(
                ApplyErrorKind::Keyframes,
                "You cannot use `@apply` inside `@keyframes`.",
                node.src.clone(),
            ));
        }
        if let Some(children) = node.children() {
            validate_tree(children, depth + 1, keyframes || is_keyframes, count, bytes)?;
        }
    }
    Ok(())
}
fn collect_applies<'a>(nodes: &'a [AstNode], out: &mut Vec<&'a AstNode>) {
    for node in nodes {
        if matches!(&node.kind, NodeKind::AtRule { name, .. } if name == "@apply") {
            out.push(node);
        }
        if let Some(children) = node.children() {
            collect_applies(children, out);
        }
    }
}
fn bind_definitions(
    nodes: &[AstNode],
    path: &mut Vec<usize>,
    system: &DesignSystem,
    bindings: &mut BTreeMap<Vec<usize>, UtilityId>,
    bound: &mut BTreeSet<UtilityId>,
) -> Result<(), ApplyError> {
    for (index, node) in nodes.iter().enumerate() {
        path.push(index);
        if matches!(&node.kind, NodeKind::AtRule { name, .. } if name == "@utility") {
            let normalized = CustomUtility::from_ast(node.clone()).map_err(|message| {
                error(
                    ApplyErrorKind::UnregisteredDefinition,
                    message,
                    node.src.clone(),
                )
            })?;
            let id = system.definitions_for_root(normalized.root()).find(|(id, definition)| {
                !bound.contains(id) && definition.name() == normalized.name() && definition.body() == normalized.body()
            }).map(|(id, _)| id).ok_or_else(|| error(ApplyErrorKind::UnregisteredDefinition, format!("@utility {} must be registered with its current body before @apply expansion", normalized.name()), node.src.clone()))?;
            bindings.insert(path.clone(), id);
            bound.insert(id);
        }
        if let Some(children) = node.children() {
            bind_definitions(children, path, system, bindings, bound)?;
        }
        path.pop();
    }
    Ok(())
}
fn restore_definitions(
    nodes: &mut [AstNode],
    path: &mut Vec<usize>,
    bindings: &BTreeMap<Vec<usize>, UtilityId>,
    system: &DesignSystem,
) {
    for (index, node) in nodes.iter_mut().enumerate() {
        path.push(index);
        if let Some(id) = bindings.get(path) {
            *node.children_mut().expect("utility body") = system
                .utility(*id)
                .expect("registered identity")
                .body()
                .to_vec();
        } else if let Some(children) = node.children_mut() {
            restore_definitions(children, path, bindings, system);
        }
        path.pop();
    }
}
type Dependencies = BTreeMap<UtilityId, Vec<(UtilityId, ApplyExpansion)>>;
fn visit(
    id: UtilityId,
    dependencies: &Dependencies,
    states: &mut BTreeMap<UtilityId, u8>,
    order: &mut Vec<UtilityId>,
    depth: usize,
) -> Result<(), ApplyError> {
    if states.get(&id) == Some(&2) {
        return Ok(());
    }
    if depth > MAX_DEPTH {
        return Err(error(
            ApplyErrorKind::Limit,
            "@apply dependency depth limit exceeded",
            None,
        ));
    }
    states.insert(id, 1);
    for (dependency, token) in &dependencies[&id] {
        if states.get(dependency) == Some(&1) {
            return Err(candidate_error(
                ApplyErrorKind::CircularDependency,
                format!(
                    "You cannot `@apply` the `{}` utility here because it creates a circular dependency.",
                    token.candidate
                ),
                token,
            ));
        }
        visit(*dependency, dependencies, states, order, depth + 1)?;
    }
    states.insert(id, 2);
    order.push(id);
    Ok(())
}
fn expand(
    nodes: &[AstNode],
    system: &DesignSystem,
    options: &ApplyOptions,
    result: &mut ApplyResult,
    count: &mut usize,
    depth: usize,
) -> Result<Vec<AstNode>, ApplyError> {
    if depth > MAX_DEPTH {
        return Err(error(
            ApplyErrorKind::Limit,
            "@apply expanded nesting limit exceeded",
            None,
        ));
    }
    let mut output = Vec::new();
    for node in nodes {
        *count += 1;
        if *count > MAX_NODES {
            return Err(error(
                ApplyErrorKind::Limit,
                "@apply expansion node limit exceeded",
                node.src.clone(),
            ));
        }
        if matches!(&node.kind, NodeKind::AtRule { name, .. } if name == "@apply") {
            result.has_apply = true;
            let tokens = tokens(node, options)?;
            let candidates: Vec<_> = tokens.iter().map(|t| t.candidate.clone()).collect();
            let compiled = compile_with_design_system(
                &candidates,
                system,
                &CompileOptions {
                    respect_important: false,
                    property_fallbacks: false,
                    ..CompileOptions::default()
                },
            )
            .map_err(|message| error(ApplyErrorKind::Compilation, message, node.src.clone()))?;
            // Preserve the first input candidate's diagnostic, independently of
            // the compiler's property/variant ordering of successful output.
            for token in &tokens {
                if let Some(unsupported) = compiled
                    .unsupported
                    .iter()
                    .find(|u| u.candidate == token.candidate)
                {
                    return Err(candidate_error(
                        ApplyErrorKind::UnsupportedCandidate,
                        format!(
                            "Cannot apply utility class `{}`: {}",
                            token.candidate,
                            unsupported.reasons.join("; ")
                        ),
                        token,
                    ));
                }
                if compiled.invalid.contains(&token.candidate) {
                    return Err(unknown_candidate(token, system));
                }
            }
            for rule in compiled.rules {
                let token = tokens
                    .iter()
                    .find(|t| t.candidate == rule.candidate)
                    .expect("compiled requested candidate");
                let mut node = rule.node;
                stamp_source(&mut node, &token.source, count, 0)?;
                match node.kind {
                    NodeKind::Rule { nodes, .. } => output.extend(nodes),
                    _ => output.push(node),
                }
            }
            result.expansions.extend(tokens);
        } else {
            let mut node = node.clone();
            if let Some(children) = node.children_mut() {
                *children = expand(children, system, options, result, count, depth + 1)?;
            }
            output.push(node);
        }
    }
    Ok(output)
}
fn stamp_source(
    node: &mut AstNode,
    source: &Option<SourceLocation>,
    count: &mut usize,
    depth: usize,
) -> Result<(), ApplyError> {
    *count += 1;
    if *count > MAX_NODES || depth > MAX_DEPTH {
        return Err(error(
            ApplyErrorKind::Limit,
            "@apply generated output limit exceeded",
            source.clone(),
        ));
    }
    node.src = source.clone();
    node.dst = None;
    if let Some(children) = node.children_mut() {
        for child in children {
            stamp_source(child, source, count, depth + 1)?;
        }
    }
    Ok(())
}
fn unknown_candidate(token: &ApplyExpansion, system: &DesignSystem) -> ApplyError {
    let candidate = &token.candidate;
    let message = if let Some(prefix) = system
        .theme()
        .prefix
        .as_ref()
        .filter(|p| !candidate.starts_with(p.as_str()))
    {
        format!(
            "Cannot apply unprefixed utility class `{candidate}`. Did you mean `{prefix}:{candidate}`?"
        )
    } else if system.is_excluded(candidate) {
        format!(
            "Cannot apply utility class `{candidate}` because it has been explicitly disabled: https://tailwindcss.com/docs/detecting-classes-in-source-files#explicitly-excluding-classes"
        )
    } else {
        let mut parts = segment(candidate, ':');
        let mut unknown = Vec::new();
        if parts.len() > 1 {
            let utility = parts.pop().unwrap();
            if compile_with_design_system(&[utility.into()], system, &CompileOptions::default())
                .is_ok_and(|r| !r.rules.is_empty())
            {
                for variant in parts {
                    let probe = format!("{variant}:[--tw-variant-check:1]");
                    if compile_with_design_system(&[probe], system, &CompileOptions::default())
                        .is_ok_and(|r| r.rules.is_empty())
                    {
                        unknown.push(format!("`{variant}`"));
                    }
                }
            }
        }
        if !unknown.is_empty() {
            let plural = unknown.len() > 1;
            let list = match unknown.len() {
                1 => unknown[0].clone(),
                2 => unknown.join(" and "),
                _ => format!(
                    "{}, and {}",
                    unknown[..unknown.len() - 1].join(", "),
                    unknown.last().unwrap()
                ),
            };
            format!(
                "Cannot apply utility class `{candidate}` because the {list} variant{} do{} not exist.",
                if plural { "s" } else { "" },
                if plural { "" } else { "es" }
            )
        } else if system.theme().is_empty() {
            format!(
                "Cannot apply unknown utility class `{candidate}`. Are you using CSS modules or similar and missing `@reference`? https://tailwindcss.com/docs/functions-and-directives#reference-directive"
            )
        } else {
            format!("Cannot apply unknown utility class `{candidate}`")
        }
    };
    candidate_error(ApplyErrorKind::UnknownCandidate, message, token)
}

fn js_whitespace(c: char) -> bool {
    matches!(c, '\u{0009}'..='\u{000D}' | ' ' | '\u{00A0}' | '\u{1680}' | '\u{2000}'..='\u{200A}' | '\u{2028}' | '\u{2029}' | '\u{202F}' | '\u{205F}' | '\u{3000}' | '\u{FEFF}')
}
fn tokens(node: &AstNode, options: &ApplyOptions) -> Result<Vec<ApplyExpansion>, ApplyError> {
    let NodeKind::AtRule { params, .. } = &node.kind else {
        unreachable!()
    };
    if params.is_empty() {
        return Ok(vec![ApplyExpansion {
            candidate: String::new(),
            source: node.src.clone(),
            exact_source: false,
        }]);
    }
    let parsed: Vec<_> = params
        .split(js_whitespace)
        .filter(|p| !p.is_empty())
        .collect();
    let mut ranges = Vec::new();
    if let Some((source, text)) = node
        .src
        .as_ref()
        .and_then(|source| options.sources.get(&source.file).map(|text| (source, text)))
    {
        let raw = text.get(source.start..source.end).ok_or_else(|| {
            error(
                ApplyErrorKind::SourceMismatch,
                "@apply range is outside the supplied UTF-8 snapshot",
                node.src.clone(),
            )
        })?;
        let raw = raw.strip_prefix("@apply").ok_or_else(|| {
            error(
                ApplyErrorKind::SourceMismatch,
                "@apply source does not match the supplied snapshot",
                node.src.clone(),
            )
        })?;
        let raw_tokens = source_tokens(raw, source.start + 6)?;
        if raw_tokens
            .iter()
            .map(|(text, _, _)| text.as_str())
            .collect::<Vec<_>>()
            != parsed
        {
            return Err(error(
                ApplyErrorKind::SourceMismatch,
                "@apply candidates differ from the supplied source snapshot",
                node.src.clone(),
            ));
        }
        for (_, start, end) in raw_tokens {
            ranges.push(SourceLocation {
                file: source.file.clone(),
                start,
                end,
            });
        }
    }
    // JavaScript object keys keep insertion order but duplicate candidates use
    // the last occurrence's offset. Rust retains the same two properties.
    let mut output: Vec<ApplyExpansion> = Vec::new();
    for (index, candidate) in parsed.into_iter().enumerate() {
        let token = ApplyExpansion {
            candidate: candidate.into(),
            source: ranges.get(index).cloned().or_else(|| node.src.clone()),
            exact_source: !ranges.is_empty(),
        };
        if let Some(previous) = output.iter_mut().find(|t| t.candidate == candidate) {
            *previous = token;
        } else {
            output.push(token);
        }
    }
    Ok(output)
}
/// CSS comments are removed by the parser, and CRLF/whitespace may be
/// normalized. Keep original byte spans while reproducing candidate spellings.
fn source_tokens(raw: &str, base: usize) -> Result<Vec<(String, usize, usize)>, ApplyError> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut start = 0;
    let mut end = 0;
    let mut i = 0;
    let mut quote = None;
    let mut escaped = false;
    while i < raw.len() {
        if quote.is_none() && !escaped && raw[i..].starts_with("/*") {
            let finish = raw[i + 2..].find("*/").ok_or_else(|| {
                error(
                    ApplyErrorKind::SourceMismatch,
                    "Unterminated comment in @apply snapshot",
                    None,
                )
            })?;
            i += finish + 4;
            continue;
        }
        let c = raw[i..].chars().next().unwrap();
        if !escaped {
            if quote == Some(c) {
                quote = None;
            } else if quote.is_none() && matches!(c, '\'' | '"') {
                quote = Some(c);
            }
        }
        if js_whitespace(c) {
            if !current.is_empty() {
                out.push((std::mem::take(&mut current), start, end));
            }
        } else {
            if current.is_empty() {
                start = base + i;
            }
            current.push(c);
            end = base + i + c.len_utf8();
        }
        escaped = c == '\\' && !escaped;
        i += c.len_utf8();
    }
    if !current.is_empty() {
        out.push((current, start, end));
    }
    Ok(out)
}

#[cfg(test)]
#[path = "apply_tests.rs"]
mod tests;
