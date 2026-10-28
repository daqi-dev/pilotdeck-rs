// PR #537 round 7. Real fixed TS robustRenameSync failures, plus separately
// labelled Node native boundaries for races not deterministically reachable.
import assert from 'node:assert/strict';
import {execFileSync} from 'node:child_process';
import {createHash} from 'node:crypto';
import fs from 'node:fs';
import {syncBuiltinESMExports} from 'node:module';
import {tmpdir} from 'node:os';
import {join,resolve} from 'node:path';
import {pathToFileURL} from 'node:url';
import { sourceClosureSha256 } from '../../baseline_source_guard.mjs';
import { bindBaseline } from '../_shared/generator_guard.mjs';
assert.equal(process.version,'v22.22.0');
assert.ok(process.env.PDRS_BASELINE);assert.ok(process.argv[2]);
const baseline=resolve(process.env.PDRS_BASELINE!),out=resolve(process.argv[2]!);
const source='src/context/memory/edgeclaw-memory-core/src/core/storage/sqlite.ts';
const commit='cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';
const git=fs.realpathSync(process.env.PDRS_GIT||execFileSync('which',['git'],{encoding:'utf8'}).trim());
const provenance=bindBaseline({ baseline: baseline, commit: commit, gitExecutable: git }).provenance;
const sourceSha256=sourceClosureSha256(baseline,commit,git,[source]);
const scratch=fs.realpathSync(fs.mkdtempSync(join(tmpdir(),'pdrs-pr537-round7-')));
process.env.HOME=join(scratch,'home');
const {MemoryRepository}=await import(pathToFileURL(join(baseline,source)).href);
const cases:Record<string,unknown>={};
const nativeBoundaries:Record<string,unknown>={};
function capture(rows:Record<string,unknown>,name:string,root:string,action:()=>unknown){
  assert.throws(action,(e:any)=>{
    assert.ok(e.code && e.syscall,`${name}: expected native IO error`);
    assert.equal(e.syscall,name,`${name}: wrong fault boundary`);
    rows[name]={error:e.code,message:e.message.replaceAll(root,'$ROOT')};return true;
  },`${name}: must fail`);
}
const rename=fs.renameSync;
const cp=fs.cpSync;
let repo:any;
try{
  const root=join(scratch,'case');fs.mkdirSync(root);
  repo=new MemoryRepository(join(root,'control.sqlite'),{memoryDir:join(root,'memory'),globalRootDir:join(root,'global'),workspaceDir:join(scratch,'workspace')});
  const src=join(root,'source'),dst=join(root,'blocked/destination');
  fs.mkdirSync(join(root,'blocked'));fs.writeFileSync(join(root,'target'),'target');
  for(const name of ['lstat','copyfile','symlink','rename']){
    let renamed=0,copied=0;
    if(name==='copyfile')fs.writeFileSync(src,'payload');
    if(name==='symlink')fs.symlinkSync(join(root,'target'),src);
    fs.chmodSync(join(root,'blocked'),name==='copyfile'||name==='symlink'?0o500:0o700);
    fs.renameSync=((...args:any[])=>{
      renamed++;assert.equal(args[0],src);assert.equal(args[1],dst);
      if(name!=='lstat')return (rename as any)(...args);
      // Only the lstat case injects rename admission (source is absent). The
      // fallback is real cpSync; its error is never manufactured or rewritten.
      const e:any=new Error('force robustRenameSync fallback');e.code='EACCES';throw e;
    }) as any;
    fs.cpSync=((...args:any[])=>{copied++;return (cp as any)(...args);}) as any;
    syncBuiltinESMExports();
    try{
      capture(cases,name,root,()=>repo.replaceDirectoryWithStaged(dst,src));
      assert.equal(renamed,1);assert.equal(copied,name==='rename'?0:1);
    }finally{fs.renameSync=rename;fs.cpSync=cp;syncBuiltinESMExports();fs.chmodSync(join(root,'blocked'),0o700);fs.rmSync(src,{force:true});}
  }
  // Native-only evidence, not claimed as end-to-end TS cpSync races. These
  // operations are the Rust counterparts of readlink / canonical / permissions.
  const missing=join(root,'missing');
  capture(nativeBoundaries,'readlink',root,()=>fs.readlinkSync(missing));
  capture(nativeBoundaries,'realpath',root,()=>fs.realpathSync.native(missing));
  capture(nativeBoundaries,'chmod',root,()=>fs.chmodSync(missing,0o700));
  repo.close();repo=undefined;
  const bytes=JSON.stringify({sourceCommit:provenance.ref,sourceSha256,nodeVersion:process.version,cases,nativeBoundaries},null,2)+'\n';
  fs.mkdirSync(out,{recursive:true});fs.writeFileSync(join(out,'copy-io-oracle.json'),bytes);
  console.log(`sha256 ${createHash('sha256').update(bytes).digest('hex')}; ${Object.keys(cases).length} real TS failures; ${Object.keys(nativeBoundaries).length} native-only boundaries`);
}finally{
  fs.renameSync=rename;fs.cpSync=cp;syncBuiltinESMExports();repo?.close();
  const blocked=join(scratch,'case/blocked');if(fs.existsSync(blocked))fs.chmodSync(blocked,0o700);
  fs.rmSync(scratch,{recursive:true,force:true});
}
