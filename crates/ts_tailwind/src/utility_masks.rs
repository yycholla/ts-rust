//! Mask gradients share stop resolution but preserve distinct upstream defaults.
use super::*;
const IMAGE: &str = "var(--tw-mask-linear), var(--tw-mask-radial), var(--tw-mask-conic)";
pub(super) fn register(u: &mut Utilities) {
    // These short roots do not exist upstream: only mask-{edge}-{from,to} does.
    for name in ["mask-t", "mask-r", "mask-b", "mask-l", "mask-x", "mask-y"] {
        u.known_functional.remove(name);
    }
    for root in [
        "mask",
        "mask-linear",
        "-mask-linear",
        "mask-radial",
        "mask-conic",
        "-mask-conic",
    ] {
        u.known_functional.insert(root.into());
    }
    for group in ["t", "r", "b", "l", "x", "y", "linear", "radial", "conic"] {
        for stop in ["from", "to"] {
            u.known_functional.insert(format!("mask-{group}-{stop}"));
        }
    }
    for (root, prop) in [
        ("mask-size", "mask-size"),
        ("mask-position", "mask-position"),
        ("mask-radial-at", "--tw-mask-radial-position"),
    ] {
        u.spec(root, Spec::new(&[prop], &[], Bare::None));
    }
}
fn common() -> AstNode {
    hoist(
        ["linear", "radial", "conic"]
            .iter()
            .map(|name| {
                property(
                    &format!("--tw-mask-{name}"),
                    Some("linear-gradient(#fff, #fff)"),
                    None,
                )
            })
            .collect(),
    )
}
fn support(kind: &str) -> AstNode {
    let mut defaults = vec![
        ("from-position", "0%"),
        ("to-position", "100%"),
        ("from-color", "black"),
        ("to-color", "transparent"),
    ];
    if kind == "radial" {
        defaults.extend([
            ("shape", "ellipse"),
            ("size", "farthest-corner"),
            ("position", "center"),
        ]);
    } else {
        defaults.insert(0, ("position", "0deg"));
    }
    hoist(
        defaults
            .into_iter()
            .map(|(p, v)| property(&format!("--tw-mask-{kind}-{p}"), Some(v), None))
            .collect(),
    )
}
fn base(kind: &str) -> Vec<AstNode> {
    vec![
        common(),
        support(kind),
        d("mask-image", IMAGE),
        d("mask-composite", "intersect"),
    ]
}
pub(super) fn compile(i: &Input<'_>) -> Option<Compile> {
    if i.root == "mask" {
        return Some(arbitrary(i));
    }
    let root = i.root.trim_start_matches('-');
    if matches!(root, "mask-linear" | "mask-radial" | "mask-conic") {
        return Some(gradient(i));
    }
    let (group, stop) = root.strip_prefix("mask-")?.rsplit_once('-')?;
    if !matches!(stop, "from" | "to")
        || !matches!(
            group,
            "t" | "r" | "b" | "l" | "x" | "y" | "linear" | "radial" | "conic"
        )
    {
        return None;
    }
    Some(stop_value(i).map(|(value, color)| {
        let suffix = if color { "color" } else { "position" };
        if matches!(group, "linear" | "radial" | "conic") {
            let mut nodes = base(group);
            let prefix = match group {
                "radial" => "var(--tw-mask-radial-shape) var(--tw-mask-radial-size) at var(--tw-mask-radial-position)",
                "conic" => "from var(--tw-mask-conic-position)",
                _ => "var(--tw-mask-linear-position)",
            };
            nodes.extend([
                d(&format!("--tw-mask-{group}-stops"), &format!("{prefix}, var(--tw-mask-{group}-from-color) var(--tw-mask-{group}-from-position), var(--tw-mask-{group}-to-color) var(--tw-mask-{group}-to-position)")),
                d(&format!("--tw-mask-{group}"), &format!("{group}-gradient(var(--tw-mask-{group}-stops))")),
                d(&format!("--tw-mask-{group}-{stop}-{suffix}"), &value),
            ]);
            nodes
        } else {
            let mut nodes = vec![common(), hoist(["left", "right", "bottom", "top"].iter().map(|edge| property(&format!("--tw-mask-{edge}"), Some("linear-gradient(#fff, #fff)"), None)).collect()), d("mask-image", IMAGE), d("mask-composite", "intersect"), d("--tw-mask-linear", "var(--tw-mask-left), var(--tw-mask-right), var(--tw-mask-bottom), var(--tw-mask-top)")];
            for (edge, enabled) in [("top", matches!(group, "t" | "y")), ("right", matches!(group, "r" | "x")), ("bottom", matches!(group, "b" | "y")), ("left", matches!(group, "l" | "x"))] {
                if !enabled { continue; }
                nodes.push(d(&format!("--tw-mask-{edge}"), &format!("linear-gradient(to {edge}, var(--tw-mask-{edge}-from-color) var(--tw-mask-{edge}-from-position), var(--tw-mask-{edge}-to-color) var(--tw-mask-{edge}-to-position))")));
                nodes.push(hoist([("from-position", "0%"), ("to-position", "100%"), ("from-color", "black"), ("to-color", "transparent")].into_iter().map(|(p, v)| property(&format!("--tw-mask-{edge}-{p}"), Some(v), None)).collect()));
                nodes.push(d(&format!("--tw-mask-{edge}-{stop}-{suffix}"), &value));
            }
            nodes
        }
    }))
}
fn arbitrary(i: &Input<'_>) -> Compile {
    if i.modifier.is_some() {
        return Err(Problem::Invalid);
    }
    let (value, hint) = invalid(i.arbitrary())?;
    let property = match hint.or_else(|| {
        values::infer(
            value,
            &[
                "image",
                "percentage",
                "position",
                "bg-size",
                "length",
                "url",
            ],
        )
    }) {
        Some("percentage" | "position") => "mask-position",
        Some("bg-size" | "length" | "size") => "mask-size",
        _ => "mask-image",
    };
    Ok(vec![d(property, value)])
}
fn gradient(i: &Input<'_>) -> Compile {
    if i.modifier.is_some() {
        return Err(Problem::Invalid);
    }
    let negative = i.root.starts_with('-');
    let kind = i
        .root
        .trim_start_matches('-')
        .strip_prefix("mask-")
        .unwrap();
    let value = if let Some((value, _)) = i.arbitrary() {
        if negative {
            format!("calc({value} * -1)")
        } else {
            value.into()
        }
    } else if kind != "radial" {
        let name = invalid(i.named().filter(|name| values::integer(name)))?;
        format!("calc(1deg * {}{name})", if negative { "-" } else { "" })
    } else {
        return Err(Problem::Invalid);
    };
    let prop = if kind == "radial" { "size" } else { "position" };
    let mut nodes = base(kind);
    nodes.extend([
        d(
            &format!("--tw-mask-{kind}"),
            &format!("{kind}-gradient(var(--tw-mask-{kind}-stops, var(--tw-mask-{kind}-{prop})))"),
        ),
        d(&format!("--tw-mask-{kind}-{prop}"), &value),
    ]);
    Ok(nodes)
}
fn stop_value(i: &Input<'_>) -> Result<(String, bool), Problem> {
    if let Some((value, hint)) = i.arbitrary() {
        let kind = hint.or_else(|| values::infer(value, &["length", "percentage", "color"]));
        if kind == Some("color") {
            return Ok((invalid(i.color(&[]))?, true));
        }
        if i.modifier.is_some() {
            return Err(Problem::Invalid);
        }
        if kind == Some("percentage") && !value.strip_suffix('%').is_some_and(values::integer) {
            return Err(Problem::Invalid);
        }
        return Ok((value.into(), false));
    }
    if let Some(value) = i.color(&["--background-color", "--color"]) {
        return Ok((value, true));
    }
    if i.modifier.is_some() {
        return Err(Problem::Invalid);
    }
    let name = invalid(i.named())?;
    if name.strip_suffix('%').is_some_and(values::integer) {
        return Ok((name.into(), false));
    }
    if values::quarter(name) {
        let spacing = invalid(i.theme.resolve(None, &["--spacing"], ThemeOptions::NONE))?;
        return Ok((format!("calc({spacing} * {name})"), false));
    }
    Err(Problem::Invalid)
}
