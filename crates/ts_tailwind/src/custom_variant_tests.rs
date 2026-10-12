use super::*;
fn parse(css: &str) -> Vec<AstNode> {
    crate::css::parse(css, "variants.css").unwrap()
}
fn variant(variants: &Variants, name: &str) -> Variant {
    let mut registry = CandidateRegistry::default();
    variants.register_candidates(&mut registry);
    parse_variant(name, &registry).unwrap()
}
fn apply(variants: &Variants, name: &str) -> Vec<AstNode> {
    variants
        .apply(
            &variant(variants, name),
            vec![AstNode::declaration("display", "flex")],
        )
        .unwrap()
}
#[test]
fn selector_lists_group_styles_before_at_rules_and_keep_overwrite_positions() {
    let mut variants = Variants::new(&Theme::default());
    let hover_order = variants.metadata("hover").unwrap().order;
    variants
        .register_custom_selectors(
            "hocus",
            vec!["@media print".into(), " &:hover ".into(), "&:focus".into()],
        )
        .unwrap();
    let order = variants.metadata("hocus").unwrap().order;
    let output = apply(&variants, "hocus");
    assert!(
        matches!(&output[0].kind, NodeKind::Rule { selector, .. } if selector == "&:hover, &:focus")
    );
    assert!(
        matches!(&output[1].kind, NodeKind::AtRule { name, params, .. } if name == "@media" && params == "print")
    );
    variants
        .register_custom_selectors("hocus", vec!["&:active".into()])
        .unwrap();
    variants
        .register_custom_selectors("hover", vec!["&:focus".into()])
        .unwrap();
    assert_eq!(variants.metadata("hocus").unwrap().order, order);
    assert_eq!(variants.metadata("hover").unwrap().order, hover_order);
    assert_eq!(variants.keys().filter(|s| *s == "hocus").count(), 1);
    assert!(
        matches!(&apply(&variants, "hover")[0].kind, NodeKind::Rule { selector, .. } if selector == "&:focus")
    );
}
#[test]
fn slots_duplicate_real_bodies_and_property_keyframes_hoist() {
    let mut variants = Variants::new(&Theme::default());
    let body = parse(
        "&:hover { @slot; } &:focus { @slot; } @property --custom { syntax: '<number>'; inherits: false; initial-value: 0; } @keyframes spin { to { rotate: 360deg; } }",
    );
    let property_source = body[2].src.clone();
    variants.register_custom_template("fancy", body).unwrap();
    let output = apply(&variants, "fancy");
    assert_eq!(output.len(), 4);
    for node in &output[..2] {
        assert_eq!(
            node.children().unwrap(),
            [AstNode::declaration("display", "flex")]
        );
    }
    for node in &output[2..] {
        assert!(matches!(node.kind, NodeKind::AtRoot { .. }));
    }
    assert_eq!(output[2].src, property_source);
    assert_eq!(
        variants.metadata("fancy").unwrap().compounds,
        Compounds::NEVER
    );
}
#[test]
fn forward_dependencies_expand_before_slots_without_reordering_registration() {
    let mut variants = Variants::new(&Theme::default());
    variants
        .register_custom_variants(&[
            CustomVariantDefinition {
                name: "outer".into(),
                body: CustomVariantBody::Template(parse("@variant inner { @slot; }")),
            },
            CustomVariantDefinition {
                name: "inner".into(),
                body: CustomVariantBody::Selectors(vec!["&:focus".into()]),
            },
        ])
        .unwrap();
    assert!(variants.metadata("outer").unwrap().order < variants.metadata("inner").unwrap().order);
    let output = apply(&variants, "outer");
    assert!(matches!(&output[0].kind, NodeKind::Rule { selector, .. } if selector == "&"));
    assert!(
        matches!(&output[0].children().unwrap()[0].kind, NodeKind::Rule { selector, .. } if selector == "&:focus")
    );
    assert_eq!(
        output[0].children().unwrap()[0].children().unwrap(),
        [AstNode::declaration("display", "flex")]
    );
    // Metadata comes from the original template, before expanding @variant.
    assert_eq!(
        variants.metadata("outer").unwrap().compounds,
        Compounds::NEVER
    );
}
#[test]
fn dependency_failures_are_transactional_and_detect_overwrite_cycles() {
    let mut variants = Variants::new(&Theme::default());
    let before = variants.keys().map(str::to_string).collect::<Vec<_>>();
    let error = variants
        .register_custom_variants(&[
            CustomVariantDefinition {
                name: "one".into(),
                body: CustomVariantBody::Template(parse("@variant two { @slot; }")),
            },
            CustomVariantDefinition {
                name: "two".into(),
                body: CustomVariantBody::Template(parse("@variant one { @slot; }")),
            },
        ])
        .unwrap_err();
    assert!(error.message.contains("one -> two -> one"));
    assert_eq!(
        variants.keys().collect::<Vec<_>>(),
        before.iter().map(String::as_str).collect::<Vec<_>>()
    );
    assert!(
        variants
            .register_custom_template("missing", parse("@variant absent { @slot; }"))
            .is_err()
    );
    variants
        .register_custom_template("custom", parse("&:hover { @variant focus { @slot; } }"))
        .unwrap();
    let error = variants
        .register_custom_template("focus", parse("@variant group-custom { @slot; }"))
        .unwrap_err();
    assert!(error.message.contains("Circular"));
    assert_eq!(
        variants.metadata("focus").unwrap().compounds,
        Compounds::STYLE_RULES
    );
}
#[test]
fn simple_templates_compound_using_real_selector_rules() {
    let mut variants = Variants::new(&Theme::default());
    variants
        .register_custom_template("hocus", parse("&:hover { @slot; } &:focus { @slot; }"))
        .unwrap();
    let output = apply(&variants, "group-hocus");
    assert_eq!(output.len(), 2);
    assert!(
        matches!(&output[0].kind, NodeKind::Rule { selector, .. } if selector == "&:is(:where(.group):hover *)")
    );
    assert!(
        matches!(&output[1].kind, NodeKind::Rule { selector, .. } if selector == "&:is(:where(.group):focus *)")
    );
    variants
        .register_custom_selectors("pseudos", vec!["&::before".into()])
        .unwrap();
    let mut registry = CandidateRegistry::default();
    variants.register_candidates(&mut registry);
    assert!(parse_variant("group-pseudos", &registry).is_none());
    assert!(
        variants
            .apply(&variant(&variants, "not-hocus"), vec![])
            .is_err()
    );
}
#[test]
fn direct_at_variant_lowering_nests_real_builtins_and_preserves_body() {
    let variants = Variants::new(&Theme::default());
    let source = parse("@variant hover { @variant focus { color: red; } }");
    let output = variants.substitute_at_variants(&source).unwrap();
    let mut rendered = output;
    let css = crate::ast::to_css(&mut rendered, "out.css").unwrap();
    assert!(css.contains("&:hover"));
    assert!(css.contains("@media (hover: hover)"));
    assert!(css.contains("&:focus"));
    assert!(css.contains("color: red"));
    assert!(
        variants
            .substitute_at_variants(&parse("@variant missing { x: y; }"))
            .is_err()
    );
}
#[test]
fn invalid_names_empty_definitions_and_unrepresentable_compounds_are_explicit() {
    let mut variants = Variants::new(&Theme::default());
    for name in ["Upper", "-bad", "bad_", "bad-", "", "@", "two words"] {
        assert!(
            variants
                .register_custom_selectors(name, vec!["&".into()])
                .is_err()
        );
    }
    assert!(variants.register_custom_selectors("empty", vec![]).is_err());
    assert!(variants.register_custom_template("empty", vec![]).is_err());
    variants
        .register_custom_selectors("@valid_2", vec!["&".into()])
        .unwrap();
    variants
        .register_custom_template("rich", parse("&:hover { color: red; @slot; }"))
        .unwrap();
    let output = apply(&variants, "rich");
    assert_eq!(output[0].children().unwrap().len(), 2);
    let grouped = apply(&variants, "group-rich");
    assert_eq!(grouped[0].children().unwrap().len(), 2);
    assert!(
        matches!(&grouped[0].kind, NodeKind::Rule { selector, .. } if selector == "&:is(:where(.group):hover *)")
    );
    assert_eq!(
        variants
            .apply(&variant(&variants, "not-rich"), vec![])
            .unwrap_err()
            .kind,
        VariantErrorKind::Unsupported
    );
}
