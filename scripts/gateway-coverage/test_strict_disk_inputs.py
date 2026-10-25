#!/usr/bin/env python3
"""Disk-level type-strictness controls for the A5.1 ledger format.

The in-memory controls in ``check_ledger.py`` mutate the *expanded* ledger, so
they cannot observe a retype of the raw ``format_version`` (``expand`` used to
accept ``2.0`` and rewrite it to the int ``1``).  This script edits the raw
JSON on disk, runs the real validator against it, and requires a reported
``format_version`` failure.  It also unit-checks the two ``ledger_format``
entry points that read the raw version.

Fast by design: a rejected ``format_version`` skips the 117 in-memory controls;
the baseline graph is still parsed once for the report's parser controls.  The
unchanged ledger's full PASS is the sibling CI step
(``python3 -I scripts/gateway-coverage/check_ledger.py``); this script is only
the mutated half of the pair.

Runs under ``python3 -I`` (stdlib only).
"""
import json
import os
import subprocess
import sys
import tempfile
from pathlib import Path

SCRIPT_DIR = Path(__file__).resolve().parent
ROOT = SCRIPT_DIR.parents[1]
CHECK_LEDGER = SCRIPT_DIR / 'check_ledger.py'
LEDGER = ROOT / 'specs/gateway-coverage/ledger.json'
NEEDLE = '"format_version": 2,'


def baseline_path():
    env = os.environ.get('PDRS_BASELINE')
    if env:
        return Path(env)
    for parent in (ROOT, *ROOT.parents):
        candidate = parent / 'resources/PilotDeck'
        if candidate.is_dir():
            return candidate
    return ROOT / 'resources/PilotDeck'


def main():
    raw = LEDGER.read_text()
    if raw.count(NEEDLE) != 1:
        raise SystemExit('raw ledger no longer contains exactly one %r' % NEEDLE)
    cases = []
    with tempfile.TemporaryDirectory() as tmp:
        mutated = Path(tmp) / 'ledger-format-version-float.json'
        mutated.write_text(raw.replace(NEEDLE, '"format_version": 2.0,', 1))
        result = subprocess.run(
            [sys.executable, '-I', str(CHECK_LEDGER), '--ledger', str(mutated),
             '--baseline', str(baseline_path())],
            capture_output=True, text=True, timeout=300)
        try:
            report = json.loads(result.stdout)
        except ValueError:
            report = {}
        codes = [failure.get('code') for failure in report.get('failures', [])]
        cases.append({
            'name': 'disk_format_version_float',
            'mutation': '%s -> "format_version": 2.0,' % NEEDLE,
            'expected': 'exit!=0 and format_version failure',
            'observed': {'exit': result.returncode, 'status': report.get('status'),
                         'failure_codes': codes, 'stderr': result.stderr[-500:]},
            'result': 'PASS' if result.returncode != 0 and 'format_version' in codes else 'FAIL',
        })

    # The same retype must not be accepted by the compaction entry points.
    sys.path.insert(0, str(SCRIPT_DIR))
    from ledger_format import compact, expand  # noqa: E402
    expanded, failures = expand({'format_version': 2.0})
    cases.append({
        'name': 'expand_rejects_format_version_float',
        'mutation': "{'format_version': 2.0} through expand()",
        'expected': 'format_version failure, no expansion',
        'observed': {'format_version': expanded.get('format_version'),
                     'failure_codes': [failure.get('code') for failure in failures]},
        'result': 'PASS' if [failure.get('code') for failure in failures] == ['format_version']
                  and expanded.get('format_version') == 2.0 else 'FAIL',
    })
    try:
        compact({'format_version': 1.0})
        compact_result = 'accepted'
    except ValueError:
        compact_result = 'rejected'
    cases.append({
        'name': 'compact_rejects_format_version_float',
        'mutation': "{'format_version': 1.0} through compact()",
        'expected': 'rejected',
        'observed': compact_result,
        'result': 'PASS' if compact_result == 'rejected' else 'FAIL',
    })

    failed = any(case['result'] != 'PASS' for case in cases)
    print(json.dumps({'status': 'FAIL' if failed else 'PASS',
                      'scope': 'a51_disk_type_strictness', 'cases': cases},
                     ensure_ascii=False, indent=2))
    return 1 if failed else 0


if __name__ == '__main__':
    sys.exit(main())
