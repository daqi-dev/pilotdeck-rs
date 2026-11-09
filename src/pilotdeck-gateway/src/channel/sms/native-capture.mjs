// Fixed TS SMS optional SDK/credentials/HTTP/send contract; external Twilio is explicit mock.
import assert from 'node:assert/strict';
import {execFileSync} from 'node:child_process';
import {createRequire} from 'node:module';
import {mkdir,writeFile} from 'node:fs/promises';
import {createHash} from 'node:crypto';
import {join,resolve,posix} from 'node:path';
const [base,outArg]=process.argv.slice(2),out=resolve(outArg);await mkdir(out,{recursive:true});
assert.equal(process.version,'v22.22.0');assert.equal(process.versions.icu,'78.2');
const fixed='cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85',require=createRequire(join(base,'package.json')),{transformSync}=createRequire(require.resolve('tsx'))('esbuild');
const sources={},cases=[];
for(const kind of ['sdk-absent','config-missing','configured','factory-error']){
const cache=new Map(),sdk=[],submits=[],logs=[],sent=[];
function load(path){if(cache.has(path))return cache.get(path).exports;const source=execFileSync('git',['-C',base,'show',fixed+':'+path],{encoding:'utf8'});sources[path]=createHash('sha256').update(source).digest('hex');const module={exports:{}};cache.set(path,module);
new Function('require','module','exports',transformSync(source,{loader:'ts',format:'cjs',target:'es2022'}).code)(name=>{if(name==='twilio'){if(kind==='sdk-absent')throw Error('mock optional module absent');return(sid,token)=>{sdk.push({sid,token});if(kind==='factory-error')throw Error('mock SDK constructor failed');return{messages:{create:async value=>{sent.push(value);return{sid:'SMmock'};}}};};}if(name.startsWith('node:'))return require(name);assert(name.startsWith('.'));return load(posix.normalize(posix.join(posix.dirname(path),name)).replace(/\.js$/,'.ts'));},module,module.exports);return module.exports;}
const {SmsChannel}=load('src/adapters/channel/sms/SmsChannel.ts'),configured=kind!=='config-missing';
const c=new SmsChannel({extra:{accountSid:configured?'AC00000000000000000000000000000000':'',authToken:configured?'mock-token':'',phoneNumber:configured?'+15550000001':'',webhookHost:'127.0.0.1',webhookPort:0}});
const handle=await c.start({gateway:{submitTurn:async function*(value){submits.push(value);yield{type:'assistant_text_delta',text:'SMS mock reply'};}},logger:{info:s=>logs.push(['info',s]),error:s=>logs.push(['error',s]),warn:s=>logs.push(['warn',s])}});
let response;
if(c.server){const address=c.server.address();response=await fetch('http://127.0.0.1:'+address.port+'/sms/incoming',{method:'POST',headers:{'content-type':'application/x-www-form-urlencoded'},body:new URLSearchParams({From:'+15550000002',Body:'SMS mock input'})}).then(async r=>({status:r.status,body:await r.text()}));const deadline=Date.now()+3000;while(sent.length===0&&Date.now()<deadline)await new Promise(r=>setTimeout(r,5));assert.equal(sent.length,1);}
await handle.stop('fixture');cases.push({kind,sdk,submits,sent,response,logs:logs.map(([level,s])=>[level,s.replace(/http:\/\/127\.0\.0\.1:\d+/g,'http://127.0.0.1:$PORT')])});
}
await writeFile(join(out,'oracle.json'),JSON.stringify({fixed,sourceSha256:sources,environment:process.versions,formalAcceptance:false,declaredMocks:['optional require(twilio) availability','twilio SDK factory/messages.create','Gateway submitTurn'],cases},null,2)+String.fromCharCode(10),{flag:'wx'});console.log(JSON.stringify({cases:cases.length}));
