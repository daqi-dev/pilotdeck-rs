import { baselineSourceHashes } from '../_shared/baseline.mts';
// Node 22; run with the fixed baseline's tsx loader. See README.md.
import assert from 'node:assert/strict';
import { constants } from 'node:buffer';
import { EventEmitter } from 'node:events';
import { TextDecoder } from 'node:util';
import { createHash } from 'node:crypto';
import { execFileSync, spawnSync } from 'node:child_process';
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { resolve, join } from 'node:path';
import { pathToFileURL, fileURLToPath } from 'node:url';
import { bindBaseline } from '../_shared/generator_guard.mjs';

assert.equal(process.version, 'v22.22.0');
const baseline = resolve(process.env.PDRS_BASELINE!);
const ref = 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';
const git = execFileSync('which', ['git'], { encoding: 'utf8' }).trim();
const binding = bindBaseline({ baseline, commit: ref, gitExecutable: git }).provenance;
const load = (p: string) => import(pathToFileURL(join(baseline, p)).href);
const { NodeShellCommandRunner, createShellOutputDecoder } = await load('src/tool/builtin/bash/commandRunner.ts');
const { CommandHookExecutor } = await load('src/extension/hooks/execution/CommandHookExecutor.ts');
const limit = constants.MAX_STRING_LENGTH;
assert.equal(limit, 0x1fffffe8);
const options = { hook: { type: 'command', command: '' }, hookInput: {}, cwd: '/tmp', timeoutMs: 30_000 };

// Exercise the unchanged TS data listener and real V8 concatenation limit.
// Only the spawn and decoder boundaries are injected: ropes avoid a 512 MiB
// allocation. This is deliberately NOT claimed as a 512 MiB pipe transfer.
async function boundary(stream: string, uncaught = false, hook = false) {
  const child: any = new EventEmitter();
  child.stdout = new EventEmitter(); child.stderr = new EventEmitter();
  child.stdin = Object.assign(new EventEmitter(), { end() {} });
  const decode = TextDecoder.prototype.decode;
  let next = '';
  TextDecoder.prototype.decode = function(input) { return input ? next : ''; };
  const runner = new NodeShellCommandRunner(() => child);
  let settled = false;
  const promise = hook
    ? new CommandHookExecutor({ execute: (r: any) => runner.run(r.command, r) }).execute(options)
    : runner.run('', options);
  promise.then(() => { settled = true; });
  const emit = (which: string, value: string) => { next = value; child[which].emit('data', Buffer.from([1])); };
  const failures: any[] = [];
  try {
    emit(stream, 'x'.repeat(limit - 1));
    if (uncaught) {
      // A genuine asynchronous uncaught exception, isolated by the parent.
      setImmediate(() => emit(stream, 'zz'));
      await new Promise(() => {});
    }
    try { emit(stream, 'zz'); } catch (error: any) {
      failures.push({ name: error.name, message: error.message });
    }
    await Promise.resolve();
    const settledAfterOverflow = settled;
    emit(stream, 'y'); // a smaller subsequent chunk still fits
    emit(stream === 'stdout' ? 'stderr' : 'stdout', 'ok');
    child.emit('close', 0);
    const result = await promise;
    return { stream, failures, settledAfterOverflow, stdoutLength: result.stdout.length,
      stderrLength: result.stderr.length, exitCode: result.exitCode,
      ...(hook ? { outcome: result.outcome } : { timedOut: result.timedOut }),
      keys: Object.keys(result).filter(k => k !== 'durationMs').sort() };
  } finally { TextDecoder.prototype.decode = decode; }
}

if (process.argv[2] === '--uncaught') {
  await boundary(process.argv[3], true);
} else {
  const sources = ['src/extension/hooks/execution/CommandHookExecutor.ts',
    'src/tool/execution-world/ShellPort.ts', 'src/tool/execution-world/SubprocessPort.ts',
    'src/tool/builtin/bash/commandRunner.ts'];
  const sourceSha256 = await baselineSourceHashes(baseline, sources);
  const large = [];
  for (const stream of ['stdout', 'stderr']) {
    const command = `head -c 67108864 /dev/zero${stream === 'stderr' ? ' >&2' : ''}`;
    const result = await new CommandHookExecutor().execute({ ...options, hook: { type: 'command', command } });
    large.push({ stream, bytes: 64 * 1024 * 1024, stdoutLength: result.stdout.length,
      stderrLength: result.stderr.length, sha256: createHash('sha256').update(result[stream]).digest('hex'),
      exitCode: result.exitCode, outcome: result.outcome, output: result.output, keys: Object.keys(result).sort() });
    assert.equal(result[stream].length, 64 * 1024 * 1024);
    assert.equal(result.outcome, 'success');
  }
  const boundaries = [];
  for (const stream of ['stdout', 'stderr']) boundaries.push(await boundary(stream));
  // stderr avoids parsing/flattening a 512 MiB stdout rope in parseHookOutput.
  const hookCatch = await boundary('stderr', false, true);
  const uncaught = [];
  for (const stream of ['stdout', 'stderr']) {
    const run = spawnSync(process.execPath, [...process.execArgv, fileURLToPath(import.meta.url), '--uncaught', stream],
      { encoding: 'utf8', timeout: 15_000, maxBuffer: 64 * 1024 });
    assert.equal(run.error, undefined);
    assert.equal(run.status, 1);
    assert.match(run.stderr, /RangeError: Invalid string length/);
    assert.match(run.stderr, /commandRunner.ts/);
    uncaught.push({ stream, status: run.status, signal: run.signal, error: 'RangeError: Invalid string length' });
    process.stderr.write(`uncaught ${stream}:\n${run.stderr}\n`);
  }
  const decoding = [['efbb', 'bff09f', '988061'], ['61f0']].map(chunks => {
    const decoder = createShellOutputDecoder();
    return { chunks, decoded: chunks.map(chunk => decoder.decode(Buffer.from(chunk, 'hex'))), tail: decoder.flush() };
  });
  const oracle = { sourceCommit: binding.ref, node: process.version, maxStringLength: limit, sourceSha256,
    large, boundaries, hookCatch, uncaught, decoding };
  mkdirSync(process.argv[2], { recursive: true });
  writeFileSync(join(process.argv[2], 'oracle.json'), JSON.stringify(oracle, null, 2) + '\n', { flag: 'wx' });
  console.log(JSON.stringify(oracle));
}
