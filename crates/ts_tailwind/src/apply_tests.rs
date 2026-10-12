use super::*;
use crate::{ast::to_css, compile::lower_ast, css, theme::Theme};

fn fixture(source: &str) -> (Vec<AstNode>, DesignSystem, ApplyOptions) {
    let ast = css::parse(source, "fixture.css").unwrap();
    let mut system = DesignSystem::new(Theme::default());
    fn register(nodes: &[AstNode], system: &mut DesignSystem) {
        for node in nodes {
            if matches!(&node.kind, NodeKind::AtRule { name, .. } if name == "@utility") {
                system.register_utility(node.clone()).unwrap();
            }
            if let Some(children) = node.children() {
                register(children, system);
            }
        }
    }
    register(&ast, &mut system);
    (
        ast,
        system,
        ApplyOptions {
            sources: BTreeMap::from([("fixture.css".into(), source.into())]),
        },
    )
}
fn css_text(result: &ApplyResult, system: &DesignSystem) -> String {
    let mut lowered = lower_ast(result.ast.clone(), system.theme(), false).unwrap();
    to_css(&mut lowered.ast, "output.css").unwrap()
}
fn walk<'a>(nodes: &'a [AstNode], out: &mut Vec<&'a AstNode>) {
    for node in nodes {
        out.push(node);
        if let Some(children) = node.children() {
            walk(children, out);
        }
    }
}

#[test]
fn builtins_sort_deduplicate_and_keep_explicit_important_and_variant_nesting() {
    let (ast, mut system, options) = fixture(".card { @apply block hover:flex! flex block; }");
    let result = substitute_at_apply(&ast, &mut system, &options).unwrap();
    let text = css_text(&result, &system);
    assert!(result.has_apply);
    assert_eq!(result.expansions.len(), 3);
    assert!(!text.contains(".block"));
    assert!(!text.contains(".hover"));
    assert!(text.contains("&:hover"));
    assert!(text.contains("@media (hover: hover)"));
    assert!(text.contains("display: flex !important;"));
    let nodes = result.ast[0].children().unwrap();
    assert_eq!(
        nodes
            .iter()
            .filter(|n| matches!(
                &n.kind,
                NodeKind::Declaration {
                    important: false,
                    ..
                }
            ))
            .count(),
        2
    );
    let block = result
        .expansions
        .iter()
        .find(|e| e.candidate == "block")
        .unwrap();
    assert_eq!(
        block.source.as_ref().unwrap().start,
        options.sources["fixture.css"].rfind("block").unwrap()
    );
}

#[test]
fn custom_dependencies_expand_before_consumers_and_keep_stable_ids() {
    let source = "@utility card { @apply pane; color: red; } @utility pane { @apply flex; } .host { @apply card; }";
    let (ast, mut system, options) = fixture(source);
    let original = ast.clone();
    let result = substitute_at_apply(&ast, &mut system, &options).unwrap();
    assert_eq!(result.updated_definitions, [UtilityId(1), UtilityId(0)]);
    assert_eq!(ast, original);
    let host = result.ast.last().unwrap();
    assert_eq!(host.children().unwrap().len(), 2);
    assert!(css_text(&result, &system).contains("display: flex;"));
    for (_, definition) in system.definitions() {
        let mut nodes = Vec::new();
        walk(definition.body(), &mut nodes);
        assert!(
            !nodes
                .iter()
                .any(|n| matches!(&n.kind, NodeKind::AtRule { name, .. } if name == "@apply"))
        );
    }
    let repeated = substitute_at_apply(&result.ast, &mut system, &options).unwrap();
    assert!(!repeated.has_apply);
    assert_eq!(repeated.ast, result.ast);
}

#[test]
fn functional_definitions_nested_apply_and_overlapping_registrations() {
    let source = "@utility pad-* { &:hover { @apply flex; width: calc(--value(integer) * 1px); } } @utility delay-* { @apply block; animation-delay: calc(--value(integer) * 1ms); } .x { @apply pad-2 delay-150; }";
    let (ast, mut system, options) = fixture(source);
    let result = substitute_at_apply(&ast, &mut system, &options).unwrap();
    let host = result.ast.last().unwrap();
    let text = to_css(&mut vec![host.clone()], "output.css").unwrap();
    assert!(text.contains("transition-delay: 150ms;"));
    assert!(text.contains("animation-delay: calc(150 * 1ms);"));
    assert!(text.contains("width: calc(2 * 1px);"));
    assert!(text.contains("&:hover"));
}

#[test]
fn cycles_self_application_and_keyframes_are_errors_before_mutation() {
    for source in [
        "@utility a { @apply a; }",
        "@utility a { @apply b; } @utility b { @apply a; }",
        "@utility a-* { @apply a-2; width: --value(integer); }",
    ] {
        let (ast, mut system, options) = fixture(source);
        let before: Vec<_> = system
            .definitions()
            .map(|(id, d)| (id, d.body().to_vec()))
            .collect();
        let failure = substitute_at_apply(&ast, &mut system, &options).unwrap_err();
        assert_eq!(failure.kind, ApplyErrorKind::CircularDependency);
        assert!(failure.exact_source);
        assert_eq!(
            before,
            system
                .definitions()
                .map(|(id, d)| (id, d.body().to_vec()))
                .collect::<Vec<_>>()
        );
    }
    let (ast, mut system, options) = fixture("@keyframes wobble { from { @apply block; } }");
    assert_eq!(
        substitute_at_apply(&ast, &mut system, &options)
            .unwrap_err()
            .kind,
        ApplyErrorKind::Keyframes
    );
}

#[test]
fn late_failure_rolls_back_expanded_definitions_and_reports_unknown_variant() {
    let (ast, mut system, options) =
        fixture("@utility good { @apply flex; } .host { @apply missing:flex; }");
    let before = system.utility(UtilityId(0)).unwrap().body().to_vec();
    let failure = substitute_at_apply(&ast, &mut system, &options).unwrap_err();
    assert_eq!(failure.kind, ApplyErrorKind::UnknownCandidate);
    assert!(failure.message.contains("`missing` variant does not exist"));
    assert_eq!(system.utility(UtilityId(0)).unwrap().body(), before);
    assert_eq!(failure.candidate.as_deref(), Some("missing:flex"));
}

#[test]
fn utf8_crlf_comments_and_repeated_tokens_have_original_byte_spans() {
    let source = ".é😀 { @apply\tfl/**/ex\r\n [--名:é] block  flex; }";
    let (ast, mut system, options) = fixture(source);
    let result = substitute_at_apply(&ast, &mut system, &options).unwrap();
    assert_eq!(result.expansions.len(), 3);
    for expansion in &result.expansions {
        assert!(expansion.exact_source);
        let location = expansion.source.as_ref().unwrap();
        assert_eq!(&source[location.start..location.end], expansion.candidate);
    }
    let named = result
        .expansions
        .iter()
        .find(|e| e.candidate == "[--名:é]")
        .unwrap();
    let mut nodes = Vec::new();
    walk(&result.ast, &mut nodes);
    let declaration = nodes
        .iter()
        .find(|n| matches!(&n.kind, NodeKind::Declaration { property, .. } if property == "--名"))
        .unwrap();
    assert_eq!(declaration.src, named.source);
    let (ast, mut system, options) = fixture(".x { @apply fl/**/ex; }");
    let result = substitute_at_apply(&ast, &mut system, &options).unwrap();
    let range = result.expansions[0].source.as_ref().unwrap();
    assert_eq!(
        &options.sources["fixture.css"][range.start..range.end],
        "fl/**/ex"
    );
}

#[test]
fn missing_snapshot_retains_directive_range_and_stale_snapshot_is_rejected() {
    let (ast, mut system, mut options) = fixture(".x { @apply flex; }");
    let fallback = substitute_at_apply(&ast, &mut system, &ApplyOptions::default()).unwrap();
    assert!(!fallback.expansions[0].exact_source);
    assert_eq!(
        fallback.expansions[0].source,
        ast[0].children().unwrap()[0].src
    );
    options
        .sources
        .insert("fixture.css".into(), ".x { @apply grid; }".into());
    assert_eq!(
        substitute_at_apply(&ast, &mut system, &options)
            .unwrap_err()
            .kind,
        ApplyErrorKind::SourceMismatch
    );
}

#[test]
fn unsupported_functions_exclusions_prefix_and_empty_apply_are_explicit() {
    let (ast, mut system, options) =
        fixture("@utility unsupported { width: --spacing(2); } .x { @apply unsupported; }");
    assert_eq!(
        substitute_at_apply(&ast, &mut system, &options)
            .unwrap_err()
            .kind,
        ApplyErrorKind::UnsupportedCandidate
    );
    let (ast, mut system, options) = fixture(".x { @apply flex; }");
    system.exclude("flex".into());
    assert!(
        substitute_at_apply(&ast, &mut system, &options)
            .unwrap_err()
            .message
            .contains("explicitly disabled")
    );
    let mut theme = Theme::default();
    theme.prefix = Some("tw".into());
    let mut system = DesignSystem::new(theme);
    assert!(
        substitute_at_apply(&ast, &mut system, &options)
            .unwrap_err()
            .message
            .contains("tw:flex")
    );
    let (ast, mut system, options) = fixture(".x { @apply; }");
    assert_eq!(
        substitute_at_apply(&ast, &mut system, &options)
            .unwrap_err()
            .kind,
        ApplyErrorKind::UnknownCandidate
    );
}

#[test]
fn at_root_support_ir_survives_without_nested_class_wrappers() {
    let (ast, mut system, options) = fixture(".x { @apply before:block; }");
    let result = substitute_at_apply(&ast, &mut system, &options).unwrap();
    let mut nodes = Vec::new();
    walk(&result.ast, &mut nodes);
    assert!(
        nodes
            .iter()
            .any(|n| matches!(&n.kind, NodeKind::AtRoot { .. }))
    );
    assert!(
        nodes
            .iter()
            .any(|n| matches!(&n.kind, NodeKind::AtRule { name, .. } if name == "@property"))
    );
    assert!(!nodes.iter().any(
        |n| matches!(&n.kind, NodeKind::Rule { selector, .. } if selector.contains("before\\:"))
    ));
    assert!(css_text(&result, &system).contains("@property --tw-content"));
}

#[test]
fn native_function_hook_is_used_and_unregistered_definitions_do_not_disappear() {
    let (ast, _, options) = fixture("@utility custom { @apply flex; }");
    let mut system = DesignSystem::new(Theme::default());
    assert_eq!(
        substitute_at_apply(&ast, &mut system, &options)
            .unwrap_err()
            .kind,
        ApplyErrorKind::UnregisteredDefinition
    );
    let (ast, mut system, options) =
        fixture("@utility custom { width: --spacing(2); } .x { @apply custom; }");
    system.set_candidate_transform(|nodes, theme| {
        crate::functions::substitute_functions(nodes, theme)
            .map(|output| output.ast)
            .map_err(|failure| crate::design_system::CandidateTransformError {
                kind: match failure.kind {
                    crate::functions::FunctionErrorKind::Invalid => {
                        crate::design_system::CandidateTransformErrorKind::Invalid
                    }
                    crate::functions::FunctionErrorKind::Unsupported => {
                        crate::design_system::CandidateTransformErrorKind::Unsupported
                    }
                    crate::functions::FunctionErrorKind::Limit => {
                        crate::design_system::CandidateTransformErrorKind::Limit
                    }
                },
                message: failure.message,
            })
    });
    // No spacing theme is configured, so the real substitution hook rejects
    // this candidate rather than leaking --spacing() into emitted CSS.
    assert_eq!(
        substitute_at_apply(&ast, &mut system, &options)
            .unwrap_err()
            .kind,
        ApplyErrorKind::UnknownCandidate
    );
    let mut theme = Theme::default();
    theme
        .add(
            "--spacing",
            "0.25rem",
            crate::theme::ThemeOptions::INLINE,
            None,
        )
        .unwrap();
    let mut system = DesignSystem::new(theme);
    system.register_utility(ast[0].clone()).unwrap();
    system.set_candidate_transform(crate::functions::candidate_transform);
    let result = substitute_at_apply(&ast, &mut system, &options).unwrap();
    let host = result.ast.last().unwrap();
    let text = to_css(&mut vec![host.clone()], "output.css").unwrap();
    assert!(text.contains("width: calc(0.25rem * 2);"));
}
