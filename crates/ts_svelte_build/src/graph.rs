//! Bridges the framework plan to an actual native graph. All named virtual
//! modules acquire stable absolute identities without materializing files.
use crate::*;
use ts_svelte_kit::{
    build_dependencies::{EmittedAssetRecord, EmittedDependencyGraph, EmittedModuleRecord},
    build_env::EnvironmentModule,
};

pub(crate) fn virtual_path(request: &BuildRequest, id: &str) -> PathBuf {
    request
        .kit
        .inventory
        .generated_directory
        .join(".native-virtual")
        .join(format!("{}.js", sha(id.as_bytes())))
}
fn merge_source(
    sources: &mut BTreeMap<PathBuf, String>,
    file: PathBuf,
    source: String,
) -> Result<(), String> {
    if !file.is_absolute()
        || file.components().any(|part| {
            matches!(
                part,
                std::path::Component::ParentDir | std::path::Component::CurDir
            )
        })
    {
        return Err(format!(
            "Source snapshot must have a normalized absolute path: {}",
            file.display()
        ));
    }
    if let Some(previous) = sources.get(&file) {
        if previous != &source {
            return Err(format!(
                "Conflicting source snapshots at {}",
                file.display()
            ));
        }
    } else {
        sources.insert(file, source);
    }
    Ok(())
}
pub(crate) fn bundle_request(
    request: &BuildRequest,
    plan: &NativeBuildPlan,
    client: bool,
) -> Result<BundleRequest, String> {
    let graph = if client { &plan.client } else { &plan.server };
    let environment = if client {
        &plan.environment.client_modules
    } else {
        &plan.environment.server_modules
    };
    graph_request(request, plan, graph, environment, &BTreeMap::new(), false)
}

pub(crate) fn worker_bundle_request(
    request: &BuildRequest,
    plan: &NativeBuildPlan,
    worker: &ts_svelte_kit::build_service_worker::GeneratedServiceWorker,
) -> Result<BundleRequest, String> {
    if worker.graph.target != ts_svelte_kit::build_plan::BuildGraphTarget::ServiceWorker {
        return Err("Expected a service-worker graph".into());
    }
    graph_request(
        request,
        plan,
        &worker.graph,
        &plan.environment.service_worker_modules,
        &worker.files,
        true,
    )
}

fn graph_request(
    request: &BuildRequest,
    plan: &NativeBuildPlan,
    graph: &ts_svelte_kit::build_plan::BuildEntryGraph,
    environment: &BTreeMap<String, EnvironmentModule>,
    additional_generated: &BTreeMap<PathBuf, String>,
    worker: bool,
) -> Result<BundleRequest, String> {
    let client = graph.target != ts_svelte_kit::build_plan::BuildGraphTarget::Server;
    if !graph.unresolved_defines.is_empty() {
        return Err(format!(
            "Unresolved bundle defines: {:?}",
            graph.unresolved_defines.keys().collect::<Vec<_>>()
        ));
    }
    if !worker && !graph.entry_preludes.is_empty() {
        return Err("Entry prelude ordering requires a separate native bridge".into());
    }
    if graph.output.format != "esm" || (!worker && !graph.output.code_splitting) {
        return Err("Only split ESM graphs are integrated".into());
    }
    let mut sources = BTreeMap::new();
    for (file, code) in &request.sources {
        merge_source(&mut sources, file.clone(), code.clone())?;
    }
    for (file, module) in &request.modules {
        merge_source(&mut sources, file.into(), module.source.clone())?;
    }
    let mut generated = plan.generated_files.clone();
    generated.extend(
        additional_generated
            .iter()
            .map(|(file, code)| (file.clone(), code.clone())),
    );
    for (file, code) in &generated {
        if sources.contains_key(file) {
            return Err(format!(
                "Application snapshot overlaps generated file {}",
                file.display()
            ));
        }
        merge_source(&mut sources, file.clone(), code.clone())?;
    }
    let mut aliases = Vec::new();
    let mut virtual_ids = BTreeMap::new();
    let mut add_virtual = |id: &str, code: &str| -> Result<(), String> {
        let file = virtual_path(request, id);
        merge_source(&mut sources, file.clone(), code.into())?;
        aliases.push(json!({"find":id,"replacement":file,"matching":"exact"}));
        virtual_ids.insert(id.to_owned(), file);
        Ok(())
    };
    for (id, source) in &graph.virtual_modules {
        add_virtual(id, source)?;
    }
    for (id, module) in environment {
        if let EnvironmentModule::Source(source) = module {
            add_virtual(id, source)?;
        }
    }
    drop(add_virtual);
    for (id, module) in environment {
        if let EnvironmentModule::File(file) = module {
            aliases.push(json!({"find":id,"replacement":file,"matching":"exact"}));
            virtual_ids.insert(id.clone(), file.clone());
        }
    }
    for alias in &graph.aliases {
        let mut replacement = alias.replacement.clone();
        // The first server graph uses Kit's conservative client manifest. Its
        // optimized counterpart cannot exist until this graph has been analyzed.
        if !client && alias.name == "__sveltekit/manifest" && !sources.contains_key(&replacement) {
            let initial = request
                .kit
                .inventory
                .generated_directory
                .join("client/app.js");
            if sources.contains_key(&initial) {
                replacement = initial;
            } else {
                return Err("Missing conservative server route manifest".into());
            }
        }
        aliases
            .push(json!({"find":alias.name,"replacement":replacement,"matching":alias.matching}));
    }
    let entries = graph
        .entries
        .iter()
        .map(|(name, id)| {
            let file = virtual_ids
                .get(id)
                .cloned()
                .unwrap_or_else(|| PathBuf::from(id));
            if !file.is_absolute() {
                return Err(format!("Unresolved entry {id:?}"));
            }
            Ok((name.clone(), file))
        })
        .collect::<Result<BTreeMap<_, _>, String>>()?;
    // An external and a forced bundled dependency cannot both be honored.
    for external in &graph.external {
        if graph
            .must_bundle
            .iter()
            .any(|required| external == required || external.starts_with(&format!("{required}/")))
        {
            return Err(format!(
                "External {external} conflicts with required framework bundling"
            ));
        }
    }
    let mut prepared: BundleRequest = serde_json::from_value(json!({
        "root":request.kit.inventory.project_root,"entries":entries,"target":if client {"client"} else {"server"},
        "strictExecutionOrder":if worker {Some(true)} else {None},
        "compatibility":{"svelteVersion":request.compiler.svelte_version,"kitVersion":request.kit.inventory.kit_version,"typescriptVersion":request.compiler.typescript_version,"allowPrerelease":request.compiler.allow_prerelease},
        "runes":request.compiler.runes,"minify":request.compiler.minify,"sourcemap":request.compiler.sourcemap,
        "aliases":aliases,"external":graph.external,"conditions":graph.conditions,"define":graph.defines,
        "tsconfig":request.compiler.tsconfig,"virtualModules":sources,"compilerOptions":request.compiler.options,
        "mdsvex":request.compiler.mdsvex,"typescript":request.compiler.typescript,
        "css":if worker {Value::Null} else {json!({"publicBase":if plan.relative_asset_urls {None} else {Some(plan.asset_url_base.clone())},"nonce":null,"tailwind":request.tailwind})},
        "output":{"entryFileNames":graph.output.entries,"chunkFileNames":graph.output.chunks,"assetFileNames":graph.output.assets,
            "preserveEntrySignatures":graph.output.preserve_entry_signatures,"hoistTransitiveImports":graph.output.hoist_transitive_imports,"target":graph.javascript_target,"codeSplitting":graph.output.code_splitting}
    })).map_err(|error|format!("Native bundle contract: {error}"))?;
    let mut fixed_chunks = BTreeMap::<String, Vec<String>>::new();
    for fixed in &graph.output.fixed_chunks {
        let resolved = prepared
            .aliases
            .iter()
            .find_map(|alias| alias.resolve(&fixed.module))
            .unwrap_or_else(|| PathBuf::from(&fixed.module));
        if !resolved.is_absolute() {
            return Err(format!("Unresolved fixed chunk module {:?}", fixed.module));
        }
        // Snapshot modules retain their exact loader identity. Installed modules
        // use the same native canonical path identity as the bundler resolver.
        let resolved = if prepared.virtual_modules.contains_key(&resolved) {
            resolved
        } else {
            match std::fs::canonicalize(&resolved) {
                Ok(canonical) => canonical,
                Err(error) => {
                    return Err(format!(
                        "Cannot resolve fixed chunk module {}: {error}",
                        resolved.display()
                    ));
                }
            }
        };
        fixed_chunks
            .entry(fixed.file.clone())
            .or_default()
            .push(path(&resolved)?);
    }
    prepared.output.fixed_chunks = fixed_chunks;
    if !graph.entry_preludes.is_empty() {
        // An ESM facade keeps bootstrap evaluation before the worker dependency
        // subtree without shifting the original worker's source/map coordinates.
        // Only the generator's side-effect imports are a supported contract.
        let mut imports = String::new();
        for prelude in &graph.entry_preludes {
            let arena = oxc_allocator::Allocator::default();
            let parsed =
                oxc_parser::Parser::new(&arena, prelude, oxc_span::SourceType::mjs()).parse();
            if !parsed.diagnostics.is_empty() || parsed.program.body.len() != 1 {
                return Err("Worker prelude must be one resolved side-effect import".into());
            }
            let oxc_ast::ast::Statement::ImportDeclaration(import) = &parsed.program.body[0] else {
                return Err("Worker prelude must be a side-effect import".into());
            };
            if import.specifiers.is_some()
                || import.with_clause.is_some()
                || import.phase.is_some()
                || import.import_kind.is_type()
            {
                return Err("Worker prelude has unsupported bindings or import attributes".into());
            }
            imports.push_str(&format!(
                "import {};\n",
                serde_json::to_string(import.source.value.as_str()).map_err(|e| e.to_string())?
            ));
        }
        for (name, entry) in &mut prepared.entries {
            let facade = virtual_path(request, &format!("service-worker-entry:{name}"));
            let code = format!(
                "{imports}export * from {};\n",
                serde_json::to_string(&path(entry)?).map_err(|e| e.to_string())?
            );
            merge_source(&mut prepared.virtual_modules, facade.clone(), code)?;
            *entry = facade;
        }
    }
    Ok(prepared)
}

pub(crate) fn dependencies(
    request: &BuildRequest,
    bundle: &BundleResult,
) -> Result<EmittedDependencyGraph, String> {
    let mut graph = dependency_records(
        bundle,
        &request.kit.inventory.kit_version,
        &request.kit.inventory.project_root,
        &path(&virtual_path(request, "$env/dynamic/public"))?,
        &path(&virtual_path(request, "__sveltekit/env/public/client"))?,
    )?;
    // The resolver canonicalizes installed modules (including symlinked package
    // stores). Record actual filesystem observations for inventory spellings.
    let inventory = &request.kit.inventory;
    let mut files: BTreeSet<PathBuf> = request.modules.keys().map(PathBuf::from).collect();
    files.extend(request.sources.keys().cloned());
    for node in &inventory.nodes {
        files.extend(
            [&node.component, &node.universal, &node.server]
                .into_iter()
                .flatten()
                .cloned(),
        );
    }
    files.extend(
        inventory
            .routes
            .iter()
            .filter_map(|route| route.endpoint.clone()),
    );
    for directory in [&inventory.project_root, &inventory.runtime_directory] {
        if let Ok(canonical) = std::fs::canonicalize(directory) {
            for chunk in &bundle.chunks {
                for id in chunk.module_ids.iter().chain(chunk.facade_module_id.iter()) {
                    if let Ok(relative) = Path::new(id).strip_prefix(&canonical) {
                        let original = directory.join(relative);
                        // A physical identity observation, not a textual alias
                        // guess: the re-spelled path must resolve to this module.
                        if std::fs::canonicalize(&original).ok().as_deref() == Some(Path::new(id)) {
                            files.insert(original);
                        }
                    }
                }
            }
        }
    }
    for file in files {
        if request.sources.contains_key(&file) || request.modules.contains_key(&path(&file)?) {
            continue;
        }
        let canonical = match std::fs::canonicalize(&file) {
            Ok(file) => Some(file),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => {
                return Err(format!(
                    "Cannot resolve dependency identity {}: {error}",
                    file.display()
                ));
            }
        };
        graph.real_paths.insert(file, canonical);
    }
    Ok(graph)
}
fn dependency_records(
    bundle: &BundleResult,
    version: &str,
    root: &Path,
    dynamic_public: &str,
    explicit_public: &str,
) -> Result<EmittedDependencyGraph, String> {
    let keys: BTreeMap<_, _> = bundle
        .chunks
        .iter()
        .map(|c| {
            (
                c.file_name.clone(),
                c.facade_module_id
                    .clone()
                    .unwrap_or_else(|| c.file_name.clone()),
            )
        })
        .collect();
    if keys.len() != bundle.chunks.len() {
        return Err("Duplicate emitted chunk filenames".into());
    }
    let mut records = BTreeMap::new();
    let mut aliases = BTreeMap::new();
    for chunk in &bundle.chunks {
        let key = keys[&chunk.file_name].clone();
        aliases.insert(chunk.file_name.clone(), key.clone());
        let imports = chunk
            .imports
            .iter()
            .filter_map(|id| keys.get(id).cloned())
            .collect();
        let dynamic_imports = chunk
            .dynamic_imports
            .iter()
            .filter_map(|id| keys.get(id).cloned())
            .collect();
        let external_imports = chunk
            .imports
            .iter()
            .chain(&chunk.dynamic_imports)
            .filter(|id| !keys.contains_key(*id))
            .cloned()
            .collect();
        let mut modules = chunk.module_ids.clone();
        if modules.iter().any(|id| id == dynamic_public) {
            modules.push("\0virtual:env/dynamic/public".into());
        }
        if modules.iter().any(|id| id == explicit_public) {
            modules.push("\0virtual:__sveltekit/env/public/client".into());
        }
        let css = bundle
            .css
            .chunks
            .get(&chunk.file_name)
            .map(|metadata| metadata.own.clone());
        let mut assets = Vec::new();
        for module in bundle.css.modules.values() {
            if chunk.module_ids.contains(&module.module_id)
                || css
                    .as_ref()
                    .is_some_and(|files| files.contains(&module.file_name))
            {
                for asset in &module.assets {
                    if !assets.contains(asset) {
                        assets.push(asset.clone());
                    }
                }
            }
        }
        let record = EmittedModuleRecord {
            file: chunk.file_name.clone(),
            source: chunk.facade_module_id.clone(),
            facade: chunk.facade_module_id.clone(),
            modules,
            imports,
            dynamic_imports,
            external_imports,
            css,
            assets: Some(assets),
        };
        if records.insert(key, record).is_some() {
            return Err("Duplicate emitted facade identity".into());
        }
    }
    // CSS and sourcemap originalFileNames describe provenance, not additional
    // module entries. A component commonly emits JS, CSS and a CSS map sharing
    // one source path. Keep that provenance in the asset catalog without
    // competing with the component's facade/source identity.
    let source_maps: BTreeSet<_> = bundle
        .chunks
        .iter()
        .filter_map(|chunk| chunk.sourcemap_file_name.as_deref())
        .chain(
            bundle
                .css
                .modules
                .values()
                .filter_map(|module| module.map_file_name.as_deref()),
        )
        .collect();
    let mut assets = vec![];
    for asset in &bundle.assets {
        let css_source = if asset.file_name.ends_with(".css") {
            Some(String::from_utf8(asset_bytes(asset)?).map_err(|_| "CSS output is not UTF-8")?)
        } else {
            None
        };
        assets.push(EmittedAssetRecord {
            file: asset.file_name.clone(),
            original_names: asset.original_file_names.clone(),
            css_source,
        });
        if asset.file_name.ends_with(".css") || source_maps.contains(asset.file_name.as_str()) {
            continue;
        }
        for original in &asset.original_file_names {
            let record = EmittedModuleRecord {
                file: asset.file_name.clone(),
                source: Some(original.clone()),
                facade: None,
                modules: vec![],
                imports: vec![],
                dynamic_imports: vec![],
                external_imports: vec![],
                css: None,
                assets: None,
            };
            if let Some(old) = records.get(original) {
                if old.file != record.file {
                    return Err(format!("Conflicting emitted asset source {original}"));
                }
            } else {
                records.insert(original.clone(), record);
            }
        }
    }
    Ok(EmittedDependencyGraph {
        kit_version: version.into(),
        project_root: root.into(),
        records,
        aliases,
        real_paths: BTreeMap::new(),
        assets,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn source_overlay_conflicts_are_atomic() {
        let mut map = BTreeMap::new();
        merge_source(&mut map, "/project/a.js".into(), "one".into()).unwrap();
        assert!(merge_source(&mut map, "/project/a.js".into(), "two".into()).is_err());
        assert_eq!(map[Path::new("/project/a.js")], "one");
        assert!(merge_source(&mut map, "../a.js".into(), "x".into()).is_err());
    }
    fn chunk(
        file: &str,
        facade: Option<&str>,
        imports: &[&str],
        dynamic: &[&str],
    ) -> ts_svelte_bundle::Chunk {
        ts_svelte_bundle::Chunk {
            referenced_files: vec![],
            file_name: file.into(),
            name: file.into(),
            code: String::new(),
            is_entry: facade.is_some(),
            is_dynamic_entry: false,
            facade_module_id: facade.map(str::to_owned),
            module_ids: facade.into_iter().map(str::to_owned).collect(),
            imports: imports.iter().map(|s| (*s).into()).collect(),
            dynamic_imports: dynamic.iter().map(|s| (*s).into()).collect(),
            exports: vec![],
            map: None,
            sourcemap_file_name: None,
        }
    }
    fn bundle() -> BundleResult {
        BundleResult {
            rolldown_version: "1.2.13",
            target: ts_svelte_bundle::Target::Client,
            css_mode: "extracted",
            css: Default::default(),
            module_graph: BTreeMap::new(),
            chunks: vec![],
            assets: vec![],
            diagnostics: vec![],
            compiler_responses: BTreeMap::new(),
        }
    }
    #[test]
    fn dependency_adapter_preserves_dynamic_groups_and_empty_css_metadata() {
        let mut bundle = bundle();
        bundle.chunks = vec![
            chunk(
                "entry.js",
                Some("/project/page.js"),
                &["shared.js", "node:fs"],
                &["lazy.js"],
            ),
            chunk("shared.js", None, &[], &[]),
            chunk("lazy.js", Some("/project/lazy.js"), &[], &[]),
        ];
        bundle
            .css
            .chunks
            .insert("entry.js".into(), Default::default());
        let graph = dependency_records(
            &bundle,
            "2.69.2",
            Path::new("/project"),
            "/env.js",
            "/explicit.js",
        )
        .unwrap();
        let entry = &graph.records["/project/page.js"];
        assert_eq!(entry.imports, ["shared.js"]);
        assert_eq!(entry.dynamic_imports, ["/project/lazy.js"]);
        assert_eq!(entry.external_imports, ["node:fs"]);
        assert_eq!(entry.css, Some(vec![]));
        assert_eq!(graph.records["shared.js"].css, None);
        let graph =
            ts_svelte_kit::build_dependencies::PreparedDependencyGraph::new(&graph).unwrap();
        let dependencies = graph.find_dependencies("/project/page.js", true).unwrap();
        assert!(dependencies.imports.contains(&"entry.js".into()));
        assert!(dependencies.imports.contains(&"shared.js".into()));
        assert!(!dependencies.imports.contains(&"lazy.js".into()));
    }
    #[test]
    fn dependency_adapter_retains_css_url_asset_identity_and_env_usage() {
        let mut bundle = bundle();
        let mut entry = chunk("entry.js", Some("/project/page.svelte"), &[], &[]);
        entry.sourcemap_file_name = Some("entry.js.map".into());
        entry.module_ids.push("/env.js".into());
        bundle.chunks.push(entry);
        bundle.css.chunks.insert(
            "entry.js".into(),
            ts_svelte_bundle::css::CssChunkOutput {
                own: vec!["assets/page.css".into()],
                ..Default::default()
            },
        );
        bundle.css.modules.insert(
            "virtual-style".into(),
            ts_svelte_bundle::css::CssModuleOutput {
                tailwind: None,
                module_id: "virtual-style".into(),
                origin: "/project/page.svelte".into(),
                file_name: "assets/page.css".into(),
                map_file_name: Some("assets/page.css.map".into()),
                sources: vec![],
                assets: vec!["assets/font.woff2".into()],
                exports: json!({}),
                dependencies: vec![],
            },
        );
        for (name, source, original) in [
            (
                "assets/page.css",
                b"p{color:red}".as_slice(),
                "/project/page.svelte",
            ),
            (
                "assets/font.woff2",
                b"font".as_slice(),
                "/project/font.woff2",
            ),
            (
                "assets/page.css.map",
                b"{}".as_slice(),
                "/project/page.svelte",
            ),
            ("entry.js.map", b"{}".as_slice(), "/project/page.svelte"),
        ] {
            bundle.assets.push(ts_svelte_bundle::Asset {
                file_name: name.into(),
                names: vec![],
                original_file_names: vec![original.into()],
                encoding: "base64",
                source: base64::engine::general_purpose::STANDARD.encode(source),
            });
        }
        let graph = dependency_records(
            &bundle,
            "2.69.2",
            Path::new("/project"),
            "/env.js",
            "/explicit.js",
        )
        .unwrap();
        assert_eq!(
            graph.records["/project/font.woff2"].file,
            "assets/font.woff2"
        );
        assert_eq!(graph.records["/project/page.svelte"].file, "entry.js");
        for file in ["assets/page.css", "assets/page.css.map", "entry.js.map"] {
            assert_eq!(
                graph
                    .assets
                    .iter()
                    .find(|asset| asset.file == file)
                    .unwrap()
                    .original_names,
                ["/project/page.svelte"]
            );
        }
        assert!(
            graph.records["/project/page.svelte"]
                .modules
                .contains(&"\0virtual:env/dynamic/public".into())
        );
        let graph =
            ts_svelte_kit::build_dependencies::PreparedDependencyGraph::new(&graph).unwrap();
        let dependencies = graph
            .find_dependencies("/project/page.svelte", true)
            .unwrap();
        assert_eq!(dependencies.stylesheets, ["assets/page.css"]);
        assert_eq!(
            dependencies.fonts,
            [ts_svelte_kit::build_dependencies::DependencyFont::Kit2(
                "assets/font.woff2".into()
            )]
        );
    }
}
