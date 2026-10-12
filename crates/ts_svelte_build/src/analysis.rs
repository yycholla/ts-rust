//! Enrich metadata analysis with the exact static environment modules supplied
//! to the server bundler. Dynamic runtime environment values remain unresolved.
use crate::*;
use oxc_allocator::Allocator;
use oxc_ast::ast::{ImportDeclarationSpecifier, Statement};
use oxc_parser::Parser;
use oxc_span::SourceType;

pub(crate) fn runtime_imports(file: &str, source: &str) -> Result<BTreeSet<String>, String> {
    let arena = Allocator::default();
    let parsed = Parser::new(
        &arena,
        source,
        SourceType::from_path(file)
            .unwrap_or(SourceType::ts())
            .with_module(true),
    )
    .parse();
    if !parsed.diagnostics.is_empty() {
        return Err(format!(
            "Cannot inspect original imports in {file}: {:?}",
            parsed.diagnostics[0]
        ));
    }
    let mut imports = BTreeSet::new();
    for statement in &parsed.program.body {
        match statement {
            Statement::ImportDeclaration(import) if !import.import_kind.is_type() => {
                let runtime = import.specifiers.as_ref().is_none_or(|specifiers| {
                    specifiers.is_empty()
                        || specifiers.iter().any(|specifier| match specifier {
                            ImportDeclarationSpecifier::ImportSpecifier(specifier) => {
                                !specifier.import_kind.is_type()
                            }
                            _ => true,
                        })
                });
                if runtime {
                    imports.insert(import.source.value.to_string());
                }
            }
            Statement::ExportFromDeclaration(export) if !export.export_kind.is_type() => {
                if export
                    .specifiers
                    .iter()
                    .any(|specifier| !specifier.export_kind.is_type())
                {
                    imports.insert(export.source.value.to_string());
                }
            }
            Statement::ExportAllDeclaration(export) if !export.export_kind.is_type() => {
                imports.insert(export.source.value.to_string());
            }
            _ => {}
        }
    }
    Ok(imports)
}
fn canonical(file: &str) -> String {
    std::fs::canonicalize(file)
        .map(|file| file.to_string_lossy().into_owned())
        .unwrap_or_else(|_| file.into())
}
fn owned_environment_target(
    prepared: &BundleRequest,
    plan: &NativeBuildPlan,
    specifier: &str,
) -> Option<String> {
    // Recognize the environment contract before resolving aliases, so arbitrary
    // application aliases do not acquire framework meaning merely by spelling.
    if !plan.environment.server_modules.contains_key(specifier) {
        return None;
    }
    prepared
        .aliases
        .iter()
        .find_map(|alias| alias.resolve(specifier))
        .map(|file| file.to_string_lossy().into_owned())
}
pub(crate) fn enrich(
    input: &mut AnalysisInput,
    request: &BuildRequest,
    plan: &NativeBuildPlan,
    prepared: &BundleRequest,
    bundle: &BundleResult,
) -> Result<(), String> {
    if bundle.module_graph.is_empty() {
        return Err("Complete server module graph is required before native analysis".into());
    }
    let mut static_modules = BTreeMap::new();
    for name in ["$env/static/private", "$env/static/public"] {
        let Some(ts_svelte_kit::build_env::EnvironmentModule::Source(source)) =
            plan.environment.server_modules.get(name)
        else {
            continue;
        };
        let id = path(&graph::virtual_path(request, name))?;
        if prepared.virtual_modules.get(Path::new(&id)) != Some(source) {
            return Err(format!(
                "Generated environment snapshot differs from the actual bundle input: {name}"
            ));
        }
        if input.modules.contains_key(&id) {
            return Err(format!(
                "Application analysis snapshot overlaps framework-owned module {name}"
            ));
        }
        static_modules.insert(id, source.clone());
    }
    let mut additions = BTreeMap::new();
    for (id, snapshot) in &mut input.modules {
        let module = bundle
            .module_graph
            .get(id)
            .or_else(|| bundle.module_graph.get(&canonical(id)));
        let imports = runtime_imports(id, &snapshot.source)?;
        for specifier in imports {
            let owned = owned_environment_target(prepared, plan, &specifier);
            let supplied = snapshot.imports.get(&specifier).cloned();
            let Some(mut target) = owned.clone().or(supplied.clone()) else {
                continue;
            };
            if let (Some(expected), Some(supplied)) = (&owned, &supplied) {
                if supplied != &specifier
                    && supplied != expected
                    && canonical(supplied) != canonical(expected)
                {
                    return Err(format!(
                        "Supplied environment resolution {id} -> {specifier} conflicts with the actual generated identity"
                    ));
                }
            }
            let module = module.ok_or_else(|| {
                format!("Analysis module {id} was not present in the actual server graph")
            })?;
            let Some(resolved) = module
                .imports
                .iter()
                .find(|edge| **edge == target || **edge == canonical(&target))
            else {
                // The TypeScript transform elides imports whose bindings are
                // unused, so a supplied resolution may have no edge. Dropping
                // it is always safe: an elided import cannot affect exports,
                // and a used import stays unresolved (unknown) instead of
                // trusting a resolution the bundler did not make.
                if owned.is_none() {
                    snapshot.imports.remove(&specifier);
                    continue;
                }
                return Err(format!(
                    "Analysis resolution {id} -> {specifier} -> {target} is not an actual static graph edge"
                ));
            };
            target.clone_from(resolved);
            snapshot.imports.insert(specifier, target.clone());
            if let Some(source) = static_modules
                .get(&target)
                .or_else(|| static_modules.get(&canonical(&target)))
            {
                if bundle
                    .module_graph
                    .get(&target)
                    .is_none_or(|module| module.is_external)
                {
                    return Err(
                        "A generated static environment module was externalized instead of loaded"
                            .into(),
                    );
                }
                additions.insert(
                    target,
                    ModuleSnapshot {
                        source: source.clone(),
                        imports: BTreeMap::new(),
                    },
                );
            }
        }
    }
    for (id, snapshot) in additions {
        if let Some(existing) = input.modules.get(&id) {
            if existing.source != snapshot.source || !existing.imports.is_empty() {
                return Err(format!("Conflicting generated analysis module {id}"));
            }
        } else {
            input.modules.insert(id, snapshot);
        }
    }
    Ok(())
}
