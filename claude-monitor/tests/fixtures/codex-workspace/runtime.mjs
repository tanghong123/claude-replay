import assert from 'node:assert/strict';

/** Seed the installed App's source-verified local onboarding atom for an isolated test home. */
export function createDesktopBootstrapState(globalState = {}) {
  assert(globalState && typeof globalState === 'object' && !Array.isArray(globalState),
    'Disposable Desktop global state must be a JSON object.');
  const atoms = globalState['electron-persisted-atom-state'] ?? {};
  assert(atoms && typeof atoms === 'object' && !Array.isArray(atoms),
    'Disposable Desktop persisted atom state must be a JSON object.');
  return { ...globalState, 'electron-persisted-atom-state': {
    ...atoms, 'electron:onboarding-override': 'app',
  } };
}

/**
 * Test-only relay: Desktop shares the real native server, while inference is
 * refused unless the harness authorizes one exact text on its own thread.
 * Socket constructors come from the declared ws test dependency; no production
 * App/server modification is needed.
 */
export async function createDesktopRelay({ nativeUrl, WebSocket, WebSocketServer }) {
  const listener = new WebSocketServer({ host: '127.0.0.1', port: 0 });
  await new Promise((resolve, reject) => {
    listener.once('listening', resolve);
    listener.once('error', reject);
  });
  const sockets = new Set();
  const refusals = [];
  const allowed = [];
  const failures = [];
  const reads = [];
  const readResponses = [];
  let authorization;
  let closePromise;
  const inferenceMethods = new Set(['turn/start', 'turn/steer', 'thread/queue/add',
    'thread/queue/start', 'thread/startAeon', 'thread/realtime/start',
    'thread/realtime/appendText', 'thread/realtime/appendAudio']);
  listener.on('connection', client => {
    const upstream = new WebSocket(nativeUrl);
    const buffered = [];
    const pendingReads = new Map();
    sockets.add(client);
    sockets.add(upstream);
    client.on('error', error => failures.push({ operation: 'Desktop relay client', cause: error.message }));
    upstream.on('error', error => {
      failures.push({ operation: 'Desktop relay native connection', cause: error.message });
      client.close();
    });
    client.on('message', raw => {
      let message;
      try { message = JSON.parse(raw.toString()); }
      catch (error) {
        failures.push({ operation: 'parse Desktop relay request', cause: error.message });
        client.close(1007, 'Invalid JSON request');
        return;
      }
      if (['thread/read', 'thread/resume'].includes(message.method)) {
        const read = { method: message.method, threadId: message.params?.threadId };
        reads.push(read);
        pendingReads.set(message.id, read);
      }
      if (inferenceMethods.has(message.method)) {
        const params = message.params || {};
        const input = params.input;
        const text = Array.isArray(input) && input.length === 1 && input[0]?.type === 'text'
          ? input[0].text : undefined;
        // The pinned macOS App appends one LF to its composer text. Permit only
        // that observed representation, preserving exact one-shot authorization.
        const matches = authorization && ['turn/start', 'thread/queue/add'].includes(message.method)
          && params.threadId === authorization.threadId && typeof text === 'string'
          && (text === authorization.text || text === `${authorization.text}\n`);
        const metadata = { method: message.method, threadId: params.threadId };
        if (!matches) {
          refusals.push(metadata);
          if (message.id !== undefined) client.send(JSON.stringify({ id: message.id,
            error: { code: -32000, message: 'The disposable Desktop relay refused unrequested inference.' } }));
          return;
        }
        allowed.push(metadata);
        authorization = undefined;
      }
      if (upstream.readyState === WebSocket.OPEN) upstream.send(raw.toString());
      else buffered.push(raw.toString());
    });
    upstream.on('open', () => {
      for (const message of buffered) upstream.send(message);
      buffered.length = 0;
    });
    upstream.on('message', raw => {
      try {
        const message = JSON.parse(raw.toString());
        const read = pendingReads.get(message.id);
        if (read) {
          pendingReads.delete(message.id);
          readResponses.push({ ...read, ok: !message.error,
            nativeThreadId: message.result?.thread?.id });
        }
      } catch (error) {
        failures.push({ operation: 'parse Desktop relay native response', cause: error.message });
      }
      if (client.readyState === WebSocket.OPEN) client.send(raw.toString());
    });
    client.on('close', () => { upstream.close(); sockets.delete(client); });
    upstream.on('close', () => { client.close(); sockets.delete(upstream); });
  });
  return {
    url: `ws://127.0.0.1:${listener.address().port}`, refusals, allowed, failures, reads, readResponses,
    authorize(threadId, text) {
      assert(typeof threadId === 'string' && threadId && typeof text === 'string' && text);
      assert(!authorization, 'Only one pending Desktop inference authorization is allowed.');
      authorization = { threadId, text };
    },
    async close() {
      if (closePromise) return closePromise;
      for (const socket of sockets) socket.terminate();
      closePromise = new Promise((resolve, reject) => listener.close(error => error ? reject(error) : resolve()));
      return closePromise;
    },
  };
}
