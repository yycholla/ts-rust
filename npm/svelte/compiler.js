import { createWorkerClient } from './worker-client.js';

/** A disposable worker owning its Rust/WASM instance. */
export function createCompiler(input) {
  return createWorkerClient(input, new URL('./compiler-worker.js', import.meta.url));
}
