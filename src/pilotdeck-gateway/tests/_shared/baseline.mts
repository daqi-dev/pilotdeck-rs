// Shared provenance guard for real TypeScript fixture generators.
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { readFile } from 'node:fs/promises';
import { join } from 'node:path';

export const BASELINE_COMMIT = 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';

function git(baseline: string, ...args: string[]): Buffer {
  // An inherited GIT_DIR/index/object override must not redirect the guard.
  const env = Object.fromEntries(Object.entries(process.env).filter(([key]) => !key.startsWith('GIT_')));
  return execFileSync('git', ['--no-replace-objects', '-C', baseline, ...args], { env, maxBuffer: 32 * 1024 * 1024 });
}

export function assertBaseline(baseline: string): string {
  assert.equal(git(baseline, 'rev-parse', 'HEAD').toString().trim(), BASELINE_COMMIT,
    'baseline must be the fixed PilotDeck commit');
  const tracked = git(baseline, 'status', '--porcelain', '--untracked-files=no').toString();
  assert.equal(tracked, '', `baseline has uncommitted changes to tracked files:\n${tracked}`);
  return BASELINE_COMMIT;
}

export async function baselineSourceHashes(baseline: string, paths: string[]): Promise<Record<string, string>> {
  assert.ok(paths.length > 0, 'baseline source list must not be empty');
  const hashes: Record<string, string> = {};
  for (const path of paths) {
    assert.ok(path.startsWith('src/') && path.split('/').every(part => part !== '..' && part !== ''),
      `expected a baseline-relative source path: ${path}`);
    const committed = git(baseline, 'show', `${BASELINE_COMMIT}:${path}`);
    const expected = createHash('sha256').update(committed).digest('hex');
    const working = createHash('sha256').update(await readFile(join(baseline, path))).digest('hex');
    // status alone misses assume-unchanged/skip-worktree files.
    assert.equal(working, expected, `${path} differs from ${BASELINE_COMMIT}`);
    hashes[path] = expected;
  }
  return hashes;
}
