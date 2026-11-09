// Run only against the pinned TS checkout; the output directory must be new.
import {mkdir,writeFile} from "node:fs/promises";
import {resolve} from "node:path";
import {pathToFileURL} from "node:url";
const source=process.env.PILOTDECK_TS_ROOT!;
const load=(path:string)=>import(pathToFileURL(resolve(source,"src",path)).href);
const {GatewaySessionModelBundle}=await load("cli/GatewaySessionModelBundle.ts");
const {NativeSessionModelSelectionPolicy}=await load("model/session/NativeSessionModelSelectionPolicy.ts");
const {createDialogProjectRegistry}=await load("gateway/dialog/projectRegistry.ts");
const {listRegisteredWebProjects,createRegisteredWebProjectResolver}=await load("web/server/listProjects.ts");
const root=resolve(process.argv[2]);await mkdir(root);const home=resolve(root,"home"),project=resolve(root,"project"),other=resolve(root,"other");
for(const path of [home,project,other,home+"/projects/main",home+"/projects/other"])await mkdir(path,{recursive:true});
await writeFile(home+"/projects/main/.cwd",project);await writeFile(home+"/projects/other/.cwd",other);
await writeFile(home+"/pilotdeck.yaml",`schemaVersion: 1\nagent: {model: fixture/mock}\nmodel:\n  providers:\n    fixture:\n      protocol: openai\n      url: http://127.0.0.1:1/v1\n      apiKey: fixture-only\n      models: {mock: {}}\n`);
process.chdir(root);
const registry=createDialogProjectRegistry({pilotHome:home,listProjects:()=>listRegisteredWebProjects({pilotHome:home}),resolveProject:createRegisteredWebProjectResolver({pilotHome:home})});
const reads:unknown[]=[];
const bundle=new GatewaySessionModelBundle({fallbackProjectKey:project,resolveProjectKey:registry.resolveProjectKey,router:{hasActiveTurn:()=>false,close:async()=>{}},selectionPort:{read:async(scope:unknown)=>{reads.push(scope);return undefined},write:async()=>{},clear:async()=>{}},policy:new NativeSessionModelSelectionPolicy({PILOT_HOME:home,HOME:home})});
const inputs=[{name:"key_empty",projectKey:""},{name:"key_number",projectKey:123},{name:"key_object",projectKey:{}},{name:"key_array",projectKey:[]},{name:"key_null",projectKey:null},{name:"key_undefined",projectKey:undefined},{name:"key_alias",projectKey:project+"/./child/.."},{name:"key_relative",projectKey:"project"},{name:"key_other",projectKey:other},{name:"key_home",projectKey:home},{name:"override_empty",modelOverride:""},{name:"override_zero",modelOverride:0},{name:"override_true",modelOverride:true},{name:"override_string",modelOverride:"model"},{name:"override_array",modelOverride:[]},{name:"selection_empty",modelSelection:{}},{name:"selection_undefined",modelSelection:undefined}];
const rows=[];for(const {name,...test} of inputs){const input={projectKey:project,sessionKey:"session",...test};const before=reads.length;try{rows.push({name,input,result:await bundle.resolveTurnModelSelection(input),reads:reads.length-before})}catch(error:any){rows.push({name,input,error:{name:error.name,code:error.code,message:error.message},reads:reads.length-before})}}
await writeFile(root+"/raw.json",JSON.stringify({node:process.version,root,rows},null,2)+"\n",{flag:"wx"});
const normalized=JSON.parse(JSON.stringify({node:process.version,rows}).replaceAll(other,"<other>").replaceAll(project,"<project>").replaceAll(home,"<home>"));
await writeFile(root+"/oracle.json",JSON.stringify(normalized,null,2)+"\n",{flag:"wx"});
