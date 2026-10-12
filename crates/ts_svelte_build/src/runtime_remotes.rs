//! Strict data boundary for native `analyzeRemotes` result receipts.
//! Matches the emitted server build and ordered remote inventory; never infers
//! exports from source, coerces values, or reruns remote module initialization.
//! Dynamic values are retained as tagged booleans/undefined. Other runtime JS
//! values can exist, but cannot satisfy Kit's typed remote dynamic option and
//! are rejected explicitly instead of being silently discarded for the client.
use serde::Serialize;
use serde_json::{Map, Value};
use std::collections::BTreeSet;
use ts_svelte_kit::build_remote::{
    EvaluatedRemoteExports, RemoteExport, RemoteIdentity, RemoteKind, validate_remote_identities,
};

const MAX_REMOTES: usize = 16_384;
const MAX_EXPORTS: usize = 16_384;
const MAX_TOTAL_EXPORTS: usize = 65_536;
const MAX_STRING: usize = 1024 * 1024;
const MAX_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", content = "value", rename_all = "camelCase")]
pub enum RuntimeDynamic {
    Undefined,
    Json(bool),
}
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ValidatedRemoteExport {
    pub name: String,
    pub kind: RemoteKind,
    pub dynamic: RuntimeDynamic,
}
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ValidatedRemoteMetadata {
    pub hash: String,
    pub file: String,
    pub exports: Vec<ValidatedRemoteExport>,
}
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeRemotes {
    pub server_build_id: String,
    /// Direct input to the native client remote transform, in receipt order.
    pub evaluated: Vec<EvaluatedRemoteExports>,
    /// Complete accepted dynamic facts; client generation must not discard them
    /// from the build report or later prerender decisions.
    pub remotes: Vec<ValidatedRemoteMetadata>,
}
fn fields<'a>(
    value: &'a Value,
    keys: &[&str],
    label: &str,
) -> Result<&'a Map<String, Value>, String> {
    let object = value
        .as_object()
        .ok_or_else(|| format!("{label} must be an object"))?;
    if object.len() != keys.len() || keys.iter().any(|key| !object.contains_key(*key)) {
        return Err(format!("{label} must contain exactly {}", keys.join(", ")));
    }
    Ok(object)
}
fn text<'a>(value: &'a Value, label: &str, budget: &mut usize) -> Result<&'a str, String> {
    let text = value
        .as_str()
        .ok_or_else(|| format!("{label} must be a string"))?;
    charge(text, label, budget)?;
    Ok(text)
}
fn charge(value: &str, label: &str, budget: &mut usize) -> Result<(), String> {
    if value.len() > MAX_STRING {
        return Err(format!("{label} exceeds native string limit"));
    }
    *budget = budget
        .checked_sub(value.len())
        .ok_or_else(|| "Runtime remote receipt exceeds native byte limit".to_owned())?;
    Ok(())
}
fn kind(value: &Value) -> Result<RemoteKind, String> {
    let object = fields(value, &["kind", "value"], "Remote kind tag")?;
    if object["kind"].as_str() != Some("json") {
        return Err("Remote kind must be a tagged JSON string".into());
    }
    match object["value"].as_str() {
        Some("command") => Ok(RemoteKind::Command),
        Some("form") => Ok(RemoteKind::Form),
        Some("prerender") => Ok(RemoteKind::Prerender),
        Some("query") => Ok(RemoteKind::Query),
        Some("query_batch") => Ok(RemoteKind::QueryBatch),
        Some("query_live") => Ok(RemoteKind::QueryLive),
        _ => Err("Remote kind is not one of the six supported runtime remote types".into()),
    }
}
fn dynamic(value: &Value, kind: RemoteKind) -> Result<RuntimeDynamic, String> {
    let object = value
        .as_object()
        .ok_or_else(|| "Remote dynamic must be a tagged value".to_owned())?;
    let dynamic = match object.get("kind").and_then(Value::as_str) {
        Some("undefined") => {
            fields(value, &["kind"], "Remote undefined tag")?;
            RuntimeDynamic::Undefined
        }
        Some("json") => {
            let object = fields(value, &["kind", "value"], "Remote dynamic tag")?;
            RuntimeDynamic::Json(object["value"].as_bool().ok_or_else(||"Remote dynamic JSON value must be a boolean; other values cannot be used for typed prerender metadata".to_owned())?)
        }
        _ => return Err("Remote dynamic must be tagged undefined or JSON boolean".into()),
    };
    if kind != RemoteKind::Prerender && dynamic != RuntimeDynamic::Json(true) {
        return Err("Non-prerender remote dynamic must be tagged JSON true".into());
    }
    Ok(dynamic)
}
/// Validate the successful `analyzeRemotes` result, not its protocol envelope.
/// `expected` is the unique, ordered emitted remote entry inventory. A receipt
/// replayed against another build/inventory fails even if some names coincide.
/// Returns nothing until every record passes; caller state remains unchanged.
pub fn validate(
    server_build_id: &str,
    expected: &[RemoteIdentity],
    value: &Value,
) -> Result<RuntimeRemotes, String> {
    if server_build_id.is_empty() {
        return Err("Explicit server build identity required for remote receipt".into());
    }
    if expected.len() > MAX_REMOTES {
        return Err("Remote inventory exceeds native module limit".into());
    }
    let mut budget = MAX_BYTES;
    charge(
        server_build_id,
        "Expected server build identity",
        &mut budget,
    )?;
    let mut hashes = BTreeSet::new();
    let mut files = BTreeSet::new();
    let mut modules = BTreeSet::new();
    for identity in expected {
        charge(&identity.hash, "Expected remote hash", &mut budget)?;
        charge(&identity.file, "Expected remote file", &mut budget)?;
        charge(&identity.module_id, "Expected remote module", &mut budget)?;
        if !hashes.insert(&identity.hash)
            || !files.insert(&identity.file)
            || !modules.insert(&identity.module_id)
        {
            return Err(
                "Expected remote inventory contains duplicate or ambiguous identities".into(),
            );
        }
    }
    validate_remote_identities(expected)?;
    let mut budget = MAX_BYTES;
    let receipt = fields(
        value,
        &["serverBuildId", "remotes"],
        "Runtime remote receipt",
    )?;
    let build = text(
        &receipt["serverBuildId"],
        "Runtime server build identity",
        &mut budget,
    )?;
    if build != server_build_id {
        return Err("Runtime remote server build identity mismatch".into());
    }
    let remotes = receipt["remotes"]
        .as_array()
        .ok_or_else(|| "Runtime remotes must be an array".to_owned())?;
    if remotes.len() != expected.len() {
        return Err("Runtime remote inventory count mismatch".into());
    }
    let mut evaluated = Vec::with_capacity(expected.len());
    let mut retained = Vec::with_capacity(expected.len());
    let mut total = 0usize;
    for (index, (remote, expected)) in remotes.iter().zip(expected).enumerate() {
        let remote = fields(
            remote,
            &["hash", "file", "exports"],
            "Runtime remote module",
        )?;
        let hash = text(&remote["hash"], "Runtime remote hash", &mut budget)?;
        let file = text(&remote["file"], "Runtime remote file", &mut budget)?;
        if hash != expected.hash || file != expected.file {
            return Err(format!(
                "Runtime remote inventory mismatch at index {index}: expected {} ({})",
                expected.file, expected.hash
            ));
        }
        let exports = remote["exports"]
            .as_array()
            .ok_or_else(|| "Runtime remote exports must be an array".to_owned())?;
        total = total
            .checked_add(exports.len())
            .ok_or_else(|| "Remote export count overflow".to_owned())?;
        if exports.len() > MAX_EXPORTS || total > MAX_TOTAL_EXPORTS {
            return Err("Runtime remote exports exceed native limit".into());
        }
        let mut names = BTreeSet::new();
        let mut client = Vec::with_capacity(exports.len());
        let mut metadata = Vec::with_capacity(exports.len());
        for export in exports {
            let export = fields(
                export,
                &["name", "kind", "dynamic"],
                "Runtime remote export",
            )?;
            let name = text(&export["name"], "Runtime remote export name", &mut budget)?;
            if !names.insert(name) {
                return Err(format!(
                    "Duplicate runtime remote export {name:?} in {file}"
                ));
            }
            let kind = kind(&export["kind"])?;
            let dynamic = dynamic(&export["dynamic"], kind)?;
            client.push(RemoteExport {
                name: name.into(),
                kind,
            });
            metadata.push(ValidatedRemoteExport {
                name: name.into(),
                kind,
                dynamic,
            });
        }
        evaluated.push(EvaluatedRemoteExports {
            hash: hash.into(),
            file: file.into(),
            exports: client,
        });
        retained.push(ValidatedRemoteMetadata {
            hash: hash.into(),
            file: file.into(),
            exports: metadata,
        });
    }
    Ok(RuntimeRemotes {
        server_build_id: build.into(),
        evaluated,
        remotes: retained,
    })
}
#[cfg(test)]
#[path = "runtime_remotes_tests.rs"]
mod tests;
