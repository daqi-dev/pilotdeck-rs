/** Load original fixed TS sources; only erase types, never patch their text. */
import assert from 'node:assert/strict';
import fs from 'node:fs';
import {registerHooks,stripTypeScriptTypes} from 'node:module';
import {Server} from 'node:net';
import {resolve,relative} from 'node:path';
import {pathToFileURL,fileURLToPath} from 'node:url';
import {createHash} from 'node:crypto';
assert.ok(process.env.A19_HOST_BASELINE);
const baseline=resolve(process.env.A19_HOST_BASELINE),prefix=pathToFileURL(baseline+'/').href;
Server.prototype.listen=function(){throw Error('formal controlled stdio sidecar forbids listen')};
registerHooks({
 resolve(specifier,context,next){
  if(context.parentURL?.startsWith(prefix)&&specifier.startsWith('.')&&specifier.endsWith('.js')){
   const candidate=new URL(specifier.slice(0,-3)+'.ts',context.parentURL);
   if(fs.existsSync(candidate))return{url:candidate.href,shortCircuit:true};
  }
  return next(specifier,context);
 },
 load(url,context,next){
  if(url.startsWith(prefix)&&url.endsWith('.ts')){
   const path=fileURLToPath(url),source=fs.readFileSync(path,'utf8');
   assert.ok(process.env.A52B_CHILD_LOADLOG,'child source witness path required');
   fs.appendFileSync(process.env.A52B_CHILD_LOADLOG,JSON.stringify({pid:process.pid,path:relative(baseline,path),sha256:createHash('sha256').update(source).digest('hex')})+'\n');
   return{format:'module',source:stripTypeScriptTypes(source,{mode:'transform',sourceUrl:url}),shortCircuit:true};
  }
  return next(url,context);
 }
});
