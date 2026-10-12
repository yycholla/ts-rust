use super::*;
fn theme() -> Theme {
    let mut theme = Theme::default();
    for (key, value) in [
        ("--spacing", "0.25rem"),
        ("--color-red", "red"),
        ("--breakpoint-sm", "40rem"),
    ] {
        theme.add(key, value, ThemeOptions::NONE, None).unwrap();
    }
    theme
}
fn value(value: &str, theme: &Theme) -> Result<String, FunctionError> {
    substitute_functions_in_value(value, &AstNode::declaration("x", value), theme)
}
#[test]
fn spacing_alpha_and_nested_functions_are_real_css() {
    let theme = theme();
    assert_eq!(
        value("--spacing(4)", &theme).unwrap(),
        "calc(var(--spacing) * 4)"
    );
    assert_eq!(
        value("calc(--spacing(2) + --spacing(3))", &theme).unwrap(),
        "calc(calc(var(--spacing) * 2) + calc(var(--spacing) * 3))"
    );
    assert_eq!(
        value("--alpha(--theme(--color-red) / 0.5)", &theme).unwrap(),
        "color-mix(in oklab, var(--color-red) 50%, transparent)"
    );
    assert_eq!(
        value("--alpha(rgb(1 2 3 / .2) / 1)", &theme).unwrap(),
        "rgb(1 2 3 / .2)"
    );
    assert_eq!(
        value("--alpha(red / var(--opacity))", &theme).unwrap(),
        "color-mix(in oklab, red var(--opacity), transparent)"
    );
    assert_eq!(value("--alpha(red / 0x1)", &theme).unwrap(), "red");
    assert_eq!(
        value("--alpha(red / -0)", &theme).unwrap(),
        "color-mix(in oklab, red 0%, transparent)"
    );
    assert_eq!(
        value("--alpha(red / 1e20)", &theme).unwrap(),
        "color-mix(in oklab, red 1e+22%, transparent)"
    );
}
#[test]
fn theme_inline_reference_prefix_and_fallback_injection() {
    let mut theme = theme();
    theme.prefix = Some("tw".into());
    assert_eq!(
        value("--theme(--color-red)", &theme).unwrap(),
        "var(--tw-color-red)"
    );
    assert_eq!(value("--theme(--color-red inline)", &theme).unwrap(), "red");
    assert_eq!(
        value("--theme(--color-red, blue)", &theme).unwrap(),
        "var(--tw-color-red, blue)"
    );
    assert_eq!(
        value("--theme(--color-red, initial)", &theme).unwrap(),
        "var(--tw-color-red)"
    );
    theme
        .add("--color-ref", "green", ThemeOptions::REFERENCE, None)
        .unwrap();
    assert_eq!(
        value("--theme(--color-ref, blue)", &theme).unwrap(),
        "var(--tw-color-ref, green)"
    );
    theme
        .add(
            "--color-alias",
            "var(--external, var(--nested, initial))",
            ThemeOptions::INLINE,
            None,
        )
        .unwrap();
    assert_eq!(
        value("--theme(--color-alias, blue)", &theme).unwrap(),
        "var(--external, var(--nested, blue))"
    );
    assert_eq!(
        value("--theme(--missing, Arial, sans-serif)", &theme).unwrap(),
        "Arial, sans-serif"
    );
    assert_eq!(
        value("--theme(--color-red / 50%)", &theme).unwrap(),
        "color-mix(in oklab, var(--tw-color-red) 50%, transparent)"
    );
}
#[test]
fn stylesheet_and_candidate_paths_preserve_ranges_and_at_rule_inline_rules() {
    let theme = theme();
    let source = "@media (width >= --theme(--breakpoint-sm)) { .x { width: --spacing(2); content: 'theme(--missing)'; } } @unknown --theme(--breakpoint-sm);";
    let ast = crate::css::parse(source, "entry.css").unwrap();
    let result = substitute_functions(&ast, &theme).unwrap();
    assert!(result.has_theme_functions);
    assert_eq!(result.ast[0].src, ast[0].src);
    assert!(
        matches!(&result.ast[0].kind, NodeKind::AtRule { params, .. } if params == "(width >= 40rem)")
    );
    assert_eq!(result.ast[1], ast[1]);
    let body = result.ast[0].children().unwrap()[0].children().unwrap();
    assert!(
        matches!(&body[0].kind, NodeKind::Declaration { value: Some(value), .. } if value == "calc(var(--spacing) * 2)")
    );
    assert!(
        matches!(&body[1].kind, NodeKind::Declaration { value: Some(value), .. } if value == "'theme(--missing)'")
    );
    for name in ["@custom-media", "@container", "@supports"] {
        let node = AstNode::new(NodeKind::AtRule {
            name: name.into(),
            params: "--theme(--breakpoint-sm)".into(),
            nodes: vec![],
        });
        assert!(
            matches!(&substitute_functions(&[node], &theme).unwrap().ast[0].kind, NodeKind::AtRule { params, .. } if params == "40rem")
        );
    }
}
#[test]
fn legacy_css_keys_resolve_inline_and_config_paths_are_explicitly_unsupported() {
    let theme = theme();
    assert_eq!(value("theme('--color-red')", &theme).unwrap(), "red");
    assert_eq!(
        value("theme(\"--color-red / 0.5\")", &theme).unwrap(),
        "color-mix(in oklab, red 50%, transparent)"
    );
    assert_eq!(
        value("theme(--missing, --spacing(3))", &theme).unwrap(),
        "calc(var(--spacing) * 3)"
    );
    assert_eq!(
        value("theme('colors.red.500', blue)", &theme)
            .unwrap_err()
            .kind,
        FunctionErrorKind::Unsupported
    );
    assert_eq!(
        value("--theme(colors.red)", &theme).unwrap_err().kind,
        FunctionErrorKind::Invalid
    );
    assert_eq!(unquote(r#"'--a\\b\'c'"#), "--a\\b'c");
}
#[test]
fn recursive_values_resolve_and_cycles_and_invalid_theme_values_fail() {
    let mut theme = theme();
    theme
        .add("--a", "--theme(--b inline)", ThemeOptions::INLINE, None)
        .unwrap();
    theme
        .add("--b", "--spacing(2)", ThemeOptions::INLINE, None)
        .unwrap();
    assert_eq!(
        value("--theme(--a)", &theme).unwrap(),
        "calc(var(--spacing) * 2)"
    );
    theme
        .add("--b", "--theme(--a)", ThemeOptions::INLINE, None)
        .unwrap();
    assert!(
        value("--theme(--a)", &theme)
            .unwrap_err()
            .message
            .contains("Circular")
    );
    theme
        .add("--b", "broken(", ThemeOptions::INLINE, None)
        .unwrap();
    assert!(
        value("--theme(--b)", &theme)
            .unwrap_err()
            .message
            .contains("Invalid resolved")
    );
}
#[test]
fn errors_are_located_and_substitution_does_not_partially_mutate_inputs() {
    let theme = theme();
    let ast = crate::css::parse(
        ".x { width: --spacing(2); color: --theme(--missing); }",
        "bad.css",
    )
    .unwrap();
    let before = ast.clone();
    let error = substitute_functions(&ast, &theme).unwrap_err();
    assert_eq!(error.source.unwrap().file, "bad.css");
    assert_eq!(ast, before);
    for input in [
        "--spacing()",
        "--spacing(1, 2)",
        "--alpha(red)",
        "--alpha(red / 50%, blue)",
        "--theme(--missing)",
    ] {
        assert_eq!(
            value(input, &theme).unwrap_err().kind,
            FunctionErrorKind::Invalid
        );
    }
    assert!(
        value("--spacing(2)", &Theme::default())
            .unwrap_err()
            .message
            .contains("--spacing")
    );
}
#[test]
fn strings_unknown_functions_and_expansion_limits_are_preserved_or_explicit() {
    let theme = theme();
    let raw = "'--theme(--missing)' CUSTOM(--theme(--color-red inline))";
    assert_eq!(
        value(raw, &theme).unwrap(),
        "'--theme(--missing)' CUSTOM(red)"
    );
    let many = "--theme(--color-red) ".repeat(MAX_SUBSTITUTIONS + 1);
    assert_eq!(
        value(&many, &theme).unwrap_err().kind,
        FunctionErrorKind::Limit
    );
    assert_eq!(
        value("--alpha(red / 0x10000000000000000)", &theme)
            .unwrap_err()
            .kind,
        FunctionErrorKind::Unsupported
    );
}
