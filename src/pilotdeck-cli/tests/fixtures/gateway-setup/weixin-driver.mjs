import {readFileSync,writeFileSync} from 'node:fs';
const test=JSON.parse(readFileSync(process.argv[2],'utf8'));const events=[];let index=0,clock=0;
const timeout=globalThis.setTimeout;Date.now=()=>clock;
globalThis.setTimeout=(callback,ms,...args)=>ms===1000?timeout(()=>{clock+=ms;callback(...args);},0):timeout(callback,ms,...args);
globalThis.fetch=async(url,init)=>{
 events.push({url,method:init?.method??'GET',headers:init?.headers??null,body:init?.body??null});const response=test.responses?.[index++];if(!response)throw new Error('unexpected fixture request');
 if(response.error)throw Object.assign(new Error(response.error),{name:response.errorName??'Error'});
 return new Response(response.raw??JSON.stringify(response.json),{status:response.status??200});
};
const {runGatewaySetup}=await import(process.argv[3]);
runGatewaySetup(test.argv).catch(error=>{console.error(error.message);process.exitCode=1;}).finally(()=>{const raw=JSON.stringify({events,clock},null,2)+'\n';if(process.env.SETUP_EVENTS_STDOUT==='1')console.log('SETUP_EVENTS='+JSON.stringify({events,clock}));else writeFileSync(process.env.SETUP_EVENTS,raw,{flag:'wx'});});
