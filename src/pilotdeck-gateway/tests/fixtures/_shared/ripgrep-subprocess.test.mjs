import assert from 'node:assert/strict';
import { test } from 'node:test';
import { executeFile } from './ripgrep-subprocess.mjs';

const request = (script) => ({ executable: process.execPath, args: ['-e', script], timeoutMs: 5000 });

test('#280 ripgrep subprocess preserves actual results and fails closed', () => {
  for (const status of [0, 1, 2]) {
    const result = executeFile({
      ...request(`process.stdout.write(process.cwd()+'|'+process.env.PDRS_GLOB_TEST); process.stderr.write('diagnostic'); process.exit(${status})`),
      cwd: import.meta.dirname,
      env: { PDRS_GLOB_TEST: 'forwarded' },
    });
    assert.equal(result.exitCode, status, '#280 ripgrep subprocess preserves exit status including no-match');
    assert.equal(result.stdout, `${import.meta.dirname}|forwarded`, '#280 ripgrep subprocess preserves cwd/env/stdout');
    assert.equal(result.stderr, 'diagnostic', '#280 ripgrep subprocess preserves stderr');
    assert.equal(result.timedOut, false, '#280 ripgrep successful child did not time out');
  }
  const early = executeFile(request('process.exit(0)'));
  assert.equal(early.exitCode, 0, '#280 ripgrep early exit has no stdin pipe race');
  const signaled = executeFile(request("process.kill(process.pid, 'SIGTERM')"));
  assert.equal(signaled.exitCode, null, '#280 ripgrep signaled child has no numeric exit status');
  assert.equal(signaled.exitSignal, 'SIGTERM', '#280 ripgrep child exit signal must be preserved');
  assert.throws(() => executeFile({ ...request(''), executable: '/pdrs-280-nonexistent-executable' }),
    { code: 'ENOENT' }, '#280 ripgrep spawn error must propagate');
  assert.throws(() => executeFile({ ...request('setInterval(() => {}, 1000)'), timeoutMs: 50 }),
    { code: 'ETIMEDOUT' }, '#280 ripgrep never-ending child must be killed and timeout propagated');
  assert.throws(() => executeFile({ ...request(''), stdin: 'unconsumed' }),
    /#280 ripgrep fixture must not supply stdin/, '#280 ripgrep stdin cannot silently be ignored');
  for (const signal of [new AbortController().signal, AbortSignal.abort()]) {
    assert.throws(() => executeFile({ ...request(''), signal }),
      /#280 ripgrep fixture must not supply a cancellation signal/, '#280 ripgrep cancellation is outside this transport');
  }
});
