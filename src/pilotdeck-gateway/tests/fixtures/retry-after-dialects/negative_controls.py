#!/usr/bin/env python3
"""#360-2. Run after committing; OUT [NAME ...].

Every mutant must compile, fail the bound assertion, and pass after
restoration. The replay dialects bind the lib module mounted at
runtime::model::http::retry_after_dialect_tests; generator-epoch binds the
model_stream regeneration guard and needs PDRS_BASELINE plus the calibrated
node v22.22.0 on PATH. All selected outcomes are reported; --check-anchors is
read-only. Full suite is for the coordinator host; use at most 1-2 samples in
a worker session.

Mutants in PAIRED_HOST_ONLY are killed only through the zone children whose
host TZif is the SHA-paired local-profiles.json profile. Their positive run
therefore goes through cargo with --nocapture so the children's "#360-2
PARTIAL" downgrade marker reaches this harness: on such a host the mutants
are refused as INVALID (an explicit unpaired-host explanation, never a
silent SURVIVED or a silent pass).
"""
import argparse
import json
import os
from pathlib import Path
import subprocess
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot
from negative_suite import Case, classify, passed

root = Path(__file__).resolve().parents[5]
errors = root / 'src/pilotdeck-gateway/src/runtime/model/http/errors.rs'
http = root / 'src/pilotdeck-gateway/src/runtime/model/http.rs'
replay_rs = root / 'src/pilotdeck-gateway/tests/fixtures/retry-after-dialects/retry_after.rs'
generator = root / 'src/pilotdeck-gateway/tests/fixtures/retry-after-dialects/generate.mts'
oracle = root / 'src/pilotdeck-gateway/tests/fixtures/retry-after-dialects/oracle.json'
replay = 'runtime::model::http::retry_after_dialect_tests::retry_after_dialects_match_node_in_two_timezones'
zone_children = 'runtime::model::http::retry_after_dialect_tests::retry_after_dialects_production_wiring_matches_node_zone_children'
huge = 'runtime::model::http::retry_after_dialect_tests::retry_after_dialects_huge_delta_seconds_parse_to_infinity_and_clamp'
regen = 'retry_after_dialects_regenerate_identically'
cases = [
    ('dialect-ignored', errors, 'retry_after_header_parsed(value, now_ms, &admitted_node22_parse)',
     'retry_after_header_parsed(value, now_ms, &|_| Ok(None))', replay, '#360-2'),
    ('local-ignored', errors, '&|wall| local(wall))', '&|wall| Ok(wall))', replay, '#360-2'),
    ('delta-strict', errors, '(delta > 0.0).then_some(delta)', '(delta >= 0.0).then_some(delta)', replay, '#360-2'),
    ('seconds-negative', errors, 'if seconds.is_finite() && seconds >= 0.0 {', 'if seconds.is_finite() && seconds > 0.0 {', replay, '#360-2'),
    ('seconds-rounding', errors, 'return Some(js_round(seconds * 1000.0));', 'return Some(seconds * 1000.0);', replay, '#360-2'),
    ('empty-filter', errors, 'let value = value.filter(|value| !value.is_empty())?;', 'let value = value?;', replay, '#360-2'),
    ('generator-epoch', generator, 'Date.now = () => 0;', 'Date.now = () => 1000;', regen, '#280 retry-after-dialects'),
    ('oracle-asctime', oracle, '784082977000', '784082977001', replay, '#360-2'),
    # fix-r1 P2-1: a production entry rewired to a parser that only breaks
    # local resolution keeps the seam replay and the zone-independent cases
    # green; only the TZ-pinned children catch it (fix-r2 re-anchored to the
    # admitted gate).
    ('production-local-wiring', errors, '&crate::runtime::model::stats::admitted_local_to_utc,',
     '&|_| Ok(0),', zone_children, '#360-2'),
    # fix-r2 P1/P2-1: the production entry rewound to the ungated parser
    # (any readable host TZif, TZ=localtime via /etc/localtime) keeps the seam
    # replay green; the TZ=localtime child catches it (Node's reading of that
    # alias is host-dependent, so the admitted subset fail-closes it to None).
    ('production-gate-bypass', errors, 'retry_after_header_parsed(value, now_ms, &admitted_node22_parse)',
     'retry_after_header_parsed(value, now_ms, &pilotdeck_transport::system_clock::parse_node22_date)', zone_children, '#360-2'),
    # fix-r2 (replaces fix-r1 err-utc-fallback, whose anchor left the
    # retry-after path): a gate error swallowed into a UTC wall keeps the
    # admitted-zone children green; the fail-closed children catch it.
    ('production-utc-fallback', errors, '&crate::runtime::model::stats::admitted_local_to_utc,',
     '&|wall| Ok(crate::runtime::model::stats::admitted_local_to_utc(wall).unwrap_or(wall)),', zone_children, '#360-2'),
    # fix-r2 P2-2: the zone child's expectations must actually follow the
    # host-profile admission; forcing it false must fail the admitted children.
    ('zone-child-admission-false', replay_rs, 'crate::runtime::model::stats::stats_tzif_local(0, &bytes, &zone).is_ok()',
     'false', zone_children, '#360-2'),
    # fix-r1 P2-2: the non-finite gate and the maxDelayMs clamp behind the
    # huge delta-seconds value.
    ('seconds-nonfinite', errors, 'if seconds.is_finite() && seconds >= 0.0 {', 'if seconds >= 0.0 {', huge, '#360-2'),
    ('retry-clamp', http, 'return (max_retries, js_math_min(after, max_delay));', 'return (max_retries, after);', huge, '#360-2'),
    # fix-r1 P3: provenance must be pinned by the quick replay, not only by
    # the ignored byte-identity guard.
    ('oracle-source-commit', oracle, '"sourceCommit": "cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85",',
     '"sourceCommit": "0000000000000000000000000000000000000000",', replay, '#360-2'),
    ('oracle-source-sha', oracle, '"src/model/protocol/errors.ts": "109e8cd3191d2ae51bec90578148fc06a3f8dc22a2d28c71a96c77fe2e9984bf"',
     '"src/model/protocol/errors.ts": "0000000000000000000000000000000000000000000000000000000000000000"', replay, '#360-2'),
    # fix-r2 P3-1: the quick replay must pin the oracle's runtime provenance
    # (node/v8/icu/tzdata), not only the TS source bytes.
    ('oracle-runtime-node', oracle, '"node": "v22.22.0"', '"node": "v22.21.0"', replay, '#360-2'),
]

# fix-r3 P2-3: these mutants only die through the admitted TZ=UTC / Asia/Shanghai
# zone children, whose kill power needs a host profile SHA-paired with
# local-profiles.json. On an unpaired host those children replay fail-closed
# (None) and the mutant survives the same expectation, so the run is refused
# (INVALID with the PARTIAL evidence) instead of reporting a misleading
# outcome. production-gate-bypass / production-utc-fallback stay runnable:
# their kills come from the fail-closed children (TZ=localtime, TZ=Europe/London
# or the unpaired None expectation itself) and hold on any host.
PAIRED_HOST_ONLY = {'production-local-wiring', 'zone-child-admission-false'}
PARTIAL_MARKER = '#360-2 PARTIAL'


def main():
    if sys.argv[1:] == ['--check-anchors']:
        failures = []
        for name, path, old, _, _, _ in cases:
            count = path.read_text().count(old)
            if count != 1:
                failures.append(f'{name}: expected 1 anchor, got {count}')
        print('\n'.join(failures) if failures else f'{len(cases)} anchors OK')
        return int(bool(failures))
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('out', type=Path)
    parser.add_argument('mutations', nargs='*')
    args = parser.parse_args()
    env = os.environ.copy()
    if not env.get('CARGO_TARGET_DIR'):
        parser.error('CARGO_TARGET_DIR must be provided by the caller')
    unknown = set(args.mutations) - {row[0] for row in cases}
    if unknown:
        parser.error(f'unknown mutations: {sorted(unknown)}')
    selected = [row for row in cases if not args.mutations or row[0] in args.mutations]
    if any(row[0] == 'generator-epoch' for row in selected) and not env.get('PDRS_BASELINE'):
        parser.error('generator-epoch needs PDRS_BASELINE for the regeneration guard')
    args.out.mkdir(parents=True, exist_ok=False)
    results = []

    def run(label, test, compile_only=False, nocapture=False):
        command = ['cargo', 'test', '--offline', '--manifest-path', 'src/Cargo.toml', '-p', 'pilotdeck-gateway']
        command += ['--lib'] if test.startswith('runtime::') else ['--test', 'model_stream']
        try:
            slot = wait_for_slot(root / 'scripts/cargo-slots.sh', timeout=300)
            (args.out / f'{label}.slots.log').write_text(slot.stdout + slot.stderr)
            suffix = ['--no-run'] if compile_only else [test, '--', '--exact', '--include-ignored']
            if nocapture:
                # Only ever the positive run: the parent re-prints each
                # child's stdout, so --nocapture brings the "#360-2 PARTIAL"
                # downgrade marker into this harness's log for the
                # PAIRED_HOST_ONLY refusal (fix-r3 P2-3). The kill run stays
                # captured — an interleaved stdout chunk on the FAILED status
                # line would hide it from the judge (#938 r14).
                suffix.append('--nocapture')
            result = run_owned(command + suffix, cwd=root, env=env, timeout=600)
            (args.out / f'{label}.log').write_text(result.stdout + result.stderr)
            return result
        except Exception as error:
            raw = ''
            if isinstance(error, subprocess.TimeoutExpired):
                for item in (error.stdout, error.stderr):
                    raw += item.decode(errors='replace') if isinstance(item, bytes) else item or ''
            with (args.out / f'{label}.log').open('a') as log:
                log.write(raw + f'\n{type(error).__name__}: {error}\n')
            raise

    def record(name, test, assertion, status, reason):
        results.append(dict(mutation=name, test=test, assertion=assertion, status=status, reason=reason))
        print(f'{name}: {status}: {reason}', flush=True)

    try:
        with mutating_sources(root, sorted({row[1] for row in selected})) as snapshots:
            for name, path, old, new, test, assertion in selected:
                status, reason = 'INVALID', 'not run'
                try:
                    original = snapshots[path].decode()
                    if original.count(old) != 1:
                        raise ValueError(f'{name}: expected one source anchor')
                    positive = run(
                        name + '-positive', test,
                        nocapture=test == zone_children,
                    )
                    if not passed(positive, test):
                        reason = 'bound positive test did not pass'
                    elif name in PAIRED_HOST_ONLY and PARTIAL_MARKER in (
                        positive.stdout + positive.stderr
                    ):
                        # fix-r3 P2-3: explicit unpaired-host refusal — never a
                        # silent SURVIVED on a host whose zone children replay
                        # fail-closed, and never a silent pass either.
                        status = 'INVALID'
                        reason = (
                            'unpaired host: the bound zone-children positive run '
                            f'printed "{PARTIAL_MARKER}" (host TZif is not the '
                            'SHA-paired local-profiles.json profile), so the admitted '
                            'UTC/Asia/Shanghai children replay fail-closed there and '
                            'this mutant cannot be killed on this host; run it on a '
                            'paired host'
                        )
                    else:
                        path.write_text(original.replace(old, new, 1))
                        if run(name + '-compile', test, compile_only=True).returncode != 0:
                            reason = 'mutant failed to compile'
                        else:
                            status, reason = classify(run(name + '-negative', test), Case(name, path, old, new, test, assertion))
                except Exception as error:
                    status, reason = 'INVALID', f'{type(error).__name__}: {error}'
                finally:
                    path.write_bytes(snapshots[path])
                try:
                    if not passed(run(name + '-restored', test), test):
                        status, reason = 'INVALID', reason + '; restored bound test did not pass'
                except Exception as error:
                    status, reason = 'INVALID', reason + f'; restored: {error}'
                record(name, test, assertion, status, reason)
    except Exception as error:
        reason = f'source guard: {error}'
        for row in results:
            row.update(status='INVALID', reason=row['reason'] + '; ' + reason)
        for name, _, _, _, test, assertion in selected[len(results):]:
            record(name, test, assertion, 'INVALID', reason)
    counts = {state: sum(row['status'] == state for row in results) for state in ('KILLED', 'SURVIVED', 'INVALID')}
    (args.out / 'summary.json').write_text(json.dumps(dict(controls=results, counts=counts), indent=2) + '\n')
    print(json.dumps(counts), flush=True)
    return int(not results or any(row['status'] != 'KILLED' for row in results))


if __name__ == '__main__':
    raise SystemExit(main())
