use super::*;
fn input(sources: &[(&str, &str)], imports: &[(&str, &str, &str)]) -> BundleInput {
    BundleInput {
        entry: sources[0].0.into(),
        sources: sources
            .iter()
            .map(|(path, source)| (path.to_string(), source.to_string()))
            .collect(),
        imports: imports
            .iter()
            .map(|(from, specifier, path)| ImportResolution {
                from: from.to_string(),
                specifier: specifier.to_string(),
                target: ImportTarget::File {
                    path: path.to_string(),
                },
            })
            .collect(),
        assets: Vec::new(),
        options: Options::default(),
    }
}

#[test]
fn imports_preserve_wrappers_and_repeated_import_order() {
    let source = input(
        &[
            (
                "/entry.css",
                "@import './a.css'; @import './b.css'; @import './a.css'; @import './c.css' layer(ui) supports(display: grid) screen;",
            ),
            ("/a.css", ".a {color: red}"),
            ("/b.css", ".b {color: blue}"),
            ("/c.css", ".c {display: grid}"),
        ],
        &[
            ("/entry.css", "./a.css", "/a.css"),
            ("/entry.css", "./b.css", "/b.css"),
            ("/entry.css", "./c.css", "/c.css"),
        ],
    );
    let output = bundle(&source).unwrap();
    assert_eq!(output.code.matches(".a {").count(), 1, "{}", output.code);
    assert!(output.code.find(".b {") < output.code.find(".a {"));
    assert!(output.code.contains("@layer ui"), "{}", output.code);
    assert!(output.code.contains("@supports"));
    assert!(output.code.contains("@media screen"));
    assert_eq!(output.resolved_imports.len(), 3);
    assert_eq!(output.map.as_ref().unwrap()["version"], 3);
    assert!(!output.requires_asset_linking);
}

#[test]
fn url_placeholders_preserve_each_origin_and_asset_identity() {
    let mut source = input(
        &[
            (
                "/entry.css",
                "@import './nested/a.css'; .b {background:url('./image.svg?x=1#icon')}",
            ),
            (
                "/nested/a.css",
                ".a {background:url('./image.svg?x=1#icon')}",
            ),
        ],
        &[("/entry.css", "./nested/a.css", "/nested/a.css")],
    );
    source.assets.push(AssetResolution {
        from: "/nested/a.css".into(),
        url: "./image.svg?x=1#icon".into(),
        path: "/nested/image.svg".into(),
    });
    let output = bundle(&source).unwrap();
    assert!(output.requires_asset_linking);
    assert_eq!(output.dependencies.len(), 2);
    let nested = output
        .dependencies
        .iter()
        .find(|d| d["loc"]["filePath"] == "/nested/a.css")
        .unwrap();
    let entry = output
        .dependencies
        .iter()
        .find(|d| d["loc"]["filePath"] == "/entry.css")
        .unwrap();
    assert_eq!(nested["resolvedPath"], "/nested/image.svg");
    assert_eq!(entry["resolutionStatus"], "requiresHostResolution");
    assert_eq!(nested["url"], "./image.svg?x=1#icon");
    assert_ne!(nested["placeholder"], entry["placeholder"]);
    assert!(
        output
            .code
            .contains(nested["placeholder"].as_str().unwrap())
    );
}

#[test]
fn modules_compose_across_snapshot_files() {
    let mut source = input(
        &[
            (
                "/entry.module.css",
                ".primary {composes: base from './base.module.css';color:red}",
            ),
            ("/base.module.css", ".base {display:block}"),
        ],
        &[("/entry.module.css", "./base.module.css", "/base.module.css")],
    );
    source.options.css_modules = Some(ModulesOptions::default());
    let output = bundle(&source).unwrap();
    assert!(
        output.exports["primary"]["name"].is_string(),
        "{}",
        output.exports
    );
    assert!(
        !output.exports["primary"]["composes"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(!output.code.contains("composes:"));
    assert!(output.code.contains("display: block"));
}

#[test]
fn external_imports_and_absolute_urls_are_explicit_placeholders() {
    let mut source = input(
        &[(
            "/entry.css",
            "@import 'remote' layer(ui); .a {mask:url('#mask');background:url('data:image/svg+xml,test')}",
        )],
        &[],
    );
    source.imports.push(ImportResolution {
        from: "/entry.css".into(),
        specifier: "remote".into(),
        target: ImportTarget::External {
            url: "https://example.test/style.css".into(),
        },
    });
    let output = bundle(&source).unwrap();
    assert!(output.requires_asset_linking);
    assert!(output.code.contains("@import"));
    assert!(output.code.contains("layer(ui)"));
    assert!(
        output
            .dependencies
            .iter()
            .all(|d| d["resolutionStatus"] == "external")
    );
    assert!(
        output
            .dependencies
            .iter()
            .any(|d| d["resolvedUrl"] == "https://example.test/style.css")
    );
}

#[test]
fn no_filesystem_fallback_and_no_silent_tailwind_passthrough() {
    assert!(bundle(&input(&[("/entry.css", "@import './absent.css';")], &[])).is_err());
    for css in [
        "@theme {--color:red}",
        ".a {@apply flex}",
        "@media screen {@utility x {display:block}}",
        r"@\74 heme {--color:red}",
        ".a {color:theme('colors.red')}",
    ] {
        assert_eq!(
            bundle(&input(&[("/entry.css", css)], &[]))
                .unwrap_err()
                .code,
            "unsupported_css_syntax",
            "{css}"
        );
    }
    assert!(
        bundle(&input(
            &[(
                "/entry.css",
                r#"/* @apply flex */ .a::before {content:"@theme"}"#
            )],
            &[]
        ))
        .is_ok()
    );
    let invalid = json!({"op":"bundle","input":{"entry":"/a.css","sources":{"/a.css":".a{}"},"options":{"unknown":true}}});
    assert_eq!(dispatch(invalid.to_string().as_bytes())["ok"], false);
}

fn asset(from: &str, url: &str, emitted: &str) -> AssetLink {
    AssetLink {
        from: from.into(),
        url: url.into(),
        emitted_url: emitted.into(),
    }
}

fn position(text: &str, offset: usize) -> ts_svelte::Position {
    ts_svelte::Document::new(ts_svelte::Snapshot {
        uri: "test.css".into(),
        version: 0,
        text: text.into(),
    })
    .position_at(offset)
    .unwrap()
}

fn assert_rule_mapping(
    code: &str,
    map: &Value,
    generated_needle: &str,
    source: &str,
    original_needle: &str,
) {
    let generated = position(code, code.find(generated_needle).unwrap());
    let original = position(source, source.find(original_needle).unwrap());
    let parsed = SourceMap::from_json("/", &map.to_string()).unwrap();
    let mapping = parsed
        .get_mappings()
        .into_iter()
        .find(|m| m.generated_line == generated.line && m.generated_column == generated.character)
        .unwrap_or_else(|| panic!("No mapping at {generated:?}: {code}"));
    let location = mapping.original.unwrap();
    assert_eq!(
        (location.original_line, location.original_column),
        (original.line, original.character)
    );
}

#[test]
fn printer_maps_unicode_and_escaped_css_modules_in_utf16() {
    let css = ".é😀 {content:'漢😀'} .second {color:red}";
    let mut source = input(&[("/entry.css", css)], &[]);
    source.options.minify = true;
    let output = bundle(&source).unwrap();
    assert_rule_mapping(
        &output.code,
        output.map.as_ref().unwrap(),
        ".second",
        css,
        ".second",
    );
    source.options.css_modules = Some(ModulesOptions {
        pattern: "9_[local]".into(),
        ..Default::default()
    });
    let output = bundle(&source).unwrap();
    assert!(output.code.contains(r"\39 "), "{}", output.code);
    assert_rule_mapping(
        &output.code,
        output.map.as_ref().unwrap(),
        r".\39 _second",
        css,
        ".second",
    );
    // Linking zero dependencies still validates that every generated mapping is
    // representable in the original printer output's UTF-16 coordinate space.
    link(&output, &[]).unwrap();
}

#[test]
fn link_nested_assets_preserves_suffixes_and_map_shifts() {
    let css = ".first{content:'😀';background:url('./picture.svg?q=1#icon')} .second{color:red}";
    let source = input(
        &[
            ("/entry.css", "@import './nested/a.css'; .last{color:blue}"),
            ("/nested/a.css", css),
        ],
        &[("/entry.css", "./nested/a.css", "/nested/a.css")],
    );
    let output = bundle(&source).unwrap();
    for emitted in ["x", "../assets/😀-long-output-picture.abc123.svg"] {
        let linked = link(
            &output,
            &[asset("/nested/a.css", "./picture.svg?q=1#icon", emitted)],
        )
        .unwrap();
        assert!(linked.code.contains(&format!("{emitted}?q=1#icon")));
        assert!(!linked.requires_asset_linking);
        assert_eq!(linked.replacement_count, 1);
        assert_eq!(
            linked.dependencies[0]["linkedUrl"],
            format!("{emitted}?q=1#icon")
        );
        assert_rule_mapping(
            &linked.code,
            linked.map.as_ref().unwrap(),
            ".second",
            css,
            ".second",
        );
        assert_eq!(
            linked.map.as_ref().unwrap()["sourcesContent"],
            output.map.as_ref().unwrap()["sourcesContent"]
        );
    }
}

#[test]
fn repeated_placeholders_do_not_replace_misleading_content_strings() {
    let initial = input(
        &[(
            "/entry.css",
            ".a{background:url('./x.svg')}.b{mask:url('./x.svg')}",
        )],
        &[],
    );
    let initial = bundle(&initial).unwrap();
    let placeholder = initial.dependencies[0]["placeholder"].as_str().unwrap();
    let source =
        format!(".a{{background:url('./x.svg');content:'{placeholder}'}}.b{{mask:url('./x.svg')}}");
    let output = bundle(&input(&[("/entry.css", &source)], &[])).unwrap();
    let linked = link(&output, &[asset("/entry.css", "./x.svg", "../x.hash.svg")]).unwrap();
    assert_eq!(linked.replacement_count, 2);
    assert_eq!(linked.code.matches("../x.hash.svg").count(), 2);
    assert_eq!(linked.code.matches(placeholder).count(), 1);
    assert!(
        linked.code.contains(&format!("content: \"{placeholder}\"")),
        "{}",
        linked.code
    );
}

#[test]
fn external_imports_data_fragments_and_public_urls_are_restored() {
    let mut source = input(
        &[(
            "/entry.css",
            "@import 'remote' layer(ui) supports(display:grid) screen;.a{mask:url('#mask');background:url('data:image/svg+xml,a');cursor:url('/cursor.cur'),auto}",
        )],
        &[],
    );
    source.imports.push(ImportResolution {
        from: "/entry.css".into(),
        specifier: "remote".into(),
        target: ImportTarget::External {
            url: "https://example.test/a.css?v=1#x".into(),
        },
    });
    let output = bundle(&source).unwrap();
    let linked = link(&output, &[]).unwrap();
    for expected in [
        "https://example.test/a.css?v=1#x",
        "#mask",
        "data:image/svg+xml,a",
        "/cursor.cur",
        "layer(ui)",
        "supports(display: grid)",
    ] {
        assert!(
            linked.code.contains(expected),
            "missing {expected}: {}",
            linked.code
        );
    }
    assert_eq!(linked.replacement_count, output.dependencies.len());
    assert!(linked.consumed_links.is_empty());
}

#[test]
fn linking_escapes_output_urls_and_keeps_percent_encoded_delimiters() {
    let source = input(
        &[(
            "/entry.css",
            ".a{background:url('./x%3F.svg?raw#id')}.b{color:blue}",
        )],
        &[],
    );
    let output = bundle(&source).unwrap();
    let emitted = "../😀/quote\"slash\\newline\n%3F.svg";
    let linked = link(
        &output,
        &[asset("/entry.css", "./x%3F.svg?raw#id", emitted)],
    )
    .unwrap();
    let mut escaped = String::new();
    cssparser::serialize_string(&format!("{emitted}?raw#id"), &mut escaped).unwrap();
    assert!(linked.code.contains(&escaped));
    assert_eq!(linked.code.lines().count(), output.code.lines().count());
    assert_rule_mapping(
        &linked.code,
        linked.map.as_ref().unwrap(),
        ".b",
        &source.sources["/entry.css"],
        ".b",
    );
}

#[test]
fn linking_rejects_partial_duplicate_unused_and_tampered_requests() {
    let mut output = bundle(&input(
        &[(
            "/entry.css",
            ".a{background:url('./x.svg');mask:url('./y.svg')}",
        )],
        &[],
    ))
    .unwrap();
    let x = asset("/entry.css", "./x.svg", "x.svg");
    let y = asset("/entry.css", "./y.svg", "y.svg");
    assert!(link(&output, &[x.clone()]).is_err());
    assert!(link(&output, &[x.clone(), y.clone(), x.clone()]).is_err());
    assert!(
        link(
            &output,
            &[
                x.clone(),
                y.clone(),
                asset("/entry.css", "./z.svg", "z.svg")
            ]
        )
        .is_err()
    );
    assert!(
        link(
            &output,
            &[asset("/entry.css", "./x.svg", "x.svg?new"), y.clone()]
        )
        .is_err()
    );
    output.code = output.code.replace(
        output.dependencies[0]["placeholder"].as_str().unwrap(),
        "untracked",
    );
    assert!(link(&output, &[x, y]).is_err());
}

#[test]
fn linked_map_marks_replacement_generated_and_restores_following_mapping() {
    let css = ".a{background:url('./x.svg');color:red}.b{color:blue}";
    let mut source = input(&[("/entry.css", css)], &[]);
    source.options.minify = true;
    let output = bundle(&source).unwrap();
    let linked = link(
        &output,
        &[asset("/entry.css", "./x.svg", "longer-😀-output.svg")],
    )
    .unwrap();
    assert_rule_mapping(&linked.code, linked.map.as_ref().unwrap(), ".b", css, ".b");
    let parsed = SourceMap::from_json("/", &linked.map.as_ref().unwrap().to_string()).unwrap();
    let url = position(&linked.code, linked.code.find('"').unwrap());
    assert!(
        parsed
            .get_mappings()
            .iter()
            .any(|m| m.generated_line == url.line
                && m.generated_column == url.character
                && m.original.is_none())
    );
    source.options.source_map = false;
    assert!(
        link(
            &bundle(&source).unwrap(),
            &[asset("/entry.css", "./x.svg", "x.svg")]
        )
        .unwrap()
        .map
        .is_none()
    );
}
