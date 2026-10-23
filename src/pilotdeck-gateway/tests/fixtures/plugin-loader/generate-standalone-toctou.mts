// #594: real TS observations for a standalone skill's SKILL.md replaced after
// discovery. TS `loadStandaloneSkill` has no root containment and no readdir/
// readFile window guard (PluginCommandLoader.ts:37-42 readdir + find, :68
// readFile by lexical path), so each end state is recorded exactly as TS reads
// it. A standalone skill is a single file: a failed readFile rejects the whole
// load. Rust never synthesizes these values.
import assert from 'node:assert/strict';
import {execFileSync, spawnSync} from 'node:child_process';
import {realpathSync} from 'node:fs';
import {createServer} from 'node:net';
import fsPromises from 'node:fs/promises';
import {syncBuiltinESMExports} from 'node:module';
import {link, mkdir, mkdtemp, rename, rm, symlink, writeFile} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join, resolve} from 'node:path';
import {fileURLToPath, pathToFileURL} from 'node:url';
import {bindSourceTree, sourceClosureSha256} from '../../baseline_source_guard.mjs';

const baseline = resolve(process.env.PDRS_BASELINE!);
const commandPath = 'src/extension/plugins/loading/PluginCommandLoader.ts';
const loaderPath = 'src/extension/plugins/loading/PluginLoader.ts';
if (process.argv[2] === '--child') {
  // Observe a blocking readFile (FIFO) in a killable child, like generate-toctou.
  const {loadStandaloneSkill} = await import(pathToFileURL(join(baseline, commandPath)).href);
  const skill = await loadStandaloneSkill({name: 'skill', skillDir: process.argv[3]});
  console.log(JSON.stringify({content: skill.content}));
} else {
  assert.ok(process.env.PDRS_BASELINE, 'PDRS_BASELINE required');
  assert.ok(process.argv[2], 'output directory required');
  assert.equal(process.versions.node.split('.')[0], '22', 'generate with Node 22');
  const sourceCommit = 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';
  const git = realpathSync(execFileSync('which', ['git'], {encoding: 'utf8'}).trim());
  bindSourceTree(baseline, sourceCommit, git);
  const gitEnv = Object.fromEntries(Object.entries(process.env).filter(([k]) => !k.startsWith('GIT_')));
  assert.equal(execFileSync(git, ['--no-replace-objects', '-C', baseline, 'status', '--porcelain', '--untracked-files=no'], {env: gitEnv}).toString(), '');
  const sourceSha256 = sourceClosureSha256(baseline, sourceCommit, git, [commandPath, loaderPath]);
  const {loadStandaloneSkill} = await import(pathToFileURL(join(baseline, commandPath)).href);
  const output = resolve(process.argv[2]);
  assert.ok(output !== baseline && !output.startsWith(baseline + '/'), 'never write the baseline');
  // realpath: macOS tmpdir is under the /var -> /private/var link; Rust canonicalizes too.
  const scratch = realpathSync(await mkdtemp(join(tmpdir(), 'pdrs-594-standalone-')));
  const normalize = (v: any) => JSON.parse(JSON.stringify(v).split(scratch).join('<root>'));
  // The standalone skill's directory basename is its model-facing name.
  const load = async (skillDir: string) => normalize(await loadStandaloneSkill({name: 'skill', skillDir}));
  const project = (c: any) => ({name: c.name, path: c.path, content: c.content, isSkill: c.isSkill});
  // One directory per case; the raced entry is always <skillDir>/SKILL.md.
  const layout = async (id: string) => {
    const dir = join(scratch, id), skillDir = join(dir, 'skill');
    await mkdir(join(skillDir, 'references'), {recursive: true});
    await mkdir(join(dir, 'outside'), {recursive: true});
    await writeFile(join(skillDir, 'SKILL.md'), '---\nname: docx\ndescription: edit docs\n---\n\n# DOCX skill\n');
    await writeFile(join(skillDir, 'references', 'workflows.md'), '# Workflows\n');
    await writeFile(join(skillDir, 'real.md'), '---\nname: real\n---\n\n# In-root target\n');
    await writeFile(join(dir, 'outside', 'secret.md'), 'SECRET');
    return {dir, skillDir, leaf: join(skillDir, 'SKILL.md')};
  };
  const observe = (skillDir: string) => load(skillDir).then(
    (result: any) => ({rejected: false, skill: project(result)}),
    (e: NodeJS.ErrnoException) => ({rejected: true, code: e.code, syscall: e.syscall}));
  const cases: any[] = [];
  // The genuine, unraced load: every Rust unraced case must replay this content.
  const checkedLayout = await layout('checked');
  const checked = project(await load(checkedLayout.skillDir));
  try {
    // Static leaf links/inodes: readdir lists the entry name, readFile follows.
    const staticSwaps: Record<string, (dir: string, leaf: string) => Promise<void>> = {
      // Out-of-root symlink: TS reads the secret outside the skill directory.
      'leaf-symlink': async (_dir, leaf) => { await rm(leaf); await symlink('../outside/secret.md', leaf); },
      // Hardlink to an out-of-root inode: same name, different file, TS reads it.
      'leaf-hardlink': async (dir, leaf) => { await rm(leaf); await link(join(dir, 'outside/secret.md'), leaf); },
      // In-root symlink: TS (and the hardened Rust) both follow it inside root.
      'in-root-symlink': async (_dir, leaf) => { await rm(leaf); await symlink('./real.md', leaf); },
    };
    for (const [id, swap] of Object.entries(staticSwaps)) {
      const {dir, skillDir, leaf} = await layout(id);
      await swap(dir, leaf);
      cases.push({id, observed: await observe(skillDir)});
    }
    // A FIFO SKILL.md blocks TS readFile; observe it in a killable child.
    const fifo = await layout('static-fifo');
    await rm(fifo.leaf);
    execFileSync('mkfifo', [fifo.leaf]);
    const child = spawnSync(process.execPath, [...process.execArgv, fileURLToPath(import.meta.url), '--child', fifo.skillDir], {encoding: 'utf8', timeout: 1000, killSignal: 'SIGKILL'});
    const timedOut = (child.error as NodeJS.ErrnoException | undefined)?.code === 'ETIMEDOUT';
    assert.ok(timedOut || child.status === 0, child.stderr);
    await rm(fifo.leaf);
    cases.push({id: 'static-fifo', observed: timedOut ? {timeoutMs: 1000, timedOut} : {timedOut, content: JSON.parse(child.stdout).content}});
    // A socket SKILL.md: readFile's open fails and rejects the load. Bind on a
    // short path inside scratch (sun_path limit), then move it into place.
    const sock = await layout('static-socket');
    await rm(sock.leaf);
    const server = createServer();
    await new Promise<void>((ok, fail) => { server.once('error', fail); server.listen(join(scratch, 's'), ok); });
    await rename(join(scratch, 's'), sock.leaf);
    cases.push({id: 'static-socket', observed: await observe(sock.skillDir)});
    await new Promise(ok => server.close(ok));
    await rm(sock.leaf, {force: true});
    // Delete SKILL.md after readdir resolves (real window), before readFile.
    // Named built-in exports are synchronized so the unchanged TS module sees it.
    const deletedLayout = await layout('deleted-after-check');
    const originalReaddir = fsPromises.readdir;
    let fired: string | undefined;
    try {
      fsPromises.readdir = (async (...args: Parameters<typeof originalReaddir>) => {
        const entries = await originalReaddir(...args);
        if (String(args[0]) === deletedLayout.skillDir) {
          assert.equal(fired, undefined, 'deletion hook fires once');
          fired = String(args[0]);
          await rm(deletedLayout.leaf);
        }
        return entries;
      }) as typeof originalReaddir;
      syncBuiltinESMExports();
      const observed = await observe(deletedLayout.skillDir);
      assert.equal(fired, deletedLayout.skillDir, 'deletion hook fires on the skill dir');
      assert.equal(observed.rejected, true, 'real TS readFile rejects after deletion');
      cases.push({id: 'deleted-after-check', observed});
    } finally {
      fsPromises.readdir = originalReaddir;
      syncBuiltinESMExports();
    }
  } finally {
    await rm(scratch, {recursive: true, force: true});
  }
  await mkdir(output, {recursive: true});
  await writeFile(join(output, 'standalone-toctou.json'), JSON.stringify({sourceCommit, node: process.version, sourceSha256, checked, cases}, null, 2) + '\n');
  console.log(JSON.stringify({checked: checked.content, cases: cases.map(c => ({id: c.id, observed: c.observed}))}));
}
