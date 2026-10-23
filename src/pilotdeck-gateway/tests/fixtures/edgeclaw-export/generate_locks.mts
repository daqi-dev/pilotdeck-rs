// Public TS constructors/state writes reproduce the formerly cyclic layouts.
import assert from 'node:assert/strict';
import {execFileSync} from 'node:child_process';
import {createHash} from 'node:crypto';
import {mkdtempSync, mkdirSync, realpathSync, rmSync, writeFileSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join, resolve} from 'node:path';
import {pathToFileURL} from 'node:url';
import {bindSourceTree, sourceClosureSha256} from '../../baseline_source_guard.mjs';
import {freezeClock, clean} from './observe.mts';
assert.ok(process.env.PDRS_BASELINE); assert.ok(process.argv[2]);
const baseline=resolve(process.env.PDRS_BASELINE!);
const commit='cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';
const git=realpathSync(process.env.PDRS_GIT||execFileSync('which',['git'],{encoding:'utf8'}).trim());
const provenance=bindSourceTree(baseline,commit,git);
const source='src/context/memory/edgeclaw-memory-core/src/core/storage/sqlite.ts';
const sourceSha256=sourceClosureSha256(baseline,commit,git,[source]);
const root=mkdtempSync(join(tmpdir(),'pdrs-r9-ts-'));
process.env.HOME=join(root,'home'); freezeClock();
try {
  const {MemoryRepository}=await import(pathToFileURL(join(baseline,source)).href);
  const cases=[];
  for(const topology of ['general-single','two-general','three-general']) {
    const dir=join(root,topology), home=join(root,'home/.pilotdeck');
    const count=topology==='three-general'?3:2;
    const repos=Array.from({length:count},(_,i)=>new MemoryRepository(join(dir,'workspaces',String(i),'control.sqlite'),{
      memoryDir:join(dir,'workspaces',String(i),'memory'),
      globalRootDir:topology==='three-general'&&i===1?join(dir,'workspaces/2/memory'):join(dir,'zglobal'),
      workspaceDir:topology==='general-single'&&i===1?join(dir,'project-1'):home,
    }));
    // The process home must be the shared general workspace when constructing.
    // It is root/home/.pilotdeck for all cases; use that exact public value.
    for(const [i,r] of repos.entries()) {
      r.setPipelineState('workspaceDir',join(dir,`project-${i}`));
    }
    const observations=repos.map((r,i)=>topology==='three-general'&&i===1?r.listMemoryEntries({}):r.getUiSnapshot());
    const modes=repos.map(r=>r.getWorkspaceMode());
    // Assert this fixture exercises the intended mode, rather than silently
    // calibrating a different layout.
    assert.deepEqual(modes,Array.from({length:count},(_,i)=>topology==='general-single'&&i===1?'single':'general'));
    const warm=repos.map((r,i)=>topology==='three-general'&&i===1?r.listMemoryEntries({}):r.getUiSnapshot());
    assert.deepEqual(warm,observations);
    cases.push({topology,modes,observations:clean(observations,root)});
    repos.forEach(r=>r.close());
  }
  const text=JSON.stringify({sourceCommit:provenance.ref,sourceSha256,cases},null,2)+'\n';
  mkdirSync(resolve(process.argv[2]),{recursive:true});
  writeFileSync(join(resolve(process.argv[2]),'lock-oracle.json'),text);
  console.log(createHash('sha256').update(text).digest('hex'));
} finally {rmSync(root,{recursive:true,force:true});}
