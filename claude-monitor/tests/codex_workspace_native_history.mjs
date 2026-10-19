import assert from 'node:assert/strict';
import { eventually } from './fixtures/codex-workspace/runtime.mjs';

/** Assert one active reply retains canonical native IDs across real recovery. */
export async function assertActiveCanonicalHistory({ page, native, readGateway, threadId, text }) {
  const canonical = await eventually(
    () => native.result('thread/read', { threadId, includeTurns: true }),
    value => value.thread.turns.some(turn => turn.status === 'inProgress'
      && turn.items.some(item => item.type === 'agentMessage' && item.text === text)),
  );
  assert.equal(canonical.thread.historyMode, 'paginated');
  const turn = canonical.thread.turns.find(value => value.status === 'inProgress');
  const expectedIds = turn.items.map(item => item.id);
  assert.equal(turn.items.filter(item => item.type === 'userMessage').length, 1);
  assert.equal(turn.items.filter(item => item.type === 'agentMessage').length, 1);
  const nativeResume = await native.result('thread/resume', { threadId });
  const resumedTurn = nativeResume.thread.turns.find(value => value.id === turn.id);
  assert(resumedTurn, 'The observer resumes the same held active turn.');
  const snapshots = [];
  for (let index = 0; index < 3; index++) {
    const value = await readGateway(threadId);
    const actual = value.thread.turns.find(value => value.id === turn.id);
    assert(actual, 'Gateway recovery retains the original active turn.');
    assert.equal(actual.status, 'inProgress');
    assert.deepEqual(actual.items.map(item => item.id), expectedIds,
      'Gateway recovery uses canonical native identities instead of mixing active resume aliases.');
    snapshots.push({ turnId: actual.id, itemIds: actual.items.map(item => item.id) });
  }
  await page.reload();
  await page.getByText(text, { exact: true }).waitFor();
  const visible = await page.evaluate(({ text, expectedIds }) => {
    const replies = [...document.querySelectorAll('#transcript p')]
      .filter(element => element.textContent === text);
    const interrupt = document.querySelector('[data-testid="interrupt-turn"]');
    return { replies: replies.map(element => element.closest('[id]')?.id),
      canonicalRoots: expectedIds.map(id => [...document.querySelectorAll('#transcript [id]')]
        .filter(element => element.id === id).length),
      canInterrupt: interrupt !== null && !interrupt.disabled && !interrupt.hidden
        && interrupt.getClientRects().length > 0 };
  }, { text, expectedIds });
  assert.deepEqual(visible.replies, [turn.items.find(item => item.type === 'agentMessage').id]);
  assert(visible.canonicalRoots.every(count => count === 1), 'Both original native user and reply identities render exactly once.');
  assert.equal(visible.canInterrupt, true, 'Recovery retains the active Stop control.');
  return { nativeThreadId: threadId, nativeTurnId: turn.id, canonicalItemIds: expectedIds,
    nativeResumeItemIds: resumedTurn.items.map(item => item.id), snapshots, visible,
    checks: { activeHistoryUsesCanonicalIds: true, activeRepeatedRecoveryRendersOnce: true,
      activeCanonicalHistorySurvivesBrowserReload: true } };
}

const partialJoinNote = 'Joined while Codex was replying. The complete message appears when this reply finishes.';
const legacyCadenceNotice = 'Older task: messages refresh as Codex saves them.';

async function entered(hold, operation) {
  let timer;
  try {
    return await Promise.race([hold.entered, new Promise((_, reject) => {
      timer = setTimeout(() => reject(new Error(`${operation} did not reach its held fixture window.`)), 60_000);
    })]);
  } finally { clearTimeout(timer); }
}

const legacyItemIdentities = thread => thread.turns.map(turn => ({
  turnId: turn.id, status: turn.status, itemIds: turn.items.map(item => item.id),
}));

async function legacyDomFacts(page, canonical, provisionalId = null, coreId = null) {
  const expected = canonical.turns.flatMap(turn => turn.items);
  const userTexts = [...new Set(expected.filter(item => item.type === 'userMessage')
    .map(item => item.content.map(content => content.text ?? '').join('')))];
  const replyTexts = [...new Set(expected.filter(item => item.type === 'agentMessage').map(item => item.text))];
  await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
  return page.evaluate(({ expectedIds, userTexts, replyTexts, provisionalId, coreId,
    partialJoinNote, legacyCadenceNotice }) => {
    const transcript = document.querySelector('#transcript');
    const roots = [...transcript.querySelectorAll('[id]')];
    const paragraphs = [...transcript.querySelectorAll('p')];
    const users = [...transcript.querySelectorAll('.user-prompt .body')];
    const runtimeStatus = document.querySelector('#runtime-status');
    const interrupt = document.querySelector('[data-testid="interrupt-turn"]');
    return {
      rootIds: roots.map(element => element.id),
      canonicalRootCounts: expectedIds.map(id => ({ id, count: roots.filter(element => element.id === id).length })),
      users: userTexts.map(text => ({ text, count: users.filter(element => element.textContent === text).length })),
      replies: replyTexts.map(text => ({ text, count: paragraphs.filter(element => element.textContent === text).length })),
      provisionalRootCount: provisionalId === null ? 0 : roots.filter(element => element.id === provisionalId).length,
      provisionalText: provisionalId === null ? null : document.getElementById(provisionalId)?.textContent,
      coreRootCount: coreId === null ? 0 : roots.filter(element => element.id === coreId).length,
      partialNoteCount: paragraphs.filter(element => element.textContent === partialJoinNote).length,
      runtimeStatus: runtimeStatus?.textContent ?? '',
      legacyCadenceNoticeVisible: (runtimeStatus?.getClientRects().length ?? 0) > 0
        && runtimeStatus.textContent.includes(legacyCadenceNotice),
      canInterrupt: interrupt !== null && !interrupt.disabled && !interrupt.hidden
        && interrupt.getClientRects().length > 0,
      error: document.querySelector('#workspace-error')?.textContent ?? '',
    };
  }, { expectedIds: expected.map(item => item.id), userTexts, replyTexts, provisionalId, coreId,
    partialJoinNote, legacyCadenceNotice });
}

function assertLegacyDom(facts, canonical) {
  const items = canonical.turns.flatMap(turn => turn.items);
  assert.deepEqual(facts.rootIds, items.map(item => item.id),
    'The legacy transcript renders every native full-read identity once, in native history order.');
  assert(facts.canonicalRootCounts.every(value => value.count === 1));
  for (const value of facts.users) assert.equal(value.count,
    items.filter(item => item.type === 'userMessage'
      && item.content.map(content => content.text ?? '').join('') === value.text).length,
    'Separate user items with the same text remain separate.');
  for (const value of facts.replies) assert.equal(value.count,
    items.filter(item => item.type === 'agentMessage' && item.text === value.text).length,
    'Separate agent items with the same text remain separate.');
  assert.equal(facts.provisionalRootCount, 0);
  assert.equal(facts.coreRootCount, 0);
  assert.equal(facts.partialNoteCount, 0);
  assert.equal(facts.error, '');
}

function legacyDomMatches(facts, canonical) {
  const items = canonical.turns.flatMap(turn => turn.items);
  return JSON.stringify(facts.rootIds) === JSON.stringify(items.map(item => item.id))
    && facts.canonicalRootCounts.every(value => value.count === 1)
    && facts.users.every(value => value.count === items.filter(item => item.type === 'userMessage'
      && item.content.map(content => content.text ?? '').join('') === value.text).length)
    && facts.replies.every(value => value.count === items.filter(item => item.type === 'agentMessage'
      && item.text === value.text).length)
    && facts.provisionalRootCount === 0 && facts.coreRootCount === 0
    && facts.partialNoteCount === 0 && facts.error === '';
}

/**
 * Exercise the installed runtime's incompatible legacy identity domains. Full
 * thread/read aliases exclusively own the visible transcript. Actual unfinished
 * Core IDs remain separate wire diagnostics; persistence and completion events
 * have no identity barrier, so those diagnostics cannot safely become legacy
 * transcript rows. No text-based identity inference or response normalization
 * is permitted by these assertions.
 */
export async function assertLegacyActiveHistory({ page, native, fixture, project, readGateway,
  navigate, restartGateway, screenshot }) {
  const created = await native.result('thread/start', { cwd: project, historyMode: 'legacy',
    model: 'gpt-6.1-sol', config: { model_reasoning_effort: 'low' } });
  const threadId = created.thread.id;
  assert.equal(created.thread.historyMode, 'legacy');
  const phases = [];
  const observerFrom = native.messages.length;
  let previousCompleted = [];
  for (let index = 1; index <= 3; index++) {
    fixture.scenario(index === 3 ? 'text-twins' : 'text');
    const phase = { index, snapshots: [], checks: {} };
    phases.push(phase);
    const from = native.messages.length;
    const completionHold = fixture.holdNextCompletion();
    const outputAddedHold = index === 1 ? fixture.holdNextOutputAdded() : null;
    const firstDeltaHold = index < 3 ? fixture.holdNextTextDelta(1) : null;
    const secondDeltaHold = index < 3 ? fixture.holdNextTextDelta(2) : null;
    try {
      const requestIndex = fixture.requests.length + 1;
      const text = index === 3 ? 'Workspace fixture identical legacy response.'
        : `Workspace fixture response ${requestIndex}: text.`;
      phase.prompt = index < 3 ? 'E2E: Repeated native legacy request remains separate.'
        : 'E2E: Native legacy identical replies retain separate identities.';
      phase.text = text;
      phase.nativeTurnStart = await native.result('turn/start', {
        threadId, input: [{ type: 'text', text: phase.prompt, textElements: [] }],
      });
      const turnId = phase.nativeTurnStart.turn.id;
      phase.nativeTurnId = turnId;
      if (outputAddedHold) {
        // The installed runtime creates a legacy rollout lazily when its first
        // real turn starts. Subscribe after that native history exists and
        // before the first text delta, without writing any rollout ourselves.
        phase.beforeFirstDelta = await entered(outputAddedHold, 'Legacy first output item');
        phase.nativeBeforeFirstDelta = await eventually(
          () => native.result('thread/read', { threadId, includeTurns: true }), value =>
            value.thread.turns.find(turn => turn.id === turnId)?.items
              .some(item => item.type === 'userMessage'));
        phase.gatewayBeforeFirstDelta = await readGateway(threadId);
        await navigate(threadId);
        await eventually(() => page.locator('#transcript .user-prompt .body').allTextContents(),
          values => values.filter(text => text === phase.prompt).length === 1);
        await eventually(() => page.getByTestId('composer-input').isEnabled());
        outputAddedHold.release();
      }
      if (firstDeltaHold) {
        const firstDelta = await entered(firstDeltaHold, `Legacy phase ${index} first text delta`);
        const nativeDelta = await native.wait(message => message.method === 'item/agentMessage/delta'
          && message.params.threadId === threadId && message.params.turnId === turnId
          && message.params.itemId === firstDelta.itemId && message.params.delta === firstDelta.delta, from);
        phase.firstDelta = { fixture: firstDelta, native: nativeDelta };
        const partialRead = await native.result('thread/read', { threadId, includeTurns: true });
        const partialTurn = partialRead.thread.turns.find(turn => turn.id === turnId);
        assert.equal(partialRead.thread.historyMode, 'legacy');
        assert.equal(partialTurn.status, 'inProgress');
        assert.equal(partialTurn.items.filter(item => item.type === 'agentMessage').length, 0,
          'The real legacy full read has not persisted an unfinished reply.');
        phase.partialNativeRead = partialRead;
        phase.partialGateway = await eventually(() => readGateway(threadId), value =>
          value.provisionalItems?.some(value => value.turnId === turnId
            && value.item.id === firstDelta.itemId && value.item.text === firstDelta.delta));
        assert.deepEqual(legacyItemIdentities(phase.partialGateway.thread), legacyItemIdentities(partialRead.thread));
        const liveDomId = `legacy-live:${turnId}:${firstDelta.itemId}`;
        phase.partialDom = await eventually(
          () => legacyDomFacts(page, partialRead.thread, liveDomId, firstDelta.itemId),
          value => legacyDomMatches(value, partialRead.thread)
            && value.legacyCadenceNoticeVisible && value.canInterrupt);
        assertLegacyDom(phase.partialDom, partialRead.thread);
        assert.equal(phase.partialDom.canInterrupt, true);
        phase.checks.unfinishedLegacyReplyRetainsActualWireDiagnosticIdentity = true;
        phase.checks.partialLegacyTranscriptShowsOnlySavedAliases = true;
        if (index === 2) {
          // A new Gateway process cannot claim an already-emitted prefix was
          // replayed. Hold the next delta separately so the suffix is observable.
          phase.gatewayReplacement = await restartGateway();
          await page.reload();
          const recovered = await readGateway(threadId);
          assert.deepEqual(legacyItemIdentities(recovered.thread), legacyItemIdentities(partialRead.thread));
          assert.deepEqual(recovered.provisionalItems, []);
          phase.afterRestartBeforeFreshDelta = await eventually(
            () => legacyDomFacts(page, partialRead.thread, liveDomId, firstDelta.itemId), value =>
              legacyDomMatches(value, partialRead.thread) && value.canInterrupt
              && value.legacyCadenceNoticeVisible);
          assertLegacyDom(phase.afterRestartBeforeFreshDelta, partialRead.thread);
        }
        firstDeltaHold.release();
        const secondDelta = await entered(secondDeltaHold, `Legacy phase ${index} second text delta`);
        await native.wait(message => message.method === 'item/agentMessage/delta'
          && message.params.threadId === threadId && message.params.turnId === turnId
          && message.params.itemId === secondDelta.itemId && message.params.delta === secondDelta.delta, from);
        const expectedPrefix = index === 2 ? secondDelta.delta : firstDelta.delta + secondDelta.delta;
        phase.secondDelta = secondDelta;
        phase.partialAfterFreshDelta = await eventually(() => readGateway(threadId), value =>
          value.provisionalItems?.some(value => value.turnId === turnId
            && value.item.id === secondDelta.itemId && value.item.text === expectedPrefix));
        const provisional = phase.partialAfterFreshDelta.provisionalItems.find(value =>
          value.turnId === turnId && value.item.id === secondDelta.itemId);
        if (index === 2) assert.equal(provisional.item.partialFromJoin, true,
          'A new Gateway wire diagnostic explicitly labels a suffix whose original prefix it never received.');
        phase.freshDeltaDom = await eventually(
          () => legacyDomFacts(page, partialRead.thread, liveDomId, secondDelta.itemId),
          value => legacyDomMatches(value, partialRead.thread)
            && value.legacyCadenceNoticeVisible && value.canInterrupt);
        assertLegacyDom(phase.freshDeltaDom, partialRead.thread);
        assert.equal(phase.freshDeltaDom.canInterrupt, true);
        assert(phase.freshDeltaDom.canonicalRootCounts.every(value => value.count === 1));
        if (index === 2) {
          assert.equal(provisional.item.text.includes(firstDelta.delta), false);
          phase.checks.gatewayRestartKeepsMissingPrefixWireDiagnosticTruthful = true;
          await screenshot('legacy-saved-history-during-partial-after-gateway-restart.png');
        }
        secondDeltaHold.release();
      }
      await entered(completionHold, `Legacy phase ${index} item completion`);
      const expectedAgents = index === 3 ? 2 : 1;
      const completedItems = await eventually(() => native.messages.slice(from)
        .filter(message => message.method === 'item/completed' && message.params.threadId === threadId
          && message.params.turnId === turnId && message.params.item.type === 'agentMessage'),
      values => values.length === expectedAgents);
      phase.nativeCompletedLiveItems = completedItems;
      const canonical = await eventually(() => native.result('thread/read', { threadId, includeTurns: true }),
        value => value.thread.turns.find(turn => turn.id === turnId)?.items
          .filter(item => item.type === 'agentMessage').length === expectedAgents);
      assert.equal(canonical.thread.historyMode, 'legacy');
      assert.deepEqual(legacyItemIdentities(canonical.thread).slice(0, index - 1), previousCompleted,
        'Earlier full-read legacy aliases remain stable across continuation.');
      const activeTurn = canonical.thread.turns.find(turn => turn.id === turnId);
      assert.equal(activeTurn.status, 'inProgress');
      assert.equal(activeTurn.items.filter(item => item.type === 'userMessage').length, 1);
      assert(activeTurn.items.filter(item => item.type === 'agentMessage').every(item => item.text === text));
      phase.nativeFullRead = canonical;
      phase.nativeResume = await native.result('thread/resume', { threadId });
      phase.nativeReadAfterResume = await native.result('thread/read', { threadId, includeTurns: true });
      assert.deepEqual(legacyItemIdentities(phase.nativeReadAfterResume.thread), legacyItemIdentities(canonical.thread));
      for (let snapshotIndex = 0; snapshotIndex < 3; snapshotIndex++) {
        const snapshot = await readGateway(threadId);
        phase.snapshots.push(snapshot);
        assert.deepEqual(legacyItemIdentities(snapshot.thread), legacyItemIdentities(canonical.thread),
          'Every raw Gateway recovery uses native full-read aliases, including continuation globals.');
        assert.deepEqual(snapshot.provisionalItems, [], 'Completed Core items cannot remain alongside legacy history.');
      }
      await page.reload();
      phase.completedItemDom = await eventually(() => legacyDomFacts(page, canonical.thread),
        value => legacyDomMatches(value, canonical.thread)
          && value.legacyCadenceNoticeVisible && value.canInterrupt);
      assertLegacyDom(phase.completedItemDom, canonical.thread);
      assert.equal(phase.completedItemDom.canInterrupt, true);
      phase.checks.threeRawRecoveriesMatchNativeLegacyFullRead = true;
      phase.checks.completedLegacyItemsRetireWireDiagnostics = true;
      phase.checks.legacyActiveBrowserReloadRendersEachNativeAliasOnce = true;
      if (index === 3) {
        assert.equal(new Set(activeTurn.items.map(item => item.id)).size, 3);
        assert.equal(new Set(completedItems.map(message => message.params.item.id)).size, 2);
        phase.checks.identicalLegacyAgentMessagesKeepDistinctIdentities = true;
      }
      await screenshot(`legacy-active-canonical-${index}.png`);
      completionHold.release();
      const completed = await native.wait(message => message.method === 'turn/completed'
        && message.params.threadId === threadId && message.params.turn.id === turnId, from);
      assert.equal(completed.params.turn.status, 'completed');
      phase.nativeFinalRead = await eventually(() => native.result('thread/read', { threadId, includeTurns: true }),
        value => value.thread.turns.find(turn => turn.id === turnId)?.status === 'completed');
      previousCompleted = legacyItemIdentities(phase.nativeFinalRead.thread);
      phase.completedTurnGateway = await eventually(() => readGateway(threadId), value =>
        value.thread.turns.find(turn => turn.id === turnId)?.status === 'completed');
      assert.deepEqual(legacyItemIdentities(phase.completedTurnGateway.thread), previousCompleted,
        'A legacy native turn summary cannot overwrite its complete aliased history.');
      assert.deepEqual(phase.completedTurnGateway.provisionalItems, []);
      phase.completedTurnDom = await eventually(() => legacyDomFacts(page, phase.nativeFinalRead.thread),
        value => legacyDomMatches(value, phase.nativeFinalRead.thread)
          && value.legacyCadenceNoticeVisible && !value.canInterrupt);
      assertLegacyDom(phase.completedTurnDom, phase.nativeFinalRead.thread);
      phase.checks.legacyTerminalSummaryPreservesCanonicalHistory = true;
    } finally {
      firstDeltaHold?.release();
      secondDeltaHold?.release();
      outputAddedHold?.release();
      completionHold.release();
    }
  }
  fixture.scenario('text');
  return { nativeThreadId: threadId, nativeThreadStart: created, phases,
    nativeObserverMessages: native.messages.slice(observerFrom),
    checks: {
      legacyHistoricalThreadReadable: true,
      legacyActiveFirstTurnUsesFullNativeRead: true,
      legacyContinuationKeepsGlobalAliases: true,
      legacyPartialWireDiagnosticsStayOutsideTranscript: true,
      legacyGatewayRestartKeepsMissingPrefixWireDiagnostics: true,
      legacyCompletedItemsRetireWireDiagnostics: true,
      legacyIdenticalRepliesRemainSeparate: true,
      legacyTurnSummaryPreservesFullHistory: true,
    } };
}
