use super::*;
use crate::value::segment;
const TRANSFORM: &str = "var(--tw-rotate-x,) var(--tw-rotate-y,) var(--tw-rotate-z,) var(--tw-skew-x,) var(--tw-skew-y,)";
const SHADOW: &str = "var(--tw-inset-shadow), var(--tw-inset-ring-shadow), var(--tw-ring-offset-shadow), var(--tw-ring-shadow), var(--tw-shadow)";
const COLORS: &str = "color, background-color, border-color, outline-color, text-decoration-color, fill, stroke, --tw-gradient-from, --tw-gradient-via, --tw-gradient-to";
fn props(prefix: &str, initial: Option<&str>) -> AstNode {
    hoist(
        ["x", "y", "z"]
            .iter()
            .map(|axis| property(&format!("--tw-{prefix}-{axis}"), initial, None))
            .collect(),
    )
}
fn transform_properties() -> AstNode {
    hoist(
        ["rotate-x", "rotate-y", "rotate-z", "skew-x", "skew-y"]
            .iter()
            .map(|name| property(&format!("--tw-{name}"), None, None))
            .collect(),
    )
}
fn shadow_properties() -> AstNode {
    hoist(vec![
        property("--tw-shadow", Some("0 0 #0000"), None),
        property("--tw-shadow-color", None, None),
        property("--tw-shadow-alpha", Some("100%"), Some("<percentage>")),
        property("--tw-inset-shadow", Some("0 0 #0000"), None),
        property("--tw-inset-shadow-color", None, None),
        property(
            "--tw-inset-shadow-alpha",
            Some("100%"),
            Some("<percentage>"),
        ),
        property("--tw-ring-color", None, None),
        property("--tw-ring-shadow", Some("0 0 #0000"), None),
        property("--tw-inset-ring-color", None, None),
        property("--tw-inset-ring-shadow", Some("0 0 #0000"), None),
        property("--tw-ring-inset", None, None),
        property("--tw-ring-offset-width", Some("0px"), Some("<length>")),
        property("--tw-ring-offset-color", Some("#fff"), None),
        property("--tw-ring-offset-shadow", Some("0 0 #0000"), None),
    ])
}
fn text_shadow_properties() -> AstNode {
    hoist(vec![
        property("--tw-text-shadow-color", None, None),
        property("--tw-text-shadow-alpha", Some("100%"), Some("<percentage>")),
    ])
}
pub(super) fn register(u: &mut Utilities) {
    for root in ["translate", "translate-x", "translate-y", "translate-z"] {
        u.spacing(
            root,
            &[],
            &["--translate", "--spacing"],
            true,
            root != "translate-z",
            Handler::Translate,
        );
        if root != "translate-z" {
            for (prefix, value) in [("", "100%"), ("-", "-100%")] {
                u.static_rule(&format!("{prefix}{root}-full"), transform(root, value));
            }
        }
    }
    for axis in ["x", "y", "z"] {
        for (prefix, theme, bare, handler) in [
            ("scale", "--scale", Bare::IntegerPercent, Handler::ScaleAxis),
            ("rotate", "--rotate", Bare::Degrees, Handler::RotateAxis),
        ] {
            let root = format!("{prefix}-{axis}");
            let mut spec = Spec::new(&[], &[theme], bare);
            spec.handler = handler;
            spec.negative = true;
            u.spec(&root, spec);
        }
    }
    for root in ["skew", "skew-x", "skew-y"] {
        let mut spec = Spec::new(&[], &["--skew"], Bare::Degrees);
        spec.handler = Handler::Skew;
        spec.negative = true;
        u.spec(root, spec);
    }
    for root in [
        "scale",
        "-scale",
        "rotate",
        "-rotate",
        "transform",
        "transition",
        "flex",
        "@container",
        "ring",
        "inset-ring",
        "ring-offset",
        "shadow",
        "inset-shadow",
        "text-shadow",
    ] {
        u.known_functional.insert(root.into());
    }
    u.static_rule("transform-cpu", vec![d("transform", TRANSFORM)]);
    u.static_rule(
        "transform-gpu",
        vec![d("transform", &format!("translateZ(0) {TRANSFORM}"))],
    );
    u.static_rule(
        "translate-3d",
        vec![
            props("translate", Some("0")),
            d(
                "translate",
                "var(--tw-translate-x) var(--tw-translate-y) var(--tw-translate-z)",
            ),
        ],
    );
    u.static_rule(
        "scale-3d",
        vec![
            props("scale", Some("1")),
            d(
                "scale",
                "var(--tw-scale-x) var(--tw-scale-y) var(--tw-scale-z)",
            ),
        ],
    );
    for (name, var) in [
        ("duration-initial", "--tw-duration"),
        ("ease-initial", "--tw-ease"),
    ] {
        u.static_rule(
            name,
            vec![hoist(vec![property(var, None, None)]), d(var, "initial")],
        );
    }
    u.static_rule(
        "ring-inset",
        vec![shadow_properties(), d("--tw-ring-inset", "inset")],
    );
    for root in ["shadow", "inset-shadow", "text-shadow"] {
        let properties = if root == "text-shadow" {
            text_shadow_properties()
        } else {
            shadow_properties()
        };
        u.static_rule(
            &format!("{root}-initial"),
            vec![properties, d(&format!("--tw-{root}-color"), "initial")],
        );
    }
    u.static_rule(
        "outline-hidden",
        vec![
            d("--tw-outline-style", "none"),
            d("outline-style", "none"),
            at(
                "@media",
                "(forced-colors: active)",
                vec![
                    d("outline", "2px solid transparent"),
                    d("outline-offset", "2px"),
                ],
            ),
        ],
    );
    let numeric = "var(--tw-ordinal,) var(--tw-slashed-zero,) var(--tw-numeric-figure,) var(--tw-numeric-spacing,) var(--tw-numeric-fraction,)";
    for (name, var, value) in [
        ("ordinal", "ordinal", "ordinal"),
        ("slashed-zero", "slashed-zero", "slashed-zero"),
        ("lining-nums", "numeric-figure", "lining-nums"),
        ("oldstyle-nums", "numeric-figure", "oldstyle-nums"),
        ("proportional-nums", "numeric-spacing", "proportional-nums"),
        ("tabular-nums", "numeric-spacing", "tabular-nums"),
        (
            "diagonal-fractions",
            "numeric-fraction",
            "diagonal-fractions",
        ),
        ("stacked-fractions", "numeric-fraction", "stacked-fractions"),
    ] {
        u.static_rule(
            name,
            vec![
                hoist(
                    [
                        "ordinal",
                        "slashed-zero",
                        "numeric-figure",
                        "numeric-spacing",
                        "numeric-fraction",
                    ]
                    .iter()
                    .map(|name| property(&format!("--tw-{name}"), None, None))
                    .collect(),
                ),
                d(&format!("--tw-{var}"), value),
                d("font-variant-numeric", numeric),
            ],
        );
    }
}
pub(super) fn transform(root: &str, value: &str) -> Vec<AstNode> {
    if root.starts_with("translate") {
        let mut nodes = vec![props("translate", Some("0"))];
        for axis in ["x", "y", "z"] {
            if root == format!("translate-{axis}") || (root == "translate" && axis != "z") {
                nodes.push(d(&format!("--tw-translate-{axis}"), value));
            }
        }
        nodes.push(d(
            "translate",
            if root == "translate-z" {
                "var(--tw-translate-x) var(--tw-translate-y) var(--tw-translate-z)"
            } else {
                "var(--tw-translate-x) var(--tw-translate-y)"
            },
        ));
        return nodes;
    }
    if let Some(axis) = root.strip_prefix("scale-") {
        return vec![
            props("scale", Some("1")),
            d(&format!("--tw-scale-{axis}"), value),
            d(
                "scale",
                if axis == "z" {
                    "var(--tw-scale-x) var(--tw-scale-y) var(--tw-scale-z)"
                } else {
                    "var(--tw-scale-x) var(--tw-scale-y)"
                },
            ),
        ];
    }
    let mut nodes = vec![transform_properties()];
    if let Some(axis) = root.strip_prefix("rotate-") {
        nodes.push(d(
            &format!("--tw-rotate-{axis}"),
            &format!("rotate{}({value})", axis.to_ascii_uppercase()),
        ));
    } else {
        for axis in ["x", "y"] {
            if root == "skew" || root == format!("skew-{axis}") {
                nodes.push(d(
                    &format!("--tw-skew-{axis}"),
                    &format!("skew{}({value})", axis.to_ascii_uppercase()),
                ));
            }
        }
    }
    nodes.push(d("transform", TRANSFORM));
    nodes
}
pub(super) fn compile(i: &Input<'_>) -> Option<Compile> {
    Some(match i.root {
        "col-span" | "row-span" => {
            if i.modifier.is_some() {
                Err(Problem::Invalid)
            } else {
                let value = match i.value {
                    Some(UtilityValue::Named { value, .. }) if value == "full" => {
                        Some("1 / -1".into())
                    }
                    Some(UtilityValue::Named { value, .. }) if values::integer(value) => {
                        Some(format!("span {value} / span {value}"))
                    }
                    Some(UtilityValue::Arbitrary { value, .. }) => {
                        Some(format!("span {value} / span {value}"))
                    }
                    _ => None,
                };
                invalid(value).map(|v| {
                    vec![d(
                        if i.root == "col-span" {
                            "grid-column"
                        } else {
                            "grid-row"
                        },
                        &v,
                    )]
                })
            }
        }
        "flex" => flex(i),
        "rotate" | "-rotate" | "scale" | "-scale" => scale_rotate(i),
        "transform" => {
            if i.modifier.is_some() {
                Err(Problem::Invalid)
            } else {
                match i.value {
                    None => Ok(vec![transform_properties(), d("transform", TRANSFORM)]),
                    Some(UtilityValue::Arbitrary { value, .. }) => {
                        Ok(vec![transform_properties(), d("transform", value)])
                    }
                    _ => Err(Problem::Invalid),
                }
            }
        }
        "transition" => transition(i),
        "ring" | "inset-ring" | "ring-offset" => ring(i),
        "shadow" | "inset-shadow" | "text-shadow" => shadow(i),
        "@container" => {
            let value = match i.value {
                None => Some("inline-size"),
                Some(UtilityValue::Arbitrary { value, .. }) => Some(value.as_str()),
                Some(UtilityValue::Named { value, .. }) if value == "normal" => Some("normal"),
                _ => None,
            };
            invalid(value).map(|v| {
                let mut n = vec![d("container-type", v)];
                if let Some(Modifier::Arbitrary { value } | Modifier::Named { value }) = i.modifier
                {
                    n.push(d("container-name", value));
                }
                n
            })
        }
        _ => return None,
    })
}
fn flex(i: &Input<'_>) -> Compile {
    match i.value {
        Some(UtilityValue::Arbitrary { value, .. }) if i.modifier.is_none() => {
            Ok(vec![d("flex", value)])
        }
        Some(UtilityValue::Named { value, fraction }) => {
            if let Some(f) = fraction {
                let (a, b) = invalid(f.split_once('/'))?;
                if !values::integer(a) || !values::integer(b) {
                    return Err(Problem::Invalid);
                }
                Ok(vec![d("flex", &format!("calc({f} * 100%)"))])
            } else if i.modifier.is_none() && values::integer(value) {
                Ok(vec![d("flex", value)])
            } else {
                Err(Problem::Invalid)
            }
        }
        _ => Err(Problem::Invalid),
    }
}
fn scale_rotate(i: &Input<'_>) -> Compile {
    if i.modifier.is_some() {
        return Err(Problem::Invalid);
    }
    let root = i.root.trim_start_matches('-');
    let negative = i.root.starts_with('-');
    let mut value = if let Some((value, hint)) = i.arbitrary() {
        if root == "rotate"
            && hint.or_else(|| values::infer(value, &["angle", "vector"])) == Some("vector")
        {
            return Ok(vec![d("rotate", &format!("{value} var(--tw-rotate)"))]);
        }
        value.to_string()
    } else {
        let name = invalid(i.named())?;
        invalid(
            i.theme
                .resolve(
                    Some(name),
                    &[if root == "scale" {
                        "--scale"
                    } else {
                        "--rotate"
                    }],
                    ThemeOptions::NONE,
                )
                .or_else(|| {
                    values::integer(name)
                        .then(|| format!("{name}{}", if root == "scale" { "%" } else { "deg" }))
                }),
        )?
    };
    if negative {
        value = format!("calc({value} * -1)");
    }
    if root == "rotate" || i.arbitrary().is_some() {
        return Ok(vec![d(root, &value)]);
    }
    Ok(vec![
        props("scale", Some("1")),
        d("--tw-scale-x", &value),
        d("--tw-scale-y", &value),
        d("--tw-scale-z", &value),
        d("scale", "var(--tw-scale-x) var(--tw-scale-y)"),
    ])
}
fn transition(i: &Input<'_>) -> Compile {
    if i.modifier.is_some() {
        return Err(Problem::Invalid);
    }
    let value = match i.value {
        None => format!(
            "{COLORS}, opacity, box-shadow, transform, translate, scale, rotate, filter, -webkit-backdrop-filter, backdrop-filter, display, content-visibility, overlay, pointer-events"
        ),
        Some(UtilityValue::Arbitrary { value, .. }) => value.clone(),
        Some(UtilityValue::Named { value, fraction }) => {
            if let Some(v) = i.theme.resolve(
                Some(fraction.as_deref().unwrap_or(value)),
                &["--transition-property"],
                ThemeOptions::NONE,
            ) {
                v
            } else {
                match value.as_str() {
                    "none" => return Ok(vec![d("transition-property", "none")]),
                    "all" => "all".into(),
                    "colors" => COLORS.into(),
                    "opacity" => "opacity".into(),
                    "shadow" => "box-shadow".into(),
                    "transform" => "transform, translate, scale, rotate".into(),
                    _ => return Err(Problem::Invalid),
                }
            }
        }
    };
    let duration = i
        .theme
        .resolve(None, &["--default-transition-duration"], ThemeOptions::NONE)
        .unwrap_or("0s".into());
    let ease = i
        .theme
        .resolve(
            None,
            &["--default-transition-timing-function"],
            ThemeOptions::NONE,
        )
        .unwrap_or("ease".into());
    Ok(vec![
        d("transition-property", &value),
        d(
            "transition-timing-function",
            &format!("var(--tw-ease, {ease})"),
        ),
        d(
            "transition-duration",
            &format!("var(--tw-duration, {duration})"),
        ),
    ])
}
fn ring(i: &Input<'_>) -> Compile {
    let offset = i.root == "ring-offset";
    let inset = i.root == "inset-ring";
    let color_var = if offset {
        "--tw-ring-offset-color"
    } else if inset {
        "--tw-inset-ring-color"
    } else {
        "--tw-ring-color"
    };
    let width = |value: &str| {
        if offset {
            return vec![
                d("--tw-ring-offset-width", value),
                d(
                    "--tw-ring-offset-shadow",
                    "var(--tw-ring-inset,) 0 0 0 var(--tw-ring-offset-width) var(--tw-ring-offset-color)",
                ),
            ];
        }
        let (var, value) = if inset {
            (
                "--tw-inset-ring-shadow",
                format!("inset 0 0 0 {value} var(--tw-inset-ring-color, currentcolor)"),
            )
        } else {
            (
                "--tw-ring-shadow",
                format!(
                    "var(--tw-ring-inset,) 0 0 0 calc({value} + var(--tw-ring-offset-width)) var(--tw-ring-color, {})",
                    i.theme
                        .get(&["--default-ring-color"])
                        .unwrap_or("currentcolor")
                ),
            )
        };
        vec![shadow_properties(), d(var, &value), d("box-shadow", SHADOW)]
    };
    if i.value.is_none() {
        if offset || i.modifier.is_some() {
            return Err(Problem::Invalid);
        }
        return Ok(width(if inset {
            "1px"
        } else {
            i.theme.get(&["--default-ring-width"]).unwrap_or("1px")
        }));
    }
    if let Some((value, hint)) = i.arbitrary() {
        if hint.or_else(|| values::infer(value, &["color", "length"])) == Some("length") {
            if i.modifier.is_some() {
                return Err(Problem::Invalid);
            }
            return Ok(width(value));
        }
        return Ok(vec![d(color_var, &invalid(i.color(&[]))?)]);
    }
    let color_keys = if offset {
        ["--ring-offset-color", "--color"]
    } else {
        ["--ring-color", "--color"]
    };
    if !offset {
        if let Some(value) = i.color(&color_keys) {
            return Ok(vec![d(color_var, &value)]);
        }
    }
    let name = invalid(i.named())?;
    let value = i
        .theme
        .resolve(
            Some(name),
            &[if offset {
                "--ring-offset-width"
            } else {
                "--ring-width"
            }],
            ThemeOptions::NONE,
        )
        .or_else(|| values::integer(name).then(|| format!("{name}px")));
    if let Some(value) = value {
        if i.modifier.is_some() {
            return Err(Problem::Invalid);
        }
        return Ok(width(&value));
    }
    if offset {
        if let Some(value) = i.color(&color_keys) {
            return Ok(vec![d(color_var, &value)]);
        }
    }
    Err(Problem::Invalid)
}
fn shadow(i: &Input<'_>) -> Compile {
    let text = i.root == "text-shadow";
    let inset = i.root == "inset-shadow";
    let var = format!("--tw-{}", i.root);
    let color_var = format!("{var}-color");
    let alpha_var = format!("{var}-alpha");
    let property_name = if text { "text-shadow" } else { &var };
    let properties = || {
        if text {
            text_shadow_properties()
        } else {
            shadow_properties()
        }
    };
    let alpha = match i.modifier {
        Some(Modifier::Arbitrary { value }) => Some(value.clone()),
        Some(Modifier::Named { value }) if values::integer(value) => Some(format!("{value}%")),
        _ => None,
    };
    let build = |value: &str, prefix: bool| {
        let mut fallback = false;
        let replaced = values::replace_shadow_colors(value, |color| {
            let color = match alpha.as_deref() {
                None => color.to_string(),
                Some(a) if color.starts_with("current") => values::with_alpha(color, a),
                Some(a) => {
                    fallback |= color.starts_with("var(") || a.starts_with("var(");
                    values::replace_alpha(color, a)
                }
            };
            format!("var({color_var}, {color})")
        });
        let prefix_value = |value: String| {
            if prefix {
                segment(&value, ',')
                    .into_iter()
                    .map(|s| format!("inset {}", s.trim()))
                    .collect::<Vec<_>>()
                    .join(", ")
            } else {
                value
            }
        };
        let mut nodes = vec![properties()];
        if let Some(alpha) = &alpha {
            nodes.push(d(&alpha_var, alpha));
        }
        if fallback {
            let base = values::replace_shadow_colors(value, |c| format!("var({color_var}, {c})"));
            nodes.push(d(property_name, &prefix_value(base)));
            nodes.push(at(
                "@supports",
                "(color: lab(from red l a b))",
                vec![d(property_name, &prefix_value(replaced))],
            ));
        } else {
            nodes.push(d(property_name, &prefix_value(replaced)));
        }
        if !text {
            nodes.push(d("box-shadow", SHADOW));
        }
        nodes
    };
    let color_nodes = |color: String| {
        vec![
            properties(),
            d(
                &color_var,
                &values::with_alpha(&color, &format!("var({alpha_var})")),
            ),
        ]
    };
    if i.value.is_none() {
        return Ok(build(
            invalid(i.theme.get(&[&format!("--{}", i.root)]))?,
            false,
        ));
    }
    if let Some((value, hint)) = i.arbitrary() {
        return if hint.or_else(|| values::infer(value, &["color"])) == Some("color") {
            Ok(color_nodes(invalid(i.color(&[]))?))
        } else {
            Ok(build(value, inset))
        };
    }
    let name = invalid(i.named())?;
    if matches!(name, "none" | "inherit") {
        if i.modifier.is_some() {
            return Err(Problem::Invalid);
        }
        return Ok(if name == "inherit" {
            vec![properties(), d(&color_var, "inherit")]
        } else if text {
            vec![properties(), d("text-shadow", "none")]
        } else {
            vec![properties(), d(&var, "0 0 #0000"), d("box-shadow", SHADOW)]
        });
    }
    if let Some(value) = i.theme.get(&[&format!("--{}-{name}", i.root)]) {
        return Ok(build(value, false));
    }
    Ok(color_nodes(invalid(i.color(&[
        if text {
            "--text-shadow-color"
        } else {
            "--box-shadow-color"
        },
        "--color",
    ]))?))
}
