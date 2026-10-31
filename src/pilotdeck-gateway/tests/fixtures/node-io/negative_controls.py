#!/usr/bin/env python3
"""#117 negative controls: the Linux errno verification must be anchored.

Run on the Linux host (cb8) only, after the fixture is committed
(``mutating_sources`` refuses untracked files by design). A fresh Node sample
(``linux-errno-verify.mjs``, same pinned Node — ``PDRS_NODE`` when the caller
set it) is produced once, then each mutant is applied to the original
independently and must be killed by the named test's bound assertion; original
bytes and hashes are verified after every mutation. ``CARGO_TARGET_DIR`` must
come from the caller so no second build tree is ever created.
"""
import difflib
import hashlib
import json
import os
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_panic import named_test_killed
from negative_selection import baseline_passed
from negative_process import mutating_sources, run_owned, wait_for_slot

root = Path(__file__).resolve().parents[5]
out = Path(sys.argv[1]).resolve()
out.mkdir(parents=True, exist_ok=False)
generated = root / 'src/pilotdeck-gateway/src/node_io/generated.rs'
fixture = root / 'src/pilotdeck-gateway/tests/fixtures/node-io/errors.json'
node_io = root / 'src/pilotdeck-gateway/src/node_io.rs'
test = 'node_io::tests::linux_node_errno_numbers_match_node_os_constants'

# Every mutant is applied to the original independently and must be killed by
# the named test's bound assertion. Rows are (name, source, old, new) so the
# anchor_selfcheck preflight can statically resolve each row's path and verify
# the old anchor against the live file.
mutations = [
    # The generated table's libc value replaced by a neighboring errno: Node's
    # own os.constants number for EACCES (checked first) no longer matches.
    ('errno-number-eacces',
     generated,
     '    (libc::EACCES, "EACCES", "permission denied"),',
     '    (libc::EPERM, "EACCES", "permission denied"),'),
    # Same numeric drift on a name Node's Linux os.constants table does not
    # carry (cb8 measured null), so this row skips the constant leg and dies
    # on the libuv-number leg instead — both numeric directions stay bound.
    ('errno-number-enonet',
     generated,
     '    (libc::ENONET, "ENONET", "machine is not on the network"),',
     '    (libc::ENOSYS, "ENONET", "machine is not on the network"),'),
    # The committed fixture's description drifts from the fresh sample: the
    # shared-code description comparison of the libuv error map fails.
    ('fixture-description-drift',
     fixture,
     '"uv": -13,\n      "code": "EACCES",\n      "description": "permission denied"',
     '"uv": -13,\n      "code": "EACCES",\n      "description": "permission refused"'),
    # One committed formatting golden drifts: the shared-code comparison of
    # the UVException message tables fails.
    ('formatting-message-drift',
     fixture,
     '"code": "E2BIG",\n      "syscall": "stat",\n      "message": "E2BIG: argument list too long, stat"',
     '"code": "E2BIG",\n      "syscall": "stat",\n      "message": "E2BIG: argument list too long, statt"'),
    # The reverse lookup no longer maps the libc errno to its own code name
    # (it resolves the neighboring entry), so the lookup direction fails.
    ('lookup-direction',
     node_io,
     '        .find(|(number, _, _)| *number == errno)',
     '        .find(|(number, _, _)| *number == errno + 1)'),
]
# A killed mutant must fail through its bound assertion, not by failing to
# compile or by panicking elsewhere; the message proves the intended
# assertion fired.
failure_markers = {
    'errno-number-eacces': '#117 Node os.constants.errno EACCES differs from the generated table',
    'errno-number-enonet': '#117 libuv errno number for ENONET differs from the generated table',
    'fixture-description-drift': '#117 fresh Linux description for EACCES differs from the committed table',
    'formatting-message-drift': '#117 fresh Linux UVException formatting differs from the committed table (shared codes)',
    'lookup-direction': '#117 lookup for E2BIG recovers the wrong code',
}
selected = sys.argv[2:]
if selected:
    mutations = [mutation for mutation in mutations if mutation[0] in selected]
    assert len(mutations) == len(selected), 'unknown mutation name'
if 'CARGO_TARGET_DIR' not in os.environ:
    sys.exit('set CARGO_TARGET_DIR to the caller-owned build tree before running')
env = os.environ.copy()
summary = []
failures = []


def run_named():
    result = run_owned(['cargo', 'test', '--manifest-path', 'src/Cargo.toml',
                        '-p', 'pilotdeck-gateway', '--lib', test,
                        '--', '--exact'],
                       cwd=root, env=env, timeout=3600)
    return result.returncode, result.stdout + result.stderr


# One fresh sample from the pinned Node; both the baseline and every mutant
# read it, so sample drift cannot masquerade as a kill. The pinned binary is
# explicit (PDRS_NODE, exported by run-linux.sh) so no ambient PATH node can
# take its place.
sampler = root / 'src/pilotdeck-gateway/tests/fixtures/node-io/linux-errno-verify.mjs'
sample = out / 'node-errno-sample.json'
result = run_owned([os.environ.get('PDRS_NODE', 'node'), '--expose-internals',
                    str(sampler), str(sample)],
                   cwd=root, env=env, timeout=300)
(out / 'sampler.log').write_text(result.stdout + result.stderr)
assert result.returncode == 0, 'the Node errno sampler failed; see sampler.log'
env['PDRS_NODE_IO_LINUX_JSON'] = str(sample)

with mutating_sources(root, [generated, fixture, node_io]) as snapshots:
    # #819: an unmutated baseline must be green first.
    wait_for_slot(root / 'scripts/cargo-slots.sh')
    code, log = run_named()
    (out / 'baseline.log').write_text(log)
    baseline_ok = code == 0 and baseline_passed(log, test)
    print(f'baseline: {"PASS" if baseline_ok else "INVALID"}', flush=True)
    if not baseline_ok:
        failures.append('baseline')

    for name, path, old, new in mutations:
        original = snapshots[path]
        before = hashlib.sha256(original).hexdigest()
        content = original.decode()
        # A stale anchor (source drifted, or a copy-paste duplicate) must abort
        # the run; recording it as SURVIVED would hide a broken control.
        assert content.count(old) == 1, (
            f'{name}: anchor occurs {content.count(old)} times (expected 1); '
            'the anchor is stale and must be updated')
        changed = content.replace(old, new, 1)
        (out / f'{name}.patch').write_text(''.join(difflib.unified_diff(
            content.splitlines(True), changed.splitlines(True),
            fromfile=str(path.relative_to(root)), tofile=str(path.relative_to(root)))))
        try:
            wait_for_slot(root / 'scripts/cargo-slots.sh')
            path.write_text(changed)
            code, log = run_named()
            (out / f'{name}.log').write_text(log)
            killed = (baseline_ok and code == 101
                      and named_test_killed(log, test, failure_markers[name])
                      and 'could not compile' not in log)
            failure = next((line.strip() for line in log.splitlines()
                            if failure_markers[name] in line), '')
            status = 'KILLED' if killed else (
                'SURVIVED' if code == 0 else 'INVALID')
            summary.append(dict(name=name, test=test, status=status,
                                returncode=code, failure=failure,
                                sourceSha256=before))
            print(f'{name}: {status} rc={code} {failure}', flush=True)
            if status != 'KILLED':
                failures.append(name)
        finally:
            path.write_bytes(original)
            assert hashlib.sha256(path.read_bytes()).hexdigest() == before
            (out / 'summary.json').write_text(
                json.dumps(summary, ensure_ascii=False, indent=2) + '\n')
counts = {state: sum(row['status'] == state for row in summary)
          for state in ('KILLED', 'SURVIVED', 'INVALID')}
print('summary: ' + ', '.join(f'{state}={count}' for state, count in counts.items()), flush=True)
if failures:
    print(f'not killed: {", ".join(failures)}', file=sys.stderr)
    sys.exit(1)
print(f'{len(summary)}/{len(mutations)} killed; every source restored byte-for-byte', flush=True)
