//! Pure seed/queue/coverage state for pinned SvelteKit prerendering (MIT,
//! copyright Svelte contributors). Sources: registered Kit 2.67.0–2.70.3 and
//! Kit 3.0.0-next.32/3.0.1 `core/postbuild/prerender.js`, `queue.js`, and
//! `utils/routing.js::get_route_segments`. No runtime, crawler, or filesystem.
//!
//! Lifecycle: new -> start -> initialize the actual runtime if Ready -> dispatch
//! take_ready visits; save/process dependencies/crawl/check entry matches before
//! complete_visit. Feed remote seeds only after the runtime evaluates inputs and
//! serializes arguments. Last visit completion closes an empty queue, even if a
//! remote input generator is still pending, matching upstream queue.js. finish
//! also closes an idle queue and checks hashlinks before unseen routes.
//!
//! AssetSnapshot holds POSIX names relative to the client directory and relative
//! to server/<appDir>/immutable. The host owns filesystem path normalization.
//! Observer inputs preserve upstream insertion order. RouteSeen comes from save;
//! crawled IDs/hashlink keys are already decodeURI-decoded by the crawler host.
//! check_entry_match and record_dependency_headers decode raw route-id headers.
//! take_ready flags outside-base requests; the host must run handleHttpError(404)
//! instead of requesting them. This module does not synthesize a Response.
//!
//! Custom handlers suspend mutations until the host reports a real callback
//! return/throw via resume_handler. Callback promises are not awaited upstream;
//! the host must acknowledge synchronous return. Queuing order is deterministic;
//! concurrency >1 intentionally leaves completion order to the host.
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
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use url::Url;

type Result<T> = std::result::Result<T, String>;
const MAX_ITEMS: usize = 100_000;
const MAX_TEXT: usize = 1024 * 1024;
const MAX_TOTAL_TEXT: usize = 64 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Router {
    Pathname,
    Hash,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PrerenderValue {
    True,
    False,
    Auto,
}
impl PrerenderValue {
    fn truthy(self) -> bool {
        self != Self::False
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum HandlerPolicy {
    Default,
    Fail,
    Warn,
    Ignore,
    Custom { name: String },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct QueueConfig {
    pub kit_version: String,
    pub base: String,
    /// Resolved Kit2 prerender.origin or Kit3 paths.origin/default, explicitly
    /// supplied. Request URL construction is literal origin + encoded path.
    pub origin: String,
    pub app_dir: String,
    pub router: Router,
    pub concurrency: usize,
    pub entries: Vec<String>,
    pub handle_missing_id: HandlerPolicy,
    pub handle_entry_generator_mismatch: HandlerPolicy,
    pub handle_unseen_routes: HandlerPolicy,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RouteMetadata {
    pub id: String,
    /// None means undefined/absent, excluded from the initial prerender Map.
    pub prerender: Option<PrerenderValue>,
    /// Actual evaluated entry paths; this module never invokes entries().
    pub entries: Option<Vec<String>>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AssetSnapshot {
    pub client_files: Vec<String>,
    pub server_immutable_files: Vec<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnqueueInput {
    pub decoded: String,
    pub encoded: Option<String>,
    pub referrer: Option<String>,
    pub generated_from_id: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum EnqueueResult {
    Queued { token: u64 },
    Duplicate,
    Asset { file: String },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VisitRequest {
    pub token: u64,
    pub decoded: String,
    pub encoded: String,
    pub url: String,
    pub referrer: Option<String>,
    pub generated_from_id: Option<String>,
    /// False means visit must invoke the host's linked 404 HTTP policy, without
    /// performing a runtime request (exact startsWith(base), not segment match).
    pub within_base: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RemoteSeed {
    pub id: String,
    pub has_arg: bool,
    /// Each value is the actual stringify_remote_arg result, in inputs order.
    /// The host evaluates inputs even if has_arg is false, as upstream does.
    pub serialized_arguments: Vec<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum StartStatus {
    Ready,
    Skipped,
    FallbackRequired,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartResult {
    pub status: StartStatus,
    pub seeds: Vec<EnqueueResult>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum QueueDiagnostic {
    MissingId {
        path: String,
        id: String,
        referrers: Vec<String>,
        message: String,
    },
    EntryGeneratorMismatch {
        generated_from_id: String,
        entry: String,
        matched_id: String,
        message: String,
    },
    UnseenRoutes {
        routes: Vec<String>,
        message: String,
    },
}
impl QueueDiagnostic {
    fn message(&self) -> &str {
        match self {
            Self::MissingId { message, .. }
            | Self::EntryGeneratorMismatch { message, .. }
            | Self::UnseenRoutes { message, .. } => message,
        }
    }
    fn handler(&self) -> &'static str {
        match self {
            Self::MissingId { .. } => "handleMissingId",
            Self::EntryGeneratorMismatch { .. } => "handleEntryGeneratorMismatch",
            Self::UnseenRoutes { .. } => "handleUnseenRoutes",
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum QueueEffect {
    Warning {
        diagnostic: QueueDiagnostic,
    },
    Handler {
        token: u64,
        name: String,
        diagnostic: QueueDiagnostic,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum PolicyStatus {
    Proceed,
    Finished,
    AwaitingHandler { token: u64 },
    Failed { message: String },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyResult {
    pub status: PolicyStatus,
    pub effects: Vec<QueueEffect>,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueSnapshot {
    pub prerender_map: Vec<(String, PrerenderValue)>,
    pub seen: Vec<String>,
    pub prerendered_routes: Vec<String>,
    pub expected_hashlinks: Vec<(String, Vec<String>)>,
    pub actual_hashlinks: Vec<(String, Vec<String>)>,
}
struct PendingHandler {
    token: u64,
    remaining: VecDeque<QueueDiagnostic>,
    finishing: bool,
}
pub struct QueueState {
    config: QueueConfig,
    kit3: bool,
    routes: Vec<RouteMetadata>,
    files: BTreeSet<String>,
    seen: BTreeSet<String>,
    queue: VecDeque<VisitRequest>,
    running: BTreeMap<u64, VisitRequest>,
    snapshot: QueueSnapshot,
    started: bool,
    skipped: bool,
    closed: bool,
    finished: bool,
    failed: bool,
    pending: Option<PendingHandler>,
    next_visit: u64,
    next_handler: u64,
    attempts: usize,
    observations: usize,
    text_bytes: usize,
}
impl QueueState {
    pub fn new(
        config: QueueConfig,
        routes: Vec<RouteMetadata>,
        assets: AssetSnapshot,
    ) -> Result<Self> {
        let kit3 = KitSyncVersion::parse(&config.kit_version)?.is_kit3();
        if config.concurrency == 0 || config.concurrency > 1024 {
            return Err("Prerender concurrency must be an integer in 1..=1024".into());
        }
        if !config.base.is_empty() && (!config.base.starts_with('/') || config.base.ends_with('/'))
        {
            return Err("Expected resolved paths.base".into());
        }
        relative_file(&config.app_dir)?;
        let origin =
            Url::parse(&config.origin).map_err(|e| format!("Invalid prerender origin: {e}"))?;
        if !matches!(origin.scheme(), "http" | "https")
            || origin.host_str().is_none()
            || origin.path() != "/"
            || origin.query().is_some()
            || origin.fragment().is_some()
        {
            return Err("Expected explicit HTTP(S) prerender origin".into());
        }
        if config.entries.len() > MAX_ITEMS
            || routes.len() > MAX_ITEMS
            || assets.client_files.len() + assets.server_immutable_files.len() > MAX_ITEMS
        {
            return Err("Prerender input item limit exceeded".into());
        }
        let mut bytes = 0;
        for s in [&config.base, &config.origin, &config.app_dir] {
            charge(&mut bytes, s)?;
        }
        for policy in [
            &config.handle_missing_id,
            &config.handle_entry_generator_mismatch,
            &config.handle_unseen_routes,
        ] {
            if let HandlerPolicy::Custom { name } = policy {
                charge(&mut bytes, name)?;
            }
        }
        for entry in &config.entries {
            charge(&mut bytes, entry)?;
            if entry != "*" && !entry.starts_with('/') {
                return Err(format!("Invalid configured prerender entry {entry:?}"));
            }
        }
        let mut route_ids = BTreeSet::new();
        let mut snapshot = QueueSnapshot::default();
        let mut count = routes.len();
        for route in &routes {
            charge(&mut bytes, &route.id)?;
            if !route.id.starts_with('/') {
                return Err("Expected slash-prefixed route metadata ID".into());
            }
            if !route_ids.insert(route.id.clone()) {
                return Err(format!("Duplicate route metadata Map key {}", route.id));
            }
            if let Some(value) = route.prerender {
                snapshot.prerender_map.push((route.id.clone(), value));
            }
            if let Some(entries) = &route.entries {
                count += entries.len();
                if count > MAX_ITEMS {
                    return Err("Prerender route entry limit exceeded".into());
                }
                for entry in entries {
                    charge(&mut bytes, entry)?;
                }
            }
        }
        let mut files = BTreeSet::new();
        for file in assets.client_files {
            relative_file(&file)?;
            charge(&mut bytes, &file)?;
            files.insert(file);
        }
        files.insert(format!("{}/env.js", config.app_dir));
        for file in assets.server_immutable_files {
            relative_file(&file)?;
            charge(&mut bytes, &file)?;
            files.insert(format!("{}/immutable/{file}", config.app_dir));
        }
        Ok(Self {
            config,
            kit3,
            routes,
            files,
            seen: BTreeSet::new(),
            queue: VecDeque::new(),
            running: BTreeMap::new(),
            snapshot,
            started: false,
            skipped: false,
            closed: false,
            finished: false,
            failed: false,
            pending: None,
            next_visit: 1,
            next_handler: 1,
            attempts: 0,
            observations: 0,
            text_bytes: bytes,
        })
    }
    pub fn snapshot(&self) -> &QueueSnapshot {
        &self.snapshot
    }
    pub fn is_closed(&self) -> bool {
        self.closed
    }
    /// Call after remote namespace discovery (except hash router, whose caller
    /// bypasses discovery). The returned Ready flag authorizes runtime init;
    /// dispatch visits only once that actual init succeeds.
    pub fn start(&mut self, has_prerender_remotes: bool) -> Result<StartResult> {
        if self.started {
            return Err("Prerender queue already started".into());
        }
        self.started = true;
        if self.config.router == Router::Hash {
            self.skipped = true;
            self.closed = true;
            return Ok(StartResult {
                status: StartStatus::FallbackRequired,
                seeds: vec![],
            });
        }
        if !has_prerender_remotes
            && !self
                .snapshot
                .prerender_map
                .iter()
                .any(|(_, value)| value.truthy())
        {
            self.skipped = true;
            self.closed = true;
            return Ok(StartResult {
                status: StartStatus::Skipped,
                seeds: vec![],
            });
        }
        let mut seeds = vec![];
        for entry in self.config.entries.clone() {
            if entry == "*" {
                for (id, value) in self.snapshot.prerender_map.clone() {
                    if !value.truthy() {
                        continue;
                    }
                    let retained: Vec<_> = segments(&id)
                        .into_iter()
                        .filter(|s| !s.starts_with("[["))
                        .collect();
                    let processed = format!("/{}", retained.join("/"));
                    if processed.contains('[') {
                        continue;
                    }
                    let path = format!("/{}", segments(&processed).join("/"));
                    seeds.push(self.enqueue(EnqueueInput {
                        decoded: format!("{}{path}", self.config.base),
                        encoded: None,
                        referrer: None,
                        generated_from_id: None,
                    })?);
                }
            } else {
                seeds.push(self.enqueue(EnqueueInput {
                    decoded: format!("{}{entry}", self.config.base),
                    encoded: None,
                    referrer: None,
                    generated_from_id: None,
                })?);
            }
        }
        for route in self.routes.clone() {
            if let Some(entries) = route.entries {
                for entry in entries {
                    seeds.push(self.enqueue(EnqueueInput {
                        decoded: format!("{}{entry}", self.config.base),
                        encoded: None,
                        referrer: None,
                        generated_from_id: Some(route.id.clone()),
                    })?);
                }
            }
        }
        Ok(StartResult {
            status: StartStatus::Ready,
            seeds,
        })
    }
    pub fn enqueue(&mut self, input: EnqueueInput) -> Result<EnqueueResult> {
        self.active()?;
        self.attempts += 1;
        if self.attempts > MAX_ITEMS {
            return Err("Prerender enqueue limit exceeded".into());
        }
        charge(&mut self.text_bytes, &input.decoded)?;
        if self.seen.contains(&input.decoded) {
            return Ok(EnqueueResult::Duplicate);
        }
        self.seen.insert(input.decoded.clone());
        self.snapshot.seen.push(input.decoded.clone());
        let file = String::from_utf16(
            &input
                .decoded
                .encode_utf16()
                .skip(self.config.base.encode_utf16().count() + 1)
                .collect::<Vec<_>>(),
        )
        .map_err(|_| "Unsupported non-scalar UTF-16 asset identity")?;
        if self.files.contains(&file) {
            return Ok(EnqueueResult::Asset { file });
        }
        // Upstream q.add checks closed after seen and asset exclusion.
        if self.closed {
            return Err("Cannot add tasks to a queue that has ended".into());
        }
        let encoded = input
            .encoded
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| encode_uri(&input.decoded));
        charge(&mut self.text_bytes, &encoded)?;
        for value in [&input.referrer, &input.generated_from_id]
            .into_iter()
            .flatten()
        {
            charge(&mut self.text_bytes, value)?;
        }
        let token = self.next_visit;
        self.next_visit += 1;
        let request = VisitRequest {
            token,
            within_base: input.decoded.starts_with(&self.config.base),
            url: format!("{}{encoded}", self.config.origin),
            decoded: input.decoded,
            encoded,
            referrer: input.referrer,
            generated_from_id: input.generated_from_id,
        };
        self.queue.push_back(request);
        Ok(EnqueueResult::Queued { token })
    }
    pub fn seed_remote(&mut self, seed: RemoteSeed) -> Result<Vec<EnqueueResult>> {
        self.active()?;
        charge(&mut self.text_bytes, &seed.id)?;
        if seed.serialized_arguments.len() > MAX_ITEMS {
            return Err("Prerender remote input limit exceeded".into());
        }
        let prefix = format!(
            "{}/{}/remote/{}",
            self.config.base, self.config.app_dir, seed.id
        );
        let mut results = vec![];
        if seed.has_arg {
            for argument in seed.serialized_arguments {
                charge(&mut self.text_bytes, &argument)?;
                results.push(self.enqueue(EnqueueInput {
                    decoded: format!("{prefix}/{argument}"),
                    encoded: None,
                    referrer: None,
                    generated_from_id: None,
                })?);
            }
        } else {
            results.push(self.enqueue(EnqueueInput {
                decoded: prefix,
                encoded: None,
                referrer: None,
                generated_from_id: None,
            })?);
        }
        Ok(results)
    }

    pub fn take_ready(&mut self) -> Result<Vec<VisitRequest>> {
        self.active()?;
        let mut requests = vec![];
        while self.running.len() < self.config.concurrency {
            let Some(request) = self.queue.pop_front() else {
                break;
            };
            self.running.insert(request.token, request.clone());
            requests.push(request);
        }
        Ok(requests)
    }
    pub fn complete_visit(&mut self, token: u64) -> Result<()> {
        self.active()?;
        if self.running.remove(&token).is_none() {
            return Err("Unknown or already completed visit token".into());
        }
        if self.running.is_empty() && self.queue.is_empty() {
            self.closed = true;
        }
        Ok(())
    }
    /// Abort the entire traversal after a real runtime/save/crawler failure.
    pub fn fail_visit(&mut self, token: u64, message: String) -> Result<PolicyResult> {
        self.active()?;
        if !self.running.contains_key(&token) {
            return Err("Unknown visit token".into());
        }
        self.failed = true;
        Ok(PolicyResult {
            status: PolicyStatus::Failed { message },
            effects: vec![],
        })
    }
    pub fn record_route(&mut self, decoded_route_id: &str) -> Result<()> {
        self.observe()?;
        charge(&mut self.text_bytes, decoded_route_id)?;
        if !self
            .snapshot
            .prerendered_routes
            .iter()
            .any(|s| s == decoded_route_id)
        {
            if self.snapshot.prerendered_routes.len() >= MAX_ITEMS {
                return Err("Prerender route observation limit exceeded".into());
            }
            self.snapshot
                .prerendered_routes
                .push(decoded_route_id.into());
        }
        Ok(())
    }
    /// Called for dependency responses before their save operation. A nonempty
    /// prerender header maps exact "true" to True, everything else to Auto;
    /// existing Auto is absorbing. The route need not exist in initial metadata.
    pub fn record_dependency_headers(
        &mut self,
        prerender: Option<&str>,
        encoded_route_id: Option<&str>,
    ) -> Result<()> {
        self.observe()?;
        let Some(value) = prerender.filter(|s| !s.is_empty()) else {
            return Ok(());
        };
        let Some(id) = encoded_route_id else {
            return Ok(());
        };
        charge(&mut self.text_bytes, id)?;
        let id = decode_uri(id)?;
        let value = if value == "true" {
            PrerenderValue::True
        } else {
            PrerenderValue::Auto
        };
        if let Some((_, old)) = self
            .snapshot
            .prerender_map
            .iter_mut()
            .find(|(key, _)| key == &id)
        {
            if *old != PrerenderValue::Auto {
                *old = value;
            }
        } else {
            if self.snapshot.prerender_map.len() >= MAX_ITEMS {
                return Err("Prerender route map limit exceeded".into());
            }
            self.snapshot.prerender_map.push((id, value));
        }
        Ok(())
    }
    pub fn record_crawled_ids(&mut self, decoded_path: &str, ids: Vec<String>) -> Result<()> {
        self.observe()?;
        charge(&mut self.text_bytes, decoded_path)?;
        if ids.len() > MAX_ITEMS {
            return Err("Crawled ID limit exceeded".into());
        }
        let mut values = vec![];
        let mut seen = BTreeSet::new();
        for id in ids {
            charge(&mut self.text_bytes, &id)?;
            if !self.kit3 || seen.insert(id.clone()) {
                values.push(id);
            }
        }
        if let Some((_, old)) = self
            .snapshot
            .actual_hashlinks
            .iter_mut()
            .find(|(path, _)| path == decoded_path)
        {
            *old = values;
        } else {
            if self.snapshot.actual_hashlinks.len() >= MAX_ITEMS {
                return Err("Crawled page limit exceeded".into());
            }
            self.snapshot
                .actual_hashlinks
                .push((decoded_path.into(), values));
        }
        Ok(())
    }
    /// Receives decodeURI(pathname + hash), preserving the source's first-# split.
    /// The crawler records this before enqueue, even if that visit is duplicate.
    pub fn expect_hashlink(&mut self, decoded_key: &str, referrer: &str) -> Result<()> {
        self.observe()?;
        charge(&mut self.text_bytes, decoded_key)?;
        charge(&mut self.text_bytes, referrer)?;
        if !decoded_key.contains('#') {
            return Err("Expected decoded pathname#fragment key".into());
        }
        if let Some((_, refs)) = self
            .snapshot
            .expected_hashlinks
            .iter_mut()
            .find(|(key, _)| key == decoded_key)
        {
            if !refs.iter().any(|r| r == referrer) {
                refs.push(referrer.into());
            }
        } else {
            if self.snapshot.expected_hashlinks.len() >= MAX_ITEMS {
                return Err("Hashlink limit exceeded".into());
            }
            self.snapshot
                .expected_hashlinks
                .push((decoded_key.into(), vec![referrer.into()]));
        }
        Ok(())
    }
    pub fn check_entry_match(
        &mut self,
        token: u64,
        encoded_route_id: Option<&str>,
    ) -> Result<PolicyResult> {
        self.observe()?;
        if let Some(header) = encoded_route_id {
            charge(&mut self.text_bytes, header)?;
        }
        let request = self.running.get(&token).ok_or("Unknown visit token")?;
        // Source decodes a present header even for non-generated entries.
        let decoded = encoded_route_id.map(decode_uri).transpose()?;
        let issue = match (&request.generated_from_id, decoded) {
            (Some(expected), Some(matched)) if expected != &matched => {
                Some(self.mismatch(expected, &request.decoded, &matched))
            }
            _ => None,
        };
        self.dispatch(issue.into_iter().collect(), false)
    }
    pub fn finish(&mut self) -> Result<PolicyResult> {
        self.active_or_skipped()?;
        if !self.running.is_empty() || !self.queue.is_empty() {
            return Err("Cannot finish prerender coverage while visits remain".into());
        }
        self.closed = true;
        if self.skipped {
            self.finished = true;
            return Ok(PolicyResult {
                status: PolicyStatus::Finished,
                effects: vec![],
            });
        }
        let mut issues = VecDeque::new();
        for (key, referrers) in &self.snapshot.expected_hashlinks {
            let (path, id) = key.split_once('#').ok_or("Invalid internal hashlink key")?;
            let Some((_, ids)) = self
                .snapshot
                .actual_hashlinks
                .iter()
                .find(|(p, _)| p == path)
            else {
                continue;
            };
            if id != "" && id != "top" && !ids.iter().any(|actual| actual == id) {
                issues.push_back(self.missing_id(path, id, referrers));
            }
        }
        let unseen: Vec<_> = self
            .snapshot
            .prerender_map
            .iter()
            .filter(|(id, value)| {
                *value == PrerenderValue::True && !self.snapshot.prerendered_routes.contains(id)
            })
            .map(|(id, _)| id.clone())
            .collect();
        if !unseen.is_empty() {
            issues.push_back(self.unseen(unseen));
        }
        self.dispatch(issues, true)
    }
    pub fn resume_handler(
        &mut self,
        token: u64,
        callback: std::result::Result<(), String>,
    ) -> Result<PolicyResult> {
        let pending = self
            .pending
            .as_ref()
            .ok_or("No pending prerender handler")?;
        if pending.token != token {
            return Err("Stale prerender handler token".into());
        }
        let pending = self.pending.take().unwrap();
        if let Err(message) = callback {
            self.failed = true;
            return Ok(PolicyResult {
                status: PolicyStatus::Failed { message },
                effects: vec![],
            });
        }
        self.dispatch(pending.remaining, pending.finishing)
    }
    fn observe(&mut self) -> Result<()> {
        self.active()?;
        self.observations += 1;
        if self.observations > MAX_ITEMS {
            return Err("Prerender observation limit exceeded".into());
        }
        Ok(())
    }
    fn active_or_skipped(&self) -> Result<()> {
        if !self.started {
            return Err("Prerender queue has not started".into());
        }
        if self.finished || self.failed {
            return Err("Prerender queue has finished or failed".into());
        }
        if self.pending.is_some() {
            return Err("Resume pending prerender handler first".into());
        }
        Ok(())
    }
    fn active(&self) -> Result<()> {
        self.active_or_skipped()?;
        if self.skipped {
            return Err("Prerender traversal was skipped or requires hash fallback".into());
        }
        Ok(())
    }
    fn dispatch(
        &mut self,
        mut issues: VecDeque<QueueDiagnostic>,
        finishing: bool,
    ) -> Result<PolicyResult> {
        let mut effects = vec![];
        while let Some(issue) = issues.pop_front() {
            let policy = match &issue {
                QueueDiagnostic::MissingId { .. } => &self.config.handle_missing_id,
                QueueDiagnostic::EntryGeneratorMismatch { .. } => {
                    &self.config.handle_entry_generator_mismatch
                }
                QueueDiagnostic::UnseenRoutes { .. } => &self.config.handle_unseen_routes,
            };
            match policy {
                HandlerPolicy::Ignore => {}
                HandlerPolicy::Warn => effects.push(QueueEffect::Warning { diagnostic: issue }),
                HandlerPolicy::Fail | HandlerPolicy::Default => {
                    let message = if matches!(policy, HandlerPolicy::Default) {
                        format!(
                            "{}\nTo suppress or handle this error, implement `{}` in https://svelte.dev/docs/kit/configuration#prerender",
                            issue.message(),
                            issue.handler()
                        )
                    } else {
                        issue.message().into()
                    };
                    self.failed = true;
                    return Ok(PolicyResult {
                        status: PolicyStatus::Failed { message },
                        effects,
                    });
                }
                HandlerPolicy::Custom { name } => {
                    let token = self.next_handler;
                    self.next_handler += 1;
                    effects.push(QueueEffect::Handler {
                        token,
                        name: name.clone(),
                        diagnostic: issue,
                    });
                    self.pending = Some(PendingHandler {
                        token,
                        remaining: issues,
                        finishing,
                    });
                    return Ok(PolicyResult {
                        status: PolicyStatus::AwaitingHandler { token },
                        effects,
                    });
                }
            }
        }
        if finishing {
            self.finished = true;
        }
        Ok(PolicyResult {
            status: if finishing {
                PolicyStatus::Finished
            } else {
                PolicyStatus::Proceed
            },
            effects,
        })
    }
    fn mismatch(&self, expected: &str, entry: &str, matched: &str) -> QueueDiagnostic {
        let message = if self.kit3 {
            let matched = if matched == entry {
                "a static route"
            } else {
                matched
            };
            kit_message(
                "prerender_entry_generator_mismatch",
                &format!(
                    "The `entries` export from `{expected}` generated entry `{entry}`, which was matched by `{matched}`"
                ),
            )
        } else {
            format!(
                "The entries export from {expected} generated entry {entry}, which was matched by {matched} - see the `handleEntryGeneratorMismatch` option in https://svelte.dev/docs/kit/configuration#prerender for more info."
            )
        };
        QueueDiagnostic::EntryGeneratorMismatch {
            generated_from_id: expected.into(),
            entry: entry.into(),
            matched_id: matched.into(),
            message,
        }
    }
    fn missing_id(&self, path: &str, id: &str, referrers: &[String]) -> QueueDiagnostic {
        let message = if self.kit3 {
            kit_message(
                "prerender_missing_id",
                &format!(
                    "The following pages contain links to `{path}#{id}`, but no element with `id=\"{id}\"` exists on `{path}`:\n{}",
                    bullets(referrers)
                ),
            )
        } else {
            format!(
                "The following pages contain links to {path}#{id}, but no element with id=\"{id}\" exists on {path} - see the `handleMissingId` option in https://svelte.dev/docs/kit/configuration#prerender for more info:{}",
                referrers
                    .iter()
                    .map(|s| format!("\n  - {s}"))
                    .collect::<String>()
            )
        };
        QueueDiagnostic::MissingId {
            path: path.into(),
            id: id.into(),
            referrers: referrers.to_vec(),
            message,
        }
    }
    fn unseen(&self, routes: Vec<String>) -> QueueDiagnostic {
        let message = if self.kit3 {
            kit_message(
                "prerender_unseen_routes",
                &format!(
                    "The following routes were marked as prerenderable, but were not prerendered because they were not found while crawling your app:\n{}",
                    bullets(&routes)
                ),
            )
        } else {
            format!(
                "The following routes were marked as prerenderable, but were not prerendered because they were not found while crawling your app:\n{}\n\nSee the `handleUnseenRoutes` option in https://svelte.dev/docs/kit/configuration#prerender for more info.",
                bullets(&routes)
            )
        };
        QueueDiagnostic::UnseenRoutes { routes, message }
    }
}
fn segments(id: &str) -> Vec<&str> {
    // Source slice(1) is UTF-16; route IDs are validated slash-prefixed by Kit's
    // route inventory, so removing this ASCII slash is exact.
    id.strip_prefix('/')
        .unwrap_or(id)
        .split('/')
        .filter(|s| {
            !s.is_empty()
                && !(s.starts_with('(')
                    && s.ends_with(')')
                    && s.len() > 2
                    && !s[1..s.len() - 1].contains(')'))
        })
        .collect()
}
fn charge(total: &mut usize, value: &str) -> Result<()> {
    if value.len() > MAX_TEXT {
        return Err("Prerender text length limit exceeded".into());
    }
    *total = total
        .checked_add(value.len() + 24)
        .ok_or("Prerender text budget overflow")?;
    if *total > MAX_TOTAL_TEXT {
        return Err("Prerender text budget exceeded".into());
    }
    Ok(())
}
fn relative_file(file: &str) -> Result<()> {
    if file.is_empty()
        || file.contains(['\\', '\0'])
        || file
            .split('/')
            .any(|s| s.is_empty() || s == "." || s == "..")
    {
        return Err("Expected normalized relative POSIX asset filename".into());
    }
    Ok(())
}
fn encode_uri(value: &str) -> String {
    let mut output = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || b";,/?:@&=+$#-_.!~*'()".contains(&byte) {
            output.push(byte as char);
        } else {
            output.push_str(&format!("%{byte:02X}"));
        }
    }
    output
}
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
    Some(
        ((*bytes.get(i + 1)? as char).to_digit(16)? * 16
            + (*bytes.get(i + 2)? as char).to_digit(16)?) as u8,
    )
}
fn kit_message(code: &str, message: &str) -> String {
    format!("{code}\n{message}\nhttps://svelte.dev/e/kit/{code}")
}
fn bullets(items: &[String]) -> String {
    items
        .iter()
        .map(|s| format!("  - {s}"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
#[path = "prerender_queue_tests.rs"]
mod tests;
