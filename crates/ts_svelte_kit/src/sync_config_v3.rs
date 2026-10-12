//! Kit 3 parent tsconfig generation. This module does not rewrite the user's
//! configuration, execute callbacks, or infer Kit 2's removed `$lib` alias.
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

use serde_json::{Map, Value, json};

use super::sync_config::SyncConfig;

const MAIN_ID: &str = "$app/tsconfig";
const WORKER_ID: &str = "$app/tsconfig/service-worker";
const CONFIG_LIMIT: u64 = 1024 * 1024;

#[derive(Debug)]
pub struct GeneratedConfigs {
    pub files: BTreeMap<PathBuf, String>,
    pub warnings: Vec<String>,
    pub user_configs: Vec<PathBuf>,
    /// Full TypeScript root-file expansion is owned by project discovery, not
    /// this generator. Hosts must not present these warnings as a complete
    /// replacement for TypeScript's project validation.
    pub validation_limits: Vec<String>,
}

/// Produce absolute output paths and JSON contents without writing files.
/// User JSONC is read/resolved to surface malformed or unsupported inheritance;
/// options stay in the user's config rather than being copied into its parent.
/// Use `generate_with_diagnostics` when displaying configuration warnings.
/// # Errors
/// Rejects invalid aliases, malformed/unresolved config inheritance and limits.
pub fn generate(root: &Path, config: &SyncConfig) -> Result<BTreeMap<PathBuf, String>, String> {
    Ok(generate_with_diagnostics(root, config)?.files)
}

/// Generate both parent configs and inspect the actual user configurations.
/// Missing/mismatched `extends`, paths, types and essential options are warnings,
/// matching Kit's non-destructive validation policy.
/// # Errors
/// Returns the same generation/JSONC resolution errors as `generate`.
pub fn generate_with_diagnostics(
    root: &Path,
    config: &SyncConfig,
) -> Result<GeneratedConfigs, String> {
    if !root.is_absolute() {
        return Err("Kit 3 tsconfig generation requires an absolute project root".into());
    }
    let root = normalize(root);
    let paths = absolute_aliases(&root, config)?;
    let mut files = BTreeMap::new();
    for (id, worker) in [(MAIN_ID, false), (WORKER_ID, true)] {
        let file = root.join(format!("node_modules/{id}.json"));
        let directory = file
            .parent()
            .ok_or("Generated config has no parent directory")?;
        let mut options = json!({
            "paths": paths.iter().map(|(key, values)| (key.clone(), json!(values.iter().map(|value| relative(directory, Path::new(value))).collect::<Vec<_>>()))).collect::<Map<_,_>>(),
            "types":["$app/types"],
            "lib": if worker { vec!["ESNext", "WebWorker"] } else { vec!["ESNext", "DOM", "DOM.Iterable"] },
            "verbatimModuleSyntax":true,
            "isolatedModules":true,
            "allowJs":true,
            "checkJs":true,
            "forceConsistentCasingInFileNames":true,
            "resolveJsonModule":true,
            "moduleDetection":"force",
            "moduleResolution":"bundler",
            "allowImportingTsExtensions":true,
            "module":"esnext",
            "target":"esnext",
            "skipLibCheck":true,
            "esModuleInterop":true,
            "noEmit":true
        });
        if !worker {
            options["rootDirs"] = json!([
                relative(directory, &root),
                relative(
                    directory,
                    &normalize(&root.join(&config.out_dir).join("types"))
                )
            ]);
        }
        let content = json!({"compilerOptions":options});
        files.insert(
            file,
            serde_json::to_string_pretty(&content).map_err(|error| error.to_string())?,
        );
    }
    // The upstream writers produce no `$app/package.json`: ordinary package
    // subpath resolution finds `$app/tsconfig.json` and `$app/types/index.d.ts`.
    let mut result = GeneratedConfigs {
        files,
        warnings: Vec::new(),
        user_configs: Vec::new(),
        validation_limits: Vec::new(),
    };
    for (directory, expected_id) in [
        (&root, MAIN_ID),
        (&normalize(&root.join(&config.service_worker)), WORKER_ID),
    ] {
        let Some(file) = find_user_config(directory) else {
            continue;
        };
        let own = read_jsonc(&file)?;
        result.user_configs.push(file.clone());
        if !directly_extends(&own, expected_id) {
            result
                .warnings
                .push(format!("{} should extend {expected_id:?}", file.display()));
            continue;
        }
        let resolved = resolve_config(&file, &result.files, &mut BTreeSet::new(), 0)?;
        let options = resolved
            .get("compilerOptions")
            .and_then(Value::as_object)
            .ok_or_else(|| format!("{}: compilerOptions must be an object", file.display()))?;
        for key in ["verbatimModuleSyntax", "isolatedModules"] {
            if options.get(key) != Some(&Value::Bool(true)) {
                result.warnings.push(format!(
                    "{}: {key:?} was overwritten. It should be true",
                    file.display()
                ));
            }
        }
        if let Some(types) = options.get("types") {
            let types = string_array(types, "compilerOptions.types")?;
            if !types.iter().any(|ty| ty == "$app/types") {
                result.warnings.push(format!(
                    "{}: \"types\" was overwritten. It must include \"$app/types\"",
                    file.display()
                ));
            }
        }
        if let Some(actual) = options.get("paths") {
            let actual = actual
                .as_object()
                .ok_or("compilerOptions.paths must be an object")?;
            let mut mismatches = BTreeSet::new();
            for (name, expected) in &paths {
                if actual.get(name) != Some(&json!(expected)) {
                    mismatches.insert(name.strip_suffix("/*").unwrap_or(name));
                }
            }
            if !mismatches.is_empty() {
                result.warnings.push(format!(
                    "{}: \"paths\" was overwritten. Imports from {} may not typecheck",
                    file.display(),
                    mismatches
                        .into_iter()
                        .map(|s| format!("{s:?}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
        }
        if expected_id == MAIN_ID {
            result.validation_limits.push(format!("{}: service-worker root-file exclusion requires project root-file expansion; this generator has not validated it", file.display()));
        }
    }
    Ok(result)
}

fn absolute_aliases(
    root: &Path,
    config: &SyncConfig,
) -> Result<BTreeMap<String, Vec<String>>, String> {
    let mut paths = BTreeMap::new();
    for (key, value) in &config.alias {
        // The JS regex uses dot without dotAll, so line separators are invalid.
        if key.is_empty() || key.contains(['\n', '\r', '\u{2028}', '\u{2029}']) {
            return Err(format!("Invalid Kit alias key {key:?}"));
        }
        if value.is_empty() || value.contains(['\n', '\r', '\u{2028}', '\u{2029}']) {
            return Err(format!("Invalid Kit alias value {value:?}"));
        }
        let bare_value = value.strip_suffix("/*").unwrap_or(value);
        let resolved = slash(&normalize(&root.join(bare_value)));
        if key.ends_with("/*") {
            paths.insert(key.clone(), vec![format!("{resolved}/*")]);
        } else {
            paths.insert(key.clone(), vec![resolved.clone()]);
            let extension = value
                .rsplit_once('.')
                .map(|(_, extension)| extension)
                .filter(|extension| {
                    !extension.is_empty()
                        && extension
                            .bytes()
                            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
                });
            if extension.is_none() && !config.alias.contains_key(&format!("{key}/*")) {
                paths.insert(format!("{key}/*"), vec![format!("{resolved}/*")]);
            }
        }
    }
    Ok(paths)
}

fn slash(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn normalize(path: &Path) -> PathBuf {
    let mut output = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                output.pop();
            }
            _ => output.push(component.as_os_str()),
        }
    }
    output
}

fn relative(from: &Path, to: &Path) -> String {
    let from = normalize(from);
    let to = normalize(to);
    let from = from.components().collect::<Vec<_>>();
    let to = to.components().collect::<Vec<_>>();
    let common = from.iter().zip(&to).take_while(|(a, b)| a == b).count();
    let mut output = PathBuf::new();
    for _ in common..from.len() {
        output.push("..");
    }
    for part in &to[common..] {
        output.push(part.as_os_str());
    }
    // Node path.relative(dir, dir) is the empty string.
    slash(&output)
}

fn find_user_config(directory: &Path) -> Option<PathBuf> {
    ["tsconfig.json", "jsconfig.json"]
        .into_iter()
        .map(|name| directory.join(name))
        .find(|file| file.is_file())
}

fn read_jsonc(file: &Path) -> Result<Value, String> {
    let metadata =
        std::fs::metadata(file).map_err(|error| format!("{}: {error}", file.display()))?;
    if !metadata.is_file() || metadata.len() > CONFIG_LIMIT {
        return Err(format!(
            "{} is not a bounded configuration file (maximum 1 MiB)",
            file.display()
        ));
    }
    let text =
        std::fs::read_to_string(file).map_err(|error| format!("{}: {error}", file.display()))?;
    let value =
        super::jsonc::parse(&text).map_err(|error| format!("{}: {error}", file.display()))?;
    if !value.is_object() {
        return Err(format!(
            "{}: configuration must be an object",
            file.display()
        ));
    }
    Ok(value)
}

fn directly_extends(config: &Value, id: &str) -> bool {
    config.get("extends").is_some_and(|value| {
        value.as_str() == Some(id)
            || value
                .as_array()
                .is_some_and(|values| values.iter().any(|value| value.as_str() == Some(id)))
    })
}

fn string_array(value: &Value, name: &str) -> Result<Vec<String>, String> {
    value
        .as_array()
        .ok_or_else(|| format!("{name} must be an array"))?
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| format!("{name} must contain strings"))
        })
        .collect()
}

fn config_candidate(
    path: &Path,
    generated: &BTreeMap<PathBuf, String>,
) -> Result<Option<PathBuf>, String> {
    let path = normalize(path);
    if generated.contains_key(&path) || path.is_file() {
        return Ok(Some(path));
    }
    let with_json = PathBuf::from(format!("{}.json", path.display()));
    if generated.contains_key(&with_json) || with_json.is_file() {
        return Ok(Some(with_json));
    }
    let package_file = path.join("package.json");
    if package_file.is_file() {
        let package = read_jsonc(&package_file)?;
        if let Some(config) = package.get("tsconfig") {
            let config = config
                .as_str()
                .ok_or("Package tsconfig field must be a string")?;
            let target = normalize(&path.join(config));
            if generated.contains_key(&target) || target.is_file() {
                return Ok(Some(target));
            }
            let with_json = PathBuf::from(format!("{}.json", target.display()));
            if generated.contains_key(&with_json) || with_json.is_file() {
                return Ok(Some(with_json));
            }
            return Err(format!("Unresolved package tsconfig {}", target.display()));
        }
    }
    let nested = path.join("tsconfig.json");
    Ok((generated.contains_key(&nested) || nested.is_file()).then_some(nested))
}

fn resolve_extends(
    directory: &Path,
    id: &str,
    generated: &BTreeMap<PathBuf, String>,
) -> Result<PathBuf, String> {
    if id.starts_with('.') || Path::new(id).is_absolute() {
        return config_candidate(&directory.join(id), generated)?.ok_or_else(|| {
            format!(
                "Unresolved tsconfig extends {id:?} from {}",
                directory.display()
            )
        });
    }
    for ancestor in directory.ancestors() {
        if let Some(file) = config_candidate(&ancestor.join("node_modules").join(id), generated)? {
            return Ok(file);
        }
    }
    Err(format!(
        "Unresolved package tsconfig extends {id:?} from {}",
        directory.display()
    ))
}

fn merge(target: &mut Map<String, Value>, incoming: &Map<String, Value>) {
    for (name, value) in incoming {
        if name == "compilerOptions" {
            let options = target.entry(name.clone()).or_insert_with(|| json!({}));
            if let (Some(target), Some(source)) = (options.as_object_mut(), value.as_object()) {
                target.extend(source.clone());
            } else {
                *options = value.clone();
            }
        } else {
            target.insert(name.clone(), value.clone());
        }
    }
}

fn resolve_config(
    file: &Path,
    generated: &BTreeMap<PathBuf, String>,
    visiting: &mut BTreeSet<PathBuf>,
    depth: usize,
) -> Result<Value, String> {
    if depth >= 32 || !visiting.insert(file.to_path_buf()) {
        return Err(format!(
            "Cyclic or excessive tsconfig inheritance at {}",
            file.display()
        ));
    }
    let mut own = if let Some(content) = generated.get(file) {
        serde_json::from_str::<Value>(content).map_err(|error| error.to_string())?
    } else {
        read_jsonc(file)?
    };
    let directory = file.parent().ok_or("Config has no parent directory")?;
    let mut inherited = Map::new();
    if let Some(extends) = own.get("extends") {
        let ids = if let Some(id) = extends.as_str() {
            vec![id.to_owned()]
        } else {
            string_array(extends, "extends")?
        };
        for id in ids {
            let parent = resolve_extends(directory, &id, generated)?;
            let value = resolve_config(&parent, generated, visiting, depth + 1)?;
            merge(
                &mut inherited,
                value.as_object().ok_or("Parent config must be an object")?,
            );
        }
    }
    let own = own.as_object_mut().ok_or("Config must be an object")?;
    if let Some(options) = own.get_mut("compilerOptions") {
        let options = options
            .as_object_mut()
            .ok_or("compilerOptions must be an object")?;
        if let Some(paths) = options.get_mut("paths") {
            let paths = paths
                .as_object_mut()
                .ok_or("compilerOptions.paths must be an object")?;
            for (name, values) in paths {
                *values = json!(
                    string_array(values, &format!("paths.{name}"))?
                        .iter()
                        .map(|value| slash(&normalize(&directory.join(value))))
                        .collect::<Vec<_>>()
                );
            }
        }
        for name in ["rootDirs", "typeRoots"] {
            if let Some(values) = options.get_mut(name) {
                *values = json!(
                    string_array(values, name)?
                        .iter()
                        .map(|value| slash(&normalize(&directory.join(value))))
                        .collect::<Vec<_>>()
                );
            }
        }
        if let Some(value) = options.get_mut("baseUrl") {
            *value = json!(slash(&normalize(
                &directory.join(value.as_str().ok_or("baseUrl must be a string")?)
            )));
        }
    }
    for name in ["include", "exclude", "files"] {
        if let Some(values) = own.get_mut(name) {
            *values = json!(
                string_array(values, name)?
                    .iter()
                    .map(|value| slash(&normalize(&directory.join(value))))
                    .collect::<Vec<_>>()
            );
        }
    }
    merge(&mut inherited, own);
    visiting.remove(file);
    Ok(Value::Object(inherited))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "kit3-config-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&root).unwrap();
            Self(root)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn generates_both_parent_contracts_without_kit2_defaults() {
        let fixture = Fixture::new();
        std::fs::create_dir_all(fixture.0.join("src/lib")).unwrap();
        let files = generate(&fixture.0, &SyncConfig::default()).unwrap();
        assert_eq!(files.len(), 2);
        let main: Value =
            serde_json::from_str(&files[&fixture.0.join("node_modules/$app/tsconfig.json")])
                .unwrap();
        assert_eq!(
            main["compilerOptions"]["rootDirs"],
            json!(["../..", "../../.svelte-kit/types"])
        );
        assert_eq!(main["compilerOptions"]["types"], json!(["$app/types"]));
        assert_eq!(main["compilerOptions"]["paths"], json!({}));
        assert_eq!(main["compilerOptions"]["checkJs"], true);
        assert!(main.get("include").is_none());
        assert!(main.get("exclude").is_none());
        let worker: Value = serde_json::from_str(
            &files[&fixture
                .0
                .join("node_modules/$app/tsconfig/service-worker.json")],
        )
        .unwrap();
        assert_eq!(
            worker["compilerOptions"]["lib"],
            json!(["ESNext", "WebWorker"])
        );
        assert!(worker["compilerOptions"].get("rootDirs").is_none());
    }

    #[test]
    fn aliases_rebase_for_each_parent_and_preserve_wildcard_override() {
        let fixture = Fixture::new();
        let config = SyncConfig {
            alias: BTreeMap::from([
                ("$domain".into(), "src/domain".into()),
                ("$domain/*".into(), "src/custom/*".into()),
                ("$entry".into(), "src/entry.ts".into()),
            ]),
            ..SyncConfig::default()
        };
        let files = generate(&fixture.0, &config).unwrap();
        let main: Value =
            serde_json::from_str(&files[&fixture.0.join("node_modules/$app/tsconfig.json")])
                .unwrap();
        assert_eq!(
            main["compilerOptions"]["paths"]["$domain"],
            json!(["../../src/domain"])
        );
        assert_eq!(
            main["compilerOptions"]["paths"]["$domain/*"],
            json!(["../../src/custom/*"])
        );
        assert!(main["compilerOptions"]["paths"].get("$entry/*").is_none());
        let worker: Value = serde_json::from_str(
            &files[&fixture
                .0
                .join("node_modules/$app/tsconfig/service-worker.json")],
        )
        .unwrap();
        assert_eq!(
            worker["compilerOptions"]["paths"]["$entry"],
            json!(["../../../src/entry.ts"])
        );
    }

    #[test]
    fn jsonc_array_inheritance_is_resolved_without_overwriting_user_options() {
        let fixture = Fixture::new();
        std::fs::write(
            fixture.0.join("base.json"),
            r#"{"compilerOptions":{"strict":true,"checkJs":false}}"#,
        )
        .unwrap();
        std::fs::write(fixture.0.join("tsconfig.json"), "{ // comment\n\"extends\":[\"$app/tsconfig\",\"./base.json\"],\"compilerOptions\":{\"types\":[],\"isolatedModules\":false,},}").unwrap();
        let output = generate_with_diagnostics(&fixture.0, &SyncConfig::default()).unwrap();
        assert_eq!(output.warnings.len(), 2);
        assert!(
            output
                .warnings
                .iter()
                .any(|warning| warning.contains("isolatedModules"))
        );
        assert!(
            output
                .warnings
                .iter()
                .any(|warning| warning.contains("$app/types"))
        );
        let parent: Value =
            serde_json::from_str(&output.files[&fixture.0.join("node_modules/$app/tsconfig.json")])
                .unwrap();
        assert_eq!(parent["compilerOptions"]["checkJs"], true);
        assert!(parent["compilerOptions"].get("strict").is_none());
    }

    #[test]
    fn malformed_and_cyclic_configs_are_explicit_errors() {
        let fixture = Fixture::new();
        std::fs::write(fixture.0.join("tsconfig.json"), "{/* unterminated").unwrap();
        assert!(generate(&fixture.0, &SyncConfig::default()).is_err());
        std::fs::write(
            fixture.0.join("tsconfig.json"),
            r#"{"extends":["$app/tsconfig","./base.json"]}"#,
        )
        .unwrap();
        std::fs::write(
            fixture.0.join("base.json"),
            r#"{"extends":"./tsconfig.json"}"#,
        )
        .unwrap();
        assert!(
            generate(&fixture.0, &SyncConfig::default())
                .unwrap_err()
                .contains("Cyclic")
        );
    }

    #[test]
    fn service_worker_has_its_own_user_config_and_package_extends_resolves() {
        let fixture = Fixture::new();
        std::fs::create_dir_all(fixture.0.join("src/service-worker")).unwrap();
        std::fs::create_dir_all(fixture.0.join("node_modules/preset")).unwrap();
        std::fs::write(
            fixture.0.join("node_modules/preset/package.json"),
            r#"{"tsconfig":"base.json"}"#,
        )
        .unwrap();
        std::fs::write(
            fixture.0.join("node_modules/preset/base.json"),
            r#"{"compilerOptions":{"skipLibCheck":false}}"#,
        )
        .unwrap();
        std::fs::write(
            fixture.0.join("src/service-worker/tsconfig.json"),
            r#"{"extends":["$app/tsconfig/service-worker","preset"]}"#,
        )
        .unwrap();
        let output = generate_with_diagnostics(&fixture.0, &SyncConfig::default()).unwrap();
        assert!(output.warnings.is_empty());
        assert_eq!(
            output.user_configs,
            vec![fixture.0.join("src/service-worker/tsconfig.json")]
        );
    }
}
