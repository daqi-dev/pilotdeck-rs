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
const extract = (start, end) => {
  const first = source.indexOf(start), last = source.indexOf(end, first);
  assert(first >= 0 && last > first && source.indexOf(start, first + 1) < 0, 'unique observer source boundaries');
  return {bytes: source.slice(first, last), firstLine: source.slice(0, first).split('\n').length,
    lastLine: source.slice(0, last).split('\n').length - 1};
};
const real = extract('const real:any={};', 'const owner=');
const observer = extract('let observations:any[]=[];', 'function snapshot(');
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
  for (const which of process.argv[2] ? [process.argv[2]] : ['handle', 'pending-handle', 'delayed', 'delayed-error', 'normal']) {
    const result = spawnSync(process.execPath, ['--experimental-strip-types', unit, which, root],
      {encoding: 'utf8', timeout: 5000, maxBuffer: 1024 * 1024});
    console.log(JSON.stringify({case: which, status: result.status, signal: result.signal,
      error: result.error?.message, stdout: result.stdout, stderr: result.stderr}));
    if (result.status !== 0 || result.error || result.signal) failed = true;
  }
  console.log(JSON.stringify({recorder, sourceSha256: digest(source), node: process.version,
    extracted: [real, observer].map(({bytes, ...lines}) => ({...lines, sha256: digest(bytes)}))}));
} finally { fs.rmSync(root, {recursive: true, force: true}); }
if (failed) process.exitCode = 1;
