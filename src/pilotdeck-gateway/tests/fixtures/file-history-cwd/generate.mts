// Every case imports the real entry before chdir and starts with a cold cwd cache.
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { chmod, mkdir, mkdtemp, readFile, realpath, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { assertBaseline, baselineSourceHashes } from '../_shared/baseline.mts';

assert.equal(process.version, 'v22.22.0');
assert.ok(process.env.PDRS_BASELINE, '#879 PDRS_BASELINE required');
const baseline = resolve(process.env.PDRS_BASELINE);
const sources = ['src/session/filesystem/FileHistoryStore.ts',
  'src/session/filesystem/createBackup.ts', 'src/session/filesystem/backupNaming.ts'];
const cases = ['track', 'mark'].flatMap(entry => ['relative', 'absolute']
  .flatMap(kind => ['normal', 'deleted'].map(state => `${entry}-${kind}-${state}`)))
  .concat(['mark-missing-relative-normal', 'mark-missing-relative-deleted',
    'track-relative-warmed-deleted', 'mark-relative-warmed-deleted']);

if (process.argv[2] === '--child') {
  const name = process.argv[3];
  assert.ok(cases.includes(name), '#879 known case');
  const { FileHistoryStore } = await import(pathToFileURL(join(baseline, sources[0])).href);
  const root = await realpath(await mkdtemp(join(tmpdir(), 'pdrs-file-history-cwd-')));
  const cwd = join(root, 'cwd');
  const file = join(root, 'files/tracked.txt');
  const backupDir = join(root, 'backups');
  await mkdir(cwd);
  await mkdir(join(root, 'files'));
  await writeFile(file, 'original\n', { flag: 'wx' });
  await chmod(file, 0o640);
  const records: string[] = [];
  const store = new FileHistoryStore({ backupDir, now: () => new Date(1700000000000),
    onSnapshotRecorded(_entry: unknown, kind: string) { records.push(kind); } });
  const mark = name.startsWith('mark-');
  if (mark && !name.includes('-missing-')) await store.trackEdit(file, 'message');
  const backupName = createHash('sha256').update(file).digest('hex').slice(0, 16) + '@v1';
  async function view() {
    const state = store.getState();
    return {
      trackedFiles: [...state.trackedFiles],
      snapshots: await Promise.all(state.snapshots.map(async (snapshot: any) => ({
        messageId: snapshot.messageId, timestamp: snapshot.timestamp.getTime(),
        backups: await Promise.all(Object.entries(snapshot.trackedFileBackups).map(async ([path, b]: any) => ({
          path, nameMatches: b.backupFileName === backupName, version: b.version,
          backupTime: b.backupTime.getTime(), mode: b.mode & 0o777,
          content: await readFile(join(backupDir, b.backupFileName), 'utf8'),
        }))),
        expected: Object.entries(snapshot.expectedFileStates ?? {}).map(([path, state]) => ({ path, state })),
      }))),
      records: [...records],
    };
  }
  const before = await view();
  const input = name.includes('-absolute-') ? `${root}//files/nested/../tracked.txt`
    : '../files/./nested/../tracked.txt';
  const originalCwd = process.cwd;
  let cwdReads = 0;
  let outcome: unknown;
  process.chdir(cwd);
  if (name.includes('-warmed-')) process.cwd();
  if (name.endsWith('-deleted')) await rm(cwd, { recursive: true });
  process.cwd = () => { cwdReads++; return originalCwd(); };
  try {
    if (mark) await store.markEditCommitted(input, 'message');
    else await store.trackEdit(input, 'message');
    outcome = { ok: true };
  } catch (error: any) {
    outcome = { error: { name: error.name, code: error.code, message: error.message, syscall: error.syscall } };
  }
  try {
    const after = await view();
    process.stdout.write(JSON.stringify({ name, cwdReads, outcome, before, after }).split(root).join('<root>') + '\n');
  } finally {
    process.cwd = originalCwd;
    process.chdir(root);
    await rm(root, { recursive: true });
  }
} else {
  assert.ok(process.argv[2], '#879 output directory required');
  const output = resolve(process.argv[2]);
  const sourceCommit = assertBaseline(baseline);
  const sourceSha256 = await baselineSourceHashes(baseline, sources);
  const rows = cases.map(name => {
    const raw = execFileSync(process.execPath, ['--disable-warning=ExperimentalWarning', '--import',
      join(baseline, 'node_modules/tsx/dist/loader.mjs'), fileURLToPath(import.meta.url), '--child', name],
      { encoding: 'utf8', timeout: 30_000 });
    process.stdout.write(raw);
    return JSON.parse(raw);
  });
  await mkdir(output, { recursive: true });
  await writeFile(join(output, 'oracle.json'), JSON.stringify({
    node: process.version, sourceCommit, sourceSha256, cases: rows,
  }, null, 2) + '\n', { flag: 'wx' });
}
