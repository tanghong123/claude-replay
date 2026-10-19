import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { eventually, inputText } from './fixtures/codex-workspace/runtime.mjs';

async function nativeQueue(native, threadId) {
  const entries = [], pages = [], seen = new Set();
  let cursor;
  do {
    const page = await native.result('thread/queue/list', {
      threadId, limit: 25, ...(cursor ? { cursor } : {}),
    });
    assert(Array.isArray(page.data));
    assert(page.data.length <= 25);
    entries.push(...page.data);
    pages.push({ cursor: cursor ?? null, nextCursor: page.nextCursor, entries: page.data });
    cursor = page.nextCursor;
    assert(cursor === null || typeof cursor === 'string' && cursor.length > 0);
    assert(!cursor || !seen.has(cursor), 'Native queue cursors make progress.');
    if (cursor) seen.add(cursor);
  } while (cursor);
  return { entries, pages };
}

/** Exercise genuine Native pagination and the phone's page-two Cancel button. */
export async function assertNativeQueuePagination({ mobile, native, readGateway, threadId }) {
  const initial = await nativeQueue(native, threadId);
  assert.equal(initial.entries.length, 0, 'The isolated held turn starts with an empty queue.');
  const owned = [], prefix = `queue-pages-${randomUUID()}`;
  let result, operationError;
  try {
    for (let index = 0; index < 32; index++) {
      const entry = (await native.result('thread/queue/add', {
        threadId, clientUserMessageId: `${prefix}-${index}`,
        input: inputText(`E2E: Owned queued pagination task ${index}.`),
      })).queuedSubmission;
      assert.equal(entry.clientUserMessageId, `${prefix}-${index}`);
      assert.equal(typeof entry.id, 'string');
      owned.push(entry);
    }
    const before = await nativeQueue(native, threadId);
    assert.deepEqual(before.entries, owned, 'Real Native cursor pages retain every submission in order.');
    assert.equal(before.pages.length, 2);
    assert.equal(before.pages[0].entries.length, 25);
    assert.equal(before.pages[1].entries.length, 7);
    const recovered = await eventually(() => readGateway(threadId), value => value.queue.length === 32);
    assert.deepEqual(recovered.queue, before.entries, 'Gateway snapshot includes the actual second queue page unchanged.');
    const tail = owned.at(-1);
    const button = mobile.locator(`[data-cancel-queue="${tail.clientUserMessageId}"]`);
    await button.waitFor();
    await eventually(() => mobile.locator('#queued-messages [data-cancel-queue]').count(), count => count === 32);
    const displayed = await mobile.locator('#queued-messages [data-cancel-queue]').evaluateAll(
      elements => elements.map(element => element.dataset.cancelQueue));
    assert.deepEqual(displayed, owned.map(entry => entry.clientUserMessageId), 'Phone exposes all native queued entries in order.');
    await button.click();
    const after = await eventually(() => nativeQueue(native, threadId), value => value.entries.length === 31);
    assert.deepEqual(after.entries, owned.slice(0, -1), 'Phone cancellation removes only the exact page-two native submission.');
    const afterGateway = await eventually(() => readGateway(threadId), value => value.queue.length === 31);
    assert.deepEqual(afterGateway.queue, after.entries);
    await eventually(() => mobile.locator('#queued-messages [data-cancel-queue]').count(), count => count === 31);
    result = { nativeThreadId: threadId, beforePages: before.pages,
      gatewayQueue: recovered.queue, displayedClientIds: displayed,
      cancelledNativeSubmissionId: tail.id, cancelledClientId: tail.clientUserMessageId,
      afterPages: after.pages, afterGatewayQueue: afterGateway.queue,
      checks: { realNativeQueuePagination: true, completeQueueSnapshotInNativeOrder: true,
        phoneDisplaysSecondQueuePage: true, phoneCancelsExactSecondPageSubmission: true } };
  } catch (error) {
    operationError = error;
  }
  const cleanupErrors = [];
  for (const entry of owned) {
    try { await native.result('thread/queue/delete', { threadId, queuedSubmissionId: entry.id }); }
    catch (error) { cleanupErrors.push(error); }
  }
  try {
    const final = await nativeQueue(native, threadId);
    assert.deepEqual(final.entries, initial.entries, 'Cleanup removes only test-owned submissions before the held turn proceeds.');
    if (result) result.cleanupQueue = final.entries;
  } catch (error) { cleanupErrors.push(error); }
  if (operationError || cleanupErrors.length) {
    throw new AggregateError([operationError, ...cleanupErrors].filter(Boolean), 'Real Native queue pagination validation or owned cleanup failed');
  }
  return result;
}
