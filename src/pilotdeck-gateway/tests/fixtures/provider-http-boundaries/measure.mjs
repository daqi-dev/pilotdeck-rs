// Bounded reproduction of #411, reusing the original #396 TCP probe.
import assert from 'node:assert/strict';
import {spawnSync} from 'node:child_process';
import {mkdirSync,writeFileSync} from 'node:fs';
import {resolve,join} from 'node:path';
import {fileURLToPath} from 'node:url';
assert.equal(process.version,'v22.22.0','#411 calibrated Node');
const out=resolve(process.argv[2]),base=resolve(process.env.PDRS_BASELINE);
mkdirSync(out,{recursive:true});
const rows=[];
// 21 requests total, at most 30 s per process, no retry or CPU load process.
for(const gap of [0,100,300,500,700,900,1100])for(const run of [1,2,3]){
 const r=spawnSync(process.execPath,['--disable-warning=ExperimentalWarning','--import',join(base,'node_modules/tsx/dist/loader.mjs'),fileURLToPath(new URL('./probe.mts',import.meta.url)),'anthropic',String(5000+gap),'5000',String(run)],{encoding:'utf8',timeout:30000});
 writeFileSync(join(out,`gap-${gap}-${run}.jsonl`),r.stdout??'',{flag:'wx'});
 writeFileSync(join(out,`gap-${gap}-${run}.stderr`),r.stderr??'',{flag:'wx'});
 assert.equal(r.status,0,`#411 child failed: ${r.error??r.stderr}`);
 const events=r.stdout.trim().split('\n').map(s=>JSON.parse(s));
 const e=events.find(e=>e.kind==='stream_thrown'||e.event?.type==='error');
 assert.ok(e,'#411 terminal error required');
 rows.push({gap,run,error:(e.error??e.event.error).message,sinceFrameMs:e.sinceServerFrameMs});
 console.log(JSON.stringify(rows.at(-1)));
}
writeFileSync(join(out,'summary.json'),JSON.stringify(rows,null,2)+'\n',{flag:'wx'});
