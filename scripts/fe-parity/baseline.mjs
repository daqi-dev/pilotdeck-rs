import { execFile } from 'node:child_process';
import { createHash } from 'node:crypto';
import { promisify } from 'node:util';
import { readdir, readFile, realpath } from 'node:fs/promises';
import { dirname, resolve, sep } from 'node:path';
import { BASELINE_COMMIT } from './accounting.mjs';

const exec = promisify(execFile);

// Runner-facing packages whose resolved install must come from the pnpm store.
// Every linked "@xterm/*" package is checked too (see verifyNodeModules).
// Every direct dependency declared by the ui and root manifests is bound to the
// pinned lockfile (#61.2): whatever the runner loads from the baseline
// (requireUI, `--import tsx`, evaluated frozen sources) resolves through these.
// playwright-core does not resolve from ui at all, so callers must tolerate its
// absence; it is the only requireUI target outside the manifests.
export const UNBOUND_PACKAGES = ['playwright-core'];
const IMPORTERS = [['ui', ui => ui], ['.', (ui, repository) => repository]];

async function sha256(path) {
  return createHash('sha256').update(await readFile(path)).digest('hex');
}

// pnpm's virtual-store directory name (dependency-path depPathToFilename):
//   "<name>@<version>" with \/:*?"<>| -> "+"; when peers are present the final ")"
//   is dropped and every ")(" "(" ")" becomes "_"; names longer than
//   virtualStoreDirMaxLength, or not lower-case, keep their first max-33 chars
//   plus "_" and the first 32 hex digits of their SHA-256.
// Checked against every direct dependency of the real baseline (131 plain, 13 hashed).
export function pnpmStoreDir(name, lockVersion, maxLength = 120) {
  let file = `${name}@${lockVersion}`.replace(/[\\/:*?"<>|]/g, '+');
  if (file.includes('(')) file = file.replace(/\)$/, '').replace(/(\)\()|\(|\)/g, '_');
  if (file.length > maxLength || (file !== file.toLowerCase() && !file.startsWith('file+'))) {
    return `${file.substring(0, maxLength - 33)}_${createHash('sha256').update(file).digest('hex').slice(0, 32)}`;
  }
  return file;
}

// An importer's resolved versions from the pinned lockfile, e.g. importer "ui":
//   importers:\n  ui:\n    dependencies:\n      '<name>':\n        specifier: ...\n        version: <v>
// Only this shape is read; anything else for a wanted name fails closed.
export function lockedImporterVersions(lockText, importer, names) {
  const lines = lockText.split('\n');
  const start = lines.indexOf('importers:');
  const head = lines.indexOf(`  ${importer}:`, start);
  if (start < 0 || head < 0) throw new Error(`pnpm-lock.yaml has no importers.${importer} section`);
  const found = new Map();
  let section = null, current = null;
  for (let k = head + 1; k < lines.length; k++) {
    const line = lines[k];
    if (line && !line.startsWith('    ')) break;
    let m;
    if ((m = /^    (\w+):$/.exec(line))) { section = m[1]; current = null; continue; }
    if ((m = /^      '?([^':]+)'?:$/.exec(line))) { current = section === 'dependencies' || section === 'devDependencies' ? m[1] : null; continue; }
    if (current && (m = /^        version: (\S+)$/.exec(line))) {
      if (found.has(current)) throw new Error(`pnpm-lock.yaml importers.${importer} lists ${current} twice`);
      found.set(current, m[1]);
    }
  }
  for (const name of names) if (!found.has(name)) throw new Error(`pnpm-lock.yaml importers.${importer} does not resolve ${name}`);
  return found;
}

async function checkPackage(base, repository, name, lockVersion, maxLength) {
  const link = resolve(base, 'node_modules', name);
  let resolved;
  try {
    resolved = await realpath(link);
  } catch (error) {
    if (error && error.code === 'ENOENT') {
      throw new Error(`Baseline node_modules missing package: ${name}`);
    }
    throw error;
  }
  // Workspace packages: the link must reach exactly the locked local directory.
  if (lockVersion.startsWith('link:')) {
    const expected = await realpath(resolve(base, lockVersion.slice('link:'.length)));
    if (resolved !== expected) throw new Error(`Baseline package ${name} resolves to ${resolved}, lockfile expects ${expected}`);
    return { name, version: lockVersion, path: resolved };
  }
  // The pnpm virtual store lives at <repo>/node_modules/.pnpm; anything resolved
  // outside of it (a hoisted copy, a nested node_modules, a stray tarball) is not
  // the install the lockfile describes.
  const store = resolve(repository, 'node_modules', '.pnpm') + sep;
  if (!resolved.startsWith(store)) {
    throw new Error(`Baseline package ${name} resolves outside .pnpm: ${resolved}`);
  }
  // Bind the link to the lockfile, not to whatever store directory it points at:
  // another valid version left in the store must not pass.
  const dirName = resolved.slice(store.length).split(sep, 1)[0];
  const expectedDir = pnpmStoreDir(name, lockVersion, maxLength);
  if (dirName !== expectedDir || resolved !== resolve(store, expectedDir, 'node_modules', name)) {
    throw new Error(`Baseline package ${name} resolves to ${dirName}, lockfile expects ${expectedDir}`);
  }
  const { version } = JSON.parse(await readFile(resolve(resolved, 'package.json'), 'utf8'));
  // file: dependencies are bound by their store directory alone; their lock
  // "version" is the source path, not the package.json version.
  if (lockVersion.startsWith('file:')) return { name, version: lockVersion, path: resolved };
  const expectedVersion = lockVersion.split('(', 1)[0];
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

  const modules = JSON.parse(await readFile(resolve(repository, 'node_modules', '.modules.yaml'), 'utf8'));
  if (modules.virtualStoreDir !== '.pnpm' || modules.nodeLinker !== 'isolated') {
    throw new Error(`Baseline node_modules layout unsupported: virtualStoreDir=${modules.virtualStoreDir} nodeLinker=${modules.nodeLinker}`);
  }
  const maxLength = modules.virtualStoreDirMaxLength;
  if (!Number.isInteger(maxLength) || maxLength < 34) {
    throw new Error(`Baseline node_modules virtualStoreDirMaxLength unsupported: ${maxLength}`);
  }
  // The wanted set comes from the Git-pinned manifests, never from what happens
  // to be installed: a deleted link or a missing scope must fail.
  const lockText = declared.toString('utf8');
  const packages = [];
  for (const [importer, baseOf] of IMPORTERS) {
    const base = baseOf(ui, repository);
    const manifest = JSON.parse(await readFile(resolve(base, 'package.json'), 'utf8'));
    const names = Object.keys({ ...manifest.dependencies, ...manifest.devDependencies }).sort();
    const locked = lockedImporterVersions(lockText, importer, names);
    for (const name of names) packages.push({ importer, ...await checkPackage(base, repository, name, locked.get(name), maxLength) });
  }
  // Scoped packages actually present must all be declared (e.g. a stray @xterm/*).
  const uiManifest = JSON.parse(await readFile(resolve(ui, 'package.json'), 'utf8'));
  const uiDeclared = Object.keys({ ...uiManifest.dependencies, ...uiManifest.devDependencies, ...uiManifest.optionalDependencies });
  let installedXterm = [];
  try {
    installedXterm = (await readdir(resolve(ui, 'node_modules', '@xterm'))).map(entry => `@xterm/${entry}`);
  } catch (error) {
    if (!(error && error.code === 'ENOENT')) throw error;
  }
  const extra = installedXterm.filter(name => !uiDeclared.includes(name));
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
