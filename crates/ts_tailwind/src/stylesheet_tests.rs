use super::*;
use crate::snapshot::{ResolvedImport, StylesheetSnapshot};
fn inputs(css: &str) -> (ProjectSnapshot, StylesheetOptions) {
    (
        ProjectSnapshot {
            tailwind_version: crate::TAILWIND_VERSION.into(),
            entry: "app.css".into(),
            stylesheets: vec![StylesheetSnapshot {
                id: "app.css".into(),
                version: -3,
                css: css.into(),
            }],
            sources: vec![],
            imports: vec![],
            plugins: vec![],
        },
        StylesheetOptions {
            imports: ImportOptions {
                bases: BTreeMap::from([("app.css".into(), "/app".into())]),
                limits: Default::default(),
            },
            property_fallbacks: false,
            license_comment: false,
        },
    )
}
fn prepare(css: &str) -> PreparedStylesheet {
    let (snapshot, options) = inputs(css);
    prepare_stylesheet(&snapshot, &options).unwrap()
}

#[test]
fn native_typography_directives_compile_with_css_options_and_apply() {
    let (mut snapshot, options) = inputs(
        "@plugin '@tailwindcss/typography' { className: article; } @tailwind utilities; .body { @apply article; }",
    );
    snapshot.plugins.push(crate::snapshot::PluginRegistration {
        specifier: "@tailwindcss/typography".into(),
        implementation: "@tailwindcss/typography".into(),
        version: "0.5.19".into(),
        options: serde_json::json!({"className":"prose"}),
    });
    let prepared = prepare_stylesheet(&snapshot, &options).unwrap();
    let result = build(&prepared, &["article", "article-headings:underline"]);
    assert!(result.unsupported.is_empty(), "{:?}", result.unsupported);
    assert!(result.invalid.is_empty(), "{:?}", result.invalid);
    assert!(result.css.contains("--tw-prose-body"));
    assert!(result.css.contains(".article"));
    assert!(result.css.contains(".body"));
    assert!(!result.css.contains("@plugin"));
    assert!(!result.css.contains("@apply"));
}

#[test]
fn native_plugin_identity_and_version_fail_explicitly() {
    let (mut snapshot, options) = inputs("@plugin '@tailwindcss/typography'; @tailwind utilities;");
    assert!(prepare_stylesheet(&snapshot, &options).is_err());
    snapshot.plugins.push(crate::snapshot::PluginRegistration {
        specifier: "@tailwindcss/typography".into(),
        implementation: "@tailwindcss/typography".into(),
        version: "0.0.0".into(),
        options: serde_json::Value::Null,
    });
    let error = prepare_stylesheet(&snapshot, &options).err().unwrap();
    assert!(error.message.contains("Unsupported Typography version"));
    assert_eq!(error.source.unwrap().file, "app.css");
}
fn build(prepared: &PreparedStylesheet, candidates: &[&str]) -> StylesheetBuild {
    prepared
        .build(&candidates.iter().map(|s| s.to_string()).collect::<Vec<_>>())
        .unwrap()
}
fn import(
    snapshot: &mut ProjectSnapshot,
    options: &mut StylesheetOptions,
    specifier: &str,
    id: &str,
    base: &str,
    css: &str,
) {
    snapshot.stylesheets.push(StylesheetSnapshot {
        id: id.into(),
        version: 1,
        css: css.into(),
    });
    snapshot.imports.push(ResolvedImport {
        from: "app.css".into(),
        specifier: specifier.into(),
        target: id.into(),
    });
    options.imports.bases.insert(id.into(), base.into());
}

#[test]
fn first_utilities_slot_theme_usage_and_replacement_builds() {
    let prepared = prepare(
        "@theme { --color-brand: red; --color-unused: blue; } @layer utilities { @tailwind utilities; } @tailwind utilities; .author { color: green; }",
    );
    let first = build(&prepared, &["bg-brand", "flex", "not-real"]);
    assert!(first.css.contains("--color-brand: red;"));
    assert!(!first.css.contains("--color-unused"));
    assert_eq!(first.css.matches(".flex {").count(), 1);
    assert!(first.css.contains("background-color: var(--color-brand)"));
    assert_eq!(first.invalid, ["not-real"]);
    let second = build(&prepared, &["block"]);
    assert!(!second.css.contains("--color-brand"));
    assert!(!second.css.contains(".flex"));
    assert!(second.css.contains(".block"));
    assert!(second.css.contains(".author"));
}

#[test]
fn theme_defaults_inline_static_prefix_reset_and_external_variable_usage() {
    let prepared = prepare(
        "@theme { --color-brand: red; } @theme default { --color-brand: blue; --color-old: gold; } @theme { --color-old: initial; } @theme inline prefix(tw) { --color-inline: pink; } @theme static { --always: yes; } @tailwind utilities;",
    );
    let result = build(&prepared, &["tw:bg-brand", "tw:bg-inline", "bg-brand"]);
    assert!(result.css.contains("--tw-color-brand: red;"));
    assert!(!result.css.contains("blue"));
    assert!(!result.css.contains("gold"));
    assert!(result.css.contains("background-color: pink;"));
    assert!(result.css.contains("--tw-always: yes;"));
    assert_eq!(result.invalid, ["bg-brand"]);
    let external = build(&prepared, &["--tw-color-brand"]);
    assert!(external.css.contains("--tw-color-brand: red;"));
    assert!(external.generated.is_empty());
}

#[test]
fn custom_utilities_variants_functions_and_apply_follow_shared_pipeline() {
    let prepared = prepare(
        "@theme { --spacing: .25rem; } @custom-variant hocus (&:hover, &:focus); @utility card { @variant hocus { @apply flex; width: --spacing(2); } } .example { @apply card; } @tailwind utilities;",
    );
    let result = build(&prepared, &["card", "hocus:block"]);
    assert!(result.css.contains(".example"));
    assert!(result.css.contains("&:hover, &:focus"));
    assert!(result.css.contains("display: flex;"));
    assert!(result.css.contains("width: calc(var(--spacing) * 2);"));
    for directive in [
        "@apply",
        "@utility",
        "@variant",
        "@custom-variant",
        "@tailwind",
        "@theme",
    ] {
        assert!(!result.css.contains(directive), "{directive}");
    }
    assert_eq!(result.generated, ["card", "hocus:block"]);
}

#[test]
fn animation_theme_keyframes_are_hoisted_only_when_referenced() {
    let prepared = prepare(
        "@theme { --animate-enter: enter 1s; --animate-unused: unused 1s; @keyframes enter { from { opacity: 0; } to { opacity: 1; } } @keyframes unused { to { opacity: .5; } } } @tailwind utilities;",
    );
    let used = build(&prepared, &["animate-enter"]);
    assert!(used.css.contains("--animate-enter: enter 1s;"));
    assert_eq!(used.css.matches("@keyframes enter").count(), 1);
    assert!(!used.css.contains("@keyframes unused"));
    let empty = build(&prepared, &[]);
    assert!(!empty.css.contains("@keyframes"));
}

#[test]
fn imported_reference_theme_is_available_but_css_and_slot_are_suppressed() {
    let (mut snapshot, mut options) =
        inputs("@reference 'ref'; .author { @apply bg-brand; } @tailwind utilities;");
    import(
        &mut snapshot,
        &mut options,
        "ref",
        "ref.css",
        "/theme",
        "@theme { --color-brand: red; } .hidden { color: blue; } @tailwind utilities;",
    );
    let prepared = prepare_stylesheet(&snapshot, &options).unwrap();
    let output = build(&prepared, &["flex"]);
    assert!(
        output
            .css
            .contains("background-color: var(--color-brand, red);")
    );
    assert!(!output.css.contains("--color-brand: red;"));
    assert!(!output.css.contains(".hidden"));
    assert!(output.css.contains(".flex"));
    assert_eq!(output.metadata.imported_stylesheets[0].base, "/theme");
}

#[test]
fn import_modifiers_preserve_importer_source_base_and_important_rules() {
    let (mut snapshot, mut options) = inputs(
        "@import 'tailwind' source('../src') prefix(tw) important; .author { @apply tw:block; }",
    );
    import(
        &mut snapshot,
        &mut options,
        "tailwind",
        "tw.css",
        "/packages/tailwind",
        "@theme { --color-brand: red; } @tailwind utilities;",
    );
    let prepared = prepare_stylesheet(&snapshot, &options).unwrap();
    let output = build(&prepared, &["tw:flex", "tw:bg-brand"]);
    assert_eq!(
        output.metadata.root,
        SourceRoot::Pattern {
            base: "/app".into(),
            pattern: "../src".into()
        }
    );
    assert!(output.css.contains("display: flex !important;"));
    assert!(output.css.contains("display: block;"));
    assert!(!output.css.contains("display: block !important;"));
    assert!(output.css.contains("--tw-color-brand: red;"));
}

#[test]
fn source_metadata_inline_braces_and_exclusions_are_explicit() {
    let prepared = prepare(
        "@source '../components'; @source not '../generated'; @source inline('{hover:,}p-{1..3}'); @source not inline('p-2'); @theme { --spacing: 1px; } @tailwind utilities source(none);",
    );
    assert_eq!(prepared.metadata().root, SourceRoot::None);
    assert_eq!(prepared.metadata().sources.len(), 2);
    assert_eq!(prepared.metadata().sources[0].base, "/app");
    assert!(prepared.metadata().sources[1].negated);
    let result = build(&prepared, &["p-2", "flex"]);
    assert!(!result.css.contains(".p-2 {"));
    assert!(result.css.contains(".hover\\:p-2"));
    assert!(result.css.contains(".p-3"));
    assert!(result.css.contains(".flex"));
    assert_eq!(result.metadata.inline_candidates.len(), 6);
}

#[test]
fn errors_preserve_source_and_native_plugin_boundary() {
    for source in [
        "@utility empty {}",
        ".x { @utility nested { color: red; } }",
        "@theme { color: red; }",
        "@theme prefix(BAD) { --x: 1; }",
        "@source nope;",
        ".x { @source 'nested'; }",
        "@custom-variant hocus;",
    ] {
        let (snapshot, options) = inputs(source);
        let error = prepare_stylesheet(&snapshot, &options).err().unwrap();
        assert_eq!(
            error.kind,
            StylesheetErrorKind::Invalid,
            "{source}: {error}"
        );
        assert!(error.source.is_some(), "{source}");
    }
    for source in ["@plugin 'plugin';", "@config 'config.js';"] {
        let (snapshot, options) = inputs(source);
        assert_eq!(
            prepare_stylesheet(&snapshot, &options).err().unwrap().kind,
            StylesheetErrorKind::Unsupported
        );
    }
    let (mut snapshot, mut options) = inputs("@import 'ref' theme(reference);");
    import(
        &mut snapshot,
        &mut options,
        "ref",
        "ref.css",
        "/theme",
        ".not-theme { color: red; }",
    );
    assert_eq!(
        prepare_stylesheet(&snapshot, &options).err().unwrap().kind,
        StylesheetErrorKind::Invalid
    );
}

#[test]
fn no_utility_slot_preserves_authored_css_and_lowers_color_mix() {
    let prepared =
        prepare("@theme { --color-brand: red; } .x { color: --alpha(var(--color-brand) / 50%); }");
    let result = build(&prepared, &["flex"]);
    assert!(result.generated.is_empty());
    assert!(!result.css.contains(".flex"));
    assert!(result.css.contains("--color-brand: red;"));
    assert!(result.css.contains("color-mix("));
    assert!(result.remaining_passes.is_empty());
    assert!(
        result
            .css
            .contains("@supports (color: color-mix(in lab, red, red))")
    );
    let ordinary = build(&prepare("@media print { .empty {} }"), &[]);
    assert!(ordinary.css.contains(".empty"));
}

#[test]
fn tw_animate_style_nested_functional_utilities_preserve_all_branches() {
    let prepared = prepare(
        "@theme inline { --animate-enter: enter 1s; @keyframes enter { from { opacity: var(--tw-enter-opacity, 1); } } } @utility animate-in { animation: var(--animate-enter); } @utility fade-in-* { --tw-enter-opacity: calc(--value(integer) / 100); --tw-enter-opacity: --value([*]); } @utility delay-* { --tw-animation-delay: calc(--value(number) * 1ms); } @utility slide-in-from-start-* { &:dir(ltr) { --tw-enter-translate-x: calc(--value(integer) * -1px); --tw-enter-translate-x: calc(--value(ratio) * -100%); } &:dir(rtl) { --tw-enter-translate-x: calc(--value(integer) * 1px); --tw-enter-translate-x: calc(--value(ratio) * 100%); } } @tailwind utilities;",
    );
    let result = build(
        &prepared,
        &[
            "animate-in",
            "fade-in-50",
            "delay-150",
            "slide-in-from-start-1/2",
        ],
    );
    assert!(result.unsupported.is_empty());
    assert!(result.invalid.is_empty());
    assert!(result.css.contains("@keyframes enter"));
    assert!(result.css.contains("transition-delay: 150ms;"));
    assert!(
        result
            .css
            .contains("--tw-animation-delay: calc(150 * 1ms);")
    );
    assert!(result.css.contains("calc(1 / 2 * -100%)"));
    assert!(result.css.contains("calc(1 / 2 * 100%)"));
}

#[test]
fn brace_expansion_nested_sequences_duplicates_and_resource_errors() {
    assert_eq!(
        braces::expand("{hover:,}p-{3..1..2}").unwrap(),
        ["hover:p-3", "p-3", "hover:p-1", "p-1"]
    );
    assert_eq!(braces::expand("p-{01..03}").unwrap(), ["p-1", "p-2", "p-3"]);
    assert_eq!(
        braces::expand("{a,{b,c}}-{x,y}").unwrap(),
        ["a-x", "b-x", "c-x", "a-y", "b-y", "c-y"]
    );
    assert!(braces::expand("{1..2..0}").is_err());
    assert!(braces::expand("{1..100001}").is_err());
    assert!(braces::expand("{oops").is_err());
}
