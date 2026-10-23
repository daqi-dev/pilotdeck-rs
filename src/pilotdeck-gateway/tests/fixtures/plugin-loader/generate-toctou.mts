// #564: real TS observations for plugin Markdown replaced after a check.
// TS has no root containment and no check/open window (PluginCommandLoader.ts:92
// stat follows links, :68 readFile by lexical path), so each race end state is
// recorded exactly as TS reads it. `control` (raced entry removed) is what
// ROOT-001 retains; Rust never synthesizes these values.
import assert from 'node:assert/strict';
import {execFileSync, spawnSync} from 'node:child_process';
import {realpathSync} from 'node:fs';
import {createServer} from 'node:net';
import {link, mkdir, mkdtemp, rename, rm, symlink, writeFile} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join, resolve} from 'node:path';
import {fileURLToPath, pathToFileURL} from 'node:url';
import {bindSourceTree, sourceClosureSha256} from '../../baseline_source_guard.mjs';

const __dirname = fileURLToPath(new URL('.', import.meta.url));
const baseline = resolve(process.env.PDRS_BASELINE!);
const commandPath = 'src/extension/plugins/loading/PluginCommandLoader.ts';
if (process.argv[2] === '--child') {
  const {loadPluginCommands} = await import(pathToFileURL(join(baseline, commandPath)).href);
  const result = await loadPluginCommands({pluginName: 'p', baseDir: process.argv[3]});
  console.log(JSON.stringify({commandCount: result.length}));
} else {
  assert.ok(process.env.PDRS_BASELINE, 'PDRS_BASELINE required');
  assert.ok(process.argv[2], 'output directory required');
  assert.equal(process.versions.node.split('.')[0], '22', 'generate with Node 22');
  const sourceCommit = 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';
  const git = realpathSync(execFileSync('which', ['git'], {encoding: 'utf8'}).trim());
  bindSourceTree(baseline, sourceCommit, git);
  const gitEnv = Object.fromEntries(Object.entries(process.env).filter(([k]) => !k.startsWith('GIT_')));
  assert.equal(execFileSync(git, ['--no-replace-objects', '-C', baseline, 'status', '--porcelain', '--untracked-files=no'], {env: gitEnv}).toString(), '');
  const sourceSha256 = sourceClosureSha256(baseline, sourceCommit, git, [commandPath]);
  const {loadPluginCommands} = await import(pathToFileURL(join(baseline, commandPath)).href);
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
    'leaf-symlink': async (_dir, _base, leaf) => { await rm(leaf); await symlink('../../../outside/secret.md', leaf); },
    'leaf-hardlink': async (dir, _base, leaf) => { await rm(leaf); await link(join(dir, 'outside/secret.md'), leaf); },
    'ancestor-symlink': async (dir, base) => { await rename(join(base, 'sub'), join(dir, 'saved-sub')); await symlink('../../outside/sub', join(base, 'sub')); },
    // The checked inode itself moved outside the root behind a link (same dev+ino).
    'leaf-moved-symlink': async (dir, _base, leaf) => { await rename(leaf, join(dir, 'outside/moved.md')); await symlink('../../../outside/moved.md', leaf); },
    'ancestor-moved-symlink': async (dir, base) => { await rename(join(base, 'sub'), join(dir, 'saved-sub')); await symlink('../../saved-sub', join(base, 'sub')); },
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
    // A FIFO .md blocks TS readFile; observe it in a killable child process.
    const {base, leaf} = await layout('static-fifo');
    await rm(leaf);
    execFileSync('mkfifo', [leaf]);
    const child = spawnSync(process.execPath, [...process.execArgv, fileURLToPath(import.meta.url), '--child', base], {encoding: 'utf8', timeout: 1000, killSignal: 'SIGKILL'});
    const timedOut = (child.error as NodeJS.ErrnoException | undefined)?.code === 'ETIMEDOUT';
    assert.ok(timedOut || child.status === 0, child.stderr);
    const fifo = timedOut ? {timeoutMs: 1000, timedOut} : {timedOut, result: JSON.parse(child.stdout)};
    await rm(leaf);
    cases.push({id: 'static-fifo', fifo, control: await load(base)});
    // A socket .md: TS readFile's open fails and rejects the whole load. Bind on a
    // short path inside scratch (sun_path limit), then move it into place.
    const sock = await layout('static-socket');
    await rm(sock.leaf);
    const server = createServer();
    await new Promise<void>((ok, fail) => { server.once('error', fail); server.listen(join(scratch, 's'), ok); });
    await rename(join(scratch, 's'), sock.leaf);
    const socket = await load(sock.base).then(
      (result: any) => ({rejected: false, result}),
      (e: NodeJS.ErrnoException) => ({rejected: true, code: e.code, syscall: e.syscall}));
    await new Promise(ok => server.close(ok));
    await rm(sock.leaf, {force: true});
    cases.push({id: 'static-socket', socket, control: await load(sock.base)});
  } finally {
    await rm(scratch, {recursive: true, force: true});
  }
  await mkdir(output, {recursive: true});
  await writeFile(join(output, 'toctou.json'), JSON.stringify({sourceCommit, node: process.version, sourceSha256, cases}, null, 2) + '\n');
  console.log(JSON.stringify(cases.map(c => ({id: c.id, checked: c.checked?.length, replaced: c.replaced?.map((x: any) => x.content), control: c.control.length, fifo: c.fifo, socket: c.socket}))));
}
