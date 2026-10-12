//! Native, isolated TypeScript preprocessing for the Svelte build pipeline.
//!
//! `transform(source, filename, &options)` invokes pinned OXC parsing, semantic
//! analysis, TypeScript transformation and JavaScript generation. It never
//! reads a tsconfig, resolves imports, accesses the filesystem, or launches a
//! process. The caller resolves options and composes the returned source map
//! with subsequent Svelte compilation. Diagnostic spans are original UTF-8 byte
//! offsets; source map columns use the source-map UTF-16 convention.
//!
//! This is single-file ESNext/ESM emission, not TypeScript type checking. Runtime
//! enums, namespaces and parameter properties are lowered by OXC; rune calls,
//! ordinary class fields and ESNext JavaScript remain available to the Svelte
//! compiler. Both const and regular enums retain runtime objects. Decorators,
//! CommonJS TypeScript constructs, assignment-style class fields and transforms
//! requiring an additional OXC helper runtime are explicitly rejected. JSX must
//! opt into a resolved classic or automatic runtime; its imports are returned in
//! the generated JavaScript for the caller's bundler to resolve.

use std::{fmt, path::Path};

use oxc_allocator::Allocator;
use oxc_ast::{AstKind, ast::TSModuleReference};
use oxc_codegen::{Codegen, CodegenOptions};
use oxc_diagnostics::{OxcDiagnostic, Severity as OxcSeverity};
use oxc_parser::Parser;
use oxc_semantic::SemanticBuilder;
use oxc_span::{GetSpan, SourceType, Span};
use oxc_transformer::{
    HelperLoaderMode, JsxOptions, JsxRuntime, TransformOptions as OxcOptions, Transformer,
};
use serde::{Deserialize, Serialize};

/// Only ESNext/ESM is exposed: unknown target/module values fail deserialization.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub enum Target {
    #[default]
    #[serde(rename = "esnext")]
    Esnext,
}
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub enum Module {
    #[default]
    #[serde(rename = "esm")]
    Esm,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(
    tag = "mode",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum Jsx {
    #[default]
    Reject,
    Automatic {
        import_source: String,
        development: bool,
    },
    Classic {
        factory: String,
        fragment: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct TransformOptions {
    pub target: Target,
    pub module: Module,
    pub source_map: bool,
    /// True preserves all value imports, including those used only as types.
    /// Explicit `import type` and type-qualified specifiers are always erased.
    pub only_remove_type_imports: bool,
    /// False is rejected: lowering class fields before rune compilation changes
    /// the Svelte compiler's representation of reactive class properties.
    pub use_define_for_class_fields: bool,
    pub jsx: Jsx,
}
impl Default for TransformOptions {
    fn default() -> Self {
        Self {
            target: Target::Esnext,
            module: Module::Esm,
            source_map: true,
            only_remove_type_imports: true,
            use_define_for_class_fields: true,
            jsx: Jsx::Reject,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Phase {
    Options,
    Parse,
    Semantic,
    Transform,
    Codegen,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Severity {
    Error,
    Warning,
    Advice,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticLabel {
    pub start: u32,
    pub end: u32,
    pub primary: bool,
    pub message: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Diagnostic {
    pub filename: String,
    pub phase: Phase,
    pub severity: Severity,
    pub code: Option<String>,
    pub message: String,
    pub labels: Vec<DiagnosticLabel>,
    pub help: Option<String>,
    pub note: Option<String>,
    pub url: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TransformOutput {
    pub code: String,
    pub map: Option<serde_json::Value>,
    pub diagnostics: Vec<Diagnostic>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransformFailure {
    pub diagnostics: Vec<Diagnostic>,
}
impl fmt::Display for TransformFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, diagnostic) in self.diagnostics.iter().enumerate() {
            if index != 0 {
                writeln!(f)?;
            }
            write!(f, "{}: {}", diagnostic.filename, diagnostic.message)?;
        }
        Ok(())
    }
}
impl std::error::Error for TransformFailure {}

fn diagnostic(
    filename: &str,
    phase: Phase,
    message: impl Into<String>,
    span: Option<Span>,
) -> Diagnostic {
    Diagnostic {
        filename: filename.into(),
        phase,
        severity: Severity::Error,
        code: Some("svelte-transpile-unsupported".into()),
        message: message.into(),
        labels: span
            .into_iter()
            .map(|s| DiagnosticLabel {
                start: s.start,
                end: s.end,
                primary: true,
                message: None,
            })
            .collect(),
        help: None,
        note: None,
        url: None,
    }
}
fn convert(filename: &str, phase: Phase, error: &OxcDiagnostic) -> Diagnostic {
    Diagnostic {
        filename: filename.into(),
        phase,
        severity: match error.severity {
            OxcSeverity::Error => Severity::Error,
            OxcSeverity::Warning => Severity::Warning,
            OxcSeverity::Advice => Severity::Advice,
        },
        code: error.code.is_some().then(|| error.code.to_string()),
        message: error.message.to_string(),
        labels: error
            .labels
            .iter()
            .map(|label| DiagnosticLabel {
                start: label.offset(),
                end: label.offset() + label.len(),
                primary: label.primary(),
                message: label.label().map(str::to_owned),
            })
            .collect(),
        help: error.help.as_deref().map(str::to_owned),
        note: error.note.as_deref().map(str::to_owned),
        url: error.url.as_deref().map(str::to_owned),
    }
}
fn failed(diagnostics: &[Diagnostic]) -> bool {
    diagnostics.iter().any(|d| d.severity == Severity::Error)
}
fn qualified_identifier(value: &str) -> bool {
    value.split('.').all(|part| {
        let mut chars = part.chars();
        chars
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_' || c == '$')
            && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
    })
}

/// Transform a single source; failures never contain partial emitted JavaScript.
pub fn transform(
    source: &str,
    filename: &str,
    options: &TransformOptions,
) -> Result<TransformOutput, TransformFailure> {
    let fail = |message: &str| TransformFailure {
        diagnostics: vec![diagnostic(filename, Phase::Options, message, None)],
    };
    if filename.is_empty() || filename.contains('\0') {
        return Err(fail("A nonempty source filename without NUL is required"));
    }
    if source.len() > u32::MAX as usize {
        return Err(fail("Source exceeds OXC's UTF-8 offset range"));
    }
    let path = Path::new(filename);
    if !matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("ts" | "tsx" | "mts" | "js" | "jsx" | "mjs" | "cts" | "cjs")
    ) {
        return Err(fail(
            "Expected a .ts, .tsx, .mts, .js, .jsx or .mjs filename",
        ));
    }
    let source_type = SourceType::from_path(path)
        .map_err(|_| fail("Expected a .ts, .tsx, .mts, .js, .jsx or .mjs filename"))?;
    if source_type.is_typescript_definition()
        || filename.ends_with(".cts")
        || filename.ends_with(".cjs")
    {
        return Err(fail(
            "Declaration files and CommonJS input modes are unsupported by ESNext/ESM preprocessing",
        ));
    }
    if !options.use_define_for_class_fields {
        return Err(fail(
            "useDefineForClassFields:false is unsupported before Svelte rune compilation",
        ));
    }
    let mut transform_options = OxcOptions::from_target("esnext").map_err(|e| fail(&e))?;
    transform_options.typescript.only_remove_type_imports = options.only_remove_type_imports;
    transform_options.typescript.allow_namespaces = true;
    transform_options.typescript.optimize_const_enums = false;
    transform_options.typescript.optimize_enums = false;
    transform_options.helper_loader.mode = HelperLoaderMode::Runtime;
    transform_options.jsx = match &options.jsx {
        Jsx::Reject => JsxOptions::disable(),
        Jsx::Automatic {
            import_source,
            development,
        } => {
            if import_source.is_empty() || import_source.chars().any(char::is_control) {
                return Err(fail(
                    "JSX importSource must be a nonempty module specifier without control characters",
                ));
            }
            JsxOptions {
                runtime: JsxRuntime::Automatic,
                import_source: Some(import_source.clone()),
                development: *development,
                ..JsxOptions::enable()
            }
        }
        Jsx::Classic { factory, fragment } => {
            if !qualified_identifier(factory) || !qualified_identifier(fragment) {
                return Err(fail(
                    "Classic JSX factory and fragment must be qualified ASCII identifiers",
                ));
            }
            transform_options.typescript.jsx_pragma = factory.clone().into();
            transform_options.typescript.jsx_pragma_frag = fragment.clone().into();
            JsxOptions {
                runtime: JsxRuntime::Classic,
                pragma: Some(factory.clone()),
                pragma_frag: Some(fragment.clone()),
                ..JsxOptions::enable()
            }
        }
    };
    transform_options.jsx.conform();
    let allocator = Allocator::default();
    let parsed = Parser::new(&allocator, source, source_type.with_module(true)).parse();
    let mut diagnostics = parsed
        .diagnostics
        .iter()
        .map(|d| convert(filename, Phase::Parse, d))
        .collect::<Vec<_>>();
    if parsed.fatal_error && !failed(&diagnostics) {
        diagnostics.push(diagnostic(
            filename,
            Phase::Parse,
            "OXC parsing terminated without a complete program",
            None,
        ));
    }
    if failed(&diagnostics) {
        return Err(TransformFailure { diagnostics });
    }
    let mut program = parsed.program;
    let semantic = SemanticBuilder::new()
        .with_check_syntax_error(true)
        .with_build_nodes(true)
        .with_enum_eval(true)
        .with_excess_capacity(2.0)
        .build(&program);
    diagnostics.extend(
        semantic
            .diagnostics
            .iter()
            .map(|d| convert(filename, Phase::Semantic, d)),
    );
    for node in semantic.semantic.nodes().iter() {
        let kind = node.kind();
        let unsupported = match kind {
            AstKind::Decorator(_) => {
                Some("Decorator lowering is not enabled for Svelte preprocessing")
            }
            AstKind::TSExportAssignment(_) => {
                Some("TypeScript export = requires CommonJS emission")
            }
            AstKind::TSImportEqualsDeclaration(d)
                if matches!(
                    &d.module_reference,
                    TSModuleReference::ExternalModuleReference(_)
                ) =>
            {
                Some("TypeScript import = require requires a CommonJS interop contract")
            }
            AstKind::JSXElement(_) | AstKind::JSXFragment(_)
                if matches!(options.jsx, Jsx::Reject) =>
            {
                Some("JSX requires an explicit classic or automatic runtime")
            }
            _ => None,
        };
        if let Some(message) = unsupported {
            diagnostics.push(diagnostic(
                filename,
                Phase::Transform,
                message,
                Some(kind.span()),
            ));
        }
    }
    if failed(&diagnostics) {
        return Err(TransformFailure { diagnostics });
    }
    let scoping = semantic.semantic.into_scoping();
    let transformed = Transformer::new(&allocator, path, &transform_options)
        .build_with_scoping(scoping, &mut program);
    diagnostics.extend(
        transformed
            .diagnostics
            .iter()
            .map(|d| convert(filename, Phase::Transform, d)),
    );
    // OXC currently exposes helper usage only via this deprecated, pinned API.
    // Refuse output needing a helper package the host has not explicitly supplied.
    #[allow(deprecated)]
    if !transformed.helpers_used.is_empty() {
        diagnostics.push(diagnostic(filename, Phase::Transform, "This syntax requires an OXC helper runtime, which this preprocessing contract does not provide", None));
    }
    if failed(&diagnostics) {
        return Err(TransformFailure { diagnostics });
    }
    let emitted = Codegen::new()
        .with_source_text(source)
        .with_scoping(Some(transformed.scoping))
        .with_options(CodegenOptions {
            source_map_path: options.source_map.then(|| path.to_owned()),
            ..CodegenOptions::default()
        })
        .build(&program);
    let map = if let Some(mut map) = emitted.map {
        map.set_sources([filename]);
        map.set_source_contents(vec![Some(source)]);
        Some(
            serde_json::from_str(&map.to_json_string()).map_err(|e| TransformFailure {
                diagnostics: vec![diagnostic(
                    filename,
                    Phase::Codegen,
                    format!("Invalid generated source map: {e}"),
                    None,
                )],
            })?,
        )
    } else if options.source_map {
        diagnostics.push(diagnostic(
            filename,
            Phase::Codegen,
            "OXC did not return the requested source map",
            None,
        ));
        return Err(TransformFailure { diagnostics });
    } else {
        None
    };
    Ok(TransformOutput {
        code: emitted.code,
        map,
        diagnostics,
    })
}
