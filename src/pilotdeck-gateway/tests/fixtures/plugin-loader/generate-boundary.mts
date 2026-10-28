// #484 repair 4: real TS observations for the remaining ROOT-001 cases.
import assert from 'node:assert/strict';
import {execFileSync} from 'node:child_process';
import {realpathSync} from 'node:fs';
import {chmod, mkdir, mkdtemp, readFile, rm, symlink, writeFile} from 'node:fs/promises';
import {join, resolve} from 'node:path';
import {pathToFileURL} from 'node:url';
import { sourceClosureSha256 } from '../../baseline_source_guard.mjs';
import { bindBaseline } from '../_shared/generator_guard.mjs';

assert.ok(process.env.PDRS_BASELINE, 'PDRS_BASELINE required');
assert.ok(process.argv[2], 'output directory required');
const baseline = resolve(process.env.PDRS_BASELINE);
const sourceCommit = 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';
const git = realpathSync(execFileSync('which', ['git'], {encoding: 'utf8'}).trim());
bindBaseline({ baseline: baseline, commit: sourceCommit, gitExecutable: git });
const gitEnv = Object.fromEntries(Object.entries(process.env).filter(([k]) => !k.startsWith('GIT_')));
assert.equal(execFileSync(git, ['--no-replace-objects', '-C', baseline, 'status', '--porcelain', '--untracked-files=no'], {env: gitEnv}).toString(), '');
assert.equal(new Intl.Collator().resolvedOptions().locale, 'en-US');
const loaderPath = 'src/extension/plugins/loading/PluginLoader.ts';
const sourceSha256 = sourceClosureSha256(baseline, sourceCommit, git, [loaderPath]);
const {loadPluginFromPath, loadPluginOutputStylesFromPath} = await import(pathToFileURL(join(baseline, loaderPath)).href);
// Explicitly exercise an absolute /private/tmp configuration without writing
// outside this newly owned scratch directory or through node_modules paths.
const scratch = await mkdtemp('/private/tmp/pdrs-484-boundary-');
const normalize = (v: any) => JSON.parse(JSON.stringify(v).split(scratch).join('<root>'));
const cases: any[] = [];
const record = async (id: string, path: string, input: any = {}) => {
  const plugin = await loadPluginFromPath(path, 'global');
  const style = await loadPluginOutputStylesFromPath(path, 'global');
  cases.push({id, input: normalize(input), result: normalize({commands: plugin.commands, skills: plugin.skills, outputStyles: plugin.outputStyles, styleOnly: style.outputStyles})});
};
const manifest = async (root: string, configured?: string) => {
  await mkdir(root, {recursive: true});
  await writeFile(join(root, 'plugin.json'), JSON.stringify({name: 'p', ...(configured === undefined ? {} : {commands: configured, skills: configured, outputStyles: configured})}));
};
try {
  const outside = join(scratch, 'outside');
  await mkdir(outside);
  await writeFile(join(outside, 'SKILL.md'), 'SECRET');
  const escaped = join(scratch, 'base-symlink');
  await manifest(escaped, 'linked');
  await symlink('../outside', join(escaped, 'linked'));
  await record('configured-base-symlink', escaped);

  const real = join(scratch, 'real-root');
  await manifest(real);
  for (const dir of ['commands', 'skills', 'output-styles']) {
    await mkdir(join(real, dir));
    await writeFile(join(real, dir, 'SKILL.md'), 'safe');
  }
  const linked = join(scratch, 'root-symlink');
  await symlink('real-root', linked);
  await record('plugin-root-symlink', linked);

  const absolute = join(scratch, 'absolute');
  const configured = '/private/tmp/pdrs-484-boundary-absolute-input';
  await manifest(absolute, configured);
  // Node path.join appends even an absolute second operand. Its safe fixture
  // lives inside the plugin; Path::join's reset behavior must be caught.
  await mkdir(join(absolute, configured), {recursive: true});
  await writeFile(join(absolute, configured, 'SKILL.md'), 'safe absolute');
  await record('absolute-configured-path', absolute, {configured});

  const deniedRoot = join(scratch, 'denied-plugin');
  await manifest(deniedRoot, 'items');
  await mkdir(join(deniedRoot, 'items'));
  await writeFile(join(deniedRoot, 'items', 'SKILL.md'), 'safe');
  const denied = join(outside, 'denied.md');
  await writeFile(denied, 'SECRET denied');
  await symlink('../../outside/denied.md', join(deniedRoot, 'items', 'denied.md'));
  await chmod(denied, 0);
  try {
    await assert.rejects(readFile(denied), {code: 'EACCES'}, 'permission fixture must deny native read');
    await record('outside-unreadable-file', deniedRoot);
  } finally {
    await chmod(denied, 0o600);
  }
  // A real TS control supplies the safe contributions retained by ROOT-001;
  // do not synthesize their expected fields on the Rust side.
  await rm(join(deniedRoot, 'items', 'denied.md'));
  await record('outside-unreadable-file-safe', deniedRoot);
} finally {
  await rm(scratch, {recursive: true, force: true});
}
const output = resolve(process.argv[2]);
await mkdir(output, {recursive: true});
await writeFile(join(output, 'boundary.json'), JSON.stringify({sourceCommit, node: process.version, sourceSha256, cases}, null, 2) + '\n');
console.log(JSON.stringify(cases.map(c => ({id: c.id, counts: Object.fromEntries(Object.entries(c.result).map(([k, v]) => [k, (v as any[]).length]))}))));
