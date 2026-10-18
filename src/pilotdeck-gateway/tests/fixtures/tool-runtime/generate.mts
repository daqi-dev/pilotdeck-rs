// Deterministic fake tools around the actual fixed TS runtime and schedulers.
import { mkdtemp, mkdir, writeFile, readFile, rm, lstat } from 'node:fs/promises';
import { realpathSync } from 'node:fs';
import { join, resolve, dirname, relative, sep } from 'node:path';
import { tmpdir } from 'node:os';
import { pathToFileURL } from 'node:url';
import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import assert from 'node:assert/strict';
import { bindSourceTree } from '../../baseline_source_guard.mjs';

assert.ok(process.env.PDRS_BASELINE);
assert.ok(process.argv[2], 'provide an output parent directory; each run writes into its own mkdtemp child');
const baseline = resolve(process.env.PDRS_BASELINE!);
const outputParent = resolve(process.argv[2]);
const git = realpathSync(execFileSync('which', ['git'], { encoding: 'utf8' }).trim());
const commit = 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';
bindSourceTree(baseline, commit, git);
const load = (name: string) => import(pathToFileURL(join(baseline, 'src', name)).href);
const { ToolRegistry } = await load('tool/registry/ToolRegistry.ts');
const { ToolRuntime } = await load('tool/execution/ToolRuntime.ts');
const { ConcurrentToolScheduler } = await load('tool/scheduler/ConcurrentToolScheduler.ts');
const { SequentialToolScheduler } = await load('tool/scheduler/SequentialToolScheduler.ts');
const { PilotDeckToolRuntimeError } = await load('tool/protocol/errors.ts');
const { createToolSchedulerPort } = await load('agent/modules/capability/toolSchedulerAdapter.ts');
const epoch = Date.parse('2026-01-01T00:00:00.000Z');
const scratch = await mkdtemp(join(realpathSync(tmpdir()), 'pdrs-tool-runtime-'));
const owner = await lstat(scratch);
const clean = (value: unknown) => JSON.parse(JSON.stringify(value));
const tool = (name: string, safe: boolean, extra = {}) => ({ name, safe, ...extra });
const call = (id: string, name: string, input: unknown = {}) => ({ id, name, input });
const definitions = [tool('write_file', false), tool('read_file', true, { aliases: ['read'] }), tool('fast', true), tool('slow', false)];
const mixed = [call('a', 'write_file', { delay: 3 }), call('b', 'read_file', { delay: 20 }), call('c', 'slow', { delay: 2 }), call('d', 'fast', { delay: 5 })];
const scenarios: any[] = [
  { name: 'empty', calls: [] },
  { name: 'single-skips-predicate', definitions: [tool('read_file', true, { predicateError: true })], calls: [call('a', 'read_file', { delay: 3 })] },
  { name: 'safe-first-original-result-order', calls: mixed },
  { name: 'sequential', mode: 'sequential', calls: mixed },
  { name: 'repaired-name-is-classified-before-repair', calls: [call('a', 'tools.ReadFile', { delay: 1 }), call('b', 'fast', { delay: 4 }), call('c', 'read', { delay: 2 })] },
  { name: 'input-dependent-safety', calls: [call('a', 'read_file', { safe: false, delay: 1 }), call('b', 'read_file', { delay: 3 }), call('c', 'read_file', { delay: 2 })] },
  { name: 'hidden-safe-and-unknown', hidden: ['read_file'], calls: [call('a', 'read'), call('b', 'missing_xyz'), call('c', 'fast', { delay: 2 })] },
  { name: 'unavailable-repaired', unavailable: [{ toolName: 'web_fetch', code: 'setup_required', reason: 'Configure provider.', aliases: ['get_url'] }], calls: [call('a', 'FetchURL'), call('b', 'get_url'), call('c', 'fast')] },
  { name: 'coded-failure-does-not-skip-siblings', calls: [call('a', 'slow'), call('b', 'read_file', { delay: 5, fail: 'file_not_found' }), call('c', 'fast', { delay: 2 })] },
  { name: 'ordinary-and-non-error', calls: [call('a', 'read_file', { fail: 'Error' }), call('b', 'fast', { fail: 'nonError' }), call('c', 'slow')] },
  { name: 'already-cancelled', preCancel: true, calls: mixed },
  { name: 'cancel-during-safe-phase', cancelAt: 4, calls: mixed },
  { name: 'cancel-during-serial-phase', cancelAt: 4, calls: [call('a', 'slow', { delay: 10 }), call('b', 'write_file')] },
  { name: 'deadline-agent-budget', deadline: 7, calls: [call('a', 'read_file', { delay: 20, useBudget: true }), call('b', 'fast', { delay: 2 }), call('c', 'slow', { delay: 1 })] },
  { name: 'deadline-expired', deadline: -1, calls: [call('a', 'slow', { delay: 10, useBudget: true })] },
  { name: 'deadline-keeps-smaller-budget', deadline: 50, context: { subagentTimeoutMs: 3 }, calls: [call('a', 'read_file', { delay: 10, useBudget: true })] },
  { name: 'predicate-throws-before-start', definitions: [tool('read_file', true), tool('fast', true, { predicateError: true })], calls: [call('a', 'read_file'), call('b', 'fast')] },
  { name: 'permission-deny-cancel-ask', calls: [call('a', 'read_file', { decision: 'deny' }), call('b', 'fast', { decision: 'cancel' }), call('c', 'slow', { decision: 'ask' })] },
  { name: 'permission-prompt-denial', calls: [call('a', 'read_file', { decision: 'deny', permissionMessage: 'prompt disabled' })] },
  { name: 'permission-updates-input', calls: [call('a', 'read_file', { updatedInput: { delay: 2, value: 'changed' } })] },
  { name: 'permission-throws', calls: [call('a', 'read_file', { decision: 'throw' })] },
  { name: 'schema-validation', definitions: [tool('read_file', true, { schema: { type: 'object', required: ['path'], properties: { path: { type: 'string' } }, additionalProperties: false } })], calls: [call('a', 'read_file', { extra: true }), call('b', 'read_file', { path: 5 })] },
  { name: 'validation-throws', calls: [call('a', 'read_file', { validationError: true }), call('b', 'fast')] },
  { name: 'validation-issues', calls: [call('a', 'read_file', { validationIssues: [{ path: '$.path', code: 'invalid_schema', message: 'File has not been read yet.' }] })] },
  { name: 'prompt-required', definitions: [tool('dialog', false, { interaction: true })], context: { permissionContext: { mode: 'default', canPrompt: false, additionalWorkingDirectories: [] } }, calls: [call('a', 'dialog')] },
  { name: 'validation-empty-issues-and-raw-paths', calls: [call('a', 'read_file', { validationIssues: [] }), call('b', 'read_file', { validationIssues: [{ path: '.foo', code: 'required', message: 'm' }, { path: '$bar', code: 'invalid_type', message: 't' }, { path: '$.baz', code: 'invalid_enum', message: 'e' }] })] },
  { name: 'schema-prototype-keys-enum-arrays', definitions: [
    tool('read_file', true, { schema: { type: 'object', required: ['toString', 'path'], additionalProperties: false, properties: { path: { type: ['string', 'null'] }, mode: { enum: ['a', 0, null, true] }, list: { type: 'array', items: { type: 'integer' } }, constructor: { type: 'string' }, nested: { type: ['null', 'object'], required: ['x'], properties: { x: { type: 'number' } } } } } }),
    tool('fast', true, { schema: { type: 'object', additionalProperties: false } }),
  ], calls: [
    call('a', 'read_file', { path: null, mode: 'b', list: [1, 2.5, 'x'], extra: 1, nested: {} }),
    call('b', 'read_file', { path: 'p', mode: null, list: [], nested: null }),
    call('c', 'fast', { toString: 1, valueOf: 2, other: 3 }),
    call('d', 'fast', []),
  ] },
  { name: 'required-file-tool-truncated-output', definitions: [tool('write_file', false, { schema: { type: 'object', required: ['file_path', 'content'] } }), tool('bash', false, { schema: { type: 'object', required: ['command'] } })], context: { outputTruncated: true, maxOutputTokens: 1000 }, calls: [call('a', 'write_file', {}), call('b', 'bash', {})] },
  { name: 'required-file-tool-not-truncated', definitions: [tool('write_file', false, { schema: { type: 'object', required: ['content'] } })], context: { outputTruncated: 0, maxOutputTokens: 0 }, calls: [call('a', 'write_file', {})] },
  { name: 'repair-builtin-alias-prototype-key-throws', calls: [call('a', 'fast'), call('b', 'constructor')] },
  { name: 'configured-alias-number-target-throws', context: { toolAliases: { foo: 5 } }, calls: [call('a', 'foo')] },
  { name: 'configured-alias-null-target-throws', context: { toolAliases: { foo: null } }, calls: [call('a', 'foo')] },
  { name: 'configured-alias-integer-keys-first', context: { toolAliases: { zed: 'fast', '10': 'missing', '2': 'read_file' } }, calls: [call('a', 'zed'), call('b', '2'), call('c', '10')] },
  { name: 'permission-unknown-type-executes', calls: [call('a', 'read_file', { decision: 'weird', delay: 1 })] },
  { name: 'noncooperative-tool-finishes-after-cancel', cancelAt: 2, calls: [call('a', 'read_file', { delay: 6, ignoreAbort: true }), call('b', 'fast', { delay: 1 }), call('c', 'slow', { delay: 1 })] },
  { name: 'success-preserves-content-preview-metadata', context: { maxResultBytes: 3 }, calls: [call('a', 'read_file', { value: 'long output 中文', data: { n: 1 }, metadata: { own: true }, supplementalMessages: [{ role: 'user', content: [{ type: 'text', text: 'extra' }] }] })] },
  // Round 2. `schemaJson` / `inputJson` are raw JSON text parsed by both sides,
  // so own `__proto__` keys and non-sorted integer keys reach the validator.
  { name: 'schema-proto-property-reads-object-prototype', definitions: [
    tool('read_file', true, { schemaJson: '{"type":"object","properties":{"__proto__":{"type":"object","required":["missing","toString"]}}}' }),
    tool('fast', true, { schemaJson: '{"type":"object","properties":{"__proto__":{"additionalProperties":false,"properties":{"__proto__":{"type":"object"},"toString":{"enum":[1]},"valueOf":{"type":"object"}}}}}' }),
    tool('slow', false, { schemaJson: '{"type":"object","properties":{"__proto__":{"type":"object"}}}' }),
    tool('write_file', false, { schemaJson: '{"properties":{"__proto__":{"type":"number"}}}' }),
  ], calls: [call('a', 'read_file'), call('b', 'fast'), call('c', 'slow'), { id: 'd', name: 'slow', inputJson: '{"__proto__":5}' }, call('e', 'write_file')] },
  { name: 'schema-integer-keys-ordered-like-object-keys', definitions: [
    tool('read_file', false, { schemaJson: '{"type":"object","additionalProperties":false,"properties":{"b":{"type":"string"},"10":{"type":"string"},"2":{"type":"string"}}}' }),
  ], calls: [{ id: 'a', name: 'read_file', inputJson: '{"z":1,"b":0,"7":1,"10":1,"2":2,"01":3}' }] },
  { name: 'schema-abnormal-shapes-without-throw', definitions: [
    tool('p1', false, { schemaJson: '{"type":"object","required":"ab\\ud83d\\ude00","properties":{"a":{}}}' }),
    tool('p2', false, { schemaJson: '{"type":"object","additionalProperties":false,"properties":["s",{"type":"number"}]}' }),
    tool('p3', false, { schemaJson: '{"type":["string",null,["a",null]]}' }),
    tool('p4', false, { schemaJson: '{"type":"object","required":[5,null,true,{"a":1},["x",null]],"properties":{"items":{"type":"array","items":null},"e":{"enum":0,"type":"string"},"s":"str","n":{"enum":null}}}' }),
    tool('p5', false, { schemaJson: '"plain"' }),
    tool('p6', false, { schemaJson: '{"properties":"ab","required":null}' }),
  ], calls: [
    call('a', 'p1', { a: 1 }),
    { id: 'b', name: 'p2', inputJson: '{"x":4,"0":1,"1":"x","length":2,"map":3}' },
    call('c', 'p3', 1),
    { id: 'd', name: 'p4', inputJson: '{"5":1,"items":[1],"e":"v","s":1,"n":2}' },
    call('e', 'p5', { x: 1 }),
    { id: 'f', name: 'p6', inputJson: '{"1":2,"0":{}}' },
  ] },
  ...([
    ['null', '{}'], ['{"properties":{"a":null}}', '{"a":1}'], ['{"enum":"abc"}', '1'], ['{"enum":{"a":1}}', '1'],
    ['{"type":"object","required":5}', '{}'], ['{"required":{}}', '{}'], ['{"required":true}', '{}'],
    ['{"properties":"ab","additionalProperties":false}', '{"x":1}'], ['{"properties":5,"additionalProperties":false}', '{"x":1}'],
    ['{"properties":false,"additionalProperties":false}', '{"x":1}'], ['{"type":"array","items":{"items":null,"type":"array"},"enum":null}', '[[1]]'],
    ['{"type":"array","items":{"type":"object","properties":{"k":null}}}', '[{"k":1}]'],
  ] as const).map(([schemaJson, inputJson], i) => ({ name: `schema-abnormal-shape-${i}`, definitions: [tool('p', false, { schemaJson })], calls: [{ id: 'a', name: 'p', inputJson }] })),
  { name: 'configured-alias-string-entries', definitions: [tool('r', false), tool('fast', true)], context: { toolAliases: 'xr' }, calls: [call('a', '1'), call('b', '0')] },
  { name: 'configured-alias-array-entries', context: { toolAliases: ['fast', 'read'] }, calls: [call('a', '0', { delay: 1 }), call('b', '1', { delay: 2 })] },
  { name: 'configured-alias-truthy-primitive-has-no-entries', context: { toolAliases: true }, calls: [call('a', '0')] },
  { name: 'configured-alias-number-has-no-entries', context: { toolAliases: 7 }, calls: [call('a', '0')] },
  { name: 'configured-alias-array-null-target-throws', context: { toolAliases: [null] }, calls: [call('a', '0')] },
  { name: 'concurrent-error-lets-started-siblings-finish', drain: true, calls: [call('a', 'read_file', { decision: 'throw' }), call('b', 'fast', { delay: 5 }), call('c', 'slow', { delay: 1 }), call('d', 'read_file', { delay: 2 })] },
  { name: 'preview-binary-content-reports-original-bytes', context: { maxResultBytes: 1 }, calls: [call('a', 'read_file', { content: [{ type: 'image', data: 'AAAA', mimeType: 'image/png' }, { type: 'text', text: '中文' }, { type: 'pdf', data: 'QUJD', mimeType: 'application/pdf', pages: 2 }] })] },
  { name: 'preview-head-tail-separator', context: { maxResultBytes: 122 }, calls: [call('a', 'read_file', { content: [{ type: 'text', text: 'é'.repeat(30) + 'x'.repeat(41) + '中'.repeat(20) }, { type: 'json', value: { a: [1, 'ü'] } }, { type: 'file', path: '/w/a.txt', mimeType: 'text/plain', description: 'notes' }, { type: 'file', path: '/w/b' }] })] },
  { name: 'preview-tool-level-limit-overrides-context', definitions: [tool('read_file', true, { maxResultBytes: 4 }), tool('fast', true, { maxResultBytes: null })], context: { maxResultBytes: 1000 }, calls: [call('a', 'read_file', { value: 'abcdefgh' }), call('b', 'fast', { value: 'abcdefgh' })] },
];

async function run(spec: any) {
  let ms = 0;
  const events: any[] = [];
  const pending: { at: number; run: () => void }[] = [];
  const signal = new AbortController();
  if (spec.preCancel) signal.abort();
  if (spec.cancelAt !== undefined) pending.push({ at: spec.cancelAt, run: () => signal.abort() });
  const sleep = (delay: number) => new Promise<void>((resolve) => pending.push({ at: ms + delay, run: resolve }));
  const registry = new ToolRegistry();
  for (const def of spec.definitions ?? definitions) registry.register({
    name: def.name, aliases: def.aliases, description: def.name, kind: 'builtin',
    inputSchema: def.schemaJson !== undefined ? JSON.parse(def.schemaJson) : def.schema ?? { type: 'object' },
    ...('maxResultBytes' in def ? { maxResultBytes: def.maxResultBytes } : {}),
    isReadOnly: () => true,
    isConcurrencySafe: (input: any) => { if (def.predicateError) throw Error('predicate failed'); return input.safe ?? def.safe; },
    requiresUserInteraction: () => def.interaction ?? false,
    validateInput: (input: any) => {
      if (input.validationError) throw new PilotDeckToolRuntimeError('path_not_allowed', 'Path validation failed.');
      return input.validationIssues ? { ok: false, issues: input.validationIssues } : { ok: true, input };
    },
    async execute(input: any, context: any) {
      const id = context.currentToolCallId;
      events.push(['start', id, ms]);
      let abort: () => void;
      const aborted = new Promise<never>((_, reject) => {
        abort = () => reject(new PilotDeckToolRuntimeError('tool_aborted', 'Tool execution was aborted.'));
        if (!input.ignoreAbort) context.abortSignal.addEventListener('abort', abort, { once: true });
      });
      const delay = input.delay ?? 0;
      const timeout = input.useBudget ? context.subagentTimeoutMs : undefined;
      try {
        await Promise.race([sleep(timeout === undefined ? delay : Math.min(delay, timeout)), aborted]);
        if (timeout !== undefined && timeout < delay) throw new PilotDeckToolRuntimeError('tool_timeout', 'Budget expired.');
        if (input.fail === 'Error') throw Error('ordinary failure');
        if (input.fail === 'nonError') throw 'opaque failure';
        if (input.fail) throw new PilotDeckToolRuntimeError(input.fail, 'fake failure', { stderr: 'ENOENT detail' });
        events.push(['end', id, ms]);
        return { content: input.content ?? [{ type: 'text', text: input.value ?? id }], data: input.data,
          metadata: input.metadata, supplementalMessages: input.supplementalMessages };
      } catch (error) { events.push(['error', id, ms]); throw error; }
      finally { context.abortSignal.removeEventListener('abort', abort!); }
    },
  });
  for (const name of spec.hidden ?? []) registry.hide(name);
  for (const value of spec.unavailable ?? []) registry.markUnavailable({ toolName: value.toolName, code: value.code, reason: value.reason }, value.aliases);
  const permission = { async decide(_tool: any, input: any) {
    if (input.decision === 'throw') throw Error('permission failed');
    return { type: input.decision ?? 'allow', reason: { type: 'runtime', message: input.permissionMessage ?? 'fixture' },
      message: 'permission stopped', request: { options: ['allow'] }, updatedInput: input.updatedInput };
  }};
  const runtime = new ToolRuntime(registry, permission);
  const scheduler = spec.mode === 'sequential' ? new SequentialToolScheduler(runtime) : new ConcurrentToolScheduler(runtime, registry);
  const port = createToolSchedulerPort(registry, scheduler);
  const context = { cwd: '/workspace', sessionId: 's', turnId: 't', permissionMode: 'default',
    permissionContext: { mode: 'default', canPrompt: true, additionalWorkingDirectories: [] },
    now: () => new Date(epoch + ms), abortSignal: signal.signal, ...spec.context };
  const execution = spec.deadline === undefined ? {} : { operationDeadline: new Date(epoch + spec.deadline).toISOString() };
  let settled = false, outcome: any;
  const calls = spec.calls.map((c: any) => c.inputJson === undefined ? c : { id: c.id, name: c.name, input: JSON.parse(c.inputJson) });
  let settledAt: number | undefined;
  port.executeAll(calls, context, execution).then((results: any) => { outcome = { results }; settled = true; settledAt = ms; }, (e: Error) => { outcome = { error: e.message }; settled = true; settledAt = ms; });
  for (let iteration = 0; !settled && iteration < 100; iteration++) {
    // Flush the real runtime's Promise continuations before advancing virtual time.
    for (let i = 0; i < 100; i++) await Promise.resolve();
    if (settled) break;
    assert.ok(pending.length, `${spec.name}: no runnable virtual timer`);
    const next = Math.min(...pending.map(p => p.at));
    ms = next;
    const due = pending.filter(p => p.at === next);
    for (const timer of due) pending.splice(pending.indexOf(timer), 1);
    for (const timer of due) timer.run();
  }
  assert.ok(settled, spec.name);
  if (spec.drain) {
    // Promise.all has rejected; let the already-started siblings finish.
    for (let iteration = 0; pending.length && iteration < 100; iteration++) {
      for (let i = 0; i < 100; i++) await Promise.resolve();
      const next = Math.min(...pending.map(p => p.at));
      ms = next;
      const due = pending.filter(p => p.at === next);
      for (const timer of due) pending.splice(pending.indexOf(timer), 1);
      for (const timer of due) timer.run();
    }
    for (let i = 0; i < 100; i++) await Promise.resolve();
    assert.equal(pending.length, 0, spec.name);
    return clean({ ...spec, events, ...outcome, settledAt });
  }
  return clean({ ...spec, events, ...outcome });
}

try {
  const recorded = [];
  for (const scenario of scenarios) recorded.push(await run(scenario));
  // Hash the full relative-import source closure, including runtime helpers.
  const sources = new Set<string>();
  async function visit(file: string) {
    if (sources.has(file)) return;
    sources.add(file);
    const body = await readFile(join(baseline, file), 'utf8');
    for (const match of body.matchAll(/(?:\bfrom\s*|\bimport\s*\(\s*|\bimport\s+)(['"])([^'"]+)\1/g)) {
      if (!match[2].startsWith('.')) continue;
      const base = resolve(baseline, dirname(file), match[2]);
      const candidates = [base, base.replace(/\.js$/, '.ts'), base + '.ts', join(base, 'index.ts')];
      let found: string | undefined;
      for (const path of candidates) { try { if ((await lstat(path)).isFile()) { found = path; break; } } catch {} }
      assert.ok(found, `missing relative import ${file} ${match[2]}`);
      const name = relative(baseline, found).split(sep).join('/');
      assert.ok(name.startsWith('src/'));
      await visit(name);
    }
  }
  for (const entry of ['tool/registry/ToolRegistry.ts', 'tool/execution/ToolRuntime.ts', 'tool/scheduler/ConcurrentToolScheduler.ts', 'tool/scheduler/SequentialToolScheduler.ts', 'agent/modules/capability/toolSchedulerAdapter.ts']) await visit('src/' + entry);
  const sourceSha256: Record<string, string> = {};
  for (const source of [...sources].sort()) sourceSha256[source] = createHash('sha256').update(await readFile(join(baseline, source))).digest('hex');
  bindSourceTree(baseline, commit, git);
  await mkdir(outputParent, { recursive: true });
  const output = await mkdtemp(join(outputParent, 'tool-runtime-'));
  await writeFile(join(output, 'oracle.json'), JSON.stringify({ sourceCommit: commit, sourceSha256, definitions, scenarios: recorded }, null, 2) + '\n', { flag: 'wx' });
  console.log(`recorded ${recorded.length} runtime scenarios from ${sources.size} bound TS source files into ${output}`);
} finally {
  const current = await lstat(scratch);
  assert.equal(current.dev, owner.dev); assert.equal(current.ino, owner.ino);
  assert.equal(dirname(scratch), realpathSync(tmpdir()));
  assert.ok(scratch.split(sep).at(-1)!.startsWith('pdrs-tool-runtime-'));
  await rm(scratch, { recursive: true });
}
