#!/usr/bin/env python3
# anchor-selfcheck: unsupported run-level negative control: dirties one tracked
# source byte, then requires the peak-RSS driver to refuse before it builds.
"""#1555 fix-r2 negative control: a dirty `src/` must never yield a PASS.

`rss_probe_1555.py` builds and measures under the shared tree lock, and refuses
a tree whose `src/` differs from HEAD. The review (minimax concurrency P1)
found the older probe recording `src_dirty=false` while a same-tree negative
control mutated the source in place: the mutant `max-headers-back-to-4096`
shrinks hyper's per-parse slot vectors, so the probe measured a *lower* peak
and the #1555 acceptance item passed on a mutant.

This control dirties one tracked product source (an appended comment, so a
killed run leaves a buildable tree), releases the lock, runs the driver, and
requires exit code 2 with `status=INVALID`, no measured mode, and no build log.
The source is restored and byte-verified against HEAD in a `finally`.

    CARGO_TARGET_DIR=... python3 dirty_tree_control_1555.py NEW_OUTPUT_DIR
"""
import json
import os
from pathlib import Path
import subprocess
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import run_owned, tree_lock

root = Path(__file__).resolve().parents[5]
source = root / 'src/pilotdeck-gateway/src/user_dialog/http_store.rs'
probe = Path(__file__).resolve().parent / 'rss_probe_1555.py'
MARKER = b'\n// #1555 fix-r2 dirty-tree control\n'


def head_bytes():
    relative = source.relative_to(root).as_posix()
    return subprocess.run(['git', 'show', 'HEAD:' + relative],
                          cwd=root, capture_output=True, check=True).stdout


def main():
    if len(sys.argv) != 2:
        raise SystemExit('usage: dirty_tree_control_1555.py NEW_OUTPUT_DIR')
    out = Path(sys.argv[1]).resolve()
    out.mkdir(parents=True, exist_ok=False)
    env = os.environ.copy()
    if not env.get('CARGO_TARGET_DIR'):
        raise SystemExit('CARGO_TARGET_DIR must be provided by the caller')
    original = head_bytes()
    with tree_lock(root, timeout=900):
        if source.read_bytes() != original:
            raise SystemExit('source differs from HEAD before the control; refusing')
        source.write_bytes(original + MARKER)
    result = None
    restored = False
    refused = False
    summary = {}
    try:
        result = run_owned([sys.executable, '-I', str(probe), str(out / 'probe')],
                           cwd=root, env=env, timeout=300)
        (out / 'probe.log').write_text(result.stdout + result.stderr)
        summary_path = out / 'probe' / 'summary.json'
        if summary_path.is_file():
            summary = json.loads(summary_path.read_text())
        refused = (result.returncode == 2
                   and summary.get('status') == 'INVALID'
                   and not summary.get('minima')
                   and not (out / 'probe' / 'build.log').exists())
    finally:
        with tree_lock(root, timeout=900):
            source.write_bytes(original)
            restored = source.read_bytes() == original
    verdict = 'KILLED' if refused and restored else 'SURVIVED'
    record = dict(control='dirty-src-probe-refused', verdict=verdict,
                  returncode=None if result is None else result.returncode,
                  probe_status=summary.get('status'),
                  probe_failures=summary.get('failures'),
                  built=bool((out / 'probe' / 'build.log').exists()),
                  restored=restored)
    (out / 'summary.json').write_text(json.dumps(record, indent=2) + '\n')
    print(json.dumps(record), flush=True)
    return 0 if verdict == 'KILLED' else 1


if __name__ == '__main__':
    raise SystemExit(main())
