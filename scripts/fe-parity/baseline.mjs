import { execFile } from 'node:child_process';
import { createHash } from 'node:crypto';
import { promisify } from 'node:util';
import { readdir, readFile, realpath } from 'node:fs/promises';
import { dirname, resolve, sep } from 'node:path';
import { BASELINE_COMMIT } from './accounting.mjs';

const exec = promisify(execFile);

// Runner-facing packages whose resolved install must come from the pnpm store.
// Every linked "@xterm/*" package is checked too (see verifyNodeModules).
const REQUIRED_PACKAGES = ['vite', 'ws', '@playwright/test'];

async function sha256(path) {
  return createHash('sha256').update(await readFile(path)).digest('hex');
}

// pnpm encodes a virtual-store directory as "<name(/→+)>@<version>[_<peer>...]".
// Extract the version without being confused by "@" inside peer suffixes.
export function parsePnpmDirVersion(dirName, name) {
  const prefix = name.replace(/\//g, '+') + '@';
  if (!dirName.startsWith(prefix)) {
    throw new Error(`pnpm dir ${dirName} does not match package ${name}`);
  }
  const rest = dirName.slice(prefix.length);
  const underscore = rest.indexOf('_');
  return underscore === -1 ? rest : rest.slice(0, underscore);
}

async function checkPackage(ui, repository, name) {
  const link = resolve(ui, 'node_modules', name);
  let resolved;
  try {
    resolved = await realpath(link);
  } catch (error) {
    if (error && error.code === 'ENOENT') {
      throw new Error(`Baseline node_modules missing package: ${name}`);
    }
    throw error;
  }
  // The pnpm virtual store lives at <repo>/node_modules/.pnpm; anything resolved
  // outside of it (a hoisted copy, a nested node_modules, a stray tarball) is not
  // the install the lockfile describes.
  const store = resolve(repository, 'node_modules', '.pnpm') + sep;
  if (!resolved.startsWith(store)) {
    throw new Error(`Baseline package ${name} resolves outside .pnpm: ${resolved}`);
  }
  const dirName = resolved.slice(store.length).split(sep, 1)[0];
  const { version } = JSON.parse(await readFile(resolve(resolved, 'package.json'), 'utf8'));
  const expectedVersion = parsePnpmDirVersion(dirName, name);
  if (version !== expectedVersion) {
    throw new Error(`Baseline package ${name} version mismatch: ${version} != ${expectedVersion}`);
  }
  return { name, version, path: resolved };
}

export async function verifyNodeModules(ui) {
  const repository = (await exec('git', ['-C', ui, 'rev-parse', '--show-toplevel'], { encoding: 'utf8' })).stdout.trim();
  const lockPath = resolve(repository, 'pnpm-lock.yaml');
  const installedPath = resolve(repository, 'node_modules', '.pnpm', 'lock.yaml');
  let installed;
  try {
    installed = await readFile(installedPath);
  } catch (error) {
    if (error && error.code === 'ENOENT') throw new Error(`Baseline node_modules lockfile missing: ${installedPath}`);
    throw error;
  }
  const declared = await readFile(lockPath);
  if (!installed.equals(declared)) {
    throw new Error('Baseline node_modules lockfile differs from pnpm-lock.yaml');
  }
  const lockfileSha256 = createHash('sha256').update(declared).digest('hex');

  const packages = [];
  for (const name of REQUIRED_PACKAGES) {
    packages.push(await checkPackage(ui, repository, name));
  }
  // Every @xterm/* entry actually linked under ui/node_modules is checked; a
  // broken or out-of-store link fails rather than being skipped (#61.2).
  let xtermNames = [];
  try {
    xtermNames = (await readdir(resolve(ui, 'node_modules', '@xterm'))).sort().map(entry => `@xterm/${entry}`);
  } catch (error) {
    if (!(error && error.code === 'ENOENT')) throw error;
  }
  for (const name of xtermNames) packages.push(await checkPackage(ui, repository, name));
  return { lockfileSha256, packages };
}

// checkNodeModules=false is only for self-check fixtures that exercise the Git gate alone.
export async function verifyBaseline(ui, expected = BASELINE_COMMIT, { checkNodeModules = true } = {}) {
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
  const realUi = await realpath(ui);
  if (!checkNodeModules) return { head, repository, ui: realUi, status: 'PASS' };
  const nodeModules = await verifyNodeModules(realUi);
  return { head, repository, ui: realUi, nodeModules, status: 'PASS' };
}
