// Probe the original boot entry and home helper separately, with cold cwd caches.
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { mkdir, mkdtemp, realpath, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { assertBaseline, baselineSourceHashes } from '../_shared/baseline.mts';

assert.equal(process.version, 'v22.22.0');
assert.ok(process.env.PDRS_BASELINE);
const baseline = resolve(process.env.PDRS_BASELINE);
const sources = ['src/cli/LocalGatewayBootConfig.ts', 'src/pilot/paths.ts',
  'src/cli/createLocalGateway.ts', 'src/cli/pilotdeck.ts'];
const cases = ['relative', 'tilde', 'tilde-child', 'absolute', 'unset', 'empty']
  .flatMap(kind => ['normal', 'deleted'].map(state => `${kind}-${state}`));

if (process.argv[2] === '--child') {
  const [name, entry] = process.argv.slice(3);
  assert.ok(cases.includes(name));
  assert.ok(['boot', 'home'].includes(entry));
  const { resolveLocalGatewayBootConfig } = await import(pathToFileURL(join(baseline, sources[0])).href);
  const { resolvePilotHome } = await import(pathToFileURL(join(baseline, sources[1])).href);
  const root = await realpath(await mkdtemp(join(tmpdir(), 'pdrs-metadata-env-')));
  const cwd = join(root, 'cwd');
  const home = join(root, 'user');
  await mkdir(cwd);
  await mkdir(home);
  process.env.HOME = home;
  const env: Record<string, string> = { HOME: home };
  const kind = name.replace(/-(normal|deleted)$/, '');
  const inputs = { relative: 'relative/../home', tilde: '~', 'tilde-child': '~/nested/../home',
    absolute: `${root}/home/../home`, empty: '' };
  if (kind !== 'unset') env.PILOT_HOME = inputs[kind];
  let outcome: unknown;
  let cwdReads = 0;
  const originalCwd = process.cwd;
  process.chdir(cwd);
  if (name.endsWith('-deleted')) await rm(cwd, { recursive: true });
  process.cwd = () => { cwdReads++; return originalCwd(); };
  try {
    if (entry === 'boot') {
      // Do not supply the second argument: the real default reads process.cwd().
      const boot = resolveLocalGatewayBootConfig({ env, builtinSkillsRoot: join(root, 'skills') });
      outcome = { home: boot.pilotHome, cwd: boot.projectRoot };
    } else {
      outcome = { home: resolvePilotHome(env) };
    }
  } catch (error: any) {
    outcome = { error: { name: error.name, code: error.code, message: error.message, syscall: error.syscall } };
  } finally {
    process.cwd = originalCwd;
    process.chdir(root);
    await rm(root, { recursive: true });
  }
  process.stdout.write(JSON.stringify({ cwdReads, outcome }).split(root).join('<root>') + '\n');
} else {
  assert.ok(process.argv[2], 'output directory required');
  const output = resolve(process.argv[2]);
  const sourceCommit = assertBaseline(baseline);
  const sourceSha256 = await baselineSourceHashes(baseline, sources);
  const rows = cases.map(name => {
    const row: Record<string, unknown> = { name };
    for (const entry of ['boot', 'home']) {
      const raw = execFileSync(process.execPath, [
        '--disable-warning=ExperimentalWarning', '--import', join(baseline, 'node_modules/tsx/dist/loader.mjs'),
        fileURLToPath(import.meta.url), '--child', name, entry,
      ], { encoding: 'utf8', timeout: 30_000 });
      row[entry] = JSON.parse(raw);
    }
    process.stdout.write(JSON.stringify(row) + '\n');
    return row;
  });
  await mkdir(output, { recursive: true });
  await writeFile(join(output, 'oracle.json'), JSON.stringify({
    node: process.version, sourceCommit, sourceSha256, cases: rows,
  }, null, 2) + '\n', { flag: 'wx' });
}
