#!/usr/bin/env python3
# anchor-selfcheck: unsupported host acceptance runner, no mutation definitions
"""#748 bounded host acceptance; includes real cells and regeneration guard."""
import json
import os
from pathlib import Path
import sys

FIXTURE = Path(__file__).resolve().parent
ROOT = FIXTURE.parents[4]
sys.path.insert(0, str(FIXTURE.parent))
from negative_process import run_owned, wait_for_slot


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
        records.append({'command': command, 'exitCode': result.returncode})
        print(result.stdout, end='')
        print(result.stderr, end='', file=sys.stderr)
    oracle = json.loads((FIXTURE / 'oracle.json').read_text())
    report = {'runs': records, 'sourceCommit': oracle['sourceCommit'], 'sourceSha256': oracle['sourceSha256'],
              'apparatusSha256': oracle['apparatusSha256']}
    (output / 'report.json').write_text(json.dumps(report, ensure_ascii=False, indent=2) + '\n')
    return int(any(record['exitCode'] for record in records))


if __name__ == '__main__':
    sys.exit(main())
