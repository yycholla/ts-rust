//! Native adapter-node 6.0.0 packaging (Kit 3). The adapter's Vite plugin puts
//! its server modules into the SSR build (`server_graph` below); its `adapt`
//! then copies the Kit output, precompresses, measures every servable client
//! file and writes the hand-off module. Nothing is executed.
//!
//! Sources: adapter-node 6.0.0 `index.js` and Kit 3.0.1
//! `core/adapt/builder.js` (`writeClient`, `compress`, `mimeTypes`,
//! `generateServerInstance`).
use crate::{Artifact, BuildRequest, artifacts};
use base64::Engine as _;
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::PathBuf;
use ts_svelte_compress::{BrotliMode, Compression};
use ts_svelte_kit::adapter_node::Options;

/// The external id the adapter's modules import the generated values from.
pub const HANDOFF: &str = "#@sveltejs/adapter-node";
/// Kit 3 `builder.compress` extensions.
const COMPRESS_EXTENSIONS: [&str; 11] = [
    ".html", ".js", ".mjs", ".json", ".css", ".svg", ".xml", ".wasm", ".txt", ".md", ".mdx",
];

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AdapterRequest {
    /// Canonical installed package directory; the loader verified its files.
    pub directory: PathBuf,
    #[serde(default)]
    pub options: Options,
}

/// The adapter's additions to the Kit server graph.
pub struct ServerGraph {
    pub entries: BTreeMap<String, PathBuf>,
    /// Chunks holding a module under this prefix are `adapter-node-[name].js`.
    pub module_prefix: String,
    pub paths: BTreeMap<String, String>,
}
pub fn server_graph(adapter: &AdapterRequest) -> Result<ServerGraph, String> {
    let source = adapter.directory.join("src");
    let module_prefix = format!("{}/", source.to_str().ok_or("Adapter path must be UTF-8")?);
    Ok(ServerGraph {
        entries: BTreeMap::from([
            ("adapter-index".into(), source.join("index.js")),
            ("adapter-env".into(), source.join("env.js")),
            ("handler".into(), source.join("handler.js")),
        ]),
        module_prefix,
        paths: BTreeMap::from([(HANDOFF.into(), "../adapter-node.js".into())]),
    })
}

/// Node `path.extname` of a posix path.
fn extname(file: &str) -> &str {
    let base = file.rsplit('/').next().unwrap_or(file);
    match base.rfind('.') {
        Some(0) | None => "",
        Some(index) => &base[index..],
    }
}
/// `mrmime.lookup(ext) || ''`.
fn lookup(request: &BuildRequest, extension: &str) -> String {
    request
        .mime_types
        .get(&extension.to_lowercase())
        .cloned()
        .unwrap_or_default()
}
fn json(value: &str) -> String {
    serde_json::to_string(value).expect("string serializes")
}
/// adapter-node `is_hidden`: dotfiles other than `.well-known` are not served.
fn is_hidden(file: &str) -> bool {
    file.split('/').any(|segment| segment.starts_with('.')) && !file.starts_with(".well-known/")
}

struct Measured {
    file: String,
    size: usize,
    etag: String,
    compressed: Option<(usize, usize)>,
}
impl Measured {
    /// `JSON.stringify` of the entry object, in its property order.
    fn to_json(&self) -> String {
        let mut text = format!(
            "{{\"file\":{},\"size\":{},\"etag\":{}",
            json(&self.file),
            self.size,
            json(&self.etag)
        );
        if let Some((gz, br)) = self.compressed {
            text.push_str(&format!(",\"gz\":{gz},\"br\":{br}"));
        }
        text.push('}');
        text
    }
}

/// adapter-node `create_asset_table`, serialized like `JSON.stringify`.
fn asset_table(base: &str, measured: &[Measured]) -> String {
    let mut entries: Vec<(String, &Measured)> = measured
        .iter()
        .map(|entry| (format!("{base}/{}", entry.file), entry))
        .collect();
    entries.sort_by(|a, b| a.0.encode_utf16().cmp(b.0.encode_utf16()));
    let mut keys: std::collections::BTreeSet<String> =
        entries.iter().map(|(key, _)| key.clone()).collect();
    let mut aliases = vec![];
    for (key, entry) in &entries {
        if !entry.file.ends_with(".html") {
            continue;
        }
        let is_index = entry.file == "index.html" || entry.file.ends_with("/index.html");
        let with_slash = if is_index {
            key[..key.len() - "index.html".len()].to_owned()
        } else {
            format!("{}/", &key[..key.len() - 5])
        };
        let mut alias = |alias: String| {
            if keys.insert(alias.clone()) {
                aliases.push((alias, key.clone()));
            }
        };
        alias(with_slash.clone());
        if with_slash.len() > 1 {
            alias(with_slash[..with_slash.len() - 1].to_owned());
        }
    }
    let entries: Vec<String> = entries
        .iter()
        .map(|(key, entry)| format!("[{},{}]", json(key), entry.to_json()))
        .collect();
    let aliases: Vec<String> = aliases
        .iter()
        .map(|(alias, key)| format!("[{},{}]", json(alias), json(key)))
        .collect();
    format!(
        "{{\"entries\":[{}],\"aliases\":[{}]}}",
        entries.join(","),
        aliases.join(",")
    )
}

/// gzip level 9 and Node's default Brotli (quality 11, generic, window 22),
/// across the available cores.
/// Client file -> (gzip, brotli) bytes.
type Compressed = BTreeMap<String, (Vec<u8>, Vec<u8>)>;
fn compress_all(files: &[(&String, &Vec<u8>)]) -> Result<Compressed, String> {
    let gzip = Compression::Gzip { level: 9 };
    let brotli = Compression::Brotli {
        quality: 11,
        mode: BrotliMode::Generic,
        lgwin: 22,
        size_hint: None,
    };
    let next = std::sync::atomic::AtomicUsize::new(0);
    let results = std::sync::Mutex::new(BTreeMap::new());
    let threads = std::thread::available_parallelism()
        .map_or(1, usize::from)
        .min(files.len().max(1));
    std::thread::scope(|scope| {
        for _ in 0..threads {
            scope.spawn(|| {
                loop {
                    let index = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let Some((file, bytes)) = files.get(index) else {
                        break;
                    };
                    let result = ts_svelte_compress::compress(bytes, &gzip).and_then(|gz| {
                        ts_svelte_compress::compress(bytes, &brotli).map(|br| (gz.bytes, br.bytes))
                    });
                    results
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .insert((*file).clone(), result);
                }
            });
        }
    });
    results
        .into_inner()
        .unwrap_or_else(|e| e.into_inner())
        .into_iter()
        .map(|(file, result)| {
            result
                .map(|output| (file.clone(), output))
                .map_err(|error| format!("Compress {file}: {error}"))
        })
        .collect()
}

/// `builder.generateServerInstance(`${server}/server.js`)`: the manifest
/// expression Kit wrote to manifest.js (same routes, relative path `.`).
fn server_instance(manifest: &str) -> Result<String, String> {
    let expression = manifest
        .strip_prefix("export const manifest = ")
        .and_then(|rest| rest.trim_end().strip_suffix(';'))
        .ok_or("Unexpected server/manifest.js layout")?;
    Ok(format!(
        "import {{ create_server }} from './index.js';\nconst manifest = {expression};\nexport const server = create_server(manifest);"
    ))
}

/// adapter-node 6.0.0 `adapt` over the Kit output in `artifacts` (`client/`,
/// `server/`). Returns the deployment files, relative to the project root.
pub fn package(
    request: &BuildRequest,
    adapter: &AdapterRequest,
    artifacts: &BTreeMap<String, Artifact>,
) -> Result<BTreeMap<String, Artifact>, String> {
    let options = &adapter.options;
    let out = options.out.trim_end_matches('/');
    artifacts::relative(out)?;
    let base = &request.kit.environment.base;
    if !base.is_empty() {
        return Err("adapter-node packaging with kit.paths.base is not ported".into());
    }
    let mut output = BTreeMap::new();

    // writeClient: the client output except Vite's `.vite` directory.
    let mut client = BTreeMap::new();
    for (name, artifact) in artifacts {
        if let Some(file) = name.strip_prefix("client/")
            && !file.split('/').any(|segment| segment == ".vite")
        {
            client.insert(file.to_owned(), artifacts::bytes(artifact)?);
        }
    }
    for (file, bytes) in &client {
        let kind = &artifacts[&format!("client/{file}")].kind;
        artifacts::insert(
            &mut output,
            format!("{out}/client{base}/{file}"),
            bytes,
            kind,
        )?;
    }
    // writePrerendered: native builds prerender nothing (checked upstream).

    let compressed = if options.precompress {
        let jobs: Vec<_> = client
            .iter()
            .filter(|(file, _)| COMPRESS_EXTENSIONS.contains(&extname(file)))
            .collect();
        compress_all(&jobs)?
    } else {
        BTreeMap::new()
    };
    for (file, (gz, br)) in &compressed {
        artifacts::insert(
            &mut output,
            format!("{out}/client{base}/{file}.gz"),
            gz,
            "compressed",
        )?;
        artifacts::insert(
            &mut output,
            format!("{out}/client{base}/{file}.br"),
            br,
            "compressed",
        )?;
    }
    let measured: Vec<Measured> = client
        .iter()
        .filter(|(file, _)| !is_hidden(file))
        .map(|(file, bytes)| Measured {
            file: file.clone(),
            size: bytes.len(),
            etag: base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(bytes)),
            compressed: compressed.get(file).map(|(gz, br)| (gz.len(), br.len())),
        })
        .collect();
    let assets = asset_table(base, &measured);
    let prerendered_assets = asset_table(base, &[]);

    // builder.mimeTypes: Kit's manifest types (static assets), then every
    // client file extension.
    let mut mime_types: Vec<(String, String)> = vec![];
    let mut add = |extension: &str, mime: String| {
        if !extension.is_empty() && !mime_types.iter().any(|(known, _)| known == extension) {
            mime_types.push((extension.to_owned(), mime));
        }
    };
    for file in request.static_assets.keys() {
        let extension = extname(file);
        add(extension, lookup(request, extension));
    }
    for file in client.keys() {
        let extension = extname(file);
        add(extension, lookup(request, extension));
    }
    let mime_types = format!(
        "{{{}}}",
        mime_types
            .iter()
            .map(|(extension, mime)| format!("{}:{}", json(extension), json(mime)))
            .collect::<Vec<_>>()
            .join(",")
    );

    // builder.generateServerInstance, then builder.copy(server, out/server).
    let manifest = artifacts
        .get("server/manifest.js")
        .ok_or("Missing server/manifest.js")?;
    let manifest = String::from_utf8(artifacts::bytes(manifest)?)
        .map_err(|_| "server/manifest.js is not UTF-8")?;
    let root = &request.kit.inventory.project_root;
    let server_dir = root.join(out).join("server");
    for (name, artifact) in artifacts {
        let Some(file) = name.strip_prefix("server/") else {
            continue;
        };
        let mut bytes = artifacts::bytes(artifact)?;
        if file.ends_with(".map") {
            // The emitter's sources are relative to root/dist; deploy them
            // relative to the map's own location.
            let mut map: Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
            crate::adapter::absolute_sources(&mut map, &root.join("dist").join(file))?;
            crate::adapter::relative_sources(&mut map, &server_dir.join(file))?;
            bytes = serde_json::to_vec(&map).map_err(|e| e.to_string())?;
        }
        artifacts::insert(
            &mut output,
            format!("{out}/server/{file}"),
            &bytes,
            &artifact.kind,
        )?;
    }
    artifacts::insert(
        &mut output,
        format!("{out}/server/server.js"),
        server_instance(&manifest)?.as_bytes(),
        "adapter",
    )?;

    let hand_off = [
        "import { dirname } from 'node:path';".to_owned(),
        "import { fileURLToPath } from 'node:url';".to_owned(),
        "export { server } from './server/server.js';".to_owned(),
        "export const dir = dirname(fileURLToPath(import.meta.url));".to_owned(),
        format!("export const base = {};", json(base)),
        format!(
            "export const app_path = {};",
            json(&format!(
                "{}{}",
                base.trim_start_matches('/'),
                request.kit.environment.app_dir
            ))
        ),
        // `JSON.stringify(undefined)`: kit.paths.origin is not set natively.
        "export const origin = undefined;".to_owned(),
        format!("export const env_prefix = {};", json(&options.env_prefix)),
        format!("export const mime_types = {mime_types};"),
        format!("export const assets = JSON.parse({});", json(&assets)),
        format!(
            "export const prerendered_assets = JSON.parse({});",
            json(&prerendered_assets)
        ),
    ]
    .join("\n");
    artifacts::insert(
        &mut output,
        format!("{out}/adapter-node.js"),
        hand_off.as_bytes(),
        "adapter",
    )?;
    artifacts::insert(
        &mut output,
        format!("{out}/index.js"),
        b"export * from './server/adapter-index.js';\n",
        "adapter",
    )?;
    artifacts::insert(
        &mut output,
        format!("{out}/handler.js"),
        b"export * from './server/handler.js';\n",
        "adapter",
    )?;
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(file: &str) -> Measured {
        Measured {
            file: file.into(),
            size: 1,
            etag: "e".into(),
            compressed: None,
        }
    }

    #[test]
    fn asset_table_sorts_entries_and_aliases_html_like_adapter_node() {
        let table = asset_table(
            "",
            &[
                entry("b.js"),
                entry("about.html"),
                entry("index.html"),
                entry("a/index.html"),
            ],
        );
        assert_eq!(
            table,
            "{\"entries\":[[\"/a/index.html\",{\"file\":\"a/index.html\",\"size\":1,\"etag\":\"e\"}],[\"/about.html\",{\"file\":\"about.html\",\"size\":1,\"etag\":\"e\"}],[\"/b.js\",{\"file\":\"b.js\",\"size\":1,\"etag\":\"e\"}],[\"/index.html\",{\"file\":\"index.html\",\"size\":1,\"etag\":\"e\"}]],\"aliases\":[[\"/a/\",\"/a/index.html\"],[\"/a\",\"/a/index.html\"],[\"/about/\",\"/about.html\"],[\"/about\",\"/about.html\"],[\"/\",\"/index.html\"]]}"
        );
    }

    #[test]
    fn extname_and_hidden_files_follow_node() {
        assert_eq!(extname("robots.txt"), ".txt");
        assert_eq!(extname("a.b/c"), "");
        assert_eq!(extname(".gitignore"), "");
        assert!(is_hidden(".vite/manifest.json"));
        assert!(!is_hidden(".well-known/x.txt"));
    }

    #[test]
    fn server_instance_wraps_the_manifest_expression() {
        assert_eq!(
            server_instance("export const manifest = (() => {})();\n").unwrap(),
            "import { create_server } from './index.js';\nconst manifest = (() => {})();\nexport const server = create_server(manifest);"
        );
    }
}
