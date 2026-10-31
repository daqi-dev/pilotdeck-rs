// #1145 Adversarial input controls for the capture contract. No source mutation,
// no HEAD guard bypass, and no claim to cover the future Rust signal behavior.
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { validate } from './contract.mjs';
import { cleanupAll } from './cleanup.mjs';

assert.equal(process.version, 'v22.22.0', '#1145 negative Node version');
const here = dirname(fileURLToPath(import.meta.url));
const source = JSON.parse(readFileSync(join(here, 'source.json'), 'utf8'));
const definitions = JSON.parse(readFileSync(join(here, 'cases.json'), 'utf8'));
const first = data => data.captures[0];
const find = (data, predicate) => {
  const row = data.captures.find(predicate);
  assert.ok(row, '#1145 negative control has a real captured target');
  return row;
};
const timeout = data => find(data, row => row.status === 'OBSERVED_TIMEOUT');
const signal = data => find(data, row => row.input.signal);
const second = data => find(data, row => row.input.second);
const terminal = data => find(data, row => row.input.stage === 'terminal');
function dropTrace(row, predicate) {
  const removed = row.trace.findIndex(predicate);
  assert.ok(removed >= 0, '#1145 negative trace anchor exists');
  for (const field of ['trace', 'hooks', 'wire', 'closes']) {
    row[field] = row[field].filter(event => event.seq !== removed)
      .map(event => ({ ...event, seq: event.seq > removed ? event.seq - 1 : event.seq }));
  }
}
function swapTrace(row, left, right) {
  const a = row.trace.findIndex(left), b = row.trace.findIndex(right);
  assert.ok(a >= 0 && b >= 0 && a !== b, '#1145 distinct ordering anchors exist');
  for (const field of ['trace', 'hooks', 'wire', 'closes']) {
    row[field] = row[field].map(event => ({ ...event, seq: event.seq === a ? b : event.seq === b ? a : event.seq }))
      .sort((x, y) => x.seq - y.seq);
  }
}
const mutations = [
  ['matrix-and-source-truncated', (data, cases) => { data.captures.pop(); cases.pop(); }, 'fixed registered matrix denominator'],
  ['active-denominator', (_data, cases) => { cases[0].stage = 'stdin'; }, 'fixed active matrix denominator'],
  ['node-version', data => { data.nodeVersion = 'v0'; }, 'source Node version'],
  ['baseline-commit', data => { data.sourceCommit = 'wrong'; }, 'source baseline commit'],
  ['capture-scope', data => { data.kind = 'wire-double'; }, 'real CLI capture scope'],
  ['observation-window', data => { data.observationMs = 0; }, 'bounded observation window'],
  ['cleanup-window', data => { data.cleanupMs = 0; }, 'bounded cleanup window'],
  ['failed-cell-hidden', data => { data.failures.push({ id: 'missing' }); }, 'no omitted failed cells'],
  ['source-identity-missing', data => { delete data.sourceSha256['src/cli/sdkCli.ts']; }, 'exact producer identity fields'],
  ['producer-identity-missing', data => { delete data.probeSha256['probe.mjs']; }, 'exact producer identity fields'],
  ['producer-identity-malformed', data => { data.probeSha256['probe.mjs'] = 'bad'; }, 'producer SHA256 format'],
  ['matrix-row-missing', data => { data.captures.pop(); }, 'complete matrix cardinality'],
  ['matrix-identity', data => { first(data).input.id = 'wrong'; }, 'exact matrix identities'],
  ['matrix-input', data => { first(data).input.action = 'wrong'; }, 'captured input matches registered cell'],
  ['client-output-disabled', data => { first(data).input.argv = ['run']; }, 'client consumption output enabled'],
  ['harness-error', data => { first(data).error = 'bad'; }, 'no harness error accepted'],
  ['unreached-is-exit', data => { first(data).status = 'TRIGGER_NOT_REACHED'; }, 'reached valid observation'],
  ['milestone-missing', data => { first(data).missingMilestones.push('second-signal'); }, 'all required milestones reached'],
  ['disk-change', data => { first(data).final.unexpected = {}; }, 'runtime files unchanged'],
  ['socket-left-active', data => { first(data).activeAfterExit = 1; }, 'no remaining observed sockets after process exit'],
  ['close-missing', data => { first(data).closes.pop(); }, 'every connection has a close observation'],
  ['close-wrong-identity', data => { first(data).closes[0].connection = 42; }, 'distinct connection closure identities'],
  ['trace-order', data => { first(data).trace[0].seq = 42; }, 'ordered trace sequence'],
  ['projection-forged', data => { first(data).hooks[0].hook.kind = 'forged'; }, 'evidence projections preserve original trace'],
  ['observer-missing', data => { first(data).hooks = []; }, 'client observer initialized'],
  ['timeout-as-natural-exit', data => { timeout(data).naturalExit = { code: 0 }; }, 'natural exit bound to original trace'],
  ['cleanup-signal-wrong', data => { timeout(data).cleanupExit.signal = 'SIGTERM'; }, 'cleanup exit bound to original trace'],
  ['deadline-missing', data => { timeout(data).trace.find(event => event.kind === 'observation-deadline').kind = 'wrong'; }, 'timeout has deadline evidence'],
  ['natural-exit-missing', data => { first(data).naturalExit = null; }, 'natural exit bound to original trace'],
  ['natural-signal-rewritten', data => { first(data).naturalExit.signal = 'SIGTERM'; }, 'natural exit bound to original trace'],
  ['cleanup-borrowed', data => { first(data).cleanupExit = { code: 0 }; }, 'cleanup exit bound to original trace'],
  ['first-signal-missing', data => { signal(data).signalDelivered = false; }, 'required first signal delivered'],
  ['first-signal-unproven', data => { signal(data).trace.find(event => event.kind === 'signal-sent').delivered = false; }, 'milestones bound to original trace'],
  ['second-signal-missing', data => { second(data).secondDelivered = false; }, 'required second signal delivered'],
  ['second-signal-unproven', data => { second(data).trace.find(event => event.kind === 'second-signal-sent').delivered = false; }, 'milestones bound to original trace'],
  ['wire-sent-is-not-consumed', data => { terminal(data).hooks = terminal(data).hooks.filter(event => event.hook.kind !== 'terminal-consumed'); }, 'terminal consumed before signal, not merely wire sent'],
  ['stdin-gap-omitted', data => { data.gaps.pop(); }, 'fixed suspended matrix denominator'],
  ['stdin-gap-wrong-identity', data => { data.gaps[0].id = 'wrong'; }, 'exact explicitly suspended stdin gaps'],
  ['stdin-gap-as-pass', data => { data.gaps[0].status = 'PASS'; }, 'gap keeps its original observations'],
  ['stdin-gap-without-node-reference', data => { data.gaps[0].reason = 'unexplained'; }, 'gap keeps its original observations'],
  ['stdin-gap-wrong-attempt', data => { data.gaps[0].attempts[0].capture.input.id = 'wrong'; }, 'gap keeps its original observations'],
  ['stdin-gap-without-attempt', data => { data.gaps[0].attempts = []; }, 'gap keeps its original observations'],
  ['stdin-gap-without-evidence', data => { data.gaps[0].evidenceIds = []; }, 'each suspended cell cites actual IO failure evidence'],
  ['stdin-gap-false-evidence', data => { data.gaps[0].evidenceIds = ['invented']; }, 'each suspended cell cites actual IO failure evidence'],
  ['stdin-IO-evidence-missing', data => { for (const gap of data.gaps) for (const attempt of gap.attempts) attempt.capture.stderrRaw = ''; }, 'stdin suspension has actual IO error evidence'],
  ['abort-enter-missing', data => { dropTrace(find(data, row => row.input.action === 'release'), event => event.kind === 'client-hook' && event.hook.kind === 'abort-enter'); }, 'milestones bound to original trace'],
  ['predecessor-action-missing', data => { dropTrace(find(data, row => row.input.action === 'release'), event => event.kind === 'predecessor-action'); }, 'milestones bound to original trace'],
  ['abort-terminal-consumption-missing', data => { dropTrace(find(data, row => row.input.stage === 'abort-terminal'), event => event.kind === 'client-hook' && event.hook.kind === 'terminal-consumed'); }, 'milestones bound to original trace'],
  ['late-abort-action-missing', data => { dropTrace(find(data, row => row.input.stage === 'abort-terminal'), event => event.kind === 'late-abort-handled'); }, 'milestones bound to original trace'],
  ['repeat-before-abort', data => { swapTrace(second(data), event => event.kind === 'second-signal-sent', event => event.kind === 'wire-receive' && event.frame?.method === 'abort_turn'); }, 'repeat follows received abort RPC'],
  ['terminal-before-abort', data => { swapTrace(find(data, row => row.input.stage === 'abort-terminal'), event => event.kind === 'client-hook' && event.hook.kind === 'terminal-consumed', event => event.kind === 'wire-receive' && event.frame?.method === 'abort_turn'); }, 'milestones bound to original trace'],
  ['signal-before-consumed', data => { swapTrace(terminal(data), event => event.kind === 'child-exit', event => event.kind === 'client-hook' && event.hook.kind === 'terminal-consumed'); }, 'terminal consumed before signal, not merely wire sent'],
  ['duplicate-exit', data => { const row = first(data); row.trace.push({ ...row.trace.find(event => event.kind === 'child-exit'), seq: row.trace.length }); }, 'exactly one original child exit'],
  ['consistent-wrong-cleanup-signal', data => { const row = timeout(data); row.cleanupExit.signal = 'SIGTERM'; row.trace.find(event => event.kind === 'child-exit').signal = 'SIGTERM'; }, 'timeout cleanup is separate'],
  ['signal-before-trigger', data => { swapTrace(signal(data), event => event.kind === 'signal-sent', event => event.kind === 'trigger-reached'); }, 'trigger precedes first signal'],
  ['registered-listeners-missing', data => { dropTrace(find(data, row => row.input.stage === 'stream'), event => event.kind === 'client-hook' && event.hook.kind === 'listener-installed' && event.hook.listeners.SIGINT === 1 && event.hook.listeners.SIGTERM === 1); }, 'both listeners installed before registered trigger'],
  ['trigger-before-listeners', data => { swapTrace(find(data, row => row.input.stage === 'query-hello'), event => event.kind === 'trigger-reached', event => event.kind === 'client-hook' && event.hook.kind === 'listener-installed' && event.hook.listeners.SIGINT === 1 && event.hook.listeners.SIGTERM === 1); }, 'both listeners installed before registered trigger'],
  ['predecessor-before-abort', data => { swapTrace(find(data, row => row.input.action === 'release'), event => event.kind === 'predecessor-action', event => event.kind === 'client-hook' && event.hook.kind === 'abort-enter'); }, 'predecessor action follows signal and abort entry'],
  ['endpoint-denial-unproven', data => { dropTrace(first(data), event => event.kind === 'client-hook' && event.hook.kind === 'endpoint-denial-verified'); }, 'client observer initialized'],
  ['natural-exit-tagged-cleanup', data => { const row = first(data); const event = row.trace.find(event => event.kind === 'child-exit'); event.cleanup = true; row.naturalExit = null; row.cleanupExit = { code: event.code, signal: event.signal, cleanup: true }; }, 'natural exit observed independently'],
  ['timeout-tagged-natural', data => { const row = timeout(data); const event = row.trace.find(event => event.kind === 'child-exit'); event.cleanup = false; row.cleanupExit = null; row.naturalExit = { code: event.code, signal: event.signal, cleanup: false }; }, 'timeout is not a natural exit'],
];

validate(source, definitions);
const results = [];
for (const [name, mutate, assertion] of mutations) {
  let status = 'INVALID', reason;
  try {
    const changed = structuredClone(source);
    const changedDefinitions = structuredClone(definitions);
    mutate(changed, changedDefinitions);
    try {
      validate(changed, changedDefinitions);
      status = 'SURVIVED'; reason = 'corrupted observation accepted';
    } catch (error) {
      status = error.code === 'ERR_ASSERTION' && error.message.includes(`#1145 ${assertion}`) ? 'KILLED' : 'INVALID';
      reason = status === 'KILLED' ? `#1145 ${assertion}` : String(error);
    }
    validate(source, definitions);
  } catch (error) { status = 'INVALID'; reason = String(error); }
  const row = { name, status, reason };
  results.push(row); console.log(JSON.stringify(row));
}
// Inject failures into real cleanup control flow, without source mutation or
// subprocess signals. Every step must still run and both errors must survive.
try {
  const visited = [], firstError = new Error('first'), lastError = new Error('last');
  let caught;
  try {
    await cleanupAll([
      () => { visited.push('child'); throw firstError; },
      () => { visited.push('stdin'); },
      async () => { visited.push('server'); throw lastError; },
    ]);
  } catch (error) { caught = error; }
  assert.deepEqual(visited, ['child', 'stdin', 'server'], '#1145 cleanup visits every owned resource after failure');
  assert.ok(caught instanceof AggregateError && caught.errors[0] === firstError && caught.errors[1] === lastError,
    '#1145 cleanup reports every injected failure');
  results.push({ name: 'cleanup-step-errors', status: 'KILLED', reason: '#1145 all steps visited and injected failures rejected' });
} catch (error) {
  results.push({ name: 'cleanup-step-errors', status: 'INVALID', reason: String(error) });
}
console.log(JSON.stringify(results.at(-1)));
const counts = Object.fromEntries(['KILLED', 'SURVIVED', 'INVALID'].map(status => [status, results.filter(row => row.status === status).length]));
console.log(JSON.stringify({ counts, total: results.length }));
process.exitCode = results.some(row => row.status !== 'KILLED') ? 1 : 0;
