// Only real Node22 Date.parse and the pinned TS TokenStatsCollector produce
// expectations. Run with the baseline tsx loader; output must be new/empty.
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { mkdirSync, readFileSync, readdirSync, writeFileSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { assertBaseline, baselineSourceHashes } from '../_shared/baseline.mts';
assert.equal(process.version, 'v22.22.0');
assert.equal(process.versions.v8, '12.4.254.21-node.33');
assert.ok(process.env.PDRS_BASELINE, 'set PDRS_BASELINE');
assert.ok(process.argv[2], 'provide a new/empty output directory');
const baseline = resolve(process.env.PDRS_BASELINE);
const output = resolve(process.argv[2]);
const sourceCommit = assertBaseline(baseline);
const sourceSha256 = await baselineSourceHashes(baseline, [
  'src/router/stats/TokenStatsCollector.ts', 'src/router/utils/modelPricing.ts',
]);
const { TokenStatsCollector } = await import(pathToFileURL(join(baseline, 'src/router/stats/TokenStatsCollector.ts')).href);
const now = Date.parse('2026-03-08T08:00:00.000Z');
const retentionMs = 3_600_000;
const inputs = new Set<string>();
const add = (s: string) => inputs.add(s);
// Original P0 regressions, canonical and expanded years, ISO/legacy crossover,
// full fractional numeral (not truncated ms) for 24:00, comments, UTF16 and NUL.
const seeds = [
  '', ' ', 'not-a-date', '2026', '2026-03', '2026-03-01',
  '2026-03-01T10:00:00.000Z', '2026-03-01T10:00:00Z',
  '2026-03-01t10:00:00.000z', '2026-03-01 10:00:00.000Z',
  '2026-03-01T18:00:00.000+08:00', '2026-3-1',
  'Sun, 01 Mar 2026 10:00:00 GMT', '2026-02-30T10:00:00.000Z',
  '2026-03-01T24:00:00.000Z', '2026-03-01T10:00:00.0000Z',
  '+000000-01-01T00:00:00.000Z', '-000000-01-01T00:00:00.000Z',
  '010000-01-01T00:00:00.000Z', '002000-01-01T00:00:00Z',
  '2000-01-01T00:00.500Z', '2000-01-01T24:00:00.0001Z',
  '2000-01-01T00:00:00+汉a', '2000-01-01T00:00:00+01:99',
  '2000-01-01T00:00:00+24:00', '2000-01-01T00:00:00+99:99',
  '2026-03-08T02:30:00', '2026-11-01T01:30:00',
  'Jan 1 2000 GMT+596523:14', 'Jan 1 2000 GMT+596523:15',
  'Jan 1 2000 GMT+999999999:59', 'Jan 1 2000 GMT+000000001:01',
  '2026-03-01T10:00:00Z\0\ud800', '(\ud800) Jan 1 2000 GMT',
  'Jan 1 2000 (\udfff) GMT', '\ud800', '\udfff', '😀',
  '2026-03-01T10:00:00Z ', 'Jan 1 2000 (unterminated',
  'Jan 1 2000 (nested (a (b)) text) GMT', 'junkJan1 2000',
  '2000-01-01T24:00:00.0000000001Z', '2000-01-01T24:00:00.000000000000Z',
];
seeds.forEach(add);
for (const year of ['0000', '0001', '0049', '0099', '1600', '1900', '2000', '2024', '2026', '2100', '9999', '+000000', '-000000', '-000001', '+010000', '-010000', '+275760', '-271821', '+999999', '-999999']) {
  for (const md of ['01-01', '02-28', '02-29', '02-30', '04-31', '09-13', '04-20', '00-01', '13-01', '01-00', '01-32']) {
    for (const tail of ['', 'T00:00Z', 'T00:00:00.000Z', 'T24:00:00Z', 'T12:34:56.789+08:00', 'T12:34:56.789']) add(`${year}-${md}${tail}`);
  }
}
for (const hour of ['00', '01', '12', '23', '24', '25', '1']) {
  for (const minute of ['00', '30', '59', '60', '1']) {
    for (const sec of ['', ':00', ':59', ':60', ':00.1', ':00.0001', ':00.123456789012', '.500']) {
      for (const zone of ['Z', 'z', '', '+0800', '-05:30', '+24:00', '+汉a']) add(`2026-03-08T${hour}:${minute}${sec}${zone}`);
    }
  }
}
for (const month of ['Jan', 'FEBRUARY', 'marching', 'Apr', 'May', 'Jun', 'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec', 'xxx']) {
  for (const year of ['0', '1', '49', '50', '99', '100', '2000', '2026']) {
    for (const tail of ['GMT', 'UT', 'UTC', 'Z', 'EST', 'EDT', 'CST', 'CDT', 'MST', 'MDT', 'PST', 'PDT', 'GMT+8', 'GMT-0530', 'GMT+08:30', 'XYZ', '']) {
      add(`Sun, 01 ${month} ${year} 03:04:05 ${tail}`);
    }
    for (const time of ['0:00 AM', '12:00 AM', '12:00 PM', '13:00 PM', '1::02', '1:02.3', '1:02:03.4']) add(`${month} 1 ${year} ${time} GMT`);
  }
}
for (const n of ['0', '1', '12', '13', '31', '32', '49', '50', '99', '100', '999999999', '000000000000000001']) {
  add(n);
  for (const sep of ['/', '-', ' ', ',', '.', ':']) {
    add(`${n}${sep}1${sep}26`); add(`1${sep}${n}${sep}49`); add(`${n}${sep}Jan${sep}2000`);
  }
}
// Both sides of each gap/fold at sub-second precision; historical Shanghai DST,
// pre-standard LMT and post-2038 recurring rules exercise actual system data.
for (const date of ['1883-11-18', '1900-01-01', '1945-08-14', '1986-05-04', '1991-04-14', '1991-09-15', '2024-03-10', '2024-11-03', '2026-03-08', '2026-11-01', '2039-03-13', '2039-11-06']) {
  for (const h of ['00', '01', '02', '03', '04']) {
    for (const ms of ['00:00.000', '00:00.001', '29:59.999', '30:00.123', '59:59.999']) {
      add(`${date}T${h}:${ms}`); add(`${date} ${h}:${ms}`);
    }
  }
}
for (const s of ['+275760-09-12T23:59:59.999Z', '+275760-09-13T00:00:00.000Z', '+275760-09-13T00:00:00.001Z', '-271821-04-19T23:59:59.999Z', '-271821-04-20T00:00:00.000Z', '-271821-04-20T00:00:00.001Z']) {
  add(s); add(s.slice(0, -1)); add(s.slice(0, -1) + '+23:59'); add(s.slice(0, -1) + '-23:59');
}
for (const ws of ['\t', '\n', '\r', '\v', '\f', ' ', '\u00a0', '\u1680', '\u2000', '\u200a', '\u2028', '\u2029', '\u202f', '\u205f', '\u3000', '\ufeff', '\u0085', '\u180e', '\u200b']) {
  add(`${ws}2026-03-01${ws}`); add(`Jan${ws}1${ws}2000${ws}GMT`);
}
// Deterministic grammar mutation and free/random UTF16; no Rust values here.
let state = 0x494;
const random = () => (state = (Math.imul(state, 1664525) + 1013904223) >>> 0);
const alphabet = [...'0123456789TtZz:/+-.(), GMTJanpm', '汉', 'é', '😀', '\0', '\ud800', '\udfff', '\u200b', '\u2028'];
for (let i = 0; i < 2500; i++) {
  const s = seeds[random() % seeds.length];
  const at = random() % (s.length + 1);
  add(s.slice(0, at) + alphabet[random() % alphabet.length] + s.slice(at + (random() % 2)));
  let arbitrary = '';
  const length = random() % 96;
  for (let j = 0; j < length; j++) arbitrary += alphabet[random() % alphabet.length];
  add(arbitrary);
}
// Node's actual UTF8 replacement decoding of malformed journal bytes.
const invalidUtf8 = [[0xff], [0xc0, 0xaf], [0xed, 0xa0, 0x80], [0xf0, 0x80, 0x80, 0x80], [0xe6, 0xb1], [0x80, 0x81], [0xf4, 0x90, 0x80, 0x80]].map(bytes => {
  const raw = Buffer.concat([Buffer.from('2000-01-01T00:00:00+'), Buffer.from(bytes), Buffer.from('a')]);
  const input = raw.toString('utf8'); add(input); return { bytes: [...raw], input };
});
// Bind the local-date admission to the exact system data actually compared
// with the pinned ICU. Include every TZif transition's two local boundaries,
// not just a few current offsets. Never copy a timezone file into the repo.
const profileZones = ['UTC', 'America/New_York', 'Asia/Shanghai'];
const localProfiles = profileZones.map(tz => {
  const bytes = readFileSync(join('/usr/share/zoneinfo', tz));
  assert.equal(bytes.subarray(0, 4).toString(), 'TZif');
  function counts(header: number) { return Array.from({ length: 6 }, (_, i) => bytes.readUInt32BE(header + 20 + i * 4)); }
  function size(header: number, width: number) {
    const [gmt, std, leap, times, types, chars] = counts(header);
    return times * (width + 1) + types * 6 + chars + leap * (width + 4) + std + gmt;
  }
  const version = bytes[4];
  const header = version === 0 ? 0 : 44 + size(0, 4);
  const width = version === 0 ? 4 : 8;
  const [, , , times, types] = counts(header);
  const start = header + 44;
  const typeStart = start + times * (width + 1);
  const offsets = Array.from({ length: types }, (_, i) => bytes.readInt32BE(typeStart + i * 6));
  for (let i = 0; i < times; i++) {
    const time = width === 8 ? Number(bytes.readBigInt64BE(start + i * width)) : bytes.readInt32BE(start + i * width);
    const before = i === 0 ? offsets[0] : offsets[bytes[start + times * width + i - 1]];
    const after = offsets[bytes[start + times * width + i]];
    for (const offset of [before, after]) {
      for (const delta of [-3_600_000, -1, 0, 1, 3_600_000]) {
        const wall = time * 1000 + offset * 1000 + delta;
        if (Math.abs(wall) <= 8_640_000_000_000_000) add(new Date(wall).toISOString().slice(0, -1));
      }
    }
  }
  return { tz, sha256: createHash('sha256').update(bytes).digest('hex') };
});
// Recurring rule tails and dates before the first transition are independently
// observed by Node; this includes the full Gregorian 400-year future cycle.
for (const year of [-271820, -10000, -1, 0, 1, 1600, 1800, 275759, ...Array.from({ length: 400 }, (_, i) => 2038 + i)]) {
  const y = year < 0 ? `-${String(-year).padStart(6, '0')}` : year > 9999 ? `+${String(year).padStart(6, '0')}` : String(year).padStart(4, '0');
  for (const md of ['01-01', '03-08', '03-14', '07-01', '11-01', '11-07', '12-31']) add(`${y}-${md}T01:30:00.123`);
}
const corpus = [...inputs];
const receiver = { retentionMs };
const retained = (input: string, clock = now) => TokenStatsCollector.prototype.isRetained.call(receiver, { endedAt: input }, clock);
if (process.argv[3] === '--child') {
  const cases = corpus.map(input => {
    const epoch = Date.parse(input);
    return [Number.isFinite(epoch) ? epoch : null, retained(input),
      Number.isFinite(epoch) ? [-1, 0, 1].map(delta => retained(input, epoch + retentionMs + delta)) : null];
  });
  const home = join(output, process.env.TZ!.replaceAll('/', '_'));
  mkdirSync(home);
  const filePath = join(home, 'stats.jsonl');
  // A real journal exercises constructor compaction, aggregate rebuild and
  // reopen, including every old accepted-difference example and P0 input.
  const journalInputs = [...seeds, ...corpus.filter(s => /^(1986-05-04|1991-09-15|2026-03-08|2026-11-01)T/.test(s)).slice(-140)];
  const records = journalInputs.map((endedAt, i) => ({
    sessionId: `row-${i}`, startedAt: '2026-03-08T00:00:00.000Z', endedAt,
    usage: { inputTokens: i + 1, outputTokens: 1 }, provider: 'openai', model: 'gpt-4o',
  }));
  const journal = records.map(row => JSON.stringify(row) + '\n').join('');
  const malformedRows = invalidUtf8.map(({ bytes }, i) => Buffer.concat([
    Buffer.from(JSON.stringify({ sessionId: `invalid-utf8-${i}`, startedAt: '2026-03-08T00:00:00.000Z' }).slice(0, -1) + ',"endedAt":"'),
    Buffer.from(bytes), Buffer.from('","usage":{"inputTokens":1,"outputTokens":1},"provider":"openai","model":"gpt-4o"}\n'),
  ]));
  const inputBytes = Buffer.concat([Buffer.from(journal), ...malformedRows]);
  writeFileSync(filePath, inputBytes, { flag: 'wx' });
  const originalNow = Date.now;
  Date.now = () => now;
  let expected: any;
  try {
    const stats = new TokenStatsCollector({ enabled: true, filePath, retentionMs });
    const compacted = readFileSync(filePath, 'utf8');
    const snapshot = stats.snapshot(); stats.dispose();
    const reopened = new TokenStatsCollector({ enabled: true, filePath, retentionMs });
    expected = { input: inputBytes.toString('utf8'), inputBytes: [...inputBytes], compacted, snapshot, reopened: reopened.snapshot() };
    reopened.dispose();
  } finally { Date.now = originalNow; }
  process.stdout.write(JSON.stringify({ tz: process.env.TZ, resolvedTz: Intl.DateTimeFormat().resolvedOptions().timeZone, cases, journal: expected }));
} else {
  mkdirSync(output, { recursive: true });
  assert.equal(readdirSync(output).length, 0, 'output directory must be empty');
  const zones = profileZones.map(tz => {
    const child = spawnSync(process.execPath, [...process.execArgv, import.meta.filename, output, '--child'], {
      env: { ...process.env, TZ: tz }, encoding: 'utf8', maxBuffer: 32 * 1024 * 1024,
    });
    assert.equal(child.status, 0, child.stderr);
    return JSON.parse(child.stdout);
  });
  const result = { sourceCommit, sourceSha256, node: process.version, v8: process.versions.v8,
    icu: process.versions.icu, tzdata: process.versions.tz, localProfiles, now, retentionMs, inputs: corpus, invalidUtf8, zones };
  writeFileSync(join(output, 'oracle.json'), JSON.stringify(result, null, 0) + '\n', { flag: 'wx' });
  writeFileSync(join(output, 'local-profiles.json'), JSON.stringify({ node: process.version, v8: process.versions.v8, icuTzdata: process.versions.tz, profiles: localProfiles }, null, 2) + '\n', { flag: 'wx' });
  console.log(`real TS: ${corpus.length} inputs x ${zones.length} TZ; oracle.json written`);
}
