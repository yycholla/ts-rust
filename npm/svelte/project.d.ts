import type { Json, SemanticInitialize, SemanticOptions, SemanticReply, WasmSource } from './semantic.js';

export interface ProjectDocument {
  /** Canonical file URI matching the absolute POSIX virtual path. */
  uri: string;
  /** Signed LSP integer, strictly increasing when the text changes. */
  version: number;
  text: string;
}
export interface ProjectComponent {
  /** Relative to the virtual project root. */
  path: string;
  document: ProjectDocument;
  typescript?: boolean;
  check?: boolean;
  /** External preprocessors are currently unsupported and rejected. */
  preprocess?: null;
}
/** A complete caller-resolved filesystem snapshot. Dependency packages and
 * their real declaration files must be supplied, including Svelte's package,
 * types, svelte-html and svelte-check's svelte-shims-v4.d.ts for components.
 * Supply resolved compilerOptions; the frontend owns root tsconfig.json.
 * All nondependency .svelte files require components; rune modules require
 * documents. Ordinary closed files can appear only in files/rootFiles.
 */
export interface SvelteProjectSnapshot {
  root: string;
  files: Record<string, string>;
  components?: ProjectComponent[];
  documents?: ProjectDocument[];
  rootFiles?: string[];
  compilerOptions?: Record<string, Json>;
  compatibility?: {
    svelteVersion?: string;
    kitVersion?: string | null;
    typescriptVersion?: string | null;
    allowPrerelease?: boolean;
  };
  runes?: boolean | null;
  preprocess?: null;
}
export interface SvelteProjectOptions extends SvelteProjectSnapshot {
  /** Module, bytes, Response or fetch URL. Local paths must be read by caller. */
  toolingWasm?: WasmSource;
  /** Module/bytes/URL; Node also accepts a local path. */
  semanticWasm?: WasmSource;
  capabilities?: Json;
  stackSizeMb?: number;
  onRefresh?: SemanticOptions['onRefresh'];
  onOutput?: SemanticOptions['onOutput'];
}
export interface PreparedSvelteProject {
  revision: number;
  initialize: SemanticInitialize;
  open: Array<{ op: string; params: Json }>;
  projections: Json[];
  diagnostics: Json[];
  moduleTranspilation: Array<{ document: ProjectDocument; fileName: string; source: string }>;
  loadedFiles: string[] | null;
  deferredComponents: string[];
  semanticUnavailable: Json[];
  /** False when a required or loaded component cannot produce a checking projection. */
  semanticComplete: boolean;
  /** Whether requested rune-module transpilation has completed. */
  complete: boolean;
}
export interface SvelteProjectSession {
  /** Semantic initialization reply, including the semantic token legend. */
  readonly initialized: SemanticReply;
  /** Internal positive revision; original signed editor versions are retained. */
  readonly revision: number;
  /** LSP method and original-source coordinates. Rust owns projection, mapping,
   * ticket lifetimes and edit validation. workspace/diagnostic returns native
   * compiler diagnostics, mapped document reports and explicit unmapped reports.
   * These requests serialize with replacements and shutdown.
   */
  request<T = Json>(method: string, params?: Json): Promise<T>;
  /** Includes generated sources and supplied files; may contain private code. */
  prepared(): Promise<PreparedSvelteProject>;
  /** Prepare a complete replacement, synchronize semantics, then commit mapping.
   * Preparation rejection preserves the active session. A partially applied
   * update retires both workers; recreate from the desired complete snapshot.
   */
  replace(snapshot: SvelteProjectSnapshot): Promise<PreparedSvelteProject>;
  shutdown(): Promise<SemanticReply>;
  /** Immediate termination, including rejection of pending and future work. */
  dispose(): void;
}
export class SvelteProjectError extends Error {
  readonly code: string;
  readonly fatal?: boolean;
  readonly reply?: unknown;
  readonly result?: Json;
  readonly uri?: string;
  constructor(code: string, message: string, details?: Record<string, unknown>);
}
export function createSvelteProjectSession(options: SvelteProjectOptions): Promise<SvelteProjectSession>;
