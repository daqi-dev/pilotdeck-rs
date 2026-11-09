// Fixed TS loader omissions versus the existing component route capability.
import assert from 'node:assert/strict';
import {execFileSync} from 'node:child_process';
import {createRequire} from 'node:module';
import {mkdir,writeFile} from 'node:fs/promises';
import {createHash,createHmac} from 'node:crypto';
import {join,resolve,posix} from 'node:path';
const [base,outArg]=process.argv.slice(2),out=resolve(outArg);await mkdir(out,{recursive:true});assert.equal(process.version,'v22.22.0');assert.equal(process.versions.icu,'78.2');
const fixed='cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85',require=createRequire(join(base,'package.json')),{transformSync}=createRequire(require.resolve('tsx'))('esbuild'),sources={},cache=new Map();
function load(path){if(cache.has(path))return cache.get(path).exports;const source=execFileSync('git',['-C',base,'show',fixed+':'+path],{encoding:'utf8'});sources[path]=createHash('sha256').update(source).digest('hex');const module={exports:{}};cache.set(path,module);new Function('require','module','exports',transformSync(source,{loader:'ts',format:'cjs',target:'es2022',supported:{'dynamic-import':false}}).code)(name=>{if(name.startsWith('node:'))return require(name);assert(name.startsWith('.'));return load(posix.normalize(posix.join(posix.dirname(path),name)).replace(/\.js$/,'.ts'));},module,module.exports);return module.exports;}
const {loadEnabledChannels}=load('src/adapters/channel/loadEnabledChannels.ts'),{WebhookChannel}=load('src/adapters/channel/webhook/WebhookChannel.ts'),cases=[];
for(const kind of ['production-loader','direct-constructor']){
const options={port:0,secret:'mock-signing-key',routes:{main:{deliver:'log'}}},logs=[],submits=[];
const channel=kind==='production-loader'?(await loadEnabledChannels({webhook:{enabled:true,extra:options}}))[0]:new WebhookChannel(options);
const handle=await channel.start({gateway:{submitTurn:async function*(v){submits.push(v);yield{type:'assistant_text_delta',text:'Webhook TS reply'};}},logger:{info:s=>logs.push(['info',s]),error:s=>logs.push(['error',s]),warn:s=>logs.push(['warn',s])}});
const port=channel.server.address().port,body=JSON.stringify({text:'Webhook TS route input'}),response=await fetch('http://127.0.0.1:'+port+'/webhooks/main',{method:'POST',headers:{'content-type':'application/json','x-delivery-id':'fixed-event','x-hub-signature-256':'sha256='+createHmac('sha256','mock-signing-key').update(body).digest('hex')},body}).then(async r=>({status:r.status,body:await r.text()}));
if(kind==='direct-constructor'){const deadline=Date.now()+3000;while(!logs.some(([,s])=>s.includes('Webhook TS reply'))&&Date.now()<deadline)await new Promise(r=>setTimeout(r,5));assert.equal(response.status,200);assert.equal(submits.length,1);}else{assert.equal(response.status,404);assert.equal(submits.length,0);}
await handle.stop('fixture');cases.push({kind,configuredRoutes:Object.keys(options.routes),constructedRoutes:Object.keys(channel.routes),response,submits,logs:logs.map(([level,s])=>[level,s.replace(/http:\/\/127\.0\.0\.1:\d+/g,'http://127.0.0.1:$PORT')])});
}
await writeFile(join(out,'oracle.json'),JSON.stringify({fixed,sourceSha256:sources,environment:process.versions,formalAcceptance:false,declaredMocks:['Gateway submitTurn only'],cases},null,2)+String.fromCharCode(10),{flag:'wx'});console.log(JSON.stringify({cases:cases.length}));
