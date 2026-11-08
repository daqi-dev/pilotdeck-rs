import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { mkdir, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
const root=resolve(process.env.PDRS_BASELINE||''); const expected='cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';
const files=['src/tool/builtin/requestUserInput.ts','src/tool/builtin/requestUserChoice.ts','src/tool/builtin/requestUserConfirmation.ts','src/tool/builtin/requestUserForm.ts','src/tool/dialog/FormDialogSchema.ts'];
const verify=()=>{ assert.equal(execFileSync('git',['-C',root,'rev-parse','HEAD'],{encoding:'utf8'}).trim(),expected); for(const f of files) assert.equal(execFileSync('git',['-C',root,'hash-object',f],{encoding:'utf8'}).trim(),execFileSync('git',['-C',root,'rev-parse',`HEAD:${f}`],{encoding:'utf8'}).trim()); };
assert.equal(process.version,'v22.22.0'); verify(); const out=process.argv[2]; assert.ok(out?.startsWith('/private/tmp/')); await mkdir(out,{recursive:false});
const modules=await Promise.all(files.slice(0,4).map(f=>import(pathToFileURL(join(root,f)).href)));
const tools=[modules[0].createRequestUserInputTool(),modules[1].createRequestUserChoiceTool(),modules[2].createRequestUserConfirmationTool(),modules[3].createRequestUserFormTool()];
const cases=[
 ['input',0,{prompt:'What?',placeholder:'Type',allowEmpty:false},{type:'answered',value:'hello'}],
 ['input-surrogate',0,{prompt:'What?'},{type:'answered',value:'\ud800'}],
 ['input-empty',0,{prompt:'What?'},{type:'answered',value:'  '}],
 ['input-allowed-empty',0,{prompt:'What?',allowEmpty:true},{type:'answered',value:''}],
 ['choice',1,{prompt:'Pick',options:[{value:'a'},{value:'b',label:'Bee'}],defaultValue:'a'},{type:'answered',value:'b'}],
 ['choice-duplicate',1,{prompt:'Pick',options:[{value:'a'},{value:'a'}]},{type:'answered',value:'a'}],
 ['confirm-false',2,{prompt:'Proceed?',defaultValue:false},{type:'answered',value:false}],
 ['confirm-blank-label',2,{prompt:'Proceed?',confirmLabel:' '},{type:'answered',value:true}],
 ['form',3,{prompt:'Fill',schema:{type:'object',properties:{x:{type:'string'}}}},{type:'answered',value:{x:'ok'}}],
 ['cancel',0,{prompt:'What?'},{type:'cancelled',reason:'gone'}],
 ['cancel-empty',2,{prompt:'Proceed?'},{type:'cancelled'}],
];
const records=[];
for(const [id,index,input,answer] of cases){ const tool=tools[index]; const calls=[]; const channel={}; for(const key of ['requestInput','requestSelect','requestConfirm','requestForm'])channel[key]=async arg=>{calls.push({method:key,arg});return answer;};
 let validation=tool.validateInput?await tool.validateInput(input):undefined; let result,error;
 try{result=await tool.execute(input,{turnId:'turn-1',currentToolCallId:'call-1',userDialog:channel});}catch(e){error={code:e?.code,message:e?.message};}
 records.push({id,index,input,answer,validation,calls,result,error}); }
verify(); await writeFile(join(out,'oracle.json'),JSON.stringify({records},null,2)+'\n');
