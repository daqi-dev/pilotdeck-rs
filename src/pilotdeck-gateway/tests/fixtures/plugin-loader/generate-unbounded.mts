// #484 repair 3: real TS observations; no writes to installed dependencies.
import assert from 'node:assert/strict';
import {execFileSync} from 'node:child_process';
import {realpathSync} from 'node:fs';
import {createHash} from 'node:crypto';
import {mkdir, mkdtemp, readFile, rm, symlink, writeFile} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join, resolve} from 'node:path';
import {fileURLToPath, pathToFileURL} from 'node:url';
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
const helper = fileURLToPath(new URL('./dependency-tree.py', import.meta.url));
await mkdir(output, {recursive:true});
const snapshot = join(output, 'dependency-tree.json.gz');
// Capture real installed paths/Markdown bytes read-only. Duplicate content is
// interned in the archive; non-Markdown contents are never read by the loader.
const inventory = JSON.parse(execFileSync('python3', [helper, 'capture', join(baseline,'node_modules'), snapshot], {encoding:'utf8'}));
const snapshotSha256 = createHash('sha256').update(await readFile(snapshot)).digest('hex');
const scratch = await mkdtemp(join(tmpdir(), 'pdrs-484-unbounded-'));
const normalize = (v:any) => JSON.parse(JSON.stringify(v).split(scratch).join('<root>'));
const cases:any[] = [];
try {
  const real = join(scratch, 'real-plugin');
  const vendor = join(real, 'skills/s0/vendor');
  await mkdir(vendor, {recursive:true});
  await writeFile(join(real,'plugin.json'), '{"name":"real-plugin"}');
  await writeFile(join(real,'skills/s0/SKILL.md'), 's0');
  await mkdir(join(real,'skills/s1'));
  await writeFile(join(real,'skills/s1/SKILL.md'), 's1');
  const backing = execFileSync('python3', [helper, 'materialize', snapshot, join(real,'dependency-data')], {encoding:'utf8'}).trim();
  await symlink(backing, join(vendor,'node_modules'));
  const loaded = await loadPluginFromPath(real, 'global');
  cases.push({id:'real-node-modules', input:{...inventory,snapshotSha256}, result:normalize(loaded.skills)});

  const mixed = join(scratch, 'mixed-plugin');
  const assets = join(mixed, 'dependency-data');
  const input = {skillCount:3,readmeCount:5000,jsCount:54000};
  await mkdir(assets, {recursive:true});
  await writeFile(join(mixed,'plugin.json'), '{"name":"mixed-plugin"}');
  for(let i=0;i<input.readmeCount;i++) {
    const pkg=join(assets, `p${String(i).padStart(5,'0')}`);
    await mkdir(pkg); await writeFile(join(pkg,'README.md'),'readme');
  }
  for(let i=0;i<input.jsCount;i++)
    await writeFile(join(assets,`asset-${String(i).padStart(5,'0')}.js`),'asset');
  for(let i=0;i<input.skillCount;i++) {
    const skill=join(mixed,`skills/s${i}`);
    await mkdir(skill,{recursive:true}); await writeFile(join(skill,'SKILL.md'),`s${i}`);
  }
  await symlink('../../dependency-data',join(mixed,'skills/s0/node_modules'));
  cases.push({id:'readme-and-js', input, result:normalize((await loadPluginFromPath(mixed,'global')).skills)});

  // All four configured loader paths see the same deep acyclic input.
  const deep = join(scratch,'configured-deep');
  await mkdir(deep);
  const depth=80;
  let leaf=join(deep,'deep');
  for(let i=0;i<depth;i++)leaf=join(leaf,'d');
  await mkdir(leaf,{recursive:true});
  const deepData=join(deep,'deep-data'); await mkdir(deepData);
  await writeFile(join(deepData,'SKILL.md'),'deep');
  await symlink(deepData,join(leaf,'node_modules'));
  await writeFile(join(deep,'plugin.json'),'{"name":"p","commands":"deep","skills":"deep","outputStyles":"deep"}');
  const plugin=await loadPluginFromPath(deep,'global');
  const {loadPluginOutputStylesFromPath}=await import(pathToFileURL(join(baseline,loaderPath)).href);
  const styles=await loadPluginOutputStylesFromPath(deep,'global');
  cases.push({id:'configured-deep',input:{depth},result:normalize({commands:plugin.commands,skills:plugin.skills,outputStyles:plugin.outputStyles,styleOnly:styles.outputStyles})});
} finally { await rm(scratch, {recursive:true,force:true}); }
await writeFile(join(output,'unbounded.json'), JSON.stringify({sourceCommit,node:process.version,sourceSha256,cases},null,2)+'\n');
console.log(JSON.stringify(cases.map(c=>({id:c.id,count:Array.isArray(c.result)?c.result.length:undefined,input:c.input}))));
