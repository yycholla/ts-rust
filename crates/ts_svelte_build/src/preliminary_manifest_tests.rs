use super::*;
use ts_svelte_kit::build_analysis::{
    self, AnalysisInput, AnalysisNode, AnalysisPage, AnalysisRoute, ModuleSnapshot,
};

fn chunk(file: &str, id: &Path, code: &str) -> ts_svelte_bundle::Chunk {
    ts_svelte_bundle::Chunk {
        file_name: file.into(),
        name: file.into(),
        code: code.into(),
        is_entry: true,
        is_dynamic_entry: false,
        facade_module_id: Some(path(id).unwrap()),
        module_ids: vec![path(id).unwrap()],
        imports: vec![],
        dynamic_imports: vec![],
        referenced_files: vec![],
        exports: vec![],
        map: None,
        sourcemap_file_name: None,
    }
}
fn fixture(
    root: PathBuf,
    version: &str,
    universal: &str,
) -> (BuildRequest, NativeBuildPlan, AnalysisOutput, BundleResult) {
    let runtime = root.join("node_modules/@sveltejs/kit/src/runtime");
    let generated = root.join(if version.starts_with('3') {
        ".kit/generated/build"
    } else {
        ".kit/generated"
    });
    let page = root.join("src/routes/+page.ts");
    let endpoint = root.join("src/routes/+server.ts");
    let request: BuildRequest = serde_json::from_value(json!({
        "kit": {
            "inventory": {"kitVersion":version,"projectRoot":root,"runtimeDirectory":runtime,"generatedDirectory":generated,
                "options":{"svelteMajor":5,"router":{"type":"pathname","resolution":"client"},"handleRenderingErrors":false,"asyncCompiler":false,"dev":false},
                "nodes":[
                    {"kind":"layout","routeId":"/","depth":0,"component":runtime.join("components/svelte-5/layout.svelte"),"universal":null,"server":null,"parent":null,"childPages":[2]},
                    {"kind":"error","routeId":"/","depth":0,"component":runtime.join("components/svelte-5/error.svelte"),"universal":null,"server":null,"parent":null,"childPages":[]},
                    {"kind":"page","routeId":"/","depth":1,"component":null,"universal":page,"server":null,"parent":0,"childPages":[]}
                ],"routes":[{"id":"/","parentId":null,"params":[],"page":{"leaf":2,"layouts":[0],"errors":[1]},"layout":0,"error":1,"endpoint":endpoint}],
                "hooks":{},"matchers":{},"paramsModule":null,"serviceWorker":null,"inputs":[],"pageOptionsAnalyzed":false},
            "server":{"kitVersion":version,"output":generated,"appTemplate":"%sveltekit.head% %sveltekit.body%","errorTemplate":"%sveltekit.status% %sveltekit.error.message%","versionName":"preliminary-test","runtimeDirectory":runtime,"svelteMajor":5,"serviceWorkerPresent":false,"options":{}},
            "environment":{"kitVersion":version,"environmentDirectory":generated.join("env"),"runtimeDirectory":runtime,"versionName":"preliminary-test","base":"","appDir":"_app","buildValues":{},"publicPrefix":"PUBLIC_","privatePrefix":"","explicitEnvironmentVariables":false,"definition":null},
            "config":{"outDir":root.join(".kit"),"routesDirectory":root.join("src/routes"),"libDirectory":root.join("src/lib"),"assetsDirectory":root.join("static"),"aliases":[],"bundleStrategy":"split","mode":"production","assetsUrl":"","pathsRelative":true,"pathsOrigin":null,"forkPreloads":false,"tracingServer":false,"linkHeaderPreload":false,"versionPollInterval":0,"adapterName":null,"instrumentation":null,"remoteFunctionsEnabled":false,"remoteModules":[],"serverConditions":["svelte","node","production"],"clientConditions":["svelte","browser","production"],"serverExternals":[]},"clientAnalysis":null
        },
        "compiler":{"svelteVersion":if version.starts_with('3') {"5.57.2"} else {"5.53.11"},"typescriptVersion":null,"allowPrerelease":false,"runes":true,"minify":false,"sourcemap":true,"options":{},"tsconfig":null,"mdsvex":null,"typescript":{}},
        "moduleExtensions":[".js",".ts"],"modules":{},"sources":{},"staticAssets":{},"staticAssetsComplete":true
    })).unwrap();
    let plan = ts_svelte_kit::build_plan::create_build_plan(&request.kit).unwrap();
    let analysis = build_analysis::analyze(&AnalysisInput {
        kit_version: version.into(),
        modules: BTreeMap::from([
            (
                path(&page).unwrap(),
                ModuleSnapshot {
                    source: universal.into(),
                    imports: BTreeMap::new(),
                },
            ),
            (
                path(&endpoint).unwrap(),
                ModuleSnapshot {
                    source: "export function GET(){return new Response('ok')}".into(),
                    imports: BTreeMap::new(),
                },
            ),
        ]),
        nodes: vec![
            AnalysisNode::default(),
            AnalysisNode::default(),
            AnalysisNode {
                universal: Some(path(&page).unwrap()),
                server: None,
            },
        ],
        routes: vec![AnalysisRoute {
            id: "/".into(),
            page: Some(AnalysisPage {
                layouts: vec![Some(0)],
                leaf: 2,
            }),
            endpoint: Some(path(&endpoint).unwrap()),
        }],
    })
    .unwrap();
    let mut server = BundleResult {
        rolldown_version: "1.2.13",
        target: ts_svelte_bundle::Target::Server,
        css_mode: "external",
        css: Default::default(),
        module_graph: BTreeMap::new(),
        chunks: vec![
            chunk(
                "entries/layout.js",
                &runtime.join("components/svelte-5/layout.svelte"),
                "export default function Layout(){}",
            ),
            chunk(
                "entries/error.js",
                &runtime.join("components/svelte-5/error.svelte"),
                "export default function Error(){}",
            ),
            chunk("entries/page.js", &page, universal),
            chunk(
                "entries/endpoint.js",
                &endpoint,
                "export function GET(){return new Response('ok')}",
            ),
        ],
        assets: vec![],
        diagnostics: vec![],
        compiler_responses: BTreeMap::new(),
    };
    let map = json!({"version":3,"sources":[path(&page).unwrap()],"sourcesContent":[universal],"names":[],"mappings":"AAAA"});
    server.chunks[2].map = Some(map.clone());
    server.chunks[2].sourcemap_file_name = Some("entries/page.js.map".into());
    for (file, source) in [
        ("entries/page.js.map", serde_json::to_vec(&map).unwrap()),
        ("assets/payload.bin", vec![0, 255, 17]),
    ] {
        server.assets.push(ts_svelte_bundle::Asset {
            file_name: file.into(),
            names: vec![],
            original_file_names: vec![],
            encoding: "base64",
            source: base64::Engine::encode(&base64::engine::general_purpose::STANDARD, source),
        });
    }
    (request, plan, analysis, server)
}
fn text(snapshot: &PreliminaryManifest, file: &str) -> String {
    String::from_utf8(artifacts::bytes(&snapshot.artifacts[file]).unwrap()).unwrap()
}

#[test]
fn preliminary_snapshots_keep_unknown_options_and_actual_emitted_sources() {
    for version in ["2.69.2", "3.0.1"] {
        let (request, plan, analysis, server) = fixture(
            PathBuf::from("/native-preliminary-fixture"),
            version,
            "const determineSsr = () => true; export const ssr = determineSsr(); export async function entries(){return [{}]}",
        );
        assert!(!analysis.requirements.is_empty());
        let before = serde_json::to_value(&server).unwrap();
        let output = preliminary(&request, &plan, &analysis, &server).unwrap();
        assert_eq!(serde_json::to_value(&server).unwrap(), before);
        assert_eq!(output.manifest_path, "server/manifest-full.js");
        assert!(text(&output, &output.manifest_path).contains("client:null"));
        let node = text(&output, "server/nodes/2.js");
        assert!(
            node.contains("import * as universal from \"../entries/page.js\""),
            "{node}"
        );
        assert!(!node.contains("export const universal ="));
        assert!(
            output
                .artifacts
                .keys()
                .all(|file| file.starts_with("server/"))
        );
        assert!(!output.artifacts.contains_key("server/manifest.js"));
        assert!(output.generated_files.contains("server/nodes/2.js"));
        for chunk in &server.chunks {
            let file = format!("server/{}", chunk.file_name);
            assert_eq!(text(&output, &file), chunk.code);
            assert!(!output.generated_files.contains(&file));
        }
        for asset in &server.assets {
            let file = format!("server/{}", asset.file_name);
            assert_eq!(
                artifacts::bytes(&output.artifacts[&file]).unwrap(),
                asset_bytes(asset).unwrap()
            );
            assert!(!output.generated_files.contains(&file));
        }
        assert_eq!(
            output.server_build_id,
            sha(&serde_json::to_vec(&server).unwrap())
        );
    }
}
#[test]
fn preliminary_and_final_artifacts_have_separate_lifetimes() {
    let (request, plan, analysis, server) = fixture(
        PathBuf::from("/native-preliminary-fixture"),
        "2.69.2",
        "export const csr = true;",
    );
    let preliminary = preliminary(&request, &plan, &analysis, &server).unwrap();
    let mut final_artifacts = BTreeMap::new();
    generate(
        &request,
        &plan,
        &analysis,
        &server,
        None,
        &mut final_artifacts,
    )
    .unwrap();
    assert!(final_artifacts.contains_key("server/manifest.js"));
    assert!(!final_artifacts.contains_key("server/manifest-full.js"));
    assert!(
        preliminary
            .artifacts
            .contains_key("server/manifest-full.js")
    );
    assert!(!preliminary.artifacts.contains_key("server/manifest.js"));
    assert!(!final_artifacts.contains_key("server/entries/page.js"));
}
#[test]
fn preliminary_generation_rejects_emitted_path_ownership_collision() {
    let (request, plan, analysis, mut server) = fixture(
        PathBuf::from("/native-preliminary-fixture"),
        "2.69.2",
        "export const csr = true;",
    );
    server.chunks.push(chunk(
        "manifest-full.js",
        Path::new("/fake/module.js"),
        "export const occupied=true;",
    ));
    let before = serde_json::to_value(&server).unwrap();
    let error = preliminary(&request, &plan, &analysis, &server).unwrap_err();
    assert!(error.contains("collides"), "{error}");
    assert_eq!(serde_json::to_value(&server).unwrap(), before);
}
#[cfg(unix)]
#[test]
fn canonical_component_facades_keep_original_inventory_identity() {
    let root = std::env::temp_dir().join(format!(
        "ts-preliminary-canonical-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    struct Cleanup(PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _cleanup = Cleanup(root.clone());
    let real = root.join("real-kit");
    std::fs::create_dir_all(real.join("components/svelte-5")).unwrap();
    for file in ["layout.svelte", "error.svelte"] {
        std::fs::write(real.join("components/svelte-5").join(file), "<slot />").unwrap();
    }
    let alias = root.join("node_modules/@sveltejs/kit/src/runtime");
    std::fs::create_dir_all(alias.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&real, &alias).unwrap();
    let (request, plan, analysis, mut server) = fixture(root, "2.69.2", "export const ssr=true;");
    for chunk in &mut server.chunks[..2] {
        let real = std::fs::canonicalize(chunk.facade_module_id.as_ref().unwrap()).unwrap();
        chunk.facade_module_id = Some(path(&real).unwrap());
        chunk.module_ids = vec![path(&real).unwrap()];
    }
    let output = preliminary(&request, &plan, &analysis, &server).unwrap();
    assert!(text(&output, "server/nodes/0.js").contains("../entries/layout.js"));
    assert!(text(&output, "server/nodes/1.js").contains("../entries/error.js"));
}
