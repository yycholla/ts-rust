import { createWorkerClient } from './worker-client.js';

/** A disposable worker owning its Rust/WASM instance. */
export function createFormatter(input) {
  return createWorkerClient(input, new URL('./formatter-worker.js', import.meta.url));
}
