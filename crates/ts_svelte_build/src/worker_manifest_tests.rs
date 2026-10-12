use super::*;
use ts_svelte_bundle::{
    Asset,
    css::{CssChunkOutput, CssModuleOutput, CssOutput},
};
fn chunk(file: &str, facade: Option<&str>) -> Chunk {
    Chunk {
        referenced_files: vec![],
        file_name: file.into(),
        name: "service-worker".into(),
        code: "export {};".into(),
        is_entry: true,
        is_dynamic_entry: false,
        facade_module_id: facade.map(str::to_string),
        module_ids: vec![],
        imports: vec![],
        dynamic_imports: vec![],
        exports: vec![],
        map: None,
        sourcemap_file_name: None,
    }
}
fn asset(file: &str, names: &[&str], original: &[&str]) -> Asset {
    Asset {
        file_name: file.into(),
        names: names.iter().map(|s| (*s).into()).collect(),
        original_file_names: original.iter().map(|s| (*s).into()).collect(),
        encoding: "base64",
        source: String::new(),
    }
}
fn bundle(chunks: Vec<Chunk>, assets: Vec<Asset>) -> BundleResult {
    BundleResult {
        rolldown_version: "1.2.13",
        target: Target::Client,
        css_mode: "extract",
        css: CssOutput::default(),
        module_graph: BTreeMap::new(),
        chunks,
        assets,
        diagnostics: vec![],
        compiler_responses: BTreeMap::new(),
    }
}
fn run(bundle: &BundleResult) -> GeneratedWorkerManifest {
    generate_worker_manifest(
        Path::new("/project"),
        bundle,
        &WorkerManifestOptions::default(),
    )
    .unwrap()
}
#[test]
fn worker_entry_matches_vite_shape_order_and_output_location() {
    let result = run(&bundle(
        vec![chunk(
            "service-worker.js",
            Some("/project/src/service-worker.ts"),
        )],
        vec![],
    ));
    assert_eq!(result.file_name, ".vite/service-worker-manifest.json");
    assert_eq!(
        result.json,
        "{\n  \"src/service-worker.ts\": {\n    \"file\": \"service-worker.js\",\n    \"name\": \"service-worker\",\n    \"src\": \"src/service-worker.ts\",\n    \"isEntry\": true\n  }\n}"
    );
}
#[test]
fn chunk_edges_use_manifest_identities_and_externals_are_omitted() {
    let mut entry = chunk("service-worker.js", Some("/project/src/service-worker.ts"));
    entry.imports = vec!["chunks/shared-HASH.js".into(), "/_app/env.js".into()];
    entry.dynamic_imports = vec!["chunks/lazy-HASH.js".into()];
    let mut shared = chunk("chunks/shared-HASH.js", None);
    shared.is_entry = false;
    let mut lazy = chunk("chunks/lazy-HASH.js", Some("/outside/lazy.ts?worker"));
    lazy.is_entry = false;
    lazy.is_dynamic_entry = true;
    let bundle = bundle(vec![entry, shared, lazy], vec![]);
    let options = WorkerManifestOptions {
        external_imports: BTreeSet::from(["/_app/env.js".into()]),
        ..Default::default()
    };
    let result = generate_worker_manifest(Path::new("/project"), &bundle, &options).unwrap();
    let entry = &result.entries["src/service-worker.ts"];
    assert_eq!(entry.imports, ["_shared-HASH.js"]);
    assert_eq!(entry.dynamic_imports, ["../outside/lazy.ts?worker"]);
    assert!(result.entries["../outside/lazy.ts?worker"].is_dynamic_entry);
    assert_eq!(result.entries["_shared-HASH.js"].src, None);
    assert!(
        result.json.find("../outside/lazy.ts?worker\":").unwrap()
            < result.json.find("_shared-HASH.js\":").unwrap()
    );
}
#[test]
fn named_assets_keep_original_aliases_and_javascript_records_win() {
    let mut bundle = bundle(
        vec![chunk("service-worker.js", Some("/project/src/worker.ts"))],
        vec![
            asset(
                "assets/worker-HASH.css",
                &["worker.css"],
                &["src/worker.ts", "src/worker.css"],
            ),
            asset(
                "assets/icon-HASH.png",
                &["icon.png"],
                &["src/icon.png", "src/duplicate.png"],
            ),
            asset("assets/untraced-HASH.svg", &["icon.svg"], &[]),
            asset("service-worker.js.map", &[], &[]),
        ],
    );
    let options = WorkerManifestOptions {
        css_entries: BTreeMap::from([("assets/worker-HASH.css".into(), "worker-style".into())]),
        ..Default::default()
    };
    let result = generate_worker_manifest(Path::new("/project"), &bundle, &options).unwrap();
    assert_eq!(result.entries["src/worker.ts"].file, "service-worker.js");
    let style = &result.entries["src/worker.css"];
    assert!(style.is_entry);
    assert_eq!(style.name.as_deref(), Some("worker-style"));
    assert_eq!(style.names, Some(vec!["worker.css".into()]));
    // Aliases share the first original identity as src, matching Vite.
    assert_eq!(style.src.as_deref(), Some("src/worker.ts"));
    assert_eq!(
        result.entries["src/duplicate.png"].src.as_deref(),
        Some("src/icon.png")
    );
    assert!(result.entries.contains_key("_untraced-HASH.svg"));
    assert!(!result.entries.keys().any(|s| s.contains(".map")));
    bundle.assets.push(asset(
        "assets/new-HASH.png",
        &["new.png"],
        &["src/icon.png"],
    ));
    assert_eq!(
        run(&bundle).entries["src/icon.png"].file,
        "assets/new-HASH.png"
    );
}
#[test]
fn css_and_asset_dependencies_come_from_real_emitted_metadata() {
    let mut entry = chunk("service-worker.js", Some("/project/src/worker.ts"));
    entry.module_ids = vec!["/project/src/worker.css".into()];
    let mut bundle = bundle(
        vec![entry],
        vec![
            asset("assets/style.css", &["style.css"], &["src/worker.css"]),
            asset("assets/font.woff2", &["font.woff2"], &["src/font.woff2"]),
        ],
    );
    bundle.css.chunks.insert(
        "service-worker.js".into(),
        CssChunkOutput {
            own: vec!["assets/style.css".into()],
            ..Default::default()
        },
    );
    bundle.css.modules.insert(
        "/project/src/worker.css".into(),
        CssModuleOutput {
            tailwind: None,
            module_id: "/project/src/worker.css".into(),
            origin: "/project/src/worker.css".into(),
            file_name: "assets/style.css".into(),
            map_file_name: None,
            sources: vec!["/project/src/worker.css".into()],
            assets: vec!["assets/font.woff2".into()],
            exports: serde_json::Value::Null,
            dependencies: vec![],
        },
    );
    let result = run(&bundle);
    assert_eq!(result.entries["src/worker.ts"].css, ["assets/style.css"]);
    assert_eq!(
        result.entries["src/worker.ts"].assets,
        ["assets/font.woff2"]
    );
    assert_eq!(
        result.entries["src/worker.css"].assets,
        ["assets/font.woff2"]
    );
    bundle.assets.pop();
    assert!(
        generate_worker_manifest(
            Path::new("/project"),
            &bundle,
            &WorkerManifestOptions::default()
        )
        .unwrap_err()
        .contains("not emitted")
    );
}
#[test]
fn native_sorted_keys_and_javascript_compatibility_index_order_are_distinct() {
    let mut bundle = bundle(
        vec![
            chunk("ten.js", Some("/project/10")),
            chunk("two.js", Some("/project/2")),
        ],
        vec![asset("style.css", &["style.css"], &["style.css"])],
    );
    let result = run(&bundle);
    assert!(result.json.find("\"10\":").unwrap() < result.json.find("\"2\":").unwrap());
    bundle.css.chunks.insert(
        "ten.js".into(),
        CssChunkOutput {
            own: vec!["style.css".into()],
            ..Default::default()
        },
    );
    let result = run(&bundle);
    assert!(result.json.find("\"2\":").unwrap() < result.json.find("\"10\":").unwrap());
}
#[test]
fn missing_edges_collisions_invalid_paths_and_virtual_sources_fail_explicitly() {
    let mut entry = chunk("service-worker.js", Some("/project/src/worker.ts"));
    entry.imports.push("chunks/missing.js".into());
    assert!(
        generate_worker_manifest(
            Path::new("/project"),
            &bundle(vec![entry], vec![]),
            &WorkerManifestOptions::default()
        )
        .unwrap_err()
        .contains("missing/unclassified")
    );
    let collision = bundle(
        vec![chunk("one/shared.js", None), chunk("two/shared.js", None)],
        vec![],
    );
    assert!(
        generate_worker_manifest(
            Path::new("/project"),
            &collision,
            &WorkerManifestOptions::default()
        )
        .unwrap_err()
        .contains("identity collision")
    );
    let bad = bundle(
        vec![chunk("../worker.js", Some("/project/src/worker.ts"))],
        vec![],
    );
    assert!(
        generate_worker_manifest(
            Path::new("/project"),
            &bad,
            &WorkerManifestOptions::default()
        )
        .is_err()
    );
    let virtual_bundle = bundle(
        vec![chunk("service-worker.js", Some("\0virtual:worker"))],
        vec![],
    );
    assert!(
        generate_worker_manifest(
            Path::new("/project"),
            &virtual_bundle,
            &WorkerManifestOptions::default()
        )
        .is_err()
    );
    let options = WorkerManifestOptions {
        non_path_facade_keys: BTreeMap::from([(
            "\0virtual:worker".into(),
            "virtual:worker".into(),
        )]),
        ..Default::default()
    };
    assert!(
        generate_worker_manifest(Path::new("/project"), &virtual_bundle, &options)
            .unwrap()
            .entries
            .contains_key("virtual:worker")
    );
}
