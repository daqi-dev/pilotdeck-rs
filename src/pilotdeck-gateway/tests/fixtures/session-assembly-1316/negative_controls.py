#!/usr/bin/env python3
"""#1316 A7.8-pr1 session-domain assembly: product mutations for the wire-up.

Every mutant removes one piece of the assembly (a registry entry, a describe
capability, the always-on family registration, the explicit HTTP same-domain
entry) and must be killed by the named oracle test in the same process, with
the marker of the mutant inside that test's own panic payload. Original source
bytes and hashes are verified after every mutation; the summary is emitted
only after all mutants have run.
"""
import difflib
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import time

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_panic import named_test_failed, panic_evidence
from negative_selection import baseline_passed
from negative_process import mutating_sources, run_owned

root = Path(__file__).resolve().parents[5]
source = root / 'src/pilotdeck-gateway/src'
out = Path(sys.argv[1]).resolve()
out.mkdir(parents=True, exist_ok=False)

# (name, file under src/pilotdeck-gateway/src, old bytes, replacement,
#  full libtest name, required panic-payload marker). Each mutant is applied
# to the original tree independently, never cumulatively.
mutations = [
    ('registry-drops-reconnect-interaction',
     'native_process/methods/interaction.rs',
     '    "reconnect_interaction",\n    "elicitation_respond",',
     '    "elicitation_respond",',
     'native_process::methods::tests::session_domain_reconciles_with_coverage_manifest',
     '#1316 A7 method reconnect_interaction is dispatched'),
    ('describe-drops-permission-decide',
     'native_process/requests.rs',
     '                            "permission_decide",\n'
     '                            "async_hook_result",',
     '                            "async_hook_result",',
     'native_process::management_tests::production_host_management_and_restart',
     '#1231 exact wired capability set'),
    # Dropping the registry entry sends `always_on_apply` to the
    # `metadata::unavailable` fallback, whose code is the SAME
    # CAPABILITY_UNAVAILABLE the family returns (metadata.rs:69). The code
    # assertion therefore passes on this mutant; the family-specific message
    # assertion is the discriminator that must panic, so the required marker
    # names it. The test still asserts the code first for every other mutant
    # (the marker here is evidence, not a weakening: any registry drop still
    # fails the run).
    ('registry-drops-always-on',
     'native_process/methods/mod.rs',
     '    interaction: interaction::METHODS,\n    always_on: always_on::METHODS,',
     '    interaction: interaction::METHODS,',
     'native_process::methods::always_on::tests::always_on_methods_answer_capability_unavailable',
     '#1316 always_on_apply message'),
    ('http-store-branch-removed',
     'native_http.rs',
     '    if path.starts_with(crate::user_dialog::http_store::STORE_PATH_PREFIX) {\n'
     '        return Ok(handler_response(\n'
     '            hyper::StatusCode::SERVICE_UNAVAILABLE,\n'
     '            "application/json",\n'
     '            "{\\"error\\":\\"not_yet_open\\",\\"store\\":\\"gateway-user-dialog-store\\"}"'
     '.to_owned(),\n'
     '            keep_alive,\n'
     '        ));\n'
     '    }\n',
     '',
     'native_http::tests::user_dialog_store_entry_is_explicitly_not_yet_open',
     '#1316 user-dialog store entry is a 503'),
]
selected = sys.argv[2:]
if selected:
    mutations = [mutation for mutation in mutations if mutation[0] in selected]
    assert len(mutations) == len(selected), 'unknown mutation name'
env = os.environ.copy()
if not env.get('CARGO_TARGET_DIR'):
    sys.exit('CARGO_TARGET_DIR must be supplied by the caller')
summary = []
failures = []


def acquire_slot():
    slot_deadline = time.monotonic() + 1800
    while True:
        slots = subprocess.run([str(root / 'scripts/cargo-slots.sh')], cwd=root,
                               capture_output=True, text=True, timeout=10)
        if slots.returncode != 1:
            assert slots.returncode == 0
            return slots
        assert time.monotonic() < slot_deadline, 'cargo slots unavailable for 30 minutes'
        time.sleep(5)


def run(full):
    # No -q/--quiet: libtest must print `test <name> ... ok` lines (#273, #978).
    result = run_owned(
        ['cargo', 'test', '--manifest-path', 'src/Cargo.toml', '-p',
         'pilotdeck-gateway', '--lib', full, '--', '--exact'],
        cwd=root, env=env, timeout=900)
    return result.returncode, result.stdout + result.stderr


def wait_for_slot(name):
    slots = acquire_slot()
    (out / f'{name}.slots.log').write_text(slots.stdout + slots.stderr)


with mutating_sources(root, sorted({source / filename for _, filename, _, _, _, _ in mutations})) as snapshots:
    # #819: an unmutated baseline of every named target must be green first.
    baseline_ok = set()
    for full in dict.fromkeys(test for *_, test, _ in mutations):
        wait_for_slot('baseline-' + full.rsplit('::', 1)[-1])
        code, log = run(full)
        (out / f'baseline-{full.rsplit("::", 1)[-1]}.log').write_text(log)
        ok = code == 0 and baseline_passed(log, full)
        print(f'baseline {full}: {"PASS" if ok else "INVALID"}', flush=True)
        if ok:
            baseline_ok.add(full)
        else:
            failures.append('baseline-' + full)

    for name, filename, old, new, full, marker in mutations:
        path = source / filename
        original = snapshots[path]
        before = hashlib.sha256(original).hexdigest()
        content = original.decode()
        try:
            # A stale anchor must abort as INVALID, never count as SURVIVED.
            assert content.count(old) == 1, (name, content.count(old))
            changed = content.replace(old, new, 1)
            (out / f'{name}.patch').write_text(''.join(difflib.unified_diff(
                content.splitlines(True), changed.splitlines(True),
                fromfile=filename, tofile=filename)))
            wait_for_slot(name)
            path.write_text(changed)
            code, log = run(full)
            (out / f'{name}.log').write_text(log)
            evidence = panic_evidence(log, full)
            killed = (full in baseline_ok and code == 101
                      and named_test_failed(log, full)
                      and marker in evidence
                      and 'could not compile' not in log)
            failure = next((line.strip() for line in log.splitlines()
                            if 'panicked at' in line or 'assertion' in line), '')
            status = 'KILLED' if killed else (
                'SURVIVED' if code == 0 else 'INVALID')
            summary.append(dict(name=name, test=full, status=status,
                                returncode=code, failure=failure,
                                sourceSha256=before))
            print(f'{name}: {status} rc={code} {failure}', flush=True)
            if status != 'KILLED':
                failures.append(name)
        except Exception as error:
            summary.append(dict(name=name, test=full, status='INVALID',
                                reason=f'{type(error).__name__}: {error}',
                                sourceSha256=before))
            failures.append(name)
        finally:
            path.write_bytes(original)
            assert hashlib.sha256(path.read_bytes()).hexdigest() == before
# The guarded window closed here: `mutating_sources` restored and re-verified
# every source on context exit. Only now publish the single summary, atomically
# (temp + rename, #166 r8), so no reader can see a completed-looking KILLED
# list while a later mutant is still applied or a restore is still unverified
# (#979 P0 / #1076). A cancellation before this point leaves no summary at all,
# and a missing summary is fail-closed.
temporary = out / 'summary.json.tmp'
temporary.write_text(json.dumps(summary, ensure_ascii=False, indent=2) + '\n')
temporary.replace(out / 'summary.json')
counts = {state: sum(row['status'] == state for row in summary)
          for state in ('KILLED', 'SURVIVED', 'INVALID')}
print('summary: ' + ', '.join(f'{state}={count}' for state, count in counts.items()), flush=True)
if failures:
    print(f'not killed: {", ".join(failures)}', file=sys.stderr)
    sys.exit(1)
print(f'{len(summary)}/{len(mutations)} killed; every source restored byte-for-byte', flush=True)
