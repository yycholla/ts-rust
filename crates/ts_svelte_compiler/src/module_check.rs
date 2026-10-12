//! Compiler diagnostics for original rune-module JS/erasable TypeScript inputs.
use oxc_ast::ast::{
    Decorator, FormalParameter, TSEnumDeclaration, TSImportEqualsDeclaration, TSModuleBlock,
};
use oxc_ast_visit::{Visit, walk};
use serde_json::{Value, json};
use ts_svelte::{ByteRange, Document, Snapshot, compatibility::Profile};

#[derive(Default)]
struct RuntimeTypescript(bool);
impl<'a> Visit<'a> for RuntimeTypescript {
    fn visit_ts_enum_declaration(&mut self, _: &TSEnumDeclaration<'a>) {
        self.0 = true;
    }
    fn visit_ts_module_block(&mut self, _: &TSModuleBlock<'a>) {
        self.0 = true;
    }
    fn visit_ts_import_equals_declaration(&mut self, _: &TSImportEqualsDeclaration<'a>) {
        self.0 = true;
    }
    fn visit_decorator(&mut self, _: &Decorator<'a>) {
        self.0 = true;
    }
    fn visit_formal_parameter(&mut self, parameter: &FormalParameter<'a>) {
        self.0 |= parameter.accessibility.is_some() || parameter.readonly;
        walk::walk_formal_parameter(self, parameter);
    }
}

pub fn check(snapshot: Snapshot, typescript: bool, profile: &Profile) -> Value {
    if let Some(error) = super::validate(&snapshot) {
        return error;
    }
    let original = Document::new(snapshot.clone());
    let mut input = snapshot;
    if typescript {
        let allocator = oxc_allocator::Allocator::default();
        let parsed =
            oxc_parser::Parser::new(&allocator, original.text(), oxc_span::SourceType::ts())
                .parse();
        if !parsed.diagnostics.is_empty() {
            return super::failure(
                "module_typescript_parse",
                "TypeScript module parsing failed; compiler diagnostics are incomplete",
            );
        }
        let mut unsupported = RuntimeTypescript::default();
        unsupported.visit_program(&parsed.program);
        if unsupported.0 {
            return super::failure(
                "unsupported_module_typescript",
                "Rune-module checking currently requires erasable TypeScript (no enums, namespaces, import-equals, decorators or parameter properties)",
            );
        }
        input.text = rsvelte_core::compiler::phases::phase2_analyze::types::blank_typescript(
            original.text(),
        );
        if input.text.len() != original.text().len() {
            return super::failure("module_mapping", "Type erasure changed source byte length");
        }
    }
    let mut response = super::compile_module(
        input,
        super::Target::Client,
        false,
        profile,
        &super::build_options::BuildOptions::default(),
    );
    let body = if response["ok"] == true {
        &mut response["result"]
    } else {
        &mut response
    };
    if let Some(diagnostics) = body["diagnostics"].as_array_mut() {
        for diagnostic in diagnostics {
            let bytes = serde_json::from_value::<ByteRange>(diagnostic["bytes"].clone()).ok();
            diagnostic["range"] = json!(bytes.and_then(|r| original.editor_range(r).ok()));
        }
    }
    if let Some(object) = body.as_object_mut() {
        object.remove("javascript");
        object.remove("css");
    }
    response
}
