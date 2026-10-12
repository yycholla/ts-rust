//! `@sentry/sveltekit` 10.43.0 Vite plugin behaviors that change application
//! modules (MIT): load auto-instrumentation (`sentry-auto-instrumentation`)
//! and global values injection (`sentry-sveltekit-global-values-injection-plugin`).
//! Release/debug-ID injection, source-map flattening and upload belong to
//! `@sentry/vite-plugin` and are not ported here.
use anyhow::Context;
use oxc_allocator::Allocator;
use oxc_ast::ast::{BindingPattern, Declaration, ModuleExportName, Statement};
use oxc_parser::Parser;
use oxc_span::SourceType;
use regress::Regex;
use rolldown::ModuleType;
use rolldown::plugin::{
    HookLoadArgs, HookLoadOutput, HookLoadReturn, HookResolveIdArgs, HookResolveIdOutput,
    HookResolveIdReturn, HookTransformArgs, HookTransformOutput, HookTransformReturn, HookUsage,
    Plugin, PluginContext, SharedLoadPluginContext, SharedTransformPluginContext,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::borrow::Cow;
use std::path::Path;

const WRAPPED_MODULE_SUFFIX: &str = "?sentry-auto-wrap";
const GLOBAL_VALUES: &str = "\0sentry-inject-global-values-file";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SentryOptions {
    /// Wrap universal `load` of `+page`/`+layout` modules (`wrapLoadWithSentry`).
    #[serde(default)]
    pub wrap_load: bool,
    /// Wrap server `load` of `+page.server`/`+layout.server` modules.
    #[serde(default)]
    pub wrap_server_load: bool,
    /// `kit.files.hooks.server` or `src/hooks.server`, project-relative.
    pub server_hooks: String,
    /// Values assigned on `globalThis` by the virtual module.
    #[serde(default)]
    pub global_values: Map<String, Value>,
}

#[derive(Debug)]
pub(crate) struct SentryPlugin {
    options: SentryOptions,
    hooks: Regex,
}

impl SentryPlugin {
    pub(crate) fn new(options: SentryOptions) -> anyhow::Result<Self> {
        // `new RegExp(`/${escapeStringForRegex(serverHooksFile)}(.(js|ts|mjs|mts))?`)`
        let escaped = regress::escape(&options.server_hooks);
        let hooks = Regex::new(&format!("/{escaped}(.(js|ts|mjs|mts))?"))
            .map_err(|e| anyhow::anyhow!("Invalid Sentry hooks pattern: {e:?}"))?;
        Ok(Self { options, hooks })
    }
    fn wrapper(&self, id: &str) -> Option<&'static str> {
        let name = Path::new(id).file_name()?.to_str()?;
        let (stem, extension) = name.rsplit_once('.')?;
        if !matches!(extension, "js" | "ts" | "mjs" | "mts") {
            return None;
        }
        match stem {
            "+page" | "+layout" if self.options.wrap_load => Some("wrapLoadWithSentry"),
            "+page.server" | "+layout.server" if self.options.wrap_server_load => {
                Some("wrapServerLoadWithSentry")
            }
            _ => None,
        }
    }
}

/// `canWrapLoad`: an exported `load` variable, function or specifier.
fn declares_load(id: &str, code: &str) -> bool {
    let allocator = Allocator::default();
    let source_type = SourceType::from_path(id)
        .unwrap_or_else(|_| SourceType::mjs())
        .with_module(true);
    let parsed = Parser::new(&allocator, code, source_type).parse();
    if !parsed.diagnostics.is_empty() {
        return false;
    }
    // ESTree `ExportNamedDeclaration` covers declarations, local specifiers
    // and re-export specifiers; oxc splits them into three statement kinds.
    let exported = |name: &ModuleExportName<'_>| match name {
        ModuleExportName::IdentifierName(name) => name.name == "load",
        ModuleExportName::IdentifierReference(name) => name.name == "load",
        ModuleExportName::StringLiteral(name) => name.value == "load",
    };
    parsed.program.body.iter().any(|statement| match statement {
        Statement::ExportDeclaration(export) => match &export.declaration {
            Declaration::VariableDeclaration(variable) => variable.declarations.iter().any(
                |d| matches!(&d.id, BindingPattern::BindingIdentifier(id) if id.name == "load"),
            ),
            Declaration::FunctionDeclaration(function) => {
                function.id.as_ref().is_some_and(|id| id.name == "load")
            }
            _ => false,
        },
        Statement::ExportNamedDeclaration(export) => {
            export.specifiers.iter().any(|s| exported(&s.exported))
        }
        Statement::ExportFromDeclaration(export) => {
            export.specifiers.iter().any(|s| exported(&s.exported))
        }
        _ => false,
    })
}

impl Plugin for SentryPlugin {
    fn name(&self) -> Cow<'static, str> {
        "ts-rust-sentry-sveltekit".into()
    }
    fn register_hook_usage(&self) -> HookUsage {
        HookUsage::ResolveId | HookUsage::Load | HookUsage::Transform
    }

    async fn resolve_id(
        &self,
        ctx: &PluginContext,
        args: &HookResolveIdArgs<'_>,
    ) -> HookResolveIdReturn {
        if args.specifier == GLOBAL_VALUES {
            return Ok(Some(HookResolveIdOutput::from_id(GLOBAL_VALUES)));
        }
        let Some(path) = args.specifier.strip_suffix(WRAPPED_MODULE_SUFFIX) else {
            return Ok(None);
        };
        let resolved = ctx
            .resolve(path, args.importer, None)
            .await?
            .map_err(|error| anyhow::anyhow!("Cannot resolve {}: {error:?}", args.specifier))?;
        Ok(Some(HookResolveIdOutput::from_id(format!(
            "{}{WRAPPED_MODULE_SUFFIX}",
            resolved.id.as_str()
        ))))
    }

    /// `enforce: 'pre'` load: a wrapper module replaces the original, which is
    /// re-imported under the wrap suffix and loaded from disk unchanged.
    async fn load(&self, _ctx: SharedLoadPluginContext, args: &HookLoadArgs<'_>) -> HookLoadReturn {
        if args.id == GLOBAL_VALUES {
            let code = self
                .options
                .global_values
                .iter()
                .map(|(key, value)| {
                    format!(
                        "globalThis[{}] = {value};",
                        serde_json::to_string(key).unwrap_or_default()
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            let code = if code.is_empty() {
                code
            } else {
                format!("{code}\n")
            };
            return Ok(Some(HookLoadOutput {
                code: code.into(),
                module_type: Some(ModuleType::Js),
                ..Default::default()
            }));
        }
        if let Some(path) = args.id.strip_suffix(WRAPPED_MODULE_SUFFIX) {
            let code =
                std::fs::read_to_string(path).with_context(|| format!("Cannot read {path}"))?;
            let module_type = match Path::new(path).extension().and_then(|e| e.to_str()) {
                Some("ts" | "mts") => ModuleType::Ts,
                _ => ModuleType::Js,
            };
            return Ok(Some(HookLoadOutput {
                code: code.into(),
                module_type: Some(module_type),
                ..Default::default()
            }));
        }
        let Some(wrapper) = self.wrapper(args.id) else {
            return Ok(None);
        };
        let Ok(code) = std::fs::read_to_string(args.id) else {
            return Ok(None);
        };
        if !declares_load(args.id, &code) {
            return Ok(None);
        }
        let wrapped = serde_json::to_string(&format!("{}{WRAPPED_MODULE_SUFFIX}", args.id))?;
        Ok(Some(HookLoadOutput {
            code: format!(
                "import {{ {wrapper} }} from \"@sentry/sveltekit\";\
                 import * as userModule from {wrapped};\
                 export const load = userModule.load ? {wrapper}(userModule.load) : undefined;\
                 export * from {wrapped};"
            )
            .into(),
            module_type: Some(ModuleType::Js),
            ..Default::default()
        }))
    }

    async fn transform(
        &self,
        _ctx: SharedTransformPluginContext,
        args: &HookTransformArgs<'_>,
    ) -> HookTransformReturn {
        let server_entry =
            args.id.contains("instrumentation.server.") || self.hooks.find(args.id).is_some();
        if !server_entry || args.id.starts_with('\0') {
            return Ok(None);
        }
        // `ms.append(...)` with `generateMap({ hires: true })`.
        let mut magic = string_wizard::MagicString::new(args.code.as_str());
        magic.append("\n; import \"\\0sentry-inject-global-values-file\";\n");
        let map = magic.source_map(string_wizard::SourceMapOptions {
            include_content: true,
            source: args.id.into(),
            hires: string_wizard::Hires::True,
        });
        Ok(Some(HookTransformOutput {
            code: Some(magic.to_string()),
            map: map.into(),
            ..Default::default()
        }))
    }
}
