// #1265: all expected semantics are observed with the pinned Node runtime.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import {fileURLToPath} from 'node:url';
assert.equal(process.version, 'v22.22.0', '#1265 fixed Node');
assert.equal(process.versions.icu, '78.2', '#1265 fixed ICU');
const {oracle: sdk} = await import('./generate-sdk.mjs');
const here = path.dirname(fileURLToPath(import.meta.url));
const out = process.argv[2];
const units = s => Array.from({length:s.length}, (_,i) => s.charCodeAt(i));
const text = a => a.map(x => String.fromCharCode(x)).join('');
const records = JSON.parse(fs.readFileSync(path.join(here, 'report-cases.json'), 'utf8'))
  .map(c => ({id:`report/${c.id}`, pattern:units(c.pattern), flags:c.flags, input:c.units, start:0}));
const add = (id, pattern, flags, input, start=0) => records.push({id,pattern:units(pattern),flags,input:units(input),start});
for (const flags of ['', 'u', 'v', 'g', 'gu', 'gv', 'y', 'uy', 'vy', 'dgy']) {
  for (const start of [0, 1, 2, 3, 4]) add(`start/${flags}/${start}`, '.', flags, '😀x', start);
}
for (const flags of ['', 'u', 'v']) {
  for (const surrogate of ['\ud800','\udfff','😀']) add(`pattern/${flags}/${units(surrogate).join('-')}`, `^${surrogate}$`, flags, surrogate);
}
add('flags/ims', '^a.b$', 'ims', 'z\nA\nB\ny');
add('flags/iu-kelvin', '^k$', 'iu', 'K');
add('flags/i-kelvin', '^k$', 'i', 'K');
add('flags/v-intersection', '^[\\p{ASCII}&&\\p{Letter}]+$', 'v', 'abc');
add('flags/v-subtraction', '^[a-z--[aeiou]]+$', 'v', 'xyz');
add('flags/v-string', '^[\\q{ab|cd}]$', 'v', 'ab');
add('flags/invalid-uv', '.', 'uv', 'a');
add('flags/duplicate', '.', 'gg', 'a');
add('flags/unknown', '.', 'x', 'a');
add('captures/named-indices', '(?<first>a)(?<unset>b)?', 'dg', 'za', 1);
add('captures/repeated', '(a(b)?)+', 'd', 'aba');
add('empty/zero-length-global', '(?:)', 'dg', 'a', 1);
add('empty/end-sticky', '$', 'dy', 'a', 1);
add('empty/failed-sticky', 'b', 'dy', 'a', 1);
add('injection/pattern', "');globalThis.injected=true;//", '', "');globalThis.injected=true;//");
add('injection/input', '^.*$', '', "');globalThis.injected=true;//");
const observe = c => {
  try {
    const re = new RegExp(text(c.pattern), c.flags); re.lastIndex = c.start;
    const m = re.exec(text(c.input));
    const groups = g => g === undefined ? null : Object.fromEntries(Object.entries(g).map(([k,v])=>[k,v === undefined ? null : units(v)]));
    return {lastIndex:re.lastIndex, matched:m === null ? null : {
      range:[m.index,m.index+m[0].length], captures:Array.from(m, x=>x === undefined ? null : units(x)), groups:groups(m.groups),
      indices:m.indices === undefined ? null : {captures:Array.from(m.indices, x=>x ?? null), groups:m.indices.groups === undefined ? null : Object.fromEntries(Object.entries(m.indices.groups).map(([k,v])=>[k,v ?? null]))},
    }};
  } catch(e) {return {error:e.name};}
};
const normalizedSdk = sdk.cases.map(c => ({
  id:c.id, pattern:units(c.schema.pattern ?? Object.keys(c.schema.patternProperties)[0]), flags:'u',
  input:units(typeof c.data === 'string' ? c.data : Object.keys(c.data)[0]),
  expected:c.compileError ? {error:c.compileError.name} : {matched:c.valid},
  regexp:observe({pattern:units(c.schema.pattern ?? Object.keys(c.schema.patternProperties)[0]), flags:'u', input:units(typeof c.data === 'string' ? c.data : Object.keys(c.data)[0]), start:0}),
}));
assert.equal(normalizedSdk.length, 108, '#1265 complete legacy corpus');
assert.equal(records.filter(c=>c.id.startsWith('report/')).length, 25, '#1265 complete report corpus');
const oracle = {node:process.version,icu:process.versions.icu,sourceCommit:sdk.sourceCommit,
  sourceSha256:sdk.sourceSha256, regexCompilation:sdk.regexCompilation,
  sdk:normalizedSdk, cases:records.map(c=>({...c,expected:observe(c)})),
  boundaries:[254,255].map(count=>{const c={id:`captures/${count}`,pattern:units('()'.repeat(count)),flags:'u',input:[],start:0};return {...c,expected:observe(c)};})};
fs.mkdirSync(out,{recursive:true});
fs.writeFileSync(path.join(out,'oracle.json'),JSON.stringify(oracle,null,2)+'\n',{flag:'wx'});
console.log(JSON.stringify({node:process.version,icu:process.versions.icu,sdk:normalizedSdk.length,semantics:records.length}));
