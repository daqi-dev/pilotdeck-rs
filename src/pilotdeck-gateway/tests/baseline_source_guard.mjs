// Read-only Git-object binding; ignore index flags and .gitignore completely.
import fs from 'node:fs';
import path from 'node:path';
import {createHash} from 'node:crypto';
import {execFileSync} from 'node:child_process';
import {fileURLToPath} from 'node:url';
export function bindSourceTree(baseline, ref, gitExecutable) {
  if (!gitExecutable || !path.isAbsolute(gitExecutable)) throw Error('explicit absolute Git required');
  gitExecutable=fs.realpathSync(gitExecutable);
  for (const location of [baseline,path.resolve(path.dirname(fileURLToPath(import.meta.url)),'../../..')]) {
    const start=fs.realpathSync(location);
    for(let root=start;;root=path.dirname(root)) {
      if(root===start||fs.existsSync(path.join(root,'.git'))) {
        const relative=path.relative(root,gitExecutable);
        if(relative===''||(!relative.startsWith('..'+path.sep)&&relative!=='..'&&!path.isAbsolute(relative)))throw Error('workspace-local Git rejected');
      }
      if(path.dirname(root)===root)break;
    }
  }
  const gitSha256=createHash('sha256').update(fs.readFileSync(gitExecutable)).digest('hex');
  const env=Object.fromEntries(Object.entries(process.env).filter(([key])=>!key.startsWith('GIT_')));
  const git = (...args) => execFileSync(gitExecutable, ['--no-replace-objects','-C',baseline,...args], {env,maxBuffer:32*1024*1024});
  if (git('rev-parse','HEAD').toString().trim() !== ref) throw Error('baseline HEAD mismatch');
  const trackedChanges=git('status','--porcelain','--untracked-files=no').toString();
  if (trackedChanges!=='') throw Error('baseline has uncommitted changes to tracked files:\n'+trackedChanges);
  // Status deliberately ignores untracked files, but index flags can also hide
  // tracked modifications (including files outside src). Refuse those flags;
  // the src walk below independently verifies every source byte against HEAD.
  const hidden=git('ls-files','-v','-z').toString().split('\0')
    .filter(row=>row&&(row[0]==='S'||row[0]!==row[0].toUpperCase()));
  if (hidden.length) throw Error('#303 baseline has skip-worktree/assume-unchanged tracked files');
  const expected = new Map();
  for (const row of git('ls-tree','-rz',ref,'--','src').toString('utf8').split('\0').filter(Boolean)) {
    const at=row.indexOf('\t'), [mode,type,blob]=row.slice(0,at).split(' '), name=row.slice(at+1);
    if (!['100644','100755'].includes(mode)||type!=='blob'||!name.startsWith('src/')) throw Error('unsupported source entry '+name);
    expected.set(name,blob);
  }
  if (!expected.size) throw Error('empty baseline source tree');
  const files={};
  function walk(relative) {
    const absolute=path.join(baseline,relative), stat=fs.lstatSync(absolute);
    if (stat.isSymbolicLink()) throw Error('source symlink '+relative);
    if (stat.isDirectory()) {for(const entry of fs.readdirSync(absolute).sort())walk(relative+'/'+entry);return;}
    if (!stat.isFile()||!expected.has(relative)) throw Error('unregistered source '+relative);
    const bytes=fs.readFileSync(absolute), blob=createHash('sha1').update(`blob ${bytes.length}\0`).update(bytes).digest('hex');
    if (blob!==expected.get(relative)) throw Error('source bytes differ from trusted Git blob '+relative);
    files[relative]={blob,sha256:createHash('sha256').update(bytes).digest('hex'),size:bytes.length};
  }
  walk('src');
  if (Object.keys(files).length!==expected.size) throw Error('missing tracked source file');
  return {ref,git:{path:gitExecutable,sha256:gitSha256},count:expected.size,sha256:createHash('sha256').update(JSON.stringify(files)).digest('hex')};
}
// Relative-import closure of baseline entry files (bare/node: imports are
// external), each verified against and hashed from the commit blob. Shared by
// fixture generators whose `sourceSha256` covers the calibrating closure.
export function sourceClosureSha256(baseline, ref, gitExecutable, entries) {
  const resolveSource = (absolute) => {
    const stem = absolute.endsWith('.js') ? absolute.slice(0, -3) : undefined;
    for (const candidate of [absolute, `${absolute}.ts`, `${absolute}.tsx`, `${absolute}.mts`, path.join(absolute, 'index.ts'),
      ...(stem ? [`${stem}.ts`, `${stem}.tsx`] : [])]) {
      try { if (fs.lstatSync(candidate).isFile()) return candidate; } catch { /* next candidate */ }
    }
    return undefined;
  };
  const seen = new Set(entries), queue = [...entries];
  while (queue.length > 0) {
    const current = queue.shift();
    const text = fs.readFileSync(path.join(baseline, current), 'utf8');
    for (const match of text.matchAll(/(?:\bfrom\s*|\bimport\s*\(\s*|\bimport\s+)(['"])([^'"]+)\1/g)) {
      const specifier = match[2];
      if (!specifier.startsWith('./') && !specifier.startsWith('../')) continue;
      const resolved = resolveSource(path.join(baseline, path.dirname(current), specifier));
      if (!resolved) throw Error(`unresolved baseline import ${specifier} in ${current}`);
      const relative = path.relative(baseline, resolved).split(path.sep).join('/');
      if (!seen.has(relative)) { seen.add(relative); queue.push(relative); }
    }
  }
  const env=Object.fromEntries(Object.entries(process.env).filter(([key])=>!key.startsWith('GIT_')));
  const hashes = {};
  for (const relative of [...seen].sort()) {
    if (!relative.startsWith('src/')) throw Error(`import closure escaped src/: ${relative}`);
    const committed = execFileSync(gitExecutable, ['--no-replace-objects','-C',baseline,'show',`${ref}:${relative}`], {env,maxBuffer:32*1024*1024});
    if (!fs.readFileSync(path.join(baseline, relative)).equals(committed)) throw Error(`${relative} differs from ${ref}`);
    hashes[relative] = createHash('sha256').update(committed).digest('hex');
  }
  return hashes;
}
