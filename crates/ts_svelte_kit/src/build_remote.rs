//! Pure remote-module transform from pinned SvelteKit Vite plugins (MIT,
//! copyright Svelte contributors). Kit 2.67.0–2.70.3 share one implementation;
//! Kit 3.0.0-next.32/3.0.1 share the newer implementation.
//!
//! Contract: discovery receives normalized absolute POSIX identities and explicit
//! package-peer evidence; no filesystem/config execution. Transform receives JS
//! after TS preprocessing, preserving its actual input map. Server output uses
//! installed Kit's real init_remote_functions, including runtime validation of
//! reexports/value types. Client output requires evaluated server metadata and
//! replaces the module completely: no application/server imports survive.
//!
//! Server production output returns a dedicated virtual entry for the host to
//! emit/deduplicate by hash. Hosts must reject hash collisions between different
//! identities (validate_remote_identities). Client metadata order is retained.
//! Dev adds the upstream microtask/HMR hooks; executing a dev server is outside
//! this pure transform. Prerendered-remote postbuild tree shaking is a separate
//! operation and is not represented by this result.
//!
//! AST analysis rejects invalid JS/default exports and stale explicit export
//! metadata. It does not infer remote types from syntax or execute user code.
//! Generated server imports avoid capturing existing bindings/references. Kit 2
//! reserved/string export names, and Kit 3 non-IdentifierName exports, are
//! explicitly unsupported because those upstream client generators emit invalid
//! code for them. Source maps keep exact original anchors and generated suffixes
//! unmapped. Client modules are wholly generated and deliberately have no map.
//!
//! Permission is hereby granted, free of charge, to any person obtaining a copy
//! of this software and associated documentation files (the "Software"), to deal
//! in the Software without restriction, including without limitation the rights
//! to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
//! copies of the Software, and to permit persons to whom the Software is
//! furnished to do so, subject to the following conditions: the above copyright
//! notice and this permission notice shall be included in all copies or
//! substantial portions of the Software. THE SOFTWARE IS PROVIDED "AS IS",
//! WITHOUT WARRANTY OF ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED
//! TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND
//! NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE
//! LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF
//! CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION WITH THE
//! SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.
use crate::build_finalize::{JavaScriptArtifact, SourceMapPolicy, rewrite_generated_artifact};
use crate::sync_versions::KitSyncVersion;
use oxc_allocator::Allocator;
use oxc_ast::ast::{BindingIdentifier, Declaration, IdentifierReference, Statement};
use oxc_ast_visit::Visit;
use oxc_parser::Parser;
use oxc_span::SourceType;
use oxc_syntax::identifier::is_identifier_name;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

const MAX_SOURCE: usize = 16 * 1024 * 1024;
const MAX_EXPORTS: usize = 16_384;
const RESERVED: &[&str] = &[
    "do",
    "if",
    "in",
    "for",
    "let",
    "new",
    "try",
    "var",
    "case",
    "else",
    "enum",
    "eval",
    "null",
    "this",
    "true",
    "void",
    "with",
    "await",
    "break",
    "catch",
    "class",
    "const",
    "false",
    "super",
    "throw",
    "while",
    "yield",
    "delete",
    "export",
    "import",
    "public",
    "return",
    "static",
    "switch",
    "typeof",
    "default",
    "extends",
    "finally",
    "package",
    "private",
    "continue",
    "debugger",
    "function",
    "arguments",
    "interface",
    "protected",
    "implements",
    "instanceof",
];
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RemoteDiscoveryInput {
    pub kit_version: String,
    pub module_id: String,
    pub module_extensions: Vec<String>,
    /// Required for Kit 3 identities containing node_modules. True means the
    /// host inspected the applicable package ancestry and found a Kit peer.
    pub package_has_kit_peer: Option<bool>,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum RemoteTarget {
    Server,
    Client,
    ServiceWorker,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RemoteKind {
    Command,
    Form,
    Prerender,
    Query,
    QueryBatch,
    QueryLive,
}
impl RemoteKind {
    fn function(self) -> &'static str {
        match self {
            Self::Command => "command",
            Self::Form => "form",
            Self::Prerender => "prerender",
            Self::Query => "query",
            Self::QueryBatch => "query_batch",
            Self::QueryLive => "query_live",
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RemoteExport {
    pub name: String,
    pub kind: RemoteKind,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EvaluatedRemoteExports {
    pub hash: String,
    pub file: String,
    /// Actual evaluated module exports in analysis order, not guessed syntax.
    pub exports: Vec<RemoteExport>,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RemoteTransformInput {
    pub discovery: RemoteDiscoveryInput,
    pub project_root: String,
    pub enabled: bool,
    pub development: bool,
    pub target: RemoteTarget,
    /// Relative emitted artifact identity, ending in .js/.mjs, for map updates.
    pub artifact_name: String,
    pub artifact: JavaScriptArtifact,
    pub source_maps: SourceMapPolicy,
    /// Installed runtime/client/remote-functions/index.js (Kit 3 client only).
    pub client_runtime_id: Option<String>,
    pub metadata: Option<EvaluatedRemoteExports>,
}
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RemoteIdentity {
    pub hash: String,
    pub file: String,
    pub module_id: String,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteEntry {
    pub id: String,
    pub name: String,
    pub code: String,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteTransformOutput {
    pub identity: RemoteIdentity,
    pub artifact: JavaScriptArtifact,
    pub entry: Option<RemoteEntry>,
    pub explicit_exports: Vec<String>,
    pub has_star_reexports: bool,
    pub updated_map_files: BTreeMap<String, Value>,
    pub removed_map_files: BTreeSet<String>,
}
fn absolute(path: &str) -> Result<Vec<&str>, String> {
    if !path.starts_with('/')
        || path.contains(['\\', '\0', '?', '#'])
        || path.chars().any(char::is_control)
    {
        return Err(format!(
            "Expected normalized absolute POSIX module path without query/fragment: {path:?}"
        ));
    }
    let parts: Vec<_> = path[1..].split('/').collect();
    if path != "/"
        && parts
            .iter()
            .any(|part| part.is_empty() || matches!(*part, "." | ".."))
    {
        return Err(format!("Unnormalized remote module path: {path:?}"));
    }
    Ok(if path == "/" { vec![] } else { parts })
}
fn relative(from: &str, to: &str) -> Result<String, String> {
    let from = absolute(from)?;
    let to = absolute(to)?;
    let common = from.iter().zip(&to).take_while(|(a, b)| a == b).count();
    Ok(std::iter::repeat_n("..", from.len() - common)
        .chain(to[common..].iter().copied())
        .collect::<Vec<_>>()
        .join("/"))
}
/// Exact Kit djb2 over *reversed UTF-16 code units*, rendered unsigned base 36.
pub fn remote_hash(file: &str) -> String {
    let mut hash = 5381u32;
    for unit in file.encode_utf16().collect::<Vec<_>>().into_iter().rev() {
        hash = hash.wrapping_mul(33) ^ u32::from(unit);
    }
    if hash == 0 {
        return "0".into();
    }
    let mut digits = vec![];
    while hash != 0 {
        digits.push(b"0123456789abcdefghijklmnopqrstuvwxyz"[(hash % 36) as usize] as char);
        hash /= 36;
    }
    digits.into_iter().rev().collect()
}
pub fn discover_remote(input: &RemoteDiscoveryInput) -> Result<bool, String> {
    let version = KitSyncVersion::parse(&input.kit_version)?;
    absolute(&input.module_id)?;
    if version.is_kit3() {
        let basename = input.module_id.rsplit('/').next().unwrap_or("");
        let candidate = basename
            .strip_prefix("remote.")
            .is_some_and(|suffix| !suffix.is_empty())
            || basename
                .rsplit_once(".remote.")
                .is_some_and(|(_, suffix)| !suffix.is_empty());
        if !candidate {
            return Ok(false);
        }
        if input.module_id.contains("node_modules") {
            return input.package_has_kit_peer.ok_or_else(|| {
                "Kit 3 dependency remote discovery requires explicit package Kit-peer evidence"
                    .into()
            });
        }
        Ok(true)
    } else {
        for ext in &input.module_extensions {
            if !ext.starts_with('.') || ext.len() < 2 || ext.contains(['/', '\\', '?', '#']) {
                return Err(format!("Invalid resolved module extension {ext:?}"));
            }
        }
        Ok(input
            .module_extensions
            .iter()
            .any(|ext| input.module_id.ends_with(&format!(".remote{ext}"))))
    }
}
/// Run for the complete remote inventory before emitting entries or metadata.
pub fn validate_remote_identities(identities: &[RemoteIdentity]) -> Result<(), String> {
    let mut hashes = BTreeMap::new();
    for identity in identities {
        if remote_hash(&identity.file) != identity.hash {
            return Err("Remote identity hash mismatch".into());
        }
        if let Some(old) = hashes.insert(&identity.hash, &identity.module_id) {
            if old != &identity.module_id {
                return Err(format!(
                    "Remote hash collision {} between {old} and {}",
                    identity.hash, identity.module_id
                ));
            }
        }
    }
    Ok(())
}
#[derive(Default)]
struct Names {
    used: BTreeSet<String>,
}
impl<'a> Visit<'a> for Names {
    fn visit_binding_identifier(&mut self, node: &BindingIdentifier<'a>) {
        self.used.insert(node.name.to_string());
    }
    fn visit_identifier_reference(&mut self, node: &IdentifierReference<'a>) {
        self.used.insert(node.name.to_string());
    }
}
struct Inspection {
    names: BTreeSet<String>,
    exports: BTreeSet<String>,
    stars: bool,
}
fn inspect(code: &str) -> Result<Inspection, String> {
    if code.len() > MAX_SOURCE {
        return Err("Remote JavaScript source exceeds 16 MiB".into());
    }
    let allocator = Allocator::default();
    let parsed = Parser::new(&allocator, code, SourceType::mjs()).parse();
    if let Some(error) = parsed.diagnostics.first() {
        return Err(format!(
            "Remote transform requires preprocessed JavaScript: {error}"
        ));
    }
    let mut names = Names::default();
    names.visit_program(&parsed.program);
    let mut exports = BTreeSet::new();
    let mut stars = false;
    for statement in &parsed.program.body {
        match statement {
            Statement::ExportDefaultDeclaration(_) => {
                exports.insert("default".into());
            }
            Statement::ExportNamedDeclaration(node) => {
                for specifier in &node.specifiers {
                    exports.insert(specifier.exported.name().to_string());
                }
            }
            Statement::ExportFromDeclaration(node) => {
                for specifier in &node.specifiers {
                    exports.insert(specifier.exported.name().to_string());
                }
            }
            Statement::ExportAllDeclaration(node) => {
                if let Some(name) = &node.exported {
                    exports.insert(name.name().to_string());
                } else {
                    stars = true;
                }
            }
            Statement::ExportDeclaration(node) => match &node.declaration {
                Declaration::VariableDeclaration(node) => {
                    for declaration in &node.declarations {
                        for id in declaration.id.get_binding_identifiers() {
                            exports.insert(id.name.to_string());
                        }
                    }
                }
                Declaration::FunctionDeclaration(node) => {
                    if let Some(id) = &node.id {
                        exports.insert(id.name.to_string());
                    }
                }
                Declaration::ClassDeclaration(node) => {
                    if let Some(id) = &node.id {
                        exports.insert(id.name.to_string());
                    }
                }
                _ => return Err("Unsupported non-JavaScript remote export declaration".into()),
            },
            _ => {}
        }
    }
    if exports.contains("default") {
        return Err("Cannot export default from a remote module; use named remote exports".into());
    }
    if exports.len() > MAX_EXPORTS {
        return Err("Too many remote exports".into());
    }
    Ok(Inspection {
        names: names.used,
        exports,
        stars,
    })
}
fn fresh(names: &mut BTreeSet<String>, base: &str) -> String {
    let mut value = base.to_owned();
    let mut n = 0;
    while !names.insert(value.clone()) {
        n += 1;
        value = format!("{base}{n}");
    }
    value
}
fn quoted(value: &str) -> String {
    serde_json::to_string(value).expect("String serialization")
}
fn vlq(value: i64, out: &mut String) {
    let mut v = if value < 0 {
        ((-value) as u64) * 2 + 1
    } else {
        (value as u64) * 2
    };
    loop {
        let mut digit = (v & 31) as usize;
        v >>= 5;
        if v != 0 {
            digit |= 32;
        }
        out.push(
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/"[digit] as char,
        );
        if v == 0 {
            break;
        }
    }
}
fn identity_map(source: &str, id: &str) -> Result<Value, String> {
    // Exact scalar boundaries, UTF-16 columns. CRLF is one line; generated
    // suffixes are subsequently unmapped by the shared edit composer.
    let mut mappings = String::new();
    let (mut line, mut col) = (0i64, 0i64);
    let (mut old_line, mut old_col, mut generated_col) = (0i64, 0i64, 0i64);
    let mut segment = false;
    let mut count = 0;
    let mut previous_cr = false;
    for ch in source.chars() {
        if ch == '\n' && previous_cr {
            previous_cr = false;
            continue;
        }
        previous_cr = false;
        if matches!(ch, '\n' | '\r' | '\u{2028}' | '\u{2029}') {
            mappings.push(';');
            line += 1;
            col = 0;
            generated_col = 0;
            segment = false;
            previous_cr = ch == '\r';
            continue;
        }
        count += 1;
        if count > 1_000_000 {
            return Err("Identity source map exceeds one million exact anchors; supply a composed input map".into());
        }
        if segment {
            mappings.push(',');
        }
        vlq(col - generated_col, &mut mappings);
        vlq(0, &mut mappings);
        vlq(line - old_line, &mut mappings);
        vlq(col - old_col, &mut mappings);
        old_line = line;
        old_col = col;
        generated_col = col;
        segment = true;
        col += ch.len_utf16() as i64;
    }
    Ok(json!({"version":3,"sources":[id],"sourcesContent":[source],"names":[],"mappings":mappings}))
}

pub fn transform_remote(input: &RemoteTransformInput) -> Result<RemoteTransformOutput, String> {
    let version = KitSyncVersion::parse(&input.discovery.kit_version)?;
    if !discover_remote(&input.discovery)? {
        return Err("Input is not a remote module for this registered Kit contract".into());
    }
    if !input.enabled {
        return Err("Remote functions require experimental.remoteFunctions: true".into());
    }
    if input.target == RemoteTarget::ServiceWorker {
        return Err("Remote functions cannot be transformed for a service worker".into());
    }
    if !input.artifact.pending.is_empty() || input.artifact.receipt_sha256.is_some() {
        return Err("Remote transforms must precede manifest receipt phases".into());
    }
    let file = relative(&input.project_root, &input.discovery.module_id)?;
    let identity = RemoteIdentity {
        hash: remote_hash(&file),
        file,
        module_id: input.discovery.module_id.clone(),
    };
    let mut inspected = inspect(&input.artifact.code)?;
    let mut output = RemoteTransformOutput {
        identity: identity.clone(),
        artifact: input.artifact.clone(),
        entry: None,
        explicit_exports: inspected.exports.iter().cloned().collect(),
        has_star_reexports: inspected.stars,
        updated_map_files: BTreeMap::new(),
        removed_map_files: BTreeSet::new(),
    };
    if input.target == RemoteTarget::Server {
        let namespace = fresh(&mut inspected.names, "$$_self_$$");
        let init = fresh(&mut inspected.names, "$$_init_$$");
        let basename = input
            .discovery
            .module_id
            .rsplit('/')
            .next()
            .ok_or("Missing module basename")?;
        let internal = if version.is_kit3() {
            "@sveltejs/kit/internal/server"
        } else {
            "@sveltejs/kit/internal"
        };
        let dev = if input.development {
            "await Promise.resolve();\n\n"
        } else {
            ""
        };
        let suffix = format!(
            "\n\nimport * as {namespace} from {};\nimport {{ init_remote_functions as {init} }} from {};\n\n{dev}{init}({namespace}, {}, {});\n\nfor (const [name, fn] of Object.entries({namespace})) {{\n  fn.__.id = {} + '/' + name;\n  fn.__.name = name;\n}}\n",
            quoted(&format!("./{basename}")),
            quoted(internal),
            quoted(&identity.file),
            quoted(&identity.hash),
            quoted(&identity.hash)
        );
        let mut artifact = input.artifact.clone();
        if input.source_maps == SourceMapPolicy::Preserve
            && artifact.map.is_none()
            && artifact.map_file.is_none()
        {
            artifact.map = Some(identity_map(&artifact.code, &identity.module_id)?);
        }
        let result = rewrite_generated_artifact(
            &input.artifact_name,
            &artifact,
            vec![(artifact.code.len(), artifact.code.len(), suffix)],
            input.source_maps,
        )?;
        output.artifact = result
            .artifacts
            .get(&input.artifact_name)
            .ok_or("Remote artifact was not produced")?
            .clone();
        output.updated_map_files = result.updated_map_files;
        output.removed_map_files = result.removed_map_files;
        if !input.development {
            output.entry = Some(RemoteEntry {
                id: format!("\0sveltekit-remote:{}", identity.hash),
                name: format!("remote-{}", identity.hash),
                code: format!(
                    "import * as m from {};\nexport default m;",
                    quoted(&identity.module_id)
                ),
            });
        }
    } else {
        let metadata = input
            .metadata
            .as_ref()
            .ok_or("Client remote transform requires evaluated server export metadata")?;
        if metadata.hash != identity.hash || metadata.file != identity.file {
            return Err("Evaluated remote metadata identity does not match this module".into());
        }
        if metadata.exports.len() > MAX_EXPORTS {
            return Err("Too many evaluated remote exports".into());
        }
        let mut names = BTreeSet::new();
        for export in &metadata.exports {
            if !names.insert(export.name.clone()) {
                return Err(format!("Duplicate evaluated remote export {}", export.name));
            }
            if export.name == "default"
                || !is_identifier_name(&export.name)
                || (!version.is_kit3() && RESERVED.contains(&export.name.as_str()))
            {
                return Err(format!(
                    "Unsupported remote export name {:?} for Kit {} client generator",
                    export.name, input.discovery.kit_version
                ));
            }
        }
        if !inspected.exports.is_subset(&names) || (!inspected.stars && names != inspected.exports)
        {
            return Err("Evaluated remote export names disagree with the parsed module".into());
        }
        let mut namespace = "__remote".to_owned();
        let mut counter = if version.is_kit3() { 0 } else { 1 };
        while names.contains(&namespace) {
            namespace = format!("__remote{counter}");
            counter += 1;
        }
        let runtime = if version.is_kit3() {
            let runtime = input
                .client_runtime_id
                .as_ref()
                .ok_or("Kit 3 requires its installed client remote runtime identity")?;
            let directory = input
                .discovery
                .module_id
                .rsplit_once('/')
                .map(|(d, _)| if d.is_empty() { "/" } else { d })
                .ok_or("Module directory missing")?;
            let path = relative(directory, runtime)?;
            if path.starts_with('.') {
                path
            } else {
                format!("./{path}")
            }
        } else {
            "__sveltekit/remote".into()
        };
        let mut code = format!("import * as {namespace} from {};\n\n", quoted(&runtime));
        let mut aliases = vec![];
        let mut alias_index = 0;
        for export in &metadata.exports {
            let expression = format!(
                "{namespace}.{}({})",
                export.kind.function(),
                quoted(&format!("{}/{}", identity.hash, export.name))
            );
            if RESERVED.contains(&export.name.as_str()) {
                let mut alias = format!("_{alias_index}");
                alias_index += 1;
                while names.contains(&alias) {
                    alias = format!("_{alias_index}");
                    alias_index += 1;
                }
                code.push_str(&format!("const {alias} = {expression};\n"));
                aliases.push(format!("{alias} as {}", export.name));
            } else {
                code.push_str(&format!("export const {} = {expression};\n", export.name));
            }
        }
        if !aliases.is_empty() {
            code.push_str(&format!("export {{ {} }};\n", aliases.join(", ")));
        }
        if input.development {
            code.push_str("\nimport.meta.hot?.accept();\n");
        }
        if input.source_maps == SourceMapPolicy::Reject
            && (input.artifact.map.is_some() || input.artifact.map_file.is_some())
        {
            return Err("Replacing a client remote module removes its original source map; select Preserve or Drop".into());
        }
        if let Some(file) = &input.artifact.map_file {
            output.removed_map_files.insert(file.clone());
        }
        output.artifact = JavaScriptArtifact {
            code,
            map: None,
            map_file: None,
            pending: vec![],
            receipt_sha256: None,
        };
        // Parse generated output too: export names must never inject source.
        inspect(&output.artifact.code)?;
    }
    Ok(output)
}
#[cfg(test)]
#[path = "build_remote_tests.rs"]
mod tests;
