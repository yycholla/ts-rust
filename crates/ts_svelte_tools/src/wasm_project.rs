//! Filesystem-free Svelte project snapshots for the persistent semantic worker.
//! Framework plugins supply resolved files/options. This layer owns projections
//! and exact editor mappings, never a filesystem, process, or JavaScript checker.
use crate::editor_map::EditorMap;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet};
use ts_svelte::{ByteRange, Mapping, Position, Range, Snapshot, compatibility::Versions};

const MAX_FILES: usize = 100_000;
const MAX_BYTES: usize = 256 * 1024 * 1024;

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EditorSnapshot {
    pub uri: String,
    pub version: i32,
    pub text: String,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Input {
    root: String,
    files: BTreeMap<String, String>,
    #[serde(default)]
    components: Vec<Component>,
    #[serde(default)]
    documents: Vec<EditorSnapshot>,
    #[serde(default)]
    root_files: Vec<String>,
    #[serde(default)]
    compiler_options: Option<Map<String, Value>>,
    #[serde(default)]
    compatibility: Versions,
    #[serde(default)]
    runes: Option<bool>,
    #[serde(default)]
    preprocess: Value,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Component {
    path: String,
    document: EditorSnapshot,
    #[serde(default)]
    typescript: Option<bool>,
    #[serde(default = "yes")]
    check: bool,
    #[serde(default)]
    preprocess: Value,
}
const fn yes() -> bool {
    true
}

struct Source {
    original: EditorSnapshot,
    generated_uri: String,
    code: String,
    language: &'static str,
    component: bool,
    required: bool,
    semantic_available: bool,
}
struct Ticket {
    op: String,
    original: String,
    generated: String,
    syntax: Option<Value>,
}

/// Owns the original and generated snapshots together. Request tickets and
/// completion tokens belong to this revision and are never accepted by another.
pub struct WasmProject {
    revision: u32,
    root: String,
    files: BTreeMap<String, String>,
    sources: BTreeMap<String, Source>,
    maps: EditorMap,
    projections: Vec<Value>,
    diagnostics: Vec<Value>,
    module_transpilation: Vec<Value>,
    versions: Versions,
    runes: Option<bool>,
    component_diagnostics: BTreeMap<String, Vec<Value>>,
    projection_failures: BTreeMap<String, Vec<Value>>,
    loaded_files: Option<BTreeSet<String>>,
    tickets: BTreeMap<u64, Ticket>,
    completions: BTreeMap<u64, (String, Value)>,
    next_token: u64,
}

fn path(value: &str) -> Result<String, String> {
    if !value.starts_with('/')
        || value.contains(['\\', '\0', '?', '#'])
        || (value != "/" && value.ends_with('/'))
        || value[1..]
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err("Expected a canonical absolute POSIX path".into());
    }
    Ok(value.to_owned())
}
fn relative(root: &str, value: &str) -> Result<String, String> {
    if value.starts_with('/') {
        return Err("Project source paths must be relative".into());
    }
    let full = path(&format!("{root}/{value}"))?;
    if value == "tsconfig.json"
        || value.starts_with("__svelte/")
        || value.starts_with("node_modules/")
    {
        return Err("Source path uses a reserved project name".into());
    }
    Ok(full)
}

// Built-in TypeScript declarations use a virtual filesystem identity, not a
// local path. Accept only the checker's canonical bundled library namespace.
fn bundled_library(value: &str) -> bool {
    value.strip_prefix("bundled:///libs/").is_some_and(|name| {
        name.starts_with("lib.")
            && name.ends_with(".d.ts")
            && name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'.')
    })
}

fn loaded_identity(value: &str) -> Result<String, String> {
    if bundled_library(value) {
        Ok(value.into())
    } else {
        path(value)
    }
}

fn report_identity(value: &str) -> Result<String, String> {
    if bundled_library(value) {
        Ok(value.into())
    } else {
        uri_path(value)
    }
}
fn uri(path: &str) -> Result<String, String> {
    let mut url = url::Url::parse("file:///").map_err(|e| e.to_string())?;
    {
        let mut segments = url.path_segments_mut().map_err(|()| "Invalid file URL")?;
        segments.clear();
        for segment in path.trim_start_matches('/').split('/') {
            segments.push(segment);
        }
    }
    Ok(url.into())
}
fn uri_path(value: &str) -> Result<String, String> {
    let url = url::Url::parse(value).map_err(|e| e.to_string())?;
    if url.scheme() != "file"
        || url.host_str().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err("Expected a local file URI without query or fragment".into());
    }
    let decoded = percent_encoding::percent_decode_str(url.path())
        .decode_utf8()
        .map_err(|e| e.to_string())?;
    path(&decoded)
}

fn normalize_semantic_uris(value: &mut Value) -> Result<(), String> {
    match value {
        Value::Array(items) => {
            for item in items {
                normalize_semantic_uris(item)?;
            }
        }
        Value::Object(object) => {
            for (key, value) in object {
                if key == "changes" {
                    if let Some(changes) = value.as_object_mut() {
                        let old = std::mem::take(changes);
                        for (name, edits) in old {
                            let name = if name.starts_with("file:") {
                                uri(&uri_path(&name)?)?
                            } else {
                                name
                            };
                            if changes.insert(name, edits).is_some() {
                                return Err("Duplicate workspace edit URI aliases".into());
                            }
                        }
                    }
                }
                if matches!(key.as_str(), "uri" | "targetUri") {
                    if let Some(text) = value.as_str().filter(|text| text.starts_with("file:")) {
                        *value = json!(uri(&uri_path(text)?)?);
                    }
                } else {
                    normalize_semantic_uris(value)?;
                }
            }
        }
        _ => {}
    }
    Ok(())
}
fn snapshot(document: &EditorSnapshot, revision: u32) -> Snapshot {
    Snapshot {
        uri: document.uri.clone(),
        version: revision,
        text: document.text.clone(),
    }
}
fn compiler_response(
    op: &str,
    document: &Snapshot,
    input: &Input,
    typescript: Option<bool>,
) -> Value {
    let mut request = json!({"op":op,"document":document,"compatibility":input.compatibility});
    if op != "checkModule" {
        request["runes"] = json!(input.runes);
    }
    if op == "compile" {
        request["target"] = json!("client");
    }
    if let Some(typescript) = typescript {
        request["typescript"] = json!(typescript);
    }
    ts_svelte_compiler::dispatch(request.to_string().as_bytes())
}
fn compiler(
    op: &str,
    document: &Snapshot,
    input: &Input,
    typescript: Option<bool>,
) -> Result<Value, String> {
    let response = compiler_response(op, document, input, typescript);
    if response["ok"] != true {
        return Err(response.to_string());
    }
    Ok(response["result"].clone())
}

fn diagnostics_of(value: &Value) -> Vec<Value> {
    value["diagnostics"].as_array().cloned().unwrap_or_default()
}

/// Only source diagnostics are recoverable. Unsupported versions, backend
/// limitations and protocol failures must never become an empty success.
fn component_result(response: Value) -> Result<Result<Value, Vec<Value>>, String> {
    if response["ok"] == true {
        return Ok(Ok(response["result"].clone()));
    }
    let diagnostics = diagnostics_of(&response);
    if matches!(
        response["error"]["code"].as_str(),
        Some("svelte_compile" | "svelte_projection")
    ) && !diagnostics.is_empty()
        && diagnostics.iter().any(|d| d["severity"] == "error")
    {
        Ok(Err(diagnostics))
    } else {
        Err(response.to_string())
    }
}
fn insert(files: &mut BTreeMap<String, String>, name: String, text: String) -> Result<(), String> {
    if files.insert(name.clone(), text).is_some() {
        return Err(format!(
            "Generated path collides with supplied file: {name}"
        ));
    }
    Ok(())
}
fn masked(text: &str) -> String {
    text.bytes()
        .map(|byte| {
            if byte == b'\r' || byte == b'\n' {
                char::from(byte)
            } else {
                ' '
            }
        })
        .collect()
}

fn validate_languages(document: &Snapshot, runes: Option<bool>) -> Result<(), String> {
    let prepared = rsvelte::Engine::new()
        .prepare(
            &document.text,
            rsvelte::ComponentOptions::new()
                .filename(&document.uri)
                .runes(runes),
        )
        .map_err(|error| error.to_string())?;
    let facts = prepared.facts();
    let mut headers: Vec<(&str, bool)> = facts
        .scripts
        .iter()
        .map(|script| {
            (
                &document.text[script.tag.start() as usize..script.content.start() as usize],
                true,
            )
        })
        .collect();
    if let Some(style) = &facts.style {
        headers.push((
            &document.text[style.tag.start() as usize..style.content.start() as usize],
            false,
        ));
    }
    for (header, script) in headers {
        let bytes = header.as_bytes();
        let mut at = 1;
        while at < bytes.len() && !bytes[at].is_ascii_whitespace() && bytes[at] != b'>' {
            at += 1;
        }
        while at < bytes.len() {
            while at < bytes.len() && bytes[at].is_ascii_whitespace() {
                at += 1;
            }
            if at == bytes.len() || bytes[at] == b'>' {
                break;
            }
            let start = at;
            while at < bytes.len()
                && !bytes[at].is_ascii_whitespace()
                && !matches!(bytes[at], b'=' | b'>')
            {
                at += 1;
            }
            let name = &header[start..at];
            while at < bytes.len() && bytes[at].is_ascii_whitespace() {
                at += 1;
            }
            let mut value = None;
            if at < bytes.len() && bytes[at] == b'=' {
                at += 1;
                while at < bytes.len() && bytes[at].is_ascii_whitespace() {
                    at += 1;
                }
                if at < bytes.len() && matches!(bytes[at], b'\'' | b'"') {
                    let quote = bytes[at];
                    at += 1;
                    let start = at;
                    while at < bytes.len() && bytes[at] != quote {
                        at += 1;
                    }
                    value = Some(&header[start..at]);
                    at += usize::from(at < bytes.len());
                } else {
                    let start = at;
                    while at < bytes.len() && !bytes[at].is_ascii_whitespace() && bytes[at] != b'>'
                    {
                        at += 1;
                    }
                    value = Some(&header[start..at]);
                }
            }
            if name == "src"
                || name == "lang"
                    && !matches!(
                        (script, value),
                        (true, Some("ts" | "typescript" | "js" | "javascript"))
                            | (false, Some("css"))
                    )
                || name == "type"
                    && !matches!(
                        (script, value),
                        (
                            true,
                            Some("text/javascript" | "application/javascript" | "module")
                        ) | (false, Some("text/css"))
                    )
            {
                return Err(format!(
                    "Unsupported preprocessing attribute {name} in {}",
                    document.uri
                ));
            }
            if at == start {
                return Err("Invalid script/style header".into());
            }
        }
    }
    Ok(())
}

impl WasmProject {
    pub fn new(input: Value) -> Result<Self, String> {
        Self::build(input, 1)
    }

    fn unavailable(&mut self, component: &Component, diagnostics: Vec<Value>) {
        let original = component.document.uri.clone();
        self.projection_failures
            .insert(original.clone(), diagnostics);
        self.sources.insert(
            original.clone(),
            Source {
                original: component.document.clone(),
                generated_uri: original,
                code: String::new(),
                language: "svelte",
                component: true,
                required: component.check,
                semantic_available: false,
            },
        );
    }

    fn is_loaded(&self, source: &Source) -> bool {
        self.loaded_files.as_ref().is_some_and(|files| {
            uri_path(&source.generated_uri).is_ok_and(|path| files.contains(&path))
        })
    }

    fn source_compiler_diagnostics(&self, original: &str) -> Vec<Value> {
        let mut diagnostics = self
            .component_diagnostics
            .get(original)
            .cloned()
            .unwrap_or_default();
        for diagnostic in self.projection_failures.get(original).into_iter().flatten() {
            // Compile and projection validation may report the same source error.
            if !diagnostics
                .iter()
                .any(|d| d["code"] == diagnostic["code"] && d["range"] == diagnostic["range"])
            {
                diagnostics.push(diagnostic.clone());
            }
        }
        if let Some(source) = self.sources.get(original) {
            for diagnostic in &mut diagnostics {
                diagnostic["version"] = json!(source.original.version);
            }
        }
        diagnostics
    }

    fn all_compiler_diagnostics(&self) -> Vec<Value> {
        let mut diagnostics = self.diagnostics.clone();
        for (original, source) in &self.sources {
            if source.component && (source.required || self.is_loaded(source)) {
                diagnostics.extend(self.source_compiler_diagnostics(original));
            }
        }
        diagnostics
    }

    fn unavailable_metadata(&self) -> Vec<Value> {
        self.sources
            .values()
            .filter(|s| !s.semantic_available)
            .map(|source| {
                json!({"uri":source.original.uri,"version":source.original.version,
                "required":source.required,"reason":"source_diagnostics",
                "diagnostics":self.source_compiler_diagnostics(&source.original.uri)})
            })
            .collect()
    }

    fn semantic_complete(&self) -> bool {
        // An unavailable projection may be referenced through a module that
        // currently fails to resolve. Loaded-file absence is not proof that an
        // invalid auxiliary is irrelevant; preserve that limitation explicitly.
        self.sources
            .values()
            .all(|source| source.semantic_available)
            && (self.loaded_files.is_some()
                || !self
                    .sources
                    .values()
                    .any(|source| source.component && !source.required))
    }

    fn compiler_report(&self, original: &str, semantic_complete: bool) -> Value {
        let mut items = Vec::new();
        let mut unmapped = Vec::new();
        for diagnostic in self.source_compiler_diagnostics(original) {
            if serde_json::from_value::<Range>(diagnostic["range"].clone()).is_ok() {
                items.push(json!({"range":diagnostic["range"],"code":diagnostic["code"],
                    "message":diagnostic["message"],"severity":if diagnostic["severity"] == "error" {1} else {2},
                    "source":"svelte","data":{"compilerDiagnostic":diagnostic}}));
            } else {
                unmapped.push(json!({"status":"unmapped","diagnostic":diagnostic}));
            }
        }
        json!({"kind":"full","items":items,"unmappedCompilerDiagnostics":unmapped,
            "semanticComplete":semantic_complete})
    }

    /// A complete, verified checker graph is the authority for deferred checks.
    /// No auxiliary is opened just to discover whether it belongs to the graph.
    fn acknowledge_loaded(&mut self, response: &Value) -> Result<(), String> {
        if response["projectionsVerified"] != true {
            return Err("Checker did not acknowledge the current projection set".into());
        }
        let values = response["loadedFiles"]
            .as_array()
            .ok_or("Missing loaded checker files")?;
        if values.len() > MAX_FILES {
            return Err("Loaded file acknowledgement exceeds limits".into());
        }
        let loaded = values
            .iter()
            .map(|value| loaded_identity(value.as_str().ok_or("Invalid loaded file")?))
            .collect::<Result<BTreeSet<_>, String>>()?;
        for projection in &self.projections {
            if projection["required"] == true
                && !loaded.contains(
                    projection["fileName"]
                        .as_str()
                        .ok_or("Invalid projection filename")?,
                )
            {
                return Err("Checked projection absent from acknowledged checker graph".into());
            }
        }
        for report in response["documents"]
            .as_array()
            .ok_or("Missing checker reports")?
        {
            if !loaded.contains(&report_identity(
                report["uri"].as_str().ok_or("Missing report URI")?,
            )?) {
                return Err("Diagnostic report does not belong to acknowledged graph".into());
            }
        }
        let mut checked = Vec::new();
        for (original, source) in &self.sources {
            if !source.component
                || source.required
                || self.component_diagnostics.contains_key(original)
                || !loaded.contains(&uri_path(&source.generated_uri)?)
            {
                continue;
            }
            let response = ts_svelte_compiler::dispatch(
                json!({"op":"compile",
                "document":snapshot(&source.original,self.revision),"compatibility":self.versions,
                "runes":self.runes,"target":"client"})
                .to_string()
                .as_bytes(),
            );
            let diagnostics = component_result(response)?
                .map_or_else(|errors| errors, |value| diagnostics_of(&value));
            checked.push((original.clone(), diagnostics));
        }
        self.component_diagnostics.extend(checked);
        self.loaded_files = Some(loaded);
        Ok(())
    }

    fn build(value: Value, revision: u32) -> Result<Self, String> {
        let input: Input = serde_json::from_value(value).map_err(|e| e.to_string())?;
        ts_svelte::compatibility::resolve(&input.compatibility)?;
        let root = path(&input.root)?;
        if root == "/" {
            return Err("Use a named virtual project root".into());
        }
        if !input.preprocess.is_null() || input.components.iter().any(|c| !c.preprocess.is_null()) {
            return Err("Unsupported preprocessing: supply original Svelte snapshots; preprocessed sources require composed original mappings".into());
        }
        if input.root_files.is_empty() && !input.components.iter().any(|component| component.check)
        {
            return Err("Provide checked components or explicit rootFiles".into());
        }
        if input.files.len() > MAX_FILES
            || input.files.values().map(String::len).sum::<usize>() > MAX_BYTES
        {
            return Err("Project file snapshot exceeds limits".into());
        }
        if input.components.len() + input.documents.len() > 4096 {
            return Err("At most 4096 editor snapshots are supported per project".into());
        }
        for (name, text) in &input.files {
            path(name)?;
            if !name.starts_with(&format!("{root}/"))
                || name.starts_with(&format!("{root}/__svelte/"))
            {
                return Err(
                    "All supplied files must remain inside root and outside __svelte".into(),
                );
            }
            if text.len() > 8 * 1024 * 1024 {
                return Err("File exceeds 8 MiB".into());
            }
        }
        let mut files = input.files.clone();
        // The project accepts resolved compiler options, not an implicit second
        // tsconfig with extends/references that would silently change selection.
        if files.contains_key(&format!("{root}/tsconfig.json")) {
            return Err("Supply resolved compilerOptions instead of root tsconfig.json".into());
        }
        let mut output = Self {
            revision,
            root: root.clone(),
            files: BTreeMap::new(),
            sources: BTreeMap::new(),
            maps: EditorMap::new(),
            projections: Vec::new(),
            diagnostics: Vec::new(),
            module_transpilation: Vec::new(),
            versions: input.compatibility.clone(),
            runes: input.runes,
            component_diagnostics: BTreeMap::new(),
            projection_failures: BTreeMap::new(),
            loaded_files: None,
            tickets: BTreeMap::new(),
            completions: BTreeMap::new(),
            next_token: u64::from(revision) << 32,
        };
        let mut checked = BTreeSet::new();
        let mut declarations = Map::new();
        if !input.components.is_empty() {
            let package_path = format!("{root}/node_modules/svelte/package.json");
            let package: Value = serde_json::from_str(
                files
                    .get(&package_path)
                    .ok_or("Missing supplied Svelte package.json")?,
            )
            .map_err(|e| e.to_string())?;
            if package["version"] != input.compatibility.svelte_version {
                return Err(
                    "Supplied Svelte package version differs from compatibility profile".into(),
                );
            }
            let types = files
                .get(&format!("{root}/node_modules/svelte/types/index.d.ts"))
                .ok_or("Missing Svelte declarations")?
                .clone();
            let marker = "declare module '*.svelte' {";
            let start = types
                .find(marker)
                .ok_or("Missing expected Svelte wildcard declaration")?;
            if types[start + marker.len()..].contains(marker) {
                return Err("Ambiguous Svelte wildcard declaration".into());
            }
            let end = start
                + types[start..]
                    .find("\n}")
                    .ok_or("Unclosed Svelte wildcard declaration")?
                + 2;
            if !types[start..end].is_ascii() {
                return Err("Unexpected non-ASCII wildcard declaration".into());
            }
            let mut sanitized = types.clone();
            sanitized.replace_range(start..end, &masked(&types[start..end]));
            insert(&mut files, format!("{root}/__svelte/types.d.ts"), sanitized)?;
            let shim = files
                .get(&format!(
                    "{root}/node_modules/svelte-check/dist/src/svelte-shims-v4.d.ts"
                ))
                .ok_or("Missing supplied svelte-check v4 shim")?
                .clone();
            insert(&mut files, format!("{root}/__svelte/shims.d.ts"), shim)?;
            insert(
                &mut files,
                format!("{root}/__svelte/type-roots/svelte/index.d.ts"),
                "// Real Svelte declarations are explicit project roots.\n".into(),
            )?;
            let html = format!("{root}/node_modules/svelte/svelte-html.d.ts");
            if !files.contains_key(&html) {
                return Err("Missing supplied Svelte HTML declarations".into());
            }
            checked.extend([
                format!("{root}/__svelte/types.d.ts"),
                format!("{root}/__svelte/shims.d.ts"),
                html,
            ]);
            for quote in ['\'', '"'] {
                for declaration in types.split(&format!("declare module {quote}")).skip(1) {
                    let name = declaration
                        .split(quote)
                        .next()
                        .ok_or("Malformed module declaration")?;
                    if !name.contains('*') {
                        declarations
                            .insert(name.into(), json!([format!("{root}/__svelte/types.d.ts")]));
                    }
                }
            }
        }
        let mut requires_js = false;
        for component in &input.components {
            let full = relative(&root, &component.path)?;
            if uri(&full)? != component.document.uri {
                return Err("Document URI must use canonical path escaping".into());
            }
            if !full.ends_with(".svelte") || uri_path(&component.document.uri)? != full {
                return Err("Component path and original .svelte URI must match".into());
            }
            if files
                .get(&full)
                .is_some_and(|text| text != &component.document.text)
            {
                return Err("Component snapshot differs from supplied file".into());
            }
            if output.sources.contains_key(&component.document.uri) {
                return Err("Duplicate component URI".into());
            }
            files.insert(full.clone(), component.document.text.clone());
            let document = snapshot(&component.document, revision);
            if component.check {
                let response = compiler_response("compile", &document, &input, None);
                let diagnostics = component_result(response)?
                    .map_or_else(|errors| errors, |value| diagnostics_of(&value));
                output
                    .component_diagnostics
                    .insert(document.uri.clone(), diagnostics);
            }
            let inspected =
                component_result(compiler_response("componentInfo", &document, &input, None))?;
            let info = match inspected {
                Ok(info) => info,
                Err(errors) => {
                    output.unavailable(component, errors);
                    continue;
                }
            };
            validate_languages(&document, input.runes)?;
            let inferred = info["typescript"]
                .as_bool()
                .ok_or("Cannot infer script language")?;
            if component
                .typescript
                .is_some_and(|declared| declared != inferred)
            {
                return Err("Explicit script language disagrees with component syntax".into());
            }
            let projected = match component_result(compiler_response(
                "project",
                &document,
                &input,
                Some(inferred),
            ))? {
                Ok(projected) => projected,
                Err(errors) => {
                    output.unavailable(component, errors);
                    continue;
                }
            };
            requires_js |= !inferred;
            let mut code = projected["code"]
                .as_str()
                .ok_or("Projection has no code")?
                .to_owned();
            let mappings: Vec<Mapping> = projected["mappings"]
                .as_array()
                .ok_or("Missing exact projection mappings")?
                .iter()
                .map(|mapping| {
                    Ok(Mapping {
                        source: serde_json::from_value::<ByteRange>(mapping["source"].clone())
                            .map_err(|e| e.to_string())?,
                        generated: serde_json::from_value::<ByteRange>(
                            mapping["generated"].clone(),
                        )
                        .map_err(|e| e.to_string())?,
                    })
                })
                .collect::<Result<_, String>>()?;
            for directive in [
                "///<reference types=\"svelte\" />",
                "/// <reference types=\"svelte\" />",
            ] {
                if code.starts_with(directive)
                    && mappings
                        .iter()
                        .all(|m| m.generated.start >= directive.len())
                {
                    code.replace_range(..directive.len(), &masked(directive));
                }
            }
            let generated = format!("{full}.{}", if inferred { "tsx" } else { "jsx" });
            insert(&mut files, generated.clone(), code.clone())?;
            let basename = generated
                .rsplit('/')
                .next()
                .ok_or("Invalid generated path")?;
            let specifier =
                serde_json::to_string(&format!("./{basename}")).map_err(|e| e.to_string())?;
            let bridge =
                format!("export {{default}} from {specifier};\nexport * from {specifier};\n");
            insert(&mut files, format!("{full}.d.ts"), bridge.clone())?;
            insert(
                &mut files,
                format!(
                    "{}.d.svelte.ts",
                    full.strip_suffix(".svelte").ok_or("Invalid component")?
                ),
                bridge,
            )?;
            let generated_uri = uri(&generated)?;
            output
                .maps
                .register(
                    document.clone(),
                    generated_uri.clone(),
                    code.clone(),
                    mappings,
                )
                .map_err(|e| e.to_string())?;
            output.projections.push(json!({"fileName":generated,"required":component.check,"document":document,"code":code,"mappings":projected["mappings"]}));
            if component.check {
                checked.insert(generated.clone());
            }
            if output
                .sources
                .insert(
                    component.document.uri.clone(),
                    Source {
                        original: component.document.clone(),
                        generated_uri,
                        code,
                        language: if inferred {
                            "typescriptreact"
                        } else {
                            "javascriptreact"
                        },
                        component: true,
                        required: component.check,
                        semantic_available: true,
                    },
                )
                .is_some()
            {
                return Err("Duplicate component URI".into());
            }
        }
        for document in &input.documents {
            let full = uri_path(&document.uri)?;
            if uri(&full)? != document.uri {
                return Err("Document URI must use canonical path escaping".into());
            }
            if !full.starts_with(&format!("{root}/"))
                || full.starts_with(&format!("{root}/node_modules/"))
                || full.starts_with(&format!("{root}/__svelte/"))
            {
                return Err("Editor document is outside project source paths".into());
            }
            if full.ends_with(".svelte")
                || !(full.ends_with(".ts")
                    || full.ends_with(".js")
                    || full.ends_with(".tsx")
                    || full.ends_with(".jsx"))
            {
                return Err(
                    "Ordinary editor documents must be JS/TS; use components for Svelte".into(),
                );
            }
            if output.sources.contains_key(&document.uri) {
                return Err("Duplicate editor document".into());
            }
            if files.contains_key(&full) && !input.files.contains_key(&full) {
                return Err("Editor document collides with generated projection".into());
            }
            files.insert(full.clone(), document.text.clone());
            output
                .maps
                .register_identity(snapshot(document, revision), document.uri.clone())
                .map_err(|e| e.to_string())?;
            if full.ends_with(".svelte.js")
                || full.ends_with(".svelte.ts") && !full.ends_with(".d.svelte.ts")
            {
                match compiler("checkModule", &snapshot(document, revision), &input, Some(full.ends_with(".ts"))) {
                    Ok(result) => output.diagnostics.extend(result["diagnostics"].as_array().cloned().unwrap_or_default()),
                    Err(error) if error.contains("unsupported_module_typescript") => output.module_transpilation.push(json!({"document":snapshot(document,revision),"fileName":full,"source":document.text})),
                    Err(error) => return Err(error),
                }
            }
            output.sources.insert(
                document.uri.clone(),
                Source {
                    original: document.clone(),
                    generated_uri: document.uri.clone(),
                    code: document.text.clone(),
                    language: if document.uri.ends_with(".tsx") {
                        "typescriptreact"
                    } else if document.uri.ends_with(".jsx") {
                        "javascriptreact"
                    } else if document.uri.ends_with(".js") {
                        "javascript"
                    } else {
                        "typescript"
                    },
                    component: false,
                    required: true,
                    semantic_available: true,
                },
            );
        }
        for name in &input.root_files {
            let full = relative(&root, name)?;
            if !files.contains_key(&full) {
                return Err(format!("Missing root file {name}"));
            }
            if name.ends_with(".svelte") {
                return Err("Svelte roots must be declared as components".into());
            }
            checked.insert(full);
        }
        for name in input
            .files
            .keys()
            .filter(|name| !name.starts_with(&format!("{root}/node_modules/")))
        {
            let original_uri = uri(name)?;
            if name.ends_with(".svelte")
                && !output
                    .sources
                    .get(&original_uri)
                    .is_some_and(|s| s.component)
            {
                return Err(
                    "Every supplied .svelte source requires an explicit component snapshot".into(),
                );
            }
            if (name.ends_with(".svelte.js")
                || name.ends_with(".svelte.ts") && !name.ends_with(".d.svelte.ts"))
                && !output.sources.contains_key(&original_uri)
            {
                return Err(
                    "Rune modules require explicit editor snapshots for compiler diagnostics"
                        .into(),
                );
            }
        }
        if checked.is_empty() {
            return Err("Provide checked components or explicit rootFiles".into());
        }
        // Register ordinary supplied files too, so navigation and edits into
        // imported closed files retain their real URI and exact ranges.
        for (name, text) in &input.files {
            let original_uri = uri(name)?;
            if !output.sources.contains_key(&original_uri) {
                output
                    .maps
                    .register_identity(
                        Snapshot {
                            uri: original_uri.clone(),
                            version: revision,
                            text: text.clone(),
                        },
                        original_uri,
                    )
                    .map_err(|e| e.to_string())?;
            }
        }
        let mut options = input.compiler_options.clone().unwrap_or_else(|| json!({"strict":true,"target":"ESNext","module":"ESNext","moduleResolution":"Bundler","allowJs":true,"checkJs":true,"types":[]}).as_object().unwrap().clone());
        if requires_js
            && !options
                .get("allowJs")
                .and_then(Value::as_bool)
                .unwrap_or_else(|| {
                    options
                        .get("checkJs")
                        .and_then(Value::as_bool)
                        .unwrap_or(false)
                })
        {
            return Err(
                "JavaScript projections require allowJs in resolved compilerOptions".into(),
            );
        }
        if let Some(paths) = options.remove("paths") {
            for (name, targets) in paths.as_object().ok_or("paths must be an object")? {
                if declarations.insert(name.clone(), targets.clone()).is_some() {
                    return Err(format!(
                        "Path alias conflicts with Svelte declarations: {name}"
                    ));
                }
            }
        }
        options.insert("paths".into(), Value::Object(declarations));
        for (name, value) in [
            ("noEmit", json!(true)),
            ("incremental", json!(false)),
            ("composite", json!(false)),
            ("jsx", json!("preserve")),
            ("allowArbitraryExtensions", json!(true)),
            ("allowImportingTsExtensions", json!(true)),
        ] {
            options.insert(name.into(), value);
        }
        let mut roots = options
            .remove("typeRoots")
            .map(|value| {
                value
                    .as_array()
                    .cloned()
                    .ok_or("typeRoots must be an array")
            })
            .transpose()?
            .unwrap_or_else(|| vec![json!(format!("{root}/node_modules/@types"))]);
        roots.insert(0, json!(format!("{root}/__svelte/type-roots")));
        options.insert("typeRoots".into(), json!(roots));
        insert(
            &mut files,
            format!("{root}/tsconfig.json"),
            json!({"compilerOptions":options,"files":checked}).to_string(),
        )?;
        if files.len() > MAX_FILES
            || files.values().map(String::len).sum::<usize>() > MAX_BYTES
            || files.values().any(|text| text.len() > 8 * 1024 * 1024)
        {
            return Err("Generated project exceeds filesystem limits".into());
        }
        output.files = files;
        Ok(output)
    }

    pub fn prepared(&self) -> Value {
        json!({"revision":self.revision,"initialize":{"root":self.root,"project":format!("{}/tsconfig.json",self.root),"files":self.files},
            "open":self.sources.values().filter(|source| source.required && source.semantic_available).map(|source| self.open(source)).collect::<Vec<_>>(),"projections":self.projections,
            "diagnostics":self.all_compiler_diagnostics(),"moduleTranspilation":self.module_transpilation,
            "loadedFiles":self.loaded_files,"deferredComponents":self.sources.values().filter(|s| s.component && !s.required && !self.is_loaded(s)).map(|s| &s.original.uri).collect::<Vec<_>>(),
            "semanticUnavailable":self.unavailable_metadata(),"semanticComplete":self.semantic_complete(),
            "complete":self.module_transpilation.is_empty()})
    }

    /// Complete an explicitly requested native TypeScript transpilation before
    /// claiming module diagnostics complete. The caller supplies the result from
    /// semantic `transpileModule` for the exact pending source.
    pub fn complete_module(
        &mut self,
        original_uri: &str,
        transpiled: &Value,
    ) -> Result<Value, String> {
        let pending = self
            .module_transpilation
            .iter()
            .position(|p| p["document"]["uri"] == original_uri)
            .ok_or("No pending module transpilation")?;
        let source = self
            .sources
            .get(original_uri)
            .ok_or("Missing rune source")?;
        if transpiled["ok"] != true {
            return Err("Native module transpilation failed".into());
        }
        let code = transpiled["code"]
            .as_str()
            .ok_or("Missing transpiled JavaScript")?;
        let map = transpiled["map"]
            .as_str()
            .ok_or("Missing transpilation source map")?;
        let parsed: Value = serde_json::from_str(map).map_err(|e| e.to_string())?;
        if parsed["sourcesContent"] != json!([source.original.text]) {
            return Err("Transpilation map does not contain the pending original source".into());
        }
        let document = snapshot(&source.original, self.revision);
        let response = ts_svelte_compiler::dispatch(
            json!({"op":"checkModule","typescript":false,"compatibility":self.versions,
            "document":{"uri":original_uri,"version":self.revision,"text":code}})
            .to_string()
            .as_bytes(),
        );
        let body = if response["ok"] == true {
            &response["result"]
        } else {
            &response
        };
        let diagnostics = body["diagnostics"].as_array().cloned().unwrap_or_default();
        if response["ok"] != true && diagnostics.is_empty() {
            return Err(response.to_string());
        }
        let mapped = diagnostics
            .into_iter()
            .map(|d| ts_svelte_mapping::map_diagnostic(&document, code, map, d))
            .collect::<Result<Vec<_>, _>>()?;
        self.diagnostics.extend(mapped.clone());
        self.module_transpilation.remove(pending);
        Ok(json!({"diagnostics":mapped,"complete":self.module_transpilation.is_empty()}))
    }
    fn open(&self, source: &Source) -> Value {
        json!({"op":"open","params":{"textDocument":{"uri":source.generated_uri,"version":self.revision,"languageId":source.language,"text":source.code}}})
    }

    /// Build a replacement without mutating this snapshot. Apply the returned
    /// ordered requests with semantic queries paused; install the candidate only
    /// after all acknowledgements succeed. On partial failure rebuild the worker.
    pub fn replacement(&self, input: Value) -> Result<(Self, Value), String> {
        let revision = self
            .revision
            .checked_add(1)
            // Tickets combine revision and counter in an exact JS integer.
            .filter(|v| *v <= 0x001f_ffff)
            .ok_or("Project revision exhausted")?;
        let candidate = Self::build(input, revision)?;
        if candidate.root != self.root {
            return Err("Replacement cannot change project root".into());
        }
        for (uri, source) in &candidate.sources {
            if let Some(previous) = self.sources.get(uri) {
                if source.original.version < previous.original.version
                    || source.original.text != previous.original.text
                        && source.original.version == previous.original.version
                {
                    return Err(
                        "Changed snapshots require increasing signed editor versions".into(),
                    );
                }
            }
        }
        let mut files = Map::new();
        for (name, text) in &candidate.files {
            if self.files.get(name) != Some(text) {
                files.insert(name.clone(), json!(text));
            }
        }
        for name in self.files.keys() {
            if !candidate.files.contains_key(name) {
                files.insert(name.clone(), Value::Null);
            }
        }
        let mut requests = vec![json!({"op":"files","params":{"files":files}})];
        for (uri, source) in &self.sources {
            if !source.required || !source.semantic_available {
                continue;
            }
            if candidate.sources.get(uri).is_none_or(|next| {
                !next.required
                    || !next.semantic_available
                    || next.generated_uri != source.generated_uri
            }) {
                requests.push(
                    json!({"op":"close","params":{"textDocument":{"uri":source.generated_uri}}}),
                );
            }
        }
        for (uri, source) in &candidate.sources {
            if !source.required || !source.semantic_available {
                continue;
            }
            if self.sources.get(uri).is_some_and(|old| {
                old.required && old.semantic_available && old.generated_uri == source.generated_uri
            }) {
                requests.push(json!({"op":"change","params":{"textDocument":{"uri":source.generated_uri,"version":revision},"contentChanges":[{"text":source.code}]}}));
            } else {
                requests.push(candidate.open(source));
            }
        }
        Ok((
            candidate,
            json!({"previousRevision":self.revision,"revision":revision,"requests":requests}),
        ))
    }

    fn token(&mut self) -> Result<u64, String> {
        self.next_token = self.next_token.checked_add(1).ok_or("Token exhausted")?;
        if self.next_token >> 32 != u64::from(self.revision) {
            return Err("Revision token space exhausted".into());
        }
        Ok(self.next_token)
    }

    pub fn query(&mut self, method: &str, params: &Value) -> Result<Value, String> {
        if self.tickets.len() >= 10_000 {
            return Err("Too many outstanding semantic requests".into());
        }
        if method == "workspace/diagnostic" {
            let generated = if let Some(source) = self
                .sources
                .values()
                .find(|source| source.required && source.semantic_available)
            {
                source.generated_uri.clone()
            } else {
                let config: Value = serde_json::from_str(
                    self.files
                        .get(&format!("{}/tsconfig.json", self.root))
                        .ok_or("Missing generated configuration")?,
                )
                .map_err(|e| e.to_string())?;
                uri(config["files"][0].as_str().ok_or("Empty checked project")?)?
            };
            let ticket = self.token()?;
            self.tickets.insert(
                ticket,
                Ticket {
                    op: "projectDiagnostics".into(),
                    original: String::new(),
                    generated: generated.clone(),
                    syntax: None,
                },
            );
            return Ok(
                json!({"ticket":ticket,"revision":self.revision,"op":"projectDiagnostics","params":{"uri":generated,"projections":self.projections}}),
            );
        }
        if method == "completionItem/resolve" {
            let key = params["data"]["wasmProjectCompletion"]
                .as_u64()
                .ok_or("Missing completion token")?;
            let (generated, raw) = self
                .completions
                .get(&key)
                .cloned()
                .ok_or("Stale completion token")?;
            let original = self
                .sources
                .iter()
                .find(|(_, source)| source.generated_uri == generated)
                .map(|(uri, _)| uri.clone())
                .ok_or("Missing completion source")?;
            let ticket = self.token()?;
            self.tickets.insert(
                ticket,
                Ticket {
                    op: "completionResolve".into(),
                    original,
                    generated,
                    syntax: None,
                },
            );
            return Ok(
                json!({"ticket":ticket,"revision":self.revision,"op":"completionResolve","params":raw}),
            );
        }
        let original = params["textDocument"]["uri"]
            .as_str()
            .ok_or("Missing original document URI")?;
        let source = self
            .sources
            .get(original)
            .ok_or("Document is not an editor snapshot")?;
        if !source.semantic_available {
            if method == "textDocument/diagnostic" {
                return Ok(
                    json!({"revision":self.revision,"local":self.compiler_report(original, false)}),
                );
            }
            return Err(format!(
                "Semantic projection is unavailable for {original}; request compiler diagnostics and repair the source"
            ));
        }
        if !source.required && !self.is_loaded(source) {
            return Err(format!(
                "Auxiliary component is not acknowledged as loaded: {original}; request workspace/diagnostic after project synchronization"
            ));
        }
        let generated = source.generated_uri.clone();
        let op = operation(method).ok_or("Unsupported semantic method")?;
        let syntax = if source.component {
            crate::syntax_editor::query(
                snapshot(&source.original, self.revision),
                &ts_svelte::compatibility::resolve(&self.versions)?,
                method,
                params,
            )?
        } else {
            None
        };
        if op == "linkedEditingRange" && syntax.is_some() {
            return Ok(json!({"revision":self.revision,"local":syntax}));
        }
        if op == "selectionRange"
            && syntax.is_some()
            && params["positions"].as_array().is_some_and(|positions| {
                positions.iter().any(|position| {
                    self.request_position(original, &generated, position)
                        .is_err()
                })
            })
        {
            return Ok(json!({"revision":self.revision,"local":syntax}));
        }
        let mut request = params.clone();
        request["textDocument"]["uri"] = json!(generated);
        if let Some(position) = params.get("position") {
            request["position"] = self.request_position(original, &generated, position)?;
        }
        if let Some(range) = params.get("range") {
            request["range"] = self.request_range(original, &generated, range)?;
        }
        if let Some(positions) = params.get("positions") {
            request["positions"] = json!(
                positions
                    .as_array()
                    .ok_or("positions must be an array")?
                    .iter()
                    .map(|position| self.request_position(original, &generated, position))
                    .collect::<Result<Vec<_>, _>>()?
            );
        }
        if op == "codeAction" {
            if let Some(diagnostics) = request["context"]["diagnostics"].as_array_mut() {
                for diagnostic in diagnostics {
                    diagnostic["range"] =
                        self.request_range(original, &generated, &diagnostic["range"])?;
                    // Client diagnostic payloads are context, not trusted edits.
                    if let Some(object) = diagnostic.as_object_mut() {
                        object.remove("data");
                    }
                }
            }
        }
        let ticket = self.token()?;
        self.tickets.insert(
            ticket,
            Ticket {
                op: op.into(),
                original: original.into(),
                generated,
                syntax,
            },
        );
        Ok(json!({"ticket":ticket,"revision":self.revision,"op":op,"params":request}))
    }
    fn request_position(
        &self,
        original: &str,
        generated: &str,
        value: &Value,
    ) -> Result<Value, String> {
        let position: Position =
            serde_json::from_value(value.clone()).map_err(|e| e.to_string())?;
        self.maps
            .generated_positions(original, position)
            .map_err(|e| e.to_string())?
            .into_iter()
            .find(|p| p.uri == generated)
            .map(|p| json!(p.position))
            .ok_or("Position has no exact checking projection".into())
    }
    fn request_range(
        &self,
        original: &str,
        generated: &str,
        value: &Value,
    ) -> Result<Value, String> {
        let range: Range = serde_json::from_value(value.clone()).map_err(|e| e.to_string())?;
        self.maps
            .generated_ranges(original, range)
            .map_err(|e| e.to_string())?
            .into_iter()
            .find(|r| r.uri == generated)
            .map(|r| json!(r.range))
            .ok_or("Range has no contiguous exact checking projection".into())
    }

    /// Map a raw semantic result, not its outer worker envelope. A ticket is
    /// consumed even when mapping fails; replay or client-edited plans cannot
    /// redirect mappings to a different snapshot.
    pub fn respond(&mut self, ticket: u64, mut response: Value) -> Result<Value, String> {
        let ticket = self
            .tickets
            .remove(&ticket)
            .ok_or("Unknown or consumed semantic ticket")?;
        normalize_semantic_uris(&mut response)?;
        if ticket.op == "projectDiagnostics" {
            return self.project_diagnostics(response);
        }
        let raw = response.clone();
        let mut mapped = match ticket.op.as_str() {
            "hover" => self.maps.map_hover(&ticket.generated, response),
            "completion" | "completionResolve" => {
                self.maps.map_completions(&ticket.generated, response)
            }
            "rename" => self.maps.map_workspace_edit(response),
            "prepareRename" => self.maps.map_prepare_rename(&ticket.generated, response),
            "signatureHelp" => self.maps.map_signature_help(response),
            "documentHighlight" => self.maps.map_highlights(&ticket.generated, response),
            "documentSymbols" => self.maps.map_document_symbols(&ticket.generated, response),
            "selectionRange" => self.maps.map_selection_ranges(&ticket.generated, response),
            "foldingRange" => self.maps.map_folding_ranges(&ticket.generated, response),
            "semanticTokens" | "semanticTokensRange" => {
                self.maps.map_semantic_tokens(&ticket.generated, response)
            }
            "codeAction" => self.maps.map_code_actions(&ticket.generated, response),
            "inlayHint" => self.maps.map_inlay_hints(&ticket.generated, response),
            "linkedEditingRange" => self
                .maps
                .map_linked_editing_ranges(&ticket.generated, response),
            "diagnostics" => {
                let mut report = self.diagnostics(&ticket.generated, response)?;
                let compiler = self.compiler_report(&ticket.original, true);
                report["items"]
                    .as_array_mut()
                    .ok_or("Missing diagnostic items")?
                    .extend(compiler["items"].as_array().cloned().unwrap_or_default());
                report["unmappedCompilerDiagnostics"] =
                    compiler["unmappedCompilerDiagnostics"].clone();
                report["semanticComplete"] = json!(true);
                return Ok(report);
            }
            _ => self.maps.map_locations(&ticket.generated, response),
        }
        .map_err(|e| e.to_string())?;
        self.signed_edits(&mut mapped)?;
        if ticket.op == "completion" {
            let raws = if let Some(items) = raw.as_array() {
                items.clone()
            } else {
                raw["items"].as_array().cloned().unwrap_or_default()
            };
            if raws.len() > 10_000 {
                return Err("Completion response exceeds 10000 items".into());
            }
            if self.completions.len() + raws.len() > 10_000 {
                self.completions.clear();
            }
            let items = if mapped.is_array() {
                mapped.as_array_mut()
            } else {
                mapped["items"].as_array_mut()
            };
            if let Some(items) = items {
                if items.len() != raws.len() {
                    return Err("Completion mapping changed item cardinality".into());
                }
                for (item, raw) in items.iter_mut().zip(raws) {
                    let key = self.token()?;
                    self.completions
                        .insert(key, (ticket.generated.clone(), raw));
                    item["data"] = json!({"wasmProjectCompletion":key});
                }
            }
        } else if ticket.op == "completionResolve" {
            if self.completions.len() >= 10_000 {
                self.completions.clear();
            }
            let key = self.token()?;
            self.completions
                .insert(key, (ticket.generated.clone(), raw));
            mapped["data"] = json!({"wasmProjectCompletion":key});
        }
        if matches!(ticket.op.as_str(), "foldingRange" | "documentSymbols") {
            if let Some(syntax) = ticket.syntax {
                if mapped.is_null() {
                    mapped = json!([]);
                }
                if let (Some(items), Some(additions)) = (mapped.as_array_mut(), syntax.as_array()) {
                    for item in additions {
                        if !items.contains(item) {
                            items.push(item.clone());
                        }
                    }
                }
            }
        }
        let _ = ticket.original;
        Ok(mapped)
    }

    fn signed_edits(&self, value: &mut Value) -> Result<(), String> {
        match value {
            Value::Array(values) => {
                for value in values {
                    self.signed_edits(value)?;
                }
            }
            Value::Object(object) => {
                if let Some(changes) = object.remove("changes") {
                    let changes = changes
                        .as_object()
                        .ok_or("Workspace changes must be an object")?;
                    let mut edits = Vec::new();
                    for (original, items) in changes {
                        edits.push(json!({"textDocument":{"uri":original,"version":self.sources.get(original).map(|s|s.original.version)},"edits":items}));
                    }
                    object.insert("documentChanges".into(), json!(edits));
                }
                if let Some(changes) = object
                    .get_mut("documentChanges")
                    .and_then(Value::as_array_mut)
                {
                    for change in changes {
                        let original = change["textDocument"]["uri"]
                            .as_str()
                            .ok_or("Missing mapped edit URI")?;
                        let version = self.sources.get(original).map(|s| s.original.version);
                        change["textDocument"]["version"] = json!(version);
                    }
                }
                for value in object.values_mut() {
                    self.signed_edits(value)?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn project_diagnostics(&mut self, mut response: Value) -> Result<Value, String> {
        self.acknowledge_loaded(&response)?;
        let reports = response["documents"]
            .as_array()
            .ok_or("Missing semantic document reports")?;
        let mut documents = Vec::new();
        let mut generated_diagnostics = Vec::new();
        for report in reports {
            let generated = report["uri"]
                .as_str()
                .ok_or("Missing diagnostic report URI")?;
            let original = self
                .sources
                .iter()
                .find(|(_, source)| source.generated_uri == generated)
                .map(|(uri, _)| uri.clone());
            let report_items = report["diagnostics"]
                .as_array()
                .ok_or("Missing diagnostic list")?;
            let known = self
                .files
                .contains_key(&uri_path(generated).unwrap_or_default())
                && !generated.contains("/__svelte/")
                && !generated.ends_with(".svelte.d.ts")
                && !generated.ends_with(".d.svelte.ts");
            if original.is_some() || known {
                // Bundled TypeScript libraries are external. Supplied ordinary
                // files and projections have registered identity/exact mappings.
                match self.diagnostics(generated,json!({"kind":"full","items":report_items})) {
                    Ok(mapped)=>documents.push(json!({"uri":original.unwrap_or_else(||generated.into()),"version":self.sources.values().find(|s|s.generated_uri==generated).map(|s|s.original.version),"report":mapped})),
                    Err(error)=>return Err(format!("Cannot map {generated}: {error}")),
                }
            } else if !report_items.is_empty() {
                generated_diagnostics.push(json!({"status":"unmapped","generatedUri":generated,"diagnostics":report_items}));
            }
        }
        for source in self
            .sources
            .values()
            .filter(|source| source.component && !source.semantic_available && source.required)
        {
            documents.push(json!({"uri":source.original.uri,"version":source.original.version,"report":self.compiler_report(&source.original.uri, false)}));
        }
        fn versions(value: &mut Value, sources: &BTreeMap<String, Source>) {
            if let Some(uri) = value["original"]["uri"].as_str() {
                if let Some(source) = sources.get(uri) {
                    value["original"]["version"] = json!(source.original.version);
                }
            }
            match value {
                Value::Array(items) => {
                    for item in items {
                        versions(item, sources);
                    }
                }
                Value::Object(items) => {
                    for item in items.values_mut() {
                        versions(item, sources);
                    }
                }
                _ => {}
            }
        }
        versions(&mut response, &self.sources);
        Ok(
            json!({"revision":self.revision,"documents":documents,"generatedDiagnostics":generated_diagnostics,
            "compilerDiagnostics":self.all_compiler_diagnostics(),"checker":response,"complete":self.module_transpilation.is_empty(),
            "semanticUnavailable":self.unavailable_metadata(),"semanticComplete":self.semantic_complete()}),
        )
    }

    pub fn diagnostics(&self, generated: &str, mut response: Value) -> Result<Value, String> {
        if response["kind"] == "unchanged" {
            return Err(
                "Diagnostic result IDs belong to generated snapshots; request a full report".into(),
            );
        }
        let items = response["items"]
            .as_array()
            .ok_or("Expected full diagnostic items")?;
        let mut mapped = Vec::new();
        let mut unmapped = Vec::new();
        for item in items {
            let range: Range =
                serde_json::from_value(item["range"].clone()).map_err(|e| e.to_string())?;
            match self
                .maps
                .map_range(generated, range)
                .map_err(|e| e.to_string())?
            {
                Some(location) => {
                    let mut item = item.clone();
                    item["range"] = json!(location.range);
                    if let Some(related) = item["relatedInformation"].as_array_mut() {
                        for entry in related {
                            entry["location"] = self
                                .maps
                                .map_locations(generated, entry["location"].clone())
                                .map_err(|e| e.to_string())?;
                        }
                    }
                    mapped.push(item);
                }
                None => unmapped
                    .push(json!({"status":"unmapped","generatedUri":generated,"diagnostic":item})),
            }
        }
        if response.get("relatedDocuments").is_some() {
            return Err("Related diagnostic reports require explicit per-document mapping".into());
        }
        response["items"] = json!(mapped);
        response["unmappedDiagnostics"] = json!(unmapped);
        if let Some(object) = response.as_object_mut() {
            object.remove("resultId");
        }
        Ok(response)
    }
}

fn operation(method: &str) -> Option<&'static str> {
    Some(match method {
        "textDocument/hover" => "hover",
        "textDocument/completion" => "completion",
        "textDocument/definition" => "definition",
        "textDocument/typeDefinition" => "typeDefinition",
        "textDocument/references" => "references",
        "textDocument/prepareRename" => "prepareRename",
        "textDocument/rename" => "rename",
        "textDocument/signatureHelp" => "signatureHelp",
        "textDocument/documentHighlight" => "documentHighlight",
        "textDocument/documentSymbol" => "documentSymbols",
        "textDocument/selectionRange" => "selectionRange",
        "textDocument/foldingRange" => "foldingRange",
        "textDocument/semanticTokens/full" => "semanticTokens",
        "textDocument/semanticTokens/range" => "semanticTokensRange",
        "textDocument/codeAction" => "codeAction",
        "textDocument/inlayHint" => "inlayHint",
        "textDocument/linkedEditingRange" => "linkedEditingRange",
        "textDocument/diagnostic" => "diagnostics",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ts_svelte::Document;

    fn ordinary(text: &str, version: i32) -> Value {
        json!({"root":"/project","files":{"/project/main.ts":text},"rootFiles":["main.ts"],
            "documents":[{"uri":"file:///project/main.ts","version":version,"text":text}]})
    }
    #[test]
    fn bundled_libraries_are_acknowledged_without_becoming_project_inputs() {
        let mut project = WasmProject::new(ordinary("let value = 1;", 1)).unwrap();
        let library = "bundled:///libs/lib.es2026.d.ts";
        let response = json!({"projectionsVerified":true,
            "loadedFiles":["/project/main.ts",library],
            "documents":[{"uri":"file:///project/main.ts","diagnostics":[]},
                {"uri":library,"diagnostics":[]}],"diagnostics":[]});
        project.acknowledge_loaded(&response).unwrap();
        assert!(project.loaded_files.as_ref().unwrap().contains(library));
        assert!(!project.files.contains_key(library));
        for invalid in [
            "bundled:///libs/../app.ts",
            "bundled:///libs/lib.es5.d.ts?x",
            "bundled:///other/lib.es5.d.ts",
        ] {
            assert!(loaded_identity(invalid).is_err());
        }
        let mut missing = response;
        missing["loadedFiles"] = json!(["/project/main.ts"]);
        assert!(project.acknowledge_loaded(&missing).is_err());
    }
    #[test]
    fn checker_uri_escaping_maps_to_registered_file_identities() {
        let mut response = json!({"uri":"file:///project/node_modules/%40types/estree/index.d.ts",
            "changes":{"file:///project/%2Bpage.ts":[]},
            "links":[{"targetUri":"file:///project/%2Bpage.ts"}]});
        normalize_semantic_uris(&mut response).unwrap();
        assert_eq!(
            response["uri"],
            "file:///project/node_modules/@types/estree/index.d.ts"
        );
        assert!(
            response["changes"]
                .get("file:///project/+page.ts")
                .is_some()
        );
        assert_eq!(
            response["links"][0]["targetUri"],
            "file:///project/+page.ts"
        );
    }
    fn component(text: &str) -> Value {
        // These declarations are inputs to projection/mapping unit tests, not a
        // semantic-check fixture or a substitute for installed package types.
        json!({"root":"/project","files":{
            "/project/node_modules/svelte/package.json":"{\"version\":\"5.53.11\"}",
            "/project/node_modules/svelte/types/index.d.ts":"declare module 'svelte' {}\ndeclare module '*.svelte' {\n}\n",
            "/project/node_modules/svelte/svelte-html.d.ts":"",
            "/project/node_modules/svelte-check/dist/src/svelte-shims-v4.d.ts":""
        },"components":[{"path":"App.svelte","document":{"uri":"file:///project/App.svelte","version":-7,"text":text}}]})
    }
    fn cursor(uri: &str, character: u32) -> Value {
        json!({"textDocument":{"uri":uri},"position":{"line":0,"character":character}})
    }
    fn range(start: u32, end: u32) -> Value {
        json!({"start":{"line":0,"character":start},"end":{"line":0,"character":end}})
    }

    #[test]
    fn identity_edits_restore_signed_versions_and_tickets_are_single_use() {
        let mut project = WasmProject::new(ordinary("let value = 1;", -7)).unwrap();
        let prepared = project.prepared();
        assert_eq!(prepared["open"][0]["params"]["textDocument"]["version"], 1);
        let query = project
            .query("textDocument/rename", &cursor("file:///project/main.ts", 5))
            .unwrap();
        let ticket = query["ticket"].as_u64().unwrap();
        let mapped = project.respond(ticket,json!({"changes":{"file:///project/main.ts":[{"range":range(4,9),"newText":"renamed"}]}})).unwrap();
        assert_eq!(mapped["documentChanges"][0]["textDocument"]["version"], -7);
        assert_eq!(
            mapped["documentChanges"][0]["edits"][0]["range"],
            range(4, 9)
        );
        assert!(project.respond(ticket, Value::Null).is_err());
    }

    #[test]
    fn replacement_keeps_old_snapshot_until_acknowledged_and_rejects_stale_sources() {
        let mut old = WasmProject::new(ordinary("let value = 1;", -7)).unwrap();
        let query = old
            .query("textDocument/hover", &cursor("file:///project/main.ts", 5))
            .unwrap();
        let (mut next, plan) = old.replacement(ordinary("let value = 2;", -6)).unwrap();
        assert_eq!(plan["requests"][1]["op"], "change");
        assert_eq!(plan["requests"][1]["params"]["textDocument"]["version"], 2);
        assert_eq!(
            old.prepared()["initialize"]["files"]["/project/main.ts"],
            "let value = 1;"
        );
        assert!(
            next.respond(query["ticket"].as_u64().unwrap(), Value::Null)
                .is_err()
        );
        assert!(old.replacement(ordinary("let value = 3;", -7)).is_err());
        assert!(old.replacement(ordinary("let value = 1;", -8)).is_err());
        assert!(
            old.respond(
                query["ticket"].as_u64().unwrap(),
                json!({"contents":"old","range":range(4,9)})
            )
            .is_ok()
        );
    }

    #[test]
    fn svelte_projection_bridges_and_exact_unicode_ranges() {
        let text = "<script lang=\"ts\">let value: number = 1;</script>\n<p>😀 {value}</p>";
        let mut project = WasmProject::new(component(text)).unwrap();
        let prepared = project.prepared();
        assert!(
            prepared["initialize"]["files"]["/project/App.svelte.tsx"]
                .as_str()
                .unwrap()
                .contains("value")
        );
        assert!(
            prepared["initialize"]["files"]["/project/App.svelte.d.ts"]
                .as_str()
                .unwrap()
                .contains("./App.svelte.tsx")
        );
        let document = Document::new(Snapshot {
            uri: "file:///project/App.svelte".into(),
            version: 0,
            text: text.into(),
        });
        let offset = text.find("value").unwrap();
        let original = document
            .editor_range(ByteRange {
                start: offset,
                end: offset + 5,
            })
            .unwrap();
        let query=project.query("textDocument/hover",&json!({"textDocument":{"uri":"file:///project/App.svelte"},"position":original.start})).unwrap();
        let mut generated: Position =
            serde_json::from_value(query["params"]["position"].clone()).unwrap();
        let start = generated;
        generated.character += 5;
        let mapped = project
            .respond(
                query["ticket"].as_u64().unwrap(),
                json!({"contents":"number","range":{"start":start,"end":generated}}),
            )
            .unwrap();
        assert_eq!(mapped["range"], json!(original));
        let diagnostics = project
            .diagnostics(
                "file:///project/App.svelte.tsx",
                json!({"kind":"full","items":[
                    {"range":{"start":start,"end":generated},"message":"copied","code":2322},
                    {"range":range(0,1),"message":"helper","code":999}
                ]}),
            )
            .unwrap();
        assert_eq!(diagnostics["items"].as_array().unwrap().len(), 1);
        assert_eq!(
            diagnostics["unmappedDiagnostics"].as_array().unwrap().len(),
            1
        );
    }

    #[test]
    fn helpers_never_become_workspace_edits_and_stale_generated_versions_fail() {
        let mut project = WasmProject::new(component("<p>Hello</p>")).unwrap();
        let request=project.query("textDocument/codeAction",&json!({"textDocument":{"uri":"file:///project/App.svelte"},"context":{"diagnostics":[]}})).unwrap();
        assert!(project.respond(request["ticket"].as_u64().unwrap(),json!([{"title":"unsafe","edit":{"changes":{"file:///project/App.svelte.jsx":[{"range":range(0,1),"newText":"bad"}]}}}])).is_err());
        let mut project = WasmProject::new(ordinary("let value = 1;", -7)).unwrap();
        let request = project
            .query("textDocument/rename", &cursor("file:///project/main.ts", 5))
            .unwrap();
        assert!(project.respond(request["ticket"].as_u64().unwrap(),json!({"documentChanges":[{"textDocument":{"uri":"file:///project/main.ts","version":2},"edits":[{"range":range(4,9),"newText":"bad"}]}]})).is_err());
    }

    #[test]
    fn malformed_component_preserves_diagnostics_other_documents_and_repair() {
        let mut input = component("{#if true}<p>unfinished");
        input["files"]["/project/main.ts"] = json!("let value = 1;");
        input["rootFiles"] = json!(["main.ts"]);
        input["documents"] =
            json!([{"uri":"file:///project/main.ts","version":-2,"text":"let value = 1;"}]);
        let mut project = WasmProject::new(input.clone()).unwrap();
        let prepared = project.prepared();
        assert_eq!(prepared["semanticComplete"], false);
        assert_eq!(prepared["semanticUnavailable"][0]["version"], -7);
        assert!(
            prepared["diagnostics"]
                .as_array()
                .unwrap()
                .iter()
                .any(|d| d["severity"] == "error")
        );
        assert_eq!(prepared["open"].as_array().unwrap().len(), 1);
        assert!(
            prepared["initialize"]["files"]
                .get("/project/App.svelte.jsx")
                .is_none()
        );
        assert!(
            prepared["initialize"]["files"]
                .get("/project/App.svelte.d.ts")
                .is_none()
        );
        let report = project
            .query(
                "textDocument/diagnostic",
                &json!({"textDocument":{"uri":"file:///project/App.svelte"}}),
            )
            .unwrap();
        assert_eq!(report["local"]["semanticComplete"], false);
        assert!(!report["local"]["items"].as_array().unwrap().is_empty());
        assert!(
            project
                .query(
                    "textDocument/hover",
                    &cursor("file:///project/App.svelte", 1)
                )
                .is_err()
        );
        let request = project
            .query("textDocument/hover", &cursor("file:///project/main.ts", 5))
            .unwrap();
        let hover = project
            .respond(
                request["ticket"].as_u64().unwrap(),
                json!({"contents":"number","range":range(4,9)}),
            )
            .unwrap();
        assert_eq!(hover["range"], range(4, 9));
        input["components"][0]["document"]["text"] = json!("<p>repaired</p>");
        input["components"][0]["document"]["version"] = json!(-6);
        let (repaired, plan) = project.replacement(input).unwrap();
        assert_eq!(repaired.prepared()["semanticComplete"], true);
        assert!(
            repaired.prepared()["semanticUnavailable"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        assert!(
            plan["requests"]
                .as_array()
                .unwrap()
                .iter()
                .any(|request| request["op"] == "open"
                    && request["params"]["textDocument"]["uri"]
                        == "file:///project/App.svelte.jsx")
        );
        assert_eq!(project.prepared()["semanticComplete"], false);
    }

    #[test]
    fn auxiliary_compiler_checks_follow_verified_loaded_graph_without_opening() {
        let mut input = component("<p>root</p>");
        input["components"].as_array_mut().unwrap().push(json!({"path":"Child.svelte","check":false,
            "document":{"uri":"file:///project/Child.svelte","version":-4,"text":"<img src=\"image.png\">"}}));
        let mut project = WasmProject::new(input.clone()).unwrap();
        let prepared = project.prepared();
        assert_eq!(prepared["open"].as_array().unwrap().len(), 1);
        assert_eq!(
            prepared["deferredComponents"],
            json!(["file:///project/Child.svelte"])
        );
        assert!(
            !project
                .component_diagnostics
                .contains_key("file:///project/Child.svelte")
        );
        assert!(
            project
                .query(
                    "textDocument/diagnostic",
                    &json!({"textDocument":{"uri":"file:///project/Child.svelte"}})
                )
                .is_err()
        );
        let acknowledge = |project: &mut WasmProject, loaded: Value| {
            let request = project.query("workspace/diagnostic", &json!({})).unwrap();
            project
                .respond(
                    request["ticket"].as_u64().unwrap(),
                    json!({"documents":[],"diagnostics":[],
                "projectDiagnostics":[],"loadedFiles":loaded,"projectionsVerified":true}),
                )
                .unwrap()
        };
        let loaded = json!(["/project/App.svelte.jsx", "/project/Child.svelte.jsx"]);
        let report = acknowledge(&mut project, loaded.clone());
        assert!(
            project
                .component_diagnostics
                .contains_key("file:///project/Child.svelte")
        );
        assert!(
            report["compilerDiagnostics"]
                .as_array()
                .unwrap()
                .iter()
                .any(|d| d["uri"] == "file:///project/Child.svelte"
                    && d["severity"] == "warning"
                    && d["version"] == -4)
        );
        assert!(
            project.prepared()["deferredComponents"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        assert_eq!(project.prepared()["open"].as_array().unwrap().len(), 1);
        let request = project
            .query(
                "textDocument/diagnostic",
                &json!({"textDocument":{"uri":"file:///project/Child.svelte"}}),
            )
            .unwrap();
        let report = project
            .respond(
                request["ticket"].as_u64().unwrap(),
                json!({"kind":"full","items":[]}),
            )
            .unwrap();
        assert!(!report["items"].as_array().unwrap().is_empty());
        let report = acknowledge(&mut project, json!(["/project/App.svelte.jsx"]));
        assert!(
            !report["compilerDiagnostics"]
                .as_array()
                .unwrap()
                .iter()
                .any(|d| d["uri"] == "file:///project/Child.svelte")
        );
        assert!(
            project
                .query(
                    "textDocument/diagnostic",
                    &json!({"textDocument":{"uri":"file:///project/Child.svelte"}})
                )
                .is_err()
        );
        let (next, plan) = project.replacement(input).unwrap();
        assert!(next.prepared()["loadedFiles"].is_null());
        assert!(
            !plan["requests"]
                .as_array()
                .unwrap()
                .iter()
                .any(|request| request["params"]["textDocument"]["uri"]
                    == "file:///project/Child.svelte.jsx")
        );
    }

    #[test]
    fn loaded_acknowledgement_requires_verified_current_required_projections() {
        let mut project = WasmProject::new(component("<p>hello</p>")).unwrap();
        assert!(
            project
                .acknowledge_loaded(&json!({"loadedFiles":[],"documents":[]}))
                .is_err()
        );
        assert!(
            project
                .acknowledge_loaded(
                    &json!({"loadedFiles":[],"documents":[],"projectionsVerified":true})
                )
                .is_err()
        );
        assert!(project.prepared()["loadedFiles"].is_null());
        assert!(component_result(json!({"ok":false,"error":{"code":"runtime_compatibility"},"diagnostics":[{"severity":"warning"}]})).is_err());
    }

    #[test]
    fn configuration_source_identity_and_preprocessing_are_explicit() {
        let mut input = ordinary("export {};", 0);
        input["documents"][0]["uri"] = json!("file:///elsewhere/main.ts");
        assert!(WasmProject::new(input).is_err());
        let mut input = component("<p>Hello</p>");
        input["compilerOptions"] = json!({"strict":true,"allowJs":false});
        assert!(WasmProject::new(input).is_err());
        let mut input = component("<p>Hello</p>");
        input["preprocess"] = json!({"mdsvex":true});
        assert!(WasmProject::new(input).is_err());
        assert!(
            WasmProject::new(component(
                "<style lang=\"scss\">p { color: red; }</style><p>Hello</p>"
            ))
            .is_err()
        );
        let mut input = ordinary("export {};", 0);
        input["files"]["/project/tsconfig.json"] = json!("{}");
        assert!(WasmProject::new(input).is_err());
    }
}
