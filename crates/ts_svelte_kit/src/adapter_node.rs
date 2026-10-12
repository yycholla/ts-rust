//! Pure adapter-node 5.5.4 packaging. Runtime templates are application JS;
//! this module never executes Node, reads an environment, or touches a disk.
//!
//! The host supplies compiled Kit artifacts and the adapter's installed runtime
//! templates. `prepare` materializes assets/runtime/staging inputs; `materialize`
//! accepts completed native rebundling and compression before exposing a final
//! deployment tree. The host must write into a fresh directory without following
//! symlinks, then publish it atomically. No destructive cleanup is requested.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

pub const ADAPTER_VERSION: &str = "5.5.4";
pub type Files = BTreeMap<String, Vec<u8>>;

const TEMPLATES: [(&str, &str); 4] = [
    (
        "env.js",
        "9d9eeae9a4e14f8bd6596ff58d4ba70726eba235b91990bf705afa9dfd23f198",
    ),
    (
        "handler.js",
        "da524dca7c4089ad7ef151c779f97e03985fbc55cdf2b4881bfdc350da6c906f",
    ),
    (
        "index.js",
        "5977a04d4ccc020290aa19ef61a995e28a2dac807faa778535104be1edfa5b51",
    ),
    (
        "shims.js",
        "6b93e2a13908c8140a03e62687f71911865aa0d947cd46914f0bdc9e7b7498eb",
    ),
];
const COMPRESS_EXTENSIONS: [&str; 11] = [
    "html", "js", "mjs", "json", "css", "svg", "xml", "wasm", "txt", "md", "mdx",
];

fn default_out() -> String {
    "build".into()
}
fn yes() -> bool {
    true
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Options {
    #[serde(default = "default_out")]
    pub out: String,
    #[serde(default = "yes")]
    pub precompress: bool,
    #[serde(default)]
    pub env_prefix: String,
}
impl Default for Options {
    fn default() -> Self {
        Self {
            out: default_out(),
            precompress: true,
            env_prefix: String::new(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuntimeTemplates {
    pub version: String,
    /// Exact contents of the installed adapter's `files` directory.
    pub files: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct Prerendered {
    /// URL paths in builder.prerendered.paths; ordering is retained.
    pub paths: Vec<String>,
    /// Each map is relative to its respective Kit prerender output directory.
    pub pages: Files,
    pub dependencies: Files,
    pub data: Files,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AdapterInput {
    pub templates: RuntimeTemplates,
    #[serde(default)]
    pub options: Options,
    /// Resolved kit.paths.base, empty or beginning with / without trailing /.
    #[serde(default)]
    pub base: String,
    /// Native generateManifest({relativePath:'./'}) expression, not a JS module.
    pub manifest_expression: String,
    /// Resolved application package metadata; only production dependencies are
    /// externalized. Dev dependencies remain eligible for native bundling.
    pub package_json: Value,
    pub server: Files,
    pub client: Files,
    #[serde(default)]
    pub prerendered: Prerendered,
    /// Relative host staging directory, disjoint from options.out.
    pub staging_directory: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RebundlePlan {
    /// Entry name to path in PackagingPlan.staging_files.
    pub entries: BTreeMap<String, String>,
    pub output_directory: String,
    pub format: String,
    pub sourcemap: bool,
    pub chunk_file_names: String,
    pub prefer_builtins: bool,
    /// Augments the native Node resolver's import/default conditions.
    pub export_conditions: Vec<String>,
    pub strict_commonjs_requires: bool,
    pub json_modules: bool,
    /// Exact package-root matching, including deep exports; not regex matching.
    pub production_dependencies: BTreeMap<String, String>,
}
impl RebundlePlan {
    #[must_use]
    pub fn is_production_external(&self, specifier: &str) -> bool {
        self.production_dependencies.keys().any(|name| {
            specifier == name
                || specifier
                    .strip_prefix(name)
                    .is_some_and(|rest| rest.starts_with('/'))
        })
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "format", rename_all = "camelCase")]
pub enum Compression {
    Gzip {
        level: u8,
    },
    Brotli {
        quality: u8,
        mode: String,
        size_hint: u64,
    },
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CompressionJob {
    pub source: String,
    pub destination: String,
    pub source_sha256: String,
    pub compression: Compression,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CompressionOutput {
    /// Identifies the exact source bytes processed by the native codec host.
    pub source_sha256: String,
    pub bytes: Vec<u8>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PackagingPlan {
    pub adapter_version: String,
    pub output_directory: String,
    /// Fully materialized assets and runtime JS, paths relative to project root.
    pub files: Files,
    /// Compiled server inputs and the actual generated manifest; never deployed
    /// directly in place of completing the native rebundle operation.
    pub staging_files: Files,
    pub bundle: RebundlePlan,
    pub compression: Vec<CompressionJob>,
    pub instrumentation: bool,
    /// Builder's pages -> dependencies -> data copy order intentionally permits
    /// overwrite. Record each overwritten destination for host observability.
    pub overwritten_assets: Vec<String>,
    pub required_stages: Vec<String>,
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn safe_relative(path: &str) -> Result<(), String> {
    if path.is_empty()
        || path.starts_with('/')
        || path.contains(['\\', '\0', ':'])
        || path
            .split('/')
            .any(|part| part.is_empty() || matches!(part, "." | ".."))
        || path.chars().any(char::is_control)
    {
        return Err(format!("Expected a safe relative POSIX path: {path:?}"));
    }
    Ok(())
}
fn overlaps(a: &str, b: &str) -> bool {
    a == b
        || a.strip_prefix(b).is_some_and(|rest| rest.starts_with('/'))
        || b.strip_prefix(a).is_some_and(|rest| rest.starts_with('/'))
}
fn validate_files(files: &Files) -> Result<(), String> {
    if files.len() > 100_000 || files.values().map(Vec::len).sum::<usize>() > 512 * 1024 * 1024 {
        return Err("Adapter artifact inventory exceeds limits".into());
    }
    for name in files.keys() {
        safe_relative(name)?;
    }
    // A file may not also be an ancestor directory of another file.
    for name in files.keys() {
        for (offset, _) in name.match_indices('/') {
            if files.contains_key(&name[..offset]) {
                return Err(format!("File/directory collision: {name}"));
            }
        }
    }
    Ok(())
}
fn validate_templates(templates: &RuntimeTemplates) -> Result<(), String> {
    if templates.version != ADAPTER_VERSION {
        return Err(format!(
            "Unsupported adapter-node version {}",
            templates.version
        ));
    }
    if templates.files.len() != TEMPLATES.len() {
        return Err("Expected the four pinned adapter-node runtime files".into());
    }
    for (name, expected) in TEMPLATES {
        let text = templates
            .files
            .get(name)
            .ok_or_else(|| format!("Missing runtime template {name}"))?;
        if digest(text.as_bytes()) != expected {
            return Err(format!(
                "Runtime template {name} differs from adapter-node {ADAPTER_VERSION}"
            ));
        }
    }
    Ok(())
}
fn package_name(name: &str) -> bool {
    let valid = |part: &str| {
        !part.is_empty()
            && !matches!(part, "." | "..")
            && part
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.'))
    };
    if let Some(scoped) = name.strip_prefix('@') {
        scoped
            .split_once('/')
            .is_some_and(|(scope, package)| valid(scope) && valid(package))
    } else {
        valid(name)
    }
}

/// Match Kit's copy replacement (ASCII JavaScript regexp word boundaries).
/// Replacement is one pass, including comments and template literals. Inserted
/// envPrefix text is never rescanned as another placeholder.
fn replace_template(text: &str, replacements: &BTreeMap<&str, String>) -> String {
    let word = |byte: u8| byte.is_ascii_alphanumeric() || byte == b'_';
    let bytes = text.as_bytes();
    let mut output = String::with_capacity(text.len());
    let mut start = 0;
    let mut offset = 0;
    while offset < bytes.len() {
        if !word(bytes[offset]) {
            offset += 1;
            continue;
        }
        let end = (offset + 1..bytes.len())
            .find(|index| !word(bytes[*index]))
            .unwrap_or(bytes.len());
        // Every matched token consists of ASCII bytes and has UTF-8 boundaries.
        if let Some(replacement) = replacements.get(&text[offset..end]) {
            output.push_str(&text[start..offset]);
            output.push_str(replacement);
            start = end;
        }
        offset = end;
    }
    output.push_str(&text[start..]);
    output
}

fn validate_manifest(expression: &str) -> Result<(), String> {
    if expression.trim().is_empty() || expression.len() > 16 * 1024 * 1024 {
        return Err("Invalid manifest expression size".into());
    }
    let source = format!("export const manifest = {expression};");
    let allocator = oxc_allocator::Allocator::default();
    let parsed = oxc_parser::Parser::new(&allocator, &source, oxc_span::SourceType::mjs()).parse();
    if !parsed.diagnostics.is_empty() || parsed.program.body.len() != 1 {
        return Err("Manifest must be a single valid JavaScript expression".into());
    }
    let oxc_ast::ast::Statement::ExportDeclaration(export) = &parsed.program.body[0] else {
        return Err("Expected the manifest export".into());
    };
    let oxc_ast::ast::Declaration::VariableDeclaration(declaration) = &export.declaration else {
        return Err("Expected the manifest variable declaration".into());
    };
    if declaration.declarations.len() != 1 {
        return Err("Manifest expression introduces another declaration".into());
    }
    Ok(())
}

/// Prepare an actual deployment tree plus explicit native build work. This is
/// not a completed deployment until `materialize` accepts every required output.
pub fn prepare(input: AdapterInput) -> Result<PackagingPlan, String> {
    validate_templates(&input.templates)?;
    prepare_validated(input)
}

fn prepare_validated(input: AdapterInput) -> Result<PackagingPlan, String> {
    let out = &input.options.out;
    safe_relative(out)?;
    safe_relative(&input.staging_directory)?;
    if overlaps(out, &input.staging_directory) {
        return Err("Output and server staging directories must be disjoint".into());
    }
    if !input.base.is_empty() {
        let base = input
            .base
            .strip_prefix('/')
            .ok_or("paths.base must start with /")?;
        safe_relative(base)?;
        if base.contains(['?', '#']) {
            return Err("paths.base cannot contain query or fragment".into());
        }
    }
    if input.options.env_prefix.contains(['\0', '\n', '\r']) {
        return Err("envPrefix must not contain NUL or line breaks".into());
    }
    validate_manifest(&input.manifest_expression)?;
    for files in [
        &input.server,
        &input.client,
        &input.prerendered.pages,
        &input.prerendered.dependencies,
        &input.prerendered.data,
    ] {
        validate_files(files)?;
    }
    if !input.server.contains_key("index.js") {
        return Err("Compiled server index.js is missing".into());
    }
    let package = input
        .package_json
        .as_object()
        .ok_or("Application package metadata must be an object")?;
    let mut dependencies = BTreeMap::new();
    if let Some(declared) = package.get("dependencies") {
        for (name, version) in declared
            .as_object()
            .ok_or("package.dependencies must be an object")?
        {
            if !package_name(name) {
                return Err(format!("Invalid production dependency name {name:?}"));
            }
            dependencies.insert(
                name.clone(),
                version
                    .as_str()
                    .ok_or("Dependency version must be a string")?
                    .to_owned(),
            );
        }
    }
    for path in &input.prerendered.paths {
        if !path.starts_with('/') || path.contains(['\0', '\r', '\n', '\\', '?', '#']) {
            return Err("Prerendered paths must be pathname-only absolute URL paths".into());
        }
    }
    let prefix = serde_json::to_string(&input.options.env_prefix).map_err(|e| e.to_string())?;
    let replacements = BTreeMap::from([
        ("ENV", "./env.js".into()),
        ("HANDLER", "./handler.js".into()),
        ("MANIFEST", "./server/manifest.js".into()),
        ("SERVER", "./server/index.js".into()),
        ("SHIMS", "./shims.js".into()),
        ("ENV_PREFIX", prefix),
        ("PRECOMPRESS", input.options.precompress.to_string()),
    ]);
    let mut files = Files::new();
    for (name, text) in &input.templates.files {
        files.insert(
            format!("{out}/{name}"),
            replace_template(text, &replacements).into_bytes(),
        );
    }
    let instrumentation = input.server.contains_key("instrumentation.server.js");
    if instrumentation {
        let index = files
            .remove(&format!("{out}/index.js"))
            .ok_or("Missing adapter index template")?;
        files.insert(format!("{out}/start.js"), index);
        files.insert(format!("{out}/index.js"), b"import './server/instrumentation.server.js';\nconst { path, host, port, server } = await import('./start.js');\nexport { path, host, port, server };".to_vec());
    }
    let mut overwritten = Vec::new();
    for (name, bytes) in &input.client {
        if name.split('/').any(|segment| segment == ".vite") {
            continue;
        }
        files.insert(format!("{out}/client{}/{name}", input.base), bytes.clone());
    }
    for group in [
        &input.prerendered.pages,
        &input.prerendered.dependencies,
        &input.prerendered.data,
    ] {
        for (name, bytes) in group {
            let destination = format!("{out}/prerendered{}/{name}", input.base);
            if files.insert(destination.clone(), bytes.clone()).is_some() {
                overwritten.push(destination);
            }
        }
    }
    validate_files(&files)?;
    let mut compression = Vec::new();
    if input.options.precompress {
        for (name, bytes) in &files {
            if !(name.starts_with(&format!("{out}/client/"))
                || name.starts_with(&format!("{out}/prerendered/")))
            {
                continue;
            }
            if !name
                .rsplit_once('.')
                .is_some_and(|(_, extension)| COMPRESS_EXTENSIONS.contains(&extension))
            {
                continue;
            }
            for (suffix, format) in [
                ("gz", Compression::Gzip { level: 9 }),
                (
                    "br",
                    Compression::Brotli {
                        quality: 11,
                        mode: "text".into(),
                        size_hint: bytes.len() as u64,
                    },
                ),
            ] {
                let destination = format!("{name}.{suffix}");
                if files.contains_key(&destination) {
                    // Upstream replaces existing compressed sidecars. Do not let
                    // a supplied stale sidecar count as completed compression.
                    overwritten.push(destination.clone());
                }
                compression.push(CompressionJob {
                    source: name.clone(),
                    destination,
                    source_sha256: digest(bytes),
                    compression: format,
                });
            }
        }
    }
    let staging = &input.staging_directory;
    let mut staging_files: Files = input
        .server
        .iter()
        .map(|(name, bytes)| (format!("{staging}/{name}"), bytes.clone()))
        .collect();
    let manifest = format!(
        "export const manifest = {};\n\nexport const prerendered = new Set({});\n\nexport const base = {};",
        input.manifest_expression,
        serde_json::to_string(&input.prerendered.paths).map_err(|e| e.to_string())?,
        serde_json::to_string(&input.base).map_err(|e| e.to_string())?
    );
    staging_files.insert(format!("{staging}/manifest.js"), manifest.into_bytes());
    let mut entries = BTreeMap::from([
        ("index".into(), format!("{staging}/index.js")),
        ("manifest".into(), format!("{staging}/manifest.js")),
    ]);
    if instrumentation {
        entries.insert(
            "instrumentation.server".into(),
            format!("{staging}/instrumentation.server.js"),
        );
    }
    validate_files(&staging_files)?;
    let mut required_stages = vec!["nativeRebundle".into()];
    if !compression.is_empty() {
        required_stages.push("nativeCompression".into());
    }
    Ok(PackagingPlan {
        adapter_version: ADAPTER_VERSION.into(),
        output_directory: out.clone(),
        files,
        staging_files,
        bundle: RebundlePlan {
            entries,
            output_directory: format!("{out}/server"),
            format: "esm".into(),
            sourcemap: true,
            chunk_file_names: "chunks/[name]-[hash].js".into(),
            prefer_builtins: true,
            export_conditions: vec!["node".into()],
            strict_commonjs_requires: true,
            json_modules: true,
            production_dependencies: dependencies,
        },
        compression,
        instrumentation,
        overwritten_assets: overwritten,
        required_stages,
    })
}

impl PackagingPlan {
    /// Complete native host stages. Bundle file names are relative to server/;
    /// compression keys are the exact destination paths in this plan. The codec
    /// host must return valid streams; digest acknowledgements prevent accidentally
    /// applying a result from a different source snapshot. This does not execute
    /// or re-interpret emitted JS or decode compressed streams.
    pub fn materialize(
        &self,
        bundle_files: Files,
        compressed: BTreeMap<String, CompressionOutput>,
    ) -> Result<Files, String> {
        validate_files(&bundle_files)?;
        for entry in self.bundle.entries.keys() {
            for suffix in [".js", ".js.map"] {
                let Some(bytes) = bundle_files.get(&format!("{entry}{suffix}")) else {
                    return Err(format!("Missing rebundled entry {entry}{suffix}"));
                };
                if bytes.is_empty() {
                    return Err(format!("Empty rebundled entry {entry}{suffix}"));
                }
                if suffix == ".js.map" {
                    let map: Value = serde_json::from_slice(bytes)
                        .map_err(|error| format!("Invalid map for {entry}: {error}"))?;
                    if map["version"] != 3
                        || !map["sources"].is_array()
                        || !map["names"].is_array()
                        || !map["mappings"].is_string()
                    {
                        return Err(format!("Expected a version 3 source map for {entry}"));
                    }
                }
            }
        }
        let expected: BTreeSet<_> = self
            .compression
            .iter()
            .map(|job| job.destination.as_str())
            .collect();
        if compressed.len() != expected.len()
            || compressed
                .keys()
                .any(|key| !expected.contains(key.as_str()))
        {
            return Err("Native compression outputs do not match required jobs".into());
        }
        let mut files = self.files.clone();
        for (name, bytes) in bundle_files {
            let destination = format!("{}/{name}", self.bundle.output_directory);
            if files.insert(destination.clone(), bytes).is_some() {
                return Err(format!("Rebundle output collision: {destination}"));
            }
        }
        for job in &self.compression {
            let output = compressed
                .get(&job.destination)
                .ok_or("Missing compressed asset")?;
            if output.source_sha256 != job.source_sha256 || output.bytes.is_empty() {
                return Err(format!(
                    "Invalid or stale compression output for {}",
                    job.source
                ));
            }
            if matches!(job.compression, Compression::Gzip { .. })
                && !output.bytes.starts_with(&[0x1f, 0x8b, 8])
            {
                return Err("Gzip output lacks its stream header".into());
            }
            files.insert(job.destination.clone(), output.bytes.clone());
        }
        validate_files(&files)?;
        Ok(files)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn input() -> AdapterInput {
        // Isolated planner inputs, deliberately not accepted by public prepare:
        // pinned production template identity is tested separately.
        AdapterInput { templates: RuntimeTemplates { version: ADAPTER_VERSION.into(), files: BTreeMap::from([
            ("index.js".into(), "import { handler } from 'HANDLER'; import { env } from 'ENV';".into()),
            ("handler.js".into(), "import 'SHIMS'; import { Server } from 'SERVER'; import { manifest } from 'MANIFEST'; const compressed = PRECOMPRESS;".into()),
            ("env.js".into(), "export const prefix = ENV_PREFIX;".into()), ("shims.js".into(), "export {};".into()),
        ]) }, options: Options::default(), base: "/hr".into(), manifest_expression: "{ appPath: '_app', _: { routes: [] } }".into(),
            package_json: serde_json::json!({"type":"module","dependencies":{"@scope/pkg":"1.0.0","foo.bar":"2"},"devDependencies":{"dev-only":"1"}}),
            server: BTreeMap::from([("index.js".into(), b"export class Server {}".to_vec())]),
            client: BTreeMap::from([("_app/main.js".into(), b"console.log(1);".to_vec()), (".vite/manifest.json".into(), b"{}".to_vec()), ("photo.png".into(), vec![1,2,3])]),
            prerendered: Prerendered { paths: vec!["/hr/".into()], pages: BTreeMap::from([("index.html".into(), b"page".to_vec())]), ..Default::default() },
            staging_directory: ".svelte-kit/adapter-node".into() }
    }
    #[test]
    fn placeholders_match_copy_word_boundaries_and_do_not_rescan_values() {
        let replacements = BTreeMap::from([
            ("ENV", "./env.js".into()),
            ("ENV_PREFIX", "\"ENV_PREFIX SERVER\"".into()),
        ]);
        assert_eq!(
            replace_template("'ENV' ENV_PREFIX XENV ENV_extra $ENV éENV", &replacements),
            "'./env.js' \"ENV_PREFIX SERVER\" XENV ENV_extra $./env.js é./env.js"
        );
    }
    #[test]
    fn defaults_assets_manifest_and_real_native_work_are_materialized() {
        let plan = prepare_validated(input()).unwrap();
        assert!(plan.files.contains_key("build/client/hr/_app/main.js"));
        assert!(!plan.files.keys().any(|path| path.contains(".vite")));
        assert_eq!(plan.compression.len(), 4);
        assert!(
            !plan
                .compression
                .iter()
                .any(|job| job.source.ends_with(".png") || job.source == "build/index.js")
        );
        assert!(plan.bundle.is_production_external("@scope/pkg/deep"));
        assert!(plan.bundle.is_production_external("foo.bar"));
        assert!(!plan.bundle.is_production_external("fooXbar"));
        assert!(!plan.bundle.is_production_external("@scope/pkg-extra"));
        assert!(!plan.bundle.is_production_external("dev-only"));
        let manifest =
            String::from_utf8(plan.staging_files[".svelte-kit/adapter-node/manifest.js"].clone())
                .unwrap();
        assert!(
            manifest.contains("new Set([\"/hr/\"])")
                && manifest.contains("export const base = \"/hr\"")
        );
        assert!(plan.materialize(Files::new(), BTreeMap::new()).is_err());
    }
    #[test]
    fn instrumentation_is_loaded_before_dynamic_start_with_exact_exports() {
        let mut input = input();
        input
            .server
            .insert("instrumentation.server.js".into(), b"export {};".to_vec());
        input.options.precompress = false;
        input.options.env_prefix = "APP_\"\\ENV_PREFIX".into();
        let plan = prepare_validated(input).unwrap();
        let index = String::from_utf8(plan.files["build/index.js"].clone()).unwrap();
        assert!(index.starts_with("import './server/instrumentation.server.js';\n"));
        assert!(index.contains("await import('./start.js')"));
        assert!(index.contains("export { path, host, port, server }"));
        assert!(plan.files.contains_key("build/start.js"));
        assert!(plan.bundle.entries.contains_key("instrumentation.server"));
        assert!(plan.compression.is_empty());
        assert_eq!(
            String::from_utf8(plan.files["build/env.js"].clone()).unwrap(),
            "export const prefix = \"APP_\\\"\\\\ENV_PREFIX\";"
        );
    }
    #[test]
    fn escapes_identity_changes_and_unfinished_jobs_are_rejected() {
        assert!(prepare(input()).unwrap_err().contains("differs"));
        let mut unsafe_input = input();
        unsafe_input.options.out = "../build".into();
        assert!(prepare_validated(unsafe_input).is_err());
        let mut unsafe_input = input();
        unsafe_input.staging_directory = "build/tmp".into();
        assert!(prepare_validated(unsafe_input).is_err());
        let mut unsafe_input = input();
        unsafe_input.client.insert("../../escape".into(), vec![]);
        assert!(prepare_validated(unsafe_input).is_err());
        assert!(validate_manifest("{}; process.exit(1)").is_err());
        assert!(validate_manifest("{}, injected = 1").is_err());
        let plan = prepare_validated(input()).unwrap();
        let bundled = BTreeMap::from([
            ("index.js".into(), b"export class Server {}".to_vec()),
            (
                "index.js.map".into(),
                br#"{"version":3,"sources":[],"names":[],"mappings":""}"#.to_vec(),
            ),
            (
                "manifest.js".into(),
                b"export const manifest = {};".to_vec(),
            ),
            (
                "manifest.js.map".into(),
                br#"{"version":3,"sources":[],"names":[],"mappings":""}"#.to_vec(),
            ),
        ]);
        assert!(
            plan.materialize(bundled, BTreeMap::new())
                .unwrap_err()
                .contains("compression")
        );
    }
    #[test]
    fn prerender_copy_order_and_disabled_compression_preserve_assets() {
        let mut input = input();
        input.options.precompress = false;
        input
            .prerendered
            .dependencies
            .insert("index.html".into(), b"dependency".to_vec());
        input
            .prerendered
            .data
            .insert("index.html".into(), b"data".to_vec());
        let plan = prepare_validated(input).unwrap();
        assert_eq!(plan.files["build/prerendered/hr/index.html"], b"data");
        assert_eq!(
            plan.overwritten_assets,
            vec!["build/prerendered/hr/index.html"; 2]
        );
        let bundled = BTreeMap::from([
            ("index.js".into(), b"export {};".to_vec()),
            (
                "index.js.map".into(),
                br#"{"version":3,"sources":[],"names":[],"mappings":""}"#.to_vec(),
            ),
            ("manifest.js".into(), b"export {};".to_vec()),
            (
                "manifest.js.map".into(),
                br#"{"version":3,"sources":[],"names":[],"mappings":""}"#.to_vec(),
            ),
        ]);
        assert!(
            plan.materialize(bundled, BTreeMap::new())
                .unwrap()
                .contains_key("build/server/index.js")
        );
    }
}
