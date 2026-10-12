import { createSemanticHost, SemanticError } from './semantic.js';

// One worker, one host. A promise chain also serializes module instantiation and
// snapshots, so generations are attached only after the preceding reply.
let post, subscribe;
if (typeof globalThis.postMessage === 'function' && typeof globalThis.document === 'undefined') {
  post = message => globalThis.postMessage(message);
  subscribe = fn => globalThis.addEventListener('message', event => fn(event.data));
} else {
  const { parentPort } = await import('node:worker_threads');
  if (!parentPort) throw new Error('Semantic worker must run inside a worker');
  post = message => parentPort.postMessage(message);
  subscribe = fn => parentPort.on('message', fn);
}
let epoch, host, poisoned = false, started = false, queue = Promise.resolve();
const fatal = error => {
  if (poisoned) return;
  poisoned = true;
  host?.dispose();
  post({ epoch, type: 'fatal', error: { code: error.code ?? 'engine_trap', message: error.message ?? String(error), stderr: error.stderr } });
};
subscribe(message => {
  // Do not allow messages from another instance to enter this worker's queue.
  if (!message || typeof message.epoch !== 'string' || !Number.isSafeInteger(message.id) || message.id < 1) return;
  if (epoch !== undefined && message.epoch !== epoch) return;
  epoch ??= message.epoch;
  queue = queue.then(async () => {
    if (poisoned) return;
    let value;
    try {
      if (message.type === 'initialize') {
        if (started) throw new SemanticError('invalid_request', 'Worker already initialized');
        started = true;
        host = await createSemanticHost(message.module, { onOutput: (fd, bytes) => post({ epoch, type: 'output', fd, bytes }) });
        const snapshot = message.snapshot;
        if (snapshot !== undefined && (snapshot?.format !== 1 || !snapshot.initialize || !Array.isArray(snapshot.documents))) {
          throw new SemanticError('invalid_snapshot', 'Unsupported semantic snapshot');
        }
        value = await host.initialize(snapshot === undefined ? message.params : snapshot.initialize);
        if (snapshot !== undefined && value.ok) {
          const check = reply => {
            if (!reply.ok) throw new SemanticError(reply.error.code, reply.error.message);
            value.generation = reply.generation;
          };
          if (snapshot.preferences !== null) check(await host.request('configure', snapshot.preferences));
          for (const document of snapshot.documents) check(await host.request('open', { textDocument: document }));
        }
      } else if (!host) throw new SemanticError('not_initialized', 'Worker is not initialized');
      else if (message.type === 'snapshot') value = await host.snapshot();
      else if (message.type === 'request') value = await host.request(message.op, message.params);
      else throw new SemanticError('invalid_request', `Unknown worker message ${message.type}`);
      post({ epoch, id: message.id, type: 'response', value });
      if (value?.ok === false && ['closed', 'engine_panic', 'unported', 'exhausted', 'frame_too_large'].includes(value.error?.code)) {
        fatal(new SemanticError(value.error.code, value.error.message));
      }
    } catch (error) {
      if (!error.fatal && (['invalid_request', 'invalid_snapshot', 'snapshot_unavailable', 'not_initialized', 'frame_too_large'].includes(error.code)
          || error instanceof TypeError && message.type !== 'initialize')) {
        post({ epoch, id: message.id, type: 'response', error: { code: error.code ?? 'invalid_request', message: error.message } });
      } else fatal(error);
    }
  }).catch(fatal);
});
