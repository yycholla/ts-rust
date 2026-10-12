use super::*;
use crate::manifest::PreliminaryManifest;
use serde_json::json;
fn request(version: &str) -> BuildRequest {
    let root = PathBuf::from("/native project/é😀");
    let runtime = root.join("node_modules/@sveltejs/kit/src/runtime");
    let generated = root.join(".kit/generated");
    serde_json::from_value(json!({
        "kit":{
            "inventory":{"kitVersion":version,"projectRoot":root,"runtimeDirectory":runtime,"generatedDirectory":generated,"options":{"svelteMajor":5,"router":{"type":"pathname","resolution":"client"},"handleRenderingErrors":false,"asyncCompiler":false,"dev":false},"nodes":[],"routes":[],"hooks":{},"matchers":{},"paramsModule":null,"serviceWorker":null,"inputs":[],"pageOptionsAnalyzed":false},
            "server":{"kitVersion":version,"output":generated,"appTemplate":"%sveltekit.head% %sveltekit.body%","errorTemplate":"%sveltekit.status% %sveltekit.error.message%","versionName":"snapshot","runtimeDirectory":runtime,"svelteMajor":5,"serviceWorkerPresent":false,"options":{}},
            "environment":{"kitVersion":version,"environmentDirectory":generated.join("env"),"runtimeDirectory":runtime,"versionName":"snapshot","base":"","appDir":"_app","buildValues":{"PUBLIC_LABEL":"visible","SECRET_LABEL":"explicit fixture value"},"publicPrefix":"PUBLIC_","privatePrefix":"SECRET_","explicitEnvironmentVariables":false,"definition":null},
            "config":{"outDir":root.join(".kit"),"routesDirectory":root.join("src/routes"),"libDirectory":root.join("src/lib"),"assetsDirectory":root.join("static"),"aliases":[],"bundleStrategy":"split","mode":"production","assetsUrl":"","pathsRelative":true,"pathsOrigin":null,"forkPreloads":false,"tracingServer":false,"linkHeaderPreload":false,"versionPollInterval":0,"adapterName":null,"instrumentation":null,"remoteFunctionsEnabled":false,"remoteModules":[],"serverConditions":["svelte","node","production"],"clientConditions":["svelte","browser","production"],"serverExternals":[]},"clientAnalysis":null},
        "compiler":{"svelteVersion":"5.57.2","typescriptVersion":null,"allowPrerelease":false,"runes":true,"minify":false,"sourcemap":true,"options":{},"tsconfig":null,"mdsvex":null,"typescript":{}},
        "moduleExtensions":[".js"],"modules":{},"sources":{},"staticAssets":{},"staticAssetsComplete":true
    })).unwrap()
}
fn add(snapshot: &mut PreliminaryManifest, file: &str, bytes: &[u8], kind: &str) {
    artifacts::insert(&mut snapshot.artifacts, file.into(), bytes, kind).unwrap();
}
fn fixture() -> PreliminaryManifest {
    let mut snapshot = PreliminaryManifest {
        manifest_path: "server/manifest-full.js".into(),
        expression: "unused data".into(),
        server_build_id: "a".repeat(64),
        artifacts: BTreeMap::new(),
        generated_files: BTreeSet::from(["server/manifest-full.js".into()]),
    };
    for (name, source) in [
        (
            "server/manifest-full.js",
            "export const manifest={nodes:[]};",
        ),
        ("server/internal.js", "export function set_building(){}"),
        ("server/env.js", "export function set_env(){}"),
        ("server/index.js", "export async function configure(){}"),
    ] {
        let kind = if snapshot.generated_files.contains(name) {
            "serverManifest"
        } else {
            "server"
        };
        add(&mut snapshot, name, source.as_bytes(), kind);
    }
    snapshot
}
fn id(request: &BuildRequest, file: &str) -> String {
    file_url(&request.kit.config.out_dir.join("output").join(file))
        .unwrap()
        .into()
}
#[test]
fn bootstrap_profiles_include_only_explicit_environment_and_real_modules() {
    for version in ["2.69.2", "3.0.1", "3.0.0-next.32"] {
        let request = request(version);
        let mut preliminary = fixture();
        add(
            &mut preliminary,
            "server/asset.wasm",
            &[0, 97, 115, 109, 255],
            "server-asset",
        );
        add(&mut preliminary, "server/raw.js", &[255, 0], "server-asset");
        let prepared = prepare(&request, &preliminary).unwrap();
        assert!(prepared.requirements.is_empty());
        let bootstrap = prepared.bootstrap_json().unwrap();
        assert_eq!(bootstrap["serverBuildId"], "a".repeat(64));
        assert_eq!(
            bootstrap["manifestEntry"],
            id(&request, "server/manifest-full.js")
        );
        assert_eq!(
            bootstrap["environment"],
            json!(request.kit.environment.build_values)
        );
        assert_eq!(
            bootstrap["assets"]["asset.wasm"],
            json!([0, 97, 115, 109, 255])
        );
        assert_eq!(bootstrap["assets"]["raw.js"], json!([255, 0]));
        assert_eq!(prepared.bootstrap.snapshot.modules.len(), 4);
        if version.starts_with('2') {
            assert_eq!(
                bootstrap["initialization"],
                json!({"profile":"kit2","internal":id(&request,"server/internal.js"),"environment":id(&request,"server/env.js"),"publicPrefix":"PUBLIC_","privatePrefix":"SECRET_"})
            );
        } else {
            assert_eq!(
                bootstrap["initialization"],
                json!({"profile":"kit3","server":id(&request,"server/index.js")})
            );
        }
        assert!(
            prepared
                .bootstrap
                .snapshot
                .modules
                .keys()
                .all(|key| key.contains("native%20project/%C3%A9%F0%9F%98%80/"))
        );
    }
}
#[test]
fn nested_imports_reexports_builtin_aliases_and_exact_literal_decoding() {
    let request = request("2.69.2");
    let mut preliminary = fixture();
    add(
        &mut preliminary,
        "server/nodes/a.js",
        r#"import {AsyncLocalStorage} from 'node:async_hooks';
        export {default as data} from '../chunks/\u00e9.js';
        export * from '../chunks/é.js';
        export function nested(){return import(`../chunks/é.js`)}
        export class Loader{async load(){return import('async_hooks')}}"#
            .as_bytes(),
        "server",
    );
    add(
        &mut preliminary,
        "server/chunks/é.js",
        b"export default 1;",
        "server",
    );
    let prepared = prepare(&request, &preliminary).unwrap();
    let edges = &prepared.bootstrap.snapshot.resolutions[&id(&request, "server/nodes/a.js")];
    assert_eq!(edges.len(), 3);
    assert_eq!(
        edges["../chunks/é.js"],
        Resolution::Module {
            id: id(&request, "server/chunks/é.js")
        }
    );
    assert_eq!(
        edges["node:async_hooks"],
        Resolution::Builtin {
            name: "node:async_hooks".into()
        }
    );
    assert_eq!(edges["async_hooks"], edges["node:async_hooks"]);
    assert!(prepared.requirements.is_empty());
}
#[test]
fn query_fragment_instances_preserve_source_and_resolve_descendants() {
    let request = request("3.0.1");
    let mut preliminary = fixture();
    add(&mut preliminary,"server/entry.js",b"import './a%20b.js?one#fragment'; import './a b.js?two'; import './%61%20b.js?one#fragment';","server");
    add(
        &mut preliminary,
        "server/a b.js",
        b"export const child=()=>import('./child.js');",
        "server",
    );
    add(
        &mut preliminary,
        "server/child.js",
        b"export default 2;",
        "server",
    );
    let prepared = prepare(&request, &preliminary).unwrap();
    let base = id(&request, "server/a b.js");
    for suffix in ["", "?one#fragment", "?two"] {
        let identity = format!("{base}{suffix}");
        let module = &prepared.bootstrap.snapshot.modules[&identity];
        assert_eq!(module.import_meta_url, identity);
        assert_eq!(
            module.source,
            "export const child=()=>import('./child.js');"
        );
        assert_eq!(
            prepared.bootstrap.snapshot.resolutions[&identity]["./child.js"],
            Resolution::Module {
                id: id(&request, "server/child.js")
            }
        );
    }
    let edges = &prepared.bootstrap.snapshot.resolutions[&id(&request, "server/entry.js")];
    assert_eq!(
        edges["./a%20b.js?one#fragment"],
        edges["./%61%20b.js?one#fragment"]
    );
}
#[test]
fn unsupported_imports_are_explicit_without_invented_resolutions() {
    let request = request("2.69.2");
    let mut preliminary = fixture();
    add(&mut preliminary,"server/unknown.js",b"import 'node:fs'; export async function load(value){await import('package');return import(value)}","server");
    let prepared = prepare(&request, &preliminary).unwrap();
    let edges = &prepared.bootstrap.snapshot.resolutions[&id(&request, "server/unknown.js")];
    assert_eq!(
        edges["node:fs"],
        Resolution::External {
            name: "node:fs".into()
        }
    );
    assert_eq!(
        edges["package"],
        Resolution::External {
            name: "package".into()
        }
    );
    assert_eq!(edges.len(), 2);
    assert_eq!(
        prepared
            .requirements
            .iter()
            .filter(|item| item.code == "externalModule")
            .count(),
        2
    );
    assert_eq!(
        prepared
            .requirements
            .iter()
            .filter(|item| item.code == "dynamicSpecifier")
            .count(),
        1
    );
    assert!(
        prepared
            .requirements
            .iter()
            .all(|item| item.end > item.start)
    );
}
#[test]
fn missing_relative_edges_do_not_become_external_imports() {
    for specifier in [
        "./missing.js",
        "./child",
        "../outside.js",
        "./dir%2fchild.js",
        "./bad%zz.js",
    ] {
        let request = request("2.69.2");
        let mut preliminary = fixture();
        add(
            &mut preliminary,
            "server/edge.js",
            format!("import {};", serde_json::to_string(specifier).unwrap()).as_bytes(),
            "server",
        );
        assert!(prepare(&request, &preliminary).is_err(), "{specifier}");
    }
}
#[test]
fn file_url_imports_and_literal_percent_filenames_have_exact_identity() {
    let request = request("3.0.1");
    let mut preliminary = fixture();
    add(
        &mut preliminary,
        "server/a%20b.js",
        b"export const name='literal percent';",
        "server",
    );
    add(
        &mut preliminary,
        "server/a b.js",
        b"export const name='space';",
        "server",
    );
    let target = id(&request, "server/a%20b.js");
    add(
        &mut preliminary,
        "server/entry.js",
        format!(
            "export * from {}; import './a%20b.js';",
            serde_json::to_string(&target).unwrap()
        )
        .as_bytes(),
        "server",
    );
    let prepared = prepare(&request, &preliminary).unwrap();
    let edges = &prepared.bootstrap.snapshot.resolutions[&id(&request, "server/entry.js")];
    assert_eq!(edges[&target], Resolution::Module { id: target.clone() });
    assert_ne!(edges[&target], edges["./a%20b.js"]);
}
#[test]
fn stale_owned_files_invalid_utf8_and_path_collisions_fail() {
    let request = request("2.69.2");
    let mut preliminary = fixture();
    preliminary
        .generated_files
        .insert("server/not-emitted.js".into());
    assert!(
        prepare(&request, &preliminary)
            .unwrap_err()
            .contains("Missing generated")
    );
    preliminary.generated_files.remove("server/not-emitted.js");
    add(&mut preliminary, "server/invalid.js", &[255], "server");
    assert!(
        prepare(&request, &preliminary)
            .unwrap_err()
            .contains("UTF-8")
    );
    preliminary.artifacts.remove("server/invalid.js");
    add(
        &mut preliminary,
        "server/collision",
        b"file",
        "server-asset",
    );
    add(
        &mut preliminary,
        "server/collision/child.js",
        b"export {};",
        "server",
    );
    assert!(
        prepare(&request, &preliminary)
            .unwrap_err()
            .contains("file/directory collision")
    );
    preliminary.artifacts.remove("server/collision");
    preliminary.artifacts.remove("server/env.js");
    assert!(
        prepare(&request, &preliminary)
            .unwrap_err()
            .contains("initialization module")
    );
}
#[test]
fn attributes_and_computed_templates_remain_reported() {
    let request = request("2.69.2");
    let mut preliminary = fixture();
    add(&mut preliminary,"server/options.js",b"import './child.js' with {type:'json'}; export const load=(name)=>import(`./${name}.js`);","server");
    add(&mut preliminary, "server/child.js", b"export {};", "server");
    let prepared = prepare(&request, &preliminary).unwrap();
    assert!(
        prepared
            .requirements
            .iter()
            .any(|item| item.code == "importAttributes")
    );
    assert!(
        prepared
            .requirements
            .iter()
            .any(|item| item.code == "dynamicSpecifier")
    );
}
