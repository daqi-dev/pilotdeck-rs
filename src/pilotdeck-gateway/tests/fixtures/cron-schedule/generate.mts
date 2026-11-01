// #1324 A8.3-pr1b: record the real TS cron schedule computation behavior.
//
// Loads the baseline sources (CronSchedule.ts + CronTimezone.ts) and drives
// computeNextRunAt / cronDayFieldsUseOr / isValidCronTimezone /
// resolveCronTimezone / delayToMilliseconds over a fixed matrix, recording
// every return value (and the RangeError the minute scan throws on an
// out-of-range candidate) as oracle.json. Nothing here reads the wall clock,
// so no clock pinning is needed; every `after` is an explicit epoch, and
// every `runAt` string is timezone-independent (Z- or offset-suffixed,
// date-only, or invalid), so neither this run nor the Rust replay depends on
// the process timezone.
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { readFile, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { assertBaseline, baselineSourceHashes } from '../_shared/baseline.mts';

assert.ok(process.env.PDRS_BASELINE); assert.ok(process.argv[2]);
const baseline = resolve(process.env.PDRS_BASELINE);
const sourceCommit = assertBaseline(baseline);
const sources = ['src/cron/runtime/CronSchedule.ts', 'src/cron/CronTimezone.ts'];
const sourceSha256 = await baselineSourceHashes(baseline, sources);

const req = createRequire(join(baseline, 'package.json'));
const { transformSync } = createRequire(req.resolve('tsx'))('esbuild');
async function load(source: string, imports: Record<string, any> = {}) {
  const code = transformSync(await readFile(join(baseline, source), 'utf8'),
    { loader: 'ts', format: 'cjs', target: 'es2022', sourcefile: source }).code;
  const module = { exports: {} as any };
  new Function('require', 'module', 'exports', code)((name: string) => {
    if (name in imports) return imports[name];
    if (name.startsWith('node:')) return req(name);
    return new Proxy({}, { get(_t, p) { if (p === '__esModule') return true;
      throw new Error(`unexercised import ${source} -> ${name}.${String(p)}`); } });
  }, module, module.exports);
  return module.exports;
}
const timezoneMod = await load(sources[1]);
const scheduleMod = await load(sources[0], { '../CronTimezone.js': timezoneMod });
const { computeNextRunAt, cronDayFieldsUseOr, delayToMilliseconds,
  CRON_SCHEDULE_COMPUTATION_VERSION } = scheduleMod;
const { isValidCronTimezone, resolveCronTimezone } = timezoneMod;

const out = resolve(process.argv[2]);
assert.ok(!out.startsWith(baseline + '/'), 'never write into the baseline');

// -- A. computeNextRunAt -------------------------------------------------------
// Cell fields: `once` (runAt string), `delay` [amount, unit], `cron`
// expression + optional `tz`, `after` epoch ms (null = NaN Invalid Date),
// `fallback` (cron only). Recorded result: null = undefined, "invalid" =
// Invalid Date (delay path beyond TimeClip), number = the Date's getTime();
// `error` carries the RangeError the scan throws. Strings that carry lone
// surrogates are recorded as JSON escapes (well-formed stringify) and the
// Rust replay revives them through its JS-faithful JSON parser.
interface NextRunCell {
  name: string;
  once?: string;
  delay?: [number, string];
  cron?: string;
  tz?: string;
  after?: number | null;
  fallback?: string;
}
const nextRunCells: NextRunCell[] = [
  // -- once: the runtime date parser (V8), through computeNextRunAt -------
  { name: 'once/iso-z', once: '2026-03-05T09:30:00.000Z' },
  { name: 'once/iso-z-no-ms', once: '2026-03-05T09:30:00Z' },
  { name: 'once/time-clip-max', once: '+275760-09-13T00:00:00.000Z' },
  { name: 'once/beyond-time-clip', once: '+275760-09-14T00:00:00.000Z' },
  { name: 'once/time-clip-min', once: '-271821-04-20T00:00:00.000Z' },
  { name: 'once/one-ms-below-max', once: '+275760-09-12T23:59:59.999Z' },
  { name: 'once/fractional-ms-truncates', once: '1970-01-01T00:00:00.0005Z' },
  { name: 'once/explicit-plus-8-offset', once: '2026-03-05T09:30:00+08:00' },
  { name: 'once/explicit-zero-offset', once: '2026-03-05T09:30:00.000+00:00' },
  { name: 'once/date-only', once: '2026-03-05' },
  { name: 'once/year-month-only', once: '2026-03' },
  { name: 'once/feb-29-nonleap-rolls', once: '2026-02-29T09:30:00Z' },
  { name: 'once/apr-31-rolls', once: '2026-04-31T12:00:00Z' },
  { name: 'once/day-32-rejected', once: '2026-01-32T00:00:00Z' },
  { name: 'once/day-0-rejected', once: '2026-03-00T00:00:00Z' },
  { name: 'once/month-13-rejected', once: '2026-13-01T00:00:00Z' },
  { name: 'once/month-0-rejected', once: '2026-00-01T00:00:00Z' },
  { name: 'once/hour-24-rolls-to-next-day', once: '1970-01-01T24:00:00Z' },
  { name: 'once/minute-60-rejected', once: '1970-01-01T00:60:00Z' },
  { name: 'once/second-60-rejected', once: '1970-01-01T00:00:60Z' },
  { name: 'once/extended-negative-year', once: '-000001-01-01T00:00:00.000Z' },
  { name: 'once/year-zero', once: '0000-01-01T00:00:00Z' },
  { name: 'once/not-a-date', once: 'not a date' },
  { name: 'once/empty-string', once: '' },
  { name: 'once/padded-valid', once: ' 2026-03-05T09:30:00.000Z ' },
  { name: 'once/lone-surrogate', once: '\u{D800}' },
  // -- delay: TimeClip on after.getTime() + delayMs ------------------------
  { name: 'delay/one-second-from-epoch', delay: [1, 'second'], after: 0 },
  { name: 'delay/half-minute', delay: [0.5, 'minute'], after: 1735689600000 },
  { name: 'delay/negative-after', delay: [1.5, 'hour'], after: -86_400_000 },
  { name: 'delay/one-day-overflows-clip', delay: [1, 'day'], after: 8_640_000_000_000_000 },
  { name: 'delay/one-second-at-clip', delay: [1, 'second'], after: 8_640_000_000_000_000 },
  { name: 'delay/zero-amount', delay: [0, 'second'], after: 0 },
  { name: 'delay/negative-amount', delay: [-5, 'minute'], after: 0 },
  { name: 'delay/quarter-day', delay: [0.25, 'day'], after: 1735689600000 },
  { name: 'delay/huge-amount-overflows', delay: [1e300, 'second'], after: 0 },
  { name: 'delay/sub-millisecond-truncates', delay: [1e-7, 'second'], after: 1735689600000 },
  { name: 'delay/nan-after', delay: [1, 'minute'], after: null },
  { name: 'delay/nan-after-zero-amount', delay: [0, 'day'], after: null },
  { name: 'delay/two-and-a-half-days', delay: [2.5, 'day'], after: 1735689600000 },
  { name: 'delay/pre-epoch', delay: [1, 'second'], after: -1 },
  { name: 'delay/sub-ms-negative-after', delay: [0.0005, 'second'], after: -1 },
  { name: 'delay/one-millisecond', delay: [0.001, 'second'], after: 0 },
  // -- cron: alignment, day-field semantics --------------------------------
  { name: 'cron/daily-9am-before', cron: '0 9 * * *', tz: 'UTC', after: Date.UTC(2026, 0, 15, 8, 59, 0, 1) },
  { name: 'cron/daily-9am-after', cron: '0 9 * * *', tz: 'UTC', after: Date.UTC(2026, 0, 15, 9, 0, 0, 1) },
  { name: 'cron/daily-9am-exact', cron: '0 9 * * *', tz: 'UTC', after: Date.UTC(2026, 0, 15, 9) },
  { name: 'cron/every-15-minutes', cron: '*/15 * * * *', tz: 'UTC', after: Date.UTC(2026, 0, 1, 0, 0, 0, 30) },
  { name: 'cron/every-15-exact', cron: '*/15 * * * *', tz: 'UTC', after: Date.UTC(2026, 0, 1, 0, 45, 0, 1) },
  { name: 'cron/minute-list-hour-step', cron: '5,35 */2 * * *', tz: 'UTC', after: Date.UTC(2026, 0, 1, 3, 10) },
  { name: 'cron/dow-7-is-sunday', cron: '0 12 * * 7', tz: 'UTC', after: Date.UTC(2026, 0, 16) },
  { name: 'cron/dow-0-and-7-normalize', cron: '0 12 * * 0,7', tz: 'UTC', after: Date.UTC(2026, 0, 17) },
  { name: 'cron/and-weekly-friday', cron: '0 12 * * 5', tz: 'UTC', after: Date.UTC(2026, 0, 14) },
  { name: 'cron/or-13th-first', cron: '0 12 13 * 5', tz: 'UTC', after: Date.UTC(2026, 0, 10) },
  { name: 'cron/or-next-friday', cron: '0 12 13 * 5', tz: 'UTC', after: Date.UTC(2026, 1, 14) },
  { name: 'cron/or-dom-or-monday', cron: '0 12 1 * 1', tz: 'UTC', after: Date.UTC(2026, 0, 2) },
  { name: 'cron/dow-range', cron: '0 12 * * 1-3', tz: 'UTC', after: Date.UTC(2026, 0, 14, 13) },
  { name: 'cron/hour-range-step', cron: '0 8-18/4 * * *', tz: 'UTC', after: Date.UTC(2026, 0, 1) },
  { name: 'cron/last-day-of-month', cron: '0 0 31 * *', tz: 'UTC', after: Date.UTC(2026, 0, 1) },
  { name: 'cron/month-list', cron: '0 0 1 3,6 *', tz: 'UTC', after: Date.UTC(2026, 0, 1) },
  { name: 'cron/dom-29-nonfeb', cron: '0 0 29 * *', tz: 'UTC', after: Date.UTC(2026, 0, 1) },
  // -- cron: DST transitions ------------------------------------------------
  { name: 'cron/spring-forward-gap-skips', cron: '30 2 * * *', tz: 'America/New_York', after: Date.UTC(2026, 2, 8, 6, 30) },
  { name: 'cron/spring-forward-hour-skips', cron: '0 2 * * *', tz: 'America/New_York', after: Date.UTC(2026, 2, 8, 6) },
  { name: 'cron/fall-back-first-occurrence', cron: '0 1 * * *', tz: 'America/New_York', after: Date.UTC(2026, 10, 1, 4, 30) },
  { name: 'cron/fall-back-second-occurrence', cron: '0 1 * * *', tz: 'America/New_York', after: Date.UTC(2026, 10, 1, 5, 30) },
  { name: 'cron/berlin-spring-forward', cron: '0 3 * * *', tz: 'Europe/Berlin', after: Date.UTC(2026, 2, 29, 1, 30) },
  { name: 'cron/berlin-fall-back-first', cron: '0 3 * * *', tz: 'Europe/Berlin', after: Date.UTC(2026, 9, 25, 0, 30) },
  { name: 'cron/berlin-fall-back-second', cron: '0 3 * * *', tz: 'Europe/Berlin', after: Date.UTC(2026, 9, 25, 1, 30) },
  { name: 'cron/lord-howe-dst-end', cron: '0 2 * * *', tz: 'Australia/Lord_Howe', after: Date.UTC(2026, 3, 4) },
  { name: 'cron/chatham-quarter-offset', cron: '0 0 * * *', tz: 'Pacific/Chatham', after: Date.UTC(2026, 5, 1, 12) },
  { name: 'cron/sao-paulo-no-dst', cron: '0 9 * * *', tz: 'America/Sao_Paulo', after: Date.UTC(2026, 0, 15, 11, 30) },
  { name: 'cron/kiritimati-skipped-day', cron: '0 12 31 12 *', tz: 'Pacific/Kiritimati', after: Date.UTC(1994, 11, 30) },
  { name: 'cron/ny-pre-1970', cron: '0 0 1 1 *', tz: 'America/New_York', after: Date.UTC(1938, 5, 1) },
  { name: 'cron/gap-minute-fixed-offset', cron: '30 2 * * *', tz: '+03:00', after: Date.UTC(2026, 2, 8, 22, 30, 0, 1) },
  // -- cron: timezone resolution ---------------------------------------------
  { name: 'cron/ist-9am-same-day', cron: '0 9 * * *', tz: 'Asia/Kolkata', after: Date.UTC(2026, 0, 15, 3, 0, 0, 1) },
  { name: 'cron/ist-9am-next-day', cron: '0 9 * * *', tz: 'Asia/Kolkata', after: Date.UTC(2026, 0, 15, 3, 30, 0, 1) },
  { name: 'cron/offset-identifier', cron: '0 9 * * *', tz: '+05:30', after: Date.UTC(2026, 0, 15, 3, 31) },
  { name: 'cron/offset-hour-only', cron: '0 9 * * *', tz: '+05', after: Date.UTC(2026, 0, 15, 3, 31) },
  { name: 'cron/etc-gmt-plus-5', cron: '0 9 * * *', tz: 'Etc/GMT+5', after: Date.UTC(2026, 0, 15, 13, 30) },
  { name: 'cron/lowercase-zone', cron: '0 9 * * *', tz: 'america/new_york', after: Date.UTC(2026, 0, 15, 13, 30) },
  { name: 'cron/fallback-timezone-used', cron: '0 9 * * *', after: Date.UTC(2026, 0, 15, 13, 30), fallback: 'America/New_York' },
  { name: 'cron/schedule-timezone-wins', cron: '0 9 * * *', tz: 'Asia/Tokyo', after: Date.UTC(2026, 0, 15, 0, 0, 0, 1), fallback: 'America/New_York' },
  { name: 'cron/empty-explicit-timezone', cron: '0 9 * * *', tz: '', after: Date.UTC(2026, 0, 15, 13, 30), fallback: 'America/New_York' },
  { name: 'cron/surrogate-timezone', cron: '0 9 * * *', tz: '\u{D800}', after: Date.UTC(2026, 0, 15, 13, 30) },
  // -- cron: leap-day shortcut -----------------------------------------------
  { name: 'cron/leap-day-2028', cron: '0 0 29 2 *', tz: 'UTC', after: Date.UTC(2027, 11, 1) },
  { name: 'cron/leap-day-late-window', cron: '0 0 29 2 *', tz: 'UTC', after: Date.UTC(2028, 2, 1) },
  { name: 'cron/leap-day-ny-midnight', cron: '0 0 29 2 *', tz: 'America/New_York', after: Date.UTC(2028, 1, 28, 20, 0, 0, 1) },
  { name: 'cron/leap-day-kiritimati', cron: '0 0 29 2 *', tz: 'Pacific/Kiritimati', after: Date.UTC(2028, 1, 29) },
  { name: 'cron/leap-day-dow-breaks-shortcut', cron: '0 0 29 2 1', tz: 'UTC', after: Date.UTC(2026, 0, 10) },
  { name: 'cron/leap-day-after-year-0050', cron: '0 0 29 2 *', tz: 'UTC', after: Date.parse('0050-01-01T00:00:00.000Z') },
  { name: 'cron/leap-day-after-year-0000', cron: '0 0 29 2 *', tz: 'UTC', after: Date.parse('0000-01-01T00:00:00.000Z') },
  { name: 'cron/leap-day-after-year-1999', cron: '0 0 29 2 *', tz: 'UTC', after: Date.UTC(1999, 5, 1) },
  { name: 'cron/leap-day-nan-after', cron: '0 0 29 2 *', tz: 'UTC', after: null },
  // -- cron: scan edges (RangeError / exhausted search) ----------------------
  { name: 'cron/nan-after-throws', cron: '0 9 * * *', tz: 'UTC', after: null },
  { name: 'cron/after-clip-throws', cron: '0 9 * * *', tz: 'UTC', after: 8_640_000_000_000_000 },
  { name: 'cron/scan-past-clip-throws', cron: '0 9 * * *', tz: 'UTC', after: 8_640_000_000_000_000 - 1 },
  { name: 'cron/never-matches', cron: '0 0 30 2 *', tz: 'UTC', after: Date.UTC(2026, 0, 1) },
  { name: 'cron/negative-epoch-alignment', cron: '0 0 * * *', tz: 'UTC', after: -60_001 },
  { name: 'cron/pre-epoch-exact', cron: '0 0 * * *', tz: 'UTC', after: -1 },
  // -- cron: expression parsing ----------------------------------------------
  { name: 'cron/whitespace-u2028', cron: '0 9 * * * *', tz: 'UTC', after: Date.UTC(2026, 0, 15, 8, 59, 0, 1) },
  { name: 'cron/tabs-as-separators', cron: '0\t9\t*\t*\t*', tz: 'UTC', after: Date.UTC(2026, 0, 15, 8, 59, 0, 1) },
  { name: 'cron/trimmed-expression', cron: '  0 9 * * *  ', tz: 'UTC', after: Date.UTC(2026, 0, 15, 8, 59, 0, 1) },
  { name: 'cron/nbsp-inside-field', cron: ' 5 * * * *', tz: 'UTC', after: Date.UTC(2026, 0, 1, 0, 0, 30) },
  { name: 'cron/surrogate-expression-invalid', cron: '\u{D800} * * * *', tz: 'UTC', after: Date.UTC(2026, 0, 1) },
  { name: 'cron/minute-out-of-range', cron: '60 * * * *', tz: 'UTC', after: Date.UTC(2026, 0, 1) },
  { name: 'cron/hour-out-of-range', cron: '* 24 * * *', tz: 'UTC', after: Date.UTC(2026, 0, 1) },
  { name: 'cron/dom-out-of-range', cron: '* * 32 * *', tz: 'UTC', after: Date.UTC(2026, 0, 1) },
  { name: 'cron/month-out-of-range', cron: '* * * 13 *', tz: 'UTC', after: Date.UTC(2026, 0, 1) },
  { name: 'cron/dow-out-of-range', cron: '* * * * 8', tz: 'UTC', after: Date.UTC(2026, 0, 1) },
  { name: 'cron/zero-step', cron: '*/0 * * * *', tz: 'UTC', after: Date.UTC(2026, 0, 1) },
  { name: 'cron/infinite-step-rejected', cron: `*/${'9'.repeat(400)} * * * *`, tz: 'UTC', after: Date.UTC(2026, 0, 1) },
  { name: 'cron/infinite-step-dow-rejected', cron: `* * * * */${'9'.repeat(400)}`, tz: 'UTC', after: Date.UTC(2026, 0, 1) },
  { name: 'cron/huge-finite-step', cron: `*/${'9'.repeat(308)} * * * *`, tz: 'UTC', after: Date.UTC(2026, 0, 1) },
  { name: 'cron/dot-leading-step-rejected', cron: '*/.5 * * * *', tz: 'UTC', after: Date.UTC(2026, 0, 1) },
  { name: 'cron/too-many-fields', cron: '* * * * * *', tz: 'UTC', after: Date.UTC(2026, 0, 1) },
  { name: 'cron/too-few-fields', cron: '* * * *', tz: 'UTC', after: Date.UTC(2026, 0, 1) },
  { name: 'cron/empty-expression', cron: '', tz: 'UTC', after: Date.UTC(2026, 0, 1) },
  { name: 'cron/whitespace-only-expression', cron: '   ', tz: 'UTC', after: Date.UTC(2026, 0, 1) },
  { name: 'cron/not-cron', cron: 'not cron', tz: 'UTC', after: Date.UTC(2026, 0, 1) },
  { name: 'cron/range-extra-piece-dropped', cron: '1-5-3 * * * *', tz: 'UTC', after: Date.UTC(2026, 0, 1, 0, 0, 0, 1) },
  { name: 'cron/parse-int-junk-ignored', cron: '5x * * * *', tz: 'UTC', after: Date.UTC(2026, 0, 1, 0, 0, 0, 1) },
  { name: 'cron/negative-value-rejected', cron: '-5 * * * *', tz: 'UTC', after: Date.UTC(2026, 0, 1) },
  { name: 'cron/dot-in-month-field', cron: '* * * 1.5 *', tz: 'UTC', after: Date.UTC(2026, 0, 15, 3, 4, 5) },
  { name: 'cron/decimal-step-parses', cron: '*/2.5 * * * *', tz: 'UTC', after: Date.UTC(2026, 0, 1, 0, 1, 0, 1) },
  { name: 'cron/trailing-comma-rejected', cron: '5, * * * *', tz: 'UTC', after: Date.UTC(2026, 0, 1) },
  { name: 'cron/three-slash-parts-rejected', cron: '1/2/3 * * * *', tz: 'UTC', after: Date.UTC(2026, 0, 1) },
  { name: 'cron/leading-plus-allowed', cron: '+5 * * * *', tz: 'UTC', after: Date.UTC(2026, 0, 1, 0, 0, 30) },
  { name: 'cron/leading-dash-minute-rejected', cron: '-0 * * * *', tz: 'UTC', after: Date.UTC(2026, 0, 1, 0, 0, 0, 1) },
];

function buildSchedule(cell: NextRunCell): any {
  if (cell.once !== undefined) return { type: 'once', runAt: cell.once };
  if (cell.delay !== undefined) return { type: 'delay', amount: cell.delay[0], unit: cell.delay[1] };
  return { type: 'cron', expression: cell.cron,
    ...(cell.tz === undefined ? {} : { timezone: cell.tz }) };
}

const nextRuns = nextRunCells.map((cell) => {
  const schedule = buildSchedule(cell);
  const after = cell.after === undefined ? 0 : cell.after;
  let result: any = null;
  let error: string | null = null;
  try {
    const date = computeNextRunAt(schedule, after === null ? new Date(NaN) : new Date(after), cell.fallback);
    result = date === undefined ? null : (Number.isNaN(date.getTime()) ? 'invalid' : date.getTime());
  } catch (caught) {
    error = (caught as any).constructor.name + ': ' + (caught as any).message;
  }
  return {
    name: cell.name, schedule,
    afterMs: after,
    fallback: cell.fallback ?? 'UTC',
    result, error,
  };
});

// -- B. cronDayFieldsUseOr -----------------------------------------------------
const dayOrCells: { name: string, expression: string }[] = [
  { name: 'star-both', expression: '0 0 * * *' },
  { name: 'dom-and-dow-lists', expression: '0 0 1 1 1' },
  { name: 'dom-star-prefixed-step', expression: '0 0 */2 * *' },
  { name: 'dow-star-prefixed-step', expression: '0 0 1 * */2' },
  { name: 'dom-star-only', expression: '0 0 * 1 1' },
  { name: 'dow-range', expression: '0 0 1 1 1-5' },
  { name: 'leap-day', expression: '0 0 29 2 *' },
  { name: 'leap-day-with-dow', expression: '0 0 29 2 1' },
  { name: 'dow-7-only', expression: '0 0 * * 7' },
  { name: 'range-in-dom', expression: '0 0 1-5 * 1' },
  { name: 'unparsable', expression: 'not cron' },
  { name: 'empty', expression: '' },
  { name: 'too-many-fields', expression: '* * * * * *' },
];
const dayOr = dayOrCells.map((cell) => ({
  name: cell.name, expression: cell.expression,
  expect: cronDayFieldsUseOr(cell.expression),
}));

// -- C. isValidCronTimezone ----------------------------------------------------
const validTimezoneCells = [
  'UTC', 'utc', 'GMT', 'Etc/UTC', 'Etc/GMT+5', 'Etc/GMT-14', 'Etc/GMT+99',
  'America/New_York', 'America/new_york', 'america/new_york', 'America/New York',
  'US/Pacific', 'EST', 'est', 'Asia/Calcutta', 'Asia/Kolkata', 'Asia/Shanghai',
  'Asia/Tokyo', 'Europe/Berlin', 'Australia/Lord_Howe', 'Pacific/Chatham',
  'Pacific/Kiritimati', 'America/Sao_Paulo', 'america//new_york',
  '+05:30', '+0530', '-00:00', '+05:30:00', '+5:30', '05:30', '+99:00', '+24:00', '+00:60',
  '', ' ', 'Not/AZone', 'Z', '+05', '+00',
];
const slug = (text: string) => text.replace(/[^A-Za-z0-9]+/g, '-').replace(/^-+|-+$/g, '') || 'blank';
const validTimezones = validTimezoneCells.map((timezone, index) => ({
  name: String(index).padStart(2, '0') + '-' + slug(timezone),
  timezone, expect: isValidCronTimezone(timezone),
}));

// -- D. resolveCronTimezone -----------------------------------------------------
const resolveTimezoneCells: { name: string, args: (string | undefined)[] }[] = [
  { name: 'all-undefined', args: [undefined, undefined, undefined] },
  { name: 'schedule-valid', args: ['America/New_York', undefined, undefined] },
  { name: 'schedule-empty-falls-to-task', args: ['', 'Europe/Berlin', 'Asia/Shanghai'] },
  { name: 'schedule-invalid-falls-to-task', args: ['Not/AZone', 'Europe/Berlin', undefined] },
  { name: 'task-invalid-falls-to-config', args: ['Not/AZone', 'Nope', 'Asia/Shanghai'] },
  { name: 'all-invalid', args: ['x', 'y', 'z'] },
  { name: 'all-empty', args: ['', '', ''] },
  { name: 'alias-zone', args: ['US/Pacific', undefined, undefined] },
  { name: 'lowercase-zone', args: ['utc', undefined, undefined] },
  { name: 'offset-identifier', args: ['+05:30', 'UTC', 'UTC'] },
  { name: 'task-only', args: [undefined, 'Asia/Tokyo', undefined] },
  { name: 'config-only', args: [undefined, undefined, 'Europe/Berlin'] },
];
const resolveTimezones = resolveTimezoneCells.map((cell) => ({
  name: cell.name,
  scheduleTimezone: cell.args[0] ?? null,
  taskTimezone: cell.args[1] ?? null,
  configTimezone: cell.args[2] ?? null,
  expect: resolveCronTimezone(cell.args[0], cell.args[1], cell.args[2]),
}));

// -- E. delayToMilliseconds ------------------------------------------------------
const delayCells: { name: string, amount: number, unit: 'second' | 'minute' | 'hour' | 'day' }[] = [
  { name: 'one-second', amount: 1, unit: 'second' },
  { name: 'half-minute', amount: 0.5, unit: 'minute' },
  { name: 'one-hour', amount: 1, unit: 'hour' },
  { name: 'one-day', amount: 1, unit: 'day' },
  { name: 'zero', amount: 0, unit: 'second' },
  { name: 'negative', amount: -5, unit: 'minute' },
  { name: 'fractional-second', amount: 0.25, unit: 'second' },
  { name: 'huge-but-finite', amount: 1e300, unit: 'second' },
  { name: 'sub-millisecond', amount: 1e-7, unit: 'second' },
  { name: 'tiny-day', amount: 0.001, unit: 'day' },
  { name: 'two-and-a-half-days', amount: 2.5, unit: 'day' },
  { name: 'five-minutes', amount: 5, unit: 'minute' },
];
const delayMs = delayCells.map((cell) => ({
  name: cell.name, amount: cell.amount, unit: cell.unit,
  expect: delayToMilliseconds(cell.amount, cell.unit) ?? null,
}));

// -- publish --------------------------------------------------------------------

// The matrix shape is frozen: the Rust replay asserts the same five counts
// before replaying, so adding or removing a cell must update both sides.
for (const [label, actual, expected] of [
  ['nextRuns', nextRuns.length, 126],
  ['dayOr', dayOr.length, 13],
  ['validTimezones', validTimezones.length, 39],
  ['resolveTimezones', resolveTimezones.length, 12],
  ['delayMs', delayMs.length, 12],
] as const) {
  assert.equal(actual, expected, `${label} cell count drifted`);
}

const oracle = {
  sourceCommit, sourceSha256, node: process.version, icu: process.versions.icu,
  computationVersion: CRON_SCHEDULE_COMPUTATION_VERSION,
  nextRuns, dayOr, validTimezones, resolveTimezones, delayMs,
};
await writeFile(join(out, 'oracle.json'), JSON.stringify(oracle, null, 2) + '\n', { flag: 'wx' });
console.log(`recorded ${nextRuns.length} next-run cells, ${dayOr.length} day-or cells, ` +
  `${validTimezones.length} timezone-validity cells, ${resolveTimezones.length} resolve cells, ` +
  `${delayMs.length} delay cells`);
