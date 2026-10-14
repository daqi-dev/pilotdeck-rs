// Node v22, baseline yaml@2.9.0. Input bytes are shared with the Rust worker.
import { readFileSync } from 'node:fs';
import { pathToFileURL } from 'node:url';
import { resolve, join } from 'node:path';
const root = join(resolve(process.argv[2]), 'node_modules/yaml');
if (Number(process.versions.node.split('.')[0]) !== 22) throw Error('Use Node v22');
if (JSON.parse(readFileSync(join(root, 'package.json'))).version !== '2.9.0') throw Error('Use yaml@2.9.0');
const { parseAllDocuments } = await import(pathToFileURL(join(root, 'dist/index.js')));
const source = readFileSync(process.argv[3], 'utf8');
const start = performance.now();
const docs = parseAllDocuments(source);
const parsed = (performance.now() - start) / 1000;
const values = [];
let conversion_errors = 0;
// stringify-key's warning is informational and would otherwise measure terminal IO.
const warn = process.emitWarning;
process.emitWarning = () => {};
for (const doc of docs) {
  try { values.push(doc.toJSON()); }
  catch (error) { ++conversion_errors; values.push(error); }
}
process.emitWarning = warn;
const seconds = (performance.now() - start) / 1000;
globalThis.retained = { docs, values };
console.log('RESOURCE_SAMPLE ' + JSON.stringify({seconds, parse_seconds: parsed,
  documents: docs.length, errors: docs.reduce((n, d) => n + d.errors.length, 0), conversion_errors}));
