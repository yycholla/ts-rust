use super::*;
use serde_json::json;
use ts_svelte_bundle::metadata::Module;

fn fixture(prefix: &str) -> (BundleResult, BTreeMap<String, Artifact>) {
    let mut bundle = BundleResult {
        rolldown_version: "1.2.13",
        target: Target::Server,
        css_mode: "native",
        css: Default::default(),
        module_graph: BTreeMap::new(),
        chunks: vec![],
        assets: vec![],
        diagnostics: vec![],
        compiler_responses: BTreeMap::new(),
    };
    let mut files = BTreeMap::new();
    for (name, source, code, exports) in [
        (
            "index",
            "/app/runtime/server.js",
            "import './chunks/runtime.js'; export const server = {};",
            vec!["server"],
        ),
        (
            "instrumentation.server",
            "/app/src/instrumentation.server.ts",
            "import './chunks/runtime.js'; await setup();",
            vec![],
        ),
        (
            "env",
            "<sveltekit:generated>/env/config.js",
            "export function set_env(value) { env = value }; let env;",
            vec!["set_env"],
        ),
        (
            "chunks/runtime",
            "/app/src/shared.ts",
            "export const shared = 1;",
            vec!["shared"],
        ),
    ] {
        let file = format!("{name}.js");
        let map_file = format!("{file}.map");
        let map = json!({"version":3,"file":file,"sources":[source],"sourcesContent":[code],"names":[],"mappings":"AAAA"});
        let code = format!(
            "{code}\n//# sourceMappingURL={}\n",
            Path::new(&map_file).file_name().unwrap().to_str().unwrap()
        );
        artifacts::insert(
            &mut files,
            path(prefix, &file).unwrap(),
            code.as_bytes(),
            "server",
        )
        .unwrap();
        artifacts::insert(
            &mut files,
            path(prefix, &map_file).unwrap(),
            &serde_json::to_vec(&map).unwrap(),
            "server",
        )
        .unwrap();
        let exports = exports.into_iter().map(str::to_owned).collect::<Vec<_>>();
        let imports = if name == "index" || name == "instrumentation.server" {
            vec!["chunks/runtime.js".into()]
        } else {
            vec![]
        };
        bundle.module_graph.insert(
            source.into(),
            Module {
                id: source.into(),
                is_entry: !name.starts_with("chunks/"),
                is_external: false,
                imports: vec![],
                dynamic_imports: vec![],
                importers: vec![],
                dynamic_importers: vec![],
                exports: exports.clone(),
            },
        );
        bundle.chunks.push(Chunk {
            referenced_files: vec![],
            file_name: file,
            name: name.into(),
            code,
            is_entry: !name.starts_with("chunks/"),
            is_dynamic_entry: false,
            facade_module_id: Some(source.into()),
            module_ids: vec![source.into()],
            imports,
            dynamic_imports: vec![],
            exports,
            map: Some(map),
            sourcemap_file_name: Some(map_file),
        });
    }
    (bundle, files)
}

#[test]
fn validates_actual_server_identity_and_static_dynamic_dependencies() {
    let (mut bundle, files) = fixture("server");
    bundle.chunks[1].dynamic_imports.push("env.js".into());
    let receipt = validate_entry(
        "2.69.2",
        Path::new("/app/src/instrumentation.server.ts"),
        Some(true),
        None,
        &bundle,
        "server",
        &files,
    )
    .unwrap();
    assert_eq!(
        receipt.dependencies,
        vec!["chunks/runtime.js", "env.js", "instrumentation.server.js"]
    );
    assert!(!receipt.environment_initializer_required);
    assert!(
        validate_entry(
            "2.69.2",
            Path::new("/app/src/different.ts"),
            Some(true),
            None,
            &bundle,
            "server",
            &files
        )
        .is_err()
    );
    bundle.chunks[1].dynamic_imports.push("missing.js".into());
    assert!(
        validate_entry(
            "2.69.2",
            Path::new("/app/src/instrumentation.server.ts"),
            Some(true),
            None,
            &bundle,
            "server",
            &files
        )
        .unwrap_err()
        .contains("Unresolved emitted")
    );
}

#[test]
fn validates_external_graph_identity_and_stale_artifacts() {
    let (mut bundle, mut files) = fixture("server");
    bundle.chunks[1].imports.push("node:async_hooks".into());
    assert!(closure(&bundle, "instrumentation.server.js", "server", &files).is_err());
    bundle.module_graph.insert(
        "node:async_hooks".into(),
        Module {
            id: "node:async_hooks".into(),
            is_entry: false,
            is_external: true,
            imports: vec![],
            dynamic_imports: vec![],
            importers: vec![],
            dynamic_importers: vec![],
            exports: vec![],
        },
    );
    assert_eq!(
        closure(&bundle, "instrumentation.server.js", "server", &files)
            .unwrap()
            .1,
        vec!["node:async_hooks"]
    );
    files
        .get_mut("server/chunks/runtime.js")
        .unwrap()
        .source
        .clear();
    assert!(
        closure(&bundle, "instrumentation.server.js", "server", &files)
            .unwrap_err()
            .contains("Stale")
    );
}

#[test]
fn kit2_wrapper_defers_application_and_preserves_map_bytes_and_identity() {
    let (bundle, files) = fixture("build/server");
    let output = instrument_bundle_entry(
        "2.69.2",
        &bundle,
        &files,
        "build/server",
        "index.js",
        "instrumentation.server.js",
        None,
    )
    .unwrap();
    let code =
        String::from_utf8(bytes(&output.artifacts, "build/server/index.js").unwrap()).unwrap();
    assert_eq!(
        code,
        "import \"./instrumentation.server.js\";\nconst { server } = await import(\"./start.js\");\nexport { server };"
    );
    assert_eq!(
        bytes(&output.artifacts, "build/server/start.js").unwrap(),
        bytes(&files, "build/server/index.js").unwrap()
    );
    for name in ["build/server/start.js.map", "build/server/index.js.map"] {
        assert_eq!(
            bytes(&output.artifacts, name).unwrap(),
            bytes(&files, "build/server/index.js.map").unwrap()
        );
        assert_eq!(
            output.identities[name].source_map_for.as_deref(),
            Some("build/server/start.js")
        );
    }
    assert!(output.identities["build/server/index.js"].generated);
    assert_eq!(
        output.identities["build/server/start.js"]
            .original_module
            .as_deref(),
        Some("/app/runtime/server.js")
    );
    assert_eq!(files.len() + 2, output.artifacts.len());
}

#[test]
fn kit3_uses_real_environment_entry_before_instrumentation_and_dynamic_start() {
    let (bundle, files) = fixture("deploy/server");
    for version in ["3.0.1", "3.0.0-next.32"] {
        let output = instrument_bundle_entry(
            version,
            &bundle,
            &files,
            "deploy/server",
            "index.js",
            "instrumentation.server.js",
            Some(InstrumentationEnvironment::ProcessEnv),
        )
        .unwrap();
        assert_eq!(
            output.static_preludes,
            vec!["./__sveltekit_env_init.js", "./instrumentation.server.js"]
        );
        let initializer = String::from_utf8(
            bytes(&output.artifacts, "deploy/server/__sveltekit_env_init.js").unwrap(),
        )
        .unwrap();
        assert_eq!(
            initializer,
            "import env from \"./__sveltekit_env.js\";\nimport { set_env } from \"./env.js\";\nset_env(env);\n"
        );
        assert_eq!(
            bytes(&output.artifacts, "deploy/server/__sveltekit_env.js").unwrap(),
            b"export default process.env;"
        );
        let wrapper =
            String::from_utf8(bytes(&output.artifacts, "deploy/server/index.js").unwrap()).unwrap();
        assert!(wrapper.contains("const __mod = await import(\"./start.js\")"));
        assert!(
            validate_adapter_node_version(version)
                .unwrap_err()
                .contains("5.5.4")
        );
    }
}

#[test]
fn kit3_rejects_missing_env_export_and_initializer_cycles_without_mutation() {
    let (mut bundle, files) = fixture("build/server");
    bundle.chunks[2].exports.clear();
    let error = instrument_bundle_entry(
        "3.0.1",
        &bundle,
        &files,
        "build/server",
        "index.js",
        "instrumentation.server.js",
        Some(InstrumentationEnvironment::ProcessEnv),
    )
    .unwrap_err();
    assert!(error.contains("set_env"));
    bundle.chunks[2].exports.push("set_env".into());
    bundle.chunks[2]
        .imports
        .push("instrumentation.server.js".into());
    assert!(
        instrument_bundle_entry(
            "3.0.1",
            &bundle,
            &files,
            "build/server",
            "index.js",
            "instrumentation.server.js",
            Some(InstrumentationEnvironment::ProcessEnv)
        )
        .unwrap_err()
        .contains("cannot run first")
    );
    assert!(!files.contains_key("build/server/start.js"));
}

#[test]
fn adapter_node_finalization_authenticates_existing_wrapper_and_actual_rebundle() {
    let (server_bundle, server_files) = fixture("server");
    let receipt = validate_entry(
        "2.69.2",
        Path::new("/app/src/instrumentation.server.ts"),
        Some(true),
        Some(InstrumentationAdapter {
            name: "@sveltejs/adapter-node".into(),
            supports_instrumentation: Some(true),
        }),
        &server_bundle,
        "server",
        &server_files,
    )
    .unwrap();
    let (mut bundle, mut files) = fixture("build/server");
    bundle.chunks[1].facade_module_id =
        Some("/app/.kit/native-adapter-node/instrumentation.server.js".into());
    let facade = b"import './server/instrumentation.server.js';\nconst { path, host, port, server } = await import('./start.js');\nexport { path, host, port, server };";
    artifacts::insert(&mut files, "build/index.js".into(), facade, "adapter-node").unwrap();
    artifacts::insert(
        &mut files,
        "build/start.js".into(),
        b"import './handler.js'; export const path='',host='',port=0,server={};",
        "adapter-node",
    )
    .unwrap();
    let output = finalize_adapter_node(
        "2.69.2",
        "build",
        ".kit/native-adapter-node",
        &receipt,
        &bundle,
        &files,
    )
    .unwrap();
    assert_eq!(
        output.static_preludes,
        vec!["./server/instrumentation.server.js"]
    );
    assert_eq!(
        bytes(&output.artifacts, "build/start.js").unwrap(),
        bytes(&files, "build/start.js").unwrap()
    );
    assert_eq!(output.artifacts.len(), files.len());
    bundle.chunks[1].facade_module_id = Some("/app/unrelated.js".into());
    assert!(
        finalize_adapter_node(
            "2.69.2",
            "build",
            ".kit/native-adapter-node",
            &receipt,
            &bundle,
            &files
        )
        .unwrap_err()
        .contains("staging entry")
    );
}
