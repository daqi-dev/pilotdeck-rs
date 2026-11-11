import assert from 'node:assert/strict';
import {writeFileSync} from 'node:fs';
const [url,project,out]=process.argv.slice(2);const rows:any[]=[];
try{for(const path of ['/api/module-host/approvals/status','/api/module-host/approvals/resume']){
 const body={tenantId:'tenant',agentId:'target',sessionKey:'web:sop-fixture',projectKey:project};
 const response=await fetch(new URL(path,url.replace('ws://','http://')),{method:'POST',headers:{authorization:'Bearer test-token','content-type':'application/json','x-staffdeck-approver-authorization':'Bearer approve-only'},body:JSON.stringify(body)});
 const raw=Buffer.from(await response.arrayBuffer());rows.push({path,requestBody:body,requestBodyHex:Buffer.from(JSON.stringify(body)).toString('hex'),status:response.status,headers:Object.fromEntries(response.headers),body:raw.toString(),responseBodyHex:raw.toString('hex')});assert.equal(response.status,403);assert.equal(JSON.parse(raw.toString()).code,'APPROVAL_SUBJECT_MISMATCH');
}}finally{writeFileSync(out+'/result.json',JSON.stringify({rows},null,2)+'\n',{flag:'wx'});}
