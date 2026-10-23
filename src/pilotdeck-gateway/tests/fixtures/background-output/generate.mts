// Real fixed-baseline TS oracle. Writes only the supplied output directory and /tmp.
// PDRS_BASELINE=/abs/PilotDeck timeout 90 node --import /abs/PilotDeck/node_modules/tsx/dist/loader.mjs generate.mts /abs/output
import assert from 'node:assert/strict';
import {execFileSync} from 'node:child_process';
import {createHash} from 'node:crypto';
import {promises as fs, realpathSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join, resolve} from 'node:path';
import {pathToFileURL} from 'node:url';
import {bindSourceTree, sourceClosureSha256} from '../../baseline_source_guard.mjs';
assert(process.env.PDRS_BASELINE && process.argv[2]);
assert.equal(process.versions.node.split('.')[0], '22');
const baseline = resolve(process.env.PDRS_BASELINE!);
const commit = 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';
const git = realpathSync(execFileSync('which', ['git'], {encoding:'utf8'}).trim());
bindSourceTree(baseline, commit, git);
const entry = 'src/task/storage/TaskOutputStore.ts';
const {TaskOutputStore} = await import(pathToFileURL(join(baseline, entry)).href);
const sha = (bytes: Buffer | string) => createHash('sha256').update(bytes).digest('hex');
const append = (text: string, repeat: number) => ({op:'append', text, repeat});
const read = (offset = 0, cap?: number) => ({op:'read', offset, ...(cap === undefined ? {} : {cap})});
const flush = {op:'flush'};
const capture = {op:'disk'};
const close = {op:'close'};
const long = [append('a',999999),flush,capture,read(),append('b',1),flush,capture,read(),append('c',1),read(0,1000009),flush,capture,read(0,1000009),append('D',800000),append('中🙂',60000),flush,capture,read(999998,800020),read(1800000,420001),read(2200000,10),read(0,0),read(-9,3),close,append('ignored',1),capture,read(0,2000000)];
const specs = [
  {name:'configured',spill:true,steps:long},
  {name:'memory-only',spill:false,steps:long},
  {name:'unflushed-gap',spill:true,steps:[append('x',1000001),read(0,1000009),flush,read(0,1000009),capture]},
  {name:'blocked-directory-retry',spill:true,steps:[{op:'block'},append('A',1000000),flush,capture,append('B',1),flush,read(0,1000009),{op:'unblock'},append('tail',1),flush,capture,read(0,1000009)]},
  {name:'permission-retry',spill:true,steps:[{op:'deny'},append('P',1000001),flush,capture,read(0,1000009),{op:'allow'},append('ok',1),flush,capture,read(0,1000009)]},
  {name:'disk-full-retry',spill:true,steps:[{op:'full'},append('F',1000001),flush,capture,read(0,1000009),{op:'healthy'},append('ok',1),flush,capture,read(0,1000009)]},
  {name:'partial-file-join',spill:true,steps:[append('ab中🙂',150000),flush,{op:'truncate',size:350007},read(350000,40),capture,{op:'remove'},read(0,50)]},
  {name:'preexisting-append',spill:true,steps:[{op:'seed',text:'OLD'},append('new',333334),flush,capture,read(0,1000010),{op:'remove'},read(0,1000010)]},
  {name:'disk-ring-utf8-boundary',spill:true,steps:[append('🙂',250001),append('x',1),flush,{op:'truncate',size:5},read(4,8),read(0,2)]},
  {name:'disk-cap-and-closed-read',spill:true,steps:[append('中',333335),flush,read(0,2),close,read(0,20),read(999999,3)]},
  {name:'unflushed-utf8-ring-boundary',spill:true,steps:[append('🙂',250001),append('x',1),read(0,12),flush,read(0,12),close,read(0,12)]},
];
const original = fs.appendFile;
const cases = [];
for (const spec of specs) {
  const root = await fs.mkdtemp(join(tmpdir(),'pdrs-553-ts-'));
  const dir = join(root,'output'), file = join(dir,'task.log');
  const store = new TaskOutputStore({taskId:'task',...(spec.spill ? {diskSpillDir:dir} : {})});
  const steps = [];
  try {
    for (const step of spec.steps) {
      let result;
      switch (step.op) {
        case 'append': store.append(step.text.repeat(step.repeat)); break;
        case 'flush': await store.flush(); break;
        case 'close': await store.close(); break;
        case 'read': {
          const value = store.readSlice(step.offset,step.cap);
          const {content,...meta} = value;
          result = {...meta,bytes:Buffer.byteLength(content),sha256:sha(content)};
          break;
        }
        case 'disk': {
          try {const bytes=await fs.readFile(file);result={bytes:bytes.length,sha256:sha(bytes)};}
          catch (error:any) {assert(['ENOENT','ENOTDIR'].includes(error.code));result=null;}
          break;
        }
        case 'block': await fs.writeFile(dir,'block'); break;
        case 'unblock': await fs.unlink(dir); break;
        case 'deny': await fs.mkdir(dir); await fs.chmod(dir,0o500); break;
        case 'allow': await fs.chmod(dir,0o700); break;
        // Fault injection at the real Node fs boundary; no disk filling or spinning.
        case 'full': fs.appendFile = async () => {throw Object.assign(new Error('No space left on device'),{code:'ENOSPC'});}; break;
        case 'healthy': fs.appendFile=original; break;
        case 'seed': await fs.mkdir(dir); await fs.writeFile(file,step.text); break;
        case 'truncate': await fs.truncate(file,step.size); break;
        case 'remove': await fs.unlink(file); break;
        default: throw new Error('unknown operation');
      }
      steps.push({...step,...(result === undefined ? {} : {result})});
    }
    await store.close();
    cases.push({...spec,steps});
  } finally {fs.appendFile=original;await fs.chmod(dir,0o700).catch(()=>{});await fs.rm(root,{recursive:true,force:true});}
}
const output = {baseline:commit,sourceSha256:sourceClosureSha256(baseline,commit,git,[entry]),cases};
await fs.mkdir(resolve(process.argv[2]),{recursive:true});
await fs.writeFile(join(resolve(process.argv[2]),'oracle.json'),JSON.stringify(output,null,2)+'\n');
