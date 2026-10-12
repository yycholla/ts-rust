import type { SemanticInitialize, SemanticOptions, SemanticSession, SemanticSnapshot } from './semantic.js';
export function connectSemanticWorker(worker: Worker | {
  postMessage(message: unknown): void;
  terminate(): unknown;
  on(event: string, listener: (...args: any[]) => void): unknown;
  off(event: string, listener: (...args: any[]) => void): unknown;
}, options: {
  module: WebAssembly.Module;
  params?: SemanticInitialize;
  snapshot?: SemanticSnapshot;
  epoch?: string;
  onRefresh?: SemanticOptions['onRefresh'];
  onOutput?: SemanticOptions['onOutput'];
}): Promise<SemanticSession>;
