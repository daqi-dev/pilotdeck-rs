import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { realpath } from 'node:fs/promises';
import { resolve } from 'node:path';
import { BASELINE_COMMIT } from './accounting.mjs';

const exec = promisify(execFile);
export async function verifyBaseline(ui, expected = BASELINE_COMMIT) {
  const git = async args => (await exec('git', ['-C', ui, ...args], { encoding: 'utf8' })).stdout.trim();
  const head = await git(['rev-parse', 'HEAD']);
  if (head !== expected) throw new Error(`Baseline commit mismatch: ${head} != ${expected}`);
  const repository = await git(['rev-parse', '--show-toplevel']);
  if (await realpath(ui) !== await realpath(resolve(repository, 'ui'))) {
    throw new Error('BASELINE_UI must be the pinned repository ui directory, not an untracked copy');
  }
  // HEAD alone does not detect edits in a checkout at the correct commit.
  if (await git(['diff', 'HEAD', '--name-only', '--'])) throw new Error('Baseline tracked source differs from pinned HEAD');
  if (await git(['ls-files', '--others', '--exclude-standard', '--', 'e2e'])) {
    throw new Error('Baseline has untracked e2e sources');
  }
  return { head, repository, ui: await realpath(ui), status: 'PASS' };
}
