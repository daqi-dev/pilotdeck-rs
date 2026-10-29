#!/usr/bin/env python3
# anchor-selfcheck: unsupported fixture verifier, no source mutations
"""#748 independently regenerate all TS observations and compare complete JSON bytes."""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys

FIXTURE = Path(__file__).resolve().parent


def main():
    if 'LIVE_E2E_MITM' in os.environ:
        raise RuntimeError('#748 selftest rejects LIVE_E2E_MITM')
    if not os.environ.get('PDRS_BASELINE'):
        raise RuntimeError('#748 selftest requires PDRS_BASELINE')
    output = Path(sys.argv[1]).resolve()
    output.mkdir(parents=True, exist_ok=False)
    baseline = Path(os.environ['PDRS_BASELINE'])
    result = subprocess.run(['node', '--import', str(baseline / 'node_modules/tsx/dist/loader.mjs'),
                             str(FIXTURE / 'generate.mts'), str(output / 'regenerated')],
                            env=os.environ.copy(), text=True, capture_output=True, timeout=600)
    (output / 'stdout.log').write_text(result.stdout)
    (output / 'stderr.log').write_text(result.stderr)
    failures = []
    if result.returncode:
        failures.append('#748 selftest TS generation failed: ' + result.stderr[-2000:])
    cells = []
    oracle = {}
    if not failures:
        oracle = json.loads((output / 'regenerated/oracle.json').read_text())
        if (FIXTURE / 'oracle.json').read_bytes() != (output / 'regenerated/oracle.json').read_bytes():
            failures.append('#748 selftest provenance or frozen byte hashes differ')
        for name, digest in oracle['expectedSha256'].items():
            expected = (FIXTURE / 'expected' / (name + '.json')).read_bytes()
            actual = (output / 'regenerated/expected' / (name + '.json')).read_bytes()
            verified = expected == actual and hashlib.sha256(actual).hexdigest() == digest
            cells.append({'case': name, 'verified': verified})
            if not verified:
                failures.append('#748 selftest full client payload/durable evidence mismatch: ' + name)
    report = {'cases': cells, 'failures': failures, 'node': 'v22.22.0',
              'sourceCommit': oracle.get('sourceCommit'), 'sourceSha256': oracle.get('sourceSha256'),
              'apparatusSha256': oracle.get('apparatusSha256')}
    (output / 'report.json').write_text(json.dumps(report, ensure_ascii=False, indent=2) + '\n')
    print(result.stdout, end='')
    for failure in failures:
        print(failure, file=sys.stderr)
    return 1 if failures else 0


if __name__ == '__main__':
    sys.exit(main())
