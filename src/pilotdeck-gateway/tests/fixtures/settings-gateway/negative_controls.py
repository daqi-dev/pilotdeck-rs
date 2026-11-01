#!/usr/bin/env python3
"""Temporary product mutations of the settings gateway adapter; each must be
killed by the oracle replay test's own #1318 panic markers, then restore.

Run with no other writers/builds in this worktree. Evidence goes in a new
output folder; original source bytes and hashes are verified after every
mutation. The five mutants all target
``src/pilotdeck-gateway/src/runtime/settings/gateway.rs``:

* ``apply-busy-gate-removed`` — the SESSION_BUSY gate never fires; the
  ``apply-busy`` row must answer with the error frame, not a success.
* ``update-capture-ignored`` — the store event's ``changedPaths`` no longer
  replaces the writer's; ``update-merges-store-paths`` pins the merge.
* ``reload-config-unsupported-flipped`` — the bare-assembly projection claims
  ``reloaded: true``; ``assemblyCases.capability`` pins ``false``.
* ``dispatch-drops-reload-extensions`` — the dispatch entry no longer owns
  ``reload_extensions``; the replay's dispatch-ownership panic pins it.
* ``apply-close-skipped`` — the post-apply session close disappears;
  ``apply-success-closes`` pins the evict/dispose event tail.

The mutated files are new in this branch, so the runner must execute after
the branch is committed (``mutating_sources`` requires ``HEAD`` bytes);
``CARGO_TARGET_DIR`` must be supplied by the caller.
"""
import difflib
import hashlib
import json
import os
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_panic import named_test_failed, panic_evidence
from negative_selection import baseline_passed
from negative_process import mutating_sources, run_owned, wait_for_slot

root = Path(__file__).resolve().parents[5]
out = Path(sys.argv[1]).resolve()
out.mkdir(parents=True, exist_ok=False)
source = root / 'src/pilotdeck-gateway/src/runtime/settings'
REPLAY = 'runtime::settings::tests::settings_gateway_replays_the_real_ts_oracle'
# Each row: (name, file, original anchor text, mutant text). The kill marker
# must appear inside the replay test's own panic payload.
mutations = [
    ('apply-busy-gate-removed', 'gateway.rs',
     'if self.options.owner.has_active_turn(&session_key) {',
     'if false && self.options.owner.has_active_turn(&session_key) {'),
    ('update-capture-ignored', 'gateway.rs',
     'let changed_paths = std::mem::take(&mut *lock(&captured));',
     'let changed_paths = Vec::new();'),
    ('reload-config-unsupported-flipped', 'gateway.rs',
     'object.insert("reloaded".into(), JsValue::Bool(false));',
     'object.insert("reloaded".into(), JsValue::Bool(true));'),
    ('dispatch-drops-reload-extensions', 'gateway.rs',
     '"reload_extensions" => self.reload_extensions(params).await,',
     '"reload_extensions_disabled" => self.reload_extensions(params).await,'),
    ('apply-close-skipped', 'gateway.rs',
     'if !result.applied.is_empty() || !result.cleared.is_empty() {',
     'if false {'),
]
MARKERS = {
    'apply-busy-gate-removed': 'case apply-busy',
    'update-capture-ignored': 'case update-merges-store-paths',
    'reload-config-unsupported-flipped': 'the bare assembly must answer reload_config',
    'dispatch-drops-reload-extensions': 'dispatch must own the five settings methods',
    'apply-close-skipped': 'case apply-success-closes',
}
selected = sys.argv[2:]
if selected:
    mutations = [mutation for mutation in mutations if mutation[0] in selected]
    assert len(mutations) == len(selected), 'unknown mutation name'
env = os.environ.copy()
if not env.get('CARGO_TARGET_DIR'):
    sys.exit('CARGO_TARGET_DIR must be supplied by the caller')
summary = []
failures = []
slot_script = root / 'scripts/cargo-slots.sh'


with mutating_sources(root, sorted({source / filename for _, filename, *_ in mutations})) as snapshots:
    def run():
        result = run_owned(['cargo', 'test', '--manifest-path', 'src/Cargo.toml',
                            '-p', 'pilotdeck-gateway', '--lib', REPLAY,
                            '--', '--exact'], cwd=root, env=env, timeout=240)
        return result.returncode, result.stdout + result.stderr

    # An unmutated baseline must be green first, or every verdict is INVALID.
    wait_for_slot(slot_script)
    code, log = run()
    (out / 'baseline-replay.log').write_text(log)
    if code == 0 and baseline_passed(log, REPLAY):
        print('baseline replay: PASS', flush=True)
    else:
        print('baseline replay: INVALID', flush=True)
        sys.exit(1)

    for name, filename, old, new in mutations:
        marker = MARKERS[name]
        path = source / filename
        original = snapshots[path]
        before = hashlib.sha256(original).hexdigest()
        try:
            content = original.decode()
            # An expired anchor is an error exit, never a SURVIVED verdict.
            assert content.count(old) == 1, (name, content.count(old))
            changed = content.replace(old, new, 1)
            (out / f'{name}.patch').write_text(''.join(difflib.unified_diff(
                content.splitlines(True), changed.splitlines(True),
                fromfile=filename, tofile=filename)))
            wait_for_slot(slot_script)
            path.write_text(changed)
            code, log = run()
            (out / f'{name}.log').write_text(log)
            evidence = panic_evidence(log, REPLAY)
            killed = (code == 101
                      and named_test_failed(log, REPLAY)
                      and marker in evidence
                      and '#1318' in evidence
                      and 'could not compile' not in log)
            failure = next((line.strip() for line in log.splitlines()
                            if 'panicked at' in line or 'assertion' in line), '')
            status = 'KILLED' if killed else (
                'SURVIVED' if code == 0 else 'INVALID')
            summary.append(dict(name=name, test=REPLAY, status=status,
                                returncode=code, failure=failure,
                                marker=marker, sourceSha256=before))
            print(f'{name}: {status} rc={code} {failure}', flush=True)
            if status != 'KILLED':
                failures.append(name)
        except Exception as error:
            summary.append(dict(name=name, test=REPLAY, status='INVALID',
                                reason=f'{type(error).__name__}: {error}',
                                sourceSha256=before))
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
