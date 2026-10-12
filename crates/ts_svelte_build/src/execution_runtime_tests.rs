//! Explicit native-binary integration fixtures. No Node/config execution and no
//! production application inputs: all application sources belong to Fixture.
#![cfg(unix)]

use crate::{build, pipeline_tests::Fixture};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    io::{BufRead, BufReader, Read, Write},
    path::PathBuf,
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc::{self, Receiver},
    thread::JoinHandle,
    time::Duration,
};
use ts_svelte_kit::{
    build_analysis::ModuleSnapshot,
    build_env::{ResolvedEnvDefinition, ResolvedEnvSchema, ResolvedEnvVariable},
};

/// A timeout or assertion failure kills and reaps the explicit native child.
/// A reader thread prevents an unsupported/deadlocked operation hanging Cargo.
pub(crate) struct NativeProcess {
    child: Child,
    input: Option<ChildStdin>,
    responses: Receiver<Result<Value, String>>,
    reader: Option<JoinHandle<()>>,
}
impl NativeProcess {
    pub(crate) fn start() -> Self {
        let binary = PathBuf::from(
            std::env::var_os("TS_SVELTE_NATIVE_RUNTIME")
                .expect("Set TS_SVELTE_NATIVE_RUNTIME to the native ts_svelte_runtime binary"),
        );
        assert!(binary.is_absolute() && binary.is_file());
        let mut child = Command::new(binary)
            .arg("--serve")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("Start explicitly supplied native runtime");
        let input = child.stdin.take();
        let stdout = child.stdout.take().unwrap();
        let (sender, responses) = mpsc::channel();
        let reader = std::thread::spawn(move || {
            let mut stdout = BufReader::new(stdout);
            const MAX_RESPONSE: usize = 8 * 1024 * 1024;
            loop {
                let mut line = Vec::new();
                let result = (&mut stdout)
                    .take((MAX_RESPONSE + 1) as u64)
                    .read_until(b'\n', &mut line);
                let response = match result {
                    Ok(0) => break,
                    Ok(_) if line.len() > MAX_RESPONSE => {
                        Err("Native response exceeds 8 MiB".into())
                    }
                    Ok(_) => serde_json::from_slice(&line).map_err(|e| e.to_string()),
                    Err(error) => Err(error.to_string()),
                };
                let failed = response.is_err();
                if sender.send(response).is_err() || failed {
                    break;
                }
            }
        });
        Self {
            child,
            input,
            responses,
            reader: Some(reader),
        }
    }
    pub(crate) fn request(&mut self, request: Value) -> Value {
        let bytes = serde_json::to_vec(&request).unwrap();
        assert!(bytes.len() < 96 * 1024 * 1024);
        let input = self.input.as_mut().unwrap();
        input.write_all(&bytes).unwrap();
        input.write_all(b"\n").unwrap();
        input.flush().unwrap();
        self.responses
            .recv_timeout(Duration::from_secs(60))
            .expect("Native runtime response within 60 seconds")
            .expect("Valid bounded native JSON response")
    }
    pub(crate) fn success(&mut self, request: Value) -> Value {
        let operation = request["op"].clone();
        let response = self.request(request);
        assert_eq!(response["ok"], true, "Native {operation}: {response}");
        response["result"].clone()
    }
}
impl Drop for NativeProcess {
    fn drop(&mut self) {
        self.input.take();
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

pub(crate) fn fixture(kit3: bool) -> Fixture {
    let variable = if kit3 {
        "TS_SVELTE_BUILD_KIT3_FIXTURE"
    } else {
        "TS_SVELTE_BUILD_KIT_FIXTURE"
    };
    let dependency_project =
        PathBuf::from(std::env::var_os(variable).unwrap_or_else(|| {
            panic!("Set {variable} to the read-only installed dependency fixture")
        }));
    assert!(dependency_project.is_absolute());
    let kit = if kit3 { "3.0.1" } else { "2.69.2" };
    let svelte = if kit3 { "5.57.2" } else { "5.53.11" };
    for (package, expected) in [("@sveltejs/kit", kit), ("svelte", svelte)] {
        let package_json: Value = serde_json::from_slice(
            &std::fs::read(
                dependency_project
                    .join("node_modules")
                    .join(package)
                    .join("package.json"),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(
            package_json["version"], expected,
            "Registered fixture profile"
        );
    }
    let mut fixture = Fixture::new(
        "export const prerender=false; export function load(){throw new Error('Metadata must not invoke universal load');}",
    );
    std::os::unix::fs::symlink(
        dependency_project.join("node_modules"),
        fixture.root.join("node_modules"),
    )
    .unwrap();
    fixture.write(
        "package.json",
        r#"{"name":"native-server-execution-fixture","type":"module"}"#,
    );
    if kit3 {
        let generated = fixture.root.join(".kit/generated/build");
        let runtime = fixture.request.kit.inventory.runtime_directory.clone();
        fixture.request.kit.inventory.kit_version = kit.into();
        fixture.request.kit.inventory.generated_directory = generated.clone();
        fixture.request.kit.inventory.nodes[0].component =
            Some(runtime.join("components/layout.svelte"));
        fixture.request.kit.inventory.nodes[1].component =
            Some(runtime.join("components/error.svelte"));
        fixture.request.kit.server.kit_version = kit.into();
        fixture.request.kit.server.output = generated.clone();
        fixture.request.kit.environment.kit_version = kit.into();
        fixture.request.kit.environment.environment_directory = generated.join("env");
        fixture.request.compiler.svelte_version = svelte.into();
        let env_entry = fixture.root.join("src/env.ts");
        fixture.request.sources.insert(
            env_entry.clone(),
            "export const variables={NATIVE_TEST_VALUE:{public:false,static:false}};".into(),
        );
        fixture.request.kit.environment.definition = Some(ResolvedEnvDefinition {
            entry: env_entry,
            declaration_order: vec!["NATIVE_TEST_VALUE".into()],
            variables: BTreeMap::from([(
                "NATIVE_TEST_VALUE".into(),
                ResolvedEnvVariable {
                    public: false,
                    is_static: false,
                    schema: ResolvedEnvSchema::None,
                },
            )]),
        });
    }
    fixture
        .request
        .kit
        .environment
        .build_values
        .insert("NATIVE_TEST_VALUE".into(), "snapshot-😀".into());
    let env_import = if kit3 {
        "import { NATIVE_TEST_VALUE as value } from '$app/env/private';"
    } else {
        "import { env } from '$env/dynamic/private'; const value = env.NATIVE_TEST_VALUE;"
    };
    let server = fixture.root.join("src/routes/+page.server.ts");
    fixture.request.kit.inventory.nodes[2].server = Some(server.clone());
    fixture.request.modules.insert(
        server.to_string_lossy().into_owned(),
        ModuleSnapshot {
            source: format!(
                r#"
{env_import}
import {{ building }} from '$app/environment';
if (!building || value !== 'snapshot-😀') throw new Error('Environment initialization order');
const stream = new ReadableStream({{
  start(controller) {{ controller.enqueue(new TextEncoder().encode(value)); controller.close(); }}
}});
const text = await new Response(stream).text();
if (text !== value || !stream.locked) throw new Error('Native Fetch stream consumption');
globalThis.__native_metadata_evaluations = (globalThis.__native_metadata_evaluations ?? 0) + 1;
if (globalThis.__native_metadata_evaluations !== 1) throw new Error('Node module evaluated twice');
export const ssr = (() => true)();
export function load() {{ throw new Error('Metadata must not invoke server load'); }}
"#
            ),
            imports: BTreeMap::new(),
        },
    );
    let component = fixture.root.join("src/routes/+page.svelte");
    fixture.request.kit.inventory.nodes[2].component = Some(component.clone());
    fixture
        .request
        .sources
        .insert(component, "<h1>Native metadata 😀</h1>".into());
    fixture
}

async fn actual_emitted_server(kit3: bool) {
    let fixture = fixture(kit3);
    let report = build(fixture.request.clone()).await;
    let details = json!({"status":report.status,"phases":report.phases,"requirements":report.requirements,"diagnostics":report.diagnostics});
    assert_eq!(report.status, "incomplete", "{details}");
    assert!(report.diagnostics.is_empty(), "{details}");
    assert!(
        !report.requirements.is_empty(),
        "Dynamic metadata must require actual evaluation"
    );
    assert!(
        report.requirements.iter().all(|r| r.stage == "analysis"
            && matches!(r.code.as_str(), "runtime_value" | "load_presence")),
        "{details}"
    );
    assert!(
        report.server.is_some() && report.client.is_none(),
        "{details}"
    );
    assert!(
        !report.artifacts.contains_key("server/manifest-full.js"),
        "Analysis artifacts must remain isolated"
    );
    let preliminary = report
        .runtime_snapshot
        .as_ref()
        .expect("Actual preliminary manifest");
    let execution = report
        .execution_snapshot
        .as_ref()
        .expect("Actual execution snapshot");
    let bootstrap = execution.bootstrap_json().unwrap();
    assert_eq!(bootstrap["serverBuildId"], preliminary.server_build_id);
    assert_eq!(bootstrap["environment"]["NATIVE_TEST_VALUE"], "snapshot-😀");
    assert_eq!(
        bootstrap["initialization"]["profile"],
        if kit3 { "kit3" } else { "kit2" }
    );
    let manifest = bootstrap["manifestEntry"].as_str().unwrap();
    assert!(manifest.ends_with("/.kit/output/server/manifest-full.js"));
    assert!(
        bootstrap["snapshot"]["modules"][manifest]["source"]
            .as_str()
            .unwrap()
            .contains("export const manifest")
    );
    assert!(bootstrap["assets"]["nodes/2.js"].is_array());

    let mut runtime = NativeProcess::start();
    let capabilities = runtime.success(json!({"op":"capabilities"}));
    assert_eq!(capabilities["engine"], "boa");
    assert_eq!(capabilities["implicitFilesystem"], false);
    assert_eq!(capabilities["networkTransport"], false);
    let created = runtime.success(json!({"op":"create","bootstrap":bootstrap}));
    assert_eq!(created["serverBuildId"], preliminary.server_build_id);
    let session = created["session"].as_str().unwrap();
    let loaded = runtime.success(json!({"op":"loadNodes","session":session}));
    assert_eq!(loaded["serverBuildId"], preliminary.server_build_id);
    assert_eq!(loaded["nodes"].as_array().unwrap().len(), 3);
    for index in 0..3 {
        assert_eq!(
            loaded["nodes"][index],
            json!({
                "index": index,
                "hasServerLoad": index == 2,
                "hasUniversalLoad": index == 2,
                "serverLoadCallable": index == 2,
                "universalLoadCallable": index == 2
            })
        );
    }
    assert_eq!(
        runtime.success(json!({"op":"loadNodes","session":session})),
        loaded
    );
    let metadata_request = json!({"op":"analyzeMetadata","session":session,"options":{"profile":if kit3 {"3.0.1"} else {"2.69.2"}}});
    let metadata = runtime.success(metadata_request.clone());
    assert_eq!(metadata["serverBuildId"], preliminary.server_build_id);
    assert_eq!(metadata["nodes"], loaded["nodes"]);
    assert_eq!(metadata["routes"].as_array().unwrap().len(), 1);
    assert_eq!(
        metadata["routes"][0]["page"]["ssr"],
        json!({"kind":"json","value":true})
    );
    assert_eq!(metadata["routes"][0]["methods"], json!(["GET"]));
    assert_eq!(runtime.success(metadata_request), metadata);
    assert_eq!(
        runtime.success(json!({"op":"dispose","session":session})),
        json!({"disposed":true})
    );
    let after_dispose = runtime.request(json!({"op":"loadNodes","session":session}));
    assert_eq!(after_dispose["ok"], false);
    assert_eq!(after_dispose["error"]["code"], "unknown_session");
    assert_eq!(
        runtime.success(json!({"op":"dispose","session":session})),
        json!({"disposed":false})
    );
    // The same source snapshot must advance past dynamic analysis all the way
    // through the ordinary client/manifest pipeline with the explicit evaluator.
    let binary = PathBuf::from(std::env::var_os("TS_SVELTE_NATIVE_RUNTIME").unwrap());
    let mut evaluator = crate::native_runtime::NativeRuntime::new(binary).unwrap();
    let completed = crate::build_with_runtime(fixture.request.clone(), &mut evaluator).await;
    assert_eq!(
        completed.status,
        "complete",
        "{}",
        json!({"phases":completed.phases,"requirements":completed.requirements,"diagnostics":completed.diagnostics})
    );
    assert!(completed.client.is_some());
    assert!(completed.artifacts.contains_key("server/manifest.js"));
    assert!(
        completed
            .phases
            .iter()
            .any(|phase| phase.name == "runtime analysis" && phase.status == "complete")
    );
    assert!(completed.analysis.as_ref().unwrap().requirements.is_empty());
}

#[tokio::test]
#[ignore = "requires TS_SVELTE_NATIVE_RUNTIME and TS_SVELTE_BUILD_KIT_FIXTURE (Kit 2.69.2 / Svelte 5.53.11)"]
async fn emitted_kit2_snapshot_initializes_and_loads_in_native_runtime() {
    actual_emitted_server(false).await;
}

#[tokio::test]
#[ignore = "requires TS_SVELTE_NATIVE_RUNTIME and TS_SVELTE_BUILD_KIT3_FIXTURE (Kit 3.0.1 / Svelte 5.57.2)"]
async fn emitted_kit3_snapshot_initializes_and_loads_in_native_runtime() {
    actual_emitted_server(true).await;
}
