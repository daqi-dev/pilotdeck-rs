// Real fixed-baseline methods; no registry logic is reimplemented here.
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { realpathSync } from 'node:fs';
import { lstat, mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { basename, dirname, join, relative, resolve, sep } from 'node:path';
import { pathToFileURL } from 'node:url';
import { bindSourceTree } from '../../baseline_source_guard.mjs';

assert.ok(process.env.PDRS_BASELINE, 'set PDRS_BASELINE');
assert.ok(process.argv[2], 'provide output directory');
const baseline = resolve(process.env.PDRS_BASELINE);
const output = resolve(process.argv[2]);
const commit = 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';
const git = realpathSync(process.env.PDRS_GIT || execFileSync('which', ['git'], { encoding: 'utf8' }).trim());
const gitEnv = Object.fromEntries(Object.entries(process.env).filter(([key]) => !key.startsWith('GIT_')));
const runGit = (...args: string[]) => execFileSync(git, ['--no-replace-objects', '-C', baseline, ...args], { env: gitEnv, maxBuffer: 32 * 1024 * 1024 });
// bindSourceTree pins HEAD and every tracked src byte; the whole tracked tree
// (package.json, tsconfig, ...) must also be clean, before and after the run.
const assertTrackedClean = () => {
  const changes = runGit('status', '--porcelain', '--untracked-files=no').toString();
  assert.equal(changes, '', `baseline has uncommitted changes to tracked files:\n${changes}`);
};
const sourceCommit = bindSourceTree(baseline, commit, git).ref;
assertTrackedClean();
const sources = ['src/gateway/client/InProcessGateway.ts', 'src/agent/session/SteerMailbox.ts', 'src/gateway/dialog/GatewayAttachmentTurnComposer.ts'];
// Like workspace-snapshot: hash the relative-import closure of the entries,
// not the entries alone. Bare and node: specifiers are external.
async function resolveSource(absolute: string): Promise<string | undefined> {
  const stem = absolute.endsWith('.js') ? absolute.slice(0, -3) : undefined;
  for (const candidate of [absolute, `${absolute}.ts`, `${absolute}.tsx`, `${absolute}.mts`, join(absolute, 'index.ts'), ...(stem ? [`${stem}.ts`, `${stem}.tsx`] : [])]) {
    try { if ((await lstat(candidate)).isFile()) return candidate; } catch { /* next candidate */ }
  }
  return undefined;
}
const closure = new Set<string>(sources);
for (const queue = [...sources]; queue.length > 0;) {
  const file = queue.shift()!;
  const text = await readFile(join(baseline, file), 'utf8');
  for (const [, , specifier] of text.matchAll(/(?:\bfrom\s*|\bimport\s*\(\s*|\bimport\s+)(['"])([^'"]+)\1/g)) {
    if (!specifier.startsWith('./') && !specifier.startsWith('../')) continue;
    const resolved = await resolveSource(join(baseline, dirname(file), specifier));
    assert.ok(resolved, `unresolved baseline import ${specifier} in ${file}`);
    const path = relative(baseline, resolved).split(sep).join('/');
    if (!closure.has(path)) { closure.add(path); queue.push(path); }
  }
}
const sourceSha256: Record<string, string> = {};
for (const file of [...closure].sort()) {
  assert.ok(file.startsWith('src/'), `import closure escaped src/: ${file}`);
  const committed = runGit('show', `${commit}:${file}`);
  assert.deepEqual(await readFile(join(baseline, file)), committed, `${file} differs from ${commit}`);
  sourceSha256[file] = createHash('sha256').update(committed).digest('hex');
}
const { InProcessGateway } = await import(pathToFileURL(join(baseline, sources[0])).href);
const { SteerMailbox } = await import(pathToFileURL(join(baseline, sources[1])).href);
const { GatewayAttachmentTurnComposer } = await import(pathToFileURL(join(baseline, sources[2])).href);
const scratch = await mkdtemp(join(realpathSync(tmpdir()), 'pdrs-async-hooks-'));
const originalCwd = process.cwd();
const originalNow = Date.now, originalTimeout = globalThis.setTimeout, originalClear = globalThis.clearTimeout;
const clone = (v: any) => JSON.parse(JSON.stringify(v));
const capture = async (f: () => any) => {
  try { return { ok: true, value: clone((await f()) ?? null) }; }
  catch (e: any) { return { ok: false, error: e.message, ...(e.code ? { code: e.code } : {}) }; }
};
const start = (run = 'r', session = 's') => ({ op: 'start', run, session });
const reg = (id = 'a', extra = {}) => ({ op: 'register', input: { sessionKey: 's', invocationId: id, hookName: 'hook', hookEvent: 'PreToolUse', includeHookEvents: true, timeoutMs: 10, ...extra } });
const sub = (id = 'a', output: any = { hookSpecificOutput: { hookEventName: 'PreToolUse', additionalContext: 'context' } }, session = 's') => ({ op: 'submit', input: { sessionKey: session, invocationId: id, output } });
const tick = (ms: number) => ({ op: 'tick', ms });
const now = (ms: number) => ({ op: 'now', ms });
const expire = (run = 'r', session = 's') => ({ op: 'expire', run, session });
const drain = { op: 'drain', session: 's', run: 'r' };
const block = { op: 'block' }, wait = { op: 'wait' }, release = { op: 'release' };
const begin = (key: string, action: any) => ({ op: 'begin', key, action });
const joinOp = (key: string) => ({ op: 'join', key });
const specs = [
  { name: 'delivered-duplicate-unknown-session', steps: [sub(),start(),reg(),sub('a', {}, 'other'),sub(),sub('a', null),drain,sub('missing')] },
  { name: 'empty-context-no-steer', steps: [start(),reg(),sub('a', {}),drain,sub(),reg('b'),sub('b',{hookSpecificOutput:{hookEventName:'PreToolUse'}}),drain] },
  { name: 'explicit-empty-string-still-steers', steps: [start(),reg(),sub('a',{hookSpecificOutput:{hookEventName:'PreToolUse',additionalContext:''}}),drain] },
  ...[9,10,11].map(ms => ({ name: `expiry-timer-${ms}`, steps: [start(),reg(),tick(ms),sub(),drain] })),
  ...[9,10,11].map(ms => ({ name: `expiry-check-${ms}`, steps: [start(),reg(),now(ms),sub(),drain] })),
  ...[299999,300000,300001].map(ms => ({ name: `retention-delivered-${ms}`, steps: [start(),reg(),sub('a',{}),tick(ms),sub()] })),
  ...[299999,300000,300001].map(ms => ({ name: `retention-expired-${ms}`, steps: [start(),reg(),tick(10),tick(ms),sub()] })),
  { name: 'retention-waits-for-timer', steps: [start(),reg(),sub('a',{}),now(300001),sub(),tick(0),sub()] },
  { name: 'registration-ownership-no-refresh', steps: [reg(),start(),reg('',{}),reg(' \ufeff'),reg(),now(9),reg('a',{timeoutMs:100,hookName:'ignored'}),start('next'),reg(),start('r','other'),reg('a',{sessionKey:'other'}),now(1),sub()] },
  { name: 'minimum-and-default-timeout', steps: [start(),reg('a',{timeoutMs:0}),reg('b',{timeoutMs:-4}),reg('c',{timeoutMs:undefined}),tick(1),sub(),sub('b'),tick(59998),sub('c',{}),reg('d',{timeoutMs:undefined}),tick(60000),sub('d')] },
  { name: 'minimum-timeout-deadline-check', steps: [start(),reg('a',{timeoutMs:0}),reg('b',{timeoutMs:-4}),reg('c',{timeoutMs:0.5}),sub('a',{}),now(0.5),sub('b',{}),now(0.5),sub('c',{})] },
  { name: 'fractional-timer-fires-before-expiry', steps: [start(),reg('a',{timeoutMs:1.9}),tick(1),sub()] },
  { name: 'old-run-context-fence', steps: [start(),reg(),start('next'),sub(),{...drain,run:'next'}] },
  { name: 'old-run-empty-fence', steps: [start(),reg(),start('next'),sub('a',{}),{...drain,run:'next'}] },
  { name: 'no-active-run', steps: [start(),reg(),{op:'finish',session:'s',run:'r'},sub()] },
  { name: 'closing-mailbox', steps: [start(),reg(),{op:'close',session:'s',run:'r'},sub(),sub()] },
  { name: 'expire-only-matching-turn', steps: [start(),start('r','other'),reg(),reg('b',{sessionKey:'other'}),start('next'),reg('c'),expire(),sub(),sub('b',{},'other'),sub('c',{}),expire(),expire('next'),sub('c')] },
  { name: 'outcome-shadows-reused-id', steps: [start(),reg(),sub('a',{}),reg('a',{timeoutMs:400000}),sub(),tick(300000),sub(),drain] },
  { name: 'expired-outcome-shadows-new-owner', steps: [start(),reg(),expire(),start('n','other'),reg('a',{sessionKey:'other',timeoutMs:400000}),sub('a',{},'other'),sub(),tick(300000),sub('a',{},'other')] },
  { name: 'events-disabled', steps: [start(),reg('a',{includeHookEvents:false}),sub(),reg('b',{includeHookEvents:false}),tick(10),sub('b')] },
  { name: 'concurrent-empty', steps: [start(),reg(),begin('one',sub('a',{})),begin('two',sub('a',{})),joinOp('one'),joinOp('two'),drain] },
  { name: 'concurrent-duplicate', steps: [start(),reg(),block,begin('one',sub()),wait,begin('two',sub('a',null)),release,joinOp('one'),joinOp('two'),drain] },
  { name: 'run-switch-during-write', steps: [start(),reg(),block,begin('one',sub()),wait,begin('two',sub()),start('next'),release,joinOp('one'),joinOp('two'),{...drain,run:'next'}] },
  { name: 'expiry-during-write-then-accepted', steps: [start(),reg(),block,begin('one',sub()),wait,tick(10),sub(),tick(5),release,joinOp('one'),sub(),drain,tick(299999),sub(),tick(1),sub()] },
  { name: 'delivery-failure-retry', steps: [start(),reg(),{op:'fail'},sub(),sub(),drain] },
  { name: 'shared-delivery-failure-retry', steps: [start(),reg(),block,{op:'fail'},begin('one',sub()),wait,begin('two',sub()),release,joinOp('one'),joinOp('two'),sub(),drain] },
  { name: 'identity-and-payload-validation', steps: [sub('',{}),sub('a',{},' \ufeff'),start(),reg(),...[null,[],false,'x',{continue:false},{hookSpecificOutput:null},{hookSpecificOutput:[]},{hookSpecificOutput:{decision:'block'}},{hookSpecificOutput:{}},{hookSpecificOutput:{hookEventName:'Stop'}},{hookSpecificOutput:{hookEventName:{toString:42}}},{hookSpecificOutput:{hookEventName:[{toString:42}]}},{hookSpecificOutput:{hookEventName:['a',null,{}]}},{hookSpecificOutput:{hookEventName:'PreToolUse',additionalContext:1}}].map(output=>sub('a',output)),sub(),drain] },
  { name: 'deep-event-conversion', steps: [start(),reg(),{op:'deepSubmit',depth:5000},sub('a',{})] },
  { name: 'utf16-identities-context', steps: [start('\ud800','s\ud801'),reg('a\ud802',{sessionKey:'s\ud801',hookEvent:'X\ud803',hookName:'h\ud804'}),sub('a\ud802',{hookSpecificOutput:{hookEventName:'X\ud803',additionalContext:'\ud805<&'}},'s\ud801'),{op:'drain',session:'s\ud801',run:'\ud800'}] },
];
async function run(spec: any) {
  let time = 0, counter = 0;
  const timers = new Map<number, any>();
  Date.now = () => time;
  globalThis.setTimeout = ((callback: any, delay: number) => {
    const id = ++counter;
    const token = { id, unref() { return this; } };
    timers.set(id,{ token, at: time + (delay >= 1 && delay <= 2147483647 ? Math.trunc(delay) : 1), callback });
    return token;
  }) as any;
  globalThis.clearTimeout = ((token: any) => { timers.delete(token?.id); }) as any;
  const advance = (ms: number) => {
    const target = time + ms;
    for (;;) {
      const next = [...timers.values()].filter(t=>t.at<=target).sort((a,b)=>a.at-b.at||a.token.id-b.token.id)[0];
      if (!next) break;
      time = Math.max(time,next.at); timers.delete(next.token.id); next.callback();
    }
    time = target;
  };
  const mailboxes = new Map<string, any>(), active = new Map<string,string>();
  const mutations: any[] = [], events: any[] = [], pending = new Map<string, any>();
  let gate: any, fail = false;
  const mailbox = (session: string) => {
    if (!mailboxes.has(session)) mailboxes.set(session,new SteerMailbox({ recordMutation: async (turn: string, mutation: any) => {
      mutations.push(clone({ session, turn, mutation }));
      if (gate && mutation.mutation === 'insert') { const g = gate; gate = undefined; g.enter(); await g.blocked; }
      if (fail && mutation.mutation === 'insert') { fail = false; throw Error('fixture storage failure'); }
    }}));
    return mailboxes.get(session);
  };
  const gateway: any = Object.create(InProcessGateway.prototype);
  gateway.pendingAsyncHooks = new Map(); gateway.asyncHookOutcomes = new Map();
  gateway.router = {
    activeTurnRunId: (session: string) => active.get(session),
    steer: (session: string, input: any) => { const { turnId, ...message } = input; return mailbox(session).enqueue(turnId,message); },
  };
  // Real attachment composer (steerTurn passes no attachments). The install
  // command is only used for attachment notes; pin it so no package lookup runs.
  // Registry, context validation, run fence, steerTurn, prepareAttachmentTurn,
  // mailbox, timer callbacks and settlement are all original TS.
  gateway.options = { funasrInstallCommand: 'fixture-unused' };
  gateway.attachmentTurnComposer = new GatewayAttachmentTurnComposer();
  gateway.emitForSession = (session: string, event: any) => events.push(clone({session,event}));
  const invoke = (step: any): any => {
    switch (step.op) {
      case 'start': active.set(step.session,step.run); return mailbox(step.session).start(step.run);
      case 'finish': active.delete(step.session); mailbox(step.session).finish(step.run); return;
      case 'register': return gateway.registerAsyncHook(step.input);
      case 'submit': return gateway.submitAsyncHookResult(step.input);
      case 'deepSubmit': { let event: any = 'event'; for(let i=0;i<step.depth;i++)event=[event]; return gateway.submitAsyncHookResult({sessionKey:'s',invocationId:'a',output:{hookSpecificOutput:{hookEventName:event}}}); }
      case 'tick': return advance(step.ms);
      case 'now': time += step.ms; return;
      case 'expire': return gateway.expireAsyncHooksForTurn(step.session,step.run);
      case 'drain': return mailbox(step.session).drain(step.run);
      case 'close': return mailbox(step.session).drainOrClose(step.run);
      default: throw Error('unknown op '+step.op);
    }
  };
  const steps: any[] = [];
  let blocked: any;
  for (const step of spec.steps) {
    let result: any = null;
    if (step.op === 'block') {
      gate = {}; blocked = gate;
      gate.entered = new Promise(r=>{gate.enter=r}); gate.blocked = new Promise(r=>{gate.release=r});
    } else if (step.op === 'wait') await blocked.entered;
    else if (step.op === 'release') blocked.release();
    else if (step.op === 'fail') fail = true;
    else if (step.op === 'begin') pending.set(step.key,capture(()=>invoke(step.action)));
    else if (step.op === 'join') { result = await pending.get(step.key); pending.delete(step.key); }
    else result = await capture(()=>invoke(step));
    steps.push({ ...clone(step), result });
  }
  assert.equal(pending.size,0,'unjoined operation');
  return {name:spec.name,steps,events,mutations};
}
try {
  process.chdir(scratch);
  const scenarios = [];
  for (const spec of specs) scenarios.push(await run(spec));
  assert.equal(scenarios.length,37);
  bindSourceTree(baseline,commit,git);
  assertTrackedClean();
  const data = JSON.stringify({sourceCommit,sourceSha256,scenarios},null,2)+'\n';
  for (const path of [scratch,baseline,output]) assert.ok(!data.includes(path),'fixture path leak');
  await mkdir(output,{recursive:true});
  await writeFile(join(output,'oracle.json'),data);
  console.log(`oracle.json: ${scenarios.length} scenarios, ${scenarios.reduce((n,s)=>n+s.steps.length,0)} steps`);
} finally {
  process.chdir(originalCwd); Date.now=originalNow; globalThis.setTimeout=originalTimeout; globalThis.clearTimeout=originalClear;
  assert.equal(dirname(scratch),realpathSync(tmpdir())); assert.ok(basename(scratch).startsWith('pdrs-async-hooks-'));
  await rm(scratch,{recursive:true,force:true});
}
