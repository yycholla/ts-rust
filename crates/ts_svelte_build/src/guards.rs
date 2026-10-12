//! Complete framework guard bridge. External exposure is explicit evidence;
//! generated CSS virtual modules retain their actual graph and are never
//! exempted from traversal or misclassified merely because they lack a path.
use crate::*;
use ts_svelte_kit::build_guards::{
    self, BuildGuardInput, BuildGuardReport, ExternalExposure, GuardExternal, GuardStatus,
    GuardTarget, LoadedGuardModule,
};
use ts_svelte_kit::build_plan::{AliasMatch, BuildAlias};

fn observe(paths: &mut BTreeMap<PathBuf, PathBuf>, path: &Path) -> Result<(), String> {
    if !path.is_absolute() || path.to_string_lossy().contains('\0') {
        return Ok(());
    }
    match std::fs::canonicalize(path) {
        Ok(canonical) => {
            if let Some(previous) = paths.get(path) {
                if previous != &canonical {
                    return Err(format!("Stale realpath observation for {}", path.display()));
                }
            }
            paths.insert(path.to_owned(), canonical);
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(format!(
                "Cannot establish module identity {}: {error}",
                path.display()
            ));
        }
    }
    Ok(())
}
pub(crate) fn validate(
    bundle: &BundleResult,
    prepared: &BundleRequest,
    plan: &NativeBuildPlan,
    request: &BuildRequest,
) -> Result<BuildGuardReport, String> {
    validate_for_target(
        bundle,
        prepared,
        plan,
        request,
        match prepared.target {
            ts_svelte_bundle::Target::Server => GuardTarget::Server,
            ts_svelte_bundle::Target::Client => GuardTarget::Client,
        },
    )
}

pub(crate) fn validate_for_target(
    bundle: &BundleResult,
    prepared: &BundleRequest,
    plan: &NativeBuildPlan,
    request: &BuildRequest,
    target: GuardTarget,
) -> Result<BuildGuardReport, String> {
    let aliases = prepared
        .aliases
        .iter()
        .map(|alias| BuildAlias {
            name: alias.find.clone(),
            replacement: alias.replacement.clone(),
            matching: match alias.matching {
                ts_svelte_bundle::AliasMatch::Exact => AliasMatch::Exact,
                ts_svelte_bundle::AliasMatch::Prefix => AliasMatch::Prefix,
                ts_svelte_bundle::AliasMatch::Children => AliasMatch::Children,
            },
        })
        .collect::<Vec<_>>();
    let mut real_paths = request.guard_real_paths.clone();
    for file in request
        .modules
        .keys()
        .map(Path::new)
        .chain(request.sources.keys().map(PathBuf::as_path))
        .chain(aliases.iter().map(|alias| alias.replacement.as_path()))
        .chain(bundle.module_graph.keys().map(Path::new))
    {
        observe(&mut real_paths, file)?;
    }
    let roots = [
        &request.kit.inventory.project_root,
        &request.kit.inventory.runtime_directory,
        &plan.guards.lib_directory,
        &plan.guards.routes_directory,
        &plan.guards.assets_directory,
    ];
    for root in roots {
        observe(&mut real_paths, root)?;
    }
    // A directory symlink can hide the original $lib/server ancestry after
    // resolution. Recover only physical counterparts whose realpath is observed
    // to match, never infer a private path merely from a textual suffix.
    let directory_aliases = aliases
        .iter()
        .filter(|alias| alias.matching != AliasMatch::Exact)
        .map(|alias| alias.replacement.clone())
        .chain(roots.into_iter().cloned())
        .collect::<BTreeSet<_>>();
    let mut candidates = BTreeSet::new();
    for original in directory_aliases {
        let Some(canonical) = real_paths.get(&original) else {
            continue;
        };
        if canonical == &original {
            continue;
        }
        for id in bundle.module_graph.keys() {
            if let Ok(relative) = Path::new(id).strip_prefix(canonical) {
                candidates.insert(original.join(relative));
            }
        }
    }
    for candidate in candidates {
        observe(&mut real_paths, &candidate)?;
    }
    let mut externals = vec![];
    for module in bundle
        .module_graph
        .values()
        .filter(|module| module.is_external)
    {
        let exposure = if module.id.starts_with("node:") {
            if matches!(
                request.external_exposure.get(&module.id),
                Some(ExternalExposure::Public { .. })
            ) {
                return Err(format!(
                    "Node builtin {} cannot be classified as a public browser external; resolve an actual browser replacement",
                    module.id
                ));
            }
            ExternalExposure::ServerOnly
        } else {
            request
                .external_exposure
                .get(&module.id)
                .cloned()
                .unwrap_or(ExternalExposure::Unknown)
        };
        externals.push(GuardExternal {
            id: module.id.clone(),
            exposure,
        });
    }
    let mut additional = plan.guards.server_only_ids.clone();
    for id in &plan.guards.server_only_ids {
        if matches!(
            plan.environment.server_modules.get(id),
            Some(ts_svelte_kit::build_env::EnvironmentModule::Source(_))
        ) || plan.server.virtual_modules.contains_key(id)
        {
            additional.insert(path(&graph::virtual_path(request, id))?);
        }
    }
    build_guards::validate_build_imports(&BuildGuardInput {
        kit_version: request.kit.inventory.kit_version.clone(),
        target,
        project_root: request.kit.inventory.project_root.clone(),
        runtime_directory: request.kit.inventory.runtime_directory.clone(),
        lib_directory: plan.guards.lib_directory.clone(),
        routes_directory: plan.guards.routes_directory.clone(),
        assets_directory: plan.guards.assets_directory.clone(),
        aliases,
        real_paths,
        modules: bundle
            .module_graph
            .values()
            .map(|module| LoadedGuardModule {
                id: module.id.clone(),
                is_entry: module.is_entry,
                is_external: module.is_external,
                imports: module.imports.clone(),
                dynamic_imports: module.dynamic_imports.clone(),
                importers: module.importers.clone(),
                dynamic_importers: module.dynamic_importers.clone(),
                exports: module.exports.clone(),
            })
            .collect(),
        externals,
        resolutions: request
            .guard_resolutions
            .iter()
            .filter(|resolution| bundle.module_graph.contains_key(&resolution.importer))
            .cloned()
            .collect(),
        entrypoints: vec![],
        additional_server_only_ids: additional,
        module_extensions: request.module_extensions.clone(),
        remote_functions_enabled: Some(request.kit.config.remote_functions_enabled),
        explicit_environment_variables: Some(
            request.kit.environment.explicit_environment_variables,
        ),
        runtime_features: None,
    })
}
pub(crate) fn record(
    report: &mut BuildReport,
    target: &str,
    result: Result<BuildGuardReport, String>,
) -> bool {
    let stage = format!("{target} guards");
    let guard = match result {
        Ok(guard) => guard,
        Err(error) => {
            report.fail(&stage, error);
            return false;
        }
    };
    let mut failed = false;
    let mut incomplete = false;
    for diagnostic in guard
        .import_diagnostics
        .iter()
        .chain(&guard.configuration_diagnostics)
    {
        report.diagnostics.push(json!({"source":"native-kit-guard","stage":stage,"severity":"error","detail":diagnostic}));
    }
    for unknown in guard
        .import_unknowns
        .iter()
        .chain(&guard.configuration_unknowns)
    {
        report.requirement(&stage, &unknown.code, &unknown.message);
    }
    for status in [guard.imports_status, guard.configuration_status] {
        match status {
            GuardStatus::Passed => {}
            GuardStatus::Failed => failed = true,
            GuardStatus::Incomplete | GuardStatus::NotRequested => incomplete = true,
        }
    }
    report.guards.insert(target.into(), guard);
    if failed {
        report.fail(&stage, "Framework import/configuration guards failed");
        false
    } else if incomplete {
        report.phase(&stage, "incomplete", 0);
        false
    } else {
        report.phase(&stage, "complete", 0);
        true
    }
}
