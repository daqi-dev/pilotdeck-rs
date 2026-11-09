// Non-frozen actual fixed Weixin dispatch, real helper classes and registry.
import assert from 'node:assert/strict';
import {execFileSync} from 'node:child_process';
import {createRequire} from 'node:module';
import {mkdtemp,mkdir,writeFile} from 'node:fs/promises';
import {createHash} from 'node:crypto';
import {tmpdir} from 'node:os';
import {join,resolve,posix} from 'node:path';
import {pathToFileURL} from 'node:url';
const [base,outArg]=process.argv.slice(2),out=resolve(outArg);await mkdir(out,{recursive:true});
const fixed='cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85',home=await mkdtemp(join(tmpdir(),'weixin-command-ts-'));
const require=createRequire(join(base,'package.json')), {transformSync}=createRequire(require.resolve('tsx'))('esbuild');
const ilink=await import(pathToFileURL(join(base,'node_modules/weixin-ilink/dist/index.js')).href),sources={},cache=new Map();
function load(path){if(cache.has(path))return cache.get(path).exports;
const source=execFileSync('git',['-C',base,'show',fixed+':'+path],{encoding:'utf8'});sources[path]=createHash('sha256').update(source).digest('hex');const module={exports:{}};cache.set(path,module);
const code=transformSync(source,{loader:'ts',format:'cjs',target:'es2022',supported:{'dynamic-import':false},define:{'import.meta.url':JSON.stringify(pathToFileURL(join(base,path)).href)}}).code;
new Function('require','module','exports',code)(name=>{if(name==='weixin-ilink')return ilink;if(name==='node:os')return {...require(name),homedir:()=>home};if(name.startsWith('node:'))return require(name);assert(name.startsWith('.'));return load(posix.normalize(posix.join(posix.dirname(path),name)).replace(/\.js$/,'.ts'));},module,module.exports);return module.exports;}
const {WeixinChannel}=load('src/adapters/channel/weixin/WeixinChannel.ts');const cases=[];
for(const kind of ['projects','switch','permission','elicitation','new-clears']){
const replies=[],calls=[],saved=[];const c=new WeixinChannel({credentialsPath:join(home,kind+'.json'),onStateChange:s=>saved.push(s)});
c.gateway={listProjects:async()=>{calls.push(['list_projects']);return{projects:[{name:'One',projectKey:'/fixture/one',fullPath:'/fixture/one'}]};},permissionDecide:async v=>{calls.push(['permissionDecide',v]);return{delivered:true};},respondElicitation:async v=>{calls.push(['respondElicitation',v]);},abortTurn:async v=>calls.push(['abortTurn',v])};
c.sendReply=async(user,text)=>{replies.push({user,text});return true;};
const dispatch=async text=>c.dispatchMessage({from_user_id:'chat',context_token:'ctx',message_type:1,item_list:[{type:1,text_item:{text}}]});
if(kind==='permission'||kind==='new-clears'){c.permissions.capture('chat','weixin:chat=chat:general',{type:'permission_request',requestId:'p1',toolName:'bash',payload:{command:'echo mock'}});c.permissions.confirmInitialPrompt('chat',true,'p1');}
if(kind==='elicitation'){c.elicitation.capture('chat','weixin:chat=chat:general',{type:'elicitation_request',requestId:'e1',questions:[{id:'q',question:'Choose',options:[{label:'One',value:'one'},{label:'Two',value:'two'}]}]});}
await dispatch(kind==='projects'?'/projects':kind==='switch'?'/switch-project One':kind==='new-clears'?'/new':'1');
cases.push({kind,replies,calls,savedCount:saved.length,permissionPending:c.permissions.hasPending('chat'),elicitationPending:c.elicitation.hasPending('chat'),project:c.mapper.getProject('chat')});
}
await writeFile(join(out,'oracle.json'),JSON.stringify({fixed,sourceSha256:sources,environment:process.versions,formalAcceptance:false,declaredMocks:['gateway host RPC','sendReply transport only','homedir isolated'],cases},null,2)+String.fromCharCode(10),{flag:'wx'});console.log(JSON.stringify({cases:cases.length}));
