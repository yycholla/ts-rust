import { createWorkerClient } from './worker-client.js';

/** A disposable worker owning its Rust/WASM instance. */
export function createTooling(input) {
  return createWorkerClient(input, new URL('./tooling-worker.js', import.meta.url));
}
