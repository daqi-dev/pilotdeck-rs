// Actual fixed TS describeServer option matrix; never rewrites the existing oracle.
import assert from 'node:assert/strict';
import { mkdirSync,readFileSync,writeFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
const [baseArg,outArg,oldOracleArg]=process.argv.slice(2); assert(baseArg&&outArg);
const baseline=resolve(baseArg),out=resolve(outArg);mkdirSync(out,{recursive:false});
for(const name of ['HOME','PILOT_HOME','XDG_CONFIG_HOME','XDG_CACHE_HOME','XDG_DATA_HOME']){const path=out+'/'+name.toLowerCase();mkdirSync(path);process.env[name]=path;}
assert.equal(execFileSync('git',['-C',baseline,'rev-parse','HEAD'],{encoding:'utf8'}).trim(),'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85');
const { InProcessGateway }=await import(pathToFileURL(baseline+'/src/gateway/client/InProcessGateway.ts').href);
const { SessionRouter }=await import(pathToFileURL(baseline+'/src/gateway/SessionRouter.ts').href);
let invoked=0;const callback=async()=>{invoked++;throw Error('describeServer must not execute providers');};
const matrix=[['off',{}],['managerBrowsers',{managerBrowsers:callback}],['applyFlagSettings',{applyFlagSettings:callback}],['updateSettings',{updateSettings:callback}],['reloadOutputStyles',{reloadOutputStyles:callback}],['all',{managerBrowsers:callback,applyFlagSettings:callback,updateSettings:callback,reloadOutputStyles:callback}]] as const;
const rows:any[]=[];
for(const [name,options]of matrix){
 const router=new SessionRouter({idleSweepIntervalMs:0,createSession(){throw Error('describeServer must not create session');}});
 const gateway=new InProcessGateway(router,options as any); const description=await gateway.describeServer();
 assert.equal(invoked,0); rows.push({name,options:Object.keys(options),description});
}
const off=rows[0].description.capabilities;
for(const row of rows){row.added=row.description.capabilities.filter((name:string)=>!off.includes(name));row.removed=off.filter((name:string)=>!row.description.capabilities.includes(name));}
assert.deepEqual(rows.map(row=>row.added),[[],['manager_browsers'],['apply_flag_settings'],['update_settings'],['reload_output_styles'],['manager_browsers','apply_flag_settings','update_settings','reload_output_styles']]);
const oldOracle=resolve(oldOracleArg??new URL('./oracle.json',import.meta.url).pathname); const old=JSON.parse(readFileSync(oldOracle,'utf8'));
assert(old.contract.capabilities.includes('reload_output_styles'));
const oldOptionalPresence=Object.fromEntries(['manager_browsers','apply_flag_settings','update_settings','reload_output_styles'].map(name=>[name,old.contract.capabilities.includes(name)]));
const sources=Object.fromEntries(['src/gateway/client/InProcessGateway.ts','src/gateway/SessionRouter.ts'].map(path=>[path,createHash('sha256').update(readFileSync(baseline+'/'+path)).digest('hex')]));
writeFileSync(out+'/optional-capabilities.json',JSON.stringify({sourceCommit:'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85',node:{execPath:process.execPath,versions:process.versions},sources,generatorSha256:createHash('sha256').update(readFileSync(new URL(import.meta.url))).digest('hex'),oldOracleSha256:createHash('sha256').update(readFileSync(oldOracle)).digest('hex'),oldOptionalPresence,rows,providerInvocations:invoked},null,2)+'\n',{flag:'wx'});
