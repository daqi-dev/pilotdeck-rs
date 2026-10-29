#!/usr/bin/env python3
"""#988/#996 HEAD-only controls for the feature boundary and exact selector.

Commit the new mutants.rs first. No alternate baseline, source copy or target.
Usage: CARGO_TARGET_DIR=/abs/target python3 switch_negative.py NEW_OUTPUT_DIR
Each row fixes its own selector environment and cargo invocation: the lib tests
guard the no-feature and selector contracts (#988), and the integration test
guards that ordinary library builds stay constant false even with the feature
(#996 r2 P2-1).
"""
import json
import os
from pathlib import Path
import subprocess
import sys
import time
from types import SimpleNamespace

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot
from negative_suite import classify, passed
from _shared.mutant_switch import enforce_denominator
import anchor_selfcheck

ROOT = Path(__file__).resolve().parents[5]
SOURCE = ROOT / 'src/pilotdeck-gateway/src/mutants.rs'
# Both cfg gates must flip together: leaving either `all(test, ...)` shape in
# place would duplicate the definition instead of widening the selector arm.
TEST_GATE = ('#[cfg(all(test, feature = "pdrs-mutants"))]\n'
             'pub fn on(name: &str) -> bool {\n'
             '    !name.is_empty() && std::env::var("PDRS_MUTANT").is_ok_and(|selected| selected == name)\n'
             '}\n\n'
             '#[cfg(not(all(test, feature = "pdrs-mutants")))]\n'
             '#[inline(always)]')
TEST_GATE_FLIPPED = ('#[cfg(feature = "pdrs-mutants")]\n'
                     'pub fn on(name: &str) -> bool {\n'
                     '    !name.is_empty() && std::env::var("PDRS_MUTANT").is_ok_and(|selected| selected == name)\n'
                     '}\n\n'
                     '#[cfg(not(feature = "pdrs-mutants"))]\n'
                     '#[inline(always)]')
MUTATIONS = [
    ('default-enabled', SOURCE, '    false\n}', '    true\n}',
     'mutants::tests::switches_are_disabled_without_feature', 'default-root-dir', []),
    ('selector-inverted', SOURCE, 'selected == name', 'selected != name',
     'mutants::tests::selection_matches_only_the_requested_name', 'default-root-dir',
     ['--features', 'pdrs-mutants']),
    ('empty-name', SOURCE,
     '!name.is_empty() && std::env::var("PDRS_MUTANT").is_ok_and(|selected| selected == name)',
     'std::env::var("PDRS_MUTANT").is_ok_and(|selected| selected == name)',
     'mutants::tests::selection_matches_only_the_requested_name', '', ['--features', 'pdrs-mutants']),
    ('test-gate', SOURCE, TEST_GATE, TEST_GATE_FLIPPED,
     'feature_builds_keep_switches_const_false', 'default-root-dir',
     ['--features', 'pdrs-mutants', '--test', 'mutants_boundary']),
]
MESSAGES = {
    'default-enabled': '#988 non-feature switch must be constant false',
    'selector-inverted': '#988 selected switch',
    'empty-name': '#988 selected switch',
    'test-gate': '#996 ordinary library builds must not enable switches',
}


def main():
    if len(sys.argv) != 2 or not os.environ.get('CARGO_TARGET_DIR'):
        raise SystemExit('NEW_OUTPUT_DIR and caller CARGO_TARGET_DIR are required')
    out = Path(sys.argv[1]).resolve()
    out.mkdir(parents=True, exist_ok=False)
    results = []

    def publish():
        counts = {status: sum(r['status'] == status for r in results)
                  for status in ('KILLED', 'SURVIVED', 'INVALID')}
        temp = out / 'summary.json.tmp'
        temp.write_text(json.dumps({'controls': results, 'counts': counts}, indent=2) + '\n')
        temp.replace(out / 'summary.json')

    def run(name, test, phase, selector, args, compile_only=False):
        command = ['cargo', 'test', '--offline', '--locked', '--manifest-path', str(ROOT / 'src/Cargo.toml'),
                   '-p', 'pilotdeck-gateway', *args]
        command += ['--no-run'] if compile_only else [test, '--', '--exact']
        with (out / f'{name}-{phase}.log').open('w') as log:
            try:
                slot = wait_for_slot(ROOT / 'scripts/cargo-slots.sh', timeout=900)
                (out / f'{name}-{phase}.slots.log').write_text(slot.stdout + slot.stderr)
                result = run_owned(command, cwd=ROOT, env=dict(os.environ, PDRS_MUTANT=selector),
                                   timeout=900)
                log.write(result.stdout + result.stderr)
            except Exception as error:
                if isinstance(error, subprocess.TimeoutExpired):
                    for part in (error.stdout, error.stderr):
                        log.write(part.decode(errors='replace') if isinstance(part, bytes) else part or '')
                log.write(f'\n{type(error).__name__}: {error}\n')
                raise
        return result

    try:
        with mutating_sources(ROOT, [SOURCE]) as originals:
            for name, path, old, new, test, selector, args in MUTATIONS:
                status, reason = 'INVALID', ''
                try:
                    source = originals[path].decode()
                    if source.count(old) != 1:
                        raise ValueError(f'{name}: stale anchor')
                    if not passed(run(name, test, 'positive', selector, args), test):
                        raise ValueError('positive test failed')
                    path.write_text(source.replace(old, new, 1))
                    time.sleep(1.1)
                    if run(name, test, 'compile', selector, args, True).returncode:
                        raise ValueError('mutant does not compile')
                    status, reason = classify(run(name, test, 'negative', selector, args),
                        SimpleNamespace(test=test, assertion=MESSAGES[name]))
                except Exception as error:
                    status, reason = 'INVALID', str(error)
                finally:
                    path.write_bytes(originals[path])
                    time.sleep(1.1)
                try:
                    if not passed(run(name, test, 'restored', selector, args), test):
                        status, reason = 'INVALID', reason + '; restored test failed'
                except Exception as error:
                    status, reason = 'INVALID', reason + '; restored: ' + str(error)
                results.append(dict(name=name, status=status, reason=reason))
                publish()
    except Exception as error:
        for row in results:
            row.update(status='INVALID', reason=str(error))
        for name, *_ in MUTATIONS[len(results):]:
            results.append(dict(name=name, status='INVALID', reason='source guard: ' + str(error)))
    # #996 fix-r5: the same post-execution denominator as the switch
    # executors; a MUTATIONS table shrunk at runtime must not publish a
    # shorter success. The declared source is this script's static table,
    # never the runtime list the loop could already have shrunk.
    enforce_denominator(ROOT, Path(__file__), results, (),
                        declared=anchor_selfcheck.static_mutant_names(Path(__file__), ROOT))
    publish()
    for row in results:
        print(f"{row['name']}: {row['status']}: {row['reason']}")
    return int(any(r['status'] != 'KILLED' for r in results))


if __name__ == '__main__':
    sys.exit(main())
