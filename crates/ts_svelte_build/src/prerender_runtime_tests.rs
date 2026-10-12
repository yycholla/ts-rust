//! Actual native compilation -> emitted Kit server -> native SSR visits.
//! Isolated synthetic application sources and explicitly selected installed
//! packages/binary only. This is not a crawler or complete-build parity claim.
#![cfg(unix)]

use crate::{
    build_with_runtime, execution_runtime_tests::fixture, execution_snapshot, manifest,
    native_runtime::NativeRuntime,
};
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::PathBuf};
use ts_svelte_kit::{
    build_analysis::{Fact, ModuleSnapshot},
    build_plan,
};

fn bytes(value: &Value) -> Vec<u8> {
    value
        .as_array()
        .expect("Native bytes array")
        .iter()
        .map(|value| {
            u8::try_from(value.as_u64().expect("Unsigned native byte")).expect("Byte range")
        })
        .collect()
}

async fn actual_server_render(kit3: bool) {
    let mut fixture = fixture(kit3);
    let universal = fixture.root.join("src/routes/+page.ts");
    fixture.request.modules.insert(
        universal.to_string_lossy().into_owned(),
        ModuleSnapshot {
            source: "export const prerender=true; export const csr=false; export const ssr=true;"
                .into(),
            imports: BTreeMap::new(),
        },
    );
    let environment = if kit3 {
        "import { NATIVE_TEST_VALUE as value } from '$app/env/private';"
    } else {
        "import { env } from '$env/dynamic/private'; const value=env.NATIVE_TEST_VALUE;"
    };
    let server = fixture.root.join("src/routes/+page.server.ts");
    fixture.request.modules.insert(
        server.to_string_lossy().into_owned(),
        ModuleSnapshot {
            source: format!(
                r#"
{environment}
import {{ building }} from '$app/environment';
if (!building || value !== 'snapshot-😀') throw Error('Rendering environment is not initialized');
let visits=0;
export async function load() {{
  visits += 1;
  const stream=new ReadableStream({{
    start(controller) {{
      controller.enqueue(new TextEncoder().encode('native-SSR:' + value));
      controller.close();
    }}
  }});
  const label=await new Response(stream).text();
  if (!stream.locked) throw Error('Body consumption must retain its reader');
  return {{label, visits}};
}}
"#
            ),
            imports: BTreeMap::new(),
        },
    );
    fixture.request.sources.insert(
        fixture.root.join("src/routes/+page.svelte"),
        r#"
<script>let { data } = $props();</script>
<h1>{data.label}</h1>
<p data-visit={data.visits}>native visit {data.visits}</p>
"#
        .into(),
    );
    let binary = PathBuf::from(
        std::env::var_os("TS_SVELTE_NATIVE_RUNTIME")
            .expect("Explicit native framework runtime binary"),
    );
    let mut evaluator = NativeRuntime::new(binary).unwrap();
    let report = build_with_runtime(fixture.request.clone(), &mut evaluator).await;
    let details = json!({"status":report.status,"phases":report.phases,"requirements":report.requirements,"diagnostics":report.diagnostics});
    assert_eq!(report.status, "incomplete", "{details}");
    assert!(report.diagnostics.is_empty(), "{details}");
    assert_eq!(
        report.requirements.len(),
        1,
        "Only actual prerender execution remains: {details}"
    );
    assert_eq!(report.requirements[0].stage, "prerender", "{details}");
    assert_eq!(
        report.requirements[0].code, "runtime_execution",
        "{details}"
    );
    assert!(
        report
            .phases
            .iter()
            .any(|p| p.name == "runtime analysis" && p.status == "complete"),
        "{details}"
    );
    let analysis = report.analysis.as_ref().expect("Resolved runtime metadata");
    assert!(analysis.requirements.is_empty(), "{details}");
    assert_eq!(
        analysis.routes["/"].prerender,
        Fact::Known { value: json!(true) }
    );
    assert_eq!(analysis.routes["/"].csr, Fact::Known { value: false });
    assert_eq!(analysis.routes["/"].ssr, Fact::Known { value: true });
    let server = report.server.as_ref().expect("Actual emitted server graph");
    assert!(
        server
            .chunks
            .iter()
            .any(|chunk| chunk.file_name == "index.js")
    );

    // Build an isolated full-route manifest from the actual current server bytes.
    // No client metadata is needed for csr=false. Reusing a pre-analysis receipt
    // would give a stale build identity after Kit3's immutable finalization.
    let plan = build_plan::create_build_plan(&fixture.request.kit).unwrap();
    let preliminary = manifest::preliminary(&fixture.request, &plan, analysis, server).unwrap();
    let execution = execution_snapshot::prepare(&fixture.request, &preliminary).unwrap();
    let bootstrap = execution.bootstrap_json().unwrap();
    let server_entry = url::Url::from_file_path(
        fixture
            .request
            .kit
            .config
            .out_dir
            .join("output/server/index.js"),
    )
    .unwrap()
    .to_string();
    assert!(
        bootstrap["snapshot"]["modules"]
            .get(&server_entry)
            .is_some()
    );
    let build_id = preliminary.server_build_id;
    // Deterministic bytes are explicitly a test entropy fixture; they are never
    // offered as a production random source. CSP requests fresh nonce bytes.
    let entropy: Vec<u8> = (0..1024).map(|index| (index % 256) as u8).collect();
    let mut runtime = evaluator
        .prerender(
            &execution,
            crate::prerender_transport::PrerenderOptions {
                server_entry,
                origin: "https://native-render.example".into(),
                base: fixture.request.kit.environment.base.clone(),
                app_dir: fixture.request.kit.environment.app_dir.clone(),
                static_assets: BTreeMap::new(),
                entropy,
            },
        )
        .unwrap();
    runtime.start().unwrap();
    let mut saved = ts_svelte_kit::prerender_save::SaveState::new(
        ts_svelte_kit::prerender_save::SaveConfig {
            kit_version: fixture.request.kit.inventory.kit_version.clone(),
            base: fixture.request.kit.environment.base.clone(),
            origin: "https://native-render.example".into(),
            http_error: ts_svelte_kit::prerender_save::HttpErrorPolicy::Fail,
        },
        BTreeMap::new(),
    )
    .unwrap();
    let mut written = BTreeMap::new();
    for visit in 1..=2 {
        let head = runtime.begin_visit("/").unwrap();
        assert_eq!(head.response.route_id.as_deref(), Some("/"));
        let main = runtime.read_main(head.token).unwrap();
        save_response(
            &mut saved,
            &mut written,
            &mut runtime,
            &main,
            "/",
            "/",
            None,
            ts_svelte_kit::prerender_save::Category::Pages,
        );
        let mut dependencies = vec![];
        while let Some(next) = runtime.next_dependency(head.token).unwrap() {
            let dependency = runtime.read_dependency(head.token).unwrap();
            assert_eq!(next.path, dependency.path);
            save_response(
                &mut saved,
                &mut written,
                &mut runtime,
                &dependency.response,
                &dependency.decoded_path,
                &dependency.encoded_path,
                Some("/"),
                ts_svelte_kit::prerender_save::Category::Dependencies,
            );
            dependencies.push(dependency);
        }
        let rendered = serde_json::to_value(crate::prerender_transport::Visit {
            server_build_id: head.server_build_id,
            encoded_path: head.encoded_path,
            decoded_path: head.decoded_path,
            response: main,
            dependencies,
        })
        .unwrap();
        assert_eq!(rendered["serverBuildId"], build_id);
        assert_eq!(rendered["encodedPath"], "/");
        assert_eq!(rendered["decodedPath"], "/");
        let response = &rendered["response"];
        let html = String::from_utf8(bytes(&response["body"])).expect("SSR document is UTF-8");
        assert_eq!(
            response["status"], 200,
            "Native SSR returned an error document: {html}"
        );
        assert_eq!(response["routeId"], "/");
        assert!(
            html.contains("native-SSR:snapshot-😀"),
            "Rendered data missing: {html}"
        );
        assert!(
            html.contains(&format!("native visit {visit}")),
            "Server module state did not persist: {html}"
        );
        assert!(
            html.contains("<h1>"),
            "Compiled component markup missing: {html}"
        );
        let headers = response["headers"].as_array().expect("Raw header entries");
        assert!(headers.iter().any(|entry| {
            bytes(&entry[0]) == b"content-type"
                && String::from_utf8(bytes(&entry[1]))
                    .unwrap()
                    .starts_with("text/html")
        }));
        // Both pinned page/index.js implementations always serialize server
        // load data when prerendering, even with csr=false. text(data) supplies
        // a UTF-8 Content-Length; the original NDJSON string is also retained as
        // dependency.body and must take precedence over response consumption.
        let dependencies = rendered["dependencies"].as_array().unwrap();
        assert_eq!(
            dependencies.len(),
            1,
            "Expected the generated data dependency, got paths {:?}",
            dependencies
                .iter()
                .take(4)
                .map(|value| &value["path"])
                .collect::<Vec<_>>()
        );
        let dependency = &dependencies[0];
        for field in ["path", "encodedPath", "decodedPath"] {
            assert_eq!(dependency[field], "/__data.json");
        }
        let data_response = &dependency["response"];
        assert_eq!(data_response["status"], 200);
        assert_eq!(data_response["statusText"], json!([]));
        assert_eq!(data_response["routeId"], Value::Null);
        let data_bytes = bytes(&data_response["body"]);
        assert!(
            data_bytes.len() < 16_384,
            "Unexpected generated data size {}",
            data_bytes.len()
        );
        let data_text =
            String::from_utf8(data_bytes.clone()).expect("Data dependency must be UTF-8");
        assert!(data_text.ends_with('\n'));
        assert_eq!(
            data_text.lines().count(),
            1,
            "No deferred load values in this fixture: {data_text}"
        );
        let data: Value = serde_json::from_str(&data_text).expect("Kit data NDJSON record");
        assert_eq!(data["type"], "data");
        let nodes = data["nodes"]
            .as_array()
            .expect("Serialized server node data");
        assert_eq!(nodes.len(), 2, "Root layout and leaf page: {data_text}");
        assert!(nodes[0].is_null());
        assert_eq!(nodes[1]["type"], "data");
        assert!(nodes[1]["uses"].is_object());
        // For this plain two-property object, devalue encodes the object at
        // index zero and its values as references into the same flat array.
        let pool = nodes[1]["data"].as_array().expect("devalue reference pool");
        let properties = pool[0].as_object().expect("Plain load data object");
        assert_eq!(properties.len(), 2);
        let value = |name: &str| {
            let index = properties[name].as_u64().expect("devalue value reference") as usize;
            pool.get(index).expect("Reference within devalue pool")
        };
        assert_eq!(value("label"), &json!("native-SSR:snapshot-😀"));
        assert_eq!(value("visits"), &json!(visit));
        let data_headers = data_response["headers"]
            .as_array()
            .expect("Data response headers");
        assert!(
            data_headers.iter().any(|entry| {
                bytes(&entry[0]) == b"content-length"
                    && bytes(&entry[1]) == data_bytes.len().to_string().as_bytes()
            }),
            "Data Content-Length must count UTF-8 bytes"
        );
    }
    assert_eq!(
        written.len(),
        2,
        "A repeated path must not overwrite either saved response"
    );
    let html = String::from_utf8(written["pages/index.html"].clone()).unwrap();
    assert!(html.contains("native visit 1"));
    assert!(!html.contains("native visit 2"));
    assert!(written.contains_key("dependencies/__data.json"));
    assert_eq!(saved.snapshot().paths, vec!["/", "/__data.json"]);
    assert_eq!(saved.snapshot().prerendered_routes, vec!["/"]);
    runtime.dispose().unwrap();
    // Execution above verifies SSR only. No crawler output is silently marked as
    // a completed build and the original report remains explicitly incomplete.
    assert_eq!(report.status, "incomplete");
}

#[tokio::test]
#[ignore = "requires refreshed TS_SVELTE_NATIVE_RUNTIME and TS_SVELTE_BUILD_KIT_FIXTURE (Kit 2.69.2 / Svelte 5.53.11)"]
async fn emitted_kit2_server_renders_native_ssr_data_on_repeated_visits() {
    actual_server_render(false).await;
}

#[tokio::test]
#[ignore = "requires refreshed TS_SVELTE_NATIVE_RUNTIME and TS_SVELTE_BUILD_KIT3_FIXTURE (Kit 3.0.1 / Svelte 5.57.2)"]
async fn emitted_kit3_server_renders_native_ssr_data_on_repeated_visits() {
    actual_server_render(true).await;
}

fn save_response(
    state: &mut ts_svelte_kit::prerender_save::SaveState,
    written: &mut BTreeMap<String, Vec<u8>>,
    runtime: &mut crate::prerender_transport::NativePrerender,
    response: &crate::prerender_transport::Response,
    decoded: &str,
    encoded: &str,
    referrer: Option<&str>,
    category: ts_svelte_kit::prerender_save::Category,
) {
    use ts_svelte_kit::prerender_save::*;
    let result = state
        .save(SaveInput {
            decoded: decoded.into(),
            encoded: encoded.into(),
            referrer: referrer.map(str::to_owned),
            reference_type: if category == Category::Pages {
                ReferenceType::Linked
            } else {
                ReferenceType::Fetched
            },
            category,
            status: response.status,
            headers: response.normalized_headers().unwrap(),
            body: response.body.clone(),
        })
        .unwrap();
    assert!(
        matches!(
            result.status,
            SaveStatus::Complete | SaveStatus::Duplicate { .. }
        ),
        "{result:?}"
    );
    for effect in result.effects {
        match effect {
            SaveEffect::Write { path, body } => {
                assert!(written.insert(path, body).is_none());
            }
            SaveEffect::Saved { file, destination } => {
                runtime.publish(&file, &written[&destination]).unwrap()
            }
            SaveEffect::RouteSeen { .. }
            | SaveEffect::Page { .. }
            | SaveEffect::Asset { .. }
            | SaveEffect::Path { .. }
            | SaveEffect::ManifestAsset { .. } => {}
            effect => panic!("Unexpected fixture save effect {effect:?}"),
        }
    }
}
