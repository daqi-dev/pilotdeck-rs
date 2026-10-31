// Native Git + real rustc fixtures; no Rust-source scanner or handwritten dep-info oracle.
import test from 'node:test';
import assert from 'node:assert/strict';
import {mkdtempSync,mkdirSync,writeFileSync,readFileSync,unlinkSync,symlinkSync,rmSync,realpathSync,utimesSync,existsSync} from 'node:fs';
import {join,dirname} from 'node:path';
import childProcess,{execFileSync,spawnSync} from 'node:child_process';
import {syncBuiltinESMExports} from 'node:module';
import {fileURLToPath} from 'node:url';
import {createHash} from 'node:crypto';
import {producerInputs,captureSourceIdentity,verifySourceIdentity,createBuildEvidence,assertBuildBinary,parseDepInfo,localPackages} from './source_identity.mjs';

test('#1166 build evidence target precedence keeps default outside src',t=>{
  const root=realpathSync(fileURLToPath(new URL('../../../../',import.meta.url)));
  const originalExec=execFileSync,originalTarget=process.env.CARGO_TARGET_DIR;
  const stop=Error('#1166 stopped before Cargo'),observed=[];
  t.mock.method(childProcess,'execFileSync',(command,args,options)=>{
    if(command!=='cargo')return originalExec(command,args,options);
    observed.push(options.env.CARGO_TARGET_DIR);throw stop;
  });
  syncBuiltinESMExports();
  t.after(()=>{
    t.mock.restoreAll();syncBuiltinESMExports();
    if(originalTarget===undefined)delete process.env.CARGO_TARGET_DIR;
    else process.env.CARGO_TARGET_DIR=originalTarget;
  });
  for(const [envTarget,options,expected] of [
    [undefined,{},join(root,'_build/cargo-target')],
    [join(root,'_build/env-target'),{},join(root,'_build/env-target')],
    [join(root,'_build/env-target'),{targetDir:join(root,'_build/option-target')},join(root,'_build/option-target')],
  ]){
    if(envTarget===undefined)delete process.env.CARGO_TARGET_DIR;
    else process.env.CARGO_TARGET_DIR=envTarget;
    assert.throws(()=>createBuildEvidence(root,options),error=>error===stop,'#1166 build evidence reaches Cargo boundary');
    assert.equal(observed.at(-1),expected,'#1166 build evidence uses the selected target');
  }
  assert.equal(observed.length,3,'#1166 build evidence checks every target mode');
});

function fixture(t,{outside=false}={}){
  const root=realpathSync(mkdtempSync(join('/tmp','pdrs-source-identity-')));
  t.after(()=>{rmSync(root,{recursive:true,force:true});console.log('REMOVED '+root);});
  const put=(path,value)=>{mkdirSync(dirname(join(root,path)),{recursive:true});writeFileSync(join(root,path),value);};
  const git=(...args)=>execFileSync('/usr/bin/git',['-c','core.hooksPath=/dev/null','-C',root,...args],{timeout:10000,maxBuffer:1024*1024,env:{PATH:'/usr/bin:/bin',HOME:root,GIT_CONFIG_GLOBAL:'/dev/null',GIT_CONFIG_NOSYSTEM:'1',LANG:'C'}});
  git('init','-q');
  put('.gitignore','/target/\n**/ignored.rs\n.cargo/config.toml\n');
  put('Cargo.toml','[workspace]\nresolver="3"\nmembers=["app", "tokenizer", "transport", "agent", "vendor"]\n');
  const crates=[['app','producer'],['tokenizer','tokenizer'],['transport','transport'],['agent','agent'],['vendor','vocab']];
  for(const [dir,name]of crates){
    const deps=dir==='app'?'tokenizer={path="../tokenizer"}\ntransport={path="../transport"}':dir==='transport'?'agent={path="../agent"}':dir==='tokenizer'?'vocab={path="../vendor"}':'';
    put(dir+'/Cargo.toml',`[package]\nname="${name}"\nversion="0.1.0"\nedition="2024"\n[dependencies]\n${deps}\n`);
    if(dir!=='app')put(dir+'/src/lib.rs',`pub fn value()->usize{1${dir==='tokenizer'?'+vocab::value()':dir==='transport'?'+agent::value()':''}}\n`);
  }
  put('app/src/main.rs',`use std::include_str as inc;
#[path="../modules/external.rs"] mod external;
const COMMENT:&str=include_str /* gap */ ! ["../assets/comment.txt"];
const BRACE:&[u8]=include_bytes!{"../assets/brace.bin"};
const WEIRD:&str=include_str!("../assets/space #$.txt");
const ALIAS:&str=inc!("../assets/alias.txt");
const TEST_ASSET:&str=include_str!("../tests/data.txt");
const GENERATED:&str=include_str!(concat!(env!("OUT_DIR"),"/generated.txt"));
include!("../modules/included.rs");
fn main(){let _=WEIRD;println!("{}{}{}{}{}{}{}{}{}",COMMENT,BRACE.len(),ALIAS,TEST_ASSET,GENERATED,external::VALUE,NESTED,tokenizer::value(),transport::value());}
`);
  for(const name of ['comment.txt','brace.bin','alias.txt','nested.txt','path.txt'])put('app/assets/'+name,'asset '+name+'\n');
  put('app/assets/space #$.txt','escaped-path asset\n');
  put('app/tests/not_built.rs','#[test] fn test_only(){}\n');
  put('app/tests/data.txt','production includes a tests asset\n');
  put('app/modules/external.rs','pub const VALUE:&str=include_str!("../assets/path.txt");\n');
  put('app/modules/included.rs','const NESTED:&str=include_str!("../assets/nested.txt");\n');
  put('vendor/assets/vocabulary.txt','vocabulary\n');
  put('vendor/src/lib.rs','pub fn value()->usize{include_str!("../assets/vocabulary.txt").len()}\n');
  put('app/build.rs','fn main(){std::fs::write(std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap()).join("generated.txt"), "generated content").unwrap();}\n');
  const cargo=(...args)=>execFileSync('cargo',args,{cwd:root,env:{...process.env,CARGO_TARGET_DIR:join(root,'target')},timeout:90000,maxBuffer:4*1024*1024,encoding:'utf8'});
  cargo('generate-lockfile','--offline');
  const commit=()=>{git('add','-f','--','.gitignore','Cargo.toml','Cargo.lock','app','tokenizer','transport','agent','vendor');git('-c','user.name=Fixture','-c','user.email=fixture@example.invalid','commit','-qm','fixture');};
  let extra;
  if(outside){
    extra=realpathSync(mkdtempSync(join('/tmp','pdrs-outside-input-')));t.after(()=>rmSync(extra,{recursive:true,force:true}));
    writeFileSync(join(extra,'outside.txt'),'tree-external input');
    put('vendor/src/lib.rs',`pub fn value()->usize{include_str!(${JSON.stringify(join(extra,'outside.txt'))}).len()}\n`);
  }
  commit();
  const build=()=>createBuildEvidence(root,{manifestPath:join(root,'Cargo.toml'),packageName:'producer',targetDir:join(root,'target'),recorder:false});
  return {root,put,git,commit,build,cargo,extra};
}
const sha=value=>createHash('sha256').update(value).digest('hex');
const capture=f=>captureSourceIdentity(f.root,f.evidence);
const verify=(f,before)=>verifySourceIdentity(f.root,before,f.evidence);
const ready=t=>{const f=fixture(t);f.evidence=f.build();return f;};

// Every case uses a compiler build, including the historical gaps that a regex missed.
test('dep-info covers comment gaps, all delimiters, alias, path module, nested include and tests asset',t=>{
  const f=ready(t),contract=producerInputs(f.root,f.evidence),before=capture(f);
  assert.deepEqual(contract.packages,['agent','app','tokenizer','transport','vendor']);
  for(const path of ['app/assets/space #$.txt','app/assets/comment.txt','app/assets/brace.bin','app/assets/alias.txt','app/assets/nested.txt','app/assets/path.txt','app/modules/external.rs','app/modules/included.rs','app/tests/data.txt','vendor/assets/vocabulary.txt','app/build.rs']){
    assert.ok(contract.required.includes(path),path);
    assert.equal(before.files.find(x=>x.path===path)?.sha256,sha(readFileSync(join(f.root,path))),path);
  }
  assert.ok(!contract.trees.includes('app/tests'),'unbuilt integration-test targets do not expand the compiler source scope');
  assert.ok(!before.files.some(x=>x.path==='app/tests/not_built.rs'));
  assert.ok(before.generated.some(x=>x.path.endsWith('/generated.txt')&&x.sha256===sha('generated content')));
  assert.deepEqual(verify(f,before),before);
  assertBuildBinary(f.root,f.evidence.executable,f.evidence);
  assert.throws(()=>assertBuildBinary(f.root,join(f.root,'different-binary'),f.evidence),{code:'BUILD_EVIDENCE'});
});

test('G8 asset hash is independently checked and verify detects hash drift with identical HEAD',t=>{
  const f=ready(t),before=capture(f),path='vendor/assets/vocabulary.txt';
  const entry=before.files.find(x=>x.path===path);
  assert.equal(entry.sha256,sha('vocabulary\n'));
  const forged=structuredClone(before);forged.files.find(x=>x.path===path).sha256=sha('changed vocabulary\n');
  assert.equal(forged.head,before.head);
  assert.throws(()=>verify(f,forged),{code:'SOURCE_IDENTITY_DRIFT'});
  // Change real bytes, keep HEAD and mtime fixed. The build receipt input hash
  // must reject this even when a timestamp check alone could not detect it.
  const full=join(f.root,path),stamp=f.evidence.startedAt-1000;
  f.put(path,'changed vocabulary\n');utimesSync(full,stamp/1000,stamp/1000);
  assert.notEqual(sha(readFileSync(full)),entry.sha256);
  assert.throws(()=>capture(f),{code:'BUILD_EVIDENCE_DRIFT'});
});

test('build.rs output is bound and cannot drift at constant timestamps',t=>{
  const f=ready(t),before=capture(f),path=before.generated.find(x=>x.path.endsWith('/generated.txt')).path;
  f.put(path,'modified generation');const stamp=f.evidence.startedAt-1000;utimesSync(join(f.root,path),stamp/1000,stamp/1000);
  assert.throws(()=>verify(f,before),{code:'BUILD_EVIDENCE_DRIFT'});
});

test('metadata follows transitive path dependencies, including paths reached through registry nodes',()=>{
  const p=(id,source)=>({id,name:id,source,manifest_path:'/repo/'+id+'/Cargo.toml'});
  const metadata={packages:[p('producer',null),p('registry','registry+https://example.invalid'),p('patched',null)],resolve:{nodes:[{id:'producer',dependencies:['registry']},{id:'registry',dependencies:['patched']},{id:'patched',dependencies:[]}]}};
  assert.deepEqual(localPackages(metadata,'producer').map(p=>p.name),['patched','producer']);
  metadata.resolve.nodes.pop();assert.throws(()=>localPackages(metadata,'producer'),{code:'CARGO_METADATA'});
});

test('dep-info parse errors propagate through capture (never silently omit a unit)',t=>{
  const f=ready(t),unit=f.evidence.units.find(u=>u.target==='vocab');
  f.put(unit.depInfo,'this is not a Make rule\n');
  assert.throws(()=>capture(f),{code:'DEP_INFO_PARSE'});
});

test('dep-info missing, old timestamp and mismatched compiler-artifact all fail closed',t=>{
  const f=ready(t),unit=f.evidence.units.find(u=>u.target==='vocab'),path=join(f.root,unit.depInfo),original=readFileSync(path);
  unlinkSync(path);assert.throws(()=>capture(f),{code:'DEP_INFO_MISSING'});
  f.put(unit.depInfo,original);utimesSync(path,(f.evidence.startedAt-1)/1000,(f.evidence.startedAt-1)/1000);assert.throws(()=>capture(f),{code:'DEP_INFO_STALE'});
  f.put(unit.depInfo,original.toString().replaceAll('libvocab-','libunrelated-'));
  assert.throws(()=>capture(f),{code:'DEP_INFO_MISSING'});
});

test('cached Cargo artifacts are rejected as stale build evidence',t=>{
  const f=ready(t);assert.throws(()=>f.build(),{code:'DEP_INFO_STALE'});
});

test('real compiler include outside checkout fails closed',t=>{
  const f=fixture(t,{outside:true});assert.throws(()=>f.build(),{code:'SOURCE_OUTSIDE'});
});

test('successful complete Cargo stream and artifact-to-metadata binding are required',t=>{
  const f=ready(t),original=structuredClone(f.evidence);
  f.evidence.messages.pop();assert.throws(()=>capture(f),{code:'BUILD_EVIDENCE'});
  f.evidence=structuredClone(original);f.evidence.messages.at(-1).success=false;assert.throws(()=>capture(f),{code:'BUILD_EVIDENCE'});
  f.evidence=structuredClone(original);f.evidence.messages.find(m=>m.reason==='compiler-artifact').fresh=true;assert.throws(()=>capture(f),{code:'DEP_INFO_STALE'});
  f.evidence=structuredClone(original);f.evidence.messages.find(m=>m.reason==='compiler-artifact').manifest_path='/incorrect';assert.throws(()=>capture(f),{code:'BUILD_EVIDENCE'});
  assert.throws(()=>captureSourceIdentity(f.root,{}),{code:'BUILD_EVIDENCE'});
});

test('native Git cleanliness, hidden flags, config shape, contract and before validation',t=>{
  const f=ready(t),before=capture(f);
  const path='app/src/main.rs',original=readFileSync(join(f.root,path));
  // Dirty tracked files not used by rustc still belong to the source tree scope.
  f.put('app/src/unused.rs','// unused source\n');assert.throws(()=>capture(f),{code:'SOURCE_DIRTY'});unlinkSync(join(f.root,'app/src/unused.rs'));
  f.put('app/src/ignored.rs','// ignored source\n');assert.throws(()=>capture(f),{code:'SOURCE_UNTRACKED'});unlinkSync(join(f.root,'app/src/ignored.rs'));
  for(const option of ['--skip-worktree','--assume-unchanged']){
    f.git('update-index',option,'--',path);
    assert.throws(()=>capture(f),error=>error.code==='SOURCE_DIRTY'&&error.details.path===path);
    f.git('update-index',option.replace('--','--no-'),'--',path);
  }
  f.git('update-index','--skip-worktree','--',path);f.git('update-index','--assume-unchanged','--',path);
  assert.throws(()=>capture(f),error=>error.code==='SOURCE_DIRTY'&&error.details.flag==='s');
  f.git('update-index','--no-skip-worktree','--',path);f.git('update-index','--no-assume-unchanged','--',path);
  // Stage an edit, restore bytes and timestamps: avoid build-drift short circuit.
  f.put(path,'// staged\n');f.git('add','--',path);f.put(path,original);const stamp=f.evidence.startedAt-1000;utimesSync(join(f.root,path),stamp/1000,stamp/1000);
  assert.throws(()=>capture(f),{code:'SOURCE_DIRTY'});f.git('reset','-q','HEAD','--',path);
  mkdirSync(join(f.root,'.cargo/config.toml'),{recursive:true});assert.throws(()=>capture(f),{code:'SOURCE_KIND'});rmSync(join(f.root,'.cargo'),{recursive:true});
  for(const files of [[],before.files.slice(1),[...before.files,{path:'invented',sha256:'invented'}]])assert.throws(()=>verify(f,{...before,files}),{code:files.length?'SOURCE_IDENTITY_DRIFT':'SOURCE_BEFORE_INVALID'});
  assert.throws(()=>verify(f,{...before,contract:{...before.contract,version:999}}),{code:'SOURCE_BEFORE_INVALID'});
  f.put('docs/unrelated.md','docs\n');f.put('state/unrelated.md','state\n');assert.deepEqual(verify(f,before),before);
  unlinkSync(join(f.root,path));symlinkSync('different.rs',join(f.root,path));assert.throws(()=>capture(f),{code:'SOURCE_SYMLINK'});
});

test('Git and metadata command/JSON failures propagate; non-checkout roots fail',t=>{
  const f=ready(t);
  mkdirSync(join(f.root,'empty'));assert.throws(()=>captureSourceIdentity(join(f.root,'empty'),f.evidence),{code:'SOURCE_ROOT'});
  assert.throws(()=>captureSourceIdentity(join(f.root,'missing'),f.evidence),{code:'SOURCE_ROOT'});
  f.put('bad-evidence.json','{invalid');assert.throws(()=>captureSourceIdentity(f.root,join(f.root,'bad-evidence.json')),SyntaxError);
  const saved=readFileSync(join(f.root,'.git/config'));f.put('.git/config','[invalid\n');
  assert.throws(()=>capture(f),error=>Number.isInteger(error.status)&&error.status!==0);f.put('.git/config',saved);
  const metadata=structuredClone(f.evidence);metadata.metadata.resolve=null;assert.throws(()=>captureSourceIdentity(f.root,metadata),{code:'CARGO_METADATA'});
});

test('dep-info tokenizer matches rustc filename escaping and rejects malformed rules',()=>{
  assert.deepEqual(parseDepInfo(String.raw`out.d: path\ name hash#name dollar$name back\slash colon:name
path\ name:
# env-dep:VALUE=x
`)[0],{targets:['out.d'],inputs:['path name','hash#name','dollar$name','back\\slash','colon:name']});
  for(const input of ['garbage\n','only:\n','out: a\0b','out: a\tb'])assert.throws(()=>parseDepInfo(input),{code:'DEP_INFO_PARSE'});
});

test('invalid include arguments and broken Cargo manifests propagate actual compiler/metadata errors',t=>{
  const f=fixture(t);
  f.put('app/src/main.rs','fn main(){let _=include_str![unknown!()];}');f.commit();
  assert.throws(()=>f.build(),error=>error.status!==0&&Number.isInteger(error.status));
  f.put('Cargo.toml','[not valid toml');
  assert.throws(()=>f.build(),error=>error.status!==0&&Number.isInteger(error.status));
});

test('recorder rejects missing or foreign build evidence before creating output or reading baseline',t=>{
  const f=ready(t),record=new URL('./record.mjs',import.meta.url),output=join(f.root,'record-output');
  const env={...process.env};delete env.PDRS_SOURCE_IDENTITY_BUILD;
  let p=spawnSync(process.execPath,[record.pathname,'/missing-baseline','/missing-binary',output],{env,timeout:10000,encoding:'utf8'});
  assert.equal(p.status,1);assert.match(p.stderr,/PDRS_SOURCE_IDENTITY_BUILD is required/);assert.equal(existsSync(output),false);
  const path=join(f.root,'build-evidence.json');writeFileSync(path,JSON.stringify(f.evidence));
  p=spawnSync(process.execPath,[record.pathname,'/missing-baseline','/missing-binary',output],{env:{...env,PDRS_SOURCE_IDENTITY_BUILD:path},timeout:10000,encoding:'utf8'});
  assert.equal(p.status,1);assert.match(p.stderr,/build checkout\/cwd mismatch/);assert.equal(existsSync(output),false);
});

test('dep-info symlinks and non-UTF8 files fail closed before parsing inputs',t=>{
  const f=ready(t),unit=f.evidence.units.find(u=>u.target==='vocab'),path=join(f.root,unit.depInfo);
  unlinkSync(path);symlinkSync('missing-target',path);assert.throws(()=>capture(f),{code:'SOURCE_SYMLINK'});
  unlinkSync(path);f.put(unit.depInfo,Buffer.from([0xff,0xfe]));assert.throws(()=>capture(f),{code:'DEP_INFO_PARSE'});
});
