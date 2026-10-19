import assert from 'node:assert/strict';
import { test } from 'node:test';
import { commandEvidence } from './fixtures/codex-workspace/evidence.mjs';

const project = '/private/fixture/project';
const command = { id: 'write', status: 'completed', command: "printf 'marker\\n' > marker.txt",
  cwd: project, exitCode: 0, aggregatedOutput: null };

test('Native command proof retains a successful silent write and separate read output', () => {
  const read = { ...command, id: 'read', command: 'cat marker.txt', aggregatedOutput: 'marker\n' };
  assert.deepEqual(JSON.parse(JSON.stringify(commandEvidence([command, read], project))), [command, read]);
  assert.deepEqual(commandEvidence([{ ...command, aggregatedOutput: '' }], project),
    [{ ...command, aggregatedOutput: '' }]);
});

test('Native command proof rejects malformed or unbounded output without inventing it', () => {
  for (const output of [undefined, {}, 0, 'x'.repeat(8193)]) {
    assert.throws(() => commandEvidence([{ ...command, aggregatedOutput: output }], project),
      /null or a bounded string/);
  }
});

test('Native command proof rejects foreign, unfinished, failed and credential-reading commands', () => {
  for (const item of [{ ...command, cwd: '/foreign' }, { ...command, status: 'inProgress' },
    { ...command, exitCode: 1 }, { ...command, command: 'cat auth.json' }]) {
    assert.throws(() => commandEvidence([item], project));
  }
});
