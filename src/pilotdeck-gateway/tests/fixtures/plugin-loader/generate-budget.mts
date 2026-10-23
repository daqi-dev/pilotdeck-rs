// #484 repair 2: real TS observations; no writes to installed dependencies.
import assert from 'node:assert/strict';
import {execFileSync} from 'node:child_process';
import {realpathSync} from 'node:fs';
import {mkdir, mkdtemp, rm, symlink, writeFile} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join, resolve} from 'node:path';
import {pathToFileURL} from 'node:url';
import {bindSourceTree, sourceClosureSha256} from '../../baseline_source_guard.mjs';

assert.ok(process.env.PDRS_BASELINE, 'PDRS_BASELINE required');
assert.ok(process.argv[2], 'output directory required');
const baseline = resolve(process.env.PDRS_BASELINE);
const output = resolve(process.argv[2]);
assert.ok(!output.startsWith(baseline + '/'), 'output must be outside baseline');
const sourceCommit = 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';
const git = realpathSync(execFileSync('which', ['git'], {encoding:'utf8'}).trim());
bindSourceTree(baseline, sourceCommit, git);
const gitEnv = Object.fromEntries(Object.entries(process.env).filter(([k]) => !k.startsWith('GIT_')));
assert.equal(execFileSync(git, ['--no-replace-objects', '-C', baseline, 'status', '--porcelain', '--untracked-files=no'], {env:gitEnv}).toString(), '');
assert.equal(new Intl.Collator().resolvedOptions().locale, 'en-US');
const commandPath = 'src/extension/plugins/loading/PluginCommandLoader.ts';
const loaderPath = 'src/extension/plugins/loading/PluginLoader.ts';
const sourceSha256 = sourceClosureSha256(baseline, sourceCommit, git, [commandPath, loaderPath]);
const {loadPluginCommands} = await import(pathToFileURL(join(baseline, commandPath)).href);
const {loadPluginFromPath} = await import(pathToFileURL(join(baseline, loaderPath)).href);
const scratch = await mkdtemp(join(tmpdir(), 'pdrs-484-budget-'));
const normalize = (v:any) => JSON.parse(JSON.stringify(v).split(scratch).join('<root>'));
// Each physical directory has a unique body. This projection retains the first
// TS path to each directory's contribution, without fabricating Rust results.
const firstVisit = (commands:any[]) => {
  const seen = new Set<string>();
  return commands.filter(c => {
    if (seen.has(c.content)) return false;
    seen.add(c.content); return true;
  });
};
const cases:any[] = [];
try {
  const plugin = join(scratch, 'assets-plugin');
  const input = {skillCount:3, assetsPerSkill:1600};
  await mkdir(plugin);
  await writeFile(join(plugin, 'plugin.json'), '{"name":"assets-plugin"}');
  for (let i=0; i<input.skillCount; i++) {
    const assets = join(plugin, 'assets-'+i);
    const skill = join(plugin, 'skills/skill-'+i);
    await mkdir(assets); await mkdir(skill, {recursive:true});
    // Write only to asset directories, then expose them as node_modules links.
    // Nothing is ever written through a node_modules path.
    for (let j=0; j<input.assetsPerSkill; j++) await writeFile(join(assets, `${j}.js`), 'asset');
    await writeFile(join(skill, 'SKILL.md'), `skill-${i}`);
    await symlink(`../../assets-${i}`, join(skill, 'node_modules'));
  }
  const loaded = await loadPluginFromPath(plugin, 'global');
  cases.push({id:'node-modules-assets', input, result:normalize(loaded.skills)});

  const levels = 12;
  const dag = join(scratch, 'dag'); await mkdir(dag);
  for (let i=0; i<=levels; i++) {
    const dir = join(dag, `d${String(i).padStart(2,'0')}`);
    await mkdir(dir); await writeFile(join(dir, 'z.md'), `node-${i}`);
    if (i<levels) for (const name of ['a','b'])
      await symlink(`../d${String(i+1).padStart(2,'0')}`, join(dir, name));
  }
  const expanded = await loadPluginCommands({pluginName:'p', baseDir:dag});
  cases.push({id:'alias-dag', input:{levels}, result:{commandCount:expanded.length, firstVisitCommands:normalize(firstVisit(expanded))}});

  const cycle = join(scratch, 'simple-cycle'); await mkdir(cycle);
  for (const name of ['a','b']) {
    await mkdir(join(cycle, name)); await writeFile(join(cycle, name, 'z.md'), name);
    await symlink('../'+(name==='a'?'b':'a'), join(cycle, name, 'link'));
  }
  const repeated = await loadPluginCommands({pluginName:'p', baseDir:cycle});
  cases.push({id:'simple-cycle', result:{commandCount:repeated.length, firstVisitCommands:normalize(firstVisit(repeated))}});
} finally { await rm(scratch, {recursive:true, force:true}); }
await mkdir(output, {recursive:true});
await writeFile(join(output, 'budget.json'), JSON.stringify({sourceCommit, node:process.version, sourceSha256, cases}, null, 2)+'\n');
console.log(JSON.stringify(cases.map(c => ({id:c.id, count:Array.isArray(c.result)?c.result.length:c.result.commandCount}))));
