// Reopen actual Rust-written files through the guarded, unmodified TS store.
import assert from 'node:assert/strict';
import {execFileSync} from 'node:child_process';
import {readFileSync,realpathSync} from 'node:fs';
import {resolve,join,dirname} from 'node:path';
import {fileURLToPath,pathToFileURL} from 'node:url';
import {bindSourceTree,sourceClosureSha256} from '../../baseline_source_guard.mjs';
import {freezeClock,clean,readBack,tree} from './observe.mts';
assert.ok(process.env.PDRS_BASELINE);assert.ok(process.argv[2]);
const baseline=resolve(process.env.PDRS_BASELINE!);const root=resolve(process.argv[2]!);
const oracle=JSON.parse(readFileSync(join(dirname(fileURLToPath(import.meta.url)),'oracle.json'),'utf8'));
const git=realpathSync(process.env.PDRS_GIT||execFileSync('which',['git'],{encoding:'utf8'}).trim());
bindSourceTree(baseline,oracle.sourceCommit,git);
assert.deepEqual(sourceClosureSha256(baseline,oracle.sourceCommit,git,['src/context/memory/edgeclaw-memory-core/src/core/file-memory.ts','src/context/memory/edgeclaw-memory-core/src/core/storage/sqlite.ts']),oracle.sourceSha256);
freezeClock();const {FileMemoryStore}=await import(pathToFileURL(join(baseline,'src/context/memory/edgeclaw-memory-core/src/core/file-memory.ts')).href);
for(const scenario of oracle.scenarios){const dir=join(root,scenario.name);const store=new FileMemoryStore(dir,scenario.options);assert.deepEqual(clean(readBack(store),dir),scenario.readback,`${scenario.name} TS readback of Rust files`);assert.deepEqual(tree(dir),scenario.files,`${scenario.name} disk bytes after TS reopen`);}
console.log(`real TS reopened ${oracle.scenarios.length} Rust trees; all APIs and bytes match`);
