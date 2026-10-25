// Real fixed TS; gate only the first native write between its two byte legs.
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { mkdir, mkdtemp, open, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { bindSourceTree, sourceClosureSha256 } from '../../baseline_source_guard.mjs';
const baseline = resolve(process.env.PDRS_BASELINE!);
const output = resolve(process.argv[2]);
assert.equal(process.version, 'v22.22.0');
const git = execFileSync('which', ['git'], {encoding:'utf8'}).trim();
const bound = bindSourceTree(baseline, 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85', git);
const sources = ['src/context/budget/ToolResultBudget.ts', 'src/context/budget/ToolResultSpillPort.ts'];
const { ToolResultBudget } = await import(pathToFileURL(join(baseline, sources[0])).href);
const { createNodeToolResultSpillPort } = await import(pathToFileURL(join(baseline, sources[1])).href);
const scratch = await mkdtemp(join(tmpdir(), 'pdrs-spill-isolation-'));
const rows = [];
const deferred = () => { let resolve; const promise = new Promise<void>(r => resolve = r); return {promise, resolve}; };
const bounded = async p => { let timer; try { return await Promise.race([p, new Promise((_, reject) => timer=setTimeout(()=>reject(Error('watchdog')),10000))]); } finally { clearTimeout(timer); } };
const message = (id, text) => ({role:'user',content:[{type:'tool_result',toolCallId:id,content:[{type:'text',text}]}]});
try {
  for (const relation of ['same-id','different-id','different-root'])
    for (const event of ['sequential','concurrent','never','cancelled'])
      for (const budgets of ['one','two']) {
        const name = `${relation}-${event}-${budgets}`;
        const root = join(scratch,name); const dir = join(root,'.pilotdeck/tool-results/a');
        const entered=deferred(), release=deferred(); let fired=false;
        const native=createNodeToolResultSpillPort();
        const port={...native, async writeTextIfAbsent(path, content) {
          if (event==='sequential' || fired) return native.writeTextIfAbsent(path,content);
          fired=true; await mkdir(dirname(path),{recursive:true});
          const file=await open(path,'wx',0o600);
          try { await file.writeFile(content.slice(0,4)); entered.resolve(); await release.promise; await file.writeFile(content.slice(4)); }
          finally { await file.close(); }
          return {created:true};
        }};
        const make = toolResultsDir => new ToolResultBudget({toolResultsDir,maxResultSizeChars:1,previewBytes:100,spillPort:port});
        const a=make(dir), b=budgets==='one'?a:make(relation==='different-root'?join(root,'.pilotdeck/tool-results/b'):dir);
        if (relation==='different-root' && budgets==='one') {
          assert.equal(a,b); assert.equal(a.toolResultsDir,dir);
          rows.push({name,na:'one budget has one immutable root'}); continue;
        }
        const leader=a.applyToMessage(message('c/1','XXXXXXXX'),{turnId:'t'});
        if (event==='sequential') await bounded(leader); else await bounded(entered.promise);
        // cancelled = caller stops observing A; TS promises expose no abort API.
        const result=await bounded(b.applyToMessage(message(relation==='different-id'?'other':'c/1','YYYYYYYY'),{turnId:'t'}));
        const ref=result.content[0];
        const normalize=s=>s.replace(root,'<root>');
        rows.push({name,path:normalize(ref.path),readFilePath:ref.readFilePath,preview:ref.preview,
          bytes:await readFile(ref.path,'utf8'),aliasBytes:await readFile(join(root,ref.readFilePath),'utf8')});
        release.resolve(); await bounded(leader);
      }
  await mkdir(output,{recursive:true});
  await writeFile(join(output,'oracle.json'),JSON.stringify({sourceCommit:bound.ref,
    sourceSha256:sourceClosureSha256(baseline,bound.ref,git,sources),node:process.version,rows})+'\n',{flag:'wx'});
  console.log(JSON.stringify(rows));
} finally { await rm(scratch,{recursive:true,force:true}); }
