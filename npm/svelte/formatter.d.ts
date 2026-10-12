import type { Document, Range, ByteRange } from './index.js';
import type { CompilerReply } from './compiler.js';

/** Resolved options; executable configurations and JS plugins are not evaluated. */
export interface FormatOptions {
  printWidth?: number;
  tabWidth?: number;
  useTabs?: boolean;
  singleQuote?: boolean;
  semi?: boolean;
  trailingComma?: 'all' | 'es5' | 'none';
  quoteProps?: 'as-needed' | 'consistent' | 'preserve';
  arrowParens?: 'always' | 'avoid';
  bracketSpacing?: boolean;
  bracketSameLine?: boolean;
  singleAttributePerLine?: boolean;
  endOfLine?: 'lf' | 'crlf' | 'cr' | 'auto';
  sortImports?: boolean;
  svelteAllowShorthand?: boolean;
  svelteIndentScriptAndStyle?: boolean;
  svelteSortOrder?: string;
}
export interface FormattedDocument {
  uri: string;
  version: number;
  filename: string;
  language: 'svelte' | 'typescript' | 'javascript' | 'css' | 'json';
  code: string;
  changed: boolean;
  /** Empty if unchanged; otherwise one replacement of the original document.
   * range uses UTF-16, bytes uses UTF-8, including the original BOM/line endings.
   */
  edits: Array<{ range: Range; bytes: ByteRange; newText: string }>;
}
export interface Formatter {
  /** URI selects the language unless filename overrides it. Requests perform
   * no filesystem/config discovery. Parse and option failures return ok:false
   * without edits; they do not close the worker.
   */
  request(request: { op: 'format'; document: Document; filename?: string; options?: FormatOptions }): Promise<CompilerReply<FormattedDocument>>;
  request(request: { op: 'capabilities' }): Promise<CompilerReply<Record<string, unknown>>>;
  /** Release the worker/WASM instance. Pending and later requests reject.
   * Repeated disposal is safe.
   */
  dispose(): void;
}
/** Create an isolated Rust/WASM formatter worker from bytes or a compiled module.
 * No native child process or JavaScript formatter is used by this interface.
 */
export function createFormatter(input: BufferSource | WebAssembly.Module): Promise<Formatter>;
