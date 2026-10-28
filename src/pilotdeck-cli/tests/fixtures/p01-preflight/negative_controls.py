#!/usr/bin/env python3
"""#982 (#123 D1-P01) preflight negative controls for pilotdeck-cli sdk.rs.

Runs after the branch is committed: ``mutating_sources`` proves
``src/pilotdeck-cli/src/sdk.rs`` equals HEAD before mutating, so these
controls are executed by the coordinator on the host (10-27 rule 11: this
tree's fix is not in HEAD while the branch is unmerged).

Every mutant must be killed by an assertion carrying a ``#982 <grid>`` message
in ``tests/sdk_preflight_exit.rs``; a failing run without such a message is
INVALID, not KILLED. All mutants run to completion before the summary, and any
non-KILLED verdict exits non-zero. An expired anchor (the fixed source text
changed) is a hard error, never SURVIVED.

Usage (CARGO_TARGET_DIR is mandatory; it is never created here):

    CARGO_TARGET_DIR=<dir> python3 -I negative_controls.py [mutant-name ...]
"""
import os
from pathlib import Path
import sys

REPO = Path(__file__).resolve().parents[5]
sys.path.insert(0, str(REPO / 'src/pilotdeck-gateway/tests/fixtures'))
from negative_process import mutating_sources, run_owned, wait_for_slot  # noqa: E402

SDK = 'src/pilotdeck-cli/src/sdk.rs'
TEST = ['cargo', 'test', '--manifest-path', str(REPO / 'src/Cargo.toml'),
        '-p', 'pilotdeck-cli', '--test', 'sdk_preflight_exit']

# (name, before, after, bound assertion). ``before`` must occur exactly once.
MUTANTS = [
    (
        'settings-get-url-check-removed',
        '        "get" => {\n'
        '            create_client(&connection)?;\n',
        '        "get" => {\n'
        '            let _ = &connection;\n',
        '#982 settings-default/get--gateway-empty assertions',
    ),
    (
        'settings-set-url-check-removed',
        '            // above already precedes both (#982 settings-set--gateway-empty).\n'
        '            create_client(&connection)?;\n',
        '            // above already precedes both (#982 settings-set--gateway-empty).\n'
        '            let _ = &connection;\n',
        '#982 settings-set--gateway-empty assertion',
    ),
    (
        'settings-url-check-before-subcommand',
        '    let connection = read_connection(argv, env, cwd)?;\n'
        '    match argv.first().map_or("get", String::as_str) {\n',
        '    let connection = read_connection(argv, env, cwd)?;\n'
        '    create_client(&connection)?;\n'
        '    match argv.first().map_or("get", String::as_str) {\n',
        '#982 settings-unknown--gateway-empty usage assertion',
    ),
    (
        'sdk-token-empty-falls-through',
        '            .string("PILOTDECK_SDK_AUTH_TOKEN")\n'
        '            .or_else(|| env.string("PILOTDECK_TOKEN"))\n',
        '            .string("PILOTDECK_SDK_AUTH_TOKEN")\n'
        '            .filter(|value| !value.is_empty())\n'
        '            .or_else(|| env.string("PILOTDECK_TOKEN"))\n',
        '#982 token-sdk-empty assertions',
    ),
]


def run_mutant(name, bound, results):
    wait_for_slot(REPO / 'scripts/cargo-slots.sh')
    outcome = run_owned(TEST, env=os.environ, cwd=REPO, timeout=1200)
    # The kill needs a #982-tagged failure point; an untagged failure or a
    # pass is not a usable verdict.
    tagged = '#982' in outcome.stdout + outcome.stderr
    if outcome.returncode == 0:
        verdict, reason = 'SURVIVED', 'test passed with the mutant applied'
    elif not tagged:
        verdict, reason = ('INVALID',
                           'failed without a #982-tagged assertion; first lines: '
                           + '\n'.join((outcome.stdout + outcome.stderr).splitlines()[:5]))
    else:
        verdict, reason = 'KILLED', f'bound to {bound}'
    results.append((name, verdict, reason))
    return verdict


def main():
    if not os.environ.get('CARGO_TARGET_DIR'):
        print('negative_controls: CARGO_TARGET_DIR must be set by the caller',
              file=sys.stderr)
        return 2
    selected = sys.argv[1:] or [m[0] for m in MUTANTS]
    results = []
    with mutating_sources(REPO, [SDK]) as snapshot:
        original = snapshot[SDK].decode()
        for name, before, after, bound in MUTANTS:
            if name not in selected:
                continue
            count = original.count(before)
            if count != 1:
                print(f'negative_controls: anchor expired for {name}: '
                      f'{count} occurrences of the fixed source text in {SDK}',
                      file=sys.stderr)
                return 3
            (REPO / SDK).write_bytes(original.replace(before, after).encode())
            run_mutant(name, bound, results)
    print(f'#982 P01 negative controls: {len(results)} mutants')
    failures = 0
    for name, verdict, reason in results:
        print(f'  {verdict:8} {name}: {reason}')
        failures += verdict != 'KILLED'
    if failures:
        print(f'non-KILLED verdicts: {failures}', file=sys.stderr)
        return 1
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
