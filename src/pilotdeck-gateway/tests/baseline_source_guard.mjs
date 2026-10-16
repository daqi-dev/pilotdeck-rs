// Read-only Git-object binding; ignore index flags and .gitignore completely.
import fs from 'node:fs';
import path from 'node:path';
import {createHash} from 'node:crypto';
import {execFileSync} from 'node:child_process';
export function bindSourceTree(baseline, ref) {
  const git = (...args) => execFileSync('git', ['--no-replace-objects','-C',baseline,...args], {maxBuffer:32*1024*1024});
  if (git('rev-parse','HEAD').toString().trim() !== ref) throw Error('baseline HEAD mismatch');
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
  return {ref,count:expected.size,sha256:createHash('sha256').update(JSON.stringify(files)).digest('hex')};
}
