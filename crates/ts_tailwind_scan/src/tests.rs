use super::*;
fn source(id: &str, extension: &str, text: &str) -> SourceSnapshot {
    SourceSnapshot {
        id: id.into(),
        version: 7,
        extension: extension.into(),
        text: text.into(),
    }
}
#[test]
fn svelte_directives_have_exact_original_utf8_ranges_and_retained_versions() {
    let input = source(
        "Component.svelte",
        "svelte",
        "😀\r\n<div\nclass:flex={yes} class:px-2.5={yes} class=\"hover:bg-red-500\" />",
    );
    let result = scan_sources(&[input.clone()], &ScanOptions::default()).unwrap();
    assert!(result.sources[0].preprocessed);
    assert_eq!(result.sources[0].version, 7);
    for candidate in ["flex", "px-2.5", "hover:bg-red-500"] {
        assert!(result.candidates.iter().any(|value| value == candidate));
        let hit = result.sources[0]
            .occurrences
            .iter()
            .find(|hit| hit.value == candidate)
            .unwrap();
        let range = hit.original_range.unwrap();
        assert_eq!(&input.text[range.start..range.end], candidate);
        assert_eq!(hit.mapping, MappingStatus::Exact);
        assert_eq!(range.start, input.text.find(candidate).unwrap());
    }
}
#[test]
fn html_javascript_typescript_and_markdown_use_official_extraction() {
    let inputs = vec![
        source("page", "html", "<div class=\"grid grid-cols-2\"></div>"),
        source(
            "script",
            "js",
            "const styles = ['flex', 'md:hover:text-red-500'];",
        ),
        source(
            "typed",
            "ts",
            "const styles: string = 'bg-[color:var(--brand)]';",
        ),
        source("markdown", "md", ":span[Some text]{.p-4.bg-red-500}"),
    ];
    let result = scan_sources(&inputs, &ScanOptions::default()).unwrap();
    for expected in [
        "grid",
        "grid-cols-2",
        "flex",
        "md:hover:text-red-500",
        "bg-[color:var(--brand)]",
        "p-4",
        "bg-red-500",
    ] {
        assert!(
            result.candidates.iter().any(|c| c == expected),
            "missing {expected}"
        );
    }
    assert!(result.custom_properties.contains(&"--brand".into()));
    assert!(
        result.sources[3]
            .occurrences
            .iter()
            .filter(|h| ["p-4", "bg-red-500"].contains(&h.value.as_str()))
            .all(|h| h.mapping == MappingStatus::Exact)
    );
}
#[test]
fn css_contributes_variable_uses_not_class_or_variable_definitions() {
    let input = source(
        "style",
        "css",
        ":root { --defined: red; } .flex { color: var(--used); width: var( --spaced); }",
    );
    let output = scan_sources(&[input.clone()], &ScanOptions::default()).unwrap();
    assert!(output.candidates.is_empty());
    // Upstream matches the exact bytes `var(` immediately before a variable.
    assert_eq!(output.custom_properties, ["--used"]);
    let range = output.sources[0].occurrences[0].original_range.unwrap();
    assert_eq!(&input.text[range.start..range.end], "--used");
    let options = ScanOptions {
        mode: ExtractionMode::Candidates,
        ..Default::default()
    };
    let output = scan_sources(&[input], &options).unwrap();
    assert!(output.custom_properties.contains(&"--defined".into()));
}
#[test]
fn equal_length_preprocessor_output_does_not_invent_a_mapping() {
    let input = source(
        "template",
        "vue",
        "<template lang=\"pug\">\n.bg-neutral-900.text-red-500 Hello\n</template>",
    );
    let output = scan_sources(&[input.clone()], &ScanOptions::default()).unwrap();
    assert!(output.sources[0].preprocessed);
    for candidate in ["bg-neutral-900", "text-red-500"] {
        let hit = output.sources[0]
            .occurrences
            .iter()
            .find(|h| h.value == candidate)
            .unwrap();
        assert_eq!(hit.mapping, MappingStatus::Unavailable);
        assert_eq!(hit.original_range, None);
    }
}
#[test]
fn repeated_occurrences_keep_positions_while_candidate_union_is_sorted_unique() {
    let inputs = [
        source("one", "txt", "flex grid flex"),
        source("two", "txt", "grid"),
    ];
    let output = scan_sources(&inputs, &ScanOptions::default()).unwrap();
    assert_eq!(output.candidates, ["flex", "grid"]);
    assert_eq!(
        output.sources[0]
            .occurrences
            .iter()
            .filter(|h| h.value == "flex")
            .map(|h| h.original_range.unwrap().start)
            .collect::<Vec<_>>(),
        [0, 10]
    );
    assert_eq!(
        output,
        scan_sources(&inputs, &ScanOptions::default()).unwrap()
    );
}
#[test]
fn failures_are_explicit_and_json_protocol_matches_native() {
    let input = source("a", "txt", "flex grid");
    assert_eq!(
        scan_sources(&[input.clone(), input.clone()], &ScanOptions::default())
            .unwrap_err()
            .code,
        "invalid_source"
    );
    let mut options = ScanOptions::default();
    options.limits.line_bytes = 2;
    assert_eq!(
        scan_sources(&[input.clone()], &options).unwrap_err().code,
        "line_limit"
    );
    options = ScanOptions::default();
    options.limits.occurrences = 1;
    assert_eq!(
        scan_sources(&[input.clone()], &options).unwrap_err().code,
        "output_limit"
    );
    let request = ScanRequest {
        protocol: 1,
        sources: vec![input],
        options: ScanOptions::default(),
    };
    let output: ScanOutput =
        serde_json::from_str(&scan_json(&serde_json::to_string(&request).unwrap()).unwrap())
            .unwrap();
    assert_eq!(
        output,
        scan_sources(&request.sources, &request.options).unwrap()
    );
    assert_eq!(
        scan_json(r#"{"protocol":2,"sources":[]}"#)
            .unwrap_err()
            .code,
        "unsupported_protocol"
    );
}
#[test]
fn nested_template_preprocessing_has_a_real_recursion_bound() {
    let text = format!(
        "{}flex{}",
        "<template lang=\"vue\">".repeat(70),
        "</template>".repeat(70)
    );
    assert_eq!(
        scan_sources(&[source("nested", "vue", &text)], &ScanOptions::default())
            .unwrap_err()
            .code,
        "preprocess_limit"
    );
    // A rejected operation does not leave the next snapshot's counters poisoned.
    assert_eq!(
        scan_sources(&[source("next", "txt", "flex")], &ScanOptions::default())
            .unwrap()
            .candidates,
        ["flex"]
    );
}
