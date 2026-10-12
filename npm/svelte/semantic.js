// Persistent Rust semantics. This module runs inside a worker; it never reads a
// native filesystem. Browser/Node entrypoints own loading and worker creation.
export const MAX_FRAME = 64 * 1024 * 1024;
const encoder = new TextEncoder();
const decoder = new TextDecoder('utf-8', { fatal: true });

export class SemanticError extends Error {
  constructor(code, message, details = {}) {
    super(message);
    this.name = 'SemanticError';
    this.code = code;
    Object.assign(this, details);
  }
}

export async function compileSemanticModule(source) {
  if (source instanceof WebAssembly.Module) return source;
  if (source instanceof ArrayBuffer || ArrayBuffer.isView(source)) return WebAssembly.compile(source);
  const response = typeof Response !== 'undefined' && source instanceof Response ? source : await fetch(source);
  if (!response.ok) throw new SemanticError('load_failed', `Cannot load semantic WASM: ${response.status}`);
  return WebAssembly.compile(await response.arrayBuffer());
}

function jsonCopy(value) {
  const text = JSON.stringify(value);
  if (text === undefined) throw new SemanticError('invalid_request', 'Value must be JSON serializable');
  if (encoder.encode(text).length > MAX_FRAME) throw new SemanticError('frame_too_large', 'Request exceeds 64 MiB');
  return JSON.parse(text);
}

function importsFor(module, output) {
  let memory;
  let stderr = '';
  const bytes = (ptr, len) => {
    ptr >>>= 0; len >>>= 0;
    if (!memory || ptr + len > memory.buffer.byteLength) throw new SemanticError('invalid_memory', 'WASI memory access out of bounds');
    return new Uint8Array(memory.buffer, ptr, len);
  };
  const view = (ptr, len) => new DataView(bytes(ptr, len).buffer, ptr >>> 0, len >>> 0);
  const emptySizes = (count, size) => { view(count, 4).setUint32(0, 0, true); view(size, 4).setUint32(0, 0, true); return 0; };
  const unsupported = name => () => { throw new SemanticError('unsupported_host_operation', `Semantic host does not support WASI ${name}`); };
  const wasi = {
    args_sizes_get: emptySizes, args_get: () => 0,
    environ_sizes_get: emptySizes, environ_get: () => 0,
    clock_time_get(id, _precision, ptr) {
      if (id !== 0 && id !== 1) return 28;
      const ms = id === 0 ? performance.timeOrigin + performance.now() : performance.now();
      view(ptr, 8).setBigUint64(0, BigInt(Math.round(ms * 1e6)), true);
      return 0;
    },
    clock_res_get(id, ptr) { if (id !== 0 && id !== 1) return 28; view(ptr, 8).setBigUint64(0, 1000n, true); return 0; },
    random_get(ptr, len) {
      const target = bytes(ptr, len);
      for (let at = 0; at < target.length; at += 65536) crypto.getRandomValues(target.subarray(at, at + 65536));
      return 0;
    },
    fd_write(fd, iovs, count, written) {
      if (fd !== 1 && fd !== 2) return 8;
      if (count >>> 0 > MAX_FRAME / 8) throw new SemanticError('invalid_memory', 'Too many WASI output vectors');
      const table = view(iovs, (count >>> 0) * 8);
      let total = 0;
      for (let i = 0; i < count; i++) {
        const ptr = table.getUint32(i * 8, true), len = table.getUint32(i * 8 + 4, true);
        if ((total += len) > MAX_FRAME) throw new SemanticError('frame_too_large', 'WASI output exceeds 64 MiB');
        const chunk = bytes(ptr, len).slice();
        if (fd === 2) stderr = (stderr + new TextDecoder().decode(chunk)).slice(-65536);
        output?.(fd, chunk);
      }
      view(written, 4).setUint32(0, total, true);
      return 0;
    },
    fd_fdstat_get(fd, ptr) { if (fd > 2) return 8; bytes(ptr, 24).fill(0); return 0; },
    fd_prestat_get: () => 8,
    proc_exit(code) { throw new SemanticError('wasi_exit', `Semantic WASM exited with status ${code}`, { exitCode: code }); },
    sched_yield: () => 0,
    poll_oneoff: unsupported('poll_oneoff'),
  };
  // These may remain linked from unreachable native paths. They fail explicitly
  // if called, rather than pretending that a filesystem or scheduler exists.
  const denied = new Set(['fd_read', 'fd_close', 'fd_seek', 'fd_tell', 'fd_sync', 'fd_datasync',
    'fd_filestat_get', 'fd_filestat_set_size', 'fd_filestat_set_times', 'fd_fdstat_set_flags',
    'fd_fdstat_set_rights', 'fd_prestat_dir_name', 'fd_readdir', 'fd_advise', 'fd_allocate',
    'path_open', 'path_filestat_get', 'path_filestat_set_times', 'path_readlink', 'path_rename',
    'path_create_directory', 'path_remove_directory', 'path_unlink_file', 'path_symlink', 'path_link',
    'sock_accept', 'sock_recv', 'sock_send', 'sock_shutdown', 'proc_raise']);
  const imports = { wasi_snapshot_preview1: {} };
  for (const item of WebAssembly.Module.imports(module)) {
    if (item.module !== 'wasi_snapshot_preview1' || item.kind !== 'function' || !(item.name in wasi) && !denied.has(item.name)) {
      throw new SemanticError('unsupported_import', `Unsupported WASM import ${item.module}.${item.name} (${item.kind})`);
    }
    imports.wasi_snapshot_preview1[item.name] = wasi[item.name] ?? unsupported(item.name);
  }
  return { imports, attach(value) { memory = value; }, stderr: () => stderr };
}

function applyChanges(text, changes) {
  const offset = position => {
    let line = 0, at = 0;
    while (line < position.line && at < text.length) {
      const c = text[at++];
      if (c === '\r') { if (text[at] === '\n') at++; line++; }
      else if (c === '\n') line++;
    }
    return at + position.character;
  };
  for (const change of changes) {
    if (change.range) text = text.slice(0, offset(change.range.start)) + change.text + text.slice(offset(change.range.end));
    else text = change.text;
  }
  return text;
}

const documentKey = uri => decodeURIComponent(new URL(uri).pathname);

/** Create an isolated low-level host. Initialize explicitly before requests. */
export async function createSemanticHost(source, options = {}) {
  const module = await compileSemanticModule(source);
  const wasi = importsFor(module, options.onOutput);
  const instance = await WebAssembly.instantiate(module, wasi.imports);
  const exports = instance.exports;
  for (const name of ['semantic_input', 'semantic_request', 'semantic_output', 'semantic_output_len']) {
    if (typeof exports[name] !== 'function') throw new SemanticError('invalid_module', `Missing export ${name}`);
  }
  if (!(exports.memory instanceof WebAssembly.Memory)) throw new SemanticError('invalid_module', 'Missing exported memory');
  wasi.attach(exports.memory);
  exports._initialize?.();
  let session, generation, nextId = 0, closed = false, queue = Promise.resolve();
  let checkpoint, uncertain = false;
  const terminal = new Set(['closed', 'engine_panic', 'unported', 'exhausted']);
  const mutations = new Set(['initialize', 'open', 'change', 'close', 'files', 'configure', 'watchedFiles']);
  const serialized = fn => {
    const result = queue.then(fn);
    queue = result.catch(() => {});
    return result;
  };
  const record = (op, params) => {
    if (op === 'initialize') {
      checkpoint = { format: 1, initialize: jsonCopy(params), documents: [], preferences: null };
      uncertain = false;
    }
    else if (op === 'files') {
      for (const [path, text] of Object.entries(params.files)) {
        if (text === null) delete checkpoint.initialize.files[path];
        else Object.defineProperty(checkpoint.initialize.files, path, { value: text, enumerable: true, writable: true, configurable: true });
      }
    } else if (op === 'configure') checkpoint.preferences = jsonCopy(params);
    else if (op === 'open') checkpoint.documents.push(jsonCopy(params.textDocument));
    else if (op === 'close') checkpoint.documents = checkpoint.documents.filter(d => documentKey(d.uri) !== documentKey(params.textDocument.uri));
    else if (op === 'change') {
      const document = checkpoint.documents.find(d => documentKey(d.uri) === documentKey(params.textDocument.uri));
      document.text = applyChanges(document.text, params.contentChanges);
      document.version = params.textDocument.version;
    }
  };
  const call = (op, params) => serialized(() => {
    if (closed) throw new SemanticError('closed', 'Semantic host is closed or poisoned');
    if (op !== 'initialize' && session === undefined) throw new SemanticError('not_initialized', 'Initialize the semantic host first');
    const id = ++nextId;
    const envelope = { id, op, params };
    if (op !== 'initialize') Object.assign(envelope, { session, generation });
    const bytes = encoder.encode(JSON.stringify(envelope));
    if (bytes.length > MAX_FRAME) throw new SemanticError('frame_too_large', 'Request exceeds 64 MiB');
    let reply;
    try {
      const ptr = exports.semantic_input(bytes.length) >>> 0;
      if (!ptr || ptr + bytes.length > exports.memory.buffer.byteLength) throw new SemanticError('invalid_abi', 'Input allocation rejected or out of bounds');
      new Uint8Array(exports.memory.buffer).set(bytes, ptr);
      if (exports.semantic_request() !== 0) throw new SemanticError('invalid_abi', 'Semantic request was rejected by the ABI');
      const out = exports.semantic_output() >>> 0, length = exports.semantic_output_len() >>> 0;
      if (!out || length > MAX_FRAME || out + length > exports.memory.buffer.byteLength) throw new SemanticError('invalid_abi', 'Invalid semantic output range');
      reply = JSON.parse(decoder.decode(new Uint8Array(exports.memory.buffer, out, length).slice()));
      if (reply.id !== id || typeof reply.ok !== 'boolean' || !Number.isInteger(reply.session) || !Number.isInteger(reply.generation)
          || reply.session < 0 || reply.session > 0xffffffff || reply.generation < 0 || reply.generation > 0xffffffff
          || session !== undefined && reply.session !== session || generation !== undefined && reply.generation < generation) {
        throw new SemanticError('invalid_reply', 'Semantic response identity or generation mismatch');
      }
      generation = reply.generation;
      if (reply.session !== 0) session = reply.session;
      if (reply.ok) record(op, params);
      else if (mutations.has(op)) uncertain = true;
      if (terminal.has(reply.error?.code) || reply.error?.code === 'frame_too_large' || op === 'shutdown' && reply.ok) closed = true;
    } catch (error) {
      closed = true;
      throw new SemanticError(error.code ?? 'engine_trap', error.message ?? String(error), { stderr: wasi.stderr(), cause: error, fatal: true });
    }
    return reply;
  });
  return {
    initialize(params) { try { return call('initialize', jsonCopy(params)); } catch (e) { return Promise.reject(e); } },
    request(op, params = {}) {
      if (typeof op !== 'string' || op === 'initialize') return Promise.reject(new SemanticError('invalid_request', 'Use initialize() once; operation must be a string'));
      try { return call(op, jsonCopy(params)); } catch (e) { return Promise.reject(e); }
    },
    snapshot() { return serialized(() => {
      if (!checkpoint || uncertain || closed) throw new SemanticError('snapshot_unavailable', 'No certain live checkpoint is available');
      return jsonCopy(checkpoint);
    }); },
    shutdown() { return call('shutdown', {}); },
    dispose() { closed = true; checkpoint = undefined; },
  };
}

/** Replay inputs into a fresh host. This does not restore serialized checker memory. */
export async function restoreSemanticHost(source, snapshot, options = {}) {
  const copy = jsonCopy(snapshot);
  if (copy.format !== 1 || !copy.initialize || !Array.isArray(copy.documents)) throw new SemanticError('invalid_snapshot', 'Unsupported semantic snapshot');
  const host = await createSemanticHost(source, options);
  try {
    const check = reply => { if (!reply.ok) throw new SemanticError(reply.error.code, reply.error.message, { reply }); };
    check(await host.initialize(copy.initialize));
    if (copy.preferences !== null) check(await host.request('configure', copy.preferences));
    for (const document of copy.documents) check(await host.request('open', { textDocument: document }));
    return host;
  } catch (error) { host.dispose(); throw error; }
}
