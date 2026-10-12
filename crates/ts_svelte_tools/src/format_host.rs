//! Native formatter process boundary. The formatter uses a separately pinned
//! OXC AST; its dependency graph must not change the checker/plugin AST identity.
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};
use ts_svelte::Snapshot;

const LIMIT: usize = 8 * 1024 * 1024;

fn executable() -> Result<PathBuf, String> {
    std::env::current_exe()
        .map(|path| path.with_file_name(format!("svelte-format{}", std::env::consts::EXE_SUFFIX)))
        .map_err(|error| error.to_string())
}

pub fn available() -> bool {
    executable().is_ok_and(|path| path.is_file())
}

/// One bounded operation, always reaping the child. Source/config travel over
/// pipes; no project config is evaluated and no source file is written.
pub fn format(document: &Snapshot, lsp_options: &Value) -> Result<Value, String> {
    let insert_spaces = lsp_options["insertSpaces"]
        .as_bool()
        .ok_or("Formatting requires insertSpaces")?;
    let tab_width = lsp_options["tabSize"]
        .as_u64()
        .filter(|width| (1..=16).contains(width))
        .ok_or("Formatting tabSize must be between 1 and 16")?;
    let request = serde_json::to_vec(&json!({"op":"format","document":document,
        "filename":document.uri,"options":{"useTabs":!insert_spaces,"tabWidth":tab_width}}))
    .map_err(|error| error.to_string())?;
    if request.len() > LIMIT {
        return Err("Formatter request exceeds 8 MiB".into());
    }
    let started = Instant::now();
    let mut child = Command::new(executable()?)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("Cannot start native formatter: {error}"))?;
    let mut input = child.stdin.take().expect("piped stdin");
    let output = child.stdout.take().expect("piped stdout");
    let (sender, receiver) = mpsc::channel();
    let transport = std::thread::spawn(move || {
        let result = (|| -> Result<Value, String> {
            input.write_all(&request).map_err(|e| e.to_string())?;
            drop(input);
            let mut bytes = Vec::new();
            output
                .take(LIMIT as u64 + 1)
                .read_to_end(&mut bytes)
                .map_err(|e| e.to_string())?;
            if bytes.len() > LIMIT {
                return Err("Formatter response exceeds 8 MiB".into());
            }
            serde_json::from_slice(&bytes).map_err(|e| format!("Invalid formatter response: {e}"))
        })();
        let _ = sender.send(result);
    });
    let response = receiver.recv_timeout(Duration::from_secs(30));
    // The reader also bounds the output before EOF; kill on oversized output,
    // malformed response, blocked I/O or timeout so joining cannot hang.
    if !matches!(&response, Ok(Ok(_))) {
        let _ = child.kill();
    }
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) if started.elapsed() < Duration::from_secs(30) => {
                std::thread::sleep(Duration::from_millis(5));
            }
            result => {
                let _ = child.kill();
                let _ = child.wait();
                break Err(match result {
                    Err(error) => error.to_string(),
                    _ => "Native formatter did not exit within 30 seconds".into(),
                });
            }
        }
    };
    let joined = transport.join();
    let response = response.map_err(|_| "Native formatting exceeded 30 seconds")??;
    if joined.is_err() {
        return Err("Formatter transport failed".into());
    }
    if response["ok"] != true {
        return Err(response["error"]["message"]
            .as_str()
            .unwrap_or("Native formatting failed")
            .into());
    }
    if !status?.success() {
        return Err("Native formatter returned an unsuccessful exit status".into());
    }
    let edits = response["result"]["edits"]
        .as_array()
        .ok_or("Formatter response has no edits")?;
    Ok(Value::Array(
        edits
            .iter()
            .map(|edit| json!({"range":edit["range"],"newText":edit["newText"]}))
            .collect(),
    ))
}
