// Execute only the frozen harness's input declarations; no baseline patching,
// network, product imports or output fixtures. The full TS harness is run
// separately for real boundary observations.
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { stripTypeScriptTypes } from 'node:module';
import { runInNewContext } from 'node:vm';
assert.equal(process.version, 'v22.22.0', '#1368 Node baseline');
assert.equal(process.versions.icu, '78.2', '#1368 ICU baseline');
const source = readFileSync(new URL('../../../../../scripts/a5-19/six_cells.mts', import.meta.url), 'utf8');
const start = 'const cells: ';
const end = '\nasync function measure(';
assert.equal(source.split(start).length, 2, '#1368 preregistration start anchor');
assert.equal(source.split(end).length, 2, '#1368 preregistration end anchor');
const from = source.indexOf(start), to = source.indexOf(end);
assert.ok(from < to, '#1368 preregistration anchor order');
const declarations = stripTypeScriptTypes(source.slice(from, to));
const registration = source.split('\n').filter(line => line.startsWith("writeFileSync(resolve(OUTPUT, 'preregistration.json'),"));
assert.equal(registration.length, 1, '#1368 preregistration publication anchor');
let published;
runInNewContext(declarations + '\n' + registration[0], {
  OUTPUT: 'unused', RULES: {}, resolve: (...parts) => parts.join('/'),
  writeFileSync: (_path, contents) => { published = JSON.parse(contents); },
}, { timeout: 1000 });
assert.ok(published, '#1368 preregistration must be published');
console.log(JSON.stringify({ ...published, node: process.version, icu: process.versions.icu }));
