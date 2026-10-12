//! Native @tailwindcss/typography 0.5.19, pinned to Tailwind 4.2.1 colors.
//! Port of index.js/styles.js/utils.js (MIT, Tailwind Labs). Attribution and
//! exact input hashes: typography_LICENSE.txt / typography_provenance.json.
//! Defaults are ordered, merged CSS data extracted without executing JavaScript.
use crate::{
    ast::{AstNode, NodeKind},
    candidate::{Candidate, CandidateKind},
    custom_utility::valid_static_name,
    design_system::{DesignSystem, UtilityId},
    theme::Theme,
    value::{escape_identifier, segment, unescape_identifier},
    variants::{CustomVariantBody, CustomVariantDefinition},
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
pub const TYPOGRAPHY_VERSION: &str = "0.5.19";
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TypographyTarget {
    #[default]
    Modern,
    Legacy,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct TypographyOptions {
    pub class_name: String,
    pub target: TypographyTarget,
}
impl Default for TypographyOptions {
    fn default() -> Self {
        Self {
            class_name: "prose".into(),
            target: TypographyTarget::Modern,
        }
    }
}
/// Fully resolved theme('typography') entry. CSS properties are kebab-case;
/// array/object config merging belongs to the configuration resolver. Supplying
/// a modifier list replaces defaults, matching a resolved theme dictionary.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TypographyModifier {
    pub name: String,
    pub css: Vec<AstNode>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TypographyRegistration {
    pub version: String,
    pub utilities: Vec<AstNode>,
    pub variants: Vec<CustomVariantDefinition>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TypographyError {
    pub message: String,
}
impl std::fmt::Display for TypographyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for TypographyError {}
fn error(message: impl Into<String>) -> TypographyError {
    TypographyError {
        message: message.into(),
    }
}

pub fn default_modifiers() -> Result<Vec<TypographyModifier>, TypographyError> {
    serde_json::from_str(include_str!("typography_defaults.json"))
        .map_err(|e| error(format!("Invalid bundled Typography data: {e}")))
}
pub fn generate_typography(
    version: &str,
    options: &TypographyOptions,
    prefix: Option<&str>,
    resolved_modifiers: Option<&[TypographyModifier]>,
) -> Result<TypographyRegistration, TypographyError> {
    if version != TYPOGRAPHY_VERSION {
        return Err(error(format!(
            "Unsupported Typography version {version}; native implementation is pinned to {TYPOGRAPHY_VERSION}"
        )));
    }
    if !valid_static_name(&options.class_name) || options.class_name.len() > 256 {
        return Err(error(
            "Typography className must be a valid Tailwind utility name",
        ));
    }
    if prefix.is_some_and(|p| !p.bytes().all(|c| c.is_ascii_lowercase())) {
        return Err(error("Typography prefix must be lowercase ASCII letters"));
    }
    let defaults;
    let modifiers = if let Some(modifiers) = resolved_modifiers {
        modifiers
    } else {
        defaults = default_modifiers()?;
        &defaults
    };
    if modifiers.len() > 4096 {
        return Err(error("Typography modifier limit exceeded"));
    }
    let mut utilities = Vec::new();
    let mut seen = BTreeSet::new();
    let mut budget = (0, 0);
    for modifier in modifiers {
        if !seen.insert(&modifier.name) {
            return Err(error(format!(
                "Duplicate resolved Typography modifier {}",
                modifier.name
            )));
        }
        let name = if modifier.name == "DEFAULT" {
            options.class_name.clone()
        } else {
            format!("{}-{}", options.class_name, modifier.name)
        };
        if !valid_static_name(&name) {
            return Err(error(format!("Invalid Typography utility name {name}")));
        }
        validate_css(&modifier.css, 0, &mut budget)?;
        let mut nodes = modifier.css.clone();
        if options.target == TypographyTarget::Modern {
            // configToCss rewrites immediate object selector keys. Nested
            // objects remain intact (Object.fromEntries ignores its third tuple
            // element), so do not recursively rewrite nested authored selectors.
            for node in &mut nodes {
                match &mut node.kind {
                    NodeKind::Rule { selector, .. } => {
                        *selector = in_where(selector, &options.class_name, &modifier.name)?
                    }
                    NodeKind::AtRule { .. } => {
                        return Err(error(
                            "Modern Typography CSS object at-rules require explicit selector-compatible configuration",
                        ));
                    }
                    _ => {}
                }
            }
        }
        if let Some(prefix) = prefix.filter(|p| !p.is_empty()) {
            rewrite_rules(&mut nodes, &|class| Some(format!("{prefix}:{class}")))?;
        }
        utilities.push(AstNode::new(NodeKind::AtRule {
            name: "@utility".into(),
            params: name,
            nodes,
        }));
    }
    let mut variants = Vec::new();
    for name in [
        "headings",
        "h1",
        "h2",
        "h3",
        "h4",
        "h5",
        "h6",
        "p",
        "a",
        "blockquote",
        "figure",
        "figcaption",
        "strong",
        "em",
        "kbd",
        "code",
        "pre",
        "ol",
        "ul",
        "li",
        "dl",
        "dt",
        "dd",
        "table",
        "thead",
        "tr",
        "th",
        "td",
        "img",
        "picture",
        "video",
        "hr",
        "lead",
    ] {
        let selectors = match name {
            "headings" => vec!["h1", "h2", "h3", "h4", "h5", "h6", "th"],
            "lead" => vec!["[class~=\"lead\"]"],
            _ => vec![name],
        };
        let selectors = if options.target == TypographyTarget::Legacy {
            selectors.iter().map(|s| format!("& {s}")).collect()
        } else {
            vec![format!(
                "& :is({})",
                in_where(&selectors.join(", "), &options.class_name, "")?
            )]
        };
        variants.push(CustomVariantDefinition {
            name: format!("{}-{name}", options.class_name),
            body: CustomVariantBody::Selectors(selectors),
        });
    }
    Ok(TypographyRegistration {
        version: version.into(),
        utilities,
        variants,
    })
}
/// Atomic plugin installation; a failure never leaves a partial variant or
/// utility registry. The generic generator transform preserves nested class
/// references for variant/important raw candidates, as Tailwind's plugin API does.
pub fn register_typography(
    system: &mut DesignSystem,
    version: &str,
    options: &TypographyOptions,
    resolved_modifiers: Option<&[TypographyModifier]>,
) -> Result<Vec<UtilityId>, TypographyError> {
    let registration = generate_typography(
        version,
        options,
        system.theme().prefix.as_deref(),
        resolved_modifiers,
    )?;
    let mut next = system.clone();
    next.variants_mut()
        .register_custom_variants(&registration.variants)
        .map_err(|e| error(e.message))?;
    let mut ids = Vec::new();
    for definition in registration.utilities {
        ids.push(
            next.register_utility_with_transform(definition, candidate_transform)
                .map_err(error)?,
        );
    }
    *system = next;
    Ok(ids)
}
/// Tailwind plugin-api.ts::replaceNestedClassNameReferences. Prefixing occurs
/// at registration time; the match here is the exact original utility name.
pub fn candidate_transform(
    candidate: &Candidate,
    nodes: &[AstNode],
    _theme: &Theme,
) -> Result<Vec<AstNode>, String> {
    let CandidateKind::Static { root } = &candidate.kind else {
        return Err("Typography generators only accept static candidates".into());
    };
    let mut output = nodes.to_vec();
    rewrite_rules(&mut output, &|name| {
        (name == root).then(|| candidate.raw.clone())
    })
    .map_err(|e| e.message)?;
    Ok(output)
}
fn validate_css(
    nodes: &[AstNode],
    depth: usize,
    budget: &mut (usize, usize),
) -> Result<(), TypographyError> {
    if depth > 128 {
        return Err(error("Typography CSS nesting limit exceeded"));
    }
    for node in nodes {
        budget.0 += 1;
        budget.1 = budget.1.saturating_add(match &node.kind {
            NodeKind::Declaration {
                property,
                value: Some(value),
                ..
            } => property.len() + value.len(),
            NodeKind::Declaration { value: None, .. } => {
                return Err(error("Unresolved Typography CSS declaration"));
            }
            NodeKind::Rule { selector, .. } => selector.len(),
            NodeKind::AtRule { name, params, .. } => name.len() + params.len(),
            NodeKind::Comment { value } => value.len(),
            NodeKind::Context { .. } | NodeKind::AtRoot { .. } => {
                return Err(error(
                    "Typography resolved CSS must not contain compiler-only context nodes",
                ));
            }
        });
        if budget.0 > 100_000 || budget.1 > 16 * 1024 * 1024 {
            return Err(error("Typography CSS exceeds input bounds"));
        }
        if let Some(children) = node.children() {
            validate_css(children, depth + 1, budget)?;
        }
    }
    Ok(())
}
fn in_where(selector: &str, class_name: &str, modifier: &str) -> Result<String, TypographyError> {
    let selector_prefix = if selector.starts_with('>') {
        format!(
            ".{} ",
            if modifier == "DEFAULT" {
                class_name.into()
            } else {
                format!("{class_name}-{modifier}")
            }
        )
    } else {
        String::new()
    };
    let (pseudo, selector) = trailing_pseudos(selector)?;
    // Tailwind 4 plugin prefix() is intentionally identity. The opt-out class
    // remains not-prose even when utility candidates use a theme prefix.
    Ok(format!(
        ":where({selector_prefix}{selector}):not(:where([class~=\"not-{class_name}\"],[class~=\"not-{class_name}\"] *)){pseudo}"
    ))
}

fn trailing_pseudos(selector: &str) -> Result<(String, String), TypographyError> {
    if selector.len() > 64 * 1024 {
        return Err(error("Typography selector exceeds bounds"));
    }
    let branches = segment(selector, ',');
    let mut parsed = branches
        .iter()
        .map(|branch| split_pseudos(branch))
        .collect::<Result<Vec<_>, _>>()?;
    let mut common = Vec::new();
    loop {
        let Some(first) = parsed
            .first()
            .and_then(|(_, pseudos)| pseudos.last())
            .cloned()
        else {
            break;
        };
        // Preserve the pinned selector helper's sparse-column behavior: rows
        // end at their last present pseudo; a hole before that point prevents
        // extraction, while trailing absent entries are not part of the row.
        let last = parsed
            .iter()
            .rposition(|(_, pseudos)| !pseudos.is_empty())
            .unwrap();
        if !parsed[..=last].iter().all(|(_, pseudos)| {
            pseudos
                .last()
                .is_some_and(|s| pseudo_name(s) == pseudo_name(&first))
        }) {
            break;
        }
        common.push(first);
        for (_, pseudos) in &mut parsed[..=last] {
            pseudos.pop();
        }
    }
    if common.is_empty() {
        return Ok((String::new(), selector.into()));
    }
    common.reverse();
    Ok((
        common.concat(),
        parsed
            .into_iter()
            .map(|(base, pseudos)| format!("{base}{}", pseudos.concat()))
            .collect::<Vec<_>>()
            .join(","),
    ))
}
fn pseudo_name(pseudo: &str) -> &str {
    pseudo.split_once('(').map_or(pseudo, |(name, _)| name)
}
fn split_pseudos(selector: &str) -> Result<(String, Vec<String>), TypographyError> {
    let bytes = selector.as_bytes();
    let mut top = Vec::new();
    let mut stack = Vec::new();
    let mut quote = None;
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        let char_len = selector[i..].chars().next().unwrap().len_utf8();
        if quote.is_none() && selector[i..].starts_with("/*") {
            let end = selector[i + 2..]
                .find("*/")
                .ok_or_else(|| error("Unterminated Typography selector comment"))?;
            i += end + 4;
            continue;
        }
        if c == b'\\' {
            i = escape_end(selector, i);
            continue;
        }
        if let Some(q) = quote {
            if c == q {
                quote = None;
            }
            i += char_len;
            continue;
        }
        if matches!(c, b'\'' | b'"') {
            quote = Some(c);
            i += 1;
            continue;
        }
        match c {
            b'(' | b'[' => stack.push(c),
            b')' | b']' => {
                if stack.pop() != Some(if c == b')' { b'(' } else { b'[' }) {
                    return Err(error("Unbalanced Typography selector"));
                }
            }
            b':' if stack.is_empty() && bytes.get(i + 1) == Some(&b':') => {
                top.push(i);
                i += 1;
            }
            _ => {}
        }
        i += char_len;
    }
    if quote.is_some() || !stack.is_empty() {
        return Err(error("Unbalanced Typography selector"));
    }
    let mut end = selector.trim_end().len();
    let mut pseudos = Vec::new();
    for start in top.into_iter().rev() {
        let piece = &selector[start..end];
        let name_end = piece[2..]
            .find(|c: char| !(c.is_alphanumeric() || c == '-' || c == '_' || c == '\\'))
            .map_or(piece.len(), |n| n + 2);
        if name_end == 2
            || !(name_end == piece.len()
                || (piece.as_bytes().get(name_end) == Some(&b'(') && piece.ends_with(')')))
        {
            break;
        }
        pseudos.push(piece.to_string());
        end = start;
    }
    pseudos.reverse();
    Ok((selector[..end].into(), pseudos))
}
fn escape_end(value: &str, start: usize) -> usize {
    let mut end = start + 1;
    let bytes = value.as_bytes();
    while end < bytes.len() && end < start + 7 && bytes[end].is_ascii_hexdigit() {
        end += 1;
    }
    if end > start + 1 {
        if bytes.get(end).is_some_and(u8::is_ascii_whitespace) {
            end += 1;
        }
    } else if let Some(c) = value[end..].chars().next() {
        end += c.len_utf8();
    }
    end
}
fn rewrite_rules(
    nodes: &mut [AstNode],
    transform: &impl Fn(&str) -> Option<String>,
) -> Result<(), TypographyError> {
    for node in nodes {
        if let NodeKind::Rule { selector, .. } = &mut node.kind {
            *selector = rewrite_classes(selector, transform)?;
        }
        if let Some(children) = node.children_mut() {
            rewrite_rules(children, transform)?;
        }
    }
    Ok(())
}
fn rewrite_classes(
    selector: &str,
    transform: &impl Fn(&str) -> Option<String>,
) -> Result<String, TypographyError> {
    if selector.len() > 64 * 1024 {
        return Err(error("Typography selector exceeds bounds"));
    }
    let mut output = String::new();
    let mut i = 0;
    let mut attribute = false;
    let mut quote = None;
    while i < selector.len() {
        let c = selector[i..].chars().next().unwrap();
        if quote.is_none() && selector[i..].starts_with("/*") {
            let end = selector[i + 2..]
                .find("*/")
                .ok_or_else(|| error("Unterminated Typography selector comment"))?
                + i
                + 4;
            output.push_str(&selector[i..end]);
            i = end;
            continue;
        }
        if c == '\\' {
            let end = escape_end(selector, i);
            output.push_str(&selector[i..end]);
            i = end;
            continue;
        }
        if let Some(q) = quote {
            if c == q {
                quote = None;
            }
        } else if c == '\'' || c == '"' {
            quote = Some(c);
        } else if c == '[' {
            attribute = true;
        } else if c == ']' {
            attribute = false;
        } else if c == '.' && !attribute {
            let start = i + 1;
            let mut end = start;
            while end < selector.len() {
                let next = selector[end..].chars().next().unwrap();
                if next == '\\' {
                    end = escape_end(selector, end);
                } else if next.is_alphanumeric() || next == '-' || next == '_' || !next.is_ascii() {
                    end += next.len_utf8();
                } else {
                    break;
                }
            }
            if end > start {
                output.push('.');
                let old = &selector[start..end];
                if let Some(new) = transform(&unescape_identifier(old)) {
                    output.push_str(&escape_identifier(&new));
                } else {
                    output.push_str(old);
                }
                i = end;
                continue;
            }
        }
        output.push(c);
        i += c.len_utf8();
    }
    Ok(output)
}

#[cfg(test)]
#[path = "typography_tests.rs"]
mod tests;
