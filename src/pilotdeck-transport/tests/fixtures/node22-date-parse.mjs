// Regenerate with Node v22.22.0:
// node src/pilotdeck-transport/tests/fixtures/node22-date-parse.mjs > /tmp/node22-date-parse.json
// Compare/review the result before replacing the checked-in fixture.
import { spawnSync } from 'node:child_process';

if (process.version !== 'v22.22.0') throw new Error(`Expected v22.22.0, got ${process.version}`);
const inputs = [
  ['iso-millis', '2026-10-14T05:01:18.204Z'],
  ['iso-offset-colon', '2024-01-02T03:04:05.678+08:00'],
  ['iso-offset-compact', '2024-01-02T03:04:05.678-0530'],
  ['iso-fraction-truncation', '2024-01-02T03:04:05.123456Z'],
  ['iso-next-day', '2024-01-02T24:00:00Z'],
  ['iso-normalized-day', '2024-02-30T00:00:00Z'],
  ['iso-extended-year', '+010000-01-01T00:00:00.000Z'],
  ['iso-negative-year', '-000001-01-01T00:00:00.000Z'],
  ['iso-timeclip-end', '+275760-09-13T00:00:00.000Z'],
  ['iso-date-only', '2024-01-02'],
  ['iso-local', '2024-01-02T03:04:05.678'],
  ['iso-local-gap', '2024-03-10T02:30:00.123'],
  ['iso-local-fold', '2024-11-03T01:30:00.123'],
  ['iso-local-fold-end', '2024-11-03T02:00:00'],
  ['iso-local-future-gap', '2039-03-13T02:30:00'],
  ['legacy-rfc-gmt', 'Thu, 01 Jan 1970 00:00:00 GMT'],
  ['legacy-slashes', '01/02/2024'],
  ['legacy-am-pm', 'Jan 2 2024 3:04 PM GMT'],
  ['legacy-named-zone', 'Jan 2 2024 03:04:05 PST'],
  ['legacy-two-digit-year', '01/02/49 GMT'],
  ['legacy-single-number', '0'],
  ['legacy-width', '2024-1-2'],
  ['legacy-whitespace', ' 2024-01-02 '],
  ['legacy-comments', 'Jan 2 2024 (comment) GMT'],
  ['legacy-unknown-prefix', 'junk (nested (comment)) 1 Jan 1970 GMT'],
  ['legacy-smi-boundary', 'Jan 1 2000 GMT+596523:14'],
  ['nul-terminates', '2024-01-02T00:00:00Z\u0000ignored'],
  ['invalid-empty', ''],
  ['invalid-text', 'not a date'],
  ['invalid-month', '2024-13-01T00:00:00Z'],
  ['invalid-day', '2024-01-32T00:00:00Z'],
  ['invalid-leap-second', '2024-01-02T03:04:60Z'],
  ['invalid-offset', '2024-01-02T00:00:00+24:00'],
  ['invalid-iso-trailing-space', '2024-01-02T00:00:00Z '],
  ['invalid-ampm', 'Jan 2 2024 13:00 PM GMT'],
  ['invalid-zone', 'Jan 2 2024 00:00 XYZ'],
  ['invalid-slashes', '13/32/2024'],
  ['invalid-timeclip', '+275760-09-13T00:00:00.001Z'],
  ['invalid-surrogate', '\ud800'],
];

if (process.argv[2] === '--child') {
  const cases = inputs.map(([id, input]) => {
    const epoch = Date.parse(input);
    return { id, input, epoch: Number.isNaN(epoch) ? null : epoch,
      iso: Number.isNaN(epoch) ? null : new Date(epoch).toISOString() };
  });
  process.stdout.write(JSON.stringify({ tz: process.env.TZ, cases }));
} else {
  const zones = ['UTC', 'Asia/Shanghai', 'America/Los_Angeles'].map(tz => {
    const result = spawnSync(process.execPath, [import.meta.filename, '--child'], {
      encoding: 'utf8', env: { ...process.env, TZ: tz },
    });
    if (result.status !== 0) throw new Error(result.stderr);
    return JSON.parse(result.stdout);
  });
  process.stdout.write(JSON.stringify({ node: process.version, v8: process.versions.v8,
    icu: process.versions.icu, tzdata: process.versions.tz, zones }, null, 2) + '\n');
}
