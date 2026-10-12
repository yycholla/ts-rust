/** Each compiler owns a disposable worker and its Rust/WASM memory. */
export async function createWorkerClient(input, workerUrl) {
  const module = input instanceof WebAssembly.Module ? input : await WebAssembly.compile(input);
  const browser = typeof process === 'undefined' || !process.versions?.node;
  const WorkerClass = browser ? globalThis.Worker : (await import('node:worker_threads')).Worker;
  // This packaged module needs no caller loaders or CLI entry-point flags.
  // Inheriting --input-type breaks file workers; --test flags can start another
  // runner instead of the worker when the caller disables test isolation.
  const worker = new WorkerClass(workerUrl, browser ? { type: 'module' } : { execArgv: [] });
  let nextId = 0;
  let closed = false;
  const pending = new Map();
  let readyResolve, readyReject;
  const ready = new Promise((resolve, reject) => { readyResolve = resolve; readyReject = reject; });
  function close(reason = new Error('Compiler disposed')) {
    if (closed) return;
    closed = true;
    readyReject(reason);
    for (const { reject } of pending.values()) reject(reason);
    pending.clear();
    void worker.terminate();
  }
  function receive(message) {
    if (message.ready) { readyResolve(); return; }
    if (message.fatal) { close(new Error(message.fatal)); return; }
    const item = pending.get(message.id);
    if (!item) return;
    pending.delete(message.id);
    item.resolve(message.response);
  }
  if (browser) {
    worker.addEventListener('message', event => receive(event.data));
    worker.addEventListener('error', event => close(new Error(event.message)));
    worker.addEventListener('messageerror', () => close(new Error('Compiler worker message could not be decoded')));
  } else {
    worker.on('message', receive);
    worker.on('error', close);
    worker.on('messageerror', close);
    worker.on('exit', code => close(new Error(`Compiler worker exited (${code})`)));
  }
  try {
    worker.postMessage({ module });
    await ready;
  } catch (error) {
    close(error);
    throw error;
  }
  return Object.freeze({
    request(request) {
      if (closed) return Promise.reject(new Error('Compiler disposed'));
      let input;
      try {
        input = JSON.stringify(request);
        if (typeof input !== 'string') throw new TypeError('Request must be JSON serializable');
        if (new TextEncoder().encode(input).length > 16 * 1024 * 1024) {
          throw new RangeError('Request exceeds 16 MiB');
        }
      } catch (error) { return Promise.reject(error); }
      if (closed) return Promise.reject(new Error('Compiler disposed'));
      const id = nextId++;
      return new Promise((resolve, reject) => {
        pending.set(id, { resolve, reject });
        try { worker.postMessage({ id, input }); }
        catch (error) { pending.delete(id); reject(error); }
      });
    },
    dispose() { close(); },
  });
}
