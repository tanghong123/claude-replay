import assert from 'node:assert/strict';

/** Retain only scoped completed native command fields; absent output is null. */
export function commandEvidence(items, project) {
  assert(Array.isArray(items), 'Native command evidence must be an array.');
  return items.map(item => {
    assert.equal(item.cwd, project, 'Native command evidence belongs to the isolated project.');
    assert.equal(item.status, 'completed');
    assert.equal(item.exitCode, 0);
    assert(typeof item.command === 'string' && item.command.length <= 8192,
      'Native command evidence has a bounded command string.');
    assert(!/auth\.json|config\.toml|CODEX_HOME|OPENAI_API_KEY|\bprintenv\b/.test(item.command),
      'Native file-task proof must not include authentication/configuration commands.');
    assert(item.aggregatedOutput === null || (typeof item.aggregatedOutput === 'string'
      && item.aggregatedOutput.length <= 8192),
    'Native file-task output must be null or a bounded string.');
    return { id: item.id, status: item.status, command: item.command, cwd: item.cwd,
      exitCode: item.exitCode, aggregatedOutput: item.aggregatedOutput };
  });
}
