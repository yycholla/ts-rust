use super::*;
fn server_input(version: &str, source: &str) -> RemoteTransformInput {
    RemoteTransformInput {
        discovery: RemoteDiscoveryInput {
            kit_version: version.into(),
            module_id: "/project/src/api.remote.ts".into(),
            module_extensions: vec![".js".into(), ".ts".into()],
            package_has_kit_peer: None,
        },
        project_root: "/project".into(),
        enabled: true,
        development: false,
        target: RemoteTarget::Server,
        artifact_name: "api.remote.js".into(),
        artifact: JavaScriptArtifact {
            code: source.into(),
            map: None,
            map_file: None,
            pending: vec![],
            receipt_sha256: None,
        },
        source_maps: SourceMapPolicy::Preserve,
        client_runtime_id: Some(
            "/project/node_modules/@sveltejs/kit/src/runtime/client/remote-functions/index.js"
                .into(),
        ),
        metadata: None,
    }
}
fn client(version: &str, source: &str, exports: &[(&str, RemoteKind)]) -> RemoteTransformInput {
    let mut input = server_input(version, source);
    input.target = RemoteTarget::Client;
    let file = "src/api.remote.ts".to_owned();
    input.metadata = Some(EvaluatedRemoteExports {
        hash: remote_hash(&file),
        file,
        exports: exports
            .iter()
            .map(|(name, kind)| RemoteExport {
                name: (*name).into(),
                kind: *kind,
            })
            .collect(),
    });
    input
}
#[test]
fn server_profiles_preserve_code_and_emit_actual_namespace_entries() {
    for version in [
        "2.67.0",
        "2.68.0",
        "2.69.2",
        "2.69.3",
        "2.70.3",
        "3.0.0-next.32",
        "3.0.1",
    ] {
        let input = server_input(
            version,
            "import { query } from '$app/server';\nexport const get = query(() => 1) // no semicolon",
        );
        let out = transform_remote(&input).unwrap();
        assert!(out.artifact.code.starts_with(&input.artifact.code));
        assert!(
            out.artifact
                .code
                .contains("\n\nimport * as $$_self_$$ from \"./api.remote.ts\";")
        );
        let internal = if version.starts_with('3') {
            "@sveltejs/kit/internal/server"
        } else {
            "@sveltejs/kit/internal"
        };
        assert!(
            out.artifact
                .code
                .contains(&format!("from {}", quoted(internal)))
        );
        assert!(out.artifact.code.contains("fn.__.name = name"));
        assert_eq!(out.identity.file, "src/api.remote.ts");
        assert_eq!(out.explicit_exports, vec!["get"]);
        let entry = out.entry.unwrap();
        assert_eq!(
            entry.id,
            format!("\0sveltekit-remote:{}", out.identity.hash)
        );
        assert_eq!(entry.name, format!("remote-{}", out.identity.hash));
        assert_eq!(
            entry.code,
            "import * as m from \"/project/src/api.remote.ts\";\nexport default m;"
        );
        assert!(!out.artifact.code.contains("await Promise.resolve"));
    }
}
#[test]
fn dev_microtask_and_capture_avoidance_use_real_imports() {
    let mut input = server_input(
        "3.0.1",
        "const $$_self_$$=1; const $$_init_$$=2; export {value} from './other.js'; // export default ignored",
    );
    input.development = true;
    let out = transform_remote(&input).unwrap();
    assert!(out.artifact.code.contains("import * as $$_self_$$1"));
    assert!(
        out.artifact
            .code
            .contains("init_remote_functions as $$_init_$$1")
    );
    assert!(out.artifact.code.contains("await Promise.resolve();"));
    assert!(out.entry.is_none());
    assert_eq!(out.explicit_exports, vec!["value"]);
}
#[test]
fn client_metadata_drives_all_six_remote_kinds_and_no_server_import_survives() {
    let exports = [
        ("a", RemoteKind::Command),
        ("b", RemoteKind::Form),
        ("c", RemoteKind::Prerender),
        ("d", RemoteKind::Query),
        ("e", RemoteKind::QueryBatch),
        ("f", RemoteKind::QueryLive),
    ];
    let source = "import secret from '$env/static/private';export const a=0,b=0,c=0,d=0,e=0,f=0;";
    for version in ["2.69.2", "3.0.1"] {
        let input = client(version, source, &exports);
        let out = transform_remote(&input).unwrap();
        assert!(!out.artifact.code.contains("secret"));
        assert!(!out.artifact.code.contains("$env"));
        for (name, kind) in exports {
            assert!(out.artifact.code.contains(&format!(
                "export const {name} = __remote.{}(\"{}/{name}\");",
                kind.function(),
                out.identity.hash
            )));
        }
        assert!(out.artifact.map.is_none());
        assert!(out.entry.is_none());
        let runtime = if version.starts_with('2') {
            "__sveltekit/remote"
        } else {
            "../node_modules/@sveltejs/kit/src/runtime/client/remote-functions/index.js"
        };
        assert!(out.artifact.code.contains(&quoted(runtime)));
    }
}
#[test]
fn kit3_reserved_exports_and_namespace_aliases_follow_upstream_order() {
    let source = "const a=1,b=2;export {a as class,b as delete};export const __remote=3,_0=4;";
    let mut input = client(
        "3.0.1",
        source,
        &[
            ("class", RemoteKind::Query),
            ("delete", RemoteKind::Command),
            ("__remote", RemoteKind::Form),
            ("_0", RemoteKind::Query),
        ],
    );
    input.development = true;
    let out = transform_remote(&input).unwrap();
    assert!(out.artifact.code.starts_with("import * as __remote0"));
    assert!(out.artifact.code.contains("const _1 = __remote0.query("));
    assert!(out.artifact.code.contains("const _2 = __remote0.command("));
    assert!(
        out.artifact
            .code
            .contains("export { _1 as class, _2 as delete };")
    );
    assert!(out.artifact.code.ends_with("import.meta.hot?.accept();\n"));
    input.discovery.kit_version = "2.69.3".into();
    assert!(
        transform_remote(&input)
            .unwrap_err()
            .contains("Unsupported remote export name")
    );
    let plain = client(
        "2.69.3",
        "export const __remote=0;",
        &[("__remote", RemoteKind::Query)],
    );
    assert!(
        transform_remote(&plain)
            .unwrap()
            .artifact
            .code
            .starts_with("import * as __remote1")
    );
}
#[test]
fn metadata_is_required_and_checked_against_real_ast_exports() {
    let mut input = client(
        "3.0.1",
        "export const {a,b:renamed}={}; export {other} from './other.js';",
        &[
            ("a", RemoteKind::Query),
            ("renamed", RemoteKind::Query),
            ("other", RemoteKind::Form),
        ],
    );
    assert_eq!(
        transform_remote(&input).unwrap().explicit_exports,
        vec!["a", "other", "renamed"]
    );
    input.metadata.as_mut().unwrap().exports.pop();
    assert!(transform_remote(&input).unwrap_err().contains("disagree"));
    input.metadata = None;
    assert!(
        transform_remote(&input)
            .unwrap_err()
            .contains("requires evaluated")
    );
    let mut star = client(
        "3.0.1",
        "export * from './other.js';",
        &[("evaluated", RemoteKind::Query)],
    );
    assert!(transform_remote(&star).unwrap().has_star_reexports);
    star.metadata.as_mut().unwrap().hash = "stale".into();
    assert!(transform_remote(&star).unwrap_err().contains("identity"));
    let default = server_input("3.0.1", "const fn=1;export {fn as default};");
    assert!(transform_remote(&default).unwrap_err().contains("default"));
}
#[test]
fn discovery_requires_actual_dependency_peer_evidence_for_kit3() {
    let mut input = server_input("3.0.1", "").discovery;
    input.module_id = "/project/src/remote.ts".into();
    assert!(discover_remote(&input).unwrap());
    input.kit_version = "2.70.3".into();
    assert!(!discover_remote(&input).unwrap());
    input.module_id = "/project/node_modules/pkg/a.remote.ts".into();
    assert!(discover_remote(&input).unwrap());
    input.kit_version = "3.0.1".into();
    assert!(discover_remote(&input).unwrap_err().contains("evidence"));
    input.package_has_kit_peer = Some(false);
    assert!(!discover_remote(&input).unwrap());
    input.package_has_kit_peer = Some(true);
    assert!(discover_remote(&input).unwrap());
    input.module_id = "/project/src/api.remote.ts?raw".into();
    assert!(discover_remote(&input).is_err());
}
#[test]
fn source_maps_preserve_original_content_and_unmap_generated_suffix() {
    let mut input = server_input("3.0.1", "// 😀\r\nexport const value = query();\n");
    let out = transform_remote(&input).unwrap();
    let map = out.artifact.map.unwrap();
    assert_eq!(map["sourcesContent"], json!([input.artifact.code]));
    assert_eq!(map["sources"], json!([input.discovery.module_id]));
    // Exact per-scalar identity anchors: ASCII comment columns 0,1,2 then
    // astral scalar at column 3. Next line resets columns and increments line.
    assert!(
        map["mappings"]
            .as_str()
            .unwrap()
            .starts_with("AAAA,CAAC,CAAC,CAAC;AACH")
    );
    input.artifact.map = Some(
        json!({"version":3,"sources":["original.ts"],"sourcesContent":["typed source"],"sourceRoot":"/source/","names":["value"],"mappings":"AAAA;AACAA"}),
    );
    input.artifact.map_file = Some("api.remote.js.map".into());
    let preserved = transform_remote(&input).unwrap();
    let map = preserved.artifact.map.as_ref().unwrap();
    assert_eq!(map["sourceRoot"], "/source/");
    assert_eq!(map["names"], json!(["value"]));
    assert_eq!(preserved.updated_map_files["api.remote.js.map"], *map);
    input.source_maps = SourceMapPolicy::Drop;
    let dropped = transform_remote(&input).unwrap();
    assert!(dropped.artifact.map.is_none());
    assert!(dropped.removed_map_files.contains("api.remote.js.map"));
    input.source_maps = SourceMapPolicy::Reject;
    assert!(transform_remote(&input).is_err());
}
#[test]
fn disabled_worker_invalid_js_and_unknown_versions_fail_explicitly() {
    let mut input = server_input("3.0.1", "export const f=query();");
    input.enabled = false;
    assert!(
        transform_remote(&input)
            .unwrap_err()
            .contains("experimental.remoteFunctions")
    );
    input.enabled = true;
    input.target = RemoteTarget::ServiceWorker;
    assert!(
        transform_remote(&input)
            .unwrap_err()
            .contains("service worker")
    );
    input.target = RemoteTarget::Server;
    input.artifact.code = "export const x: number = 1;".into();
    assert!(
        transform_remote(&input)
            .unwrap_err()
            .contains("preprocessed JavaScript")
    );
    input.discovery.kit_version = "3.1.0".into();
    assert!(
        transform_remote(&input)
            .unwrap_err()
            .contains("Unsupported native Kit")
    );
}
#[test]
fn identity_hash_uses_reverse_utf16_and_collision_checks_are_explicit() {
    assert_eq!(remote_hash(""), "45h");
    assert_eq!(remote_hash("a"), "3t1g");
    // Surrogate-pair order differs from reversing Unicode scalar values.
    let units = [0x61u16, 0xd83d, 0xde00];
    let mut expected = 5381u32;
    for unit in units.into_iter().rev() {
        expected = expected.wrapping_mul(33) ^ u32::from(unit);
    }
    assert_eq!(
        u32::from_str_radix(&remote_hash("a😀"), 36).unwrap(),
        expected
    );
    let first = RemoteIdentity {
        hash: remote_hash("a"),
        file: "a".into(),
        module_id: "/root/a".into(),
    };
    validate_remote_identities(&[first.clone(), first.clone()]).unwrap();
    let second = RemoteIdentity {
        module_id: "/elsewhere/a".into(),
        ..first.clone()
    };
    assert!(
        validate_remote_identities(&[first, second])
            .unwrap_err()
            .contains("collision")
    );
}
