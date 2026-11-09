import assert from 'node:assert/strict';
import {mkdir,writeFile,readFile} from 'node:fs/promises';
import {createHash} from 'node:crypto';
import {execFileSync} from 'node:child_process';
import {join,resolve} from 'node:path';
import {pathToFileURL} from 'node:url';
const baseline=resolve(process.env.PDRS_BASELINE!),out=resolve(process.argv[2]);
assert.equal(process.version,'v22.22.0');assert.equal(process.versions.icu,'78.2');
const sha=execFileSync('git',['rev-parse','HEAD'],{cwd:baseline,encoding:'utf8'}).trim();assert.equal(sha,'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85');
await mkdir(out,{recursive:false});
const source=join(baseline,'src/pilot/config/loadPilotConfig.ts');const {loadPilotConfig}=await import(pathToFileURL(source).href);
const names=['constructor','toString','valueOf','hasOwnProperty','__defineGetter__','__defineSetter__','__lookupGetter__','__lookupSetter__','isPrototypeOf','propertyIsEnumerable','toLocaleString'];
function descriptors(){return JSON.stringify([Object.getOwnPropertyDescriptors(Object.prototype),...names.map(n=>Object.getOwnPropertyDescriptors((Object.prototype as any)[n]))]);}
const initial=descriptors(),cases=[];
for(const name of names)for(const [kind,value] of [['number',7],['null',null],['object',{x:1}],['array',[1,{y:'ok'}]]] as const)for(const location of ['global','project','project-entry','merge-entry']){
 const id=`${name}-${kind}-${location}`,home=join(out,id,'home'),project=join(out,id,'project');await mkdir(home,{recursive:true});await mkdir(join(project,'.pilotdeck'),{recursive:true});
 const special=Object.fromEntries([[name,value]]),common={schemaVersion:1,agent:{model:'fixture/mock'},model:{providers:{fixture:{protocol:'openai',url:'http://127.0.0.1:1/v1',apiKey:'test',models:{mock:{}}}}}};
 const entry={projects:{'/fixture-project':{enabled:true,...special}}};
 const global={...common,alwaysOn:location==='global'?special:location==='merge-entry'?entry:{language:'en'}};
 const projectConfig=location==='global'?null:{alwaysOn:location==='project'?special:location==='project-entry'?entry:{projects:{'/fixture-project':{enabled:false}}}};
 await writeFile(join(home,'pilotdeck.yaml'),JSON.stringify(global));if(projectConfig)await writeFile(join(project,'.pilotdeck/pilotdeck.yaml'),JSON.stringify(projectConfig));
 const results=[];for(let round=0;round<2;round++){
  const snapshot=loadPilotConfig({env:{HOME:home,PILOT_HOME:home},configPath:join(home,'pilotdeck.yaml'),projectRoot:project,version:1});results.push({config:snapshot.config,diagnostics:snapshot.diagnostics});
 }
 assert.deepEqual(JSON.parse(JSON.stringify(results[0])),JSON.parse(JSON.stringify(results[1])),id+' repeat changed output');assert.equal(initial,descriptors(),id+' changed shared intrinsics');
 cases.push({id,home,projectRoot:project,global,project:projectConfig,result:results[0],repeatedEqual:true,intrinsicsUnchanged:true});
}
await writeFile(join(out,'oracle.json'),JSON.stringify({baseline:sha,node:process.version,icu:process.versions.icu,sourceSha256:createHash('sha256').update(await readFile(source)).digest('hex'),cases},null,2)+'\n');
console.log(JSON.stringify({cases:cases.length,repeated:true,intrinsicsUnchanged:true}));
