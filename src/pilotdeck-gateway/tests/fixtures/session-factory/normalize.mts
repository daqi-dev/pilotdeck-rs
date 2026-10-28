import path from 'node:path';
import assert from 'node:assert/strict';
export function normalize(report:any, root:string):any {
 const caseRoot=path.resolve(root,report.caseDir);
 const projects=report.finalFiles.filter((f:any)=>/^home\/projects\/[^/]+$/.test(f.path)).map((f:any)=>f.path.split('/')[2]);
 assert(projects.length<=1);
 const project=projects[0];
 function visit(value:any, at:string[]):any {
  if(Array.isArray(value))return value.map((v,i)=>visit(v,[...at,String(i)]));
  if(value&&typeof value==='object')return Object.fromEntries(Object.entries(value).filter(([k])=>k!=='elapsedNs').map(([k,v])=>[k,visit(v,[...at,k])]));
  if(typeof value!=='string')return value;
  const key=at.at(-1);
  if(key==='caseDir')return `run-<RUN>/${report.name}`;
  if(value.includes(caseRoot)||(project&&value.includes(project))){
   assert(['path','cwd','transcriptPath','message'].includes(key!),`unregistered dynamic path: ${at.join('.')}`);
   return value.split(caseRoot).join('<CASE>').split(project??'\0').join('<PROJECT>');
  }
  return value;
 }
 return visit(report,[]);
}
