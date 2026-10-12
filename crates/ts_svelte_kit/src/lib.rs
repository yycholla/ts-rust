//! SvelteKit host integration. The compiler never depends on this crate.
//! Build orchestration currently requires an installed Node/Vite/Kit toolchain.
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};
/// Explicit host contract; no project configuration is evaluated by inspection.
pub struct BuildHost {
    pub node: PathBuf,
    pub loader: PathBuf,
    pub compiler: PathBuf,
    pub svelte_version: String,
}
impl BuildHost {
    /// Run a full installed Kit/Vite pipeline with native Svelte compilation.
    /// # Errors
    /// Returns I/O errors when the host cannot start; nonzero status is preserved.
    pub fn build(&self, project: &Path, trace: &Path) -> std::io::Result<ExitStatus> {
        let trace = if trace.is_absolute() {
            trace.to_owned()
        } else {
            std::env::current_dir()?.join(trace)
        };
        let loader = self
            .loader
            .to_str()
            .ok_or_else(|| std::io::Error::other("Loader path must be UTF-8"))?;
        if loader.chars().any(char::is_control) {
            return Err(std::io::Error::other(
                "Loader path contains control characters",
            ));
        }
        let loader_option = format!(
            "--import={}",
            serde_json::to_string(loader).map_err(std::io::Error::other)?
        );
        if trace.exists() {
            return Err(std::io::Error::other("Build trace must be a new file"));
        }
        let status = Command::new(&self.node)
            .arg(project.join("node_modules/vite/bin/vite.js"))
            .arg("build")
            .arg("--configLoader")
            .arg("runner")
            .current_dir(project)
            .env("NODE_OPTIONS", loader_option)
            .env("TS_SVELTE_BINARY", &self.compiler)
            .env("TS_SVELTE_VERSION", &self.svelte_version)
            .env("TS_SVELTE_BUILD_TRACE", &trace)
            .status()?;
        if status.success() {
            use std::io::BufRead;
            let file = std::fs::File::open(&trace)?;
            if file.metadata()?.len() > 64 * 1024 * 1024 {
                return Err(std::io::Error::other("Build trace exceeds 64 MiB"));
            }
            let mut calls = std::collections::BTreeMap::<String, i64>::new();
            let mut completed = 0;
            for line in std::io::BufReader::new(file).lines() {
                let entry: serde_json::Value =
                    serde_json::from_str(&line?).map_err(std::io::Error::other)?;
                let key = serde_json::json!([entry["op"], entry["filename"], entry["target"]])
                    .to_string();
                if entry["event"] == "start" {
                    *calls.entry(key).or_default() += 1;
                } else if entry["ok"] == true {
                    *calls.entry(key).or_default() -= 1;
                    completed += 1;
                } else {
                    return Err(std::io::Error::other(
                        "Native compiler trace contains a failure",
                    ));
                }
            }
            if completed == 0 || calls.values().any(|n| *n != 0) {
                return Err(std::io::Error::other(
                    "Missing or incomplete native compilation trace",
                ));
            }
        }
        Ok(status)
    }
}
mod jsonc;
pub mod project;

pub mod sync;
pub mod sync_ambient;
pub mod sync_config;
pub mod sync_env;
pub mod sync_types;
pub mod sync_versions;

pub mod sync_config_v3;

pub mod build_manifest;

pub mod build_server;

pub mod build_root;

pub mod build_inventory;

pub mod build_ssr_manifest;

pub mod build_nodes;

pub mod adapter_node;
pub mod build_analysis;
pub mod build_dependencies;
pub mod build_env;
pub mod build_guards;
pub mod build_plan;

pub mod build_instrumentation;

pub mod build_finalize;
pub mod build_remote;
pub mod build_service_worker;

pub mod build_tracking;
pub mod prerender_crawl;
pub mod prerender_queue;
pub mod prerender_save;
pub mod vite_env;
pub mod vite_ssr;
