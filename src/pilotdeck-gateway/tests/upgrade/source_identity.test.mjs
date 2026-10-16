// Source identity regression fixtures using native Git and an explicit owned parent.
import test from 'node:test';
import assert from 'node:assert/strict';
import {mkdtempSync,mkdirSync,writeFileSync,readFileSync,unlinkSync,symlinkSync,renameSync} from 'node:fs';
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
  for(const [id,path,code]of [
    ['G6-untracked','src/pilotdeck-gateway/src/new.rs','SOURCE_DIRTY'],
    ['G6-ignored-source','src/pilotdeck-gateway/src/ignored.rs','SOURCE_UNTRACKED'],
    ['G6-ignored-build','src/pilotdeck-gateway/build.rs','SOURCE_UNTRACKED'],
    ['G6-ignored-config','.cargo/config.toml','SOURCE_UNTRACKED'],
  ])run(id,f=>{f.put(path,'untracked\n');assert.throws(()=>captureSourceIdentity(f.root),{code});});
  run('G7-deleted',f=>{unlinkSync(join(f.root,'src/pilotdeck-gateway/Cargo.toml'));assert.throws(()=>captureSourceIdentity(f.root),{code:'SOURCE_DIRTY'});});
  run('G7-missing-required-clean',f=>{unlinkSync(join(f.root,'src/pilotdeck-gateway/Cargo.toml'));f.git('add','-u');f.commit();assert.throws(()=>captureSourceIdentity(f.root),{code:'SOURCE_REQUIRED'});});
  run('G7-empty-tree-clean',f=>{unlinkSync(join(f.root,'src/pilotdeck-protocol/src/entry.rs'));f.git('add','-u');f.commit();assert.throws(()=>captureSourceIdentity(f.root),{code:'SOURCE_TREE'});});
  run('G7-empty-before',f=>assert.throws(()=>verifySourceIdentity(f.root,{version:1,contract:producerInputs,files:[]}),{code:'SOURCE_BEFORE_INVALID'}));
  run('G7-trim-before',f=>{const before=captureSourceIdentity(f.root);assert.throws(()=>verifySourceIdentity(f.root,{...before,files:before.files.slice(1)}),{code:'SOURCE_IDENTITY_DRIFT'});});
  run('G7-extra-before',f=>{const before=captureSourceIdentity(f.root);assert.throws(()=>verifySourceIdentity(f.root,{...before,files:[...before.files,{path:'invented',sha256:'invented'}]}),{code:'SOURCE_IDENTITY_DRIFT'});});
  run('G8-source-after',f=>{const before=captureSourceIdentity(f.root);f.put('src/pilotdeck-gateway/src/upstream.rs','after\n');assert.throws(()=>verifySourceIdentity(f.root,before),{code:'SOURCE_DIRTY'});});
  run('G8-head-after',f=>{const before=captureSourceIdentity(f.root);f.put('docs/only.md','new head\n');f.git('add','docs');f.commit();assert.throws(()=>verifySourceIdentity(f.root,before),{code:'SOURCE_IDENTITY_DRIFT'});});
  run('G9-outside-scope',f=>{const before=captureSourceIdentity(f.root);f.put('state/example.md','state\n');f.put('docs/example.md','docs\n');assert.deepEqual(verifySourceIdentity(f.root,before),before);});
  for(const [id,option,tag]of [
    ['G12-skip-worktree','--skip-worktree','S'],
    ['G12-assume-unchanged','--assume-unchanged','h'],
  ])run(id,f=>{
    const path='src/pilotdeck-gateway/src/space\n name.rs';
    f.git('update-index',option,'--',path);f.put(path,'hidden changed\n');
    assert.equal(f.git('status','--porcelain=v1','-z','--',path).length,0,'actual Git hides the changed input');
    assert.equal(f.git('ls-files','-v','-z','--',path).toString(),`${tag} ${path}\0`);
    assert.notDeepEqual(readFileSync(join(f.root,path)),f.git('show',`HEAD:${path}`));
    assert.throws(()=>captureSourceIdentity(f.root),error=>error.code==='SOURCE_DIRTY'&&error.details.flag===tag&&error.details.path===path);
  });
  for(const [id,option]of [
    ['G12-outside-skip-worktree','--skip-worktree'],
    ['G12-outside-assume-unchanged','--assume-unchanged'],
  ])run(id,f=>{
    const path='docs/hidden-source-excluded.md';f.put(path,'outside original\n');f.git('add','--',path);f.commit();
    const before=captureSourceIdentity(f.root);f.git('update-index',option,'--',path);f.put(path,'outside changed\n');
    assert.deepEqual(verifySourceIdentity(f.root,before),before);
  });
  run('G12-missing-root',f=>{
    const holding=join(mkdtempSync(join(parent,'missing-root-held-')),'checkout');
    renameSync(f.root,holding);
    try{assert.throws(()=>captureSourceIdentity(f.root),{code:'SOURCE_ROOT'});}
    finally{renameSync(holding,f.root);}
    assert.doesNotThrow(()=>captureSourceIdentity(f.root));
  });
  run('G11-tracked-symlink',f=>{const path='src/pilotdeck-gateway/src/upstream.rs';unlinkSync(join(f.root,path));symlinkSync('main.rs',join(f.root,path));f.git('add','--',path);f.commit();assert.throws(()=>captureSourceIdentity(f.root),{code:'SOURCE_SYMLINK'});});
  run('G11-not-repository',f=>{mkdirSync(join(f.root,'empty'));assert.throws(()=>captureSourceIdentity(join(f.root,'empty')),{code:'SOURCE_ROOT'});});
  run('G11-git-command-error',f=>{f.put('.git/config','[invalid\n');assert.throws(()=>captureSourceIdentity(f.root),error=>Number.isInteger(error.status)&&error.status!==0);});
  run('G12-missing-tree-clean',f=>{
    const tree='src/pilotdeck-protocol/src';unlinkSync(join(f.root,tree,'entry.rs'));f.git('add','-u');f.commit();
    const holding=join(mkdtempSync(join(parent,'missing-tree-held-')),'source');renameSync(join(f.root,tree),holding);
    try{assert.throws(()=>captureSourceIdentity(f.root),{code:'SOURCE_TREE'});}
    finally{renameSync(holding,join(f.root,tree));}
  });
  run('G13-optional-config-directory',f=>{
    const path='.cargo/config.toml';mkdirSync(join(f.root,path),{recursive:true});
    assert.equal(f.git('status','--porcelain=v1','-z').length,0,'physical empty config directory stays Git-clean');
    assert.equal(f.git('ls-files','--others','-z','--',path).length,0);
    assert.throws(()=>captureSourceIdentity(f.root),error=>error.code==='SOURCE_KIND'&&error.details.path===path);
  });
  run('G13-contract-only-before',f=>{
    const before=captureSourceIdentity(f.root);
    const changed={...before,contract:{...before.contract,version:before.contract.version+1}};
    assert.deepEqual({...changed,contract:before.contract},before,'only contract changes');
    assert.throws(()=>verifySourceIdentity(f.root,changed),{code:'SOURCE_BEFORE_INVALID'});
  });
  run('G13-combined-index-flags',f=>{
    const path='src/pilotdeck-gateway/src/space\n name.rs';
    f.git('update-index','--skip-worktree','--',path);f.git('update-index','--assume-unchanged','--',path);
    f.put(path,'combined hidden changed\n');
    assert.equal(f.git('status','--porcelain=v1','-z','--',path).length,0,'actual Git hides combined flags');
    assert.equal(f.git('ls-files','-v','-z','--',path).toString(),`s ${path}\0`);
    assert.notDeepEqual(readFileSync(join(f.root,path)),f.git('show',`HEAD:${path}`));
    assert.throws(()=>captureSourceIdentity(f.root),error=>error.code==='SOURCE_DIRTY'&&error.details.flag==='s'&&error.details.path===path);
  });
  assert.equal(rows.length,35,'all registered source-identity rows completed');
  assert.equal(new Set(rows.map(row=>row.id)).size,35,'all source-identity row IDs unique');
  completed=true;
});
