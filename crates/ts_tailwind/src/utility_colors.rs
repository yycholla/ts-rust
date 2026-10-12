use super::*;
pub(super) fn register(u: &mut Utilities) {
    for root in [
        "bg",
        "text",
        "font",
        "fill",
        "stroke",
        "accent",
        "caret",
        "divide",
        "placeholder",
        "decoration",
        "outline",
        "ring",
        "inset-ring",
        "ring-offset",
        "shadow",
        "inset-shadow",
        "text-shadow",
        "border",
        "border-x",
        "border-y",
        "border-s",
        "border-e",
        "border-bs",
        "border-be",
        "border-t",
        "border-r",
        "border-b",
        "border-l",
    ] {
        u.known_functional.insert(root.into());
    }
}
pub(super) fn compile(i: &Input<'_>) -> Option<Compile> {
    Some(match i.root {
        "bg" => background(i),
        "text" => text(i),
        "font" => font(i),
        "border" | "border-x" | "border-y" | "border-s" | "border-e" | "border-bs"
        | "border-be" | "border-t" | "border-r" | "border-b" | "border-l" => border(i),
        "outline" => outline(i),
        "stroke" => stroke(i),
        "decoration" => decoration(i),
        "fill" | "accent" | "caret" | "divide" | "placeholder" => {
            let (prop, keys) = match i.root {
                "fill" => ("fill", vec!["--fill", "--color"]),
                "accent" => ("accent-color", vec!["--accent-color", "--color"]),
                "caret" => ("caret-color", vec!["--caret-color", "--color"]),
                "divide" => (
                    "border-color",
                    vec!["--divide-color", "--border-color", "--color"],
                ),
                _ => ("color", vec!["--placeholder-color", "--color"]),
            };
            invalid(i.color(&keys)).map(|value| match i.root {
                "divide" => vec![rule(
                    ":where(& > :not(:last-child))",
                    vec![d("--tw-sort", "divide-color"), d(prop, &value)],
                )],
                "placeholder" => vec![rule("&::placeholder", vec![d(prop, &value)])],
                _ => vec![d(prop, &value)],
            })
        }
        _ => return None,
    })
}
fn background(i: &Input<'_>) -> Compile {
    if let Some((value, hint)) = i.arbitrary() {
        let ty = hint.or_else(|| {
            values::infer(
                value,
                &[
                    "image",
                    "color",
                    "percentage",
                    "position",
                    "bg-size",
                    "length",
                    "url",
                ],
            )
        });
        let prop = match ty {
            Some("percentage" | "position") => Some("background-position"),
            Some("bg-size" | "length" | "size") => Some("background-size"),
            Some("image" | "url") => Some("background-image"),
            _ => None,
        };
        if let Some(prop) = prop {
            if i.modifier.is_some() {
                return Err(Problem::Invalid);
            }
            return Ok(vec![d(prop, value)]);
        }
        return Ok(vec![d("background-color", &invalid(i.color(&[]))?)]);
    }
    let name = invalid(i.named())?;
    if let Some(value) = i.color(&["--background-color", "--color"]) {
        return Ok(vec![d("background-color", &value)]);
    }
    if i.modifier.is_some() {
        return Err(Problem::Invalid);
    }
    Ok(vec![d(
        "background-image",
        &invalid(
            i.theme
                .resolve(Some(name), &["--background-image"], ThemeOptions::NONE),
        )?,
    )])
}
fn leading(i: &Input<'_>) -> Option<String> {
    match i.modifier? {
        Modifier::Arbitrary { value } => Some(value.clone()),
        Modifier::Named { value } => i
            .theme
            .resolve(Some(value), &["--leading"], ThemeOptions::NONE)
            .or_else(|| {
                if values::quarter(value) {
                    i.theme
                        .resolve(None, &["--spacing"], ThemeOptions::NONE)
                        .map(|v| format!("calc({v} * {value})"))
                } else if value == "none" {
                    Some("1".into())
                } else {
                    None
                }
            }),
    }
}
fn text(i: &Input<'_>) -> Compile {
    if let Some((value, hint)) = i.arbitrary() {
        if matches!(
            hint.or_else(|| values::infer(
                value,
                &[
                    "color",
                    "length",
                    "percentage",
                    "absolute-size",
                    "relative-size"
                ]
            )),
            Some("size" | "length" | "percentage" | "absolute-size" | "relative-size")
        ) {
            let mut nodes = vec![d("font-size", value)];
            if i.modifier.is_some() {
                nodes.push(d("line-height", &invalid(leading(i))?));
            }
            return Ok(nodes);
        }
        return Ok(vec![d("color", &invalid(i.color(&[]))?)]);
    }
    let name = invalid(i.named())?;
    if let Some(value) = i.color(&["--text-color", "--color"]) {
        return Ok(vec![d("color", &value)]);
    }
    let (size, extra) = invalid(i.theme.resolve_with(
        name,
        &["--text"],
        &["--line-height", "--letter-spacing", "--font-weight"],
    ))?;
    let mut nodes = vec![d("font-size", &size)];
    if i.modifier.is_some() {
        nodes.push(d("line-height", &invalid(leading(i))?));
        return Ok(nodes);
    }
    for (key, value) in extra {
        let (prop, var) = match key.as_str() {
            "--line-height" => ("line-height", "--tw-leading"),
            "--letter-spacing" => ("letter-spacing", "--tw-tracking"),
            _ => ("font-weight", "--tw-font-weight"),
        };
        nodes.push(d(prop, &format!("var({var}, {value})")));
    }
    Ok(nodes)
}
fn font(i: &Input<'_>) -> Compile {
    if i.modifier.is_some() {
        return Err(Problem::Invalid);
    }
    let weight = |value: &str| {
        vec![
            hoist(vec![property("--tw-font-weight", None, None)]),
            d("--tw-font-weight", value),
            d("font-weight", value),
        ]
    };
    if let Some((value, hint)) = i.arbitrary() {
        return Ok(
            if matches!(
                hint.or_else(|| values::infer(value, &["number", "generic-name", "family-name"])),
                Some("generic-name" | "family-name")
            ) {
                vec![d("font-family", value)]
            } else {
                weight(value)
            },
        );
    }
    let name = invalid(i.named())?;
    if let Some((family, extra)) = i.theme.resolve_with(
        name,
        &["--font"],
        &["--font-feature-settings", "--font-variation-settings"],
    ) {
        let mut nodes = vec![d("font-family", &family)];
        nodes.extend(
            extra
                .iter()
                .map(|(key, value)| d(key.trim_start_matches("--"), value)),
        );
        return Ok(nodes);
    }
    Ok(weight(&invalid(i.theme.resolve(
        Some(name),
        &["--font-weight"],
        ThemeOptions::NONE,
    ))?))
}
fn border(i: &Input<'_>) -> Compile {
    let side = match i.root {
        "border" => "",
        "border-x" => "-inline",
        "border-y" => "-block",
        "border-s" => "-inline-start",
        "border-e" => "-inline-end",
        "border-bs" => "-block-start",
        "border-be" => "-block-end",
        "border-t" => "-top",
        "border-r" => "-right",
        "border-b" => "-bottom",
        _ => "-left",
    };
    let width = |value: &str| {
        vec![
            hoist(vec![property("--tw-border-style", Some("solid"), None)]),
            d(&format!("border{side}-style"), "var(--tw-border-style)"),
            d(&format!("border{side}-width"), value),
        ]
    };
    let color = |value: &str| vec![d(&format!("border{side}-color"), value)];
    if i.value.is_none() {
        if i.modifier.is_some() {
            return Err(Problem::Invalid);
        }
        return Ok(width(
            i.theme.get(&["--default-border-width"]).unwrap_or("1px"),
        ));
    }
    if let Some((value, hint)) = i.arbitrary() {
        if matches!(
            hint.or_else(|| values::infer(value, &["color", "line-width", "length"])),
            Some("line-width" | "length")
        ) {
            if i.modifier.is_some() {
                return Err(Problem::Invalid);
            }
            return Ok(width(value));
        }
        return Ok(color(&invalid(i.color(&[]))?));
    }
    if let Some(value) = i.color(&["--border-color", "--color"]) {
        return Ok(color(&value));
    }
    if i.modifier.is_some() {
        return Err(Problem::Invalid);
    }
    let name = invalid(i.named())?;
    Ok(width(&invalid(
        i.theme
            .resolve(Some(name), &["--border-width"], ThemeOptions::NONE)
            .or_else(|| values::integer(name).then(|| format!("{name}px"))),
    )?))
}
fn outline(i: &Input<'_>) -> Compile {
    let width = |value: &str| {
        vec![
            hoist(vec![property("--tw-outline-style", Some("solid"), None)]),
            d("outline-style", "var(--tw-outline-style)"),
            d("outline-width", value),
        ]
    };
    if i.value.is_none() {
        if i.modifier.is_some() {
            return Err(Problem::Invalid);
        }
        return Ok(width(
            i.theme.get(&["--default-outline-width"]).unwrap_or("1px"),
        ));
    }
    if let Some((value, hint)) = i.arbitrary() {
        if matches!(
            hint.or_else(|| values::infer(value, &["color", "length", "number", "percentage"])),
            Some("length" | "number" | "percentage")
        ) {
            if i.modifier.is_some() {
                return Err(Problem::Invalid);
            }
            return Ok(width(value));
        }
        return Ok(vec![d("outline-color", &invalid(i.color(&[]))?)]);
    }
    if let Some(value) = i.color(&["--outline-color", "--color"]) {
        return Ok(vec![d("outline-color", &value)]);
    }
    if i.modifier.is_some() {
        return Err(Problem::Invalid);
    }
    let name = invalid(i.named())?;
    Ok(width(&invalid(
        i.theme
            .resolve(Some(name), &["--outline-width"], ThemeOptions::NONE)
            .or_else(|| values::integer(name).then(|| format!("{name}px"))),
    )?))
}
fn stroke(i: &Input<'_>) -> Compile {
    if let Some((value, hint)) = i.arbitrary() {
        if matches!(
            hint.or_else(|| values::infer(value, &["color", "number", "length", "percentage"])),
            Some("number" | "length" | "percentage")
        ) {
            if i.modifier.is_some() {
                return Err(Problem::Invalid);
            }
            return Ok(vec![d("stroke-width", value)]);
        }
        return Ok(vec![d("stroke", &invalid(i.color(&[]))?)]);
    }
    if let Some(value) = i.color(&["--stroke", "--color"]) {
        return Ok(vec![d("stroke", &value)]);
    }
    let name = invalid(i.named())?;
    Ok(vec![d(
        "stroke-width",
        &invalid(
            i.theme
                .resolve(Some(name), &["--stroke-width"], ThemeOptions::NONE)
                .or_else(|| values::integer(name).then(|| name.into())),
        )?,
    )])
}
fn decoration(i: &Input<'_>) -> Compile {
    if let Some((value, hint)) = i.arbitrary() {
        if matches!(
            hint.or_else(|| values::infer(value, &["color", "length", "percentage"])),
            Some("length" | "percentage")
        ) {
            if i.modifier.is_some() {
                return Err(Problem::Invalid);
            }
            return Ok(vec![d("text-decoration-thickness", value)]);
        }
        return Ok(vec![d("text-decoration-color", &invalid(i.color(&[]))?)]);
    }
    let name = invalid(i.named())?;
    if let Some(value) = i
        .theme
        .resolve(
            Some(name),
            &["--text-decoration-thickness"],
            ThemeOptions::NONE,
        )
        .or_else(|| values::integer(name).then(|| format!("{name}px")))
    {
        if i.modifier.is_some() {
            return Err(Problem::Invalid);
        }
        return Ok(vec![d("text-decoration-thickness", &value)]);
    }
    Ok(vec![d(
        "text-decoration-color",
        &invalid(i.color(&["--text-decoration-color", "--color"]))?,
    )])
}
