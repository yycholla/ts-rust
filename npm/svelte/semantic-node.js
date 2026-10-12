import { readFile } from 'node:fs/promises';
import { Worker } from 'node:worker_threads';
import { compileSemanticModule } from './semantic.js';
import { connectSemanticWorker } from './semantic-client.js';
export { SemanticError } from './semantic.js';

/** Load a supplied module, bytes, local path/file URL, or HTTP URL. */
export async function createSemanticSession(options) {
  let source = options.wasm ?? new URL('./generated/ts_svelte_semantic.wasm', import.meta.url);
  if (typeof source === 'string' && source.startsWith('file:')) source = new URL(source);
  if ((typeof source === 'string' && !/^https?:\/\//.test(source)) || (source instanceof URL && source.protocol === 'file:')) {
    source = await readFile(source);
  }
  const module = await compileSemanticModule(source);
  const stackSizeMb = options.stackSizeMb ?? 256;
  if (!Number.isInteger(stackSizeMb) || stackSizeMb < 1 || stackSizeMb > 1024) throw new RangeError('stackSizeMb must be an integer from 1 to 1024');
  const worker = new Worker(new URL('./semantic-worker.js', import.meta.url), {
    execArgv: [], resourceLimits: { stackSizeMb },
  });
  return connectSemanticWorker(worker, {
    module, params: { root: options.root, project: options.project, files: options.files, capabilities: options.capabilities },
    snapshot: options.snapshot, onRefresh: options.onRefresh, onOutput: options.onOutput,
  });
}
