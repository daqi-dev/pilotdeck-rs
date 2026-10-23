// Compiler-derived recorder inputs. This is evidence binding, not build attestation.
import {execFileSync} from 'node:child_process';
import {lstatSync,readFileSync,realpathSync,readdirSync,writeFileSync} from 'node:fs';
import {createHash} from 'node:crypto';
import {resolve,join,dirname,relative,isAbsolute} from 'node:path';
import {fileURLToPath} from 'node:url';

const hash=bytes=>createHash('sha256').update(bytes).digest('hex');
const order=(a,b)=>Buffer.compare(Buffer.from(a),Buffer.from(b));
const sorted=values=>[...new Set(values)].sort(order);
const recorderFiles=['record.mjs','observe.mjs','check_suffix.mjs','source_identity.mjs'].map(p=>'src/pilotdeck-gateway/tests/upgrade/'+p).concat('scripts/gateway-parity/watcher.mjs');
function fail(code,message,details={}){throw Object.assign(Error(message),{code,details});}
function git(root,args){
  return execFileSync('/usr/bin/git',['--no-replace-objects','-c','core.hooksPath=/dev/null','-c','core.fsmonitor=false','-C',root,...args],{
    timeout:10000,maxBuffer:16*1024*1024,env:{PATH:'/usr/bin:/bin',GIT_CONFIG_NOSYSTEM:'1',GIT_CONFIG_GLOBAL:'/dev/null',GIT_OPTIONAL_LOCKS:'0',LANG:'C'},
  });
}
function paths(bytes){
  const values=[];let start=0;
  for(let i=0;i<bytes.length;i++)if(bytes[i]===0){const raw=bytes.subarray(start,i),text=raw.toString('utf8');
    if(!Buffer.from(text).equals(raw))fail('UNSUPPORTED_PATH_ENCODING','Git path cannot round-trip UTF-8');
    if(text)values.push(text);start=i+1;
  }
  if(start!==bytes.length)fail('INVALID_GIT_PATH_LIST','expected NUL-terminated Git path list');
  return values;
}
function checkout(root){
  try{root=realpathSync(resolve(root));}catch(error){if(error.code==='ENOENT')fail('SOURCE_ROOT','checkout does not exist');throw error;}
  if(realpathSync(git(root,['rev-parse','--show-toplevel']).toString().trim())!==root)fail('SOURCE_ROOT','expected Git checkout root');
  return root;
}
function inside(root,full){
  const path=relative(root,resolve(full));
  if(!path||path==='..'||path.startsWith('../')||isAbsolute(path))fail('SOURCE_OUTSIDE','input must be inside checkout',{full});
  return path;
}
function stat(root,path,missing=false){
  inside(root,join(root,path));let at=root;
  for(const piece of path.split('/')){at=join(at,piece);try{if(lstatSync(at).isSymbolicLink())fail('SOURCE_SYMLINK','symlink in source input',{path});}
    catch(error){if(error.code==='ENOENT'&&missing)return null;throw error;}}
  const value=lstatSync(at);if(!value.isFile()&&!value.isDirectory())fail('SOURCE_KIND','nonregular input',{path});return value;
}
function digest(root,path){if(!stat(root,path)?.isFile())fail('SOURCE_KIND','input must be regular file',{path});return hash(readFileSync(join(root,path)));}
function clean(root,scopes){
  const specs=scopes.map(p=>':(literal)'+p);
  for(const entry of paths(git(root,['ls-files','-v','-z','--',...specs]))){
    if(entry.length<3||entry[1]!==' ')fail('INVALID_GIT_PATH_LIST','expected index tag and NUL-delimited path');
    const flag=entry[0],path=entry.slice(2);
    if(flag==='S'||/^[a-z]$/.test(flag))fail('SOURCE_DIRTY','hidden index flag on source input',{flag,path});
  }
  const status=git(root,['status','--porcelain=v1','-z','--untracked-files=all','--',...specs]);
  if(status.length)fail('SOURCE_DIRTY','staged/unstaged/scoped untracked source changes',{statusHex:status.toString('hex')});
  const others=paths(git(root,['ls-files','--others','-z','--',...specs]));
  if(others.length)fail('SOURCE_UNTRACKED','untracked or ignored files in source scope',{paths:others});
}

// rustc escapes only spaces in filenames (escape_dep_filename in rustc_interface).
// In particular, #, $ and backslashes are literal, unlike a general Make parser.
// Do not interpret Rust tokens, macros or cfgs. Unsupported dep-info is an error.
export function parseDepInfo(text){
  if(text.includes('\0')||text.includes('\r')||text.includes('\t'))fail('DEP_INFO_PARSE','unsupported dep-info encoding');
  const rules=[];
  for(const line of text.split('\n')){
    if(!line.trim()||line.startsWith('#'))continue;
    const sides=[[]],word=[];let colon=false;
    const flush=()=>{if(word.length){sides.at(-1).push(word.join(''));word.length=0;}};
    for(let i=0;i<line.length;i++){
      const c=line[i];
      if(c==='\\'&&line[i+1]===' '){word.push(' ');i++;}
      else if(c===':'&&!colon&&(i+1===line.length||line[i+1]===' ')){flush();sides.push([]);colon=true;}
      else if(c===' ')flush();
      else word.push(c);
    }
    flush();
    if(!colon||!sides[0].length)fail('DEP_INFO_PARSE','expected rustc dependency rule',{line});
    rules.push({targets:sides[0],inputs:sides[1]});
  }
  if(!rules.some(r=>r.inputs.length))fail('DEP_INFO_PARSE','no dependency rule');
  return rules;
}
export function localPackages(metadata,packageName){
  if(!Array.isArray(metadata?.packages)||!Array.isArray(metadata?.resolve?.nodes))fail('CARGO_METADATA','resolved Cargo metadata required');
  const byId=new Map(metadata.packages.map(p=>[p.id,p])),nodes=new Map(metadata.resolve.nodes.map(n=>[n.id,n]));
  const roots=metadata.packages.filter(p=>p.name===packageName&&p.source===null);
  if(roots.length!==1)fail('CARGO_METADATA','expected one local producer package',{packageName});
  const seen=new Set(),pending=[roots[0].id],result=[];
  while(pending.length){const id=pending.pop();if(seen.has(id))continue;seen.add(id);
    const pkg=byId.get(id),node=nodes.get(id);if(!pkg||!node||!Array.isArray(node.dependencies))fail('CARGO_METADATA','incomplete resolve graph',{id});
    if(pkg.source===null)result.push(pkg);
    pending.push(...node.dependencies);
  }
  return result.sort((a,b)=>order(a.manifest_path,b.manifest_path));
}
function evidence(value){
  value??=process.env.PDRS_SOURCE_IDENTITY_BUILD;
  if(typeof value==='string')value=JSON.parse(readFileSync(value,'utf8'));
  if(value?.version!==2||!Number.isFinite(value.startedAt)||!Number.isFinite(value.finishedAt)||value.finishedAt<value.startedAt||!Array.isArray(value.messages))fail('BUILD_EVIDENCE','version 2 Cargo build evidence required');
  return value;
}
function units(root,build,checkBindings=true){
  if(build.root!==root||!isAbsolute(build.cwd)||build.cwd!==dirname(build.manifestPath))fail('BUILD_EVIDENCE','build checkout/cwd mismatch');
  inside(root,build.manifestPath);const targetPath=inside(root,build.targetDir);stat(root,targetPath);
  const packages=localPackages(build.metadata,build.packageName),byId=new Map(packages.map(p=>[p.id,p]));
  for(const pkg of packages)stat(root,inside(root,pkg.manifest_path));
  if(build.messages.filter(m=>m.reason==='build-finished').length!==1||!build.messages.at(-1)?.success||build.messages.at(-1)?.reason!=='build-finished')fail('BUILD_EVIDENCE','successful complete Cargo stream required');
  const artifacts=build.messages.filter(m=>m.reason==='compiler-artifact'&&byId.has(m.package_id));
  if(!artifacts.some(m=>m.package_id===packages.find(p=>p.name===build.packageName)?.id&&m.target.kind.includes('bin')&&m.executable===build.executable))fail('BUILD_EVIDENCE','producer executable missing from Cargo stream');
  const generatedDirs=build.messages.filter(m=>m.reason==='build-script-executed'&&byId.has(m.package_id)).map(m=>{
    const path=inside(root,m.out_dir);if(!path.startsWith(targetPath+'/'))fail('SOURCE_OUTSIDE','OUT_DIR must be in owned target');return path;
  });
  const sourceFiles=[],generated=[],result=[];
  for(const artifact of artifacts){
    const pkg=byId.get(artifact.package_id);
    if(artifact.fresh!==false)fail('DEP_INFO_STALE','local compiler-artifact reused; use a fresh owned build',{package:pkg.name});
    if(artifact.manifest_path!==pkg.manifest_path||!pkg.targets.some(t=>t.name===artifact.target.name&&t.src_path===artifact.target.src_path&&JSON.stringify(t.kind)===JSON.stringify(artifact.target.kind)))fail('BUILD_EVIDENCE','artifact not in metadata targets');
    const outputs=artifact.filenames.map(p=>inside(root,p));
    outputs.forEach(p=>{if(!p.startsWith(targetPath+'/'))fail('BUILD_EVIDENCE','compiler output outside owned target',{path:p});stat(root,p);});
    // Library filenames contain the rustc unit hash. Cargo's top-level bin
    // name does not; bind its deps output by inode or byte identity instead.
    const dirs=sorted(artifact.filenames.map(dirname).concat(artifact.target.kind.includes('bin')?[join(dirname(artifact.executable),'deps')]:[]));
    const matches=[];
    for(const dir of dirs)for(const entry of readdirSync(dir)){
      if(!entry.endsWith('.d'))continue;
      const path=inside(root,join(dir,entry));
      const stem=entry.slice(0,-2);
      if(!stem.startsWith(artifact.target.name.replaceAll('-','_')+'-')&&!artifact.target.kind.includes('custom-build'))continue;
      if(!stat(root,path).isFile())fail('SOURCE_KIND','dep-info must be a regular file',{path});
      const bytes=readFileSync(join(root,path)),text=bytes.toString('utf8');
      if(!Buffer.from(text).equals(bytes))fail('DEP_INFO_PARSE','dep-info cannot round-trip UTF-8',{path});
      const rules=parseDepInfo(text);
      if(!rules.some(r=>r.targets.some(p=>resolve(build.cwd,p)===join(root,path))))continue;
      const targets=rules.filter(r=>r.inputs.length).flatMap(r=>r.targets).map(p=>resolve(build.cwd,p));
      const aliases=artifact.target.kind.some(k=>k==='bin'||k==='custom-build')?outputs.map(p=>digest(root,p)):[];
      const matched=targets.some(p=>artifact.filenames.includes(p)||(aliases.length&&stat(root,inside(root,p),true)?.isFile()&&aliases.includes(digest(root,inside(root,p)))));
      if(matched)matches.push({path,rules});
    }
    if(matches.length!==1)fail('DEP_INFO_MISSING','expected exactly one dep-info matching compiler-artifact: '+pkg.name,{package:pkg.name,matches:matches.map(m=>m.path)});
    const {path,rules}=matches[0],mtime=stat(root,path).mtimeMs;
    if(mtime<build.startedAt||mtime>build.finishedAt)fail('DEP_INFO_STALE','dep-info outside this build interval',{path,mtime});
    const inputs=sorted(rules.filter(r=>r.inputs.length).flatMap(r=>r.inputs).map(p=>inside(root,resolve(build.cwd,p))));
    if(!inputs.includes(inside(root,artifact.target.src_path)))fail('DEP_INFO_STALE','dep-info lacks compiler target source',{path});
    for(const input of inputs){
      const value=stat(root,input);
      if(!value.isFile())fail('SOURCE_KIND','compiler input is not a file',{input});
      if(value.mtimeMs>mtime)fail('DEP_INFO_STALE','compiler input newer than dep-info',{input,path});
      if(input.startsWith(targetPath+'/')){
        if(!generatedDirs.some(d=>input.startsWith(d+'/')))fail('BUILD_EVIDENCE','unbound generated compiler input',{input});
        generated.push(input);
      }else sourceFiles.push(input);
    }
    result.push({packageId:pkg.id,target:artifact.target.name,depInfo:path,depInfoSHA256:digest(root,path),outputs:outputs.map(path=>({path,sha256:digest(root,path)})),inputs:inputs.map(path=>({path,sha256:digest(root,path)}))});
  }
  result.sort((a,b)=>order(a.depInfo,b.depInfo));
  if(checkBindings&&JSON.stringify(result)!==JSON.stringify(build.units))fail('BUILD_EVIDENCE_DRIFT','compiler inputs, dep-info or artifacts changed since build');
  return {packages,sourceFiles:sorted(sourceFiles),generated:sorted(generated),units:result};
}
export function producerInputs(root,buildValue){
  root=checkout(root);const build=evidence(buildValue),derived=units(root,build);
  const packageDirs=derived.packages.map(p=>dirname(inside(root,p.manifest_path)));
  const manifests=new Map(derived.packages.map(p=>[p.id,dirname(inside(root,p.manifest_path))]));
  const trees=sorted(build.messages.filter(m=>m.reason==='compiler-artifact'&&manifests.has(m.package_id)&&!m.target.kind.includes('custom-build')).map(m=>dirname(inside(root,m.target.src_path))).filter(d=>!packageDirs.includes(d)));
  const required=sorted([inside(root,build.manifestPath),inside(root,join(build.metadata.workspace_root,'Cargo.lock')),...derived.packages.map(p=>inside(root,p.manifest_path)),...derived.sourceFiles,...(build.recorder?['rust-toolchain.toml',...recorderFiles]:[])]);
  const optional=sorted([...packageDirs.map(p=>p+'/build.rs'),...sorted(['','src',...packageDirs]).flatMap(p=>['config','config.toml'].map(n=>(p?p+'/':'')+'.cargo/'+n))]);
  return {version:2,packages:packageDirs,trees,required,optional,generated:derived.generated,meaning:'Cargo metadata local dependency closure and rustc dep-info from this build; generated outputs hashed separately; build-script runtime reads and toolchain/environment provenance are caller-owned'};
}
export function captureSourceIdentity(root,buildValue){
  root=checkout(root);const build=evidence(buildValue),contract=producerInputs(root,build),scopes=sorted([...contract.trees,...contract.required,...contract.optional]);
  clean(root,scopes);const head=git(root,['rev-parse','HEAD']).toString().trim();
  if(head!==build.head)fail('BUILD_EVIDENCE_DRIFT','HEAD differs from build');
  const tracked=paths(git(root,['ls-files','-z','--',...scopes.map(p=>':(literal)'+p)])).sort(order);
  if(!tracked.length||new Set(tracked).size!==tracked.length)fail('SOURCE_FILE_SET','empty/duplicate source set');
  for(const required of contract.required)if(!tracked.includes(required))fail('SOURCE_REQUIRED','required source missing from tracked set',{path:required});
  const optional=contract.optional.map(path=>{const value=stat(root,path,true);if(value&&!value.isFile())fail('SOURCE_KIND','optional build/config input must be regular file',{path});return {path,present:value!==null};});
  const files=tracked.map(path=>({path,sha256:digest(root,path)}));
  const generated=contract.generated.map(path=>({path,sha256:digest(root,path)}));
  clean(root,scopes);if(git(root,['rev-parse','HEAD']).toString().trim()!==head)fail('SOURCE_HEAD_DRIFT','HEAD changed during identity snapshot');
  units(root,build); // Recheck generated inputs and artifacts after the snapshot.
  return {version:2,root,contract,head,clean:true,files,optional,generated,buildSHA256:hash(JSON.stringify(build)),artifactOrigin:'CALLER_BUILD_EVIDENCE_NOT_ATTESTATION'};
}
export function verifySourceIdentity(root,before,buildValue){
  if(before?.version!==2||before.contract?.version!==2||!Array.isArray(before.files)||!before.files.length)fail('SOURCE_BEFORE_INVALID','version 2 and nonempty before snapshot required');
  const after=captureSourceIdentity(root,buildValue);
  if(JSON.stringify(before.contract)!==JSON.stringify(after.contract))fail('SOURCE_BEFORE_INVALID','source contract changed');
  if(JSON.stringify(after)!==JSON.stringify(before))fail('SOURCE_IDENTITY_DRIFT','HEAD, root, input set, optional presence or content changed',{after});
  return after;
}
export function assertBuildBinary(root,binary,buildValue){
  const build=evidence(buildValue);units(checkout(root),build);
  if(resolve(binary)!==build.executable)fail('BUILD_EVIDENCE','recorder binary differs from Cargo executable');
}
// Retain raw metadata and JSON messages, with hashes captured immediately after
// the bounded build. No new target is chosen: caller supplies its owned target.
export function createBuildEvidence(root,{manifestPath=join(root,'src/Cargo.toml'),packageName='pilotdeck-gateway',binaryName=packageName,targetDir=process.env.CARGO_TARGET_DIR??join(root,'src/target'),recorder=true}={}){
  root=checkout(root);manifestPath=resolve(manifestPath);targetDir=resolve(targetDir);
  inside(root,targetDir);inside(root,manifestPath);
  const cwd=dirname(manifestPath),env={...process.env,CARGO_TARGET_DIR:targetDir};
  const run=args=>execFileSync('cargo',args,{cwd,env,timeout:20*60*1000,maxBuffer:64*1024*1024,encoding:'utf8',stdio:['ignore','pipe','inherit']});
  const metadata=JSON.parse(run(['metadata','--offline','--locked','--format-version','1','--manifest-path',manifestPath]));
  const head=git(root,['rev-parse','HEAD']).toString().trim(),startedAt=Date.now();
  const raw=run(['build','--offline','--locked','--manifest-path',manifestPath,'-p',packageName,'--bin',binaryName,'--message-format=json']);
  const finishedAt=Date.now(),messages=raw.trimEnd().split('\n').map(line=>JSON.parse(line));
  const executable=messages.find(m=>m.reason==='compiler-artifact'&&m.target.kind.includes('bin')&&m.target.name===binaryName)?.executable;
  const build={version:2,root,cwd,manifestPath,targetDir,packageName,executable,recorder,head,startedAt,finishedAt,metadata,messages};
  build.units=units(root,build,false).units;
  return build;
}
if(process.argv[1]&&resolve(process.argv[1])===fileURLToPath(import.meta.url)){
  if(process.argv.length!==4)throw Error('usage: node source_identity.mjs CHECKOUT NEW_BUILD_JSON (owned CARGO_TARGET_DIR required)');
  const build=createBuildEvidence(process.argv[2]);
  captureSourceIdentity(process.argv[2],build);
  writeFileSync(process.argv[3],JSON.stringify(build,null,2)+'\n',{flag:'wx'});
}
