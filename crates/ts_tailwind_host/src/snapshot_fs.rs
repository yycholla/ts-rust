//! Immutable source overlays. Files retain real path identities; an extracted
//! stylesheet is a separate document and never shadows its component source.
use super::*;
type SourceMap = oxc_sourcemap::SourceMap<'static>;
use oxc_resolver::{FileMetadata, FileSystem, FileSystemOs, ResolveError};
use std::{io, sync::Arc};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct HostSnapshots {
    pub files: Vec<FileSnapshot>,
    pub entry: Option<EntryStylesheet>,
    pub resolver: ResolverProfile,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FileSnapshot {
    pub path: PathBuf,
    pub version: i64,
    pub text: String,
    #[serde(default)]
    pub map: Option<serde_json::Value>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EntryStylesheet {
    pub id: String,
    pub origin: PathBuf,
    pub version: i64,
    pub css: String,
    #[serde(default)]
    pub map: Option<serde_json::Value>,
}
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ResolverProfile {
    #[default]
    Node,
    ViteDevelopment,
    ViteProduction,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct SnapshotFs {
    pub files: Arc<BTreeMap<PathBuf, FileSnapshot>>,
    pub dirs: Arc<BTreeSet<PathBuf>>,
}
/// Follow existing symlinks before processing `..`; preserve nonexistent suffixes.
/// This is intentionally not lexical cleanup before realpath.
pub(crate) fn identity(path: &Path) -> io::Result<PathBuf> {
    if !path.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Snapshot path must be absolute",
        ));
    }
    let mut result = PathBuf::new();
    for part in path.components() {
        use std::path::Component;
        match part {
            Component::CurDir => (),
            Component::ParentDir => {
                if !result.pop() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "Path escapes filesystem root",
                    ));
                }
            }
            _ => {
                result.push(part.as_os_str());
                match fs::symlink_metadata(&result) {
                    Ok(_) => result = dunce::canonicalize(&result)?,
                    Err(e) if e.kind() == io::ErrorKind::NotFound => (),
                    Err(e) => return Err(e),
                }
            }
        }
    }
    Ok(result)
}
impl SnapshotFs {
    pub fn prepare(input: &HostSnapshots, limits: &HostLimits) -> Result<Self, HostError> {
        if input.files.len() > limits.files {
            return Err(fail("limit", Path::new(""), "Too many source snapshots"));
        }
        let mut files = BTreeMap::new();
        let mut dirs = BTreeSet::new();
        let mut total = 0usize;
        for file in &input.files {
            let path = identity(&file.path).map_err(|e| fail("path", &file.path, e))?;
            path_string(&path)?;
            if path.ancestors().any(environment_file) {
                return Err(fail(
                    "protectedInput",
                    &path,
                    "Environment snapshots are not permitted",
                ));
            }
            // These affect traversal or package interpretation. They must not be
            // accepted then silently read from disk by the ignore implementation.
            if path
                .components()
                .any(|p| p.as_os_str() == ".git" || p.as_os_str() == ".jj")
                || path
                    .file_name()
                    .is_some_and(|n| n == ".ignore" || n == ".gitignore")
            {
                return Err(fail(
                    "unsupported",
                    &path,
                    "Ignore/repository metadata overlays require an ignore-file snapshot provider",
                ));
            }
            if path.is_dir() {
                return Err(fail(
                    "snapshot",
                    &path,
                    "File snapshot collides with a directory",
                ));
            }
            total = total.saturating_add(file.text.len());
            if file.text.len() > limits.file_bytes || total > limits.total_bytes {
                return Err(fail("limit", &path, "Source snapshots exceed byte limits"));
            }
            if let Some(map) = &file.map {
                total = total.saturating_add(
                    serde_json::to_vec(map)
                        .map_err(|e| fail("sourceMap", &path, e))?
                        .len(),
                );
                if total > limits.total_bytes {
                    return Err(fail(
                        "limit",
                        &path,
                        "Snapshot maps exceed total byte limit",
                    ));
                }
                validate_map_for_text(map, &file.text, limits)
                    .map_err(|e| fail("sourceMap", &path, e))?;
            }
            let mut file = file.clone();
            file.path = path.clone();
            if files.insert(path.clone(), file).is_some() {
                return Err(fail(
                    "snapshot",
                    &path,
                    "Duplicate or aliased snapshot identity",
                ));
            }
            for parent in path.ancestors().skip(1) {
                dirs.insert(parent.to_owned());
            }
        }
        if let Some(path) = files.keys().find(|path| dirs.contains(*path)) {
            return Err(fail(
                "snapshot",
                path,
                "File snapshot collides with another snapshot's directory",
            ));
        }
        if dirs.len() > limits.directories {
            return Err(fail(
                "limit",
                Path::new(""),
                "Snapshot directories exceed limit",
            ));
        }
        if let Some(entry) = &input.entry {
            let origin = identity(&entry.origin).map_err(|e| fail("path", &entry.origin, e))?;
            if entry.id == path_string(&origin)?
                || files.contains_key(Path::new(&entry.id))
                || Path::new(&entry.id).exists()
            {
                return Err(fail(
                    "snapshot",
                    &entry.origin,
                    "Extracted stylesheet identity must be distinct from real source files",
                ));
            }
            if !Path::new(&entry.id).is_absolute() || entry.id.contains('\0') || entry.id.is_empty()
            {
                return Err(fail(
                    "snapshot",
                    &entry.origin,
                    "Extracted stylesheet id must be an absolute document identity",
                ));
            }
            if entry.origin.ancestors().any(environment_file) {
                return Err(fail(
                    "protectedInput",
                    &entry.origin,
                    "Environment entry is not permitted",
                ));
            }
            if entry.css.len() > limits.file_bytes
                || total.saturating_add(entry.css.len()) > limits.total_bytes
            {
                return Err(fail(
                    "limit",
                    &entry.origin,
                    "Entry stylesheet exceeds snapshot limits",
                ));
            }
            if let Some(map) = &entry.map {
                total = total.saturating_add(entry.css.len()).saturating_add(
                    serde_json::to_vec(map)
                        .map_err(|e| fail("sourceMap", &entry.origin, e))?
                        .len(),
                );
                if total > limits.total_bytes {
                    return Err(fail(
                        "limit",
                        &entry.origin,
                        "Snapshot maps exceed total byte limit",
                    ));
                }
                validate_map_for_text(map, &entry.css, limits)
                    .map_err(|e| fail("sourceMap", &entry.origin, e))?;
            }
        }
        Ok(Self {
            files: Arc::new(files),
            dirs: Arc::new(dirs),
        })
    }
    pub fn get(&self, path: &Path) -> Option<&FileSnapshot> {
        let path = identity(path).ok()?;
        self.files.get(&path)
    }
    pub fn canonical(&self, path: &Path) -> Result<PathBuf, HostError> {
        let result = identity(path).map_err(|e| fail("path", path, e))?;
        if self.files.contains_key(&result) || self.dirs.contains(&result) {
            Ok(result)
        } else {
            canonical(path)
        }
    }
    pub fn kind(&self, path: &Path) -> io::Result<(bool, bool)> {
        let meta = <Self as FileSystem>::metadata(self, path)?;
        Ok((meta.is_file(), meta.is_dir()))
    }
}
impl FileSystem for SnapshotFs {
    fn new(_yarn_pnp: bool) -> Self {
        Self::default()
    }
    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        if environment_file(path) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Environment files are protected",
            ));
        }
        if let Some(file) = self.get(path) {
            Ok(file.text.as_bytes().to_vec())
        } else {
            FileSystemOs::new(false).read(path)
        }
    }
    fn read_to_string(&self, path: &Path) -> io::Result<String> {
        self.read(path)
            .map(|v| String::from_utf8_lossy(&v).into_owned())
    }
    fn metadata(&self, path: &Path) -> io::Result<FileMetadata> {
        let id = identity(path)?;
        if self.files.contains_key(&id) {
            Ok(FileMetadata::new(true, false, false))
        } else if self.dirs.contains(&id) {
            Ok(FileMetadata::new(false, true, false))
        } else {
            FileSystemOs::new(false).metadata(path)
        }
    }
    fn symlink_metadata(&self, path: &Path) -> io::Result<FileMetadata> {
        // Preserve real symlinks for the resolver's canonicalization loop.
        match fs::symlink_metadata(path) {
            Ok(m) if m.file_type().is_symlink() => {
                Ok(FileMetadata::new(m.is_file(), m.is_dir(), true))
            }
            _ => self.metadata(path),
        }
    }
    fn read_link(&self, path: &Path) -> Result<PathBuf, ResolveError> {
        FileSystemOs::new(false).read_link(path)
    }
    fn canonicalize(&self, path: &Path) -> io::Result<PathBuf> {
        let id = identity(path)?;
        if self.files.contains_key(&id) || self.dirs.contains(&id) {
            Ok(id)
        } else {
            dunce::canonicalize(path)
        }
    }
}

pub(crate) fn validate_map(map: &serde_json::Value) -> Result<SourceMap, String> {
    if map["version"] != 3
        || map.get("sections").is_some()
        || map
            .get("sourceRoot")
            .is_some_and(|v| !v.is_null() && v != "")
    {
        return Err("Expected a flat version-3 map with resolved source identities".into());
    }
    let map = SourceMap::from_json(serde_json::from_value(map.clone()).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    for (i, path) in map.get_sources().enumerate() {
        if !Path::new(path).is_absolute()
            || path.contains('\0')
            || map.get_source_content(i as u32).is_none()
        {
            return Err(
                "Snapshot maps require absolute original sources and sourcesContent".into(),
            );
        }
    }
    Ok(map)
}

pub(crate) fn compose_maps(
    generated: &serde_json::Value,
    inputs: &BTreeMap<String, serde_json::Value>,
) -> Result<serde_json::Value, String> {
    use oxc_sourcemap::SourceMapBuilder;
    if inputs.is_empty() {
        return Ok(generated.clone());
    }
    let last =
        SourceMap::from_json(serde_json::from_value(generated.clone()).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    let first = inputs
        .iter()
        .map(|(id, value)| Ok((id.as_str(), validate_map(value)?)))
        .collect::<Result<BTreeMap<_, _>, String>>()?;
    let lookup = first
        .iter()
        .map(|(id, map)| (*id, map.generate_lookup_table()))
        .collect::<BTreeMap<_, _>>();
    let mut builder = SourceMapBuilder::default();
    // Preserve document-only source entries as well as mapped anchors. An input
    // map replaces its generated stylesheet document with original documents.
    let mut documents = BTreeMap::<String, String>::new();
    for (id, source) in last.get_sources().enumerate() {
        if let Some(input) = first.get(source) {
            for (original_id, original) in input.get_sources().enumerate() {
                let content = input
                    .get_source_content(original_id as u32)
                    .ok_or("Input map lacks sourcesContent")?;
                if documents
                    .get(original)
                    .is_some_and(|previous| previous != content)
                {
                    return Err("Input maps disagree on original document content".into());
                }
                documents.insert(original.into(), content.into());
            }
        } else {
            let content = last
                .get_source_content(id as u32)
                .ok_or("Generated map lacks sourcesContent")?;
            if documents
                .get(source)
                .is_some_and(|previous| previous != content)
            {
                return Err("Input maps disagree on original document content".into());
            }
            documents.insert(source.into(), content.into());
        }
    }
    for (source, content) in &documents {
        builder.add_source_and_content(source, content);
    }
    let mut mapped_sources = BTreeSet::new();
    for token in last.get_source_view_tokens() {
        let mut unmapped =
            || builder.add_token(token.get_dst_line(), token.get_dst_col(), 0, 0, None, None);
        let Some(source_id) = token.get_source_id() else {
            unmapped();
            continue;
        };
        let source = last
            .get_source(source_id)
            .ok_or("Invalid generated source index")?;
        if let Some(first) = first.get(source) {
            let mapped = first.lookup_source_view_token(
                &lookup[source],
                token.get_src_line(),
                token.get_src_col(),
            );
            // A source map segment is an anchor, not a character-by-character
            // promise. Do not invent offsets or precision between anchors.
            let Some(mapped) = mapped.filter(|m| {
                m.get_dst_line() == token.get_src_line()
                    && m.get_dst_col() == token.get_src_col()
                    && m.get_source_id().is_some()
            }) else {
                unmapped();
                continue;
            };
            let id = mapped.get_source_id().unwrap();
            let source = first.get_source(id).ok_or("Invalid input source index")?;
            let content = first
                .get_source_content(id)
                .ok_or("Input map lacks sourcesContent")?;
            mapped_sources.insert(source.to_owned());
            let output = builder.add_source_and_content(source, content);
            let name = mapped
                .get_name()
                .or_else(|| token.get_name())
                .map(|n| builder.add_name(n));
            builder.add_token(
                token.get_dst_line(),
                token.get_dst_col(),
                mapped.get_src_line(),
                mapped.get_src_col(),
                Some(output),
                name,
            );
        } else {
            let content = last
                .get_source_content(source_id)
                .ok_or("Generated map lacks sourcesContent")?;
            mapped_sources.insert(source.to_owned());
            let output = builder.add_source_and_content(source, content);
            let name = token.get_name().map(|n| builder.add_name(n));
            builder.add_token(
                token.get_dst_line(),
                token.get_dst_col(),
                token.get_src_line(),
                token.get_src_col(),
                Some(output),
                name,
            );
        }
    }
    let mut result: serde_json::Value =
        serde_json::from_str(&builder.into_owned_sourcemap().to_json_string())
            .map_err(|e| e.to_string())?;
    if let Some(object) = generated.as_object() {
        for (name, value) in object {
            if name.starts_with("x_") {
                result[name] = value.clone();
            }
        }
    }
    result["x_ts_rust_documentOnlySources"] = serde_json::json!(
        documents
            .keys()
            .filter(|id| !mapped_sources.contains(*id))
            .collect::<Vec<_>>()
    );
    result["x_ts_rust_snapshotComposition"] = serde_json::json!({"mapping":"exactAnchorsOnly","inputs": inputs.keys().collect::<Vec<_>>()});
    Ok(result)
}

fn validate_map_for_text(
    map: &serde_json::Value,
    generated: &str,
    limits: &HostLimits,
) -> Result<(), String> {
    if serde_json::to_vec(map).map_err(|e| e.to_string())?.len() > limits.total_bytes {
        return Err("Input source map exceeds byte limit".into());
    }
    let map = validate_map(map)?;
    let generated_positions = position_index(generated);
    let originals = map
        .get_sources()
        .enumerate()
        .map(|(i, _)| position_index(map.get_source_content(i as u32).unwrap()))
        .collect::<Vec<_>>();
    let mut previous = None;
    for token in map.get_source_view_tokens() {
        let dst = (token.get_dst_line(), token.get_dst_col());
        if previous.is_some_and(|p| p >= dst) || !valid_position(&generated_positions, dst.0, dst.1)
        {
            return Err(
                "Input map has unordered, duplicate, or out-of-bounds generated anchors".into(),
            );
        }
        previous = Some(dst);
        if let Some(id) = token.get_source_id() {
            let source = originals
                .get(id as usize)
                .ok_or("Input map has invalid source index")?;
            if !valid_position(source, token.get_src_line(), token.get_src_col()) {
                return Err(
                    "Input map original anchor is outside a UTF-16 character boundary".into(),
                );
            }
        }
    }
    Ok(())
}
fn position_index(text: &str) -> Vec<Vec<u32>> {
    text.split('\n')
        .map(|line| {
            let line = line.strip_suffix('\r').unwrap_or(line);
            let mut result = vec![0];
            let mut column = 0;
            for c in line.chars() {
                column += c.len_utf16() as u32;
                result.push(column);
            }
            result
        })
        .collect()
}
fn valid_position(index: &[Vec<u32>], line: u32, column: u32) -> bool {
    index
        .get(line as usize)
        .is_some_and(|columns| columns.binary_search(&column).is_ok())
}
