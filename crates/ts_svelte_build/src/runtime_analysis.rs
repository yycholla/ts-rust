//! Strict data-only boundary for a successful native analyzeMetadata result.
//! Runtime route entries are resolved URLs, not the exported entries callback:
//! keep them separate from AnalysisOutput and preserve all original module facts.
use crate::BuildRequest;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use ts_svelte_kit::{
    build_analysis::{AnalysisOutput, Fact},
    sync_versions::KitSyncVersion,
};

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(
    tag = "kind",
    content = "value",
    rename_all = "camelCase",
    deny_unknown_fields
)]
pub enum RuntimeEntries {
    Undefined,
    /// Either the unchanged falsy export or the actual resolved URL array.
    Json(Value),
}
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeAnalysis {
    pub analysis: AnalysisOutput,
    pub resolved_entries: BTreeMap<String, RuntimeEntries>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Metadata {
    server_build_id: String,
    nodes: Vec<Node>,
    routes: Vec<Route>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Node {
    index: usize,
    has_server_load: bool,
    has_universal_load: bool,
    server_load_callable: bool,
    universal_load_callable: bool,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Route {
    id: String,
    config: RuntimeEntries,
    prerender: RuntimeEntries,
    entries: RuntimeEntries,
    methods: Vec<String>,
    page_methods: Vec<String>,
    endpoint_methods: Vec<String>,
    page: Option<Page>,
    endpoint: Option<Endpoint>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Endpoint {
    trailing_slash: RuntimeEntries,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Page {
    csr: RuntimeEntries,
    ssr: RuntimeEntries,
    prerender: RuntimeEntries,
    trailing_slash: RuntimeEntries,
    actions: Vec<Action>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Action {
    name: String,
    callable: bool,
}
fn known<T>(value: T) -> Fact<T> {
    Fact::Known { value }
}
fn is_known<T>(fact: &Fact<T>) -> bool {
    matches!(fact, Fact::Known { .. })
}
fn boolean(value: &RuntimeEntries, label: &str) -> Result<bool, String> {
    match value {
        RuntimeEntries::Json(Value::Bool(value)) => Ok(*value),
        _ => Err(format!("{label}: expected a tagged JSON boolean")),
    }
}
fn prerender(value: &RuntimeEntries, endpoint_only: bool) -> Result<Value, String> {
    match value {
        RuntimeEntries::Json(value @ Value::Bool(_)) => Ok(value.clone()),
        RuntimeEntries::Json(Value::String(value)) if value == "auto" => Ok(value.clone().into()),
        RuntimeEntries::Undefined if endpoint_only => Ok(Value::Bool(false)),
        _ => Err("Runtime prerender must be false, true or auto".into()),
    }
}
fn finite_json(value: &Value, depth: usize, remaining: &mut usize) -> Result<(), String> {
    if depth > 128 || *remaining == 0 {
        return Err("Runtime metadata JSON exceeds structural bounds".into());
    }
    *remaining -= 1;
    match value {
        // Build dependencies enable serde_json arbitrary_precision. Parsing a
        // Value therefore does not establish that its numbers can cross the
        // ECMAScript Number boundary. Check every nested value before typed
        // deserialization, including config objects and falsy entries exports.
        Value::Number(n) if n.as_f64().is_none_or(|n| !n.is_finite()) => {
            return Err("Runtime metadata number is outside finite JavaScript Number range".into());
        }
        Value::Array(values) => {
            for value in values {
                finite_json(value, depth + 1, remaining)?;
            }
        }
        Value::Object(values) => {
            for value in values.values() {
                finite_json(value, depth + 1, remaining)?;
            }
        }
        _ => {}
    }
    Ok(())
}
fn validate_entries(entries: &RuntimeEntries) -> Result<(), String> {
    match entries {
        RuntimeEntries::Undefined | RuntimeEntries::Json(Value::Null | Value::Bool(false)) => {
            Ok(())
        }
        RuntimeEntries::Json(Value::Number(n)) if n.as_f64() == Some(0.0) => Ok(()),
        RuntimeEntries::Json(Value::String(s)) if s.is_empty() => Ok(()),
        RuntimeEntries::Json(Value::Array(values)) if values.iter().all(Value::is_string) => Ok(()),
        _ => {
            Err("Runtime entries must be undefined, a falsy export, or resolved URL strings".into())
        }
    }
}
fn methods(values: &[String], allowed: &[&str], label: &str) -> Result<(), String> {
    let mut last = None;
    for value in values {
        let index = allowed
            .iter()
            .position(|method| *method == value)
            .ok_or_else(|| format!("{label}: invalid HTTP method {value:?}"))?;
        if last.is_some_and(|last| index <= last) {
            return Err(format!("{label}: duplicate or reordered HTTP methods"));
        }
        last = Some(index);
    }
    Ok(())
}

/// `metadata` is the successful analyzeMetadata **result**, not its protocol
/// envelope. The caller supplies the identity of the actual emitted server.
/// No mutation is returned unless the entire receipt is validated.
pub fn merge(
    request: &BuildRequest,
    server_build_id: &str,
    original: &AnalysisOutput,
    metadata: &Value,
) -> Result<RuntimeAnalysis, String> {
    let mut budget = 1_000_000;
    finite_json(metadata, 0, &mut budget)?;
    if let Some(routes) = metadata.get("routes").and_then(Value::as_array) {
        for route in routes {
            if route.get("page").is_none() || route.get("endpoint").is_none() {
                return Err(
                    "Runtime route must explicitly include page and endpoint presence".into(),
                );
            }
        }
    }
    let metadata: Metadata = serde_json::from_value(metadata.clone())
        .map_err(|e| format!("Invalid runtime metadata: {e}"))?;
    if server_build_id.is_empty() || metadata.server_build_id != server_build_id {
        return Err("Runtime metadata server build identity mismatch".into());
    }
    let inventory = &request.kit.inventory;
    let version = KitSyncVersion::parse(&inventory.kit_version)?;
    // This receipt covers route/node metadata only. The build independently
    // validates remote metadata against the same server identity before either
    // receipt is committed, so remote modules do not invalidate route facts.
    if inventory.nodes.len() < 2 || original.nodes.len() != inventory.nodes.len() {
        return Err("Static analysis node inventory mismatch".into());
    }
    let mut used = BTreeSet::from([0, 1]);
    let mut ids = BTreeSet::new();
    for route in &inventory.routes {
        if !ids.insert(route.id.clone()) {
            return Err(format!("Duplicate inventory route {}", route.id));
        }
        if let Some(page) = &route.page {
            used.extend(
                std::iter::once(page.leaf)
                    .chain(page.layouts.iter().chain(&page.errors).flatten().copied()),
            );
        }
    }
    if ids != original.routes.keys().cloned().collect::<BTreeSet<_>>() {
        return Err("Static analysis route inventory mismatch".into());
    }
    if used.iter().any(|index| *index >= inventory.nodes.len()) {
        return Err("Inventory page refers to a missing node".into());
    }
    if metadata.nodes.len() != used.len() {
        return Err("Runtime metadata omitted or added manifest nodes".into());
    }
    let mut output = original.clone();
    for (index, node) in used.iter().zip(&metadata.nodes) {
        if node.index != *index {
            return Err("Runtime metadata node indices are duplicate or reordered".into());
        }
        let declared = &inventory.nodes[*index];
        if node.server_load_callable && !node.has_server_load
            || node.universal_load_callable && !node.has_universal_load
            || declared.server.is_none() && (node.has_server_load || node.server_load_callable)
            || declared.universal.is_none()
                && (node.has_universal_load || node.universal_load_callable)
        {
            return Err(format!(
                "Runtime node {index} load facts contradict its module inventory"
            ));
        }
        let target = &mut output.nodes[*index];
        target.has_server_load = known(node.has_server_load);
        target.has_universal_load = known(node.has_universal_load);
        target.server_load_callable = known(node.server_load_callable);
        target.universal_load_callable = known(node.universal_load_callable);
    }
    let included: Vec<_> = inventory
        .routes
        .iter()
        .filter(|route| route.page.is_some() || route.endpoint.is_some())
        .collect();
    if metadata.routes.len() != included.len() {
        return Err("Runtime metadata omitted or added routes".into());
    }
    let endpoint_order = if version.is_kit3() {
        &[
            "GET", "POST", "PUT", "PATCH", "DELETE", "OPTIONS", "HEAD", "QUERY", "*",
        ][..]
    } else {
        &[
            "GET", "POST", "PUT", "PATCH", "DELETE", "OPTIONS", "HEAD", "*",
        ][..]
    };
    let mut resolved_entries = BTreeMap::new();
    for (declared, route) in included.into_iter().zip(metadata.routes) {
        if declared.id != route.id {
            return Err("Runtime metadata route IDs are duplicate, missing or reordered".into());
        }
        if declared.page.is_some() != route.page.is_some() {
            return Err(format!("Runtime page presence mismatch for {}", route.id));
        }
        if declared.endpoint.is_some() != route.endpoint.is_some() {
            return Err(format!(
                "Runtime endpoint presence mismatch for {}",
                route.id
            ));
        }
        if declared.endpoint.is_none() && !route.endpoint_methods.is_empty() {
            return Err("Runtime methods refer to an absent endpoint".into());
        }
        methods(&route.endpoint_methods, endpoint_order, &route.id)?;
        if declared.page.is_some() {
            if route.page_methods != ["GET"] && route.page_methods != ["GET", "POST"] {
                return Err("Runtime page methods must be GET or GET, POST".into());
            }
        } else if !route.page_methods.is_empty() {
            return Err("Runtime methods refer to an absent page".into());
        }
        let mut union = route.page_methods.clone();
        for method in &route.endpoint_methods {
            if !union.contains(method) {
                union.push(method.clone());
            }
        }
        if route.methods != union {
            return Err("Runtime combined methods do not match ordered page/endpoint union".into());
        }
        validate_entries(&route.entries)?;
        let target = output
            .routes
            .get_mut(&route.id)
            .expect("Inventory validated");
        target.config = match route.config {
            RuntimeEntries::Json(value @ Value::Object(_)) => known(value),
            _ => return Err("Runtime route config must be a tagged JSON object".into()),
        };
        target.prerender = known(prerender(&route.prerender, declared.page.is_none())?);
        if let Some(page) = route.page {
            target.csr = known(boolean(&page.csr, "csr")?);
            target.ssr = known(boolean(&page.ssr, "ssr")?);
            let page_prerender = prerender(&page.prerender, false)?;
            if target.prerender != known(page_prerender) {
                return Err("Runtime page/route prerender facts disagree".into());
            }
            target.trailing_slash = match page.trailing_slash {
                RuntimeEntries::Json(Value::String(value))
                    if ["never", "always", "ignore"].contains(&value.as_str()) =>
                {
                    known(value)
                }
                _ => return Err("Runtime trailingSlash must be never, always or ignore".into()),
            };
            let mut actions = BTreeMap::new();
            for action in page.actions {
                if actions.insert(action.name, action.callable).is_some() {
                    return Err("Duplicate runtime action name".into());
                }
            }
            if !actions.is_empty() && !route.page_methods.iter().any(|m| m == "POST") {
                return Err("Runtime actions require the page POST method".into());
            }
            let leaf = declared.page.as_ref().expect("Page validated").leaf;
            if inventory.nodes[leaf].server.is_none() && !actions.is_empty() {
                return Err("Runtime actions refer to an absent leaf server module".into());
            }
            target.actions = known(actions);
        }
        if let Some(endpoint) = route.endpoint {
            let trailing = match endpoint.trailing_slash {
                RuntimeEntries::Undefined | RuntimeEntries::Json(Value::Null) => "never".into(),
                RuntimeEntries::Json(Value::String(value))
                    if ["never", "always", "ignore"].contains(&value.as_str()) =>
                {
                    value
                }
                _ => {
                    return Err(
                        "Runtime endpoint trailingSlash must be never, always or ignore".into(),
                    );
                }
            };
            if declared.page.is_none() {
                target.trailing_slash = known(trailing);
            }
        }
        target.page_methods = known(route.page_methods);
        target.endpoint_methods = known(route.endpoint_methods);
        target.methods = known(route.methods);
        resolved_entries.insert(route.id, route.entries);
    }
    // Unused manifest nodes and irrelevant page options on endpoint-only routes
    // retain their static facts. Never erase requirements on unknown facts.
    for node in &output.nodes {
        if !is_known(&node.has_server_load)
            || !is_known(&node.has_universal_load)
            || !is_known(&node.server_load_callable)
            || !is_known(&node.universal_load_callable)
        {
            return Err("Runtime receipt does not cover unresolved unused node facts".into());
        }
    }
    for (id, route) in &output.routes {
        if !is_known(&route.csr)
            || !is_known(&route.ssr)
            || !is_known(&route.prerender)
            || !is_known(&route.trailing_slash)
            || !is_known(&route.config)
            || !is_known(&route.page_methods)
            || !is_known(&route.endpoint_methods)
            || !is_known(&route.methods)
            || !is_known(&route.actions)
        {
            return Err(format!(
                "Runtime receipt does not cover all route facts for {id}"
            ));
        }
    }
    output.requirements.clear();
    output.errors.clear();
    Ok(RuntimeAnalysis {
        analysis: output,
        resolved_entries,
    })
}

#[cfg(test)]
#[path = "runtime_analysis_tests.rs"]
mod tests;
