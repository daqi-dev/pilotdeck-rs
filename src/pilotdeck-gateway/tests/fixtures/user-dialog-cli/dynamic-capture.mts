import assert from 'node:assert/strict';import{execFileSync}from'node:child_process';import{mkdir,writeFile}from'node:fs/promises';import{join,resolve}from'node:path';import{pathToFileURL}from'node:url';
const root=resolve(process.env.PDRS_BASELINE||'');const source='src/tool/dialog/FormDialogSchema.ts';const verify=()=>{assert.equal(execFileSync('git',['-C',root,'rev-parse','HEAD'],{encoding:'utf8'}).trim(),'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85');assert.equal(execFileSync('git',['-C',root,'hash-object',source],{encoding:'utf8'}).trim(),execFileSync('git',['-C',root,'rev-parse',`HEAD:${source}`],{encoding:'utf8'}).trim());};assert.equal(process.version,'v22.22.0');verify();const out=process.argv[2];assert.ok(out?.startsWith('/private/tmp/'));await mkdir(out,{recursive:false});const m=await import(pathToFileURL(join(root,source)).href);const D='https://json-schema.org/draft/2020-12/schema';
const cases=[
 ['root-dynamic',{$schema:D,type:'object',$dynamicAnchor:'node',properties:{child:{$dynamicRef:'#node'}}},{child:{}}],
 ['root-dynamic-bad',{$schema:D,type:'object',$dynamicAnchor:'node',properties:{child:{$dynamicRef:'#node'}}},{child:'x'}],
 ['root-dynamic-empty',{$schema:D,type:'object',$dynamicAnchor:'node',properties:{child:{$dynamicRef:'#node'}}},{child:{child:{}}}],
 ['nested-dynamic-string',{$schema:D,type:'object',$defs:{base:{$dynamicAnchor:'node',type:'string'}},properties:{x:{$dynamicRef:'#node'}}},{x:'a'}],
 ['nested-dynamic-object',{$schema:D,type:'object',$defs:{base:{$dynamicAnchor:'node',type:'string'}},properties:{x:{$dynamicRef:'#node'}}},{x:{}}],
 ['nested-dynamic-number',{$schema:D,type:'object',$defs:{base:{$dynamicAnchor:'node',type:'string'}},properties:{x:{$dynamicRef:'#node'}}},{x:1}],
 ['missing-dynamic',{$schema:D,type:'object',properties:{x:{$dynamicRef:'#missing'}}},{x:1}],
 ['dynamic-pointer',{$schema:D,type:'object',$defs:{base:{type:'string'}},properties:{x:{$dynamicRef:'#/$defs/base'}}},{x:'a'}],
 ['recursive-anchor',{$schema:D,type:'object',$recursiveAnchor:true},{}],
 ['recursive-ref',{$schema:D,type:'object',properties:{x:{$recursiveRef:'#'}}},{x:{}}],
 ['dynamic-anchor-bad',{$schema:D,type:'object',$dynamicAnchor:'a b'},{}],
 ['nested-id',{$schema:D,type:'object',$defs:{nested:{$id:'https://example.test/nested',type:'string'}},properties:{x:{$ref:'#/$defs/nested'}}},{x:'a'}],
 ['local-ref-root',{$schema:D,type:'object',properties:{child:{$ref:'#'}}},{child:{}}],
];
const records=cases.map(([id,schema,answer])=>{try{const result=m.validateFormDialogSchema(schema);return{id,schema,answer,result,accepted:result.ok?m.acceptsFormDialogAnswer(result.schema,answer):null};}catch(e){return{id,schema,answer,error:{name:e?.constructor?.name,message:e?.message}};}});verify();await writeFile(join(out,'oracle.json'),JSON.stringify({records},null,2)+'\n');
