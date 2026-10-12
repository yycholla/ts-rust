//! Persistent native prerender transport. Only explicit immutable snapshots and
//! host entropy cross the process boundary; application code stays in the runtime.
use crate::{
    execution_snapshot::ExecutionSnapshot,
    native_runtime::{NativeRuntime, Process},
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PrerenderOptions {
    pub server_entry: String,
    pub origin: String,
    pub base: String,
    pub app_dir: String,
    pub static_assets: BTreeMap<String, Vec<u8>>,
    /// Genuine host-provided random bytes. Deterministic values belong only in
    /// fixtures; production callers must supply a cryptographic entropy source.
    #[serde(skip)]
    pub entropy: Vec<u8>,
}

/// Raw HTTP ByteStrings, before consuming a response body. Headers are allowed
/// to change between this snapshot and Response; do not compare them for identity.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResponseHead {
    pub status: u16,
    pub status_text: Vec<u8>,
    pub headers: Vec<(Vec<u8>, Vec<u8>)>,
    pub route_id: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VisitHead {
    pub token: u64,
    pub server_build_id: String,
    pub encoded_path: String,
    pub decoded_path: String,
    pub response: ResponseHead,
}
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DependencyHead {
    pub path: String,
    pub encoded_path: String,
    pub decoded_path: String,
    pub response: ResponseHead,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Response {
    pub status: u16,
    pub status_text: Vec<u8>,
    pub headers: Vec<(Vec<u8>, Vec<u8>)>,
    pub body: Vec<u8>,
    pub route_id: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Dependency {
    pub path: String,
    pub encoded_path: String,
    pub decoded_path: String,
    pub response: Response,
}
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Visit {
    pub server_build_id: String,
    pub encoded_path: String,
    pub decoded_path: String,
    pub response: Response,
    pub dependencies: Vec<Dependency>,
}

pub struct NativePrerender {
    process: Process,
    session: String,
    build: String,
    failed: bool,
    progress: VisitProgress,
}
impl NativeRuntime {
    /// Configure a fresh isolated rendering realm. Starting application hooks
    /// is a separate operation, so callers may skip empty prerender work.
    pub fn prerender(
        &self,
        snapshot: &ExecutionSnapshot,
        options: PrerenderOptions,
    ) -> Result<NativePrerender, String> {
        let mut process = Process::start(self)?;
        let mut input = serde_json::to_value(&options).map_err(|e| e.to_string())?;
        input["bootstrap"] = snapshot.bootstrap_json()?;
        let created = process.request(json!({
            "op":"createPrerender", "input":input,
            "cryptoOptions":{"secureContext":true,"entropy":options.entropy}
        }))?;
        if created["serverBuildId"] != snapshot.bootstrap.server_build_id {
            return Err("Native prerender initialized a different server build".into());
        }
        let session = created["session"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or("Native prerender omitted session identity")?
            .to_owned();
        Ok(NativePrerender {
            process,
            session,
            build: snapshot.bootstrap.server_build_id.clone(),
            failed: false,
            progress: VisitProgress::default(),
        })
    }
}
impl NativePrerender {
    fn operation<T>(
        &mut self,
        f: impl FnOnce(&mut Self) -> Result<T, String>,
    ) -> Result<T, String> {
        if self.failed {
            return Err("Native prerender transport is failed".into());
        }
        let result = f(self);
        if result.is_err() {
            self.failed = true;
        }
        result
    }
    pub fn start(&mut self) -> Result<(), String> {
        self.operation(|this| {
            let result = this
                .process
                .request(json!({"op":"startPrerender","session":this.session}))?;
            if result != json!({"started":true}) {
                return Err("Native prerender did not start".into());
            }
            Ok(())
        })
    }
    pub fn visit(&mut self, encoded_path: &str) -> Result<Visit, String> {
        self.operation(|this| {
            this.progress.ensure_idle()?;
            let result = this.process.request(
                json!({"op":"visitPrerender","session":this.session,"path":encoded_path}),
            )?;
            let visit: Visit = serde_json::from_value(result)
                .map_err(|e| format!("Invalid native prerender visit: {e}"))?;
            validate_visit_identity(
                &visit.server_build_id,
                &visit.encoded_path,
                &visit.decoded_path,
                &this.build,
                encoded_path,
            )?;
            visit.response.normalized_headers()?;
            for dependency in &visit.dependencies {
                validate_dependency_identity(
                    &dependency.path,
                    &dependency.encoded_path,
                    &dependency.decoded_path,
                )?;
                dependency.response.normalized_headers()?;
            }
            // Runtime's convenience collector also allocates one visit token.
            this.progress.note_collected_visit()?;
            Ok(visit)
        })
    }
    /// Inspect the response before consuming its body, allowing route policy to
    /// execute in Kit order. Exactly one staged visit may be active at a time.
    pub fn begin_visit(&mut self, encoded_path: &str) -> Result<VisitHead, String> {
        self.operation(|this| {
            this.progress.ensure_idle()?;
            let result = this.process.request(
                json!({"op":"beginPrerenderVisit","session":this.session,"path":encoded_path}),
            )?;
            let head: VisitHead = serde_json::from_value(result)
                .map_err(|e| format!("Invalid native prerender visit head: {e}"))?;
            this.progress
                .accept_begin(&head, &this.build, encoded_path)?;
            Ok(head)
        })
    }
    pub fn read_main(&mut self, token: u64) -> Result<Response, String> {
        self.operation(|this| {
            this.progress.require_main(token)?;
            let result = this
                .process
                .request(json!({"op":"readPrerenderMain","session":this.session,"token":token}))?;
            let response: Response = serde_json::from_value(result)
                .map_err(|e| format!("Invalid native prerender main response: {e}"))?;
            this.progress.accept_main(token, &response)?;
            Ok(response)
        })
    }
    /// A None response ends the staged visit. Publish a saved main/dependency
    /// before calling this again when subsequent runtime body reads need it.
    pub fn next_dependency(&mut self, token: u64) -> Result<Option<DependencyHead>, String> {
        self.operation(|this| {
            this.progress.require_next(token)?;
            let result = this.process.request(
                json!({"op":"nextPrerenderDependency","session":this.session,"token":token}),
            )?;
            let head: Option<DependencyHead> = serde_json::from_value(result)
                .map_err(|e| format!("Invalid native prerender dependency head: {e}"))?;
            this.progress.accept_next(token, head.as_ref())?;
            Ok(head)
        })
    }
    pub fn read_dependency(&mut self, token: u64) -> Result<Dependency, String> {
        self.operation(|this| {
            this.progress.require_dependency(token)?;
            let result = this.process.request(
                json!({"op":"readPrerenderDependency","session":this.session,"token":token}),
            )?;
            let dependency: Dependency = serde_json::from_value(result)
                .map_err(|e| format!("Invalid native prerender dependency response: {e}"))?;
            this.progress.accept_dependency(token, &dependency)?;
            Ok(dependency)
        })
    }
    pub fn publish(&mut self, file: &str, bytes: &[u8]) -> Result<(), String> {
        self.operation(|this| {
            let result = this.process.request(
                json!({"op":"publishPrerender","session":this.session,"file":file,"bytes":bytes}),
            )?;
            if result != json!({"published":true}) {
                return Err("Native prerender did not publish the asset".into());
            }
            Ok(())
        })
    }
    /// Consumes the client so a disposed session cannot be reused. Dropping an
    /// undisposed client also kills/reaps its isolated process and joins its IO.
    pub fn dispose(mut self) -> Result<(), String> {
        let result = self
            .process
            .request(json!({"op":"dispose","session":self.session}))?;
        if result != json!({"disposed":true}) {
            return Err("Native prerender did not dispose the session".into());
        }
        Ok(())
    }
}

impl Response {
    /// Lookup view of Object.fromEntries(new Headers(raw).entries()). Each byte
    /// becomes the same U+0000..U+00FF character; no UTF-8 decoding is performed.
    /// Ordinary duplicates combine with ", "; separate set-cookie iterations
    /// overwrite the same property, leaving the last cookie. Key iteration order
    /// is not a JS object's integer-index ordering; this is a lookup map.
    pub fn normalized_headers(&self) -> Result<BTreeMap<String, String>, String> {
        normalized_headers(&self.headers)
    }
    pub fn status_text_bytestring(&self) -> String {
        bytestring(&self.status_text)
    }
}
impl ResponseHead {
    pub fn normalized_headers(&self) -> Result<BTreeMap<String, String>, String> {
        normalized_headers(&self.headers)
    }
    pub fn status_text_bytestring(&self) -> String {
        bytestring(&self.status_text)
    }
}
fn bytestring(bytes: &[u8]) -> String {
    bytes.iter().copied().map(char::from).collect()
}
fn normalized_headers(headers: &[(Vec<u8>, Vec<u8>)]) -> Result<BTreeMap<String, String>, String> {
    if headers.len() > 16_384 {
        return Err("Native prerender header count exceeds limit".into());
    }
    let mut result = BTreeMap::<String, String>::new();
    let mut size = 0usize;
    for (name, value) in headers {
        size = size
            .checked_add(name.len())
            .and_then(|s| s.checked_add(value.len() + 2))
            .ok_or("Native prerender header size overflow")?;
        if size > 1024 * 1024 {
            return Err("Native prerender header bytes exceed limit".into());
        }
        if name.is_empty()
            || !name
                .iter()
                .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(b))
        {
            return Err("Invalid native prerender HTTP header name".into());
        }
        let name: String = name
            .iter()
            .map(|b| char::from(b.to_ascii_lowercase()))
            .collect();
        let trim = |b: &u8| matches!(b, b' ' | b'\t' | b'\r' | b'\n');
        let start = value.iter().position(|b| !trim(b)).unwrap_or(value.len());
        let end = value
            .iter()
            .rposition(|b| !trim(b))
            .map_or(start, |i| i + 1);
        let value = &value[start..end];
        if value.iter().any(|b| matches!(b, 0 | b'\r' | b'\n')) {
            return Err("Invalid native prerender HTTP header value".into());
        }
        let value = bytestring(value);
        if name != "set-cookie" {
            if let Some(previous) = result.get_mut(&name) {
                previous.push_str(", ");
                previous.push_str(&value);
                continue;
            }
        }
        result.insert(name, value);
    }
    Ok(result)
}

#[derive(Default)]
struct VisitProgress {
    last_token: u64,
    active: Option<ActiveVisit>,
}
struct ActiveVisit {
    token: u64,
    main_read: bool,
    dependency: Option<DependencyIdentity>,
}
#[derive(Debug, PartialEq, Eq)]
struct DependencyIdentity {
    path: String,
    encoded: String,
    decoded: String,
}
impl VisitProgress {
    fn note_collected_visit(&mut self) -> Result<(), String> {
        self.ensure_idle()?;
        self.last_token = self
            .last_token
            .checked_add(1)
            .ok_or("Native prerender token overflow")?;
        Ok(())
    }
    fn ensure_idle(&self) -> Result<(), String> {
        if self.active.is_some() {
            Err("Finish the active native prerender visit first".into())
        } else {
            Ok(())
        }
    }
    fn active(&self, token: u64) -> Result<&ActiveVisit, String> {
        self.active
            .as_ref()
            .filter(|active| active.token == token)
            .ok_or_else(|| "Unknown active native prerender token".into())
    }
    fn accept_begin(&mut self, head: &VisitHead, build: &str, path: &str) -> Result<(), String> {
        self.ensure_idle()?;
        validate_visit_identity(
            &head.server_build_id,
            &head.encoded_path,
            &head.decoded_path,
            build,
            path,
        )?;
        if head.token == 0 || head.token <= self.last_token {
            return Err("Native prerender returned a reused or invalid visit token".into());
        }
        head.response.normalized_headers()?;
        self.last_token = head.token;
        self.active = Some(ActiveVisit {
            token: head.token,
            main_read: false,
            dependency: None,
        });
        Ok(())
    }
    fn require_main(&self, token: u64) -> Result<(), String> {
        if self.active(token)?.main_read {
            return Err("Native prerender main body was already read".into());
        }
        Ok(())
    }
    fn accept_main(&mut self, token: u64, response: &Response) -> Result<(), String> {
        self.require_main(token)?;
        response.normalized_headers()?;
        self.active.as_mut().unwrap().main_read = true;
        Ok(())
    }
    fn require_next(&self, token: u64) -> Result<(), String> {
        let active = self.active(token)?;
        if !active.main_read || active.dependency.is_some() {
            return Err(
                "Read the current native prerender response before advancing dependencies".into(),
            );
        }
        Ok(())
    }
    fn accept_next(&mut self, token: u64, head: Option<&DependencyHead>) -> Result<(), String> {
        self.require_next(token)?;
        if let Some(head) = head {
            validate_dependency_identity(&head.path, &head.encoded_path, &head.decoded_path)?;
            head.response.normalized_headers()?;
            // Runtime deliberately does not decode dependency route IDs until
            // policy/collection time. A decoded head would change error order.
            if head.response.route_id.is_some() {
                return Err(
                    "Native prerender dependency head unexpectedly decoded its route ID".into(),
                );
            }
            self.active.as_mut().unwrap().dependency = Some(DependencyIdentity {
                path: head.path.clone(),
                encoded: head.encoded_path.clone(),
                decoded: head.decoded_path.clone(),
            });
        } else {
            self.active = None;
        }
        Ok(())
    }
    fn require_dependency(&self, token: u64) -> Result<(), String> {
        if self.active(token)?.dependency.is_none() {
            return Err("Inspect the next native prerender dependency before reading it".into());
        }
        Ok(())
    }
    fn accept_dependency(&mut self, token: u64, dependency: &Dependency) -> Result<(), String> {
        self.require_dependency(token)?;
        let expected = self.active(token)?.dependency.as_ref().unwrap();
        if dependency.path != expected.path
            || dependency.encoded_path != expected.encoded
            || dependency.decoded_path != expected.decoded
        {
            return Err("Native prerender dependency does not match its inspected identity".into());
        }
        dependency.response.normalized_headers()?;
        // Body getters/stream pulls may mutate headers. Compare path identity,
        // never the earlier head's header snapshot or route-id value.
        self.active.as_mut().unwrap().dependency = None;
        Ok(())
    }
}
fn validate_visit_identity(
    build: &str,
    encoded: &str,
    decoded: &str,
    expected_build: &str,
    expected_path: &str,
) -> Result<(), String> {
    if build != expected_build || encoded != expected_path || decode_uri(encoded)? != decoded {
        return Err("Native prerender visit does not match its build and request".into());
    }
    Ok(())
}
fn validate_dependency_identity(path: &str, encoded: &str, decoded: &str) -> Result<(), String> {
    if path.len() > 1024 * 1024 || encoded.len() > 1024 * 1024 || decoded.len() > 1024 * 1024 {
        return Err("Native prerender dependency identity exceeds limit".into());
    }
    let base = url::Url::parse("http://localhost").map_err(|e| e.to_string())?;
    let url = base
        .join(path)
        .map_err(|e| format!("Invalid native prerender dependency URL: {e}"))?;
    if url.path() != encoded || decode_uri(encoded)? != decoded {
        return Err("Native prerender dependency URL and pathname do not match".into());
    }
    Ok(())
}
fn decode_uri(value: &str) -> Result<String, String> {
    if value.len() > 1024 * 1024 {
        return Err("Native prerender pathname exceeds limit".into());
    }
    let bytes = value.as_bytes();
    let mut i = 0;
    let mut result = String::new();
    while i < bytes.len() {
        if bytes[i] != b'%' {
            let c = value[i..].chars().next().unwrap();
            result.push(c);
            i += c.len_utf8();
            continue;
        }
        let start = i;
        let first = percent(bytes, i).ok_or("Invalid native prerender encoded pathname")?;
        let count = match first {
            0..=0x7f => 1,
            0xc2..=0xdf => 2,
            0xe0..=0xef => 3,
            0xf0..=0xf4 => 4,
            _ => return Err("Invalid native prerender encoded pathname".into()),
        };
        let mut scalar = vec![first];
        i += 3;
        for _ in 1..count {
            scalar.push(percent(bytes, i).ok_or("Invalid native prerender encoded pathname")?);
            i += 3;
        }
        let text = std::str::from_utf8(&scalar)
            .map_err(|_| "Invalid native prerender encoded pathname")?;
        if count == 1 && b";/?:@&=+$,#".contains(&first) {
            result.push_str(&value[start..i]);
        } else {
            result.push_str(text);
        }
    }
    Ok(result)
}
fn percent(bytes: &[u8], i: usize) -> Option<u8> {
    if bytes.get(i) != Some(&b'%') {
        return None;
    }
    Some(
        ((*bytes.get(i + 1)? as char).to_digit(16)? * 16
            + (*bytes.get(i + 2)? as char).to_digit(16)?) as u8,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    fn response_head() -> ResponseHead {
        ResponseHead {
            status: 200,
            status_text: b"OK".to_vec(),
            headers: vec![],
            route_id: None,
        }
    }
    fn response() -> Response {
        Response {
            status: 200,
            status_text: b"OK".to_vec(),
            headers: vec![],
            route_id: None,
            body: vec![],
        }
    }
    fn visit_head() -> VisitHead {
        VisitHead {
            token: 1,
            server_build_id: "build".into(),
            encoded_path: "/%C3%A9%2F".into(),
            decoded_path: "/é%2F".into(),
            response: response_head(),
        }
    }
    fn dependency_head() -> DependencyHead {
        DependencyHead {
            path: "https://example.org/a%20b?q=1#fragment".into(),
            encoded_path: "/a%20b".into(),
            decoded_path: "/a b".into(),
            response: response_head(),
        }
    }
    #[test]
    fn headers_match_iteration_folding_and_bytestrings() {
        let mut head = response_head();
        head.status_text = vec![0xc3, 0xa9];
        head.headers = vec![
            (b"X-Byte".to_vec(), vec![0x80, 0xff]),
            (b"x-byte".to_vec(), b"next".to_vec()),
            (b"Set-Cookie".to_vec(), b"one=1".to_vec()),
            (b"SET-cookie".to_vec(), b"two=2".to_vec()),
            (b"Empty".to_vec(), b"".to_vec()),
            (b"empty".to_vec(), b" second \t".to_vec()),
        ];
        let expected = BTreeMap::from([
            ("x-byte".into(), "\u{80}ÿ, next".into()),
            ("set-cookie".into(), "two=2".into()),
            ("empty".into(), ", second".into()),
        ]);
        assert_eq!(head.normalized_headers().unwrap(), expected);
        assert_eq!(head.status_text_bytestring(), "Ã©");
        let mut full = response();
        full.headers = head.headers;
        full.status_text = head.status_text;
        assert_eq!(full.normalized_headers().unwrap(), expected);
        assert_eq!(full.status_text_bytestring(), "Ã©");
    }
    #[test]
    fn header_view_rejects_invalid_names_values_and_bounds() {
        for headers in [
            vec![(b"bad name".to_vec(), vec![])],
            vec![(vec![0x80], vec![])],
            vec![(b"x".to_vec(), b"a\nb".to_vec())],
            vec![(b"x".to_vec(), vec![0])],
        ] {
            assert!(normalized_headers(&headers).is_err());
        }
        assert_eq!(
            normalized_headers(&[(b"x".to_vec(), b"\r\n value \t".to_vec())]).unwrap()["x"],
            "value"
        );
        assert!(normalized_headers(&vec![(b"x".to_vec(), vec![]); 16_385]).is_err());
    }
    #[test]
    fn staged_order_tokens_and_identity_are_enforced() {
        let mut progress = VisitProgress::default();
        let head = visit_head();
        progress
            .accept_begin(&head, "build", &head.encoded_path)
            .unwrap();
        assert!(progress.ensure_idle().is_err());
        assert!(progress.require_main(2).is_err());
        assert!(progress.require_next(1).is_err());
        assert!(progress.require_dependency(1).is_err());
        progress.accept_main(1, &response()).unwrap();
        assert!(progress.require_main(1).is_err());
        let dep = dependency_head();
        progress.accept_next(1, Some(&dep)).unwrap();
        assert!(progress.require_next(1).is_err());
        let mut full = Dependency {
            path: "/different".into(),
            encoded_path: dep.encoded_path.clone(),
            decoded_path: dep.decoded_path.clone(),
            response: response(),
        };
        assert!(progress.accept_dependency(1, &full).is_err());
        full.path = dep.path;
        progress.accept_dependency(1, &full).unwrap();
        assert!(progress.require_dependency(1).is_err());
        progress.accept_next(1, None).unwrap();
        progress.ensure_idle().unwrap();
        assert!(progress.require_next(1).is_err());
        assert!(
            progress
                .accept_begin(&head, "build", &head.encoded_path)
                .is_err()
        );
    }
    #[test]
    fn body_time_header_mutations_are_valid() {
        let mut progress = VisitProgress::default();
        let mut head = visit_head();
        head.response.headers = vec![(b"x-sveltekit-routeid".to_vec(), b"before".to_vec())];
        head.response.route_id = Some("before".into());
        progress
            .accept_begin(&head, "build", &head.encoded_path)
            .unwrap();
        let mut full = response();
        full.headers = vec![(b"x-new".to_vec(), b"after".to_vec())];
        full.route_id = Some("changed-before-read".into());
        progress.accept_main(1, &full).unwrap();
        let dep = dependency_head();
        progress.accept_next(1, Some(&dep)).unwrap();
        progress
            .accept_dependency(
                1,
                &Dependency {
                    path: dep.path,
                    encoded_path: dep.encoded_path,
                    decoded_path: dep.decoded_path,
                    response: full,
                },
            )
            .unwrap();
    }
    #[test]
    fn incorrect_build_decoding_and_dependency_source_fail() {
        let mut progress = VisitProgress::default();
        let mut head = visit_head();
        assert!(
            progress
                .accept_begin(&head, "other", &head.encoded_path)
                .is_err()
        );
        assert!(progress.accept_begin(&head, "build", "/other").is_err());
        head.decoded_path = "/é/".into();
        assert!(
            progress
                .accept_begin(&head, "build", &head.encoded_path)
                .is_err()
        );
        assert!(validate_dependency_identity("/actual?q=1", "/other", "/other").is_err());
        assert!(decode_uri("/%ED%A0%80").is_err());
        assert_eq!(decode_uri("/%3f%F0%9F%98%80").unwrap(), "/%3f😀");
        validate_dependency_identity("../é", "/%C3%A9", "/é").unwrap();
    }
    #[test]
    fn typed_protocol_shapes_reject_unexpected_fields() {
        let head = visit_head();
        let mut json = serde_json::to_value(&head).unwrap();
        json["unknown"] = json!(true);
        assert!(serde_json::from_value::<VisitHead>(json).is_err());
        let json = serde_json::to_value(dependency_head()).unwrap();
        assert_eq!(json["encodedPath"], "/a%20b");
        assert_eq!(json["response"]["routeId"], serde_json::Value::Null);
    }
    #[test]
    fn collected_visits_also_reserve_token_identity() {
        let mut progress = VisitProgress::default();
        progress.note_collected_visit().unwrap();
        let mut head = visit_head();
        assert!(
            progress
                .accept_begin(&head, "build", &head.encoded_path)
                .is_err()
        );
        head.token = 2;
        progress
            .accept_begin(&head, "build", &head.encoded_path)
            .unwrap();
        assert!(progress.note_collected_visit().is_err());
    }
}
