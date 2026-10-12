//! Filter and drop-shadow generators from pinned Tailwind utilities.ts.
use super::*;
use crate::value::segment;

fn filter_value(backdrop: bool) -> String {
    let prefix = if backdrop { "backdrop-" } else { "" };
    [
        "blur",
        "brightness",
        "contrast",
        "grayscale",
        "hue-rotate",
        "invert",
        "opacity",
        "saturate",
        "sepia",
        "drop-shadow",
    ]
    .into_iter()
    .filter(|name| {
        if backdrop {
            *name != "drop-shadow"
        } else {
            *name != "opacity"
        }
    })
    .map(|name| format!("var(--tw-{prefix}{name},)"))
    .collect::<Vec<_>>()
    .join(" ")
}
fn properties(backdrop: bool) -> AstNode {
    let prefix = if backdrop { "backdrop-" } else { "" };
    let mut nodes: Vec<_> = [
        "blur",
        "brightness",
        "contrast",
        "grayscale",
        "hue-rotate",
        "invert",
        "opacity",
        "saturate",
        "sepia",
    ]
    .iter()
    .map(|name| property(&format!("--tw-{prefix}{name}"), None, None))
    .collect();
    if !backdrop {
        nodes.extend([
            property("--tw-drop-shadow", None, None),
            property("--tw-drop-shadow-color", None, None),
            property("--tw-drop-shadow-alpha", Some("100%"), Some("<percentage>")),
            property("--tw-drop-shadow-size", None, None),
        ]);
    }
    hoist(nodes)
}
fn declarations(backdrop: bool, value: &str) -> Vec<AstNode> {
    if backdrop {
        vec![
            d("-webkit-backdrop-filter", value),
            d("backdrop-filter", value),
        ]
    } else {
        vec![d("filter", value)]
    }
}
pub(super) fn render(root: &str, value: &str) -> Vec<AstNode> {
    let backdrop = root.starts_with("backdrop-");
    let function = root.strip_prefix("backdrop-").unwrap_or(root);
    let mut nodes = vec![
        properties(backdrop),
        d(
            &format!("--tw-{root}"),
            &if value == " " {
                " ".into()
            } else {
                format!("{function}({value})")
            },
        ),
    ];
    nodes.extend(declarations(backdrop, &filter_value(backdrop)));
    nodes
}
pub(super) fn register(u: &mut Utilities) {
    for prefix in ["", "backdrop-"] {
        for name in [
            "blur",
            "brightness",
            "contrast",
            "grayscale",
            "hue-rotate",
            "invert",
            "saturate",
            "sepia",
            "opacity",
        ] {
            if prefix.is_empty() && name == "opacity" {
                continue;
            }
            let root = format!("{prefix}{name}");
            let key = format!("--{root}");
            let base = format!("--{name}");
            let keys = if prefix.is_empty() {
                vec![key.as_str()]
            } else {
                vec![key.as_str(), base.as_str()]
            };
            let bare = match name {
                "blur" => Bare::None,
                "hue-rotate" => Bare::Degrees,
                "opacity" => Bare::Percent,
                _ => Bare::IntegerPercent,
            };
            let mut spec = Spec::new(&[], &keys, bare);
            spec.handler = Handler::Filter;
            spec.negative = name == "hue-rotate";
            if matches!(name, "grayscale" | "invert" | "sepia") {
                spec.default = Some("100%".into());
            }
            if name == "blur" {
                spec.fallback.insert("none".into(), " ".into());
            }
            u.spec(&root, spec);
        }
    }
    for root in ["filter", "backdrop-filter", "drop-shadow"] {
        u.known_functional.insert(root.into());
    }
    u.static_rule(
        "drop-shadow-none",
        vec![
            properties(false),
            d("--tw-drop-shadow", " "),
            d("filter", &filter_value(false)),
        ],
    );
}
pub(super) fn compile(i: &Input<'_>) -> Option<Compile> {
    Some(match i.root {
        "filter" | "backdrop-filter" => {
            if i.modifier.is_some() {
                return Some(Err(Problem::Invalid));
            }
            let backdrop = i.root == "backdrop-filter";
            match i.value {
                None => {
                    let mut nodes = vec![properties(backdrop)];
                    nodes.extend(declarations(backdrop, &filter_value(backdrop)));
                    Ok(nodes)
                }
                Some(UtilityValue::Arbitrary { value, .. }) => Ok(declarations(backdrop, value)),
                Some(UtilityValue::Named { value, .. }) if value == "none" => {
                    Ok(declarations(backdrop, "none"))
                }
                _ => Err(Problem::Invalid),
            }
        }
        "drop-shadow" => drop_shadow(i),
        _ => return None,
    })
}
fn wrapped(value: &str) -> String {
    segment(value, ',')
        .into_iter()
        .map(|v| format!("drop-shadow({v})"))
        .collect::<Vec<_>>()
        .join(" ")
}
fn replaced(value: &str, alpha: Option<&str>) -> Vec<AstNode> {
    let mut fallback = false;
    let inject = |color: &str| format!("var(--tw-drop-shadow-color, {color})");
    let mapped = segment(value, ',')
        .into_iter()
        .map(|part| {
            let part = values::replace_shadow_colors(part, |color| {
                let color = match alpha {
                    None => color.into(),
                    Some(a) if color.starts_with("current") => values::with_alpha(color, a),
                    Some(a) => {
                        fallback |= color.starts_with("var(") || a.starts_with("var(");
                        values::replace_alpha(color, a)
                    }
                };
                inject(&color)
            });
            format!("drop-shadow({part})")
        })
        .collect::<Vec<_>>()
        .join(" ");
    let prop = "--tw-drop-shadow-size";
    if fallback {
        let original = segment(value, ',')
            .into_iter()
            .map(|part| {
                format!(
                    "drop-shadow({})",
                    values::replace_shadow_colors(part, inject)
                )
            })
            .collect::<Vec<_>>()
            .join(" ");
        vec![
            d(prop, &original),
            at(
                "@supports",
                "(color: lab(from red l a b))",
                vec![d(prop, &mapped)],
            ),
        ]
    } else {
        vec![d(prop, &mapped)]
    }
}
fn drop_shadow(i: &Input<'_>) -> Compile {
    let alpha = match i.modifier {
        Some(Modifier::Arbitrary { value }) => Some(value.clone()),
        Some(Modifier::Named { value }) if values::integer(value) => Some(format!("{value}%")),
        _ => None,
    };
    let size = |value: &str, resolved: Option<&str>| {
        let mut nodes = vec![properties(false)];
        if let Some(alpha) = &alpha {
            nodes.push(d("--tw-drop-shadow-alpha", alpha));
        }
        nodes.extend(replaced(value, alpha.as_deref()));
        nodes.push(d(
            "--tw-drop-shadow",
            &resolved
                .map(wrapped)
                .unwrap_or_else(|| "var(--tw-drop-shadow-size)".into()),
        ));
        nodes.push(d("filter", &filter_value(false)));
        nodes
    };
    let color = |value: String| {
        vec![
            properties(false),
            d(
                "--tw-drop-shadow-color",
                &if value == "inherit" {
                    value
                } else {
                    values::with_alpha(&value, "var(--tw-drop-shadow-alpha)")
                },
            ),
            d("--tw-drop-shadow", "var(--tw-drop-shadow-size)"),
        ]
    };
    if i.value.is_none() {
        let raw = invalid(i.theme.get(&["--drop-shadow"]))?;
        let resolved = invalid(
            i.theme
                .resolve(None, &["--drop-shadow"], ThemeOptions::NONE),
        )?;
        return Ok(size(raw, Some(&resolved)));
    }
    if let Some((value, hint)) = i.arbitrary() {
        if hint.or_else(|| values::infer(value, &["color"])) == Some("color") {
            return Ok(color(invalid(i.color(&[]))?));
        }
        if i.modifier.is_some() && alpha.is_none() {
            return Err(Problem::Invalid);
        }
        return Ok(size(value, None));
    }
    let name = invalid(i.named())?;
    if let (Some(raw), Some(resolved)) = (
        i.theme.get(&[&format!("--drop-shadow-{name}")]),
        i.theme
            .resolve(Some(name), &["--drop-shadow"], ThemeOptions::NONE),
    ) {
        if i.modifier.is_some() && alpha.is_none() {
            return Err(Problem::Invalid);
        }
        return Ok(size(
            raw,
            if alpha.is_some() {
                None
            } else {
                Some(&resolved)
            },
        ));
    }
    Ok(color(invalid(
        i.color(&["--drop-shadow-color", "--color"]),
    )?))
}
