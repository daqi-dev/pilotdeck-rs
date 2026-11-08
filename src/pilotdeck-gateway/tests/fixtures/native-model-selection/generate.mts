import {mkdir,writeFile} from "node:fs/promises";
import {GatewaySessionModelBundle} from "/Users/da/ws/pdrs-runner/resources/PilotDeck/src/cli/GatewaySessionModelBundle.ts";
import {NativeSessionModelSelectionPolicy} from "/Users/da/ws/pdrs-runner/resources/PilotDeck/src/model/session/NativeSessionModelSelectionPolicy.ts";
import {DialogGatewayError} from "/Users/da/ws/pdrs-runner/resources/PilotDeck/src/gateway/dialog/errors.ts";
const root=process.argv[2],home=root+"/home",project=root+"/project"; await mkdir(home,{recursive:true});await mkdir(project,{recursive:true});
await writeFile(home+"/pilotdeck.yaml",`schemaVersion: 1
agent: {model: fixture/mock}
model:
  providers:
    fixture:
      protocol: openai
      url: http://127.0.0.1:1/v1
      apiKey: fixture-only
      models: {mock: {}}
`);
const policy=new NativeSessionModelSelectionPolicy({PILOT_HOME:home,HOME:home});
const read=[];const bundle=new GatewaySessionModelBundle({fallbackProjectKey:project,resolveProjectKey:async(key)=>{if(key!==project)throw new DialogGatewayError("PROJECT_NOT_FOUND","Unknown projectKey: "+key);return key},router:{hasActiveTurn:()=>false,close:async()=>{}},selectionPort:{read:async(scope)=>{read.push(scope);return undefined},write:async()=>{},clear:async()=>{}},policy});
const inputs=[{name:"default"},{name:"both_null",modelSelection:null,modelOverride:null},{name:"selected_null",modelSelection:null},{name:"selected_false",modelSelection:false},{name:"selected_auto",modelSelection:{mode:"auto"}},{name:"selected_model",modelSelection:{mode:"model",provider:"fixture",model:"mock"}},{name:"override_model",modelOverride:{mode:"model",provider:"fixture",model:"mock"}},{name:"override_false",modelOverride:false},{name:"override_null",modelOverride:null},{name:"override_invalid",modelOverride:{mode:"model",provider:"missing",model:"mock"}},{name:"override_auto",modelOverride:{mode:"auto"}},{name:"selected_array",modelSelection:[]},{name:"unknown_before_both",projectKey:"unknown",modelSelection:null,modelOverride:null},{name:"unknown_before_null",projectKey:"unknown",modelSelection:null},{name:"unknown_before_override",projectKey:"unknown",modelOverride:{provider:"fixture",model:"mock"}}];
const rows=[];for(const {name,...test} of inputs){const input={projectKey:project,sessionKey:"session",...test};const calls=read.length;try{rows.push({name,input,result:await bundle.resolveTurnModelSelection(input as never),reads:read.length-calls})}catch(error){rows.push({name,input,error:{name:error.name,code:error.code,message:error.message},reads:read.length-calls})}}
await writeFile(root+"/oracle.json",JSON.stringify({node:process.version,rows},null,2)+"\n",{flag:"wx"});
