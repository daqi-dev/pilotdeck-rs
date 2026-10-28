// Real fixed-baseline IO failures. No writes to the baseline or node_modules.
import assert from 'node:assert/strict';
import {execFileSync} from 'node:child_process';
import {createHash} from 'node:crypto';
import fs from 'node:fs';
import {tmpdir} from 'node:os';
import {join,resolve} from 'node:path';
import {pathToFileURL} from 'node:url';
import { sourceClosureSha256 } from '../../baseline_source_guard.mjs';
import { bindBaseline } from '../_shared/generator_guard.mjs';
const source='src/context/memory/edgeclaw-memory-core/src/core/storage/sqlite.ts';
const baseline=resolve(process.env.PDRS_BASELINE!),out=resolve(process.argv[2]!);
assert.equal(process.version,'v22.22.0'); assert.ok(process.env.PDRS_BASELINE); assert.ok(process.argv[2]);
const commit='cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';
const git=fs.realpathSync(process.env.PDRS_GIT||execFileSync('which',['git'],{encoding:'utf8'}).trim());
const provenance=bindBaseline({ baseline: baseline, commit: commit, gitExecutable: git }).provenance,sourceSha256=sourceClosureSha256(baseline,commit,git,[source]);
const scratch=fs.realpathSync(fs.mkdtempSync(join(tmpdir(),'pdrs-entries-io-')));
// Isolate os.homedir(), including the default repository configuration.
process.env.HOME=join(scratch,'home');
const {MemoryRepository}=await import(pathToFileURL(join(baseline,source)).href);
const cases:Record<string,unknown>={};
const locked:string[]=[];
function capture(name:string,root:string,action:()=>unknown){
  assert.throws(action,(error:any)=>{
    assert.ok(error.code && error.syscall,`${name}: no native IO failure`);
    cases[name]={error:error.code,message:error.message.replaceAll(root,'$ROOT')};return true;
  },`${name}: operation must fail`);
}
function lock(path:string){fs.chmodSync(path,0);locked.push(path);}
try{
  const root=join(scratch,'case');fs.mkdirSync(root);
  const repo=new MemoryRepository(join(root,'control.sqlite'),{memoryDir:join(root,'current/memory'),globalRootDir:join(root,'global'),workspaceDir:join(process.env.HOME!,'.pilotdeck')});
  lock(join(root,'current/memory/Project'));
  capture('file-scandir',root,()=>repo.getFileMemoryStore().listMemoryEntries({}));
  fs.chmodSync(join(root,'current/memory/Project'),0o700);
  // Traversal allowed, enumeration forbidden; native scandir validates the host.
  fs.chmodSync(root,0o100);locked.push(root);
  capture('external-scandir',root,()=>repo.getExternalWorkspaceSnapshots());
  fs.chmodSync(root,0o700);
  const last=join(root,'last_dream');fs.mkdirSync(join(last,'workspace'),{recursive:true});fs.mkdirSync(join(last,'global'));fs.writeFileSync(join(last,'metadata.json'),'{"version":1}');
  lock(join(last,'workspace'));
  capture('snapshot-scandir',root,()=>repo.getLastDreamSnapshotOverview());
  fs.chmodSync(join(last,'workspace'),0o700);
  fs.writeFileSync(join(last,'workspace/locked.md'),'saved');lock(join(last,'workspace/locked.md'));
  capture('snapshot-read',root,()=>repo.getLastDreamSnapshotOverview());
  fs.chmodSync(join(last,'workspace/locked.md'),0o600);
  lock(join(last,'metadata.json'));
  capture('metadata-read',root,()=>repo.getLastDreamSnapshotOverview());
  fs.chmodSync(join(last,'metadata.json'),0o600);
  fs.mkdirSync(join(root,'blocked'));lock(join(root,'blocked'));
  capture('snapshot-mkdir',root,()=>repo.writeSnapshotFilesToRoot(join(root,'blocked/new'),[]));
  fs.chmodSync(join(root,'blocked'),0o700);
  fs.mkdirSync(join(root,'write'));fs.writeFileSync(join(root,'write/locked.md'),'old');lock(join(root,'write/locked.md'));
  capture('snapshot-write',root,()=>repo.writeSnapshotFilesToRoot(join(root,'write'),[{relativePath:'locked.md',content:'new'}]));
  // cpSync's symlink fault aborts in this host's libc++; obtain the same native
  // unlink boundary from the baseline store's deleteEntries instead.
  fs.writeFileSync(join(root,'unlink-target'),'target');
  fs.symlinkSync(join(root,'unlink-target'),join(root,'current/memory/Project/copied.md'));
  fs.chmodSync(join(root,'current/memory/Project'),0o500);
  capture('copy-unlink',root,()=>repo.getFileMemoryStore().deleteEntries(['Project/copied.md']));
  repo.close();
  const bytes=JSON.stringify({sourceCommit:provenance.ref,sourceSha256,nodeVersion:process.version,cases},null,2)+'\n';
  fs.mkdirSync(out,{recursive:true});fs.writeFileSync(join(out,'io-oracle.json'),bytes);
  console.log(`sha256 ${createHash('sha256').update(bytes).digest('hex')}; ${Object.keys(cases).length} real TS IO failures`);
}finally{
  // Restore ancestors first, including when a permission probe fails early.
  for(const path of locked.sort((a,b)=>a.length-b.length))fs.chmodSync(path,0o700);
  fs.rmSync(scratch,{recursive:true,force:true});
}
