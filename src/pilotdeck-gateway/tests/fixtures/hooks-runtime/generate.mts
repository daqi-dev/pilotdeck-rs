// Run from the repository root with Node 22 and the fixed baseline's tsx
// loader; see README.md. Everything below executes the real baseline modules:
// command hooks spawn real scripts in this run's mkdtemp tree, HTTP hooks talk
// to a loopback mock server, and callback hooks are injected.
import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { realpathSync } from 'node:fs';
import { createServer } from 'node:http';
import { tmpdir } from 'node:os';
import { dirname, join, relative, resolve, sep } from 'node:path';
import { pathToFileURL } from 'node:url';
import { createHash } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import assert from 'node:assert/strict';
import { bindSourceTree } from '../../baseline_source_guard.mjs';

assert.ok(process.env.PDRS_BASELINE, 'set PDRS_BASELINE to the fixed TS baseline');
assert.equal(process.version, 'v22.22.0');
assert.equal(process.versions.unicode, '17.0');
// ICU default locale affects collation only, but pin it so a host locale
// change cannot silently alter fixture behavior.
assert.equal(new Intl.Collator().resolvedOptions().locale, 'en-US', 'generate with LANG=en_US.UTF-8');
assert.ok(process.argv[2], 'provide the fixture directory');
const BASELINE_COMMIT = 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';
const baseline = resolve(process.env.PDRS_BASELINE);
const output = resolve(process.argv[2]);

const gitExecutable = realpathSync(
  process.env.PDRS_GIT || execFileSync('which', ['git'], { encoding: 'utf8' }).trim(),
);
const gitEnv = Object.fromEntries(Object.entries(process.env).filter(([key]) => !key.startsWith('GIT_')));
const git = (...args: string[]) =>
  execFileSync(gitExecutable, ['--no-replace-objects', '-C', baseline, ...args], {
    env: gitEnv,
    maxBuffer: 32 * 1024 * 1024,
  });
const sourceCommit = bindSourceTree(baseline, BASELINE_COMMIT, gitExecutable).ref;
assert.equal(git('status', '--porcelain', '--untracked-files=no').toString(), '',
  'baseline has uncommitted changes to tracked files');

// Explicit provenance list: the files whose behavior this oracle calibrates.
// (The shared guard above already verified the complete tracked src tree.)
const sources = [
  'src/extension/hooks/execution/HookRuntime.ts',
  'src/extension/hooks/execution/CommandHookExecutor.ts',
  'src/extension/hooks/execution/HttpHookExecutor.ts',
  'src/extension/hooks/execution/CallbackHookExecutor.ts',
  'src/extension/hooks/execution/PromptHookExecutor.ts',
  'src/extension/hooks/execution/AgentHookExecutor.ts',
  'src/extension/hooks/execution/parseHookOutput.ts',
  'src/extension/hooks/execution/AsyncHookRegistry.ts',
  'src/extension/hooks/events/HookExecutionEventBus.ts',
  'src/extension/hooks/protocol/events.ts',
  'src/extension/hooks/protocol/input.ts',
  'src/extension/hooks/protocol/output.ts',
  'src/extension/hooks/protocol/settings.ts',
  'src/extension/hooks/config/matchHook.ts',
  'src/extension/hooks/config/matchHookCondition.ts',
  'src/lifecycle/runtime/LifecycleRuntime.ts',
  'src/lifecycle/protocol/effects.ts',
  'src/lifecycle/protocol/payloads.ts',
  'src/agent/modules/lifecycle/hostLifecycleRuntime.ts',
  'src/agent/turn/TurnEnvironment.ts',
  'src/tool/execution-world/ShellPort.ts',
  'src/tool/execution-world/SubprocessPort.ts',
  'src/tool/builtin/bash/commandRunner.ts',
  'src/runtime/commandShell.ts',
  'src/agent/modules/transport/sidecarDefaultModuleDispatcher.ts',
];
const sourceSha256: Record<string, string> = {};
for (const path of sources) {
  const committed = git('show', `${BASELINE_COMMIT}:${path}`);
  const working = await readFile(join(baseline, path));
  assert.deepEqual(working, committed, `${path} differs from ${BASELINE_COMMIT}`);
  sourceSha256[path] = createHash('sha256').update(committed).digest('hex');
}

// Pin the one run-varying field HookRuntime puts on a pending async hook.
const RealDate = Date;
const FIXED_MS = RealDate.UTC(2026, 0, 1);
class FixedDate extends RealDate {
  constructor(...args: any[]) {
    super(...((args.length === 0 ? [FIXED_MS] : args) as []));
  }
  static now(): number {
    return FIXED_MS;
  }
}
globalThis.Date = FixedDate as unknown as DateConstructor;

const root = await mkdtemp(join(tmpdir(), 'pdrs-hr-'));
const realRoot = realpathSync(root);
function mask(value: any): any {
  // The realpath (/private/var/...) contains the mkdtemp path as a suffix;
  // mask the longer realpath first so it cannot become "/private<dir>".
  const text = JSON.stringify(value);
  return text.split(realRoot).join('<dir>').split(root).join('<dir>');
}
function json(value: any): string {
  return mask(value);
}
const actual = (value: string) => value.split('<dir>').join(root);
const actualize = (value: any): any =>
  JSON.parse(JSON.stringify(value).split('<dir>').join(root));
const S: Record<string, string> = {};
async function named(key: string, text: string) {
  S[key] = await script(key, text);
}
const load = (path: string) => import(pathToFileURL(join(baseline, path)).href);
const { HookRuntime } = await load('src/extension/hooks/execution/HookRuntime.ts');
const { CommandHookExecutor } = await load('src/extension/hooks/execution/CommandHookExecutor.ts');
const { HttpHookExecutor } = await load('src/extension/hooks/execution/HttpHookExecutor.ts');
const { CallbackHookExecutor } = await load('src/extension/hooks/execution/CallbackHookExecutor.ts');
const { parseHookOutput } = await load('src/extension/hooks/execution/parseHookOutput.ts');
const { HookExecutionEventBus } = await load('src/extension/hooks/events/HookExecutionEventBus.ts');
const { LifecycleRuntime } = await load('src/lifecycle/runtime/LifecycleRuntime.ts');
const { HostLifecycleRuntime } = await load('src/agent/modules/lifecycle/hostLifecycleRuntime.ts');
const { buildTurnEnvironment } = await load('src/agent/turn/TurnEnvironment.ts');

const baseInput = { sessionId: 'session-1', transcriptPath: '', cwd: '/tmp', permissionMode: 'default' };

// ---------------------------------------------------------------- parseHookOutput
const parseCases: any[] = [];
function parse(name: string, stdout: string) {
  parseCases.push({ name, stdout, expected: json(parseHookOutput(stdout)) });
}
parse('empty', '');
parse('spaces', '   \n\t ');
parse('plain', 'hello');
parse('plain-crlf', 'hello\r\nworld');
parse('first-object', '{"a":1}\n{"b":2}');
parse('skips-non-json-lines', 'x\n{"good":1}\n{bad}');
parse('invalid-first-object-stops', 'x\n{bad}\n{"good":1}');
parse('array', '[1,2]');
parse('null', 'null');
parse('string', '"x"');
parse('number', '42');
parse('whitespace-around-object', '  \t {"continue":false} \r\n');
parse('bom-before-object', '\ufeff{"continue":false}');
parse('array-object', '[]\n{"continue":true}');
parse('async-minimal', '{"async":true}');
parse('async-false-sync', '{"async":false,"continue":true}');
parse('async-non-boolean', '{"async":1}');
parse('async-id', '{"async":true,"asyncHookId":"id"}');
parse('async-id-blank', '{"async":true,"asyncHookId":"  "}');
parse('async-id-untrimmed', '{"async":true,"asyncHookId":"  id  "}');
parse('async-id-non-string', '{"async":true,"asyncHookId":5}');
parse('async-timeout-fraction', '{"async":true,"asyncTimeout":0.001}');
parse('async-timeout-one', '{"async":true,"asyncTimeout":1}');
parse('async-timeout-negative', '{"async":true,"asyncTimeout":-2}');
parse('async-timeout-zero', '{"async":true,"asyncTimeout":0}');
parse('async-timeout-infinite', '{"async":true,"asyncTimeout":1e999}');
parse('async-timeout-string', '{"async":true,"asyncTimeout":"3"}');
parse('sync-continue', '{"continue":false,"suppressOutput":true}');
parse('sync-non-boolean', '{"continue":"false","suppressOutput":0}');
parse('sync-fields', '{"stopReason":"stop","decision":"approve","reason":"why","systemMessage":"msg"}');
parse('sync-decision-other', '{"decision":"deny"}');
parse('specific-array', '{"hookSpecificOutput":[]}');
parse('specific-null', '{"hookSpecificOutput":null}');
parse('specific-no-event', '{"hookSpecificOutput":{"additionalContext":"x"}}');
parse('specific-empty-event', '{"hookSpecificOutput":{"hookEventName":""}}');
parse('specific-fields', '{"hookSpecificOutput":{"hookEventName":"Stop","additionalContext":"ctx","initialUserMessage":"ium","watchPaths":["a",1,"b"],"permissionDecision":"deny","permissionDecisionReason":"pdr","updatedInput":{"x":1},"updatedMCPToolOutput":null,"retry":true,"worktreePath":"wt"}}');
parse('specific-watch-all-invalid', '{"hookSpecificOutput":{"hookEventName":"Stop","watchPaths":[1,2]}}');
parse('specific-watch-empty', '{"hookSpecificOutput":{"hookEventName":"Stop","watchPaths":[]}}');
parse('specific-permission-values', '{"hookSpecificOutput":{"hookEventName":"Stop","permissionDecision":"passthrough"}}');
parse('specific-permission-invalid', '{"hookSpecificOutput":{"hookEventName":"Stop","permissionDecision":"maybe"}}');
parse('specific-updated-input-array', '{"hookSpecificOutput":{"hookEventName":"Stop","updatedInput":[1]}}');
parse('specific-mcp-false', '{"hookSpecificOutput":{"hookEventName":"Stop","updatedMCPToolOutput":false}}');
parse('specific-decision-allow', '{"hookSpecificOutput":{"hookEventName":"Stop","decision":{"behavior":"allow","updatedInput":{"a":1},"updatedPermissions":[{"b":2}]}}}');
parse('specific-decision-deny', '{"hookSpecificOutput":{"hookEventName":"Stop","decision":{"behavior":"deny","message":"no","interrupt":true}}}');
parse('specific-decision-other', '{"hookSpecificOutput":{"hookEventName":"Stop","decision":{"behavior":"maybe"}}}');
parse('specific-decision-array', '{"hookSpecificOutput":{"hookEventName":"Stop","decision":[]}}');
parse('specific-retry-false', '{"hookSpecificOutput":{"hookEventName":"Stop","retry":false}}');
parse('duplicate-keys', '{"continue":true,"continue":false}');
parse('integer-keys', '{"2":"b","1":"a"}');
parse('proto-key', '{"__proto__":{"x":1},"continue":true}');
parse('unicode', '{"reason":"你好\\ud800","continue":false}');
parse('trailing-junk', '{"continue":false} junk');

// ---------------------------------------------------------------- command hooks
const commandCases: any[] = [];
const scriptContents: Record<string, string> = {};
let scriptIndex = 0;
async function script(_name: string, text: string): Promise<string> {
  scriptIndex += 1;
  const file = `cmd-${scriptIndex}.sh`;
  await writeFile(join(root, file), text, { mode: 0o755 });
  const path = `<dir>/${file}`;
  scriptContents[path] = text;
  return path;
}
function env() {
  return { PATH: process.env.PATH, HOME: process.env.HOME };
}
async function commandCase(input: {
  name: string;
  script?: string;
  command?: string;
  cwd?: string;
  env?: any;
  timeoutMs?: number;
  abortAtMs?: number;
  hookInput?: any;
}) {
  const maskedCommand = input.command ?? `/bin/bash '${input.script}'`;
  const command = actual(maskedCommand);
  const ac = input.abortAtMs === undefined ? undefined : new AbortController();
  if (ac && input.abortAtMs === 0) ac.abort();
  const running = new CommandHookExecutor().execute({
    hook: { type: 'command', command },
    hookInput: input.hookInput ?? baseInput,
    cwd: actual(input.cwd ?? '<dir>'),
    env: input.env ?? env(),
    ...(input.timeoutMs !== undefined ? { timeoutMs: input.timeoutMs } : {}),
    ...(ac ? { signal: ac.signal } : {}),
  });
  if (ac && input.abortAtMs! > 0) setTimeout(() => ac.abort(), input.abortAtMs);
  const result = await running;
  commandCases.push({
    name: input.name,
    command: command.split(root).join('<dir>'),
    cwd: (input.cwd ?? '<dir>').split(root).join('<dir>'),
    env: input.env ?? env(),
    hookInput: input.hookInput ?? baseInput,
    ...(input.timeoutMs !== undefined ? { timeoutMs: input.timeoutMs } : {}),
    ...(input.abortAtMs !== undefined ? { abortAtMs: input.abortAtMs } : {}),
    ...(input.script ? { script: input.script } : {}),
    expected: json(result),
  });
}

await named('stdin', `#!/bin/bash
input=$(cat)
printf 'STDIN:%s\\n' "$input"
printf '{"systemMessage":"hello","hookSpecificOutput":{"hookEventName":"Stop","additionalContext":"ctx"}}\\n'
`);
await named('exit2', `#!/bin/bash
printf 'out\\n'
printf 'err\\n' >&2
exit 2
`);
await named('exit1', `#!/bin/bash
printf 'out\\n'
exit 1
`);
await named('exit3', `#!/bin/bash
printf 'oops\\n'
exit 3
`);
await named('bytes', `#!/bin/bash
printf '\\377'
`);
await named('signal', `#!/bin/bash
kill -TERM $$
`);
await named('crlf', `#!/bin/bash
printf '{"continue":false,"reason":"r"}\\r\\n'
`);
await named('lines', `#!/bin/bash
printf 'x\\n{bad}\\n{"decision":"block"}\\n'
`);
await named('timeout', `#!/bin/bash
printf 'start\\n'
sleep 5
printf 'end\\n'
`);
await named('abort0', `#!/bin/bash
printf 'ran\\n'
`);
await named('abort1', `#!/bin/bash
printf 'start\\n'
sleep 5
printf 'end\\n'
`);
await named('env', `#!/bin/bash
printf 'A:%s\\nB:%s\\nC:%s\\nPWD:%s\\n' "$A" "$B" "$C" "$PWD"
`);
await named('expand', `#!/bin/bash
printf 'args:%s\\n' "$1"
`);
await named('async', `#!/bin/bash
printf '{"async":true,"asyncHookId":"aid","asyncTimeout":2.5}\\n'
`);
await named('sleep', `#!/bin/bash
printf 'start\\n'
sleep 5
printf 'end\\n'
`);

await commandCase({ name: 'success-json-stdin', script: S.stdin });
await commandCase({ name: 'exit-blocking', script: S.exit2 });
await commandCase({ name: 'exit-nonblocking', script: S.exit1 });
await commandCase({ name: 'exit-three', script: S.exit3 });
await commandCase({ name: 'empty-command', command: '' });
await commandCase({ name: 'missing-cwd', command: 'true', cwd: '<dir>/missing' });
await commandCase({ name: 'invalid-utf8-stdout', script: S.bytes });
await commandCase({ name: 'killed-by-signal', script: S.signal });
await commandCase({ name: 'crlf-json', script: S.crlf });
await commandCase({ name: 'invalid-json-then-valid', script: S.lines });
await commandCase({ name: 'timeout', script: S.timeout, timeoutMs: 500 });
await commandCase({ name: 'abort-before-start', script: S.abort0, abortAtMs: 0 });
await commandCase({ name: 'abort-during', script: S.abort1, abortAtMs: 500 });
await commandCase({
  name: 'env-values',
  script: S.env,
  env: { A: 'a', B: 'b b', PATH: process.env.PATH },
});
// Argument expansion is the shell's, not the executor's.
await commandCase({
  name: 'shell-expansion',
  command: `/bin/bash '${S.expand}' 'x y'`,
});
// Pipe reads must run concurrently with the wait; these exceed the 64 KiB
// macOS pipe buffer and would otherwise deadlock until the timeout.
await commandCase({
  name: 'stdout-over-pipe-buffer',
  command: `head -c 70000 /dev/zero | tr '\\0' a; printf '\\n{"decision":"block"}\\n'`,
});
await commandCase({
  name: 'stderr-over-pipe-buffer',
  command: `head -c 70000 /dev/zero | tr '\\0' a 1>&2; exit 2`,
});
// The leader exits immediately but a same-group grandchild keeps stdout open;
// timeout/abort must still reach the whole group (TS clears these at 1004/501
// ms). The TERM-ignoring case needs the 3 s SIGKILL grace (TS: 4004 ms).
await commandCase({
  name: 'grandchild-holds-stdout-timeout',
  command: 'sleep 20 & echo hi',
  timeoutMs: 1000,
});
await commandCase({
  name: 'grandchild-holds-stdout-abort',
  command: 'sleep 20 & echo hi',
  timeoutMs: 600000,
  abortAtMs: 500,
});
await commandCase({
  name: 'grandchild-ignores-term',
  command: `bash -c 'trap "" TERM; sleep 20' & echo hi; sleep 20`,
  timeoutMs: 1000,
});
// PILOTDECK_SHELL_PATH truthiness/PATH semantics (runtime/commandShell.ts).
await commandCase({
  name: 'shell-path-empty-falls-back',
  command: `printf 'empty-ok\\n'`,
  env: { ...env(), PILOTDECK_SHELL_PATH: '' },
});
await commandCase({
  name: 'shell-path-bare-name',
  command: `printf 'bare-ok\\n'`,
  env: { ...env(), PILOTDECK_SHELL_PATH: 'bash' },
});

// ---------------------------------------------------------------- HTTP hooks
const requests: any[] = [];
const selectedHeaders = ['content-type', 'x-a', 'x-b', 'x-c', 'x-d'];
function selectHeaders(headers: any) {
  const selected: Record<string, string> = {};
  for (const name of selectedHeaders) {
    if (headers[name] !== undefined) selected[name] = headers[name];
  }
  return selected;
}
const server = createServer((req, res) => {
  let body = '';
  req.on('data', chunk => body += chunk);
  req.on('end', () => {
    requests.push({ method: req.method, path: req.url, headers: selectHeaders(req.headers), body });
    if (req.url === '/bad') {
      res.writeHead(400, { 'content-type': 'application/json' });
      res.end('{"continue":false,"reason":"bad"}');
      return;
    }
    if (req.url === '/empty') {
      res.writeHead(204);
      res.end();
      return;
    }
    if (req.url === '/invalid-utf8') {
      res.writeHead(200, { 'content-type': 'application/octet-stream' });
      res.end(Buffer.from([0xff, 0xfe]));
      return;
    }
    if (req.url === '/slow') {
      setTimeout(() => {
        res.writeHead(200, { 'content-type': 'text/plain' });
        res.end('slow');
      }, 5000);
      return;
    }
    if (req.url === '/echo') {
      res.writeHead(200, { 'content-type': 'application/json' });
      res.end(JSON.stringify({ body, headers: selectHeaders(req.headers) }));
      return;
    }
    if (req.url === '/server-error') {
      res.writeHead(500);
      res.end();
      return;
    }
    res.writeHead(200, { 'content-type': 'application/json' });
    res.end('{"systemMessage":"hello"}');
  });
});
await new Promise<void>(resolve => server.listen(0, '127.0.0.1', resolve));
const port = (server.address() as any).port;
const url = (path: string) => `http://127.0.0.1:${port}${path}`;
// A just-closed loopback port is a stable "connection refused" target; the
// oracle stores it as <refused> so reruns stay byte-identical.
const closed = createServer(() => {});
await new Promise<void>(resolve => closed.listen(0, '127.0.0.1', resolve));
const closedPort = (closed.address() as any).port;
await new Promise<void>(resolve => closed.close(() => resolve()));
const httpCases: any[] = [];
async function httpCase(input: {
  name: string;
  target: string;
  headers?: any;
  allowedEnvVars?: string[];
  hookInput?: any;
  abortAtMs?: number;
}) {
  const before = requests.length;
  const ac = input.abortAtMs === undefined ? undefined : new AbortController();
  if (ac && input.abortAtMs === 0) ac.abort();
  const running = new HttpHookExecutor().execute({
    hook: {
      type: 'http',
      url: input.target
        .split('<url>').join(url(''))
        .split('<refused>').join(`http://127.0.0.1:${closedPort}`),
      ...(input.headers ? { headers: input.headers } : {}),
      ...(input.allowedEnvVars ? { allowedEnvVars: input.allowedEnvVars } : {}),
    },
    hookInput: input.hookInput ?? baseInput,
    env: { TOKEN: 't', SECRET: 's' },
    ...(ac ? { signal: ac.signal } : {}),
  });
  if (ac && input.abortAtMs! > 0) setTimeout(() => ac.abort(), input.abortAtMs);
  const result = await running;
  httpCases.push({
    name: input.name,
    target: input.target,
    ...(input.headers ? { headers: input.headers } : {}),
    ...(input.allowedEnvVars ? { allowedEnvVars: input.allowedEnvVars } : {}),
    hookInput: input.hookInput ?? baseInput,
    ...(input.abortAtMs !== undefined ? { abortAtMs: input.abortAtMs } : {}),
    expected: json(result),
    request: requests.length > before ? requests[before] : null,
  });
}
await httpCase({ name: 'ok-json', target: '<url>/ok' });
await httpCase({ name: 'bad-status', target: '<url>/bad' });
await httpCase({ name: 'empty-status', target: '<url>/empty' });
await httpCase({ name: 'invalid-utf8', target: '<url>/invalid-utf8' });
await httpCase({
  name: 'headers-env',
  target: '<url>/echo',
  headers: { 'X-A': '$TOKEN', 'X-B': '${TOKEN}', 'X-C': '$SECRET', 'X-D': 'pre$TOKEN-post' },
  allowedEnvVars: ['TOKEN'],
  hookInput: { ...baseInput, extra: 'v' },
});
await httpCase({ name: 'server-error', target: '<url>/server-error' });
await httpCase({ name: 'invalid-url', target: 'not a url' });
await httpCase({ name: 'non-string-header', target: '<url>/ok', headers: { A: 1 } as any });
await httpCase({ name: 'invalid-header-value', target: '<url>/ok', headers: { A: 'bad\nvalue' } });
// Node headers are ByteStrings: U+00E9 is sent as the latin1 byte E9 while
// U+4E2D (> 0xFF) is rejected before the abort check runs.
await httpCase({ name: 'header-latin1', target: '<url>/echo', headers: { 'X-A': 'José' } });
await httpCase({ name: 'header-cjk-bytestring', target: '<url>/ok', headers: { 'X-A': '中' } });
await httpCase({
  name: 'pre-aborted-invalid-header',
  target: '<url>/ok',
  headers: { A: 'bad\nvalue' },
  abortAtMs: 0,
});
await httpCase({ name: 'pre-aborted', target: '<url>/ok', abortAtMs: 0 });
await httpCase({ name: 'abort-during', target: '<url>/slow', abortAtMs: 500 });
await httpCase({ name: 'refused', target: '<refused>/x' });
server.closeAllConnections();
await new Promise<void>(resolve => server.close(() => resolve()));

// ---------------------------------------------------------------- callback hooks
const callbackCases: any[] = [];
function handlerFor(tag: any): any {
  switch (tag.kind) {
    case 'text': return () => tag.value;
    case 'output': return () => tag.value;
    case 'void': return () => undefined;
    case 'throw': return () => { throw new Error(tag.value); };
    case 'throw-string': return () => { throw tag.value; };
    case 'promise-text': return async () => tag.value;
    default: throw new Error(`bad handler tag ${tag.kind}`);
  }
}
function callback(name: string, value: any) {
  callbackCases.push({ name, value: value === undefined ? { undefined: true } : JSON.parse(json(value)) });
}
{
  const ex = new CallbackHookExecutor();
  const text = { kind: 'text', value: '{"continue":false,"reason":"cb"}' };
  const r1 = ex.register('  cb  ', handlerFor(text));
  callback('register-trimmed', { name: r1.name, generation: r1.generation, active: r1.active });
  callback('execute-text', await ex.execute({ hook: { type: 'callback', name: 'cb' }, hookInput: baseInput }));
  callback('execute-missing', await ex.execute({ hook: { type: 'callback', name: 'nope' }, hookInput: baseInput }));
  const output = { kind: 'output', value: { type: 'sync', decision: 'block', reason: 'r' } };
  const r2 = ex.register('cb', handlerFor(output));
  callback('replace', { oldActive: r1.active, generation: r2.generation, active: r2.active });
  callback('execute-output', await ex.execute({ hook: { type: 'callback', name: 'cb' }, hookInput: baseInput }));
  const rv = ex.register('void', handlerFor({ kind: 'void' }));
  callback('execute-void', await ex.execute({ hook: { type: 'callback', name: 'void' }, hookInput: baseInput }));
  ex.register('thrower', handlerFor({ kind: 'throw', value: 'boom' }));
  callback('execute-throw', await ex.execute({ hook: { type: 'callback', name: 'thrower' }, hookInput: baseInput }));
  ex.register('throwstr', handlerFor({ kind: 'throw-string', value: 'plain' }));
  callback('execute-throw-string', await ex.execute({ hook: { type: 'callback', name: 'throwstr' }, hookInput: baseInput }));
  ex.register('promise', handlerFor({ kind: 'promise-text', value: '{"decision":"approve"}' }));
  callback('execute-promise', await ex.execute({ hook: { type: 'callback', name: 'promise' }, hookInput: baseInput }));
  callback('unregister', ex.unregister('cb'));
  callback('unregister-again', ex.unregister('cb'));
  try { ex.register('', handlerFor({ kind: 'void' })); } catch (error: any) { callback('empty-name-error', error.message); }
  callback('state-active', ex.lifecycleState);
  await ex.dispose();
  callback('state-disposed', ex.lifecycleState);
  callback('execute-after-dispose', await ex.execute({ hook: { type: 'callback', name: 'thrower' }, hookInput: baseInput }));
  try { ex.register('x', handlerFor({ kind: 'void' })); } catch (error: any) { callback('register-after-dispose-error', error.message); }
  callback('old-registration-after-dispose', { r2: r2.active });
  callback('void-registration-after-dispose', { rv: rv.active });
}
{
  // dispose waits for an in-flight callback.
  const ex = new CallbackHookExecutor();
  ex.register('slow', async () => { await new Promise(resolve => setTimeout(resolve, 150)); return '{"decision":"block"}'; });
  const running = ex.execute({ hook: { type: 'callback', name: 'slow' }, hookInput: baseInput });
  const disposing = ex.dispose();
  callback('dispose-drain-state', ex.lifecycleState);
  await disposing;
  callback('dispose-drain-state-after', ex.lifecycleState);
  callback('dispose-drain-result', await running);
}

// ---------------------------------------------------------------- HookRuntime
const runtimeSettings = {
  Stop: [
    {
      matcher: 'm',
      pluginName: 'plug',
      hooks: [
        { type: 'command', command: `/bin/bash '${S.stdin}'` },
        { type: 'command', command: `/bin/bash '${S.exit2}'` },
        { type: 'prompt', prompt: 'p $ARGUMENTS' },
        { type: 'agent', prompt: 'a $ARGUMENTS' },
      ],
    },
    { matcher: '*', hooks: [{ type: 'command', command: `/bin/bash '${S.exit1}'` }] },
  ],
  PreToolUse: [
    {
      matcher: 'Read',
      hooks: [
        { type: 'command', command: `/bin/bash '${S.exit3}'`, if: 'Read(*)' },
        { type: 'command', command: `/bin/bash '${S.bytes}'`, if: 'Edit(*)' },
      ],
    },
  ],
};
const runtimeRuns: any[] = [];
async function captureRun(rt: any, input: any) {
  const events: any[] = [];
  const sub = rt.subscribeExecutionEvents((event: any) => events.push(event));
  const result = await rt.run({ ...input, cwd: actual(input.cwd) });
  sub.dispose();
  return {
    name: input.name,
    event: input.event,
    hookInput: input.hookInput,
    ...(input.matchQuery !== undefined ? { matchQuery: input.matchQuery } : {}),
    cwd: input.cwd,
    env: input.env,
    expected: json(result),
    expectedEvents: json(events),
  };
}
async function runtimeRun(rt: any, input: any) {
  const value = await captureRun(rt, input);
  runtimeRuns.push(value);
  return value;
}
const runtimeDispose: any = {};
{
  const rt = new HookRuntime(actualize(runtimeSettings));
  const env = { PATH: process.env.PATH, HOME: process.env.HOME };
  await runtimeRun(rt, { name: 'mixed-order', event: 'Stop', hookInput: baseInput, matchQuery: 'm', cwd: '<dir>', env });
  await runtimeRun(rt, { name: 'no-match-query', event: 'Stop', hookInput: baseInput, cwd: '<dir>', env });
  await runtimeRun(rt, {
    name: 'condition',
    event: 'PreToolUse',
    hookInput: { ...baseInput, toolName: 'Read', toolInput: { file_path: '/x' } },
    matchQuery: 'Read',
    cwd: '<dir>',
    env,
  });
  await runtimeRun(rt, {
    name: 'condition-false',
    event: 'PreToolUse',
    hookInput: { ...baseInput, toolName: 'Write', toolInput: { file_path: '/x' } },
    matchQuery: 'Read',
    cwd: '<dir>',
    env,
  });
  // Subscriber errors never change execution semantics.
  const observed: any[] = [];
  const throwing = rt.subscribeExecutionEvents(() => { throw new Error('observer'); });
  const recording = rt.subscribeExecutionEvents(event => observed.push(event));
  const result = await rt.run({
    event: 'Stop',
    hookInput: baseInput,
    matchQuery: 'm',
    cwd: actual('<dir>'),
    env,
  });
  throwing.dispose();
  recording.dispose();
  runtimeRuns.push({
    name: 'observer-error',
    event: 'Stop',
    hookInput: baseInput,
    matchQuery: 'm',
    cwd: '<dir>',
    env,
    expected: json(result),
    expectedEvents: json(observed),
  });
  await rt.dispose();
  runtimeDispose.state = rt.lifecycleState;
  try { rt.subscribeExecutionEvents(() => {}); } catch (error: any) { runtimeDispose.subscribeError = error.message; }
  try { await rt.run({ event: 'Stop', hookInput: baseInput, matchQuery: 'm', cwd: actual('<dir>'), env }); } catch (error: any) { runtimeDispose.runError = error.message; }
}
const runtimeExtra: any = { pluginRoot: {}, sessionEnd: {}, async: {}, disposeDrain: {}, callbackHook: {} };
{
  // pluginRoot replaces cwd for command hooks.
  const settings = {
    Stop: [{ pluginRoot: '<dir>', hooks: [{ type: 'command', command: 'printf "PWD:%s\\n" "$PWD"' }] }],
  };
  const rt = new HookRuntime(actualize(settings));
  runtimeExtra.pluginRoot = {
    settings: JSON.parse(json(settings)),
    run: await captureRun(rt, {
      name: 'plugin-root',
      event: 'Stop',
      hookInput: baseInput,
      matchQuery: 'Stop',
      cwd: '<dir>/does-not-exist',
      env: { PATH: process.env.PATH, HOME: process.env.HOME },
    }),
  };
  await rt.dispose();
}
{
  // SessionEnd uses the 1500 ms hook timeout.
  const settings = {
    SessionEnd: [{ hooks: [{ type: 'command', command: `/bin/bash '${S.sleep}'` }] }],
  };
  const rt = new HookRuntime(actualize(settings));
  const started = performance.now();
  runtimeExtra.sessionEnd = {
    settings: JSON.parse(json(settings)),
    run: await captureRun(rt, {
      name: 'session-end-timeout',
      event: 'SessionEnd',
      hookInput: baseInput,
      matchQuery: 'SessionEnd',
      cwd: '<dir>',
      env: { PATH: process.env.PATH, HOME: process.env.HOME },
    }),
  };
  runtimeExtra.sessionEnd.elapsedAtLeast1500 = performance.now() - started >= 1500;
  await rt.dispose();
}
{
  const settings = {
    Stop: [{ hooks: [{ type: 'command', command: `/bin/bash '${S.async}'`, asyncRewake: true }] }],
  };
  const rt = new HookRuntime(actualize(settings));
  const result = await rt.run({
    event: 'Stop',
    hookInput: baseInput,
    matchQuery: 'Stop',
    cwd: root,
    env: { PATH: process.env.PATH, HOME: process.env.HOME },
  });
  const asyncRuntime: any = {
    settings: JSON.parse(json(settings)),
    run: json(result),
    events: json(result.events),
    pending: json(result.pendingAsyncHooks),
    collectBefore: json(rt.collectAsyncResponses()),
  };
  asyncRuntime.completeInvalid = rt.completeAsyncHook(result.pendingAsyncHooks[0].id, { output: { type: 'async' } });
  asyncRuntime.completeValid = rt.completeAsyncHook(result.pendingAsyncHooks[0].id, { stdout: '{"continue":false,"reason":"r"}' });
  asyncRuntime.collect = json(rt.collectAsyncResponses());
  asyncRuntime.collectAgain = json(rt.collectAsyncResponses());
  rt.removeDeliveredAsyncResponses();
  asyncRuntime.afterRemove = json(rt.collectAsyncResponses());
  const second = await rt.run({
    event: 'Stop',
    hookInput: baseInput,
    matchQuery: 'Stop',
    cwd: root,
    env: { PATH: process.env.PATH, HOME: process.env.HOME },
  });
  asyncRuntime.secondPending = json(second.pendingAsyncHooks);
  asyncRuntime.cancel = rt.cancelAsyncHook(second.pendingAsyncHooks[0].id);
  asyncRuntime.cancelAgain = rt.cancelAsyncHook(second.pendingAsyncHooks[0].id);
  asyncRuntime.afterCancel = json(rt.collectAsyncResponses());
  await rt.dispose();
  asyncRuntime.completeAfterDispose = rt.completeAsyncHook(result.pendingAsyncHooks[0].id, { stdout: '{}' });
  asyncRuntime.collectAfterDispose = json(rt.collectAsyncResponses());
  runtimeExtra.async = asyncRuntime;
}
{
  // HookRuntime routes callback hooks through the injected executor.
  const settings = {
    Stop: [{ pluginName: 'plug', hooks: [
      { type: 'callback', name: 'cb' },
      { type: 'command', command: `/bin/bash '${S.stdin}'` },
    ] }],
  };
  const rt = new HookRuntime(actualize(settings));
  rt.getCallbackExecutor().register('cb', () => ({
    systemMessage: 'sys',
    hookSpecificOutput: { hookEventName: 'Stop', additionalContext: 'ctx' },
  }));
  runtimeExtra.callbackHook = {
    settings: JSON.parse(json(settings)),
    run: await captureRun(rt, {
      name: 'callback-hook',
      event: 'Stop',
      hookInput: baseInput,
      matchQuery: 'Stop',
      cwd: '<dir>',
      env: { PATH: process.env.PATH, HOME: process.env.HOME },
    }),
  };
  await rt.dispose();
}
{
  // dispose waits for an in-flight run.
  const settings = { Stop: [{ hooks: [{ type: 'command', command: 'sleep 0.3' }] }] };
  const rt = new HookRuntime(actualize(settings));
  const running = rt.run({ event: 'Stop', hookInput: baseInput, matchQuery: 'Stop', cwd: root, env: { PATH: process.env.PATH } });
  const disposing = rt.dispose();
  runtimeExtra.disposeDrain = {
    settings: JSON.parse(json(settings)),
    immediateState: rt.lifecycleState,
    result: null,
  };
  await disposing;
  runtimeExtra.disposeDrain.afterState = rt.lifecycleState;
  runtimeExtra.disposeDrain.result = json(await running);
}

// ---------------------------------------------------------------- lifecycle dispatch
const lifecycleSettings = {
  Stop: [
    {
      pluginName: 'plug',
      hooks: [
        { type: 'command', command: `/bin/bash '${S.stdin}'` },
        { type: 'command', command: `/bin/bash '${S.exit2}'` },
        { type: 'prompt', prompt: 'p $ARGUMENTS' },
        { type: 'command', command: `/bin/bash '${S.async}'` },
      ],
    },
  ],
};
const lifecycle: any = {};
const lifecycleBaseInput = { ...baseInput, cwd: '<dir>' };
{
  const hooks = new HookRuntime(actualize(lifecycleSettings));
  const runtime = new LifecycleRuntime(hooks);
  lifecycle.baseInput = JSON.parse(json(lifecycleBaseInput));
  const result = await runtime.dispatch({
    event: 'Stop',
    baseInput: actualize(lifecycleBaseInput),
    payload: { extra: 'v' },
    matchQuery: 'Stop',
    env: { PATH: process.env.PATH, HOME: process.env.HOME },
    cwd: '<dir>',
  });
  lifecycle.dispatch = json(result);
  lifecycle.dispatchMessages = json(result.messages);
  // A session-level LifecycleRuntime is reused across turns (the dispatcher's
  // dispose never touches it): the second dispatch still runs hooks and its
  // pending async registry keeps the first turn's registration alive.
  const second = await runtime.dispatch({
    event: 'Stop',
    baseInput: actualize(lifecycleBaseInput),
    payload: { extra: 'v' },
    matchQuery: 'Stop',
    env: { PATH: process.env.PATH, HOME: process.env.HOME },
    cwd: '<dir>',
  });
  lifecycle.acrossTurns = {
    firstPending: json(result.pendingAsyncHooks),
    first: json(result),
    second: json(second),
    secondPending: json(second.pendingAsyncHooks),
  };
  // A second dispatch after dispose rejects with the runtime message.
  await runtime.dispose();
  try {
    await runtime.dispatch({ event: 'Stop', baseInput, payload: {}, matchQuery: 'Stop' });
  } catch (error: any) {
    lifecycle.disposedError = error.message;
  }
}
{
  const nullRuntime = new (class extends LifecycleRuntime {
    constructor() { super(new HookRuntime({})); }
    override async dispatch() {
      return { effects: [], messages: [], events: [], blockingErrors: [], nonBlockingErrors: [], pendingAsyncHooks: [] };
    }
  })();
  lifecycle.nullDispatch = json(await nullRuntime.dispatch({ event: 'Stop', baseInput, payload: {}, matchQuery: 'Stop' }));
  await nullRuntime.dispose();
}

// HostLifecycleRuntime request/response and error projection.
const hostClientCases: any[] = [];
{
  const requests: any[] = [];
  const responses = [
    { ok: true, payload: { result: { effects: [], messages: [], events: [], blockingErrors: [], nonBlockingErrors: [] } } },
    { ok: true, payload: { result: JSON.parse(json({
      effects: [{ type: 'system_message', content: 'sys' }, { type: 'additional_context', content: 'ctx', source: 'plug:command' }],
      messages: [{ role: 'user', content: [{ type: 'text', text: '<hook_context source="plug:command">\nctx\n</hook_context>' }], metadata: { synthetic: true, purpose: 'hook_context' } }],
      events: [{ type: 'started', sessionId: 'session-1', hookName: 'plug:command', hookEvent: 'Stop' }],
      blockingErrors: [],
      nonBlockingErrors: [],
      pendingAsyncHooks: [],
    })) } },
    { ok: false, code: 'BOOM', error: { message: 'kaboom' } },
    { ok: false, code: 'NOERR' },
    { ok: false, code: null, error: { message: null } },
    { ok: true, payload: { result: {} } },
    { ok: true },
    { ok: true, payload: { result: { effects: [], messages: [], events: [], blockingErrors: [], nonBlockingErrors: [], pendingAsyncHooks: 'x' } } },
    { ok: true, payload: { result: { effects: [], messages: [], events: [], blockingErrors: [], nonBlockingErrors: [], pendingAsyncHooks: [] } } },
    { ok: true, payload: { result: { effects: [], messages: [], events: [], blockingErrors: [], nonBlockingErrors: [], pendingAsyncHooks: [] } } },
  ];
  let index = 0;
  const client = async (request: any) => {
    requests.push(request);
    return responses[index++] ?? { ok: true, payload: { result: { effects: [], messages: [], events: [], blockingErrors: [], nonBlockingErrors: [], pendingAsyncHooks: [] } } };
  };
  const runtime = new HostLifecycleRuntime(client, { runId: 'run-1', operationId: 'op-1', idempotencyKey: 'idem' }, () => 'uuid');
  const result = await runtime.dispatch({ event: 'Stop', baseInput, payload: { x: 1 }, matchQuery: 'Stop' });
  hostClientCases.push({ name: 'request', request: json(requests[0]), result: json(result) });
  for (let i = 1; i < responses.length; i += 1) {
    try {
      const value = await runtime.dispatch({ event: 'Stop', baseInput, payload: {} });
      hostClientCases.push({ name: `response-${i}`, response: JSON.parse(json(responses[i])), resolved: json(value) });
    } catch (error: any) {
      hostClientCases.push({ name: `response-${i}`, response: JSON.parse(json(responses[i])), error: error.message, code: error.code === undefined ? null : error.code });
    }
  }
  const bindingWithoutKey = new HostLifecycleRuntime(client, { runId: 'run-2', operationId: 'op-2' }, () => 'uuid2');
  await bindingWithoutKey.dispatch({ event: 'Stop', baseInput });
  hostClientCases.push({ name: 'request-without-idempotency-key', request: json(requests[requests.length - 1]) });
}

// Turn environment.
const envCases: any[] = [];
function envCase(name: string, base: any, cwd: string, sessionId: string, turnId: string) {
  const value = buildTurnEnvironment(base, cwd.split('<dir>').join(root), sessionId, turnId);
  const expected = json(value).split(process.cwd()).join('<cwd>');
  envCases.push({ name, base, cwd, sessionId, turnId, expected });
}
envCase('ascii', { A: '1' }, '<dir>', 'sess', 'turn');
envCase('fullwidth', { A: '1' }, '<dir>', 'sé ss/../x', 'ＡＢＣ-..');
envCase('relative', { A: '1' }, 'rel/dir', 's', 't');
envCase('normalized', { A: '1' }, '<dir>/sub/../.', 's', 't');
envCase('long', {}, '<dir>', 'a'.repeat(120), '-.-');
envCase('empty', {}, '<dir>', '', '');

const oracle = {
  sourceCommit,
  nodeVersion: process.version,
  sourceSha256,
  scripts: scriptContents,
  parse: parseCases,
  command: commandCases,
  http: { cases: httpCases },
  callback: { cases: callbackCases },
  runtime: {
    settings: JSON.parse(json(runtimeSettings)),
    runs: runtimeRuns,
    dispose: runtimeDispose,
    ...runtimeExtra,
  },
  lifecycle: {
    settings: JSON.parse(json(lifecycleSettings)),
    ...lifecycle,
    hostClientCases,
    envCases,
  },
};
const text = JSON.stringify(oracle, null, 2) + '\n';
assert.ok(!text.includes(root) && !text.includes(realRoot), 'oracle leaked the run directory');
assert.ok(!text.includes(baseline), 'oracle leaked the baseline path');
await writeFile(join(output, 'oracle.json'), text);
assert.ok(root.startsWith(join(tmpdir(), 'pdrs-hr-')) && root !== tmpdir(), 'refuse to remove an unowned directory');
await rm(root, { recursive: true, force: true });
console.log(`hooks-runtime oracle: ${parseCases.length} parse, ${commandCases.length} command, ${httpCases.length} http, ${callbackCases.length} callback, ${runtimeRuns.length} runtime runs, ${hostClientCases.length} host client cases, ${envCases.length} env cases`);
