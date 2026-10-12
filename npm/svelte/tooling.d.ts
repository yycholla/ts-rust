import type { Document, Edit, Range, ByteRange } from './index.js';
import type { Versions, CompilerDiagnostic, CompilerReply } from './compiler.js';
export type LintSeverity = 'off' | 'warn' | 'error' | 0 | 1 | 2;
/** Resolved rule configuration. Active unsupported rules/options are errors. */
export interface LintConfig { rules?: Record<string, LintSeverity | [LintSeverity, ...unknown[]]> }
export interface LintFix {
  message: string;
  uri: string;
  version: number;
  edits: Array<{ range: Range; bytes: ByteRange; newText: string }>;
}
export interface LintDiagnostic extends CompilerDiagnostic {
  version?: number;
  fix?: LintFix;
  suggestions?: Array<{ description: string; fix: LintFix }>;
}
export type ToolingRequest =
  | { op: 'lint'; document: Document; runes?: boolean | null; compatibility?: Versions; lintConfig?: LintConfig }
  | { op: 'sessionConfigure'; runes?: boolean | null; compatibility?: Versions; lintConfig?: LintConfig }
  | { op: 'sessionOpen'; document: Document }
  | { op: 'sessionChange'; uri: string; version: number; edits: Edit[] }
  | { op: 'sessionClose'; uri: string }
  | { op: 'sessionDiagnostics'; uri: string }
  | { op: 'sessionCodeActions'; uri: string; params: {
      textDocument: { uri: string }; range: Range;
      context?: { only?: string[]; diagnostics?: unknown[] }
    } }
  | { op: 'sessionSyntax'; uri: string;
      method: 'textDocument/linkedEditingRange' | 'textDocument/foldingRange' | 'textDocument/documentSymbol' | 'textDocument/selectionRange';
      params?: { position?: import('./index.js').Position; positions?: import('./index.js').Position[] } }
  | { op: 'sessionStats' };
export interface ToolingResult {
  uri?: string;
  version?: number;
  diagnostics?: LintDiagnostic[];
  /** Browser tooling provides compiler/lint diagnostics, not project type checking. */
  typeChecking?: false;
  documents?: number;
  sourceBytes?: number;
  cachedDiagnostics?: number;
  invalidated?: string[];
}
export interface TemplateSymbol {
  name: string;
  kind: number;
  range: Range;
  selectionRange: Range;
  children: TemplateSymbol[];
}
export interface TemplateSelection { range: Range; parent?: TemplateSelection }
export interface TemplateFold {
  startLine: number;
  startCharacter: number;
  endLine: number;
  endCharacter: number;
  kind: 'comment' | 'region';
}
export type SyntaxResult = { ranges: Range[]; wordPattern?: string }
  | TemplateSymbol[] | TemplateSelection[] | TemplateFold[] | null;
export interface LintCodeAction {
  title: string;
  kind: 'quickfix' | 'source.fixAll.svelte';
  isPreferred?: boolean;
  diagnostics?: unknown[];
  edit?: { documentChanges: Array<{
    textDocument: { uri: string; version: number };
    edits: Array<{ range: Range; newText: string }>;
  }> };
  disabled?: { reason: string };
}
export interface Tooling {
  request(request: Extract<ToolingRequest, { op: 'sessionCodeActions' }>): Promise<CompilerReply<LintCodeAction[]>>;
  request(request: Extract<ToolingRequest, { op: 'sessionSyntax' }>): Promise<CompilerReply<SyntaxResult>>;
  request(request: Exclude<ToolingRequest, { op: 'sessionSyntax' | 'sessionCodeActions' }>): Promise<CompilerReply<ToolingResult>>;
  request(request: ToolingRequest): Promise<CompilerReply<ToolingResult | SyntaxResult | LintCodeAction[]>>;
  dispose(): void;
}
export function createTooling(input: BufferSource | WebAssembly.Module): Promise<Tooling>;
