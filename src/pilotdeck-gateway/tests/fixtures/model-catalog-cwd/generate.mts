// Original catalog entry; a fresh child keeps Node's cwd cache cold per case.
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { mkdir, mkdtemp, readFile, realpath, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { assertBaseline, baselineSourceHashes } from '../_shared/baseline.mts';

assert.equal(process.version, 'v22.22.0');
assert.ok(process.env.PDRS_BASELINE);
const baseline = resolve(process.env.PDRS_BASELINE);
const sources = ['src/gateway/dialog/modelCatalog.ts', 'src/pilot/config/loadPilotConfig.ts',
  'src/pilot/paths.ts', 'src/gateway/server/GatewayWsConnection.ts'];
const cases = ['relative', 'empty', 'absolute', 'tilde', 'tilde-child', 'unset']
  .flatMap(kind => ['normal', 'deleted'].map(state => `${kind}-${state}`));

if (process.argv[2] === '--child') {
  const name = process.argv[3];
  assert.ok(cases.includes(name));
  const { listModelCatalog } = await import(pathToFileURL(join(baseline, sources[0])).href);
  const { GatewayWsConnection } = await import(pathToFileURL(join(baseline, sources[3])).href);
  const config = await readFile(new URL('./config.json', import.meta.url));
  const root = await realpath(await mkdtemp(join(tmpdir(), 'pdrs-model-cwd-')));
  const cwd = join(root, 'cwd');
  const user = join(root, 'user');
  const kind = name.replace(/-(normal|deleted)$/, '');
  const inputs = { relative: '../home', empty: '', absolute: `${root}/home/../home`,
    tilde: '~', 'tilde-child': '~/nested/../home' };
  const homes = { relative: join(root, 'home'), empty: cwd, absolute: join(root, 'home'),
    tilde: user, 'tilde-child': join(user, 'home'), unset: join(user, '.pilotdeck') };
  await mkdir(cwd, { recursive: true });
  await mkdir(user, { recursive: true });
  await mkdir(homes[kind], { recursive: true });
  await writeFile(join(homes[kind], 'pilotdeck.yaml'), config, { flag: 'wx' });
  process.env.HOME = user;
  const env: Record<string, string> = { HOME: user };
  if (kind !== 'unset') env.PILOT_HOME = inputs[kind];
  let outcome: unknown;
  const frames: unknown[] = [];
  const connection = new GatewayWsConnection({
    onMessage() {}, onClose() {}, close() {},
    sendText(text: string) { frames.push(JSON.parse(text)); },
  }, { gateway: { async modelCatalogList(input: any) { return listModelCatalog(input, env); } } as any,
    token: 'isolated', serverVersion: 'fixture' });
  let cwdReads = 0;
  const originalCwd = process.cwd;
  process.chdir(cwd);
  if (name.endsWith('-deleted')) await rm(cwd, { recursive: true });
  process.cwd = () => { cwdReads++; return originalCwd(); };
  try {
    outcome = { result: listModelCatalog({}, env) };
  } catch (error: any) {
    outcome = { error: { name: error.name, code: error.code, message: error.message, syscall: error.syscall } };
  }
  try {
    // Call the original request boundary, including structured error framing.
    await (connection as any).handleRequest({ type: 'request', id: name,
      method: 'model_catalog_list', params: {} });
    assert.equal(frames.length, 1);
  } finally {
    process.cwd = originalCwd;
    process.chdir(root);
    await rm(root, { recursive: true });
  }
  process.stdout.write(JSON.stringify({ name, cwdReads, outcome, frame: frames[0] }).split(root).join('<root>') + '\n');
} else {
  assert.ok(process.argv[2], 'output directory required');
  const output = resolve(process.argv[2]);
  const sourceCommit = assertBaseline(baseline);
  const sourceSha256 = await baselineSourceHashes(baseline, sources);
  const rows = cases.map(name => {
    const raw = execFileSync(process.execPath, [
      '--disable-warning=ExperimentalWarning', '--import', join(baseline, 'node_modules/tsx/dist/loader.mjs'),
      fileURLToPath(import.meta.url), '--child', name,
    ], { encoding: 'utf8', timeout: 30_000 });
    process.stdout.write(raw);
    return JSON.parse(raw);
  });
  await mkdir(output, { recursive: true });
  await writeFile(join(output, 'oracle.json'), JSON.stringify({
    node: process.version, sourceCommit, sourceSha256, cases: rows,
  }, null, 2) + '\n', { flag: 'wx' });
}
