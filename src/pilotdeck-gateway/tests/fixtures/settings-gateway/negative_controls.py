#!/usr/bin/env python3
"""Temporary product mutations of the settings gateway adapter; each must be
killed by its named test's own #1318 panic markers, then restore.

Run with no other writers/builds in this worktree. Evidence goes in a new
output folder; original source bytes and hashes are verified after every
mutation. The seven mutants all target
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
  ``apply-effort-low`` (the first row whose events carry the dispose/evict
  tail) pins the close.
* ``update-drop-leak`` — the capture guard no longer unsubscribes; the
  dedicated drop-mid-reload test pins the listener detachment.
* ``extensions-truthiness-flipped`` — ``projectKey`` branches on the
  ``String()`` coercion again instead of the original JS truthiness; the
  dedicated truthiness-matrix test pins the all-runtimes branch of the
  JS-falsy cells.

The mutated files must match the baseline tree byte-for-byte
(``mutating_sources`` proves it); on the host that baseline is ``HEAD``.
The table is registered by the static ``EXPECTED_TOTAL`` *and* the frozen
``EXPECTED_NAMES`` roster (#1538): a zero, shorter, longer, renamed or
duplicated table is refused before anything runs, and ``MARKERS`` must cover
exactly the same names — a denominator or roster taken from the running table
would shrink with it.
``PDRS_NEGATIVE_BASELINE`` may name another commit object (the sandbox
verification pass pins a ``git stash create`` commit of the reviewed tree,
because the sandbox cannot commit); unset it everywhere else. The ref is
resolved to a full SHA once, up front, and that same immutable SHA is both
recorded (every summary row, ``baseline.json``, the ``.log`` first lines)
and passed to ``mutating_sources`` — the bytes validated are the bytes the
evidence names, even if a movable ref moves mid-run. A non-``HEAD``
baseline is flagged as ``NON-HEAD BASELINE`` / ``NON-HOST EVIDENCE`` on
stdout and in ``baseline.json``, and the script then exits with the
dedicated code ``NON_HEAD_EXIT`` (42) *even when every mutant is killed*:
the host gate only trusts ``HEAD`` evidence, so a non-HEAD run can never
look like a green host run.
``CARGO_TARGET_DIR`` must be supplied by the caller.
"""
import difflib
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_panic import named_test_failed, panic_evidence
from negative_selection import baseline_passed, select, verdict_exit
from negative_process import mutating_sources, run_owned, wait_for_slot

root = Path(__file__).resolve().parents[5]


def atomic_write(path, text):
    """#1538 fix-r1 (concurrency P2): publish evidence through tmp + rename.

    `write_text` truncates in place and this loop rewrites the evidence after
    every mutant; a concurrent reader (host gate, another agent polling the
    evidence directory) could parse a half-written file.  `summary.json` also
    carries `complete: false` until the run finishes, so an all-KILLED prefix
    of a running suite cannot pass for a finished subset run.
    `specs/negative-controls/README.md` asks for tmp + rename.
    """
    temporary = path.with_name(path.name + '.tmp')
    temporary.write_text(text, encoding='utf-8')
    temporary.replace(path)


def refuse(message):
    """#1538 fix-r3 (concurrency/boundary P2): "never ran" gets its own code.

    `sys.exit('text')` prints and exits 1, the same code a completed run uses
    when a mutant is not killed — but those two states differ in whether any
    evidence exists, and the README tells the host to triage on exactly that.
    """
    print(message, file=sys.stderr, flush=True)
    sys.exit(2)


out = Path(sys.argv[1]).resolve()
try:
    out.mkdir(parents=True, exist_ok=False)
except FileExistsError:
    # #1538 fix-r4 (concurrency P1): this is the only exit-2 branch that can
    # sit next to files. A doomed re-run into a finished directory used to look
    # like "never ran" while `summary.json` from the *previous* run (complete,
    # all KILLED, a different `runId`) was still there for a host to read as
    # this run's verdict. Say so in the message.
    refuse(f'#1538 output directory already exists: {out}'
           ' (this run wrote nothing; any files inside belong to an earlier run)')
source = root / 'src/pilotdeck-gateway/src/runtime/settings'
REPLAY = 'runtime::settings::tests::settings_gateway_replays_the_real_ts_oracle'
UPDATE_DROP = ('runtime::settings::tests::'
               'update_settings_detaches_the_capture_listener_when_dropped_mid_reload')
EXTENSIONS_TRUTHINESS = ('runtime::settings::tests::'
                         'extensions_reload_branches_on_the_original_project_key_truthiness')
# Each row: (name, file, original anchor text, mutant text, killed test).
# The kill marker must appear inside that test's own panic payload.
mutations = [
    ('apply-busy-gate-removed', 'gateway.rs',
     'if self.options.owner.has_active_turn(&session_key) {',
     'if false && self.options.owner.has_active_turn(&session_key) {', REPLAY),
    ('update-capture-ignored', 'gateway.rs',
     'let changed_paths = std::mem::take(&mut *lock(&captured));',
     'let changed_paths = Vec::new();', REPLAY),
    ('reload-config-unsupported-flipped', 'gateway.rs',
     'object.insert("reloaded".into(), JsValue::Bool(false));',
     'object.insert("reloaded".into(), JsValue::Bool(true));', REPLAY),
    ('dispatch-drops-reload-extensions', 'gateway.rs',
     '"reload_extensions" => self.reload_extensions(params).await,',
     '"reload_extensions_disabled" => self.reload_extensions(params).await,', REPLAY),
    ('apply-close-skipped', 'gateway.rs',
     'if !result.applied.is_empty() || !result.cleared.is_empty() {',
     'if false {', REPLAY),
    ('update-drop-leak', 'gateway.rs',
     'subscription.unsubscribe();',
     'drop(subscription);', UPDATE_DROP),
    ('extensions-truthiness-flipped', 'gateway.rs',
     'Some(value) if truthy(value) => Some(JsString::from_utf16(',
     'Some(value) => Some(JsString::from_utf16(', EXTENSIONS_TRUTHINESS),
]
# #1538 fix-r1/r2 (boundary P0/P1): an emptied table used to run zero mutants,
# print `0/0 killed` and exit 0 — a ruler that passes without measuring
# anything (`mutating_sources([])` accepts an empty path set, so nothing
# downstream caught it). Guarding only the empty case was half a fix: the
# denominator was `len(mutations)` itself, so dropping one row rewrote the
# denominator and the remaining six controls still reported `6/6 killed`.
# `specs/negative-controls/README.md` («分母核对») requires the declared set to
# come from a static source, never from the runtime table, so the total is a
# literal here: adding or removing a mutant re-registers it in the same edit.
EXPECTED_TOTAL = 7
# #1538 fix-r3 (boundary P1): a count is not a registry.  Rename one row and
# delete another, or duplicate a row and drop a different one, and the length
# still reads 7, `MARKERS` still agrees two-way (sets dedupe), and one defense
# never runs while the run reports 7/7.  The roster is a literal too, compared
# as an exact tuple so order and repeats are both pinned.
EXPECTED_NAMES = (
    'apply-busy-gate-removed',
    'update-capture-ignored',
    'reload-config-unsupported-flipped',
    'dispatch-drops-reload-extensions',
    'apply-close-skipped',
    'update-drop-leak',
    'extensions-truthiness-flipped',
)
if EXPECTED_TOTAL < 1:
    refuse('#1538 the registered mutation denominator must be positive')
if EXPECTED_TOTAL != len(EXPECTED_NAMES):
    refuse('#1538 the registered denominator and roster disagree:'
           f' EXPECTED_TOTAL={EXPECTED_TOTAL}, names={list(EXPECTED_NAMES)}')
if len(mutations) != EXPECTED_TOTAL:
    refuse(f'#1538 fixed product mutation denominator: expected '
           f'{EXPECTED_TOTAL}, found {len(mutations)}')
REGISTERED_TOTAL = EXPECTED_TOTAL
REGISTERED_NAMES = [row[0] for row in mutations]
if tuple(REGISTERED_NAMES) != EXPECTED_NAMES:
    refuse('#1538 registered mutation roster must equal the frozen list:'
           f' expected={list(EXPECTED_NAMES)} found={REGISTERED_NAMES}')
# A dedicated exit code for "everything was killed, but the baseline was not
# HEAD, so this run is not host evidence" — distinct from 1 (a mutant not
# killed / INVALID) so automation and humans cannot mistake a green non-HEAD
# sandbox run for a host-accepted run.
NON_HEAD_EXIT = 42
MARKERS = {
    'apply-busy-gate-removed': 'case apply-busy',
    'update-capture-ignored': 'case update-merges-store-paths',
    'reload-config-unsupported-flipped': 'the bare assembly must answer reload_config',
    'dispatch-drops-reload-extensions': 'dispatch must own the five settings methods',
    'apply-close-skipped': 'case apply-effort-low',
    'update-drop-leak': '#1318-r1 update-drop: dropping the future must detach the capture listener',
    'extensions-truthiness-flipped': ('#1318-r2 extensions-truthiness: a JS-falsy projectKey '
                                       'must take the all-runtimes branch'),
}
# #1538 fix-r2 (boundary P2a): a mutant without a marker raised KeyError outside
# the per-mutant `try`, so the run died on a traceback and the evidence file
# kept the previous rows — an all-KILLED prefix that reads green; a marker
# without a mutant was silently ignored. Require exact two-way coverage before
# anything runs.
if set(MARKERS) != set(REGISTERED_NAMES):
    refuse('#1538 marker table must cover exactly the registered mutations:'
           f' missing={sorted(set(REGISTERED_NAMES) - set(MARKERS))}'
           f' extra={sorted(set(MARKERS) - set(REGISTERED_NAMES))}')
selected = sys.argv[2:]
if selected:
    # #1538: `select` raises on an unknown name instead of `assert`-ing: under
    # `python3 -O` an assert is stripped, a misspelt name silently selected
    # nothing, and the loop below ran zero mutants, printed `0/0 killed` and
    # exited 0. It also reports the names that are actually unknown rather than
    # echoing the whole selection, and folds a repeated name instead of calling
    # it unknown. Exit code 2 keeps "never ran" distinguishable from "ran and
    # did not kill" (1) for the host gate.
    try:
        mutations = select(mutations, selected, always=())
    except ValueError as error:
        refuse(f'#1538 {error}')
env = os.environ.copy()
if not env.get('CARGO_TARGET_DIR'):
    refuse('#1538 CARGO_TARGET_DIR must be supplied by the caller')
# The actual baseline must be resolved and recorded (fix-r2 P1): every
# summary row and the replay log carry the full commit SHA and where it came
# from, so sandbox stash-commit evidence cannot be mistaken for host HEAD
# evidence — the host gate only trusts HEAD. The resolved SHA (not the
# movable ref) is also what `mutating_sources` validates against (fix-r3):
# one resolution, one object for both the evidence and the byte checks.
baseline_ref = os.environ.get('PDRS_NEGATIVE_BASELINE') or 'HEAD'
baseline_source = ('PDRS_NEGATIVE_BASELINE'
                   if os.environ.get('PDRS_NEGATIVE_BASELINE') else 'HEAD')
resolved = subprocess.run(
    ['git', '-C', str(root), 'rev-parse', '--verify', baseline_ref + '^{commit}'],
    capture_output=True, check=False)
if resolved.returncode != 0:
    refuse(f'cannot resolve baseline {baseline_ref!r}: '
           f'{resolved.stderr.decode(errors="replace").strip()}')
baseline_sha = resolved.stdout.decode().strip()
baseline_line = f'baseline: {baseline_sha} (source: {baseline_source})'
print(baseline_line, flush=True)
if baseline_source != 'HEAD':
    print(f'NON-HEAD BASELINE: {baseline_sha} via PDRS_NEGATIVE_BASELINE; '
          'the host gate only trusts HEAD evidence', flush=True)
# A `case <id>` marker must name a real oracle row, or the mutant can never
# be judged KILLED (fix-r1 P1: `apply-success-closes` never existed).
oracle = json.loads((Path(__file__).with_name('oracle.json')).read_text(encoding='utf-8'))
case_ids = {case['id'] for case in oracle['cases']}
for marker in MARKERS.values():
    if marker.startswith('case ') and marker[5:] not in case_ids:
        # #1538: explicit, so `-O` cannot strip the check.
        refuse(f'#1318 marker {marker!r} names no oracle case id')
summary = []
failures = []
slot_script = root / 'scripts/cargo-slots.sh'
# #1538 fix-r2 (concurrency P2): one generation stamp for both evidence files,
# so a reader that sees `summary.json` and `baseline.json` side by side can
# tell whether they come from the same run instead of guessing from mtimes.
# #1538 fix-r3 (reuse P3): a PID can be reused, and the host routinely re-runs
# a subset on the same HEAD, so a nonce — not the process id — makes the stamp
# unique across runs.
run_id = f'{baseline_sha[:12]}-{os.urandom(6).hex()}'


def publish_summary(complete):
    """Publish `summary.json` atomically; `complete` says the run finished.

    #1538 fix-r2 (boundary P2): the file is rewritten after every mutant, so
    during a run it is — atomically — an all-KILLED prefix that looks exactly
    like a finished subset run. `complete` is the flag a polling reader must
    check before treating the file as a verdict.
    """
    counts = {state: sum(row['status'] == state for row in summary)
              for state in ('KILLED', 'SURVIVED', 'INVALID')}
    atomic_write(out / 'summary.json', json.dumps({
        'runId': run_id,
        'registeredTotal': REGISTERED_TOTAL,
        'selectedTotal': len(mutations),
        'complete': complete,
        'counts': counts,
        'results': summary,
    }, ensure_ascii=False, indent=2) + '\n')


def publish_baseline():
    # #1538 fix-r2 (simplify P2 / concurrency P2): record the selected subset
    # next to the registered total; a subset run and a full run used to look
    # the same apart from the row count.
    atomic_write(out / 'baseline.json', json.dumps({
        'runId': run_id,
        'baseline': baseline_sha,
        'baselineSource': baseline_source,
        'nonHeadBaseline': baseline_source != 'HEAD',
        'hostEvidence': baseline_source == 'HEAD',
        'registeredTotal': REGISTERED_TOTAL,
        'selectedTotal': len(mutations),
        'selected': [row[0] for row in mutations],
    }, ensure_ascii=False, indent=2) + '\n')


class demote_on_restore_failure:
    """#1538 fix-r4 (boundary P1): never leave a green verdict on a dirty tree.

    fix-r3 moved the final `publish_summary(complete=True)` inside the
    `mutating_sources` block so it lands while the tree lock is still held —
    right, but it then lands *before* that block's own `finally` does the last
    restore + byte-verify. If that verification fails (write error, unreadable
    git object, bytes differ) the process dies with a traceback while
    `summary.json` still says the run finished all-KILLED: a reader that only
    polls the directory reads a clean verdict for a run that never restored the
    source. Demote the flag on the way out and let the original error through.

    `SystemExit` from a completed-but-failed run is not caught here: that run
    really did finish, and its `complete: true` is the truth.
    """

    def __init__(self, manager):
        self.manager = manager

    def __enter__(self):
        return self.manager.__enter__()

    def __exit__(self, exc_type, exc, traceback):
        try:
            return self.manager.__exit__(exc_type, exc, traceback)
        except Exception:
            # Only a run that actually produced rows can have published a
            # `complete: true` to demote; a failure before the first mutant
            # leaves no `summary.json` at all, which the README promises.
            if summary:
                try:
                    publish_summary(complete=False)
                except OSError:
                    pass
            raise


with demote_on_restore_failure(
        mutating_sources(root, sorted({source / filename for _, filename, *_ in mutations}),
                         baseline=baseline_sha)) as snapshots:
    def run(tests):
        # libtest takes several full-name filters after `--` (one positional
        # TESTNAME per cargo invocation); `--exact` keeps them literal.
        result = run_owned(['cargo', 'test', '--manifest-path', 'src/Cargo.toml',
                            '-p', 'pilotdeck-gateway', '--lib', '--', *tests,
                            '--exact'], cwd=root, env=env, timeout=240)
        return result.returncode, result.stdout + result.stderr

    # An unmutated baseline must be green first, or every verdict is INVALID.
    wait_for_slot(slot_script)
    code, log = run([REPLAY, UPDATE_DROP, EXTENSIONS_TRUTHINESS])
    atomic_write(out / 'baseline-replay.log', baseline_line + '\n\n' + log)
    if code == 0 and all(baseline_passed(log, test)
                         for test in {REPLAY, UPDATE_DROP, EXTENSIONS_TRUTHINESS}):
        print('baseline replay: PASS', flush=True)
    else:
        print('baseline replay: INVALID', flush=True)
        # A broken baseline is "ran, no verdict": publish the run's identity
        # next to the log so the exit-1 evidence directory is not empty.
        publish_baseline()
        sys.exit(1)

    for name, filename, old, new, test in mutations:
        path = source / filename
        original = snapshots[path]
        before = hashlib.sha256(original).hexdigest()
        try:
            # The coverage gate above already proved every row has a marker, so
            # this is a plain lookup: `MARKERS.get(name, '')` used to hand the
            # kill test an empty anchor, and `'' in evidence` is true for every
            # string — the marker check silently disappeared exactly when it
            # mattered (#1538 fix-r3, both families).
            marker = MARKERS[name]
            content = original.decode()
            # An expired anchor is an error exit, never a SURVIVED verdict.
            # #1538: explicit, so `-O` cannot strip the check.
            if content.count(old) != 1:
                raise RuntimeError(
                    f'#1318 {name}: expected one anchor, found {content.count(old)}')
            changed = content.replace(old, new, 1)
            atomic_write(out / f'{name}.patch', ''.join(difflib.unified_diff(
                content.splitlines(True), changed.splitlines(True),
                fromfile=filename, tofile=filename)))
            wait_for_slot(slot_script)
            path.write_text(changed)
            code, log = run([test])
            atomic_write(out / f'{name}.log', baseline_line + '\n\n' + log)
            evidence = panic_evidence(log, test)
            killed = (code == 101
                      and named_test_failed(log, test)
                      and marker in evidence
                      and '#1318' in evidence
                      and 'could not compile' not in log)
            failure = next((line.strip() for line in log.splitlines()
                            if 'panicked at' in line or 'assertion' in line), '')
            status = 'KILLED' if killed else (
                'SURVIVED' if code == 0 else 'INVALID')
            summary.append(dict(name=name, test=test, status=status,
                                returncode=code, failure=failure,
                                marker=marker, sourceSha256=before,
                                baseline=baseline_sha,
                                baselineSource=baseline_source,
                                hostEvidence=baseline_source == 'HEAD'))
            print(f'{name}: {status} rc={code} {failure}', flush=True)
            if status != 'KILLED':
                failures.append(name)
        except Exception as error:
            summary.append(dict(name=name, test=test, status='INVALID',
                                reason=f'{type(error).__name__}: {error}',
                                sourceSha256=before,
                                baseline=baseline_sha,
                                baselineSource=baseline_source,
                                hostEvidence=baseline_source == 'HEAD'))
            failures.append(name)
        finally:
            # #1538 fix-r2 (concurrency P2): the restore write and the re-read
            # are bare calls in a `finally`; if either raised (ENOSPC, EIO,
            # the file replaced by a directory), control left the block and
            # both evidence writes below were skipped, leaving the previous
            # all-KILLED prefix on disk as the last word. Capture the failure
            # instead, record it, publish, then raise.
            restore_error = None
            try:
                path.write_bytes(original)
                restored = hashlib.sha256(path.read_bytes()).hexdigest() == before
            except OSError as error:
                restored = False
                restore_error = error
            if not restored:
                # #1538 fix-r1 (boundary P1): record the broken round *before*
                # raising. Raising first skipped both evidence writes, so the
                # files ended with the previous mutant and read as all-green.
                detail = (f'restore failed: {restore_error}'
                          if restore_error is not None
                          else 'restore failed: source bytes differ')
                row = dict(name=name, test=test, status='INVALID',
                           reason=detail,
                           sourceSha256=before,
                           baseline=baseline_sha,
                           baselineSource=baseline_source,
                           hostEvidence=baseline_source == 'HEAD')
                if summary and summary[-1]['name'] == name:
                    previous = summary[-1].get('reason')
                    if previous:
                        row['reason'] = f'{previous}; {detail}'
                    summary[-1] = row
                else:
                    summary.append(row)
                failures.append(name)
            publish_summary(complete=False)
            publish_baseline()
            if not restored:
                raise RuntimeError(f'#1318 restore failed: {filename}')
    # #1538 fix-r3 (concurrency P2): the final verdict is published *inside*
    # the `mutating_sources` block on purpose. Outside it the tree lock is
    # already released, so another run may start mutating (and rewriting its
    # own evidence) while this one is still writing `complete: true` — the
    # reader would see a finished-looking summary whose tree is no longer the
    # one that produced it.
    publish_summary(complete=True)
    counts = {state: sum(row['status'] == state for row in summary)
              for state in ('KILLED', 'SURVIVED', 'INVALID')}
    print('summary: ' + ', '.join(f'{state}={count}' for state, count in counts.items()), flush=True)
    # `verdict_exit` is the shared rule (a zero-row run is a zero denominator,
    # never a pass), so this script cannot drift back to `if failures:` alone.
    if verdict_exit(summary, key='status') != 0:
        print(f'not killed: {", ".join(failures)}', file=sys.stderr)
        sys.exit(1)
    print(f'{len(summary)}/{len(mutations)} selected ({REGISTERED_TOTAL} registered) killed; '
          f'every source restored byte-for-byte '
          f'against baseline {baseline_sha} ({baseline_source})', flush=True)
    if baseline_source != 'HEAD':
        # Even a fully-killed run against a non-HEAD baseline is not host
        # evidence (review fix-r3): fail closed with the dedicated code and say
        # so on stdout, so a sandbox stash-commit run can never exit like a
        # green host run.
        print(f'NON-HOST EVIDENCE (非宿主证据): baseline {baseline_sha} came from '
              f'{baseline_source}, not HEAD; the host gate only trusts HEAD '
              f'evidence; exit {NON_HEAD_EXIT}', flush=True)
        sys.exit(NON_HEAD_EXIT)
