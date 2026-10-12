//! Nested divide rules and static utilities requiring shared @property support.
use super::*;
pub(super) fn register(u: &mut Utilities) {
    u.spec(
        "font-stretch",
        Spec::new(&["font-stretch"], &[], Bare::FontStretch),
    );
    for axis in ["x", "y"] {
        let var = format!("--tw-divide-{axis}-reverse");
        u.known_functional.insert(format!("divide-{axis}"));
        u.static_rule(
            &format!("divide-{axis}-reverse"),
            vec![
                hoist(vec![property(&var, Some("0"), None)]),
                rule(":where(& > :not(:last-child))", vec![d(&var, "1")]),
            ],
        );
    }
    for value in ["solid", "dashed", "dotted", "double", "none"] {
        u.static_rule(
            &format!("divide-{value}"),
            vec![rule(
                ":where(& > :not(:last-child))",
                vec![
                    d("--tw-sort", "divide-style"),
                    d("--tw-border-style", value),
                    d("border-style", value),
                ],
            )],
        );
    }
    for value in ["x", "left", "right", "y", "up", "down", "pinch-zoom"] {
        let (name, var, value) = if value == "pinch-zoom" {
            ("touch-pinch-zoom".into(), "--tw-pinch-zoom", value.into())
        } else {
            (
                format!("touch-pan-{value}"),
                if matches!(value, "x" | "left" | "right") {
                    "--tw-pan-x"
                } else {
                    "--tw-pan-y"
                },
                format!("pan-{value}"),
            )
        };
        u.static_rule(
            &name,
            vec![
                hoist(
                    ["--tw-pan-x", "--tw-pan-y", "--tw-pinch-zoom"]
                        .iter()
                        .map(|p| property(p, None, None))
                        .collect(),
                ),
                d(var, &value),
                d(
                    "touch-action",
                    "var(--tw-pan-x,) var(--tw-pan-y,) var(--tw-pinch-zoom,)",
                ),
            ],
        );
    }
    for value in ["x", "y", "both", "mandatory", "proximity"] {
        let declaration = if matches!(value, "mandatory" | "proximity") {
            d("--tw-scroll-snap-strictness", value)
        } else {
            d(
                "scroll-snap-type",
                &format!("{value} var(--tw-scroll-snap-strictness)"),
            )
        };
        u.static_rule(
            &format!("snap-{value}"),
            vec![
                hoist(vec![property(
                    "--tw-scroll-snap-strictness",
                    Some("proximity"),
                    None,
                )]),
                declaration,
            ],
        );
    }
    for value in ["size", "inline-size", "layout", "paint", "style"] {
        let prop = if value == "inline-size" {
            "size"
        } else {
            value
        };
        u.static_rule(&format!("contain-{value}"), vec![hoist(["size", "layout", "paint", "style"].iter().map(|p| property(&format!("--tw-contain-{p}"), None, None)).collect()), d(&format!("--tw-contain-{prop}"), value), d("contain", "var(--tw-contain-size,) var(--tw-contain-layout,) var(--tw-contain-paint,) var(--tw-contain-style,)")]);
    }
    u.spec("contain", Spec::new(&["contain"], &[], Bare::None));
}
pub(super) fn compile(i: &Input<'_>) -> Option<Compile> {
    if !matches!(i.root, "divide-x" | "divide-y") {
        return None;
    }
    Some(divide(i))
}
fn divide(i: &Input<'_>) -> Compile {
    let mut spec = Spec::new(&[], &["--divide-width", "--border-width"], Bare::Pixels);
    spec.default = Some(
        i.theme
            .get(&["--default-border-width"])
            .unwrap_or("1px")
            .into(),
    );
    let value = invalid(resolve(&spec, i.value, i.modifier, false, i.theme))?;
    let x = i.root == "divide-x";
    let var = format!("--tw-{}-reverse", i.root);
    let mut nested = vec![
        d("--tw-sort", &format!("{}-width", i.root)),
        hoist(vec![property("--tw-border-style", Some("solid"), None)]),
        d(&var, "0"),
    ];
    if x {
        nested.push(d("border-inline-style", "var(--tw-border-style)"));
    } else {
        nested.extend([
            d("border-bottom-style", "var(--tw-border-style)"),
            d("border-top-style", "var(--tw-border-style)"),
        ]);
    }
    nested.extend([
        d(
            if x {
                "border-inline-start-width"
            } else {
                "border-top-width"
            },
            &format!("calc({value} * var({var}))"),
        ),
        d(
            if x {
                "border-inline-end-width"
            } else {
                "border-bottom-width"
            },
            &format!("calc({value} * calc(1 - var({var})))"),
        ),
    ]);
    Ok(vec![
        hoist(vec![property(&var, Some("0"), None)]),
        rule(":where(& > :not(:last-child))", nested),
    ])
}
