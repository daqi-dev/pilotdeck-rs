#!/usr/bin/env python3
# anchor-selfcheck: unsupported guarded manifest/event data controls; no checker source mutations
"""#1089 real CLI controls, after both source and generated lists are committed.

Usage: python3 -I test_cli.py BASE_REF NEW_OUTPUT_DIRECTORY
The base must predate this issue's checker changes. Every data mutation is
HEAD-guarded and restored; no fixture repository or altered bootstrap is used.
"""
import json
from pathlib import Path
import re
import subprocess
import sys

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot

ROOT = Path(__file__).resolve().parents[5]


def main():
    if len(sys.argv) != 3:
        raise SystemExit('#1089 expected BASE_REF and a fresh output directory')
    base, destination = sys.argv[1:]
    out = Path(destination).resolve()
    out.mkdir(parents=True, exist_ok=False)
    results = []
    for kind in ('parity', 'gateway-parity', 'ui-server-parity'):
        checker = ROOT / f'scripts/{kind}/check_frozen.py'
        folder = ROOT / f'specs/{kind}'
        command = [sys.executable, '-I', str(checker), '--base', base]
        for control in ('missing-event', 'manual-hash'):
            status, reason = 'INVALID', 'not run'
            try:
                positive = run_owned(command, cwd=ROOT, timeout=60)
                save(out, kind + '-' + control + '-positive', positive)
                if positive.returncode:
                    raise RuntimeError('#1089 real CLI positive failed')
                target = folder / ('frozen/20261030-1089.md' if control == 'missing-event' else 'FROZEN.md')
                with mutating_sources(ROOT, [target]) as originals:
                    if control == 'missing-event':
                        target.unlink()
                    else:
                        raw = originals[target]
                        # Exact registered checker row, excluding prose and other hashes.
                        pattern = (rb'(\| `scripts/parity/check_frozen\.py` \| `)([0-9a-f]{64})(`)' if kind == 'parity'
                                   else rb'^([0-9a-f]{64})(  \.\./\.\./scripts/' + kind.encode() + rb'/check_frozen\.py)$')
                        matches = list(re.finditer(pattern, raw, re.M))
                        if len(matches) != 1:
                            raise RuntimeError('#1089 stale manual-hash anchor')
                        match = matches[0]
                        start, end = match.span(2 if kind == 'parity' else 1)
                        pin = raw[start:end]
                        changed = (b'0' if pin[:1] != b'0' else b'1') + pin[1:]
                        target.write_bytes(raw[:start] + changed + raw[end:])
                    negative = run_owned(command, cwd=ROOT, timeout=60)
                    save(out, kind + '-' + control + '-negative', negative)
                    output = negative.stdout + negative.stderr
                    expected = ('lacks exact new event chain' if control == 'missing-event' else
                                'unsafe apparatus' if kind == 'ui-server-parity' else 'hash list differs')
                    if negative.returncode == 1 and expected in output:
                        status, reason = 'KILLED', expected
                    elif negative.returncode == 0:
                        status, reason = 'SURVIVED', 'real CLI still passed'
                    else:
                        reason = 'unexpected failure: ' + output[-500:]
                restored = run_owned(command, cwd=ROOT, timeout=60)
                save(out, kind + '-' + control + '-restored', restored)
                if restored.returncode:
                    status, reason = 'INVALID', 'restored real CLI failed'
            except Exception as error:
                status, reason = 'INVALID', str(error)
            results.append(dict(apparatus=kind, control=control, status=status, reason=reason))
            print(kind, control, status, reason, flush=True)
    counts = {status: sum(item['status'] == status for item in results)
              for status in ('KILLED', 'SURVIVED', 'INVALID')}
    temporary = out / 'summary.json.tmp'
    temporary.write_text(json.dumps({'controls': results, 'counts': counts}, ensure_ascii=False, indent=2) + '\n')
    temporary.replace(out / 'summary.json')
    print(json.dumps(counts), flush=True)
    return int(any(item['status'] != 'KILLED' for item in results))


def save(out, name, result):
    for suffix, data in (('stdout', result.stdout), ('stderr', result.stderr), ('exit', str(result.returncode) + '\n')):
        (out / (name + '.' + suffix)).write_text(data)


if __name__ == '__main__':
    raise SystemExit(main())
