#!/usr/bin/env python3
"""#2396 bounded smoke: three representative cells against the real binaries.

Few-cell smoke only — the full 72-cell frozen-deadline captures belong to the
coordinator host chain (taskbook division of labour). Every smoke case is
driven through the real s2-s4 entries (#1369 registry construction, #1370
storage barriers/faults, #1371 controlled loop) and must come back WITNESSED
with raw receipts; anything else prints the precise missing list and fails.
The manifest patch below is explicit and printed: this is a smoke of the
adapter seam, never a denominator change — full-manifest captures use
collect.py unpatched.
"""
import argparse
import importlib.util
import json
import sys
from pathlib import Path
from unittest import mock

spec = importlib.util.spec_from_file_location('collector', Path(__file__).with_name('collect.py'))
c = importlib.util.module_from_spec(spec)
spec.loader.exec_module(c)

# One storage-barrier choreography (slowStatus holds the writer write, the
# registry append barrier rides along), one unfired-timer order (unknown-first)
# and one held-return order (K-P) — the three families the attestation scores
# most state from.
SMOKE_CASES = {
    'on': ['registry-slow/status-slow-append-fast/model',
           'registry-fast/unknown-first/model',
           'registry-fast/known-final-before-timeout-pending/model'],
    'off': ['no-registry-slow/status-slow/model',
            'no-registry-fast/unknown-first/model',
            'no-registry-fast/after-complete/model'],
}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--gateway', required=True)
    parser.add_argument('--sidecar', required=True)
    parser.add_argument('--output', required=True)
    parser.add_argument('--port', type=int, default=28999)
    parser.add_argument('--registry', choices=('on', 'off'), default='on')
    args = parser.parse_args()
    cases = SMOKE_CASES[args.registry]
    output = Path(args.output)
    output.parent.mkdir(parents=True, exist_ok=True)
    with mock.patch.object(c, 'session_manifest', return_value=list(cases)):
        c.native_run(args.gateway, args.sidecar, output, port=args.port,
                     session_domain=True, registry=args.registry)
    rows = json.loads((output / 'session-domain' / 'cases.json').read_text())['cases']
    verdict = {row['case']: row['stimulus']['status'] for row in rows}
    missing = {row['case']: row['stimulus'].get('missing')
               for row in rows if row['stimulus']['status'] != 'WITNESSED'}
    print(json.dumps({'smoke': '#2396 s2-s4 adapter', 'registry': args.registry,
                      'cases': cases, 'verdicts': verdict, 'missing': missing},
                     ensure_ascii=False, indent=1))
    if missing or sorted(verdict) != sorted(cases):
        sys.exit('#2396 smoke cells missing raw receipts: ' + json.dumps(missing))


if __name__ == '__main__':
    main()
