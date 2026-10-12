//! Native, pure SvelteKit server-instrumentation entry and adapter transforms.
//!
//! Ported from SvelteKit's MIT-licensed `core/adapt/builder.js` instrument,
//! create_instrumentation_facade and createInstrumentationInitializer, and
//! `core/env.js` create_exported_declarations. Copyright Svelte contributors.
//! The registered Kit 2 versions share one facade; Kit 3.0.1 and next.32 share
//! the initializer/namespace facade. No application, config, callback, filesystem
//! or process environment is evaluated here.
//!
//! `plan_instrumentation_entry` selects the actual independent server entry.
//! After bundling, `instrument_server_entry` transforms the supplied asset map:
//! it copies the original entry and sibling map to start, then replaces the
//! entry with the instrumentation facade. Static dependencies evaluate before
//! its top-level-await dynamic start import. Never flatten this into a static
//! start import or prepend instrumentation to the application module itself.
//!
//! Map copies deliberately follow upstream byte-for-byte: no invented mapping
//! for the generated facade and no rewriting sourceMappingURL, map file/source
//! identities, or relative imports. The original entry map remains present, as
//! upstream does. `map_copy` exposes that contract to the host. Start must remain
//! alongside entrypoint here because arbitrary relocation requires rebundling
//! relative imports/assets; the JS helper does not implement such rewriting.
//! Adapter callbacks (`module.generateText`) are not executed or approximated.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::sync_versions::KitSyncVersion;

pub type InstrumentationFiles = BTreeMap<PathBuf, Vec<u8>>;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InstrumentationAdapter {
    pub name: String,
    /// Actual supports.instrumentation() result, resolved by the host.
    pub supports_instrumentation: Option<bool>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InstrumentationEntryInput {
    pub kit_version: String,
    /// Observed existing source entry. None means no instrumentation file.
    pub source: Option<PathBuf>,
    /// Kit 2 requires Some(true); Kit 3 no longer has this flag.
    pub kit2_experimental_enabled: Option<bool>,
    pub adapter: Option<InstrumentationAdapter>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstrumentationEntryPlan {
    pub entries: BTreeMap<String, PathBuf>,
    pub output_file: Option<PathBuf>,
    pub adapter_wrapper_required: bool,
    pub environment_initializer_required: bool,
    /// Kit 3 shares this dependency between instrumentation and server tracing.
    pub required_externals: Vec<String>,
}

pub fn plan_instrumentation_entry(
    input: &InstrumentationEntryInput,
) -> Result<InstrumentationEntryPlan, String> {
    let kit3 = KitSyncVersion::parse(&input.kit_version)?.is_kit3();
    let Some(source) = &input.source else {
        return Ok(InstrumentationEntryPlan {
            entries: BTreeMap::new(),
            output_file: None,
            adapter_wrapper_required: false,
            environment_initializer_required: false,
            required_externals: vec![],
        });
    };
    validate_path(source)?;
    if !kit3 && input.kit2_experimental_enabled != Some(true) {
        return Err(
            "Kit 2 instrumentation requires resolved experimental.instrumentation.server=true"
                .into(),
        );
    }
    if let Some(adapter) = &input.adapter {
        if adapter.name.is_empty() || adapter.supports_instrumentation != Some(true) {
            return Err(format!(
                "{} is unsupported or unresolved in adapter {}: supports.instrumentation must be true",
                source.display(),
                adapter.name
            ));
        }
    }
    Ok(InstrumentationEntryPlan {
        entries: BTreeMap::from([("instrumentation.server".into(), source.clone())]),
        output_file: Some("instrumentation.server.js".into()),
        adapter_wrapper_required: true,
        environment_initializer_required: kit3,
        required_externals: if kit3 {
            vec!["@opentelemetry/api".into()]
        } else {
            vec![]
        },
    })
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum InstrumentationEnvironment {
    /// Emit the upstream Node provider. This reads process.env only when the
    /// deployed JS application executes, never during native build planning.
    ProcessEnv,
    /// Explicit adapter module source exporting a default environment object.
    /// Source is emitted verbatim; it is not evaluated to infer build values.
    ModuleSource { source: String },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InstrumentationInitializerInput {
    pub kit_version: String,
    pub output_directory: PathBuf,
    pub server_directory: PathBuf,
    pub environment: InstrumentationEnvironment,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstrumentationInitializerPlan {
    pub initializer: PathBuf,
    pub provider: PathBuf,
    /// Must identify the actual emitted server module exporting set_env.
    pub server_env_module: PathBuf,
    pub files: InstrumentationFiles,
}

pub fn create_instrumentation_initializer(
    input: &InstrumentationInitializerInput,
) -> Result<InstrumentationInitializerPlan, String> {
    if !KitSyncVersion::parse(&input.kit_version)?.is_kit3() {
        return Err("createInstrumentationInitializer is available only in Kit 3".into());
    }
    validate_path(&input.output_directory)?;
    validate_path(&input.server_directory)?;
    let provider = input.output_directory.join("__sveltekit_env.js");
    let initializer = input.output_directory.join("__sveltekit_env_init.js");
    let server_env_module = input.server_directory.join("env.js");
    let source = match &input.environment {
        InstrumentationEnvironment::ProcessEnv => "export default process.env;".to_owned(),
        InstrumentationEnvironment::ModuleSource { source } => source.clone(),
    };
    let code = format!(
        "import env from {};\nimport {{ set_env }} from {};\nset_env(env);\n",
        js(&specifier(&input.output_directory, &provider)?)?,
        js(&specifier(&input.output_directory, &server_env_module)?)?,
    );
    Ok(InstrumentationInitializerPlan {
        initializer: initializer.clone(),
        provider: provider.clone(),
        server_env_module,
        files: BTreeMap::from([
            (provider, source.into_bytes()),
            (initializer, code.into_bytes()),
        ]),
    })
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum InstrumentationInitializer {
    Existing {
        file: PathBuf,
    },
    Generate {
        output_directory: PathBuf,
        server_directory: PathBuf,
        environment: InstrumentationEnvironment,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InstrumentServerInput {
    pub kit_version: String,
    /// All paths use the same coordinate system (absolute or relative).
    pub files: InstrumentationFiles,
    pub entrypoint: PathBuf,
    pub instrumentation: PathBuf,
    /// Defaults to entrypoint's sibling start.js. Must be a distinct sibling.
    pub start: Option<PathBuf>,
    /// Actual entry exports in emitted order. Empty means side effects only.
    pub exports: Vec<String>,
    /// Required for Kit 3; rejected on Kit 2 rather than ignored.
    pub initializer: Option<InstrumentationInitializer>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstrumentationMapCopy {
    pub original: PathBuf,
    pub copied: PathBuf,
    pub bytes_unchanged: bool,
    pub original_retained: bool,
    pub facade_has_source_map: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstrumentServerPlan {
    /// Complete asset map after the upstream copy-and-replace operations.
    pub files: InstrumentationFiles,
    pub entrypoint: PathBuf,
    pub start: PathBuf,
    /// Static facade dependencies in source order. Kit 3 initializer is first.
    pub entry_preludes: Vec<String>,
    pub dynamic_start_specifier: String,
    pub requires_top_level_await: bool,
    pub map_copy: Option<InstrumentationMapCopy>,
    pub generated_files: Vec<PathBuf>,
}

fn validate_path(path: &Path) -> Result<(), String> {
    if path.as_os_str().is_empty()
        || path
            .to_str()
            .is_none_or(|s| s.contains(['\0', '\\', '?', '#']))
        || path.components().any(|c| {
            matches!(
                c,
                Component::ParentDir | Component::CurDir | Component::Prefix(_)
            )
        })
        || path
            .to_str()
            .is_some_and(|s| s.split('/').any(|p| p == "." || p == ".."))
    {
        return Err(format!(
            "Instrumentation paths must be normalized UTF-8 filesystem paths: {}",
            path.display()
        ));
    }
    Ok(())
}
fn js(value: &str) -> Result<String, String> {
    serde_json::to_string(value).map_err(|error| error.to_string())
}
fn relative(from: &Path, to: &Path) -> Result<String, String> {
    if from.is_absolute() != to.is_absolute() {
        return Err("Instrumentation paths must share absolute/relative coordinates".into());
    }
    let a = from.components().collect::<Vec<_>>();
    let b = to.components().collect::<Vec<_>>();
    let common = a.iter().zip(&b).take_while(|(a, b)| a == b).count();
    let mut parts = vec!["..".to_owned(); a.len() - common];
    parts.extend(
        b[common..]
            .iter()
            .map(|c| c.as_os_str().to_string_lossy().into_owned()),
    );
    Ok(parts.join("/"))
}
fn specifier(from: &Path, to: &Path) -> Result<String, String> {
    let relative = relative(from, to)?;
    Ok(if relative.starts_with('.') {
        relative
    } else {
        format!("./{relative}")
    })
}
fn map_path(path: &Path) -> PathBuf {
    PathBuf::from(format!("{}.map", path.display()))
}
fn reserved(name: &str) -> bool {
    matches!(
        name,
        "do" | "if"
            | "in"
            | "for"
            | "let"
            | "new"
            | "try"
            | "var"
            | "case"
            | "else"
            | "enum"
            | "eval"
            | "null"
            | "this"
            | "true"
            | "void"
            | "with"
            | "await"
            | "break"
            | "catch"
            | "class"
            | "const"
            | "false"
            | "super"
            | "throw"
            | "while"
            | "yield"
            | "delete"
            | "export"
            | "import"
            | "public"
            | "return"
            | "static"
            | "switch"
            | "typeof"
            | "default"
            | "extends"
            | "finally"
            | "package"
            | "private"
            | "continue"
            | "debugger"
            | "function"
            | "arguments"
            | "interface"
            | "protected"
            | "implements"
            | "instanceof"
    )
}
fn export_names(names: &[String], kit3: bool) -> Result<Vec<String>, String> {
    let mut seen = BTreeSet::new();
    let mut result = Vec::new();
    for name in names {
        let mut chars = name.chars();
        if !chars
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_' || c == '$')
            || !chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
        {
            return Err(format!(
                "Instrumentation facade requires identifier export names; unsupported export {name:?}"
            ));
        }
        if !seen.insert(name.clone()) {
            if kit3 {
                continue;
            }
            return Err(format!("Duplicate Kit 2 instrumentation export {name}"));
        }
        result.push(name.clone());
    }
    Ok(result)
}
fn facade(
    kit3: bool,
    names: &[String],
    preludes: &[String],
    start: &str,
) -> Result<String, String> {
    let names = export_names(names, kit3)?;
    let mut aliases = BTreeMap::new();
    let mut index = 0;
    for name in &names {
        if !reserved(name) {
            continue;
        }
        loop {
            let alias = format!("_{index}");
            index += 1;
            if !names.contains(&alias) {
                aliases.insert(name.clone(), alias);
                break;
            }
        }
    }
    let mut lines = preludes
        .iter()
        .map(|path| js(path).map(|s| format!("import {s};")))
        .collect::<Result<Vec<_>, _>>()?;
    if kit3 {
        let mut namespace = "__mod".to_owned();
        let mut index = 0;
        while names.contains(&namespace) {
            namespace = format!("__mod{index}");
            index += 1;
        }
        lines.push(format!("const {namespace} = await import({});", js(start)?));
        let mut reexports = vec![];
        for name in &names {
            if let Some(alias) = aliases.get(name) {
                lines.push(format!("const {alias} = {namespace}.{name};"));
                reexports.push(format!("{alias} as {name}"));
            } else {
                lines.push(format!("export const {name} = {namespace}.{name};"));
            }
        }
        if !reexports.is_empty() {
            lines.push(format!("export {{ {} }};", reexports.join(", ")));
        }
    } else {
        let imports = names
            .iter()
            .map(|name| {
                aliases
                    .get(name)
                    .map_or_else(|| name.clone(), |alias| format!("{name}: {alias}"))
            })
            .collect::<Vec<_>>();
        let exports = names
            .iter()
            .map(|name| {
                aliases
                    .get(name)
                    .map_or_else(|| name.clone(), |alias| format!("{alias} as {name}"))
            })
            .collect::<Vec<_>>();
        lines.push(format!(
            "const {{ {} }} = await import({});",
            imports.join(", "),
            js(start)?
        ));
        if !exports.is_empty() {
            lines.push(format!("export {{ {} }};", exports.join(", ")));
        }
    }
    Ok(lines.join("\n"))
}

pub fn instrument_server_entry(
    input: &InstrumentServerInput,
) -> Result<InstrumentServerPlan, String> {
    let kit3 = KitSyncVersion::parse(&input.kit_version)?.is_kit3();
    for path in input
        .files
        .keys()
        .chain([&input.entrypoint, &input.instrumentation])
    {
        validate_path(path)?;
    }
    let parent = input
        .entrypoint
        .parent()
        .ok_or("Entrypoint must name a file")?;
    let start = input
        .start
        .clone()
        .unwrap_or_else(|| parent.join("start.js"));
    validate_path(&start)?;
    if start.parent() != Some(parent) {
        return Err("Cross-directory start relocation requires import/asset rebundling, which instrument does not perform".into());
    }
    if start == input.entrypoint
        || start == input.instrumentation
        || input.entrypoint == input.instrumentation
    {
        return Err("Instrumentation, start and entrypoint must be distinct".into());
    }
    let original = input
        .files
        .get(&input.entrypoint)
        .ok_or("Entrypoint file not found in emitted asset map")?;
    if !input.files.contains_key(&input.instrumentation) {
        return Err("Instrumentation file not found in emitted asset map".into());
    }
    if input.files.contains_key(&start) || input.files.contains_key(&map_path(&start)) {
        return Err("Start output collides with an existing emitted asset".into());
    }
    let mut files = input.files.clone();
    let mut generated_files = vec![];
    let initializer = match (&input.initializer, kit3) {
        (None, false) => None,
        (Some(_), false) => {
            return Err("Kit 2 instrument has no environment initializer parameter".into());
        }
        (None, true) => {
            return Err("Kit 3 instrumentation requires its environment initializer".into());
        }
        (Some(InstrumentationInitializer::Existing { file }), true) => {
            validate_path(file)?;
            if !files.contains_key(file) {
                return Err("Instrumentation initializer not found in emitted asset map".into());
            }
            Some(file.clone())
        }
        (
            Some(InstrumentationInitializer::Generate {
                output_directory,
                server_directory,
                environment,
            }),
            true,
        ) => {
            let plan = create_instrumentation_initializer(&InstrumentationInitializerInput {
                kit_version: input.kit_version.clone(),
                output_directory: output_directory.clone(),
                server_directory: server_directory.clone(),
                environment: environment.clone(),
            })?;
            if !files.contains_key(&plan.server_env_module) {
                return Err(
                    "Kit 3 server env.js is absent; cannot initialize its actual set_env export"
                        .into(),
                );
            }
            for (path, bytes) in plan.files {
                if path == start || path == input.entrypoint || files.contains_key(&path) {
                    return Err(format!(
                        "Generated instrumentation initializer collides with {}",
                        path.display()
                    ));
                }
                generated_files.push(path.clone());
                files.insert(path, bytes);
            }
            Some(plan.initializer)
        }
    };
    let mut preludes = vec![];
    if let Some(initializer) = initializer {
        if initializer == input.entrypoint
            || initializer == input.instrumentation
            || initializer == start
        {
            return Err("Environment initializer must be distinct from instrumentation and application entries".into());
        }
        preludes.push(specifier(parent, &initializer)?);
    }
    // Kit 2 prepends ./ even for ../ paths. Preserve that version's spelling;
    // JSON string escaping is equivalent and also handles quotes in filenames.
    preludes.push(if kit3 {
        specifier(parent, &input.instrumentation)?
    } else {
        format!("./{}", relative(parent, &input.instrumentation)?)
    });
    let dynamic_start_specifier = specifier(parent, &start)?;
    let source = facade(kit3, &input.exports, &preludes, &dynamic_start_specifier)?;
    let map_copy = input.files.get(&map_path(&input.entrypoint)).map(|bytes| {
        files.insert(map_path(&start), bytes.clone());
        InstrumentationMapCopy {
            original: map_path(&input.entrypoint),
            copied: map_path(&start),
            bytes_unchanged: true,
            original_retained: true,
            facade_has_source_map: false,
        }
    });
    files.insert(start.clone(), original.clone());
    files.insert(input.entrypoint.clone(), source.into_bytes());
    generated_files.push(input.entrypoint.clone());
    Ok(InstrumentServerPlan {
        files,
        entrypoint: input.entrypoint.clone(),
        start,
        entry_preludes: preludes,
        dynamic_start_specifier,
        requires_top_level_await: true,
        map_copy,
        generated_files,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn request(version: &str) -> InstrumentServerInput {
        InstrumentServerInput {
            kit_version: version.into(), entrypoint: "build/index.js".into(), instrumentation: "build/server/instrumentation.server.js".into(), start: None,
            files: BTreeMap::from([("build/index.js".into(), b"import './handler.js';\nexport const server = {};\n//# sourceMappingURL=index.js.map".to_vec()), ("build/index.js.map".into(), b"{\"version\":3,\"file\":\"index.js\",\"sources\":[\"../src/app.js\"],\"mappings\":\"AAAA\"}".to_vec()), ("build/server/instrumentation.server.js".into(), b"await setup();".to_vec()), ("build/server/env.js".into(), b"export function set_env(env) {}".to_vec())]),
            exports: vec!["server".into()], initializer: None,
        }
    }
    fn text(plan: &InstrumentServerPlan, path: &str) -> String {
        String::from_utf8(plan.files[Path::new(path)].clone()).unwrap()
    }
    #[test]
    fn kit2_keeps_instrumentation_before_dynamic_start_and_copies_maps() {
        let input = request("2.69.2");
        let plan = instrument_server_entry(&input).unwrap();
        assert_eq!(
            text(&plan, "build/index.js"),
            "import \"./server/instrumentation.server.js\";\nconst { server } = await import(\"./start.js\");\nexport { server };"
        );
        assert_eq!(
            plan.files[Path::new("build/start.js")],
            input.files[Path::new("build/index.js")]
        );
        assert_eq!(
            plan.files[Path::new("build/start.js.map")],
            input.files[Path::new("build/index.js.map")]
        );
        assert!(plan.map_copy.as_ref().unwrap().original_retained);
        assert!(!plan.map_copy.as_ref().unwrap().facade_has_source_map);
    }
    #[test]
    fn kit3_environment_initializer_precedes_instrumentation() {
        let mut input = request("3.0.1");
        input.initializer = Some(InstrumentationInitializer::Generate {
            output_directory: "build".into(),
            server_directory: "build/server".into(),
            environment: InstrumentationEnvironment::ProcessEnv,
        });
        let plan = instrument_server_entry(&input).unwrap();
        assert_eq!(
            text(&plan, "build/__sveltekit_env_init.js"),
            "import env from \"./__sveltekit_env.js\";\nimport { set_env } from \"./server/env.js\";\nset_env(env);\n"
        );
        assert_eq!(
            text(&plan, "build/__sveltekit_env.js"),
            "export default process.env;"
        );
        assert_eq!(
            text(&plan, "build/index.js"),
            "import \"./__sveltekit_env_init.js\";\nimport \"./server/instrumentation.server.js\";\nconst __mod = await import(\"./start.js\");\nexport const server = __mod.server;"
        );
    }
    #[test]
    fn reserved_exports_and_namespace_collisions_follow_versioned_facades() {
        let names = vec![
            "default".into(),
            "class".into(),
            "_0".into(),
            "__mod".into(),
            "__mod0".into(),
        ];
        let kit2 = facade(false, &names, &["./instrument.js".into()], "./start.js").unwrap();
        assert!(kit2.contains("default: _1, class: _2, _0, __mod, __mod0"));
        assert!(kit2.contains("export { _1 as default, _2 as class, _0, __mod, __mod0 };"));
        let kit3 = facade(
            true,
            &names,
            &["./init.js".into(), "./instrument.js".into()],
            "./start.js",
        )
        .unwrap();
        assert!(kit3.contains("const __mod1 = await import"));
        assert!(kit3.contains("const _1 = __mod1.default;"));
        assert!(kit3.contains("export const __mod = __mod1.__mod;"));
        assert!(kit3.ends_with("export { _1 as default, _2 as class };"));
    }
    #[test]
    fn side_effect_only_start_is_still_awaited() {
        assert_eq!(
            facade(false, &[], &["./i.js".into()], "./s.js").unwrap(),
            "import \"./i.js\";\nconst {  } = await import(\"./s.js\");"
        );
        assert_eq!(
            facade(true, &[], &["./i.js".into()], "./s.js").unwrap(),
            "import \"./i.js\";\nconst __mod = await import(\"./s.js\");"
        );
    }
    #[test]
    fn kit3_custom_environment_is_source_not_build_execution() {
        let source = "export default globalThis.adapterEnvironment;";
        let plan = create_instrumentation_initializer(&InstrumentationInitializerInput {
            kit_version: "3.0.0-next.32".into(),
            output_directory: "/out/init".into(),
            server_directory: "/out/server".into(),
            environment: InstrumentationEnvironment::ModuleSource {
                source: source.into(),
            },
        })
        .unwrap();
        assert_eq!(plan.files[&plan.provider], source.as_bytes());
        let code = String::from_utf8(plan.files[&plan.initializer].clone()).unwrap();
        assert!(code.contains("from \"../server/env.js\""));
    }
    #[test]
    fn entries_require_version_flag_and_actual_adapter_support() {
        let mut input = InstrumentationEntryInput {
            kit_version: "2.70.3".into(),
            source: Some("/app/src/instrumentation.server.ts".into()),
            kit2_experimental_enabled: None,
            adapter: None,
        };
        assert!(
            plan_instrumentation_entry(&input)
                .unwrap_err()
                .contains("experimental.instrumentation")
        );
        input.kit2_experimental_enabled = Some(true);
        assert!(
            plan_instrumentation_entry(&input)
                .unwrap()
                .adapter_wrapper_required
        );
        input.adapter = Some(InstrumentationAdapter {
            name: "adapter".into(),
            supports_instrumentation: None,
        });
        assert!(plan_instrumentation_entry(&input).is_err());
        input.adapter.as_mut().unwrap().supports_instrumentation = Some(true);
        input.kit_version = "3.0.1".into();
        input.kit2_experimental_enabled = None;
        assert!(
            plan_instrumentation_entry(&input)
                .unwrap()
                .environment_initializer_required
        );
        input.source = None;
        assert!(
            plan_instrumentation_entry(&input)
                .unwrap()
                .entries
                .is_empty()
        );
    }
    #[test]
    fn missing_inputs_and_output_collisions_fail_without_mutation() {
        let mut input = request("3.0.1");
        assert!(
            instrument_server_entry(&input)
                .unwrap_err()
                .contains("requires its environment initializer")
        );
        input.kit_version = "2.70.3".into();
        input.start = Some(input.entrypoint.clone());
        assert!(
            instrument_server_entry(&input)
                .unwrap_err()
                .contains("distinct")
        );
        input.start = Some("build/nested/start.js".into());
        assert!(
            instrument_server_entry(&input)
                .unwrap_err()
                .contains("rebundling")
        );
        input.start = None;
        input.files.insert("build/start.js".into(), vec![]);
        assert!(
            instrument_server_entry(&input)
                .unwrap_err()
                .contains("collides")
        );
    }
    #[test]
    fn registered_versions_have_matching_facades_and_unsafe_names_fail() {
        let expected = instrument_server_entry(&request("2.67.0")).unwrap().files;
        for version in ["2.68.0", "2.69.2", "2.69.3", "2.70.3"] {
            assert_eq!(
                instrument_server_entry(&request(version)).unwrap().files,
                expected
            );
        }
        assert!(facade(true, &["not-an-identifier".into()], &[], "./start.js").is_err());
        assert!(instrument_server_entry(&request("3.1.0")).is_err());
    }
}
