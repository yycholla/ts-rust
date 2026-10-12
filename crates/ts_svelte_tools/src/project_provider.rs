//! Framework plugins can rediscover an explicit project without linking their
//! framework-specific graph rules into the Svelte editor.
use serde::Deserialize;
use serde_json::Value;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

#[cfg(unix)]
pub(crate) static ACTIVE_PROVIDER: std::sync::Mutex<Option<u32>> = std::sync::Mutex::new(None);

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProjectProvider {
    /// Resolved by the host at initialization, never taken from a watched file.
    pub executable: PathBuf,
    pub arguments: Vec<String>,
}
impl ProjectProvider {
    pub fn validate(&self) -> Result<(), String> {
        if !self.executable.is_absolute() || !self.executable.is_file() {
            return Err("Project provider must name an absolute executable file".into());
        }
        if self.arguments.len() > 32
            || self
                .arguments
                .iter()
                .any(|arg| arg.len() > 8192 || arg.contains('\0'))
        {
            return Err("Project provider arguments exceed limits".into());
        }
        Ok(())
    }

    /// Invoke the configured native plugin and obtain a fresh explicit graph.
    /// No shell interpolation or JavaScript configuration evaluation occurs here.
    pub fn discover(&self, root: &Path) -> Result<Value, String> {
        self.validate()?;
        let mut command = Command::new(&self.executable);
        command
            .args(&self.arguments)
            .current_dir(root)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        #[cfg(unix)]
        let mut active = ACTIVE_PROVIDER
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut child = command.spawn().map_err(|e| e.to_string())?;
        #[cfg(unix)]
        {
            *active = Some(child.id());
            drop(active);
        }
        let Some(stdout) = child.stdout.take() else {
            let _ = child.kill();
            let _ = child.wait();
            #[cfg(unix)]
            {
                *ACTIVE_PROVIDER
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
            }
            return Err("Project provider has no stdout".into());
        };
        let (sender, receiver) = mpsc::channel();
        let reader = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let result = stdout
                .take(64 * 1024 * 1024 + 1)
                .read_to_end(&mut bytes)
                .map_err(|e| e.to_string())
                .and_then(|_| {
                    if bytes.len() > 64 * 1024 * 1024 {
                        Err("Project provider response exceeds 64 MiB".to_owned())
                    } else {
                        serde_json::from_slice::<Value>(&bytes).map_err(|e| e.to_string())
                    }
                });
            let _ = sender.send(result);
        });
        let deadline = Instant::now() + Duration::from_secs(30);
        let result = receiver
            .recv_timeout(Duration::from_secs(30))
            .map_err(|_| "Project provider timed out or failed to read its response".to_owned())
            .and_then(|result| result);
        // A provider must terminate after writing its JSON response. Kill any
        // remaining group before reaping, also bounding oversized output/errors.
        let mut status = child.try_wait().map_err(|e| e.to_string());
        while result.is_ok() && matches!(status, Ok(None)) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
            status = child.try_wait().map_err(|e| e.to_string());
        }
        if result.is_err() || !matches!(status, Ok(Some(_))) {
            #[cfg(unix)]
            {
                let _ = Command::new("kill")
                    .args(["-KILL", "--", &format!("-{}", child.id())])
                    .status();
            }
            let _ = child.kill();
        }
        let final_status = child.wait().map_err(|e| e.to_string());
        #[cfg(unix)]
        {
            *ACTIVE_PROVIDER
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        }
        let _ = reader.join();
        let response = result?;
        let final_status = final_status?;
        if !final_status.success() {
            return Err(format!(
                "Project provider exited with {final_status}: {}",
                response
                    .pointer("/error/message")
                    .and_then(Value::as_str)
                    .unwrap_or("Discovery failed")
            ));
        }
        Ok(response)
    }
}
