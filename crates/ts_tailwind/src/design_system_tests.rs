use super::*;
use crate::{
    compile::{CompileOptions, compile_with_design_system},
    css,
};

fn definition(source: &str) -> AstNode {
    css::parse(source, "custom.css").unwrap().remove(0)
}
fn compile(system: &DesignSystem, raw: &[&str]) -> crate::compile::CandidateCompilation {
    compile_with_design_system(
        &raw.iter().map(|v| v.to_string()).collect::<Vec<_>>(),
        system,
        &CompileOptions::default(),
    )
    .unwrap()
}
fn text(system: &DesignSystem, raw: &[&str]) -> String {
    crate::ast::to_css(&mut compile(system, raw).ast, "out.css").unwrap()
}

#[test]
fn custom_delay_coexists_with_builtin_and_other_registrations() {
    let mut system = DesignSystem::new(Theme::default());
    system
        .register_utility(definition(
            "@utility delay-* { --tw-animation-delay: calc(--value(number) * 1ms); }",
        ))
        .unwrap();
    system
        .register_utility(definition(
            "@utility delay-* { custom-delay: --value(integer); }",
        ))
        .unwrap();
    let compiled = compile(&system, &["delay-150"]);
    assert_eq!(compiled.rules.len(), 3);
    assert_eq!(compiled.generated, ["delay-150"]);
    let css = text(&system, &["delay-150"]);
    assert!(css.contains("transition-delay: 150ms;"));
    assert!(css.contains("--tw-animation-delay: calc(150 * 1ms);"));
    assert!(css.contains("custom-delay: 150;"));
}
#[test]
fn stop_result_preserves_earlier_generators_and_stops_later_ones() {
    let mut system = DesignSystem::new(Theme::default());
    system
        .register_utility(definition("@utility delay-* { test: --value('never'); }"))
        .unwrap();
    system
        .register_utility(definition(
            "@utility delay-* { should-not-appear: --value(integer); }",
        ))
        .unwrap();
    let css = text(&system, &["delay-100"]);
    assert!(css.contains("transition-delay: 100ms;"));
    assert!(!css.contains("should-not-appear"));
}
#[test]
fn authoritative_bodies_change_without_stale_compilation_or_identity() {
    let mut system = DesignSystem::new(Theme::default());
    let id = system
        .register_utility(definition("@utility card { @apply flex; } "))
        .unwrap();
    assert_eq!(compile(&system, &["card"]).unsupported.len(), 1);
    system
        .replace_utility_body(id, vec![AstNode::declaration("display", "flex")])
        .unwrap();
    assert!(text(&system, &["card"]).contains("display: flex;"));
    assert_eq!(system.definitions_for_root("card").next().unwrap().0, id);
    let unknown = system.replace_utility_body(UtilityId(999), vec![]);
    assert!(unknown.is_err());
    assert!(text(&system, &["card"]).contains("display: flex;"));
}
#[test]
fn custom_static_functional_prefix_variants_and_exclusions() {
    let mut theme = Theme::default();
    theme.prefix = Some("tw".into());
    let mut system = DesignSystem::new(theme);
    system
        .register_utility(definition("@utility tab-2 { static-case: yes; }"))
        .unwrap();
    system
        .register_utility(definition("@utility tab-* { tab-size: --value(integer); }"))
        .unwrap();
    let output = compile(&system, &["tw:hover:tab-2!", "tab-2"]);
    assert_eq!(output.rules.len(), 2);
    assert_eq!(output.invalid, ["tab-2"]);
    let text = text(&system, &["tw:hover:tab-2!"]);
    assert!(text.contains("static-case: yes !important;"));
    assert!(text.contains("tab-size: 2 !important;"));
    system.exclude("tw:hover:tab-2!".into());
    assert_eq!(compile(&system, &["tw:hover:tab-2!"]).invalid.len(), 1);
}
#[test]
fn candidate_transform_is_atomic_and_distinguishes_failure_kinds() {
    let mut system = DesignSystem::new(Theme::default());
    system
        .register_utility(definition("@utility token { width: --spacing(2); }"))
        .unwrap();
    assert_eq!(compile(&system, &["token"]).unsupported.len(), 1);
    system.set_candidate_transform(|_, _| {
        Err(CandidateTransformError {
            kind: CandidateTransformErrorKind::Invalid,
            message: "missing --spacing".into(),
        })
    });
    assert_eq!(compile(&system, &["token"]).invalid, ["token"]);
    system.set_candidate_transform(|nodes, _| {
        Ok(nodes
            .iter()
            .cloned()
            .map(|mut n| {
                if let crate::ast::NodeKind::Declaration { value, .. } = &mut n.kind {
                    *value = Some(".5rem".into());
                }
                n
            })
            .collect())
    });
    assert!(text(&system, &["token"]).contains("width: .5rem;"));
    system.set_candidate_transform(|_, _| {
        Err(CandidateTransformError {
            kind: CandidateTransformErrorKind::Limit,
            message: "substitution budget exceeded".into(),
        })
    });
    assert_eq!(
        compile_with_design_system(&["token".into()], &system, &CompileOptions::default())
            .unwrap_err(),
        "substitution budget exceeded"
    );
}
