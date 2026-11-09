import assert from 'node:assert/strict';import{execFileSync}from'node:child_process';import{mkdir,writeFile}from'node:fs/promises';import{join,resolve}from'node:path';import{pathToFileURL}from'node:url';
const root=resolve(process.env.PDRS_BASELINE||'');const source='src/tool/dialog/FormDialogSchema.ts';const verify=()=>{assert.equal(execFileSync('git',['-C',root,'rev-parse','HEAD'],{encoding:'utf8'}).trim(),'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85');assert.equal(execFileSync('git',['-C',root,'hash-object',source],{encoding:'utf8'}).trim(),execFileSync('git',['-C',root,'rev-parse',`HEAD:${source}`],{encoding:'utf8'}).trim());};assert.equal(process.version,'v22.22.0');verify();const out=process.argv[2];assert.ok(out?.startsWith('/private/tmp/'));await mkdir(out,{recursive:false});
const m=await import(pathToFileURL(join(root,source)).href);const draft='https://json-schema.org/draft/2020-12/schema';const base=(x)=>({$schema:draft,type:'object',...x});
const cases=[
 ['required-parent',base({properties:{x:{type:'string'}},allOf:[{required:['x']}]}),{x:'a'}],
 ['required-duplicate',base({properties:{x:{type:'string'}},required:['x','x']}),{x:'a'}],
 ['union-ok',base({properties:{x:{type:['string','null']}}}),{x:null}],['union-reject',base({properties:{x:{type:['string','number']}}}),{x:1}],
 ['type-invalid',base({properties:{x:{type:'wrong'}}}),{}],['empty-enum',base({properties:{x:{enum:[]}}}),{}],['duplicate-enum',base({properties:{x:{enum:[1,1]}}}),{}],
 ['numeric-type',base({properties:{x:{type:'number',minimum:'x'}}}),{}],['exclusive-order',base({properties:{x:{type:'number',minimum:10,maximum:1}}}),{x:5}],
 ['then-without-if',base({then:{properties:{a:{type:'string'}}}}),{}],['if-only',base({if:{properties:{a:{type:'string'}}}}),{}],
 ['pattern-type-missing',base({properties:{x:{pattern:'a'}}}),{x:'a'}],['unknown-format',base({properties:{x:{type:'string',format:'made-up'}}}),{x:'a'}],
 ['local-pointer-escape',base({$defs:{'x/y':{type:'string'}},properties:{x:{$ref:'#/$defs/x~1y'}}}),{x:'a'}],
 ['dynamic-anchor',base({$defs:{base:{$dynamicAnchor:'node',type:'string'}},properties:{x:{$dynamicRef:'#node'}}}),{x:'a'}],
 ['recursive-ref',base({$defs:{node:{type:'object',properties:{child:{$ref:'#/$defs/node'}}}},properties:{x:{$ref:'#/$defs/node'}}}),{x:{child:{}}}],
 ['nested-unevaluated',base({properties:{obj:{type:'object',properties:{x:{type:'number'}},unevaluatedProperties:false}}}),{obj:{x:1,y:2}}],
 ['pattern-additional',base({patternProperties:{'^x':{type:'integer'}},additionalProperties:false}),{x:1}],
 ['false-additional',base({properties:{x:{type:'string'}},additionalProperties:false}),{x:'a',y:1}],
 ['not-annotation',base({not:{properties:{x:{type:'string'}}},unevaluatedProperties:false}),{x:3}],
 ['ref-annotation',base({$defs:{obj:{properties:{x:{type:'string'}}}},$ref:'#/$defs/obj',unevaluatedProperties:false}),{x:'yes'}],
 ['prefix-contains',base({properties:{x:{type:'array',prefixItems:[{type:'integer'}],contains:{type:'integer'},unevaluatedItems:false}}}),{x:[1,'x']}],
 ['min-length',base({properties:{x:{type:'string',minLength:2}}}),{x:'🙂'}],
 ['unicode-pattern',base({properties:{x:{type:'string',pattern:'^\\uD800$'}}}),{x:'\ud800'}],
];
const records=cases.map(([id,schema,answer])=>{try{const result=m.validateFormDialogSchema(schema);return{id,schema,answer,result,accepted:result.ok?m.acceptsFormDialogAnswer(result.schema,answer):null};}catch(e){return{id,schema,answer,error:{name:e?.constructor?.name,message:e?.message}};}});verify();await writeFile(join(out,'oracle.json'),JSON.stringify({records},null,2)+'\n');
