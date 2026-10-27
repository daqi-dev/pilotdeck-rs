import assert from 'node:assert/strict';
import {execFileSync} from 'node:child_process';
import {mkdirSync, writeFileSync} from 'node:fs';
import {join} from 'node:path';
assert.equal(process.version, 'v22.22.0');
const cases = [
  ['unset', {}], ['C', {LANG:'C'}], ['POSIX', {LANG:'POSIX'}],
  ['C.UTF-8', {LANG:'C.UTF-8'}], ['invalid', {LANG:'not_a_locale!!!'}],
  ['modifier', {LANG:'en_US@modifier'}], ['empty-LANG', {LANG:''}],
  ['empty-LC_ALL', {LC_ALL:'', LANG:'en_US.UTF-8'}],
  ['empty-LC_MESSAGES', {LC_MESSAGES:'', LANG:'en_US.UTF-8'}],
  ['hyphen', {LANG:'zh-CN'}], ['en-US', {LANG:'en_US.UTF-8'}],
  ['zh-CN', {LANG:'zh_CN.UTF-8'}],
  ['invalid-language', {LANG:'!!!'}],
  ['invalid-suffix', {LANG:'tr_TR_invalid!!!'}],
  ['invalid-extension', {LANG:'not-a-real-locale'}],
  ['modifier-charset', {LANG:'en_US.UTF-8@modifier'}],
  ['modifier-short', {LANG:'en_US@foo'}],
  ['messages-priority', {LANG:'en_US.UTF-8', LC_MESSAGES:'zh_CN.UTF-8'}],
  ['all-priority', {LANG:'zh_CN.UTF-8', LC_MESSAGES:'zh_CN.UTF-8', LC_ALL:'en_US.UTF-8'}],
];
const script = `const names=['z','ä','Z','a','A','你好','文件','😀','_x']; console.log(JSON.stringify({locale:new Intl.Collator().resolvedOptions().locale,order:names.sort((a,b)=>a.localeCompare(b))}));`;
const localeProbe=[];
for (const [id, localeEnv] of cases) {
  const env = Object.fromEntries(Object.entries(process.env).filter(([k])=>k!=='LANG'&&!k.startsWith('LC_')));
  Object.assign(env,localeEnv);
  localeProbe.push({id,env:localeEnv,...JSON.parse(execFileSync(process.execPath,['-e',script],{env,encoding:'utf8'}))});
}

assert.ok(process.argv[2], 'output directory required');
mkdirSync(process.argv[2],{recursive:true});
const data=JSON.stringify({node:process.version,localeProbe},null,2)+'\n';
writeFileSync(join(process.argv[2],'oracle.json'),data,{flag:'wx'});
console.log(data);
