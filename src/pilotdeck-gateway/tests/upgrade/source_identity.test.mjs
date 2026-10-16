// PREPARED_NOT_RUN. Actual helper and actual native Git, no product process.
import test from 'node:test';
import assert from 'node:assert/strict';
import {mkdtempSync,mkdirSync,writeFileSync,readFileSync,unlinkSync,symlinkSync} from 'node:fs';
import {join,dirname,isAbsolute} from 'node:path';
import {execFileSync} from 'node:child_process';
import {producerInputs,captureSourceIdentity,verifySourceIdentity} from './source_identity.mjs';

const parent=process.env.PDRS_SOURCE_IDENTITY_FIXTURES;
function fixture(){
  assert.ok(parent&&isAbsolute(parent),'explicit owned fixture parent required');
  const root=mkdtempSync(join(parent,'source-identity-'));
  const put=(path,value)=>{mkdirSync(dirname(join(root,path)),{recursive:true});writeFileSync(join(root,path),value);};
  const git=(...args)=>execFileSync('/usr/bin/git',['-c','core.hooksPath=/dev/null','-C',root,...args],{timeout:10000,maxBuffer:1024*1024,env:{PATH:'/usr/bin:/bin',HOME:root,GIT_CONFIG_GLOBAL:'/dev/null',GIT_CONFIG_NOSYSTEM:'1',LANG:'C'}});
  git('init','-q');
  for(const path of producerInputs.required)put(path,path.endsWith('/source_identity.mjs')?readFileSync(new URL('./source_identity.mjs',import.meta.url)):'fixture '+path+'\n');
  for(const tree of producerInputs.trees)put(tree+'/entry.rs','fixture tree\n');
  for(const name of ['upstream.rs','main.rs','acceptor.rs'])put('src/pilotdeck-gateway/src/'+name,'fixture '+name+'\n');
  put('src/pilotdeck-gateway/src/space\n name.rs','newline path\n');
  put('.gitignore','src/pilotdeck-gateway/build.rs\n.cargo/config.toml\nsrc/pilotdeck-gateway/src/ignored.rs\n');
  git('add','-f','.');
  const commit=()=>git('-c','user.name=Fixture','-c','user.email=fixture@example.invalid','commit','-qm','fixture');
  commit();return {root,put,git,commit};
}

test('source identity fixed producerInputs git fixture matrix',t=>{
  const rows=[];let completed=false;
  t.after(()=>writeFileSync(join(parent,'source-identity-results.json'),JSON.stringify({status:completed&&!rows.some(r=>r.status==='FAIL')?'CHECKED_HELPER_ONLY':'FAILED_HELPER_ONLY',completed,rows,product:'NOT_RUN',artifactOrigin:'NOT_PROVED'},null,2)+'\n',{flag:'wx'}));
  function run(id,action){const f=fixture();try{action(f);rows.push({id,fixture:f.root,status:'CHECKED'});}catch(error){rows.push({id,fixture:f.root,status:'FAIL',error:String(error)});throw error;}}
  run('G0-clean',f=>{const before=captureSourceIdentity(f.root);assert.ok(before.files.length>producerInputs.required.length);assert.ok(before.files.some(x=>x.path.includes('\n')));assert.ok(before.optional.some(x=>x.path.endsWith('/build.rs')&&!x.present));assert.deepEqual(verifySourceIdentity(f.root,before),before);});
  run('G0-optional-present',f=>{const path='src/pilotdeck-gateway/build.rs';f.put(path,'fixture build\n');f.git('add','-f','--',path);f.commit();const before=captureSourceIdentity(f.root);assert.ok(before.files.some(x=>x.path===path));assert.ok(before.optional.some(x=>x.path===path&&x.present));assert.deepEqual(verifySourceIdentity(f.root,before),before);});
  for(const [id,path,staged]of [
    ['G1-unstaged','src/pilotdeck-gateway/src/upstream.rs',false],
    ['G2-staged','src/pilotdeck-gateway/src/upstream.rs',true],
    ['G4-main','src/pilotdeck-gateway/src/main.rs',true],
    ['G4-acceptor','src/pilotdeck-gateway/src/acceptor.rs',true],
    ['G5-manifest','src/pilotdeck-gateway/Cargo.toml',true],
    ['G5-protocol','src/pilotdeck-protocol/src/entry.rs',true],
    ['G11-newline','src/pilotdeck-gateway/src/space\n name.rs',true],
  ])run(id,f=>{f.put(path,'changed\n');if(staged)f.git('add','--',path);if(id==='G2-staged')assert.doesNotThrow(()=>f.git('diff','--exit-code','--','src/pilotdeck-gateway/src','src/Cargo.toml','src/Cargo.lock'));assert.throws(()=>captureSourceIdentity(f.root),{code:'SOURCE_DIRTY'});});
  run('G3-index-worktree-cancel',f=>{const path='src/pilotdeck-gateway/src/upstream.rs';const original=readFileSync(join(f.root,path));f.put(path,'staged\n');f.git('add','--',path);f.put(path,original);assert.doesNotThrow(()=>f.git('diff','HEAD','--exit-code','--',path));assert.throws(()=>captureSourceIdentity(f.root),{code:'SOURCE_DIRTY'});});
  for(const [id,path]of [['G6-untracked','src/pilotdeck-gateway/src/new.rs'],['G6-ignored-source','src/pilotdeck-gateway/src/ignored.rs'],['G6-ignored-build','src/pilotdeck-gateway/build.rs'],['G6-ignored-config','.cargo/config.toml']])run(id,f=>{f.put(path,'untracked\n');assert.throws(()=>captureSourceIdentity(f.root),error=>['SOURCE_DIRTY','SOURCE_UNTRACKED'].includes(error.code));});
  run('G7-deleted',f=>{unlinkSync(join(f.root,'src/pilotdeck-gateway/Cargo.toml'));assert.throws(()=>captureSourceIdentity(f.root),{code:'SOURCE_DIRTY'});});
  run('G7-missing-required-clean',f=>{unlinkSync(join(f.root,'src/pilotdeck-gateway/Cargo.toml'));f.git('add','-u');f.commit();assert.throws(()=>captureSourceIdentity(f.root),{code:'SOURCE_REQUIRED'});});
  run('G7-empty-tree-clean',f=>{unlinkSync(join(f.root,'src/pilotdeck-protocol/src/entry.rs'));f.git('add','-u');f.commit();assert.throws(()=>captureSourceIdentity(f.root),{code:'SOURCE_TREE'});});
  run('G7-empty-before',f=>assert.throws(()=>verifySourceIdentity(f.root,{version:1,contract:producerInputs,files:[]}),{code:'SOURCE_BEFORE_INVALID'}));
  run('G7-trim-before',f=>{const before=captureSourceIdentity(f.root);assert.throws(()=>verifySourceIdentity(f.root,{...before,files:before.files.slice(1)}),{code:'SOURCE_IDENTITY_DRIFT'});});
  run('G7-extra-before',f=>{const before=captureSourceIdentity(f.root);assert.throws(()=>verifySourceIdentity(f.root,{...before,files:[...before.files,{path:'invented',sha256:'invented'}]}),{code:'SOURCE_IDENTITY_DRIFT'});});
  run('G8-source-after',f=>{const before=captureSourceIdentity(f.root);f.put('src/pilotdeck-gateway/src/upstream.rs','after\n');assert.throws(()=>verifySourceIdentity(f.root,before),{code:'SOURCE_DIRTY'});});
  run('G8-head-after',f=>{const before=captureSourceIdentity(f.root);f.put('docs/only.md','new head\n');f.git('add','docs');f.commit();assert.throws(()=>verifySourceIdentity(f.root,before),{code:'SOURCE_IDENTITY_DRIFT'});});
  run('G9-outside-scope',f=>{const before=captureSourceIdentity(f.root);f.put('state/example.md','state\n');f.put('docs/example.md','docs\n');assert.deepEqual(verifySourceIdentity(f.root,before),before);});
  run('G11-tracked-symlink',f=>{const path='src/pilotdeck-gateway/src/upstream.rs';unlinkSync(join(f.root,path));symlinkSync('main.rs',join(f.root,path));f.git('add','--',path);f.commit();assert.throws(()=>captureSourceIdentity(f.root),{code:'SOURCE_SYMLINK'});});
  run('G11-not-repository',f=>{mkdirSync(join(f.root,'empty'));assert.throws(()=>captureSourceIdentity(join(f.root,'empty')),{code:'SOURCE_ROOT'});});
  run('G11-git-command-error',f=>{f.put('.git/config','[invalid\n');assert.throws(()=>captureSourceIdentity(f.root),error=>Number.isInteger(error.status)&&error.status!==0);});
  completed=true;
});
