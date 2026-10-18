// Run with PDRS_BASELINE and node --experimental-strip-types --test <this file>.
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { assertBaseline, baselineSourceHashes, BASELINE_COMMIT } from './baseline.mts';

test('fixed baseline accepts untracked files but rejects tracked and hidden changes', async () => {
  assert.ok(process.env.PDRS_BASELINE, 'set PDRS_BASELINE');
  const scratch = await mkdtemp(join(tmpdir(), 'pdrs-baseline-'));
  const checkout = join(scratch, 'baseline');
  const env = Object.fromEntries(Object.entries(process.env).filter(([key]) => !key.startsWith('GIT_')));
  const git = (...args: string[]) => execFileSync('git', ['-C', checkout, ...args], { env });
  const source = 'src/pilot/config/classifyChanges.ts';
  try {
    execFileSync('git', ['clone', '--shared', '--no-checkout', process.env.PDRS_BASELINE, checkout], { env });
    git('sparse-checkout', 'set', '--no-cone', `/${source}`, '/README.md');
    git('checkout', '--detach', BASELINE_COMMIT);
    assert.equal(assertBaseline(checkout), BASELINE_COMMIT);
    const hashes = await baselineSourceHashes(checkout, [source]);
    assert.equal(hashes[source].length, 64);
    await writeFile(join(checkout, 'untracked.txt'), 'ignored by tracked-file guard');
    assertBaseline(checkout);
    await writeFile(join(checkout, 'README.md'), 'dirty unrelated tracked file');
    assert.throws(() => assertBaseline(checkout), /uncommitted changes/);
    git('add', 'README.md');
    assert.throws(() => assertBaseline(checkout), /uncommitted changes/);
    git('restore', '--staged', '--worktree', 'README.md');
    const original = await readFile(join(checkout, source));
    for (const flag of ['assume-unchanged', 'skip-worktree']) {
      git('update-index', `--${flag}`, source);
      await writeFile(join(checkout, source), Buffer.concat([original, Buffer.from('\n// hidden change\n')]));
      // Hash checks must reject the bytes regardless of Git index/status flags.
      await assert.rejects(baselineSourceHashes(checkout, [source]), /differs from/);
      await writeFile(join(checkout, source), original);
      git('update-index', `--no-${flag}`, source);
    }
    await assert.rejects(baselineSourceHashes(checkout, ['src/missing.ts']));
    await assert.rejects(baselineSourceHashes(checkout, []), /must not be empty/);
    await assert.rejects(baselineSourceHashes(checkout, ['src/../README.md']), /baseline-relative/);
    const inherited = process.env.GIT_DIR;
    try {
      process.env.GIT_DIR = join(scratch, 'does-not-exist');
      assertBaseline(checkout);
      assert.deepEqual(await baselineSourceHashes(checkout, [source]), hashes);
    } finally {
      if (inherited === undefined) delete process.env.GIT_DIR;
      else process.env.GIT_DIR = inherited;
    }
    git('checkout', '--detach', `${BASELINE_COMMIT}^`);
    assert.throws(() => assertBaseline(checkout), /fixed PilotDeck commit/);
  } finally { await rm(scratch, { recursive: true, force: true }); }
});
