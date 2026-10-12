//! Trusted native drivers keep manifests, callbacks and module namespaces in
//! Boa while returning only normalized JSON across the application boundary.
use crate::{Result, Runtime, RuntimeError, RuntimeValue, engine, error, validate_json_roots};
use boa_engine::{Context, JsError, JsObject, JsValue};
use serde_json::Value;

pub struct NativeSession<'a> {
    runtime: &'a mut Runtime,
}
impl NativeSession<'_> {
    /// Trusted embedding access. The caller must not change the engine's loader,
    /// job executor or execution limits. This is not exposed by the JSON API.
    pub fn context(&mut self) -> &mut Context {
        &mut self.runtime.context
    }
    pub fn namespace(&mut self, entry: &str) -> Result<JsObject> {
        let module = self.runtime.evaluate_inner(entry)?;
        Ok(module.namespace(&mut self.runtime.context))
    }
    /// Invoke without settling: drivers can start all node loaders before
    /// draining their promises, preserving concurrent Promise.all semantics.
    pub fn call(
        &mut self,
        function: &JsValue,
        receiver: &JsValue,
        args: &[JsValue],
    ) -> Result<JsValue> {
        function
            .as_callable()
            .ok_or_else(|| error("not_callable", "Native driver expected a callable value"))?
            .call(receiver, args, &mut self.runtime.context)
            .map_err(engine)
    }
    pub fn settle(&mut self, value: JsValue) -> Result<JsValue> {
        self.runtime.await_value(value)
    }
    pub fn normalize(&mut self, value: JsValue) -> Result<RuntimeValue> {
        self.runtime.output(value)
    }
}
impl From<JsError> for RuntimeError {
    fn from(value: JsError) -> Self {
        engine(value)
    }
}
impl Runtime {
    /// Execute a trusted Rust operation with the same cumulative engine/job
    /// limits and failure poisoning as JSON operations. It grants no additional
    /// filesystem, network or module capabilities. Native allocation and CPU
    /// outside Boa remain the embedding's responsibility.
    pub fn native_operation(
        &mut self,
        operation: impl FnOnce(&mut NativeSession<'_>) -> Result<Value>,
    ) -> Result<Value> {
        self.check()?;
        let result = (|| {
            let value = operation(&mut NativeSession { runtime: self })?;
            self.drain()?;
            validate_json_roots(std::slice::from_ref(&value), &self.limits)?;
            Ok(value)
        })();
        self.finish(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{HostSnapshot, ModuleSource, RuntimeLimits};
    use boa_engine::{
        JsString,
        object::builtins::{JsArray, JsPromise},
    };
    use serde_json::json;
    use std::collections::BTreeMap;
    fn runtime() -> Runtime {
        Runtime::new(HostSnapshot { modules: BTreeMap::from([("entry".into(), ModuleSource {
            source: "const order=[]; export const manifest={assets:new Set(['a']),nodes:[async()=>{order.push('start-a');await 0;order.push('end-a');return {name:'a'}},async()=>{order.push('start-b');await 0;order.push('end-b');return {name:'b'}}]}; export {order}; export async function broken(){throw Error('native failure')};".into(),
            import_meta_url: "file:///entry.js".into(),
        })]), resolutions: BTreeMap::new() }, RuntimeLimits::default()).unwrap()
    }
    #[test]
    fn real_manifest_functions_stay_in_engine_and_loaders_start_together() {
        let mut runtime = runtime();
        let value = runtime
            .native_operation(|session| {
                let namespace = session.namespace("entry")?;
                let manifest = namespace
                    .get(JsString::from("manifest"), session.context())?
                    .as_object()
                    .unwrap();
                let nodes = manifest
                    .get(JsString::from("nodes"), session.context())?
                    .as_object()
                    .unwrap();
                let nodes = JsArray::from_object(nodes)?;
                let mut pending = vec![];
                for index in 0..nodes.length(session.context())? {
                    let loader = nodes.get(index, session.context())?;
                    pending.push(session.call(&loader, &JsValue::undefined(), &[])?);
                }
                let mut names = vec![];
                for promise in pending {
                    let node = session.settle(promise)?.as_object().unwrap();
                    let name = node.get(JsString::from("name"), session.context())?;
                    names.push(match session.normalize(name)? {
                        RuntimeValue::Json(value) => value,
                        _ => unreachable!(),
                    });
                }
                let order = namespace.get(JsString::from("order"), session.context())?;
                let RuntimeValue::Json(order) = session.normalize(order)? else {
                    unreachable!()
                };
                Ok(json!({"names":names,"order":order}))
            })
            .unwrap();
        assert_eq!(
            value,
            json!({"names":["a","b"],"order":["start-a","start-b","end-a","end-b"]})
        );
    }
    #[test]
    fn failed_native_operation_poisons_later_json_calls() {
        let mut runtime = runtime();
        let failure = runtime
            .native_operation(|session| {
                let namespace = session.namespace("entry")?;
                let function = namespace.get(JsString::from("broken"), session.context())?;
                let value = session.call(&function, &JsValue::undefined(), &[])?;
                session.settle(value)?;
                Ok(Value::Null)
            })
            .unwrap_err();
        assert_eq!(failure.code, "promise_rejected");
        assert_eq!(
            runtime.evaluate("entry").unwrap_err().code,
            "runtime_failed"
        );
    }
    #[test]
    fn promise_resolve_constructor_getter_throw_is_catchable_with_identity() {
        let mut runtime = Runtime::new(HostSnapshot { modules: BTreeMap::from([("entry".into(), ModuleSource {
            source: "export const thrown=Error('constructor');export function hostile(){const p=Promise.resolve(1);Object.defineProperty(p,'constructor',{get(){throw thrown}});return p}".into(),
            import_meta_url: "file:///entry.js".into(),
        })]), resolutions: BTreeMap::new() }, RuntimeLimits::default()).unwrap();
        let value = runtime
            .native_operation(|session| {
                let namespace = session.namespace("entry")?;
                let hostile = namespace.get(JsString::from("hostile"), session.context())?;
                let thrown = namespace.get(JsString::from("thrown"), session.context())?;
                let promise = session.call(&hostile, &JsValue::undefined(), &[])?;
                // Boa's unpatched resolve turned this into an uncatchable
                // EnginePanic; the getter's own value must be thrown instead.
                let error = JsPromise::resolve(promise, session.context()).unwrap_err();
                let error = error.into_opaque(session.context())?;
                Ok(json!({"same": JsValue::same_value(&error, &thrown)}))
            })
            .unwrap();
        assert_eq!(value, json!({"same": true}));
    }
}
