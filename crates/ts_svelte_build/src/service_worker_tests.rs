//! Actual native worker bundling with imported binary resources. No worker JS
//! or application/configuration module is executed by these fixtures.
use super::*;
use std::sync::atomic::{AtomicU64, Ordering};

struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "ts-native-worker-assets-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(path.join("src/nested")).unwrap();
        Self(path)
    }
    fn write(&self, file: &str, bytes: &[u8]) {
        let path = self.0.join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn fixture(
    directory: &Directory,
    version: &str,
    source: &str,
) -> (
    BuildRequest,
    NativeBuildPlan,
    ServiceWorkerRequest,
    BundleResult,
) {
    let (mut input, _, request, client) = tests::fixture_at_root(source, directory.0.clone());
    input.kit.environment.base = "/tenant".into();
    if version.starts_with('3') {
        input.kit.inventory.kit_version = version.into();
        input.kit.server.kit_version = version.into();
        input.kit.environment.kit_version = version.into();
        input.compiler.svelte_version = "5.57.2".into();
        input.compiler.allow_prerelease = version.contains('-');
        let generated = input.kit.config.out_dir.join("generated/build");
        input.kit.inventory.generated_directory = generated.clone();
        input.kit.server.output = generated.clone();
        input.kit.environment.environment_directory = generated.join("env");
    }
    let plan = build_plan::create_build_plan(&input.kit).unwrap();
    (input, plan, request, client)
}
fn resources(output: &ServiceWorkerOutput) -> Vec<&ts_svelte_bundle::Asset> {
    output
        .bundle
        .assets
        .iter()
        .filter(|asset| {
            !output
                .bundle
                .chunks
                .iter()
                .any(|chunk| chunk.sourcemap_file_name.as_deref() == Some(asset.file_name.as_str()))
        })
        .collect()
}
fn verify_maps(output: &ServiceWorkerOutput) {
    for chunk in &output.bundle.chunks {
        let Some(file) = chunk.sourcemap_file_name.as_ref() else {
            // Rolldown may synthesize a pure facade without any rendered source
            // modules. Its imports/reexports have no original locations. Require
            // both structural and graph evidence rather than accepting missing
            // maps on application code or generated asset expressions.
            let arena = oxc_allocator::Allocator::default();
            let parsed =
                oxc_parser::Parser::new(&arena, &chunk.code, oxc_span::SourceType::mjs()).parse();
            let facade = parsed.diagnostics.is_empty()
                && parsed.program.body.iter().all(|statement| {
                    use oxc_ast::ast::Statement;
                    match statement {
                        Statement::ImportDeclaration(_)
                        | Statement::ExportAllDeclaration(_)
                        | Statement::EmptyStatement(_) => true,
                        Statement::ExportNamedDeclaration(_)
                        | Statement::ExportFromDeclaration(_) => true,
                        _ => false,
                    }
                });
            let generated_runtime = !chunk.module_ids.is_empty()
                && chunk
                    .module_ids
                    .iter()
                    .all(|id| id == "\0rolldown/runtime.js");
            assert!(
                (chunk.module_ids.is_empty() && facade) || generated_runtime,
                "Missing requested source map on non-facade chunk {} (modules {:?}): {}",
                chunk.file_name,
                chunk.module_ids,
                chunk.code
            );
            assert!(
                chunk.map.is_none(),
                "Facade cannot retain a map without its map artifact"
            );
            assert!(!chunk.code.contains("sourceMappingURL="));
            continue;
        };
        let map: Value = serde_json::from_slice(
            &artifacts::bytes(&output.artifacts[&format!("client/{file}")]).unwrap(),
        )
        .unwrap();
        assert_eq!(chunk.map.as_ref(), Some(&map));
        assert_eq!(map["version"], 3);
        assert!(map["sources"].is_array());
        assert!(map["sourcesContent"].is_array());
        assert!(
            map["mappings"]
                .as_str()
                .is_some_and(|mappings| !mappings.is_empty())
        );
    }
}

#[tokio::test]
async fn kit2_worker_imported_asset_uses_worker_location_and_real_output_bytes() {
    let directory = Directory::new();
    let asset_bytes = b"<svg xmlns='http://www.w3.org/2000/svg'><path id='native'/></svg>";
    directory.write("src/payload.svg", asset_bytes);
    let (input, plan, request, client) = fixture(
        &directory,
        "2.69.2",
        "import resource from './payload.svg'; globalThis.workerResource=resource; __SVELTEKIT_TRACK__('fixture:worker-asset');",
    );
    let output = build(&input, &plan, &request, &client, &BTreeMap::new())
        .await
        .unwrap();
    assert_eq!(output.status, "complete");
    let assets = resources(&output);
    assert_eq!(assets.len(), 1);
    let asset = assets[0];
    assert!(asset.file_name.starts_with("_app/immutable/assets/"));
    assert!(asset.file_name.ends_with(".svg"));
    assert_eq!(crate::asset_bytes(asset).unwrap(), asset_bytes);
    let artifact = &output.artifacts[&format!("client/{}", asset.file_name)];
    assert_eq!(artifact.kind, "service-worker-asset");
    assert_eq!(artifacts::bytes(artifact).unwrap(), asset_bytes);
    let code = &output.bundle.chunks[0].code;
    assert!(code.contains(&asset.file_name));
    assert!(code.contains("location.href") && code.contains(".pathname"));
    assert!(!code.contains("import.meta.url"));
    assert!(
        !code.contains("/tenant/"),
        "Worker assets derive the deployment base at runtime"
    );
    assert!(!code.contains("__SVELTEKIT_TRACK__"));
    assert_eq!(
        output.tracked_features["service-worker.js"],
        ["fixture:worker-asset"]
    );
    assert!(!output.artifacts.contains_key("client/.vite/manifest.json"));
    verify_maps(&output);
}

#[tokio::test]
async fn kit3_split_worker_assets_are_relative_to_worker_and_listed_in_own_manifest() {
    let directory = Directory::new();
    directory.write("src/nested/runtime.wasm", b"\0asm\x01\0\0\0");
    let (mut input, _, mut request, client) = fixture(
        &directory,
        "3.0.1",
        "import('./nested/lazy.ts').then(value=>globalThis.workerWasm=value.resource);",
    );
    input.sources.insert(
        directory.0.join("src/nested/lazy.ts"),
        "import resource from './runtime.wasm'; export { resource };".into(),
    );
    request.code_splitting = Some(true);
    let plan = build_plan::create_build_plan(&input.kit).unwrap();
    let mut previous = BTreeMap::new();
    artifacts::insert(
        &mut previous,
        "client/.vite/manifest.json".into(),
        b"{\"mainClient\":true}",
        "client-manifest",
    )
    .unwrap();
    let output = build(&input, &plan, &request, &client, &previous)
        .await
        .unwrap();
    assert_eq!(output.status, "complete");
    assert!(output.bundle.chunks.len() >= 2);
    assert!(!output.artifacts.contains_key("client/.vite/manifest.json"));
    assert_eq!(
        artifacts::bytes(&previous["client/.vite/manifest.json"]).unwrap(),
        b"{\"mainClient\":true}"
    );
    let assets = resources(&output);
    assert_eq!(assets.len(), 1);
    let asset = assets[0];
    let owner = output
        .bundle
        .chunks
        .iter()
        .find(|chunk| chunk.code.contains(&asset.file_name))
        .expect("Emitted worker asset must have an actual JS reference");
    assert_ne!(owner.file_name, "service-worker.js");
    assert!(
        owner.map.is_some(),
        "The chunk containing the imported asset URL must preserve its source map"
    );
    assert!(
        output
            .bundle
            .chunks
            .iter()
            .any(|chunk| chunk.code.contains("workerWasm") && chunk.map.is_some()),
        "Actual worker application code must remain mapped"
    );
    assert!(owner.code.contains("location.href") && owner.code.contains(".pathname"));
    assert!(
        !owner.code.contains(&format!("../{}", asset.file_name)),
        "URL is relative to worker location, not nested chunk location"
    );
    let manifest: Value = serde_json::from_slice(
        &artifacts::bytes(&output.artifacts["client/.vite/service-worker-manifest.json"]).unwrap(),
    )
    .unwrap();
    let record = manifest
        .as_object()
        .unwrap()
        .values()
        .find(|record| record["file"] == owner.file_name)
        .unwrap();
    assert!(
        record["assets"]
            .as_array()
            .unwrap()
            .iter()
            .any(|file| file == &asset.file_name)
    );
    assert!(
        manifest
            .as_object()
            .unwrap()
            .values()
            .any(|record| record["file"] == asset.file_name)
    );
    verify_maps(&output);
}

#[tokio::test]
async fn worker_asset_duplicates_are_omitted_and_conflicts_fail_without_mutating_client() {
    let directory = Directory::new();
    directory.write("src/payload.svg", b"<svg/>");
    let (input, plan, request, client) = fixture(
        &directory,
        "2.69.2",
        "import resource from './payload.svg'; globalThis.workerResource=resource;",
    );
    let first = build(&input, &plan, &request, &client, &BTreeMap::new())
        .await
        .unwrap();
    let asset_name = format!("client/{}", resources(&first)[0].file_name);
    let existing = first.artifacts[&asset_name].clone();
    let mut previous = BTreeMap::from([(asset_name.clone(), existing.clone())]);
    let repeat = build(&input, &plan, &request, &client, &previous)
        .await
        .unwrap();
    assert_eq!(repeat.status, "complete");
    assert!(!repeat.artifacts.contains_key(&asset_name));
    assert_eq!(previous[&asset_name].source, existing.source);
    previous.remove(&asset_name);
    artifacts::insert(
        &mut previous,
        asset_name.clone(),
        b"different bytes",
        "client-asset",
    )
    .unwrap();
    let error = build(&input, &plan, &request, &client, &previous)
        .await
        .unwrap_err();
    assert!(
        error.contains("conflicts with existing client artifact"),
        "{error}"
    );
    assert_eq!(
        artifacts::bytes(&previous[&asset_name]).unwrap(),
        b"different bytes"
    );
    assert_eq!(previous.len(), 1);
}

#[tokio::test]
async fn worker_asset_query_and_fragment_stay_inside_url_before_pathname() {
    let directory = Directory::new();
    directory.write("src/payload.svg", b"<svg/>");
    let (input, plan, request, client) = fixture(
        &directory,
        "2.69.2",
        "import resource from './payload.svg?cache=1#symbol'; globalThis.workerResource=resource;",
    );
    let output = build(&input, &plan, &request, &client, &BTreeMap::new())
        .await
        .unwrap();
    assert_eq!(output.status, "complete");
    let assets = resources(&output);
    assert_eq!(assets.len(), 1);
    let asset = assets[0];
    assert!(!asset.file_name.contains(['?', '#']));
    let code = &output.bundle.chunks[0].code;
    let literal = serde_json::to_string(&format!("{}?cache=1#symbol", asset.file_name)).unwrap();
    assert!(
        code.contains(&format!("new URL({literal}, location.href).pathname")),
        "Worker asset suffix belongs inside the URL argument: {code}"
    );
    assert_eq!(
        output.bundle.chunks[0].referenced_files,
        [asset.file_name.clone()]
    );
    verify_maps(&output);
}
