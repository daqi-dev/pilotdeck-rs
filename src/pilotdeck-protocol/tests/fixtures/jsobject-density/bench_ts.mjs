#!/usr/bin/env node
// #1128 TS-side workload runner: JSON.parse + JSON.stringify(value, null, 2),
// the PilotDeck product's pretty-output shape in Node. Driven by measure.py
// through the same OBJECT_DENSITY_* environment variables as the Rust
// harness; prints one #1128-TS JSON line. Peak RSS is taken by the caller
// (wait4 rusage), matching the Rust side. Drop cost is GC and is not timed.
//
// V8's indented JSON.stringify is recursive: at the issue's 10^4-level deep
// spine it throws RangeError: Maximum call stack size exceeded on the default
// stack (measured on v22.22.0). That is a real baseline property, not a
// harness failure, so the error is reported in the JSON line instead of
// aborting the run; the parse timings stay comparable.
import { readFileSync, writeFileSync, renameSync } from 'node:fs';

const input = process.env.OBJECT_DENSITY_INPUT;
const label = process.env.OBJECT_DENSITY_LABEL || 'ts';
const reps = Number(process.env.OBJECT_DENSITY_REPS ?? 7);
const warmup = Number(process.env.OBJECT_DENSITY_WARMUP ?? 1);
if (!input) {
  console.error('#1128 OBJECT_DENSITY_INPUT is required');
  process.exit(2);
}
const text = readFileSync(input, 'utf8');

function atomicWrite(path, bytes) {
  const temporary = path.replace(/(\.[^.]*)?$/, '.tmp');
  writeFileSync(temporary, bytes);
  renameSync(temporary, path);
}

function pass(dump) {
  const t0 = performance.now();
  const value = JSON.parse(text);
  const t1 = performance.now();
  let pretty = null;
  try {
    pretty = JSON.stringify(value, null, 2);
  } catch (error) {
    return { parseMs: t1 - t0, stringifyMs: null, prettyBytes: null,
             stringifyError: String(error) };
  }
  const t2 = performance.now();
  if (dump) {
    atomicWrite(dump, pretty);
  }
  return { parseMs: t1 - t0, stringifyMs: t2 - t1, prettyBytes: pretty.length,
           stringifyError: null };
}

for (let index = 0; index < warmup; index += 1) {
  pass(null);
}
const parseMs = [];
const stringifyMs = [];
let prettyBytes = null;
let stringifyError = null;
for (let index = 0; index < reps; index += 1) {
  const result = pass(index === 0 ? process.env.OBJECT_DENSITY_DUMP_PRETTY : null);
  parseMs.push(result.parseMs);
  if (result.stringifyError) {
    stringifyError = result.stringifyError;
    break;
  }
  stringifyMs.push(result.stringifyMs);
  prettyBytes = result.prettyBytes;
}
console.log(`#1128-TS ${JSON.stringify({
  kind: 'file', label, inputBytes: text.length, reps: parseMs.length,
  parseMs, stringifyMs, prettyBytes, stringifyError,
})}`);
