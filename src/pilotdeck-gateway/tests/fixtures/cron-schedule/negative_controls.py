#!/usr/bin/env python3
"""#1324 HEAD-only controls: compile, fail at the bound assertion, restore.

Usage: negative_controls.py --check
       negative_controls.py [--compile-only] NEW_OUT [mutation ...]
Commit the tests and this script first. CARGO_TARGET_DIR is caller-owned.
Every selected row is reported, including anchor/build/source-guard failures.

Rows bind one mutation to one named cell replay in
`runtime::cron::schedule_tests` (a lib test, run with `--exact`); the kill
evidence is that test's own panic payload carrying its `#1324` message. Nine
rows pin the port's load-bearing behaviors: the version-3 day-field OR rule,
the dow 7→0 normalization, the floor (not trunc) minute alignment, the
leap-day shortcut, the schedule-timezone `??` precedence, the recorded-ICU
timezone acceptance, the delay unit table, NaN `parseInt` rejection, and the
UTC-offset-identifier resolution through the shared transport resolver.

All bound replays are host-tzdata-independent (they pin UTC, IANA, or offset
identifiers), so every row runs on both Linux and macOS.
"""
from pathlib import Path
import json
import os
import subprocess
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot
from negative_suite import Case, classify, passed

root = Path(__file__).resolve().parents[5]
source = root / 'src/pilotdeck-gateway/src/runtime/cron/schedule.rs'
clock = root / 'src/pilotdeck-transport/src/system_clock.rs'
tests = root / 'src/pilotdeck-gateway/src/runtime/cron/schedule_tests.rs'
NEXT_RUN = '#1324 {cell}: computed next run differs from the TS oracle'
mutations = [
    # The version-3 day-field OR rule reverts to Unix AND semantics, so
    # `0 12 13 * 5` stops matching the 13th-or-Friday disjunction.
    ('day-or-to-and', source,
     '        day_of_month_matches || day_of_week_matches',
     '        day_of_month_matches && day_of_week_matches',
     'runtime::cron::schedule_tests::cron_or_13th_first',
     NEXT_RUN.format(cell='cron/or-13th-first')),
    # `7` stops normalizing to Sunday (0): a dow-7-only mask never matches a
    # 0..6 weekday, so the scan exhausts instead of finding Sunday.
    ('dow-7-not-normalized', source,
     '    let days_of_week = if days_of_week & (1 << 7) != 0 {\n'
     '        (days_of_week | 1) & !(1 << 7)\n'
     '    } else {\n'
     '        days_of_week\n'
     '    };',
     '    let days_of_week = days_of_week;',
     'runtime::cron::schedule_tests::cron_dow_7_is_sunday',
     NEXT_RUN.format(cell='cron/dow-7-is-sunday')),
    # The scan start aligns with truncation toward zero instead of floor, so
    # a pre-epoch `after` of -1ms starts at +1min and misses epoch midnight.
    ('minute-align-trunc', source,
     '    let candidate = (after_epoch_ms / MINUTE_MS).floor() * MINUTE_MS + MINUTE_MS;',
     '    let candidate = (after_epoch_ms / MINUTE_MS).trunc() * MINUTE_MS + MINUTE_MS;',
     'runtime::cron::schedule_tests::cron_pre_epoch_exact',
     NEXT_RUN.format(cell='cron/pre-epoch-exact')),
    # The leap-day shortcut never fires (`0 0 29 2 *` keeps the full dow
    # mask, so `!=` excludes it): the 366-day scan cannot reach a leap day
    # more than a year out.
    ('leap-day-shortcut-off', source,
     '        && cron.days_of_week == 0b0111_1111',
     '        && cron.days_of_week != 0b0111_1111',
     'runtime::cron::schedule_tests::cron_leap_day_late_window',
     NEXT_RUN.format(cell='cron/leap-day-late-window')),
    # `schedule.timezone ?? fallback` becomes an always-fallback: the
    # schedule's Asia/Tokyo run is computed in America/New_York instead.
    ('schedule-timezone-precedence-ignored', source,
     '            let resolved = timezone.as_ref().and_then(|tz| tz.to_utf8().ok());\n'
     '            if resolved.is_none() && timezone.is_some() {\n'
     '                return Ok(ComputedRunAt::Undefined);\n'
     '            }\n'
     '            let timezone = resolved.as_deref().unwrap_or(fallback_timezone);',
     '            let timezone = fallback_timezone;',
     'runtime::cron::schedule_tests::cron_schedule_timezone_wins',
     NEXT_RUN.format(cell='cron/schedule-timezone-wins')),
    # The recorded ICU list acceptance is dropped: every string (including
    # `Not/AZone`) becomes a valid cron timezone.
    ('valid-timezone-accepts-all', source,
     'pub fn is_valid_cron_timezone(timezone: &str) -> bool {\n'
     '    crate::config::sections::valid_timezone(timezone)\n'
     '}',
     'pub fn is_valid_cron_timezone(_timezone: &str) -> bool {\n'
     '    true\n'
     '}',
     'runtime::cron::schedule_tests::tz_35_not_azone',
     '#1324 timezone 35-Not-AZone: acceptance differs from the TS oracle'),
    # The delay minute is 61 seconds: 0.5 minute stops being 30,000ms.
    ('delay-minute-ms-wrong', source,
     '            Self::Minute => MINUTE_MS,',
     '            Self::Minute => 61_000.0,',
     'runtime::cron::schedule_tests::delay_ms_half_minute',
     '#1324 delay half-minute: delay in milliseconds differs from the TS oracle'),
    # A NaN `Number.parseInt` becomes 0 instead of rejecting the field, so
    # the leading-dash minute `-0` parses as minute 0 and computes a run.
    ('parse-int-nan-becomes-zero', source,
     '    if digits == 0 {\n'
     '        return None;\n'
     '    }',
     '    if digits == 0 {\n'
     '        return Some(0.0);\n'
     '    }',
     'runtime::cron::schedule_tests::cron_leading_dash_minute_rejected',
     NEXT_RUN.format(cell='cron/leading-dash-minute-rejected')),
    # The shared transport resolver rejects ICU UTC-offset identifiers, so
    # `+05:30` resolves fail-closed instead of computing the 9am run.
    ('offset-identifier-zone-unresolved', clock,
     '            Some(offset) => Ok(Self::FixedOffset(offset)),',
     '            Some(_offset) => Err(JsString::from("Invalid time zone specified")),',
     'runtime::cron::schedule_tests::cron_offset_identifier',
     NEXT_RUN.format(cell='cron/offset-identifier')),
]


def check_anchor(row, original):
    if original.count(row[2]) != 1 or row[2] == row[3]:
        raise ValueError(f'{row[0]}: stale or unchanged anchor (count={original.count(row[2])})')


def main():
    args = sys.argv[1:]
    if args == ['--check']:
        errors = []
        for row in mutations:
            try:
                check_anchor(row, row[1].read_text())
            except Exception as error:
                errors.append(str(error))
        print('\n'.join(errors) if errors else f'#1324 {len(mutations)} unique anchors checked')
        return int(bool(errors))
    compile_only = '--compile-only' in args
    args = [arg for arg in args if arg != '--compile-only']
    if not args:
        raise SystemExit('provide NEW_OUT and optional mutation names, or --check')
    names = args[1:]
    selected = [row for row in mutations if not names or row[0] in names]
    if not selected or (names and len(selected) != len(names)):
        raise SystemExit('unknown or duplicate mutation')
    env = dict(os.environ)
    if not env.get('CARGO_TARGET_DIR'):
        raise SystemExit('CARGO_TARGET_DIR must be supplied by the caller')
    out = Path(args[0]).resolve()
    out.mkdir(parents=True, exist_ok=False)
    summary = []
    guarded = list(dict.fromkeys([source, clock, tests, Path(__file__).resolve(),
        Path(__file__).resolve().parents[1] / 'negative_process.py',
        Path(__file__).resolve().parents[1] / 'negative_suite.py']))

    def save(name, text):
        with (out / name).open('x') as stream:
            stream.write(text)

    def run(label, test, *, compile=False):
        slot = wait_for_slot(root / 'scripts/cargo-slots.sh', timeout=600)
        save(label + '.slots.log', slot.stdout + slot.stderr)
        # negative_suite needs libtest's named result lines, including positives.
        command = ['cargo', 'test', '--offline', '-p', 'pilotdeck-gateway', '--lib']
        if compile:
            command.append('--no-run')
        else:
            command += [test, '--', '--exact', '--show-output']
        save(label + '.command.json', json.dumps(command) + '\n')
        try:
            result = run_owned(command, cwd=root / 'src', env=env, timeout=300)
        except Exception as error:
            raw = ''
            if isinstance(error, subprocess.TimeoutExpired):
                raw = ''.join(value.decode(errors='replace') if isinstance(value, bytes)
                              else value or '' for value in (error.stdout, error.stderr))
            save(label + '.log', raw + f'\n{type(error).__name__}: {error}\n')
            raise
        save(label + '.log', result.stdout + result.stderr)
        return result

    for row in selected:
        name, path, old, new, test, assertion = row
        item = dict(name=name, test=test, assertion=assertion,
                    status='INVALID', reason='not executed', compiled=False)
        try:
            # No caller-supplied baseline, dirty-file snapshot, or alternate git.
            with mutating_sources(root, guarded) as originals:
                original = originals[path].decode()
                check_anchor(row, original)
                if not compile_only and not passed(run(name + '-positive', test), test):
                    raise RuntimeError('bound positive control did not pass')
                try:
                    path.write_text(original.replace(old, new, 1))
                    if run(name + '-compile', test, compile=True).returncode != 0:
                        raise RuntimeError('mutant failed compilation; not a runtime kill')
                    item['compiled'] = True
                    if compile_only:
                        item.update(status='COMPILED', reason='cargo test --no-run passed')
                    else:
                        case = Case(name, path, old, new, test, assertion)
                        status, reason = classify(run(name + '-negative', test), case)
                        item.update(status=status, reason=reason)
                finally:
                    # Bytes come only from the HEAD-validated guard; it also
                    # restores and verifies every guarded source on exit.
                    path.write_bytes(originals[path])
                if not compile_only and not passed(run(name + '-restored', test), test):
                    raise RuntimeError('bound restored control did not pass')
        except Exception as error:
            item.update(status='INVALID', reason=f'{type(error).__name__}: {error}')
        summary.append(item)
        print(f"{name}: {item['status']}: {item['reason']}", flush=True)
    save('summary.json', json.dumps(summary, indent=2) + '\n')
    counts = {state: sum(row['status'] == state for row in summary)
              for state in ('KILLED', 'SURVIVED', 'INVALID', 'COMPILED')}
    print('summary: ' + ', '.join(f'{key}={value}' for key, value in counts.items()), flush=True)
    # A run that killed nothing proved nothing: with zero KILLED rows the exit
    # stays nonzero so a caller cannot read an empty selection as verified
    # (same rule as exact-rerun-receipt).
    success = 'COMPILED' if compile_only else 'KILLED'
    return int(not summary
               or not counts[success]
               or any(row['status'] != success for row in summary))


if __name__ == '__main__':
    raise SystemExit(main())
