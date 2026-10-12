//! Pure composition of Kit's save, crawl, and queue policies. This is NOT a
//! runtime driver: StartVisit/Publish/ManifestAsset effects must be applied in
//! order by the scheduler inside the real JS promise graph. No IO is performed.
//!
//! Source order: pinned Kit2/Kit3 core/postbuild/prerender.js::visit/save. Inspect
//! main headers and apply entry policy before reading its body; save main; then
//! inspect/update metadata/read/save each dependency; finally re-read main
//! headers, crawl its retained bytes, and complete the visit. Complete all visits
//! before finish. Handler promises are NOT awaited; resume reports synchronous
//! callback return/throw. Every failure is explicit and poisons this composition.
//!
//! main_head/crawl_main require actual current snapshots. In particular, earlier
//! main headers cannot be substituted for the final snapshot if dependency body
//! getters/pulls may mutate them. Queue tokens belong to this policy, not to the
//! runtime transport; the scheduler binds them to its own runtime continuations.
//!
//! Files remain in memory relative to output/prerendered. SavedAlias may point
//! to a non-written file (e.g. 204): Publish is emitted only for bytes actually
//! present. Hosts must preserve a missing alias as a read failure, never publish
//! empty bytes. Outside-base visits invoke standalone HTTP policy without a
//! response. An internal work deque retains every remaining ready visit/save/
//! crawl effect across synchronous callbacks; rejected stale replies preserve it.
//!
//! OutsideBaseReturned settles the async visit only; its queue slot stays held.
//! Call complete_outside_visit from the scheduler's SECOND queue promise reaction
//! (after task fulfil/reject), never while processing that effect synchronously.
//! Kit2 wraps the visit with Promise.resolve; Kit3 uses async-return adoption.
//! Preserve those real promise chains rather than guessing microtask counts.
//!
//! LIMITATION: SaveState permanently fails on HTTP policy rejection. Until that
//! primitive supports per-visit failures, outside-base Fail/default/throw still
//! terminates this composition. It cannot yet model upstream's rejected visit
//! promise, subsequent queue release, and continued unrelated task dispatch.
use crate::prerender_transport::{Dependency, DependencyHead, Response, ResponseHead};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use ts_svelte_kit::{
    prerender_crawl::{self, CrawlProfile},
    prerender_queue::{
        self as queue, AssetSnapshot, EnqueueInput, HandlerPolicy, QueueConfig, QueueState,
        RemoteSeed, RouteMetadata, VisitRequest,
    },
    prerender_save::{
        self as save, Category, OutputInventory, ReferenceType, SaveConfig, SaveEffect, SaveInput,
        SaveState,
    },
    sync_versions::KitSyncVersion,
};

type Result<T> = std::result::Result<T, String>;
const MAX_RETAINED: usize = 64 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PolicyConfig {
    pub save: SaveConfig,
    pub queue: QueueConfig,
    pub crawl: bool,
    pub invalid_url: HandlerPolicy,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum HandlerSource {
    Save,
    Queue,
    InvalidUrl,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandlerToken {
    pub source: HandlerSource,
    pub token: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum Effect {
    InitializeRuntime,
    /// The async outside-base visit returned; its queue slot is still held.
    OutsideBaseReturned {
        visit_token: u64,
    },
    InvalidUrlHandler {
        handle: HandlerToken,
        name: String,
        href: String,
        referrer: String,
        message: String,
    },
    StartVisit {
        request: VisitRequest,
    },
    /// Actual output bytes; does not by itself make the file readable through
    /// the runtime's saved map. A subsequent SavedAlias/Publish does that.
    Write {
        path: String,
        bytes: Vec<u8>,
    },
    ManifestAsset {
        file: String,
    },
    SavedAlias {
        file: String,
        destination: String,
    },
    Publish {
        file: String,
        bytes: Vec<u8>,
    },
    SaveMetadata {
        effect: SaveEffect,
    },
    Warning {
        message: String,
    },
    HttpHandler {
        handle: HandlerToken,
        name: String,
        details: save::HttpErrorDetails,
    },
    QueueHandler {
        handle: HandlerToken,
        name: String,
        diagnostic: queue::QueueDiagnostic,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Status {
    Initializing,
    Ready,
    Skipped,
    FallbackRequired,
    AwaitingHandler {
        handle: HandlerToken,
    },
    Finished,
    Failed {
        message: String,
    },
    /// Rejected caller operation; any pending continuation remains intact.
    Rejected {
        message: String,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyResult {
    pub status: Status,
    pub effects: Vec<Effect>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Head,
    Main,
    Dependencies,
    DependencyBody,
    Crawl,
    Complete,
}
struct Active {
    request: VisitRequest,
    phase: Phase,
    main: Option<Response>,
    dependency: Option<(String, String, String)>,
}
enum After {
    Head(u64),
    Main(u64, Response),
    Dependency(u64),
    Crawled(u64),
    Outside(u64),
    Finish,
}
struct Pending {
    handle: HandlerToken,
}
enum Work {
    Dispatch,
    Visit(VisitRequest),
    Save(SaveEffect),
    Queue(queue::QueueEffect),
    After(After),
    Wait(HandlerToken),
    Fail(String),
    InvalidUrl { href: String, path: String },
    Ids { path: String, ids: Vec<String> },
    Link { href: String, path: String },
}
pub struct PrerenderPolicy {
    config: PolicyConfig,
    profile: CrawlProfile,
    queue: QueueState,
    save: SaveState,
    active: BTreeMap<u64, Active>,
    outside_returned: BTreeSet<u64>,
    artifacts: BTreeMap<String, Vec<u8>>,
    pending: Option<Pending>,
    work: VecDeque<Work>,
    next_invalid_handler: u64,
    started: bool,
    initialized: bool,
    skipped: bool,
    failed: bool,
    finished: bool,
    retained: usize,
}
impl PrerenderPolicy {
    pub fn new(
        config: PolicyConfig,
        routes: Vec<RouteMetadata>,
        assets: AssetSnapshot,
        inventory: OutputInventory,
    ) -> Result<Self> {
        if config.save.kit_version != config.queue.kit_version
            || config.save.base != config.queue.base
            || config.save.origin != config.queue.origin
        {
            return Err("Save and queue require identical version/base/origin provenance".into());
        }
        let profile = if KitSyncVersion::parse(&config.queue.kit_version)?.is_kit3() {
            CrawlProfile::Kit3_0_1
        } else {
            CrawlProfile::Kit2_69_2
        };
        if let HandlerPolicy::Custom { name } = &config.invalid_url {
            if name.len() > 1024 * 1024 {
                return Err("Invalid-URL callback identity exceeds limit".into());
            }
        }
        let queue = QueueState::new(config.queue.clone(), routes, assets)?;
        let save = SaveState::new(config.save.clone(), inventory)?;
        Ok(Self {
            config,
            profile,
            queue,
            save,
            active: BTreeMap::new(),
            outside_returned: BTreeSet::new(),
            artifacts: BTreeMap::new(),
            pending: None,
            work: VecDeque::new(),
            next_invalid_handler: 1,
            started: false,
            initialized: false,
            skipped: false,
            failed: false,
            finished: false,
            retained: 0,
        })
    }
    pub fn save_snapshot(&self) -> &save::SaveSnapshot {
        self.save.snapshot()
    }
    pub fn queue_snapshot(&self) -> &queue::QueueSnapshot {
        self.queue.snapshot()
    }
    pub fn artifacts(&self) -> &BTreeMap<String, Vec<u8>> {
        &self.artifacts
    }
    pub fn start(&mut self, has_prerender_remotes: bool) -> PolicyResult {
        self.run(false, |this, effects| {
            if this.started {
                return Err("Prerender policy already started".into());
            }
            this.started = true;
            let result = this.queue.start(has_prerender_remotes)?;
            match result.status {
                queue::StartStatus::Ready => {
                    effects.push(Effect::InitializeRuntime);
                    Ok(Status::Initializing)
                }
                queue::StartStatus::Skipped => {
                    this.skipped = true;
                    Ok(Status::Skipped)
                }
                queue::StartStatus::FallbackRequired => {
                    this.skipped = true;
                    Ok(Status::FallbackRequired)
                }
            }
        })
    }
    /// Call only after the actual runtime init returned successfully.
    pub fn initialized(&mut self) -> PolicyResult {
        self.run(false, |this, effects| {
            if !this.started || this.initialized || this.skipped {
                return Err("Unexpected runtime initialization acknowledgement".into());
            }
            this.initialized = true;
            this.dispatch_ready(effects)?;
            Ok(Status::Ready)
        })
    }
    pub fn enqueue(&mut self, input: EnqueueInput) -> PolicyResult {
        self.run(false, |this, effects| {
            this.ensure_initialized()?;
            this.queue.enqueue(input)?;
            this.dispatch_ready(effects)?;
            Ok(Status::Ready)
        })
    }
    pub fn seed_remote(&mut self, seed: RemoteSeed) -> PolicyResult {
        self.run(false, |this, effects| {
            this.ensure_initialized()?;
            this.queue.seed_remote(seed)?;
            this.dispatch_ready(effects)?;
            Ok(Status::Ready)
        })
    }
    pub fn main_head(&mut self, token: u64, head: &ResponseHead) -> PolicyResult {
        self.run(false, |this, effects| {
            this.phase(token, Phase::Head)?;
            let headers = head.normalized_headers()?;
            let result = this.queue.check_entry_match(
                token,
                headers.get("x-sveltekit-routeid").map(String::as_str),
            )?;
            this.queue_result(result, After::Head(token), effects)
        })
    }
    pub fn save_main(&mut self, token: u64, response: Response) -> PolicyResult {
        self.run(false, |this, effects| {
            this.phase(token, Phase::Main)?;
            let request = &this.active[&token].request;
            let bytes = this
                .retained
                .checked_add(response.body.len())
                .ok_or("Retained main body size overflow")?;
            if bytes > MAX_RETAINED {
                return Err("Retained concurrent main body limit exceeded".into());
            }
            let input = SaveInput {
                decoded: request.decoded.clone(),
                encoded: request.encoded.clone(),
                referrer: request.referrer.clone(),
                reference_type: ReferenceType::Linked,
                category: this.category(&request.decoded, false),
                status: response.status,
                headers: response.normalized_headers()?,
                body: response.body.clone(),
            };
            let result = this.save.save(input)?;
            this.retained = bytes;
            this.save_result(result, After::Main(token, response), effects)
        })
    }
    pub fn dependency_head(&mut self, token: u64, head: &DependencyHead) -> PolicyResult {
        self.run(false, |this, _| {
            this.phase(token, Phase::Dependencies)?;
            let headers = head.response.normalized_headers()?;
            this.queue.record_dependency_headers(
                headers.get("x-sveltekit-prerender").map(String::as_str),
                headers.get("x-sveltekit-routeid").map(String::as_str),
            )?;
            let active = this.active.get_mut(&token).unwrap();
            active.dependency = Some((
                head.path.clone(),
                head.encoded_path.clone(),
                head.decoded_path.clone(),
            ));
            active.phase = Phase::DependencyBody;
            Ok(Status::Ready)
        })
    }
    pub fn save_dependency(&mut self, token: u64, dependency: Dependency) -> PolicyResult {
        self.run(false, |this, effects| {
            this.phase(token, Phase::DependencyBody)?;
            let active = &this.active[&token];
            let expected = active.dependency.as_ref().unwrap();
            if (
                &dependency.path,
                &dependency.encoded_path,
                &dependency.decoded_path,
            ) != (&expected.0, &expected.1, &expected.2)
            {
                return Err("Dependency body identity differs from inspected head".into());
            }
            let input = SaveInput {
                referrer: Some(active.request.decoded.clone()),
                reference_type: ReferenceType::Fetched,
                category: this.category(&dependency.decoded_path, true),
                status: dependency.response.status,
                headers: dependency.response.normalized_headers()?,
                body: dependency.response.body,
                decoded: dependency.decoded_path,
                encoded: dependency.encoded_path,
            };
            let result = this.save.save(input)?;
            this.save_result(result, After::Dependency(token), effects)
        })
    }
    pub fn dependencies_done(&mut self, token: u64) -> PolicyResult {
        self.run(false, |this, _| {
            this.phase(token, Phase::Dependencies)?;
            this.active.get_mut(&token).unwrap().phase = Phase::Crawl;
            Ok(Status::Ready)
        })
    }
    /// Head must be re-snapshotted after the dependency iterator finishes. Body
    /// bytes are retained from read_main, as in Buffer.from(await arrayBuffer()).
    pub fn crawl_main(&mut self, token: u64, current_head: &ResponseHead) -> PolicyResult {
        self.run(false, |this, _effects| {
            this.phase(token, Phase::Crawl)?;
            let active = &this.active[&token];
            let path = active.request.decoded.clone();
            let headers = current_head.normalized_headers()?;
            let content_type = headers.get("content-type").map(String::as_str);
            if this.config.crawl
                && (200..300).contains(&current_head.status)
                && this.html(content_type)
            {
                // Node Buffer.toString() uses UTF-8 replacement; unlike HTTP
                // ByteStrings this particular conversion is deliberately lossy.
                let body = &active.main.as_ref().ok_or("Main body missing")?.body;
                let result =
                    prerender_crawl::crawl(&String::from_utf8_lossy(body), &path, this.profile)
                        .map_err(|e| e.to_string())?;
                for href in result.invalid {
                    this.work.push_back(Work::InvalidUrl {
                        href,
                        path: path.clone(),
                    });
                }
                this.work.push_back(Work::Ids {
                    path: path.clone(),
                    ids: result.ids,
                });
                for href in result.hrefs {
                    this.work.push_back(Work::Link {
                        href,
                        path: path.clone(),
                    });
                }
            }
            this.work.push_back(Work::After(After::Crawled(token)));
            Ok(Status::Ready)
        })
    }
    pub fn complete_visit(&mut self, token: u64) -> PolicyResult {
        self.run(false, |this, effects| {
            this.phase(token, Phase::Complete)?;
            this.queue.complete_visit(token)?;
            let active = this.active.remove(&token).unwrap();
            this.retained -= active.main.map_or(0, |main| main.body.len());
            this.dispatch_ready(effects)?;
            Ok(Status::Ready)
        })
    }
    /// Acknowledge only from the queue's second promise reaction. Unknown,
    /// duplicate, and not-yet-returned tokens leave all continuations intact.
    pub fn complete_outside_visit(&mut self, token: u64) -> PolicyResult {
        if !self.outside_returned.contains(&token) {
            return PolicyResult {
                status: Status::Rejected {
                    message: "Unknown, unresolved, or already completed outside-base visit token"
                        .into(),
                },
                effects: vec![],
            };
        }
        self.run(false, |this, effects| {
            this.queue.complete_visit(token)?;
            this.outside_returned.remove(&token);
            this.dispatch_ready(effects)?;
            Ok(Status::Ready)
        })
    }
    pub fn finish(&mut self) -> PolicyResult {
        if !self.active.is_empty() || !self.outside_returned.is_empty() {
            return PolicyResult {
                status: Status::Rejected {
                    message: "Cannot finish before all visit queue-release acknowledgments".into(),
                },
                effects: vec![],
            };
        }
        self.run(false, |this, effects| {
            if !this.active.is_empty() {
                return Err("Cannot finish while native visits are active".into());
            }
            let result = this.queue.finish()?;
            this.queue_result(result, After::Finish, effects)
        })
    }
    pub fn resume_handler(&mut self, handle: HandlerToken, callback: Result<()>) -> PolicyResult {
        if self
            .pending
            .as_ref()
            .is_none_or(|pending| pending.handle != handle)
        {
            return PolicyResult {
                status: Status::Rejected {
                    message: "Stale or absent policy handler token/source".into(),
                },
                effects: vec![],
            };
        }
        self.run(true, |this, _| {
            this.pending.take();
            match handle.source {
                HandlerSource::Save => {
                    let result = this.save.resume_http(handle.token, callback)?;
                    this.prepend_save(result, None);
                }
                HandlerSource::Queue => {
                    let result = this.queue.resume_handler(handle.token, callback)?;
                    this.prepend_queue(result, None);
                }
                HandlerSource::InvalidUrl => callback?,
            }
            Ok(Status::Ready)
        })
    }
    /// Report a genuine scheduler/runtime failure without inventing a response.
    pub fn fail(&mut self, message: String) -> PolicyResult {
        self.failed = true;
        PolicyResult {
            status: Status::Failed { message },
            effects: vec![],
        }
    }
    fn run(
        &mut self,
        resuming: bool,
        f: impl FnOnce(&mut Self, &mut Vec<Effect>) -> Result<Status>,
    ) -> PolicyResult {
        let mut effects = vec![];
        if self.pending.is_some() && !resuming {
            return PolicyResult {
                status: Status::Rejected {
                    message: "Resume the synchronous policy handler before proceeding".into(),
                },
                effects,
            };
        }
        let result = if self.failed || self.finished {
            Err("Prerender policy already finished or failed".into())
        } else if self.pending.is_some() && !resuming {
            Err("Resume the synchronous policy handler before proceeding".into())
        } else {
            f(self, &mut effects)
        };
        let result = result.and_then(|status| {
            if matches!(status, Status::Ready) {
                self.drain(&mut effects)
            } else {
                Ok(status)
            }
        });
        let status = match result {
            Ok(status) => status,
            Err(message) => {
                self.failed = true;
                Status::Failed { message }
            }
        };
        if matches!(status, Status::Failed { .. }) {
            self.failed = true;
        }
        PolicyResult { status, effects }
    }
    fn ensure_initialized(&self) -> Result<()> {
        if !self.initialized {
            Err("Runtime initialization has not been acknowledged".into())
        } else {
            Ok(())
        }
    }
    fn phase(&self, token: u64, expected: Phase) -> Result<()> {
        self.ensure_initialized()?;
        let active = self
            .active
            .get(&token)
            .ok_or("Unknown policy visit token")?;
        if active.phase != expected {
            return Err(format!(
                "Visit {token} is {:?}, expected {expected:?}",
                active.phase
            ));
        }
        Ok(())
    }
    fn dispatch_ready(&mut self, _effects: &mut Vec<Effect>) -> Result<()> {
        self.work.push_front(Work::Dispatch);
        Ok(())
    }
    fn category(&self, decoded: &str, dependency: bool) -> Category {
        let prefix = format!(
            "{}/{}/remote/",
            self.config.queue.base, self.config.queue.app_dir
        );
        if decoded.starts_with(&prefix) {
            Category::Data
        } else if dependency {
            Category::Dependencies
        } else {
            Category::Pages
        }
    }
    fn after(&mut self, after: After) -> Result<Status> {
        match after {
            After::Head(token) => {
                self.active
                    .get_mut(&token)
                    .ok_or("Missing main head visit")?
                    .phase = Phase::Main
            }
            After::Main(token, response) => {
                let active = self
                    .active
                    .get_mut(&token)
                    .ok_or("Missing main response visit")?;
                active.main = Some(response);
                active.phase = Phase::Dependencies;
            }
            After::Dependency(token) => {
                let active = self
                    .active
                    .get_mut(&token)
                    .ok_or("Missing dependency visit")?;
                active.dependency = None;
                active.phase = Phase::Dependencies;
            }
            After::Crawled(token) => {
                self.active
                    .get_mut(&token)
                    .ok_or("Missing crawled visit")?
                    .phase = Phase::Complete;
            }
            After::Outside(token) => {
                if !self.outside_returned.insert(token) {
                    return Err("Outside-base visit returned more than once".into());
                }
            }
            After::Finish => {
                self.finished = true;
                return Ok(Status::Finished);
            }
        }
        Ok(Status::Ready)
    }
    fn save_result(
        &mut self,
        result: save::SaveResult,
        after: After,
        _effects: &mut Vec<Effect>,
    ) -> Result<Status> {
        self.prepend_save(result, Some(after));
        Ok(Status::Ready)
    }
    fn queue_result(
        &mut self,
        result: queue::PolicyResult,
        after: After,
        _effects: &mut Vec<Effect>,
    ) -> Result<Status> {
        self.prepend_queue(result, Some(after));
        Ok(Status::Ready)
    }
    fn prepend_save(&mut self, result: save::SaveResult, after: Option<After>) {
        if !matches!(result.status, save::SaveStatus::Failed { .. }) {
            if let Some(after) = after {
                self.work.push_front(Work::After(after));
            }
        }
        match result.status {
            save::SaveStatus::AwaitingHttpHandler { token } => {
                self.work.push_front(Work::Wait(HandlerToken {
                    source: HandlerSource::Save,
                    token,
                }))
            }
            save::SaveStatus::Failed { message } => self.work.push_front(Work::Fail(message)),
            _ => {}
        }
        for effect in result.effects.into_iter().rev() {
            self.work.push_front(Work::Save(effect));
        }
    }
    fn prepend_queue(&mut self, result: queue::PolicyResult, after: Option<After>) {
        if !matches!(result.status, queue::PolicyStatus::Failed { .. }) {
            if let Some(after) = after {
                self.work.push_front(Work::After(after));
            }
        }
        match result.status {
            queue::PolicyStatus::AwaitingHandler { token } => {
                self.work.push_front(Work::Wait(HandlerToken {
                    source: HandlerSource::Queue,
                    token,
                }))
            }
            queue::PolicyStatus::Failed { message } => self.work.push_front(Work::Fail(message)),
            _ => {}
        }
        for effect in result.effects.into_iter().rev() {
            self.work.push_front(Work::Queue(effect));
        }
    }
    fn drain(&mut self, effects: &mut Vec<Effect>) -> Result<Status> {
        while let Some(work) = self.work.pop_front() {
            if self.work.len() > 300_000 {
                return Err("Prerender continuation work limit exceeded".into());
            }
            match work {
                Work::Dispatch => {
                    for request in self.queue.take_ready()?.into_iter().rev() {
                        self.work.push_front(Work::Visit(request));
                    }
                }
                Work::Visit(request) => {
                    if request.within_base {
                        self.active.insert(
                            request.token,
                            Active {
                                request: request.clone(),
                                phase: Phase::Head,
                                main: None,
                                dependency: None,
                            },
                        );
                        effects.push(Effect::StartVisit { request });
                    } else {
                        let result = self.save.handle_http_error(save::HttpErrorInput {
                            status: 404,
                            path: request.decoded,
                            referrer: request.referrer,
                            reference_type: ReferenceType::Linked,
                        })?;
                        self.prepend_save(result, Some(After::Outside(request.token)));
                    }
                }
                Work::Save(effect) => match effect {
                    SaveEffect::RouteSeen { id } => {
                        self.queue.record_route(&id)?;
                        effects.push(Effect::SaveMetadata {
                            effect: SaveEffect::RouteSeen { id },
                        });
                    }
                    SaveEffect::Enqueue {
                        referrer,
                        decoded,
                        encoded,
                    } => {
                        self.queue.enqueue(EnqueueInput {
                            decoded,
                            encoded: Some(encoded),
                            referrer: Some(referrer),
                            generated_from_id: None,
                        })?;
                        self.work.push_front(Work::Dispatch);
                    }
                    SaveEffect::Write { path, body } => {
                        self.artifacts.insert(path.clone(), body.clone());
                        effects.push(Effect::Write { path, bytes: body });
                    }
                    SaveEffect::ManifestAsset { file } => {
                        effects.push(Effect::ManifestAsset { file })
                    }
                    SaveEffect::Saved { file, destination } => {
                        effects.push(Effect::SavedAlias {
                            file: file.clone(),
                            destination: destination.clone(),
                        });
                        if let Some(bytes) = self.artifacts.get(&destination) {
                            effects.push(Effect::Publish {
                                file,
                                bytes: bytes.clone(),
                            });
                        }
                    }
                    SaveEffect::Warning { message } => effects.push(Effect::Warning { message }),
                    SaveEffect::HttpHandler {
                        token,
                        name,
                        details,
                    } => effects.push(Effect::HttpHandler {
                        handle: HandlerToken {
                            source: HandlerSource::Save,
                            token,
                        },
                        name,
                        details,
                    }),
                    effect => effects.push(Effect::SaveMetadata { effect }),
                },
                Work::Queue(effect) => match effect {
                    queue::QueueEffect::Warning { diagnostic } => effects.push(Effect::Warning {
                        message: diagnostic_message(&diagnostic).into(),
                    }),
                    queue::QueueEffect::Handler {
                        token,
                        name,
                        diagnostic,
                    } => effects.push(Effect::QueueHandler {
                        handle: HandlerToken {
                            source: HandlerSource::Queue,
                            token,
                        },
                        name,
                        diagnostic,
                    }),
                },
                Work::After(after) => {
                    let outside = if let After::Outside(token) = &after {
                        Some(*token)
                    } else {
                        None
                    };
                    let status = self.after(after)?;
                    if let Some(token) = outside {
                        effects.push(Effect::OutsideBaseReturned { visit_token: token });
                    }
                    if status == Status::Finished {
                        return Ok(status);
                    }
                }
                Work::Wait(handle) => {
                    self.pending = Some(Pending { handle });
                    return Ok(Status::AwaitingHandler { handle });
                }
                Work::Fail(message) => return Ok(Status::Failed { message }),
                Work::InvalidUrl { href, path } => {
                    let status = self.invalid_url(&href, &path, effects)?;
                    if let Some(status) = status {
                        return Ok(status);
                    }
                }
                Work::Ids { path, ids } => self.queue.record_crawled_ids(&path, ids)?,
                Work::Link { href, path } => self.link(&href, &path, effects)?,
            }
        }
        Ok(Status::Ready)
    }
    fn html(&self, content_type: Option<&str>) -> bool {
        content_type.is_some_and(|value| match self.profile {
            CrawlProfile::Kit2_69_2 => value == "text/html",
            CrawlProfile::Kit3_0_1 => value
                .split(';')
                .next()
                .unwrap_or("")
                .trim_matches(js_whitespace)
                .eq_ignore_ascii_case("text/html"),
        })
    }
    fn link(&mut self, href: &str, referrer: &str, effects: &mut Vec<Effect>) -> Result<()> {
        let origin = &self.config.queue.origin;
        let href = if href == origin {
            "/"
        } else if href
            .strip_prefix(origin)
            .is_some_and(|rest| rest.starts_with('/'))
        {
            &href[origin.len()..]
        } else {
            href
        };
        if !href.starts_with('/') || href.starts_with("//") {
            return Ok(());
        }
        let base = url::Url::parse("http://localhost").map_err(|e| e.to_string())?;
        let url = base
            .join(href)
            .map_err(|e| format!("Invalid crawled root-relative URL: {e}"))?;
        if let Some(fragment) = url.fragment().filter(|s| !s.is_empty()) {
            let key = prerender_crawl::decode_uri(&format!("{}#{fragment}", url.path()))
                .map_err(|e| e.to_string())?;
            self.queue.expect_hashlink(&key, referrer)?;
        }
        self.queue.enqueue(EnqueueInput {
            decoded: prerender_crawl::decode_uri(url.path()).map_err(|e| e.to_string())?,
            encoded: Some(url.path().into()),
            referrer: Some(referrer.into()),
            generated_from_id: None,
        })?;
        self.dispatch_ready(effects)
    }
    fn invalid_url(
        &mut self,
        href: &str,
        referrer: &str,
        effects: &mut Vec<Effect>,
    ) -> Result<Option<Status>> {
        let message = match self.profile {
            CrawlProfile::Kit2_69_2 => format!("Invalid URL {href} (linked from {referrer})"),
            CrawlProfile::Kit3_0_1 => format!(
                "prerender_invalid_url\nInvalid URL `{href}` (linked from `{referrer}`)\nhttps://svelte.dev/e/kit/prerender_invalid_url"
            ),
        };
        match &self.config.invalid_url {
            HandlerPolicy::Ignore => Ok(None),
            HandlerPolicy::Warn => {
                effects.push(Effect::Warning { message });
                Ok(None)
            }
            HandlerPolicy::Fail => Err(message),
            HandlerPolicy::Default => Err(format!(
                "{message}\nTo suppress or handle this error, implement `handleInvalidUrl` in https://svelte.dev/docs/kit/configuration#prerender"
            )),
            HandlerPolicy::Custom { name } => {
                let handle = HandlerToken {
                    source: HandlerSource::InvalidUrl,
                    token: self.next_invalid_handler,
                };
                self.next_invalid_handler = self
                    .next_invalid_handler
                    .checked_add(1)
                    .ok_or("Invalid URL callback token overflow")?;
                effects.push(Effect::InvalidUrlHandler {
                    handle,
                    name: name.clone(),
                    href: href.into(),
                    referrer: referrer.into(),
                    message,
                });
                self.pending = Some(Pending { handle });
                Ok(Some(Status::AwaitingHandler { handle }))
            }
        }
    }
}
fn diagnostic_message(diagnostic: &queue::QueueDiagnostic) -> &str {
    match diagnostic {
        queue::QueueDiagnostic::MissingId { message, .. }
        | queue::QueueDiagnostic::EntryGeneratorMismatch { message, .. }
        | queue::QueueDiagnostic::UnseenRoutes { message, .. } => message,
    }
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

#[cfg(test)]
#[path = "prerender_policy_tests.rs"]
mod tests;
