// Execute the recorder's own observer bytes; no Gateway/baseline source is copied.
import fs from 'node:fs';
import {createHash} from 'node:crypto';
import {spawnSync} from 'node:child_process';
import {tmpdir} from 'node:os';
import {dirname, join} from 'node:path';
import {fileURLToPath} from 'node:url';
import assert from 'node:assert/strict';
const recorder = join(dirname(fileURLToPath(import.meta.url)), 'record_metadata_io.mts');
const source = fs.readFileSync(recorder, 'utf8');
const extract = (input, start, end) => {
  const first = input.indexOf(start), last = input.indexOf(end, first);
  assert(first >= 0 && last > first && input.indexOf(start, first + 1) < 0
    && input.indexOf(end) === last && input.indexOf(end, last + 1) < 0, 'unique observer source boundaries');
  return {bytes: input.slice(first, last), firstLine: input.slice(0, first).split('\n').length,
    lastLine: input.slice(0, last).split('\n').length - 1};
};
const markers = [['const real:any={};', 'const owner='], ['let observations:any[]=[];', 'function snapshot(']];
for (const [start, end] of markers) {
  for (const broken of [source.replace(start, ''), source + start, source.replace(end, ''), source + end]) {
    assert.throws(() => extract(broken, start, end), /unique observer source boundaries/);
  }
}
const real = extract(source, ...markers[0]);
const observer = extract(source, ...markers[1]);
const digest = text => createHash('sha256').update(text).digest('hex');
const base = process.env.PDRS_OBSERVER_TMP_ROOT || tmpdir();
fs.mkdirSync(base, {recursive: true});
const root = fs.mkdtempSync(join(base, 'pdrs-metadata-observer-'));
const unit = join(root, 'unit.mts');
const probe = `
import assert from 'node:assert/strict';
const root=process.argv[3],which=process.argv[2],file=join(root,'visible');
fs.writeFileSync(file,'native observer contents');
if(which==='handle') {
 install();const h=await fsp.open(file,'r');reset();observations=[];
 try { assert.equal((await h.stat()).isFile(),true);assert.equal(observations.length,0,'reset must invalidate retained FileHandle traces'); }
 finally {await h.close();reset();}
} else if(which==='pending-handle') {
 install();const h=await fsp.open(file,'r');
 try {const pending=h.stat();reset();observations=[];assert.equal((await pending).isFile(),true);assert.equal(observations.length,0,'old pending handle completion must be invalid');}
 finally {await h.close();reset();}
} else if(which==='delayed'||which==='delayed-error') {
 const native=real['async:readdir'];let release:any;const gate=new Promise<void>(done=>{release=done;});
 real['async:readdir']=async(...args:any[])=>{await gate;return native(...args);};
 try {
  install();const old=fsp.readdir(which==='delayed'?root:join(root,'missing'));reset();observations=[];install();await fsp.stat(file);
  release();if(which==='delayed')await old;else await assert.rejects(old,{code:'ENOENT'});reset();
  assert.deepEqual(observations.map(o=>[o.api,o.path]),[['fsp.stat',file]],'old completion must not enter the next generation');
 } finally {release();real['async:readdir']=native;reset();}
} else if(which==='sync-error') {
 const missing=join(root,'missing'),expected=[];install();
 try {
  for(const name of ['statSync','readdirSync','readFileSync']) {
   let native:any;try{real['sync:'+name](missing);}catch(e){native=e;}
   assert.equal(native.code,'ENOENT');assert.throws(()=>fs[name](missing),(e:any)=>e.code===native.code&&e.syscall===native.syscall&&e.path===native.path);
   expected.push({api:'fs.'+name,path:missing,ok:false,code:native.code});
  }
 } finally {reset();}
 assert.deepEqual(observations.map(o=>({api:o.api,path:o.path,ok:o.ok,code:o.code})),expected);
} else if(which==='handle-error') {
 install();const h=await fsp.open(file,'r');await h.close();observations=[];
 const native=await real['async:open'](file,'r');await native.close();const expected=[];
 try {
  for(const method of ['stat','read','readFile']) {
   const args=method==='read'?[Buffer.alloc(1),0,1,0]:[];let error:any;try{await native[method](...args);}catch(e){error=e;}
   assert.equal(error.code,'EBADF');await assert.rejects(h[method](...args),(e:any)=>e.code===error.code&&e.syscall===error.syscall);
   expected.push({api:'FileHandle.'+method,path:file,ok:false,code:error.code});
  }
 } finally {reset();}
 assert.deepEqual(observations.map(o=>({api:o.api,path:o.path,ok:o.ok,code:o.code})),expected);
} else if(which==='normal') {
 install();
 try {
  assert.equal(fs.statSync(file).isFile(),true);fs.readdirSync(root);assert.equal(fs.readFileSync(file,'utf8'),'native observer contents');assert.equal(fs.existsSync(file),true);
  await fsp.stat(file);await fsp.readdir(root);assert.equal(await fsp.readFile(file,'utf8'),'native observer contents');
  const h=await fsp.open(file,'r');try {await h.stat();await h.read(Buffer.alloc(6),0,6,0);await h.readFile();}finally{await h.close();}
 } finally {reset();}
 assert.deepEqual([...new Set(observations.map(o=>o.api))].sort(),['fs.statSync','fs.readdirSync','fs.readFileSync','fs.existsSync','fsp.stat','fsp.readdir','fsp.readFile','fsp.open','FileHandle.stat','FileHandle.read','FileHandle.readFile'].sort());
 assert(observations.every(o=>o.ok===true));
} else throw Error('unknown probe');
console.log(JSON.stringify({case:which,observations:observations.map(o=>({api:o.api,path:o.path,ok:o.ok}))}));
`;
fs.writeFileSync(unit, "import fs from 'node:fs';\nimport fsp from 'node:fs/promises';\nimport {syncBuiltinESMExports} from 'node:module';\nimport {join} from 'node:path';\n" + real.bytes + observer.bytes + probe);
let failed = false;
try {
  for (const which of process.argv[2] ? [process.argv[2]] : ['handle', 'pending-handle', 'delayed', 'delayed-error', 'sync-error', 'handle-error', 'normal']) {
    const result = spawnSync(process.execPath, ['--experimental-strip-types', unit, which, root],
      {encoding: 'utf8', timeout: 5000, maxBuffer: 1024 * 1024});
    console.log(JSON.stringify({case: which, status: result.status, signal: result.signal,
      error: result.error?.message, stdout: result.stdout, stderr: result.stderr}));
    if (result.status !== 0 || result.error || result.signal) failed = true;
  }
  console.log(JSON.stringify({recorder, sourceSha256: digest(source), unitSha256: digest(fs.readFileSync(unit, 'utf8')), node: process.version,
    extracted: [real, observer].map(({bytes, ...lines}) => ({...lines, sha256: digest(bytes)}))}));
} finally { fs.rmSync(root, {recursive: true, force: true}); }
if (failed) process.exitCode = 1;
