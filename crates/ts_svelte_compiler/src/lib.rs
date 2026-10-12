//! Native/WASM Svelte compilation using a pinned Rust backend.
//! This layer compiles components and exposes checking projections. It does not
//! claim TypeScript checking, `SvelteKit` builds or version parity with the app.

use rsvelte::{ComponentOptions, Diagnostic, Engine, ProjectionOptions};
use serde::Deserialize;
use serde_json::{Value, json};
use ts_svelte::compatibility::{self, Profile, Versions};
use ts_svelte::{ByteRange, Document, Snapshot};

mod build_options;
mod module_check;
mod runtime;
pub mod syntax;

pub const BACKEND_COMMIT: &str = "5ed8ea3a3401b9fbbe780d3b18d6f90073291d6b";
pub const MAX_SOURCE_BYTES: usize = 1024 * 1024;

#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "camelCase", deny_unknown_fields)]
enum Request {
    Capabilities {},
    ResolveCompatibility {
        versions: Versions,
    },
    Compile {
        document: Snapshot,
        target: Target,
        #[serde(default)]
        runes: Option<bool>,
        #[serde(default)]
        dev: bool,
        #[serde(default)]
        options: build_options::BuildOptions,
        #[serde(default)]
        compatibility: Versions,
    },
    CompileModule {
        document: Snapshot,
        target: Target,
        #[serde(default)]
        dev: bool,
        #[serde(default)]
        compatibility: Versions,
        #[serde(default)]
        options: build_options::BuildOptions,
    },
    CheckModule {
        document: Snapshot,
        typescript: bool,
        #[serde(default)]
        compatibility: Versions,
    },
    ComponentInfo {
        document: Snapshot,
        #[serde(default)]
        runes: Option<bool>,
        #[serde(default)]
        compatibility: Versions,
    },
    Project {
        document: Snapshot,
        #[serde(default)]
        runes: Option<bool>,
        typescript: bool,
        #[serde(default)]
        compatibility: Versions,
    },
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Target {
    Client,
    Server,
}

fn failure(code: &str, message: &str) -> Value {
    json!({"ok":false,"error":{"code":code,"message":message}})
}

fn backend(profile: &Profile) -> Value {
    let fingerprint = Engine::new().fingerprint();
    json!({"name":"rsvelte", "commit":BACKEND_COMMIT,
        "version":fingerprint.facade_version,"svelteTarget":fingerprint.svelte_version,"appTarget":profile.svelte_version,"patches":["child-reset-5.53.11","declaration-tags-5.56","select-default-5.57"],"clientDevelopment":profile.client_development()})
}

fn diagnostic(diagnostic: &Diagnostic, document: &Document, profile: &Profile) -> Value {
    let bytes = diagnostic.span.map(|span| ByteRange {
        start: span.start() as usize,
        end: span.end() as usize,
    });
    let range = bytes.and_then(|bytes| document.editor_range(bytes).ok());
    // The app's pinned 5.53.11 warning predates the backend's element-name wording.
    // This changes the public adapter output, not the oracle's comparison rules.
    let message = if diagnostic.code == "a11y_click_events_have_key_events"
        && diagnostic.severity == rsvelte::DiagnosticSeverity::Warning
        && profile.svelte_minor() < 55
    {
        "Visible, non-interactive elements with a click event must be accompanied by a keyboard event handler. Consider whether an interactive element such as `<button type=\"button\">` or `<a>` might be more appropriate\nhttps://svelte.dev/e/a11y_click_events_have_key_events"
    } else {
        &diagnostic.message
    };
    json!({"code":diagnostic.code,"message":message,
        "severity":if diagnostic.severity == rsvelte::DiagnosticSeverity::Warning {"warning"} else {"error"},
        "uri":document.snapshot().uri,"bytes":bytes,"range":range})
}

fn core_failure(
    error: &rsvelte_core::CompileError,
    document: &Document,
    profile: &Profile,
) -> Value {
    let d = error.diagnostic();
    let bytes = d.span.map(|(start, end)| ByteRange {
        start: start as usize,
        end: end as usize,
    });
    let range = bytes.and_then(|bytes| document.editor_range(bytes).ok());
    json!({"ok":false,"error":{"code":"svelte_compile","message":d.message},
        "diagnostics":[{"code":d.code,"message":d.message,"severity":"error","uri":document.snapshot().uri,"bytes":bytes,"range":range}],"backend":backend(profile)})
}

fn utf16_byte(text: &str, offset: usize) -> Option<usize> {
    let mut units = 0;
    for (byte, ch) in text.char_indices() {
        if units == offset {
            return Some(byte);
        }
        units += ch.len_utf16();
        if units > offset {
            return None;
        }
    }
    (units == offset).then_some(text.len())
}

fn core_warning(
    warning: &rsvelte_core::compiler::Warning,
    document: &Document,
    profile: &Profile,
) -> Value {
    let bytes = warning.start.as_ref().and_then(|start| {
        Some(ByteRange {
            start: utf16_byte(document.text(), start.character)?,
            end: utf16_byte(
                document.text(),
                warning.end.as_ref().unwrap_or(start).character,
            )?,
        })
    });
    let range = bytes.and_then(|bytes| document.editor_range(bytes).ok());
    let message = if warning.code == "a11y_click_events_have_key_events"
        && profile.svelte_minor() < 55
    {
        "Visible, non-interactive elements with a click event must be accompanied by a keyboard event handler. Consider whether an interactive element such as `<button type=\"button\">` or `<a>` might be more appropriate\nhttps://svelte.dev/e/a11y_click_events_have_key_events"
    } else {
        &warning.message
    };
    json!({"code":warning.code,"message":message,"severity":"warning","uri":document.snapshot().uri,"bytes":bytes,"range":range})
}

fn core_options(
    document: &Document,
    profile: &Profile,
    runes: Option<bool>,
    dev: bool,
    target: Target,
) -> rsvelte_core::CompileOptions {
    rsvelte_core::CompileOptions {
        filename: Some(document.snapshot().uri.clone()),
        runes,
        dev,
        compatibility_minor: profile.svelte_minor(),
        generate: match target {
            Target::Client => rsvelte_core::GenerateMode::Client,
            Target::Server => rsvelte_core::GenerateMode::Server,
        },
        ..Default::default()
    }
}

fn projection_failure(error: &Diagnostic, document: &Document, profile: &Profile) -> Value {
    json!({"ok":false,"error":{"code":"svelte_projection","message":error.message},
        "diagnostics":[diagnostic(error,document,profile)],"backend":backend(profile)})
}

fn validate(document: &Snapshot) -> Option<Value> {
    if document.text.len() > MAX_SOURCE_BYTES {
        return Some(failure("size_limit", "component exceeds 1 MiB"));
    }
    // Upstream strips BOM before analysis but reports some offsets against the
    // untrimmed input. Reject it rather than returning incorrect source maps.
    if document.text.starts_with('\u{feff}') {
        return Some(failure(
            "unsupported_bom",
            "BOM-prefixed components are not supported by this backend adapter yet",
        ));
    }
    None
}

fn execute(request: Request) -> Value {
    let versions = match &request {
        Request::Capabilities {} => Versions::default(),
        Request::ResolveCompatibility { versions } => versions.clone(),
        Request::Compile { compatibility, .. }
        | Request::CompileModule { compatibility, .. }
        | Request::CheckModule { compatibility, .. }
        | Request::ComponentInfo { compatibility, .. }
        | Request::Project { compatibility, .. } => compatibility.clone(),
    };
    let profile = match compatibility::resolve(&versions) {
        Ok(profile) => profile,
        Err(message) => return failure("unsupported_version", &message),
    };
    let mut response = execute_with_profile(request, &profile);
    let body = if response["ok"] == true {
        &mut response["result"]
    } else {
        &mut response
    };
    body["compatibility"] = json!(profile);
    response
}

fn execute_with_profile(request: Request, profile: &Profile) -> Value {
    match request {
        Request::Capabilities {} => json!({"ok":true,"result":{
            "stage":"experimental-compiler","backend":backend(profile),
            "operations":["capabilities","compile","compileModule","checkModule","componentInfo","project","resolveCompatibility"],
            "svelteVersions":compatibility::SVELTE_VERSIONS,"kitVersions":compatibility::KIT_VERSIONS,
            "targets":["client","server"],"sourceLanguages":["javascript","typescript"],
            "typeChecking":false,"svelteKitBuild":false,"lsp":false,
            "maxSourceBytes":MAX_SOURCE_BYTES,"bom":false}}),
        Request::ResolveCompatibility { .. } => json!({"ok":true,"result":{}}),
        Request::Compile {
            document,
            target,
            runes,
            dev,
            options,
            ..
        } => compile(document, target, runes, dev, profile, &options),
        Request::CompileModule {
            document,
            target,
            dev,
            options,
            ..
        } => compile_module(document, target, dev, profile, &options),
        Request::CheckModule {
            document,
            typescript,
            ..
        } => module_check::check(document, typescript, profile),
        Request::ComponentInfo {
            document, runes, ..
        } => {
            if let Some(error) = validate(&document) {
                return error;
            }
            let document = Document::new(document);
            match Engine::new().prepare(
                document.text(),
                ComponentOptions::new()
                    .filename(&document.snapshot().uri)
                    .runes(runes),
            ) {
                Ok(component) => {
                    json!({"ok":true,"result":{"typescript":component.facts().scripts.iter().any(|s|s.typescript)}})
                }
                Err(error) => projection_failure(&error.diagnostic, &document, profile),
            }
        }
        Request::Project {
            document,
            runes,
            typescript,
            ..
        } => project(document, runes, typescript, profile),
    }
}

fn compile(
    document: Snapshot,
    target: Target,
    runes: Option<bool>,
    dev: bool,
    profile: &Profile,
    options: &build_options::BuildOptions,
) -> Value {
    if let Some(error) = validate(&document) {
        return error;
    }
    if dev && matches!(target, Target::Client) && !profile.client_development() {
        return failure(
            "runtime_compatibility",
            &format!(
                "Client development instrumentation requires a newer Svelte runtime; Svelte {} is not supported in dev mode yet",
                profile.svelte_version
            ),
        );
    }
    let document = Document::new(document);
    let mut config = core_options(&document, profile, runes, dev, target);
    if let Err(message) = options.apply(&mut config) {
        return failure("unsupported_option", &message);
    }
    let mut component =
        match rsvelte_core::toolchain::Toolchain::new().prepare(document.text(), config) {
            Ok(component) => component,
            Err(error) => return core_failure(&error, &document, profile),
        };
    let scope = component.facts().css_scope_hash.clone();
    let runtime_target = match target {
        Target::Client => rsvelte_core::toolchain::RuntimeTarget::Client,
        Target::Server => rsvelte_core::toolchain::RuntimeTarget::Server,
    };
    match component.compile(runtime_target) {
        Ok(artifact) => {
            let diagnostics = artifact
                .warnings
                .iter()
                .map(|d| core_warning(d, &document, profile))
                .collect::<Vec<_>>();
            if let Err(message) =
                runtime::validate(&artifact.js.code, profile, matches!(target, Target::Client))
            {
                let mut error = failure("runtime_compatibility", &message);
                error["diagnostics"] = json!(diagnostics);
                return error;
            }
            json!({"ok":true,"result":{
                "uri":document.snapshot().uri,"version":document.snapshot().version,
                "javascript":{"code":artifact.js.code,"map":artifact.js.map},
                "css":artifact.css.map(|css| json!({"code":css.code,"map":css.map,"scope":scope})),
                "diagnostics":diagnostics,"runes":artifact.metadata.runes,"backend":backend(profile)}})
        }
        Err(error) => core_failure(&error, &document, profile),
    }
}

fn compile_module(
    document: Snapshot,
    target: Target,
    dev: bool,
    profile: &Profile,
    options: &build_options::BuildOptions,
) -> Value {
    if let Some(error) = validate(&document) {
        return error;
    }
    if let Err(message) = options.validate_module() {
        return failure("unsupported_option", &message);
    }
    if dev && matches!(target, Target::Client) && !profile.client_development() {
        return failure(
            "runtime_compatibility",
            "Module client development mode is unsupported for this runtime",
        );
    }
    let document = Document::new(document);
    let mut config = core_options(&document, profile, None, dev, target);
    if let Err(message) = options.apply(&mut config) {
        return failure("unsupported_option", &message);
    }
    let config = rsvelte_core::compiler::ModuleCompileOptions {
        filename: config.filename,
        root_dir: config.root_dir,
        dev,
        generate: config.generate,
        experimental: config.experimental,
        ..Default::default()
    };
    match rsvelte_core::compile_module(document.text(), config) {
        Err(error) => core_failure(&error, &document, profile),
        Ok(artifact) => {
            if let Err(message) =
                runtime::validate(&artifact.js.code, profile, matches!(target, Target::Client))
            {
                return failure("runtime_compatibility", &message);
            }
            json!({"ok":true,"result":{"uri":document.snapshot().uri,"version":document.snapshot().version,
                "javascript":{"code":artifact.js.code,"map":artifact.js.map},"css":null,"runes":true,
                "diagnostics":artifact.warnings.iter().map(|w|core_warning(w,&document,profile)).collect::<Vec<_>>(),"backend":backend(profile)}})
        }
    }
}

fn project(document: Snapshot, runes: Option<bool>, typescript: bool, profile: &Profile) -> Value {
    if let Some(error) = validate(&document) {
        return error;
    }
    let document = Document::new(document);
    // Validate the selected language version before the newest checking transform.
    if profile.svelte_minor() < 56
        && let Err(error) = rsvelte_core::toolchain::Toolchain::new().prepare(
            document.text(),
            core_options(&document, profile, runes, false, Target::Server),
        )
    {
        return core_failure(&error, &document, profile);
    }
    if !typescript {
        let inspected = Engine::new().prepare(
            document.text(),
            ComponentOptions::new()
                .filename(&document.snapshot().uri)
                .runes(runes),
        );
        match inspected {
            Ok(component)
                if component
                    .facts()
                    .scripts
                    .iter()
                    .any(|script| script.typescript) =>
            {
                return failure(
                    "projection_language",
                    "TypeScript script blocks require typescript: true",
                );
            }
            Err(error) => return projection_failure(&error.diagnostic, &document, profile),
            Ok(_) => {}
        }
    }
    let options = ProjectionOptions::new()
        .filename(&document.snapshot().uri)
        .runes(runes)
        .typescript(typescript)
        .emit_jsdoc(!typescript);
    match Engine::new().project(document.text(), options) {
        Ok(artifact) => json!({"ok":true,"result":{
                    "uri":document.snapshot().uri,"version":document.snapshot().version,
                    "code":artifact.code,"map":artifact.source_map,
                    "mappings":artifact.exact_mappings.map(|mappings| mappings.segments().iter().map(|mapping| json!({
                        "source":{"start":mapping.source.start(),"end":mapping.source.end()},
                        "generated":{"start":mapping.generated.start(),"end":mapping.generated.end()}
                    })).collect::<Vec<_>>()),
                    "runes":artifact.facts.runes,"props":artifact.facts.props.iter().map(|prop| json!({
                        "name":prop.name,"optional":prop.optional,"bindable":prop.bindable,"type":prop.type_annotation
                    })).collect::<Vec<_>>(),"backend":backend(profile)}}),
        Err(error) => projection_failure(&error.diagnostic, &document, profile),
    }
}

/// One request and reply, shared by native and WASM. No filesystem access.
#[must_use]
pub fn dispatch(input: &[u8]) -> Value {
    if input.len() > ts_svelte::protocol::MAX_REQUEST_BYTES {
        return failure("size_limit", "request exceeds 16 MiB");
    }
    let request = match serde_json::from_slice::<Request>(input) {
        Ok(request) => request,
        Err(error) => return failure("invalid_request", &error.to_string()),
    };
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| execute(request))).unwrap_or_else(
        |_| {
            failure(
                "backend_panic",
                "Rust Svelte backend panicked; no successful result is available",
            )
        },
    )
}

/// WASM entry point. Panics trap on WASM; the host must retire that instance.
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen]
#[must_use]
pub fn request_json(input: &str) -> String {
    dispatch(input.as_bytes()).to_string()
}
