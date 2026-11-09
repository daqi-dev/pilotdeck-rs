import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { mkdir, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
const root = resolve(process.env.PDRS_BASELINE ?? '');
assert.equal(process.version, 'v22.22.0');
assert.equal(execFileSync('git',['-C',root,'rev-parse','HEAD'],{encoding:'utf8'}).trim(),
  'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85');
const source = 'src/always-on/contracts/PlanContract.ts';
const verify = () => assert.equal(execFileSync('git',['-C',root,'hash-object',source],{encoding:'utf8'}).trim(),
  execFileSync('git',['-C',root,'rev-parse',`HEAD:${source}`],{encoding:'utf8'}).trim());
verify();
const out = process.argv[2]; assert.ok(out?.startsWith('/private/tmp/')); await mkdir(out,{recursive:false});
const {parsePlanMarkdown} = await import(pathToFileURL(join(root,source)).href);
const valid = '# Demo\n\n> Always-On Discovery Plan\n> id: p1\n> sourceRunId: r1\n> createdAt: 2026-01-02\n> projectRoot: /p\n> dedupeKey: d1\n\n## Summary\nShort summary\n\n## Rationale\nReason\n\n## Context Signals\n- signal\n\n## Proposed Change\nPerform the update\n\n## Execution Steps\n1. Perform it\n\n## Verification\n- check\n';
const cases = [
  ['valid',valid],['crlf',valid.replaceAll('\n','\r\n')],['empty',''],
  ['no-title',valid.replace('# Demo','Demo')],['no-metadata',valid.replace('> Always-On Discovery Plan','> wrong')],
  ['missing-id',valid.replace('> id: p1','> id:')],['unknown-meta',valid.replace('> id: p1','> extra: x')],
  ['duplicate',valid+'\n## Summary\nagain'],['order',valid.replace('## Rationale','## Wrong')],
  ['summary-length',valid.replace('Short summary','x'.repeat(201))],
  ['summary-utf16',valid.replace('Short summary','🙂'.repeat(101))],
  ['missing-signals',valid.replace('- signal','signal')],['fuzzy',valid.replace('Perform the update','TODO later')],
  ['steps-unordered',valid.replace('1. Perform it','- Perform it')],['missing-verification',valid.replace('- check','check')],
  ['surrogate',valid.replace('Short summary','\ud800')],['nbsp',valid.replace('Short summary','hello\u00a0there')],
  ['metadata-prototype',valid.replace('> id: p1','> __proto__: x\n> constructor: y\n> id: p1')],
  ['section-prototype',valid.replace('## Rationale','## __proto__')],
];
const records = cases.map(([id,input])=>{
  try { return {id,input,result:parsePlanMarkdown(input)}; }
  catch (e) { return {id,input,error:{name:e?.constructor?.name,code:e?.code,message:e?.message}}; }
});
verify(); await writeFile(join(out,'oracle.json'),JSON.stringify({records},null,2)+'\n');
