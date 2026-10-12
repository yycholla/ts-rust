//! Tailwind CSS 4.2.1 `optimizeAst` color-mix compatibility fallback pass.
//! Ported from the MIT-licensed pinned sources in color_mix_reference/.
//!
//! This is Tailwind's variable-aware AST transformation, not a color evaluator:
//! constant mixes and resolved srgb mixes remain for the downstream CSS optimizer.
//! Run on the retained tree after theme/keyframe pruning and AtRoot insertion,
//! before property fallback insertion and final printing. Inputs are immutable.
use crate::{
    ast::{AstNode, ContextValue, NodeKind, SourceLocation},
    theme::Theme,
    value::{ValueNode, parse_value, value_to_css},
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ColorMixResult {
    pub ast: Vec<AstNode>,
    pub fallback_declarations: usize,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ColorMixErrorKind {
    Invalid,
    Limit,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ColorMixError {
    pub kind: ColorMixErrorKind,
    pub message: String,
    pub source: Option<SourceLocation>,
}
impl std::fmt::Display for ColorMixError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for ColorMixError {}

const SUPPORTS: &str = "(color: color-mix(in lab, red, red))";
const MAX_STEPS: usize = 4_000_000;
struct State<'a> {
    theme: &'a Theme,
    steps: usize,
    fallbacks: usize,
    source: Option<SourceLocation>,
}
impl State<'_> {
    fn fail(&self, kind: ColorMixErrorKind, message: impl Into<String>) -> ColorMixError {
        ColorMixError {
            kind,
            message: message.into(),
            source: self.source.clone(),
        }
    }
    fn tick(&mut self, depth: usize) -> Result<(), ColorMixError> {
        self.steps += 1;
        if depth > 256 || self.steps > MAX_STEPS {
            return Err(self.fail(
                ColorMixErrorKind::Limit,
                "Color-mix compatibility traversal exceeds bounds",
            ));
        }
        Ok(())
    }
    fn parse(&self, value: &str) -> Result<Vec<ValueNode>, ColorMixError> {
        if value.len() > 16 * 1024 * 1024 {
            return Err(self.fail(
                ColorMixErrorKind::Limit,
                "Color-mix declaration exceeds byte limit",
            ));
        }
        parse_value(value).map_err(|e| self.fail(ColorMixErrorKind::Invalid, e))
    }
    fn ast(
        &mut self,
        nodes: &[AstNode],
        context: &BTreeMap<String, ContextValue>,
        depth: usize,
    ) -> Result<Vec<AstNode>, ColorMixError> {
        let mut output = Vec::with_capacity(nodes.len());
        for node in nodes {
            self.source = node.src.clone();
            self.tick(depth)?;
            if let NodeKind::Declaration {
                value: Some(value), ..
            } = &node.kind
            {
                if value.contains("color-mix(")
                    && !truthy(context, "keyframes")
                    && !truthy(context, "supportsColorMix")
                    && !truthy(context, "reference")
                {
                    let mut values = self.parse(value)?;
                    let mut required = false;
                    self.mixes(&mut values, &mut required, 0)?;
                    if required {
                        let mut fallback = node.clone();
                        if let NodeKind::Declaration { value, .. } = &mut fallback.kind {
                            *value = Some(value_to_css(&values));
                        }
                        let mut supports = AstNode::new(NodeKind::AtRule {
                            name: "@supports".into(),
                            params: SUPPORTS.into(),
                            nodes: vec![node.clone()],
                        });
                        supports.src = node.src.clone();
                        output.push(fallback);
                        output.push(supports);
                        self.fallbacks += 1;
                        continue;
                    }
                }
            }
            let mut copied = node.clone();
            let mut context = context.clone();
            match &node.kind {
                NodeKind::Context { context: own, .. } => context.extend(own.clone()),
                NodeKind::AtRule { name, params, .. } => {
                    if name == "@keyframes" {
                        context.insert("keyframes".into(), ContextValue::Bool(true));
                    } else if name == "@supports" && params.contains("color-mix(") {
                        context.insert("supportsColorMix".into(), ContextValue::Bool(true));
                    }
                }
                _ => {}
            }
            if let Some(children) = copied.children_mut() {
                *children = self.ast(children, &context, depth + 1)?;
            }
            output.push(copied);
        }
        Ok(output)
    }
    /// The outer upstream walk re-enters replacement nodes. The declaration-wide
    /// required flag intentionally carries between sibling/nested mixes.
    fn mixes(
        &mut self,
        nodes: &mut [ValueNode],
        required: &mut bool,
        depth: usize,
    ) -> Result<(), ColorMixError> {
        for node in nodes {
            loop {
                self.tick(depth)?;
                let ValueNode::Function {
                    value,
                    nodes: children,
                } = node
                else {
                    break;
                };
                if value == "color-mix" {
                    let mut unresolved = false;
                    let mut currentcolor = false;
                    self.inline(
                        children,
                        required,
                        &mut unresolved,
                        &mut currentcolor,
                        depth + 1,
                    )?;
                    if unresolved || currentcolor {
                        let replacement=children.iter().position(|n|matches!(n,ValueNode::Separator{value} if value.trim().contains(',')))
                            .and_then(|index|children.get(index+1)).cloned();
                        if let Some(replacement) = replacement {
                            *node = replacement;
                            continue;
                        }
                    } else if *required {
                        // Match the pinned positional token contract: "in",
                        // separator, colorspace. Do not parse or rewrite hue modes.
                        match children.get_mut(2) {
                            Some(ValueNode::Word { value })
                                if matches!(value.as_str(), "oklab" | "oklch" | "lab" | "lch") =>
                            {
                                *value = "srgb".into()
                            }
                            Some(_) => {}
                            None => {
                                return Err(self.fail(
                                    ColorMixErrorKind::Invalid,
                                    "Color-mix fallback lacks its colorspace token",
                                ));
                            }
                        }
                    }
                }
                self.mixes(children, required, depth + 1)?;
                break;
            }
        }
        Ok(())
    }
    /// Inline variables at every depth within one mix. An unresolved variable
    /// retains its original fallback syntax; it does not choose the var fallback.
    fn inline(
        &mut self,
        nodes: &mut [ValueNode],
        required: &mut bool,
        unresolved: &mut bool,
        currentcolor: &mut bool,
        depth: usize,
    ) -> Result<(), ColorMixError> {
        for node in nodes {
            self.tick(depth)?;
            if matches!(node,ValueNode::Word{value} if value.eq_ignore_ascii_case("currentcolor")) {
                *currentcolor = true;
                *required = true;
            }
            let mut current = node.clone();
            let mut seen = BTreeSet::new();
            let replacement = loop {
                let ValueNode::Function {
                    value,
                    nodes: children,
                } = &current
                else {
                    break None;
                };
                if value != "var" {
                    break None;
                }
                let Some(ValueNode::Word { value: name }) = children.first() else {
                    break None;
                };
                if !seen.insert(name.clone()) {
                    *unresolved = true;
                    break None;
                }
                self.tick(depth)?;
                *required = true;
                let Some(color) = self
                    .theme
                    .resolve_value(None, &[name.as_str()])
                    .filter(|s| !s.is_empty())
                else {
                    *unresolved = true;
                    break None;
                };
                if color.eq_ignore_ascii_case("currentcolor") {
                    *currentcolor = true;
                    break None;
                }
                if color.starts_with("var(") {
                    let parsed = self.parse(color)?;
                    let Some(first) = parsed.into_iter().next() else {
                        break None;
                    };
                    current = first;
                } else {
                    break Some(color.to_owned());
                }
            };
            if let Some(value) = replacement {
                // Upstream replaces with an opaque word, even if the theme value
                // contains CSS functions. It does not recursively parse that word.
                *node = ValueNode::Word { value };
                continue;
            }
            if let ValueNode::Function {
                nodes: children, ..
            } = node
            {
                self.inline(children, required, unresolved, currentcolor, depth + 1)?;
            }
        }
        Ok(())
    }
}
fn truthy(context: &BTreeMap<String, ContextValue>, key: &str) -> bool {
    match context.get(key) {
        Some(ContextValue::Bool(v)) => *v,
        Some(ContextValue::String(v)) => !v.is_empty(),
        None => false,
    }
}
/// Applies exactly the Tailwind color-mix fallback operation. It preserves
/// declaration importance/source ranges and nesting, does not mutate Theme, and
/// never returns a partially transformed tree on invalid input or limits.
pub fn color_mix_compatibility(
    ast: &[AstNode],
    theme: &Theme,
) -> Result<ColorMixResult, ColorMixError> {
    let mut state = State {
        theme,
        steps: 0,
        fallbacks: 0,
        source: None,
    };
    let ast = state.ast(ast, &BTreeMap::new(), 0)?;
    Ok(ColorMixResult {
        ast,
        fallback_declarations: state.fallbacks,
    })
}

#[cfg(test)]
#[path = "color_mix_tests.rs"]
mod tests;
