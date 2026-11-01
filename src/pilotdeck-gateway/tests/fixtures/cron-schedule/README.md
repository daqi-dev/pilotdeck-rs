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
| `nextRuns` | 117 | `computeNextRunAt` for all three schedule types: V8 date-parser edges (TimeClip bounds, day/hour rollover, fractional-ms truncation, surrogates), delay TimeClip semantics (Invalid Date beyond range, NaN `after`), minute alignment (incl. pre-epoch), day-field OR vs AND, DST gap/fold instants (New York, Berlin, Lord Howe, Chatham), fixed-offset and case-insensitive zones, `timezone ?? fallback` precedence, the leap-day shortcut (incl. a dow field that breaks it and +14 local-midnight alignment), the `RangeError: Invalid time value` thrown when the scan leaves the Date range, the exhausted 366-day search, and the full parseField acceptance table (ranges, steps, `parseInt` junk/sign/decimal, `-` splitting, whitespace set) |
| `dayOr` | 13 | `cronDayFieldsUseOr` incl. `*/n` star-prefix AND rule |
| `validTimezones` | 37 | `isValidCronTimezone` acceptance (recorded ICU list + UTC-offset identifiers; validity is host-independent) |
| `resolveTimezones` | 12 | `resolveCronTimezone` precedence (schedule > task > config > UTC, empty vs invalid) |
| `delayMs` | 12 | `delayToMilliseconds` (units, fractions, zero/negative) |

`nextRuns[].result`: `null` = TS `undefined`, `"invalid"` = an Invalid Date
(delay path beyond TimeClip), number = `getTime()`. `afterMs: null` = the
NaN of an Invalid `after` Date. `error` carries the RangeError the minute
scan throws.

## Deliberate divergence (not in this oracle)

An ICU-valid *legacy alias* zone that the host zoneinfo tree lacks (e.g.
`US/Pacific` on cb8's split tzdata install) resolves fail-closed to
`undefined` in the Rust port instead of computing a run — Node has the alias
rule compiled into its ICU data. Validity/resolution cells above only
exercise the recorded-list acceptance (host-independent), so no cell depends
on host tzdata. Registered in `specs/gateway-parity/diffs/`; zone-rule
equivalence itself was probed separately (decision card
`state/decisions-20261102-issue1324-cron-tz.md`).
