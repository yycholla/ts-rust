/** Exact importer IDs/specifiers and all code are supplied by the embedding. */
export interface HostSnapshot {
  modules: Record<string, { source: string; importMetaUrl: string }>;
  resolutions: Record<string, Record<string,
    | { kind: 'module'; id: string }
    | { kind: 'builtin'; name: string }
    | { kind: 'external'; name: string }
  >>;
}
/** Omit the entire object for Rust defaults; provided objects require all fields. */
export interface RuntimeLimits {
  maxModules: number;
  maxSourceBytes: number;
  maxTotalSourceBytes: number;
  instructions: number;
  loopIterations: number;
  recursion: number;
  stack: number;
  jobs: number;
  queuedJobs: number;
  jsonBytes: number;
  jsonNodes: number;
  jsonDepth: number;
}
export type Initialization =
  | { profile: 'kit2'; internal: string; environment: string; publicPrefix: string; privatePrefix: string }
  | { profile: 'kit3'; server: string };
export interface BootstrapRequest {
  serverBuildId: string;
  snapshot: HostSnapshot;
  manifestEntry: string;
  initialization: Initialization;
  environment: Record<string, string>;
  /** Server-relative exact identities mapped to byte arrays (not typed arrays)
   * or standard base64 strings; prefer base64 for large snapshots. */
  assets: Record<string, number[] | string>;
  limits?: RuntimeLimits;
}
export interface FetchOptions {
  baseUrl?: string;
  origin?: string;
  /** Host-provided entropy: one 16-byte array per multipart boundary. */
  multipartEntropy?: number[][];
}
export interface CryptoOptions {
  secureContext?: boolean;
  /** Host-provided bytes, consumed once; exhaustion is an error. */
  entropy?: number[];
}
export interface CreateOptions {
  bootstrap: BootstrapRequest;
  fetchOptions?: FetchOptions;
  cryptoOptions?: CryptoOptions;
}
export interface CreatedSession { session: string; serverBuildId: string }
export interface NodeLoad {
  index: number;
  hasServerLoad: boolean;
  hasUniversalLoad: boolean;
  serverLoadCallable: boolean;
  universalLoadCallable: boolean;
}
export interface LoadedNodes { serverBuildId: string; nodes: NodeLoad[] }
export type JsonValue = null | boolean | number | string | JsonValue[] | { [key: string]: JsonValue };
/** Native normalization preserves absent/undefined separately from JSON null. */
export type RuntimeValue =
  | { kind: 'absent' }
  | { kind: 'undefined' }
  | { kind: 'json'; value: JsonValue };
export type MetadataProfile = '2.67.0' | '2.68.0' | '2.69.2' | '2.69.3' | '2.70.3' | '3.0.0-next.32' | '3.0.1';
export interface MetadataOptions { profile: MetadataProfile; hash?: boolean }
export interface PageMetadata {
  csr: RuntimeValue;
  ssr: RuntimeValue;
  prerender: RuntimeValue;
  trailingSlash: RuntimeValue;
  actions: { name: string; callable: boolean }[];
}
export interface RouteMetadata {
  id: string;
  config: RuntimeValue;
  prerender: RuntimeValue;
  entries: RuntimeValue;
  methods: string[];
  pageMethods: string[];
  endpointMethods: string[];
  page: PageMetadata | null;
  endpoint: { trailingSlash: RuntimeValue } | null;
}
export interface AnalyzedMetadata extends LoadedNodes { routes: RouteMetadata[] }
/** Exact compiler inventory, in transform order; no paths are read by this API. */
export interface RemoteChunk { hash: string; file: string }
export interface RemoteExportMetadata {
  name: string;
  kind: RuntimeValue;
  /** Prerender dynamic may be undefined; other kinds yield true. */
  dynamic: RuntimeValue;
}
export interface RemoteModuleMetadata extends RemoteChunk { exports: RemoteExportMetadata[] }
export interface AnalyzedRemotes { serverBuildId: string; remotes: RemoteModuleMetadata[] }
/** Omit the whole object for defaults; supplied objects require every field. */
export interface PrerenderLimits {
  assetBytes: number;
  totalAssetBytes: number;
  assetFiles: number;
  responseBytes: number;
  visitBytes: number;
  dependencies: number;
  visits: number;
}
export interface PrerenderInput {
  /** A fresh rendering realm; this cannot reuse a metadata session. */
  bootstrap: BootstrapRequest;
  serverEntry: string;
  /** Explicit resolved origin, such as https://example.test. */
  origin: string;
  /** Empty or slash-prefixed path without a trailing slash. */
  base: string;
  appDir: string;
  /** Static-file identities mapped to byte arrays or base64 strings. Server assets live in bootstrap.assets. */
  staticAssets: Record<string, number[] | string>;
  limits?: PrerenderLimits;
}
export interface CreatePrerenderOptions {
  input: PrerenderInput;
  fetchOptions?: FetchOptions;
  cryptoOptions?: CryptoOptions;
}
/** HTTP ByteString bytes, preserving duplicate headers; not UTF-8 strings. */
export type PrerenderHeaders = [name: number[], value: number[]][];
export interface PrerenderResponseHead {
  status: number;
  statusText: number[];
  headers: PrerenderHeaders;
  /** Main route ID is decoded at inspection; dependency heads report null. */
  routeId: string | null;
}
export interface PrerenderResponse extends PrerenderResponseHead {
  body: number[];
}
export interface PrerenderDependency {
  /** Exact dependency-map key, before URL pathname normalization. */
  path: string;
  encodedPath: string;
  /** decodeURI semantics preserve reserved escapes such as %2F. */
  decodedPath: string;
  response: PrerenderResponse;
}
export interface PrerenderVisitHead {
  /** Native session-local token. Do not reuse after nextDependency returns null. */
  token: number;
  serverBuildId: string;
  encodedPath: string;
  decodedPath: string;
  response: PrerenderResponseHead;
}
export interface PrerenderDependencyHead {
  path: string;
  encodedPath: string;
  decodedPath: string;
  response: PrerenderResponseHead;
}
export interface PrerenderVisitResult {
  serverBuildId: string;
  encodedPath: string;
  decodedPath: string;
  response: PrerenderResponse;
  dependencies: PrerenderDependency[];
}
export interface Capabilities {
  engine: string;
  framework: string;
  persistent: boolean;
  initializationProfiles: string[];
  maxSessions: number;
  nodeLoads: boolean;
  routeMetadata: boolean;
  remoteMetadata: boolean;
  prerenderVisits: boolean;
  stagedPrerenderVisits: boolean;
  prerender: boolean;
  networkTransport: boolean;
  implicitFilesystem: boolean;
  [key: string]: unknown;
}
export interface RuntimeErrorDetail { code: string; message: string; [key: string]: unknown }
export type RuntimeResponse<T = unknown> =
  | { ok: true; result: T }
  | { ok: false; error: RuntimeErrorDetail };
export type RuntimeRequest =
  | { op: 'capabilities' }
  | ({ op: 'create' } & CreateOptions)
  | { op: 'loadNodes'; session: string }
  | { op: 'analyzeMetadata'; session: string; options: MetadataOptions }
  | { op: 'analyzeRemotes'; session: string; chunks: RemoteChunk[] }
  | ({ op: 'createPrerender' } & CreatePrerenderOptions)
  | { op: 'startPrerender'; session: string }
  | { op: 'visitPrerender'; session: string; path: string }
  | { op: 'beginPrerenderVisit'; session: string; path: string }
  | { op: 'readPrerenderMain'; session: string; token: number }
  | { op: 'nextPrerenderDependency'; session: string; token: number }
  | { op: 'readPrerenderDependency'; session: string; token: number }
  | { op: 'publishPrerender'; session: string; file: string; bytes: number[] }
  | { op: 'dispose'; session: string };
export interface RuntimeBinding { svelte_runtime_json(request: string): string }
export type WasmInput = string | URL | Request | Response | BufferSource | WebAssembly.Module;
export interface RuntimeBindings extends RuntimeBinding {
  default(options?: WasmInput | Promise<WasmInput> | { module_or_path: WasmInput | Promise<WasmInput> }): Promise<unknown>;
}
export class SvelteRuntimeError extends Error {
  readonly code: string;
  readonly detail: RuntimeErrorDetail;
  constructor(detail: RuntimeErrorDetail);
}
export class SvelteRuntime {
  constructor(bindings: RuntimeBinding);
  request(request: { op: 'capabilities' }): RuntimeResponse<Capabilities>;
  request(request: { op: 'create' } & CreateOptions): RuntimeResponse<CreatedSession>;
  request(request: { op: 'loadNodes'; session: string }): RuntimeResponse<LoadedNodes>;
  request(request: { op: 'analyzeMetadata'; session: string; options: MetadataOptions }): RuntimeResponse<AnalyzedMetadata>;
  request(request: { op: 'analyzeRemotes'; session: string; chunks: RemoteChunk[] }): RuntimeResponse<AnalyzedRemotes>;
  request(request: { op: 'createPrerender' } & CreatePrerenderOptions): RuntimeResponse<CreatedSession>;
  request(request: { op: 'startPrerender'; session: string }): RuntimeResponse<{ started: true }>;
  request(request: { op: 'visitPrerender'; session: string; path: string }): RuntimeResponse<PrerenderVisitResult>;
  request(request: { op: 'beginPrerenderVisit'; session: string; path: string }): RuntimeResponse<PrerenderVisitHead>;
  request(request: { op: 'readPrerenderMain'; session: string; token: number }): RuntimeResponse<PrerenderResponse>;
  request(request: { op: 'nextPrerenderDependency'; session: string; token: number }): RuntimeResponse<PrerenderDependencyHead | null>;
  request(request: { op: 'readPrerenderDependency'; session: string; token: number }): RuntimeResponse<PrerenderDependency>;
  request(request: { op: 'publishPrerender'; session: string; file: string; bytes: number[] }): RuntimeResponse<{ published: true }>;
  request(request: { op: 'dispose'; session: string }): RuntimeResponse<{ disposed: boolean }>;
  /** Extension point for operations supplied by newer native services. */
  request<T = unknown>(request: { op: string; [key: string]: unknown }): RuntimeResponse<T>;
  capabilities(): Capabilities;
  create(options: CreateOptions): CreatedSession;
  loadNodes(session: string): LoadedNodes;
  analyzeMetadata(session: string, options: MetadataOptions): AnalyzedMetadata;
  /** Requires successful analyzeMetadata first; repeated identical calls reuse native metadata. */
  analyzeRemotes(session: string, chunks: RemoteChunk[]): AnalyzedRemotes;
  /** Configure only; does not invoke application init or visit pages. */
  createPrerender(options: CreatePrerenderOptions): CreatedSession;
  /** Invoke application init once. Repeated successful starts are idempotent. */
  startPrerender(session: string): { started: true };
  /** Requires start. Execution/input failures can poison the owned rendering session. */
  visitPrerender(session: string, path: string): PrerenderVisitResult;
  /** Requires start and no active visit. Returns headers without consuming the body. */
  beginPrerenderVisit(session: string, path: string): PrerenderVisitHead;
  /** Read once, before inspecting dependencies. */
  readPrerenderMain(session: string, token: number): PrerenderResponse;
  /** Read each inspected dependency before advancing. Null completes the visit. */
  nextPrerenderDependency(session: string, token: number): PrerenderDependencyHead | null;
  /** Invokes the dependency body getter here; headers may change during the read. */
  readPrerenderDependency(session: string, token: number): PrerenderDependency;
  /** No auto-save: publish accepted artifact bytes explicitly, including between stages. */
  publishPrerender(session: string, file: string, bytes: number[]): { published: true };
  /** Disposes either metadata or rendering sessions; both share the live-session limit. */
  dispose(session: string): { disposed: boolean };
}
export interface InitializeOptions {
  /** wasm-bindgen input: URL, Request, Response, buffer, module or promised input. */
  moduleOrPath?: WasmInput | Promise<WasmInput>;
  bindings?: RuntimeBindings;
}
export function initializeRuntime(options?: InitializeOptions): Promise<SvelteRuntime>;
