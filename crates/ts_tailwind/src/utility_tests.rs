use super::*;

fn theme() -> Theme {
    let mut t = Theme::default();
    for (key, value) in [
        ("--spacing", "0.25rem"),
        ("--color-red-500", "#f00"),
        ("--text-sm", "0.875rem"),
        ("--text-sm--line-height", "1.25rem"),
        ("--font-weight-bold", "700"),
        ("--radius-lg", "0.5rem"),
        ("--animate-spin", "spin 1s linear infinite"),
        ("--shadow-sm", "0 1px 2px var(--color-shadow)"),
    ] {
        t.add(key, value, ThemeOptions::NONE, None).unwrap();
    }
    t
}
fn result(raw: &str, t: &Theme) -> UtilityResult {
    let u = Utilities::new();
    let parsed = crate::candidate::parse_candidate(raw, &u.candidate_registry(t));
    let mut result = UtilityResult::InvalidCandidate;
    for candidate in parsed {
        result = u.compile_base(&candidate, t);
        if matches!(result, UtilityResult::Generated { .. }) {
            break;
        }
    }
    result
}
fn generated(raw: &str, t: &Theme) -> GeneratedUtility {
    match result(raw, t) {
        UtilityResult::Generated { utility } => utility,
        value => panic!("{raw}: {value:?}"),
    }
}
fn declarations(nodes: &[AstNode]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for node in nodes {
        match &node.kind {
            NodeKind::Declaration {
                property,
                value: Some(value),
                ..
            } => out.push((property.clone(), value.clone())),
            NodeKind::Rule { nodes, .. } | NodeKind::AtRule { nodes, .. } => {
                out.extend(declarations(nodes))
            }
            _ => {}
        }
    }
    out
}
fn has(raw: &str, t: &Theme, property: &str, value: &str) {
    let output = generated(raw, t);
    assert!(
        declarations(&output.nodes).contains(&(property.into(), value.into())),
        "{raw}: {:?}",
        output.nodes
    );
}

#[test]
fn static_layout_and_explicit_incomplete_families() {
    let t = theme();
    for (raw, property, value) in [
        ("flex", "display", "flex"),
        ("hidden", "display", "none"),
        ("items-center", "align-items", "center"),
        ("overflow-x-auto", "overflow-x", "auto"),
    ] {
        has(raw, &t, property, value);
    }
    // A discovered but unported registration must never be mistaken for invalid
    // input or silently generate an empty successful rule.
    let mut u = Utilities::new();
    u.known_functional.insert("future-test-generator".into());
    let parsed =
        crate::candidate::parse_candidate("future-test-generator-1", &u.candidate_registry(&t));
    assert!(matches!(
        u.compile_base(&parsed[0], &t),
        UtilityResult::Unsupported { .. }
    ));
    assert!(matches!(
        result("invented-utility", &t),
        UtilityResult::InvalidCandidate
    ));
}

#[test]
fn spacing_fraction_negative_and_theme_override() {
    let mut t = theme();
    has("p-4", &t, "padding", "calc(var(--spacing) * 4)");
    has("-mt-2.5", &t, "margin-top", "calc(var(--spacing) * -2.5)");
    has("w-1/2", &t, "width", "calc(1 / 2 * 100%)");
    has("size-px", &t, "height", "1px");
    has("mx-auto", &t, "margin-inline", "auto");
    t.add("--spacing-4", "3rem", ThemeOptions::NONE, None)
        .unwrap();
    has("p-4", &t, "padding", "var(--spacing-4)");
    for raw in ["p-1.1", "-p-4", "p-4/2", "w-[3rem]/2"] {
        assert!(
            matches!(result(raw, &t), UtilityResult::InvalidCandidate),
            "{raw}"
        );
    }
}

#[test]
fn grid_arbitrary_span_and_origin_contracts() {
    let t = theme();
    has(
        "grid-cols-3",
        &t,
        "grid-template-columns",
        "repeat(3, minmax(0, 1fr))",
    );
    has(
        "col-span-[var(--columns)]",
        &t,
        "grid-column",
        "span var(--columns) / span var(--columns)",
    );
    has("row-span-full", &t, "grid-row", "1 / -1");
    has("-col-start-2", &t, "grid-column-start", "calc(2 * -1)");
    has("origin-top-right", &t, "transform-origin", "100% 0");
    has("object-top-right", &t, "object-position", "right top");
    assert!(matches!(
        result("grid-cols-0", &t),
        UtilityResult::InvalidCandidate
    ));
}

#[test]
fn typography_color_alpha_and_disambiguation() {
    let t = theme();
    has("text-sm", &t, "font-size", "var(--text-sm)");
    has(
        "text-sm",
        &t,
        "line-height",
        "var(--tw-leading, var(--text-sm--line-height))",
    );
    has("text-sm/6", &t, "line-height", "calc(var(--spacing) * 6)");
    has("text-[length:14px]/[1.5]", &t, "line-height", "1.5");
    has(
        "text-red-500/50",
        &t,
        "color",
        "color-mix(in oklab, var(--color-red-500) 50%, transparent)",
    );
    has(
        "bg-[url(/picture.png)]",
        &t,
        "background-image",
        "url(/picture.png)",
    );
    has("font-bold", &t, "font-weight", "var(--font-weight-bold)");
    let mut ambiguous = t;
    ambiguous
        .add("--color-2", "red", ThemeOptions::NONE, None)
        .unwrap();
    has(
        "decoration-2",
        &ambiguous,
        "text-decoration-thickness",
        "2px",
    );
    assert!(matches!(
        result("decoration-2/50", &ambiguous),
        UtilityResult::InvalidCandidate
    ));
}

#[test]
fn border_ring_and_transform_support_properties() {
    let t = theme();
    has("border-x-2", &t, "border-inline-width", "2px");
    has("rounded-lg", &t, "border-radius", "var(--radius-lg)");
    has(
        "ring-red-500/50",
        &t,
        "--tw-ring-color",
        "color-mix(in oklab, var(--color-red-500) 50%, transparent)",
    );
    let ring = generated("ring-2", &t);
    assert!(ring.nodes.iter().any(|n| matches!(&n.kind, NodeKind::AtRoot { nodes } if nodes.iter().any(|n| matches!(&n.kind, NodeKind::AtRule { name, params, .. } if name == "@property" && params == "--tw-ring-shadow")))));
    has("scale-x-50", &t, "--tw-scale-x", "50%");
    has("rotate-x-45", &t, "--tw-rotate-x", "rotateX(45deg)");
    assert!(matches!(
        result("scale-x-50.5", &t),
        UtilityResult::InvalidCandidate
    ));
}

#[test]
fn shadows_emit_relative_color_fallback_and_animation_stays_theme_reference() {
    let t = theme();
    let shadow = generated("shadow-sm/50", &t);
    assert!(shadow.nodes.iter().any(|n| matches!(&n.kind, NodeKind::AtRule { name, params, .. } if name == "@supports" && params == "(color: lab(from red l a b))")));
    has(
        "shadow-sm/50",
        &t,
        "--tw-shadow",
        "0 1px 2px var(--tw-shadow-color, var(--color-shadow))",
    );
    has("animate-spin", &t, "animation", "var(--animate-spin)");
    has("transition-opacity", &t, "transition-property", "opacity");
    has("duration-150", &t, "transition-duration", "150ms");
}

#[test]
fn sorting_counts_nested_declarations_and_excludes_hoisted_support() {
    let u = Utilities::new();
    let t = theme();
    let space = generated("space-x-reverse", &t);
    let gap = generated("gap-y-4", &t);
    assert_eq!(space.property_order, gap.property_order);
    assert_eq!(space.declaration_count, 2);
    let nodes = vec![
        hoist(vec![property("--example", Some("0"), None)]),
        rule("& > *", vec![d("display", "flex")]),
    ];
    assert_eq!(u.property_sort(&nodes).1, 1);
}

#[test]
fn filters_compose_and_drop_shadows_preserve_dynamic_fallbacks() {
    let mut t = theme();
    t.add("--blur-sm", "8px", ThemeOptions::NONE, None).unwrap();
    t.add(
        "--drop-shadow-sm",
        "0 1px 2px var(--shadow-color)",
        ThemeOptions::NONE,
        None,
    )
    .unwrap();
    has("blur-sm", &t, "--tw-blur", "blur(var(--blur-sm))");
    has("blur-none", &t, "--tw-blur", " ");
    has("grayscale", &t, "--tw-grayscale", "grayscale(100%)");
    has(
        "-backdrop-hue-rotate-45",
        &t,
        "--tw-backdrop-hue-rotate",
        "hue-rotate(calc(45deg * -1))",
    );
    has(
        "backdrop-opacity-12.5",
        &t,
        "--tw-backdrop-opacity",
        "opacity(12.5%)",
    );
    has(
        "backdrop-filter-[url(#fx)]",
        &t,
        "-webkit-backdrop-filter",
        "url(#fx)",
    );
    has(
        "drop-shadow-sm",
        &t,
        "--tw-drop-shadow",
        "drop-shadow(var(--drop-shadow-sm))",
    );
    has(
        "drop-shadow-sm/50",
        &t,
        "--tw-drop-shadow",
        "var(--tw-drop-shadow-size)",
    );
    let nodes = generated("drop-shadow-sm/50", &t).nodes;
    assert!(nodes.iter().any(|n| matches!(&n.kind, NodeKind::AtRule { name, params, .. } if name == "@supports" && params == "(color: lab(from red l a b))")));
    for raw in [
        "brightness-1.5",
        "filter/50",
        "drop-shadow-sm/nonsense",
        "blur-[4px]/50",
    ] {
        assert!(
            matches!(result(raw, &t), UtilityResult::InvalidCandidate),
            "{raw}"
        );
    }
}

#[test]
fn gradient_interpolation_stops_and_arbitrary_negative_angles() {
    let t = theme();
    has(
        "bg-linear-to-r/longer",
        &t,
        "--tw-gradient-position",
        "to right in oklch longer hue",
    );
    has(
        "-bg-linear-[45deg]",
        &t,
        "--tw-gradient-position",
        "calc(45deg * -1)",
    );
    has(
        "bg-conic-90/srgb",
        &t,
        "--tw-gradient-position",
        "from 90deg in srgb",
    );
    has(
        "bg-radial",
        &t,
        "background-image",
        "radial-gradient(var(--tw-gradient-stops))",
    );
    has(
        "from-red-500/50",
        &t,
        "--tw-gradient-from",
        "color-mix(in oklab, var(--color-red-500) 50%, transparent)",
    );
    has("via-50%", &t, "--tw-gradient-via-position", "50%");
    has("to-[length:12px]", &t, "--tw-gradient-to-position", "12px");
    for raw in [
        "-bg-linear-to-r",
        "bg-radial-10",
        "from-12.5%",
        "bg-linear-[45deg]/oklab",
    ] {
        assert!(
            matches!(result(raw, &t), UtilityResult::InvalidCandidate),
            "{raw}"
        );
    }
}

#[test]
fn mask_edges_and_shapes_emit_real_composites() {
    let t = theme();
    has(
        "-mask-linear-45",
        &t,
        "--tw-mask-linear-position",
        "calc(1deg * -45)",
    );
    has(
        "mask-x-from-4",
        &t,
        "--tw-mask-left-from-position",
        "calc(var(--spacing) * 4)",
    );
    has(
        "mask-x-from-4",
        &t,
        "--tw-mask-right-from-position",
        "calc(var(--spacing) * 4)",
    );
    has(
        "mask-t-to-red-500/50",
        &t,
        "--tw-mask-top-to-color",
        "color-mix(in oklab, var(--color-red-500) 50%, transparent)",
    );
    has("mask-radial-from-50%", &t, "mask-composite", "intersect");
    has(
        "mask-radial-[40px_80px]",
        &t,
        "--tw-mask-radial-size",
        "40px 80px",
    );
    has(
        "mask-conic-to-[var(--end)]",
        &t,
        "--tw-mask-conic-to-position",
        "var(--end)",
    );
    has("mask-[url(/mask.svg)]", &t, "mask-image", "url(/mask.svg)");
    for raw in [
        "mask-linear",
        "mask-linear-12.5",
        "mask-t-from-[12.5%]",
        "mask-x-to-4/50",
        "mask-radial-30",
    ] {
        assert!(
            matches!(result(raw, &t), UtilityResult::InvalidCandidate),
            "{raw}"
        );
    }
}

#[test]
fn structural_utilities_hoist_support_and_keep_nested_declarations() {
    let t = theme();
    has(
        "divide-x-2",
        &t,
        "border-inline-start-width",
        "calc(2px * var(--tw-divide-x-reverse))",
    );
    has(
        "divide-y",
        &t,
        "border-bottom-width",
        "calc(1px * calc(1 - var(--tw-divide-y-reverse)))",
    );
    has("divide-x-reverse", &t, "--tw-divide-x-reverse", "1");
    has("divide-dashed", &t, "border-style", "dashed");
    has("touch-pan-left", &t, "--tw-pan-x", "pan-left");
    has(
        "snap-x",
        &t,
        "scroll-snap-type",
        "x var(--tw-scroll-snap-strictness)",
    );
    has(
        "contain-inline-size",
        &t,
        "--tw-contain-size",
        "inline-size",
    );
    has("font-stretch-125%", &t, "font-stretch", "125%");
    assert!(matches!(
        result("font-stretch-201%", &t),
        UtilityResult::InvalidCandidate
    ));
}
