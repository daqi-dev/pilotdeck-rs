// Fixture-only transport: these rg file searches never consume stdin. The baseline's
// asynchronous port writes an empty pipe after spawn, racing rg's early exit.
// Keep the actual executable, arguments, output and exit status; only remove
// that unused pipe. Product cancellation semantics are outside this fixture.
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';

export function executeFile(request) {
  assert.equal(request.stdin, undefined, '#280 ripgrep fixture must not supply stdin');
  assert.equal(request.signal, undefined, '#280 ripgrep fixture must not supply a cancellation signal');
  const started = Date.now();
  const result = spawnSync(request.executable, request.args, {
    cwd: request.cwd,
    env: request.env,
    encoding: 'utf8',
    stdio: ['ignore', 'pipe', 'pipe'],
    timeout: request.timeoutMs,
    killSignal: 'SIGKILL',
    maxBuffer: 16 * 1024 * 1024,
  });
  if (result.error) throw result.error;
  return {
    exitCode: result.status,
    exitSignal: result.signal,
    stdout: result.stdout,
    stderr: result.stderr,
    timedOut: false,
    durationMs: Date.now() - started,
  };
}
