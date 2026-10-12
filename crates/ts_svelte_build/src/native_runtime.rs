//! Explicit process transport to this project's native framework runtime.
//! The selected executable is supplied by the caller, never by project config.
use crate::execution_snapshot::ExecutionSnapshot;
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Read, Write},
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::mpsc::{self, Receiver, Sender},
    thread::JoinHandle,
    time::{Duration, Instant},
};
use ts_svelte_kit::build_remote::RemoteIdentity;

/// Trusted embedding boundary. Returned facts are validated against this build's
/// identity and inventory before they can replace static analysis requirements.
pub trait MetadataEvaluator {
    fn analyze(
        &mut self,
        snapshot: &ExecutionSnapshot,
        kit_version: &str,
        hash: bool,
    ) -> Result<Value, String>;
    fn analyze_with_remotes(
        &mut self,
        snapshot: &ExecutionSnapshot,
        kit_version: &str,
        hash: bool,
        remotes: &[RemoteIdentity],
    ) -> Result<MetadataReceipt, String> {
        if !remotes.is_empty() {
            return Err("The selected metadata evaluator does not support remote analysis".into());
        }
        Ok(MetadataReceipt {
            routes: self.analyze(snapshot, kit_version, hash)?,
            remotes: None,
        })
    }
}

pub struct MetadataReceipt {
    pub routes: Value,
    pub remotes: Option<Value>,
}

pub struct NativeRuntime {
    executable: PathBuf,
    timeout: Duration,
}
impl NativeRuntime {
    pub fn new(executable: PathBuf) -> Result<Self, String> {
        if !executable.is_absolute() || !executable.is_file() {
            return Err("Native runtime executable must be an existing absolute file path".into());
        }
        Ok(Self {
            executable,
            timeout: Duration::from_secs(60),
        })
    }
}
impl MetadataEvaluator for NativeRuntime {
    fn analyze(
        &mut self,
        snapshot: &ExecutionSnapshot,
        kit_version: &str,
        hash: bool,
    ) -> Result<Value, String> {
        Ok(self
            .analyze_with_remotes(snapshot, kit_version, hash, &[])?
            .routes)
    }
    fn analyze_with_remotes(
        &mut self,
        snapshot: &ExecutionSnapshot,
        kit_version: &str,
        hash: bool,
        remotes: &[RemoteIdentity],
    ) -> Result<MetadataReceipt, String> {
        let mut process = Process::start(self)?;
        let created =
            process.request(json!({"op":"create","bootstrap":snapshot.bootstrap_json()?}))?;
        if created["serverBuildId"] != snapshot.bootstrap.server_build_id {
            return Err("Native runtime initialized a different server build".into());
        }
        let session = created["session"]
            .as_str()
            .ok_or("Native runtime omitted session identity")?;
        let result = process.request(json!({"op":"analyzeMetadata","session":session,"options":{"profile":kit_version,"hash":hash}}))?;
        let remotes = if remotes.is_empty() {
            None
        } else {
            let chunks: Vec<_> = remotes
                .iter()
                .map(|remote| json!({"hash":remote.hash,"file":remote.file}))
                .collect();
            Some(
                process
                    .request(json!({"op":"analyzeRemotes","session":session,"chunks":chunks}))?,
            )
        };
        let disposed = process.request(json!({"op":"dispose","session":session}))?;
        if disposed["disposed"] != true {
            return Err("Native runtime did not dispose the analysis session".into());
        }
        Ok(MetadataReceipt {
            routes: result,
            remotes,
        })
    }
}

pub(super) struct Process {
    child: Child,
    requests: Option<Sender<Vec<u8>>>,
    responses: Receiver<Result<Value, String>>,
    threads: Vec<JoinHandle<()>>,
    timeout: Duration,
}
impl Process {
    pub(super) fn start(runtime: &NativeRuntime) -> Result<Self, String> {
        let mut child = Command::new(&runtime.executable)
            .arg("--serve")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| format!("Start native framework runtime: {e}"))?;
        let mut input = child.stdin.take().ok_or("Native runtime stdin missing")?;
        let output = child.stdout.take().ok_or("Native runtime stdout missing")?;
        let (requests, pending) = mpsc::channel::<Vec<u8>>();
        let (sender, responses) = mpsc::channel();
        let write_errors = sender.clone();
        let writer = std::thread::spawn(move || {
            for bytes in pending {
                if let Err(error) = input
                    .write_all(&bytes)
                    .and_then(|_| input.write_all(b"\n"))
                    .and_then(|_| input.flush())
                {
                    let _ =
                        write_errors.send(Err(format!("Write native framework request: {error}")));
                    break;
                }
            }
        });
        let reader = std::thread::spawn(move || {
            let mut output = BufReader::new(output);
            const MAX_RESPONSE: usize = 8 * 1024 * 1024;
            loop {
                let mut line = vec![];
                let response = match (&mut output)
                    .take((MAX_RESPONSE + 1) as u64)
                    .read_until(b'\n', &mut line)
                {
                    Ok(0) => {
                        let _ = sender.send(Err("Native framework runtime closed stdout".into()));
                        break;
                    }
                    Ok(_) if line.len() > MAX_RESPONSE => {
                        Err("Native framework response exceeds 8 MiB".into())
                    }
                    Ok(_) => serde_json::from_slice(&line)
                        .map_err(|e| format!("Invalid native framework response: {e}")),
                    Err(error) => Err(format!("Read native framework response: {error}")),
                };
                let failed = response.is_err();
                if sender.send(response).is_err() || failed {
                    break;
                }
            }
        });
        Ok(Self {
            child,
            requests: Some(requests),
            responses,
            threads: vec![writer, reader],
            timeout: runtime.timeout,
        })
    }
    pub(super) fn request(&mut self, request: Value) -> Result<Value, String> {
        self.request_with_callback(request, |_| {
            Err("Unexpected native policy callback for this operation".into())
        })
    }
    /// A callback is synchronous: its reply is sent before the child resumes
    /// the promise continuation. The outer operation has one fixed deadline,
    /// so a stream of callbacks cannot indefinitely extend a hung operation.
    pub(super) fn request_with_callback(
        &mut self,
        request: Value,
        mut callback: impl FnMut(Value) -> Result<Value, String>,
    ) -> Result<Value, String> {
        let deadline = Instant::now() + self.timeout;
        self.send(request)?;
        let mut sequence = 0u64;
        loop {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .ok_or("Native framework operation timed out")?;
            let response = self
                .responses
                .recv_timeout(remaining)
                .map_err(|e| format!("Native framework response timeout or disconnect: {e}"))??;
            if response.get("hostEvent").is_some() {
                let reply = host_reply(response, &mut sequence, &mut callback)?;
                self.send(reply)?;
            } else {
                return result(response);
            }
        }
    }
    fn send(&self, request: Value) -> Result<(), String> {
        let bytes = serde_json::to_vec(&request).map_err(|e| e.to_string())?;
        if bytes.len() + 1 > crate::MAX_REQUEST_BYTES {
            return Err("Native framework request exceeds 96 MiB".into());
        }
        self.requests
            .as_ref()
            .ok_or("Native framework transport is closed")?
            .send(bytes)
            .map_err(|e| format!("Queue native framework request: {e}"))
    }
}
fn result(mut response: Value) -> Result<Value, String> {
    match response.get("ok").and_then(Value::as_bool) {
        Some(true) => response
            .as_object_mut()
            .and_then(|r| r.remove("result"))
            .ok_or_else(|| "Native framework response omitted result".into()),
        Some(false) => Err(format!(
            "Native framework execution failed: {}",
            response["error"]
        )),
        None => Err("Native framework response omitted boolean status".into()),
    }
}

fn host_reply(
    frame: Value,
    sequence: &mut u64,
    callback: &mut impl FnMut(Value) -> Result<Value, String>,
) -> Result<Value, String> {
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct Envelope {
        host_event: Event,
    }
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Event {
        sequence: u64,
        event: Value,
    }
    let frame: Envelope =
        serde_json::from_value(frame).map_err(|e| format!("Invalid native policy event: {e}"))?;
    if *sequence >= 100_000 || frame.host_event.sequence != *sequence + 1 {
        return Err("Native policy event has stale, unexpected, or exhausted sequence".into());
    }
    *sequence += 1;
    let outcome = match callback(frame.host_event.event) {
        Ok(value) => json!({"kind":"return", "value":value}),
        Err(message) => json!({"kind":"throw", "message":message}),
    };
    Ok(json!({"hostReply":{"sequence":sequence,"outcome":outcome}}))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn host_callbacks_preserve_binary_payloads_and_return_failures_to_child() {
        let mut sequence = 0;
        let returned = host_reply(
            json!({"hostEvent":{"sequence":1,"event":{"body":[0,128,255]}}}),
            &mut sequence,
            &mut |v| Ok(v),
        )
        .unwrap();
        assert_eq!(
            returned,
            json!({"hostReply":{"sequence":1,"outcome":{"kind":"return","value":{"body":[0,128,255]}}}})
        );
        let failed = host_reply(
            json!({"hostEvent":{"sequence":2,"event":null}}),
            &mut sequence,
            &mut |_| Err("cannot save".into()),
        )
        .unwrap();
        assert_eq!(
            failed,
            json!({"hostReply":{"sequence":2,"outcome":{"kind":"throw","message":"cannot save"}}})
        );
    }
    #[test]
    fn invalid_callback_frames_never_execute_host_work() {
        for frame in [
            json!({"hostEvent":{"sequence":0,"event":null}}),
            json!({"hostEvent":{"sequence":2,"event":null}}),
            json!({"hostEvent":{"sequence":1,"event":null,"extra":true}}),
            json!({"hostEvent":{"sequence":1,"event":null},"ok":true}),
            json!({"hostEvent":{"sequence":1}}),
        ] {
            assert!(
                host_reply(frame, &mut 0, &mut |_| panic!(
                    "invalid frame invoked callback"
                ))
                .is_err()
            );
        }
    }
}
impl Drop for Process {
    fn drop(&mut self) {
        self.requests.take();
        let _ = self.child.kill();
        let _ = self.child.wait();
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}
