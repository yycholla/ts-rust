//! Native adapter-node orchestration over immutable emitted artifacts.
//! Runtime templates are authenticated by the Kit adapter planner. No Node,
//! configuration, application code, or generated deployment code is executed.
use crate::{Artifact, BuildRequest, artifacts};
use base64::Engine as _;
use oxc_allocator::Allocator;
use oxc_parser::Parser;
use oxc_span::SourceType;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use ts_svelte_bundle::{BundleRequest, BundleResult};
use ts_svelte_kit::adapter_node::{self, Files, PackagingPlan, Prerendered};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AdapterRequest {
    pub templates: adapter_node::RuntimeTemplates,
    #[serde(default)]
    pub options: adapter_node::Options,
    pub package_json: Value,
    /// Absolute virtual output directory used by the original server emitter
    /// when making sourcemap sources relative. This is not the relocated
    /// BuildReport artifact directory. The native bundler currently uses root/dist.
    pub server_map_base: PathBuf,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdapterOutput {
    /// Deployment paths relative to the project root, including options.out.
    pub artifacts: BTreeMap<String, Artifact>,
    pub bundle: BundleResult,
    pub overwritten_assets: Vec<String>,
    pub instrumentation: Option<InstrumentationOutput>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstrumentationOutput {
    pub identities: BTreeMap<String, crate::instrumentation::ArtifactIdentity>,
    pub entrypoint: String,
    pub start: String,
    pub static_preludes: Vec<String>,
    pub dynamic_start: String,
    pub map_copy: Option<ts_svelte_kit::build_instrumentation::InstrumentationMapCopy>,
}

fn normalized(path: &Path) -> Result<PathBuf, String> {
    if !path.is_absolute() || path.to_str().is_none_or(|path| path.contains('\0')) {
        return Err("Adapter source identities require absolute UTF-8 paths".into());
    }
    let mut output = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !output.pop() {
                    return Err("Path escapes root".into());
                }
            }
            component => output.push(component.as_os_str()),
        }
    }
    Ok(output)
}

pub(crate) fn absolute_sources(map: &mut Value, original_map: &Path) -> Result<(), String> {
    let object = map.as_object_mut().ok_or("Expected a sourcemap object")?;
    if object.get("version") != Some(&json!(3))
        || object.contains_key("sections")
        || !object.get("names").is_some_and(Value::is_array)
        || !object.get("mappings").is_some_and(Value::is_string)
    {
        return Err("Adapter rebundling requires a flat version 3 source map".into());
    }
    let source_root = match object.remove("sourceRoot") {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(root)) => root,
        _ => return Err("Invalid sourceRoot in adapter input map".into()),
    };
    let directory = original_map.parent().ok_or("Map has no parent directory")?;
    let sources = object
        .get_mut("sources")
        .and_then(Value::as_array_mut)
        .ok_or("Source map sources must be an array")?;
    for source in sources {
        let name = source
            .as_str()
            .ok_or("Source map sources must be strings")?;
        if name.contains("://") || name.starts_with("data:") || name.starts_with('\0') {
            continue;
        }
        if source_root.contains("://") {
            return Err(
                "Relative sources under a URL sourceRoot need an explicit resolved source identity"
                    .into(),
            );
        }
        let absolute = normalized(&directory.join(&source_root).join(name))?;
        *source = Value::String(absolute.to_string_lossy().into_owned());
    }
    Ok(())
}

/// Extract only real sourcemap comments, never text inside JS strings. Blanking
/// preserves every generated coordinate before passing the map to the loader.
fn module_source(
    file: &str,
    bytes: &[u8],
    staging: &Files,
    staging_directory: &str,
    map_base: &Path,
) -> Result<(String, Option<Value>), String> {
    let code = std::str::from_utf8(bytes).map_err(|_| format!("Module {file} is not UTF-8"))?;
    if file.ends_with(".json") {
        return Ok((code.into(), None));
    }
    let allocator = Allocator::default();
    let parsed = Parser::new(
        &allocator,
        code,
        SourceType::from_path(file).map_err(|error| error.to_string())?,
    )
    .parse();
    if let Some(error) = parsed.diagnostics.first() {
        return Err(format!("Invalid adapter module {file}: {error:?}"));
    }
    let mut references = Vec::new();
    for comment in &parsed.program.comments {
        let text =
            code[comment.content_span().start as usize..comment.content_span().end as usize].trim();
        if let Some(url) = text
            .strip_prefix("# sourceMappingURL=")
            .or_else(|| text.strip_prefix("@ sourceMappingURL="))
        {
            references.push((comment.span, url.trim()));
        }
    }
    if references.len() > 1 {
        return Err(format!("Multiple sourceMappingURL comments in {file}"));
    }
    let sibling = format!("{file}.map");
    let map_identity;
    let mut map = if let Some((_, url)) = references.first() {
        if let Some(payload) = url.strip_prefix("data:application/json;base64,") {
            map_identity = file.to_owned();
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(payload)
                .map_err(|e| e.to_string())?;
            Some(
                serde_json::from_slice(&bytes)
                    .map_err(|e| format!("Invalid inline source map: {e}"))?,
            )
        } else {
            artifacts::relative(url)?;
            map_identity = Path::new(file)
                .parent()
                .unwrap()
                .join(url)
                .to_string_lossy()
                .into_owned();
            let bytes = staging
                .get(&map_identity)
                .ok_or_else(|| format!("Missing referenced sourcemap {map_identity}"))?;
            Some(
                serde_json::from_slice(bytes)
                    .map_err(|e| format!("Invalid source map {map_identity}: {e}"))?,
            )
        }
    } else if let Some(bytes) = staging.get(&sibling) {
        map_identity = sibling;
        Some(
            serde_json::from_slice(bytes)
                .map_err(|e| format!("Invalid source map {map_identity}: {e}"))?,
        )
    } else {
        map_identity = file.to_owned();
        None
    };
    if let Some(map) = &mut map {
        let relative = map_identity
            .strip_prefix(&format!("{staging_directory}/"))
            .ok_or("Sourcemap escapes adapter staging identity")?;
        absolute_sources(map, &map_base.join(relative))?;
    }
    let mut output = code.to_owned();
    for (span, _) in references.into_iter().rev() {
        let text = &code[span.start as usize..span.end as usize];
        let mut blank = String::new();
        for character in text.chars() {
            if matches!(character, '\r' | '\n') {
                blank.push(character);
            } else {
                blank.extend(std::iter::repeat_n(' ', character.len_utf16()));
            }
        }
        output.replace_range(span.start as usize..span.end as usize, &blank);
    }
    Ok((output, map))
}

fn request_for_plan(
    root: &Path,
    compatibility: Value,
    plan: &PackagingPlan,
    staging_directory: &str,
    map_base: &Path,
    minified: bool,
) -> Result<BundleRequest, String> {
    if plan.bundle.format != "esm"
        || !plan.bundle.sourcemap
        || !plan.bundle.prefer_builtins
        || !plan.bundle.strict_commonjs_requires
        || !plan.bundle.json_modules
    {
        return Err("Unsupported adapter-node native rebundle contract".into());
    }
    normalized(root)?;
    normalized(map_base)?;
    let mut modules = BTreeMap::new();
    let mut maps = BTreeMap::new();
    for (file, bytes) in &plan.staging_files {
        if ![".js", ".mjs", ".cjs", ".json"]
            .iter()
            .any(|extension| file.ends_with(extension))
        {
            continue;
        }
        let (source, map) = module_source(
            file,
            bytes,
            &plan.staging_files,
            staging_directory,
            map_base,
        )?;
        let file = root.join(file);
        if let Some(map) = map {
            maps.insert(file.clone(), map);
        }
        modules.insert(file, source);
    }
    serde_json::from_value(json!({
        "root":root,"entries":plan.bundle.entries.iter().map(|(name,file)|(name,root.join(file))).collect::<BTreeMap<_,_>>(),
        "target":"server","compatibility":compatibility,"nodeRebundle":true,
        // Rollup leaves its input text as is: minified server code stays minified.
        "sourcemap":true,"minify":minified,"externalPackages":plan.bundle.production_dependencies.keys().collect::<Vec<_>>(),
        "conditions":plan.bundle.export_conditions,"virtualModules":modules,"virtualModuleMaps":maps,
        "output":{"entryFileNames":"[name].js","chunkFileNames":plan.bundle.chunk_file_names,"preserveEntrySignatures":"strict","hoistTransitiveImports":false}
    })).map_err(|error|format!("Native adapter rebundle contract: {error}"))
}

fn compressed(
    plan: &PackagingPlan,
) -> Result<BTreeMap<String, adapter_node::CompressionOutput>, String> {
    // The codec bounds each batch (jobs, sources, input, repeated work and
    // output). Split by source, keeping each source's jobs together; output is
    // budgeted at a margin below the cap since compressed text rarely grows.
    use ts_svelte_compress::{
        MAX_BATCH_INPUT_BYTES, MAX_BATCH_WORK_BYTES, MAX_JOBS, MAX_OUTPUT_BYTES,
    };
    // At least one batch per available core, so batches compress in parallel.
    let threads = std::thread::available_parallelism().map_or(1, usize::from);
    let total_work: usize = plan
        .compression
        .iter()
        .map(|job| plan.files.get(&job.source).map_or(0, Vec::len))
        .sum();
    let work_cap = MAX_BATCH_WORK_BYTES
        .min(MAX_OUTPUT_BYTES / 10 * 9)
        .min(total_work.div_ceil(threads).max(1));
    let mut by_source: BTreeMap<&str, Vec<ts_svelte_compress::Job>> = BTreeMap::new();
    for job in &plan.compression {
        if !plan.files.contains_key(&job.source) {
            return Err(format!("Missing compression source {}", job.source));
        }
        let compression = serde_json::from_value(
            serde_json::to_value(&job.compression).map_err(|e| e.to_string())?,
        )
        .map_err(|e| format!("Compression options: {e}"))?;
        by_source
            .entry(&job.source)
            .or_default()
            .push(ts_svelte_compress::Job {
                source: job.source.clone(),
                destination: job.destination.clone(),
                source_sha256: job.source_sha256.clone(),
                compression,
            });
    }
    let mut batches = vec![ts_svelte_compress::Batch {
        sources: BTreeMap::new(),
        jobs: vec![],
    }];
    let (mut input, mut work) = (0usize, 0usize);
    for (source, jobs) in by_source {
        let bytes = &plan.files[source];
        let job_work = bytes.len() * jobs.len();
        let current = batches.last().expect("one batch");
        if !current.jobs.is_empty()
            && (current.jobs.len() + jobs.len() > MAX_JOBS
                || current.sources.len() + 1 > MAX_JOBS
                || input + bytes.len() > MAX_BATCH_INPUT_BYTES
                || work + job_work > work_cap)
        {
            batches.push(ts_svelte_compress::Batch {
                sources: BTreeMap::new(),
                jobs: vec![],
            });
            (input, work) = (0, 0);
        }
        let current = batches.last_mut().expect("one batch");
        current.sources.insert(source.to_owned(), bytes.clone());
        current.jobs.extend(jobs);
        input += bytes.len();
        work += job_work;
    }
    let next = std::sync::atomic::AtomicUsize::new(0);
    let results = std::sync::Mutex::new(Vec::new());
    std::thread::scope(|scope| {
        for _ in 0..threads.min(batches.len()) {
            scope.spawn(|| {
                loop {
                    let index = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let Some(batch) = batches.get(index) else {
                        break;
                    };
                    let result = ts_svelte_compress::compress_batch(batch);
                    results
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .push((index, result));
                }
            });
        }
    });
    let mut results = results.into_inner().unwrap_or_else(|e| e.into_inner());
    results.sort_by_key(|(index, _)| *index);
    let mut outputs = BTreeMap::new();
    for (_, result) in results {
        for (file, output) in result.map_err(|e| e.to_string())? {
            if outputs.insert(file.clone(), output).is_some() {
                return Err(format!("Duplicate compression output {file}"));
            }
        }
    }
    outputs
        .into_iter()
        .map(|(file, output)| {
            Ok((
                file,
                adapter_node::CompressionOutput {
                    source_sha256: output.source_sha256,
                    bytes: output.bytes,
                },
            ))
        })
        .collect()
}

async fn complete_plan(
    root: &Path,
    compatibility: Value,
    plan: PackagingPlan,
    staging_directory: &str,
    map_base: &Path,
    minified: bool,
) -> Result<AdapterOutput, String> {
    let request = request_for_plan(
        root,
        compatibility,
        &plan,
        staging_directory,
        map_base,
        minified,
    )?;
    let mut bundle = ts_svelte_bundle::generate(request)
        .await
        .map_err(|error| format!("Adapter rebundle failed: {}", error.message))?;
    let mut files = Files::new();
    for chunk in &mut bundle.chunks {
        if let Some(map) = &mut chunk.map {
            absolute_sources(
                map,
                &root.join("dist").join(
                    chunk
                        .sourcemap_file_name
                        .as_deref()
                        .unwrap_or(&chunk.file_name),
                ),
            )?;
        }
        files.insert(chunk.file_name.clone(), chunk.code.as_bytes().to_vec());
    }
    for asset in &mut bundle.assets {
        let mut bytes = crate::asset_bytes(asset)?;
        if let Some(chunk) = bundle
            .chunks
            .iter()
            .find(|chunk| chunk.sourcemap_file_name.as_deref() == Some(asset.file_name.as_str()))
        {
            let mut map: Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
            absolute_sources(&mut map, &root.join("dist").join(&asset.file_name))?;
            if chunk.map.as_ref().is_some_and(|expected| expected != &map) {
                return Err(format!(
                    "Inconsistent rebundle sourcemap {}",
                    asset.file_name
                ));
            }
            bytes = serde_json::to_vec(&map).map_err(|e| e.to_string())?;
            asset.source = base64::engine::general_purpose::STANDARD.encode(&bytes);
        }
        if files.insert(asset.file_name.clone(), bytes).is_some() {
            return Err("Duplicate native rebundle output".into());
        }
    }
    let compressed = compressed(&plan)?;
    let deployed = plan.materialize(files, compressed)?;
    let mut artifacts = BTreeMap::new();
    for (file, bytes) in deployed {
        artifacts::insert(&mut artifacts, file, &bytes, "adapter-node")?;
    }
    Ok(AdapterOutput {
        artifacts,
        bundle,
        overwritten_assets: plan.overwritten_assets,
        instrumentation: None,
    })
}

/// Package a completed framework artifact snapshot. The caller must finish
/// metadata, prerendering and framework guards first; this function does not
/// turn an incomplete framework build into a successful deployment.
pub async fn package(
    build: &BuildRequest,
    request: &AdapterRequest,
    artifacts: &BTreeMap<String, Artifact>,
    manifest_expression: &str,
    prerendered: Prerendered,
    instrumentation: Option<&crate::instrumentation::ServerInstrumentation>,
) -> Result<AdapterOutput, String> {
    if build.kit.config.instrumentation.is_some() != instrumentation.is_some() {
        return Err("Adapter instrumentation requires the validated server entry receipt".into());
    }
    if instrumentation.is_some() {
        crate::instrumentation::validate_adapter_node_version(&build.kit.inventory.kit_version)?;
    }
    let root = &build.kit.inventory.project_root;
    let staging = build
        .kit
        .config
        .out_dir
        .strip_prefix(root)
        .map_err(|_| "Adapter staging output must be inside project root")?
        .join("native-adapter-node")
        .to_string_lossy()
        .into_owned();
    let mut server = Files::new();
    let mut client = Files::new();
    let mut total = 0usize;
    for (file, artifact) in artifacts {
        artifacts::relative(file)?;
        let (destination, name) = if let Some(name) = file.strip_prefix("server/") {
            (&mut server, name)
        } else if let Some(name) = file.strip_prefix("client/") {
            (&mut client, name)
        } else {
            return Err(format!("Unexpected framework artifact namespace {file}"));
        };
        if artifact.source.len() > 512 * 1024 * 1024 {
            return Err("Adapter artifact exceeds encoded limit".into());
        }
        let bytes = artifacts::bytes(artifact)?;
        total = total
            .checked_add(bytes.len())
            .ok_or("Artifact size overflow")?;
        if total > 512 * 1024 * 1024 {
            return Err("Adapter inputs exceed 512 MiB".into());
        }
        destination.insert(name.to_owned(), bytes);
    }
    let mut plan = adapter_node::prepare(adapter_node::AdapterInput {
        templates: request.templates.clone(),
        options: request.options.clone(),
        base: build.kit.environment.base.clone(),
        manifest_expression: manifest_expression.into(),
        package_json: request.package_json.clone(),
        server,
        client,
        prerendered,
        staging_directory: staging.clone(),
    })?;
    // prepare replaces the manifest module with adapter-specific exports. Any
    // old framework manifest map would now describe different generated text.
    plan.staging_files
        .remove(&format!("{staging}/manifest.js.map"));
    let mut output=complete_plan(root,json!({"svelteVersion":build.compiler.svelte_version,"kitVersion":build.kit.inventory.kit_version,
        "typescriptVersion":build.compiler.typescript_version,"allowPrerelease":build.compiler.allow_prerelease}),
        plan,&staging,&request.server_map_base,build.compiler.server_minify).await?;
    if let Some(receipt) = instrumentation {
        let wrapped = crate::instrumentation::finalize_adapter_node(
            &build.kit.inventory.kit_version,
            &request.options.out,
            &staging,
            receipt,
            &output.bundle,
            &output.artifacts,
        )?;
        output.artifacts = wrapped.artifacts;
        output.instrumentation = Some(InstrumentationOutput {
            identities: wrapped.identities,
            entrypoint: wrapped.entrypoint,
            start: wrapped.start,
            static_preludes: wrapped.static_preludes,
            dynamic_start: wrapped.dynamic_start,
            map_copy: wrapped.map_copy,
        });
    }
    let server = format!("{}/server/", request.options.out.trim_end_matches('/'));
    for (file, artifact) in &mut output.artifacts {
        if !(file.starts_with(&server) && file.ends_with(".map")) {
            continue;
        }
        let bytes = crate::artifacts::bytes(artifact)?;
        let mut map: Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        if relative_sources(&mut map, &root.join(file))? {
            artifact.source = base64::engine::general_purpose::STANDARD
                .encode(serde_json::to_vec(&map).map_err(|e| e.to_string())?);
        }
    }
    Ok(output)
}

/// Rollup writes `sources` relative to the emitted map. The rebundle composes
/// maps against absolute identities; the deployed map gets relative ones.
pub(crate) fn relative_sources(map: &mut Value, deployed_map: &Path) -> Result<bool, String> {
    let directory = deployed_map.parent().ok_or("Map has no parent directory")?;
    let Some(sources) = map.get_mut("sources").and_then(Value::as_array_mut) else {
        return Ok(false);
    };
    let mut changed = false;
    for source in sources {
        let Some(name) = source.as_str() else {
            continue;
        };
        let path = Path::new(name);
        if !path.is_absolute() {
            continue;
        }
        let from: Vec<_> = directory.components().collect();
        let to: Vec<_> = path.components().collect();
        let common = from.iter().zip(&to).take_while(|(a, b)| a == b).count();
        let mut parts: Vec<String> = vec!["..".into(); from.len() - common];
        parts.extend(
            to[common..]
                .iter()
                .map(|part| part.as_os_str().to_string_lossy().into_owned()),
        );
        *source = Value::String(parts.join("/"));
        changed = true;
    }
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deployed_maps_use_relative_sources() {
        let mut map = json!({"version":3,"sources":["/app/src/a.ts","../x.js","/app/build/server/chunks/b.js","node:fs"],"names":[],"mappings":""});
        assert!(
            relative_sources(&mut map, Path::new("/app/build/server/chunks/a.js.map")).unwrap()
        );
        assert_eq!(
            map["sources"],
            json!(["../../../src/a.ts", "../x.js", "b.js", "node:fs"])
        );
    }
    use ts_svelte_kit::adapter_node::{Compression, CompressionJob, RebundlePlan};

    fn plan() -> PackagingPlan {
        let source = b"<h1>Native compression</h1>".to_vec();
        let file = "build/client/index.html";
        PackagingPlan {
            adapter_version:adapter_node::ADAPTER_VERSION.into(),output_directory:"build".into(),
            files:BTreeMap::from([(file.into(),source.clone()),("build/index.js".into(),b"export {};".to_vec())]),
            staging_files:BTreeMap::from([
                (".stage/index.js".into(),b"import data from './data.json'; import value from './legacy.cjs'; import { sep } from 'node:path'; import { dependency } from '@scope/production/deep'; export const result={data,value,sep,dependency,mode:process.env.NODE_ENV};".to_vec()),
                (".stage/legacy.cjs".into(),b"module.exports = {legacy:42};".to_vec()),
                (".stage/data.json".into(),b"{\"value\":7}".to_vec()),
                (".stage/manifest.js".into(),b"export const manifest={}; export const prerendered=new Set(); export const base='';".to_vec()),
            ]),
            bundle:RebundlePlan {entries:BTreeMap::from([("index".into(),".stage/index.js".into()),("manifest".into(),".stage/manifest.js".into())]),
                output_directory:"build/server".into(),format:"esm".into(),sourcemap:true,chunk_file_names:"chunks/[name]-[hash].js".into(),
                prefer_builtins:true,export_conditions:vec!["node".into()],strict_commonjs_requires:true,json_modules:true,
                production_dependencies:BTreeMap::from([("@scope/production".into(),"1".into())])},
            compression:vec![
                CompressionJob {source:file.into(),destination:format!("{file}.gz"),source_sha256:ts_svelte_compress::sha256(&source),compression:Compression::Gzip {level:9}},
                CompressionJob {source:file.into(),destination:format!("{file}.br"),source_sha256:ts_svelte_compress::sha256(&source),compression:Compression::Brotli {quality:11,mode:"text".into(),size_hint:source.len() as u64}},
            ],instrumentation:false,overwritten_assets:vec![],required_stages:vec!["nativeRebundle".into(),"nativeCompression".into()],
        }
    }
    #[test]
    fn source_map_comments_are_syntax_aware_and_preserve_original_identity() {
        let code=b"export const text='//# sourceMappingURL=not-a-map';\n//# sourceMappingURL=index.js.map\n";
        let source = json!({"version":3,"sources":["../../src/page.ts"],"sourcesContent":["export const text='source';"],"names":[],"mappings":"AAAA"});
        let files = BTreeMap::from([(
            ".stage/index.js.map".into(),
            serde_json::to_vec(&source).unwrap(),
        )]);
        let (code, map) = module_source(
            ".stage/index.js",
            code,
            &files,
            ".stage",
            Path::new("/project/dist/nested"),
        )
        .unwrap();
        assert!(code.contains("'//# sourceMappingURL=not-a-map'"));
        assert!(!code.contains("//# sourceMappingURL=index.js.map"));
        assert_eq!(map.unwrap()["sources"][0], "/project/src/page.ts");
        assert!(
            module_source(
                ".stage/index.js",
                b"export {};\n//# sourceMappingURL=absent.map",
                &files,
                ".stage",
                Path::new("/project/dist")
            )
            .is_err()
        );
    }
    #[test]
    fn stale_compression_is_rejected_before_materialization() {
        let mut plan = plan();
        plan.files.insert(
            "build/client/index.html".into(),
            b"different bytes".to_vec(),
        );
        assert!(compressed(&plan).unwrap_err().contains("stale_source"));
    }
    #[tokio::test]
    async fn real_native_rebundle_preserves_cjs_json_external_roots_and_compressed_assets() {
        // The project root exists for native package resolution; all test
        // modules are virtual. No staging or deployment directory is created.
        let root = std::env::current_dir().unwrap();
        let mut plan = plan();
        let original = root.join("native-adapter-original.ts");
        let code = plan.staging_files[".stage/index.js"].clone();
        let map = json!({"version":3,"sources":[original],"sourcesContent":[String::from_utf8(code.clone()).unwrap()],"names":[],"mappings":"AAAA"});
        plan.staging_files.insert(
            ".stage/index.js.map".into(),
            serde_json::to_vec(&map).unwrap(),
        );
        plan.staging_files
            .get_mut(".stage/index.js")
            .unwrap()
            .extend_from_slice(b"\n//# sourceMappingURL=index.js.map");
        let compatibility = json!({"svelteVersion":"5.53.11","kitVersion":"2.69.2"});
        let request = request_for_plan(
            &root,
            compatibility.clone(),
            &plan,
            ".stage",
            &root.join("dist"),
            false,
        )
        .unwrap();
        assert_eq!(request.external_packages, ["@scope/production"]);
        assert!(
            !plan
                .bundle
                .is_production_external("@scope/production-extra")
        );
        assert!(plan.bundle.is_production_external("@scope/production/deep"));
        let output = complete_plan(
            &root,
            compatibility,
            plan,
            ".stage",
            &root.join("dist"),
            false,
        )
        .await
        .unwrap();
        for file in [
            "build/server/index.js",
            "build/server/index.js.map",
            "build/server/manifest.js",
            "build/server/manifest.js.map",
            "build/client/index.html.gz",
            "build/client/index.html.br",
        ] {
            assert!(output.artifacts.contains_key(file), "Missing {file}");
        }
        assert!(output.bundle.module_graph["@scope/production/deep"].is_external);
        let code = output
            .bundle
            .chunks
            .iter()
            .map(|chunk| chunk.code.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(code.contains("process.env.NODE_ENV"));
        assert!(code.contains("42"));
        assert!(!code.contains("./legacy.cjs"));
        let map: Value = serde_json::from_slice(
            &artifacts::bytes(&output.artifacts["build/server/index.js.map"]).unwrap(),
        )
        .unwrap();
        assert!(
            map["sources"]
                .as_array()
                .unwrap()
                .iter()
                .any(|source| source.as_str() == original.to_str())
        );
        let gzip = artifacts::bytes(&output.artifacts["build/client/index.html.gz"]).unwrap();
        assert!(gzip.starts_with(&[0x1f, 0x8b, 8]));
    }
}
