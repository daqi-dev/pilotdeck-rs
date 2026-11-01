// Validate observation provenance, not a desired Rust/TS lifecycle outcome.
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';

// Identity strings only become provenance after checking the actual inputs.
// Keep this separate from validate(): data mutations must still reach their
// specifically registered structural assertion before disk identity is checked.
export function validateFiles(source, directory, baseline) {
  for (const [name, digest] of Object.entries(source.probeSha256)) {
    assert.equal(createHash('sha256').update(readFileSync(join(directory, name))).digest('hex'),
      digest, `#1145 producer file SHA256 matches disk: ${name}`);
  }
  assert.ok(baseline, '#1145 PDRS_BASELINE is required for TS file verification');
  for (const [name, digest] of Object.entries(source.sourceSha256)) {
    assert.equal(createHash('sha256').update(readFileSync(join(baseline, name))).digest('hex'),
      digest, `#1145 TS file SHA256 matches disk: ${name}`);
  }
}

export const DATA_MUTATIONS = 62;
export const EXTRA_CONTROLS = 11;
export const TOTAL_CONTROLS = DATA_MUTATIONS + EXTRA_CONTROLS;
export function validateControlCount(mutations, results) {
  assert.equal(mutations.length, DATA_MUTATIONS, '#1145 fixed data mutation denominator');
  if (results !== undefined) {
    assert.equal(results.length, TOTAL_CONTROLS, '#1145 fixed negative result denominator');
  }
}

function milestonesFromTrace(row) {
  const input = row.input;
  const hook = kind => row.trace.some(event => event.kind === 'client-hook' && event.hook.kind === kind);
  const firstSignal = row.trace.some(event => event.kind === 'signal-sent' && event.delivered && event.signal === input.signal);
  const secondSignal = row.trace.some(event => event.kind === 'second-signal-sent' && event.delivered && event.signal === input.second);
  const abort = row.trace.find(event => event.kind === 'wire-receive' && event.frame?.method === 'abort_turn');
  const terminal = row.trace.find(event => event.kind === 'client-hook' && event.hook.kind === 'terminal-consumed');
  return {
    observer: hook('hooks-ready'), trigger: row.trace.some(event => event.kind === 'trigger-reached'),
    firstSignal: !input.signal || firstSignal,
    abortEntered: !input.signal || ['control', 'stdin'].includes(input.stage) || hook('abort-enter'),
    secondSignal: !input.second || secondSignal,
    predecessorAction: !['release', 'reject'].includes(input.action) || row.trace.some(event => event.kind === 'predecessor-action' && event.action === input.action && event.stage === input.stage),
    terminalConsumed: input.stage !== 'terminal' || hook('terminal-consumed'),
    terminalAfterAbort: input.stage !== 'abort-terminal' || Boolean(abort && terminal && abort.seq < terminal.seq),
    lateAbortHandled: input.stage !== 'abort-terminal' || row.trace.some(event => event.kind === 'late-abort-handled'),
    blockingStdin: input.stage !== 'stdin' || !/(?:EAGAIN|EPERM|EACCES).*?(?:read|open)/.test(row.stderrRaw),
  };
}

export function validate(source, definitions) {
  assert.equal(definitions.length, 102, '#1145 fixed registered matrix denominator');
  assert.equal(source.nodeVersion, 'v22.22.0', '#1145 source Node version');
  assert.equal(source.sourceCommit, 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85', '#1145 source baseline commit');
  assert.equal(source.kind, 'real-ts-cli-real-websocket-observation-hooks', '#1145 real CLI capture scope');
  assert.equal(source.observationMs, 12_000, '#1145 bounded observation window');
  assert.equal(source.cleanupMs, 3_000, '#1145 bounded cleanup window');
  assert.deepEqual(source.failures, [], '#1145 no omitted failed cells');
  const sourceFiles = ['src/cli/sdkCli.ts', 'packages/sdk/src/client.ts', 'packages/sdk/src/transport.ts',
    'dist/src/cli/pilotdeck.js', 'dist/src/cli/sdkCli.js', 'packages/sdk/dist/client.js', 'packages/sdk/dist/transport.js'];
  const probeFiles = ['probe.mjs', 'hooks.mjs', 'cases.json', 'capture.py', 'cleanup.mjs', 'gap-source.json'];
  for (const [record, names] of [[source.sourceSha256, sourceFiles], [source.probeSha256, probeFiles]]) {
    assert.deepEqual(Object.keys(record).sort(), [...names].sort(), '#1145 exact producer identity fields');
    assert.ok(Object.values(record).every(value => /^[a-f0-9]{64}$/.test(value)), '#1145 producer SHA256 format');
  }
  const active = definitions.filter(row => row.stage !== 'stdin');
  assert.equal(active.length, 96, '#1145 fixed active matrix denominator');
  assert.equal(source.gaps.length, 6, '#1145 fixed suspended matrix denominator');
  assert.deepEqual(source.gaps.map(row => row.id), definitions.filter(row => row.stage === 'stdin').map(row => row.id), '#1145 exact explicitly suspended stdin gaps');
  assert.ok(source.gaps.every(gap => gap.status === 'GAP' && gap.reason.includes('#123') && gap.attempts.length > 0
    && gap.attempts.every(attempt => attempt.capture.input.id === gap.id)), '#1145 gap keeps its original observations');
  assert.ok(source.gaps.some(gap => gap.attempts.some(attempt => /EAGAIN.*read/.test(attempt.capture.stderrRaw))), '#1145 stdin suspension has actual IO error evidence');
  const ioGaps = source.gaps.filter(gap => gap.attempts.some(attempt => /EAGAIN.*read/.test(attempt.capture.stderrRaw)));
  assert.ok(source.gaps.every(gap => gap.evidenceIds?.length > 0
    && gap.evidenceIds.every(id => ioGaps.some(evidence => evidence.id === id))),
  '#1145 each suspended cell cites actual IO failure evidence');
  assert.equal(source.captures.length, active.length, '#1145 complete matrix cardinality');
  assert.deepEqual(source.captures.map(row => row.input.id), active.map(row => row.id), '#1145 exact matrix identities');
  for (const [index, row] of source.captures.entries()) {
    const { argv, ...input } = row.input;
    assert.deepEqual(input, active[index], '#1145 captured input matches registered cell');
    assert.ok(argv.includes('--stream-json'), '#1145 client consumption output enabled');
    assert.equal(row.error, null, '#1145 no harness error accepted');
    assert.ok(['OBSERVED_EXIT', 'OBSERVED_TIMEOUT'].includes(row.status), '#1145 reached valid observation');
    assert.deepEqual(row.missingMilestones, [], '#1145 all required milestones reached');
    assert.deepEqual(row.initial, row.final, '#1145 runtime files unchanged');
    assert.equal(row.activeAfterExit, 0, '#1145 no remaining observed sockets after process exit');
    assert.equal(row.closes.length, row.connections, '#1145 every connection has a close observation');
    assert.deepEqual([...new Set(row.closes.map(event => event.connection))].sort(),
      Array.from({ length: row.connections }, (_, i) => i + 1), '#1145 distinct connection closure identities');
    assert.ok(row.trace.every((event, i) => event.seq === i), '#1145 ordered trace sequence');
    for (const event of [...row.hooks, ...row.wire, ...row.closes]) {
      assert.deepEqual(event, row.trace[event.seq], '#1145 evidence projections preserve original trace');
    }
    assert.ok(row.closes.every((event, index) => index === 0 || row.closes[index - 1].seq < event.seq),
      '#1145 closes follow trace order (#1252)');
    const milestones = milestonesFromTrace(row);
    assert.deepEqual(row.milestones, milestones, '#1145 milestones bound to original trace');
    assert.ok(Object.values(milestones).every(Boolean), '#1145 original trace reaches all required milestones');
    const trigger = row.trace.find(event => event.kind === 'trigger-reached');
    const delivered = row.trace.find(event => event.kind === 'signal-sent');
    if (input.signal) {
      assert.ok(trigger.seq < delivered.seq, '#1145 trigger precedes first signal');
    }
    if (['query-hello', 'query-resume', 'query-model', 'stream', 'abort-terminal', 'terminal'].includes(input.stage)) {
      assert.ok(row.hooks.some(event => event.seq < trigger.seq && event.hook.kind === 'listener-installed'
        && event.hook.listeners.SIGINT === 1 && event.hook.listeners.SIGTERM === 1),
      '#1145 both listeners installed before registered trigger');
    }
    if (['release', 'reject'].includes(input.action)) {
      const entered = row.hooks.find(event => event.hook.kind === 'abort-enter');
      const action = row.trace.find(event => event.kind === 'predecessor-action');
      assert.ok(delivered.seq < entered.seq && entered.seq < action.seq,
        '#1145 predecessor action follows signal and abort entry');
    }
    const exits = row.trace.filter(event => event.kind === 'child-exit');
    assert.equal(exits.length, 1, '#1145 exactly one original child exit');
    const { code, signal: exitSignal, cleanup } = exits[0];
    const expectedExit = { code, signal: exitSignal, cleanup };
    assert.deepEqual(row.naturalExit, cleanup ? null : expectedExit, '#1145 natural exit bound to original trace');
    assert.deepEqual(row.cleanupExit, cleanup ? expectedExit : null, '#1145 cleanup exit bound to original trace');
    const hasHook = kind => row.hooks.some(event => event.hook.kind === kind);
    assert.ok(hasHook('hooks-ready') && hasHook('endpoint-denial-verified'), '#1145 client observer initialized');
    if (row.status === 'OBSERVED_TIMEOUT') {
      assert.equal(row.naturalExit, null, '#1145 timeout is not a natural exit');
      assert.ok(row.cleanupExit?.cleanup && row.cleanupExit.signal === 'SIGKILL', '#1145 timeout cleanup is separate');
      assert.ok(row.trace.some(event => event.kind === 'observation-deadline'), '#1145 timeout has deadline evidence');
    } else {
      assert.ok(row.naturalExit && row.naturalExit.cleanup === false, '#1145 natural exit observed independently');
      assert.equal(row.cleanupExit, null, '#1145 natural exit does not borrow cleanup result');
    }
    if (input.signal) {
      assert.ok(row.signalDelivered && row.triggerReached, '#1145 required first signal delivered');
      // Original delivery evidence is already required by milestonesFromTrace.
    }
    if (input.second) {
      assert.ok(row.secondDelivered, '#1145 required second signal delivered');
      const second = row.trace.find(event => event.kind === 'second-signal-sent');
      assert.ok(second?.delivered && row.wire.some(event => event.kind === 'wire-receive'
        && event.frame?.method === 'abort_turn' && event.seq < second.seq), '#1145 repeat follows received abort RPC');
    }
    if (input.stage === 'terminal') {
      const sent = row.trace.find(event => event.kind === 'signal-sent');
      assert.ok(row.hooks.some(event => event.hook.kind === 'terminal-consumed' && event.seq < sent?.seq),
        '#1145 terminal consumed before signal, not merely wire sent');
    }
    // stdin is explicitly suspended, so its IO failures are validated through
    // gap originals above, never through an unreachable active-case assertion.
  }
}
