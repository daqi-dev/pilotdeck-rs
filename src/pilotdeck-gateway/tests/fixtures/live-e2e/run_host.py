#!/usr/bin/env python3
# anchor-selfcheck: unsupported host acceptance runner, no mutation definitions
"""#748 bounded host acceptance; includes real cells and regeneration guard."""
import json
import os
import re
from pathlib import Path
import sys

FIXTURE = Path(__file__).resolve().parent
ROOT = FIXTURE.parents[4]
sys.path.insert(0, str(FIXTURE.parent))
from negative_process import run_owned, wait_for_slot


EXPECTED_TESTS = {
    'live_e2e': {'live_text', 'live_tool', 'live_cancel_stream', 'live_cancel_tool',
                 'live_drop', 'live_duplicate', 'live_early_final', 'live_artifact_provenance',
                 'live_ts_client_payload'},
    'generator_provenance': {'live_e2e_regenerates_identically',
                            'live_e2e_companions_reject_missing_corrupt_and_symlink'},
}


def completed_tests(target, result):
    names = re.findall(r'^test ([A-Za-z0-9_:]+) \.\.\. ok$', result.stdout, re.MULTILINE)
    expected = EXPECTED_TESTS[target]
    return {'passed': result.returncode == 0 and len(names) == len(expected) and set(names) == expected,
            'expected': sorted(expected), 'observed': names}


def main():
    if not os.environ.get('CARGO_TARGET_DIR') or not os.environ.get('PDRS_BASELINE'):
        raise RuntimeError('#748 caller must set CARGO_TARGET_DIR and PDRS_BASELINE')
    if 'LIVE_E2E_MITM' in os.environ or os.environ.get('PDRS_MUTANT') or os.environ.get('LIVE_E2E_MUTANT'):
        raise RuntimeError('#748 host acceptance rejects overrides and active mutants')
    output = Path(sys.argv[1]).resolve()
    output.mkdir(parents=True, exist_ok=False)
    records = []
    for target, test in [('live_e2e', 'live_'), ('generator_provenance', 'live_e2e_')]:
        slot = wait_for_slot(ROOT / 'scripts/cargo-slots.sh', timeout=900)
        (output / (target + '.slots.log')).write_text(slot.stdout + slot.stderr)
        command = ['cargo', 'test', '--offline', '--manifest-path', 'src/Cargo.toml', '-p',
                   'pilotdeck-gateway', '--test', target, test, '--', '--include-ignored', '--test-threads=1']
        result = run_owned(command, cwd=ROOT, env=os.environ.copy(), timeout=900)
        (output / (target + '.log')).write_text(result.stdout + result.stderr)
        records.append({'command': command, 'exitCode': result.returncode, **completed_tests(target, result)})
        print(result.stdout, end='')
        print(result.stderr, end='', file=sys.stderr)
    oracle = json.loads((FIXTURE / 'oracle.json').read_text())
    report = {'runs': records, 'sourceCommit': oracle['sourceCommit'], 'sourceSha256': oracle['sourceSha256'],
              'apparatusSha256': oracle['apparatusSha256']}
    (output / 'report.json').write_text(json.dumps(report, ensure_ascii=False, indent=2) + '\n')
    return int(any(not record['passed'] for record in records))


if __name__ == '__main__':
    sys.exit(main())
