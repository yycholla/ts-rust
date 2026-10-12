use crate::{BundleRequest, absolute};
use anyhow::{Context, bail, ensure};
use rolldown::ModuleType;
use rolldown::plugin::{
    HookLoadArgs, HookLoadOutput, HookLoadReturn, HookResolveIdArgs, HookResolveIdOutput,
    HookResolveIdReturn, HookTransformArgs, HookTransformOutput, HookTransformOutputMap,
    HookTransformReturn, HookUsage, Plugin, PluginContext, SharedLoadPluginContext,
    SharedTransformPluginContext,
};
use rolldown_sourcemap::SourceMap;
use serde_json::{Value, json};
use std::borrow::Cow;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

#[derive(Debug)]
pub(crate) struct SveltePlugin {
    pub request: Arc<BundleRequest>,
    pub responses: Arc<Mutex<BTreeMap<String, Value>>>,
    pub css: Option<crate::css::CssState>,
}

fn extension(id: &str) -> &str {
    id.rsplit('.').next().unwrap_or("")
}

fn script_type(id: &str) -> ModuleType {
    match extension(id) {
        "ts" | "mts" | "cts" => ModuleType::Ts,
        "tsx" => ModuleType::Tsx,
        "jsx" => ModuleType::Jsx,
        "json" => ModuleType::Json,
        _ => ModuleType::Js,
    }
}

fn check_file_kind(id: &str, markdown: bool) -> anyhow::Result<()> {
    ensure!(
        !matches!(
            extension(id),
            "css" | "scss" | "sass" | "less" | "styl" | "stylus" | "pcss" | "postcss"
        ),
        "External stylesheets require a native CSS bundling plugin: {id}"
    );
    ensure!(
        markdown || !matches!(extension(id), "md" | "svx" | "mdx"),
        "Markdown requires a registered native preprocessor: {id}"
    );
    Ok(())
}

fn attribute_text(attribute: &Value) -> Option<String> {
    attribute["value"]
        .as_array()?
        .iter()
        .map(|part| {
            if part["type"] == "Text" {
                part["data"]
                    .as_str()
                    .or_else(|| part["raw"].as_str())
                    .map(str::to_owned)
            } else {
                None
            }
        })
        .collect::<Option<Vec<_>>>()
        .map(|parts| parts.concat())
}

fn attributes(attributes: &[Value], script: bool) -> anyhow::Result<()> {
    for attribute in attributes {
        let name = attribute["name"].as_str().unwrap_or("");
        if name == "src" {
            bail!("External script/style src requires a native preprocessor");
        }
        if name == "lang" {
            let language =
                attribute_text(attribute).context("Dynamic script/style lang is unsupported")?;
            let allowed = if script {
                matches!(language.as_str(), "js" | "javascript" | "ts" | "typescript")
            } else {
                language == "css"
            };
            ensure!(
                allowed,
                "Native {} preprocessor is not registered for lang={language:?}",
                if script { "script" } else { "style" }
            );
        }
    }
    Ok(())
}

fn css_dependencies(node: &Value, native_css: bool) -> anyhow::Result<()> {
    match node {
        Value::Object(fields) => {
            if fields.get("type").and_then(Value::as_str) == Some("Atrule") {
                let name = fields
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_ascii_lowercase();
                ensure!(
                    (native_css && name == "import")
                        || !matches!(
                            name.as_str(),
                            "import"
                                | "apply"
                                | "tailwind"
                                | "plugin"
                                | "theme"
                                | "utility"
                                | "source"
                                | "reference"
                                | "config"
                                | "custom-variant"
                                | "variant"
                        ),
                    "CSS @{name} requires a native CSS/preprocessor plugin"
                );
            }
            // CSS values retain their raw spelling. This initial plugin has no
            // asset rebasing pass: reject URLs and escaped values explicitly.
            if fields.get("type").and_then(Value::as_str) == Some("Declaration") {
                if let Some(value) = fields.get("value").and_then(Value::as_str) {
                    ensure!(
                        native_css
                            || !value.to_ascii_lowercase().contains("url(")
                                && !value.contains('\\'),
                        "CSS URLs and escaped values require a native asset-rebasing plugin"
                    );
                }
            }
            for value in fields.values() {
                if value.is_object() || value.is_array() {
                    css_dependencies(value, native_css)?;
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                css_dependencies(item, native_css)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn preflight_with_css(source: &str, native_css: bool) -> anyhow::Result<()> {
    ensure!(
        source.len() <= ts_svelte_compiler::MAX_SOURCE_BYTES,
        "Svelte source exceeds 1 MiB"
    );
    let allocator = oxc_allocator::Allocator::default();
    // A parse failure is handled by the real compiler immediately afterward,
    // preserving its versioned diagnostic rather than replacing it with ours.
    let Ok(ast) = rsvelte_core::parse(
        source,
        &allocator,
        rsvelte_core::ParseOptions {
            // This flag also defers CSS children. Preflight must inspect real
            // declarations before accepting dependencies that need rebasing.
            defer_script_parse: false,
            skip_expression_loc: true,
            skip_non_css_lang_style: true,
            ..Default::default()
        },
    ) else {
        return Ok(());
    };
    for script in ast.instance.iter().chain(ast.module.iter()) {
        let attrs = script
            .attributes
            .iter()
            .map(serde_json::to_value)
            .collect::<Result<Vec<_>, _>>()?;
        attributes(&attrs, true)?;
    }
    if let Some(css) = &ast.css {
        attributes(&css.attributes, false)?;
        for child in &css.children {
            css_dependencies(child, native_css)?;
        }
    }
    Ok(())
}

fn compile_map(value: &Value, id: &str, source: &str) -> anyhow::Result<SourceMap> {
    ensure!(
        !value.is_null(),
        "Native compiler returned no JavaScript source map for {id}"
    );
    let mut value = if let Some(text) = value.as_str() {
        serde_json::from_str(text)?
    } else {
        value.clone()
    };
    let sources = value["sources"]
        .as_array()
        .context("Compiler map is missing sources")?;
    // This map describes the compiler input. A preceding preprocessing map is
    // composed afterward, before exposing the map to Rolldown.
    ensure!(
        sources.len() <= 1,
        "Expected at most one original source in native compiler map for {id}"
    );
    value["sources"] = json!([id]);
    value["sourcesContent"] = json!([source]);
    value["sourceRoot"] = json!("");
    Ok(SourceMap::from_json(serde_json::from_value(value)?)?)
}

impl SveltePlugin {
    fn markdown(&self, id: &str) -> bool {
        self.request.mdsvex.as_ref().is_some_and(|options| {
            options
                .extensions
                .iter()
                .any(|extension| id.ends_with(extension))
        })
    }
    fn retain_failure(&self, id: &str, error: &anyhow::Error) {
        // Rolldown's load-hook wrapper retains the plugin name but may discard
        // the underlying anyhow chain. Keep our own actionable diagnostic.
        self.responses
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(id.to_owned())
            .or_insert_with(|| {
                json!({"ok":false,
                "error":{"code":"native_transform_failed","message":format!("{error:#}")}})
            });
    }
    fn virtual_path(
        &self,
        specifier: &str,
        importer: Option<&str>,
    ) -> anyhow::Result<Option<PathBuf>> {
        let mut candidate = None;
        for alias in &self.request.aliases {
            if let Some(replacement) = alias.resolve(specifier) {
                candidate = Some(absolute(&self.request.root, &replacement)?);
                break;
            }
        }
        let candidate = match candidate {
            Some(path) => path,
            None if Path::new(specifier).is_absolute() => {
                absolute(&self.request.root, Path::new(specifier))?
            }
            None if specifier.starts_with('.') => {
                let base = importer
                    .map(Path::new)
                    .and_then(Path::parent)
                    .unwrap_or(&self.request.root);
                absolute(base, Path::new(specifier))?
            }
            None => return Ok(None),
        };
        if self.request.virtual_modules.contains_key(&candidate) {
            return Ok(Some(candidate));
        }
        if candidate.to_string_lossy().ends_with(".svelte.js") {
            let typed = candidate.with_extension("ts");
            if self.request.virtual_modules.contains_key(&typed) {
                return Ok(Some(typed));
            }
        }
        // Resolver suffix probing appends an extension even when the basename
        // contains a dot: `messages.remote` resolves to `messages.remote.ts`.
        // Replacing the suffix would instead probe the unrelated `messages.ts`.
        for extension in ["mjs", "js", "mts", "ts", "jsx", "tsx", "json", "svelte"] {
            let mut extended = candidate.as_os_str().to_os_string();
            extended.push(format!(".{extension}"));
            let extended = PathBuf::from(extended);
            if self.request.virtual_modules.contains_key(&extended) {
                return Ok(Some(extended));
            }
        }
        if let Some(options) = &self.request.mdsvex {
            for suffix in &options.extensions {
                let mut extended = candidate.as_os_str().to_os_string();
                extended.push(suffix);
                let extended = PathBuf::from(extended);
                if self.request.virtual_modules.contains_key(&extended) {
                    return Ok(Some(extended));
                }
            }
        }
        Ok(None)
    }
}

impl Plugin for SveltePlugin {
    fn name(&self) -> Cow<'static, str> {
        "ts-rust-svelte".into()
    }
    fn register_hook_usage(&self) -> HookUsage {
        HookUsage::ResolveId | HookUsage::Load | HookUsage::Transform
    }

    async fn resolve_id(
        &self,
        ctx: &PluginContext,
        args: &HookResolveIdArgs<'_>,
    ) -> HookResolveIdReturn {
        if let Some(path) = self.virtual_path(args.specifier, args.importer)? {
            return Ok(Some(HookResolveIdOutput::from_id(
                path.to_string_lossy().to_string(),
            )));
        }
        for alias in &self.request.aliases {
            if let Some(replacement) = alias.resolve(args.specifier) {
                let text = replacement.to_str().context("Alias path must be UTF-8")?;
                let resolved = ctx
                    .resolve(text, args.importer, None)
                    .await?
                    .map_err(|error| {
                        anyhow::anyhow!("Cannot resolve alias {}: {error:?}", args.specifier)
                    })?;
                return Ok(Some(HookResolveIdOutput::from_resolved_id(resolved)));
            }
        }
        Ok(None)
    }

    async fn load(&self, _ctx: SharedLoadPluginContext, args: &HookLoadArgs<'_>) -> HookLoadReturn {
        if args.id.starts_with('\0') {
            return Ok(None);
        }
        if !(self.css.is_some() && args.id.ends_with(".css")) {
            check_file_kind(args.id, self.markdown(args.id))
                .inspect_err(|error| self.retain_failure(args.id, error))?;
        }
        if let Some(text) = self.request.virtual_modules.get(Path::new(args.id)) {
            return Ok(Some(HookLoadOutput {
                code: text.as_str().into(),
                module_type: Some(script_type(args.id)),
                map: self
                    .request
                    .virtual_module_maps
                    .get(Path::new(args.id))
                    .map(|value| SourceMap::from_json(serde_json::from_value(value.clone())?))
                    .transpose()?,
                ..Default::default()
            }));
        }
        Ok(None)
    }

    async fn transform(
        &self,
        _ctx: SharedTransformPluginContext,
        args: &HookTransformArgs<'_>,
    ) -> HookTransformReturn {
        // Other plugins' virtual modules may retain a component filename in
        // their identity, but their loaded source is already JavaScript.
        if args.id.starts_with('\0') {
            return Ok(None);
        }
        let markdown = self.markdown(args.id);
        if !(self.css.is_some() && args.id.ends_with(".css")) {
            check_file_kind(args.id, markdown)
                .inspect_err(|error| self.retain_failure(args.id, error))?;
        }
        let is_component = args.id.ends_with(".svelte") || markdown;
        let module_ts = args.id.ends_with(".svelte.ts");
        let is_module = args.id.ends_with(".svelte.js") || module_ts;
        if !is_component && !is_module {
            return Ok(None);
        }
        let original: &str = args.code.as_ref();
        let prepared = if is_component || module_ts {
            Some(
                crate::preprocess::prepare(original, args.id, &self.request, markdown, module_ts)
                    .map_err(|failure| {
                    self.responses
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .insert(args.id.to_owned(), failure.0.clone());
                    anyhow::Error::new(failure)
                })?,
            )
        } else {
            None
        };
        let source = prepared.as_ref().map_or(original, |p| p.code.as_str());
        if is_component {
            preflight_with_css(source, self.css.is_some())
                .with_context(|| format!("Unsupported preprocessing in {}", args.id))
                .inspect_err(|error| self.retain_failure(args.id, error))?;
        }
        let mut options = if is_component {
            serde_json::to_value(&self.request.compiler_options)?
        } else {
            json!({"experimental":self.request.compiler_options.experimental})
        };
        options["rootDir"] = json!(self.request.root);
        if is_component {
            options["css"] = json!(if self.css.is_some() {
                "external"
            } else {
                "injected"
            });
        }
        let mut request = json!({"op":if is_component {"compile"} else {"compileModule"},
            "document":{"uri":args.id,"version":0,"text":source}, "target":self.request.target,
            "dev":self.request.dev,"compatibility":self.request.compatibility,"options":options});
        if is_component {
            request["runes"] = json!(self.request.runes);
        }
        let mut response = ts_svelte_compiler::dispatch(&serde_json::to_vec(&request)?);
        let diagnostics_path = if response["ok"] == true {
            &mut response["result"]["diagnostics"]
        } else {
            &mut response["diagnostics"]
        };
        if let Some(prepared) = &prepared {
            if let Some(diagnostics) = diagnostics_path.as_array_mut() {
                if let Some(map) = &prepared.map {
                    for diagnostic in diagnostics.iter_mut() {
                        crate::preprocess::remap_diagnostic(
                            diagnostic, map, source, original, args.id,
                        );
                    }
                }
                diagnostics.extend(prepared.diagnostics.iter().cloned());
            } else if !prepared.diagnostics.is_empty() {
                *diagnostics_path = json!(prepared.diagnostics);
            }
        }
        let retained = if response["ok"] == true {
            json!({"ok":true,"diagnostics":response["result"]["diagnostics"],"backend":response["result"]["backend"],"compatibility":response["result"]["compatibility"],"metadata":prepared.as_ref().and_then(|p|p.metadata.as_ref())})
        } else {
            response.clone()
        };
        self.responses
            .lock()
            .map_err(|_| anyhow::anyhow!("Compiler response lock poisoned"))?
            .insert(args.id.to_owned(), retained);
        ensure!(
            response["ok"] == true,
            "Native Svelte compilation failed for {}: {}",
            args.id,
            response
        );
        let artifact = &response["result"];
        ensure!(
            self.css.is_some()
                || artifact["css"].is_null()
                || artifact["css"]["code"].as_str().is_none_or(str::is_empty),
            "Compiler returned external CSS despite injected mode for {}",
            args.id
        );
        let mut code = artifact["javascript"]["code"]
            .as_str()
            .context("Native compiler response has no JavaScript code")?
            .to_string();
        if let Some(css) = &self.css {
            if let Some(css_code) = artifact["css"]["code"]
                .as_str()
                .filter(|code| !code.is_empty())
            {
                let css_map = if self.request.sourcemap {
                    let map = compile_map(&artifact["css"]["map"], args.id, source)?;
                    let map = if let Some(original_map) =
                        prepared.as_ref().and_then(|p| p.map.as_ref())
                    {
                        crate::preprocess::compose(original_map, &map)?
                    } else {
                        map
                    };
                    Some(serde_json::from_str(&map.to_json_string())?)
                } else {
                    None
                };
                let id = css.register_component(args.id, css_code.to_owned(), css_map)?;
                code.push_str(&format!("\nimport {};\n", serde_json::to_string(&id)?));
            }
        }
        let map = if self.request.sourcemap {
            let compiler_map = compile_map(&artifact["javascript"]["map"], args.id, source)?;
            HookTransformOutputMap::from(
                if let Some(map) = prepared.as_ref().and_then(|p| p.map.as_ref()) {
                    crate::preprocess::compose(map, &compiler_map)?
                } else {
                    compiler_map
                },
            )
        } else {
            HookTransformOutputMap::Null
        };
        Ok(Some(HookTransformOutput {
            code: Some(code),
            map,
            module_type: Some(ModuleType::Js),
            ..Default::default()
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn preflight(source: &str) -> anyhow::Result<()> {
        preflight_with_css(source, false)
    }
    #[test]
    fn style_and_script_preprocessors_fail_explicitly() {
        assert!(
            preflight("<style lang='scss'>p { color: red }</style><p>x</p>")
                .unwrap_err()
                .to_string()
                .contains("preprocessor")
        );
        assert!(preflight("<script src='./logic.js'></script><p>x</p>").is_err());
        assert!(
            preflight("<!-- <style lang='scss'> --> <p>x</p><style>p { color: red }</style>")
                .is_ok()
        );
        assert!(preflight("<p>x</p><style>p { background: url('./x.png') }</style>").is_err());
        assert!(preflight("<p>x</p><style>@apply text-red-500;</style>").is_err());
    }
    #[test]
    fn unsupported_module_kinds_do_not_fall_through_to_javascript_parser() {
        assert!(check_file_kind("/project/state.svelte.ts", false).is_ok());
        assert!(check_file_kind("/project/app.css", false).is_err());
        assert!(check_file_kind("/project/page.md", false).is_err());
        assert!(check_file_kind("/project/page.md", true).is_ok());
        assert!(check_file_kind("/project/state.svelte.js", false).is_ok());
    }
    #[test]
    fn sourcemap_keeps_original_content_and_absolute_identity() {
        let map = compile_map(
            &json!({"version":3,"sources":["App.svelte"],"names":[],"mappings":"AAAA"}),
            "/project/App.svelte",
            "<p>x</p>",
        )
        .unwrap();
        let map: Value = serde_json::from_str(&map.to_json_string()).unwrap();
        assert_eq!(map["sources"][0], "/project/App.svelte");
        assert_eq!(map["sourcesContent"][0], "<p>x</p>");
        assert!(compile_map(&Value::Null, "x", "").is_err());
    }
}
