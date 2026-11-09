import {readFileSync,writeFileSync} from 'node:fs';
const test=JSON.parse(readFileSync(process.argv[2],'utf8'));
const events=[];let index=0,clock=0;
const originalTimeout=globalThis.setTimeout;
Date.now=()=>clock;
globalThis.setTimeout=(callback,ms,...args)=>ms===3000?originalTimeout(()=>{clock+=ms;callback(...args);},0):originalTimeout(callback,ms,...args);
globalThis.fetch=async(url,init)=>{
 events.push({url,method:init?.method??'GET',body:init?.body??null,headers:init?.headers??null});
 const response=test.responses?.[index++];
 if(!response)throw new Error('unexpected fixture request');
 if(response.error)throw new Error(response.error);
 return new Response(response.raw??JSON.stringify(response.json),{status:response.status??200});
};
const {runGatewaySetup}=await import(process.argv[3]);
runGatewaySetup(test.argv).catch(error=>{console.error(error.message);process.exitCode=1;}).finally(()=>{
 writeFileSync(process.env.SETUP_EVENTS,JSON.stringify({events,clock},null,2)+'\n',{flag:'wx'});
});
