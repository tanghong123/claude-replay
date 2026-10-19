import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { setTimeout as delay } from 'node:timers/promises';

/** Poll observable state, retaining the most recent operation failure. */
export async function eventually(read, predicate = Boolean, timeout = 20_000) {
  const deadline = Date.now() + timeout;
  let latest;
  let failure;
  while (Date.now() < deadline) {
    try {
      latest = await read();
      if (predicate(latest)) return latest;
    } catch (error) { failure = error; }
    await delay(50);
  }
  throw new Error(`Observable state did not converge: ${failure?.message ?? JSON.stringify(latest)}`);
}

export async function freePort() {
  const listener = createServer();
  await new Promise((resolve, reject) => listener.listen(0, '127.0.0.1', resolve).on('error', reject));
  const port = listener.address().port;
  await new Promise(resolve => listener.close(resolve));
  return port;
}

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
 * Socket constructors come from the installed Playwright runtime; no package
 * installation or production App/server modification is needed.
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

function event(type, fields) {
  return `event: ${type}\ndata: ${JSON.stringify({ type, ...fields })}\n\n`;
}

/** The real installed Codex runtime calls this deterministic Responses provider. */
export async function createModelFixture(project) {
  const requests = [];
  const failures = [];
  let scenario = 'text';
  let scenarioCount = 0;
  let nextCompletionHold = null;
  let nextOutputAddedHold = null;
  let nextDeltaHolds = [];
  const completionHolds = new Set();
  const outputAddedHolds = new Set();
  const deltaHolds = new Set();
  const sockets = new Set();
  const server = createServer(async (request, response) => {
    if (request.method !== 'POST' || !request.url.endsWith('/responses')) {
      response.writeHead(404).end();
      return;
    }
    try {
      const chunks = [];
      for await (const chunk of request) chunks.push(chunk);
      const body = JSON.parse(Buffer.concat(chunks).toString());
      const index = requests.length + 1;
      const currentScenario = scenario;
      const count = ++scenarioCount;
      const completionHold = nextCompletionHold;
      nextCompletionHold = null;
      const outputAddedHold = nextOutputAddedHold;
      nextOutputAddedHold = null;
      const responseDeltaHolds = nextDeltaHolds;
      nextDeltaHolds = [];
      requests.push({ index, scenario: currentScenario, model: body.model,
        effort: body.reasoning?.effort, serviceTier: body.service_tier, input: body.input });
      const id = `workspace-e2e-response-${index}`;
      response.writeHead(200, { 'Content-Type': 'text/event-stream', Connection: 'close' });
      response.write(event('response.created', { response: { id } }));
      if (['approval', 'question', 'tool', 'read-tool'].includes(currentScenario) && count === 1) {
        let name;
        let argumentsValue;
        if (currentScenario === 'approval') {
          name = 'request_permissions';
          argumentsValue = { reason: 'Disposable workspace E2E permission fixture.',
            permissions: { file_system: { write: [project] } } };
        } else if (currentScenario === 'question') {
          name = 'request_user_input';
          argumentsValue = { questions: [{ id: 'fixture-choice', header: 'Fixture',
            question: 'Choose the local fixture result.', options: [
              { label: 'First', description: 'Choose the first fixture.' },
              { label: 'Second', description: 'Choose the second fixture.' },
            ] }] };
        } else if (currentScenario === 'read-tool') {
          name = 'exec_command';
          argumentsValue = { cmd: 'cat browser-proof.txt', workdir: project,
            yield_time_ms: 1000, max_output_tokens: 1000 };
        } else {
          name = 'workspace_fixture_tool';
          argumentsValue = { value: index };
        }
        response.write(event('response.output_item.done', { item: {
          type: 'function_call', call_id: `workspace-e2e-call-${index}`, name,
          arguments: JSON.stringify(argumentsValue),
        } }));
      } else {
        // Two distinct identical messages detect semantic deduplication without
        // inferring an identity mapping between native legacy and live items.
        const messageCount = currentScenario === 'text-twins' ? 2 : 1;
        let emittedDeltaCount = 0;
        for (let messageIndex = 0; messageIndex < messageCount; messageIndex++) {
          const messageId = `workspace-e2e-message-${index}${messageCount > 1 ? `-${messageIndex + 1}` : ''}`;
          const text = currentScenario === 'text-twins'
            ? 'Workspace fixture identical legacy response.'
            : `Workspace fixture response ${index}: ${currentScenario}.`;
          response.write(event('response.output_item.added', { output_index: messageIndex, item: {
            type: 'message', id: messageId, role: 'assistant', status: 'in_progress', content: [],
          } }));
          if (outputAddedHold && messageIndex === 0 && !response.destroyed) {
            outputAddedHold.enter({ index, itemId: messageId });
            await Promise.race([outputAddedHold.released,
              new Promise(resolve => response.once('close', resolve))]);
          }
          response.write(event('response.content_part.added', { item_id: messageId,
            output_index: messageIndex, content_index: 0, part: { type: 'output_text', text: '' } }));
          const chunks = text.match(/.{1,8}/g);
          for (const chunk of chunks) {
            if (response.destroyed) break;
            response.write(event('response.output_text.delta', { item_id: messageId,
              output_index: messageIndex, content_index: 0, delta: chunk }));
            emittedDeltaCount++;
            const deltaHold = responseDeltaHolds.find(hold => hold.afterDelta === emittedDeltaCount);
            if (deltaHold) {
              deltaHold.enter({ index, itemId: messageId, delta: chunk, deltaOrdinal: emittedDeltaCount });
              await Promise.race([deltaHold.released,
                new Promise(resolve => response.once('close', resolve))]);
            }
            await delay(currentScenario === 'slow' ? 700 : 35);
          }
          if (currentScenario === 'slow' && !response.destroyed) {
            // Leave an explicit active-turn window after readable deltas. UI tests
            // must interrupt a running turn, rather than race response completion.
            await Promise.race([delay(8_000, undefined, { ref: false }),
              new Promise(resolve => response.once('close', resolve))]);
          }
          if (!response.destroyed) response.write(event('response.output_item.done', { item: {
            type: 'message', role: 'assistant', id: messageId,
            content: [{ type: 'output_text', text }],
          } }));
        }
      }
      // Expose the real native item/completed -> turn/completed window without
      // starting another model task or relying on scheduler timing.
      if (completionHold && !response.destroyed) {
        completionHold.enter({ index });
        await Promise.race([completionHold.released,
          new Promise(resolve => response.once('close', resolve))]);
      }
      if (!response.destroyed) response.end(event('response.completed', { response: { id,
        usage: { input_tokens: 0, input_tokens_details: null, output_tokens: 0,
          output_tokens_details: null, total_tokens: 0 },
      } }));
    } catch (error) {
      failures.push(error.message);
      if (!response.headersSent) response.writeHead(500);
      response.end();
    }
  });
  server.on('connection', socket => { sockets.add(socket); socket.once('close', () => sockets.delete(socket)); });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  return {
    port: server.address().port, requests, failures,
    scenario(value) { scenario = value; scenarioCount = 0; },
    holdNextOutputAdded() {
      if (nextOutputAddedHold) throw new Error('A fixture output item hold is already armed.');
      let enter, release;
      const entered = new Promise(resolve => { enter = resolve; });
      const released = new Promise(resolve => { release = resolve; });
      const hold = { enter, released, release() { release(); outputAddedHolds.delete(hold); } };
      outputAddedHolds.add(hold);
      nextOutputAddedHold = hold;
      return { entered, release: () => hold.release() };
    },
    holdNextTextDelta(afterDelta) {
      if (!Number.isSafeInteger(afterDelta) || afterDelta < 1)
        throw new Error('A fixture text delta hold requires a positive delta ordinal.');
      if (nextDeltaHolds.some(hold => hold.afterDelta === afterDelta))
        throw new Error(`A fixture text delta hold is already armed for ordinal ${afterDelta}.`);
      let enter, release;
      const entered = new Promise(resolve => { enter = resolve; });
      const released = new Promise(resolve => { release = resolve; });
      const hold = { afterDelta, enter, released, release() { release(); deltaHolds.delete(hold); } };
      deltaHolds.add(hold);
      nextDeltaHolds.push(hold);
      return { entered, release: () => hold.release() };
    },
    holdNextCompletion() {
      if (nextCompletionHold) throw new Error('A fixture completion hold is already armed.');
      let enter, release;
      const entered = new Promise(resolve => { enter = resolve; });
      const released = new Promise(resolve => { release = resolve; });
      const hold = { enter, released, release() { release(); completionHolds.delete(hold); } };
      completionHolds.add(hold);
      nextCompletionHold = hold;
      return { entered, release: () => hold.release() };
    },
    async close() {
      for (const hold of completionHolds) hold.release();
      for (const hold of outputAddedHolds) hold.release();
      for (const hold of deltaHolds) hold.release();
      for (const socket of sockets) socket.destroy();
      await new Promise(resolve => server.close(resolve));
    },
  };
}

/** A second initialized native client proves shared state independently of HTTP. */
export class NativeClient {
  constructor(socket, name) {
    this.socket = socket;
    this.name = name;
    this.messages = [];
    this.pending = new Map();
    this.nextId = 1;
    socket.addEventListener('message', ({ data }) => {
      const message = JSON.parse(data);
      this.messages.push(message);
      const waiting = this.pending.get(message.id);
      if (waiting && ('result' in message || 'error' in message)) {
        this.pending.delete(message.id);
        clearTimeout(waiting.timer);
        waiting.resolve(message);
      }
    });
    socket.addEventListener('close', () => {
      for (const waiting of this.pending.values()) {
        clearTimeout(waiting.timer);
        waiting.reject(new Error(`${this.name}: native socket closed`));
      }
      this.pending.clear();
    });
  }
  static async connect(url, name) {
    const socket = new WebSocket(url);
    await new Promise((resolve, reject) => {
      socket.addEventListener('open', resolve, { once: true });
      socket.addEventListener('error', reject, { once: true });
    });
    const client = new NativeClient(socket, name);
    await client.result('initialize', { clientInfo: { name, title: name, version: '0.0.0' },
      capabilities: { experimentalApi: true } });
    client.send({ method: 'initialized' });
    return client;
  }
  send(message) { this.socket.send(JSON.stringify(message)); }
  call(method, params) {
    const id = `${this.name}-${this.nextId++}`;
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => { this.pending.delete(id); reject(new Error(`${method} timeout`)); }, 20_000);
      this.pending.set(id, { resolve, reject, timer });
      this.send({ id, method, params });
    });
  }
  async result(method, params) {
    const response = await this.call(method, params);
    if (response.error) throw new Error(`${method}: ${JSON.stringify(response.error)}`);
    return response.result;
  }
  wait(predicate, from = 0, timeout = 20_000) {
    return eventually(() => this.messages.slice(from).find(predicate), Boolean, timeout);
  }
  close() { this.socket.close(); }
}

export const inputText = text => [{ type: 'text', text, textElements: [] }];

export function assertModelRequest(request, model, effort) {
  assert.equal(request.model, model);
  assert.equal(request.effort, effort);
}
