use super::*;
pub(super) fn register(u: &mut Utilities) {
    for (root, prop) in [
        ("inset", "inset"),
        ("inset-x", "inset-inline"),
        ("inset-y", "inset-block"),
        ("inset-s", "inset-inline-start"),
        ("inset-e", "inset-inline-end"),
        ("inset-bs", "inset-block-start"),
        ("inset-be", "inset-block-end"),
        ("top", "top"),
        ("right", "right"),
        ("bottom", "bottom"),
        ("left", "left"),
    ] {
        u.spacing(
            root,
            &[prop],
            &["--inset", "--spacing"],
            true,
            true,
            Handler::Plain,
        );
        for (suffix, value) in [("auto", "auto"), ("full", "100%")] {
            u.static_rule(&format!("{root}-{suffix}"), vec![d(prop, value)]);
        }
        u.static_rule(&format!("-{root}-full"), vec![d(prop, "-100%")]);
    }
    let sides = [
        ("", ""),
        ("x", "-inline"),
        ("y", "-block"),
        ("s", "-inline-start"),
        ("e", "-inline-end"),
        ("bs", "-block-start"),
        ("be", "-block-end"),
        ("t", "-top"),
        ("r", "-right"),
        ("b", "-bottom"),
        ("l", "-left"),
    ];
    for (prefix, property, theme, negative) in [
        ("m", "margin", "--margin", true),
        ("p", "padding", "--padding", false),
        ("scroll-m", "scroll-margin", "--scroll-margin", true),
        ("scroll-p", "scroll-padding", "--scroll-padding", false),
    ] {
        for (suffix, side) in sides {
            let root = format!("{prefix}{suffix}");
            let prop = format!("{property}{side}");
            u.spacing(
                &root,
                &[&prop],
                &[theme, "--spacing"],
                negative,
                false,
                Handler::Plain,
            );
            if prefix == "m" {
                u.static_rule(&format!("{root}-auto"), vec![d(&prop, "auto")]);
            }
        }
    }
    for (root, props, keys) in [
        ("size", vec!["width", "height"], vec!["--size", "--spacing"]),
        (
            "w",
            vec!["width"],
            vec!["--width", "--spacing", "--container"],
        ),
        (
            "min-w",
            vec!["min-width"],
            vec!["--min-width", "--spacing", "--container"],
        ),
        (
            "max-w",
            vec!["max-width"],
            vec!["--max-width", "--spacing", "--container"],
        ),
        ("h", vec!["height"], vec!["--height", "--spacing"]),
        (
            "min-h",
            vec!["min-height"],
            vec!["--min-height", "--height", "--spacing"],
        ),
        (
            "max-h",
            vec!["max-height"],
            vec!["--max-height", "--height", "--spacing"],
        ),
        (
            "inline",
            vec!["inline-size"],
            vec!["--spacing", "--container"],
        ),
        (
            "min-inline",
            vec!["min-inline-size"],
            vec!["--spacing", "--container"],
        ),
        (
            "max-inline",
            vec!["max-inline-size"],
            vec!["--spacing", "--container"],
        ),
        ("block", vec!["block-size"], vec!["--spacing"]),
        ("min-block", vec!["min-block-size"], vec!["--spacing"]),
        ("max-block", vec!["max-block-size"], vec!["--spacing"]),
    ] {
        let handler = if root == "size" {
            Handler::Size
        } else {
            Handler::Plain
        };
        u.spacing(root, &props, &keys, false, true, handler);
        for (suffix, value) in [
            ("full", "100%"),
            ("min", "min-content"),
            ("max", "max-content"),
            ("fit", "fit-content"),
            ("svw", "100svw"),
            ("lvw", "100lvw"),
            ("dvw", "100dvw"),
            ("svh", "100svh"),
            ("lvh", "100lvh"),
            ("dvh", "100dvh"),
        ] {
            if (root.contains("inline") && suffix.ends_with("vh"))
                || (root.contains("block") && suffix.ends_with("vw"))
            {
                continue;
            }
            let spec = &u.specs[root];
            u.static_rule(&format!("{root}-{suffix}"), render(spec, root, value));
        }
    }
    for (root, prop, keys) in [
        (
            "basis",
            "flex-basis",
            vec!["--flex-basis", "--spacing", "--container"],
        ),
        ("gap", "gap", vec!["--gap", "--spacing"]),
        ("gap-x", "column-gap", vec!["--gap", "--spacing"]),
        ("gap-y", "row-gap", vec!["--gap", "--spacing"]),
        ("indent", "text-indent", vec!["--text-indent", "--spacing"]),
    ] {
        u.spacing(
            root,
            &[prop],
            &keys,
            root == "indent",
            root == "basis",
            Handler::Plain,
        );
    }
    for (root, prop) in [("space-x", "margin-inline"), ("space-y", "margin-block")] {
        u.spacing(
            root,
            &[prop],
            &["--space", "--spacing"],
            true,
            false,
            Handler::Space,
        );
        let axis = if root == "space-x" { "x" } else { "y" };
        let name = format!("--tw-space-{axis}-reverse");
        u.static_rule(
            &format!("{root}-reverse"),
            vec![
                hoist(vec![property(&name, Some("0"), None)]),
                rule(
                    ":where(& > :not(:last-child))",
                    vec![
                        d(
                            "--tw-sort",
                            if axis == "x" { "row-gap" } else { "column-gap" },
                        ),
                        d(&name, "1"),
                    ],
                ),
            ],
        );
    }
    for (root, props) in [
        (
            "border-spacing",
            vec!["--tw-border-spacing-x", "--tw-border-spacing-y"],
        ),
        ("border-spacing-x", vec!["--tw-border-spacing-x"]),
        ("border-spacing-y", vec!["--tw-border-spacing-y"]),
    ] {
        u.spacing(
            root,
            &props,
            &["--border-spacing", "--spacing"],
            false,
            false,
            Handler::BorderSpacing,
        );
    }
    u.spacing(
        "leading",
        &["line-height"],
        &["--leading", "--spacing"],
        false,
        false,
        Handler::Leading,
    );
    u.specs
        .get_mut("leading")
        .unwrap()
        .fallback
        .insert("none".into(), "1".into());
    for (root, prop, key, bare, negative, default) in [
        ("z", "z-index", "--z-index", Bare::Integer, true, None),
        ("order", "order", "--order", Bare::Integer, true, None),
        ("shrink", "flex-shrink", "", Bare::Integer, false, Some("1")),
        ("grow", "flex-grow", "", Bare::Integer, false, Some("1")),
        (
            "columns",
            "columns",
            "--columns",
            Bare::Integer,
            false,
            None,
        ),
        (
            "opacity",
            "opacity",
            "--opacity",
            Bare::Percent,
            false,
            None,
        ),
        (
            "underline-offset",
            "text-underline-offset",
            "--text-underline-offset",
            Bare::Pixels,
            true,
            None,
        ),
        (
            "outline-offset",
            "outline-offset",
            "--outline-offset",
            Bare::Pixels,
            true,
            None,
        ),
        (
            "delay",
            "transition-delay",
            "--transition-delay",
            Bare::Milliseconds,
            false,
            None,
        ),
        ("cursor", "cursor", "--cursor", Bare::None, false, None),
        ("align", "vertical-align", "", Bare::None, false, None),
        ("will-change", "will-change", "", Bare::None, false, None),
        (
            "font-features",
            "font-feature-settings",
            "",
            Bare::None,
            false,
            None,
        ),
        ("animate", "animation", "--animate", Bare::None, false, None),
        (
            "list",
            "list-style-type",
            "--list-style-type",
            Bare::None,
            false,
            None,
        ),
        (
            "list-image",
            "list-style-image",
            "--list-style-image",
            Bare::None,
            false,
            None,
        ),
        (
            "perspective",
            "perspective",
            "--perspective",
            Bare::None,
            false,
            None,
        ),
    ] {
        let keys = if key.is_empty() {
            Vec::new()
        } else {
            vec![key]
        };
        let mut spec = Spec::new(&[prop], &keys, bare);
        spec.negative = negative;
        spec.default = default.map(str::to_string);
        match root {
            "z" | "columns" | "underline-offset" => {
                spec.fallback.insert("auto".into(), "auto".into());
            }
            "order" => {
                spec = spec.fallback(&[("first", "-9999"), ("last", "9999")]);
            }
            "animate" | "list-image" | "perspective" => {
                spec.fallback.insert("none".into(), "none".into());
            }
            "list" => {
                spec = spec.fallback(&[("none", "none"), ("disc", "disc"), ("decimal", "decimal")]);
            }
            _ => {}
        }
        if root == "columns" {
            spec.themes.push("--container".into());
        }
        u.spec(root, spec);
    }
    for (root, prop, key) in [
        ("duration", "transition-duration", "--transition-duration"),
        ("ease", "transition-timing-function", "--ease"),
        ("tracking", "letter-spacing", "--tracking"),
        ("content", "content", "--content"),
    ] {
        let mut s = Spec::new(
            &[prop],
            &[key],
            if root == "duration" {
                Bare::Milliseconds
            } else {
                Bare::None
            },
        );
        s.handler = match root {
            "duration" => Handler::Duration,
            "ease" => Handler::Ease,
            "tracking" => Handler::Tracking,
            _ => Handler::Content,
        };
        s.negative = root == "tracking";
        if root == "duration" {
            s.theme_default = false;
        }
        if root == "ease" {
            s.fallback.insert("linear".into(), "linear".into());
        }
        u.spec(root, s);
    }
    for axis in ["col", "row"] {
        let dimension = if axis == "col" { "column" } else { "row" };
        for side in ["", "-start", "-end"] {
            let root = format!("{axis}{side}");
            let prop = format!("grid-{dimension}{side}");
            let key = format!("--{prop}");
            let mut s = Spec::new(&[&prop], &[&key], Bare::Integer).fallback(&[("auto", "auto")]);
            s.negative = true;
            u.spec(&root, s);
        }
        u.known_functional.insert(format!("{axis}-span"));
    }
    for (root, prop, key, bare, fall) in [
        (
            "grid-cols",
            "grid-template-columns",
            "--grid-template-columns",
            Bare::Grid,
            vec![("none", "none"), ("subgrid", "subgrid")],
        ),
        (
            "grid-rows",
            "grid-template-rows",
            "--grid-template-rows",
            Bare::Grid,
            vec![("none", "none"), ("subgrid", "subgrid")],
        ),
        (
            "auto-cols",
            "grid-auto-columns",
            "--grid-auto-columns",
            Bare::None,
            vec![
                ("auto", "auto"),
                ("min", "min-content"),
                ("max", "max-content"),
                ("fr", "minmax(0, 1fr)"),
            ],
        ),
        (
            "auto-rows",
            "grid-auto-rows",
            "--grid-auto-rows",
            Bare::None,
            vec![
                ("auto", "auto"),
                ("min", "min-content"),
                ("max", "max-content"),
                ("fr", "minmax(0, 1fr)"),
            ],
        ),
    ] {
        u.spec(root, Spec::new(&[prop], &[key], bare).fallback(&fall));
    }
    u.spec(
        "aspect",
        Spec::new(&["aspect-ratio"], &["--aspect"], Bare::Aspect)
            .fallback(&[("auto", "auto"), ("square", "1 / 1")]),
    );
    let mut clamp = Spec::new(&["-webkit-line-clamp"], &["--line-clamp"], Bare::Integer)
        .fallback(&[("none", "none")]);
    clamp.handler = Handler::LineClamp;
    u.spec("line-clamp", clamp);
    for (root, props) in [
        ("rounded", vec!["border-radius"]),
        (
            "rounded-s",
            vec!["border-start-start-radius", "border-end-start-radius"],
        ),
        (
            "rounded-e",
            vec!["border-start-end-radius", "border-end-end-radius"],
        ),
        (
            "rounded-t",
            vec!["border-top-left-radius", "border-top-right-radius"],
        ),
        (
            "rounded-r",
            vec!["border-top-right-radius", "border-bottom-right-radius"],
        ),
        (
            "rounded-b",
            vec!["border-bottom-right-radius", "border-bottom-left-radius"],
        ),
        (
            "rounded-l",
            vec!["border-top-left-radius", "border-bottom-left-radius"],
        ),
        ("rounded-ss", vec!["border-start-start-radius"]),
        ("rounded-se", vec!["border-start-end-radius"]),
        ("rounded-ee", vec!["border-end-end-radius"]),
        ("rounded-es", vec!["border-end-start-radius"]),
        ("rounded-tl", vec!["border-top-left-radius"]),
        ("rounded-tr", vec!["border-top-right-radius"]),
        ("rounded-br", vec!["border-bottom-right-radius"]),
        ("rounded-bl", vec!["border-bottom-left-radius"]),
    ] {
        u.spec(
            root,
            Spec::new(&props, &["--radius"], Bare::None)
                .fallback(&[("none", "0"), ("full", "calc(infinity * 1px)")]),
        );
    }
    let positions = [
        ("center", "center"),
        ("top", "top"),
        ("top-right", "right top"),
        ("right", "right"),
        ("bottom-right", "right bottom"),
        ("bottom", "bottom"),
        ("bottom-left", "left bottom"),
        ("left", "left"),
        ("top-left", "left top"),
    ];
    for (root, prop, key) in [
        ("origin", "transform-origin", "--transform-origin"),
        (
            "perspective-origin",
            "perspective-origin",
            "--perspective-origin",
        ),
        ("object", "object-position", "--object-position"),
    ] {
        u.spec(
            root,
            Spec::new(&[prop], &[key], Bare::None).fallback(if root == "object" {
                &positions
            } else {
                &[
                    ("center", "center"),
                    ("top", "top"),
                    ("top-right", "100% 0"),
                    ("right", "100%"),
                    ("bottom-right", "100% 100%"),
                    ("bottom", "bottom"),
                    ("bottom-left", "0 100%"),
                    ("left", "0"),
                    ("top-left", "0 0"),
                ]
            }),
        );
    }
    for (root, prop, values) in [
        ("overflow", "overflow", "auto hidden clip visible scroll"),
        (
            "overflow-x",
            "overflow-x",
            "auto hidden clip visible scroll",
        ),
        (
            "overflow-y",
            "overflow-y",
            "auto hidden clip visible scroll",
        ),
        ("overscroll", "overscroll-behavior", "auto contain none"),
        ("overscroll-x", "overscroll-behavior-x", "auto contain none"),
        ("overscroll-y", "overscroll-behavior-y", "auto contain none"),
        (
            "cursor",
            "cursor",
            "auto default pointer wait text move help not-allowed none context-menu progress cell crosshair vertical-text alias copy no-drop grab grabbing all-scroll col-resize row-resize n-resize e-resize s-resize w-resize ne-resize nw-resize se-resize sw-resize ew-resize ns-resize nesw-resize nwse-resize zoom-in zoom-out",
        ),
        (
            "break-before",
            "break-before",
            "auto avoid all avoid-page page left right column",
        ),
        (
            "break-after",
            "break-after",
            "auto avoid all avoid-page page left right column",
        ),
        (
            "break-inside",
            "break-inside",
            "auto avoid avoid-page avoid-column",
        ),
        ("touch", "touch-action", "auto none manipulation"),
    ] {
        for value in values.split(' ') {
            u.static_rule(&format!("{root}-{value}"), vec![d(prop, value)]);
        }
    }
    for value in ["none", "text", "all", "auto"] {
        u.static_rule(
            &format!("select-{value}"),
            vec![d("-webkit-user-select", value), d("user-select", value)],
        );
    }
    for value in [
        "normal",
        "multiply",
        "screen",
        "overlay",
        "darken",
        "lighten",
        "color-dodge",
        "color-burn",
        "hard-light",
        "soft-light",
        "difference",
        "exclusion",
        "hue",
        "saturation",
        "color",
        "luminosity",
    ] {
        for (root, prop) in [
            ("bg-blend", "background-blend-mode"),
            ("mix-blend", "mix-blend-mode"),
        ] {
            u.static_rule(&format!("{root}-{value}"), vec![d(prop, value)]);
        }
    }
    // Known dynamic registrations not yet implemented remain visible to the parser
    // and produce Unsupported rather than silently disappearing as unknown tokens.
    for root in [
        "mask-linear",
        "mask-radial",
        "mask-conic",
        "mask-t",
        "mask-r",
        "mask-b",
        "mask-l",
        "mask-x",
        "mask-y",
        "bg-linear",
        "bg-conic",
        "bg-radial",
        "from",
        "via",
        "to",
        "filter",
        "backdrop-filter",
        "blur",
        "brightness",
        "contrast",
        "drop-shadow",
        "grayscale",
        "hue-rotate",
        "-hue-rotate",
        "-backdrop-hue-rotate",
        "-bg-linear",
        "-bg-conic",
        "invert",
        "saturate",
        "sepia",
        "backdrop-blur",
        "backdrop-brightness",
        "backdrop-contrast",
        "backdrop-grayscale",
        "backdrop-hue-rotate",
        "backdrop-invert",
        "backdrop-opacity",
        "backdrop-saturate",
        "backdrop-sepia",
        "divide-x",
        "divide-y",
    ] {
        u.known_functional.insert(root.into());
    }
}
