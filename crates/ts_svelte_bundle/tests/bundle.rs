//! In-process graph correctness cases; external Svelte runtime imports make
//! these independent of an installed Node toolchain. Runtime execution belongs
//! to the host's client/SSR integration checks.
use serde_json::json;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use ts_svelte::compatibility::Versions;
use ts_svelte_bundle::{BundleRequest, CompilerOptions, Target, generate};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "ts-svelte-bundle-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn request(&self, target: Target, entry: &str, source: &str) -> BundleRequest {
        BundleRequest {
            root: self.0.clone(),
            entries: BTreeMap::from([("app".into(), entry.into())]),
            target,
            node_rebundle: false,
            strict_execution_order: None,
            compatibility: Versions {
                svelte_version: "5.53.11".into(),
                ..Default::default()
            },
            dev: false,
            runes: Some(true),
            runes_project_only: false,
            minify: false,
            sourcemap: true,
            aliases: vec![],
            external: vec![],
            external_packages: vec!["svelte".into()],
            ssr_external: None,
            treeshake: None,
            sentry: None,
            conditions: vec![],
            define: BTreeMap::new(),
            tsconfig: None,
            virtual_modules: BTreeMap::from([(entry.into(), source.into())]),
            virtual_module_maps: BTreeMap::new(),
            compiler_options: CompilerOptions::default(),
            mdsvex: None,
            typescript: ts_svelte_transpile::TransformOptions::default(),
            preprocessors: vec![],
            css: None,
            output: Default::default(),
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[tokio::test]
async fn output_policy_and_loaded_graph_preserve_framework_boundaries() {
    let fixture = Fixture::new();
    let mut request = fixture.request(Target::Server, "main.js",
        "import { value } from '$exact'; import '$tree/dead'; import { basename } from 'node:path'; export const answer = value + basename('/x'); export const lazy = () => import('./lazy.js');");
    request.aliases = serde_json::from_value(json!([
        {"find":"$exact","replacement":fixture.0.join("manifest.js"),"matching":"exact"},
        {"find":"$tree","replacement":fixture.0.join("tree"),"matching":"children"}
    ]))
    .unwrap();
    request.virtual_modules.extend([
        ("manifest.js".into(), "export const value = 7;".into()),
        ("tree/dead.js".into(), "export const unused = 1;".into()),
        ("lazy.js".into(), "export const lazy = true;".into()),
    ]);
    request.output = serde_json::from_value(json!({
        "entryFileNames":"server/[name].js", "chunkFileNames":"shared/[name]-[hash].js",
        "preserveEntrySignatures":"strict", "hoistTransitiveImports":false,
        "fixedChunks":{"_app/manifest.js":[fixture.0.join("manifest.js")]}
    }))
    .unwrap();
    let result = generate(request).await.unwrap();
    assert!(
        result
            .chunks
            .iter()
            .any(|chunk| chunk.file_name == "server/app.js")
    );
    assert!(
        result
            .chunks
            .iter()
            .any(|chunk| chunk.file_name == "_app/manifest.js")
    );
    let metadata = serde_json::to_value(&result.module_graph).unwrap();
    assert_eq!(metadata["node:path"]["isExternal"], true);
    assert_eq!(
        metadata[fixture.0.join("tree/dead.js").to_str().unwrap()]["isExternal"],
        false
    );
    assert_eq!(
        metadata[fixture.0.join("main.js").to_str().unwrap()]["isEntry"],
        true
    );
}

#[tokio::test]
async fn client_and_server_use_native_components_and_keep_original_source_maps() {
    let fixture = Fixture::new();
    let source = "<script>let { name = 'world' } = $props();</script><h1>Hello {name}</h1><style>h1 { color: red }</style>";
    for target in [Target::Client, Target::Server] {
        let result = generate(fixture.request(target, "App.svelte", source))
            .await
            .unwrap();
        assert_eq!(result.css_mode, "injected");
        let chunk = result.chunks.iter().find(|c| c.is_entry).unwrap();
        assert!(chunk.exports.iter().any(|e| e == "default"));
        let runtime = if target == Target::Client {
            "svelte/internal/client"
        } else {
            "svelte/internal/server"
        };
        assert!(
            chunk.imports.iter().any(|id| id == runtime),
            "{}",
            chunk.code
        );
        assert!(
            chunk.code.contains("red"),
            "Injected component CSS disappeared"
        );
        let map = chunk.map.as_ref().unwrap();
        assert!(
            map["sourcesContent"]
                .as_array()
                .unwrap()
                .iter()
                .any(|s| s == source)
        );
        let path = fixture.0.join("App.svelte").to_string_lossy().to_string();
        assert_eq!(result.compiler_responses[&path]["ok"], true);
        assert_eq!(
            result.compiler_responses[&path]["backend"]["name"],
            "rsvelte"
        );
    }
}

#[tokio::test]
async fn rune_module_and_dynamic_graph_are_transformed_without_javascript_host() {
    let fixture = Fixture::new();
    let mut request = fixture.request(
        Target::Client,
        "main.ts",
        "export const later = () => import('./state.svelte.js'); export const typed: number = 2;",
    );
    request.virtual_modules.insert("state.svelte.js".into(), "let count = $state(0); export function increment() { count += 1; } export const read = () => count;".into());
    let result = generate(request).await.unwrap();
    assert!(result.chunks.iter().any(|c| c.is_dynamic_entry));
    assert!(result.chunks.iter().any(|c| !c.dynamic_imports.is_empty()));
    assert!(!result.chunks.iter().any(|c| c.code.contains("$state(")));
    assert!(
        result
            .compiler_responses
            .keys()
            .any(|id| id.ends_with("state.svelte.js"))
    );
}

#[tokio::test]
async fn binary_asset_output_is_lossless() {
    use base64::Engine as _;
    let fixture = Fixture::new();
    let bytes = [0, 255, 17, 129, 3];
    std::fs::write(fixture.0.join("image.png"), bytes).unwrap();
    let request = fixture.request(
        Target::Client,
        "main.js",
        "export { default as image } from './image.png';",
    );
    let result = generate(request).await.unwrap();
    let asset = result
        .assets
        .iter()
        .find(|asset| asset.file_name.ends_with(".png"))
        .unwrap();
    assert_eq!(
        base64::engine::general_purpose::STANDARD
            .decode(&asset.source)
            .unwrap(),
        bytes
    );
    assert!(
        result
            .chunks
            .iter()
            .any(|c| c.code.contains(&asset.file_name))
    );
}

#[tokio::test]
async fn compiler_failure_preserves_native_error_and_diagnostics() {
    let fixture = Fixture::new();
    let error = generate(fixture.request(
        Target::Client,
        "App.svelte",
        "<script>const value = $state();</script><p>{</p>",
    ))
    .await
    .unwrap_err();
    assert!(!error.message.trim().is_empty());
    assert!(!error.diagnostics.is_empty());
    let response = error.compiler_responses.values().next().unwrap();
    assert_eq!(response["ok"], false);
    assert_ne!(response["error"], json!(null));
}

#[tokio::test]
async fn external_css_and_unconfigured_markdown_are_explicit_failures() {
    let fixture = Fixture::new();
    for (id, source, expected) in [
        ("app.css", "p{color:red}", "CSS bundling"),
        ("page.md", "# Heading", "preprocessor"),
    ] {
        let error = generate(fixture.request(Target::Client, id, source))
            .await
            .unwrap_err();
        assert!(error.message.contains(expected), "{}", error.message);
    }
}

#[tokio::test]
async fn rune_typescript_graph_preserves_runtime_typescript_and_original_maps() {
    let fixture = Fixture::new();
    let source = "export enum Step { Start = 2, Next }\nexport class Counter { value = $state<number>(Step.Start); constructor(public name: string) {} increment() { this.value += 1; } }";
    for target in [Target::Client, Target::Server] {
        let mut request = fixture.request(
            target,
            "main.ts",
            "export { Counter, Step } from './state.svelte.js';",
        );
        request
            .virtual_modules
            .insert("state.svelte.ts".into(), source.into());
        let result = generate(request).await.unwrap();
        let chunk = result.chunks.iter().find(|c| c.is_entry).unwrap();
        assert!(chunk.exports.iter().any(|s| s == "Counter"));
        assert!(chunk.exports.iter().any(|s| s == "Step"));
        assert!(chunk.code.contains("this.name = name"));
        assert!(!chunk.code.contains("$state("));
        assert!(!chunk.code.contains("<number>"));
        let map = chunk.map.as_ref().unwrap();
        assert!(
            map["sourcesContent"]
                .as_array()
                .unwrap()
                .iter()
                .any(|s| s == source)
        );
        assert!(
            map["sources"]
                .as_array()
                .unwrap()
                .iter()
                .any(|s| s.as_str().is_some_and(|s| s.ends_with("state.svelte.ts")))
        );
        assert_eq!(
            result.compiler_responses[&fixture
                .0
                .join("state.svelte.ts")
                .to_string_lossy()
                .into_owned()]["ok"],
            true
        );
    }
}

#[tokio::test]
async fn markdown_and_component_typescript_share_the_native_chain() {
    let fixture = Fixture::new();
    let source = "---\ntitle: Native\n---\n<script lang=\"ts\">enum Step { Start = 2 } let count = $state<number>(Step.Start);</script>\n\n# Native heading\n\n<button onclick={() => count++}>{count}</button>\n";
    for extension in ["md", "svx"] {
        let filename = format!("Page.{extension}");
        let mut request = fixture.request(Target::Server, &filename, source);
        request.mdsvex = Some(ts_svelte_preprocess::Options::default());
        let result = generate(request).await.unwrap();
        let chunk = result.chunks.iter().find(|c| c.is_entry).unwrap();
        assert!(chunk.exports.iter().any(|s| s == "default"));
        assert!(chunk.code.contains("Native heading"));
        assert!(!chunk.code.contains("enum Step"));
        assert!(!chunk.code.contains("$state("));
        assert!(
            chunk.map.as_ref().unwrap()["sourcesContent"]
                .as_array()
                .unwrap()
                .iter()
                .any(|s| s == source)
        );
        let response =
            &result.compiler_responses[&fixture.0.join(filename).to_string_lossy().into_owned()];
        assert_eq!(response["metadata"]["title"], "Native");
    }
    let source = "<script lang='ts'>class Item { constructor(public name: string) {} } const item = new Item('native');</script><h1>{item.name}</h1>";
    let result = generate(fixture.request(Target::Server, "App.svelte", source))
        .await
        .unwrap();
    assert!(
        result
            .chunks
            .iter()
            .any(|chunk| chunk.code.contains("this.name = name"))
    );
    assert!(result.chunks.iter().any(|chunk| {
        chunk.map.as_ref().unwrap()["sourcesContent"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s == source)
    }));
}

#[tokio::test]
async fn preprocessing_failures_keep_native_diagnostics_and_original_coordinates() {
    let fixture = Fixture::new();
    let source = "---\ntitle: Original\n---\n<script lang=\"ts\">import fs = require('node:fs');</script>\n\n# Heading\n";
    let mut request = fixture.request(Target::Server, "Bad.md", source);
    request.mdsvex = Some(ts_svelte_preprocess::Options::default());
    let error = generate(request).await.unwrap_err();
    assert!(error.message.contains("CommonJS"), "{}", error.message);
    let response = error.compiler_responses.values().next().unwrap();
    let diagnostic = &response["diagnostics"][0];
    assert_eq!(
        diagnostic["uri"],
        fixture.0.join("Bad.md").to_string_lossy().as_ref()
    );
    let start = diagnostic["bytes"]["start"].as_u64().unwrap() as usize;
    assert!(source[start..].starts_with("import fs"), "{diagnostic}");
    assert_eq!(diagnostic["mapping"], "exact");
    let error = generate(fixture.request(
        Target::Client,
        "broken.svelte.ts",
        "export const broken: = ;",
    ))
    .await
    .unwrap_err();
    assert!(error.compiler_responses.values().any(|r| {
        r["diagnostics"]
            .as_array()
            .is_some_and(|d| d.iter().any(|d| d["phase"] == "parse"))
    }));
}

#[tokio::test]
async fn mapped_svelte_warning_points_into_original_markdown_after_frontmatter() {
    let fixture = Fixture::new();
    let source = "---\ntitle: Warning\n---\n\n# Title\n\n<img src=\"photo.png\">\n";
    let mut request = fixture.request(Target::Server, "Warning.md", source);
    request.mdsvex = Some(ts_svelte_preprocess::Options::default());
    let result = generate(request).await.unwrap();
    let response = result.compiler_responses.values().next().unwrap();
    let warning = response["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["code"] == "a11y_missing_attribute")
        .unwrap();
    let start = warning["bytes"]["start"].as_u64().unwrap() as usize;
    assert!(source[start..].starts_with("<img"), "{warning}");
    assert_eq!(warning["range"]["start"]["line"], 6);
}

#[tokio::test]
async fn extracted_component_css_links_assets_for_client_and_server() {
    let fixture = Fixture::new();
    std::fs::write(fixture.0.join("photo.svg"), "<svg/>").unwrap();
    let source =
        "<p>styled</p><style>p{background:url('./photo.svg?size=2#icon');color:red}</style>";
    for target in [Target::Client, Target::Server] {
        let mut request = fixture.request(target, "Styled.svelte", source);
        request.css = Some(Default::default());
        let result = generate(request).await.unwrap();
        assert_eq!(result.css_mode, "external");
        assert_eq!(result.css.modules.len(), 1);
        let module = result.css.modules.values().next().unwrap();
        assert!(
            result
                .assets
                .iter()
                .any(|a| a.file_name == module.file_name)
        );
        assert_eq!(module.assets.len(), 1);
        assert!(
            result
                .assets
                .iter()
                .any(|a| a.file_name == module.assets[0])
        );
        let entry = result.chunks.iter().find(|c| c.is_entry).unwrap();
        assert!(
            result.css.chunks[&entry.file_name]
                .imports
                .contains(&module.file_name)
        );
        if target == Target::Server {
            assert!(!entry.code.contains("document.createElement"));
        } else {
            assert!(result.chunks.iter().any(|c| c.code.contains("await")));
        }
    }
}
