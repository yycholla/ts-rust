use super::*;
use crate::candidate::{CandidateRegistry, parse_candidate};

fn utility(css: &str) -> CustomUtility {
    let mut ast = crate::css::parse(css, "fixture.css").unwrap();
    CustomUtility::from_ast(ast.remove(0)).unwrap()
}
fn candidate(raw: &str, utility: &CustomUtility) -> Candidate {
    let mut registry = CandidateRegistry::default();
    if utility.functional() {
        registry.functional_utilities.insert(utility.root().into());
    } else {
        registry.static_utilities.insert(utility.root().into());
    }
    parse_candidate(raw, &registry).remove(0)
}
fn output(raw: &str, utility: &CustomUtility, theme: &Theme) -> String {
    let CustomUtilityResult::Generated(mut nodes) =
        utility.compile(&candidate(raw, utility), theme).unwrap()
    else {
        panic!("{raw} did not generate");
    };
    crate::ast::to_css(&mut nodes, "out.css").unwrap()
}
#[test]
fn names_static_cloning_and_negative_functional_registration() {
    for name in ["foo", "-foo", "p-1.5", "w-50%", "text-xs/8"] {
        assert!(valid_static_name(name), "{name}");
    }
    for name in ["Foo", "foo-", "foo.%", "foo/", "foo/a/b", "foo[bar]"] {
        assert!(!valid_static_name(name), "{name}");
    }
    assert!(valid_functional_name("foo--*"));
    assert!(valid_functional_name("-spin-in-*"));
    assert!(!valid_functional_name("foo.*"));
    let utility = utility("@utility w-1\\/2 { width: 50%; &:hover { color: red; } }");
    let first = output("w-1/2", &utility, &Theme::default());
    assert!(first.contains("width: 50%;"));
    assert_eq!(first, output("w-1/2", &utility, &Theme::default()));
}
#[test]
fn formatter_normalization_nested_theme_and_first_matching_alternative() {
    let mut theme = Theme::default();
    theme
        .add("--text-sm", "14px", ThemeOptions::NONE, None)
        .unwrap();
    theme
        .add("--text-sm--line-height", "20px", ThemeOptions::INLINE, None)
        .unwrap();
    let utility = utility(
        "@utility text-leading-* { line-height: --value(--text- * --line-height); other: --value([ *]); literal: --value('sm', integer); }",
    );
    let text = output("text-leading-sm", &utility, &theme);
    assert!(text.contains("line-height: 20px;"));
    assert!(text.contains("literal: sm;"));
    assert!(!text.contains("other:"));
    let text = output("text-leading-[var(--leading)]", &utility, &theme);
    assert!(text.contains("other: var(--leading);"));
    assert!(!text.contains("line-height:"));
}
#[test]
fn nested_ratio_removes_non_ratio_declarations_across_entire_utility() {
    let utility = utility(
        "@utility slide-* { &:dir(ltr) { --offset: calc(--value(integer) * -1px); --offset: calc(--value(ratio) * -100%); } &:dir(rtl) { --offset: calc(--value(integer) * 1px); --offset: calc(--value(ratio) * 100%); } display: block; }",
    );
    let text = output("slide-1/2", &utility, &Theme::default());
    assert_eq!(text.matches("--offset:").count(), 2);
    assert!(text.contains("calc(1 / 2 * -100%)"));
    assert!(text.contains("calc(1 / 2 * 100%)"));
    assert!(!text.contains("1px"));
    assert!(text.contains("display: block"));
}
#[test]
fn modifier_consumption_and_ratio_conflict() {
    let utility =
        utility("@utility tab-* { width: --value(integer); opacity: --modifier(integer); }");
    let text = output("tab-2/50", &utility, &Theme::default());
    assert!(text.contains("width: 2;"));
    assert!(text.contains("opacity: 50;"));
    assert!(!output("tab-2", &utility, &Theme::default()).contains("opacity:"));
    assert_eq!(
        utility
            .compile(&candidate("tab-2/bad", &utility), &Theme::default())
            .unwrap(),
        CustomUtilityResult::Stop
    );
    let ratio = self::utility(
        "@utility ratio-* { aspect-ratio: --value(ratio); width: --modifier(integer); }",
    );
    assert_eq!(
        ratio
            .compile(&candidate("ratio-1/2", &ratio), &Theme::default())
            .unwrap(),
        CustomUtilityResult::Stop
    );
    let value_only = self::utility("@utility val-* { width: --value(number); }");
    assert_eq!(
        value_only
            .compile(&candidate("val-1/2", &value_only), &Theme::default())
            .unwrap(),
        CustomUtilityResult::Stop
    );
}
#[test]
fn arbitrary_type_hints_and_bare_value_validation() {
    let utility = utility(
        "@utility size-* { width: --value(integer, [length]); height: --value(number); opacity: --value(percentage); }",
    );
    assert!(
        output("size-[length:var(--width)]", &utility, &Theme::default())
            .contains("width: var(--width)")
    );
    assert!(output("size-1.5", &utility, &Theme::default()).contains("height: 1.5"));
    assert!(output("size-25%", &utility, &Theme::default()).contains("opacity: 25%"));
    for raw in [
        "size-1.1",
        "size-25.5%",
        "size-[color:red]",
        "size-[var(--width)]",
    ] {
        assert_eq!(
            utility
                .compile(&candidate(raw, &utility), &Theme::default())
                .unwrap(),
            CustomUtilityResult::Stop,
            "{raw}"
        );
    }
}
#[test]
fn unresolved_alternative_warns_without_deleting_valid_alternative() {
    let utility = utility("@utility color-* { color: --value(color, [color]); }");
    assert_eq!(utility.warnings().len(), 1);
    assert!(utility.warnings()[0].source.is_some());
    assert!(output("color-[red]", &utility, &Theme::default()).contains("color: red;"));
    assert_eq!(
        utility
            .compile(&candidate("color-red", &utility), &Theme::default())
            .unwrap(),
        CustomUtilityResult::Stop
    );
}
