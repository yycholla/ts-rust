//! Native Kit type synchronization. Generates from sources, never from previous
//! Kit-generated declarations. No application code or environment is executed.
use crate::{
    sync_ambient as ambient,
    sync_config::{self, SyncConfig},
    sync_types,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Component, Path};

/// Refresh an existing native snapshot for an editor watch notification. Reuse
/// only environment names, never read environment values or execute config JS.
/// Initial synchronization remains explicit, so an editor cannot overwrite a
/// foreign generator's output just by opening a directory.
pub fn refresh_owned(root: &Path) -> Result<Value, String> {
    use std::io::Read;
    let root = root.canonicalize().map_err(|e| e.to_string())?;
    let config = sync_config::load(&root, None)?;
    let output = sync_config::output_path(&root, &config)?;
    let manifest = output.join("native-sync.json");
    if std::fs::symlink_metadata(&manifest)
        .map_err(|e| format!("Native sync snapshot required before editor refresh: {e}"))?
        .file_type()
        .is_symlink()
    {
        return Err("Native sync manifest cannot be a symlink".into());
    }
    let mut text = String::new();
    std::fs::File::open(manifest)
        .map_err(|e| e.to_string())?
        .take(16 * 1024 * 1024 + 1)
        .read_to_string(&mut text)
        .map_err(|e| e.to_string())?;
    if text.len() > 16 * 1024 * 1024 {
        return Err("Native sync manifest exceeds 16 MiB".into());
    }
    let metadata: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    if metadata["generator"] != "ts-rust-native-kit-sync" || metadata["schemaVersion"] != 1 {
        return Err("Unrecognized native sync manifest".into());
    }
    let names: Vec<String> = serde_json::from_value(metadata["environmentNames"].clone())
        .map_err(|_| "Run native sync once to record environment names before editor refresh")?;
    generate(&root, &config, &names)
}

fn hash(text: &[u8]) -> String {
    format!("{:x}", Sha256::digest(text))
}
fn slash(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}
fn trailing(value: Option<&str>) -> ambient::TrailingSlash {
    match value {
        Some("always") => ambient::TrailingSlash::Always,
        Some("never") => ambient::TrailingSlash::Never,
        Some("ignore") => ambient::TrailingSlash::Ignore,
        _ => ambient::TrailingSlash::Unknown,
    }
}
fn assets(root: &Path) -> Result<Vec<String>, String> {
    fn visit(root: &Path, path: &Path, names: &mut Vec<String>) -> Result<(), String> {
        if names.len() > 100_000 {
            return Err("Asset count limit exceeded".into());
        }
        for entry in std::fs::read_dir(path).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            let kind = entry.file_type().map_err(|e| e.to_string())?;
            if kind.is_symlink() {
                return Err("Symlink assets require an explicit resolved asset manifest".into());
            }
            if kind.is_dir() {
                visit(root, &entry.path(), names)?;
            } else if kind.is_file() {
                names.push(slash(
                    entry.path().strip_prefix(root).map_err(|e| e.to_string())?,
                ));
            }
        }
        Ok(())
    }
    let mut result = Vec::new();
    if root.exists() {
        visit(root, root, &mut result)?;
    }
    result.sort();
    Ok(result)
}

/// Generate types using explicit environment names (never values). Config is
/// statically parsed unless a resolved JSON contract is supplied by the host.
pub fn generate(root: &Path, config: &SyncConfig, env_names: &[String]) -> Result<Value, String> {
    let root = root.canonicalize().map_err(|e| e.to_string())?;
    let output = sync_config::output_path(&root, config)?;
    let version_name = sync_config::installed_kit_version(&root)?;
    let version = crate::sync_versions::KitSyncVersion::parse(&version_name)?;
    let explicit_env = sync_config::explicit_env(&root, config, version.is_kit3())?;
    let mut warnings = explicit_env.warnings;
    let mut validation_limits = Vec::<String>::new();
    let mut user_configs = Vec::new();
    let generated = sync_types::generate_route_types_for_version(
        &sync_types::RouteTypesConfig {
            root: root.clone(),
            routes: root.join(&config.routes),
            params: root.join(&config.params),
            out_dir: output.clone(),
            component_extensions: config.extensions.clone(),
            module_extensions: config.module_extensions.clone(),
        },
        &version,
    )?;
    let routes = generated
        .discovered_routes
        .iter()
        .map(|r| ambient::VersionedAppTypeRoute {
            layout: r.layout,
            route: ambient::AppTypeRoute {
                id: r.id.clone(),
                params: r
                    .params
                    .iter()
                    .map(|p| ambient::AppTypeParam {
                        name: p.name.clone(),
                        optional: p.optional,
                        matcher: p.matcher.clone(),
                    })
                    .collect(),
                page: r.page.then(|| trailing(r.page_trailing_slash.as_deref())),
                endpoint: r
                    .endpoint
                    .then(|| trailing(r.endpoint_trailing_slash.as_deref())),
            },
        })
        .collect::<Vec<_>>();
    let mut files = generated.files;
    let environment = env_names
        .iter()
        .map(|name| (name.clone(), String::new()))
        .collect::<BTreeMap<_, _>>();
    let prefixes = ambient::EnvPrefixes {
        public: config.public_prefix.clone(),
        private: config.private_prefix.clone(),
    };
    if let Some(text) = ambient::generate_ambient_for_version(
        &environment,
        &prefixes,
        config.explicit_environment_variables,
        &version,
    ) {
        files.insert(output.join("ambient.d.ts"), text);
    }
    let app_dir = root.join("node_modules/$app");
    let type_dir = if version.is_kit3() {
        app_dir.join("types")
    } else {
        output.clone()
    };
    let matcher_path = if version.is_kit3() {
        let base = root.join(&config.params);
        let path = std::iter::once(base.clone())
            .chain(
                config
                    .module_extensions
                    .iter()
                    .map(|e| root.join(format!("{}{e}", config.params))),
            )
            .chain(
                config
                    .module_extensions
                    .iter()
                    .map(|e| base.join(format!("index{e}"))),
            )
            .find(|p| p.is_file())
            .unwrap_or(base);
        sync_config::relative(
            "node_modules/$app/types",
            &slash(path.strip_prefix(&root).map_err(|e| e.to_string())?),
        )
    } else {
        sync_config::relative(&config.out_dir, &config.params)
    };
    let app_types = ambient::generate_non_ambient_for_version(
        &routes,
        &assets(&root.join(&config.assets))?,
        &matcher_path,
        &version,
    )?;
    let env_entry_relative = explicit_env
        .entry
        .as_ref()
        .map(|entry| {
            let entry = entry
                .strip_prefix(&root)
                .map_err(|error| error.to_string())?;
            Ok::<_, String>(sync_config::relative(
                if version.is_kit3() {
                    "node_modules/$app/types"
                } else {
                    &config.out_dir
                },
                &slash(entry),
            ))
        })
        .transpose()?;
    let env_variables = explicit_env
        .variables
        .iter()
        .map(|variable| ambient::ExplicitEnvVariable {
            name: variable.name.clone(),
            public: variable.public,
            description: variable.description.clone(),
            has_schema: variable.has_schema,
        })
        .collect::<Vec<_>>();
    let env_types = ambient::generate_explicit_env_for_version(
        env_entry_relative.as_deref(),
        &env_variables,
        &version,
    )?;
    if version.is_kit3() {
        let configs = crate::sync_config_v3::generate_with_diagnostics(&root, config)?;
        warnings.extend(configs.warnings);
        validation_limits.extend(configs.validation_limits);
        user_configs = configs.user_configs;
        files.extend(configs.files);
        files.insert(type_dir.join("index.d.ts"), app_types);
        files.insert(type_dir.join("env.d.ts"), env_types);
    } else {
        files.insert(
            output.join("tsconfig.json"),
            serde_json::to_string_pretty(&sync_config::tsconfig(&root, config))
                .map_err(|e| e.to_string())?,
        );
        files.insert(output.join("non-ambient.d.ts"), app_types);
        files.insert(output.join("env.d.ts"), env_types);
    }
    let mut hashes = BTreeMap::new();
    // Preflight every destination before changing anything. Generated writes
    // must stay below outDir, including through existing ancestor symlinks.
    for (path, text) in &files {
        if !path.starts_with(&output) && !(version.is_kit3() && path.starts_with(&app_dir)) {
            return Err("Generated file escapes owned Kit paths".into());
        }
        let relative = path
            .strip_prefix(&root)
            .map_err(|_| "Generated file escapes project")?;
        if relative
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
        {
            return Err("Invalid generated file path".into());
        }
        let mut ancestor = root.clone();
        for part in relative.components() {
            ancestor.push(part);
            if let Ok(meta) = std::fs::symlink_metadata(&ancestor) {
                if meta.file_type().is_symlink() {
                    return Err(format!(
                        "Generated path is a symlink: {}",
                        ancestor.display()
                    ));
                }
            }
        }
        hashes.insert(slash(relative), hash(text.as_bytes()));
    }
    let manifest = output.join("native-sync.json");
    let previous = if manifest.exists() {
        Some(
            serde_json::from_str::<Value>(
                &std::fs::read_to_string(&manifest).map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())?,
        )
    } else {
        None
    };
    let mut changed = Vec::new();
    for (path, text) in &files {
        if std::fs::read(path).ok().as_deref() == Some(text.as_bytes()) {
            continue;
        }
        let parent = path.parent().ok_or("Generated file has no parent")?;
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        let tmp = parent.join(format!(
            ".native-sync-{}-{}",
            std::process::id(),
            changed.len()
        ));
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)
            .map_err(|e| e.to_string())?;
        if let Err(e) = f
            .write_all(text.as_bytes())
            .and_then(|()| f.sync_all())
            .and_then(|()| std::fs::rename(&tmp, path))
        {
            let _ = std::fs::remove_file(tmp);
            return Err(e.to_string());
        }
        changed.push(slash(path.strip_prefix(&root).map_err(|e| e.to_string())?));
    }
    let mut removed = Vec::new();
    // Only remove files previously written by this generator, and only if the
    // content is still ours. Never sweep an existing official output tree.
    if let Some(previous) = previous.and_then(|v| v["files"].as_object().cloned()) {
        for (name, expected) in previous {
            if hashes.contains_key(&name) {
                continue;
            }
            let relative = Path::new(&name);
            if relative
                .components()
                .any(|c| !matches!(c, Component::Normal(_)))
            {
                return Err("Invalid previous sync manifest path".into());
            }
            let path = root.join(relative);
            if !path.starts_with(&output) && !(version.is_kit3() && path.starts_with(&app_dir)) {
                return Err("Previous sync path is outside generated directories".into());
            }
            if let Ok(bytes) = std::fs::read(&path) {
                if Some(hash(&bytes).as_str()) != expected.as_str() {
                    return Err(format!("Refusing to remove edited generated file: {name}"));
                }
                if !path
                    .canonicalize()
                    .map_err(|e| e.to_string())?
                    .starts_with(&root)
                {
                    return Err("Stale generated path escapes root".into());
                }
                std::fs::remove_file(path).map_err(|e| e.to_string())?;
                removed.push(name);
            }
        }
    }
    let mut input_paths = generated.inputs;
    input_paths.extend(explicit_env.entry.iter().cloned());
    input_paths.extend(user_configs.iter().cloned());
    let inputs = input_paths
        .into_iter()
        .map(|p| {
            std::fs::read(&p)
                .map(|bytes| (slash(&p), hash(&bytes)))
                .map_err(|e| e.to_string())
        })
        .collect::<Result<BTreeMap<_, _>, _>>()?;
    let environment_contract = json!({
        "explicit":version.is_kit3() || config.explicit_environment_variables,
        "entry":explicit_env.entry,
        "variables":explicit_env.variables.len(),
        "hostResolved":explicit_env.host_resolved,
        "schemaExecution":false,
        "runtimeValuesValidated":false
    });
    let metadata = json!({"schemaVersion":1,"generator":"ts-rust-native-kit-sync","config":config,"files":hashes,"inputs":inputs,"warnings":warnings,"validationLimits":validation_limits,"userConfigs":user_configs,"environment":environment_contract,"environmentNames":env_names});
    std::fs::create_dir_all(&output).map_err(|e| e.to_string())?;
    std::fs::write(
        &manifest,
        serde_json::to_vec_pretty(&metadata).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    Ok(
        json!({"ok":true,"outDir":output,"files":files.len(),"routes":generated.route_ids.len(),"changed":changed,"removed":removed,"environmentNames":env_names.len(),"environment":environment_contract,"warnings":warnings,"validationLimits":validation_limits,"userConfigs":user_configs,"native":true}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct EnvFixture(std::path::PathBuf);
    impl EnvFixture {
        fn new(version: &str) -> Self {
            let root = std::env::temp_dir().join(format!(
                "native-kit-env-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(root.join("src/routes")).unwrap();
            std::fs::create_dir_all(root.join("node_modules/@sveltejs/kit")).unwrap();
            std::fs::write(
                root.join("node_modules/@sveltejs/kit/package.json"),
                json!({"version":version}).to_string(),
            )
            .unwrap();
            std::fs::write(root.join("src/routes/+page.svelte"), "<h1>Home</h1>").unwrap();
            Self(root)
        }
    }
    impl Drop for EnvFixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn editor_refresh_regenerates_routes_and_preserves_environment_names() {
        let fixture = EnvFixture::new("2.69.2");
        assert!(refresh_owned(&fixture.0).is_err());
        generate(&fixture.0, &SyncConfig::default(), &["PUBLIC_TITLE".into()]).unwrap();
        std::fs::create_dir(fixture.0.join("src/routes/new")).unwrap();
        std::fs::write(fixture.0.join("src/routes/new/+page.svelte"), "<p>New</p>").unwrap();
        let result = refresh_owned(&fixture.0).unwrap();
        assert_eq!(result["environmentNames"], 1);
        assert!(
            fixture
                .0
                .join(".svelte-kit/types/src/routes/new/$types.d.ts")
                .is_file()
        );
        let ambient = std::fs::read_to_string(fixture.0.join(".svelte-kit/ambient.d.ts")).unwrap();
        assert!(ambient.contains("PUBLIC_TITLE"));
        std::fs::remove_file(fixture.0.join("src/routes/new/+page.svelte")).unwrap();
        assert!(
            !refresh_owned(&fixture.0).unwrap()["removed"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }
    #[test]
    fn sync_writes_and_removes_owned_routes() {
        let root =
            std::env::temp_dir().join(format!("native-kit-sync-test-{}", std::process::id()));
        std::fs::create_dir_all(root.join("src/routes/a")).unwrap();
        std::fs::create_dir_all(root.join("node_modules/@sveltejs/kit")).unwrap();
        std::fs::write(
            root.join("node_modules/@sveltejs/kit/package.json"),
            r#"{"version":"2.69.2"}"#,
        )
        .unwrap();
        std::fs::write(root.join("src/routes/a/+page.svelte"), "<h1>A</h1>").unwrap();
        let first = generate(&root, &SyncConfig::default(), &["PUBLIC_TITLE".into()]).unwrap();
        assert_eq!(first["ok"], true);
        assert!(
            root.join(".svelte-kit/types/src/routes/a/$types.d.ts")
                .is_file()
        );
        let second = generate(&root, &SyncConfig::default(), &["PUBLIC_TITLE".into()]).unwrap();
        assert_eq!(second["changed"], json!([]));
        std::fs::remove_file(root.join("src/routes/a/+page.svelte")).unwrap();
        let third = generate(&root, &SyncConfig::default(), &[]).unwrap();
        assert!(!third["removed"].as_array().unwrap().is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn kit3_sync_generates_explicit_schema_types_and_retains_config_warnings() {
        let fixture = EnvFixture::new("3.0.1");
        std::fs::write(fixture.0.join("src/env.ts"),"import {defineEnvVars} from '@sveltejs/kit/env'; export const variables=defineEnvVars({ PORT:{schema:(value:string | undefined)=>Number(value)}, TITLE:{public:true,description:'Site title'} });").unwrap();
        std::fs::write(
            fixture.0.join("tsconfig.json"),
            r#"{"extends":"$app/tsconfig","compilerOptions":{"isolatedModules":false}}"#,
        )
        .unwrap();
        let result = generate(&fixture.0, &SyncConfig::default(), &[]).unwrap();
        let env =
            std::fs::read_to_string(fixture.0.join("node_modules/$app/types/env.d.ts")).unwrap();
        assert!(env.contains("typeof import(\"../../../src/env.ts\").variables.PORT.schema"));
        assert!(env.contains("export const TITLE: string;"));
        assert_eq!(result["environment"]["variables"], 2);
        assert_eq!(result["environment"]["schemaExecution"], false);
        assert!(
            result["warnings"]
                .as_array()
                .unwrap()
                .iter()
                .any(|warning| warning.as_str().unwrap().contains("isolatedModules"))
        );
        assert!(!result["validationLimits"].as_array().unwrap().is_empty());
        assert_eq!(result["userConfigs"].as_array().unwrap().len(), 1);
        assert!(!fixture.0.join(".svelte-kit/ambient.d.ts").exists());
    }

    #[test]
    fn host_resolved_metadata_supports_factories_without_executing_them() {
        let fixture = EnvFixture::new("3.0.1");
        std::fs::write(
            fixture.0.join("src/env.ts"),
            "throw new Error('must never execute'); export const variables=makeConfig();",
        )
        .unwrap();
        assert!(
            generate(&fixture.0, &SyncConfig::default(), &[])
                .unwrap_err()
                .contains("resolvedEnv")
        );
        assert!(!fixture.0.join("node_modules/$app/types/env.d.ts").exists());
        let config = SyncConfig {
            resolved_env: Some(sync_config::ResolvedEnv {
                entry: "src/env.ts".into(),
                variables: vec![sync_config::ResolvedEnvVariable {
                    name: "PORT".into(),
                    public: false,
                    description: None,
                    has_schema: true,
                }],
            }),
            ..SyncConfig::default()
        };
        let result = generate(&fixture.0, &config, &[]).unwrap();
        assert_eq!(result["environment"]["hostResolved"], true);
        assert_eq!(result["environment"]["runtimeValuesValidated"], false);
    }

    #[test]
    fn kit2_explicit_flag_disables_legacy_env_modules_and_directory_entry_wins() {
        let fixture = EnvFixture::new("2.69.2");
        std::fs::create_dir_all(fixture.0.join("src/env")).unwrap();
        std::fs::write(
            fixture.0.join("src/env/index.ts"),
            "export const variables={ TITLE:{public:true} };",
        )
        .unwrap();
        std::fs::write(
            fixture.0.join("src/env.ts"),
            "throw new Error('directory index wins');",
        )
        .unwrap();
        let config = SyncConfig {
            explicit_environment_variables: true,
            ..SyncConfig::default()
        };
        let result = generate(&fixture.0, &config, &["PUBLIC_OLD".into()]).unwrap();
        assert_eq!(result["environment"]["variables"], 1);
        let ambient = std::fs::read_to_string(fixture.0.join(".svelte-kit/ambient.d.ts")).unwrap();
        assert!(!ambient.contains("$env/"));
        let env = std::fs::read_to_string(fixture.0.join(".svelte-kit/env.d.ts")).unwrap();
        assert!(env.contains("declare module '$app/env/public'"));
        assert!(env.contains("export const TITLE: string;"));
    }
}
