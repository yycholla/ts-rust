import { createTooling } from './tooling.js';
import { compileSemanticModule } from './semantic.js';

/** A backend or lifecycle failure. `fatal` means both workers were retired. */
export class SvelteProjectError extends Error {
  constructor(code, message, details = {}) {
    super(message);
    this.name = 'SvelteProjectError';
    this.code = code;
    Object.assign(this, details);
  }
}

// Capture inputs before enqueueing: a caller editing its object while a worker
// runs must not change which snapshot or request the queued operation observes.
function capture(value) {
  const encoded = JSON.stringify(value);
  if (encoded === undefined) throw new TypeError('Expected a JSON value');
  return JSON.parse(encoded);
}

/** Own two isolated Rust workers: Svelte projections/mapping and TS semantics.
 * Files, resolved compiler options and dependency declarations are supplied by
 * the caller. This module neither reads projects nor implements source mapping.
 */
export async function createSvelteProjectSession(options) {
  const {
    toolingWasm, semanticWasm, capabilities, stackSizeMb, onRefresh, onOutput,
    ...input
  } = options;
  const initial = capture(input);
  let tooling;
  let semantic;
  let closed = false;
  let revision = 0;
  let tail = Promise.resolve();
  const waiting = new Set();

  function stop(error = new SvelteProjectError('closed', 'Svelte project disposed', { fatal: true })) {
    if (closed) return;
    closed = true;
    tooling?.dispose();
    semantic?.dispose();
    for (const item of waiting) item.reject(error);
    waiting.clear();
  }
  function live() {
    if (closed) throw new SvelteProjectError('closed', 'Svelte project is closed', { fatal: true });
  }
  function fatal(error, code = 'worker_failure') {
    const failure = new SvelteProjectError(code, error?.message ?? String(error), { cause: error, fatal: true });
    stop(failure);
    return failure;
  }
  async function tool(op, params = {}) {
    live();
    let reply;
    try { reply = await tooling.request({ op, ...params }); }
    catch (error) { throw fatal(error); }
    live();
    if (reply?.ok === false) {
      throw new SvelteProjectError(reply.error?.code ?? 'frontend_failure', reply.error?.message ?? 'Rust project frontend rejected request', { reply, fatal: false });
    }
    if (reply?.ok !== true || !Object.hasOwn(reply, 'result')) {
      throw fatal(new Error('Malformed Rust project frontend response'), 'protocol_error');
    }
    return reply.result;
  }
  async function host(op, params) {
    live();
    let reply;
    try { reply = await semantic.request(op, params); }
    catch (error) { throw fatal(error); }
    live();
    if (reply?.ok !== true || !Object.hasOwn(reply, 'result')) {
      // Some host errors occur after a state mutation. Never leave a frontend
      // revision live against a possibly divergent checker snapshot.
      throw fatal(new SvelteProjectError(reply?.error?.code ?? 'semantic_failure', reply?.error?.message ?? 'Semantic worker rejected request', { reply }));
    }
    return reply.result;
  }
  function enqueue(operation) {
    if (closed) return Promise.reject(new SvelteProjectError('closed', 'Svelte project is closed', { fatal: true }));
    return new Promise((resolve, reject) => {
      const item = { resolve, reject };
      waiting.add(item);
      const task = tail.then(async () => { live(); return operation(); });
      tail = task.then(() => {}, () => {});
      task.then(value => { waiting.delete(item); resolve(value); }, error => { waiting.delete(item); reject(error); });
    });
  }
  async function completeModules(prepared, candidate) {
    if (!Array.isArray(prepared.moduleTranspilation)) {
      throw fatal(new Error('Missing native module transpilation inventory'), 'protocol_error');
    }
    for (const pending of prepared.moduleTranspilation) {
      const result = await host('transpileModule', { fileName: pending.fileName, source: pending.source });
      if (result.ok !== true) {
        throw new SvelteProjectError('module_transpilation_failed', 'Native TypeScript module transpilation is incomplete', { result, uri: pending.document.uri, fatal: false });
      }
      await tool('projectModuleComplete', { uri: pending.document.uri, result, candidate });
    }
  }
  async function acknowledgeGraph() {
    const query = await tool('projectQuery', { method: 'workspace/diagnostic', params: {} });
    if (Object.hasOwn(query, 'local')) return query.local;
    const response = await host(query.op, query.params);
    return tool('projectRespond', { ticket: query.ticket, response });
  }
  function verifyPrepared(prepared) {
    if (!Number.isSafeInteger(prepared?.revision) || prepared.revision < 1 || !prepared.initialize || !Array.isArray(prepared.open)) {
      throw fatal(new Error('Malformed prepared project'), 'protocol_error');
    }
  }

  try {
    const module = await compileSemanticModule(toolingWasm ?? new URL('./generated/ts_svelte_tools_bg.wasm', import.meta.url));
    tooling = await createTooling(module);
    const prepared = await tool('projectCreate', { snapshot: initial });
    verifyPrepared(prepared);
    const backend = typeof process !== 'undefined' && process.versions?.node
      ? await import('./semantic-node.js') : await import('./semantic-browser.js');
    semantic = await backend.createSemanticSession({
      wasm: semanticWasm, ...prepared.initialize, capabilities, stackSizeMb, onRefresh, onOutput,
    });
    if (semantic.initialized?.ok !== true) throw new Error('Semantic initialization failed');
    for (const request of prepared.open) await host(request.op, request.params);
    await completeModules(prepared, false);
    await acknowledgeGraph();
    const completed = await tool('projectPrepared');
    if (completed.complete !== true) throw new SvelteProjectError('incomplete_project', 'Rune module diagnostics remain unresolved');
    revision = completed.revision;
  } catch (error) {
    stop();
    throw error;
  }

  return Object.freeze({
    initialized: semantic.initialized,
    get revision() { return revision; },
    request(method, params = {}) {
      let copied;
      try { copied = capture(params); } catch (error) { return Promise.reject(error); }
      return enqueue(async () => {
        const query = await tool('projectQuery', { method, params: copied });
        if (query.revision !== revision) throw fatal(new Error('Frontend query revision mismatch'), 'stale_revision');
        if (Object.hasOwn(query, 'local')) return query.local;
        if (!Number.isSafeInteger(query.ticket) || typeof query.op !== 'string') {
          throw fatal(new Error('Malformed semantic query ticket'), 'protocol_error');
        }
        const response = await host(query.op, query.params);
        return tool('projectRespond', { ticket: query.ticket, response });
      });
    },
    prepared() { return enqueue(() => tool('projectPrepared')); },
    replace(snapshot) {
      let copied;
      try { copied = capture(snapshot); } catch (error) { return Promise.reject(error); }
      return enqueue(async () => {
        // Pure Rust preparation may fail without altering either live snapshot.
        const candidate = await tool('projectReplace', { snapshot: copied });
        verifyPrepared(candidate.prepared);
        if (candidate.sync?.previousRevision !== revision || candidate.sync.revision !== candidate.prepared.revision || !Array.isArray(candidate.sync.requests)) {
          throw fatal(new Error('Replacement revision mismatch'), 'stale_revision');
        }
        try { await completeModules(candidate.prepared, true); }
        catch (error) {
          if (!closed) {
            try { await tool('projectDiscard'); }
            catch (discardError) { throw fatal(discardError); }
          }
          throw error;
        }
        try {
          for (const request of candidate.sync.requests) await host(request.op, request.params);
          const committed = await tool('projectCommit', { revision: candidate.sync.revision });
          if (committed.revision !== candidate.sync.revision || committed.complete !== true) {
            throw new Error('Committed project is incomplete or has the wrong revision');
          }
          revision = committed.revision;
          await acknowledgeGraph();
          return tool('projectPrepared');
        } catch (error) {
          // Do not replay inverse edits: file/config notifications may already
          // have invalidated semantic state. A fresh session is the rollback.
          throw fatal(error, 'replacement_failed');
        }
      });
    },
    shutdown() {
      return enqueue(async () => {
        let reply;
        try { reply = await semantic.shutdown(); }
        catch (error) { throw fatal(error); }
        // Resolve this one operation before retiring other queued work.
        if (reply?.ok !== true) throw fatal(new Error('Semantic shutdown failed'));
        closed = true;
        tooling.dispose();
        semantic.dispose();
        return reply;
      });
    },
    dispose() { stop(); },
  });
}
