const encoder = new TextEncoder();
const decoder = new TextDecoder("utf-8", { fatal: true });
const maxRequestBytes = 16 * 1024 * 1024;

/**
 * Instantiate the portable source engine. Accepts compiled WASM or bytes;
 * loading files/URLs belongs to the host. No Node, WASI or process imports.
 * Each instance is synchronous; use a Worker for large documents in browsers.
 */
export async function createSourceTools(wasm) {
    const module = wasm instanceof WebAssembly.Module ? wasm : await WebAssembly.compile(wasm);
    const instance = await WebAssembly.instantiate(module, {});
    const api = instance.exports;
    if (!(api.memory instanceof WebAssembly.Memory) ||
        ["svelte_input", "svelte_run", "svelte_output", "svelte_output_len", "svelte_reset"]
            .some((name) => typeof api[name] !== "function")) {
        throw new TypeError("Not a ts-rust-svelte source module");
    }
    let disposed = false;
    let busy = false;
    return {
        request(request) {
            if (disposed) throw new Error("Source tools instance is disposed");
            if (busy) throw new Error("Source tools requests cannot be reentrant");
            busy = true;
            let enteredWasm = false;
            try {
                const json = JSON.stringify(request);
                if (json === undefined) throw new TypeError("Request must be JSON serializable");
                const bytes = encoder.encode(json);
                if (bytes.length > maxRequestBytes) throw new RangeError("Request exceeds 16 MiB");
                enteredWasm = true;
                const pointer = api.svelte_input(bytes.length) >>> 0;
                if (!pointer) throw new RangeError("WASM rejected input allocation");
                // Allocation and run can grow memory; never retain views between calls.
                new Uint8Array(api.memory.buffer, pointer, bytes.length).set(bytes);
                api.svelte_run();
                const result = new Uint8Array(api.memory.buffer,
                    api.svelte_output() >>> 0, api.svelte_output_len() >>> 0);
                return JSON.parse(decoder.decode(result));
            } catch (error) {
                // A trap may leave Rust state unusable. Do not reuse this instance.
                if (enteredWasm) disposed = true;
                throw error;
            } finally {
                busy = false;
            }
        },
        /** Release Rust buffers. Drop this object too to release WASM linear memory. */
        dispose() {
            if (busy) throw new Error("Cannot dispose during a request");
            if (!disposed) api.svelte_reset();
            disposed = true;
        },
    };
}
