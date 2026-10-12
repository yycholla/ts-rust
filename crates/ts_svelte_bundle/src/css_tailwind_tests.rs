use super::*;
use crate::{BundleResult, generate};
use base64::Engine as _;
use std::sync::atomic::{AtomicUsize, Ordering};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let time = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "ts-svelte-tailwind-{}-{time}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn write(&self, name: &str, text: &str) {
        let path = self.0.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    fn request(&self) -> BundleRequest {
        serde_json::from_value(json!({"root":self.0,"entries":{"entry":"entry.js"},"target":"client","compatibility":{},"sourcemap":true,"css":{"assetsInlineLimit":0,"tailwind":{"entries":["styles/app.css"]}}})).unwrap()
    }
    fn entry(&self) {
        self.write(
            "entry.js",
            "import './styles/app.css'; export const ready = true;",
        );
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn css_text(result: &BundleResult) -> String {
    result
        .assets
        .iter()
        .filter(|a| a.file_name.ends_with(".css"))
        .map(|a| {
            String::from_utf8(
                base64::engine::general_purpose::STANDARD
                    .decode(&a.source)
                    .unwrap(),
            )
            .unwrap()
        })
        .collect::<Vec<_>>()
        .join("\n")
}
fn maps(result: &BundleResult) -> Vec<Value> {
    result
        .assets
        .iter()
        .filter(|a| a.file_name.ends_with(".css.map"))
        .map(|a| {
            serde_json::from_slice(
                &base64::engine::general_purpose::STANDARD
                    .decode(&a.source)
                    .unwrap(),
            )
            .unwrap()
        })
        .collect()
}

#[tokio::test]
async fn native_tailwind_generates_imported_theme_and_selected_source_utilities() {
    let fixture = Fixture::new();
    fixture.entry();
    fixture.write("styles/app.css","@import '../theme/theme.css'; @tailwind utilities source(none); @source '../content'; @source not '../content/private.html';");
    fixture.write(
        "theme/theme.css",
        "@theme { --color-brand: #123456; } .retained { color: red; }",
    );
    fixture.write("content/view.html", "<div class='bg-brand flex'>");
    fixture.write("content/private.html", "<div class='grid'>");
    let result = generate(fixture.request()).await.unwrap();
    let css = css_text(&result);
    assert!(css.contains(".bg-brand"));
    assert!(css.contains("display: flex"));
    assert!(!css.contains("display: grid"));
    assert!(!css.contains("@tailwind"));
    assert!(!css.contains("@theme"));
    let module = result.css.modules.values().next().unwrap();
    let tailwind = module.tailwind.as_ref().unwrap();
    assert_eq!(tailwind.version, "4.2.1");
    assert!(tailwind.generated_candidates.contains(&"flex".into()));
    assert!(
        module
            .sources
            .iter()
            .any(|s| s.ends_with("theme/theme.css"))
    );
    assert!(
        tailwind
            .watch
            .files
            .iter()
            .any(|s| s.ends_with("content/view.html"))
    );
    assert!(
        tailwind
            .watch
            .missing
            .iter()
            .any(|s| s.ends_with(".ignore"))
    );
    assert!(
        maps(&result)
            .iter()
            .flat_map(|m| m["sources"].as_array().unwrap())
            .any(|s| s.as_str().unwrap().ends_with("theme/theme.css"))
    );
}

#[tokio::test]
async fn native_tailwind_links_imported_inline_theme_and_font_urls_with_original_maps() {
    let fixture = Fixture::new();
    fixture.entry();
    fixture.write(
        "styles/app.css",
        "@import '../package/theme.css'; @tailwind utilities source(none); @source '../content';",
    );
    let original = "/* 😀 */\r\n@theme inline { --background-image-banner: url('./é%20image.svg?v=1#icon'); } @font-face {font-family: demo; src: url('./font.woff2')} .from-package {color: red}";
    fixture.write("package/theme.css", original);
    fixture.write("package/é image.svg", "<svg>correct</svg>");
    fixture.write("package/font.woff2", "font bytes");
    fixture.write("styles/font.woff2", "wrong bytes");
    fixture.write("content/view.html", "<div class='bg-banner'>");
    let result = generate(fixture.request()).await.unwrap();
    let css = css_text(&result);
    assert!(css.contains(".bg-banner"));
    assert!(css.contains("?v=1#icon"));
    assert!(!css.contains("__ts_tailwind_asset_"));
    assert!(result.assets.iter().any(|a| {
        a.file_name.ends_with(".svg")
            && base64::engine::general_purpose::STANDARD
                .decode(&a.source)
                .unwrap()
                == b"<svg>correct</svg>"
    }));
    assert!(result.assets.iter().any(|a| {
        a.file_name.ends_with(".woff2")
            && base64::engine::general_purpose::STANDARD
                .decode(&a.source)
                .unwrap()
                == b"font bytes"
    }));
    assert!(
        maps(&result)
            .iter()
            .flat_map(|m| m["sourcesContent"].as_array().unwrap())
            .any(|value| value.as_str() == Some(original))
    );
    assert!(
        maps(&result)
            .iter()
            .flat_map(|m| m["sourcesContent"].as_array().unwrap())
            .all(|value| !value
                .as_str()
                .unwrap_or("")
                .contains("__ts_tailwind_asset_"))
    );
    let output = result.css.modules.values().next().unwrap();
    assert_eq!(output.assets.len(), 2);
    assert!(
        output
            .tailwind
            .as_ref()
            .unwrap()
            .watch
            .files
            .iter()
            .any(|s| s.ends_with("package/font.woff2"))
    );
}

#[tokio::test]
async fn native_tailwind_fresh_build_removes_classes_after_source_edits() {
    let fixture = Fixture::new();
    fixture.entry();
    fixture.write(
        "styles/app.css",
        "@tailwind utilities source(none); @source '../content';",
    );
    fixture.write("content/view.html", "<div class='flex'>");
    let first = generate(fixture.request()).await.unwrap();
    assert!(css_text(&first).contains(".flex"));
    fixture.write("content/view.html", "<div class='grid'>");
    let second = generate(fixture.request()).await.unwrap();
    assert!(css_text(&second).contains(".grid"));
    assert!(!css_text(&second).contains(".flex"));
    assert_ne!(
        first.css.modules.values().next().unwrap().file_name,
        second.css.modules.values().next().unwrap().file_name
    );
}

#[tokio::test]
async fn native_tailwind_typography_registration_executes_rust_plugin() {
    let fixture = Fixture::new();
    fixture.entry();
    fixture.write("styles/app.css","@plugin '@tailwindcss/typography'; @tailwind utilities source(none); @source '../content';");
    fixture.write(
        "content/view.html",
        "<article class='prose prose-sm prose-invert'>",
    );
    let mut request = fixture.request();
    request
        .css
        .as_mut()
        .unwrap()
        .tailwind
        .as_mut()
        .unwrap()
        .plugins
        .push(ts_tailwind::snapshot::PluginRegistration {
            specifier: "@tailwindcss/typography".into(),
            implementation: "@tailwindcss/typography".into(),
            version: "0.5.19".into(),
            options: Value::Null,
        });
    let result = generate(request).await.unwrap();
    let css = css_text(&result);
    assert!(css.contains(".prose"));
    assert!(css.contains("--tw-prose-body"));
    assert!(css.contains("not-prose"));
    assert!(
        result
            .css
            .modules
            .values()
            .next()
            .unwrap()
            .tailwind
            .as_ref()
            .unwrap()
            .generated_candidates
            .contains(&"prose".into())
    );
}

#[tokio::test]
async fn native_tailwind_uses_virtual_css_and_changed_or_new_scanned_sources() {
    let fixture = Fixture::new();
    fixture.entry();
    fixture.write("styles/app.css", "@tailwind utilities;");
    fixture.write("content/view.html", "<div class='flex'>");
    let mut request = fixture.request();
    request.virtual_modules.insert(
        fixture.0.join("styles/app.css"),
        ".edited {color: red} @tailwind utilities;".into(),
    );
    let result = generate(request).await.unwrap();
    assert!(css_text(&result).contains(".edited"));
    assert!(css_text(&result).contains(".flex"));
    let mut request = fixture.request();
    let path = fixture.0.join("content/view.html");
    request
        .virtual_modules
        .insert(path.clone(), "<div class='grid'>".into());
    request
        .css
        .as_mut()
        .unwrap()
        .tailwind
        .as_mut()
        .unwrap()
        .snapshot_versions
        .insert(path.clone(), -31);
    let result = generate(request).await.unwrap();
    assert!(css_text(&result).contains(".grid"));
    assert!(!css_text(&result).contains(".flex"));
    assert_eq!(
        result
            .css
            .modules
            .values()
            .next()
            .unwrap()
            .tailwind
            .as_ref()
            .unwrap()
            .watch
            .snapshots[path.to_str().unwrap()]
        .version,
        -31
    );
    let mut request = fixture.request();
    request.virtual_modules.insert(
        fixture.0.join("content/new.html"),
        "<div class='grid'>".into(),
    );
    let result = generate(request).await.unwrap();
    assert!(css_text(&result).contains(".grid"));
    assert!(css_text(&result).contains(".flex"));
    assert!(!fixture.0.join("content/new.html").exists());
}

#[tokio::test]
async fn native_tailwind_ignored_kit_virtual_modules_do_not_change_discovery() {
    let fixture = Fixture::new();
    fixture.entry();
    fixture.write("styles/app.css", "@tailwind utilities;");
    fixture.write("content/view.html", "<div class='flex'>");
    std::fs::create_dir(fixture.0.join(".svelte-kit")).unwrap();
    let mut request = fixture.request();
    request.virtual_modules.insert(
        fixture.0.join(".svelte-kit/generated.js"),
        "export const className='grid';".into(),
    );
    let result = generate(request).await.unwrap();
    assert!(css_text(&result).contains(".flex"));
    assert!(!css_text(&result).contains(".grid"));
}

#[test]
fn native_tailwind_asset_verification_rejects_changed_bytes_and_directive_scan_ignores_strings() {
    let fixture = Fixture::new();
    fixture.write("asset.svg", "first");
    let asset = ts_tailwind_host::compile::AssetReference {
        token: "token".into(),
        path: fixture.0.join("asset.svg").to_str().unwrap().into(),
        byte_length: 5,
        sha256: hash(b"first"),
        query: None,
        fragment: None,
        original_url: "asset.svg".into(),
        occurrences: vec![],
    };
    assert_eq!(verified_tailwind_asset(&asset).unwrap(), b"first");
    fixture.write("asset.svg", "other");
    assert!(verified_tailwind_asset(&asset).is_err());
    assert!(
        reject_uncompiled_tailwind(
            "virtual.css",
            r#"/* @theme */.a{content:"@apply flex; --spacing(4)"}"#
        )
        .is_ok()
    );
    assert!(
        reject_uncompiled_tailwind("virtual.css", ".a{@apply flex}")
            .unwrap_err()
            .to_string()
            .contains("css.tailwind.entries")
    );
    assert!(reject_uncompiled_tailwind("virtual.css", ".a{margin:--spacing(4)}").is_err());
}

#[tokio::test]
async fn extracted_css_tailwind_directives_require_explicit_native_opt_in() {
    use rolldown::{Bundler, BundlerOptions, InputItem, OutputFormat, Platform};
    let fixture = Fixture::new();
    fixture.write("App.svelte", "<div/>");
    let state = CssState::default();
    let id = state
        .register_component(
            fixture.0.join("App.svelte").to_str().unwrap(),
            ".a{@apply flex;}".into(),
            None,
        )
        .unwrap();
    fixture.write("entry.js", &format!("import {};", json!(id)));
    let mut request = fixture.request();
    request.sourcemap = false;
    let plugin = CssPlugin::new(Arc::new(request), CssOptions::default(), state.clone()).unwrap();
    let mut bundler = Bundler::with_plugins(
        BundlerOptions {
            cwd: Some(fixture.0.clone()),
            input: Some(vec![InputItem {
                name: Some("entry".into()),
                import: fixture.0.join("entry.js").to_str().unwrap().into(),
            }]),
            format: Some(OutputFormat::Esm),
            platform: Some(Platform::Browser),
            ..Default::default()
        },
        vec![Plugin::new_shared(plugin)],
    )
    .unwrap();
    let result = bundler.generate().await;
    bundler.close().await.unwrap();
    assert!(result.is_err());
    assert!(state.diagnostics().iter().any(|d| {
        d["message"]
            .as_str()
            .is_some_and(|m| m.contains("Enable css.tailwind"))
    }));
}

#[tokio::test]
async fn native_tailwind_resolves_new_virtual_css_import_and_its_real_asset_base() {
    let fixture = Fixture::new();
    fixture.entry();
    fixture.write("styles/app.css", "@tailwind utilities;");
    fixture.write("package/assets/icon.svg", "<svg>original asset</svg>");
    let mut request = fixture.request();
    request.virtual_modules.insert(
        fixture.0.join("styles/app.css"),
        "@import '../package/theme.css'; @tailwind utilities;".into(),
    );
    let original = "@theme inline {--background-image-icon:url('./assets/icon.svg?v=2#glyph')}";
    request
        .virtual_modules
        .insert(fixture.0.join("package/theme.css"), original.into());
    request.virtual_modules.insert(
        fixture.0.join("new/App.svelte"),
        "<div class='bg-icon'>".into(),
    );
    let result = generate(request).await.unwrap();
    let css = css_text(&result);
    assert!(css.contains(".bg-icon"));
    assert!(css.contains("?v=2#glyph"));
    assert!(!css.contains("__ts_tailwind_asset_"));
    assert!(result.assets.iter().any(|asset| {
        asset.file_name.ends_with(".svg")
            && base64::engine::general_purpose::STANDARD
                .decode(&asset.source)
                .unwrap()
                == b"<svg>original asset</svg>"
    }));
    assert!(maps(&result).iter().any(|map| {
        map["sourcesContent"]
            .as_array()
            .unwrap()
            .iter()
            .any(|source| source == original)
    }));
    assert!(!fixture.0.join("package/theme.css").exists());
}

#[tokio::test]
async fn native_tailwind_component_reference_apply_uses_dirty_component_and_original_map() {
    use rolldown::{Bundler, BundlerOptions, InputItem, OutputFormat, Platform};
    let fixture = Fixture::new();
    fixture.write("App.svelte", "<div class='block'/>");
    fixture.write(
        "theme.css",
        "@theme {--color-brand:#123456;} @tailwind utilities;",
    );
    let css = "@reference './theme.css';\n.a { @apply flex text-brand; }";
    let original = format!("<div class='grid'>🦀</div>\n<style>\n{css}\n</style>");
    let origin = fixture.0.join("App.svelte");
    let mut map = SourceMapBuilder::default();
    let source = map.add_source_and_content(origin.to_str().unwrap(), &original);
    for (line, text) in css.split('\n').enumerate() {
        let mut column = 0;
        for ch in text.chars() {
            map.add_token(
                line as u32,
                column,
                line as u32 + 2,
                column,
                Some(source),
                None,
            );
            column += ch.len_utf16() as u32;
        }
    }
    let state = CssState::default();
    let id = state
        .register_component(
            origin.to_str().unwrap(),
            css.into(),
            Some(serde_json::from_str(&map.into_owned_sourcemap().to_json_string()).unwrap()),
        )
        .unwrap();
    fixture.write("entry.js", &format!("import {};", json!(id)));
    let mut request = fixture.request();
    request
        .virtual_modules
        .insert(origin.clone(), original.clone());
    request
        .css
        .as_mut()
        .unwrap()
        .tailwind
        .as_mut()
        .unwrap()
        .entries
        .clear();
    request
        .css
        .as_mut()
        .unwrap()
        .tailwind
        .as_mut()
        .unwrap()
        .snapshot_versions
        .insert(origin.clone(), -7);
    let options = request.css.clone().unwrap();
    let plugin = CssPlugin::new(Arc::new(request), options, state.clone()).unwrap();
    let mut bundler = Bundler::with_plugins(
        BundlerOptions {
            cwd: Some(fixture.0.clone()),
            input: Some(vec![InputItem {
                name: Some("entry".into()),
                import: fixture.0.join("entry.js").to_str().unwrap().into(),
            }]),
            format: Some(OutputFormat::Esm),
            platform: Some(Platform::Browser),
            ..Default::default()
        },
        vec![Plugin::new_shared(plugin)],
    )
    .unwrap();
    let output = bundler.generate().await.unwrap();
    bundler.close().await.unwrap();
    let css = output
        .assets
        .iter()
        .filter_map(|asset| match asset {
            Output::Asset(a) if a.filename.ends_with(".css") => {
                Some(String::from_utf8(a.source.as_bytes().to_vec()).unwrap())
            }
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(css.contains("display: flex"));
    assert!(css.contains("--color-brand"));
    assert!(!css.contains("@apply") && !css.contains("@reference"));
    let map = output
        .assets
        .iter()
        .find_map(|asset| match asset {
            Output::Asset(a) if a.filename.ends_with(".css.map") => {
                Some(serde_json::from_slice::<Value>(a.source.as_bytes()).unwrap())
            }
            _ => None,
        })
        .unwrap();
    assert!(
        map["sources"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p == origin.to_str().unwrap())
    );
    assert!(
        map["sourcesContent"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s == &original)
    );
    assert!(
        !map["sources"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p.as_str().unwrap().contains("?ts-rust-tailwind-style"))
    );
    let metadata = state.output().unwrap();
    assert_eq!(
        metadata.modules[&id]
            .tailwind
            .as_ref()
            .unwrap()
            .watch
            .snapshots[origin.to_str().unwrap()]
        .version,
        -7
    );
}
