use super::*;
use serde_json::json;

fn render(source: &str) -> Preprocessed {
    preprocess(source, "document.md", &Options::default())
        .unwrap()
        .unwrap()
}

#[test]
fn raw_html_block_keeps_exact_copies_with_or_without_terminal_newline() {
    for suffix in ["", "\n"] {
        let source =
            format!("---\ntitle: Warning\n---\n\n# Title\n\n<img src=\"photo.png\">{suffix}");
        let output = render(&source);
        let start = source.find("<img").unwrap();
        let end = source[start..].find('>').unwrap() + start + 1;
        assert!(
            output
                .copies
                .iter()
                .any(|copy| copy.original.start <= start && copy.original.end >= end),
            "{:?}",
            output.copies
        );
        for copy in &output.copies {
            assert_eq!(
                &output.code[copy.generated.clone()],
                &source[copy.original.clone()]
            );
        }
    }
}

#[test]
fn frontmatter_preserves_yaml_types_aliases_dates_and_quoted_strings() {
    let output = render(
        "---\r\ntitle: Company Events\r\nupdated: 2026-06-03\r\nquoted: '2026-06-03'\r\naudience: &roles [Employee, Manager]\r\nroles: *roles\r\nfoo-bar: true\r\n---\r\n\r\n# {title}\r\n",
    );
    assert_eq!(
        output.metadata.as_ref().unwrap()["updated"],
        "2026-06-03T00:00:00.000Z"
    );
    assert_eq!(output.metadata.as_ref().unwrap()["quoted"], "2026-06-03");
    assert_eq!(
        output.metadata.as_ref().unwrap()["audience"],
        json!(["Employee", "Manager"])
    );
    assert_eq!(
        output.metadata.as_ref().unwrap()["roles"],
        json!(["Employee", "Manager"])
    );
    assert!(output.code.contains("export const metadata = "));
    assert!(output.code.contains("\"foo-bar\": foo_bar"));
    assert!(output.code.contains("<h1>{title}</h1>"));
}

#[test]
fn gfm_tables_tasks_strikethrough_and_external_links_are_native() {
    let output = render(
        "| Name | Value |\n| :--- | ---: |\n| **First** | `x` |\n\n- [x] Done\n- [ ] Pending\n\n~~removed~~ [external](https://example.com) [local](/docs)\n",
    );
    assert!(output.code.contains("<table>"));
    assert!(output.code.contains("<strong>First</strong>"));
    assert!(output.code.contains("<del>removed</del>"));
    assert!(output.code.contains("type=\"checkbox\""));
    assert!(
        output
            .code
            .contains("rel=\"nofollow\" href=\"https://example.com\"")
    );
    assert!(output.code.contains("<a href=\"/docs\">local</a>"));
}

#[test]
fn expressions_script_style_and_comments_are_copied_without_typography_changes() {
    let source = "<script lang=\"ts\">let name = 'world'; const value = { a: '**raw**' };</script>\n\n<style>p { color: red }</style>\n\n<!-- {ignored} <script>not a script</script> -->\n\n# Hello {name + ' -- ' + `value ${value.a}`}\n\n{#if name}\n\n**Welcome**\n\n{/if}\n";
    let output = render(source);
    assert!(
        output
            .code
            .contains("let name = 'world'; const value = { a: '**raw**' };")
    );
    assert!(output.code.contains("{name + ' -- ' + `value ${value.a}`}"));
    assert!(output.code.contains("<style>p { color: red }</style>"));
    assert!(
        output
            .code
            .contains("<!-- {ignored} <script>not a script</script> -->")
    );
    assert!(output.code.contains("{#if name}"));
    assert!(output.code.contains("<strong>Welcome</strong>"));
    assert!(output.code.contains("{/if}"));
    for copy in &output.copies {
        assert_eq!(
            &output.code[copy.generated.clone()],
            &source[copy.original.clone()]
        );
    }
}

#[test]
fn metadata_inserts_into_existing_module_and_cannot_close_its_script() {
    let output = render(
        "---\ntitle: '</script><script>alert(1)</script>'\n---\n<script context=\"module\">export const answer = 42;</script>\n\n# {title}",
    );
    assert_eq!(output.code.matches("<script ").count(), 1);
    assert!(output.code.contains("\\u003c/script>"));
    assert!(output.code.contains("export const answer = 42;"));
    assert_eq!(output.code.matches("</script>").count(), 1);
}

#[test]
fn code_is_escaped_and_language_highlighting_requires_an_explicit_choice() {
    let output = render("`{value} <script> &amp;`\n\n```\n{value} <script> &amp;\n```\n");
    assert!(
        output
            .code
            .contains("&#123;value&#125; &lt;script&gt; &amp;amp;")
    );
    assert!(!output.code.contains("<script>"));
    assert!(output.code.contains("language-undefined"));
    let error = preprocess(
        "```ts\nconst x = 1;\n```",
        "example.md",
        &Options::default(),
    )
    .unwrap_err();
    assert_eq!(error.code, "unsupported_highlighting");
    let options = options_from_json(&json!({"highlight":false})).unwrap();
    let output = preprocess("```ts\nconst x = { n: 1 };\n```", "example.md", &options)
        .unwrap()
        .unwrap();
    assert!(output.code.contains("language-ts"));
    assert!(output.code.contains("&#123; n: 1 &#125;"));
}

#[test]
fn unsupported_configuration_and_invalid_yaml_fail_instead_of_disappearing() {
    for configuration in [
        json!({"layout":"./Layout.svelte"}),
        json!({"remarkPlugins":["plugin"]}),
        json!({"highlight":{"highlighter":"callback"}}),
        json!({"smartypants":{"quotes":false}}),
    ] {
        assert!(options_from_json(&configuration).is_err());
    }
    for source in [
        "---\ntitle: [bad\n---\nbody",
        "---\ntitle: one\ntitle: two\n---\nbody",
        "---\na: &a [*a]\n---\nbody",
        "---\nfoo-bar: 1\nfoo_bar: 2\n---\nbody",
        "---\na: !!set {x: null}\n---\nbody",
    ] {
        assert!(
            preprocess(source, "test.md", &Options::default()).is_err(),
            "{source}"
        );
    }
    assert!(
        preprocess("# Hello", "Component.svelte", &Options::default())
            .unwrap()
            .is_none()
    );
}

#[test]
fn typography_is_optional_and_never_rewrites_svelte_expressions() {
    let source = "\"Hello\" -- world... {name === 'hello'}";
    let output = render(source);
    assert!(output.code.contains('“'));
    assert!(output.code.contains("{name === 'hello'}"));
    let output = preprocess(
        source,
        "test.md",
        &Options {
            smartypants: false,
            ..Options::default()
        },
    )
    .unwrap()
    .unwrap();
    assert!(output.code.contains("&quot;Hello&quot; -- world..."));
}

#[test]
fn source_map_uses_utf16_columns_and_keeps_generated_regions_unmapped() {
    let mut output = Output::new("a🤖b\nc");
    output.generated("<p>");
    output.copy(1..6);
    output.generated("</p>");
    let map = output.map("unicode.md");
    assert_eq!(map["mappings"], "GAAC,EAAE,C");
    assert_eq!(map["sourcesContent"][0], "a🤖b\nc");
    let source = "# 🤖 title\n\n<script>let name = '🌎';</script>\n\n{name}";
    let result = render(source);
    assert_eq!(result.map["version"], 3);
    assert!(
        result
            .copies
            .iter()
            .any(|copy| source[copy.original.clone()].contains("let name"))
    );
    for copy in &result.copies {
        assert_eq!(
            result.code[copy.generated.clone()],
            source[copy.original.clone()]
        );
    }
}

#[test]
fn yaml_merge_keys_preserve_explicit_overrides() {
    let result = render("---\nbase: &base {a: 1, b: 2}\nmerged: {b: 3, <<: *base}\n---\nHello");
    assert_eq!(result.metadata.unwrap()["merged"], json!({"a":1,"b":3}));
}
