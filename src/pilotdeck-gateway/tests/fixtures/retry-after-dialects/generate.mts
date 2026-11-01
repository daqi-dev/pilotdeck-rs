// #360-2: real baseline parseRetryAfterHeader over every Date.parse dialect.
// Parent spawns TZ-pinned children so the oracle never depends on the
// generating host's timezone. Output must be new/empty (flag 'wx').
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { mkdirSync, readdirSync, writeFileSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { bindBaseline } from '../_shared/generator_guard.mjs';
import { sourceClosureSha256 } from '../../baseline_source_guard.mjs';

assert.equal(process.version, 'v22.22.0', '#360-2 calibrated Node');
assert.equal(process.versions.v8, '12.4.254.21-node.33', '#360-2 calibrated V8');
assert.ok(process.env.PDRS_BASELINE, '#360-2 set PDRS_BASELINE');
assert.ok(process.argv[2], '#360-2 provide a new/empty output directory');
const base = resolve(process.env.PDRS_BASELINE);
const output = resolve(process.argv[2]);
const { sourceCommit, gitExecutable } = bindBaseline({ baseline: base, commit: 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85' });
const entry = 'src/model/protocol/errors.ts';
const sourceSha256 = sourceClosureSha256(base, sourceCommit, gitExecutable, [entry]);

// JS Number(delta-seconds) wins before Date.parse, including its traps:
// "2015" is a number, not a year; "  " is 0 after trim; "0x10" is 16.
const values = [
  '0', ' 0.0125 ', '1e2', '0x10', '  7  ', '2015', '-30', 'Infinity', 'NaN', '1,000', '  ',
  // IMF-fixdate anchors of the fix-r1/r2 node probes.
  'Wed, 21 Oct 2015 07:28:00 GMT', 'Wed, 21 Oct 2015 07:27:00 GMT',
  'Thu, 01 Jan 1970 00:00:00 GMT', 'Thu, 01 Jan 1970 00:00:01 GMT',
  'Sat, 20 Dec 1969 00:00:00 GMT',
  'Wed, 21 Oct 2015 07:28:61 GMT', 'Wed, 21 Oct 2015 24:00:00 GMT',
  'Wed, 21 Oct 2015 24:00:01 GMT', 'Wed, 21 Oct 2015 25:00:00 GMT',
  'Wed, 21 Oct 2015 07:60:00 GMT', 'Wed, 32 Oct 2015 07:28:00 GMT',
  'Wed, 00 Oct 2015 07:28:00 GMT', 'Wed, 31 Feb 2015 07:28:00 GMT',
  'Wed, 1 Oct 2015 07:28:00 GMT',
  'Fri, 01 Jan 0 00:00:00 GMT', 'Fri, 01 Jan 0000 00:00:00 GMT',
  'Wed, 21 Oct 0099 07:28:00 GMT', 'Fri, 01 Jan 2015 00:00:999999999 GMT',
  'Fri, 01 Jan 2015 999999999:00:00 GMT', 'Fri, 01 Jan 275760 00:00:00 GMT',
  'Fri, 13 Sep 275760 00:00:00 GMT', 'Fri, 14 Sep 275760 00:00:00 GMT',
  'Fri, 01 Jan 999999999 00:00:00 GMT', 'Fri, 01 Jan 292278995 00:00:00 GMT',
  'Wed, 21 Oct 275761 07:28:00 GMT',
  // RFC 850 (two-digit and four-digit years).
  'Wednesday, 21-Oct-15 07:28:00 GMT', 'Sunday, 06-Nov-94 08:49:37 GMT',
  'Wednesday, 21-Oct-2015 07:28:00 GMT',
  // asctime: no timezone, resolves local.
  'Sun Nov  6 08:49:37 1994', 'Wed Oct 21 07:28:00 2015',
  // ISO: date-only is UTC, a time without a zone is local.
  '2015-10-21T07:28:00Z', '2015-10-21T07:28:00+08:00', '2015-10-21T07:28:00',
  '2015-10-21', '9999-12-31T23:59:59Z', '2015-10-21T00:00:00.500Z',
  // V8-lenient forms: weekday optional, named zones, numeric offsets.
  '21 Oct 2015 07:28:00 GMT', 'Wed, 21 Oct 2015 07:28:00 EST',
  'Wed, 21 Oct 2015 07:28:00 UT', 'Wed, 21 Oct 2015 02:28:00 -0500',
  'Oct 21 2015 07:28:00 GMT', '21-Oct-15 07:28:00 GMT',
  '2015/10/21 07:28:00 GMT', 'Oct 21, 2015 07:28:00 GMT',
  'Wed, 21 Oct 15 07:28:00 GMT', 'Wed Oct 21 2015 07:28:00 GMT',
  'Wed, 21 Oct 2015 07:28:00 GMT+05:30',
  // Unparseable by both Number and Date.parse.
  'not a date', '', 'GMT', 'Wed, 99 Dec 99999999999 07:28:00 GMT',
];

// Only wall times inside Shanghai's flat +08:00 eras resolve local, so a fixed
// -8h replay resolver stays faithful to the pinned ICU zone.
const zones = ['UTC', 'Asia/Shanghai'];

if (process.argv[3] === '--child') {
  const { parseRetryAfterHeader } = await import(pathToFileURL(join(base, entry)).href);
  const realNow = Date.now;
  Date.now = () => 0;
  let cases;
  try {
    cases = values.map(value => ({ value, retryAfterMs: parseRetryAfterHeader(value) ?? null }));
  } finally {
    Date.now = realNow;
  }
  process.stdout.write(JSON.stringify({
    tz: process.env.TZ,
    resolvedTz: Intl.DateTimeFormat().resolvedOptions().timeZone,
    cases,
  }));
} else {
  mkdirSync(output, { recursive: true });
  assert.equal(readdirSync(output).length, 0, '#360-2 output directory must be empty');
  const observed = zones.map(tz => {
    const child = spawnSync(process.execPath, [...process.execArgv, import.meta.filename, output, '--child'], {
      env: { ...process.env, TZ: tz }, encoding: 'utf8', maxBuffer: 8 * 1024 * 1024,
    });
    assert.equal(child.status, 0, `#360-2 ${tz} child failed: ${child.stderr}`);
    return JSON.parse(child.stdout);
  });
  writeFileSync(join(output, 'oracle.json'), JSON.stringify({
    sourceCommit, sourceSha256, node: process.version, v8: process.versions.v8,
    icu: process.versions.icu, tzdata: process.versions.tz, now: 0, zones: observed,
  }, null, 1) + '\n', { flag: 'wx' });
  console.log(`real TS: ${values.length} values x ${zones.length} TZ; oracle.json written`);
}
