import { compileSemanticModule } from './semantic.js';
import { connectSemanticWorker } from './semantic-client.js';
export { SemanticError } from './semantic.js';

/** Browser entry: all checker execution occurs in a dedicated module Worker. */
export async function createSemanticSession(options) {
  const module = await compileSemanticModule(options.wasm ?? new URL('./generated/ts_svelte_semantic.wasm', import.meta.url));
  const worker = new Worker(new URL('./semantic-worker.js', import.meta.url), { type: 'module', name: 'ts-svelte-semantic' });
  return connectSemanticWorker(worker, {
    module, params: { root: options.root, project: options.project, files: options.files, capabilities: options.capabilities },
    snapshot: options.snapshot, onRefresh: options.onRefresh, onOutput: options.onOutput,
  });
}
