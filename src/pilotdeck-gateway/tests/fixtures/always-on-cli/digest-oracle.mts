import assert from 'node:assert/strict';
import {execFileSync} from 'node:child_process';
import {mkdir,writeFile} from 'node:fs/promises';
import {join,resolve} from 'node:path';
import {pathToFileURL} from 'node:url';
const root=resolve(process.env.PDRS_BASELINE??'');assert.equal(process.version,'v22.22.0');
assert.equal(execFileSync('git',['-C',root,'rev-parse','HEAD'],{encoding:'utf8'}).trim(),'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85');
const source='src/session/history/ProjectSessionTranscriptReader.ts';
const verify=()=>assert.equal(execFileSync('git',['-C',root,'hash-object',source],{encoding:'utf8'}).trim(),execFileSync('git',['-C',root,'rev-parse',`HEAD:${source}`],{encoding:'utf8'}).trim());verify();
const out=process.argv[2];assert.ok(out?.startsWith('/private/tmp/'));await mkdir(out,{recursive:false});
const {extractUserPromptsFromJsonl}=await import(pathToFileURL(join(root,source)).href);
const one=(text:string)=>JSON.stringify({type:'accepted_input',messages:[{content:[{type:'text',text}]}]});
const cases=[['dedupe',`${one(' hello ')}\n${one('hello')}`],['spaced','{"type": "accepted_input","messages":[{"content":[{"type":"text","text":"ignored"}]}]}'],
  ['invalid',`{bad}\n${one('ok')}`],['surrogate',one('x'.repeat(499)+'🙂')],
  ['limit',Array.from({length:12},(_,i)=>one(`p${i}`)).join('\n')],['empty',one(' \t\n ')]];
const records=cases.map(([id,input])=>({id,input,result:extractUserPromptsFromJsonl(input,{maxPrompts:8,maxPromptLength:500})}));
verify();await writeFile(join(out,'oracle.json'),JSON.stringify({records},null,2)+'\n');
