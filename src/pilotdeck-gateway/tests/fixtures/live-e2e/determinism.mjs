// Process-local clock/random hooks only for the fixed TS fixture; timers stay real.
import assert from 'node:assert/strict';
assert.equal(process.version, 'v22.22.0', '#969 determinism: requires Node v22.22.0');
const NativeDate = Date;
const epoch = NativeDate.parse('2026-10-26T00:00:00.000Z');
class FixtureDate extends NativeDate {
  constructor(...args) { super(...(args.length ? args : [epoch])); }
  static now() { return epoch; }
}
globalThis.Date = FixtureDate;
let seed = 969;
Math.random = () => ((seed = (Math.imul(seed, 1664525) + 1013904223) >>> 0) / 4294967296);

// ESM named imports must observe the same deterministic UUID implementation.
import crypto from 'node:crypto';
import { syncBuiltinESMExports } from 'node:module';
let uuid = 0;
crypto.randomUUID = () => `00000000-0000-4000-8000-${(++uuid).toString().padStart(12, '0')}`;
syncBuiltinESMExports();
