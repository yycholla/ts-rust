//! Native orchestration of component projections and TypeScript checking.
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};

use serde::Deserialize;
use serde_json::{Value, json};
use ts_svelte::Snapshot;
use ts_svelte::compatibility::{self, Versions};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Request {
    workspace: PathBuf,
    dependencies: PathBuf,
    components: Vec<ComponentInput>,
    #[serde(default)]
    files: Vec<FileInput>,
    #[serde(default)]
    allow_prerelease: bool,
    #[serde(default = "default_runes")]
    runes: Option<bool>,
    #[serde(default)]
    compiler_options: Option<serde_json::Map<String, Value>>,
    #[serde(default)]
    rune_modules: bool,
    #[serde(default)]
    prepare_only: bool,
}

// Serde needs the field's Option type: omission defaults to runes, while an
// explicit null requests inference and false selects legacy mode.
#[allow(clippy::unnecessary_wraps)]
const fn default_runes() -> Option<bool> {
    Some(true)
}

fn package_version(
    dependencies: &Path,
    name: &str,
    required: bool,
) -> Result<Option<String>, String> {
    let path = dependencies.join(name).join("package.json");
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if !required && error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("Cannot read {}: {error}", path.display())),
    };
    let package: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    package["version"]
        .as_str()
        .map(|v| Some(v.to_owned()))
        .ok_or_else(|| format!("Missing installed version for {name}"))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ComponentInput {
    path: String,
    document: Snapshot,
    #[serde(default)]
    typescript: Option<bool>,
    #[serde(default = "default_check")]
    check: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileInput {
    path: String,
    text: String,
    #[serde(default = "default_check")]
    check: bool,
}

const fn default_check() -> bool {
    true
}

fn input_path(path: &str) -> Result<&Path, String> {
    let path = Path::new(path);
    if path.components().next().is_none()
        || path
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
        || path.components().next().is_some_and(|part| {
            matches!(
                part.as_os_str().to_str(),
                Some("node_modules" | "__svelte" | "tsconfig.json")
            )
        })
    {
        return Err("input paths must be relative, without traversal or reserved names".into());
    }
    Ok(path)
}

fn write_new(workspace: &Path, path: &Path, text: &str) -> Result<(), String> {
    let destination = workspace.join(path);
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)
        .map_err(|e| e.to_string())?;
    file.write_all(text.as_bytes()).map_err(|e| e.to_string())
}

fn invoke(binary: &Path, request: &Value) -> Result<Value, String> {
    let mut child = Command::new(binary)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| e.to_string())?;
    let write = child
        .stdin
        .take()
        .ok_or("child stdin unavailable")?
        .write_all(request.to_string().as_bytes());
    if let Err(error) = write {
        let _ = child.kill();
        let _ = child.wait();
        return Err(error.to_string());
    }
    let output = child.wait_with_output().map_err(|e| e.to_string())?;
    if output.status.code().is_none() {
        return Err("compiler process terminated by signal".into());
    }
    let response: Value = serde_json::from_slice(&output.stdout).map_err(|e| e.to_string())?;
    if !response["ok"].is_boolean() || !matches!(output.status.code(), Some(0 | 1)) {
        return Err("compiler process returned an invalid response/status".into());
    }
    Ok(response)
}

fn mask(text: &str) -> String {
    text.chars()
        .map(|ch| if matches!(ch, '\r' | '\n') { ch } else { ' ' })
        .collect()
}

fn setup_types(
    workspace: &Path,
    dependencies: &Path,
) -> Result<HashMap<String, Vec<String>>, String> {
    #[cfg(unix)]
    std::os::unix::fs::symlink(dependencies, workspace.join("node_modules"))
        .map_err(|e| e.to_string())?;
    #[cfg(windows)]
    std::os::windows::fs::symlink_dir(dependencies, workspace.join("node_modules"))
        .map_err(|e| e.to_string())?;
    let types = fs::read_to_string(dependencies.join("svelte/types/index.d.ts"))
        .map_err(|e| e.to_string())?;
    let marker = "declare module '*.svelte' {";
    let start = types
        .find(marker)
        .ok_or("missing expected Svelte wildcard declaration")?;
    if types[start + marker.len()..].contains(marker) {
        return Err("ambiguous wildcard declaration".into());
    }
    let end = start
        + types[start..]
            .find("\n}")
            .ok_or("unclosed wildcard declaration")?
        + 2;
    if !types[start..end].is_ascii() {
        return Err("unexpected non-ASCII wildcard declaration".into());
    }
    let mut copied = types.clone();
    copied.replace_range(start..end, &mask(&types[start..end]));
    write_new(workspace, Path::new("__svelte/types.d.ts"), &copied)?;
    let shim = fs::read_to_string(dependencies.join("svelte-check/dist/src/svelte-shims-v4.d.ts"))
        .map_err(|e| e.to_string())?;
    write_new(workspace, Path::new("__svelte/shims.d.ts"), &shim)?;
    write_new(
        workspace,
        Path::new("__svelte/type-roots/svelte/index.d.ts"),
        "// Real declarations are explicitly included from __svelte/types.d.ts.\n",
    )?;
    let mut paths = HashMap::new();
    for quote in ['\'', '"'] {
        let prefix = format!("declare module {quote}");
        for declaration in types.split(&prefix).skip(1) {
            let name = declaration
                .split(quote)
                .next()
                .ok_or("malformed module declaration")?;
            if !name.contains('*') {
                paths.insert(name.to_owned(), vec!["./__svelte/types.d.ts".into()]);
            }
        }
    }
    Ok(paths)
}

fn prepare_workspace(request: &Request) -> Result<(PathBuf, PathBuf), String> {
    let count = request.components.len() + request.files.len();
    if count == 0 || count > 4096 {
        return Err("provide between 1 and 4096 project inputs".into());
    }
    let mut names = HashSet::new();
    for component in &request.components {
        input_path(&component.path)?;
        if !component.path.ends_with(".svelte") || !names.insert(component.path.clone()) {
            return Err("component paths must be unique .svelte files".into());
        }
    }
    for file in &request.files {
        input_path(&file.path)?;
        if !names.insert(file.path.clone()) {
            return Err("duplicate project input path".into());
        }
    }
    let dependencies = fs::canonicalize(&request.dependencies).map_err(|e| e.to_string())?;
    let parent = request
        .workspace
        .parent()
        .ok_or("workspace requires an existing parent")?;
    let workspace = fs::canonicalize(parent).map_err(|e| e.to_string())?.join(
        request
            .workspace
            .file_name()
            .ok_or("workspace requires a directory name")?,
    );
    if workspace.starts_with(&dependencies) {
        return Err("workspace must be outside dependency inputs".into());
    }
    fs::create_dir(&workspace).map_err(|e| e.to_string())?;
    Ok((workspace, dependencies))
}

fn write_projection(
    workspace: &Path,
    component: &ComponentInput,
    result: &Value,
) -> Result<(String, Value), String> {
    let mut code = result["code"]
        .as_str()
        .ok_or("projection has no code")?
        .to_owned();
    // This exact generated directive is ASCII; masking preserves all offsets.
    for directive in [
        "///<reference types=\"svelte\" />",
        "/// <reference types=\"svelte\" />",
    ] {
        let first_copy = result["mappings"]
            .as_array()
            .ok_or("projection has no exact mappings")?
            .iter()
            .filter_map(|mapping| mapping["generated"]["start"].as_u64())
            .min()
            .unwrap_or(code.len() as u64);
        if code.starts_with(directive) && directive.len() as u64 <= first_copy {
            code.replace_range(..directive.len(), &mask(directive));
        }
    }
    let extension = if component.typescript == Some(true) {
        "tsx"
    } else {
        "jsx"
    };
    let generated = format!("{}.{extension}", component.path);
    write_new(workspace, Path::new(&generated), &code)?;
    let basename = Path::new(&generated)
        .file_name()
        .and_then(|p| p.to_str())
        .ok_or("invalid generated filename")?;
    let specifier = serde_json::to_string(&format!("./{basename}")).map_err(|e| e.to_string())?;
    let bridge = format!("export {{default}} from {specifier};\nexport * from {specifier};\n");
    write_new(
        workspace,
        Path::new(&format!("{}.d.ts", component.path)),
        &bridge,
    )?;
    write_new(
        workspace,
        Path::new(&format!(
            "{}.d.svelte.ts",
            component
                .path
                .strip_suffix(".svelte")
                .expect("validated component extension")
        )),
        &bridge,
    )?;
    let projection = json!({"fileName":workspace.join(&generated),"required":component.check,"document":component.document,"code":code,"mappings":result["mappings"]});
    Ok((generated, projection))
}

fn check_rune_module(
    file: &FileInput,
    bin_dir: &Path,
    workspace: &Path,
    versions: &Versions,
) -> Result<Value, String> {
    let mut response = invoke(
        &bin_dir.join(format!("svelte-tool{}", std::env::consts::EXE_SUFFIX)),
        &json!({"op":"checkModule","typescript":Path::new(&file.path).extension().is_some_and(|ext|ext == "ts"),"compatibility":versions,"document":{"uri":file.path,"version":0,"text":file.text}}),
    )?;
    if response["error"]["code"] == "unsupported_module_typescript" {
        let transpiled = invoke(
            &bin_dir.join(format!(
                "svelte-ts-transpile{}",
                std::env::consts::EXE_SUFFIX
            )),
            &json!({"fileName":file.path,"source":file.text}),
        )?;
        if transpiled["ok"] != true {
            return Ok(
                json!({"ok":false,"stage":"transpile","workspace":workspace,"path":file.path,"failure":transpiled}),
            );
        }
        let code = transpiled["code"]
            .as_str()
            .ok_or("Native TypeScript transpiler returned no JavaScript")?;
        let source_map = transpiled["map"]
            .as_str()
            .ok_or("Native TypeScript transpiler returned no source map")?;
        response = invoke(
            &bin_dir.join(format!("svelte-tool{}", std::env::consts::EXE_SUFFIX)),
            &json!({"op":"checkModule","typescript":false,"compatibility":versions,"document":{"uri":file.path,"version":0,"text":code}}),
        )?;
        let body = if response["ok"] == true {
            &mut response["result"]
        } else {
            &mut response
        };
        let original = Snapshot {
            uri: file.path.clone(),
            version: 0,
            text: file.text.clone(),
        };
        let mut mapping_errors = Vec::new();
        for diagnostic in body["diagnostics"].as_array_mut().into_iter().flatten() {
            match ts_svelte_mapping::map_diagnostic(&original, code, source_map, diagnostic.clone())
            {
                Ok(mapped) => *diagnostic = mapped,
                Err(error) => {
                    // A corrupt or stale map cannot erase a compiler finding.
                    // Keep its generated coordinates and fail completeness.
                    diagnostic["generated"] = json!({
                        "uri":diagnostic["uri"],
                        "bytes":diagnostic["bytes"],
                        "range":diagnostic["range"],
                    });
                    diagnostic["bytes"] = Value::Null;
                    diagnostic["range"] = Value::Null;
                    diagnostic["mapping"] = json!("native-ts-transpile-unmapped");
                    diagnostic["mappingReason"] = json!(error);
                    mapping_errors.push(error);
                }
            }
        }
        if !mapping_errors.is_empty() {
            let diagnostics = body["diagnostics"].as_array().cloned().unwrap_or_default();
            return Ok(json!({
                "ok":false,"stage":"transpile-map","workspace":workspace,
                "path":file.path,"diagnostics":diagnostics,"mappingErrors":mapping_errors,
                "failure":response,
            }));
        }
    }
    let body = if response["ok"] == true {
        &response["result"]
    } else {
        &response
    };
    let diagnostics = body["diagnostics"].as_array().cloned().unwrap_or_default();
    if response["ok"] != true {
        return Ok(
            json!({"ok":false,"stage":"module","workspace":workspace,"path":file.path,"diagnostics":diagnostics,"failure":response}),
        );
    }
    Ok(json!({"ok":true,"diagnostics":diagnostics}))
}

fn project(request: Request) -> Result<Value, String> {
    let dependencies = fs::canonicalize(&request.dependencies).map_err(|e| e.to_string())?;
    let versions = Versions {
        svelte_version: package_version(&dependencies, "svelte", true)?
            .ok_or("Missing Svelte version")?,
        kit_version: package_version(&dependencies, "@sveltejs/kit", false)?,
        typescript_version: package_version(&dependencies, "typescript", false)?,
        allow_prerelease: request.allow_prerelease,
    };
    let profile = match compatibility::resolve(&versions) {
        Ok(profile) => profile,
        Err(message) => {
            return Ok(
                json!({"ok":false,"error":{"code":"unsupported_version","message":message},"versions":versions}),
            );
        }
    };
    let mut response = project_with_profile(request, &versions).unwrap_or_else(
        |message| json!({"ok":false,"error":{"code":"project_failed","message":message}}),
    );
    response["compatibility"] = json!(profile);
    Ok(response)
}

// Keep overlay construction, process invocation and evidence collection ordered.
#[allow(clippy::too_many_lines)]
fn project_with_profile(request: Request, versions: &Versions) -> Result<Value, String> {
    if !request.rune_modules
        && let Some(file) = request.files.iter().find(|file| {
            file.path.ends_with(".svelte.js")
                || (file.path.ends_with(".svelte.ts") && !file.path.ends_with(".d.svelte.ts"))
        })
    {
        return Ok(
            json!({"ok":false,"error":{"code":"unsupported_rune_module","message":format!("Rune-module checking requires explicit runeModules: true: {}",file.path)}}),
        );
    }
    let (workspace, dependencies) = prepare_workspace(&request)?;
    let paths = setup_types(&workspace, &dependencies)?;
    let sibling = std::env::current_exe().map_err(|e| e.to_string())?;
    let bin_dir = sibling.parent().ok_or("missing executable directory")?;
    let mut projections = Vec::new();
    let mut diagnostics = Vec::new();
    let mut auxiliary_modules = Vec::new();
    let mut auxiliary_components = Vec::new();
    let mut tracked_files = Vec::new();
    let mut requires_js = false;
    let mut files = vec![
        json!("__svelte/types.d.ts"),
        json!("__svelte/shims.d.ts"),
        json!(dependencies.join("svelte/svelte-html.d.ts")),
    ];
    for file in request.files {
        if file.path.ends_with(".svelte.js")
            || (file.path.ends_with(".svelte.ts") && !file.path.ends_with(".d.svelte.ts"))
        {
            if file.check {
                let result = check_rune_module(&file, bin_dir, &workspace, versions)?;
                if result["ok"] != true {
                    return Ok(result);
                }
                diagnostics.extend(
                    result["diagnostics"]
                        .as_array()
                        .cloned()
                        .unwrap_or_default(),
                );
            } else {
                auxiliary_modules.push(FileInput {
                    path: file.path.clone(),
                    text: file.text.clone(),
                    check: false,
                });
            }
        }
        write_new(&workspace, input_path(&file.path)?, &file.text)?;
        if file.check {
            files.push(json!(file.path));
        }
    }
    for mut component in request.components {
        if component.check {
            let compile = invoke(
                &bin_dir.join(format!("svelte-tool{}", std::env::consts::EXE_SUFFIX)),
                &json!({"op":"compile","target":"client","runes":request.runes,"document":component.document,"compatibility":versions}),
            )?;
            let compile_body = if compile["ok"] == true {
                &compile["result"]
            } else {
                &compile
            };
            diagnostics.extend(
                compile_body["diagnostics"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default(),
            );
            if compile["ok"] != true {
                return Ok(
                    json!({"ok":false,"stage":"compile","workspace":workspace,"diagnostics":diagnostics,"failure":compile}),
                );
            }
        }
        if component.typescript.is_none() {
            let info = invoke(
                &bin_dir.join(format!("svelte-tool{}", std::env::consts::EXE_SUFFIX)),
                &json!({"op":"componentInfo","runes":request.runes,"document":component.document,"compatibility":versions}),
            )?;
            component.typescript = Some(
                info["result"]["typescript"]
                    .as_bool()
                    .ok_or("Cannot infer component script language")?,
            );
        }
        let response = invoke(
            &bin_dir.join(format!("svelte-tool{}", std::env::consts::EXE_SUFFIX)),
            &json!({"op":"project","runes":request.runes,"typescript":component.typescript,"document":component.document,"compatibility":versions}),
        )?;
        if response["ok"] != true {
            return Ok(
                json!({"ok":false,"stage":"project","workspace":workspace,"diagnostics":diagnostics,"failure":response}),
            );
        }
        let (generated, projection) =
            write_projection(&workspace, &component, &response["result"])?;
        requires_js |= component.typescript == Some(false);
        projections.push(projection);
        if !component.check {
            tracked_files.push(workspace.join(&generated));
            auxiliary_components.push((workspace.join(&generated), component.document.clone()));
        }
        if component.check {
            files.push(json!(generated));
        }
    }
    let mut config = json!({"compilerOptions":{"strict":true,"noEmit":true,"target":"ESNext","module":"ESNext","moduleResolution":"Bundler","jsx":"preserve","allowJs":true,"checkJs":true,"allowArbitraryExtensions":true,"allowImportingTsExtensions":true,"types":[],"typeRoots":[workspace.join("__svelte/type-roots"),dependencies.join("@types")],"paths":paths},"files":files});
    if let Some(options) = request.compiler_options {
        let allow_js = options.get("allowJs").and_then(Value::as_bool).unwrap_or(
            options
                .get("checkJs")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        );
        if requires_js && !allow_js {
            return Err("JavaScript Svelte projections require allowJs in this overlay host; changing it would alter ordinary JS import checking".into());
        }
        let defaults = config["compilerOptions"]
            .as_object_mut()
            .ok_or("Missing compiler options")?;
        let declarations = defaults
            .get("paths")
            .cloned()
            .ok_or("Missing declarations")?;
        defaults.clear();
        defaults.insert("paths".into(), declarations);
        defaults.insert("jsx".into(), json!("preserve"));
        defaults.insert("allowArbitraryExtensions".into(), json!(true));
        defaults.insert("allowImportingTsExtensions".into(), json!(true));
        defaults.insert(
            "typeRoots".into(),
            json!([
                workspace.join("__svelte/type-roots"),
                dependencies.join("@types")
            ]),
        );
        for (key, value) in options {
            if key == "paths" {
                for (name, targets) in value.as_object().ok_or("paths must be an object")? {
                    if defaults["paths"].get(name).is_some() {
                        return Err(format!(
                            "Project paths conflict with Svelte declarations: {name}"
                        ));
                    }
                    defaults.get_mut("paths").unwrap()[name] = targets.clone();
                }
            } else {
                defaults.insert(key, value);
            }
        }
        // Project checking never emits or overwrites application files.
        defaults.insert("noEmit".into(), json!(true));
        defaults.insert("incremental".into(), json!(false));
        defaults.insert("composite".into(), json!(false));
    }
    write_new(&workspace, Path::new("tsconfig.json"), &config.to_string())?;
    tracked_files.extend(
        auxiliary_modules
            .iter()
            .map(|file| workspace.join(&file.path)),
    );
    let check_request = json!({"project":workspace.join("tsconfig.json"),"projections":projections,"tracked_files":tracked_files});
    if check_request.to_string().len() > 64 * 1024 * 1024 {
        return Ok(
            json!({"ok":false,"workspace":workspace,"error":{"code":"checker_size_limit","message":"Projected checker request exceeds 64 MiB; checker was not started"}}),
        );
    }
    if request.prepare_only {
        return Ok(
            json!({"ok":true,"hasErrors":false,"stage":"prepared","workspace":workspace,
            "project":workspace.join("tsconfig.json"),"projections":projections,
            "compilerDiagnostics":diagnostics,"trackedFiles":tracked_files}),
        );
    }
    let response = invoke(
        &bin_dir.join(format!("svelte-ts-check{}", std::env::consts::EXE_SUFFIX)),
        &check_request,
    )?;
    if response["ok"] == true {
        let loaded = response["loadedFiles"]
            .as_array()
            .ok_or("Missing loaded-file evidence")?;
        for file in auxiliary_modules {
            if !loaded.contains(&json!(workspace.join(&file.path))) {
                continue;
            }
            let result = check_rune_module(&file, bin_dir, &workspace, versions)?;
            if result["ok"] != true {
                return Ok(result);
            }
            diagnostics.extend(
                result["diagnostics"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default(),
            );
        }
        for (path, document) in auxiliary_components {
            if !loaded.contains(&json!(path)) {
                continue;
            }
            let compile = invoke(
                &bin_dir.join(format!("svelte-tool{}", std::env::consts::EXE_SUFFIX)),
                &json!({"op":"compile","target":"client","runes":request.runes,"document":document,"compatibility":versions}),
            )?;
            if compile["ok"] != true {
                return Ok(json!({"ok":false,"stage":"compile","failure":compile}));
            }
            diagnostics.extend(
                compile["result"]["diagnostics"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default(),
            );
        }
    }
    let has_errors = response["diagnostics"]
        .as_array()
        .is_none_or(|items| items.iter().any(|d| d["category"] == 1))
        || diagnostics.iter().any(|d| d["severity"] == "error");
    Ok(
        json!({"ok":response["ok"],"hasErrors":has_errors,"workspace":workspace,"stage":"check","compilerDiagnostics":diagnostics,"checker":response}),
    )
}

fn main() -> std::io::Result<()> {
    if std::env::args().nth(1).as_deref() == Some("--help") {
        println!(
            "Usage: svelte-project < request.json\nNative experimental project check. Request: workspace (new directory), dependencies (node_modules), components [path,document,typescript], optional files [path,text], allowPrerelease, runes (true by default; null infers).\nResolves exact installed Svelte/Kit/TypeScript profiles; requires sibling svelte-tool and svelte-ts-check binaries. No SvelteKit config/preprocessors yet."
        );
        return Ok(());
    }
    let mut input = Vec::new();
    std::io::stdin()
        .take(64 * 1024 * 1024 + 1)
        .read_to_end(&mut input)?;
    let result = if input.len() > 64 * 1024 * 1024 {
        Err("request exceeds 64 MiB".into())
    } else {
        serde_json::from_slice::<Request>(&input)
            .map_err(|e| e.to_string())
            .and_then(project)
    };
    let response = result.unwrap_or_else(
        |message| json!({"ok":false,"error":{"code":"project_failed","message":message}}),
    );
    let mut stdout = std::io::stdout().lock();
    writeln!(stdout, "{response}")?;
    stdout.flush()?;
    if response["ok"] != true || response["hasErrors"] != false {
        std::process::exit(1);
    }
    Ok(())
}
