/** Native framework error; the original JSON error remains available as `detail`. */
export class SvelteRuntimeError extends Error {
  constructor(detail) {
    super(detail.message);
    this.name = 'SvelteRuntimeError';
    this.code = detail.code;
    this.detail = detail;
  }
}

/** Thin synchronous JSON interface. Run inside a worker to avoid blocking a UI. */
export class SvelteRuntime {
  #dispatch;

  constructor(bindings) {
    if (typeof bindings?.svelte_runtime_json !== 'function') {
      throw new TypeError('An initialized svelte_runtime_json binding is required');
    }
    this.#dispatch = bindings.svelte_runtime_json;
  }

  /** Return the Rust envelope unchanged, including errors and future fields. */
  request(request) {
    return JSON.parse(this.#dispatch(JSON.stringify(request)));
  }

  #result(request) {
    const response = this.request(request);
    if (!response.ok) throw new SvelteRuntimeError(response.error);
    return response.result;
  }

  capabilities() {
    return this.#result({ op: 'capabilities' });
  }

  create(options) {
    return this.#result({ ...options, op: 'create' });
  }

  loadNodes(session) {
    return this.#result({ op: 'loadNodes', session });
  }

  analyzeMetadata(session, options) {
    return this.#result({ op: 'analyzeMetadata', session, options });
  }

  /** Requires route metadata first. The chunk inventory is fixed after success. */
  analyzeRemotes(session, chunks) {
    return this.#result({ op: 'analyzeRemotes', session, chunks });
  }

  /** Configure a separate rendering realm; application init runs only on start. */
  createPrerender(options) {
    return this.#result({ ...options, op: 'createPrerender' });
  }

  startPrerender(session) {
    return this.#result({ op: 'startPrerender', session });
  }

  /** Visit one encoded pathname. Returns bytes without crawling or saving. */
  visitPrerender(session, path) {
    return this.#result({ op: 'visitPrerender', session, path });
  }

  /** Inspect the main response before reading its body. One active visit per session. */
  beginPrerenderVisit(session, path) {
    return this.#result({ op: 'beginPrerenderVisit', session, path });
  }

  readPrerenderMain(session, token) {
    return this.#result({ op: 'readPrerenderMain', session, token });
  }

  /** Inspect the next dependency; null finishes the active visit. */
  nextPrerenderDependency(session, token) {
    return this.#result({ op: 'nextPrerenderDependency', session, token });
  }

  /** Read the inspected dependency's body getter/stream at this stage. */
  readPrerenderDependency(session, token) {
    return this.#result({ op: 'readPrerenderDependency', session, token });
  }

  /** Explicitly publish byte arrays for subsequent snapshot reads. */
  publishPrerender(session, file, bytes) {
    return this.#result({ op: 'publishPrerender', session, file, bytes });
  }

  dispose(session) {
    return this.#result({ op: 'dispose', session });
  }
}

/** Initialize the real wasm-bindgen module. Supplying bindings enables hosts to
 * choose another generated-module URL without replacing native functionality.
 * Reusing a module namespace reuses its Rust service and existing sessions.
 */
export async function initializeRuntime({ moduleOrPath, bindings } = {}) {
  bindings ??= await import('./generated/ts_svelte_runtime.js');
  await bindings.default(moduleOrPath === undefined ? undefined : { module_or_path: moduleOrPath });
  return new SvelteRuntime(bindings);
}
