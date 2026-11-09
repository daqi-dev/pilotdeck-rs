import assert from 'node:assert/strict';
import {execFileSync} from 'node:child_process';import {mkdir,writeFile} from 'node:fs/promises';import{join,resolve}from'node:path';import{pathToFileURL}from'node:url';
const root=resolve(process.env.PDRS_BASELINE||'');const source='src/tool/dialog/FormDialogSchema.ts';const sha='cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';
const verify=()=>{assert.equal(execFileSync('git',['-C',root,'rev-parse','HEAD'],{encoding:'utf8'}).trim(),sha);assert.equal(execFileSync('git',['-C',root,'hash-object',source],{encoding:'utf8'}).trim(),execFileSync('git',['-C',root,'rev-parse',`HEAD:${source}`],{encoding:'utf8'}).trim());};
assert.equal(process.version,'v22.22.0');verify();const out=process.argv[2];assert.ok(out?.startsWith('/private/tmp/'));await mkdir(out,{recursive:false});
const m=await import(pathToFileURL(join(root,source)).href);const draft='https://json-schema.org/draft/2020-12/schema';const base=(x)=>({$schema:draft,type:'object',...x});
const cases=[
 ['simple',base({properties:{x:{type:'string'}},required:['x']}),{x:'ok'}],['required',base({properties:{x:{type:'string'}},required:['x']}),{}],
 ['surrogate',base({properties:{x:{type:'string',minLength:1,maxLength:1}}}),{x:'\ud800'}],['astral-codepoints',base({properties:{x:{type:'string',maxLength:1}}}),{x:'🙂'}],
 ['astral-pattern',base({properties:{x:{type:'string',pattern:'^.$'}}}),{x:'🙂'}],
 ['unevaluated-ok',base({properties:{a:{type:'number'}},unevaluatedProperties:false}),{a:1}],['unevaluated-no',base({properties:{a:{type:'number'}},unevaluatedProperties:false}),{a:1,b:2}],
 ['allof-evaluated',base({allOf:[{properties:{a:{type:'number'}}},{properties:{b:{type:'number'}}}],unevaluatedProperties:false}),{a:1,b:2}],
 ['anyof-evaluated',base({anyOf:[{properties:{a:{type:'number'}}},{properties:{b:{type:'number'}}}],unevaluatedProperties:false}),{a:1,b:2}],
 ['oneof-evaluated',base({oneOf:[{required:['a'],properties:{a:{type:'number'}}},{required:['b'],properties:{b:{type:'number'}}}],unevaluatedProperties:false}),{a:1}],
 ['if-evaluated',base({properties:{tag:{type:'boolean'}},if:{properties:{tag:{const:true}}},then:{properties:{x:{type:'string'}}},else:{properties:{y:{type:'number'}}},unevaluatedProperties:false}),{tag:true,x:'y'}],
 ['prefix-items',base({properties:{x:{type:'array',prefixItems:[{type:'string'}],items:{type:'number'}}}}),{x:['a',1]}],
 ['unevaluated-items-ok',base({properties:{x:{type:'array',prefixItems:[{type:'string'}],unevaluatedItems:false}}}),{x:['a']}],
 ['unevaluated-items-no',base({properties:{x:{type:'array',prefixItems:[{type:'string'}],unevaluatedItems:false}}}),{x:['a',2]}],
 ['contains-evaluated',base({properties:{x:{type:'array',contains:{type:'integer'},minContains:1,maxContains:1,unevaluatedItems:false}}}),{x:[1,'x']}],
 ['ref-pointer',base({$defs:{S:{type:'string'}},properties:{x:{$ref:'#/$defs/S'}}}),{x:'v'}],
 ['ref-other-pointer',base({properties:{x:{type:'string'},y:{$ref:'#/properties/x'}}}),{x:'a',y:'b'}],
 ['ref-anchor',base({$defs:{S:{$anchor:'mytype',type:'string'}},properties:{x:{$ref:'#mytype'}}}),{x:'v'}],
 ['external',base({properties:{x:{$ref:'https://example.test/schema'}}}),{x:1}],
 ['unknown-key',base({garbage:true}),{}],['bad-required',base({required:['missing']}),{}],
 ['boolean-schema',base({properties:{x:false}}),{x:1}],['contains-minzero',base({properties:{x:{type:'array',contains:{type:'integer'},minContains:0}}}),{x:['a']}],
 ['dependent',base({properties:{a:{type:'string'},b:{type:'string'}},dependentRequired:{a:['b']}}),{a:'x'}],
 ['nullable',base({properties:{x:{type:'string',nullable:true}}}),{x:null}],
];
const records=cases.map(([id,schema,answer])=>{try{const result=m.validateFormDialogSchema(schema);return{id,schema,answer,result,accepted:result.ok?m.acceptsFormDialogAnswer(result.schema,answer):null};}catch(e){return{id,schema,answer,error:{name:e?.constructor?.name,message:e?.message}};}});
verify();await writeFile(join(out,'oracle.json'),JSON.stringify({records},null,2)+'\n');
