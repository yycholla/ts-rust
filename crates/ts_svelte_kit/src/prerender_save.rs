//! Pure response-saving stage of SvelteKit prerendering. Ported from MIT-licensed
//! SvelteKit (copyright Svelte contributors), registered Kit 2.67.0–2.70.3 and
//! Kit 3.0.0-next.32/3.0.1 `src/core/postbuild/prerender.js::save`, with redirect
//! escaping from `utils/escape.js` and devalue `src/utils.js::stringify_string`.
//!
//! No filesystem, crawler, queue, config evaluation, or runtime dependency.
//! Paths in write effects/inventory are relative to output/prerendered. Apply
//! effects in order; filename deduplication spans all three categories. The
//! snapshot separates actual writes from upstream's non-writing saved aliases.
//! An HTTP callback suspends this state until resume_http acknowledges its result.
//! Failures terminate the state. Bounds and unsafe/non-scalar output filenames
//! return explicit errors instead of changing upstream paths or escaping output.
//! Input headers are the normalized Object.fromEntries(Response.headers) view;
//! source strings must be Unicode scalar strings. No lossy UTF-16 conversion.
//! Console progress logging is left to the host. Warnings/errors are effects.
//!
//! Permission is hereby granted, free of charge, to any person obtaining a copy
//! of this software and associated documentation files (the "Software"), to deal
//! in the Software without restriction, including without limitation the rights
//! to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
//! copies of the Software, and to permit persons to whom the Software is
//! furnished to do so, subject to the following conditions: the above copyright
//! notice and this permission notice shall be included in all copies or
//! substantial portions of the Software. THE SOFTWARE IS PROVIDED "AS IS",
//! WITHOUT WARRANTY OF ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED
//! TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND
//! NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE
//! LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF
//! CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION WITH THE
//! SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.
use crate::sync_versions::KitSyncVersion;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use url::Url;

const MAX_TEXT: usize = 1024 * 1024;
const MAX_BODY: usize = 64 * 1024 * 1024;
const MAX_CALLS: usize = 100_000;
const MAX_OUTPUT: usize = 256 * 1024 * 1024;

type Result<T> = std::result::Result<T, String>;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SaveConfig {
    pub kit_version: String,
    pub base: String,
    /// Explicit resolved prerender origin. Used by the visit host, not relative
    /// redirect resolution (which uses the encoded request path, as upstream).
    pub origin: String,
    pub http_error: HttpErrorPolicy,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum HttpErrorPolicy {
    Default,
    Fail,
    Warn,
    Ignore,
    Custom { name: String },
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Category {
    Pages,
    Dependencies,
    Data,
}
impl Category {
    fn directory(self) -> &'static str {
        match self {
            Self::Pages => "pages",
            Self::Dependencies => "dependencies",
            Self::Data => "data",
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReferenceType {
    Linked,
    Fetched,
}
impl ReferenceType {
    fn label(self) -> &'static str {
        match self {
            Self::Linked => "linked",
            Self::Fetched => "fetched",
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OutputKind {
    File,
    Directory,
}
/// Includes existing generated files/directories; implicit parent directories
/// are inferred. Symlinks must be resolved/guarded by the host before this input.
pub type OutputInventory = BTreeMap<String, OutputKind>;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SaveInput {
    pub decoded: String,
    pub encoded: String,
    pub referrer: Option<String>,
    pub reference_type: ReferenceType,
    pub category: Category,
    /// Raw response status, including 0; save partitions by floor(status / 100).
    pub status: u16,
    pub headers: BTreeMap<String, String>,
    pub body: Vec<u8>,
}
/// Standalone invocation of Kit's handleHttpError. No response, output path,
/// route bookkeeping, or saved alias is synthesized by this operation.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HttpErrorInput {
    pub status: u16,
    pub path: String,
    pub referrer: Option<String>,
    pub reference_type: ReferenceType,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HttpErrorDetails {
    pub status: u16,
    pub path: String,
    pub referrer: Option<String>,
    pub reference_type: ReferenceType,
    pub message: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum SaveEffect {
    RouteSeen {
        id: String,
    },
    Enqueue {
        referrer: String,
        decoded: String,
        encoded: String,
    },
    Write {
        path: String,
        body: Vec<u8>,
    },
    Page {
        path: String,
        file: String,
    },
    Asset {
        path: String,
        content_type: Option<String>,
    },
    Redirect {
        path: String,
        status: u16,
        location: String,
    },
    Path {
        path: String,
    },
    ManifestAsset {
        file: String,
    },
    Saved {
        file: String,
        destination: String,
    },
    Warning {
        message: String,
    },
    HttpHandler {
        token: u64,
        name: String,
        details: HttpErrorDetails,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum SaveStatus {
    Complete,
    Duplicate { file: String },
    AwaitingHttpHandler { token: u64 },
    Failed { message: String },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SaveResult {
    pub effects: Vec<SaveEffect>,
    pub status: SaveStatus,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveSnapshot {
    pub pages: Vec<(String, String)>,
    pub assets: Vec<(String, Option<String>)>,
    pub redirects: Vec<(String, (u16, String))>,
    pub paths: Vec<String>,
    pub prerendered_routes: Vec<String>,
    pub written: Vec<String>,
    pub manifest_assets: Vec<String>,
    pub saved: Vec<(String, String)>,
}
struct Pending {
    token: u64,
    alias: Option<(String, String)>,
}
pub struct SaveState {
    config: SaveConfig,
    kit3: bool,
    inventory: OutputInventory,
    snapshot: SaveSnapshot,
    written: BTreeSet<String>,
    pending: Option<Pending>,
    failed: bool,
    calls: usize,
    output_bytes: usize,
}

impl SaveState {
    pub fn new(config: SaveConfig, mut inventory: OutputInventory) -> Result<Self> {
        let kit3 = KitSyncVersion::parse(&config.kit_version)?.is_kit3();
        bounded(&config.base)?;
        bounded(&config.origin)?;
        if (!config.base.is_empty()
            && (!config.base.starts_with('/') || config.base.ends_with('/')))
            || config.base.contains(['?', '#'])
        {
            return Err(
                "Expected resolved Kit paths.base (empty or root-relative without trailing slash)"
                    .into(),
            );
        }
        let origin =
            Url::parse(&config.origin).map_err(|e| format!("Invalid prerender origin: {e}"))?;
        if !matches!(origin.scheme(), "http" | "https")
            || origin.host_str().is_none()
            || origin.path() != "/"
            || origin.query().is_some()
            || origin.fragment().is_some()
            || !origin.username().is_empty()
            || origin.password().is_some()
        {
            return Err("Expected explicit HTTP(S) prerender origin".into());
        }
        if let HttpErrorPolicy::Custom { name } = &config.http_error {
            bounded(name)?;
        }
        if inventory.len() > MAX_CALLS {
            return Err("Prerender inventory limit exceeded".into());
        }
        for (path, kind) in inventory.clone() {
            safe_path(&path)?;
            let mut parent = path.as_str();
            while let Some((ancestor, _)) = parent.rsplit_once('/') {
                if inventory.get(ancestor) == Some(&OutputKind::File) {
                    return Err(format!(
                        "Output inventory file is also a parent directory: {ancestor}"
                    ));
                }
                inventory.insert(ancestor.into(), OutputKind::Directory);
                parent = ancestor;
            }
            if kind == OutputKind::File && inventory.get(&path) == Some(&OutputKind::Directory) {
                return Err(format!(
                    "Output inventory path is both file and directory: {path}"
                ));
            }
        }
        Ok(Self {
            config,
            kit3,
            inventory,
            snapshot: SaveSnapshot::default(),
            written: BTreeSet::new(),
            pending: None,
            failed: false,
            calls: 0,
            output_bytes: 0,
        })
    }
    pub fn snapshot(&self) -> &SaveSnapshot {
        &self.snapshot
    }
    pub fn inventory(&self) -> &OutputInventory {
        &self.inventory
    }

    pub fn save(&mut self, input: SaveInput) -> Result<SaveResult> {
        if self.failed {
            return Err("Prerender save state has failed".into());
        }
        if self.pending.is_some() {
            return Err("Resume the pending HTTP handler before saving another response".into());
        }
        if self.calls >= MAX_CALLS {
            return Err("Prerender response count limit exceeded".into());
        }
        validate_input(&input)?;
        self.calls += 1;
        let result = self.save_inner(input);
        if result.is_err() {
            self.failed = true;
        }
        result
    }
    fn save_inner(&mut self, input: SaveInput) -> Result<SaveResult> {
        let mut effects = vec![];
        let response_type = input.status / 100;
        let content_type = input.headers.get("content-type");
        let html = response_type == 3
            || content_type.is_some_and(|s| {
                if self.kit3 {
                    s.split(';')
                        .next()
                        .unwrap_or("")
                        .trim_matches(js_whitespace)
                        .eq_ignore_ascii_case("text/html")
                } else {
                    s == "text/html"
                }
            });
        let suffix = utf16_slice(&input.decoded, self.config.base.encode_utf16().count() + 1)?;
        if !html && input.status == 200 && suffix.is_empty() {
            let base = if self.config.base.is_empty() {
                "/"
            } else {
                &self.config.base
            };
            let message = if self.kit3 {
                kit_message(
                    "prerender_root_non_html",
                    &format!(
                        "Cannot prerender a root `+server.js` that returns a non-HTML response — static hosts always serve an HTML file for `{base}`"
                    ),
                )
            } else {
                format!(
                    "Cannot prerender a root +server.js that returns a non-HTML response - static hosts always serve an HTML file for `{base}`"
                )
            };
            return Ok(self.fail(effects, message));
        }
        let file = output_filename(&self.config.base, &input.decoded, html)?;
        if self.written.contains(&file) {
            return Ok(SaveResult {
                effects,
                status: SaveStatus::Duplicate { file },
            });
        }
        let destination = format!("{}/{file}", input.category.directory());
        if let Some(id) = input.headers.get("x-sveltekit-routeid") {
            let id = decode_uri(id)?;
            if !self.snapshot.prerendered_routes.contains(&id) {
                self.snapshot.prerendered_routes.push(id.clone());
                effects.push(SaveEffect::RouteSeen { id });
            }
        }
        if response_type == 3 {
            if let Some(location) = input.headers.get("location").filter(|s| !s.is_empty()) {
                let resolved = match resolve(&input.encoded, location) {
                    Ok(value) => value,
                    Err(error) => return Ok(self.fail(effects, error)),
                };
                if resolved.starts_with('/') && !resolved.starts_with("//") {
                    let decoded = match decode_uri(&resolved) {
                        Ok(value) => value,
                        Err(error) => return Ok(self.fail(effects, error)),
                    };
                    effects.push(SaveEffect::Enqueue {
                        referrer: input.decoded.clone(),
                        decoded,
                        encoded: resolved.clone(),
                    });
                }
                if !input
                    .headers
                    .get("x-sveltekit-normalize")
                    .is_some_and(|s| !s.is_empty())
                {
                    let body = redirect_html(location).into_bytes();
                    if let Err(error) = self.write(&destination, body, &input.decoded, &mut effects)
                    {
                        return Ok(self.fail(effects, error));
                    }
                    self.mark_written(&file);
                    if !self
                        .snapshot
                        .redirects
                        .iter()
                        .any(|(p, _)| p == &input.decoded)
                    {
                        self.snapshot
                            .redirects
                            .push((input.decoded.clone(), (input.status, resolved.clone())));
                        effects.push(SaveEffect::Redirect {
                            path: input.decoded.clone(),
                            status: input.status,
                            location: resolved,
                        });
                        self.path(&input.decoded, &mut effects);
                    }
                }
            } else {
                let message = if self.kit3 {
                    kit_message(
                        "prerender_redirect_location_missing",
                        &format!(
                            "`location` header missing on redirect received from `{}`",
                            input.decoded
                        ),
                    )
                } else {
                    format!(
                        "location header missing on redirect received from {}",
                        input.decoded
                    )
                };
                effects.push(SaveEffect::Warning { message });
            }
            return Ok(SaveResult {
                effects,
                status: SaveStatus::Complete,
            });
        }
        if input.status == 200 {
            if let Err(error) = self.write(&destination, input.body, &input.decoded, &mut effects) {
                return Ok(self.fail(effects, error));
            }
            self.mark_written(&file);
            if html {
                set(
                    &mut self.snapshot.pages,
                    input.decoded.clone(),
                    file.clone(),
                );
                effects.push(SaveEffect::Page {
                    path: input.decoded.clone(),
                    file: file.clone(),
                });
            } else {
                set(
                    &mut self.snapshot.assets,
                    input.decoded.clone(),
                    content_type.cloned(),
                );
                effects.push(SaveEffect::Asset {
                    path: input.decoded.clone(),
                    content_type: content_type.cloned(),
                });
            }
            self.path(&input.decoded, &mut effects);
        } else if response_type != 2 {
            let details = self.http_details(&HttpErrorInput {
                status: input.status,
                path: input.decoded,
                referrer: input.referrer,
                reference_type: input.reference_type,
            });
            return Ok(self.handle_http(details, Some((file, destination)), effects));
        }
        self.alias(file, destination, &mut effects);
        Ok(SaveResult {
            effects,
            status: SaveStatus::Complete,
        })
    }
    pub fn handle_http_error(&mut self, input: HttpErrorInput) -> Result<SaveResult> {
        if self.failed {
            return Err("Prerender save state has failed".into());
        }
        if self.pending.is_some() {
            return Err("Resume the pending HTTP handler first".into());
        }
        if self.calls >= MAX_CALLS {
            return Err("Prerender response count limit exceeded".into());
        }
        bounded(&input.path)?;
        if let Some(referrer) = &input.referrer {
            bounded(referrer)?;
        }
        self.calls += 1;
        let details = self.http_details(&input);
        Ok(self.handle_http(details, None, vec![]))
    }
    fn handle_http(
        &mut self,
        details: HttpErrorDetails,
        alias: Option<(String, String)>,
        mut effects: Vec<SaveEffect>,
    ) -> SaveResult {
        match &self.config.http_error {
            HttpErrorPolicy::Default | HttpErrorPolicy::Fail => {
                let message = if matches!(self.config.http_error, HttpErrorPolicy::Default) {
                    format!(
                        "{}\nTo suppress or handle this error, implement `handleHttpError` in https://svelte.dev/docs/kit/configuration#prerender",
                        details.message
                    )
                } else {
                    details.message
                };
                return self.fail(effects, message);
            }
            HttpErrorPolicy::Warn => effects.push(SaveEffect::Warning {
                message: details.message,
            }),
            HttpErrorPolicy::Ignore => {}
            HttpErrorPolicy::Custom { name } => {
                let token = self.calls as u64;
                effects.push(SaveEffect::HttpHandler {
                    token,
                    name: name.clone(),
                    details,
                });
                self.pending = Some(Pending { token, alias });
                return SaveResult {
                    effects,
                    status: SaveStatus::AwaitingHttpHandler { token },
                };
            }
        }
        if let Some((file, destination)) = alias {
            self.alias(file, destination, &mut effects);
        }
        SaveResult {
            effects,
            status: SaveStatus::Complete,
        }
    }
    /// Resume only after the configured callback actually returned or threw.
    /// Stale tokens leave the pending continuation intact.
    pub fn resume_http(
        &mut self,
        token: u64,
        callback: std::result::Result<(), String>,
    ) -> Result<SaveResult> {
        let pending = self.pending.as_ref().ok_or("No pending HTTP handler")?;
        if pending.token != token {
            return Err("Stale HTTP handler continuation token".into());
        }
        let pending = self.pending.take().unwrap();
        let mut effects = vec![];
        if let Err(message) = callback {
            return Ok(self.fail(effects, message));
        }
        if let Some((file, destination)) = pending.alias {
            self.alias(file, destination, &mut effects);
        }
        Ok(SaveResult {
            effects,
            status: SaveStatus::Complete,
        })
    }
    fn fail(&mut self, effects: Vec<SaveEffect>, message: String) -> SaveResult {
        self.failed = true;
        SaveResult {
            effects,
            status: SaveStatus::Failed { message },
        }
    }
    fn mark_written(&mut self, file: &str) {
        if self.written.insert(file.into()) {
            self.snapshot.written.push(file.into());
        }
    }
    fn path(&mut self, path: &str, effects: &mut Vec<SaveEffect>) {
        self.snapshot.paths.push(path.into());
        effects.push(SaveEffect::Path { path: path.into() });
    }
    fn alias(&mut self, file: String, destination: String, effects: &mut Vec<SaveEffect>) {
        if !self.snapshot.manifest_assets.contains(&file) {
            self.snapshot.manifest_assets.push(file.clone());
        }
        set(&mut self.snapshot.saved, file.clone(), destination.clone());
        effects.push(SaveEffect::ManifestAsset { file: file.clone() });
        effects.push(SaveEffect::Saved { file, destination });
    }
    fn write(
        &mut self,
        destination: &str,
        body: Vec<u8>,
        decoded: &str,
        effects: &mut Vec<SaveEffect>,
    ) -> Result<()> {
        safe_path(destination)?;
        if self.inventory.get(destination) == Some(&OutputKind::Directory) {
            return Err(self.collision(decoded, None));
        }
        let mut parent = destination;
        while let Some((ancestor, _)) = parent.rsplit_once('/') {
            if self.inventory.get(ancestor) == Some(&OutputKind::File) {
                return Err(self.collision(
                    decoded,
                    Some(decoded.rsplit_once('/').map_or("", |(p, _)| p)),
                ));
            }
            parent = ancestor;
        }
        let bytes = self
            .output_bytes
            .checked_add(body.len())
            .ok_or("Prerender output size overflow")?;
        if bytes > MAX_OUTPUT {
            return Err("Prerender output byte limit exceeded".into());
        }
        self.output_bytes = bytes;
        self.inventory.insert(destination.into(), OutputKind::File);
        let mut parent = destination;
        while let Some((ancestor, _)) = parent.rsplit_once('/') {
            self.inventory
                .insert(ancestor.into(), OutputKind::Directory);
            parent = ancestor;
        }
        effects.push(SaveEffect::Write {
            path: destination.into(),
            body,
        });
        Ok(())
    }
    fn collision(&self, path: &str, parent: Option<&str>) -> String {
        let detail = if let Some(parent) = parent {
            if self.kit3 {
                format!("Cannot save `{path}` as `{parent}` is already a file.")
            } else {
                format!("Cannot save {path} as {parent} is already a file.")
            }
        } else if self.kit3 {
            format!("Cannot save `{path}` as it is already a directory.")
        } else {
            format!("Cannot save {path} as it is already a directory.")
        };
        if self.kit3 {
            kit_message(
                "prerender_directory_conflict",
                &format!(
                    "{detail} See https://svelte.dev/docs/kit/page-options#prerender-Route-conflicts for more information"
                ),
            )
        } else {
            format!(
                "{detail} See https://svelte.dev/docs/kit/page-options#prerender-route-conflicts for more information"
            )
        }
    }
    fn http_details(&self, input: &HttpErrorInput) -> HttpErrorDetails {
        let reference = input.reference_type.label();
        let referrer = input.referrer.as_deref().filter(|r| !r.is_empty());
        let path = &input.path;
        let outside = input.status == 404 && !path.starts_with(&self.config.base);
        let message = if self.kit3 {
            if outside {
                let context = referrer
                    .map(|r| format!(" ({reference} from `{r}`)"))
                    .unwrap_or_default();
                kit_message(
                    "prerender_path_outside_base",
                    &format!(
                        "`{path}`{context} does not begin with `base`. You can fix this by using `resolve('{path}')` from `$app/paths`. The base path is configurable from `paths.base`"
                    ),
                )
            } else {
                let detail = if input.status == 404 {
                    referrer.map(|r| format!("`{path}` was {reference} from `{r}`"))
                } else {
                    None
                };
                kit_message(
                    "prerender_http_error",
                    &detail.unwrap_or_else(|| format!("Failed to prerender `{path}`")),
                )
            }
        } else {
            let detail = if outside {
                format!(
                    "{path} does not begin with `base`, which is configured in `paths.base` and can be imported from `$app/paths` - see https://svelte.dev/docs/kit/configuration#paths for more info"
                )
            } else {
                path.clone()
            };
            format!(
                "{} {detail}{}",
                input.status,
                referrer
                    .map(|r| format!(" ({reference} from {r})"))
                    .unwrap_or_default()
            )
        };
        HttpErrorDetails {
            status: input.status,
            path: path.clone(),
            referrer: input.referrer.clone(),
            reference_type: input.reference_type,
            message,
        }
    }
}

/// Exact JS UTF-16 slice used by output_filename. A slice splitting a surrogate
/// pair is explicitly unsupported as a filesystem identity, never repaired.
pub fn output_filename(base: &str, path: &str, html: bool) -> Result<String> {
    let mut file = utf16_slice(path, base.encode_utf16().count() + 1)?;
    if file.is_empty() {
        file = "index.html".into();
    }
    if html && !file.ends_with(".html") {
        file.push_str(if file.ends_with('/') {
            "index.html"
        } else {
            ".html"
        });
    }
    Ok(file)
}
fn utf16_slice(value: &str, start: usize) -> Result<String> {
    String::from_utf16(&value.encode_utf16().skip(start).collect::<Vec<_>>())
        .map_err(|_| "Unsupported non-scalar UTF-16 output filename".into())
}
fn set<T>(map: &mut Vec<(String, T)>, key: String, value: T) {
    if let Some((_, old)) = map.iter_mut().find(|(k, _)| k == &key) {
        *old = value;
    } else {
        map.push((key, value));
    }
}
fn bounded(text: &str) -> Result<()> {
    if text.len() > MAX_TEXT {
        Err("Prerender text size limit exceeded".into())
    } else {
        Ok(())
    }
}
fn safe_path(path: &str) -> Result<()> {
    bounded(path)?;
    if path.is_empty()
        || path.starts_with('/')
        || path.contains(['\\', '\0'])
        || path
            .split('/')
            .any(|s| s.is_empty() || s == "." || s == "..")
    {
        return Err(format!(
            "Unsupported unsafe prerender output path: {path:?}"
        ));
    }
    Ok(())
}
fn validate_input(input: &SaveInput) -> Result<()> {
    bounded(&input.decoded)?;
    bounded(&input.encoded)?;
    if let Some(referrer) = &input.referrer {
        bounded(referrer)?;
    }
    if input.body.len() > MAX_BODY || input.headers.len() > 4096 {
        return Err("Prerender response size limit exceeded".into());
    }
    let mut header_bytes = 0usize;
    for (key, value) in &input.headers {
        header_bytes = header_bytes
            .checked_add(key.len())
            .and_then(|n| n.checked_add(value.len()))
            .ok_or("Prerender header size overflow")?;
        if header_bytes > MAX_TEXT {
            return Err("Prerender header byte limit exceeded".into());
        }
        bounded(value)?;
        if key.is_empty()
            || !key.bytes().all(|c| {
                c.is_ascii_lowercase() || c.is_ascii_digit() || b"!#$%&'*+-.^_`|~".contains(&c)
            })
        {
            return Err("Expected normalized lowercase Response headers".into());
        }
        if value.contains(['\0', '\r', '\n'])
            || value.starts_with([' ', '\t'])
            || value.ends_with([' ', '\t'])
        {
            return Err("Expected normalized Response header values".into());
        }
    }
    Ok(())
}
fn js_whitespace(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n' | '\u{b}' | '\u{c}' | '\r' | ' ' | '\u{a0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200a}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202f}'
                | '\u{205f}'
                | '\u{3000}'
                | '\u{feff}'
    )
}
fn kit_message(code: &str, message: &str) -> String {
    format!("{code}\n{message}\nhttps://svelte.dev/e/kit/{code}")
}
fn resolve(base: &str, path: &str) -> Result<String> {
    if path.starts_with("//") {
        return Ok(path.into());
    }
    let internal = Url::parse("a://").map_err(|e| e.to_string())?;
    let base = internal
        .join(base)
        .map_err(|e| format!("Invalid redirect base: {e}"))?;
    let url = base
        .join(path)
        .map_err(|e| format!("Invalid redirect location: {e}"))?;
    Ok(if url.scheme() == "a" {
        format!(
            "{}{}{}",
            url.path(),
            url.query().map(|s| format!("?{s}")).unwrap_or_default(),
            url.fragment().map(|s| format!("#{s}")).unwrap_or_default()
        )
    } else {
        url.to_string()
    })
}
/// decodeURI, not decodeURIComponent: retain reserved escapes verbatim, reject
/// malformed percent sequences and non-shortest/surrogate UTF-8 sequences.
fn decode_uri(value: &str) -> Result<String> {
    let bytes = value.as_bytes();
    let mut output = String::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'%' {
            let c = value[i..].chars().next().unwrap();
            output.push(c);
            i += c.len_utf8();
            continue;
        }
        let start = i;
        let first = percent(bytes, i).ok_or_else(|| format!("Failed to decode URI: {value}"))?;
        let count = match first {
            0..=0x7f => 1,
            0xc2..=0xdf => 2,
            0xe0..=0xef => 3,
            0xf0..=0xf4 => 4,
            _ => return Err(format!("Failed to decode URI: {value}")),
        };
        let mut scalar = vec![first];
        i += 3;
        for _ in 1..count {
            scalar.push(percent(bytes, i).ok_or_else(|| format!("Failed to decode URI: {value}"))?);
            i += 3;
        }
        let decoded =
            std::str::from_utf8(&scalar).map_err(|_| format!("Failed to decode URI: {value}"))?;
        if count == 1 && b";/?:@&=+$,#".contains(&first) {
            output.push_str(&value[start..i]);
        } else {
            output.push_str(decoded);
        }
    }
    Ok(output)
}
fn percent(bytes: &[u8], i: usize) -> Option<u8> {
    if bytes.get(i) != Some(&b'%') {
        return None;
    }
    let high = (*bytes.get(i + 1)? as char).to_digit(16)?;
    let low = (*bytes.get(i + 2)? as char).to_digit(16)?;
    Some((high * 16 + low) as u8)
}
fn redirect_html(location: &str) -> String {
    let mut script = String::from("\"");
    for c in location.chars() {
        match c {
            '"' => script.push_str("\\\""),
            '<' => script.push_str("\\u003C"),
            '\\' => script.push_str("\\\\"),
            '\n' => script.push_str("\\n"),
            '\r' => script.push_str("\\r"),
            '\t' => script.push_str("\\t"),
            '\u{8}' => script.push_str("\\b"),
            '\u{c}' => script.push_str("\\f"),
            '\u{2028}' => script.push_str("\\u2028"),
            '\u{2029}' => script.push_str("\\u2029"),
            c if c < ' ' => script.push_str(&format!("\\u{:04x}", c as u32)),
            c => script.push(c),
        }
    }
    script.push('"');
    let attribute = location.replace('&', "&amp;").replace('"', "&quot;");
    format!(
        "<script>location.href={script};</script><meta http-equiv=\"refresh\" content=\"0;url={attribute}\">"
    )
}

#[cfg(test)]
#[path = "prerender_save_tests.rs"]
mod tests;
