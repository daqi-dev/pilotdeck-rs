// Original GatewayToolResultArtifactStore constructor; a fresh child keeps
// Node's cwd cache cold per case. Delete cwd only after import and chdir.
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { mkdir, mkdtemp, realpath, rm, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { tmpdir } from 'node:os';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { assertBaseline, baselineSourceHashes } from '../_shared/baseline.mts';

assert.equal(process.version, 'v22.22.0');
assert.ok(process.env.PDRS_BASELINE);
const baseline = resolve(process.env.PDRS_BASELINE);
const sources = ['src/gateway/client/GatewayToolResultArtifactStore.ts'];
const cases = [
  'relative-normal', 'relative-deleted', 'relative-warmed-normal',
  'relative-warmed-deleted', 'absolute-normal', 'absolute-deleted',
  'tmpdir-relative-normal', 'tmpdir-relative-deleted',
  'tmpdir-relative-warmed-deleted', 'tmpdir-absolute-deleted',
];

if (process.argv[2] === '--child') {
  const name = process.argv[3];
  assert.ok(cases.includes(name));
  const { GatewayToolResultArtifactStore } = await import(
    pathToFileURL(join(baseline, sources[0])).href);
  const root = await realpath(await mkdtemp(join(tmpdir(), 'pdrs-projector-cwd-')));
  const cwd = join(root, 'cwd');
  await mkdir(cwd);
  await mkdir(join(root, 'tmp'));
  await mkdir(join(root, 'tmpdir'));
  const originalCwd = process.cwd;
  let cwdReads = 0;
  let outcome: unknown;
  process.chdir(cwd);
  // Warm the process-level cwd cache only in the explicit warm cells.
  if (name.includes('-warmed-')) process.cwd();
  if (name.endsWith('-deleted')) await rm(cwd, { recursive: true });
  process.cwd = () => { cwdReads++; return originalCwd(); };
  try {
    let options: Record<string, unknown> = {};
    if (name.startsWith('tmpdir-')) {
      delete process.env.TMP;
      delete process.env.TEMP;
      process.env.TMPDIR = name === 'tmpdir-absolute-deleted'
        ? join(root, 'tmpdir')
        : '../tmp';
    } else {
      // Mirror the Rust matrix, including `.`, `..` and a doubled slash.
      options = {
        rootDir: name.startsWith('absolute-')
          ? `${root}//files/nested/../artifacts`
          : '../files/./nested/../artifacts',
      };
    }
    try {
      const store = new GatewayToolResultArtifactStore(options);
      outcome = { ok: true, rootDir: (store as any).rootDir };
    } catch (error: any) {
      outcome = { error: { name: error.name, code: error.code, message: error.message,
        syscall: error.syscall } };
    }
  } finally {
    process.cwd = originalCwd;
  }
  try {
    process.stdout.write(JSON.stringify({ name, cwdReads, outcome })
      .split(root).join('<root>') + '\n');
  } finally {
    process.chdir(root);
    await rm(root, { recursive: true });
  }
} else {
  assert.ok(process.argv[2], 'output directory required');
  const output = resolve(process.argv[2]);
  const sourceCommit = assertBaseline(baseline);
  const sourceSha256 = await baselineSourceHashes(baseline, sources);
  const rows = cases.map(name => {
    const raw = execFileSync(process.execPath, [
      '--disable-warning=ExperimentalWarning', '--import',
      join(baseline, 'node_modules/tsx/dist/loader.mjs'), fileURLToPath(import.meta.url),
      '--child', name,
    ], { encoding: 'utf8', timeout: 30_000 });
    process.stdout.write(raw);
    return JSON.parse(raw);
  });
  await mkdir(output, { recursive: true });
  await writeFile(join(output, 'oracle.json'), JSON.stringify({
    node: process.version, sourceCommit, sourceSha256, cases: rows,
  }, null, 2) + '\n', { flag: 'wx' });
}
