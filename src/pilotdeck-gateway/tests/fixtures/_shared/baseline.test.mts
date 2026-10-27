// Guard inputs are tiny synthetic Git fixtures, never a clone of the worktree
// and never used to bypass negative_process's source-equals-HEAD requirement.
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { mkdtempSync, mkdirSync, readFileSync, realpathSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { bindBaseline, recordedSourceHashes } from './generator_guard.mjs';
import { assertBaseline, baselineSourceHashes, BASELINE_COMMIT } from './baseline.mts';
import { sourceClosureSha256 } from '../../baseline_source_guard.mjs';

const gitExecutable = realpathSync(process.env.PDRS_GIT || execFileSync('which', ['git'], { encoding: 'utf8' }).trim());
function fixture(t: any) {
  const baseline = mkdtempSync(join(tmpdir(), 'pdrs-280-guard-'));
  t.after(() => rmSync(baseline, { recursive: true, force: true }));
  const env = Object.fromEntries(Object.entries(process.env).filter(([key]) => !key.startsWith('GIT_')));
  const git = (...args: string[]) => execFileSync(gitExecutable,
    ['-c', 'user.name=Fixture', '-c', 'user.email=fixture@example.invalid', '-C', baseline, ...args],
    { env, stdio: ['ignore', 'pipe', 'pipe'] });
  git('init');
  mkdirSync(join(baseline, 'src'));
  writeFileSync(join(baseline, 'src/entry.ts'), "export { value } from './dependency.js';\n");
  writeFileSync(join(baseline, 'src/dependency.ts'), 'export const value = 1;\n');
  writeFileSync(join(baseline, 'README.md'), 'fixture\n');
  git('add', '.');
  git('commit', '-m', 'synthetic guard input');
  const commit = git('rev-parse', 'HEAD').toString().trim();
  const bind = () => bindBaseline({ baseline, commit, gitExecutable });
  return { baseline, commit, git, bind };
}

test('#280 clean input, untracked cache, inherited Git override and recorded SHA', t => {
  const f = fixture(t);
  const context = { baseline: f.baseline, commit: f.commit, git: f.bind().git };
  const hashes = recordedSourceHashes(context, ['src/entry.ts']);
  assert.equal(Object.keys(hashes).length, 1, '#280 preserve legacy hash keys');
  assert.match(hashes['src/entry.ts'], /^[0-9a-f]{64}$/, '#280 SHA shape');
  mkdirSync(join(f.baseline, '__pycache__'));
  writeFileSync(join(f.baseline, '__pycache__/cache'), 'untracked');
  assert.equal(f.bind().sourceCommit, f.commit, '#280 untracked files are permitted');
  const inherited = process.env.GIT_DIR;
  try {
    process.env.GIT_DIR = join(f.baseline, 'nonexistent');
    assert.equal(f.bind().sourceCommit, f.commit, '#280 inherited Git override ignored');
  } finally {
    if (inherited === undefined) delete process.env.GIT_DIR;
    else process.env.GIT_DIR = inherited;
  }
  assert.throws(() => recordedSourceHashes(context, []), /#280.*must not be empty/);
  assert.throws(() => recordedSourceHashes(context, ['src/../README.md']), /#280.*baseline-relative/);
  writeFileSync(join(f.baseline, 'src/entry.ts'), 'export const value = 2;\n');
  assert.throws(() => recordedSourceHashes(context, ['src/entry.ts']), /#280.*differs from/);
});

for (const staged of [false, true]) test(`#280 rejects ${staged ? 'staged' : 'unstaged'} tracked changes outside src`, t => {
  const f = fixture(t);
  writeFileSync(join(f.baseline, 'README.md'), 'dirty\n');
  if (staged) f.git('add', 'README.md');
  assert.throws(f.bind, /uncommitted changes to tracked files/);
});

for (const flag of ['skip-worktree', 'assume-unchanged']) {
  for (const file of ['src/dependency.ts', 'README.md']) {
    test(`#303 rejects ${flag} hiding ${file}`, t => {
      const f = fixture(t);
      f.git('update-index', `--${flag}`, file);
      assert.throws(f.bind, /#303.*skip-worktree\/assume-unchanged/);
      writeFileSync(join(f.baseline, file), file.endsWith('.ts') ? 'export const value = 2;\n' : 'hidden dirty\n');
      assert.equal(f.git('status', '--porcelain', '--untracked-files=no').toString(), '', '#303 status misses the mutation');
      assert.throws(f.bind, /source bytes differ|#303.*skip-worktree\/assume-unchanged/);
    });
  }
}

test('#280 rejects wrong HEAD and a missing transitive source', t => {
  const f = fixture(t);
  f.git('commit', '--allow-empty', '-m', 'different head');
  assert.throws(f.bind, /baseline HEAD mismatch/);
  f.git('checkout', '--detach', f.commit);
  rmSync(join(f.baseline, 'src/dependency.ts'));
  assert.throws(f.bind, /missing tracked source file/);
});

test('#280 actual fixed baseline adapter preserves recorded oracle hashes', async () => {
  assert.ok(process.env.PDRS_BASELINE, '#280 set PDRS_BASELINE');
  assert.equal(assertBaseline(process.env.PDRS_BASELINE), BASELINE_COMMIT, '#280 actual fixed commit');
  const oracle = JSON.parse(readFileSync(new URL('../config-changes/oracle.json', import.meta.url), 'utf8'));
  assert.deepEqual(await baselineSourceHashes(process.env.PDRS_BASELINE, Object.keys(oracle.sourceSha256)),
    oracle.sourceSha256, '#280 actual commit SHA recomputation');
});

test('#303 relative import closure records and verifies transitive .js-to-.ts dependencies', t => {
  const f = fixture(t);
  const closure = () => sourceClosureSha256(f.baseline, f.commit, gitExecutable, ['src/entry.ts']);
  const hashes = closure();
  assert.deepEqual(Object.keys(hashes), ['src/dependency.ts', 'src/entry.ts'], '#303 complete import closure');
  f.git('update-index', '--skip-worktree', 'src/dependency.ts');
  writeFileSync(join(f.baseline, 'src/dependency.ts'), 'export const value = 3;\n');
  assert.equal(f.git('status', '--porcelain', '--untracked-files=no').toString(), '', '#303 status hides transitive dependency');
  assert.throws(closure, /src\/dependency.ts differs from/, '#303 transitive blob must be verified');
});

for (const [specifier, diagnostic] of [['./missing.js', /unresolved baseline import/], ['../README.md', /import closure escaped src/]] as const) {
  test(`#303 rejects unresolved or external import ${specifier}`, t => {
    const f = fixture(t);
    writeFileSync(join(f.baseline, 'src/entry.ts'), `export * from '${specifier}';\n`);
    f.git('add', 'src/entry.ts');
    f.git('commit', '-m', 'synthetic invalid import input');
    const commit = f.git('rev-parse', 'HEAD').toString().trim();
    assert.throws(() => sourceClosureSha256(f.baseline, commit, gitExecutable, ['src/entry.ts']), diagnostic);
  });
}
