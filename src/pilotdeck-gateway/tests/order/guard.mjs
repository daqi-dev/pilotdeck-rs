import fs from 'node:fs';
import path from 'node:path';
import {createHash} from 'node:crypto';
import {execFileSync} from 'node:child_process';
export const REF='cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';
export const sha = bytes=>createHash('sha256').update(bytes).digest('hex');
export function bind(baseline,gitExecutable) {
  if(!path.isAbsolute(gitExecutable))throw Error('explicit absolute Git required');
  const git=(...a)=>execFileSync(gitExecutable,['--no-replace-objects','-C',baseline,...a],{
    maxBuffer:32*1024*1024,timeout:10000,
    env:{PATH:'/usr/bin:/bin',GIT_CONFIG_NOSYSTEM:'1',GIT_CONFIG_GLOBAL:'/dev/null',GIT_OPTIONAL_LOCKS:'0',LANG:'C'},
  });
  if(git('rev-parse','HEAD').toString().trim()!==REF)throw Error('baseline HEAD mismatch');
  const expected=new Map();
  for(const row of git('ls-tree','-rz',REF,'--','src').toString().split('\0').filter(Boolean)) {
    const tab=row.indexOf('\t'), [mode,type,oid]=row.slice(0,tab).split(' '), name=row.slice(tab+1);
    if(!['100644','100755'].includes(mode)||type!=='blob')throw Error('unsupported source entry');
    expected.set(name,oid);
  }
  const seen=[];
  function visit(name) {
    const p=path.join(baseline,name), stat=fs.lstatSync(p);
    if(stat.isSymbolicLink())throw Error('source symlink: '+name);
    if(stat.isDirectory()){for(const child of fs.readdirSync(p).sort())visit(name+'/'+child);return;}
    if(!stat.isFile()||!expected.has(name))throw Error('shadow/unregistered source: '+name);
    const bytes=fs.readFileSync(p), oid=createHash('sha1').update(`blob ${bytes.length}\0`).update(bytes).digest('hex');
    if(oid!==expected.get(name))throw Error('source drift: '+name);
    seen.push([name,oid,sha(bytes)]);
  }
  visit('src'); if(seen.length!==expected.size||seen.length!==1000)throw Error('source cardinality mismatch');
  const resolution={};
  for(const name of ['package.json','tsconfig.json','pnpm-lock.yaml']){
    const p=path.join(baseline,name),stat=fs.lstatSync(p);if(!stat.isFile()||stat.isSymbolicLink())throw Error('resolution config is not regular: '+name);
    const actual=fs.readFileSync(p),trusted=git('cat-file','blob',REF+':'+name);
    if(!actual.equals(trusted))throw Error('resolution config drift: '+name);
    resolution[name]=sha(actual);
  }
  return {ref:REF,count:seen.length,manifestSha256:sha(JSON.stringify(seen)),resolution};
}
