use super::*;

fn theme() -> Theme {
    let mut theme = Theme::default();
    for (key, value) in [
        ("--spacing", ".25rem"),
        ("--color-red", "red"),
        ("--breakpoint-sm", "40rem"),
        ("--breakpoint-md", "48rem"),
    ] {
        theme.add(key, value, ThemeOptions::NONE, None).unwrap();
    }
    theme
}
fn compile(raw: &[&str], theme: &Theme) -> CandidateCompilation {
    compile_candidates(
        &raw.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
        theme,
        &CompileOptions::default(),
    )
    .unwrap()
}
fn names(output: &CandidateCompilation) -> Vec<&str> {
    output.rules.iter().map(|r| r.candidate.as_str()).collect()
}
fn css(nodes: &[AstNode]) -> String {
    crate::ast::to_css(&mut nodes.to_vec(), "<test>").unwrap()
}
fn context(key: &str, nodes: Vec<AstNode>) -> AstNode {
    AstNode::new(NodeKind::Context {
        context: BTreeMap::from([(key.into(), ContextValue::Bool(true))]),
        nodes,
    })
}
fn root(nodes: Vec<AstNode>) -> AstNode {
    AstNode::new(NodeKind::AtRoot { nodes })
}
fn property(name: &str, initial: Option<&str>) -> AstNode {
    let mut nodes = vec![
        AstNode::declaration("syntax", "\"*\""),
        AstNode::declaration("inherits", "false"),
    ];
    if let Some(value) = initial {
        nodes.push(AstNode::declaration("initial-value", value));
    }
    at("@property", name, nodes)
}

#[test]
fn classes_sort_by_properties_count_then_natural_names() {
    let output = compile(
        &["p-10", "flex", "p-2", "p-1", "px-4", "hidden", "flex"],
        &theme(),
    );
    assert_eq!(
        names(&output),
        ["flex", "hidden", "p-1", "p-2", "p-10", "px-4"]
    );
    assert_eq!(output.generated.len(), 6);
    assert!(output.invalid.is_empty());
    assert!(css(&output.ast).contains(".p-10 {\n  padding: calc(var(--spacing) * 10);"));
}

#[test]
fn numeric_compare_preserves_js_representation_and_utf16_order() {
    assert_eq!(compare_candidates("w-2", "w-10"), Ordering::Less);
    assert_eq!(compare_candidates("w-02", "w-2"), Ordering::Less);
    assert_eq!(compare_candidates("red-500", "red-500/70"), Ordering::Less);
    // Surrogates precede BMP private-use characters in JS string comparison.
    assert_eq!(compare_candidates("x-😀", "x-\u{e000}"), Ordering::Less);
    assert_eq!(
        compare_candidates("x-9007199254740993", "x-9007199254740992"),
        Ordering::Greater
    );
    let huge = "9".repeat(400);
    assert_eq!(
        compare_candidates(&format!("x-{huge}"), &format!("x-{huge}0")),
        Ordering::Less
    );
}

#[test]
fn variant_order_uses_grouped_arbitrary_precision_bits() {
    let output = compile(
        &[
            "md:flex",
            "hover:flex",
            "focus:flex",
            "sm:flex",
            "flex",
            "hover:focus:flex",
            "min-[40rem]:flex",
        ],
        &theme(),
    );
    let names = names(&output);
    let pos = |name| names.iter().position(|v| *v == name).unwrap();
    assert!(pos("flex") < pos("hover:flex"));
    assert!(pos("hover:flex") < pos("focus:flex"));
    assert!(pos("focus:flex") < pos("hover:focus:flex"));
    assert!(pos("sm:flex") < pos("md:flex"));
    let bits = |name| {
        &output
            .rules
            .iter()
            .find(|r| r.candidate == name)
            .unwrap()
            .variant_order
    };
    assert_eq!(bits("sm:flex"), bits("min-[40rem]:flex"));
    let candidates: Vec<_> = (0..90)
        .rev()
        .map(|i| format!("[&[data-index='{i}']]:flex"))
        .collect();
    let many = compile_candidates(&candidates, &theme(), &CompileOptions::default()).unwrap();
    assert_eq!(many.rules.len(), 90);
    assert_eq!(many.rules.last().unwrap().variant_order, [89]);
}

#[test]
fn prefix_important_and_compound_variant_contract() {
    let mut theme = theme();
    theme.prefix = Some("tw".into());
    let raw = [
        "tw:group-hover/sidebar:flex!",
        "tw:p-2",
        "p-2",
        "tw:before:flex",
    ];
    let output = compile(&raw, &theme);
    assert_eq!(output.invalid, ["p-2"]);
    let text = css(&output.ast);
    assert!(text.contains(".tw\\:group-hover\\/sidebar\\:flex\\!"));
    assert!(text.contains("display: flex !important"));
    assert!(text.contains("var(--tw-spacing)"));
    assert!(text.contains("@property --tw-content"));
    let all = compile_candidates(
        &["p-2".into(), "ring-2!".into()],
        &Theme::default(),
        &CompileOptions {
            important: true,
            respect_important: false,
            property_fallbacks: false,
        },
    )
    .unwrap();
    let text = css(&all.ast);
    assert!(text.contains("box-shadow:"));
    assert!(!text.contains("initial-value: 0 0 #0000 !important"));
    let global = compile_candidates(
        &["flex".into()],
        &Theme::default(),
        &CompileOptions {
            important: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(css(&global.ast).contains("display: flex !important"));
}

#[test]
fn invalid_and_unimplemented_substitution_stay_separate() {
    let output = compile(
        &[
            "unknown-class",
            "[>img]:flex",
            "p-1.1",
            "w-[theme(--spacing)]",
            "flex",
        ],
        &theme(),
    );
    assert_eq!(output.generated, ["flex"]);
    assert_eq!(output.invalid, ["unknown-class", "[>img]:flex", "p-1.1"]);
    assert_eq!(output.unsupported.len(), 1);
    assert_eq!(output.unsupported[0].candidate, "w-[theme(--spacing)]");
    assert!(output.unsupported[0].reasons[0].contains("function-substitution"));
}

#[test]
fn lowering_hoists_deduplicates_and_drops_reference_ir() {
    let nodes = vec![
        at("@import", "url('external.css')", vec![]),
        rule(
            ".example",
            vec![
                AstNode::declaration("color", "red"),
                rule("&:hover", vec![AstNode::declaration("color", "blue")]),
                AstNode::declaration("color", "red"),
                AstNode::declaration("--tw-sort", "color"),
                root(vec![property("--x", Some("0"))]),
                context("reference", vec![AstNode::declaration("display", "none")]),
                rule("&", vec![AstNode::declaration("margin", "0")]),
            ],
        ),
        root(vec![property("--x", Some("99"))]),
    ];
    let lowered = lower_ast(nodes, &Theme::default(), true).unwrap();
    let text = css(&lowered.ast);
    assert_eq!(text.matches("@property --x").count(), 1);
    assert_eq!(text.matches("color: red;").count(), 1);
    assert!(text.find("&:hover").unwrap() < text.find("color: red;").unwrap());
    assert!(text.starts_with("@import url('external.css');\n@layer properties;"));
    assert!(text.contains("*, ::before, ::after, ::backdrop"));
    assert!(text.contains("--x: 0;"));
    assert!(!text.contains("99"));
    assert!(!text.contains("--tw-sort"));
    assert!(!text.contains("display: none"));
    assert!(!text.contains("& {"));
}

#[test]
fn theme_dependencies_reference_inline_static_and_keyframes() {
    let mut theme = theme();
    for (key, value, options) in [
        ("--color-brand", "var(--palette)", ThemeOptions::NONE),
        ("--palette", "#123456", ThemeOptions::NONE),
        ("--unused", "hotpink", ThemeOptions::NONE),
        ("--always", "42", ThemeOptions::STATIC),
        ("--color-reference", "blue", ThemeOptions::REFERENCE),
        ("--color-inline", "var(--external)", ThemeOptions::INLINE),
        (
            "--animate-spin",
            "spin 1s linear infinite",
            ThemeOptions::NONE,
        ),
    ] {
        theme.add(key, value, options, None).unwrap();
    }
    for name in ["spin", "unused"] {
        theme
            .add_keyframes(at(
                "@keyframes",
                name,
                vec![rule(
                    "to",
                    vec![AstNode::declaration("transform", "rotate(360deg)")],
                )],
            ))
            .unwrap();
    }
    let output = compile(
        &["bg-brand", "bg-reference", "text-inline", "animate-spin"],
        &theme,
    );
    let declarations = css(&output.theme.declarations);
    assert!(declarations.contains("--color-brand: var(--palette)"));
    assert!(declarations.contains("--palette: #123456"));
    assert!(declarations.contains("--always: 42"));
    assert!(!declarations.contains("--unused:"));
    assert!(!declarations.contains("--color-reference:"));
    assert!(!declarations.contains("--color-inline:"));
    assert!(css(&output.ast).contains("var(--color-reference, blue)"));
    assert_eq!(output.theme.keyframes.len(), 1);
    assert!(css(&output.theme.keyframes).contains("@keyframes spin"));
    assert!(output.theme.used_variables.contains(&"--external".into()));
}

#[test]
fn color_mix_compatibility_runs_and_cyclic_theme_matches_upstream() {
    let mut theme = theme();
    theme
        .add("--a", "var(--b)", ThemeOptions::NONE, None)
        .unwrap();
    theme
        .add("--b", "var(--a)", ThemeOptions::NONE, None)
        .unwrap();
    let output = compile(&["bg-red/50"], &theme);
    assert!(output.remaining_passes.is_empty());
    assert!(css(&output.ast).contains("@supports (color: color-mix(in lab, red, red))"));
    let text = css(&output.theme.declarations);
    assert!(text.contains("--a:"));
    assert!(text.contains("--b:"));
}

#[test]
fn theme_context_removes_unused_rules_and_empty_layer_ancestors() {
    let nodes = vec![
        at(
            "@layer",
            "theme",
            vec![rule(
                ":root",
                vec![context(
                    "theme",
                    vec![AstNode::declaration("--unused", "red")],
                )],
            )],
        ),
        at("@layer", "utilities", vec![]),
    ];
    let output = lower_ast(nodes, &Theme::default(), false).unwrap();
    assert_eq!(css(&output.ast), "@layer utilities;\n");
}
