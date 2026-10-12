//! Pure bridge from an actual preliminary server artifact snapshot to the
//! standalone ts_svelte_runtime BootstrapRequest JSON contract. This crate does
//! not depend on Boa or the runtime crate's different serde pins.
//!
//! Module identities are canonical file URLs under config.outDir/output, with
//! query/fragment identity preserved. Only actual emitted chunks and generated
//! JavaScript become modules. No package/config/filesystem resolution occurs.
//! Unknown computed imports stay unresolved; external imports remain explicit
//! external resolutions. Requirements describe potential execution paths, not
//! evidence that an unreachable dynamic import was executed or needed.
use crate::{BuildRequest, artifacts, manifest::PreliminaryManifest};
use oxc_allocator::Allocator;
use oxc_ast::ast::*;
use oxc_ast_visit::{Visit, walk};
use oxc_parser::Parser;
use oxc_span::{GetSpan, SourceType, Span};
use serde::Serialize;
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    path::{Component, Path, PathBuf},
};
use ts_svelte_kit::sync_versions::KitSyncVersion;
use url::Url;

const MAX_MODULES: usize = 4096;
const MAX_SOURCE: usize = 4 * 1024 * 1024;
const MAX_TOTAL: usize = 64 * 1024 * 1024;
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionSnapshot {
    pub bootstrap: Bootstrap,
    pub requirements: Vec<ExecutionRequirement>,
}
impl ExecutionSnapshot {
    pub fn bootstrap_json(&self) -> Result<serde_json::Value, String> {
        serde_json::to_value(&self.bootstrap).map_err(|error| error.to_string())
    }
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Bootstrap {
    pub server_build_id: String,
    pub snapshot: HostSnapshot,
    pub manifest_entry: String,
    pub initialization: Initialization,
    pub environment: BTreeMap<String, String>,
    pub assets: BTreeMap<String, Vec<u8>>,
    // RuntimeLimits intentionally omitted: the standalone runtime applies its
    // declared defaults, which the bounds below also respect.
}
#[derive(Debug, Serialize)]
#[serde(tag = "profile", rename_all = "camelCase")]
pub enum Initialization {
    Kit2 {
        internal: String,
        environment: String,
        #[serde(rename = "publicPrefix")]
        public_prefix: String,
        #[serde(rename = "privatePrefix")]
        private_prefix: String,
    },
    Kit3 {
        server: String,
    },
}
#[derive(Debug, Default, Serialize)]
pub struct HostSnapshot {
    pub modules: BTreeMap<String, ModuleSource>,
    pub resolutions: BTreeMap<String, BTreeMap<String, Resolution>>,
}
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModuleSource {
    pub source: String,
    pub import_meta_url: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Resolution {
    Module { id: String },
    Builtin { name: String },
    External { name: String },
}
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionRequirement {
    pub importer: String,
    pub code: String,
    pub specifier: Option<String>,
    /// Original emitted module UTF-8 byte offsets (not UTF-16 columns).
    pub start: u32,
    pub end: u32,
    pub message: String,
}
struct Edge {
    specifier: String,
    span: Span,
}
#[derive(Default)]
struct Imports {
    edges: Vec<Edge>,
    issues: Vec<(Span, &'static str, Option<String>, &'static str)>,
}
impl Imports {
    fn literal(&mut self, literal: &StringLiteral<'_>) {
        if literal.lone_surrogates {
            self.issues.push((
                literal.span,
                "invalidUtf16Specifier",
                None,
                "Module specifier contains a lone UTF-16 surrogate",
            ));
        } else {
            self.edges.push(Edge {
                specifier: literal.value.to_string(),
                span: literal.span,
            });
        }
    }
    fn attributes(&mut self, clause: Option<&WithClause<'_>>, span: Span) {
        if clause.is_some_and(|clause| !clause.with_entries.is_empty()) {
            self.issues.push((
                span,
                "importAttributes",
                None,
                "The source-only runtime does not implement import attributes",
            ));
        }
    }
}
impl<'a> Visit<'a> for Imports {
    fn visit_import_declaration(&mut self, node: &ImportDeclaration<'a>) {
        self.literal(&node.source);
        self.attributes(node.with_clause.as_deref(), node.span);
        if node.phase.is_some() {
            self.issues.push((
                node.span,
                "importPhase",
                None,
                "Deferred/source import phases require an explicit runtime implementation",
            ));
        }
        walk::walk_import_declaration(self, node);
    }
    fn visit_export_from_declaration(&mut self, node: &ExportFromDeclaration<'a>) {
        self.literal(&node.source);
        self.attributes(node.with_clause.as_deref(), node.span);
        walk::walk_export_from_declaration(self, node);
    }
    fn visit_export_all_declaration(&mut self, node: &ExportAllDeclaration<'a>) {
        self.literal(&node.source);
        self.attributes(node.with_clause.as_deref(), node.span);
        walk::walk_export_all_declaration(self, node);
    }
    fn visit_import_expression(&mut self, node: &ImportExpression<'a>) {
        match &node.source {
            Expression::StringLiteral(literal) => self.literal(literal),
            Expression::TemplateLiteral(template) if template.expressions.is_empty() && template.quasis.len()==1 && !template.quasis[0].lone_surrogates => {
                if let Some(value) = &template.quasis[0].value.cooked {
                    self.edges.push(Edge {specifier:value.to_string(),span:node.source.span()});
                } else { self.issues.push((node.span,"dynamicSpecifier",None,"Import template has no literal cooked value")); }
            }
            _ => self.issues.push((node.span,"dynamicSpecifier",None,"Computed import requires an explicit runtime resolution; no package or filesystem fallback is provided")),
        }
        if node.options.is_some() {
            self.issues.push((node.span,"dynamicImportOptions",None,"Dynamic import options require runtime validation; they are not evaluated during snapshot preparation"));
        }
        if node.phase.is_some() {
            self.issues.push((
                node.span,
                "importPhase",
                None,
                "Deferred/source import phases require an explicit runtime implementation",
            ));
        }
        // Imports can occur inside expressions/options, functions, methods and
        // class initializers. Walking the real AST retains every literal edge.
        walk::walk_import_expression(self, node);
    }
}
fn imports(source: &str) -> Result<Imports, String> {
    let arena = Allocator::default();
    let parsed = Parser::new(&arena, source, SourceType::mjs()).parse();
    if let Some(error) = parsed.diagnostics.first() {
        return Err(format!("Invalid emitted JavaScript: {error:?}"));
    }
    let mut imports = Imports::default();
    imports.visit_program(&parsed.program);
    Ok(imports)
}
fn validate_absolute(path: &Path) -> Result<(), String> {
    if !path.is_absolute()
        || path.to_str().is_none()
        || path
            .components()
            .any(|part| matches!(part, Component::CurDir | Component::ParentDir))
    {
        return Err("Execution output directory must be an absolute normalized UTF-8 path".into());
    }
    Ok(())
}
fn file_url(path: &Path) -> Result<Url, String> {
    Url::from_file_path(path)
        .map_err(|()| format!("Invalid execution module path {}", path.display()))
}
fn module_file(path: &str) -> bool {
    matches!(
        Path::new(path).extension().and_then(|ext| ext.to_str()),
        Some("js" | "mjs" | "cjs")
    )
}
fn validate_percent_encoding(value: &str) -> Result<(), String> {
    let bytes = value.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let pair = bytes
                .get(i + 1..i + 3)
                .ok_or("Incomplete percent escape in module URL")?;
            if !pair.iter().all(u8::is_ascii_hexdigit) {
                return Err("Invalid percent escape in module URL".into());
            }
            if pair.eq_ignore_ascii_case(b"2f") || pair.eq_ignore_ascii_case(b"5c") {
                return Err("Encoded path separator in module URL".into());
            }
            i += 3;
        } else {
            i += 1;
        }
    }
    Ok(())
}
fn resolve(
    importer: &Url,
    specifier: &str,
    sources: &BTreeMap<PathBuf, String>,
) -> Result<(Resolution, Option<(Url, PathBuf)>), String> {
    if specifier.contains(['\0', '\\']) {
        return Err("NUL/backslash in emitted import specifier".into());
    }
    if matches!(specifier, "node:async_hooks" | "async_hooks") {
        return Ok((
            Resolution::Builtin {
                name: "node:async_hooks".into(),
            },
            None,
        ));
    }
    let relative = specifier.starts_with("./")
        || specifier.starts_with("../")
        || specifier.starts_with('/')
        || matches!(specifier, "." | "..");
    if !relative && !specifier.starts_with("file:") {
        return Ok((
            Resolution::External {
                name: specifier.into(),
            },
            None,
        ));
    }
    let resolved = importer
        .join(specifier)
        .map_err(|error| format!("Invalid import URL: {error}"))?;
    if resolved.scheme() != "file" {
        return Err("File import escaped into a different URL scheme".into());
    }
    validate_percent_encoding(resolved.path())?;
    let path = resolved
        .to_file_path()
        .map_err(|()| "Import URL does not identify a local file")?;
    validate_absolute(&path)?;
    if !sources.contains_key(&path) {
        return Err(format!(
            "Missing emitted module for {specifier:?} imported by {importer}"
        ));
    }
    // Artifact paths are authoritative (no realpath/extension/directory guesses).
    // Re-encode decoded paths so %61.js and a.js share identity, but query/hash
    // module instances remain independent exactly as file-URL ESM imports do.
    let mut canonical = file_url(&path)?;
    canonical.set_query(resolved.query());
    canonical.set_fragment(resolved.fragment());
    Ok((
        Resolution::Module {
            id: canonical.to_string(),
        },
        Some((canonical, path)),
    ))
}

pub fn prepare(
    request: &BuildRequest,
    preliminary: &PreliminaryManifest,
) -> Result<ExecutionSnapshot, String> {
    let version = KitSyncVersion::parse(&request.kit.inventory.kit_version)?;
    if request.kit.environment.kit_version != request.kit.inventory.kit_version
        || request.kit.server.kit_version != request.kit.inventory.kit_version
    {
        return Err("Execution initialization profile differs from resolved build profile".into());
    }
    let output = request.kit.config.out_dir.join("output");
    validate_absolute(&output)?;
    if preliminary.server_build_id.len() != 64
        || !preliminary
            .server_build_id
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return Err("Execution snapshot requires the actual SHA-256 server build identity".into());
    }
    let mut paths = BTreeSet::new();
    let mut sources = BTreeMap::new();
    let mut assets = BTreeMap::new();
    let mut bytes_total = 0usize;
    for (file, artifact) in &preliminary.artifacts {
        artifacts::relative(file)?;
        let relative = file
            .strip_prefix("server/")
            .ok_or("Execution snapshot must contain server artifacts only")?;
        let path = output.join(file);
        if !paths.insert(path.clone()) {
            return Err("Execution artifact identity collision".into());
        }
        let bytes = artifacts::bytes(artifact)?;
        bytes_total = bytes_total
            .checked_add(bytes.len())
            .ok_or("Execution artifact size overflow")?;
        if bytes.len() > 16 * 1024 * 1024 || bytes_total > MAX_TOTAL {
            return Err("Execution assets exceed standalone runtime snapshot limits".into());
        }
        // Snapshot read() mirrors the server directory; it can read emitted JS
        // bytes too, but assets never implicitly become executable modules.
        assets.insert(relative.to_owned(), bytes.clone());
        if module_file(file)
            && (artifact.kind == "server" || preliminary.generated_files.contains(file))
        {
            if bytes.len() > MAX_SOURCE {
                return Err("Execution module exceeds standalone runtime source limit".into());
            }
            let source = String::from_utf8(bytes)
                .map_err(|_| format!("Invalid UTF-8 emitted module {file}"))?;
            sources.insert(path, source);
        }
    }
    for path in &paths {
        for ancestor in path.ancestors().skip(1) {
            if paths.contains(ancestor) {
                return Err(format!(
                    "Execution artifact file/directory collision at {}",
                    ancestor.display()
                ));
            }
        }
    }
    for file in &preliminary.generated_files {
        if !preliminary.artifacts.contains_key(file) {
            return Err(format!("Missing generated execution artifact {file}"));
        }
    }
    let entry = |file: &str| -> Result<String, String> {
        artifacts::relative(file)?;
        let path = output.join(file);
        if !sources.contains_key(&path) {
            return Err(format!(
                "Missing required emitted initialization module {file}"
            ));
        }
        Ok(file_url(&path)?.to_string())
    };
    let manifest_entry = entry(&preliminary.manifest_path)?;
    if !preliminary
        .generated_files
        .contains(&preliminary.manifest_path)
    {
        return Err("Execution manifest must be the generated preliminary manifest".into());
    }
    let initialization = if version.is_kit3() {
        Initialization::Kit3 {
            server: entry("server/index.js")?,
        }
    } else {
        Initialization::Kit2 {
            internal: entry("server/internal.js")?,
            environment: entry("server/env.js")?,
            public_prefix: request.kit.environment.public_prefix.clone(),
            private_prefix: request.kit.environment.private_prefix.clone(),
        }
    };
    let mut snapshot = HostSnapshot::default();
    let mut pending = sources
        .keys()
        .map(|path| Ok((file_url(path)?, path.clone())))
        .collect::<Result<VecDeque<_>, String>>()?;
    let mut requirements = BTreeSet::new();
    let mut source_total = 0usize;
    while let Some((url, path)) = pending.pop_front() {
        let id = url.to_string();
        if snapshot.modules.contains_key(&id) {
            continue;
        }
        let source = &sources[&path];
        if snapshot.modules.len() >= MAX_MODULES {
            return Err("Execution snapshot exceeds standalone runtime module limit".into());
        }
        source_total = source_total
            .checked_add(source.len())
            .ok_or("Execution source size overflow")?;
        if source_total > MAX_TOTAL {
            return Err("Execution snapshot exceeds standalone runtime total source limit".into());
        }
        let imports = imports(source).map_err(|error| format!("{id}: {error}"))?;
        let mut resolutions = BTreeMap::new();
        for edge in imports.edges {
            let (resolution, target) = resolve(&url, &edge.specifier, &sources)?;
            if matches!(resolution, Resolution::External { .. }) {
                requirements.insert(ExecutionRequirement{importer:id.clone(),code:"externalModule".into(),specifier:Some(edge.specifier.clone()),start:edge.span.start,end:edge.span.end,message:"External module has no native capability or supplied emitted source; execution of this edge will fail explicitly".into()});
            }
            if let Some(old) = resolutions.insert(edge.specifier.clone(), resolution.clone()) {
                if old != resolution {
                    return Err("Conflicting exact emitted import resolutions".into());
                }
            }
            if let Some(target) = target {
                pending.push_back(target);
            }
        }
        for (span, code, specifier, message) in imports.issues {
            requirements.insert(ExecutionRequirement {
                importer: id.clone(),
                code: code.into(),
                specifier,
                start: span.start,
                end: span.end,
                message: message.into(),
            });
        }
        snapshot.resolutions.insert(id.clone(), resolutions);
        snapshot.modules.insert(
            id.clone(),
            ModuleSource {
                source: source.clone(),
                import_meta_url: id,
            },
        );
    }
    Ok(ExecutionSnapshot {
        bootstrap: Bootstrap {
            server_build_id: preliminary.server_build_id.clone(),
            snapshot,
            manifest_entry,
            initialization,
            environment: request.kit.environment.build_values.clone(),
            assets,
        },
        requirements: requirements.into_iter().collect(),
    })
}
#[cfg(test)]
#[path = "execution_snapshot_tests.rs"]
mod tests;
