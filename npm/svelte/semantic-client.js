import { SemanticError } from './semantic.js';

let nextEpoch = 0;
const newEpoch = () => `${Date.now()}-${++nextEpoch}-${globalThis.crypto?.randomUUID?.() ?? ''}`;

/** Connect a browser Worker or Node worker_threads Worker. Exported for hosts
 * with custom worker launchers; all messages carry an instance epoch. */
export async function connectSemanticWorker(worker, options) {
  const epoch = options.epoch ?? newEpoch();
  let id = 0, closed = false, closing = false;
  const pending = new Map();
  const cleanups = [];
  const listen = (name, fn) => {
    if (typeof worker.on === 'function') {
      worker.on(name, fn);
      cleanups.push(() => worker.off(name, fn));
    } else {
      const callback = name === 'message' ? event => fn(event.data) : fn;
      worker.addEventListener(name, callback);
      cleanups.push(() => worker.removeEventListener(name, callback));
    }
  };
  const die = error => {
    if (closed) return;
    closed = true;
    for (const { reject } of pending.values()) reject(error);
    pending.clear();
    for (const cleanup of cleanups.splice(0)) cleanup();
    try { Promise.resolve(worker.terminate()).catch(() => {}); } catch {}
  };
  listen('message', message => {
    if (!message || message.epoch !== epoch || closed) return;
    if (message.type === 'fatal') {
      die(new SemanticError(message.error?.code ?? 'worker_failed', message.error?.message ?? 'Semantic worker failed', { stderr: message.error?.stderr }));
      return;
    }
    if (message.type === 'output') {
      try { options.onOutput?.(message.fd, message.bytes); } catch {}
      return;
    }
    if (message.type !== 'response' || !pending.has(message.id)) return;
    const waiter = pending.get(message.id);
    pending.delete(message.id);
    if (message.error) waiter.reject(new SemanticError(message.error.code, message.error.message));
    else {
      const refresh = new Set([...(message.value?.refresh ?? []), ...(message.value?.result?.refresh ?? [])]);
      for (const method of refresh) { try { options.onRefresh?.(method); } catch {} }
      waiter.resolve(message.value);
    }
  });
  listen('error', error => die(new SemanticError('worker_failed', error.message ?? String(error))));
  listen('messageerror', () => die(new SemanticError('worker_failed', 'Semantic worker message could not be decoded')));
  if (typeof worker.on === 'function') listen('exit', code => die(new SemanticError('worker_exit', `Semantic worker exited (${code})`)));
  const send = (type, fields = {}, allowClosing = false) => {
    if (closed || closing && !allowClosing) return Promise.reject(new SemanticError('closed', 'Semantic worker is closed'));
    const requestId = ++id;
    return new Promise((resolve, reject) => {
      pending.set(requestId, { resolve, reject });
      try { worker.postMessage({ epoch, id: requestId, type, ...fields }); }
      catch (error) { pending.delete(requestId); reject(error); }
    });
  };
  let initialized;
  try {
    initialized = await send('initialize', { module: options.module, params: options.params, snapshot: options.snapshot });
    if (!initialized.ok) throw new SemanticError(initialized.error.code, initialized.error.message, { reply: initialized });
  } catch (error) { die(error); throw error; }
  return {
    initialized,
    request(op, params = {}) { return send('request', { op, params }); },
    files(files) { return send('request', { op: 'files', params: { files } }); },
    snapshot() { return send('snapshot'); },
    async shutdown() {
      if (closed || closing) throw new SemanticError('closed', 'Semantic worker is closed');
      closing = true;
      try { return await send('request', { op: 'shutdown', params: {} }, true); }
      finally { die(new SemanticError('closed', 'Semantic worker shut down')); }
    },
    dispose() { die(new SemanticError('closed', 'Semantic worker disposed')); },
  };
}
