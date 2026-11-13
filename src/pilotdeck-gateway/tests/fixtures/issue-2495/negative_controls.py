#!/usr/bin/env python3
"""#2495: the note-pass and hasInspectable-accumulation mutations.

Usage: negative_controls.py --check
       negative_controls.py [--compile-only] NEW_OUTPUT_DIR [mutation ...]
Every mutation must compile, fail at its bound nonce receipt, and pass again
after restoration. Source bytes must equal HEAD; no alternate baseline exists.
Each bound assertion is a static message: the receipt protocol compares the
runtime `format!` result byte-for-byte, so an interpolated template never
matches and the row can only come out NO_RECEIPT (fix-r2 review P1). The
execution table is verified against the static EXPECTED_NAMES roster with the
shared `negative_selection.registered` in every mode (--check, the default
full run, and an explicit subset) before anything runs, so a row dropped from
`mutations` cannot silently shrink the denominator, and a drift is a
never-started refusal (exit 2), not a failed control. The four domain rows pin
the #2495 boundary the TS probe fixed
(`_tmp/20261026-issue-2495/probe/probe.out`): hasInspectable is a separate
`attachments.some(...)` over the whole attachment list (never gated by the
note dedup, counting an unregistered entry whose spelling is a member of
allowedReadFiles, and never counting an entry whose spelling is not), while
the note line itself still dedups on the lexical spelling. The membership
rows pin the two gates the pass hangs on (the note-line side and the
hasInspectable side), the always-true row the conversion direction, and the
dedup-dropped row the #1446/#1321 note-line dedup the refactor must not lose.

#2590: the startup contract is the shared `negative_selection` shape the
migrated family uses (sdk-mcp-session-tools #1433, handle-path-macos #1990,
settings-gateway #1734). Every pre-mutation refusal is `refuse()` — rc 2
with `INVALID:` on stderr, never a traceback a host reads as a failed
control — including the binding self-check, the unknown/repeated name split
and the stale-anchor scan. The target is admitted by `missing_target()` +
`refuse_unusable_target()` before the evidence directory is claimed (a
presence-only check used to let a blank or `.` value fall through and die
inside the shared normalisation afterwards), the claim itself is `claim_dir`.
The stale scan reads the working tree, so it takes the same `tree_lock` the
mutation round below takes: a concurrent sibling with a mutant on disk used
to be read back as a false stale (#2590 item 4, proven by the pre-fix run in
this round's evidence directory), and a lock that stays busy past the wait —
or a source read that fails — is itself that same refusal (fix-r2: never an
uncaught TimeoutError / OSError traceback a host would read as rc 1). The
guard's lock and the mutation round's lock are two acquisitions, not one
critical section: a sibling landing in the window between them is stopped
fail-closed by the mutation round's own acquisition — its pristine check or,
from fix-r3, its lock timeout, both the same "never
started" rc 2 refusal the guard already produces (fix-r3b scopes the refusal
to the acquisition phase and withdraws the empty evidence directory; anything
that fails after the acquisition is a red run: INVALID rows, published
summary, rc 1 — signals excepted, 128+signum per the README signal
paragraph). A
bound assertion containing `{` / `}` is
refused up front (#2590 item 3): the receipt protocol compares the runtime
`format!` result byte-for-byte, so a template can never match and the row
could only come out NO_RECEIPT. The exit contract is `verdict_exit` over a
summary whose denominator is the static roster: a row that records no
verdict publishes `roster-mismatch.json` and fails instead of shrinking the
run to `N-1/N-1 killed` and exiting 0 (#2590 item 2), and under
`--compile-only` the summary reports `COMPILED=n/N` — the verdict the exit
contract counts — instead of a false `KILLED=0/N` (fix-r2).
"""
import argparse
import hashlib
import json
import os
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[5]
FIXTURES = Path(__file__).resolve().parents[1]
SERVICES = ROOT / 'src/pilotdeck-gateway/src/native_process/services.rs'
TESTS = ROOT / 'src/pilotdeck-gateway/src/native_process/services_tests.rs'
sys.path.insert(0, str(FIXTURES))
from negative_process import (SourceBaselineError, mutating_sources,
                              run_owned, wait_for_slot, tree_lock,
                              write_source_atomic)
from negative_selection import (claim_dir, missing_target, refuse,
                                refuse_unusable_target, registered, verdict_exit)
from negative_suite import ReceiptRun, invalidate_completed, judge, passed

mutations = [
    # The #2495 bug itself: the accumulation moved back behind the note
    # dedup continue, so a second entry with the same lexical spelling but
    # a different inspectable stops counting.
    ('dedup-gating-restored', SERVICES,
     '''        has_inspectable |= is_read_file_inspectable(entry);
        if !seen.insert(lexical.clone()) {
            continue;
        }''',
     '''        if !seen.insert(lexical.clone()) {
            continue;
        }
        has_inspectable |= is_read_file_inspectable(entry);''',
     'native_process::services_tests::note_dedup_keeps_second_entry_inspectable',
     "#2495 the deduped second entry's inspectable must still select the read_file form"),
    # The hasInspectable domain only: unregistered entries stop counting
    # even when their spelling is a member of allowedReadFiles.
    ('inspectable-registered-domain-restored', SERVICES,
     '        has_inspectable |= is_read_file_inspectable(entry);',
     '        has_inspectable |= entry.channel_key && is_read_file_inspectable(entry);',
     'native_process::services_tests::unregistered_colliding_spelling_counts_for_guidance',
     "#2495 the unregistered colliding entry's inspectable must still select the read_file form"),
    # The hasInspectable membership gate (fix-r1 sibling review P2, the
    # inspectable-side dual of the note-domain row below): the accumulation
    # hoisted above the allowedReadFiles membership test — note lines keep
    # the gate and the dedup order — so an inspectable entry whose own
    # spelling is a member of nothing starts counting.
    ('inspectable-membership-gate-dropped', SERVICES,
     '''        if !allowed.contains(&lexical) {
            continue;
        }
        has_inspectable |= is_read_file_inspectable(entry);''',
     '''        has_inspectable |= is_read_file_inspectable(entry);
        if !allowed.contains(&lexical) {
            continue;
        }''',
     'native_process::services_tests::unregistered_non_member_inspectable_does_not_count',
     '#2495 a non-member inspectable must not flip the guidance to the read_file form'),
    # The note-line domain: the whole pass restricted to channel-registered
    # entries, so a colliding unregistered spelling loses its line too.
    ('note-domain-registered-only', SERVICES,
     '''        if !allowed.contains(&lexical) {
            continue;
        }''',
     '''        if !allowed.contains(&lexical) || !entry.channel_key {
            continue;
        }''',
     'native_process::services_tests::unregistered_colliding_spelling_gets_note_line',
     '#2495 the unregistered entry naming the registered realpath spelling must get its own note line'),
    # The membership gate the whole pass hangs on: without it every path
    # attachment gets a line, registered or not (#1469's upload fixture).
    ('membership-gate-dropped', SERVICES,
     '''        if !allowed.contains(&lexical) {
            continue;
        }''',
     '''        if false {
            continue;
        }''',
     'native_process::services_tests::binary_container_info_diagnostic_gating',
     '#1469 only the registered binary container keeps a note line'),
    # The conversion direction: the accumulation stuck at true keeps the
    # read_file form even when nothing inspectable is left.
    ('inspectable-always-true', SERVICES,
     '        has_inspectable |= is_read_file_inspectable(entry);',
     '        has_inspectable = true;',
     'native_process::services_tests::note_dedup_both_non_inspectable_keeps_conversion_form',
     '#2495 two non-inspectable entries must keep the conversion form'),
    # The #1446/#1321 note-line dedup the refactor must not lose.
    ('note-dedup-dropped', SERVICES,
     '        if !seen.insert(lexical.clone()) {',
     '        if false {',
     'native_process::services_tests::note_name_cleaning_and_dedupe',
     '#1321 duplicate normalized paths must produce exactly one note line'),
]

# fix-r1 (Grok P0): `mutations` is both the registry and the execution table,
# so a row dropped from the table would silently shrink the denominator —
# `--check` would print a smaller count and still say OK, and the default
# full run would judge only the survivors. fix-r2 (review P2a): the roster
# check is the shared `negative_selection.registered` instead of a local
# frozenset compare — ordered, so a reordered row is also a drift, and a
# refusal is exit 2 ("never started"), never a traceback that reads as a
# failed control. #2590 item 7 (MM-sibling#2): the EXPECTED_NAMES roster is
# the form the *migrated* family uses — sdk-mcp-session-tools (#1433),
# handle-path-macos (#1990) and settings-gateway (#1734) are the precedents
# to cite. #1642 fix-r2 migrated native-management and
# native-registry-switch to this form; the remaining rc 1 leftovers of the
# same family are native-settings (`sys.exit(str)`), turn-gateway-clock-edge
# (lines 99-108) and config-store (lines 950-959), whose `enforce_denominator`
# helpers raise `SystemExit` with a string — explicitly not this precedent
# yet; their migration is tracked in #3114 (c-class follow-up of this PR).
EXPECTED_NAMES = (
    'dedup-gating-restored',
    'inspectable-registered-domain-restored',
    'inspectable-membership-gate-dropped',
    'note-domain-registered-only',
    'membership-gate-dropped',
    'inspectable-always-true',
    'note-dedup-dropped',
)


def check_roster():
    try:
        registered(mutations, EXPECTED_NAMES)
    except ValueError as error:
        refuse(str(error))


def stale_names():
    """Rows whose anchor / bound-test / assertion binding no longer holds.

    #2590 item 3 (DS-boundary#1): fail-closed on template braces. A bound
    assertion is the receipt's identity and must be a static literal — the
    receipt protocol compares the runtime `format!` result byte-for-byte, so
    an interpolated template can never match and the row could only come out
    NO_RECEIPT. The template string sits in the test body too, so the plain
    binding scan stays green on that edit and only a full host round exposes
    it (exactly the r1 -> r2 regression shape); refusing up front makes it a
    `--check` failure instead.
    """
    stale = []
    for name, path, old, new, test, assertion in mutations:
        if '{' in assertion or '}' in assertion:
            stale.append(f'{name} (bound assertion is a format! template)')
            continue
        # fix-r3 (MM r2 boundary#7 P3): the guarded sources carry non-ASCII
        # (em-dashes in services_tests.rs), so decode explicitly instead of
        # following the host locale (same read the shared matrix uses).
        text = path.read_text(encoding='utf-8')
        body = test_body(test)
        if (text.count(old) != 1 or old == new or
                not body or f'"{assertion}"' not in body):
            stale.append(name)
    return stale


def roster_drift(expected, results):
    """#2590 item 2: the recorded verdict names vs the static roster.

    ``None`` when every expected name has exactly one recorded verdict, in
    roster order; otherwise the ``(recorded, expected)`` pair. The runtime
    ``results`` list is not the denominator: a row whose flow is skipped
    entirely would leave the survivors all-KILLED and a green ``N-1/N-1
    killed`` exit 0, so the caller must publish the mismatch and fail. Both
    sides are normalised to lists, so a tuple roster compares by content, not
    by type (a list never equals a tuple in Python).
    """
    recorded = [row['name'] for row in results]
    return None if recorded == list(expected) else (recorded, list(expected))


def summary_line(counts, denominator, compile_only):
    """#2590 fix-r2 (DS-boundary#2): the summary's verdict word follows the mode.

    Under ``--compile-only`` every row records ``COMPILED`` and the exit
    contract counts exactly that, so a fixed ``KILLED=0/N`` line reads like
    "everything survived" next to a green exit — the word must be the verdict
    ``verdict_exit`` counts. The default full run keeps ``KILLED``.
    """
    state = 'COMPILED' if compile_only else 'KILLED'
    return (f'summary: {state}={counts[state]}/{denominator}, '
            f'SURVIVED={counts["SURVIVED"]}, INVALID={counts["INVALID"]}')


def test_body(test, texts=None):
    if texts is None:
        texts = {TESTS: TESTS.read_text(encoding='utf-8')}
    # Top-level test functions: a column-zero closing brace ends this
    # function, so a sibling's assertion cannot satisfy it.
    pattern = r'^(?:async )?fn ' + re.escape(test.rsplit('::', 1)[-1]) + r'\([^\n]*\).*?^}'
    bodies = re.findall(pattern, texts[TESTS], re.M | re.S)
    return bodies[0] if len(bodies) == 1 else ''


def check_binding_scope():
    fixture = 'fn target() {\n}\nfn sibling() {\n assert!(false, "only sibling");\n}\n'
    if 'only sibling' in test_body('target', {TESTS: fixture}):
        raise RuntimeError('#2495 binding checker accepted sibling assertion')
    if 'only sibling' not in test_body('sibling', {TESTS: fixture}):
        raise RuntimeError('#2495 binding checker lost its own assertion')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--check', action='store_true')
    parser.add_argument('--compile-only', action='store_true')
    parser.add_argument('output', nargs='?')
    parser.add_argument('selected', nargs='*')
    args = parser.parse_args()
    # fix-r3 (MM r2 boundary#1 P2a; fix-r3b DS r3 boundary#1 P2a + MM r3
    # boundary#5 P3): --check's argv contract (the usage line above: no output
    # directory, no names, no flags it would silently drop) is machine-checked,
    # not just documented -- a typo'd `--check some-name` used to return the
    # same green "OK" line as the real roster check without ever reading it.
    # `args.output is not None`, not truthiness: an empty-string positional
    # (`--check ""`, an unset shell variable unquoted) is still a positional.
    if args.check and (args.output is not None or args.selected
                       or args.compile_only):
        refuse('#2495 --check takes no output directory, no mutation names, '
               'and no --compile-only flag')
    # #2590 item 1 (handle-path-macos:148-157, sdk-mcp-session-tools:209-216):
    # the binding self-check runs before any mutation, so its failure is a
    # "never started" refusal (rc 2) — the pre-`mutating_sources` boundary
    # every guard below already follows — not an uncaught traceback a host
    # would read as "ran and not green" (1).
    try:
        check_binding_scope()
    except RuntimeError as error:
        refuse(str(error))
    check_roster()
    # #2590 item 1 (sdk-mcp-session-tools:225-242): the caller-side name
    # errors are refused from argv alone, before any guard reads live
    # sources. A full mutation round holds the tree lock with `services.rs`
    # mutated on disk; a concurrent guards run that passes an unknown or
    # repeated name must not queue behind that round just to be told so.
    # `--check` keeps its own argv contract (no output, no names).
    selected = [row for row in mutations if not args.selected or row[0] in args.selected]
    unknown = sorted(set(args.selected) - {row[0] for row in mutations})
    if not args.check and unknown:
        refuse(f'#2495 unknown mutation names: {unknown}')
    if not args.check and args.selected and len(selected) != len(args.selected):
        repeated = sorted({name for name in args.selected
                           if args.selected.count(name) > 1})
        refuse(f'#2495 repeated mutation names: {repeated}')
    # #2590 item 1 (sdk-mcp-session-tools:243-264): the output-path and
    # target guards read argv/env only, so they too fire before any source
    # bytes are read and before the evidence directory is claimed. The target
    # is admitted by the one shared rule (absent, empty, blank, dot,
    # filesystem or repository root, inside `src`): a presence-only check
    # used to let a blank or `.` value through, and the run then died inside
    # the shared normalisation — with artifacts on their way to the
    # repository root and an evidence directory that read like a finished
    # run (#1666 fix-r7/r9 shape). The output-path precheck is a fast fail on
    # the same condition `claim_dir` owns below (an atomic
    # `mkdir(exist_ok=False)`), so a path created after it is still refused.
    if not args.check:
        if not args.output:
            refuse('#2495 an output directory is required')
        if missing_target():
            refuse('#2495 set CARGO_TARGET_DIR to the caller-owned build tree')
        refuse_unusable_target()
        if Path(args.output).exists():
            refuse(f'#2495 output directory already exists: {Path(args.output).resolve()}')
    # #2590 item 4 (issue-1246:251-255): the stale scan reads the working
    # tree, so it takes the same tree lock the mutation round below takes. A
    # concurrent sibling holding a mutant on disk used to be read back as a
    # false "stale anchor" — proven by the pre-fix run in this round's
    # evidence directory — and the refusal is rc 2 ("never started"), never
    # a traceback (1). fix-r2 (DS-boundary#1 / MM-concurrency#1, 取高 P1):
    # "never a traceback" must hold when the wait itself fails too. A
    # sibling mutation round can hold this lock far past the 900 s default
    # (every row is up to four cargo calls), and the guarded reads can fail
    # with OSError / UnicodeDecodeError, so the acquisition and the scan are
    # one refused block, not an uncaught exception a host reads as rc 1.
    # fix-r3 (MM r2 boundary#3 P2a): the wait itself is bounded to 60 s, the
    # parity window's bound — the scan is a milliseconds-long read, so a busy
    # lock must surface as the rc 2 refusal well inside a CI step's budget
    # instead of eating the full 900 s default; the mutation round below is
    # the only acquisition that may legitimately need to queue that long.
    try:
        with tree_lock(ROOT, timeout=60):
            stale = stale_names()
    except (TimeoutError, OSError, UnicodeDecodeError) as error:
        refuse(f'#2495 tree lock or stale read failed: '
               f'{type(error).__name__}: {error}')
    if stale:
        refuse('#2495 stale anchor/test/assertion bindings: ' + ', '.join(stale)
               + ' (a killed sibling round can leave residue that reads as '
                 'stale; inspect with git status)')
    if args.check:
        print(f'#2495 {len(mutations)} registered mutations present '
              f'(EXPECTED_NAMES roster matched); {len(mutations)} unique anchors '
              f'and assertion bindings: OK')
        return 0
    # The guards above have run; `claim_dir` makes the atomic claim (an
    # existing directory is a "never started" refusal, not a verdict).
    output = claim_dir(args.output)
    env = dict(os.environ)
    for key in [ReceiptRun.ENV, ReceiptRun.PARENT_ENV, 'PDRS_MUTANT', 'LIVE_E2E_MUTANT']:
        env.pop(key, None)
    guarded = [SERVICES, TESTS, Path(__file__).resolve(),
               FIXTURES / 'negative_process.py',
               FIXTURES / 'negative_suite.py',
               ROOT / 'src/pilotdeck-gateway/src/lib.rs']
    results = []
    guarded_sha256 = {}

    def run(label, run_env, test, *, compile_only=False):
        slot = wait_for_slot(ROOT / 'scripts/cargo-slots.sh', timeout=600)
        (output / f'{label}.slots.log').write_text(slot.stdout + slot.stderr)
        command = ['cargo', 'test', '--offline', '--manifest-path', 'src/Cargo.toml',
                   '-p', 'pilotdeck-gateway', '--lib']
        command += ['--no-run'] if compile_only else [test, '--', '--exact', '--include-ignored']
        (output / f'{label}.command.json').write_text(json.dumps(command) + '\n')
        result = run_owned(command, cwd=ROOT, env=run_env, timeout=900)
        (output / f'{label}.log').write_text(result.stdout + result.stderr)
        return result

    # fix-r3b4 (CI red on 849702ac9 — both the negative-controls job and its
    # rust mirror): the phase boundary is carried by the acquired flag, not
    # by an ExitStack-scoped try — the anchor self-check models only the
    # canonical `with mutating_sources(...) as originals:` capture, and an
    # `originals = stack.enter_context(...)` acquisition is an unrecognized
    # snapshot binding there (an ERROR, not a skippable UNSUPPORTED). The
    # flag keeps the same four-way rc table without touching the ruler.
    acquired = False
    try:
        try:
            with mutating_sources(ROOT, guarded) as originals:
                # fix-r3 (MM r2 concurrency#1 P1 / DS r2 concurrency#1, 两家
                # 同撞取高 P1): the mutation round takes the very same
                # ROOT/.negative-source.lock the guard segment above already
                # refuses on, so a sibling seizing it in the window between
                # the two acquisitions must land on the same "never started"
                # side — refuse() and rc 2, never verdict_exit's 1 over
                # INVALID rows a host would read as a finished, not-green
                # round (fix-r2's README/CI wording "领锁超时本身也拒跑
                # （rc 2，非 traceback）" carries no stage qualifier).
                # fix-r3b (DS r3 concurrency#1 P1 / boundary#2 P3 + MM r3
                # boundary#2 P2a, 实测 TimeoutError ⊂ OSError 且
                # errno.ETIMEDOUT 映射到它): the catch below is scoped by
                # PHASE (the with-statement's own entry: the tree-lock
                # wait, the pristine checks and the snapshot reads, where
                # nothing has been mutated yet — the acquired flag flips
                # only after all of them), and within that phase to these
                # two exception types, not to every exception the phase
                # can raise. Once the flag is set, every TimeoutError —
                # including an ETIMEDOUT on the restore write-back — hits
                # the acquired guard, re-raises, and falls to the generic
                # handler below: the round did run, rows may be INVALID and the
                # tree may be mid-restore, so it must invalidate, publish
                # and exit 1, never a "never started" refusal. Per-row
                # timeouts (run_owned's 900 s, wait_for_slot's 600 s) are
                # subprocess.TimeoutExpired and are caught by the inner
                # per-row handler either way.
                # fix-r3b2 (DS r3b review, 四角色撞条; fix-r3b3 精化 by
                # final DS boundary#1/sib#3): within the acquisition phase
                # the refusal still maps only these two exception types —
                # the RAW acquisition failures fall to the red-run handler
                # below: a bare `_git` RuntimeError from the preflight
                # diff/ls-files calls, a cold-boot cargo failure in
                # `_ensure_workspace_scanner`, or a bare read_bytes/stat
                # OSError on a guarded file (fail-closed, never a false
                # green; git failures via `_head_bytes` and scanner-run
                # `(ValueError, OSError, SubprocessError)` via
                # `_workspace_literal_inputs` are already wrapped into
                # SourceBaselineError, so they DO reach rc 2 — pinned by
                # test_acquisition_phase_raw_failure_is_a_red_run; the
                # started-flag shape that refuses the whole acquisition
                # phase is follow-up entry 22 on #2590, same-json-surfaces
                # / utf16-residual-sort precedent). Signals are outside
                # this table entirely: 128+signum per the README signal
                # paragraph.
                acquired = True
                guarded_sha256 = {str(path.relative_to(ROOT)): hashlib.sha256(originals[path]).hexdigest() for path in guarded}
                for name, path, old, new, test, assertion in selected:
                    item = dict(name=name, test=test, assertion=assertion,
                                status='INVALID', reason='not run', compiled=False)
                    try:
                        if not args.compile_only and not passed(run(name + '-positive', env, test), test):
                            raise RuntimeError('positive bound test failed')
                        # fix-r1 (concurrency review P2): one atomic rename, never a
                        # truncating in-place write — a SIGKILL mid-window must not
                        # leave a half-written source (#1903, write_source_atomic).
                        write_source_atomic(
                            ROOT, path, originals[path].decode().replace(old, new, 1).encode())
                        if run(name + '-compile', env, test, compile_only=True).returncode:
                            raise RuntimeError('mutant did not compile; not a runtime kill')
                        item['compiled'] = True
                        if args.compile_only:
                            item.update(status='COMPILED', reason='mutated source compiled')
                        else:
                            receipt = ReceiptRun(output, name, [(test, assertion)])
                            result = run(name + '-negative', receipt.environment(env), test)
                            killed, reason, hits = judge(result, '', [(test, assertion)], receipt)
                            item.update(status='KILLED' if killed else ('SURVIVED' if passed(result, test) else 'INVALID'),
                                        reason=reason, hits=hits)
                    except Exception as error:
                        item.update(status='INVALID', reason=f'{type(error).__name__}: {error}')
                    finally:
                        write_source_atomic(ROOT, path, originals[path])
                    try:
                        if not args.compile_only:
                            if not passed(run(name + '-restored', env, test), test):
                                raise RuntimeError('restored bound test failed')
                            item['restoredPass'] = True
                    except Exception as error:
                        item.update(status='INVALID', reason=item['reason'] + f'; restored: {error}')
                    results.append(item)
                    print(f'{name}: {item["status"]}: {item["reason"]}', flush=True)
        except (TimeoutError, SourceBaselineError) as error:
            if acquired:
                # Post-acquisition phase, the restore window included: the
                # generic handler below owns it, never a "never started"
                # refusal — re-raise.
                raise
            # fix-r3b (MM r3 concurrency#1 P2a): SourceBaselineError at
            # entry is the same never-started phase — a sibling round
            # SIGKILLed mid-mutation leaves its mutant on disk and this
            # round's first act under the lock is to refuse it, not to
            # fold it into seven INVALID rows and a published summary
            # (the rc table: 树脏 → 2). Restore-phase failures keep the
            # rc 1 shape.
            # fix-r3b (MM r3 boundary#1 P2a, utf16-residual-sort
            # startup_refusal precedent): the empty directory claim_dir
            # made is withdrawn so a "never started" refusal leaves no
            # claimed-looking evidence directory — rmdir only removes
            # the empty case; a run that already wrote logs keeps them.
            try:
                output.rmdir()
            except OSError:
                pass
            phase = ('source lock busy' if isinstance(error, TimeoutError)
                     else 'tree not pristine')
            refuse(f'#2495 {phase} (never started): '
                   f'{type(error).__name__}: {error}')
    except Exception as error:
        reason = f'source guard: {type(error).__name__}: {error}'
        invalidate_completed(results, reason)
        for name, _, _, _, test, assertion in selected[len(results):]:
            results.append(dict(name=name, test=test, assertion=assertion,
                                status='INVALID', reason=reason, compiled=False))
    counts = {state: sum(row['status'] == state for row in results)
              for state in ['KILLED', 'SURVIVED', 'INVALID', 'COMPILED']}
    # #2590 item 2 (sdk-mcp-session-tools:374-391): exactly one verdict per
    # selected name, compared against the static roster before anything that
    # reads as success is published. The runtime `results` list is not the
    # denominator: a row whose flow is skipped entirely (no trigger path
    # today; the guard is for future branches) would leave the survivors
    # all-KILLED and a green `N-1/N-1 killed` exit 0, so a mismatch publishes
    # no summary.json — only a file whose name says what happened.
    expected = [name for name in EXPECTED_NAMES
                if not args.selected or name in args.selected]
    drift = roster_drift(expected, results)
    if drift is not None:
        recorded, expected_roster = drift
        missing = [name for name in expected_roster if name not in recorded]
        message = (f'#2495 verdict roster mismatch: missing={missing} '
                   f'recorded={recorded} expected={expected_roster}')
        print(message, file=sys.stderr)
        # fix-r3 (DS r2 concurrency#4 P3): publish the mismatch the same
        # atomic tmp+replace shape summary.json uses below, so a SIGKILL
        # mid-write can never leave a truncated JSON standing in for the one
        # file that explains the missing rows.
        mismatch = output / 'roster-mismatch.json.tmp'
        mismatch.write_text(json.dumps(
            {'verdict': 'ROSTER_MISMATCH', 'missing': missing,
             'expected': expected_roster, 'recorded': recorded,
             'rows': results, 'counts': counts,
             'guardedSha256': guarded_sha256}, indent=2) + '\n')
        mismatch.replace(output / 'roster-mismatch.json')
        return 1
    temporary = output / 'summary.json.tmp'
    temporary.write_text(json.dumps(
        {'controls': results, 'counts': counts, 'guardedSha256': guarded_sha256}, indent=2) + '\n')
    temporary.replace(output / 'summary.json')
    # The summary denominator is the static roster (never len(results)).
    print(summary_line(counts, len(expected), args.compile_only), flush=True)
    print(json.dumps({'controls': results, 'counts': counts}), flush=True)
    # #2590 item 2: the shared exit contract — zero rows is never 0 (`0/0
    # killed` is not a pass) and every row must be KILLED — COMPILED under
    # --compile-only.
    return verdict_exit(results, key='status',
                        killed='COMPILED' if args.compile_only else 'KILLED')


if __name__ == '__main__':
    raise SystemExit(main())
