import { execFileSync } from 'node:child_process';
import { mkdir, writeFile } from 'node:fs/promises';
import { resolve, join } from 'node:path';
import { pathToFileURL } from 'node:url';
import assert from 'node:assert/strict';
const root = resolve(process.env.PDRS_BASELINE || '');
const source = 'src/tool/dialog/FormDialogSchema.ts';
const expected = 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';
const verify = () => {
  assert.equal(execFileSync('git',['-C',root,'rev-parse','HEAD'],{encoding:'utf8'}).trim(), expected);
  assert.equal(execFileSync('git',['-C',root,'hash-object',source],{encoding:'utf8'}).trim(), execFileSync('git',['-C',root,'rev-parse',`HEAD:${source}`],{encoding:'utf8'}).trim());
};
assert.equal(process.version,'v22.22.0'); verify();
const out=process.argv[2]; assert.ok(out?.startsWith('/private/tmp/')); await mkdir(out,{recursive:false});
const m=await import(pathToFileURL(join(root,source)).href);
const cases = [
 ['empty',{},{}],['object',{type:'object'},{}],['required',{type:'object',properties:{name:{type:'string',minLength:2}},required:['name']},{name:'ok'}],
 ['missing',{type:'object',properties:{name:{type:'string'}},required:['name']},{}],
 ['astral-utf16',{type:'object',properties:{name:{type:'string',minLength:2}}},{name:'🙂'}],
 ['lone-surrogate',{type:'object',properties:{name:{type:'string',maxLength:1}}},{name:'\ud800'}],
 ['extra',{type:'object',properties:{x:{type:'integer'}},additionalProperties:false},{x:2,y:3}],
 ['prefix',{type:'object',properties:{a:{type:'array',prefixItems:[{type:'string'}],items:{type:'integer'},contains:{const:2}}}},{a:['x',2]}],
 ['refs',{type:'object',$defs:{X:{type:'string',enum:['yes']}},properties:{x:{$ref:'#/$defs/X'}}},{x:'yes'}],
 ['unknown-ref',{type:'object',properties:{x:{$ref:'#/$defs/Missing'}}},{}],
 ['circular',{type:'object',$defs:{A:{$ref:'#/$defs/B'},B:{$ref:'#/$defs/A'}}},{}],
 ['dependent',{type:'object',properties:{a:{type:'number'},b:{type:'number'}},dependentRequired:{a:['b']}},{a:2}],
 ['unsupported-key',{type:'object',contentEncoding:'base64'},{}],
 ['regex-unicode',{type:'object',properties:{s:{type:'string',pattern:'^\\w+$'}}},{s:'é'}],
 ['pattern-additional',{type:'object',patternProperties:{'^x':{type:'number'}},additionalProperties:false},{x:1}],
 ['draft-unevaluated',{$schema:'https://json-schema.org/draft/2020-12/schema',type:'object',properties:{x:{type:'string'}},unevaluatedProperties:false},{x:'ok',y:1}],
 ['draft-prefix',{$schema:'https://json-schema.org/draft/2020-12/schema',type:'object',properties:{a:{type:'array',prefixItems:[{type:'string'}],items:{type:'number'}}}},{a:['x',2]}],
];
const records=cases.map(([id,schema,answer])=>{ try {const result=m.validateFormDialogSchema(schema); return {id,schema,answer,result,accepted:result.ok?m.acceptsFormDialogAnswer(result.schema,answer):null};} catch(error) {return {id,schema,answer,error:{name:error?.constructor?.name,message:error?.message}};} });
verify(); await writeFile(join(out,'oracle.json'), JSON.stringify({records},null,2)+'\n');
