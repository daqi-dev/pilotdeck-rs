// Shared Node errno-table collection: the single implementation behind both
// generate_node_io.mjs (the recording generator, darwin fixture) and
// fixtures/node-io/linux-errno.mjs-style fresh sampling (linux-errno-verify.mjs),
// so the verifier cannot drift from the generator it is meant to check
// (#133 fix-r1 review: near-identical copies had already diverged in comments).
// Run with the pinned Node: node --expose-internals <caller>.
import {getSystemErrorMap} from 'node:util';
import {constants} from 'node:os';
import {createRequire} from 'node:module';
import {chmodSync,mkdirSync,openSync,readdirSync,statSync} from 'node:fs';
const require=createRequire(import.meta.url);
const {UVException}=require('internal/errors');

// Collect the whole pinned-Node errno oracle: the libuv name/description map,
// `os.constants.errno`, and every UVException formatting row. The version
// gate is the pinned baseline build (v22.22.0 / ICU 78.2 / libuv 1.52.1); a
// mismatch fails rather than silently updating Node data.
export function collectErrnoTables(){
 if(process.version!=='v22.22.0'||process.versions.icu!=='78.2'||process.versions.uv!=='1.52.1')throw Error('Node v22.22.0 / ICU 78.2 / libuv 1.52.1 required');
 const all=[...getSystemErrorMap()].map(([uv,[code,description]])=>({uv,code,description})).sort((a,b)=>a.code.localeCompare(b.code,'en'));
 // These libuv codes have no POSIX errno. Platform-only POSIX names are retained.
 const entries=all.filter(e=>!e.code.startsWith('EAI_')&&!['UNKNOWN','EOF','ECHARSET'].includes(e.code));
 // Real fs errors report through the C++ binding formatter, which appends a
 // provided path verbatim, empty included; the JS `UVException` class used
 // below truthiness-drops an empty path (`if (ctx.path)` — node v22.22.0
 // `lib/internal/errors.js:645`, inside `class UVException`). Measure the
 // binding rule through real fs calls before recording it for the '' rows,
 // so the goldens match what the syscalls actually print.
 for(const [syscall,probe] of [['open',()=>openSync('')],['stat',()=>statSync('')],['scandir',()=>readdirSync('')],['mkdir',()=>mkdirSync('')],['chmod',()=>chmodSync('',0o644)]]){
  try{probe();}catch(error){
   if(error?.code==='ENOENT'&&error.message.endsWith(` ${syscall} ''`))continue;
   throw Error('binding empty-path rule changed: '+error?.message);
  }
  throw Error('empty-path fs call unexpectedly succeeded');
 }
 // An empty pathname only ever yields ENOENT, so the probes above cannot
 // witness another errno carrying an empty path; the '' rows for the other
 // errnos rest on the binding formatter appending a provided path independent
 // of the errno. Node v22.22.0 `src/api/exceptions.cc` `UVException` appends
 // ` 'path'` whenever `path != nullptr` and uses the errno only for the code
 // and description, and `src/node_file.cc` `FSReqAfterScope::Reject` passes
 // `req->path`, non-null whenever the call carried a path, `''` included.
 // Witness that errno independence through real non-ENOENT errors.
 const longPath='/tmp/'+'x'.repeat(300);
 for(const [code,syscall,path,probe] of [['ENOTDIR','scandir','/etc/hosts',()=>readdirSync('/etc/hosts')],['ENOTDIR','stat','/etc/hosts/x',()=>statSync('/etc/hosts/x')],['ENAMETOOLONG','stat',longPath,()=>statSync(longPath)]]){
  try{probe();}catch(error){
   if(error?.code===code&&error.message.endsWith(` ${syscall} '${path}'`))continue;
   throw Error('binding path-append rule changed: '+error?.message);
  }
  throw Error('non-ENOENT fs call unexpectedly succeeded');
 }
 const formatting=[];
 for(const e of entries)for(const syscall of ['stat','open','read','fstat','scandir','realpath'])for(const path of [undefined,'',"/fixture/quote' and\nline"]){
  const error=new UVException({errno:e.uv,syscall,...(path===undefined?{}:{path})});
  const message=path===''?error.message+" ''":error.message;
  formatting.push({code:e.code,syscall,...(path===undefined?{}:{path}),message});
 }
 return {document:{node:process.version,uv:process.versions.uv,platform:process.platform,all,errno:constants.errno,formatting},entries};
}
