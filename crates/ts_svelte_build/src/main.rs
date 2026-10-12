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
}
async fn dispatch(bytes: &[u8]) -> Value {
    let input = match serde_json::from_slice::<Request>(bytes) {
        Ok(input) => input,
        Err(error) => return json!({"ok":false,"error":error.to_string()}),
    };
    match input {
        Request::Capabilities => {
            json!({"ok":true,"native":true,"executesApplicationCode":true,"applicationExecutionDefault":false,"runtimeSelection":"explicitRuntimeBinary","writesByDefault":false,"pipeline":["resolvedPlan","serverBundle","nativeRemoteTransforms","featureTracking","staticAnalysis","nativeRuntimeAnalysis","nativeRemoteMetadata","optimizedClientBundle","kit3ManifestFinalization","nodesAndManifest","serviceWorker","adapterNode"],"bundleStrategy":"split","kitFamilies":[2,3],"adapterNodeVersion":"5.5.4","unresolvedStages":["prerenderExecution"],"browserWasm":false})
        }
        Request::Build {
            request,
            output_directory,
            runtime_binary,
        } => {
            let report = if let Some(binary) = runtime_binary {
                let mut runtime = match ts_svelte_build::native_runtime::NativeRuntime::new(binary)
                {
                    Ok(runtime) => runtime,
                    Err(error) => return json!({"ok":false,"error":error}),
                };
                ts_svelte_build::build_with_runtime(*request, &mut runtime).await
            } else {
                ts_svelte_build::build(*request).await
            };
            if let Some(directory) = output_directory {
                if report.status != "complete" {
                    return json!({"ok":false,"error":"Output materialization requires a complete build report","report":report});
                }
                if let Err(error) = ts_svelte_build::write_new(&directory, &report.artifacts) {
                    return json!({"ok":false,"error":error,"report":report});
                }
                json!({"ok":true,"outputDirectory":directory,"report":report})
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
            "svelte-build [--serve]\nRead one JSON request from stdin, or newline-delimited requests with --serve.\nOperations: capabilities; build {{request: resolvedBuildRequest, outputDirectory?: newAbsolutePath, runtimeBinary?: absoluteNativeRuntimePath}}.\nNo output files are written unless outputDirectory is explicitly provided."
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
