//! `node:module`, for bundler interop only. Rolldown's CommonJS runtime
//! imports `createRequire`; it exists so that import links, but the `require`
//! it returns always throws: the source-only snapshot host has no CommonJS
//! loader and grants no filesystem access.
use boa_engine::{
    Context, JsNativeError, JsResult, JsString, JsValue, Module, NativeFunction,
    module::SyntheticModuleInitializer,
    object::{FunctionObjectBuilder, ObjectInitializer},
    property::Attribute,
};

fn unavailable_require(_: &JsValue, _: &[JsValue], _: &mut Context) -> JsResult<JsValue> {
    Err(JsNativeError::typ()
        .with_message("require() is unavailable in the native snapshot runtime")
        .into())
}

fn create_require(_: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    Ok(FunctionObjectBuilder::new(
        context.realm(),
        NativeFunction::from_fn_ptr(unavailable_require),
    )
    .name(JsString::from("require"))
    .length(1)
    .build()
    .into())
}

pub fn register(context: &mut Context) -> JsResult<Module> {
    let create =
        FunctionObjectBuilder::new(context.realm(), NativeFunction::from_fn_ptr(create_require))
            .name(JsString::from("createRequire"))
            .length(1)
            .build();
    let default = ObjectInitializer::new(context)
        .property(
            JsString::from("createRequire"),
            create.clone(),
            Attribute::all(),
        )
        .build();
    Ok(Module::synthetic(
        &[JsString::from("createRequire"), JsString::from("default")],
        SyntheticModuleInitializer::from_copy_closure_with_captures(
            |module, captures: &(boa_engine::JsObject, boa_engine::JsObject), _| {
                module.set_export(&JsString::from("createRequire"), captures.0.clone().into())?;
                module.set_export(&JsString::from("default"), captures.1.clone().into())?;
                Ok(())
            },
            (create.into(), default),
        ),
        None,
        None,
        context,
    ))
}

#[cfg(test)]
mod tests {
    use crate::{HostCapability, HostSnapshot, ModuleSource, Resolution};
    use std::collections::BTreeMap;

    #[test]
    fn create_require_links_but_require_throws() {
        let source = "import { createRequire } from 'node:module';\nimport 'module';\nconst require = createRequire(import.meta.url);\nlet message = '';\ntry { require('fs'); } catch (error) { message = error.message; }\nexport const result = typeof createRequire + ':' + message;";
        let snapshot = HostSnapshot {
            modules: BTreeMap::from([(
                "app".into(),
                ModuleSource {
                    source: source.into(),
                    import_meta_url: "file:///app.js".into(),
                },
            )]),
            resolutions: BTreeMap::from([(
                "app".into(),
                BTreeMap::from([
                    (
                        "node:module".into(),
                        Resolution::Builtin {
                            name: "node:module".into(),
                        },
                    ),
                    (
                        "module".into(),
                        Resolution::Builtin {
                            name: "node:module".into(),
                        },
                    ),
                ]),
            )]),
        };
        let capabilities = std::collections::BTreeSet::from([HostCapability::NodeModule]);
        let mut runtime = crate::Runtime::with_host_options(
            snapshot,
            Default::default(),
            &capabilities,
            Default::default(),
        )
        .unwrap();
        assert_eq!(
            runtime.read_export("app", "result").unwrap(),
            crate::RuntimeValue::Json(serde_json::json!(
                "function:require() is unavailable in the native snapshot runtime"
            ))
        );
    }
}
