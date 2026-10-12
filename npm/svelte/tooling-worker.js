import initialize, { tooling_request_json } from './generated/ts_svelte_tools.js';
import { createReceiver } from './compiler-worker-runtime.js';

const browser = typeof process === 'undefined' || !process.versions?.node;
const port = browser ? globalThis : (await import('node:worker_threads')).parentPort;
const receive = createReceiver(initialize, tooling_request_json, message => port.postMessage(message));
if (browser) port.addEventListener('message', event => void receive(event.data));
else port.on('message', message => void receive(message));
