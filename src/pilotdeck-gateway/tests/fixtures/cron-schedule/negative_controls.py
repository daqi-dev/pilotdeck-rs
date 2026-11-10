#!/usr/bin/env python3
"""#1324 HEAD-only controls: compile, fail at the bound assertion, restore.

Usage: negative_controls.py --check
       negative_controls.py [--compile-only] NEW_OUT [mutation ...]
Commit the tests and this script first. CARGO_TARGET_DIR is caller-owned.
Every selected row is reported, including anchor/build/source-guard failures.

Rows bind one mutation to one named cell replay in
`runtime::cron::schedule_tests` (a lib test, run with `--exact`); the kill
evidence is that test's own panic payload carrying its `#1324` message. The
rows pin the port's load-bearing behaviors: the version-3 day-field OR rule,
the dow 7→0 normalization, the floor (not trunc) minute alignment, the
leap-day shortcut (its `!dayFieldsUseOr` gate and its Date.UTC 0-99→1900-1999
year mapping), the exact 366-day `MAX_SEARCH_MINUTES` bound, the
schedule-timezone `??` precedence, the recorded-ICU timezone acceptance, the
delay unit table, NaN/Infinity `parseInt` rejection, the UTC-offset-identifier
resolution through the shared transport resolver (incl. the hour-only `+HH`
form), the fail-closed branches of both unresolvable-zone and local-wall
once parsing, and TimeClip's -0→+0 normalization.

All bound replays are host-tzdata-independent (they pin UTC, IANA, or offset
identifiers), so every row runs on both Linux and macOS.
"""
from pathlib import Path
import json
import os
import re
import subprocess
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot
from negative_suite import Case, classify, declares_test, passed

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
    # #1509 P2-2a: the 366-day search bound narrows to 365 days, so a run
    # landing on the very last scanned minute (a 366-day gap across a leap
    # year) is no longer reached and the cell computes undefined instead.
    ('search-bound-narrowed', source,
     'const MAX_SEARCH_MINUTES: u32 = 366 * 24 * 60;',
     'const MAX_SEARCH_MINUTES: u32 = 365 * 24 * 60;',
     'runtime::cron::schedule_tests::cron_scan_bound_366_found',
     NEXT_RUN.format(cell='cron/scan-bound-366-found')),
    # The leap-day shortcut never fires (`0 0 29 2 *` keeps the full dow
    # mask, so `!=` excludes it): the 366-day scan cannot reach a leap day
    # more than a year out.
    ('leap-day-shortcut-off', source,
     '        && cron.days_of_week == 0b0111_1111',
     '        && cron.days_of_week != 0b0111_1111',
     'runtime::cron::schedule_tests::cron_leap_day_late_window',
     NEXT_RUN.format(cell='cron/leap-day-late-window')),
    # #1509 P2-2b: the shortcut's `!day_fields_use_or` gate is dropped, so a
    # full-mask dow written as `0-6` (OR semantics: every February day) takes
    # the leap-day path and reports 2032-02-29 instead of 2029-02-01.
    ('leap-day-or-gate-dropped', source,
     '    !cron.day_fields_use_or\n        && cron.days_of_month.count_ones() == 1',
     '    cron.days_of_month.count_ones() == 1',
     'runtime::cron::schedule_tests::cron_leap_day_or_gate',
     NEXT_RUN.format(cell='cron/leap-day-or-gate')),
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
     '    crate::config::valid_timezone(timezone)\n'
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
    # fix-r1 P1-1: an Infinity step (>308 digits) parses as a huge integer
    # again, so `*/999… * * * *` computes a run instead of TS's undefined.
    ('infinite-step-accepted', source,
     '        if !step.is_finite() || step <= 0.0 {',
     '        if step <= 0.0 {',
     'runtime::cron::schedule_tests::cron_infinite_step_rejected',
     NEXT_RUN.format(cell='cron/infinite-step-rejected')),
    # fix-r1 P1-2: the Date.UTC 0-99→1900-1999 year mapping is dropped, so
    # the leap-day scan under a two-digit year lands in year 52, not 1952.
    ('leap-day-year-mapping-dropped', source,
     '    let year = if (0..=99).contains(&year) {\n'
     '        year + 1900\n'
     '    } else {\n'
     '        year\n'
     '    };',
     '    let year = year;',
     'runtime::cron::schedule_tests::cron_leap_day_after_year_0050',
     NEXT_RUN.format(cell='cron/leap-day-after-year-0050')),
    # fix-r1: a local-wall once runAt whose process-TZ resolution fails is
    # guessed as epoch 0 instead of failing closed to undefined.
    ('once-local-wall-guessed-utc', source,
     '        Ok(Some(epoch_ms)) => ComputedRunAt::Valid(epoch_ms),\n'
     '        Ok(None) | Err(_) => ComputedRunAt::Undefined,',
     '        Ok(Some(epoch_ms)) => ComputedRunAt::Valid(epoch_ms),\n'
     '        Ok(None) => ComputedRunAt::Undefined,\n'
     '        Err(_) => ComputedRunAt::Valid(0.0),',
     'runtime::cron::schedule_tests::once_local_wall_unresolvable_fails_closed',
     '#1324 once local-wall unresolved: must fail closed to undefined, not guess a date'),
    # fix-r1: a zone construction failure returns a run instead of failing
    # closed to undefined (never silently fall back to UTC).
    ('zone-resolver-failure-returns-run', source,
     '    let Ok(resolver) = resolver else {\n        return Ok(None);\n    };',
     '    let Ok(resolver) = resolver else {\n        return Ok(Some(0.0));\n    };',
     'runtime::cron::schedule_tests::zone_resolver_failure_fails_closed',
     '#1324 zone resolver failure: must fail closed to undefined, not fall back to UTC'),
    # fix-r1: the hour-only `+HH` branch is dropped from the shared transport
    # resolver, so `+05` resolves fail-closed instead of computing the 9am run.
    ('offset-hour-only-branch-dropped', clock,
     '    } else if body.len() == 2 {\n        (body, "0")',
     '    } else if body.len() == 12 {\n        (body, "0")',
     'runtime::cron::schedule_tests::cron_offset_hour_only',
     NEXT_RUN.format(cell='cron/offset-hour-only')),
    # fix-r1: TimeClip's -0→+0 normalization is dropped, so a sub-ms delay
    # over a pre-epoch after yields the -0.0 bit pattern.
    ('negative-zero-not-normalized', source,
     '        ComputedRunAt::Valid(epoch_ms.trunc() + 0.0)',
     '        ComputedRunAt::Valid(epoch_ms.trunc())',
     'runtime::cron::schedule_tests::delay_negative_zero_is_positive_zero',
     '#1324 delay negative zero: TimeClip must normalize -0 to +0'),
]


def check_anchor(row, original):
    if original.count(row[2]) != 1 or row[2] == row[3]:
        raise ValueError(f'{row[0]}: stale or unchanged anchor (count={original.count(row[2])})')


readme = Path(__file__).with_name('README.md')
oracle = Path(__file__).with_name('oracle.json')
SECTIONS = ('nextRuns', 'dayOr', 'validTimezones', 'resolveTimezones', 'delayMs')


def check_manifest():
    """Every hand-copied number must equal the artifact it describes.

    The README's section counts and `Rows:` list, and the `--exact` test name
    of each mutation, are hand-maintained copies of oracle.json / the replay
    macro table / this file. A copy that drifts is invisible until the cell or
    row it names is deleted, so `--check` derives them from the sources.
    """
    errors = []
    manifest = json.loads(oracle.read_text())
    # 修复轮 r6 P2（DS sibling#2，always-on 侧 fix-r3 空 oracle 拒绝的同族）：
    # 各节被清空成 []（README 计数表同步归零）时逐节 0==0、replay 绑定循环
    # 零次，全部空心通过；整份被删成 {} 则在下面 KeyError 冒 traceback——先
    # 拒绝空清单本身，--check 对被清空的 oracle.json 不再绿灯。
    if not manifest:
        return ['oracle.json: the scenario manifest is empty']
    text = readme.read_text()
    for section in SECTIONS:
        if not manifest.get(section):
            errors.append(f'oracle.json: section {section} is empty')
            continue
        match = re.search(rf'^\| `{section}` \| (\d+) \|', text, re.M)
        count = len(manifest[section])
        if not match:
            errors.append(f'README: no counts row for section {section}')
        elif int(match.group(1)) != count:
            errors.append(f'README: {section} says {match.group(1)} cells, oracle has {count}')
    # The list lives in its own paragraph (`Rows: `a`, `b`, …`), so scope the
    # scan to that paragraph: prose elsewhere in the README that mentions the
    # list would otherwise be swept into `listed` and compared as rows.
    blocks = [block for block in re.split(r'\n\s*\n', text) if 'Rows:' in block]
    listed = (re.findall(r'`([a-z0-9-]+)`', blocks[0].split('Rows:', 1)[1])
              if len(blocks) == 1 else [])
    if len(blocks) != 1:
        errors.append(f'README: expected exactly one Rows paragraph, found {len(blocks)}')
    expected_rows = [row[0] for row in mutations]
    if listed != expected_rows:
        errors.append(f"README: Rows list {listed} != mutations {expected_rows}")
    replay_source = tests.read_text()
    bound = set(re.findall(r'=>\s*"([^"]+)"', replay_source))
    for section in SECTIONS:
        for entry in manifest[section]:
            if entry['name'] not in bound:
                errors.append(f"{section}/{entry['name']}: no bound named replay in schedule_tests.rs")
    for row in mutations:
        ident = row[4].rsplit('::', 1)[-1]
        # 修复轮 r3 P1（DS 评审，r2 P2 升级）：绑定测试存在性不再用自写裸
        # 正则——注释/字符串里的 `fn name(` 或 `name => "` 满足裸正则，
        # 满足不了 declares_test（#2064 r10 硬化版：先抹白注释/字符串、
        # 左边界、fn 与 match-arm 两拼写折叠成一条规则），与 always-on
        # 侧 fix-r1 同一规则，合并登记在 #2303 条目 6/17。
        if not declares_test(replay_source, ident):
            errors.append(f'{row[0]}: bound test {ident} is not defined in schedule_tests.rs')
    return errors


def main():
    args = sys.argv[1:]
    if args == ['--check']:
        errors = []
        for row in mutations:
            try:
                check_anchor(row, row[1].read_text())
            except Exception as error:
                errors.append(str(error))
        errors.extend(check_manifest())
        print('\n'.join(errors) if errors else
              f'#1324 {len(mutations)} unique anchors and fixture manifest checked')
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
