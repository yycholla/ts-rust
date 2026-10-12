use super::*;
struct Fixture(tempfile::TempDir);
impl Fixture {
    fn new() -> Self {
        Self(tempfile::tempdir().unwrap())
    }
    fn write(&self, name: &str, text: &str) {
        let path = self.0.path().join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }
    fn request(&self) -> HostRequest {
        HostRequest {
            root: self.0.path().into(),
            entry: "styles/app.css".into(),
            plugins: vec![],
            aliases: vec![],
            module_paths: vec![],
            limits: Default::default(),
        }
    }
    fn compile(&self) -> CompiledProject {
        compile_project(&self.request(), &Default::default()).unwrap()
    }
}
#[test]
fn compiles_discovered_sources_from_host_root_and_returns_maps() {
    let f = Fixture::new();
    f.write("styles/app.css", "@tailwind utilities;");
    f.write("src/App.svelte", "<div class='flex hover:underline'></div>");
    let result = f.compile();
    assert!(result.build.stylesheet.css.contains(".flex"));
    assert!(
        result
            .build
            .stylesheet
            .css
            .contains("text-decoration-line: underline")
    );
    assert!(
        result
            .build
            .selection
            .selected
            .iter()
            .any(|s| s.path.ends_with("src/App.svelte"))
    );
    assert_eq!(result.map["version"], 3);
    assert!(!result.requires_asset_linking);
    assert!(result.diagnostics.is_empty());
}
#[test]
fn imported_background_and_font_assets_keep_their_original_bases() {
    let f = Fixture::new();
    f.write(
        "styles/app.css",
        "@import '../package/theme.css'; @tailwind utilities source(none);",
    );
    f.write("package/theme.css",".a { background: url('./images/back.png?size=2#sprite'); } @font-face { font-family: test; src: url('./font.woff2') format('woff2'); }");
    f.write("package/images/back.png", "correct-image");
    f.write("package/font.woff2", "correct-font");
    f.write("styles/font.woff2", "incorrect-font");
    let result = f.compile();
    assert!(result.requires_asset_linking);
    assert_eq!(result.assets.len(), 2);
    let image = result
        .assets
        .iter()
        .find(|a| a.path.ends_with("back.png"))
        .unwrap();
    assert_eq!(image.query.as_deref(), Some("size=2"));
    assert_eq!(image.fragment.as_deref(), Some("sprite"));
    assert_eq!(image.sha256, hash(b"correct-image"));
    assert!(image.occurrences[0].file.ends_with("package/theme.css"));
    assert!(
        result
            .build
            .stylesheet
            .css
            .contains(&format!("{}?size=2#sprite", image.token))
    );
    assert!(
        result
            .assets
            .iter()
            .any(|a| a.path.ends_with("package/font.woff2") && a.sha256 == hash(b"correct-font"))
    );
    for asset in &result.assets {
        assert!(result.project.watch.files.contains(&asset.path));
    }
    assert!(
        result.map["sourcesContent"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s.as_str().unwrap().contains("./images/back.png"))
    );
    assert!(
        result
            .preprocessed_snapshot
            .stylesheets
            .iter()
            .any(|s| s.css.contains("__ts_tailwind_asset_"))
    );
}
#[test]
fn imported_inline_theme_urls_are_bound_before_utility_generation() {
    let f = Fixture::new();
    f.write(
        "styles/app.css",
        "@import '../package/theme.css'; @tailwind utilities;",
    );
    f.write(
        "package/theme.css",
        "@theme inline { --background-image-banner: url('./banner.svg'); }",
    );
    f.write("package/banner.svg", "<svg/>");
    f.write("src/App.html", "<div class='bg-banner'>");
    let result = f.compile();
    assert_eq!(result.assets.len(), 1);
    assert!(result.assets[0].path.ends_with("package/banner.svg"));
    assert!(result.build.stylesheet.css.contains(".bg-banner"));
    assert!(
        result
            .build
            .stylesheet
            .css
            .contains(&result.assets[0].token)
    );
}
#[test]
fn css_escapes_percent_encoding_and_url_suffixes_resolve_real_files() {
    let f = Fixture::new();
    f.write(
        "styles/app.css",
        r#".a { background: u\72l("./my%20image.svg?v=%20#icon"); mask: url(./my\20 image.svg); }"#,
    );
    f.write("styles/my image.svg", "<svg/>");
    let result = f.compile();
    assert_eq!(result.assets.len(), 2);
    assert!(
        result
            .assets
            .iter()
            .all(|a| a.path.ends_with("styles/my image.svg"))
    );
    assert!(
        result
            .assets
            .iter()
            .any(|a| a.query.as_deref() == Some("v=%20") && a.fragment.as_deref() == Some("icon"))
    );
    assert_eq!(result.preprocess[0].replacements.len(), 2);
}
#[test]
fn external_data_hash_and_root_urls_preserve_browser_semantics() {
    let f = Fixture::new();
    let css = r##"/* url(./not-a-file) */ .a { --content: "url(./also-not-a-file)"; background: url(https://cdn.example/a.png), url('//cdn.example/b.png'), url(data:image/png;base64,AAAA), url(#mask), url('/public/icon.svg'); }"##;
    f.write("styles/app.css", css);
    let result = f.compile();
    assert!(result.assets.is_empty());
    assert!(result.preprocess[0].replacements.is_empty());
    for url in [
        "https://cdn.example/a.png",
        "//cdn.example/b.png",
        "data:image/png;base64,AAAA",
        "#mask",
        "/public/icon.svg",
    ] {
        assert!(result.build.stylesheet.css.contains(url));
    }
}
#[test]
fn source_map_preserves_original_unicode_css_and_unmaps_linker_tokens() {
    let f = Fixture::new();
    let css = "/* 😀 */\r\n.a { background: url('./é image.svg'); color: red; }\r\n";
    f.write("styles/app.css", css);
    f.write("styles/é image.svg", "<svg/>");
    let result = f.compile();
    assert!(
        result.map["sourcesContent"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v.as_str() == Some(css))
    );
    let replacement = &result.preprocess[0].replacements[0];
    assert_eq!(
        &css[replacement.original_start..replacement.original_end],
        "url('./é image.svg')"
    );
    assert_eq!(
        original_offset(
            replacement.generated_start + 5,
            &result.preprocess[0].replacements
        ),
        None
    );
    let events = decode_mappings(result.map["mappings"].as_str().unwrap()).unwrap();
    for region in result.map["x_ts_rust_assetRegions"].as_array().unwrap() {
        let a = ts_tailwind::snapshot::position(
            &result.build.stylesheet.css,
            region["start"].as_u64().unwrap() as usize,
        )
        .unwrap();
        let b = ts_tailwind::snapshot::position(
            &result.build.stylesheet.css,
            region["end"].as_u64().unwrap() as usize,
        )
        .unwrap();
        assert!(
            events.iter().any(|e| e.line == a.line
                && e.column == a.character as i64
                && e.original.is_none())
        );
        assert!(!events.iter().any(|e| e.original.is_some()
            && (e.line, e.column) >= (a.line, a.character as i64)
            && (e.line, e.column) < (b.line, b.character as i64)));
    }
    source_map::generate(&result.build.stylesheet, &result.project.snapshot).unwrap();
}
#[test]
fn unused_theme_urls_do_not_force_assets_but_used_missing_assets_fail() {
    let f = Fixture::new();
    f.write("styles/app.css","@theme { --background-image-unused: url('./missing.svg'); } @tailwind utilities source(none);");
    assert!(f.compile().assets.is_empty());
    f.write("styles/app.css", ".a { background: url('./missing.svg'); }");
    let error = compile_project(&f.request(), &Default::default()).unwrap_err();
    assert!(
        error
            .watch
            .missing
            .iter()
            .any(|p| p.ends_with("missing.svg"))
    );
}
#[test]
fn url_tokenizer_rejects_dynamic_invalid_and_ambiguous_inputs() {
    for input in [
        ".a{color:url(var(--asset))}",
        ".a{color:url(a b)}",
        ".a{color:url('missing)}",
    ] {
        assert!(urls(input).is_err(), "{input}");
    }
    for input in ["a%2fb.svg", "a%00.svg", "a%5cb.svg", "a%zz.svg"] {
        assert!(decode_path(input).is_err());
    }
    assert_eq!(decode_path("%C3%A9%20x.svg").unwrap(), "é x.svg");
    let found = urls(".a{a:URL( x.svg );b:u\\72 l('a\\\nb.svg');content:'url(no.svg)'}").unwrap();
    assert_eq!(
        found.iter().map(|u| u.url.as_str()).collect::<Vec<_>>(),
        ["x.svg", "ab.svg"]
    );
}
