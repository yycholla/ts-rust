export type Json = null | boolean | number | string | Json[] | { [key: string]: Json };
export type WasmSource = WebAssembly.Module | ArrayBuffer | ArrayBufferView | URL | string | Response;
export interface SemanticReply<T = Json> {
  id: number;
  ok: boolean;
  session: number;
  generation: number;
  result?: T;
  error?: { code: string; message: string; details?: Json };
  refresh?: string[];
}
export interface SemanticInitialize {
  root: string;
  project: string;
  files: Record<string, string>;
  capabilities?: Json;
}
export interface SemanticDocument {
  uri: string;
  languageId: string;
  /** Signed LSP integer; subsequent versions must strictly increase. */
  version: number;
  text: string;
}
/** Replayable inputs, not serialized checker memory. Keep snapshots private if
 * project sources are private. Snapshot export is bounded to 64 MiB JSON. */
export interface SemanticSnapshot {
  format: 1;
  initialize: SemanticInitialize;
  documents: SemanticDocument[];
  preferences: Json | null;
}
export interface SemanticSession {
  /** Initialization reply, including UTF-16 encoding and semantic token legend. */
  readonly initialized: SemanticReply;
  /** Calls serialize automatically. Engine request failures return ok:false;
   * transport, lifecycle and trap failures reject the promise. */
  request<T = Json>(op: string, params?: Json): Promise<SemanticReply<T>>;
  files(files: Record<string, string | null>): Promise<SemanticReply>;
  /** Rejects after a failed mutation because engine state may have changed. */
  snapshot(): Promise<SemanticSnapshot>;
  shutdown(): Promise<SemanticReply>;
  /** Immediate worker termination; rejects queued and future work. */
  dispose(): void;
}
export interface SemanticOptions {
  wasm?: WasmSource;
  root?: string;
  project?: string;
  files?: Record<string, string>;
  capabilities?: Json;
  /** Restore instead of initializing from root/project/files. */
  snapshot?: SemanticSnapshot;
  /** Node only, default 256 MiB; integer 1..1024. */
  stackSizeMb?: number;
  onRefresh?: (method: string) => void;
  onOutput?: (fd: 1 | 2, bytes: Uint8Array) => void;
}
export interface SemanticHost {
  initialize(params: SemanticInitialize): Promise<SemanticReply>;
  request<T = Json>(op: string, params?: Json): Promise<SemanticReply<T>>;
  snapshot(): Promise<SemanticSnapshot>;
  shutdown(): Promise<SemanticReply>;
  dispose(): void;
}
export class SemanticError extends Error {
  readonly code: string;
  readonly stderr?: string;
  readonly reply?: SemanticReply;
  readonly fatal?: boolean;
  constructor(code: string, message: string, details?: Record<string, unknown>);
}
export const MAX_FRAME: number;
/** Strings/URLs use fetch here; the Node session entry also supports local paths. */
export function compileSemanticModule(source: WasmSource): Promise<WebAssembly.Module>;
/** Low-level execution runs on the calling thread. Prefer a worker entrypoint. */
export function createSemanticHost(source: WasmSource, options?: Pick<SemanticOptions, 'onOutput'>): Promise<SemanticHost>;
export function restoreSemanticHost(source: WasmSource, snapshot: SemanticSnapshot, options?: Pick<SemanticOptions, 'onOutput'>): Promise<SemanticHost>;
