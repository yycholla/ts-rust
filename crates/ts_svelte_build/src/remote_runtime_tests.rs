//! Actual remote transforms -> emitted server -> native metadata -> client stubs.
//! Uses only explicitly installed fixture dependencies and synthetic app sources.
#![cfg(unix)]
use crate::*;
use ts_svelte_kit::build_remote::{RemoteKind, remote_hash};

async fn roundtrip(kit3: bool, prerender: bool) {
    let mut fixture = crate::execution_runtime_tests::fixture(kit3);
    let remote = fixture.root.join("src/messages.remote.ts");
    fixture.request.kit.config.remote_functions_enabled = true;
    fixture.request.kit.config.remote_modules = vec![remote.clone()];
    let remote_source = r#"
import { query } from '$app/server';
import { building } from '$app/environment';
if (!building) throw Error('NATIVE_REMOTE_SERVER_ONLY');
const check: boolean = true;
if (!check) throw Error('TypeScript source did not execute');
export const message = query(() => { throw Error('Remote handler must not execute during metadata'); });
"#;
    fixture.request.sources.insert(
        remote,
        if prerender {
            remote_source.replace("query", "prerender")
        } else {
            remote_source.into()
        },
    );
    fixture.request.sources.insert(
        fixture.root.join("src/routes/+page.svelte"),
        r#"
<script>import { message } from '../messages.remote';</script>
<button onclick={() => message()}>Remote message</button>
"#
        .into(),
    );
    let binary =
        std::env::var_os("TS_SVELTE_NATIVE_RUNTIME").expect("Explicit native runtime binary");
    let mut runtime = native_runtime::NativeRuntime::new(binary.into()).unwrap();
    let report = build_with_runtime(fixture.request.clone(), &mut runtime).await;
    let details = json!({"status":report.status,"phases":report.phases,"requirements":report.requirements,"diagnostics":report.diagnostics});
    assert_eq!(
        report.status,
        if prerender { "incomplete" } else { "complete" },
        "{details}"
    );
    if prerender {
        assert_eq!(report.requirements.len(), 1, "{details}");
        assert_eq!(report.requirements[0].stage, "prerender");
        assert_eq!(report.requirements[0].code, "remote_execution");
    }
    let receipt = report
        .remote_metadata
        .as_ref()
        .expect("Evaluated remote receipt");
    assert_eq!(
        Some(&receipt.server_build_id),
        report.server_build_id.as_ref()
    );
    assert_eq!(receipt.evaluated.len(), 1);
    let expected_hash = remote_hash("src/messages.remote.ts");
    assert_eq!(receipt.evaluated[0].hash, expected_hash);
    assert_eq!(receipt.evaluated[0].file, "src/messages.remote.ts");
    assert_eq!(receipt.evaluated[0].exports.len(), 1);
    assert_eq!(receipt.evaluated[0].exports[0].name, "message");
    assert_eq!(
        receipt.evaluated[0].exports[0].kind,
        if prerender {
            RemoteKind::Prerender
        } else {
            RemoteKind::Query
        }
    );
    assert_eq!(
        receipt.remotes[0].exports[0].dynamic,
        if prerender {
            runtime_remotes::RuntimeDynamic::Undefined
        } else {
            runtime_remotes::RuntimeDynamic::Json(true)
        }
    );
    let server = report.server.as_ref().unwrap();
    let chunks = remote_graph::manifest_chunks(server).unwrap();
    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0].hash, expected_hash);
    let artifact = report.artifacts.get("server/manifest.js").unwrap();
    let manifest = String::from_utf8(STANDARD.decode(&artifact.source).unwrap()).unwrap();
    assert!(manifest.contains(&expected_hash));
    assert!(manifest.contains(&chunks[0].chunk));
    let client = report.client.as_ref().expect("Client graph");
    assert!(
        client
            .chunks
            .iter()
            .any(|c| c.code.contains(&format!("{expected_hash}/message")))
    );
    assert!(
        !client
            .chunks
            .iter()
            .any(|c| c.code.contains("NATIVE_REMOTE_SERVER_ONLY")
                || c.code.contains("Remote handler must not execute"))
    );
}
use base64::engine::general_purpose::STANDARD;

#[tokio::test]
#[ignore = "requires explicit installed Kit2 dependency fixture and native runtime binary"]
async fn kit2_remote_build_uses_real_native_server_metadata() {
    roundtrip(false, false).await
}
#[tokio::test]
#[ignore = "requires explicit installed Kit3 dependency fixture and native runtime binary"]
async fn kit3_remote_build_uses_real_native_server_metadata() {
    roundtrip(true, false).await
}

#[tokio::test]
#[ignore = "requires explicit installed Kit2 dependency fixture and native runtime binary"]
async fn kit2_remote_prerender_is_not_complete_until_execution() {
    roundtrip(false, true).await
}
#[tokio::test]
#[ignore = "requires explicit installed Kit3 dependency fixture and native runtime binary"]
async fn kit3_remote_prerender_is_not_complete_until_execution() {
    roundtrip(true, true).await
}
