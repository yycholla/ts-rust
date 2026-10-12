use super::*;
use crate::{ast::to_css, css, theme::ThemeOptions};

fn theme(entries: &[(&str, &str)]) -> Theme {
    let mut theme = Theme::default();
    for (name, value) in entries {
        theme.add(name, value, ThemeOptions::NONE, None).unwrap();
    }
    theme
}
fn run(input: &str, theme: &Theme) -> (String, ColorMixResult) {
    let ast = css::parse(input, "input.css").unwrap();
    let result = color_mix_compatibility(&ast, theme).unwrap();
    let css = to_css(&mut result.ast.clone(), "output.css").unwrap();
    (css, result)
}

#[test]
fn resolved_theme_alpha_retains_transparent_and_adds_local_supports() {
    let theme = theme(&[("--color-red", "oklch(63.7% 0.237 25.331)")]);
    let (css, result) = run(
        ".red { color: color-mix(in oklab, var(--color-red) 25%, transparent) !important; }",
        &theme,
    );
    assert_eq!(result.fallback_declarations, 1);
    assert!(css.contains(
        "color: color-mix(in srgb, oklch(63.7% 0.237 25.331) 25%, transparent) !important;"
    ));
    assert!(css.contains("@supports (color: color-mix(in lab, red, red))"));
    assert!(
        css.contains("color: color-mix(in oklab, var(--color-red) 25%, transparent) !important;")
    );
    let original = css::parse(
        ".red { color: color-mix(in oklab, var(--color-red) 25%, transparent) !important; }",
        "input.css",
    )
    .unwrap();
    let children = result.ast[0].children().unwrap();
    assert_eq!(children[0].src, original[0].children().unwrap()[0].src);
    assert_eq!(children[1].src, children[0].src);
    assert_eq!(
        children[1].children().unwrap()[0],
        original[0].children().unwrap()[0]
    );
}

#[test]
fn unresolved_variables_and_currentcolor_use_first_color_token() {
    let empty = Theme::default();
    for (input, fallback) in [
        (
            "color-mix(in oklab, var(--missing, red) 50%, transparent)",
            "var(--missing, red)",
        ),
        ("color-mix(in oklab, red 50%, var(--missing))", "red"),
        (
            "color-mix(in oklab, CURRENTcolor 50%, transparent)",
            "CURRENTcolor",
        ),
        (
            "color-mix(in lch, transparent 0%, currentcolor)",
            "transparent",
        ),
    ] {
        let (css, result) = run(&format!(".a {{ color: {input}; }}"), &empty);
        assert_eq!(result.fallback_declarations, 1);
        assert!(
            css.starts_with(&format!(".a {{\n  color: {fallback};\n")),
            "{css}"
        );
    }
}

#[test]
fn variable_alias_chains_cycles_and_theme_currentcolor() {
    let theme = theme(&[
        ("--a", "var(--b)"),
        ("--b", "var(--c, blue)"),
        ("--c", "red"),
        ("--cycle-a", "var(--cycle-b)"),
        ("--cycle-b", "var(--cycle-a)"),
        ("--current", "currentColor"),
    ]);
    let (css, result) = run(
        ".a { color: color-mix(in lab, var(--a) 50%, transparent); fill: color-mix(in lab, var(--cycle-a) 50%, transparent); stroke: color-mix(in lab, var(--current) 50%, transparent); }",
        &theme,
    );
    assert_eq!(result.fallback_declarations, 3);
    assert!(css.contains("color: color-mix(in srgb, red 50%, transparent);"));
    assert!(css.contains("fill: var(--cycle-a);"));
    assert!(css.contains("stroke: var(--current);"));
}

#[test]
fn literal_mixes_and_other_color_spaces_are_not_evaluated() {
    let theme = theme(&[("--red", "red")]);
    let literals =
        ".a { color: color-mix(in oklab, red 25%, transparent); fill: rgb(1 2 3 / 0.5); }";
    let ast = css::parse(literals, "input.css").unwrap();
    let result = color_mix_compatibility(&ast, &theme).unwrap();
    assert_eq!(result.fallback_declarations, 0);
    assert_eq!(result.ast, ast);
    for space in [
        "oklab",
        "oklch",
        "lab",
        "lch",
        "srgb",
        "srgb-linear",
        "hsl",
        "display-p3",
    ] {
        let expected = if matches!(space, "oklab" | "oklch" | "lab" | "lch") {
            "srgb"
        } else {
            space
        };
        let (css, _) = run(
            &format!(".a {{color: color-mix(in {space}, var(--red) 10%, transparent)}}"),
            &theme,
        );
        assert!(
            css.contains(&format!("color-mix(in {expected}, red 10%, transparent)")),
            "{css}"
        );
    }
}

#[test]
fn keyframes_and_color_mix_supports_are_unchanged() {
    let input = "@keyframes fade { from { color: color-mix(in oklab, var(--red) 0%, transparent); } } @supports (color: color-mix(in lab, red, red)) { .a { color: color-mix(in oklab, var(--red) 50%, transparent); } }";
    let ast = css::parse(input, "input.css").unwrap();
    let result = color_mix_compatibility(&ast, &theme(&[("--red", "red")])).unwrap();
    assert_eq!(result.ast, ast);
    assert_eq!(result.fallback_declarations, 0);
}

#[test]
fn sibling_global_flag_and_nested_mix_walk_order_match_pinned_algorithm() {
    let theme = theme(&[("--red", "red")]);
    let (css, _) = run(
        ".a { background: color-mix(in lab, var(--red), transparent), color-mix(in oklch, blue, transparent); }",
        &theme,
    );
    assert!(css.contains(
        "background: color-mix(in srgb, red, transparent), color-mix(in srgb, blue, transparent);"
    ));
    let (css, _) = run(
        ".a { color: color-mix(in lab, color-mix(in oklab, var(--red), transparent), currentcolor); }",
        &theme,
    );
    assert!(css.contains("color: color-mix(in srgb, red, transparent);"));
}

#[test]
fn inlined_theme_functions_remain_opaque_and_var_fallbacks_are_walked() {
    let theme = theme(&[("--complex", "rgb(var(--channels))"), ("--red", "red")]);
    let (css, _) = run(
        ".a { color: color-mix(in oklab, var(--complex), transparent); fill: color-mix(in lab, var(--missing, var(--red)), transparent); }",
        &theme,
    );
    assert!(css.contains("color: color-mix(in srgb, rgb(var(--channels)), transparent);"));
    assert!(css.contains("fill: var(--missing, red);"));
}

#[test]
fn contexts_preserve_explicit_opt_out_and_at_root_structure() {
    let declaration = AstNode::declaration("color", "color-mix(in lab, var(--red), transparent)");
    let ast = vec![
        AstNode::new(NodeKind::Context {
            context: BTreeMap::from([("supportsColorMix".into(), ContextValue::Bool(true))]),
            nodes: vec![declaration.clone()],
        }),
        AstNode::new(NodeKind::AtRoot {
            nodes: vec![AstNode::new(NodeKind::Rule {
                selector: ".a".into(),
                nodes: vec![declaration],
            })],
        }),
    ];
    let result = color_mix_compatibility(&ast, &theme(&[("--red", "red")])).unwrap();
    assert_eq!(result.ast[0], ast[0]);
    assert_eq!(result.fallback_declarations, 1);
    assert!(matches!(result.ast[1].kind, NodeKind::AtRoot { .. }));
}

#[test]
fn invalid_value_errors_are_transactional_and_keep_byte_source() {
    let mut ast = vec![
        AstNode::declaration("color", "color-mix(in lab, var(--red), transparent)"),
        AstNode::declaration("fill", "color-mix("),
    ];
    ast[1].src = Some(SourceLocation {
        file: "é.css".into(),
        start: 7,
        end: 19,
    });
    let before = ast.clone();
    let error = color_mix_compatibility(&ast, &theme(&[("--red", "red")])).unwrap_err();
    assert_eq!(error.kind, ColorMixErrorKind::Invalid);
    assert_eq!(error.source, ast[1].src);
    assert_eq!(ast, before);
}
