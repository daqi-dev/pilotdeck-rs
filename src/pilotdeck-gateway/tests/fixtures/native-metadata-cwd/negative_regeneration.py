#!/usr/bin/env python3
"""#1154 golden corruption must fail the actual ignored regeneration guard.

The identical oracle-node-version mutant is compiled first by
negative_controls.py --compile-only. This runner uses the shared guarded
suite to execute the ignored test with positive/negative/restored controls.
"""
from pathlib import Path
import os
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
# run_cases owns the guarded mutation window and cargo process/slot handling.
from negative_process import mutating_sources, run_owned, wait_for_slot
from negative_suite import Case, run_cases

ROOT = Path(__file__).resolve().parents[5]
ORACLE = Path(__file__).resolve().parent / 'oracle.json'
mutations = [
    ('oracle-node-version', ORACLE, '"node": "v22.22.0"', '"node": "v0.0.0"',
     'native_process::tests::metadata_cwd::regenerates_identically',
     '#280 native-metadata-cwd/oracle.json differs'),
]


def mutate(text, scope, edits):
    for old, new in edits:
        if text.count(old) != 1 or old == new:
            raise ValueError('#1154 stale regeneration mutation anchor')
        text = text.replace(old, new, 1)
    return text


if __name__ == '__main__':
    if sys.argv[1:] == ['--check']:
        for _, path, old, new, *_ in mutations:
            mutate(path.read_text(), None, [(old, new)])
        print('#1154 regeneration anchor OK')
        raise SystemExit(0)
    if len(sys.argv) != 2 or sys.argv[1].startswith('--'):
        raise SystemExit('usage: negative_regeneration.py NEW_OUTPUT_DIRECTORY')
    env = dict(os.environ)
    if not env.get('CARGO_TARGET_DIR') or not env.get('PDRS_BASELINE'):
        raise SystemExit('CARGO_TARGET_DIR and PDRS_BASELINE must be supplied')
    out = Path(sys.argv[1]).resolve()
    out.mkdir(parents=True, exist_ok=False)
    # Protect the executable judge too, matching matrix_main's HEAD policy. The
    # #280 verdict itself lives in tests/common/baseline.rs (compiled into the
    # lib by src/lib.rs's #[path]), with regeneration_masks.rs and the shared
    # mask table outside the src/pilotdeck-gateway/src pristine scan.
    fixture_root = Path(__file__).resolve().parents[1]
    guarded = [Path(__file__).resolve(), *[
        fixture_root / name for name in (
            'negative_suite.py', 'negative_process.py', 'negative_panic.py',
            'pending_reanchor.py',
        )
    ], *[
        ROOT / name for name in (
            'src/pilotdeck-gateway/src/lib.rs',
            'src/pilotdeck-gateway/tests/common/baseline.rs',
            'src/pilotdeck-gateway/tests/common/regeneration_masks.rs',
            'src/pilotdeck-gateway/tests/fixtures/_shared/regeneration-masks.tsv',
        )
    ]]
    with mutating_sources(ROOT, guarded):
        raise SystemExit(run_cases(ROOT, [Case(*row) for row in mutations], out, env,
                                  mutate, script=__file__))
