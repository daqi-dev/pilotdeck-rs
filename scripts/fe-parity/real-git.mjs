// Expose exactly Node and one real Git, never an operator's whole CLI directory.
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { constants } from 'node:fs';
import { access, stat, realpath, mkdir, symlink, readdir, lstat } from 'node:fs/promises';
import { resolve } from 'node:path';
import { assertRealExecutionEnv } from './execution-env.mjs';
const exec = promisify(execFile);
// The Command Line Tools git is a real binary (not the /usr/bin xcrun shim) and
// does not run the Xcode license check.
export const realGitCandidates = Object.freeze(['/opt/homebrew/bin/git', '/usr/local/bin/git',
  '/Library/Developer/CommandLineTools/usr/bin/git']);
export function assertRealGitTarget(target) {
  if (!target || resolve(target) !== target || target === '/usr/bin/git'
    || target === '/Applications/Xcode.app' || target.startsWith('/Applications/Xcode.app/')) {
    throw new Error(`FAIL: real git target is a system/Xcode shim or noncanonical: ${target}`);
  }
}
export async function discoverRealGit(env, { canonical = realpath, executable = async target => {
  if (!(await stat(target)).isFile()) throw new Error('FAIL: real git target is not a file');
  await access(target, constants.X_OK);
}, version = async target => {
  const result = await exec(target, ['--version'], { env, cwd: env.PILOTDECK_E2E_RUN_DIR,
    encoding: 'utf8', timeout: 5000, maxBuffer: 64 * 1024 });
  if (result.stderr.trim()) throw new Error('FAIL: git --version wrote stderr');
  return result.stdout.trim();
} } = {}) {
  assertRealExecutionEnv(env);
  const unavailable = [];
  for (const candidate of realGitCandidates) {
    let target;
    try { target = await canonical(candidate); }
    catch (error) {
      if (error.code !== 'ENOENT') throw error;
      unavailable.push(candidate); continue;
    }
    // An existing but unsafe link is a violation, not permission to fall back.
    assertRealGitTarget(target);
    await executable(target);
    const text = await version(target);
    if (!/^git version [^\r\n]+$/.test(text)) throw new Error('FAIL: unrecognized git --version');
    return { candidate, realpath: target, version: text, argv: [target, '--version'] };
  }
  throw new Error(`FAIL: no real git executable at ${unavailable.join(' or ')}; no system git fallback`);
}
export async function prepareRealGitBin(env, io) {
  assertRealExecutionEnv(env);
  const git = await discoverRealGit(env, io), bin = resolve(env.PILOTDECK_E2E_RUN_DIR, 'bin');
  if (env.PATH !== `${bin}:/usr/bin:/bin:/usr/sbin:/sbin`) throw new Error('FAIL: real git PATH drift');
  await mkdir(bin); // exclusive: never adopt an existing bin with global CLIs
  await symlink(process.execPath, resolve(bin, 'node'));
  await symlink(git.realpath, resolve(bin, 'git'));
  await assertRealGitBin(env, io);
  return { ...git, link: resolve(bin, 'git'), PATH: env.PATH };
}
export async function assertRealGitBin(env, { canonical = realpath } = {}) {
  assertRealExecutionEnv(env);
  const bin = resolve(env.PILOTDECK_E2E_RUN_DIR, 'bin');
  if (env.PATH !== `${bin}:/usr/bin:/bin:/usr/sbin:/sbin` || await realpath(bin) !== bin
    || JSON.stringify((await readdir(bin)).sort()) !== JSON.stringify(['git', 'node'])) throw new Error('FAIL: real bin/PATH drift');
  for (const name of ['git', 'node']) if (!(await lstat(resolve(bin, name))).isSymbolicLink()) throw new Error('FAIL: real bin requires links');
  const target = await realpath(resolve(bin, 'git'));
  assertRealGitTarget(target);
  // Re-resolve the permitted installations at the runtime boundary.
  const targets = await Promise.all(realGitCandidates.map(candidate => canonical(candidate).catch(error => {
    if (error.code !== 'ENOENT') throw error;
    return undefined;
  })));
  if (!targets.includes(target) || await realpath(resolve(bin, 'node')) !== await realpath(process.execPath)) throw new Error('FAIL: real bin link drift');
  if (!(await stat(target)).isFile()) throw new Error('FAIL: real git target is not a file');
  await access(target, constants.X_OK);
  return target;
}
