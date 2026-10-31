// #117 Linux-side fresh measurement of Node's errno tables, for the gated test
// linux_node_errno_numbers_match_node_os_constants. The table construction is
// the very code the recording generator uses (shared node-io-tables.mjs), so
// the verifier cannot drift from what it verifies; this script adds nothing
// but the argv output path and writes nothing inside the repository.
//
// Run with the pinned Node: node --expose-internals \
//   tests/fixtures/node-io/linux-errno-verify.mjs <output.json>
import {renameSync,writeFileSync} from 'node:fs';
import {collectErrnoTables} from '../../node-io-tables.mjs';
const {document,entries}=collectErrnoTables();
const output=process.argv[2];
if(!output)throw Error('usage: linux-errno-verify.mjs <output.json>');
// Atomic publish for the Rust reader (#166 r8: never let it see a partial file).
const temporary=`${output}.tmp`;
writeFileSync(temporary,JSON.stringify(document,null,2)+'\n');
renameSync(temporary,output);
console.log(JSON.stringify({node:document.node,uv:document.uv,platform:document.platform,libuvEntries:document.all.length,posixNames:entries.length,formatting:document.formatting.length,errnoConstants:Object.keys(document.errno).length,output}));
