import {PromptAssembler} from '/Users/da/ws/pdrs-runner/resources/PilotDeck/src/context/prompt/PromptAssembler.ts';
import {PluginRuntime} from '/Users/da/ws/pdrs-runner/resources/PilotDeck/src/extension/plugins/runtime/PluginRuntime.ts';
import {buildCachePlan} from '/Users/da/ws/pdrs-runner/resources/PilotDeck/src/context/cache/CachePlan.ts';
import {readFileSync,writeFileSync} from 'node:fs';
import {execFileSync} from 'node:child_process';
if(process.version!=='v22.22.0'||process.versions.icu!=='78.2')throw Error('Node/ICU mismatch');
const root='/Users/da/ws/pdrs-runner/resources/PilotDeck';
for(const rel of ['src/context/prompt/PromptAssembler.ts','src/context/cache/CachePlan.ts','src/extension/plugins/runtime/PluginRuntime.ts','src/extension/plugins/runtime/truncateMcpString.ts']) {
 if(!execFileSync('git',['-C',root,'show','cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85:'+rel]).equals(readFileSync(root+'/'+rel)))throw Error('source mismatch '+rel);
}
if(!process.env.PDRS_ORACLE_OUT)throw Error('PDRS_ORACLE_OUT required');
const runtime=new PluginRuntime({projectRoot:'/fixture',pilotHome:'/fixture/home',builtinPlugins:[]});
const plugin={name:'fixture',path:'/fixture/plugin',source:'project',commands:[],skills:[],mcpServers:{['name<&"\ud800']:{instructions:' '+ 'x'.repeat(2047)+'😀'},normal:{instructions:'  HELLO \n'},['\ud800']:{instructions:'HIGH'},['\udc00']:{instructions:'LOW'},['\ufffd']:{instructions:'REPLACEMENT'}}};
const lease=runtime.acquireSessionContributions([plugin]);
const mcp=lease.contributions.mcpInstructions;

const extension={listCommands:()=>[],listSkills:()=>[],listToolContributions:()=>[],listPromptContributions:()=>[],listMcpInstructions:()=>mcp};
const assembler=new PromptAssembler(extension);
const input={cwd:'/fixture',provider:'fixture',model:'mock',permissionMode:'default',additionalWorkingDirectories:[],tools:[],includeUserContextInSystemPrompt:false,now:()=>new Date('2026-01-02T03:04:05.678Z')};
const results=[];for(const [id,extra] of [['default',{}],['custom-suppressed',{customSystemPrompt:'custom'}],['custom-extensions',{customSystemPrompt:'custom',includeExtensionsWithCustomSystemPrompt:true}],['custom-lone',{customSystemPrompt:'PRE\ud800POST'}],['append-lone',{appendSystemPrompt:'APP\udc00END'}]]) {
 const prompt=assembler.assemble({...input,...extra});const plan=buildCachePlan({provider:'fixture',model:'mock',systemPrompt:prompt.joined,tools:[],messages:[],enabled:true},1);results.push({id,prompt,plan});
}
const names=['\ud800','\ufffd','A','\udc00','z'];
const toolPlans=buildCachePlan({provider:'fixture',model:'mock',systemPrompt:'x',tools:names.map(name=>({name,description:'',inputSchema:{type:'object'}})),messages:[],enabled:true},1);
writeFileSync(process.env.PDRS_ORACLE_OUT!,JSON.stringify({node:process.version,icu:process.versions.icu,mcp,results,names,toolPlans},null,2),{flag:'wx'});
