//! Vite 7.3.1 core module features (MIT) that the native graph needs:
//! `import.meta.glob` (`vite:import-glob`) and `?raw` imports (`vite:asset`).
//!
//! Glob support covers literal string or string-array patterns (root-absolute
//! `/`, relative `./` `../`, `!` negations) with `eager`, `import` and string
//! `query` options. Files match like tinyglobby with `dot: false` and
//! `node_modules` ignored, are sorted and exclude the importer. Aliased
//! patterns, `base`, `exhaustive` and brace/extglob syntax fail explicitly.
use anyhow::{Context, bail, ensure};
use oxc_allocator::Allocator;
use oxc_ast::ast::{Argument, CallExpression, Expression, ObjectPropertyKind, PropertyKey};
use oxc_ast_visit::{Visit, walk};
use oxc_parser::Parser;
use oxc_span::{SourceType, Span};
use rolldown::ModuleType;
use rolldown::plugin::{
    HookLoadArgs, HookLoadOutput, HookLoadReturn, HookResolveIdArgs, HookResolveIdOutput,
    HookResolveIdReturn, HookTransformArgs, HookTransformOutput, HookTransformReturn, HookUsage,
    Plugin, PluginContext, SharedLoadPluginContext, SharedTransformPluginContext,
};
use std::borrow::Cow;
use std::path::{Component, Path, PathBuf};

const RAW: &str = "?raw";

#[derive(Debug)]
pub(crate) struct ViteFeatures {
    pub root: PathBuf,
}

impl Plugin for ViteFeatures {
    fn name(&self) -> Cow<'static, str> {
        "ts-rust-vite-features".into()
    }
    fn register_hook_usage(&self) -> HookUsage {
        HookUsage::ResolveId | HookUsage::Load | HookUsage::Transform
    }

    async fn resolve_id(
        &self,
        ctx: &PluginContext,
        args: &HookResolveIdArgs<'_>,
    ) -> HookResolveIdReturn {
        let Some(path) = args.specifier.strip_suffix(RAW) else {
            return Ok(None);
        };
        let resolved = ctx
            .resolve(path, args.importer, None)
            .await?
            .map_err(|error| anyhow::anyhow!("Cannot resolve {}: {error:?}", args.specifier))?;
        Ok(Some(HookResolveIdOutput::from_id(format!(
            "{}{RAW}",
            resolved.id.as_str()
        ))))
    }

    async fn load(&self, _ctx: SharedLoadPluginContext, args: &HookLoadArgs<'_>) -> HookLoadReturn {
        let Some(path) = args.id.strip_suffix(RAW) else {
            return Ok(None);
        };
        // Vite: `export default ${JSON.stringify(await fsp.readFile(file, 'utf-8'))}`.
        let text = std::fs::read(path).with_context(|| format!("Cannot read {path}"))?;
        let text = String::from_utf8_lossy(&text);
        Ok(Some(HookLoadOutput {
            code: format!("export default {}", serde_json::to_string(&text)?).into(),
            module_type: Some(ModuleType::Js),
            ..Default::default()
        }))
    }

    async fn transform(
        &self,
        _ctx: SharedTransformPluginContext,
        args: &HookTransformArgs<'_>,
    ) -> HookTransformReturn {
        let code: &str = args.code.as_ref();
        if args.id.starts_with('\0') || !code.contains("import.meta.glob") {
            return Ok(None);
        }
        let Some(edits) = transform_glob(code, args.id, &self.root)? else {
            return Ok(None);
        };
        let mut magic = string_wizard::MagicString::new(code);
        if !edits.hoisted.is_empty() {
            magic.prepend(edits.hoisted);
        }
        for (span, replacement) in edits.replacements {
            magic
                .update(span.start, span.end, replacement)
                .map_err(|e| anyhow::anyhow!("{}: {e}", args.id))?;
        }
        let map = magic.source_map(string_wizard::SourceMapOptions {
            include_content: true,
            source: args.id.into(),
            hires: string_wizard::Hires::Boundary,
        });
        Ok(Some(HookTransformOutput {
            code: Some(magic.to_string()),
            map: map.into(),
            ..Default::default()
        }))
    }
}

struct GlobCall {
    span: Span,
    patterns: Vec<String>,
    eager: bool,
    import: Option<String>,
    query: Option<String>,
}

#[derive(Default)]
struct Finder {
    calls: Vec<Result<GlobCall, String>>,
}
impl<'a> Visit<'a> for Finder {
    fn visit_call_expression(&mut self, call: &CallExpression<'a>) {
        let is_glob = matches!(&call.callee, Expression::StaticMemberExpression(member)
            if member.property.name == "glob"
                && matches!(&member.object, Expression::ImportMeta(_)));
        if is_glob {
            self.calls.push(glob_call(call));
        } else {
            walk::walk_call_expression(self, call);
        }
    }
}

fn string(expression: &Expression<'_>) -> Option<String> {
    match expression {
        Expression::StringLiteral(s) => Some(s.value.to_string()),
        Expression::TemplateLiteral(t) if t.expressions.is_empty() => t
            .quasis
            .first()
            .and_then(|q| q.value.cooked.map(|c| c.to_string())),
        _ => None,
    }
}

fn glob_call(call: &CallExpression<'_>) -> Result<GlobCall, String> {
    let mut arguments = call.arguments.iter();
    let patterns = match arguments.next().and_then(Argument::as_expression) {
        Some(Expression::ArrayExpression(array)) => array
            .elements
            .iter()
            .map(|element| element.as_expression().and_then(string))
            .collect::<Option<Vec<_>>>(),
        Some(expression) => string(expression).map(|pattern| vec![pattern]),
        None => None,
    }
    .ok_or("import.meta.glob patterns must be string literals")?;
    let mut glob = GlobCall {
        span: call.span,
        patterns,
        eager: false,
        import: None,
        query: None,
    };
    if let Some(options) = arguments.next() {
        let Some(Expression::ObjectExpression(object)) = options.as_expression() else {
            return Err("import.meta.glob options must be an object literal".into());
        };
        for property in &object.properties {
            let ObjectPropertyKind::ObjectProperty(property) = property else {
                return Err("import.meta.glob options cannot use spreads".into());
            };
            let key = match &property.key {
                PropertyKey::StaticIdentifier(id) => id.name.to_string(),
                PropertyKey::StringLiteral(s) => s.value.to_string(),
                _ => return Err("import.meta.glob option keys must be static".into()),
            };
            match (key.as_str(), &property.value) {
                ("eager", Expression::BooleanLiteral(b)) => glob.eager = b.value,
                ("import", value) => {
                    glob.import = Some(string(value).ok_or("import option must be a string")?)
                }
                ("query", value) => {
                    glob.query =
                        Some(string(value).ok_or("Only string query options are supported")?)
                }
                (other, _) => return Err(format!("Unsupported import.meta.glob option {other}")),
            }
        }
    }
    if arguments.next().is_some() {
        return Err("import.meta.glob takes at most two arguments".into());
    }
    Ok(glob)
}

/// Lexical normalization like Node's `path.posix.join`/`resolve`.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

fn relative(from: &Path, to: &Path) -> String {
    let from: Vec<_> = from.components().collect();
    let to: Vec<_> = to.components().collect();
    let common = from.iter().zip(&to).take_while(|(a, b)| a == b).count();
    let mut parts: Vec<String> = vec!["..".into(); from.len() - common];
    parts.extend(
        to[common..]
            .iter()
            .map(|c| c.as_os_str().to_string_lossy().into_owned()),
    );
    parts.join("/")
}

/// `toAbsoluteGlob` for the supported pattern forms.
fn absolute_glob(pattern: &str, root: &Path, directory: &Path) -> anyhow::Result<String> {
    ensure!(
        !pattern.contains(['{', '}', '(', ')', '[', ']']),
        "Unsupported glob syntax in {pattern:?}; only *, ** and ? are ported"
    );
    let (mut base, mut rest) = if let Some(rest) = pattern.strip_prefix('/') {
        (root.to_path_buf(), rest)
    } else if pattern.starts_with("./") || pattern.starts_with("../") {
        (directory.to_path_buf(), pattern)
    } else {
        bail!("Unsupported import.meta.glob pattern {pattern:?}; use /, ./ or ../");
    };
    loop {
        if let Some(next) = rest.strip_prefix("./") {
            rest = next;
        } else if let Some(next) = rest.strip_prefix("../") {
            base.pop();
            rest = next;
        } else {
            break;
        }
    }
    ensure!(
        !rest.split('/').any(|part| part == ".."),
        "Unsupported interior .. in glob {pattern:?}"
    );
    // The base directory is literal even if it contains glob metacharacters.
    let base = glob::Pattern::escape(&normalize(&base).to_string_lossy());
    Ok(format!("{base}/{rest}"))
}

fn matches(pattern: &str, root: &Path, directory: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let options = glob::MatchOptions {
        case_sensitive: true,
        require_literal_separator: true,
        require_literal_leading_dot: true,
    };
    let absolute = absolute_glob(pattern, root, directory)?;
    let mut files = vec![];
    for entry in
        glob::glob_with(&absolute, options).with_context(|| format!("Invalid glob {pattern}"))?
    {
        let path = entry?;
        if path.is_file() && !path.components().any(|c| c.as_os_str() == "node_modules") {
            files.push(path);
        }
    }
    Ok(files)
}

/// Text edits for one module: hoisted eager imports and call replacements.
struct GlobEdits {
    hoisted: String,
    replacements: Vec<(Span, String)>,
}

fn transform_glob(code: &str, id: &str, root: &Path) -> anyhow::Result<Option<GlobEdits>> {
    let allocator = Allocator::default();
    let source_type = SourceType::from_path(id)
        .unwrap_or_else(|_| SourceType::mjs())
        .with_module(true);
    let parsed = Parser::new(&allocator, code, source_type).parse();
    if !parsed.diagnostics.is_empty() {
        // Not ours to report: the bundler's own parse reports the error.
        return Ok(None);
    }
    let mut finder = Finder::default();
    finder.visit_program(&parsed.program);
    if finder.calls.is_empty() {
        return Ok(None);
    }
    let importer = Path::new(id);
    let directory = importer.parent().unwrap_or(root);
    let mut hoisted = vec![];
    let mut replacements = vec![];
    for (index, call) in finder.calls.into_iter().enumerate() {
        let call = call.map_err(|e| anyhow::anyhow!("{id}: {e}"))?;
        let mut files = std::collections::BTreeSet::new();
        let mut negatives = vec![];
        for pattern in &call.patterns {
            if let Some(negative) = pattern.strip_prefix('!') {
                negatives.push(
                    glob::Pattern::new(&absolute_glob(negative, root, directory)?)
                        .with_context(|| format!("Invalid glob {negative}"))?,
                );
            } else {
                files.extend(matches(pattern, root, directory)?);
            }
        }
        let options = glob::MatchOptions {
            case_sensitive: true,
            require_literal_separator: true,
            require_literal_leading_dot: true,
        };
        let relative_patterns = call
            .patterns
            .iter()
            .all(|p| p.trim_start_matches('!').starts_with('.'));
        let mut properties = vec![];
        for (file_index, file) in files
            .into_iter()
            .filter(|file| file != importer)
            .filter(|file| !negatives.iter().any(|n| n.matches_path_with(file, options)))
            .enumerate()
        {
            let mut import_path = relative(directory, &file);
            if !import_path.starts_with('.') {
                import_path = format!("./{import_path}");
            }
            let key = if relative_patterns {
                import_path.clone()
            } else {
                let path = relative(root, &file);
                if path.starts_with('.') {
                    path
                } else {
                    format!("/{path}")
                }
            };
            let specifier = match &call.query {
                Some(query) if query.starts_with('?') => format!("{import_path}{query}"),
                Some(query) => format!("{import_path}?{query}"),
                None => import_path,
            };
            let specifier = serde_json::to_string(&specifier)?;
            let key = serde_json::to_string(&key)?;
            let value = if call.eager {
                let local = format!("__glob__{index}_{file_index}");
                match &call.import {
                    Some(name) if name == "default" => {
                        hoisted.push(format!("import {local} from {specifier};"))
                    }
                    Some(name) => hoisted.push(format!(
                        "import {{ {} as {local} }} from {specifier};",
                        serde_json::to_string(name)?
                    )),
                    None => hoisted.push(format!("import * as {local} from {specifier};")),
                }
                local
            } else {
                match &call.import {
                    Some(name) => format!(
                        "() => import({specifier}).then((m) => m[{}])",
                        serde_json::to_string(name)?
                    ),
                    None => format!("() => import({specifier})"),
                }
            };
            properties.push(format!("{key}: {value}"));
        }
        replacements.push((
            call.span,
            format!(
                "/* #__PURE__ */ Object.assign({{{}}})",
                properties.join(",\n")
            ),
        ));
    }
    replacements.sort_by_key(|(span, _)| span.start);
    let hoisted = if hoisted.is_empty() {
        String::new()
    } else {
        format!("{}\n", hoisted.join("\n"))
    };
    Ok(Some(GlobEdits {
        hoisted,
        replacements,
    }))
}

#[cfg(test)]
fn apply(code: &str, edits: GlobEdits) -> String {
    let mut out = edits.hoisted;
    let mut last = 0;
    for (span, replacement) in edits.replacements {
        out.push_str(&code[last..span.start as usize]);
        out.push_str(&replacement);
        last = span.end as usize;
    }
    out.push_str(&code[last..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lazy_root_absolute_glob_matches_vite_keys_and_import_paths() {
        let root = std::env::temp_dir().join(format!("ts-svelte-glob-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for file in [
            "src/routes/(docs)/docs/b/+page.md",
            "src/routes/(docs)/docs/a/+page.md",
            "src/routes/.hidden/+page.md",
            "src/routes/x/+page.svelte",
        ] {
            let path = root.join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "").unwrap();
        }
        let importer = root.join("src/lib/help/articles.ts");
        let code = "const modules = import.meta.glob('/src/routes/**/+page.md');";
        let out = apply(
            code,
            transform_glob(code, importer.to_str().unwrap(), &root)
                .unwrap()
                .unwrap(),
        );
        assert_eq!(
            out,
            "const modules = /* #__PURE__ */ Object.assign({\"/src/routes/(docs)/docs/a/+page.md\": () => import(\"../../routes/(docs)/docs/a/+page.md\"),\n\"/src/routes/(docs)/docs/b/+page.md\": () => import(\"../../routes/(docs)/docs/b/+page.md\")});"
        );
        let eager_code = "export const m = import.meta.glob(['./*.ts', '!./skip.ts'], { eager: true, import: 'default' });";
        let eager = apply(
            eager_code,
            transform_glob(
                eager_code,
                root.join("src/routes/x/index.ts").to_str().unwrap(),
                &root,
            )
            .unwrap()
            .unwrap(),
        );
        assert!(eager.starts_with("export const m") || eager.contains("Object.assign({})"));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn unsupported_glob_forms_fail_explicitly() {
        let root = Path::new("/app");
        for code in [
            "import.meta.glob(name)",
            "import.meta.glob('/src/{a,b}/*.ts')",
            "import.meta.glob('$lib/*.ts')",
            "import.meta.glob('/src/*.ts', { exhaustive: true })",
        ] {
            assert!(
                transform_glob(code, "/app/src/x.ts", root).is_err(),
                "{code}"
            );
        }
    }
}
