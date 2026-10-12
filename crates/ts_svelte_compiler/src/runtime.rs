//! Reject known runtime-version gaps without rewriting emitted JavaScript/maps.
use std::collections::{BTreeSet, HashSet};

use oxc_allocator::Allocator;
use oxc_ast::ast::{
    ComputedMemberExpression, Expression, ImportDeclarationSpecifier, Statement,
    StaticMemberExpression,
};
use oxc_ast_visit::{Visit, walk};
use oxc_parser::Parser;
use oxc_semantic::{Scoping, SemanticBuilder};
use oxc_span::SourceType;
use oxc_syntax::{reference::ReferenceId, symbol::SymbolId};

#[cfg(test)]
fn validate_client(code: &str) -> Result<(), String> {
    let profile =
        ts_svelte::compatibility::resolve(&ts_svelte::compatibility::Versions::default()).unwrap();
    validate(code, &profile, true)
}

// Keep each exact profile bound to its own generated manifest, even when
// neighboring releases currently export identical names.
#[allow(clippy::match_same_arms)]
fn runtime_exports(version: &str) -> Result<(&'static str, &'static str), String> {
    Ok(match version {
        "5.51.5" => (
            include_str!("profiles/client-5.51.5.txt"),
            include_str!("profiles/server-5.51.5.txt"),
        ),
        "5.52.0" => (
            include_str!("profiles/client-5.52.0.txt"),
            include_str!("profiles/server-5.52.0.txt"),
        ),
        "5.53.5" => (
            include_str!("profiles/client-5.53.5.txt"),
            include_str!("profiles/server-5.53.5.txt"),
        ),
        "5.53.11" => (
            include_str!("profiles/client-5.53.11.txt"),
            include_str!("profiles/server-5.53.11.txt"),
        ),
        "5.53.13" => (
            include_str!("profiles/client-5.53.13.txt"),
            include_str!("profiles/server-5.53.13.txt"),
        ),
        "5.54.1" => (
            include_str!("profiles/client-5.54.1.txt"),
            include_str!("profiles/server-5.54.1.txt"),
        ),
        "5.55.10" => (
            include_str!("profiles/client-5.55.10.txt"),
            include_str!("profiles/server-5.55.10.txt"),
        ),
        "5.56.10" => (
            include_str!("profiles/client-5.56.10.txt"),
            include_str!("profiles/server-5.56.10.txt"),
        ),
        "5.57.2" => (
            include_str!("profiles/client-5.57.2.txt"),
            include_str!("profiles/server-5.57.2.txt"),
        ),
        _ => return Err("Unknown runtime profile".into()),
    })
}

pub fn validate(
    code: &str,
    profile: &ts_svelte::compatibility::Profile,
    client: bool,
) -> Result<(), String> {
    let (client_exports, server_exports) = runtime_exports(&profile.svelte_version)?;
    let runtime_module = if client {
        "svelte/internal/client"
    } else {
        "svelte/internal/server"
    };
    let allocator = Allocator::default();
    let parsed = Parser::new(&allocator, code, SourceType::mjs()).parse();
    if !parsed.diagnostics.is_empty() {
        return Err("Generated JavaScript could not be parsed for runtime validation".into());
    }
    let semantic = SemanticBuilder::new().build(&parsed.program);
    if !semantic.diagnostics.is_empty() {
        return Err("Generated JavaScript could not be bound for runtime validation".into());
    }
    let exports: BTreeSet<&str> = (if client {
        client_exports
    } else {
        server_exports
    })
    .lines()
    .collect();
    let mut namespaces = HashSet::new();
    let mut missing = BTreeSet::new();
    for statement in &parsed.program.body {
        let Statement::ImportDeclaration(import) = statement else {
            continue;
        };
        if import.source.value != runtime_module {
            continue;
        }
        for specifier in import.specifiers.iter().flatten() {
            match specifier {
                ImportDeclarationSpecifier::ImportNamespaceSpecifier(specifier) => {
                    namespaces.insert(
                        specifier
                            .local
                            .symbol_id
                            .get()
                            .ok_or("Runtime import has no symbol")?,
                    );
                }
                ImportDeclarationSpecifier::ImportSpecifier(specifier) => {
                    let name = specifier.imported.name();
                    if !exports.contains(name.as_str()) {
                        missing.insert(name.to_string());
                    }
                }
                ImportDeclarationSpecifier::ImportDefaultSpecifier(_) => {
                    missing.insert("default".into());
                }
            }
        }
    }
    let mut visitor = Members {
        namespaces,
        exports,
        missing,
        scoping: semantic.semantic.scoping(),
        checked: HashSet::new(),
    };
    visitor.visit_program(&parsed.program);
    for namespace in &visitor.namespaces {
        if visitor
            .scoping
            .get_resolved_reference_ids(*namespace)
            .iter()
            .any(|id| !visitor.checked.contains(id))
        {
            return Err(format!(
                "Runtime namespace escapes or dynamic accesses are not supported by the Svelte {} compatibility validator",
                profile.svelte_version
            ));
        }
    }
    if visitor.missing.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "Svelte {} runtime does not export required helpers: {}",
            profile.svelte_version,
            visitor.missing.into_iter().collect::<Vec<_>>().join(", ")
        ))
    }
}

struct Members<'s> {
    namespaces: HashSet<SymbolId>,
    exports: BTreeSet<&'static str>,
    missing: BTreeSet<String>,
    scoping: &'s Scoping,
    checked: HashSet<ReferenceId>,
}

impl Members<'_> {
    fn runtime_reference(&self, object: &Expression<'_>) -> Option<ReferenceId> {
        let Expression::Identifier(object) = object.without_parentheses() else {
            return None;
        };
        let id = object.reference_id.get()?;
        let symbol = self.scoping.get_reference(id).symbol_id()?;
        self.namespaces.contains(&symbol).then_some(id)
    }

    fn check_member(&mut self, id: ReferenceId, name: &str) {
        self.checked.insert(id);
        if !self.exports.contains(name) {
            self.missing.insert(name.to_owned());
        }
    }
}

impl<'a> Visit<'a> for Members<'_> {
    fn visit_static_member_expression(&mut self, member: &StaticMemberExpression<'a>) {
        if let Some(id) = self.runtime_reference(&member.object) {
            self.check_member(id, member.property.name.as_str());
        }
        walk::walk_static_member_expression(self, member);
    }

    fn visit_computed_member_expression(&mut self, member: &ComputedMemberExpression<'a>) {
        if let Some(id) = self.runtime_reference(&member.object)
            && let Expression::StringLiteral(name) = &member.expression
        {
            self.check_member(id, name.value.as_str());
        }
        walk::walk_computed_member_expression(self, member);
    }
}

#[cfg(test)]
mod tests {
    use super::validate_client;

    #[test]
    fn runtime_imports_are_resolved_by_binding() {
        assert!(validate_client("import * as $ from 'svelte/internal/client'; $.child(node); function local($) { return $.unsave(); } const text = '$.unsave()';").is_ok());
        assert!(
            validate_client("import * as $ from 'svelte/internal/client'; $['child'](node);")
                .is_ok()
        );
        assert!(
            validate_client("import { child as read } from 'svelte/internal/client'; read(node);")
                .is_ok()
        );
    }

    #[test]
    fn missing_and_unverifiable_helpers_fail_explicitly() {
        for source in [
            "import * as $ from 'svelte/internal/client'; $.unsave();",
            "import * as $ from 'svelte/internal/client'; $['unsave']();",
            "import * as $ from 'svelte/internal/client'; $[name]();",
            "import * as $ from 'svelte/internal/client'; const alias = $; alias.child();",
            "import { unsave } from 'svelte/internal/client'; unsave();",
        ] {
            assert!(validate_client(source).is_err(), "{source}");
        }
    }
}
