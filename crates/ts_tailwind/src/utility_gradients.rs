//! Gradient direction/interpolation and stop generators, Tailwind 4.2.1.
use super::*;
pub(super) fn register(u: &mut Utilities) {
    for root in [
        "bg-linear",
        "-bg-linear",
        "bg-conic",
        "-bg-conic",
        "bg-radial",
        "from",
        "via",
        "to",
    ] {
        u.known_functional.insert(root.into());
    }
    for (root, prop) in [
        ("bg-size", "background-size"),
        ("bg-position", "background-position"),
    ] {
        u.spec(root, Spec::new(&[prop], &[], Bare::None));
    }
}
fn interpolation(modifier: Option<&Modifier>) -> String {
    match modifier {
        None => "in oklab".into(),
        Some(Modifier::Arbitrary { value }) => value.clone(),
        Some(Modifier::Named { value }) => {
            if matches!(
                value.as_str(),
                "longer" | "shorter" | "increasing" | "decreasing"
            ) {
                format!("in oklch {value} hue")
            } else {
                format!("in {value}")
            }
        }
    }
}
fn direction(value: &str) -> Option<&'static str> {
    Some(match value {
        "to-t" => "to top",
        "to-tr" => "to top right",
        "to-r" => "to right",
        "to-br" => "to bottom right",
        "to-b" => "to bottom",
        "to-bl" => "to bottom left",
        "to-l" => "to left",
        "to-tl" => "to top left",
        _ => return None,
    })
}
pub(super) fn compile(i: &Input<'_>) -> Option<Compile> {
    Some(match i.root {
        "bg-linear" | "-bg-linear" | "bg-conic" | "-bg-conic" | "bg-radial" => gradient(i),
        "from" | "via" | "to" => stop(i),
        _ => return None,
    })
}
fn gradient(i: &Input<'_>) -> Compile {
    let negative = i.root.starts_with('-');
    let kind = i.root.trim_start_matches('-').strip_prefix("bg-").unwrap();
    let image = |fallback: Option<&str>| {
        d(
            "background-image",
            &match fallback {
                Some(value) => format!("{kind}-gradient(var(--tw-gradient-stops,{value}))"),
                None => format!("{kind}-gradient(var(--tw-gradient-stops))"),
            },
        )
    };
    if let Some((value, hint)) = i.arbitrary() {
        if i.modifier.is_some() {
            return Err(Problem::Invalid);
        }
        let mut value = value.to_string();
        if kind == "linear" && negative {
            if hint.or_else(|| values::infer(&value, &["angle"])) != Some("angle") {
                return Err(Problem::Invalid);
            }
            value = format!("calc({value} * -1)");
        }
        return Ok(vec![
            d("--tw-gradient-position", &value),
            image(Some(&value)),
        ]);
    }
    let method = interpolation(i.modifier);
    if i.value.is_none() {
        return if kind == "linear" {
            Err(Problem::Invalid)
        } else {
            Ok(vec![d("--tw-gradient-position", &method), image(None)])
        };
    }
    let name = invalid(i.named())?;
    if kind == "radial" {
        return Err(Problem::Invalid);
    }
    let value = if kind == "linear" && !negative && direction(name).is_some() {
        direction(name).unwrap().into()
    } else if values::integer(name) {
        if negative {
            format!("calc({name}deg * -1)")
        } else {
            format!("{name}deg")
        }
    } else {
        return Err(Problem::Invalid);
    };
    if kind == "conic" {
        return Ok(vec![
            d("--tw-gradient-position", &format!("from {value} {method}")),
            image(None),
        ]);
    }
    Ok(vec![
        d("--tw-gradient-position", &value),
        at(
            "@supports",
            "(background-image: linear-gradient(in lab, red, red))",
            vec![d("--tw-gradient-position", &format!("{value} {method}"))],
        ),
        image(None),
    ])
}
fn stop_properties() -> AstNode {
    hoist(vec![
        property("--tw-gradient-position", None, None),
        property("--tw-gradient-from", Some("#0000"), Some("<color>")),
        property("--tw-gradient-via", Some("#0000"), Some("<color>")),
        property("--tw-gradient-to", Some("#0000"), Some("<color>")),
        property("--tw-gradient-stops", None, None),
        property("--tw-gradient-via-stops", None, None),
        property(
            "--tw-gradient-from-position",
            Some("0%"),
            Some("<length-percentage>"),
        ),
        property(
            "--tw-gradient-via-position",
            Some("50%"),
            Some("<length-percentage>"),
        ),
        property(
            "--tw-gradient-to-position",
            Some("100%"),
            Some("<length-percentage>"),
        ),
    ])
}
fn stop(i: &Input<'_>) -> Compile {
    let (value, position) = if let Some((value, hint)) = i.arbitrary() {
        if matches!(
            hint.or_else(|| values::infer(value, &["color", "length", "percentage"])),
            Some("length" | "percentage")
        ) {
            if i.modifier.is_some() {
                return Err(Problem::Invalid);
            }
            (value.into(), true)
        } else {
            (invalid(i.color(&[]))?, false)
        }
    } else if let Some(value) = i.color(&["--background-color", "--color"]) {
        (value, false)
    } else {
        if i.modifier.is_some() {
            return Err(Problem::Invalid);
        }
        let name = invalid(i.named())?;
        (
            invalid(
                i.theme
                    .resolve(
                        Some(name),
                        &["--gradient-color-stop-positions"],
                        ThemeOptions::NONE,
                    )
                    .or_else(|| {
                        name.strip_suffix('%')
                            .filter(|v| values::integer(v))
                            .map(|_| name.into())
                    }),
            )?,
            true,
        )
    };
    let var = format!("--tw-gradient-{}", i.root);
    if position {
        return Ok(vec![
            stop_properties(),
            d(&format!("{var}-position"), &value),
        ]);
    }
    let mut nodes = vec![stop_properties(), d("--tw-sort", &var), d(&var, &value)];
    if i.root == "via" {
        nodes.extend([d("--tw-gradient-via-stops", "var(--tw-gradient-position), var(--tw-gradient-from) var(--tw-gradient-from-position), var(--tw-gradient-via) var(--tw-gradient-via-position), var(--tw-gradient-to) var(--tw-gradient-to-position)"), d("--tw-gradient-stops", "var(--tw-gradient-via-stops)")]);
    } else {
        nodes.push(d("--tw-gradient-stops", "var(--tw-gradient-via-stops, var(--tw-gradient-position), var(--tw-gradient-from) var(--tw-gradient-from-position), var(--tw-gradient-to) var(--tw-gradient-to-position))"));
    }
    Ok(nodes)
}
