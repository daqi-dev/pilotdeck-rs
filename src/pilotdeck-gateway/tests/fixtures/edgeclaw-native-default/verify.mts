import assert from 'node:assert/strict';
import {readFileSync,writeFileSync} from 'node:fs';
import {dirname,join,resolve} from 'node:path';
import {fileURLToPath} from 'node:url';
import {bindBaseline} from '../_shared/generator_guard.mjs';
import {sourceClosureSha256} from '../../baseline_source_guard.mjs';
import {SOURCES,collect} from './observe.mts';
assert.equal(process.version,'v22.22.0','#778 exact Node baseline');
assert.ok(process.env.PDRS_BASELINE,'#778 baseline required');
assert.ok(process.argv[2],'#778 Rust observations required');
const baseline=resolve(process.env.PDRS_BASELINE),oracle=JSON.parse(readFileSync(join(dirname(fileURLToPath(import.meta.url)),'oracle.json'),'utf8'));
const {gitExecutable}=bindBaseline({baseline,commit:oracle.sourceCommit});
assert.deepEqual(sourceClosureSha256(baseline,oracle.sourceCommit,gitExecutable,SOURCES),oracle.sourceSha256,'#778 source closure');
if(process.argv[2]==='--read-root'){
  assert.ok(process.argv[3]&&process.argv[4],'#778 --read-root ROOT OUTPUT required');
  const result:any=await collect(baseline,{readRoot:resolve(process.argv[3])});
  assert.deepEqual(result.before,oracle.endToEnd.persisted.database,'#778 Rust-written database before TS constructor');
  assert.deepEqual(result.persisted,oracle.endToEnd.persisted,'#778 real TS reopened Rust SQLite and complete memory tree');
  assert.deepEqual(result.readback,oracle.endToEnd.readback,'#778 real TS reads Rust memory records');
  assert.equal(result.closeCount,1,'#778 cross-language close once');
  writeFileSync(resolve(process.argv[4]),JSON.stringify(result,null,2)+'\n',{flag:'wx'});
  console.log(`Node ${process.version}; real TS reopened persisted database and tree, recall hit; close once`);
}else{
  const fresh=await collect(baseline),rust=JSON.parse(readFileSync(process.argv[2],'utf8'));
  assert.deepEqual(fresh,{scenarios:oracle.scenarios,configurations:oracle.configurations,endToEnd:oracle.endToEnd,maxLines:oracle.maxLines},'#778 fresh TS matrix');
  assert.deepEqual(rust,fresh,'#778 Rust versus real TS factory and M21');
  console.log(`Node ${process.version}; ${fresh.scenarios.length} M21 + ${fresh.configurations.length} config cells + E2E match Rust`);
}
