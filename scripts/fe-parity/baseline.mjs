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

// pnpm's virtual-store directory for an importer's resolved version, e.g.
// vite + "7.3.3(@types/node@22.19.19)(jiti@1.21.7)" -> "vite@7.3.3_@types+node@22.19.19_jiti@1.21.7".
export function pnpmStoreDir(name, lockVersion) {
  return `${name}@${lockVersion}`.replace(/\//g, '+').replace(/\)\(/g, '_').replace(/\(/g, '_').replace(/\)$/, '');
}

// The ui importer's resolved versions from the pinned lockfile:
//   importers:\n  ui:\n    dependencies:\n      '<name>':\n        specifier: ...\n        version: <v>
// Only this shape is read; anything else for a wanted name fails closed.
export function lockedUiVersions(lockText, names) {
  const lines = lockText.split('\n');
  const start = lines.indexOf('importers:');
  const ui = lines.indexOf('  ui:', start);
  if (start < 0 || ui < 0) throw new Error('pnpm-lock.yaml has no importers.ui section');
  const found = new Map();
  let section = null, current = null;
  for (let k = ui + 1; k < lines.length; k++) {
    const line = lines[k];
    if (line && !line.startsWith('    ')) break;
    let m;
    if ((m = /^    (\w+):$/.exec(line))) { section = m[1]; current = null; continue; }
    if ((m = /^      '?([^':]+)'?:$/.exec(line))) { current = section === 'dependencies' || section === 'devDependencies' ? m[1] : null; continue; }
    if (current && (m = /^        version: (\S+)$/.exec(line))) {
      if (found.has(current)) throw new Error(`pnpm-lock.yaml importers.ui lists ${current} twice`);
      found.set(current, m[1]);
    }
  }
  for (const name of names) if (!found.has(name)) throw new Error(`pnpm-lock.yaml importers.ui does not resolve ${name}`);
  return found;
}

async function checkPackage(ui, repository, name, lockVersion) {
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
  // Bind the link to the lockfile, not to whatever store directory it points at:
  // another valid version left in the store must not pass (#61.2).
  const dirName = resolved.slice(store.length).split(sep, 1)[0];
  const expectedDir = pnpmStoreDir(name, lockVersion);
  if (dirName !== expectedDir || resolved !== resolve(store, expectedDir, 'node_modules', name)) {
    throw new Error(`Baseline package ${name} resolves to ${dirName}, lockfile expects ${expectedDir}`);
  }
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

  // The wanted set comes from the Git-pinned manifest, never from what happens
  // to be installed: a deleted link or a missing @xterm scope must fail.
  const manifest = JSON.parse(await readFile(resolve(ui, 'package.json'), 'utf8'));
  const declared_ = Object.keys({ ...manifest.dependencies, ...manifest.devDependencies });
  const xtermNames = declared_.filter(name => name.startsWith('@xterm/')).sort();
  for (const name of REQUIRED_PACKAGES) if (!declared_.includes(name)) throw new Error(`ui/package.json does not declare ${name}`);
  const wanted = [...REQUIRED_PACKAGES, ...xtermNames];
  const locked = lockedUiVersions(declared.toString('utf8'), wanted);
  // pnpm hashes store directory names longer than virtualStoreDirMaxLength. That
  // encoding is not reproduced here, so such a layout is refused explicitly
  // instead of being reported as a version mismatch.
  const modules = JSON.parse(await readFile(resolve(repository, 'node_modules', '.modules.yaml'), 'utf8'));
  if (modules.virtualStoreDir !== '.pnpm' || modules.nodeLinker !== 'isolated') {
    throw new Error(`Baseline node_modules layout unsupported: virtualStoreDir=${modules.virtualStoreDir} nodeLinker=${modules.nodeLinker}`);
  }
  const maxLength = modules.virtualStoreDirMaxLength;
  for (const name of wanted) {
    const dir = pnpmStoreDir(name, locked.get(name));
    if (!Number.isInteger(maxLength) || dir.length > maxLength) {
      throw new Error(`Baseline node_modules uses a hashed store directory for ${name} (virtualStoreDirMaxLength=${maxLength}); extend the apparatus before using this layout`);
    }
  }
  const packages = [];
  for (const name of wanted) packages.push(await checkPackage(ui, repository, name, locked.get(name)));
  let installedXterm = [];
  try {
    installedXterm = (await readdir(resolve(ui, 'node_modules', '@xterm'))).map(entry => `@xterm/${entry}`);
  } catch (error) {
    if (!(error && error.code === 'ENOENT')) throw error;
  }
  const extra = installedXterm.filter(name => !xtermNames.includes(name));
  if (extra.length) throw new Error(`Baseline node_modules has undeclared @xterm packages: ${extra.join(', ')}`);
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
