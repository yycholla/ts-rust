//! Tailwind CSS 4.2.1 built-in utility generation. Variant wrapping and global
//! AST lowering belong to the compiler. Unimplemented registrations fail closed.
use crate::{
    ast::{AstNode, NodeKind},
    candidate::{Candidate, CandidateKind, CandidateRegistry, Modifier, UtilityValue},
    theme::{Theme, ThemeOptions},
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
#[path = "utility_colors.rs"]
mod colors;
#[path = "utility_effects.rs"]
mod effects;
#[path = "utility_families.rs"]
mod families;
#[path = "utility_filters.rs"]
mod filters;
#[path = "utility_gradients.rs"]
mod gradients;
#[path = "utility_masks.rs"]
mod masks;
#[path = "utility_structural.rs"]
mod structural;
#[path = "utility_values.rs"]
pub mod values;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GeneratedUtility {
    pub nodes: Vec<AstNode>,
    pub property_order: Vec<usize>,
    pub declaration_count: usize,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "camelCase")]
pub enum UtilityResult {
    Generated { utility: GeneratedUtility },
    InvalidCandidate,
    Unsupported { root: String, reason: String },
}
#[derive(Debug, Clone, Copy)]
enum Bare {
    None,
    Integer,
    Spacing,
    Pixels,
    Percent,
    IntegerPercent,
    FontStretch,
    Milliseconds,
    Degrees,
    Grid,
    Aspect,
}
#[derive(Debug, Clone)]
struct Spec {
    properties: Vec<String>,
    themes: Vec<String>,
    bare: Bare,
    negative: bool,
    fractions: bool,
    default: Option<String>,
    theme_default: bool,
    fallback: BTreeMap<String, String>,
    handler: Handler,
}
#[derive(Debug, Clone, Copy)]
enum Handler {
    Plain,
    Size,
    Leading,
    Tracking,
    Duration,
    Ease,
    Translate,
    ScaleAxis,
    RotateAxis,
    Skew,
    BorderSpacing,
    Space,
    LineClamp,
    Content,
    Filter,
}
impl Spec {
    fn new(properties: &[&str], themes: &[&str], bare: Bare) -> Self {
        Self {
            properties: properties.iter().map(|s| s.to_string()).collect(),
            themes: themes.iter().map(|s| s.to_string()).collect(),
            bare,
            negative: false,
            fractions: false,
            default: None,
            theme_default: true,
            fallback: BTreeMap::new(),
            handler: Handler::Plain,
        }
    }
    fn fallback(mut self, pairs: &[(&str, &str)]) -> Self {
        self.fallback
            .extend(pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())));
        self
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Catalog {
    #[serde(rename = "static")]
    statics: BTreeMap<String, Vec<(String, String)>>,
    known_static: BTreeSet<String>,
    known_functional: BTreeSet<String>,
    property_order: Vec<String>,
}
#[derive(Clone)]
pub struct Utilities {
    statics: BTreeMap<String, Vec<AstNode>>,
    specs: BTreeMap<String, Spec>,
    known_static: BTreeSet<String>,
    known_functional: BTreeSet<String>,
    property_order: Vec<String>,
}
impl Default for Utilities {
    fn default() -> Self {
        Self::new()
    }
}
impl Utilities {
    pub fn new() -> Self {
        let c: Catalog = serde_json::from_str(include_str!("utility_catalog.json"))
            .expect("checked-in upstream utility catalog");
        let mut this = Self {
            statics: c
                .statics
                .into_iter()
                .map(|(k, v)| (k, v.into_iter().map(|(p, v)| d(&p, &v)).collect()))
                .collect(),
            specs: BTreeMap::new(),
            known_static: c.known_static,
            known_functional: c.known_functional,
            property_order: c.property_order,
        };
        families::register(&mut this);
        effects::register(&mut this);
        colors::register(&mut this);
        filters::register(&mut this);
        gradients::register(&mut this);
        masks::register(&mut this);
        structural::register(&mut this);
        this
    }
    /// Merge utility names without discarding the caller's variant registrations.
    pub fn register_candidates(&self, registry: &mut CandidateRegistry) {
        registry
            .static_utilities
            .extend(self.known_static.iter().cloned());
        registry
            .functional_utilities
            .extend(self.known_functional.iter().cloned());
    }
    pub fn candidate_registry(&self, theme: &Theme) -> CandidateRegistry {
        let mut r = CandidateRegistry {
            prefix: theme.prefix.clone(),
            ..Default::default()
        };
        self.register_candidates(&mut r);
        r
    }
    fn static_rule(&mut self, name: &str, nodes: Vec<AstNode>) {
        self.known_static.insert(name.into());
        self.statics.insert(name.into(), nodes);
    }
    fn spec(&mut self, name: &str, spec: Spec) {
        self.known_functional.insert(name.into());
        if spec.negative {
            self.known_functional.insert(format!("-{name}"));
        }
        self.specs.insert(name.into(), spec);
    }
    fn spacing(
        &mut self,
        name: &str,
        props: &[&str],
        themes: &[&str],
        negative: bool,
        fractions: bool,
        handler: Handler,
    ) {
        let mut spec = Spec::new(props, themes, Bare::Spacing);
        spec.negative = negative;
        spec.fractions = fractions;
        spec.theme_default = false;
        spec.handler = handler;
        self.static_rule(&format!("{name}-px"), render(&spec, name, "1px"));
        if negative {
            self.static_rule(&format!("-{name}-px"), render(&spec, name, "-1px"));
        }
        self.spec(name, spec);
    }
    pub fn compile_base(&self, candidate: &Candidate, theme: &Theme) -> UtilityResult {
        let root = match &candidate.kind {
            CandidateKind::Arbitrary {
                property,
                value,
                modifier,
            } => {
                let Some(value) = values::as_color(value, modifier.as_ref(), theme) else {
                    return UtilityResult::InvalidCandidate;
                };
                return self.generated(vec![d(property, &value)]);
            }
            CandidateKind::Static { root } => {
                if let Some(nodes) = self.statics.get(root) {
                    return self.generated(nodes.clone());
                }
                if root == "container" {
                    let mut points: Vec<_> = theme
                        .namespace("--breakpoint")
                        .into_iter()
                        .map(|(_, v)| v)
                        .collect();
                    points.sort_by(|a, b| values::compare_breakpoints(a, b));
                    let mut nodes = vec![
                        d("--tw-sort", "--tw-container-component"),
                        d("width", "100%"),
                    ];
                    nodes.extend(points.into_iter().map(|v| {
                        at(
                            "@media",
                            &format!("(width >= {v})"),
                            vec![d("max-width", &v)],
                        )
                    }));
                    return self.generated(nodes);
                }
                if !self.known_static.contains(root) {
                    return UtilityResult::InvalidCandidate;
                }
                root
            }
            CandidateKind::Functional {
                root,
                value,
                modifier,
            } => {
                let negative = root.starts_with('-');
                let name = root.strip_prefix('-').unwrap_or(root);
                if let Some(spec) = self.specs.get(name) {
                    if negative && !spec.negative {
                        return UtilityResult::InvalidCandidate;
                    }
                    let Some(value) =
                        resolve(spec, value.as_ref(), modifier.as_ref(), negative, theme)
                    else {
                        return UtilityResult::InvalidCandidate;
                    };
                    return self.generated(render(spec, name, &value));
                }
                let input = Input {
                    root,
                    value: value.as_ref(),
                    modifier: modifier.as_ref(),
                    theme,
                };
                match colors::compile(&input)
                    .or_else(|| effects::compile(&input))
                    .or_else(|| filters::compile(&input))
                    .or_else(|| gradients::compile(&input))
                    .or_else(|| masks::compile(&input))
                    .or_else(|| structural::compile(&input))
                {
                    Some(Ok(nodes)) => return self.generated(nodes),
                    Some(Err(Problem::Invalid)) => return UtilityResult::InvalidCandidate,
                    None => {}
                }
                if !self.known_functional.contains(root) {
                    return UtilityResult::InvalidCandidate;
                }
                root
            }
        };
        UtilityResult::Unsupported {
            root: root.clone(),
            reason: "This registered Tailwind 4.2.1 utility generator has not been ported".into(),
        }
    }
    fn generated(&self, nodes: Vec<AstNode>) -> UtilityResult {
        let (property_order, declaration_count) = self.property_sort(&nodes);
        UtilityResult::Generated {
            utility: GeneratedUtility {
                nodes,
                property_order,
                declaration_count,
            },
        }
    }
    /// Upstream breadth-first property sorting, excluding hoisted @property and
    /// @keyframes trees. --tw-sort contributes to count and overrides later keys.
    pub fn property_sort(&self, nodes: &[AstNode]) -> (Vec<usize>, usize) {
        let mut queue: VecDeque<_> = nodes.iter().collect();
        let mut order = BTreeSet::new();
        let mut count = 0;
        let mut seen = false;
        while let Some(node) = queue.pop_front() {
            match &node.kind {
                NodeKind::Declaration {
                    property,
                    value: Some(value),
                    ..
                } => {
                    count += 1;
                    if seen {
                        continue;
                    }
                    if property == "--tw-sort" {
                        if let Some(i) = self.property_order.iter().position(|p| p == value) {
                            order.insert(i);
                            seen = true;
                            continue;
                        }
                    }
                    if let Some(i) = self.property_order.iter().position(|p| p == property) {
                        order.insert(i);
                    }
                }
                NodeKind::Rule { nodes, .. } | NodeKind::AtRule { nodes, .. } => {
                    queue.extend(nodes)
                }
                _ => {}
            }
        }
        (order.into_iter().collect(), count)
    }
}
pub(super) fn d(p: &str, v: &str) -> AstNode {
    AstNode::declaration(p, v)
}
pub(super) fn at(name: &str, params: &str, nodes: Vec<AstNode>) -> AstNode {
    AstNode::new(NodeKind::AtRule {
        name: name.into(),
        params: params.into(),
        nodes,
    })
}
pub(super) fn rule(selector: &str, nodes: Vec<AstNode>) -> AstNode {
    AstNode::new(NodeKind::Rule {
        selector: selector.into(),
        nodes,
    })
}
pub(super) fn hoist(nodes: Vec<AstNode>) -> AstNode {
    AstNode::new(NodeKind::AtRoot { nodes })
}
pub(super) fn property(name: &str, initial: Option<&str>, syntax: Option<&str>) -> AstNode {
    let mut nodes = vec![
        d("syntax", &format!("\"{}\"", syntax.unwrap_or("*"))),
        d("inherits", "false"),
    ];
    if let Some(v) = initial.filter(|v| !v.is_empty()) {
        nodes.push(d("initial-value", v));
    }
    at("@property", name, nodes)
}
pub(super) struct Input<'a> {
    root: &'a str,
    value: Option<&'a UtilityValue>,
    modifier: Option<&'a Modifier>,
    theme: &'a Theme,
}
impl Input<'_> {
    fn named(&self) -> Option<&str> {
        match self.value {
            Some(UtilityValue::Named { value, .. }) => Some(value),
            _ => None,
        }
    }
    fn arbitrary(&self) -> Option<(&str, Option<&str>)> {
        match self.value {
            Some(UtilityValue::Arbitrary { value, data_type }) => {
                Some((value, data_type.as_deref()))
            }
            _ => None,
        }
    }
    fn color(&self, keys: &[&str]) -> Option<String> {
        match self.value? {
            UtilityValue::Arbitrary { value, .. } => {
                values::as_color(value, self.modifier, self.theme)
            }
            UtilityValue::Named { value, .. } => {
                values::theme_color(value, keys, self.modifier, self.theme)
            }
        }
    }
}
pub(super) enum Problem {
    Invalid,
}
pub(super) type Compile = Result<Vec<AstNode>, Problem>;
pub(super) fn invalid<T>(value: Option<T>) -> Result<T, Problem> {
    value.ok_or(Problem::Invalid)
}
fn resolve(
    spec: &Spec,
    value: Option<&UtilityValue>,
    modifier: Option<&Modifier>,
    negative: bool,
    theme: &Theme,
) -> Option<String> {
    let keys: Vec<_> = spec.themes.iter().map(String::as_str).collect();
    let mut bare_negative = false;
    let result = match value {
        None => {
            if modifier.is_some() {
                return None;
            }
            spec.default.clone().or_else(|| {
                spec.theme_default
                    .then(|| theme.resolve(None, &keys, ThemeOptions::NONE))
                    .flatten()
            })
        }
        Some(UtilityValue::Arbitrary { value, .. }) => {
            if modifier.is_some() {
                return None;
            }
            Some(value.clone())
        }
        Some(UtilityValue::Named { value, fraction }) => {
            let mut found = theme.resolve(
                Some(fraction.as_deref().unwrap_or(value)),
                &keys,
                ThemeOptions::NONE,
            );
            if found.is_none() && spec.fractions {
                if let Some(fraction) = fraction {
                    let (a, b) = fraction.split_once('/')?;
                    if !values::integer(a) || !values::integer(b) {
                        return None;
                    }
                    found = Some(format!("calc({a} / {b} * 100%)"));
                }
            }
            if found.is_none() {
                found = match spec.bare {
                    Bare::None => None,
                    Bare::Integer if values::integer(value) => Some(value.clone()),
                    Bare::Pixels if values::integer(value) => Some(format!("{value}px")),
                    Bare::IntegerPercent if values::integer(value) => Some(format!("{value}%")),
                    Bare::FontStretch => value
                        .strip_suffix('%')
                        .and_then(|v| v.trim().parse::<f64>().ok())
                        .filter(|n| n.fract() == 0.0 && (50.0..=200.0).contains(n))
                        .map(|_| value.clone()),
                    Bare::Percent if values::quarter(value) => Some(format!("{value}%")),
                    Bare::Milliseconds if values::integer(value) => Some(format!("{value}ms")),
                    Bare::Degrees if values::integer(value) => Some(format!("{value}deg")),
                    Bare::Grid if values::integer(value) && value != "0" => {
                        Some(format!("repeat({value}, minmax(0, 1fr))"))
                    }
                    Bare::Aspect => fraction
                        .as_deref()
                        .and_then(|f| f.split_once('/'))
                        .filter(|(a, b)| values::quarter(a) && values::quarter(b))
                        .map(|_| fraction.clone().unwrap()),
                    Bare::Spacing if values::quarter(value) => {
                        bare_negative = negative;
                        theme
                            .resolve(None, &["--spacing"], ThemeOptions::NONE)
                            .map(|v| {
                                format!("calc({v} * {}{value})", if negative { "-" } else { "" })
                            })
                    }
                    _ => None,
                };
                if found.as_ref().is_some_and(|v| !v.contains('/')) && modifier.is_some() {
                    return None;
                }
            }
            if found.is_none() && !negative && modifier.is_none() {
                found = spec.fallback.get(value).cloned();
            }
            found
        }
    }?;
    Some(if negative && !bare_negative {
        format!("calc({result} * -1)")
    } else {
        result
    })
}
fn render(spec: &Spec, root: &str, value: &str) -> Vec<AstNode> {
    let declarations = || {
        spec.properties
            .iter()
            .map(|p| d(p, value))
            .collect::<Vec<_>>()
    };
    match spec.handler {
        Handler::Filter => filters::render(root, value),
        Handler::Plain => declarations(),
        Handler::Size => vec![
            d("--tw-sort", "size"),
            d("width", value),
            d("height", value),
        ],
        Handler::Leading | Handler::Tracking | Handler::Duration | Handler::Ease => {
            let (var, prop) = match spec.handler {
                Handler::Leading => ("--tw-leading", "line-height"),
                Handler::Tracking => ("--tw-tracking", "letter-spacing"),
                Handler::Duration => ("--tw-duration", "transition-duration"),
                _ => ("--tw-ease", "transition-timing-function"),
            };
            vec![
                hoist(vec![property(var, None, None)]),
                d(var, value),
                d(prop, value),
            ]
        }
        Handler::Translate | Handler::ScaleAxis | Handler::RotateAxis | Handler::Skew => {
            effects::transform(root, value)
        }
        Handler::BorderSpacing => {
            let mut nodes = vec![hoist(vec![
                property("--tw-border-spacing-x", Some("0"), Some("<length>")),
                property("--tw-border-spacing-y", Some("0"), Some("<length>")),
            ])];
            nodes.extend(declarations());
            nodes.push(d(
                "border-spacing",
                "var(--tw-border-spacing-x) var(--tw-border-spacing-y)",
            ));
            nodes
        }
        Handler::Space => {
            let axis = if root == "space-x" { "x" } else { "y" };
            let var = format!("--tw-space-{axis}-reverse");
            let (start, end) = if axis == "x" {
                ("margin-inline-start", "margin-inline-end")
            } else {
                ("margin-block-start", "margin-block-end")
            };
            vec![
                hoist(vec![property(&var, Some("0"), None)]),
                rule(
                    ":where(& > :not(:last-child))",
                    vec![
                        d(
                            "--tw-sort",
                            if axis == "x" { "row-gap" } else { "column-gap" },
                        ),
                        d(&var, "0"),
                        d(start, &format!("calc({value} * var({var}))")),
                        d(end, &format!("calc({value} * calc(1 - var({var})))")),
                    ],
                ),
            ]
        }
        Handler::LineClamp => {
            if value == "none" {
                vec![
                    d("overflow", "visible"),
                    d("display", "block"),
                    d("-webkit-box-orient", "horizontal"),
                    d("-webkit-line-clamp", "unset"),
                ]
            } else {
                vec![
                    d("overflow", "hidden"),
                    d("display", "-webkit-box"),
                    d("-webkit-box-orient", "vertical"),
                    d("-webkit-line-clamp", value),
                ]
            }
        }
        Handler::Content => vec![
            hoist(vec![property("--tw-content", Some("\"\""), None)]),
            d("--tw-content", value),
            d("content", "var(--tw-content)"),
        ],
    }
}

#[cfg(test)]
#[path = "utility_tests.rs"]
mod tests;
