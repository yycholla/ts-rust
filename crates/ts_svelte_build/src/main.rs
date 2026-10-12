use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    io::{self, BufRead, Read, Write},
    path::PathBuf,
};
use ts_svelte_build::{BuildRequest, MAX_REQUEST_BYTES};
#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "camelCase", deny_unknown_fields)]
enum Request {
    Capabilities,
    Build {
        request: Box<BuildRequest>,
        #[serde(rename = "outputDirectory")]
        output_directory: Option<PathBuf>,
        #[serde(rename = "runtimeBinary")]
        runtime_binary: Option<PathBuf>,
    },
    /// Load an installed project natively, then build it like `build`.
    Project {
        project: ts_svelte_build::project::ProjectInput,
        #[serde(rename = "outputDirectory")]
        output_directory: Option<PathBuf>,
        #[serde(rename = "runtimeBinary")]
        runtime_binary: Option<PathBuf>,
        /// Return the loaded request instead of building it.
        #[serde(default, rename = "requestOnly")]
        request_only: bool,
        /// Loader requirements (`stage/code`) accepted as non-blocking.
        #[serde(default, rename = "acceptRequirements")]
        accept_requirements: Vec<String>,
        /// Return the whole report, including intermediate bundles, even
        /// when the output is written to disk.
        #[serde(default, rename = "fullReport")]
        full_report: bool,
    },
}
async fn dispatch(bytes: &[u8]) -> Value {
    let started = std::time::Instant::now();
    let input = match serde_json::from_slice::<Request>(bytes) {
        Ok(input) => input,
        Err(error) => return json!({"ok":false,"error":error.to_string()}),
    };
    let (request, output_directory, runtime_binary, loaded, full_report) = match input {
        Request::Project {
            project,
            output_directory,
            runtime_binary,
            request_only,
            accept_requirements,
            full_report,
        } => {
            let loaded = match ts_svelte_build::project::load(&project) {
                Ok(loaded) => loaded,
                Err(error) => return json!({"ok":false,"error":error}),
            };
            if request_only {
                return json!({"ok":true,"request":loaded.request,"requirements":loaded.requirements});
            }
            let request = match serde_json::from_value::<BuildRequest>(loaded.request) {
                Ok(request) => Box::new(request),
                Err(error) => {
                    return json!({"ok":false,"error":format!("Loaded request contract: {error}")});
                }
            };
            (
                request,
                output_directory,
                runtime_binary,
                (loaded.requirements, accept_requirements),
                full_report,
            )
        }
        Request::Build {
            request,
            output_directory,
            runtime_binary,
        } => (
            request,
            output_directory,
            runtime_binary,
            (vec![], vec![]),
            true,
        ),
        Request::Capabilities => {
            return json!({"ok":true,"native":true,"executesApplicationCode":true,"applicationExecutionDefault":false,"runtimeSelection":"explicitRuntimeBinary","writesByDefault":false,"pipeline":["resolvedPlan","serverBundle","nativeRemoteTransforms","featureTracking","staticAnalysis","nativeRuntimeAnalysis","nativeRemoteMetadata","optimizedClientBundle","kit3ManifestFinalization","nodesAndManifest","serviceWorker","adapterNode"],"bundleStrategy":"split","kitFamilies":[2,3],"adapterNodeVersion":"5.5.4","adapterNodeKit3Version":"6.0.0","unresolvedStages":["prerenderExecution"],"browserWasm":false});
        }
    };
    let load_ms = started.elapsed().as_millis() as u64;
    {
        {
            let mut report = if let Some(binary) = runtime_binary {
                let mut runtime = match ts_svelte_build::native_runtime::NativeRuntime::new(binary)
                {
                    Ok(runtime) => runtime,
                    Err(error) => return json!({"ok":false,"error":error}),
                };
                ts_svelte_build::build_with_runtime(*request, &mut runtime).await
            } else {
                ts_svelte_build::build(*request).await
            };
            report.add_loader_requirements(loaded.0, &loaded.1);
            let build_ms = started.elapsed().as_millis() as u64 - load_ms;
            if let Some(directory) = output_directory {
                if report.status != "complete" {
                    return json!({"ok":false,"error":"Output materialization requires a complete build report","report":report});
                }
                if let Err(error) = ts_svelte_build::write_new(&directory, &report.artifacts) {
                    return json!({"ok":false,"error":error,"report":report});
                }
                let write_ms = started.elapsed().as_millis() as u64 - load_ms - build_ms;
                let timings = json!({"loadMs":load_ms,"buildMs":build_ms,"writeMs":write_ms});
                // The files are on disk; the response lists them without content.
                let written = std::mem::take(&mut report.artifacts)
                    .into_iter()
                    .map(|(path, artifact)| (path, artifact.kind))
                    .collect::<std::collections::BTreeMap<_, _>>();
                if full_report {
                    json!({"ok":true,"outputDirectory":directory,"timings":timings,"written":written,"report":report})
                } else {
                    json!({"ok":true,"outputDirectory":directory,"timings":timings,"written":written,"report":{
                        "status":report.status,"phases":report.phases,
                        "requirements":report.requirements,
                        "acceptedRequirements":report.accepted_requirements,
                        "diagnostics":report.diagnostics,"serverBuildId":report.server_build_id,
                    }})
                }
            } else {
                json!({"ok":report.status=="complete","report":report})
            }
        }
    }
}
#[tokio::main]
async fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.iter().any(|arg| arg == "--help" || arg == "help") {
        println!(
            "svelte-build [--serve]\nRead one JSON request from stdin, or newline-delimited requests with --serve.\nOperations: capabilities; build {{request: resolvedBuildRequest, outputDirectory?: newAbsolutePath, runtimeBinary?: absoluteNativeRuntimePath}}; project {{project: {{root, environment, mode?, versionName?, define?}}, requestOnly?, outputDirectory?, runtimeBinary?, acceptRequirements?, fullReport?}}.\nNo output files are written unless outputDirectory is explicitly provided; written artifacts are listed without content."
        );
        return;
    }
    if args.iter().any(|arg| arg != "--serve") {
        eprintln!("Unknown argument; use --help");
        std::process::exit(2);
    }
    let mut input = io::stdin().lock();
    let mut output = io::BufWriter::new(io::stdout().lock());
    if args.iter().any(|arg| arg == "--serve") {
        loop {
            let mut bytes = Vec::new();
            let read = (&mut input)
                .take((MAX_REQUEST_BYTES + 1) as u64)
                .read_until(b'\n', &mut bytes);
            match read {
                Ok(0) => break,
                Ok(_) => {}
                Err(error) => {
                    eprintln!("{error}");
                    std::process::exit(2);
                }
            }
            if bytes.len() > MAX_REQUEST_BYTES {
                let _ = writeln!(
                    output,
                    "{}",
                    json!({"ok":false,"error":"Request exceeds 96 MiB"})
                );
                let _ = output.flush();
                std::process::exit(2);
            }
            if bytes.iter().all(u8::is_ascii_whitespace) {
                continue;
            }
            if writeln!(output, "{}", dispatch(&bytes).await)
                .and_then(|_| output.flush())
                .is_err()
            {
                std::process::exit(2);
            }
        }
    } else {
        let mut bytes = Vec::new();
        if let Err(error) = input
            .take((MAX_REQUEST_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
        {
            eprintln!("{error}");
            std::process::exit(2);
        }
        if bytes.len() > MAX_REQUEST_BYTES {
            eprintln!("Request exceeds 96 MiB");
            std::process::exit(2);
        }
        let response = dispatch(&bytes).await;
        let ok = response["ok"] == true;
        if writeln!(output, "{response}")
            .and_then(|_| output.flush())
            .is_err()
        {
            std::process::exit(2);
        }
        if !ok {
            std::process::exit(1);
        }
    }
}
