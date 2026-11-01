// #1324 A8.3-CRON-TZ-ALIAS-001 evidence script (moved in-repo from the
// round's _tmp directory so the diff entry's references persist; the
// original run was cb8, Node v22.22.0 / ICU 78.2, tzdata 2025b).
// Node side: dump local parts for every recorded ICU zone x probe instants.
import { readFileSync, writeFileSync } from 'node:fs';
const zones = JSON.parse(
  readFileSync(
    new URL('../../../../src/config/timezones.json', import.meta.url),
    'utf8',
  ),
);
const instants = [
  Date.UTC(2026, 0, 15),
  Date.UTC(2026, 2, 8, 6, 30),
  Date.UTC(2026, 3, 5, 12),
  Date.UTC(2026, 6, 15),
  Date.UTC(2026, 9, 4, 1),
  Date.UTC(2026, 10, 1, 5),
  Date.UTC(2027, 2, 14, 0, 30),
  Date.UTC(2030, 5, 1),
];
const WEEK = { Sun: 0, Mon: 1, Tue: 2, Wed: 3, Thu: 4, Fri: 5, Sat: 6 };
const rows = [];
for (const z of zones) {
  for (const t of instants) {
    const f = new Intl.DateTimeFormat('en-US-u-ca-gregory-nu-latn', {
      timeZone: z,
      hourCycle: 'h23',
      month: '2-digit',
      day: '2-digit',
      hour: '2-digit',
      minute: '2-digit',
      weekday: 'short',
    });
    const v = {};
    for (const p of f.formatToParts(new Date(t))) if (p.type !== 'literal') v[p.type] = p.value;
    rows.push([z, t, v.month, v.day, v.hour, v.minute, WEEK[v.weekday]].join('\t'));
  }
}
writeFileSync('/tmp/tz_node_parts.tsv', rows.join('\n') + '\n');
console.log('node rows:', rows.length, 'node', process.version,
  'icu', process.versions.icu, 'hostzone', Intl.DateTimeFormat().resolvedOptions().timeZone);
