//! Native discovery of a checked project using an existing Kit sync snapshot.
//! This consumes generated declarations; it does not execute config or run sync.
use glob::{MatchOptions, Pattern};
use serde::Serialize;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

const FILE_LIMIT: u64 = 1024 * 1024;
const GRAPH_LIMIT: usize = 64 * 1024 * 1024;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Input {
    pub path: String,
    pub kind: &'static str,
    pub root: bool,
    pub sha256: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Project {
    pub schema_version: u32,
    pub root: PathBuf,
    pub dependencies: PathBuf,
    pub compiler_options: Map<String, Value>,
    pub inputs: Vec<Input>,
    pub config_hashes: BTreeMap<String, String>,
    pub packages: BTreeMap<String, Value>,
    pub generated_types: Value,
    pub directories: BTreeMap<String, Vec<String>>,
}

impl Project {
    fn validate_identities(&self) -> Result<(), String> {
        for (path, expected) in &self.directories {
            let full = self
                .root
                .join(path)
                .canonicalize()
                .map_err(|e| e.to_string())?;
            if !full.starts_with(&self.root) {
                return Err("Directory escaped project".into());
            }
            let mut names = std::fs::read_dir(full)
                .map_err(|e| e.to_string())?
                .map(|entry| {
                    entry
                        .map_err(|e| e.to_string())?
                        .file_name()
                        .into_string()
                        .map_err(|_| "Non-UTF8 directory entry".to_owned())
                })
                .collect::<Result<Vec<_>, _>>()?;
            names.sort();
            if &names != expected {
                return Err(format!("Project directory changed after discovery: {path}"));
            }
        }
        for (path, expected) in &self.config_hashes {
            let full = checked_config_path(&self.root, path)?;
            if digest(read(&full)?.as_bytes()) != *expected {
                return Err(format!("Configuration changed after discovery: {path}"));
            }
        }
        for (name, package) in &self.packages {
            let text = read(&self.dependencies.join(name).join("package.json"))?;
            if package["sha256"] != digest(text.as_bytes()) {
                return Err(format!("Dependency changed after discovery: {name}"));
            }
        }
        Ok(())
    }
    /// Materialize a framework-neutral check request from this immutable snapshot.
    /// # Errors
    /// Rejects a changed source after discovery and non-UTF8 paths.
    pub fn check_request(&self, workspace: &Path, runes: Option<bool>) -> Result<Value, String> {
        self.validate_identities()?;
        let mut components = Vec::new();
        let mut files = Vec::new();
        for input in &self.inputs {
            let full = self
                .root
                .join(&input.path)
                .canonicalize()
                .map_err(|e| e.to_string())?;
            if !full.starts_with(&self.root) {
                return Err("Input symlink changed after discovery".into());
            }
            let text = read(&full)?;
            if digest(text.as_bytes()) != input.sha256 {
                return Err(format!("Source changed after discovery: {}", input.path));
            }
            if input.kind == "component" {
                let uri = url::Url::from_file_path(self.root.join(&input.path))
                    .map_err(|()| "Invalid file URI")?
                    .to_string();
                components.push(
                    json!({"path":input.path,"check":input.root,"document":{"uri":uri,"version":0,"text":text}}),
                );
            } else {
                files.push(json!({"path":input.path,"text":text,"check":input.root}));
            }
        }
        self.validate_identities()?;
        let request = json!({"workspace":workspace,"dependencies":self.dependencies,"components":components,"files":files,"compilerOptions":self.compiler_options,"runeModules":true,"runes":runes});
        if request.to_string().len() > GRAPH_LIMIT {
            return Err("Serialized request exceeds 64 MiB".into());
        }
        Ok(request)
    }

    /// Explicit editor initialization options; the plugin remains outside the tools crate.
    pub fn lsp_configuration(&self, runes: Option<bool>) -> Result<Value, String> {
        self.validate_identities()?;
        let mut guarded = self
            .config_hashes
            .iter()
            .filter(|(path, _)| !path.starts_with("node_modules/"))
            .map(|(path, hash)| (path.clone(), hash.clone()))
            .collect::<BTreeMap<_, _>>();
        for input in &self.inputs {
            let name = Path::new(&input.path)
                .file_name()
                .and_then(|p| p.to_str())
                .ok_or("Invalid input path")?;
            if input.path.starts_with(".svelte-kit/")
                || input.path.starts_with("src/params/")
                || (name.starts_with('+') && input.kind != "component")
                || name.starts_with("svelte.config.")
            {
                guarded.insert(input.path.clone(), input.sha256.clone());
            }
        }
        let mut dependencies = self
            .packages
            .iter()
            .map(|(name, p)| (format!("{name}/package.json"), p["sha256"].clone()))
            .collect::<Map<_, _>>();
        for (path, hash) in &self.config_hashes {
            if let Some(path) = path.strip_prefix("node_modules/") {
                dependencies.insert(path.to_owned(), json!(hash));
            }
        }
        Ok(
            json!({"root":self.root,"dependencies":self.dependencies,"components":self.inputs.iter().filter(|i|i.kind=="component").map(|i|json!({"path":i.path,"check":i.root})).collect::<Vec<_>>(),
            "files":self.inputs.iter().filter(|i|i.kind!="component" && i.root).map(|i|&i.path).collect::<Vec<_>>(),
            "auxiliaryFiles":self.inputs.iter().filter(|i|i.kind!="component" && !i.root).map(|i|&i.path).collect::<Vec<_>>(),
            "compilerOptions":self.compiler_options,"runeModules":true,"runes":runes,
            "snapshotGuard":{"files":guarded,"directories":self.directories,"dependencies":dependencies}}),
        )
    }
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn read(path: &Path) -> Result<String, String> {
    use std::io::Read;
    let file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if file.metadata().map_err(|e| e.to_string())?.len() > FILE_LIMIT {
        return Err(format!("Input exceeds 1 MiB: {}", path.display()));
    }
    let mut text = String::new();
    file.take(FILE_LIMIT + 1)
        .read_to_string(&mut text)
        .map_err(|e| e.to_string())?;
    if text.len() as u64 > FILE_LIMIT {
        return Err("Input grew beyond 1 MiB".into());
    }
    Ok(text)
}

fn relative(base: &str, value: &str) -> Result<String, String> {
    if value.contains('\\') || Path::new(value).is_absolute() {
        return Err(format!(
            "Only portable project-relative paths are supported: {value}"
        ));
    }
    let joined = Path::new(base).join(value);
    let mut parts = Vec::new();
    for part in joined.components() {
        match part {
            Component::Normal(name) => parts.push(name.to_str().ok_or("Non-UTF8 path")?),
            Component::CurDir => {}
            Component::ParentDir => {
                if parts.pop().is_none() {
                    return Err("Config path escapes project".into());
                }
            }
            _ => return Err("Unsupported path prefix".into()),
        }
    }
    Ok(if parts.is_empty() {
        ".".into()
    } else {
        parts.join("/")
    })
}

fn strings(value: &Value, name: &str) -> Result<Vec<String>, String> {
    value
        .as_array()
        .ok_or_else(|| format!("{name} must be an array"))?
        .iter()
        .map(|v| {
            v.as_str()
                .map(str::to_owned)
                .ok_or_else(|| format!("{name} requires strings"))
        })
        .collect()
}

#[derive(Default)]
struct Configuration {
    options: Map<String, Value>,
    include: Option<Vec<String>>,
    exclude: Option<Vec<String>>,
    files: Option<Vec<String>>,
    hashes: BTreeMap<String, String>,
}

// Explicit dependency directory/package links may resolve outside root.
// Keep lexical node_modules paths for rebasing and snapshot guard identities.
fn checked_config_path(root: &Path, path: &str) -> Result<PathBuf, String> {
    let full = root.join(path).canonicalize().map_err(|e| e.to_string())?;
    if full.starts_with(root) {
        return Ok(full);
    }
    if path.starts_with("node_modules/") {
        let dependencies = root
            .join("node_modules")
            .canonicalize()
            .map_err(|e| e.to_string())?;
        if full.starts_with(dependencies) {
            return Ok(full);
        }
        let parts = path
            .strip_prefix("node_modules/")
            .unwrap()
            .split('/')
            .collect::<Vec<_>>();
        let count = if parts[0].starts_with('@') { 2 } else { 1 };
        if parts.len() > count
            && parts
                .iter()
                .all(|p| !p.is_empty() && *p != "." && *p != ".." && !p.contains('\\'))
        {
            let package = root
                .join("node_modules")
                .join(parts[..count].join("/"))
                .canonicalize()
                .map_err(|e| e.to_string())?;
            if package.is_dir() && full.starts_with(&package) {
                return Ok(full);
            }
        }
    }
    Err(format!(
        "Configuration/declaration symlink escapes allowed roots: {path}"
    ))
}

// serde_json::Map need not preserve insertion order, but conditional exports
// must. Deserialize their small tree in source order without changing crate ABI.
#[derive(Debug)]
enum ExportTarget {
    String(String),
    Array(Vec<Self>),
    Object(Vec<(String, Self)>),
    Null,
}
impl<'de> serde::Deserialize<'de> for ExportTarget {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = ExportTarget;
            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a package export string, array, object, or null")
            }
            fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Self::Value, E> {
                Ok(ExportTarget::String(value.into()))
            }
            fn visit_none<E: serde::de::Error>(self) -> Result<Self::Value, E> {
                Ok(ExportTarget::Null)
            }
            fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
                Ok(ExportTarget::Null)
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut access: A,
            ) -> Result<Self::Value, A::Error> {
                let mut values = Vec::new();
                while let Some(value) = access.next_element()? {
                    values.push(value);
                }
                Ok(ExportTarget::Array(values))
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut access: A,
            ) -> Result<Self::Value, A::Error> {
                let mut values = Vec::new();
                while let Some(value) = access.next_entry()? {
                    values.push(value);
                }
                Ok(ExportTarget::Object(values))
            }
        }
        deserializer.deserialize_any(Visitor)
    }
}

fn export_file(
    root: &Path,
    package: &str,
    target: &ExportTarget,
    star: Option<&str>,
    depth: usize,
) -> Result<Option<String>, String> {
    if depth > 32 {
        return Err("Package export nesting exceeds 32".into());
    }
    match target {
        ExportTarget::Null => Ok(None),
        ExportTarget::String(value) => {
            let value = star.map_or_else(|| value.clone(), |star| value.replace('*', star));
            if !value.starts_with("./")
                || value
                    .split('/')
                    .any(|part| matches!(part, ".." | "node_modules"))
                || value.contains(['\\', '%'])
            {
                return Err(format!("Invalid package tsconfig export: {value}"));
            }
            let file = relative(package, &value)?;
            // Export targets do not gain guessed filenames/extensions.
            Ok((file.ends_with(".json") && root.join(&file).is_file()).then_some(file))
        }
        ExportTarget::Array(values) => {
            for value in values {
                if let Some(file) = export_file(root, package, value, star, depth + 1)? {
                    return Ok(Some(file));
                }
            }
            Ok(None)
        }
        ExportTarget::Object(values) => {
            for (condition, value) in values {
                if condition.starts_with("types@") {
                    return Err("Versioned types export conditions require an explicit TypeScript version resolver".into());
                }
                if matches!(condition.as_str(), "types" | "node" | "require" | "default") {
                    if let Some(file) = export_file(root, package, value, star, depth + 1)? {
                        return Ok(Some(file));
                    }
                }
            }
            Ok(None)
        }
    }
}

fn exported_config(
    root: &Path,
    package: &str,
    exports: &ExportTarget,
    subpath: &str,
) -> Result<Option<String>, String> {
    let ExportTarget::Object(entries) = exports else {
        return if subpath == "." {
            export_file(root, package, exports, None, 0)
        } else {
            Ok(None)
        };
    };
    let paths = entries.iter().any(|(key, _)| key.starts_with('.'));
    if !paths {
        return if subpath == "." {
            export_file(root, package, exports, None, 0)
        } else {
            Ok(None)
        };
    }
    if entries.iter().any(|(key, _)| !key.starts_with('.')) {
        return Err("Package exports mixes subpaths and conditions".into());
    }
    if let Some((_, value)) = entries.iter().find(|(key, _)| key == subpath) {
        return export_file(root, package, value, None, 0);
    }
    let mut patterns = entries
        .iter()
        .filter_map(|(key, value)| {
            let (prefix, suffix) = key.split_once('*')?;
            if suffix.contains('*')
                || subpath.len() < prefix.len() + suffix.len()
                || !subpath.starts_with(prefix)
                || !subpath.ends_with(suffix)
            {
                return None;
            }
            Some((prefix, suffix, value))
        })
        .collect::<Vec<_>>();
    patterns.sort_by(|a, b| {
        b.0.len()
            .cmp(&a.0.len())
            .then_with(|| b.1.len().cmp(&a.1.len()))
    });
    if let Some((prefix, suffix, value)) = patterns.first() {
        return export_file(
            root,
            package,
            value,
            Some(&subpath[prefix.len()..subpath.len() - suffix.len()]),
            0,
        );
    }
    Ok(None)
}

fn config_file(root: &Path, candidate: &str) -> Option<String> {
    if root.join(candidate).is_file() {
        return Some(candidate.into());
    }
    let json = format!("{candidate}.json");
    (!candidate.ends_with(".json") && root.join(&json).is_file()).then_some(json)
}

fn resolve_extends(
    root: &Path,
    base: &str,
    specifier: &str,
    hashes: &mut BTreeMap<String, String>,
) -> Result<String, String> {
    if specifier.starts_with('.') {
        let candidate = relative(base, specifier)?;
        return config_file(root, &candidate)
            .ok_or_else(|| format!("Missing extended config: {specifier}"));
    }
    if specifier.is_empty()
        || specifier.starts_with(['/', '#'])
        || specifier.contains('\\')
        || specifier.split('/').any(|p| matches!(p, "" | "." | ".."))
    {
        return Err(format!("Invalid package tsconfig specifier: {specifier}"));
    }
    let parts = specifier.split('/').collect::<Vec<_>>();
    let package_parts = if specifier.starts_with('@') { 2 } else { 1 };
    if parts.len() < package_parts {
        return Err("Scoped package needs a name".into());
    }
    let package_name = parts[..package_parts].join("/");
    let rest = parts[package_parts..].join("/");
    let subpath = if rest.is_empty() {
        ".".into()
    } else {
        format!("./{rest}")
    };
    #[derive(serde::Deserialize)]
    struct PackageConfig {
        tsconfig: Option<String>,
        exports: Option<ExportTarget>,
        #[serde(rename = "typesVersions")]
        types_versions: Option<Value>,
    }
    let mut ancestor = Path::new(base);
    loop {
        // Node skips node_modules/node_modules while ascending dependencies.
        if ancestor
            .file_name()
            .is_none_or(|name| name != "node_modules")
        {
            let modules = relative(
                ancestor.to_str().ok_or("Non-UTF8 config parent")?,
                "node_modules",
            )?;
            let package = format!("{modules}/{package_name}");
            let manifest = format!("{package}/package.json");
            let metadata = if root.join(&manifest).is_file() {
                let text = read(&checked_config_path(root, &manifest)?)?;
                hashes.insert(manifest, digest(text.as_bytes()));
                Some(
                    serde_json::from_str::<PackageConfig>(&text)
                        .map_err(|e| format!("Invalid package metadata: {e}"))?,
                )
            } else {
                None
            };
            if let Some(exports) = metadata.as_ref().and_then(|m| m.exports.as_ref()) {
                return exported_config(root, &package, exports, &subpath)?
                    .ok_or_else(|| format!("Package does not export tsconfig {specifier}"));
            }
            if metadata
                .as_ref()
                .is_some_and(|m| m.types_versions.is_some())
            {
                return Err("Package tsconfig typesVersions requires an explicit TypeScript version resolver".into());
            }
            let candidate = if rest.is_empty() {
                package.clone()
            } else {
                format!("{package}/{rest}")
            };
            if let Some(file) = config_file(root, &candidate) {
                return Ok(file);
            }
            if rest.is_empty() {
                if let Some(target) = metadata.as_ref().and_then(|m| m.tsconfig.as_ref()) {
                    let target = relative(&package, target)?;
                    if !target.starts_with(&format!("{package}/")) {
                        return Err("Package tsconfig field escapes package".into());
                    }
                    if let Some(file) = config_file(root, &target) {
                        return Ok(file);
                    }
                }
            }
            let index = format!("{candidate}/tsconfig.json");
            if root.join(&index).is_file() {
                return Ok(index);
            }
        }
        let Some(parent) = ancestor.parent() else {
            break;
        };
        ancestor = parent;
    }
    Err(format!(
        "Cannot resolve package tsconfig within project dependencies: {specifier}"
    ))
}

fn load_config(
    root: &Path,
    path: &str,
    active: &mut BTreeSet<String>,
) -> Result<Configuration, String> {
    let full = checked_config_path(root, path)?;
    let identity = full.to_str().ok_or("Non-UTF8 config path")?.to_owned();
    if active.len() >= 32 || !active.insert(identity.clone()) {
        return Err("Cyclic/deep tsconfig extends".into());
    }
    let text = read(&full)?;
    let value = super::jsonc::parse(&text)?;
    let object = value.as_object().ok_or("tsconfig must be an object")?;
    if object.contains_key("references") {
        return Err("Project references are not yet supported".into());
    }
    let base = Path::new(path)
        .parent()
        .and_then(Path::to_str)
        .ok_or("Invalid config path")?;
    let mut result = Configuration::default();
    if let Some(extends) = object.get("extends") {
        let parents = if extends.is_string() {
            vec![extends.as_str().unwrap().to_owned()]
        } else {
            strings(extends, "extends")?
        };
        for parent in parents {
            let parent = resolve_extends(root, base, &parent, &mut result.hashes)?;
            let inherited = load_config(root, &parent, active)?;
            result.options.extend(inherited.options);
            if inherited.include.is_some() {
                result.include = inherited.include;
            }
            if inherited.exclude.is_some() {
                result.exclude = inherited.exclude;
            }
            if inherited.files.is_some() {
                result.files = inherited.files;
            }
            result.hashes.extend(inherited.hashes);
        }
    }
    if let Some(options) = object.get("compilerOptions") {
        let options = options
            .as_object()
            .ok_or("compilerOptions must be an object")?;
        // baseUrl changes the interpretation of paths; reject until it is
        // resolved jointly across inherited configs, rather than guessing.
        if options.contains_key("baseUrl") {
            return Err("baseUrl is not yet supported; use explicit paths".into());
        }
        for (key, value) in options {
            let value = match key.as_str() {
                "paths" => {
                    let paths = value.as_object().ok_or("paths must be an object")?;
                    let mut mapped = Map::new();
                    for (name, targets) in paths {
                        mapped.insert(
                            name.clone(),
                            json!(
                                strings(targets, "paths")?
                                    .iter()
                                    .map(|p| relative(base, p).map(|p| format!("./{p}")))
                                    .collect::<Result<Vec<_>, _>>()?
                            ),
                        );
                    }
                    Value::Object(mapped)
                }
                "rootDirs" | "typeRoots" => json!(
                    strings(value, key)?
                        .iter()
                        .map(|p| relative(base, p))
                        .collect::<Result<Vec<_>, _>>()?
                ),
                "rootDir" | "outDir" | "declarationDir" | "tsBuildInfoFile" => json!(relative(
                    base,
                    value.as_str().ok_or("Path option requires string")?
                )?),
                "plugins" => {
                    return Err("TypeScript configuration plugins require an explicit host".into());
                }
                _ => value.clone(),
            };
            result.options.insert(key.clone(), value);
        }
    }
    for (name, destination) in [
        ("include", &mut result.include),
        ("exclude", &mut result.exclude),
        ("files", &mut result.files),
    ] {
        if let Some(value) = object.get(name) {
            *destination = Some(
                strings(value, name)?
                    .iter()
                    .map(|p| relative(base, p))
                    .collect::<Result<Vec<_>, _>>()?,
            );
        }
    }
    result.hashes.insert(path.into(), digest(text.as_bytes()));
    active.remove(&identity);
    Ok(result)
}

fn pattern(value: &str) -> Result<Pattern, String> {
    // TypeScript treats route brackets literally; glob's bracket syntax differs.
    let escaped = value
        .chars()
        .map(|c| match c {
            '[' => "[[]".into(),
            ']' => "[]]".into(),
            _ => c.to_string(),
        })
        .collect::<String>();
    Pattern::new(&escaped).map_err(|e| e.to_string())
}
fn options() -> MatchOptions {
    MatchOptions {
        case_sensitive: true,
        require_literal_separator: true,
        require_literal_leading_dot: true,
    }
}
fn expanded(root: &Path, value: &str) -> String {
    if !value.contains(['*', '?'])
        && (root.join(value).is_dir() || Path::new(value).extension().is_none())
    {
        format!("{value}/**/*")
    } else {
        value.into()
    }
}
fn matches(root: &Path, patterns: &[String], value: &str) -> Result<bool, String> {
    for p in patterns {
        if pattern(&expanded(root, p))?.matches_with(value, options()) {
            return Ok(true);
        }
    }
    Ok(false)
}
fn kind(path: &str) -> Option<&'static str> {
    if path.ends_with(".svelte") {
        Some("component")
    } else if path.ends_with(".svelte.ts") || path.ends_with(".svelte.js") {
        Some("runeModule")
    } else if path.ends_with(".d.ts") || path.ends_with(".d.mts") || path.ends_with(".d.cts") {
        Some("declaration")
    } else if [".ts", ".js", ".tsx", ".jsx", ".mts", ".cts", ".mjs", ".cjs"]
        .iter()
        .any(|e| path.ends_with(e))
    {
        Some("script")
    } else if path.ends_with(".json") {
        Some("auxiliary")
    } else {
        None
    }
}

// TypeScript wildcard roots prefer source files over same-basename emitted JS.
// Explicit `files` entries are never removed by extension precedence.
fn prioritize_roots(wildcard: &mut BTreeSet<String>, explicit: &BTreeSet<String>) {
    let all: BTreeSet<_> = wildcard.union(explicit).cloned().collect();
    wildcard.retain(|name| {
        for group in [
            &[".ts", ".tsx", ".d.ts", ".js", ".jsx"][..],
            &[".cts", ".d.cts", ".cjs"][..],
            &[".mts", ".d.mts", ".mjs"][..],
        ] {
            let Some((index, extension)) = group.iter().enumerate().find(|(_, ext)| {
                name.ends_with(**ext)
                    && !(**ext == ".ts" && name.ends_with(".d.ts"))
                    && !(**ext == ".cts" && name.ends_with(".d.cts"))
                    && !(**ext == ".mts" && name.ends_with(".d.mts"))
            }) else {
                continue;
            };
            let base = &name[..name.len() - extension.len()];
            return !group[..index].iter().any(|higher| {
                // A declaration and JS implementation may both be roots.
                !(*higher == ".d.ts" && matches!(*extension, ".js" | ".jsx"))
                    && all.contains(&format!("{base}{higher}"))
            });
        }
        true
    });
}

fn collect(root: &Path, patterns: &[String], names: &mut BTreeSet<String>) -> Result<(), String> {
    let root_pattern = Pattern::escape(root.to_str().ok_or("Non-UTF8 root")?);
    for p in patterns {
        let suffix = pattern(&expanded(root, p))?.as_str().to_owned();
        for entry in glob::glob_with(&format!("{root_pattern}/{suffix}"), options())
            .map_err(|e| e.to_string())?
        {
            let path = entry.map_err(|e| e.to_string())?;
            if !path.is_file() {
                continue;
            }
            let name = path
                .strip_prefix(root)
                .map_err(|e| e.to_string())?
                .to_str()
                .ok_or("Non-UTF8 input")?
                .replace('\\', "/");
            if name
                .split('/')
                .any(|p| matches!(p, "node_modules" | ".git" | ".worktrees"))
            {
                continue;
            }
            if kind(&name).is_some() {
                names.insert(name);
            }
            if names.len() > 4096 {
                return Err("Project exceeds 4096 inputs".into());
            }
        }
    }
    Ok(())
}

fn collect_auxiliary(
    root: &Path,
    directory: &Path,
    names: &mut BTreeSet<String>,
    visited: &mut usize,
    directories: &mut BTreeMap<String, Vec<String>>,
) -> Result<(), String> {
    let entries = std::fs::read_dir(directory)
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    let mut roster = entries
        .iter()
        .map(|e| {
            e.file_name()
                .into_string()
                .map_err(|_| "Non-UTF8 directory entry".to_string())
        })
        .collect::<Result<Vec<_>, _>>()?;
    roster.sort();
    let relative = directory
        .strip_prefix(root)
        .map_err(|e| e.to_string())?
        .to_str()
        .ok_or("Non-UTF8 directory")?;
    directories.insert(
        if relative.is_empty() {
            ".".into()
        } else {
            relative.into()
        },
        roster,
    );
    for entry in entries {
        *visited += 1;
        if *visited > 100_000 {
            return Err("Project discovery exceeds 100000 directory entries".into());
        }
        let path = entry.path();
        let name = path
            .strip_prefix(root)
            .map_err(|e| e.to_string())?
            .to_str()
            .ok_or("Non-UTF8 input")?
            .replace('\\', "/");
        let file_name = entry.file_name().to_string_lossy().into_owned();
        let file_type = entry.file_type().map_err(|e| e.to_string())?;
        if file_type.is_dir() {
            if matches!(
                file_name.as_str(),
                "node_modules" | "build" | "dist" | "target" | "static"
            ) || (file_name.starts_with('.') && file_name != ".svelte-kit")
            {
                continue;
            }
            if name.starts_with(".svelte-kit/")
                && name != ".svelte-kit/types"
                && !name.starts_with(".svelte-kit/types/")
            {
                continue;
            }
            collect_auxiliary(root, &path, names, visited, directories)?;
        } else if file_type.is_symlink() {
            if kind(&name).is_some() {
                return Err(format!(
                    "Auxiliary source symlinks require an explicit input: {name}"
                ));
            }
        } else if kind(&name).is_some() && file_name != "tsconfig.json" {
            names.insert(name);
            if names.len() > 4096 {
                return Err("Project exceeds 4096 source/auxiliary inputs".into());
            }
        }
    }
    Ok(())
}

/// Discover exact source/config inputs without evaluating JavaScript or reading env values.
/// # Errors
/// Rejects missing sync output, unsupported config, escaping paths and resource limits.
pub fn discover(root: &Path) -> Result<Project, String> {
    let root = root.canonicalize().map_err(|e| e.to_string())?;
    let mut config = load_config(&root, "tsconfig.json", &mut BTreeSet::new())?;
    let dependencies = root
        .join("node_modules")
        .canonicalize()
        .map_err(|e| e.to_string())?;
    let mut packages = BTreeMap::new();
    for name in ["svelte", "@sveltejs/kit", "typescript", "svelte-check"] {
        let text = read(&dependencies.join(name).join("package.json"))?;
        let package: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
        let version = package["version"]
            .as_str()
            .ok_or("Missing dependency version")?;
        packages.insert(
            name.into(),
            json!({"version":version,"sha256":digest(text.as_bytes())}),
        );
    }
    let kit_major = packages["@sveltejs/kit"]["version"]
        .as_str()
        .and_then(|v| v.split('.').next())
        .and_then(|v| v.parse::<u64>().ok())
        .ok_or("Invalid installed SvelteKit version")?;
    let mut dependency_declarations = Vec::new();
    match kit_major {
        2 => {
            if !config.hashes.contains_key(".svelte-kit/tsconfig.json") {
                return Err("Expected existing .svelte-kit/tsconfig.json in extends chain; run Kit sync in an isolated workspace".into());
            }
            for name in [".svelte-kit/ambient.d.ts", ".svelte-kit/non-ambient.d.ts"] {
                if !root.join(name).is_file() {
                    return Err(format!("Missing synced Kit declaration: {name}"));
                }
            }
        }
        3 => {
            if ![
                "node_modules/$app/tsconfig.json",
                "node_modules/$app/tsconfig/service-worker.json",
            ]
            .iter()
            .any(|path| config.hashes.contains_key(*path))
            {
                return Err("Expected $app/tsconfig or $app/tsconfig/service-worker in the extends chain; run Kit 3 sync in an isolated workspace".into());
            }
            // Kit 3's generated config uses types: ['$app/types']. The checker
            // resolves this through linked node_modules; do not copy dependency
            // source into ordinary inputs or replace declarations with stubs.
            for name in [
                "node_modules/$app/types/index.d.ts",
                "node_modules/$app/types/env.d.ts",
            ] {
                let text = read(&checked_config_path(&root, name)?)?;
                config.hashes.insert(name.into(), digest(text.as_bytes()));
                dependency_declarations.push(name);
            }
        }
        _ => {
            return Err(format!(
                "Unsupported SvelteKit generated-type layout for major {kit_major}"
            ));
        }
    }
    let include = config.include.clone().unwrap_or_else(|| {
        if config.files.is_some() {
            vec![]
        } else {
            vec!["**/*".into()]
        }
    });
    let exclude = config
        .exclude
        .clone()
        .unwrap_or_else(|| vec!["node_modules".into()]);
    let mut candidates = BTreeSet::new();
    collect(&root, &include, &mut candidates)?;
    let explicit = config
        .files
        .clone()
        .unwrap_or_default()
        .into_iter()
        .collect::<BTreeSet<_>>();
    let mut roots = BTreeSet::new();
    let allow_js = config
        .options
        .get("allowJs")
        .and_then(Value::as_bool)
        .unwrap_or(
            config
                .options
                .get("checkJs")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        );
    let json_includes: Vec<_> = include
        .iter()
        .filter(|p| p.ends_with(".json"))
        .cloned()
        .collect();
    for name in candidates {
        if [".js", ".jsx", ".mjs", ".cjs"]
            .iter()
            .any(|ext| name.ends_with(ext))
            && !allow_js
        {
            continue;
        }
        if kind(&name) == Some("auxiliary")
            && (config.options.get("resolveJsonModule") != Some(&Value::Bool(true))
                || !matches(&root, &json_includes, &name)?)
        {
            continue;
        }
        if !matches(&root, &exclude, &name)? {
            roots.insert(name);
        }
    }
    prioritize_roots(&mut roots, &explicit);
    for name in &explicit {
        if name.split('/').any(|part| part == "node_modules") {
            return Err(format!(
                "Explicit dependency files require a dependency-root input contract; use the package's declared types entry: {name}"
            ));
        }
        if kind(name).is_none() || !root.join(name).is_file() {
            return Err(format!("Invalid explicit input: {name}"));
        }
        roots.insert(name.clone());
    }
    let mut names = roots.clone();
    let mut directories = BTreeMap::new();
    // Exclusion controls roots, not imported sources. Preserve a bounded source
    // superset so imports can still reach excluded files and JSON outside src.
    collect_auxiliary(&root, &root, &mut names, &mut 0, &mut directories)?;
    // Generated proxy modules are imported by $types, not necessarily tsconfig roots.
    collect(
        &root,
        &[".svelte-kit/types/**/*.ts".into(), "src/**/*.json".into()],
        &mut names,
    )?;
    names.insert("package.json".into());
    let mut bytes = 0;
    let mut inputs = Vec::new();
    for name in names {
        let full = root.join(&name).canonicalize().map_err(|e| e.to_string())?;
        if !full.starts_with(&root) {
            return Err(format!("Project input symlink escapes root: {name}"));
        }
        let text = read(&full)?;
        bytes += text.len();
        if bytes > GRAPH_LIMIT {
            return Err("Project source exceeds 64 MiB".into());
        }
        inputs.push(Input {
            kind: kind(&name).ok_or("Unsupported input")?,
            root: roots.contains(&name),
            sha256: digest(text.as_bytes()),
            path: name,
        });
    }
    let generated = inputs
        .iter()
        .filter(|i| i.path.starts_with(".svelte-kit/"))
        .count();
    Ok(Project {
        directories,
        schema_version: 1,
        root,
        dependencies,
        compiler_options: config.options,
        inputs,
        config_hashes: config.hashes,
        packages,
        generated_types: json!({"origin":"existing-kit-sync","kitMajor":kit_major,"files":generated + dependency_declarations.len(),"dependencyDeclarations":dependency_declarations,"freshness":"snapshot-only; regenerate with Kit sync after route/load/config changes","markdownComponents":"outside pinned svelte-check component-root coverage"}),
    })
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let root = std::env::temp_dir().join(format!(
                "ts-svelte-project-config-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&root).unwrap();
            Self(root.canonicalize().unwrap())
        }
        fn write(&self, name: &str, text: &str) {
            let path = self.0.join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
        fn config(&self) -> Result<super::Configuration, String> {
            super::load_config(&self.0, "tsconfig.json", &mut Default::default())
        }
        fn packages(&self, kit_version: &str) {
            self.write("package.json", r#"{"name":"fixture","private":true}"#);
            for (name, version) in [
                ("svelte", "5.55.0"),
                ("@sveltejs/kit", kit_version),
                ("typescript", "5.9.3"),
                ("svelte-check", "4.4.5"),
            ] {
                self.write(
                    &format!("node_modules/{name}/package.json"),
                    &serde_json::json!({"name":name,"version":version}).to_string(),
                );
            }
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }

    #[test]
    fn package_tsconfig_field_and_jsonc_rebase() {
        let fixture = Fixture::new();
        fixture.write(
            "tsconfig.json",
            r#"{"extends":"@config/base","compilerOptions":{"strict":false}}"#,
        );
        fixture.write(
            "node_modules/@config/base/package.json",
            r#"{"tsconfig":"config/base.json"}"#,
        );
        fixture.write("node_modules/@config/base/config/base.json", r#"{
            // All relative paths are rebased from this file, not the package root.
            "compilerOptions":{"strict":true,"rootDirs":["../../../.."],"paths":{"$lib/*":["../../../../src/lib/*"]}},
            "include":["../../../../src"],
        }"#);
        let config = fixture.config().unwrap();
        assert_eq!(config.options["paths"]["$lib/*"][0], "./src/lib/*");
        assert_eq!(config.options["rootDirs"][0], ".");
        assert_eq!(config.options["strict"], false);
        assert_eq!(config.include.unwrap(), ["src"]);
        assert!(
            config
                .hashes
                .contains_key("node_modules/@config/base/package.json")
        );
    }

    #[test]
    fn package_exports_preserve_condition_order_and_subpath_patterns() {
        let fixture = Fixture::new();
        fixture.write(
            "tsconfig.json",
            r#"{"extends":["settings","settings/modes/browser"]}"#,
        );
        fixture.write(
            "node_modules/settings/package.json",
            r#"{"exports":{
            ".":{"types":"./first.json","default":"./wrong.json"},
            "./modes/*":{"require":"./configs/*.json"}
        }}"#,
        );
        fixture.write(
            "node_modules/settings/first.json",
            r#"{"compilerOptions":{"strict":true}}"#,
        );
        fixture.write(
            "node_modules/settings/wrong.json",
            r#"{"compilerOptions":{"strict":false}}"#,
        );
        fixture.write(
            "node_modules/settings/configs/browser.json",
            r#"{"compilerOptions":{"lib":["DOM"]}}"#,
        );
        let config = fixture.config().unwrap();
        assert_eq!(config.options["strict"], true);
        assert_eq!(config.options["lib"][0], "DOM");
        assert!(
            !config
                .hashes
                .contains_key("node_modules/settings/wrong.json")
        );
        fixture.write("tsconfig.json", r#"{"extends":"settings/private"}"#);
        assert!(fixture.config().err().unwrap().contains("does not export"));
    }

    #[test]
    fn direct_kit3_config_and_service_worker_subpath() {
        let fixture = Fixture::new();
        fixture.write(
            "tsconfig.json",
            r#"{"extends":"$app/tsconfig","include":["src"]}"#,
        );
        fixture.write("node_modules/$app/tsconfig.json", r#"{"compilerOptions":{"types":["$app/types"],"rootDirs":["../..","../../.svelte-kit/types"]}}"#);
        fixture.write(
            "node_modules/$app/tsconfig/service-worker.json",
            r#"{"compilerOptions":{"lib":["ESNext","WebWorker"]}}"#,
        );
        let config = fixture.config().unwrap();
        assert_eq!(config.options["types"][0], "$app/types");
        assert_eq!(
            config.options["rootDirs"],
            serde_json::json!([".", ".svelte-kit/types"])
        );
        fixture.write(
            "tsconfig.json",
            r#"{"extends":"$app/tsconfig/service-worker"}"#,
        );
        assert_eq!(fixture.config().unwrap().options["lib"][1], "WebWorker");
    }

    #[test]
    fn package_config_cycle_and_escape_are_rejected() {
        let fixture = Fixture::new();
        fixture.write("tsconfig.json", r#"{"extends":"settings"}"#);
        fixture.write(
            "node_modules/settings/tsconfig.json",
            r#"{"extends":"../../tsconfig.json"}"#,
        );
        assert!(fixture.config().err().unwrap().contains("Cyclic"));
        fixture.write(
            "node_modules/settings/package.json",
            r#"{"exports":"./../../outside.json"}"#,
        );
        assert!(
            fixture
                .config()
                .err()
                .unwrap()
                .contains("Invalid package tsconfig export")
        );
    }

    #[test]
    fn kit3_discovery_keeps_generated_types_in_dependencies_and_guards_them() {
        let fixture = Fixture::new();
        fixture.packages("3.0.1");
        fixture.write(
            "tsconfig.json",
            r#"{"extends":"$app/tsconfig","include":["src"]}"#,
        );
        fixture.write("node_modules/$app/tsconfig.json", r#"{"compilerOptions":{"types":["$app/types"],"allowJs":true,"rootDirs":["../..","../../.svelte-kit/types"]}}"#);
        fixture.write(
            "node_modules/$app/types/index.d.ts",
            "import './env.js'; export type RouteId = '/';\n",
        );
        fixture.write(
            "node_modules/$app/types/env.d.ts",
            "declare module '$app/env/public' { export const PUBLIC_SITE: string; }\n",
        );
        fixture.write("src/main.ts", "export const main = 1;\n");
        let project = super::discover(&fixture.0).unwrap();
        assert_eq!(project.generated_types["kitMajor"], 3);
        assert!(
            project
                .inputs
                .iter()
                .all(|input| !input.path.starts_with("node_modules/"))
        );
        assert_eq!(project.compiler_options["types"][0], "$app/types");
        let config = project.lsp_configuration(None).unwrap();
        assert!(config["snapshotGuard"]["dependencies"]["$app/types/env.d.ts"].is_string());
        assert!(
            config["snapshotGuard"]["files"]
                .get("node_modules/$app/types/env.d.ts")
                .is_none()
        );
        let request = project
            .check_request(Path::new("/unused/project-overlay"), None)
            .unwrap();
        assert!(
            request["files"]
                .as_array()
                .unwrap()
                .iter()
                .all(|file| !file["path"].as_str().unwrap().starts_with("node_modules/"))
        );
        fixture.write("node_modules/$app/types/env.d.ts", "// changed\n");
        assert!(
            project
                .lsp_configuration(None)
                .err()
                .unwrap()
                .contains("changed after discovery")
        );
    }

    #[cfg(unix)]
    #[test]
    fn linked_dependency_configs_are_allowed_but_arbitrary_symlinks_are_not() {
        let fixture = Fixture::new();
        let dependencies = Fixture::new();
        dependencies.write(
            "settings/tsconfig.json",
            r#"{"compilerOptions":{"strict":true}}"#,
        );
        std::os::unix::fs::symlink(&dependencies.0, fixture.0.join("node_modules")).unwrap();
        fixture.write("tsconfig.json", r#"{"extends":"settings"}"#);
        assert_eq!(fixture.config().unwrap().options["strict"], true);
        std::os::unix::fs::symlink(
            dependencies.0.join("settings/tsconfig.json"),
            fixture.0.join("outside.json"),
        )
        .unwrap();
        fixture.write("tsconfig.json", r#"{"extends":"./outside.json"}"#);
        assert!(
            fixture
                .config()
                .err()
                .unwrap()
                .contains("escapes allowed roots")
        );
    }

    #[cfg(unix)]
    #[test]
    fn linked_package_configs_are_bounded_by_the_resolved_package() {
        let fixture = Fixture::new();
        let package = Fixture::new();
        fixture.write("node_modules/.keep", "");
        package.write("tsconfig.json", r#"{"compilerOptions":{"strict":true}}"#);
        std::os::unix::fs::symlink(&package.0, fixture.0.join("node_modules/settings")).unwrap();
        fixture.write("tsconfig.json", r#"{"extends":"settings"}"#);
        assert_eq!(fixture.config().unwrap().options["strict"], true);
        let outside = Fixture::new();
        outside.write("tsconfig.json", "{}");
        std::os::unix::fs::symlink(
            outside.0.join("tsconfig.json"),
            package.0.join("escape.json"),
        )
        .unwrap();
        assert!(
            super::checked_config_path(&fixture.0, "node_modules/settings/escape.json").is_err()
        );
    }

    #[test]
    fn wildcard_extension_priority_preserves_explicit_files() {
        let mut names = [
            "a.ts", "a.js", "b.d.ts", "b.js", "c.tsx", "c.jsx", "d.mts", "d.mjs", "e.cts", "e.cjs",
            "f.svelte",
        ]
        .map(str::to_owned)
        .into_iter()
        .collect();
        let explicit = ["a.js", "g.ts"].map(str::to_owned).into_iter().collect();
        super::prioritize_roots(&mut names, &explicit);
        names.extend(explicit);
        assert_eq!(
            names,
            [
                "a.ts", "a.js", "b.d.ts", "b.js", "c.tsx", "d.mts", "e.cts", "f.svelte", "g.ts"
            ]
            .map(str::to_owned)
            .into_iter()
            .collect()
        );
    }
    #[test]
    fn rebasing_and_route_globs() {
        assert_eq!(
            super::relative(".svelte-kit", "../src/lib/*").unwrap(),
            "src/lib/*"
        );
        assert!(super::relative("", "../escape").is_err());
        let p = super::pattern("src/routes/[id]/**/*.ts").unwrap();
        assert!(p.matches_with("src/routes/[id]/+page.ts", super::options()));
        assert!(!p.matches_with("src/routes/i/+page.ts", super::options()));
    }
}
