use super::*;
use serde_json::json;
use std::collections::BTreeMap;
use ts_svelte_bundle::{Asset, Chunk, css::CssOutput, metadata::Module};

fn plan(version: &str) -> RemotePrerenderEntry {
    RemotePrerenderEntry::new(
        version,
        Path::new("/project"),
        Path::new("/project/.svelte-kit/generated"),
        Path::new("/packages/kit/src/runtime"),
    )
    .unwrap()
}
fn request(version: &str) -> BundleRequest {
    serde_json::from_value(json!({
        "root":"/project", "entries":{"server":"/project/server.js"}, "target":"server",
        "compatibility":{"kitVersion":version}, "tsconfig":null
    }))
    .unwrap()
}
fn bundle(plan: &RemotePrerenderEntry) -> BundleResult {
    let id = identity(plan.module_id()).unwrap().to_owned();
    let exports: Vec<String> = plan
        .imports
        .iter()
        .map(|(_, export)| (*export).into())
        .collect();
    let chunk = Chunk {
        referenced_files: vec![],
        file_name: "chunks/remote-HASH.mjs".into(),
        name: "unrelated-output-name".into(),
        code: "/* identity comes from graph metadata, not source matching */".into(),
        is_entry: true,
        is_dynamic_entry: false,
        facade_module_id: Some(id.clone()),
        module_ids: vec![],
        imports: vec!["chunks/shared-HASH.js".into()],
        dynamic_imports: vec![],
        exports: exports.clone(),
        map: None,
        sourcemap_file_name: None,
    };
    let mut module_graph = BTreeMap::new();
    module_graph.insert(
        id.clone(),
        Module {
            id: id.clone(),
            is_entry: true,
            is_external: false,
            imports: plan
                .imports
                .iter()
                .map(|(path, _)| identity(path).unwrap().to_owned())
                .collect(),
            dynamic_imports: vec![],
            importers: vec![],
            dynamic_importers: vec![],
            exports,
        },
    );
    for (path, export) in &plan.imports {
        let path = identity(path).unwrap().to_owned();
        module_graph.insert(
            path.clone(),
            Module {
                id: path,
                is_entry: false,
                is_external: false,
                imports: vec![],
                dynamic_imports: vec![],
                importers: vec![id.clone()],
                dynamic_importers: vec![],
                exports: vec![(*export).into(), "other_framework_export".into()],
            },
        );
    }
    BundleResult {
        rolldown_version: "1.2.13",
        target: Target::Server,
        css_mode: "extract",
        css: CssOutput::default(),
        module_graph,
        chunks: vec![chunk],
        assets: vec![],
        diagnostics: vec![],
        compiler_responses: BTreeMap::new(),
    }
}

#[test]
fn family_entries_export_actual_modules_without_server_import_cycles() {
    for version in ["2.67.0", "2.68.0", "2.69.2", "2.69.3", "2.70.3"] {
        let entry = plan(version);
        assert_eq!(
            entry.source(),
            "export { stringify_remote_arg } from \"/packages/kit/src/runtime/shared.js\";\n"
        );
        let receipt = entry.emitted(&bundle(&entry)).unwrap();
        assert!(receipt.requires_hooks_transport);
        assert!(receipt.fix_stack_trace_export.is_none());
    }
    for version in ["3.0.0-next.32", "3.0.1"] {
        let entry = plan(version);
        assert_eq!(
            entry.source(),
            "export { stringify_remote_arg } from \"/packages/kit/src/runtime/shared.js\";\nexport { fix_stack_trace } from \"/packages/kit/src/runtime/server/sourcemaps.js\";\n"
        );
        assert_eq!(entry.required_modules().count(), 2);
        let receipt = entry.emitted(&bundle(&entry)).unwrap();
        assert!(!receipt.requires_hooks_transport);
        assert_eq!(
            receipt.fix_stack_trace_export.as_deref(),
            Some("fix_stack_trace")
        );
    }
    assert!(
        RemotePrerenderEntry::new("3.9.0", Path::new("/p"), Path::new("/g"), Path::new("/r"))
            .is_err()
    );
}

#[test]
fn apply_is_idempotent_and_retains_server_configuration() {
    let entry = plan("3.0.1");
    let mut request = request("3.0.1");
    request.conditions.push("custom-server-condition".into());
    request.define.insert("BUILDING".into(), "true".into());
    entry.apply(&mut request).unwrap();
    let once = serde_json::to_value(&request).unwrap();
    entry.apply(&mut request).unwrap();
    assert_eq!(serde_json::to_value(&request).unwrap(), once);
    assert_eq!(request.entries.len(), 2);
    assert_eq!(
        request.virtual_modules.get(entry.module_id()),
        Some(&entry.source)
    );
    assert_eq!(request.conditions, ["custom-server-condition"]);
    assert_eq!(request.define["BUILDING"], "true");
    assert!(matches!(
        request.output.preserve_entry_signatures,
        Some(EntrySignatures::Strict)
    ));
}

#[test]
fn failed_apply_is_transactional_for_entry_snapshot_and_map_collisions() {
    let entry = plan("2.69.2");
    for case in 0..5 {
        let mut request = request("2.69.2");
        match case {
            0 => {
                request
                    .entries
                    .insert(ENTRY_NAME.into(), "/occupied.js".into());
            }
            1 => {
                request
                    .virtual_modules
                    .insert(entry.module_id.clone(), "other source".into());
            }
            2 => {
                request
                    .virtual_module_maps
                    .insert(entry.module_id.clone(), json!({"version":3}));
            }
            3 => {
                request
                    .entries
                    .insert("another-name".into(), entry.module_id.clone());
            }
            4 => {
                request.virtual_modules.insert(
                    ".svelte-kit/generated/.native-virtual/prerender-remote-helpers.js".into(),
                    entry.source.clone(),
                );
            }
            _ => unreachable!(),
        }
        let before = serde_json::to_value(&request).unwrap();
        assert!(entry.apply(&mut request).is_err(), "case {case}");
        assert_eq!(serde_json::to_value(&request).unwrap(), before);
    }
}

#[test]
fn incompatible_graphs_and_identity_redirection_are_rejected() {
    let entry = plan("3.0.1");
    for case in 0..9 {
        let mut request = request("3.0.1");
        match case {
            0 => request.target = Target::Client,
            1 => request.node_rebundle = true,
            2 => request.compatibility.kit_version = Some("2.69.2".into()),
            3 => request.root = "/another-project".into(),
            4 => request.output.preserve_entry_signatures = Some(EntrySignatures::False),
            5 => request.output.code_splitting = Some(false),
            6 => request
                .external
                .push("/packages/kit/src/runtime/shared.js".into()),
            7 => request.external_packages.push("@sveltejs/kit".into()),
            8 => request.aliases.push(ts_svelte_bundle::Alias {
                find: "/packages/kit/src/runtime".into(),
                replacement: "/replacement".into(),
                matching: ts_svelte_bundle::AliasMatch::Prefix,
            }),
            _ => unreachable!(),
        }
        assert!(entry.apply(&mut request).is_err(), "case {case}");
        assert_eq!(request.entries.len(), 1);
        assert!(request.virtual_modules.is_empty());
    }
}

#[test]
fn output_uses_exact_facade_identity_and_actual_hashed_name() {
    let entry = plan("3.0.1");
    let mut output = bundle(&entry);
    let result = entry.emitted(&output).unwrap();
    assert_eq!(result.file_name, "chunks/remote-HASH.mjs");
    assert_eq!(
        result.facade_module_id,
        identity(entry.module_id()).unwrap()
    );
    assert_eq!(result.stringify_export, STRINGIFY);
    output.chunks[0].facade_module_id = Some("/different/prerender-remote-helpers.js".into());
    output.chunks[0].name = ENTRY_NAME.into();
    assert!(entry.emitted(&output).is_err());
}

#[test]
fn malformed_output_contracts_cannot_be_selected_by_export_coincidence() {
    let entry = plan("3.0.1");
    for case in 0..11 {
        let mut output = bundle(&entry);
        let id = identity(entry.module_id()).unwrap();
        match case {
            0 => output.target = Target::Client,
            1 => output.chunks[0].is_entry = false,
            2 => output.chunks[0].exports.pop().map(|_| ()).unwrap(),
            3 => output.chunks[0].exports.push(STRINGIFY.into()),
            4 => output.chunks[0].file_name = "../escape.js".into(),
            5 => {
                output.module_graph.remove(id);
            }
            6 => {
                output.module_graph.get_mut(id).unwrap().imports[0] = "/unrelated/shared.js".into()
            }
            7 => {
                output
                    .module_graph
                    .get_mut("/packages/kit/src/runtime/shared.js")
                    .unwrap()
                    .is_external = true
            }
            8 => output
                .module_graph
                .get_mut("/packages/kit/src/runtime/shared.js")
                .unwrap()
                .exports
                .clear(),
            9 => {
                output.assets.push(Asset {
                    file_name: output.chunks[0].file_name.clone(),
                    names: vec![],
                    original_file_names: vec![],
                    encoding: "base64",
                    source: String::new(),
                });
            }
            10 => {
                output.chunks.extend(bundle(&entry).chunks);
            }
            _ => unreachable!(),
        }
        assert!(entry.emitted(&output).is_err(), "case {case}");
    }
}

#[test]
fn imports_escape_javascript_quotes_and_paths_reject_ambiguous_identity() {
    let entry = RemotePrerenderEntry::new(
        "3.0.1",
        Path::new("/project"),
        Path::new("/generated"),
        Path::new("/packages/kit\"quote/runtime"),
    )
    .unwrap();
    assert!(entry.source().contains("kit\\\"quote/runtime/shared.js"));
    for path in [
        "relative", "/a/../b", "/a/./b", "/a//b", "/a/", "/a?query", "/a#hash", "/a\\b",
    ] {
        assert!(
            RemotePrerenderEntry::new("3.0.1", Path::new("/p"), Path::new(path), Path::new("/r"))
                .is_err(),
            "{path}"
        );
    }
}
