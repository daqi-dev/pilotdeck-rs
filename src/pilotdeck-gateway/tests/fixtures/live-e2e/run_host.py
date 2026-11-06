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


# Required host cells. #749 added the two websocket cells; they match the
# `live_` filter, so the declared inventory must grow with them (#589 close-out
# caught the stale 9-cell row reported as a false FAIL).
EXPECTED_TESTS = {
    'live_e2e': {'live_text', 'live_tool', 'live_cancel_stream', 'live_cancel_tool',
                 'live_drop', 'live_duplicate', 'live_early_final', 'live_artifact_provenance',
                 'live_ts_client_payload', 'live_ws_text', 'live_ws_tool'},
    'generator_provenance': {'live_e2e_regenerates_identically',
                            'live_e2e_companions_reject_missing_corrupt_and_symlink'},
}

# Cargo's own report is the independent count: `running N tests` is printed by
# the test harness before the per-test rows, so a filtered or renamed cell shows
# up as a mismatch between N and the `test ... ok` rows instead of a bare FAIL.
RUNNING_TESTS = re.compile(r'^running (\d+) tests?$', re.MULTILINE)
PASSED_TESTS = re.compile(r'^test ([A-Za-z0-9_:]+) \.\.\. ok$', re.MULTILINE)

# Unified cargo quota gate (#589 close-out): coordinator hosts install it under
# ~/ws/pdrs-ops, remote runner hosts under ~/ws/pdrs-runner-tools. Both are the
# same `cargo-slot --status` implementation.
SLOT_GATE_CANDIDATES = ('ws/pdrs-ops/bin/cargo-slot', 'ws/pdrs-runner-tools/bin/cargo')


def slot_gate(environ=None):
    """Return the unified cargo quota gate, or fail loudly.

    Letting `scripts/cargo-slots.sh` decide alone means a remote host without
    `~/ws/pdrs-ops` falls into its fail-closed exit 4 (or, with a stale gate
    path, into 69 = "no shared quota, continue"), which silently skips the
    per-host cargo quota. `PDRS_CARGO_SLOT_BIN` still wins when the caller sets
    it; an explicit but non-executable path is an error rather than an opt-out.
    """
    environ = os.environ if environ is None else environ
    configured = environ.get('PDRS_CARGO_SLOT_BIN')
    if configured:
        if os.access(configured, os.X_OK) and Path(configured).is_file():
            return configured
        raise RuntimeError(f'#748 PDRS_CARGO_SLOT_BIN={configured} is not an executable gate')
    home = environ.get('HOME')
    tried = []
    if home:
        for relative in SLOT_GATE_CANDIDATES:
            candidate = Path(home) / relative
            tried.append(str(candidate))
            if candidate.is_file() and os.access(candidate, os.X_OK):
                return str(candidate)
    raise RuntimeError(
        '#748 no executable cargo slot gate; refusing to skip the per-host quota. '
        'Set PDRS_CARGO_SLOT_BIN. Tried: '
        + (', '.join(tried) if tried else '(HOME is unset, no candidates)'))


def completed_tests(target, result):
    """Reconcile the declared inventory with cargo's announced and passed rows."""
    names = PASSED_TESTS.findall(result.stdout)
    announced = RUNNING_TESTS.findall(result.stdout)
    expected = EXPECTED_TESTS[target]
    observed = set(names)
    missing = sorted(expected - observed)
    unexpected = sorted(observed - expected)
    duplicated = sorted({name for name in names if names.count(name) > 1})
    announced_count = int(announced[-1]) if announced else None
    reasons = []
    if result.returncode != 0:
        reasons.append(f'cargo exit {result.returncode}')
    if announced_count is None:
        reasons.append('cargo did not print "running N tests"')
    elif announced_count != len(names):
        reasons.append(f'cargo announced {announced_count} tests but {len(names)} reported ok')
    if missing:
        reasons.append('missing: ' + ', '.join(missing))
    if unexpected:
        reasons.append('unexpected: ' + ', '.join(unexpected))
    if duplicated:
        reasons.append('duplicated: ' + ', '.join(duplicated))
    if len(names) != len(expected):
        reasons.append(f'expected {len(expected)} / observed {len(names)}')
    return {'passed': not reasons,
            'expected': sorted(expected), 'observed': names,
            'expectedCount': len(expected), 'observedCount': len(names),
            'announcedCount': announced_count,
            'missing': missing, 'unexpected': unexpected, 'reasons': reasons}


def failure_message(target, record):
    """Readable drift report: counts plus the cells that went missing."""
    return (f'#748 {target} host acceptance FAILED: expected {record["expectedCount"]} / '
            f'observed {record["observedCount"]} (cargo announced {record["announcedCount"]}); '
            + '; '.join(record['reasons']))


def main():
    if not os.environ.get('CARGO_TARGET_DIR') or not os.environ.get('PDRS_BASELINE'):
        raise RuntimeError('#748 caller must set CARGO_TARGET_DIR and PDRS_BASELINE')
    if 'LIVE_E2E_MITM' in os.environ or os.environ.get('PDRS_MUTANT') or os.environ.get('LIVE_E2E_MUTANT'):
        raise RuntimeError('#748 host acceptance rejects overrides and active mutants')
    gate = slot_gate()
    # cargo-slots.sh reads PDRS_CARGO_SLOT_BIN from the inherited environment.
    os.environ['PDRS_CARGO_SLOT_BIN'] = gate
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
        record = {'command': command, 'exitCode': result.returncode, **completed_tests(target, result)}
        records.append(record)
        print(result.stdout, end='')
        print(result.stderr, end='', file=sys.stderr)
        if not record['passed']:
            print(failure_message(target, record), file=sys.stderr)
    oracle = json.loads((FIXTURE / 'oracle.json').read_text())
    report = {'runs': records, 'slotGate': gate,
              'sourceCommit': oracle['sourceCommit'], 'sourceSha256': oracle['sourceSha256'],
              'apparatusSha256': oracle['apparatusSha256']}
    (output / 'report.json').write_text(json.dumps(report, ensure_ascii=False, indent=2) + '\n')
    return int(any(not record['passed'] for record in records))


if __name__ == '__main__':
    sys.exit(main())
