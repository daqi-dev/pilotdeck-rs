// #117 Linux-side fresh measurement of Node's errno tables, for the gated test
// linux_node_errno_numbers_match_node_os_constants. Same pinned Node and same
// table sources as tests/generate_node_io.mjs, but writes nothing inside the
// repository: output goes to the argv path, published atomically.
//
// Run with the pinned Node: node --expose-internals \
//   tests/fixtures/node-io/linux-errno-verify.mjs <output.json>
import {getSystemErrorMap} from 'node:util';
import {constants} from 'node:os';
import {createRequire} from 'node:module';
import {chmodSync, mkdirSync, openSync, readdirSync, renameSync, statSync, writeFileSync} from 'node:fs';
const require = createRequire(import.meta.url);
const {UVException} = require('internal/errors');
if (process.version !== 'v22.22.0' || process.versions.uv !== '1.52.1')
  throw Error('Node v22.22.0 / libuv 1.52.1 required');
const all = [...getSystemErrorMap()].map(([uv, [code, description]]) => ({uv, code, description})).sort((a, b) => a.code.localeCompare(b.code, 'en'));
// These libuv codes have no POSIX errno. Platform-only POSIX names are retained.
const entries = all.filter(e => !e.code.startsWith('EAI_') && !['UNKNOWN', 'EOF', 'ECHARSET'].includes(e.code));
// Real fs errors report through the C++ binding formatter, which appends a
// provided path verbatim, empty included; the JS `UVException` class used
// below truthiness-drops an empty path. Witness both rules through real fs
// calls, so the '' rows below match what the syscalls actually print.
for (const [syscall, probe] of [['open', () => openSync('')], ['stat', () => statSync('')], ['scandir', () => readdirSync('')], ['mkdir', () => mkdirSync('')], ['chmod', () => chmodSync('', 0o644)]]) {
  try {probe();} catch (error) {
    if (error?.code === 'ENOENT' && error.message.endsWith(` ${syscall} ''`)) continue;
    throw Error('binding empty-path rule changed: ' + error?.message);
  }
  throw Error('empty-path fs call unexpectedly succeeded');
}
const longPath = '/tmp/' + 'x'.repeat(300);
for (const [code, syscall, path, probe] of [['ENOTDIR', 'scandir', '/etc/hosts', () => readdirSync('/etc/hosts')], ['ENOTDIR', 'stat', '/etc/hosts/x', () => statSync('/etc/hosts/x')], ['ENAMETOOLONG', 'stat', longPath, () => statSync(longPath)]]) {
  try {probe();} catch (error) {
    if (error?.code === code && error.message.endsWith(` ${syscall} '${path}'`)) continue;
    throw Error('binding path-append rule changed: ' + error?.message);
  }
  throw Error('non-ENOENT fs call unexpectedly succeeded');
}
const formatting = [];
for (const e of entries) for (const syscall of ['stat', 'open', 'read', 'fstat', 'scandir', 'realpath']) for (const path of [undefined, '', "/fixture/quote' and\nline"]) {
  const error = new UVException({errno: e.uv, syscall, ...(path === undefined ? {} : {path})});
  const message = path === '' ? error.message + " ''" : error.message;
  formatting.push({code: e.code, syscall, ...(path === undefined ? {} : {path}), message});
}
const document = {node: process.version, uv: process.versions.uv, platform: process.platform, all, errno: constants.errno, formatting};
const output = process.argv[2];
if (!output) throw Error('usage: linux-errno-verify.mjs <output.json>');
// Atomic publish for the Rust reader (#166 r8: never let it see a partial file).
const temporary = `${output}.tmp`;
writeFileSync(temporary, JSON.stringify(document, null, 2) + '\n');
renameSync(temporary, output);
console.log(JSON.stringify({node: process.version, uv: process.versions.uv, platform: process.platform, libuvEntries: all.length, posixNames: entries.length, formatting: formatting.length, errnoConstants: Object.keys(constants.errno).length, output}));
