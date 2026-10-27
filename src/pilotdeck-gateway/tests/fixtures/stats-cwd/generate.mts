// Original stats entry; a fresh child keeps Node's cwd cache cold per case.
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { mkdir, mkdtemp, realpath, rm, writeFile } from 'node:fs/promises';
import { existsSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { assertBaseline, baselineSourceHashes } from '../_shared/baseline.mts';

assert.equal(process.version, 'v22.22.0');
assert.ok(process.env.PDRS_BASELINE);
const baseline = resolve(process.env.PDRS_BASELINE);
const sources = ['src/router/stats/TokenStatsCollector.ts', 'src/pilot/paths.ts',
  'src/router/RouterRuntime.ts', 'src/router/usage/RouterUsageObserver.ts'];
const kinds = ['relative', 'empty', 'absolute', 'tilde', 'tilde-child', 'unset',
  'empty-home-tilde', 'empty-home-unset', 'empty-home-child', 'file-path', 'disabled'];
const cases = kinds.flatMap(kind => ['normal', 'deleted'].map(state => `${kind}-${state}`));
const entries = ['collector', 'runtime'];

if (process.argv[2] === '--child') {
  const name = process.argv[3];
  const entry = process.argv[4];
  assert.ok(cases.includes(name));
  assert.ok(entries.includes(entry));
  const { TokenStatsCollector } = await import(pathToFileURL(join(baseline, sources[0])).href);
  const { createRouterRuntime } = await import(pathToFileURL(join(baseline, sources[2])).href);
  const root = await realpath(await mkdtemp(join(tmpdir(), 'pdrs-stats-cwd-')));
  const cwd = join(root, 'cwd');
  const user = join(root, 'user');
  const kind = name.replace(/-(normal|deleted)$/, '');
  const inputs: Record<string, string> = { relative: '../home', empty: '',
    absolute: `${root}/home/../home`, tilde: '~', 'tilde-child': '~/nested/../home',
    'empty-home-tilde': '~', 'empty-home-child': '~/data',
    'file-path': '../home', disabled: '../home' };
  const homes: Record<string, string> = {
    relative: join(root, 'home'), empty: cwd, absolute: join(root, 'home'),
    tilde: user, 'tilde-child': join(user, 'home'), unset: join(user, '.pilotdeck'),
    'empty-home-tilde': cwd, 'empty-home-unset': join(cwd, '.pilotdeck'),
    'empty-home-child': join(cwd, 'data'),
    'file-path': join(root, 'custom'), disabled: join(root, 'home') };
  await mkdir(cwd, { recursive: true });
  await mkdir(user, { recursive: true });
  await mkdir(homes[kind]!, { recursive: true });
  process.env.HOME = kind.startsWith('empty-home-') ? '' : user;
  if (kind in inputs) process.env.PILOT_HOME = inputs[kind]!;
  else delete process.env.PILOT_HOME;
  const filePath = kind === 'file-path' ? join(homes[kind]!, 'stats.jsonl') : undefined;
  const enabled = kind !== 'disabled';
  // Bare `~` with HOME="" lets Node's `normalizeHomePath` return "" verbatim;
  // the constructor then keeps the relative `router` path (no cwd read).
  const journal = filePath ?? (enabled
    ? (kind === 'empty-home-tilde'
      ? join('router', 'stats.jsonl')
      : join(homes[kind]!, 'router', 'stats.jsonl'))
    : null);
  let outcome: unknown;
  const originalCwd = process.cwd;
  process.chdir(cwd);
  if (name.endsWith('-deleted')) await rm(cwd, { recursive: true });
  let cwdReads = 0;
  process.cwd = () => { cwdReads++; return originalCwd(); };
  try {
    try {
      if (entry === 'collector') {
        new TokenStatsCollector({ enabled, filePath });
      } else {
        const runtime = createRouterRuntime(
          { enabled: true, stats: { enabled, filePath } } as any,
          { modelInvoker: {
            getCapabilities: () => ({}), getMultimodal: () => ({ input: ['text'] }),
          } as any },
        );
        await runtime.shutdown();
      }
      outcome = { result: { journal, exists: journal !== null && existsSync(journal) } };
    } catch (error: any) {
      outcome = { error: { name: error.name, code: error.code, message: error.message,
        syscall: error.syscall } };
    }
  } finally {
    process.cwd = originalCwd;
    process.chdir(root);
    await rm(root, { recursive: true });
  }
  process.stdout.write(JSON.stringify({ name, entry, cwdReads, outcome })
    .split(root).join('<root>') + '\n');
} else {
  assert.ok(process.argv[2], 'output directory required');
  const output = resolve(process.argv[2]);
  const sourceCommit = assertBaseline(baseline);
  const sourceSha256 = await baselineSourceHashes(baseline, sources);
  const rows = cases.map(name => {
    const row: Record<string, unknown> = { name };
    for (const entry of entries) {
      const raw = execFileSync(process.execPath, [
        '--disable-warning=ExperimentalWarning', '--import', join(baseline, 'node_modules/tsx/dist/loader.mjs'),
        fileURLToPath(import.meta.url), '--child', name, entry,
      ], { encoding: 'utf8', timeout: 30_000 });
      process.stdout.write(raw);
      row[entry] = JSON.parse(raw);
    }
    return row;
  });
  await mkdir(output, { recursive: true });
  await writeFile(join(output, 'oracle.json'), JSON.stringify({
    node: process.version, sourceCommit, sourceSha256, cases: rows,
  }, null, 2) + '\n', { flag: 'wx' });
}
