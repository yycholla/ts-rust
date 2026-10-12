//! Tailwind CSS 4.2.1 core variants, ported from reference/variants.ts (MIT,
//! copyright Tailwind Labs) and utils/compare-breakpoints.ts. Pure AST transforms.
//! Register variants before parsing candidates; apply rightmost source variant
//! first, as Candidate.variants already records. Utilities and the final class
//! rule are separate. AtRoot/property nodes are real compiler IR for lowering.
//! Custom selector and @slot templates register through transactional batches.
use crate::ast::{AstNode, NodeKind};
use crate::candidate::{
    CandidateRegistry, Compounds, Modifier, Variant, VariantKind, compounds_for_selectors,
};
use crate::theme::Theme;
use crate::value::{is_valid_arbitrary, segment};
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::BTreeMap;

#[path = "custom_variants.rs"]
mod custom_variants;
pub use custom_variants::{CustomVariantBody, CustomVariantDefinition};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum VariantErrorKind {
    Invalid,
    Unsupported,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VariantError {
    pub kind: VariantErrorKind,
    pub message: String,
}
impl std::fmt::Display for VariantError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for VariantError {}
fn invalid(message: impl Into<String>) -> VariantError {
    VariantError {
        kind: VariantErrorKind::Invalid,
        message: message.into(),
    }
}
fn unsupported(message: impl Into<String>) -> VariantError {
    VariantError {
        kind: VariantErrorKind::Unsupported,
        message: message.into(),
    }
}
type Result<T> = std::result::Result<T, VariantError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum VariantComparison {
    Lexical,
    BreakpointAscending,
    BreakpointDescending,
    ContainerAscending,
    ContainerDescending,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VariantMetadata {
    pub kind: VariantKind,
    pub order: usize,
    pub compounds: Compounds,
    pub compounds_with: Compounds,
    pub comparison: VariantComparison,
}
#[derive(Clone)]
enum Generator {
    Selectors(Vec<String>),
    Template(Vec<AstNode>),
    Hover,
    Content(String),
    Functional,
    Compound,
    Breakpoint(String),
}
#[derive(Clone)]
struct Definition {
    metadata: VariantMetadata,
    generator: Generator,
}
#[derive(Clone)]
pub struct Variants {
    theme: Theme,
    definitions: BTreeMap<String, Definition>,
    insertion: Vec<String>,
    last_order: usize,
}

#[derive(Clone)]
enum Wrapper {
    Style(String),
    At(String, String),
}
#[derive(Clone)]
struct Branch {
    wrappers: Vec<Wrapper>,
    before: Vec<AstNode>,
}
impl Branch {
    fn single(wrapper: Wrapper) -> Self {
        Self {
            wrappers: vec![wrapper],
            before: vec![],
        }
    }
}
fn wrapper(selector: &str) -> Wrapper {
    if selector.starts_with('@') {
        let (name, params) = selector
            .split_once(char::is_whitespace)
            .unwrap_or((selector, ""));
        Wrapper::At(name.into(), params.trim_start().into())
    } else {
        Wrapper::Style(selector.into())
    }
}
fn modifier(value: &Modifier) -> &str {
    match value {
        Modifier::Named { value } | Modifier::Arbitrary { value } => value,
    }
}
fn root(variant: &Variant) -> Option<&str> {
    match variant {
        Variant::Static { root }
        | Variant::Functional { root, .. }
        | Variant::Compound { root, .. } => Some(root),
        _ => None,
    }
}

impl Variants {
    fn add(
        &mut self,
        name: &str,
        kind: VariantKind,
        generator: Generator,
        compounds: Compounds,
        with: Compounds,
        group: Option<(usize, VariantComparison)>,
    ) {
        if let Some(existing) = self.definitions.get_mut(name) {
            existing.generator = generator;
            existing.metadata.kind = kind;
            existing.metadata.compounds = compounds;
            return;
        }
        let (order, comparison) =
            group.unwrap_or((self.last_order + 1, VariantComparison::Lexical));
        self.last_order = order;
        self.insertion.push(name.into());
        self.definitions.insert(
            name.into(),
            Definition {
                metadata: VariantMetadata {
                    kind,
                    order,
                    compounds,
                    compounds_with: with,
                    comparison,
                },
                generator,
            },
        );
    }
    fn static_variant(&mut self, name: &str, selectors: &[&str]) {
        self.add(
            name,
            VariantKind::Static,
            Generator::Selectors(selectors.iter().map(|s| (*s).into()).collect()),
            compounds_for_selectors(selectors),
            Compounds::NEVER,
            None,
        );
    }
    fn compound(&mut self, name: &str, with: Compounds) {
        self.add(
            name,
            VariantKind::Compound,
            Generator::Compound,
            Compounds::STYLE_RULES,
            with,
            None,
        );
    }
    fn functional(&mut self, name: &str, compounds: Compounds) {
        self.add(
            name,
            VariantKind::Functional,
            Generator::Functional,
            compounds,
            Compounds::NEVER,
            None,
        );
    }
    pub fn new(theme: &Theme) -> Self {
        let mut this = Self {
            theme: theme.clone(),
            definitions: BTreeMap::new(),
            insertion: vec![],
            last_order: 0,
        };
        for (name, selector) in [("*", ":is(& > *)"), ("**", ":is(& *)")] {
            this.add(
                name,
                VariantKind::Static,
                Generator::Selectors(vec![selector.into()]),
                Compounds::NEVER,
                Compounds::NEVER,
                None,
            );
        }
        this.compound("not", Compounds::ALL);
        this.compound("group", Compounds::STYLE_RULES);
        this.compound("peer", Compounds::STYLE_RULES);
        this.static_variant("first-letter", &["&::first-letter"]);
        this.static_variant("first-line", &["&::first-line"]);
        this.static_variant(
            "marker",
            &[
                "& *::marker",
                "&::marker",
                "& *::-webkit-details-marker",
                "&::-webkit-details-marker",
            ],
        );
        this.static_variant("selection", &["& *::selection", "&::selection"]);
        for (name, selector) in [
            ("file", "&::file-selector-button"),
            ("placeholder", "&::placeholder"),
            ("backdrop", "&::backdrop"),
            ("details-content", "&::details-content"),
        ] {
            this.static_variant(name, &[selector]);
        }
        for name in ["before", "after"] {
            this.add(
                name,
                VariantKind::Static,
                Generator::Content(name.into()),
                Compounds::NEVER,
                Compounds::NEVER,
                None,
            );
        }
        for (name, selector) in [
            ("first", "&:first-child"),
            ("last", "&:last-child"),
            ("only", "&:only-child"),
            ("odd", "&:nth-child(odd)"),
            ("even", "&:nth-child(even)"),
            ("first-of-type", "&:first-of-type"),
            ("last-of-type", "&:last-of-type"),
            ("only-of-type", "&:only-of-type"),
            ("visited", "&:visited"),
            ("target", "&:target"),
            ("open", "&:is([open], :popover-open, :open)"),
        ] {
            this.static_variant(name, &[selector]);
        }
        for name in [
            "default",
            "checked",
            "indeterminate",
            "placeholder-shown",
            "autofill",
            "optional",
            "required",
            "valid",
            "invalid",
            "user-valid",
            "user-invalid",
            "in-range",
            "out-of-range",
            "read-only",
            "empty",
            "focus-within",
        ] {
            this.static_variant(name, &[&format!("&:{name}")]);
        }
        this.add(
            "hover",
            VariantKind::Static,
            Generator::Hover,
            Compounds::STYLE_RULES,
            Compounds::NEVER,
            None,
        );
        for name in ["focus", "focus-visible", "active", "enabled", "disabled"] {
            this.static_variant(name, &[&format!("&:{name}")]);
        }
        this.static_variant("inert", &["&:is([inert], [inert] *)"]);
        this.compound("in", Compounds::STYLE_RULES);
        this.compound("has", Compounds::STYLE_RULES);
        for name in [
            "aria",
            "data",
            "nth",
            "nth-last",
            "nth-of-type",
            "nth-last-of-type",
        ] {
            this.functional(name, Compounds::STYLE_RULES);
        }
        this.functional("supports", Compounds::AT_RULES);
        for (name, selector) in [
            (
                "motion-safe",
                "@media (prefers-reduced-motion: no-preference)",
            ),
            ("motion-reduce", "@media (prefers-reduced-motion: reduce)"),
            ("contrast-more", "@media (prefers-contrast: more)"),
            ("contrast-less", "@media (prefers-contrast: less)"),
        ] {
            this.static_variant(name, &[selector]);
        }
        let group = this.last_order + 1;
        this.add(
            "max",
            VariantKind::Functional,
            Generator::Functional,
            Compounds::AT_RULES,
            Compounds::NEVER,
            Some((group, VariantComparison::BreakpointDescending)),
        );
        let group = this.last_order + 1;
        for (key, value) in theme.namespace("--breakpoint") {
            if let Some(key) = key {
                this.add(
                    &key,
                    VariantKind::Static,
                    Generator::Breakpoint(value),
                    Compounds::AT_RULES,
                    Compounds::NEVER,
                    Some((group, VariantComparison::BreakpointAscending)),
                );
            }
        }
        this.add(
            "min",
            VariantKind::Functional,
            Generator::Functional,
            Compounds::AT_RULES,
            Compounds::NEVER,
            Some((group, VariantComparison::BreakpointAscending)),
        );
        let group = this.last_order + 1;
        this.add(
            "@max",
            VariantKind::Functional,
            Generator::Functional,
            Compounds::AT_RULES,
            Compounds::NEVER,
            Some((group, VariantComparison::ContainerDescending)),
        );
        let group = this.last_order + 1;
        for name in ["@", "@min"] {
            this.add(
                name,
                VariantKind::Functional,
                Generator::Functional,
                Compounds::AT_RULES,
                Compounds::NEVER,
                Some((group, VariantComparison::ContainerAscending)),
            );
        }
        for (name, selector) in [
            ("portrait", "@media (orientation: portrait)"),
            ("landscape", "@media (orientation: landscape)"),
            ("ltr", "&:where(:dir(ltr), [dir=\"ltr\"], [dir=\"ltr\"] *)"),
            ("rtl", "&:where(:dir(rtl), [dir=\"rtl\"], [dir=\"rtl\"] *)"),
            ("dark", "@media (prefers-color-scheme: dark)"),
            ("starting", "@starting-style"),
            ("print", "@media print"),
            ("forced-colors", "@media (forced-colors: active)"),
            ("inverted-colors", "@media (inverted-colors: inverted)"),
            ("pointer-none", "@media (pointer: none)"),
            ("pointer-coarse", "@media (pointer: coarse)"),
            ("pointer-fine", "@media (pointer: fine)"),
            ("any-pointer-none", "@media (any-pointer: none)"),
            ("any-pointer-coarse", "@media (any-pointer: coarse)"),
            ("any-pointer-fine", "@media (any-pointer: fine)"),
            ("noscript", "@media (scripting: none)"),
        ] {
            this.static_variant(name, &[selector]);
        }
        this
    }
    pub fn metadata(&self, name: &str) -> Option<&VariantMetadata> {
        self.definitions.get(name).map(|d| &d.metadata)
    }
    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.insertion.iter().map(String::as_str)
    }
    pub fn register_candidates(&self, registry: &mut CandidateRegistry) {
        for name in &self.insertion {
            let m = &self.definitions[name].metadata;
            registry.register_variant(name, m.kind, m.compounds, m.compounds_with);
        }
    }
    pub fn apply(&self, variant: &Variant, nodes: Vec<AstNode>) -> Result<Vec<AstNode>> {
        self.apply_inner(variant, nodes, 0)
    }
    fn apply_inner(
        &self,
        variant: &Variant,
        nodes: Vec<AstNode>,
        depth: usize,
    ) -> Result<Vec<AstNode>> {
        if depth > 128 {
            return Err(invalid("Variant nesting limit exceeded"));
        }
        if let Variant::Static { root } = variant {
            if let Some(Definition {
                generator: Generator::Template(template),
                ..
            }) = self.definitions.get(root)
            {
                return self.apply_custom_template(template, nodes, depth + 1);
            }
        }
        if let Variant::Compound {
            root,
            modifier,
            variant: child,
        } = variant
        {
            if matches!(root.as_str(), "group" | "peer" | "has" | "in")
                && self.contains_custom_template(child)
            {
                return self.apply_custom_compound(
                    root,
                    modifier.as_ref(),
                    child,
                    nodes,
                    depth + 1,
                );
            }
        }
        let branches = self.branches(variant, depth)?;
        Ok(branches
            .into_iter()
            .map(|branch| {
                let mut body = branch.before;
                body.extend(nodes.clone());
                for wrapper in branch.wrappers.into_iter().rev() {
                    body = vec![AstNode::new(match wrapper {
                        Wrapper::Style(selector) => NodeKind::Rule {
                            selector,
                            nodes: body,
                        },
                        Wrapper::At(name, params) => NodeKind::AtRule {
                            name,
                            params,
                            nodes: body,
                        },
                    })];
                }
                body
            })
            .flatten()
            .collect())
    }
    fn width(&self, variant: &Variant, container: bool) -> Option<String> {
        let ns = if container {
            "--container"
        } else {
            "--breakpoint"
        };
        let value = match variant {
            Variant::Static { root } if !container => self
                .theme
                .resolve_value(Some(root), &[ns])
                .map(str::to_owned),
            Variant::Functional {
                value: Some(value),
                modifier,
                ..
            } if container || modifier.is_none() => match value {
                Modifier::Arbitrary { value } => Some(value.clone()),
                Modifier::Named { value } => self
                    .theme
                    .resolve_value(Some(value), &[ns])
                    .map(str::to_owned),
            },
            _ => None,
        }?;
        (!value.is_empty() && !value.contains("var(")).then_some(value)
    }
    fn branches(&self, variant: &Variant, depth: usize) -> Result<Vec<Branch>> {
        if depth > 128 {
            return Err(invalid("Variant nesting limit exceeded"));
        }
        if let Variant::Arbitrary { selector, relative } = variant {
            if *relative && depth == 0 {
                return Err(invalid(
                    "Relative arbitrary variants require a compound parent",
                ));
            }
            if selector.is_empty() || !is_valid_arbitrary(selector) {
                return Err(invalid("Invalid arbitrary selector"));
            }
            return Ok(vec![Branch::single(wrapper(selector))]);
        }
        let name = root(variant).unwrap();
        let definition = self
            .definitions
            .get(name)
            .ok_or_else(|| unsupported(format!("Unregistered/custom variant {name}")))?;
        let expected = match variant {
            Variant::Static { .. } => VariantKind::Static,
            Variant::Functional { .. } => VariantKind::Functional,
            Variant::Compound { .. } => VariantKind::Compound,
            _ => unreachable!(),
        };
        if definition.metadata.kind != expected {
            return Err(invalid("Variant kind disagrees with its registration"));
        }
        match &definition.generator {
            Generator::Template(template) => self.custom_template_branches(template, depth + 1),
            Generator::Selectors(selectors) => Ok(selectors
                .iter()
                .map(|s| Branch::single(wrapper(s)))
                .collect()),
            Generator::Hover => Ok(vec![Branch {
                wrappers: vec![
                    Wrapper::Style("&:hover".into()),
                    Wrapper::At("@media".into(), "(hover: hover)".into()),
                ],
                before: vec![],
            }]),
            Generator::Breakpoint(value) => Ok(vec![Branch::single(Wrapper::At(
                "@media".into(),
                format!("(width >= {value})"),
            ))]),
            Generator::Content(which) => Ok(vec![Branch {
                wrappers: vec![Wrapper::Style(format!("&::{which}"))],
                before: vec![
                    AstNode::new(NodeKind::AtRoot {
                        nodes: vec![AstNode::new(NodeKind::AtRule {
                            name: "@property".into(),
                            params: "--tw-content".into(),
                            nodes: vec![
                                AstNode::declaration("syntax", "\"*\""),
                                AstNode::declaration("initial-value", "\"\""),
                                AstNode::declaration("inherits", "false"),
                            ],
                        })],
                    }),
                    AstNode::declaration("content", "var(--tw-content)"),
                ],
            }]),
            Generator::Functional => self.functional_branches(variant),
            Generator::Compound => {
                let Variant::Compound {
                    modifier,
                    variant: child,
                    ..
                } = variant
                else {
                    unreachable!()
                };
                let compounds = match child.as_ref() {
                    Variant::Arbitrary { selector, .. } => compounds_for_selectors(&[selector]),
                    _ => self
                        .metadata(root(child).unwrap())
                        .map(|m| m.compounds)
                        .ok_or_else(|| unsupported("Unregistered compound child"))?,
                };
                if definition.metadata.compounds_with.0 & compounds.0 == 0 {
                    return Err(invalid("Variant cannot compound with this child"));
                }
                if matches!(name, "not" | "group" | "peer")
                    && matches!(child.as_ref(), Variant::Arbitrary { relative: true, .. })
                {
                    return Err(invalid("Compound variant rejects relative selector"));
                }
                if modifier.is_some() && !matches!(name, "group" | "peer") {
                    return Err(invalid("Compound modifier is not supported"));
                }
                let branches = self.branches(child, depth + 1)?;
                self.compound_branches(name, modifier.as_ref(), branches)
            }
        }
    }
    fn functional_branches(&self, variant: &Variant) -> Result<Vec<Branch>> {
        let Variant::Functional {
            root,
            value: Some(value),
            modifier: modif,
        } = variant
        else {
            return Err(invalid("Functional variant requires a value"));
        };
        if modif.is_some() && !matches!(root.as_str(), "@" | "@min" | "@max") {
            return Err(invalid("Variant does not accept a modifier"));
        }
        let value_text = modifier(value);
        if value_text.is_empty() || !is_valid_arbitrary(value_text) {
            return Err(invalid("Invalid variant value"));
        }
        let wrap = match root.as_str() {
            "aria" => Wrapper::Style(if matches!(value, Modifier::Arbitrary { .. }) {
                format!("&[aria-{}]", quote_attribute(value_text))
            } else {
                format!("&[aria-{value_text}=\"true\"]")
            }),
            "data" => Wrapper::Style(format!("&[data-{}]", quote_attribute(value_text))),
            "nth" | "nth-last" | "nth-of-type" | "nth-last-of-type" => {
                if matches!(value, Modifier::Named { .. })
                    && !value_text.parse::<f64>().is_ok_and(|number| {
                        number.is_finite()
                            && number >= 0.0
                            && number < 1e21
                            && number.fract() == 0.0
                            && number.to_string() == value_text
                    })
                {
                    return Err(invalid(
                        "Named nth value must be a nonnegative canonical integer",
                    ));
                }
                let pseudo = match root.as_str() {
                    "nth" => "nth-child",
                    "nth-last" => "nth-last-child",
                    other => other,
                };
                Wrapper::Style(format!("&:{pseudo}({value_text})"))
            }
            "supports" => {
                let function_prefix = value_text.split_once('(').is_some_and(|(prefix, _)| {
                    prefix
                        .trim_end()
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
                });
                let query = if function_prefix {
                    space_conditions(value_text)
                } else {
                    let value = if value_text.contains(':') {
                        value_text.to_owned()
                    } else {
                        format!("{value_text}: var(--tw)")
                    };
                    if value.starts_with('(') && value.ends_with(')') {
                        value
                    } else {
                        format!("({value})")
                    }
                };
                Wrapper::At("@supports".into(), query)
            }
            "min" | "max" | "@" | "@min" | "@max" => {
                let container = root.starts_with('@');
                let value = self.width(variant, container).ok_or_else(|| {
                    invalid("Breakpoint/container value is unresolved or contains var()")
                })?;
                let op = if root.ends_with("max") { "<" } else { ">=" };
                let prefix = modif
                    .as_ref()
                    .map(|m| format!("{} ", modifier(m)))
                    .unwrap_or_default();
                Wrapper::At(
                    if container { "@container" } else { "@media" }.into(),
                    format!("{prefix}(width {op} {value})"),
                )
            }
            _ => {
                return Err(unsupported(format!(
                    "Unsupported functional variant {root}"
                )));
            }
        };
        Ok(vec![Branch::single(wrap)])
    }
    fn compound_branches(
        &self,
        name: &str,
        modif: Option<&Modifier>,
        branches: Vec<Branch>,
    ) -> Result<Vec<Branch>> {
        // compile.ts applies compound functions to each isolated root node.
        // Negating sibling roots (OR) requires a different tree and is rejected.
        if name == "not" && branches.len() > 1 {
            return Err(invalid("not variant cannot negate sibling conditions"));
        }
        let mut output = vec![];
        for mut branch in branches {
            let styles = branch
                .wrappers
                .iter()
                .filter(|w| matches!(w, Wrapper::Style(_)))
                .count();
            let ats = branch.wrappers.len() - styles;
            if styles > 1 || !branch.before.is_empty() {
                return Err(invalid(
                    "Compound variant cannot contain nested style rules",
                ));
            }
            if name == "not" {
                if ats > 1 {
                    return Err(invalid("not variant cannot negate nested conditions"));
                }
                // De Morgan: not(style AND condition) -> not(style) OR not(condition).
                for wrap in branch.wrappers {
                    let negated = match wrap {
                        Wrapper::Style(selector) => {
                            if selector.contains("::") {
                                return Err(invalid("Pseudo-elements cannot be negated"));
                            }
                            Wrapper::Style(format!(
                                "&:not({})",
                                segment(&selector, ',')
                                    .iter()
                                    .map(|s| s.replace('&', "*"))
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            ))
                        }
                        Wrapper::At(rule, params) => {
                            if !matches!(rule.as_str(), "@media" | "@supports" | "@container")
                                || segment(&params, ',').len() != 1
                            {
                                return Err(invalid("Conditional rule cannot be negated"));
                            }
                            Wrapper::At(rule.clone(), negate_condition(&rule, &params)?)
                        }
                    };
                    output.push(Branch::single(negated));
                }
                continue;
            }
            if styles == 0 {
                return Err(invalid("Compound variant requires a style selector"));
            }
            for wrap in &mut branch.wrappers {
                let Wrapper::Style(selector) = wrap else {
                    continue;
                };
                *selector = match name {
                    "in" => format!(":where({}) &", selector.replace('&', "*")),
                    "has" => format!("&:has({})", selector.replace('&', "*")),
                    "group" | "peer" => {
                        let prefix = self
                            .theme
                            .prefix
                            .as_ref()
                            .map(|s| format!("{s}\\:"))
                            .unwrap_or_default();
                        let suffix = modif
                            .map(|m| format!("\\/{}", modifier(m)))
                            .unwrap_or_default();
                        let mut replacement =
                            selector.replace('&', &format!(":where(.{prefix}{name}{suffix})"));
                        if segment(&replacement, ',').len() > 1 {
                            replacement = format!(":is({replacement})");
                        }
                        format!(
                            "&:is({replacement} {})",
                            if name == "peer" { "~ *" } else { "*" }
                        )
                    }
                    _ => {
                        return Err(unsupported(
                            "Custom compound variants require design-system registration",
                        ));
                    }
                };
            }
            output.push(branch);
        }
        if output.is_empty() {
            return Err(invalid("Compound variant produced no conditions"));
        }
        Ok(output)
    }
    pub fn compare(&self, a: Option<&Variant>, z: Option<&Variant>) -> Result<Ordering> {
        if a == z {
            return Ok(Ordering::Equal);
        }
        let (Some(a), Some(z)) = (a, z) else {
            return Ok(if a.is_none() {
                Ordering::Less
            } else {
                Ordering::Greater
            });
        };
        match (a, z) {
            (Variant::Arbitrary { selector: a, .. }, Variant::Arbitrary { selector: z, .. }) => {
                return Ok(lexical(a, z));
            }
            (Variant::Arbitrary { .. }, _) => return Ok(Ordering::Greater),
            (_, Variant::Arbitrary { .. }) => return Ok(Ordering::Less),
            _ => {}
        }
        let ar = root(a).unwrap();
        let zr = root(z).unwrap();
        let am = self
            .metadata(ar)
            .ok_or_else(|| unsupported(format!("Unknown variant {ar}")))?;
        let zm = self
            .metadata(zr)
            .ok_or_else(|| unsupported(format!("Unknown variant {zr}")))?;
        let order = am.order.cmp(&zm.order);
        if order != Ordering::Equal {
            return Ok(order);
        }
        if let (
            Variant::Compound {
                variant: a,
                modifier: ma,
                ..
            },
            Variant::Compound {
                variant: z,
                modifier: mz,
                ..
            },
        ) = (a, z)
        {
            let order = self.compare(Some(a), Some(z))?;
            return Ok(if order != Ordering::Equal {
                order
            } else {
                compare_modifier(ma.as_ref(), mz.as_ref(), false)
            });
        }
        if am.comparison != VariantComparison::Lexical {
            let container = matches!(
                am.comparison,
                VariantComparison::ContainerAscending | VariantComparison::ContainerDescending
            );
            let descending = matches!(
                am.comparison,
                VariantComparison::BreakpointDescending | VariantComparison::ContainerDescending
            );
            return Ok(match (self.width(a, container), self.width(z, container)) {
                (Some(a), Some(z)) => compare_breakpoints(&a, &z, descending),
                (None, None) => Ordering::Equal,
                (None, _) => {
                    if descending {
                        Ordering::Greater
                    } else {
                        Ordering::Less
                    }
                }
                (_, None) => {
                    if descending {
                        Ordering::Less
                    } else {
                        Ordering::Greater
                    }
                }
            });
        }
        if ar != zr {
            return Ok(lexical(ar, zr));
        }
        if let (Variant::Functional { value: a, .. }, Variant::Functional { value: z, .. }) = (a, z)
        {
            return Ok(compare_modifier(a.as_ref(), z.as_ref(), true));
        }
        Ok(Ordering::Equal)
    }
}

fn lexical(a: &str, z: &str) -> Ordering {
    a.encode_utf16().cmp(z.encode_utf16())
}
fn compare_modifier(a: Option<&Modifier>, z: Option<&Modifier>, arbitrary_last: bool) -> Ordering {
    match (a, z) {
        (None, None) => Ordering::Equal,
        (None, _) => Ordering::Less,
        (_, None) => Ordering::Greater,
        (Some(a), Some(z)) => {
            if arbitrary_last {
                let order = matches!(a, Modifier::Arbitrary { .. })
                    .cmp(&matches!(z, Modifier::Arbitrary { .. }));
                if order != Ordering::Equal {
                    return order;
                }
            }
            lexical(modifier(a), modifier(z))
        }
    }
}
fn compare_breakpoints(a: &str, z: &str, descending: bool) -> Ordering {
    fn bucket(s: &str) -> String {
        s.split_once('(')
            .map(|(p, _)| p.to_owned())
            .unwrap_or_else(|| {
                s.chars()
                    .filter(|c| !c.is_ascii_digit() && *c != '.')
                    .collect()
            })
    }
    fn number(s: &str) -> Option<f64> {
        let s = s.trim_start();
        let end = s
            .char_indices()
            .take_while(|(i, c)| c.is_ascii_digit() || (*i == 0 && matches!(c, '+' | '-')))
            .last()
            .map(|(i, c)| i + c.len_utf8())?;
        s[..end].parse().ok()
    }
    let order = lexical(&bucket(a), &bucket(z));
    if order != Ordering::Equal {
        return order;
    }
    match (number(a), number(z)) {
        (Some(a), Some(z)) => if descending {
            z.partial_cmp(&a)
        } else {
            a.partial_cmp(&z)
        }
        .unwrap_or(Ordering::Equal),
        _ => lexical(a, z),
    }
}
fn quote_attribute(input: &str) -> String {
    if !input.contains('=') {
        return input.into();
    }
    let parts = segment(input, '=');
    let attribute = parts[0];
    let value = parts[1..].join("=");
    let value = value.trim();
    if value.starts_with(['\'', '"']) {
        return input.into();
    }
    if value.len() > 1 {
        let bytes = value.as_bytes();
        let flag = bytes[bytes.len() - 1];
        if bytes[bytes.len() - 2] == b' ' && matches!(flag, b'i' | b'I' | b's' | b'S') {
            return format!(
                "{attribute}=\"{}\" {}",
                &value[..value.len() - 2],
                flag as char
            );
        }
    }
    format!("{attribute}=\"{value}\"")
}
fn space_conditions(value: &str) -> String {
    let mut out = String::new();
    let mut token = String::new();
    fn flush(token: &mut String, out: &mut String) {
        if matches!(token.as_str(), "and" | "or" | "not") {
            out.push(' ');
            out.push_str(token);
            out.push(' ');
        } else {
            out.push_str(token);
        }
        token.clear();
    }
    for c in value.chars() {
        if c.is_ascii_alphanumeric() || c == '_' {
            token.push(c);
        } else {
            flush(&mut token, &mut out);
            out.push(c);
        }
    }
    flush(&mut token, &mut out);
    out
}
fn negate_condition(rule: &str, params: &str) -> Result<String> {
    let params = params.trim();
    let parts = segment(params, ' ');
    if parts[0] == "not" {
        return Ok(parts[1..].join(" "));
    }
    if rule == "@container" && !parts[0].starts_with('(') {
        if parts.len() < 2 {
            return Err(invalid("Named container is missing its condition"));
        }
        return Ok(if parts[1] == "not" {
            format!("{} {}", parts[0], parts[2..].join(" "))
        } else {
            format!("{} not {}", parts[0], parts[1..].join(" "))
        });
    }
    Ok(format!("not {params}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::candidate::parse_variant;
    use crate::theme::ThemeOptions;
    fn setup() -> (Variants, CandidateRegistry) {
        let mut theme = Theme::default();
        for (key, value) in [
            ("--breakpoint-sm", "40rem"),
            ("--breakpoint-lg", "64rem"),
            ("--container-sm", "24rem"),
        ] {
            theme.add(key, value, ThemeOptions::NONE, None).unwrap();
        }
        let variants = Variants::new(&theme);
        let mut registry = CandidateRegistry::default();
        variants.register_candidates(&mut registry);
        (variants, registry)
    }
    fn apply(name: &str) -> Vec<AstNode> {
        let (v, r) = setup();
        v.apply(
            &parse_variant(name, &r).unwrap(),
            vec![AstNode::declaration("color", "red")],
        )
        .unwrap()
    }
    fn style(node: &AstNode) -> &str {
        let NodeKind::Rule { selector, .. } = &node.kind else {
            panic!("Expected style rule")
        };
        selector
    }
    fn at(node: &AstNode) -> (&str, &str) {
        let NodeKind::AtRule { name, params, .. } = &node.kind else {
            panic!("Expected at-rule")
        };
        (name, params)
    }
    #[test]
    fn static_pseudo_media_and_content_are_real_nodes() {
        let hover = apply("hover");
        assert_eq!(style(&hover[0]), "&:hover");
        assert_eq!(
            at(&hover[0].children().unwrap()[0]),
            ("@media", "(hover: hover)")
        );
        assert_eq!(apply("marker").len(), 4);
        let before = apply("before");
        assert_eq!(style(&before[0]), "&::before");
        assert!(matches!(
            before[0].children().unwrap()[0].kind,
            NodeKind::AtRoot { .. }
        ));
        assert_eq!(
            at(&apply("dark")[0]),
            ("@media", "(prefers-color-scheme: dark)")
        );
    }
    #[test]
    fn group_peer_has_and_in_preserve_conditional_wrappers() {
        assert_eq!(
            style(&apply("group-hover/menu")[0]),
            "&:is(:where(.group\\/menu):hover *)"
        );
        assert_eq!(
            style(&apply("peer-checked")[0]),
            "&:is(:where(.peer):checked ~ *)"
        );
        assert_eq!(style(&apply("has-[>img]")[0]), "&:has(>img)");
        assert_eq!(style(&apply("in-focus")[0]), ":where(*:focus) &");
        assert_eq!(
            at(&apply("group-hover")[0].children().unwrap()[0]),
            ("@media", "(hover: hover)")
        );
    }
    #[test]
    fn not_hover_uses_de_morgan_union_and_conditional_negation() {
        let result = apply("not-hover");
        assert_eq!(result.len(), 2);
        assert_eq!(style(&result[0]), "&:not(*:hover)");
        assert_eq!(at(&result[1]), ("@media", "not (hover: hover)"));
        assert_eq!(
            at(&apply("not-@sm/sidebar")[0]),
            ("@container", "sidebar not (width >= 24rem)")
        );
    }
    #[test]
    fn functional_attributes_supports_and_nth_match_selectors() {
        assert_eq!(style(&apply("aria-checked")[0]), "&[aria-checked=\"true\"]");
        assert_eq!(
            style(&apply("data-[state=open_i]")[0]),
            "&[data-state=\"open\" i]"
        );
        assert_eq!(style(&apply("nth-[2n+1]")[0]), "&:nth-child(2n+1)");
        assert_eq!(
            at(&apply("supports-[display:grid]")[0]),
            ("@supports", "(display:grid)")
        );
        assert_eq!(
            at(&apply("supports-[display]")[0]),
            ("@supports", "(display: var(--tw))")
        );
    }
    #[test]
    fn breakpoints_container_conditions_and_grouped_sort_are_exact() {
        assert_eq!(at(&apply("sm")[0]), ("@media", "(width >= 40rem)"));
        assert_eq!(at(&apply("max-lg")[0]), ("@media", "(width < 64rem)"));
        assert_eq!(
            at(&apply("@sm/main")[0]),
            ("@container", "main (width >= 24rem)")
        );
        let (v, r) = setup();
        let cmp = |a, z| {
            v.compare(parse_variant(a, &r).as_ref(), parse_variant(z, &r).as_ref())
                .unwrap()
        };
        assert_eq!(cmp("sm", "lg"), Ordering::Less);
        assert_eq!(cmp("max-sm", "max-lg"), Ordering::Greater);
        assert_eq!(cmp("sm", "min-sm"), Ordering::Equal);
        assert_eq!(cmp("@sm", "@min-sm"), Ordering::Equal);
        assert_eq!(
            compare_breakpoints("40.5rem", "40.9rem", false),
            Ordering::Equal
        );
        assert_eq!(compare_breakpoints("10px", "1rem", true), Ordering::Less);
    }
    #[test]
    fn prefix_selector_lists_and_compounds_leave_utility_descendants_alone() {
        let mut theme = Theme::default();
        theme.prefix = Some("tw".into());
        let variants = Variants::new(&theme);
        let mut registry = CandidateRegistry::default();
        variants.register_candidates(&mut registry);
        let nested = AstNode::new(NodeKind::Rule {
            selector: "& > .child".into(),
            nodes: vec![AstNode::declaration("color", "red")],
        });
        let candidate = parse_variant("group-[&:focus,&:active]/menu", &registry).unwrap();
        let output = variants.apply(&candidate, vec![nested.clone()]).unwrap();
        assert_eq!(
            style(&output[0]),
            "&:is(:is(:where(.tw\\:group\\/menu):focus,:where(.tw\\:group\\/menu):active) *)"
        );
        assert_eq!(output[0].children().unwrap()[0], nested);
        let error = variants
            .apply(
                &parse_variant("group-not-hover", &registry).unwrap(),
                vec![],
            )
            .unwrap_err();
        assert_eq!(error.kind, VariantErrorKind::Invalid);
    }
    #[test]
    fn relative_modifiers_and_missing_theme_are_invalid_not_empty_css() {
        let (v, r) = setup();
        for name in [
            "[>img]",
            "data",
            "nth-01",
            "min-unknown",
            "min-[var(--size)]",
            "aria-checked/foo",
        ] {
            if let Some(candidate) = parse_variant(name, &r) {
                assert_eq!(
                    v.apply(&candidate, vec![]).unwrap_err().kind,
                    VariantErrorKind::Invalid
                );
            }
        }
        assert!(parse_variant("group-before", &r).is_none());
        assert!(parse_variant("group-[>img]", &r).is_some());
        assert!(
            v.apply(&parse_variant("group-[>img]", &r).unwrap(), vec![])
                .is_err()
        );
        assert_eq!(
            v.apply(
                &Variant::Static {
                    root: "custom".into()
                },
                vec![]
            )
            .unwrap_err()
            .kind,
            VariantErrorKind::Unsupported
        );
    }
}
