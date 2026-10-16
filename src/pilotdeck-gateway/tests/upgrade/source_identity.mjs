// Fixed recorder source contract; not proof of an external binary's build origin.
import {execFileSync} from 'node:child_process';
import {lstatSync,readFileSync,realpathSync} from 'node:fs';
import {createHash} from 'node:crypto';
import {resolve,join,dirname} from 'node:path';

const packages=['src/pilotdeck-gateway','src/pilotdeck-protocol','src/pilotdeck-yaml','src/data/icu_collator_data'];
export const producerInputs=Object.freeze({
  version:1,
  trees:Object.freeze([...packages.slice(0,3).map(p=>p+'/src'),'src/data/icu_collator_data/data']),
  required:Object.freeze([...packages.map(p=>p+'/Cargo.toml'),'src/data/icu_collator_data/lib.rs','src/Cargo.toml','src/Cargo.lock','rust-toolchain.toml',
    ...['record.mjs','observe.mjs','check_suffix.mjs','source_identity.mjs'].map(p=>'src/pilotdeck-gateway/tests/upgrade/'+p),'scripts/gateway-parity/watcher.mjs']),
  optional:Object.freeze([...packages.map(p=>p+'/build.rs'),...['','src',...packages].flatMap(p=>['config','config.toml'].map(n=>(p?p+'/':'')+'.cargo/'+n))]),
  meaning:'fixed package/source and recorder inputs only; caller owns Cargo artifact/toolchain/environment provenance; update contract for new local dependencies',
});
const scopes=[...producerInputs.trees,...producerInputs.required,...producerInputs.optional];
const hash=bytes=>createHash('sha256').update(bytes).digest('hex');
const order=(a,b)=>Buffer.compare(Buffer.from(a),Buffer.from(b));
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
function stat(root,path,missing=false){
  const full=join(root,path);let at=root;
  for(const piece of path.split('/')){at=join(at,piece);try{if(lstatSync(at).isSymbolicLink())fail('SOURCE_SYMLINK','symlink in source input',{path});}
    catch(error){if(error.code==='ENOENT'&&missing)return null;throw error;}}
  const value=lstatSync(full);if(!value.isFile()&&!value.isDirectory())fail('SOURCE_KIND','nonregular source input',{path});return value;
}
function clean(root){
  const specs=scopes.map(p=>':(literal)'+p);
  const status=git(root,['status','--porcelain=v1','-z','--untracked-files=all','--',...specs]);
  if(status.length)fail('SOURCE_DIRTY','staged/unstaged/scoped untracked source changes',{statusHex:status.toString('hex')});
  const others=paths(git(root,['ls-files','--others','-z','--',...specs]));
  if(others.length)fail('SOURCE_UNTRACKED','untracked or ignored files in source scope',{paths:others});
}
export function captureSourceIdentity(root){
  root=realpathSync(resolve(root));
  if(realpathSync(git(root,['rev-parse','--show-toplevel']).toString().trim())!==root)fail('SOURCE_ROOT','expected Git checkout root');
  clean(root);const head=git(root,['rev-parse','HEAD']).toString().trim();
  const tracked=paths(git(root,['ls-files','-z','--',...scopes.map(p=>':(literal)'+p)])).sort(order);
  if(!tracked.length||new Set(tracked).size!==tracked.length)fail('SOURCE_FILE_SET','empty/duplicate source set');
  for(const required of producerInputs.required)if(!tracked.includes(required))fail('SOURCE_REQUIRED','required source missing from tracked set',{path:required});
  for(const tree of producerInputs.trees){if(!stat(root,tree).isDirectory()||!tracked.some(p=>p.startsWith(tree+'/')))fail('SOURCE_TREE','required source tree empty',{path:tree});}
  const optional=producerInputs.optional.map(path=>{const value=stat(root,path,true);if(value&&!value.isFile())fail('SOURCE_KIND','optional build/config input must be regular file',{path});return {path,present:value!==null};});
  const files=tracked.map(path=>{if(!stat(root,path).isFile())fail('SOURCE_KIND','tracked input is not regular file',{path});return {path,sha256:hash(readFileSync(join(root,path)))};});
  clean(root);if(git(root,['rev-parse','HEAD']).toString().trim()!==head)fail('SOURCE_HEAD_DRIFT','HEAD changed during identity snapshot');
  return {version:1,root,contract:producerInputs,head,clean:true,files,optional,artifactOrigin:'NOT_PROVED_BY_SOURCE_IDENTITY'};
}
export function verifySourceIdentity(root,before){
  if(before?.version!==1||!Array.isArray(before.files)||before.files.length===0||JSON.stringify(before.contract)!==JSON.stringify(producerInputs))fail('SOURCE_BEFORE_INVALID','version/contract/nonempty before snapshot required');
  const after=captureSourceIdentity(root);
  if(JSON.stringify(after)!==JSON.stringify(before))fail('SOURCE_IDENTITY_DRIFT','HEAD, root, input set, optional presence or content changed',{after});
  return after;
}
