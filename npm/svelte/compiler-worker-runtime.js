/** Internal worker protocol, with a terminal state after every backend failure. */
export function createReceiver(initialize, requestJson, send) {
  let initialized = false;
  let failed = false;
  return async function receive(message) {
    if (failed) return;
    try {
      if (message.module) {
        await initialize({ module_or_path: message.module });
        if (failed) return;
        initialized = true;
        send({ ready: true });
        return;
      }
      if (!initialized) throw new Error('Compiler not initialized');
      send({ id: message.id, response: JSON.parse(requestJson(message.input)) });
    } catch (error) {
      // Retire synchronously: messages already queued must not re-enter WASM.
      failed = true;
      send({ fatal: String(error?.message ?? error) });
    }
  };
}
