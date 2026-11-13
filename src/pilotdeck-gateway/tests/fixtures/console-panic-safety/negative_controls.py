#!/usr/bin/env python3
"""Temporary #2558 guard mutations; require the bound guard to fail, then restore.

Run with no other writers/builds in this worktree. Evidence goes in a new
output folder; original source bytes are verified by ``mutating_sources``
(HEAD baseline — this file must be committed before it can run). Each mutant
is recorded as KILLED / SURVIVED / INVALID and the script exits non-zero
unless every selected mutant is KILLED.

Bound guards (all in ``src/pilotdeck-gateway/tests/console_panic_safety.rs``):

* ``error_console_writes_stay_non_panicking`` — the listed-file scan
* ``shared_writer_call_sites_stay_listed`` — the derived listing rule
* ``production_tree_stays_eprintln_free`` — fix-r3's whole-tree production
  scan plus its listed-file anchor
* ``assert_no_eprintln_kills_a_real_eprintln`` — the detector self-test
  (fix-r4: the three-macro match set)

Use: CARGO_TARGET_DIR=... python3 -I -B .../negative_controls.py <new output
directory> [mutation name ...] (no names: every mutant).
"""
import difflib
import hashlib
import json
import os
from pathlib import Path
import sys

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_panic import named_test_failed, panic_evidence
from negative_selection import (baseline_passed, claim_dir, missing_target,
                                 refuse, refuse_unusable_target, registered,
                                 select, verdict_exit)
from negative_process import (CARGO_SLOTS_WAIT_SECONDS, mutating_sources,
                               run_owned, wait_for_slot)

ROOT = Path(__file__).resolve().parents[5]
TLS = 'src/pilotdeck-gateway/src/runtime/model/http/tls.rs'
GUARD = 'src/pilotdeck-gateway/tests/console_panic_safety.rs'
ENCODING = 'src/pilotdeck-gateway/src/runtime/model/http/encoding.rs'
MAIN = 'src/pilotdeck-gateway/src/main.rs'

LIST_SCAN = 'error_console_writes_stay_non_panicking'
LISTING_RULE = 'shared_writer_call_sites_stay_listed'
TREE_SCAN = 'production_tree_stays_eprintln_free'
DETECTOR_SELF_TEST = 'assert_no_eprintln_kills_a_real_eprintln'

# Each mutant is applied to the original, never cumulatively. #2558 fix-r3
# review P1: the tls-warn-line-reverted / tls-guard-unlisted pair pins the
# listed-file scan and the listing rule over the new tls.rs entry; the tree
# mutants pin the whole-tree scan, its listed-file anchor, and both
# directions of the test-support exclusion. fix-r4 review P1-1: the
# stdout-banner-reverted / guard-stdout-macros-dropped pair pins the
# stdout face of the same scans — the entry's banner lines reverting to
# bare `println!` must turn the listed-file scan red, and the detector's
# three-macro match set reverting to `eprintln` alone must turn the
# self-test red.
mutations = [
    ('tls-warn-line-reverted', TLS,
     '            // #2558: the load_pem warn line\'s non-panicking shape.\n'
     '            crate::native_process::channels::console_log_line(\n'
     '                &mut std::io::stderr(),\n'
     '                &format!(\n'
     '                    "Warning: could not read CA directory {}: {error}",\n'
     '                    dir.display()\n'
     '                ),\n'
     '            );',
     '            eprintln!(\n'
     '                "Warning: could not read CA directory {}: {error}",\n'
     '                dir.display()\n'
     '            );'),
    ('tls-guard-unlisted', GUARD,
     '    "src/lib.rs",\n    "src/runtime/model/http/tls.rs",\n'
     '    "src/host/tests/resume.rs",',
     '    "src/lib.rs",\n    "src/main.rs",\n'
     '    "src/host/tests/resume.rs",'),
    ('stdout-banner-reverted', MAIN,
     '    // #2558: best-effort console lines, like TS\'s `console.log` — a raw\n'
     '    // `println!` here panics when the write fails (std behavior), and these\n'
     '    // banners run on the startup path inside the main task: a closed stdout\n'
     '    // would kill the listener right after the bind, before the accept loop\n'
     '    // answers a connection. The stdout twin of the metadata line below (the\n'
     '    // stderr half of the same fix, `refresh.rs`\'s `log` default shape).\n'
     '    pilotdeck_gateway::native_process::console_log_line(\n'
     '        &mut std::io::stdout(),\n'
     '        &format!("PilotDeck server listening: http://{address}"),\n'
     '    );\n'
     '    pilotdeck_gateway::native_process::console_log_line(\n'
     '        &mut std::io::stdout(),\n'
     '        &format!("WebSocket: ws://{address}/ws"),\n'
     '    );',
     '    println!("PilotDeck server listening: http://{address}");\n'
     '    println!("WebSocket: ws://{address}/ws");'),
    ('guard-stdout-macros-dropped', GUARD,
     '            SourceToken::Ident(word)\n'
     '                if word == "eprintln" || word == "println" || word == "print" =>\n'
     '            {',
     '            SourceToken::Ident(word) if word == "eprintln" => {'),
    ('unlisted-file-eprintln', ENCODING,
     'pub(crate) fn decode_response(',
     'fn negative_control_eprintln_probe() {\n'
     '    eprintln!("#2558 negative control: an unlisted production eprintln");\n'
     '}\n\n'
     'pub(crate) fn decode_response('),
    ('tree-walk-no-descend', GUARD,
     '                stack.push(path);',
     '                drop(path);'),
    ('exclusion-widened-to-everything', GUARD,
     '    name == "tests.rs" || name.ends_with("_tests.rs") || name.starts_with("test_")',
     '    true'),
    ('exclusion-narrowed-test-family', GUARD,
     '    name == "tests.rs" || name.ends_with("_tests.rs") || name.starts_with("test_")',
     '    name == "tests.rs" || name.ends_with("_tests.rs")'),
]
# #1666: static denominator. `len(mutations)` is not a denominator: deleting a
# row shortens the run silently, and an emptied table prints `0/0 killed`.
EXPECTED_NAMES = (
    'tls-warn-line-reverted', 'tls-guard-unlisted', 'stdout-banner-reverted',
    'guard-stdout-macros-dropped', 'unlisted-file-eprintln',
    'tree-walk-no-descend', 'exclusion-widened-to-everything',
    'exclusion-narrowed-test-family',
)
# The bound guard and the assertion-message fragment that must appear in
# that guard's own panic payload (negative_panic.panic_evidence scope).
CHECKS = {
    'tls-warn-line-reverted': (
        LIST_SCAN,
        'src/runtime/model/http/tls.rs still writes an error console line'),
    'tls-guard-unlisted': (
        LISTING_RULE,
        'names the shared writer but is not in'),
    'stdout-banner-reverted': (
        LIST_SCAN,
        'src/main.rs still writes an error console line'),
    'guard-stdout-macros-dropped': (
        DETECTOR_SELF_TEST,
        '#2558 the detector must panic on a real println! site'),
    'unlisted-file-eprintln': (
        TREE_SCAN,
        'src/runtime/model/http/encoding.rs still writes an error console line'),
    'tree-walk-no-descend': (
        TREE_SCAN,
        'the production walk skipped the #2558 source '
        'src/runtime/memory/maintenance.rs'),
    'exclusion-widened-to-everything': (
        TREE_SCAN,
        'the production walk skipped the #2558 source src/main.rs'),
    'exclusion-narrowed-test-family': (
        TREE_SCAN,
        'src/test_process.rs still writes an error console line'),
}
selected = sys.argv[2:]
# #1666: ordinary raises, so `python3 -O` cannot strip these guards.
try:
    registered(mutations, EXPECTED_NAMES)
    # `mutations` stays the frozen literal so `anchor_selfcheck.py` can read it
    # statically; the requested subset is a separate name.
    run_rows = select(mutations, selected, always=())
except ValueError as error:
    refuse(str(error))
env = os.environ.copy()
if missing_target(env):  # #1666: absent, empty or blank, one shared predicate
    refuse('CARGO_TARGET_DIR must be supplied by the caller')
refuse_unusable_target()  # #1666 fix-r9: dot / root / src are unusable too
out = claim_dir(sys.argv[1])
summary = []
failures = []


def run(full):
    result = run_owned(['cargo', 'test', '--manifest-path', 'src/Cargo.toml',
                        '-p', 'pilotdeck-gateway', '--test',
                        'console_panic_safety', full, '--', '--exact'],
                       cwd=ROOT, env=env, timeout=900)
    return result.returncode, result.stdout + result.stderr


with mutating_sources(ROOT, sorted({ROOT / row[1] for row in run_rows})) as snapshots:
    def source_path(relative):
        return ROOT / relative

    # #819: an unmutated baseline for every named target must be green first.
    baseline_ok = set()
    for test in dict.fromkeys(CHECKS[name][0] for name, *_ in run_rows):
        # #1908: the shared wait replaces the inline 300 s-sample /
        # 1800 s-deadline loop; the budget is the shared constant.
        slots = wait_for_slot(ROOT / 'scripts/cargo-slots.sh',
                              timeout=CARGO_SLOTS_WAIT_SECONDS)
        (out / f'slots-baseline-{test}.log').write_text(slots.stdout + slots.stderr)
        code, log = run(test)
        (out / f'baseline-{test}.log').write_text(log)
        ok = code == 0 and baseline_passed(log, test)
        print(f'baseline {test}: {"PASS" if ok else "INVALID"}', flush=True)
        if ok:
            baseline_ok.add(test)
        else:
            failures.append('baseline-' + test)

    for name, relative, old, new in run_rows:
        path = source_path(relative)
        original = snapshots[path]
        before = hashlib.sha256(original).hexdigest()
        test, message = CHECKS[name]
        try:
            occurrences = original.decode().count(old)
            if occurrences != 1:
                raise ValueError(
                    f'{name}: anchor occurs {occurrences} times (expected 1)')
            changed = original.decode().replace(old, new, 1)
            (out / f'{name}.patch').write_text(''.join(difflib.unified_diff(
                original.decode().splitlines(True),
                changed.splitlines(True),
                fromfile=relative, tofile=relative)))
            # #1908: same shared-wait migration as the baseline loop above.
            slots = wait_for_slot(ROOT / 'scripts/cargo-slots.sh',
                                 timeout=CARGO_SLOTS_WAIT_SECONDS)
            (out / f'{name}.slots.log').write_text(slots.stdout + slots.stderr)
            path.write_text(changed)
            code, log = run(test)
            (out / f'{name}.log').write_text(log)
            evidence = panic_evidence(log, test)
            marker_ok = message in evidence
            killed = (test in baseline_ok and code == 101
                      and named_test_failed(log, test)
                      and marker_ok
                      and 'could not compile' not in log)
            failure = next((line.strip() for line in log.splitlines()
                            if 'panicked at' in line or 'assertion' in line), '')
            status = 'KILLED' if killed else (
                'SURVIVED' if code == 0 else 'INVALID')
            summary.append(dict(name=name, test=test, status=status,
                                returncode=code, failure=failure,
                                sourceSha256=before))
            print(f'{name}: {status} rc={code} {failure}', flush=True)
            if status != 'KILLED':
                failures.append(name)
        except Exception as error:
            summary.append(dict(name=name, test=test, status='INVALID',
                                reason=f'{type(error).__name__}: {error}',
                                sourceSha256=before))
            failures.append(name)
        finally:
            path.write_bytes(original)
            restored = hashlib.sha256(path.read_bytes()).hexdigest()
            if restored != before:
                raise RuntimeError(
                    f'{name}: restore failed ({restored} != {before}); the tree is dirty')
            (out / 'summary.json').write_text(json.dumps(summary, ensure_ascii=False, indent=2) + '\n')
counts = {state: sum(row['status'] == state for row in summary)
          for state in ('KILLED', 'SURVIVED', 'INVALID')}
print('summary: ' + ', '.join(f'{state}={count}' for state, count in counts.items()), flush=True)
if failures:
    print(f'not killed: {", ".join(failures)}', file=sys.stderr)
killed_rows = sum(1 for row in summary if row['status'] == 'KILLED')
print(f'{killed_rows}/{len(run_rows)} killed; every source restored byte-for-byte', flush=True)
# #1666: a zero-row run is never success (`0/0 killed` used to exit 0).
sys.exit(verdict_exit(summary, key='status'))
