// #564 repairs 3/4: real TS observations for hardlink, deletion and ancestor file
// between traversal metadata and read. Source guard and all writes follow
// generate-toctou.mts; no socket or listener is needed for these cases.
import assert from 'node:assert/strict';
import {execFileSync} from 'node:child_process';
import {realpathSync} from 'node:fs';
import fsPromises from 'node:fs/promises';
import {syncBuiltinESMExports} from 'node:module';
import {link, mkdir, mkdtemp, rename, rm, writeFile} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join, resolve} from 'node:path';
import {pathToFileURL} from 'node:url';
import { sourceClosureSha256 } from '../../baseline_source_guard.mjs';
import { bindBaseline } from '../_shared/generator_guard.mjs';

const baseline = resolve(process.env.PDRS_BASELINE!);
const commandPath = 'src/extension/plugins/loading/PluginCommandLoader.ts';
const loaderPath = 'src/extension/plugins/loading/PluginLoader.ts';
{
  assert.ok(process.env.PDRS_BASELINE, 'PDRS_BASELINE required');
  assert.ok(process.argv[2], 'output directory required');
  assert.equal(process.versions.node.split('.')[0], '22', 'generate with Node 22');
  const sourceCommit = 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';
  const git = realpathSync(execFileSync('which', ['git'], {encoding: 'utf8'}).trim());
  bindBaseline({ baseline: baseline, commit: sourceCommit, gitExecutable: git });
  const gitEnv = Object.fromEntries(Object.entries(process.env).filter(([k]) => !k.startsWith('GIT_')));
  assert.equal(execFileSync(git, ['--no-replace-objects', '-C', baseline, 'status', '--porcelain', '--untracked-files=no'], {env: gitEnv}).toString(), '');
  const sourceSha256 = sourceClosureSha256(baseline, sourceCommit, git, [commandPath, loaderPath]);
  const {loadPluginCommands} = await import(pathToFileURL(join(baseline, commandPath)).href);
  const {loadPluginFromPath} = await import(pathToFileURL(join(baseline, loaderPath)).href);
  const output = resolve(process.argv[2]);
  assert.ok(output !== baseline && !output.startsWith(baseline + '/'), 'never write the baseline');
  // realpath: macOS tmpdir is under the /var -> /private/var link; Rust canonicalizes too.
  const scratch = realpathSync(await mkdtemp(join(tmpdir(), 'pdrs-564-toctou-')));
  const normalize = (v: any) => JSON.parse(JSON.stringify(v).split(scratch).join('<root>'));
  const load = async (base: string) => normalize(await loadPluginCommands({pluginName: 'p', baseDir: base}));
  const cases: any[] = [];
  // One layout per case; the raced entry is always commands/sub/b.md.
  const layout = async (id: string) => {
    const dir = join(scratch, id), base = join(dir, 'p/commands');
    await mkdir(join(base, 'sub'), {recursive: true});
    await mkdir(join(dir, 'outside/sub'), {recursive: true});
    await writeFile(join(base, 'a.md'), 'safe');
    await writeFile(join(base, 'sub/b.md'), '---\nkind: inside\n---\ninside');
    await writeFile(join(dir, 'outside/secret.md'), 'SECRET');
    await writeFile(join(dir, 'outside/sub/b.md'), 'SECRET');
    return {dir, base, leaf: join(base, 'sub/b.md')};
  };
  const races: Record<string, (dir: string, base: string, leaf: string) => Promise<void>> = {
    'leaf-hardlink': async (dir, _base, leaf) => { await rm(leaf); await link(join(dir, 'outside/secret.md'), leaf); },
  };
  try {
    for (const [id, race] of Object.entries(races)) {
      const {dir, base, leaf} = await layout(id);
      const checked = await load(base);
      await race(dir, base, leaf);
      const replaced = await load(base);
      // Control: the raced entry is gone, leaving an empty sub directory.
      await rm(join(base, 'sub'), {recursive: true});
      await mkdir(join(base, 'sub'));
      cases.push({id, checked, replaced, control: await load(base)});
    }
    // Remove the target only after the real stat has succeeded. Named built-in
    // exports are synchronized so the unchanged TS module observes the hook.
    // This exercises actual TS readFile rejection, not deletion before discovery.
    const deletedLayout = await layout('deleted-after-check');
    const checked = await load(deletedLayout.base);
    const originalStat = fsPromises.stat;
    let fired: string | undefined;
    let deleted: any;
    try {
      fsPromises.stat = (async (...args: Parameters<typeof originalStat>) => {
        const metadata = await originalStat(...args);
        if (String(args[0]) === deletedLayout.leaf) {
          assert.equal(fired, undefined, 'deletion hook fires once');
          fired = String(args[0]);
          await rm(deletedLayout.leaf);
        }
        return metadata;
      }) as typeof originalStat;
      syncBuiltinESMExports();
      deleted = await load(deletedLayout.base).then(
        (result: any) => ({rejected: false, result}),
        (e: NodeJS.ErrnoException) => ({rejected: true, code: e.code, syscall: e.syscall}));
      assert.equal(fired, deletedLayout.leaf, 'deletion hook fires on sub/b.md itself');
      assert.equal(deleted.rejected, true, 'real TS readFile rejects');
    } finally {
      fsPromises.stat = originalStat;
      syncBuiltinESMExports();
    }
    cases.push({id: 'deleted-after-check', checked, deleted, control: await load(deletedLayout.base)});
    // Exercise the real readFile after collection, then the real PluginLoader
    // catch per configured directory. Keep a second commands dir plus skills /
    // styles to observe the exact rejection boundary rather than infer it.
    const fileLayout = await layout('ancestor-file-after-check');
    const pluginPath = join(fileLayout.dir, 'p');
    await writeFile(join(pluginPath, 'plugin.json'), JSON.stringify({name: 'p', version: '1.0.0', commands: ['commands', 'other-commands']}));
    for (const [dir, file, content] of [['other-commands', 'c.md', 'other'], ['skills/s', 'SKILL.md', 'skill'], ['output-styles', 'style.md', 'style']]) {
      await mkdir(join(pluginPath, dir), {recursive: true});
      await writeFile(join(pluginPath, dir, file), content);
    }
    const projectPlugin = async () => {
      const plugin = await loadPluginFromPath(pluginPath, 'global');
      return normalize({commands: plugin.commands, skills: plugin.skills, outputStyles: plugin.outputStyles});
    };
    const fileChecked = await load(fileLayout.base);
    const configuredChecked = await projectPlugin();
    const swapDuringStat = async (run: () => Promise<any>) => {
      let fired: string | undefined;
      try {
        fsPromises.stat = (async (...args: Parameters<typeof originalStat>) => {
          const metadata = await originalStat(...args);
          if (String(args[0]) === fileLayout.leaf) {
            assert.equal(fired, undefined, 'ancestor-file hook fires once');
            fired = String(args[0]);
            await rename(join(fileLayout.base, 'sub'), join(fileLayout.dir, 'saved-sub'));
            await writeFile(join(fileLayout.base, 'sub'), 'ordinary file');
          }
          return metadata;
        }) as typeof originalStat;
        syncBuiltinESMExports();
        const result = await run();
        assert.equal(fired, fileLayout.leaf, 'ancestor-file hook fires on sub/b.md itself');
        return result;
      } finally {
        fsPromises.stat = originalStat;
        syncBuiltinESMExports();
      }
    };
    const replaced = await swapDuringStat(() => load(fileLayout.base).then(
      result => ({rejected: false, result}),
      (e: NodeJS.ErrnoException) => ({rejected: true, code: e.code, syscall: e.syscall})));
    await rm(join(fileLayout.base, 'sub'));
    await rename(join(fileLayout.dir, 'saved-sub'), join(fileLayout.base, 'sub'));
    const configuredRaced = await swapDuringStat(projectPlugin);
    cases.push({id: 'ancestor-file-after-check', checked: fileChecked, replaced, configuredChecked, configuredRaced});

  } finally {
    await rm(scratch, {recursive: true, force: true});
  }
  await mkdir(output, {recursive: true});
  await writeFile(join(output, 'pre-lstat.json'), JSON.stringify({sourceCommit, node: process.version, sourceSha256, cases}, null, 2) + '\n');
  console.log(JSON.stringify(cases.map(c => ({id: c.id, checked: c.checked?.length, replaced: Array.isArray(c.replaced) ? c.replaced.map((x: any) => x.content) : c.replaced, control: c.control?.length, configuredRaced: c.configuredRaced, deleted: c.deleted}))));
}
