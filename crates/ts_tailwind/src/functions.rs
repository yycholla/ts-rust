//! CSS function substitution from Tailwind CSS v4.2.1 `css-functions.ts`,
//! `design-system.ts::resolveThemeValue`, and `utilities.ts::withAlpha`.
//! MIT, copyright Tailwind Labs. Sources: reference/directives/.
//!
//! Both entry points are transactional and preserve original source ranges.
//! `substitute_functions` works on stylesheet or candidate-body ASTs, before
//! structural lowering. Native Theme CSS keys are supported; JavaScript config
//! compatibility paths explicitly return Unsupported rather than guessed values.
//! Returned trees retain CSS variable references for downstream usage tracking.
use crate::{
    ast::{AstNode, NodeKind, SourceLocation},
    theme::{Theme, ThemeOptions},
    value::{ValueNode, parse_value, segment, value_to_css},
};
use serde::{Deserialize, Serialize};

const MAX_NODES: usize = 100_000;
const MAX_BYTES: usize = 16 * 1024 * 1024;
const MAX_DEPTH: usize = 256;
const MAX_SUBSTITUTIONS: usize = 10_000;

/// Connect the same native substitution pass to candidate compilation.
pub fn candidate_transform(
    ast: &[AstNode],
    theme: &Theme,
) -> Result<Vec<AstNode>, crate::design_system::CandidateTransformError> {
    substitute_functions(ast, theme)
        .map(|result| result.ast)
        .map_err(|error| {
            use crate::design_system::{CandidateTransformError, CandidateTransformErrorKind};
            CandidateTransformError {
                kind: match error.kind {
                    FunctionErrorKind::Invalid => CandidateTransformErrorKind::Invalid,
                    FunctionErrorKind::Unsupported => CandidateTransformErrorKind::Unsupported,
                    FunctionErrorKind::Limit => CandidateTransformErrorKind::Limit,
                },
                message: error.to_string(),
            }
        })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FunctionErrorKind {
    Invalid,
    Unsupported,
    Limit,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FunctionError {
    pub kind: FunctionErrorKind,
    pub message: String,
    pub source: Option<SourceLocation>,
}
impl std::fmt::Display for FunctionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(src) = &self.source {
            write!(f, "{}:{}: ", src.file, src.start)?;
        }
        write!(f, "{}", self.message)
    }
}
impl std::error::Error for FunctionError {}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FunctionSubstitution {
    pub ast: Vec<AstNode>,
    /// Mirrors the upstream textual invocation check, even inside quoted text.
    pub has_theme_functions: bool,
}
fn fail(kind: FunctionErrorKind, message: impl Into<String>, source: &AstNode) -> FunctionError {
    FunctionError {
        kind,
        message: message.into(),
        source: source.src.clone(),
    }
}
fn invocation(value: &str) -> bool {
    ["--spacing(", "--alpha(", "--theme(", "theme("]
        .iter()
        .any(|s| value.contains(s))
}
fn supported(name: &str) -> bool {
    matches!(name, "--spacing" | "--alpha" | "--theme" | "theme")
}

pub fn substitute_functions(
    ast: &[AstNode],
    theme: &Theme,
) -> Result<FunctionSubstitution, FunctionError> {
    // Bound caller-created trees before recursive Clone or traversal.
    let mut pending = ast.iter().map(|n| (n, 0)).collect::<Vec<_>>();
    let mut count = 0;
    let mut bytes = 0;
    while let Some((node, depth)) = pending.pop() {
        count += 1;
        bytes += match &node.kind {
            NodeKind::Declaration {
                property, value, ..
            } => property.len() + value.as_ref().map_or(0, String::len),
            NodeKind::AtRule { name, params, .. } => name.len() + params.len(),
            NodeKind::Rule { selector, .. } => selector.len(),
            NodeKind::Comment { value } => value.len(),
            NodeKind::Context { context, .. } => context
                .iter()
                .map(|(k, v)| {
                    k.len()
                        + match v {
                            crate::ast::ContextValue::String(s) => s.len(),
                            crate::ast::ContextValue::Bool(_) => 1,
                        }
                })
                .sum(),
            NodeKind::AtRoot { .. } => 0,
        };
        if count > MAX_NODES || bytes > MAX_BYTES || depth > MAX_DEPTH {
            return Err(fail(
                FunctionErrorKind::Limit,
                "CSS function input exceeds bounds",
                node,
            ));
        }
        if let Some(children) = node.children() {
            pending.extend(children.iter().map(|n| (n, depth + 1)));
        }
    }
    fn walk(
        nodes: &mut [AstNode],
        state: &mut State<'_>,
        found: &mut bool,
    ) -> Result<(), FunctionError> {
        for node in nodes {
            let input = match &node.kind {
                NodeKind::Declaration {
                    value: Some(value), ..
                } if invocation(value) => Some(value.as_str()),
                NodeKind::AtRule { name, params, .. }
                    if matches!(
                        name.as_str(),
                        "@media" | "@custom-media" | "@container" | "@supports"
                    ) && invocation(params) =>
                {
                    Some(params.as_str())
                }
                _ => None,
            };
            if let Some(input) = input {
                *found = true;
                let result = state.value(input, node)?;
                match &mut node.kind {
                    NodeKind::Declaration { value, .. } => *value = Some(result),
                    NodeKind::AtRule { params, .. } => *params = result,
                    _ => unreachable!(),
                }
            }
            if let Some(children) = node.children_mut() {
                walk(children, state, found)?;
            }
        }
        Ok(())
    }
    let mut result = FunctionSubstitution {
        ast: ast.to_vec(),
        has_theme_functions: false,
    };
    let mut state = State::new(theme);
    walk(&mut result.ast, &mut state, &mut result.has_theme_functions)?;
    Ok(result)
}

pub fn substitute_functions_in_value(
    value: &str,
    source: &AstNode,
    theme: &Theme,
) -> Result<String, FunctionError> {
    State::new(theme).value(value, source)
}
struct State<'a> {
    theme: &'a Theme,
    substitutions: usize,
    bytes: usize,
    visited: Vec<String>,
}
impl<'a> State<'a> {
    fn new(theme: &'a Theme) -> Self {
        Self {
            theme,
            substitutions: 0,
            bytes: 0,
            visited: vec![],
        }
    }
    fn account(&mut self, value: &str, source: &AstNode) -> Result<(), FunctionError> {
        self.bytes = self.bytes.saturating_add(value.len());
        if self.bytes > MAX_BYTES {
            return Err(fail(
                FunctionErrorKind::Limit,
                "CSS function expansion exceeds byte limit",
                source,
            ));
        }
        Ok(())
    }
    fn value(&mut self, value: &str, source: &AstNode) -> Result<String, FunctionError> {
        self.account(value, source)?;
        let nodes = parse_value(value).map_err(|e| fail(FunctionErrorKind::Invalid, e, source))?;
        let nodes = self.visit(nodes, source, 0)?;
        let result = value_to_css(&nodes);
        self.account(&result, source)?;
        Ok(result)
    }
    fn visit(
        &mut self,
        nodes: Vec<ValueNode>,
        source: &AstNode,
        depth: usize,
    ) -> Result<Vec<ValueNode>, FunctionError> {
        if depth > MAX_DEPTH {
            return Err(fail(
                FunctionErrorKind::Limit,
                "CSS function expansion nesting exceeds limit",
                source,
            ));
        }
        let mut output = vec![];
        for node in nodes {
            match node {
                ValueNode::Function { value, nodes } if supported(&value) => {
                    self.substitutions += 1;
                    if self.substitutions > MAX_SUBSTITUTIONS {
                        return Err(fail(
                            FunctionErrorKind::Limit,
                            "CSS function substitution count exceeds limit",
                            source,
                        ));
                    }
                    let args = value_to_css(&nodes);
                    let signature = format!("{value}({args})");
                    if self.visited.contains(&signature) {
                        return Err(fail(
                            FunctionErrorKind::Invalid,
                            format!("Circular CSS theme function: {signature}"),
                            source,
                        ));
                    }
                    self.visited.push(signature);
                    let args = segment(args.trim(), ',')
                        .into_iter()
                        .map(str::trim)
                        .collect::<Vec<_>>();
                    let replacement = self.call(&value, &args, source)?;
                    self.account(&replacement, source)?;
                    let parsed = parse_value(&replacement).map_err(|e| {
                        fail(
                            FunctionErrorKind::Invalid,
                            format!("Invalid resolved theme value: {e}"),
                            source,
                        )
                    })?;
                    output.extend(self.visit(parsed, source, depth + 1)?);
                    self.visited.pop();
                }
                ValueNode::Function { value, nodes } => output.push(ValueNode::Function {
                    value,
                    nodes: self.visit(nodes, source, depth + 1)?,
                }),
                other => output.push(other),
            }
        }
        Ok(output)
    }
    fn resolve(
        &self,
        path: &str,
        inline: bool,
        source: &AstNode,
    ) -> Result<Option<String>, FunctionError> {
        let (path, modifier) = path
            .rsplit_once('/')
            .map_or((path, None), |(path, modifier)| {
                (path.trim(), Some(modifier.trim()))
            });
        if !path.starts_with("--") {
            return Err(fail(
                FunctionErrorKind::Unsupported,
                format!(
                    "Legacy JavaScript config theme path {path:?} requires a compatibility theme resolver; only native CSS theme keys are available"
                ),
                source,
            ));
        }
        let result = self
            .theme
            .resolve(
                None,
                &[path],
                if inline {
                    ThemeOptions::INLINE
                } else {
                    ThemeOptions::NONE
                },
            )
            .filter(|s| !s.is_empty());
        match (result, modifier.filter(|m| !m.is_empty())) {
            (Some(value), Some(alpha)) => Ok(Some(with_alpha(&value, alpha, source)?)),
            (result, _) => Ok(result),
        }
    }
    fn call(&self, name: &str, args: &[&str], source: &AstNode) -> Result<String, FunctionError> {
        let first = args.first().copied().unwrap_or("");
        let fallback = &args[1..];
        let invalid = |s| fail(FunctionErrorKind::Invalid, s, source);
        match name {
            "--spacing" => {
                if first.is_empty() {
                    return Err(invalid(
                        "The --spacing(…) function requires an argument, but received none.".into(),
                    ));
                }
                if !fallback.is_empty() {
                    return Err(invalid(format!(
                        "The --spacing(…) function only accepts a single argument, but received {}.",
                        args.len()
                    )));
                }
                let multiplier = self.theme.resolve(None, &["--spacing"], ThemeOptions::NONE).filter(|s| !s.is_empty()).ok_or_else(|| invalid("The --spacing(…) function requires that the `--spacing` theme variable exists, but it was not found.".into()))?;
                Ok(format!("calc({multiplier} * {first})"))
            }
            "--alpha" => {
                let parts = segment(first, '/');
                let color = parts.first().map_or("", |s| s.trim());
                let alpha = parts.get(1).map_or("", |s| s.trim());
                if color.is_empty() || alpha.is_empty() {
                    return Err(invalid("The --alpha(…) function requires a color and an alpha value, e.g.: `--alpha(var(--my-color) / 50%)`".into()));
                }
                if !fallback.is_empty() {
                    return Err(invalid(
                        "The --alpha(…) function only accepts one argument".into(),
                    ));
                }
                with_alpha(color, alpha, source)
            }
            "--theme" => {
                if !first.starts_with("--") {
                    return Err(invalid("The --theme(…) function can only be used with CSS variables from your theme.".into()));
                }
                let (path, explicit_inline) = first
                    .strip_suffix(" inline")
                    .map_or((first, false), |s| (s, true));
                let resolved = self.resolve(
                    path,
                    explicit_inline || matches!(source.kind, NodeKind::AtRule { .. }),
                    source,
                )?;
                let Some(resolved) = resolved else {
                    return if fallback.is_empty() {
                        Err(invalid(format!(
                            "Could not resolve value for theme function: `theme({path})`. Consider checking if the variable name is correct or provide a fallback value to silence this error."
                        )))
                    } else {
                        Ok(fallback.join(", "))
                    };
                };
                if fallback.is_empty() {
                    return Ok(resolved);
                }
                let fallback = fallback.join(", ");
                if fallback == "initial" {
                    return Ok(resolved);
                }
                if resolved == "initial" {
                    return Ok(fallback);
                }
                if ["var(", "theme(", "--theme("]
                    .iter()
                    .any(|s| resolved.starts_with(s))
                {
                    let mut nodes = parse_value(&resolved)
                        .map_err(|e| invalid(format!("Invalid resolved theme value: {e}")))?;
                    inject_fallback(&mut nodes, &fallback);
                    return Ok(value_to_css(&nodes));
                }
                Ok(resolved)
            }
            "theme" => {
                let path = unquote(first);
                match self.resolve(&path, true, source)? {
                    Some(value) => Ok(value),
                    None if !fallback.is_empty() => Ok(fallback.join(", ")),
                    None => Err(invalid(format!(
                        "Could not resolve value for theme function: `theme({path})`. Consider checking if the path is correct or provide a fallback value to silence this error."
                    ))),
                }
            }
            _ => unreachable!(),
        }
    }
}
fn unquote(value: &str) -> String {
    let mut chars = value.chars();
    let Some(quote @ ('\'' | '"')) = chars.next() else {
        return value.into();
    };
    let mut chars = chars.collect::<Vec<_>>();
    chars.pop();
    let mut output = String::new();
    let mut iter = chars.into_iter().peekable();
    while let Some(c) = iter.next() {
        if c == '\\'
            && iter
                .peek()
                .is_some_and(|next| *next == quote || *next == '\\')
        {
            output.push(iter.next().unwrap());
        } else {
            output.push(c);
        }
    }
    output
}
fn inject_fallback(nodes: &mut [ValueNode], fallback: &str) {
    for node in nodes {
        if let ValueNode::Function { value, nodes } = node {
            if matches!(value.as_str(), "var" | "theme" | "--theme") {
                if nodes.len() == 1 {
                    nodes.push(ValueNode::Word {
                        value: format!(", {fallback}"),
                    });
                } else if let Some(ValueNode::Word { value }) = nodes.last_mut() {
                    if value == "initial" {
                        *value = fallback.into();
                    }
                }
            }
            inject_fallback(nodes, fallback);
        }
    }
}
fn with_alpha(value: &str, alpha: &str, source: &AstNode) -> Result<String, FunctionError> {
    let alpha = match js_number(alpha, source)? {
        Some(number) => format!("{}%", number_string(number * 100.0)),
        None => alpha.into(),
    };
    Ok(if alpha == "100%" {
        value.into()
    } else {
        format!("color-mix(in oklab, {value} {alpha}, transparent)")
    })
}
// Number(string) rather than CSS numeric validation: hexadecimal, binary,
// octal, Infinity and empty strings have defined JavaScript conversions.
fn js_number(input: &str, source: &AstNode) -> Result<Option<f64>, FunctionError> {
    let input = input.trim_matches(|c: char| c.is_whitespace() || c == '\u{feff}');
    if input.is_empty() {
        return Ok(Some(0.0));
    }
    for (prefixes, radix) in [(["0x", "0X"], 16), (["0o", "0O"], 8), (["0b", "0B"], 2)] {
        if let Some(digits) = prefixes
            .iter()
            .find_map(|prefix| input.strip_prefix(prefix))
        {
            if digits.is_empty() || !digits.chars().all(|c| c.is_digit(radix)) {
                return Ok(None);
            }
            let value = u64::from_str_radix(digits, radix).map_err(|_| {
                fail(
                    FunctionErrorKind::Unsupported,
                    "Alpha radix literal exceeds supported exact conversion range",
                    source,
                )
            })?;
            return Ok(Some(value as f64));
        }
    }
    if matches!(input, "Infinity" | "+Infinity") {
        return Ok(Some(f64::INFINITY));
    }
    if input == "-Infinity" {
        return Ok(Some(f64::NEG_INFINITY));
    }
    if input
        .chars()
        .any(|c| !matches!(c, '0'..='9' | '.' | 'e' | 'E' | '+' | '-'))
    {
        return Ok(None);
    }
    Ok(input.parse::<f64>().ok().filter(|n| !n.is_nan()))
}
fn number_string(n: f64) -> String {
    if n == 0.0 {
        return "0".into();
    }
    if n == f64::INFINITY {
        return "Infinity".into();
    }
    if n == f64::NEG_INFINITY {
        return "-Infinity".into();
    }
    if n.abs() >= 1e21 || n.abs() < 1e-6 {
        let exponent = format!("{n:e}");
        let (mantissa, exponent) = exponent.split_once('e').unwrap();
        format!(
            "{mantissa}e{}{exponent}",
            if exponent.starts_with('-') { "" } else { "+" }
        )
    } else {
        n.to_string()
    }
}

#[cfg(test)]
#[path = "functions_tests.rs"]
mod tests;
