use super::*;
use crate::{
    ast::to_css,
    compile::{CompileOptions, compile_with_design_system},
};
fn setup(options: TypographyOptions) -> DesignSystem {
    let mut system = DesignSystem::new(Theme::default());
    register_typography(&mut system, TYPOGRAPHY_VERSION, &options, None).unwrap();
    system
}
fn compile(system: &DesignSystem, names: &[&str]) -> String {
    let output = compile_with_design_system(
        &names.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
        system,
        &CompileOptions {
            property_fallbacks: false,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(output.invalid.is_empty(), "{:?}", output.invalid);
    assert!(output.unsupported.is_empty(), "{:?}", output.unsupported);
    to_css(&mut output.ast.clone(), "typography.css").unwrap()
}
#[test]
fn pinned_default_inventory_covers_all_modifiers_and_element_variants() {
    let defaults = default_modifiers().unwrap();
    assert_eq!(defaults.len(), 29);
    assert_eq!(
        defaults
            .iter()
            .map(|m| m.name.as_str())
            .take(6)
            .collect::<Vec<_>>(),
        ["DEFAULT", "sm", "base", "lg", "xl", "2xl"]
    );
    let registration = generate_typography(
        TYPOGRAPHY_VERSION,
        &TypographyOptions::default(),
        None,
        None,
    )
    .unwrap();
    assert_eq!(registration.utilities.len(), 29);
    assert_eq!(registration.variants.len(), 33);
    assert_eq!(registration.variants[0].name, "prose-headings");
    assert_eq!(registration.variants.last().unwrap().name, "prose-lead");
    let system = setup(TypographyOptions::default());
    let classes = defaults
        .iter()
        .map(|m| {
            if m.name == "DEFAULT" {
                "prose".into()
            } else {
                format!("prose-{}", m.name)
            }
        })
        .collect::<Vec<String>>();
    let output = compile(
        &system,
        &classes.iter().map(String::as_str).collect::<Vec<_>>(),
    );
    assert!(output.contains(".prose-2xl"));
    assert!(output.contains(".prose-invert"));
}
#[test]
fn base_prose_has_real_layout_typographic_colors_and_pseudo_exclusions() {
    let output = compile(&setup(Default::default()), &["prose"]);
    for expected in [
        "max-width: 65ch;",
        "font-size: 1rem;",
        "line-height: 1.75;",
        "--tw-prose-body: oklch(37.3% 0.034 259.733);",
        "--tw-prose-kbd-shadows: color-mix(in oklab, oklch(21% 0.034 264.665) 10%, transparent);",
        "--tw-prose-invert-kbd-shadows: rgb(255 255 255 / 10%);",
        ":where(code):not(:where([class~=\"not-prose\"],[class~=\"not-prose\"] *))::before",
        ":where(ol > li):not(:where([class~=\"not-prose\"],[class~=\"not-prose\"] *))::marker",
        ":where(.prose > :first-child)",
        "font-size: 2.25em;",
    ] {
        assert!(output.contains(expected), "Missing {expected}");
    }
    assert!(!output.contains(":where(code::before)"));
}
#[test]
fn all_size_modifiers_and_color_invert_contracts() {
    let system = setup(Default::default());
    for (class, size) in [
        ("prose-sm", ".875rem"),
        ("prose-base", "1rem"),
        ("prose-lg", "1.125rem"),
        ("prose-xl", "1.25rem"),
        ("prose-2xl", "1.5rem"),
    ] {
        let text = compile(&system, &[class]);
        let size = if size.starts_with('.') {
            format!("0{size}")
        } else {
            size.into()
        };
        assert!(text.contains(&format!("font-size: {size};")), "{class}");
    }
    let text = compile(&system, &["prose-blue", "prose-invert"]);
    assert!(text.contains("--tw-prose-links: oklch(54.6% 0.245 262.881);"));
    assert!(text.contains("--tw-prose-invert-links: oklch(62.3% 0.214 259.815);"));
    assert!(text.contains("--tw-prose-body: var(--tw-prose-invert-body);"));
    assert!(text.contains("--tw-prose-td-borders: var(--tw-prose-invert-td-borders);"));
}
#[test]
fn custom_class_legacy_target_and_element_variants() {
    let modern = setup(TypographyOptions {
        class_name: "markdown".into(),
        target: TypographyTarget::Modern,
    });
    let output = compile(
        &modern,
        &[
            "markdown",
            "markdown-headings:underline",
            "markdown-lead:underline",
        ],
    );
    assert!(output.contains("not-markdown"));
    assert!(!output.contains("not-prose"));
    assert!(output.contains("& :is(:where(h1, h2, h3, h4, h5, h6, th)"));
    assert!(output.contains("& :is(:where([class~=\"lead\"])"));
    let legacy = setup(TypographyOptions {
        class_name: "prose".into(),
        target: TypographyTarget::Legacy,
    });
    let text = compile(&legacy, &["prose", "prose-headings:underline"]);
    assert!(!text.contains(":where("));
    assert!(!text.contains("not-prose"));
    assert!(text.contains("& h1, & h2, & h3, & h4, & h5, & h6, & th"));
    assert!(text.contains("code::before"));
}
#[test]
fn nested_root_references_follow_variant_and_important_candidate_spelling() {
    let system = setup(Default::default());
    let text = compile(&system, &["hover:prose!", "prose-lg!"]);
    assert!(text.contains(":where(.hover\\:prose\\! > :first-child)"));
    assert!(text.contains(":where(.prose-lg\\! > :first-child)"));
    assert!(text.contains("max-width: 65ch !important;"));
    assert!(text.contains("&:hover"));
}
#[test]
fn prefix_matches_tailwind_four_plugin_api_identity_and_nested_class_rules() {
    let mut theme = Theme::default();
    theme.prefix = Some("tw".into());
    let mut system = DesignSystem::new(theme);
    register_typography(
        &mut system,
        TYPOGRAPHY_VERSION,
        &TypographyOptions::default(),
        None,
    )
    .unwrap();
    let text = compile(&system, &["tw:prose", "tw:prose-a:underline"]);
    assert!(text.contains(".tw\\:prose"));
    assert!(text.contains(":where(.tw\\:prose > :first-child)"));
    assert!(text.contains("[class~=\"not-prose\"]"));
    assert!(!text.contains("not-tw"));
}
#[test]
fn resolved_theme_modifiers_replace_defaults_and_preserve_nested_selectors() {
    let css = crate::css::parse("color: purple;", "custom.css").unwrap();
    // CSS parser's top-level trailing-declaration behavior is irrelevant here:
    // a resolved configuration supplies declaration IR directly.
    assert_eq!(css.len(), 1);
    let custom = vec![TypographyModifier {
        name: "DEFAULT".into(),
        css: vec![
            AstNode::declaration("color", "purple"),
            AstNode::new(NodeKind::Rule {
                selector: "a".into(),
                nodes: vec![AstNode::new(NodeKind::Rule {
                    selector: "&:hover".into(),
                    nodes: vec![AstNode::declaration("color", "red")],
                })],
            }),
        ],
    }];
    let mut system = DesignSystem::new(Theme::default());
    register_typography(
        &mut system,
        TYPOGRAPHY_VERSION,
        &TypographyOptions::default(),
        Some(&custom),
    )
    .unwrap();
    let text = compile(&system, &["prose"]);
    assert!(text.contains("color: purple;"));
    assert!(text.contains(":where(a)"));
    assert!(text.contains("&:hover"));
    assert!(!text.contains("max-width"));
    assert!(!system.registry().static_utilities.contains("prose-lg"));
}
#[test]
fn selector_pseudos_escapes_attributes_and_comments_are_structural() {
    assert_eq!(
        trailing_pseudos("a::before, b::before").unwrap(),
        ("::before".into(), "a, b".into())
    );
    assert_eq!(
        trailing_pseudos("a::part(foo)::before,b::part(bar)::before").unwrap(),
        ("::part(foo)::before".into(), "a,b".into())
    );
    // Pinned upstream helper uses sparse pseudo columns; preserve its behavior.
    assert_eq!(
        trailing_pseudos("a::before,b").unwrap(),
        ("::before".into(), "a,b".into())
    );
    assert_eq!(trailing_pseudos("a,b::before").unwrap().0, "");
    assert_eq!(trailing_pseudos("[data-x='::before']").unwrap().0, "");
    assert_eq!(
        trailing_pseudos(".é::before").unwrap(),
        ("::before".into(), ".é".into())
    );
    assert!(trailing_pseudos(":where(a").is_err());
    let selector = rewrite_classes(
        ".prose :where(.prose) [data-x='.prose'] /* .prose */ .prose-other",
        &|name| (name == "prose").then(|| "hover:prose".into()),
    )
    .unwrap();
    assert_eq!(
        selector,
        ".hover\\:prose :where(.hover\\:prose) [data-x='.prose'] /* .prose */ .prose-other"
    );
}
#[test]
fn unsupported_version_invalid_options_and_install_errors_are_atomic() {
    let mut system = DesignSystem::new(Theme::default());
    let before = system.registry();
    assert!(
        register_typography(&mut system, "0.5.18", &TypographyOptions::default(), None).is_err()
    );
    assert_eq!(before.static_utilities, system.registry().static_utilities);
    assert!(
        generate_typography(
            TYPOGRAPHY_VERSION,
            &TypographyOptions {
                class_name: "bad class".into(),
                ..Default::default()
            },
            None,
            None
        )
        .is_err()
    );
    let bad = vec![TypographyModifier {
        name: "DEFAULT".into(),
        css: vec![],
    }];
    assert!(
        register_typography(
            &mut system,
            TYPOGRAPHY_VERSION,
            &TypographyOptions::default(),
            Some(&bad)
        )
        .is_err()
    );
    assert!(system.variants().metadata("prose-a").is_none());
}
