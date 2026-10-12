import type { Document, ByteRange, Range } from './index.js';

export type CompilerRequest =
  | { op: 'capabilities' }
  | { op: 'resolveCompatibility'; versions: Versions }
  | { op: 'compile'; document: Document; target: 'client' | 'server'; runes?: boolean | null; dev?: boolean; compatibility?: Versions; options?: BuildOptions }
  | { op: 'compileModule'; document: Document; target: 'client' | 'server'; dev?: boolean; compatibility?: Versions; options?: Pick<BuildOptions, 'rootDir' | 'experimental'> }
  | { op: 'checkModule'; document: Document; typescript: boolean; compatibility?: Versions }
  | { op: 'componentInfo'; document: Document; runes?: boolean | null; compatibility?: Versions }
  | { op: 'project'; document: Document; typescript: boolean; runes?: boolean | null; compatibility?: Versions };
export interface BuildOptions {
  rootDir?: string;
  sourcemap?: string | object;
  css?: 'external' | 'injected';
  preserveComments?: boolean;
  preserveWhitespace?: boolean;
  discloseVersion?: boolean;
  accessors?: boolean;
  immutable?: boolean;
  namespace?: 'html' | 'svg' | 'mathml';
  name?: string;
  outputFilename?: string;
  cssOutputFilename?: string;
  experimental?: { async: boolean };
}
export interface Versions {
  /** Exact installed version; defaults to MoncuraHR's 5.53.11. No tags or ranges. */
  svelteVersion?: string;
  kitVersion?: string | null;
  typescriptVersion?: string | null;
  allowPrerelease?: boolean;
}
export interface CompatibilityProfile {
  id: string;
  svelteVersion: string;
  kitVersion: string | null;
  typescriptVersion: string | null;
  channel: 'stable' | 'experimental';
  kitConfig: 'vite-plugin' | 'svelte-config-or-vite-plugin' | null;
  kitLibraryAlias: '#lib' | '$lib' | null;
  kitEnvironmentModule: '$app/env' | '$app/environment' | null;
  capabilities: {
    componentCompiler: 'prototype';
    checkingProjection: 'prototype';
    projectCheck: 'explicit-input-prototype';
    kitGraph: false;
    kitBuild: false;
    lsp: false;
  };
}
export interface CompatibilityResult { compatibility: CompatibilityProfile }
export interface CompilerDiagnostic {
  code: string;
  message: string;
  severity: 'warning' | 'error';
  uri: string;
  /** Backend UTF-8 offsets, or null when unavailable. */
  bytes: ByteRange | null;
  /** Validated UTF-16 editor range, or null when absent/invalid. */
  range: Range | null;
}
export interface Backend { name: string; commit: string; version: string; svelteTarget: string; appTarget: string; patches: string[]; clientDevelopment: boolean }
export interface CompileResult extends CompatibilityResult {
  uri: string;
  version: number;
  javascript: { code: string; map: string | null };
  css: { code: string; map: string | null; scope: string | null } | null;
  diagnostics: CompilerDiagnostic[];
  runes: boolean;
  backend: Backend;
}
export interface ProjectionResult extends CompatibilityResult {
  uri: string;
  version: number;
  code: string;
  map: string | null;
  mappings: Array<{ source: ByteRange; generated: ByteRange }> | null;
  props: Array<{ name: string; optional: boolean; bindable: boolean; type: string | null }>;
  runes: boolean;
  backend: Backend;
}
export interface CompilerCapabilities extends CompatibilityResult {
  stage: string;
  backend: Backend;
  operations: string[];
  targets: string[];
  sourceLanguages: string[];
  typeChecking: false;
  svelteKitBuild: false;
  lsp: false;
  maxSourceBytes: number;
  bom: false;
  svelteVersions: string[];
  kitVersions: string[];
}
export type CompilerReply<T> = { ok: true; result: T } | {
  ok: false; error: { code: string; message: string }; diagnostics?: CompilerDiagnostic[]; backend?: Backend; compatibility?: CompatibilityProfile;
};
export interface Compiler {
  request(request: Extract<CompilerRequest, { op: 'checkModule' }>): Promise<CompilerReply<Omit<CompileResult, 'javascript' | 'css'>>>;
  request(request: Extract<CompilerRequest, { op: 'componentInfo' }>): Promise<CompilerReply<CompatibilityResult & { typescript: boolean }>>;
  request(request: Extract<CompilerRequest, { op: 'capabilities' }>): Promise<CompilerReply<CompilerCapabilities>>;
  request(request: Extract<CompilerRequest, { op: 'resolveCompatibility' }>): Promise<CompilerReply<CompatibilityResult>>;
  request(request: Extract<CompilerRequest, { op: 'compile' }>): Promise<CompilerReply<CompileResult>>;
  request(request: Extract<CompilerRequest, { op: 'compileModule' }>): Promise<CompilerReply<CompileResult>>;
  request(request: Extract<CompilerRequest, { op: 'project' }>): Promise<CompilerReply<ProjectionResult>>;
  request(request: CompilerRequest): Promise<CompilerReply<CompilerCapabilities | CompileResult | ProjectionResult | CompatibilityResult>>;
  /** Terminates the worker; pending and future requests reject. */
  dispose(): void;
}
export function createCompiler(input: BufferSource | WebAssembly.Module): Promise<Compiler>;
