# cron-schedule oracle

Golden replay of the #1324 cron schedule computation against the TS baseline
(`src/cron/runtime/CronSchedule.ts` + `src/cron/CronTimezone.ts`, commit
cc7c4e6, Node v22.22.0 / ICU 78.2). The Rust replay is
`src/runtime/cron/schedule_tests.rs` (one named test per cell); the
integration guard in `tests/cron_schedule.rs` re-runs the generator and
requires byte-identical output.

## Regenerate

```
PDRS_BASELINE=<PilotDeck checkout> \
  node --disable-warning=ExperimentalWarning \
  --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  tests/fixtures/cron-schedule/generate.mts <fresh-output-dir>
```

No clock pinning: every `after` is an explicit epoch, and every `runAt`
string is timezone-independent (Z-/offset-suffixed, date-only, or invalid),
so neither the generator run nor the Rust replay depends on the process
timezone or the wall clock.

## Sections

| section | cells | covers |
|---|---|---|
| `nextRuns` | 128 | `computeNextRunAt` for all three schedule types: V8 date-parser edges (TimeClip bounds, day/hour rollover, fractional-ms truncation, surrogates), delay TimeClip semantics (Invalid Date beyond range, NaN `after`, sub-ms delay over a pre-epoch `after` pinning TimeClip's `-0`→`+0`), minute alignment (incl. pre-epoch), day-field OR vs AND, DST gap/fold instants (New York, Berlin, Lord Howe, Chatham), fixed-offset and case-insensitive zones plus the hour-only `+HH` identifier form, `timezone ?? fallback` precedence, the leap-day shortcut (incl. a dow field that breaks it, +14 local-midnight alignment, and `Date.UTC`'s 0–99→1900–1999 year mapping under two-digit `after` years), the `RangeError: Invalid time value` thrown when the scan leaves the Date range, the exhausted 366-day search, the exact `MAX_SEARCH_MINUTES` bound (a run landing on the very last scanned minute is found), the leap-day shortcut's `!dayFieldsUseOr` gate (a full-mask dow written as `0-6` keeps OR semantics and must **not** take the shortcut), and the full parseField acceptance table (ranges, steps — Infinity (`!Number.isInteger`) and `.5`-leading steps rejected, a 308-nine finite step accepted — `parseInt` junk/sign/decimal, `-` splitting, whitespace set) |
| `dayOr` | 13 | `cronDayFieldsUseOr` incl. `*/n` star-prefix AND rule |
| `validTimezones` | 39 | `isValidCronTimezone` acceptance (recorded ICU list + UTC-offset identifiers incl. `+05`/`+00`; validity is host-independent) |
| `resolveTimezones` | 12 | `resolveCronTimezone` precedence (schedule > task > config > UTC, empty vs invalid) |
| `delayMs` | 12 | `delayToMilliseconds` (units, fractions, zero/negative) |

`computationVersion` records the baseline `CRON_SCHEDULE_COMPUTATION_VERSION` the
generator was run against; the Rust replay asserts it matches the ported constant.

`nextRuns[].result`: `null` = TS `undefined`, `"invalid"` = an Invalid Date
(delay path beyond TimeClip), number = `getTime()`. `afterMs: null` = the
NaN of an Invalid `after` Date. `error` carries the RangeError the minute
scan throws.

## Deliberate divergences (not in this oracle)

An ICU-valid *legacy alias* zone that the host zoneinfo tree lacks (e.g.
`US/Pacific` on cb8's split tzdata install) resolves fail-closed to
`undefined` in the Rust port instead of computing a run — Node has the alias
rule compiled into its ICU data. Validity/resolution cells above only
exercise the recorded-list acceptance (host-independent), so no cell depends
on host tzdata. Registered in `specs/gateway-parity/diffs/`; zone-rule
equivalence itself was probed separately — probe scripts and the raw
cb8 evidence are kept in-repo under `tz-probe/` (decision card
`state/decisions-20261102-issue1324-cron-tz.md`).

Secondly, a local-wall `runAt` (no timezone suffix) whose *process* timezone
fails to resolve on the host also fails closed to `undefined` (TS's Node
parser always has ICU-local built in); the golden once cells never use
local-wall strings, so no cell depends on the process timezone. Registered
as a separate entry in `specs/gateway-parity/diffs/`.

## Zone-rule probe evidence

`tz-probe/` holds both halves of the zone-rule equivalence probe behind the
entry above. The Node side (`tz_node_parts.mjs`) dumps local parts for every
recorded ICU zone × 8 probe instants and **requires** an output path,
creating it exclusively and writing a `<out>.tsv.sha256` sidecar; the Python
side (`tz_zoneinfo_parts.py`) **requires** the TSV path, verifies its sha256
(second argument, else the sidecar) and the full zone × instant grid, and only
then compares against the host TZif tree.

The recorded output of the original cb8 run is committed as
`tz-probe/tz_node_parts.tsv` (4776 rows, sha256
`df9fb65a77f20abeb7829b3f14e73f34de03c2d15cfcee9eae45c6b7260a3f3d`, sidecar
alongside), so the Node half never has to be re-run to check the Python half.
That Python half is host-dependent by construction — it reads
`/usr/share/zoneinfo`, which is exactly what is being probed — so it is
evidence, not a test: re-running it on a host whose tzdata differs may report
`mismatches`, and the committed TSV is what makes the comparison reproducible
in the first place.

## Negative controls

`negative_controls.py` mutates one anchored line of `schedule.rs` /
`system_clock.rs` at a time, re-runs the single bound named cell replay, and
requires the mutant to be killed by that test's own `#1324` panic message.
All bound replays are host-tzdata-independent, so every row runs on both
Linux and macOS:

```
python3 -I src/pilotdeck-gateway/tests/fixtures/cron-schedule/negative_controls.py --check
CARGO_TARGET_DIR=/absolute/build/dir python3 -I src/pilotdeck-gateway/tests/fixtures/cron-schedule/negative_controls.py /tmp/fresh-cron-schedule-negative [mutation ...]
```

`--check` verifies every anchor matches HEAD exactly once, and that the
hand-copied numbers still describe the artifacts they name: each README
section count equals its `oracle.json` length, the Rows list below equals this
file's `mutations` in order, every oracle cell has a bound named replay in
`schedule_tests.rs`, and every row's bound test is actually defined there.
The full run
writes per-mutant logs plus `summary.json` into `OUT` and exits nonzero
unless every row is KILLED. Rows: `day-or-to-and`, `dow-7-not-normalized`,
`minute-align-trunc`, `search-bound-narrowed`, `leap-day-shortcut-off`,
`leap-day-or-gate-dropped`, `schedule-timezone-precedence-ignored`,
`valid-timezone-accepts-all`, `delay-minute-ms-wrong`, `parse-int-nan-becomes-zero`,
`offset-identifier-zone-unresolved`, `infinite-step-accepted`,
`leap-day-year-mapping-dropped`, `once-local-wall-guessed-utc`,
`zone-resolver-failure-returns-run`, `offset-hour-only-branch-dropped`,
`negative-zero-not-normalized`.
