//! Explicit, thread-confined ECMAScript module host. See crate README for the
//! capability and budget contract. No filesystem/network/process environment.
#[cfg(test)]
mod export_eof_tests;
#[cfg(test)]
mod json_intrinsic_tests;
pub mod native_session;
pub mod node_async_hooks;
pub mod node_module;
pub mod protocol;
pub mod web_abort;
pub mod web_blob;
pub mod web_crypto;
pub mod web_fetch;
pub mod web_fetch_body;
pub mod web_form_data;
pub mod web_headers;
pub mod web_streams;
pub mod web_text;
pub mod web_transform;
pub mod web_url;
pub mod web_writable;

#[cfg(target_arch = "wasm32")]
thread_local! {
    static SERVICE: std::cell::RefCell<protocol::Service> = std::cell::RefCell::new(protocol::Service::default());
}
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn js_runtime_json(request: &str) -> String {
    SERVICE.with(|service| service.borrow_mut().request_json(request))
}
use boa_engine::builtins::promise::PromiseState;
use boa_engine::job::{Job, JobExecutor};
use boa_engine::module::{ModuleLoader, ModuleRequest, Referrer};
use boa_engine::object::builtins::JsPromise;
use boa_engine::property::PropertyKey;
use boa_engine::{
    Context, JsError, JsNativeError, JsObject, JsResult, JsString, JsValue, Module, Source,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::rc::Rc;
use std::task::{Context as TaskContext, Poll, Waker};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModuleSource {
    pub source: String,
    pub import_meta_url: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum Resolution {
    Module { id: String },
    Builtin { name: String },
    External { name: String },
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HostSnapshot {
    pub modules: BTreeMap<String, ModuleSource>,
    /// Exact importer ID -> exact JS specifier -> explicit target.
    pub resolutions: BTreeMap<String, BTreeMap<String, Resolution>>,
}
/// Explicit ambient APIs. None grant filesystem, environment or network access.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum HostCapability {
    Url,
    Text,
    Headers,
    Blob,
    FormData,
    Abort,
    Streams,
    RequestResponse,
    NodeAsyncHooks,
    /// `node:module` with a `createRequire` whose `require` always throws.
    NodeModule,
    Crypto,
}
/// Native-only environment providers. No entropy is invented by the runtime.
#[derive(Default)]
pub struct HostOptions {
    pub fetch: web_fetch::FetchOptions,
    pub crypto: web_crypto::CryptoOptions,
    pub entropy: Option<std::sync::Arc<dyn web_crypto::EntropySource>>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuntimeLimits {
    pub max_modules: usize,
    pub max_source_bytes: usize,
    pub max_total_source_bytes: usize,
    pub instructions: usize,
    pub loop_iterations: u64,
    pub recursion: usize,
    pub stack: usize,
    pub jobs: usize,
    pub queued_jobs: usize,
    pub json_bytes: usize,
    pub json_nodes: usize,
    pub json_depth: usize,
}
impl Default for RuntimeLimits {
    fn default() -> Self {
        Self {
            max_modules: 4096,
            max_source_bytes: 4 * 1024 * 1024,
            max_total_source_bytes: 64 * 1024 * 1024,
            instructions: 10_000_000,
            loop_iterations: 1_000_000,
            recursion: 128,
            stack: 16384,
            jobs: 100_000,
            queued_jobs: 16_384,
            json_bytes: 4 * 1024 * 1024,
            json_nodes: 100_000,
            json_depth: 64,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "camelCase")]
pub enum RuntimeValue {
    Absent,
    Undefined,
    Json(Value),
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeError {
    pub code: String,
    pub message: String,
}
impl std::fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for RuntimeError {}
type Result<T> = std::result::Result<T, RuntimeError>;
fn error(code: &str, message: impl Into<String>) -> RuntimeError {
    RuntimeError {
        code: code.into(),
        message: message.into(),
    }
}
fn js_error(message: impl Into<String>) -> JsError {
    JsNativeError::typ().with_message(message.into()).into()
}
fn engine(error: JsError) -> RuntimeError {
    RuntimeError {
        code: "javascript_error".into(),
        message: error.to_string(),
    }
}

struct SnapshotLoader {
    snapshot: HostSnapshot,
    builtins: RefCell<BTreeMap<String, Module>>,
    modules: RefCell<BTreeMap<String, Module>>,
    paths: BTreeMap<String, PathBuf>,
    ids: BTreeMap<PathBuf, String>,
    failure: RefCell<Option<String>>,
}
impl SnapshotLoader {
    fn get(&self, id: &str, context: &mut Context) -> JsResult<Module> {
        if let Some(module) = self.modules.borrow().get(id) {
            return Ok(module.clone());
        }
        let source = self
            .snapshot
            .modules
            .get(id)
            .ok_or_else(|| js_error(format!("Missing snapshot module {id}")))?;
        let module = Module::parse(
            Source::from_bytes(&source.source).with_path(&self.paths[id]),
            None,
            context,
        )?;
        self.modules.borrow_mut().insert(id.into(), module.clone());
        Ok(module)
    }
    fn fail<T>(&self, message: String) -> JsResult<T> {
        self.failure.borrow_mut().get_or_insert(message.clone());
        Err(js_error(message))
    }
}
impl ModuleLoader for SnapshotLoader {
    async fn load_imported_module(
        self: Rc<Self>,
        referrer: Referrer,
        request: ModuleRequest,
        context: &RefCell<&mut Context>,
    ) -> JsResult<Module> {
        if !request.attributes().is_empty() {
            return self
                .fail("Import attributes are unsupported by the source-only snapshot host".into());
        }
        let Some(id) = referrer.path().and_then(|path| self.ids.get(path)) else {
            return self.fail("Import referrer is not a snapshot module".into());
        };
        let specifier = request
            .specifier()
            .to_std_string()
            .map_err(|_| js_error("Invalid UTF-16 module specifier"))?;
        let Some(resolution) = self
            .snapshot
            .resolutions
            .get(id)
            .and_then(|map| map.get(&specifier))
        else {
            return self.fail(format!("Missing exact resolution {id} -> {specifier}"));
        };
        match resolution {
            Resolution::Module { id } => self.get(id, &mut context.borrow_mut()),
            Resolution::Builtin { name } => self
                .builtins
                .borrow()
                .get(name)
                .cloned()
                .ok_or_else(|| js_error(format!("Native builtin {name} is not registered"))),
            Resolution::External { name } => self.fail(format!(
                "Unsupported external module {name} imported by {id}"
            )),
        }
    }
    fn init_import_meta(self: Rc<Self>, meta: &JsObject, module: &Module, context: &mut Context) {
        let Some(id) = module.path().and_then(|path| self.ids.get(path)) else {
            self.failure
                .borrow_mut()
                .replace("Unknown import.meta module identity".into());
            return;
        };
        let value = JsString::from(self.snapshot.modules[id].import_meta_url.as_str());
        if let Err(error) =
            meta.create_data_property_or_throw(JsString::from("url"), value, context)
        {
            self.failure.borrow_mut().replace(error.to_string());
        }
    }
}

struct BoundedJobs {
    queue: RefCell<VecDeque<Job>>,
    remaining: Cell<usize>,
    limit: usize,
    failure: RefCell<Option<String>>,
}
impl BoundedJobs {
    fn charge(&self) -> JsResult<()> {
        let remaining = self.remaining.get();
        if remaining == 0 {
            return Err(js_error("Job/poll budget exhausted"));
        }
        self.remaining.set(remaining - 1);
        Ok(())
    }
    fn check(&self) -> JsResult<()> {
        if let Some(error) = self.failure.borrow().as_ref() {
            return Err(js_error(error.clone()));
        }
        Ok(())
    }
}
impl JobExecutor for BoundedJobs {
    fn enqueue_job(self: Rc<Self>, job: Job, _: &mut Context) {
        if self.queue.borrow().len() >= self.limit {
            self.failure
                .borrow_mut()
                .get_or_insert("Queued job limit exceeded".into());
            return;
        }
        self.queue.borrow_mut().push_back(job);
    }
    fn run_jobs(self: Rc<Self>, context: &mut Context) -> JsResult<()> {
        let context = RefCell::new(context);
        let mut futures: Vec<Pin<Box<dyn Future<Output = JsResult<JsValue>> + '_>>> = vec![];
        let mut task = TaskContext::from_waker(Waker::noop());
        loop {
            self.check()?;
            loop {
                let job = { self.queue.borrow_mut().pop_front() };
                let Some(job) = job else { break };
                self.charge()?;
                match job {
                    Job::PromiseJob(job) => {
                        job.call(&mut context.borrow_mut())?;
                    }
                    Job::GenericJob(job) => {
                        job.call(&mut context.borrow_mut())?;
                    }
                    Job::AsyncJob(job) => futures.push(Box::pin(job.call(&context))),
                    // Optional GC cleanup callbacks are not host capabilities.
                    Job::FinalizationRegistryCleanupJob(_) => {}
                    _ => {
                        return Err(js_error(
                            "Timers/intervals and unknown native jobs are unsupported",
                        ));
                    }
                }
                if futures.len() > self.limit {
                    return Err(js_error("Pending async job limit exceeded"));
                }
            }
            let mut progress = false;
            let mut index = 0;
            while index < futures.len() {
                self.charge()?;
                match futures[index].as_mut().poll(&mut task) {
                    Poll::Ready(result) => {
                        result?;
                        drop(futures.remove(index));
                        progress = true;
                    }
                    Poll::Pending => index += 1,
                }
            }
            self.check()?;
            if self.queue.borrow().is_empty() {
                if futures.is_empty() {
                    return Ok(());
                }
                if !progress {
                    return Err(js_error(
                        "Pending native async operation requires an unavailable host capability",
                    ));
                }
            }
        }
    }
}

/// Intentionally !Send/!Sync: one runtime and its context stay on one thread.
pub struct Runtime {
    context: Context,
    loader: Rc<SnapshotLoader>,
    jobs: Rc<BoundedJobs>,
    limits: RuntimeLimits,
    evaluated: BTreeSet<String>,
    poisoned: bool,
}
impl Runtime {
    pub fn new(snapshot: HostSnapshot, limits: RuntimeLimits) -> Result<Self> {
        Self::with_capabilities(snapshot, limits, &BTreeSet::new())
    }
    pub fn with_capabilities(
        snapshot: HostSnapshot,
        limits: RuntimeLimits,
        capabilities: &BTreeSet<HostCapability>,
    ) -> Result<Self> {
        Self::with_fetch_options(
            snapshot,
            limits,
            capabilities,
            web_fetch::FetchOptions::default(),
        )
    }
    pub fn with_fetch_options(
        snapshot: HostSnapshot,
        limits: RuntimeLimits,
        capabilities: &BTreeSet<HostCapability>,
        fetch_options: web_fetch::FetchOptions,
    ) -> Result<Self> {
        Self::with_host_options(
            snapshot,
            limits,
            capabilities,
            HostOptions {
                fetch: fetch_options,
                ..Default::default()
            },
        )
    }
    pub fn with_host_options(
        snapshot: HostSnapshot,
        limits: RuntimeLimits,
        capabilities: &BTreeSet<HostCapability>,
        host_options: HostOptions,
    ) -> Result<Self> {
        if [
            limits.max_modules,
            limits.max_source_bytes,
            limits.max_total_source_bytes,
            limits.instructions,
            limits.recursion,
            limits.stack,
            limits.jobs,
            limits.queued_jobs,
            limits.json_bytes,
            limits.json_nodes,
            limits.json_depth,
        ]
        .contains(&0)
            || limits.loop_iterations == 0
            || limits.loop_iterations == u64::MAX
        {
            return Err(error(
                "invalid_limits",
                "Every execution limit must be positive and finite",
            ));
        }
        if snapshot.modules.len() > limits.max_modules {
            return Err(error(
                "source_limit",
                "Module count exceeds the snapshot limit",
            ));
        }
        let mut total = 0usize;
        let mut paths = BTreeMap::new();
        let mut ids = BTreeMap::new();
        for (index, (id, module)) in snapshot.modules.iter().enumerate() {
            if id.is_empty()
                || id.contains('\0')
                || module.import_meta_url.is_empty()
                || module.import_meta_url.contains('\0')
            {
                return Err(error(
                    "invalid_snapshot",
                    "Module IDs and import.meta URLs must be explicit nonempty strings without NUL",
                ));
            }
            total = total
                .checked_add(module.source.len())
                .ok_or_else(|| error("source_limit", "Source size overflow"))?;
            if module.source.len() > limits.max_source_bytes
                || total > limits.max_total_source_bytes
            {
                return Err(error("source_limit", "Snapshot source byte limit exceeded"));
            }
            let path = PathBuf::from(format!("/__snapshot__/{index}.mjs"));
            paths.insert(id.clone(), path.clone());
            ids.insert(path, id.clone());
        }
        for (importer, resolutions) in &snapshot.resolutions {
            if !snapshot.modules.contains_key(importer) {
                return Err(error(
                    "invalid_snapshot",
                    format!("Resolution importer {importer} is absent"),
                ));
            }
            for (specifier, target) in resolutions {
                if specifier.is_empty() || specifier.contains('\0') {
                    return Err(error("invalid_snapshot", "Empty or NUL import specifier"));
                }
                if let Resolution::Module { id } = target
                    && !snapshot.modules.contains_key(id)
                {
                    return Err(error(
                        "invalid_snapshot",
                        format!("Resolution target {id} is absent"),
                    ));
                }
                if let Resolution::Builtin { name } = target
                    && !match name.as_str() {
                        "node:async_hooks" | "async_hooks" => {
                            capabilities.contains(&HostCapability::NodeAsyncHooks)
                        }
                        "node:module" | "module" => {
                            capabilities.contains(&HostCapability::NodeModule)
                        }
                        _ => false,
                    }
                {
                    return Err(error(
                        "invalid_snapshot",
                        format!(
                            "Native builtin {name} requires an implemented explicit capability"
                        ),
                    ));
                }
            }
        }
        let loader = Rc::new(SnapshotLoader {
            snapshot,
            builtins: RefCell::new(BTreeMap::new()),
            modules: RefCell::new(BTreeMap::new()),
            paths,
            ids,
            failure: RefCell::new(None),
        });
        let jobs = Rc::new(BoundedJobs {
            queue: RefCell::new(VecDeque::new()),
            remaining: Cell::new(limits.jobs),
            limit: limits.queued_jobs,
            failure: RefCell::new(None),
        });
        let async_hooks = Rc::new(node_async_hooks::AsyncContextHooks);
        let mut context = Context::builder()
            .can_block(false)
            .module_loader(loader.clone())
            .job_executor(jobs.clone())
            .host_hooks(async_hooks.clone())
            .instructions_remaining(limits.instructions)
            .build()
            .map_err(engine)?;
        let engine_limits = context.runtime_limits_mut();
        engine_limits.set_loop_iteration_limit(limits.loop_iterations);
        engine_limits.set_recursion_limit(limits.recursion);
        engine_limits.set_stack_size_limit(limits.stack);
        engine_limits.set_backtrace_limit(20);
        let mut capabilities = capabilities.clone();
        if capabilities.contains(&HostCapability::RequestResponse) {
            capabilities.extend([
                HostCapability::Url,
                HostCapability::Text,
                HostCapability::Headers,
                HostCapability::FormData,
                HostCapability::Abort,
            ]);
        }
        if capabilities.contains(&HostCapability::Abort) {
            capabilities.insert(HostCapability::Text);
        }
        if capabilities.contains(&HostCapability::FormData) {
            capabilities.insert(HostCapability::Blob);
        }
        if capabilities.contains(&HostCapability::Blob) {
            capabilities.insert(HostCapability::Streams);
        }
        if capabilities.contains(&HostCapability::Streams) {
            capabilities.extend([HostCapability::Abort, HostCapability::Text]);
        }
        if capabilities.contains(&HostCapability::Crypto) {
            capabilities.insert(HostCapability::Text);
        }
        for capability in &capabilities {
            match capability {
                HostCapability::Url => web_url::register(&mut context).map_err(engine)?,
                HostCapability::Text => web_text::register(&mut context).map_err(engine)?,
                HostCapability::Headers => web_headers::register(&mut context).map_err(engine)?,
                HostCapability::Blob => web_blob::register(&mut context).map_err(engine)?,
                HostCapability::FormData => {
                    web_form_data::register(&mut context).map_err(engine)?
                }
                HostCapability::Abort => web_abort::register(&mut context).map_err(engine)?,
                HostCapability::Streams => {
                    web_streams::register(&mut context).map_err(engine)?;
                    web_writable::register(&mut context).map_err(engine)?;
                    web_transform::register(&mut context).map_err(engine)?;
                }
                HostCapability::RequestResponse => {
                    web_fetch::register(&mut context, host_options.fetch.clone()).map_err(engine)?
                }
                HostCapability::NodeAsyncHooks => {
                    let module = node_async_hooks::register(&mut context, async_hooks.clone())
                        .map_err(engine)?;
                    let mut builtins = loader.builtins.borrow_mut();
                    builtins.insert("node:async_hooks".into(), module.clone());
                    builtins.insert("async_hooks".into(), module);
                }
                HostCapability::NodeModule => {
                    let module = node_module::register(&mut context).map_err(engine)?;
                    let mut builtins = loader.builtins.borrow_mut();
                    builtins.insert("node:module".into(), module.clone());
                    builtins.insert("module".into(), module);
                }
                HostCapability::Crypto => web_crypto::register(
                    &mut context,
                    &host_options.crypto,
                    host_options.entropy.clone(),
                )
                .map_err(engine)?,
            }
        }
        Ok(Self {
            context,
            loader,
            jobs,
            limits,
            evaluated: BTreeSet::new(),
            poisoned: false,
        })
    }
    fn check(&self) -> Result<()> {
        if self.poisoned {
            return Err(error(
                "runtime_failed",
                "The runtime is poisoned by an earlier execution failure",
            ));
        }
        Ok(())
    }
    /// DOM listener exceptions are reported by the host without changing
    /// dispatchEvent's specified return value or poisoning a successful call.
    pub fn take_reported_errors(&mut self) -> Vec<RuntimeError> {
        web_abort::take_reported_errors(&mut self.context)
            .into_iter()
            .map(|value| {
                let cause = JsError::from_opaque(value);
                let message = cause
                    .try_native(&mut self.context)
                    .map(|native| native.to_string())
                    .unwrap_or_else(|_| cause.to_string());
                error(
                    "event_listener_exception",
                    message.chars().take(4096).collect::<String>(),
                )
            })
            .collect()
    }
    fn finish<T>(&mut self, result: Result<T>) -> Result<T> {
        if result.is_err() {
            self.poisoned = true;
            self.jobs.queue.borrow_mut().clear();
        }
        result
    }
    fn drain(&mut self) -> Result<()> {
        self.context.run_jobs().map_err(engine)?;
        if let Some(failure) = self.loader.failure.borrow().as_ref() {
            return Err(error("module_resolution", failure.clone()));
        }
        Ok(())
    }
    fn await_value(&mut self, value: JsValue) -> Result<JsValue> {
        let promise = JsPromise::resolve(value, &mut self.context).map_err(engine)?;
        self.drain()?;
        match promise.state() {
            PromiseState::Fulfilled(value) => Ok(value),
            PromiseState::Rejected(value) => {
                let cause = JsError::from_opaque(value);
                let message = cause
                    .try_native(&mut self.context)
                    .map(|native| format!("{native}\n{cause}"))
                    .unwrap_or_else(|_| cause.to_string());
                Err(error("promise_rejected", message))
            }
            PromiseState::Pending => Err(error(
                "pending_promise",
                "Promise did not settle using available snapshot jobs",
            )),
        }
    }
    fn evaluate_inner(&mut self, id: &str) -> Result<Module> {
        let module = self.loader.get(id, &mut self.context).map_err(engine)?;
        if !self.evaluated.contains(id) {
            let promise = module.load_link_evaluate(&mut self.context);
            self.await_value(promise.into())?;
            self.evaluated.insert(id.into());
        }
        Ok(module)
    }
    pub fn evaluate(&mut self, id: &str) -> Result<RuntimeValue> {
        self.check()?;
        let result = self.evaluate_inner(id).map(|_| RuntimeValue::Undefined);
        self.finish(result)
    }
    pub fn read_export(&mut self, id: &str, name: &str) -> Result<RuntimeValue> {
        self.check()?;
        let result = (|| {
            let module = self.evaluate_inner(id)?;
            let ns = module.namespace(&mut self.context);
            let key = JsString::from(name);
            if !ns
                .has_own_property(key.clone(), &mut self.context)
                .map_err(engine)?
            {
                return Ok(RuntimeValue::Absent);
            }
            let value = ns.get(key, &mut self.context).map_err(engine)?;
            let value = self.await_value(value)?;
            self.output(value)
        })();
        self.finish(result)
    }
    pub fn invoke_export(
        &mut self,
        id: &str,
        name: &str,
        arguments: Vec<Value>,
    ) -> Result<RuntimeValue> {
        self.check()?;
        let result = (|| {
            validate_json_roots(&arguments, &self.limits)?;
            let module = self.evaluate_inner(id)?;
            let ns = module.namespace(&mut self.context);
            let key = JsString::from(name);
            if !ns
                .has_own_property(key.clone(), &mut self.context)
                .map_err(engine)?
            {
                return Ok(RuntimeValue::Absent);
            }
            let value = ns.get(key, &mut self.context).map_err(engine)?;
            let callable = value
                .as_callable()
                .ok_or_else(|| error("not_callable", format!("Export {name} is not callable")))?;
            let args = arguments
                .iter()
                .map(|arg| JsValue::from_json(arg, &mut self.context).map_err(engine))
                .collect::<Result<Vec<_>>>()?;
            let value = callable
                .call(&JsValue::undefined(), &args, &mut self.context)
                .map_err(engine)?;
            let value = self.await_value(value)?;
            self.output(value)
        })();
        self.finish(result)
    }
    fn output(&mut self, value: JsValue) -> Result<RuntimeValue> {
        if value.is_undefined() {
            return Ok(RuntimeValue::Undefined);
        }
        let mut remaining = self.limits.json_nodes;
        let mut remaining_bytes = self.limits.json_bytes;
        let mut seen = vec![];
        let output = to_json(
            &value,
            &mut self.context,
            &self.limits,
            0,
            &mut remaining,
            &mut remaining_bytes,
            &mut seen,
        )?;
        self.drain()?;
        validate_json(&output, &self.limits)?;
        Ok(RuntimeValue::Json(output))
    }
    pub fn instructions_remaining(&self) -> usize {
        self.context.instructions_remaining()
    }
    pub fn jobs_remaining(&self) -> usize {
        self.jobs.remaining.get()
    }
}

fn validate_json(value: &Value, limits: &RuntimeLimits) -> Result<()> {
    validate_json_roots(std::slice::from_ref(value), limits)
}
fn validate_json_roots(values: &[Value], limits: &RuntimeLimits) -> Result<()> {
    if values.len() > limits.json_nodes {
        return Err(error("json_limit", "Too many JSON arguments"));
    }
    let mut stack = values.iter().map(|value| (value, 0)).collect::<Vec<_>>();
    let mut count = 0;
    while let Some((value, depth)) = stack.pop() {
        count += 1;
        if count > limits.json_nodes || depth > limits.json_depth {
            return Err(error("json_limit", "JSON node/depth limit exceeded"));
        }
        match value {
            Value::Array(values) => {
                if values.len() > limits.json_nodes.saturating_sub(count) {
                    return Err(error("json_limit", "JSON array exceeds node limit"));
                }
                stack.extend(values.iter().map(|v| (v, depth + 1)));
            }
            Value::Object(values) => {
                if values.len() > limits.json_nodes.saturating_sub(count) {
                    return Err(error("json_limit", "JSON object exceeds node limit"));
                }
                stack.extend(values.values().map(|v| (v, depth + 1)));
            }
            _ => {}
        }
    }
    if serde_json::to_vec(values)
        .map_err(|e| error("json_conversion", e.to_string()))?
        .len()
        > limits.json_bytes
    {
        return Err(error("json_limit", "JSON byte limit exceeded"));
    }
    Ok(())
}
fn charge_bytes(remaining: &mut usize, size: usize) -> Result<()> {
    *remaining = remaining
        .checked_sub(size)
        .ok_or_else(|| error("json_limit", "JSON result byte limit exceeded"))?;
    Ok(())
}
fn to_json(
    value: &JsValue,
    context: &mut Context,
    limits: &RuntimeLimits,
    depth: usize,
    remaining: &mut usize,
    remaining_bytes: &mut usize,
    seen: &mut Vec<JsObject>,
) -> Result<Value> {
    if *remaining == 0 || depth > limits.json_depth {
        return Err(error(
            "json_limit",
            "JavaScript result node/depth limit exceeded",
        ));
    }
    *remaining -= 1;
    if value.is_null() {
        charge_bytes(remaining_bytes, 4)?;
        return Ok(Value::Null);
    }
    if let Some(b) = value.as_boolean() {
        charge_bytes(remaining_bytes, if b { 4 } else { 5 })?;
        return Ok(b.into());
    }
    if let Some(n) = value.as_number() {
        // JS has one Number type. Canonicalize exactly representable safe
        // integers for the JSON boundary, including JSON's -0 -> 0 behavior.
        // Outside this range retain the engine's f64 value without an integer
        // cast that could saturate or pretend additional integer precision.
        let number = if n.abs() <= 9_007_199_254_740_991.0 && n.fract() == 0.0 {
            serde_json::Number::from(n as i64)
        } else {
            serde_json::Number::from_f64(n)
                .ok_or_else(|| error("unsupported_value", "Nonfinite numbers are not JSON"))?
        };
        charge_bytes(remaining_bytes, number.to_string().len())?;
        return Ok(Value::Number(number));
    }
    if let Some(s) = value.as_string() {
        if s.len() > *remaining_bytes {
            return Err(error("json_limit", "JSON string exceeds byte limit"));
        }
        let string = s.to_std_string().map_err(|_| {
            error(
                "unsupported_value",
                "Unpaired UTF-16 string is not representable as JSON UTF-8",
            )
        })?;
        charge_bytes(
            remaining_bytes,
            serde_json::to_string(&string)
                .map_err(|e| error("json_conversion", e.to_string()))?
                .len(),
        )?;
        return Ok(Value::String(string));
    }
    let object = value.as_object().ok_or_else(|| {
        error(
            "unsupported_value",
            "Undefined, symbols and BigInt are not nested JSON data",
        )
    })?;
    if !object.is_array() && !object.is_ordinary() {
        return Err(error(
            "unsupported_value",
            "Functions and exotic objects are not JSON data",
        ));
    }
    if seen.contains(&object) {
        return Err(error("unsupported_value", "Cyclic result is not JSON data"));
    }
    seen.push(object.clone());
    charge_bytes(remaining_bytes, 2)?;
    let output = if object.is_array() {
        let length = object
            .get(JsString::from("length"), context)
            .map_err(engine)?
            .as_number()
            .ok_or_else(|| error("unsupported_value", "Invalid array length"))?;
        if length > *remaining as f64 {
            return Err(error(
                "json_limit",
                "Array length exceeds result node limit",
            ));
        }
        let mut values = vec![];
        for index in 0..length as u32 {
            if index > 0 {
                charge_bytes(remaining_bytes, 1)?;
            }
            if !object.has_own_property(index, context).map_err(engine)? {
                return Err(error(
                    "unsupported_value",
                    "Sparse arrays are not strict JSON data",
                ));
            }
            let value = object.get(index, context).map_err(engine)?;
            values.push(to_json(
                &value,
                context,
                limits,
                depth + 1,
                remaining,
                remaining_bytes,
                seen,
            )?);
        }
        Value::Array(values)
    } else {
        let keys = object.own_property_keys(context).map_err(engine)?;
        if keys.len() > *remaining {
            return Err(error(
                "json_limit",
                "Object properties exceed result node limit",
            ));
        }
        let mut values = serde_json::Map::new();
        for key in keys {
            let name = match &key {
                PropertyKey::String(s) => s
                    .to_std_string()
                    .map_err(|_| error("unsupported_value", "Invalid UTF-16 property key"))?,
                PropertyKey::Index(index) => index.get().to_string(),
                PropertyKey::Symbol(_) => {
                    return Err(error(
                        "unsupported_value",
                        "Symbol property keys are not JSON data",
                    ));
                }
            };
            let key_size = serde_json::to_string(&name)
                .map_err(|e| error("json_conversion", e.to_string()))?
                .len();
            charge_bytes(
                remaining_bytes,
                key_size + 1 + usize::from(!values.is_empty()),
            )?;
            let value = object.get(key, context).map_err(engine)?;
            values.insert(
                name,
                to_json(
                    &value,
                    context,
                    limits,
                    depth + 1,
                    remaining,
                    remaining_bytes,
                    seen,
                )?,
            );
        }
        Value::Object(values)
    };
    seen.pop();
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn snapshot(modules: &[(&str, &str)], resolutions: &[(&str, &str, &str)]) -> HostSnapshot {
        let mut result = HostSnapshot::default();
        for (id, source) in modules {
            result.modules.insert(
                (*id).into(),
                ModuleSource {
                    source: (*source).into(),
                    import_meta_url: format!("file:///app/{id}"),
                },
            );
        }
        for (importer, specifier, target) in resolutions {
            result
                .resolutions
                .entry((*importer).into())
                .or_default()
                .insert(
                    (*specifier).into(),
                    Resolution::Module {
                        id: (*target).into(),
                    },
                );
        }
        result
    }
    #[test]
    fn exact_static_dynamic_imports_and_cached_identity() {
        let source = snapshot(
            &[
                (
                    "entry.mjs",
                    "import {next} from './dep';export const url=import.meta.url;export async function run(){const d=await import('alias');return [next(),d.next()]}",
                ),
                ("dep.mjs", "let n=0;export function next(){return ++n}"),
            ],
            &[
                ("entry.mjs", "./dep", "dep.mjs"),
                ("entry.mjs", "alias", "dep.mjs"),
            ],
        );
        let mut runtime = Runtime::new(source, Default::default()).unwrap();
        assert_eq!(
            runtime.read_export("entry.mjs", "url").unwrap(),
            RuntimeValue::Json("file:///app/entry.mjs".into())
        );
        assert_eq!(
            runtime.invoke_export("entry.mjs", "run", vec![]).unwrap(),
            RuntimeValue::Json(serde_json::json!([1, 2]))
        );
        assert_eq!(
            runtime.invoke_export("entry.mjs", "run", vec![]).unwrap(),
            RuntimeValue::Json(serde_json::json!([3, 4]))
        );
    }
    #[test]
    fn cyclic_modules_link_real_live_bindings() {
        let source = snapshot(
            &[
                (
                    "a",
                    "import {b} from 'b';export function a(){return 'a'};export const value=b();",
                ),
                (
                    "b",
                    "import {a} from 'a';export function b(){return a()+'b'}",
                ),
            ],
            &[("a", "b", "b"), ("b", "a", "a")],
        );
        let mut runtime = Runtime::new(source, Default::default()).unwrap();
        assert_eq!(
            runtime.read_export("a", "value").unwrap(),
            RuntimeValue::Json("ab".into())
        );
    }
    #[test]
    fn tla_async_json_arguments_and_absent_undefined_null_are_distinct() {
        let source = snapshot(
            &[(
                "a",
                "export const ready=await Promise.resolve(7);export const absentValue=undefined;export const nil=null;export async function echo(x){await Promise.resolve();return {value:x,ready}}",
            )],
            &[],
        );
        let mut runtime = Runtime::new(source, Default::default()).unwrap();
        assert_eq!(
            runtime.read_export("a", "missing").unwrap(),
            RuntimeValue::Absent
        );
        assert_eq!(
            runtime.read_export("a", "absentValue").unwrap(),
            RuntimeValue::Undefined
        );
        assert_eq!(
            runtime.read_export("a", "nil").unwrap(),
            RuntimeValue::Json(Value::Null)
        );
        assert_eq!(
            runtime
                .invoke_export("a", "echo", vec![serde_json::json!([true, "🙂"])])
                .unwrap(),
            RuntimeValue::Json(serde_json::json!({"value":[true,"🙂"],"ready":7}))
        );
    }
    #[test]
    fn missing_and_external_resolutions_fail_without_implicit_host_access() {
        let mut source = snapshot(&[("a", "import 'node:fs';")], &[]);
        let mut runtime = Runtime::new(source.clone(), Default::default()).unwrap();
        assert!(
            runtime
                .evaluate("a")
                .unwrap_err()
                .message
                .contains("Missing exact resolution")
        );
        source.resolutions.insert(
            "a".into(),
            BTreeMap::from([(
                "node:fs".into(),
                Resolution::External {
                    name: "node:fs".into(),
                },
            )]),
        );
        let mut runtime = Runtime::new(source, Default::default()).unwrap();
        assert!(
            runtime
                .evaluate("a")
                .unwrap_err()
                .message
                .contains("Unsupported external")
        );
    }
    #[test]
    fn errors_pending_promises_and_poisoning_are_explicit() {
        for code in [
            "throw new Error('boom');",
            "await new Promise(()=>{});",
            "export const broken = ;",
        ] {
            let mut runtime =
                Runtime::new(snapshot(&[("a", code)], &[]), Default::default()).unwrap();
            assert!(runtime.evaluate("a").is_err());
            assert_eq!(runtime.evaluate("a").unwrap_err().code, "runtime_failed");
        }
    }
    #[test]
    fn execution_and_shape_limits_terminate_work() {
        for code in [
            "while(true){}",
            "function recurse(){recurse()}recurse();",
            "const again=()=>Promise.resolve().then(again);again();",
        ] {
            let limits = RuntimeLimits {
                instructions: 2000,
                loop_iterations: 100,
                recursion: 16,
                jobs: 50,
                ..Default::default()
            };
            let mut runtime = Runtime::new(snapshot(&[("a", code)], &[]), limits).unwrap();
            assert!(runtime.evaluate("a").is_err());
        }
        let limits = RuntimeLimits {
            max_source_bytes: 2,
            ..RuntimeLimits::default()
        };
        assert!(Runtime::new(snapshot(&[("a", "export {};")], &[]), limits).is_err());
        let mut runtime = Runtime::new(
            snapshot(&[("a", "export const large=Array(1000000000)")], &[]),
            Default::default(),
        )
        .unwrap();
        assert_eq!(
            runtime.read_export("a", "large").unwrap_err().code,
            "json_limit"
        );
    }
    #[test]
    fn numeric_exports_preserve_values_and_canonicalize_safe_integers() {
        let source = snapshot(
            &[(
                "a",
                "export const numbers=[1,7.0,-0,-42,9007199254740991,-9007199254740991,1.25,9007199254740992,-9007199254740992,1e308,5e-324];",
            )],
            &[],
        );
        let mut runtime = Runtime::new(source, Default::default()).unwrap();
        assert_eq!(
            runtime.read_export("a", "numbers").unwrap(),
            RuntimeValue::Json(serde_json::json!([
                1,
                7,
                0,
                -42,
                9_007_199_254_740_991_i64,
                -9_007_199_254_740_991_i64,
                1.25,
                9_007_199_254_740_992.0,
                -9_007_199_254_740_992.0,
                1e308,
                5e-324
            ]))
        );
    }
    #[test]
    fn unsupported_result_values_never_become_fake_json() {
        for expression in ["()=>1", "1n", "{x:undefined}", "new Map()", "NaN"] {
            let code = format!("export const result={expression};");
            let mut runtime =
                Runtime::new(snapshot(&[("a", &code)], &[]), Default::default()).unwrap();
            assert_eq!(
                runtime.read_export("a", "result").unwrap_err().code,
                "unsupported_value"
            );
        }
    }
}
