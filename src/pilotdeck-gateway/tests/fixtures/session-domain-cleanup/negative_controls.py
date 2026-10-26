#!/usr/bin/env python3
"""Post-commit source negative controls for #164.

This runner mutates the tracked apparatus source in place and requires the
behavior check to turn red.  It must run only after the #164 changes are
committed: ``mutating_sources`` proves the named file is byte-identical to
HEAD before writing anything, and refuses otherwise.  The new check/launcher
files beside this script are not mutated, so their pre-commit absence from HEAD
does not matter.

Run on the coordinator's host (no cargo is launched, so CARGO_TARGET_DIR is not
required):

  python3 -I src/pilotdeck-gateway/tests/fixtures/session-domain-cleanup/negative_controls.py \
    --output _tmp/20261026-issue-164/negative/source

Exit codes: 0 every mutant killed and the restored source passes; 1 a mutant
survived or the restored source failed; 2 an anchor is stale (refuse to claim
SURVIVED).
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile

sys.pycache_prefix = tempfile.mkdtemp(prefix='pdrs-session-cleanup-neg-pyc-')
sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned

ROOT = Path(__file__).resolve().parents[5]
SOURCE = ROOT / 'scripts/gateway-parity/session_domain.py'
CHECK = Path(__file__).resolve().with_name('check_cleanup.py')

# (name, old, new, mode).  Every old block must occur exactly once.
MUTATIONS = (
    ('empty-group-probe-removed',
     "    try:\n        os.killpg(pgid, 0)\n    except ProcessLookupError:\n"
     "        return {**result, 'status': CLEANUP_VERIFIED}\n"
     "    except OSError as error:\n"
     "        return {**result, 'status': CLEANUP_BLOCKED, 'reason': str(error)}",
     "    try:\n        raise PermissionError('mutant: empty-group probe removed')\n"
     "    except ProcessLookupError:\n"
     "        return {**result, 'status': CLEANUP_VERIFIED}\n"
     "    except OSError as error:\n"
     "        return {**result, 'status': CLEANUP_BLOCKED, 'reason': str(error)}",
     'clean'),
    ('cleanup-status-write-removed',
     "            audit['processTreeCleanup'] = audit['processGroupProbe']['status']",
     "            audit['processTreeCleanup'] = CLEANUP_VERIFIED",
     'leak'),
    ('top-level-mapping-relaxed',
     "    return ('PASS' if audit.get('processTreeCleanup') == CLEANUP_VERIFIED\n"
     "            else CLEANUP_UNVERIFIED_STATUS)",
     "    return 'PASS'",
     'status'),
    ('sigterm-removed',
     "    for sig in (signal.SIGTERM, signal.SIGKILL):",
     "    for sig in (signal.SIGKILL,):",
     'leak'),
    ('blocked-branch-removed',
     "    except OSError as error:\n        return {**result, 'status': CLEANUP_BLOCKED, 'reason': str(error)}",
     "    except OSError:\n        raise",
     'blocked'),
    ('sigkill-escalation-removed',
     "    for sig in (signal.SIGTERM, signal.SIGKILL):",
     "    for sig in (signal.SIGTERM,):",
     'ignore'),
)


def pgrep_launcher():
    process = subprocess.run(
        ['pgrep', '-fl', 'session-domain-[c]leanup/launcher.py'],
        capture_output=True, text=True)
    return process.stdout


def positive_check(output, label):
    folder = output / label
    folder.mkdir(parents=True, exist_ok=False)
    result = run_owned([sys.executable, '-I', str(CHECK), '--output', str(folder),
                        '--mode', 'all'], cwd=ROOT,
                       env=dict(os.environ), timeout=180)
    (output / f'{label}.log').write_text(result.stdout + result.stderr)
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    (output / 'pgrep-before.txt').write_text(pgrep_launcher())
    summary = []
    with mutating_sources(ROOT, [SOURCE]) as snapshots:
        original = snapshots[SOURCE]
        before = hashlib.sha256(original).hexdigest()
        content = original.decode()
        try:
            for name, old, new, mode in MUTATIONS:
                count = content.count(old)
                if count != 1:
                    print(json.dumps({'status': 'ANCHOR_EXPIRED', 'mutation': name,
                                      'anchorCount': count}, ensure_ascii=False))
                    return 2
                changed = content.replace(old, new, 1)
                case = output / name
                case.mkdir(parents=True, exist_ok=False)
                SOURCE.write_text(changed)
                try:
                    run = run_owned([sys.executable, '-I', str(CHECK), '--output',
                                     str(case), '--mode', mode], cwd=ROOT,
                                    env=dict(os.environ), timeout=180)
                    log = run.stdout + run.stderr
                    (output / f'{name}.log').write_text(log)
                    killed = run.returncode != 0 and '"status": "FAIL"' in log
                    summary.append({'mutation': name, 'mode': mode, 'killed': killed,
                                    'returncode': run.returncode})
                    print(f'{name}: {"KILLED" if killed else "SURVIVED"} '
                          f'rc={run.returncode}', flush=True)
                    if not killed:
                        return 1
                finally:
                    SOURCE.write_bytes(original)
                    if hashlib.sha256(SOURCE.read_bytes()).hexdigest() != before:
                        raise RuntimeError('source restore hash mismatch')
        finally:
            SOURCE.write_bytes(original)
        restored = positive_check(output, 'restored-positive')
        (output / 'pgrep-after.txt').write_text(pgrep_launcher())
        restored_ok = restored.returncode == 0 and '"status": "PASS"' in (restored.stdout + restored.stderr)
        (output / 'summary.json').write_text(json.dumps({
            'status': 'PASS' if restored_ok and len(summary) == len(MUTATIONS) else 'FAIL',
            'sourceSha256': before,
            'mutations': summary,
            'restoredPass': restored_ok,
        }, ensure_ascii=False, indent=2) + '\n')
        print(f'restored positive: {"PASS" if restored_ok else "FAIL"} '
              f'rc={restored.returncode}', flush=True)
        return 0 if restored_ok and len(summary) == len(MUTATIONS) else 1


if __name__ == '__main__':
    raise SystemExit(main())
