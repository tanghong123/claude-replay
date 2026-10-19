import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { once } from 'node:events';
import { test } from 'node:test';
import { createDesktopBootstrapState, createDesktopRelay } from './fixtures/codex-workspace/runtime.mjs';

const require = createRequire(import.meta.url);
const { WebSocket, WebSocketServer } = require('ws');

test('Disposable Desktop onboarding bootstrap merges only its exact local atom', () => {
  const before = { 'electron-saved-workspace-roots': ['/private/fixture/project'],
    'electron-persisted-atom-state': { theme: 'dark', 'electron:onboarding-override': 'auto' } };
  const copy = structuredClone(before);
  const after = createDesktopBootstrapState(before);
  assert.deepEqual(after, { ...copy, 'electron-persisted-atom-state': {
    theme: 'dark', 'electron:onboarding-override': 'app',
  } });
  assert.deepEqual(before, copy, 'Existing global state is not mutated while preparing the merge.');
  assert.deepEqual(createDesktopBootstrapState(), {
    'electron-persisted-atom-state': { 'electron:onboarding-override': 'app' },
  });
  assert.throws(() => createDesktopBootstrapState([]), /global state must be a JSON object/);
  assert.throws(() => createDesktopBootstrapState({ 'electron-persisted-atom-state': [] }),
    /persisted atom state must be a JSON object/);
});

test('Desktop relay permits exact text or one App LF and consumes each authorization once', async () => {
  const native = new WebSocketServer({ host: '127.0.0.1', port: 0 });
  await once(native, 'listening');
  const forwarded = [];
  const upstreams = new Set();
  native.on('connection', socket => {
    upstreams.add(socket);
    socket.on('message', raw => {
      const request = JSON.parse(raw.toString());
      forwarded.push(request);
      socket.send(JSON.stringify({ id: request.id, result: { forwarded: true,
        ...(request.method === 'thread/read' ? { thread: { id: request.params.threadId } } : {}) } }));
    });
  });
  const relay = await createDesktopRelay({ nativeUrl: `ws://127.0.0.1:${native.address().port}`,
    WebSocket, WebSocketServer });
  const client = new WebSocket(relay.url);
  await once(client, 'open');
  let id = 0;
  const send = async (method, params) => {
    const response = once(client, 'message');
    client.send(JSON.stringify({ id: ++id, method, params }));
    return JSON.parse((await response)[0].toString());
  };
  try {
    assert.equal((await send('thread/read', { threadId: 'owned-thread' })).result.forwarded, true);
    assert.equal((await send('turn/start', { threadId: 'foreign-thread', input: [
      { type: 'text', text: 'unrequested work' },
    ] })).error.code, -32000);
    relay.authorize('owned-thread', 'E2E: Explicit native App fixture.');
    const input = [{ type: 'text', text: 'E2E: Explicit native App fixture.' }];
    assert.equal((await send('turn/start', { threadId: 'foreign-thread', input })).error.code, -32000);
    assert.equal((await send('thread/queue/add', { threadId: 'owned-thread', input })).result.forwarded, true);
    assert.equal((await send('thread/queue/add', { threadId: 'owned-thread', input })).error.code, -32000);
    assert.equal((await send('thread/realtime/start', { threadId: 'owned-thread' })).error.code, -32000);
    const appText = 'E2E: Native App sends through the shared server.';
    relay.authorize('owned-thread', appText);
    for (const text of [`${appText}\n\n`, `${appText}\r\n`, `${appText} `, ` ${appText}`,
      `${appText}\nPerform another task.`]) {
      assert.equal((await send('turn/start', { threadId: 'owned-thread',
        input: [{ type: 'text', text }] })).error.code, -32000,
      'Unobserved whitespace or appended instructions must not consume authorization.');
    }
    const appInput = [{ type: 'text', text: `${appText}\n` }];
    assert.equal((await send('turn/start', { threadId: 'foreign-thread', input: appInput })).error.code, -32000);
    for (const invalidInput of [[...appInput, { type: 'text', text: 'Another task.' }],
      [{ type: 'image', text: `${appText}\n` }], [null]]) {
      assert.equal((await send('turn/start', { threadId: 'owned-thread',
        input: invalidInput })).error.code, -32000);
    }
    assert.equal((await send('turn/start', { threadId: 'owned-thread', input: appInput })).result.forwarded, true);
    assert.equal((await send('turn/start', { threadId: 'owned-thread', input: appInput })).error.code, -32000);
    assert.deepEqual(forwarded.map(request => request.method), ['thread/read', 'thread/queue/add', 'turn/start']);
    assert.equal(forwarded.at(-1).params.input[0].text, `${appText}\n`,
      'The relay forwards the native App representation without rewriting its input.');
    assert.equal(relay.allowed.length, 2);
    assert.equal(relay.refusals.length, 14);
    assert.deepEqual(relay.reads, [{ method: 'thread/read', threadId: 'owned-thread' }]);
    assert.deepEqual(relay.readResponses, [{ method: 'thread/read', threadId: 'owned-thread',
      ok: true, nativeThreadId: 'owned-thread' }]);
    assert.deepEqual(relay.failures, []);
  } finally {
    client.terminate();
    await relay.close();
    await relay.close();
    for (const socket of upstreams) socket.terminate();
    await new Promise((resolve, reject) => native.close(error => error ? reject(error) : resolve()));
  }
});
