import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { mkdir, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';

const root = resolve(process.env.PDRS_BASELINE ?? '');
assert.equal(process.version, 'v22.22.0');
assert.equal(execFileSync('git',['-C',root,'rev-parse','HEAD'],{encoding:'utf8'}).trim(),
  'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85');
const source = 'src/always-on/contracts/ReportContract.ts';
const verifySource = () => assert.equal(
  execFileSync('git',['-C',root,'hash-object',source],{encoding:'utf8'}).trim(),
  execFileSync('git',['-C',root,'rev-parse',`HEAD:${source}`],{encoding:'utf8'}).trim());
verifySource();
const out = process.argv[2]; assert.ok(out?.startsWith('/private/tmp/'));
await mkdir(out,{recursive:false});
const {parseReportMarkdown,buildFallbackReport} = await import(pathToFileURL(join(root,'src/always-on/contracts/ReportContract.ts')).href);
const metadata = {runId:'r',planId:'p',startedAt:'2026-01-02T03:04:05.678Z',finishedAt:'2026-01-02T03:05:05.678Z',outcome:'executed',workspaceStrategy:'snapshot-copy',workspaceHandle:'/work'};
const cases = [
  ['empty',''], ['title','# Title\n'], ['required-h1','# Files Changed\nfile'],
  ['crlf','# T\r\n\r\n> ignored\r\n\r\n## Notes\r\nnotes\r\n'],
  ['duplicate','# T\n## Notes\na\n## Notes\nb\n'],
  ['custom','# T\n## Custom\ncustom\n## Notes\nhello\n'],
  ['title-surrogate','# \ud800\n## Notes\n\udfff'],
  ['spaces','  \n#   \n## Steps Performed  \n value  \n'],
  ['pretitle','unheaded\n## Notes\na\n'],
  ['numeric-keys','# T\n## 9\nx\n## 2\ny\n## Notes\nz'],
  ['proto','# T\n## __proto__\nx\n## constructor\ny\n## Notes\nz'],
  ['js-whitespace','\ufeff\n# T\n## Notes\ntext\ufeff  \n'],
];
const records = cases.map(([id,input])=>({id,input,result:parseReportMarkdown(input,metadata)}));
records.push({id:'fallback',input:'partial',result:buildFallbackReport({metadata,title:'Demo',reason:'failed',partial:' partial '})});
verifySource();
await writeFile(join(out,'oracle.json'),JSON.stringify({metadata,records},null,2)+'\n');
