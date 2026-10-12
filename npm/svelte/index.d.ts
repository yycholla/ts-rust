/** Zero-based editor position; character is measured in UTF-16 code units. */
export interface Position { line: number; character: number }
export interface Range { start: Position; end: Position }
/** Half-open UTF-8 byte offsets, NOT JavaScript string indices. */
export interface ByteRange { start: number; end: number }
/** Source must contain valid Unicode (no unpaired UTF-16 surrogates). */
export interface Document { uri: string; version: number; text: string }
/** Sequential edit coordinates. Missing/null range replaces the whole text. */
export interface Edit { range?: Range | null; text: string }
export type Chunk = { copy: ByteRange } | { generated: string };
export interface Mapping { generated: ByteRange; source: ByteRange }
export interface MappedRange { bytes: ByteRange; range: Range }
export interface Capabilities {
    protocolVersion: 1;
    stage: "source-foundation";
    coordinates: { offsets: "utf8-bytes"; positions: "utf16"; ranges: "half-open" };
    operations: ["capabilities", "locate", "edit", "project"];
    svelteParsing: false;
    typeChecking: false;
    compilation: false;
    lsp: false;
    limits: { requestBytes: number; operations: number; workBytes: number; projectionBytes: number };
}
export type Request =
    | { op: "capabilities" }
    | { op: "locate"; document: Document; positions: Position[]; offsets: number[] }
    | { op: "edit"; document: Document; version: number; edits: Edit[] }
    | { op: "project"; document: Document; chunks: Chunk[]; ranges: ByteRange[] };
export type ResultFor<R extends Request> =
    R extends { op: "capabilities" } ? Capabilities :
    R extends { op: "locate" } ? { uri: string; version: number; byteOffsets: number[]; positions: Position[]; lineCount: number } :
    R extends { op: "edit" } ? { document: Document; lineCount: number } :
    { uri: string; version: number; text: string; mappings: Mapping[]; ranges: (MappedRange | null)[] };
export type Response<T> = { ok: true; result: T } | { ok: false; error: { code: string; message: string } };
export interface SourceTools {
    request<R extends Request>(request: R): Response<ResultFor<R>>;
    dispose(): void;
}
/** The same Rust engine runs in Node, browsers and browser/Node workers. */
export function createSourceTools(wasm: WebAssembly.Module | BufferSource): Promise<SourceTools>;
