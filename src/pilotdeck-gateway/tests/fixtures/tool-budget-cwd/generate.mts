// Every case imports the real entry before chdir and starts with a cold cwd cache.
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { mkdir, mkdtemp, realpath, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { assertBaseline, baselineSourceHashes } from '../_shared/baseline.mts';

assert.equal(process.version, 'v22.22.0');
assert.ok(process.env.PDRS_BASELINE, '#885 PDRS_BASELINE required');
const baseline = resolve(process.env.PDRS_BASELINE);
const sources = ['src/context/budget/ToolResultBudget.ts'];
const cases = ['relative', 'absolute']
  .flatMap(kind => ['normal', 'deleted'].map(state => `${kind}-${state}`))
  .concat(['relative-warmed-normal', 'relative-warmed-deleted']);

if (process.argv[2] === '--child') {
  const name = process.argv[3];
  assert.ok(cases.includes(name), '#885 known case');
  const { ToolResultBudget } = await import(pathToFileURL(join(baseline, sources[0])).href);
  const root = await realpath(await mkdtemp(join(tmpdir(), 'pdrs-tool-budget-cwd-')));
  const cwd = join(root, 'cwd');
  await mkdir(cwd);
  const originalCwd = process.cwd;
  let cwdReads = 0;
  let outcome: unknown;
  process.chdir(cwd);
  if (name.includes('-warmed-')) process.cwd();
  if (name.endsWith('-deleted')) await rm(cwd, { recursive: true });
  process.cwd = () => { cwdReads++; return originalCwd(); };
  try {
    // `new ToolResultBudget(...)` runs the constructor's `resolve(toolResultsDir)`;
    // the input mirrors the Rust matrix, including `.`/`..` and a doubled slash.
    const input = name.startsWith('absolute-')
      ? `${root}//files/nested/../results`
      : '../files/./nested/../results';
    const budget = new ToolResultBudget({ toolResultsDir: input });
    outcome = { ok: true, toolResultsDir: (budget as any).toolResultsDir };
  } catch (error: any) {
    outcome = { error: { name: error.name, code: error.code, message: error.message, syscall: error.syscall } };
  } finally {
    process.cwd = originalCwd;
  }
  try {
    process.stdout.write(JSON.stringify({ name, cwdReads, outcome }).split(root).join('<root>') + '\n');
  } finally {
    process.chdir(root);
    await rm(root, { recursive: true });
  }
} else {
  assert.ok(process.argv[2], '#885 output directory required');
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
