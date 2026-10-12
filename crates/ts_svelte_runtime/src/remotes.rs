//! Native SvelteKit remote metadata and remote-module initialization operations.
//!
//! Ported from MIT-licensed SvelteKit (Svelte contributors):
//! - 2.67.0/2.68.0/2.69.2/2.69.3/2.70.3 core/postbuild/analyse.js and
//!   exports/internal/remote-functions.js;
//! - 3.0.0-next.32/3.0.1 core/postbuild/analyse.js and
//!   exports/internal/server/remote-functions.js.
//!
//! The Vite remote transform emits `import * as m; export default m` entry
//! wrappers and calls init_remote_functions during source-module evaluation.
//! Analysis must therefore not initialize exports a second time. Neither this
//! module nor initialization executes remote handlers or prerender inputs.
use crate::{Result, get, invalid, metadata::MetadataProfile, object};
use boa_engine::{
    JsObject, JsString, JsValue, builtins::object::OrdinaryObject, object::builtins::JsArray,
    property::PropertyKey,
};
use serde::{Deserialize, Serialize};
use ts_js_runtime::{RuntimeError, native_session::NativeSession};

const MAX_CHUNKS: usize = 16_384;
const MAX_EXPORTS: usize = 16_384;
const MAX_TOTAL_EXPORTS: usize = 65_536;
const MAX_IDENTITY: usize = 1024 * 1024;
const TYPES: &[&str] = &[
    "command",
    "form",
    "prerender",
    "query",
    "query_batch",
    "query_live",
];

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RemoteChunk {
    pub hash: String,
    pub file: String,
}
pub struct RemoteAnalysis {
    /// Map insertion order. Repeated chunk hashes replace their previous value
    /// without moving that key, while still evaluating every requested loader.
    pub remotes: Vec<RemoteModuleMetadata>,
}
pub struct RemoteModuleMetadata {
    pub hash: String,
    /// Input provenance only; the upstream metadata Map does not contain file.
    pub file: String,
    pub exports: Vec<RemoteExportMetadata>,
}
pub struct RemoteExportMetadata {
    /// Preserve even non-scalar UTF-16 export names until the host JSON boundary.
    pub name: JsString,
    pub kind: JsValue,
    /// Exactly `type !== 'prerender' || internals.dynamic`: notably undefined
    /// stays undefined, and truthy/falsy values are not coerced to booleans.
    pub dynamic: JsValue,
}
fn identity(hash: &str, file: &str) -> Result<()> {
    if hash.len() > MAX_IDENTITY || file.len() > MAX_IDENTITY {
        return Err(invalid("Remote chunk identity exceeds native limit"));
    }
    Ok(())
}
fn property(s: &mut NativeSession<'_>, value: &JsValue, key: &str) -> Result<JsValue> {
    Ok(value
        .to_object(s.context())?
        .get(JsString::from(key), s.context())?)
}
fn optional_property(s: &mut NativeSession<'_>, value: &JsValue, key: &str) -> Result<JsValue> {
    if value.is_null_or_undefined() {
        Ok(JsValue::undefined())
    } else {
        property(s, value, key)
    }
}

/// Lazy for-in traversal: properties deleted by a preceding export getter are
/// skipped, non-enumerable own names shadow inherited names, symbols are ignored.
/// Native module namespaces have null prototypes; object wrappers still receive
/// the full for-in semantics used by Kit, instead of Object.keys approximation.
struct Exports {
    current: Option<JsObject>,
    keys: Option<std::vec::IntoIter<PropertyKey>>,
    seen: Vec<JsString>,
    depth: usize,
    visits: usize,
}
impl Exports {
    fn new(s: &mut NativeSession<'_>, value: &JsValue) -> Result<Self> {
        Ok(Self {
            current: if value.is_null_or_undefined() {
                None
            } else {
                Some(value.to_object(s.context())?)
            },
            keys: None,
            seen: vec![],
            depth: 0,
            visits: 0,
        })
    }
    fn next(&mut self, s: &mut NativeSession<'_>) -> Result<Option<JsString>> {
        loop {
            let Some(current) = &self.current else {
                return Ok(None);
            };
            if self.keys.is_none() {
                self.depth += 1;
                if self.depth > 128 {
                    return Err(invalid(
                        "Remote export prototype depth exceeds native limit",
                    ));
                }
                let keys = current.own_property_keys(s.context())?;
                if keys.len() > MAX_EXPORTS {
                    return Err(invalid("Remote property count exceeds native limit"));
                }
                self.keys = Some(keys.into_iter());
            }
            if let Some(key) = self.keys.as_mut().unwrap().next() {
                self.visits += 1;
                if self.visits > MAX_EXPORTS {
                    return Err(invalid("Remote export traversal exceeds native limit"));
                }
                if matches!(key, PropertyKey::Symbol(_)) {
                    continue;
                }
                let key: JsValue = key.into();
                let name = key.to_string(s.context())?;
                if self.seen.contains(&name) {
                    continue;
                }
                let descriptor = OrdinaryObject::get_own_property_descriptor(
                    &JsValue::undefined(),
                    &[current.clone().into(), key],
                    s.context(),
                )?;
                if descriptor.is_undefined() {
                    continue;
                }
                self.seen.push(name.clone());
                if property(s, &descriptor, "enumerable")?.to_boolean() {
                    return Ok(Some(name));
                }
            } else {
                self.current = OrdinaryObject::get_prototype_of(
                    &JsValue::undefined(),
                    &[current.clone().into()],
                    s.context(),
                )?
                .as_object();
                self.keys = None;
            }
        }
    }
}

/// The exact postbuild remotes phase. Call after route analysis, with the
/// compiler's ordered RemoteChunk inventory. Loaders and namespace objects are
/// real engine values; no source evaluation or JSON module substitution occurs.
/// Rejected loaders/property getters fail the operation without a partial result.
pub fn analyse(
    s: &mut NativeSession<'_>,
    manifest: &JsObject,
    chunks: &[RemoteChunk],
    profile: MetadataProfile,
) -> Result<RemoteAnalysis> {
    if chunks.len() > MAX_CHUNKS {
        return Err(invalid("Remote chunk count exceeds native limit"));
    }
    let mut remotes: Vec<RemoteModuleMetadata> = vec![];
    let mut total = 0usize;
    for chunk in chunks {
        identity(&chunk.hash, &chunk.file)?;
        let container = if profile.is_kit3() {
            manifest.clone()
        } else {
            object(get(s, manifest, "_")?, "Kit2 manifest internals")?
        };
        let loaders = get(s, &container, "remotes")?;
        let loader = property(s, &loaders, &chunk.hash)?;
        // `const loader = ...; await loader()` does not bind the table as this.
        let pending = s.call(&loader, &JsValue::undefined(), &[])?;
        let module = s.settle(pending)?;
        let functions = property(s, &module, "default")?;
        let mut iterator = Exports::new(s, &functions)?;
        let mut exports = vec![];
        while let Some(name) = iterator.next(s)? {
            total += 1;
            if total > MAX_TOTAL_EXPORTS {
                return Err(invalid("Remote metadata total exceeds native limit"));
            }
            let function = functions
                .to_object(s.context())?
                .get(name.clone(), s.context())?;
            let internals = property(s, &function, "__")?;
            let kind = property(s, &internals, "type")?;
            let dynamic = if kind.strict_equals(&JsString::from("prerender").into()) {
                property(s, &internals, "dynamic")?
            } else {
                true.into()
            };
            exports.push(RemoteExportMetadata {
                name,
                kind,
                dynamic,
            });
        }
        let result = RemoteModuleMetadata {
            hash: chunk.hash.clone(),
            file: chunk.file.clone(),
            exports,
        };
        if let Some(existing) = remotes.iter_mut().find(|item| item.hash == chunk.hash) {
            *existing = result;
        } else {
            remotes.push(result);
        }
    }
    Ok(RemoteAnalysis { remotes })
}

fn diagnostic(profile: MetadataProfile, code: &str, message: String) -> RuntimeError {
    RuntimeError {
        code: code.into(),
        message: if profile.is_kit3() {
            format!("{code}\n{message}\nhttps://svelte.dev/e/kit/{code}")
        } else {
            message
        },
    }
}
/// Native equivalent of Kit's init_remote_functions export. This is separate
/// from analysis and intended for a native transform/runtime binding. It reads
/// the actual module object, validates known type tags, and assigns __.id/name.
/// Kit permits any value carrying valid internals, rather than checking callable.
/// Object.entries snapshots all export values before validation. Like upstream,
/// a later validation/setter failure does not roll back earlier mutations.
pub fn initialize_functions(
    s: &mut NativeSession<'_>,
    module: &JsValue,
    file: &str,
    hash: &str,
    profile: MetadataProfile,
) -> Result<()> {
    identity(hash, file)?;
    if property(s, module, "default")?.to_boolean() {
        let file = if profile.is_kit3() {
            format!("`{file}`")
        } else {
            file.into()
        };
        return Err(diagnostic(
            profile,
            "remote_module_default_export",
            format!(
                "Cannot export `default` from a remote module ({file}) — please use named exports instead"
            ),
        ));
    }
    let values = OrdinaryObject::entries(
        &JsValue::undefined(),
        std::slice::from_ref(module),
        s.context(),
    )?;
    let values = JsArray::from_object(object(values, "Remote export entries")?)?;
    let len = values.length(s.context())?;
    if len > MAX_EXPORTS as u64 {
        return Err(invalid("Remote export count exceeds native limit"));
    }
    for i in 0..len {
        let pair = object(values.get(i, s.context())?, "Remote export entry")?;
        let name = pair
            .get(0, s.context())?
            .as_string()
            .ok_or_else(|| invalid("Remote export name must be a string"))?;
        if name.len().saturating_add(hash.len()) > MAX_IDENTITY {
            return Err(invalid("Remote function identity exceeds native limit"));
        }
        let function = pair.get(1, s.context())?;
        let internals = optional_property(s, &function, "__")?;
        let kind = optional_property(s, &internals, "type")?;
        if !TYPES
            .iter()
            .any(|kind_name| kind.strict_equals(&JsString::from(*kind_name).into()))
        {
            let name = name.to_std_string_escaped();
            let file = if profile.is_kit3() {
                format!("`{file}`")
            } else {
                file.into()
            };
            return Err(diagnostic(
                profile,
                "remote_module_invalid_export",
                format!(
                    "`{name}` exported from {file} is invalid — all exports from this file must be remote functions"
                ),
            ));
        }
        // Deliberately re-read __ for each assignment: getters may change it.
        let internals = object(
            property(s, &function, "__")?,
            "Remote internals assignment target",
        )?;
        let prefix = JsString::from(hash);
        let slash = JsString::from("/");
        let id = JsString::concat_array(&[prefix.as_str(), slash.as_str(), name.as_str()]);
        internals.set(JsString::from("id"), id, true, s.context())?;
        let internals = object(
            property(s, &function, "__")?,
            "Remote internals assignment target",
        )?;
        internals.set(JsString::from("name"), name, true, s.context())?;
    }
    Ok(())
}
#[cfg(test)]
#[path = "remotes_tests.rs"]
mod tests;
