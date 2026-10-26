// Real public TS methods; inject only the asynchronous work they memoize.
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { readFile, mkdir, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { assertBaseline, baselineSourceHashes } from '../_shared/baseline.mts';
const baseline = resolve(process.env.PDRS_BASELINE!);
const sourceCommit = assertBaseline(baseline);
const sources = ['src/mcp/client/McpClient.ts', 'src/gateway/client/InProcessGateway.ts'];
const sourceSha256 = await baselineSourceHashes(baseline, sources);
const require = createRequire(join(baseline, 'package.json'));
const { transformSync } = createRequire(require.resolve('tsx'))('esbuild');
async function load(source: string) {
  const module = { exports: {} as any };
  const code = transformSync(await readFile(join(baseline, source), 'utf8'), { loader: 'ts', format: 'cjs' }).code;
  new Function('require', 'module', 'exports', code)((name: string) => {
    if (name.startsWith('node:')) return require(name);
    return new Proxy({}, { get: (_, key) => {
      if (key === '__esModule') return true;
      throw Error(`unexpected import ${name}.${String(key)}`);
    } });
  }, module, module.exports);
  return module.exports;
}
const { McpClient } = await load(sources[0]);
const { InProcessGateway } = await load(sources[1]);
const cases: any[] = [];
const deadline = setTimeout(() => { throw Error('probe exceeded 5 seconds'); }, 5000);
for (const scope of ['start', 'delivery']) for (const mode of ['panic-first', 'panic-gated', 'error', 'success']) {
  let release!: () => void, calls = 0;
  const gate = new Promise<void>(r => { release = r; });
  const result = scope === 'start' ? undefined : { invocationId: 'i', status: 'delivered' };
  const work = async () => {
    calls++;
    if (mode !== 'panic-first') await gate;
    if (mode !== 'success') throw Error('ordinary failure');
    return result;
  };
  const pending: any = { sessionKey: 's', invocationId: 'i', hookEvent: 'e', expiresAt: Infinity };
  const owner = scope === 'start' ? Object.create(McpClient.prototype) : Object.create(InProcessGateway.prototype);
  Object.assign(owner, { connectPromise: null, runConnect: work,
    asyncHookOutcomes: new Map(), pendingAsyncHooks: new Map([['i', pending]]), deliverAsyncHookContext: work });
  const invoke = () => scope === 'start' ? owner.start() : owner.submitAsyncHookResult({ sessionKey: 's', invocationId: 'i', output: {} });
  const capture = (p: Promise<any>) => p.then(v => v?.status ?? 'ok', (e: Error) => e.message);
  const first = capture(invoke()), second = capture(invoke());
  const cancelled = new AbortController();
  const abandoned = Promise.race([capture(invoke()), new Promise(r => cancelled.signal.addEventListener('abort', () => r('cancelled'), { once: true }))]);
  cancelled.abort();
  assert.equal(await abandoned, 'cancelled');
  assert.equal(calls, 1, 'concurrent callers share one operation');
  release();
  const values = await Promise.all([first, second]);
  const cached = Boolean(scope === 'start' ? owner.connectPromise : pending.delivery);
  assert.equal(cached, scope === 'start' && mode === 'success');
  owner.runConnect = owner.deliverAsyncHookContext = async () => { calls++; return result; };
  const retry = await capture(invoke());
  assert.equal(retry, scope === 'start' ? 'ok' : 'delivered');
  cases.push({ scope, mode, values, cached, calls, retry });
}
clearTimeout(deadline);
const output = resolve(process.argv[2]);
await mkdir(output, { recursive: true });
await writeFile(join(output, 'oracle.json'), JSON.stringify({ sourceCommit, sourceSha256, cases }) + '\n', { flag: 'wx' });
console.log(`TS public-method matrix: ${cases.length} cases passed; shared callers, cancelled observer, cache and retry checked`);
