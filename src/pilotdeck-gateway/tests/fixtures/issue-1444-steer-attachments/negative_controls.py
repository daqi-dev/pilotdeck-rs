#!/usr/bin/env python3
"""#1444 steer_turn attachments-merge controls; run after commit.

The #1444 tests live in methods/tests.rs; this runner mutates only files
that are already on HEAD — production sources for the first six mutants,
and methods/tests.rs itself for the three guard-duty mutants (the
FifoUnstrand drop this fixture anchors lives in the test file). Until the
#1444 tests themselves are committed, the shared source guard refuses to
run (the whole src tree must equal HEAD), so the coordinator runs this
after the commit lands.

Nine mutants (taskbook hard requirement; ledger
A7.2-STEER-ATTACH-001-R2 machine criteria):
  - attachments-rejection-restored: the pre-#1444 explicit rejection
    (STEER_ATTACHMENTS_UNSUPPORTED) is reinstated for non-empty attachments —
    the merge never happens and the live-turn steer errors.
  - boundary-narrowed-to-arrays: `steer_attachments` folds the falsy
    non-nullish spellings (""/0/NaN/false) into the boundary TypeError,
    losing the composer's `!attachments || attachments.length === 0`
    short-circuit (probe a4/a5/a6).
  - allowed-read-files-dropped: the composed `allowedReadFiles` derivation is
    replaced with an always-empty list — a registered lease path no longer
    reaches the steer message (probe a18).
  - inline-path-rerouted: the empty-attachment lane is rerouted through
    `prepare_attachment_turn`, so the raw wire message cells (a11/a16/a17)
    hit the submit-lane string-message guard and error instead of embedding
    the raw value (the residual-1 architecture decision inverts; ledger
    A7.2-STEER-ATTACH-001-R2 keeps the inline path precisely for those
    cells).
  - truthy-non-iterable-accepted (fix-r1, review DS boundary #3): the
    truthy non-iterable rejection arm is reverted to an empty set, so every
    object spelling is accepted — the `{length:2}`/`{length:"0"}` cells
    (probe b3/b4) lose the boundary TypeError the composer's front door
    (`length === 0` on the raw value) still throws for.
  - profile-gate-inline-restored (fix-r1, review MM concurrency #2): the
    profile gate's FS sweep is polled inline on the executor again instead
    of `spawn_blocking` — on the current-thread host runtime a parked
    config read pins the executor and the concurrency probe cannot finish
    before the release write.
  - rescue-open-rerouted (fix-r11, review DS sibling #3): the guard's
    rescue open is rerouted to /dev/null — the parked reader never pairs,
    and the no-other-writer cell judges the guard red: the all-Timeout
    interleaving reds at the third observation; a lost-race interleaving
    (pure or mixed — fix-r13 removed the fail-closed that redded Timeout
    evidence at the cap, reviews DS concurrency#1/boundary#1 + MM
    concurrency#1/boundary#1) exits green with the START-RACE-UNRESOLVED
    anchor instead, and the judging layer below reruns such a row once,
    so the kill lands on the rerun's third-timeout red.
  - rescue-handle-leaked (fix-r11, review DS sibling #3): the rescue
    handle is forgotten instead of closed — the parked reader pairs but
    never reads EOF, and the cell's EOF assertion turns red (the rescue
    handle's close duty); a lost-race interleaving that never reaches a
    pairing goes through the same anchor+rerun path (fix-r13).
  - timeout-retry-erased (fix-r12, reviews MM concurrency#4 + MM
    simplify#3): the fix-r11 retry semantics — one Timeout observation
    must retry on a fresh FIFO before any red verdict — are erased, and
    the observation is forced to the Timeout shape (the match scrutinee
    becomes a constant Err), so the first observation judges red with its
    own anchor instead of a healthy run never exercising the arm.

A run that ends in the cell's inconclusive exit prints the
`#1444 START-RACE-UNRESOLVED` anchor (fix-r12, review MM boundary#1;
fix-r13, reviews DS concurrency#1/boundary#1 + MM
concurrency#1/boundary#1): that green exit is a lost-race shape — pure
or mixed; fix-r13 removed the fail-closed that redded Timeout evidence
at the cap, because a single Timeout observation is scheduler noise the
unmutated cell itself produces, while a real defect reds through the
Timeout arm's repetitions — not a survived mutant, so a SURVIVED row
whose negative log carries the anchor is rerun once — only a rerun that
is again unresolvable passes, recorded as INVALID-SHAPE and released on
the exit-code wire as rc=3, never as rc=0 (fix-r13, reviews DS
boundary#3 + MM concurrency#2/simplify#1/boundary#2: "killed" and
"released without a kill" must stay machine-separable).
`negative_suite.py` itself stays untouched (M6 shared); the judging
layer here reuses its `run_cases`/`publish_summary` and
`pending_reanchor`'s `counts`.

Usage: negative_controls.py --check | OUT_DIR [mutation ...]
"""
import json
import os
import sys
from pathlib import Path

root = Path(__file__).resolve().parents[5]
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import pending_reanchor
from negative_suite import Case, publish_summary, run_cases

STEER = root / 'src/pilotdeck-gateway/src/native_process/methods/steer.rs'
ASSEMBLY = root / 'src/pilotdeck-gateway/src/native_process/assembly.rs'
TESTS = root / 'src/pilotdeck-gateway/src/native_process/methods/tests.rs'

TEST = 'native_process::methods::tests::'

# fix-r12 (reviews MM concurrency#4 + MM simplify#3): the timeout-retry-erased
# span — the observation match from its scrutinee through the Timeout arm's
# retry block, transcribed raw (the Rust string continuations must survive
# byte-for-byte). Only two lines change in the replacement: the scrutinee
# becomes a constant Err (a healthy run parks the reader and never reaches
# the Timeout arm, so the mutation could not kill deterministically without
# forcing the shape), and the retry block becomes the immediate red.
TIMEOUT_RETRY_OLD = r'''        match progress_rx.recv_timeout(std::time::Duration::from_secs(5)) {
            Ok("opened") => {
                negative_assert!(
                    !fifo.exists(),
                    "#1444 the real unstrand drop unlinks the FIFO path"
                );
                negative_assert!(
                    matches!(
                        progress_rx.recv_timeout(std::time::Duration::from_secs(5)),
                        Ok("eof")
                    ),
                    "#1444 the parked reader reads EOF once the rescue handle closes"
                );
                reader.join().expect("#1444 unstrand rescue reader join");
                if attempt > 1 {
                    crate::native_process::channels::console_log_line(
                        &mut std::io::stderr(),
                        &format!(
                            "#1444 unstrand rescue cell: parked on attempt {attempt} \
                             after {timeouts} Timeout-shaped and {disconnections} \
                             Disconnected-shaped lost races"
                        ),
                    );
                }
                return;
            }
            // Any other signal here (a duplicate "starting", an early
            // "eof") breaks the channel's own ordering — a fixture
            // anomaly, not a lost race: red now, no retry.
            Ok(other) => {
                negative_assert!(
                    false,
                    "#1444 the unstrand rescue reader reported {other:?} while its open was still pending"
                );
            }
            // "opened" never arrived: the only shape that can witness a
            // real defect (the guard leaves the reader parked) — judged
            // red only after RED_AFTER_TIMEOUTS repetitions on fresh
            // FIFOs; see the doc above for the shape split. The parked
            // reader of a lost attempt cannot be collected (no writer
            // will ever pair it); it stays parked for the process
            // lifetime, like the guard's registered residual cells,
            // bounded by the attempts.
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                timeouts += 1;
                if timeouts < RED_AFTER_TIMEOUTS {
                    crate::native_process::channels::console_log_line(
                        &mut std::io::stderr(),
                        &format!(
                            "#1444 unstrand rescue cell: attempt {attempt} never saw \
                             the parked reader's open complete ({timeouts}/\
                             {RED_AFTER_TIMEOUTS} Timeout observations); a real defect \
                             repeats on every fresh FIFO — retrying on a new one"
                        ),
                    );
                    continue;
                }'''

TIMEOUT_RETRY_NEW = r'''        match Err::<&'static str, std::sync::mpsc::RecvTimeoutError>(
            std::sync::mpsc::RecvTimeoutError::Timeout,
        ) {
            Ok("opened") => {
                negative_assert!(
                    !fifo.exists(),
                    "#1444 the real unstrand drop unlinks the FIFO path"
                );
                negative_assert!(
                    matches!(
                        progress_rx.recv_timeout(std::time::Duration::from_secs(5)),
                        Ok("eof")
                    ),
                    "#1444 the parked reader reads EOF once the rescue handle closes"
                );
                reader.join().expect("#1444 unstrand rescue reader join");
                if attempt > 1 {
                    crate::native_process::channels::console_log_line(
                        &mut std::io::stderr(),
                        &format!(
                            "#1444 unstrand rescue cell: parked on attempt {attempt} \
                             after {timeouts} Timeout-shaped and {disconnections} \
                             Disconnected-shaped lost races"
                        ),
                    );
                }
                return;
            }
            // Any other signal here (a duplicate "starting", an early
            // "eof") breaks the channel's own ordering — a fixture
            // anomaly, not a lost race: red now, no retry.
            Ok(other) => {
                negative_assert!(
                    false,
                    "#1444 the unstrand rescue reader reported {other:?} while its open was still pending"
                );
            }
            // "opened" never arrived: the only shape that can witness a
            // real defect (the guard leaves the reader parked) — judged
            // red only after RED_AFTER_TIMEOUTS repetitions on fresh
            // FIFOs; see the doc above for the shape split. The parked
            // reader of a lost attempt cannot be collected (no writer
            // will ever pair it); it stays parked for the process
            // lifetime, like the guard's registered residual cells,
            // bounded by the attempts.
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                timeouts += 1;
                negative_assert!(
                    false,
                    "#1444 a Timeout observation must retry on a fresh FIFO before \
                     any red verdict — the retry logic was erased (first observation \
                     at attempt {attempt}, {timeouts} on the record)"
                );'''

mutations = [
    # The merge itself: reinstating the old explicit rejection breaks the
    # live-turn merge cell first (probe a13).
    ('attachments-rejection-restored', STEER,
     '        let (content, allowed_read_files) = if attachments.is_empty() {',
     '        if !attachments.is_empty() {\n'
     '            return Err(RequestError {\n'
     '                code: "STEER_ATTACHMENTS_UNSUPPORTED".into(),\n'
     '                message: "attachments are not supported on steer_turn".into(),\n'
     '                details: None,\n'
     '                validation: None,\n'
     '            });\n'
     '        }\n'
     '        let (content, allowed_read_files) = if attachments.is_empty() {',
     TEST + 'steer_turn_merges_attachments_into_content',
     '#1444 attachments merge stays Ok with a live turn'),
    # The composer's own boundary: falsy non-nullish spellings short-circuit
    # to the plain turn (probe a4/a5/a6); narrowing them into the TypeError
    # is the submit-family semantics steer must not inherit.
    ('boundary-narrowed-to-arrays', STEER,
     '        JsValue::String(_) | JsValue::Bool(false) => Ok(Vec::new()),\n'
     '        JsValue::Number(value) if *value == 0.0 || value.is_nan() => Ok(Vec::new()),',
     '        JsValue::String(_) | JsValue::Bool(false) => Err(RequestError::new("attachments is not iterable")),\n'
     '        JsValue::Number(value) if *value == 0.0 || value.is_nan() => Err(RequestError::new("attachments is not iterable")),',
     TEST + 'steer_turn_non_array_attachments_follow_the_composer_boundary',
     # The plain loop dies at its first cell's `unwrap_or_else` panic.
     '#1444 empty string attachments must stay Ok'),
    # The allowedReadFiles derivation: dropping it leaves the registered lease
    # path out of the steer message (probe a18).
    ('allowed-read-files-dropped', STEER,
     '            let allowed_read_files = match prepared.get("allowedReadFiles") {\n'
     '                Some(JsValue::Array(paths)) => paths\n'
     '                    .iter()\n'
     '                    .filter_map(|path| path.as_string().cloned())\n'
     '                    .collect(),\n'
     '                _ => Vec::new(),\n'
     '            };',
     '            let allowed_read_files = Vec::new();',
     TEST + 'steer_turn_registers_leased_attachment_paths',
     '#1444 the lease path is registered in allowedReadFiles'),
    # The inline empty-attachment lane: rerouting it through
    # prepare_attachment_turn subjects the raw message cells (a11/a16/a17)
    # to the submit-lane string guard — the exact residual the #1444 split
    # keeps out of the aligned cells.
    ('inline-path-rerouted', STEER,
     '        let (content, allowed_read_files) = if attachments.is_empty() {',
     '        let (content, allowed_read_files) = if false {',
     TEST + 'steer_turn_message_cells_keep_the_raw_block_and_the_submit_guard',
     '#1444 number message must stay Ok'),
    # The truthy non-iterable rejection: reverting the object arm to an
    # empty set accepts every object spelling (b1/b2 already fold — the
    # mutation only has to stop the TypeError), so the b3/b4 cells
    # (`{length:2}` / `{length:"0"}`) lose the boundary error.
    ('truthy-non-iterable-accepted', STEER,
     '        JsValue::Bool(true) | JsValue::Number(_) | JsValue::Object(_) => {\n'
     '            Err(RequestError::new("attachments is not iterable"))\n'
     '        }\n',
     '        JsValue::Bool(true) | JsValue::Number(_) => {\n'
     '            Err(RequestError::new("attachments is not iterable"))\n'
     '        }\n'
     '        JsValue::Object(_) => Ok(Vec::new()),\n',
     TEST + 'steer_turn_object_attachments_follow_the_length_zero_front_door',
     '#1444 numeric length two must reach the for...of TypeError'),
    # The profile gate's FS sweep: polling it inline on the executor pins the
    # current-thread host for the parked config read; the concurrency probe
    # (10ms sleep after the FIFO rendezvous) cannot finish before the
    # releaser's bounded write.
    ('profile-gate-inline-restored', ASSEMBLY,
     '    let project = options.project.clone();\n'
     '    let home = options.home.clone();\n'
     '    let env = env.clone();\n'
     '    tokio::task::spawn_blocking(move || validate_profile_gate(&project, &home, &env, management))\n'
     '        .await\n'
     '        .map_err(|error| ProxyError::from(error.to_string()))?\n',
     '    validate_profile_gate(&options.project, &options.home, env, management)\n',
     TEST + 'steer_turn_profile_gate_does_not_block_the_serial_host_executor',
     '#1444 the profile gate\'s config load must not block the serial host executor'),
    # fix-r11 (review DS sibling #3): the retry criterion's guard-duty
    # binding — rerouting the rescue open to /dev/null leaves the parked
    # reader unpaired on every fresh FIFO, so the no-other-writer cell
    # collects its Timeout observations and judges the guard red at the
    # third one. A lost-race interleaving (pure or mixed) instead exits
    # green with the START-RACE-UNRESOLVED anchor, and the judging layer
    # reruns that row once — the kill lands on the rerun (fix-r13
    # removed fix-r12's fail-closed kill-alternative anchor, which
    # redded the healthy cell's scheduler-noise shape).
    ('rescue-open-rerouted', TESTS,
     '            .open(self.0);',
     '            .open("/dev/null");',
     TEST + 'steer_turn_fifo_unstrand_rescue_releases_the_parked_reader_with_no_other_writer',
     "#1444 the parked reader never pairs with the guard's rescue open"),
    # fix-r11 (review DS sibling #3): the rescue handle's close is the EOF
    # the parked reader drains — forgetting the handle instead of closing
    # it leaves the write side open, so the reader pairs but never sees
    # EOF and the cell's EOF assertion turns red. A lost-race
    # interleaving that never reaches a pairing goes through the same
    # anchor+rerun path (fix-r13).
    ('rescue-handle-leaked', TESTS,
     '        drop(rescue);',
     '        std::mem::forget(rescue);',
     TEST + 'steer_turn_fifo_unstrand_rescue_releases_the_parked_reader_with_no_other_writer',
     '#1444 the parked reader reads EOF once the rescue handle closes'),
    # fix-r12 (reviews MM concurrency#4 + MM simplify#3): the fix-r11
    # retry semantics only had the one-off mutation-retry-erased probe —
    # a future change that judges the first Timeout observation red (or
    # drops the counter) had no switch-table coverage. The observation is
    # forced to the Timeout shape (constant-Err scrutinee: a healthy run
    # parks the reader and never exercises the arm, so the mutation could
    # not kill deterministically otherwise), the retry block is replaced
    # by the immediate red, and the first observation panics with its own
    # anchor. The Ok arms stay verbatim so the mutation is exactly the
    # two surgical changes.
    ('timeout-retry-erased', TESTS,
     TIMEOUT_RETRY_OLD,
     TIMEOUT_RETRY_NEW,
     TEST + 'steer_turn_fifo_unstrand_rescue_releases_the_parked_reader_with_no_other_writer',
     '#1444 a Timeout observation must retry on a fresh FIFO before any red verdict'),
]


def mutate(content, scope, replacements):
    for old, new in replacements:
        if content.count(old) != 1 or old == new:
            raise RuntimeError(f'#1444 expired or ineffective mutation anchor: {content.count(old)} matches')
        content = content.replace(old, new, 1)
    return content


def main():
    if sys.argv[1:] == ['--check']:
        for name, path, old, new, _test, _assertion in mutations:
            mutate(path.read_text(), None, [(old, new)])
        print(f'#1444 {len(mutations)} mutation anchors OK')
        return 0
    if len(sys.argv) < 2:
        raise SystemExit('usage: negative_controls.py --check | OUT [mutation ...]')
    env = dict(os.environ)
    if not env.get('CARGO_TARGET_DIR'):
        raise SystemExit('CARGO_TARGET_DIR must be supplied by the caller')
    # `run_cases` installs the receipt protocol per case (negative_suite.py);
    # anything inherited from a caller shell is stale and would turn a real
    # KILLED into INVALID (same pop list as the sibling fixtures, #1616 r3).
    for key in ('PDRS_MUTANT', 'PDRS_NEGATIVE_RECEIPTS',
                'PDRS_NEGATIVE_PARENT_TEST', 'CARGO_TERM_QUIET'):
        env.pop(key, None)
    selected_names = sys.argv[2:]
    if set(selected_names) - {row[0] for row in mutations}:
        raise SystemExit('unknown mutation name')
    selected = [Case(*row) for row in mutations
                if not selected_names or row[0] in selected_names]
    out = Path(sys.argv[1]).resolve()
    out.mkdir(parents=True, exist_ok=False)
    env.update(CARGO_PROFILE_DEV_DEBUG='line-tables-only',
               CARGO_PROFILE_TEST_DEBUG='line-tables-only')
    rc = run_cases(root, selected, out, env, mutate, timeout=900, script=__file__)
    if rc != 1:
        # 0: every row KILLED; 2: an unverified restore or cancellation —
        # nothing here may soften either (the rc==0 shortcut it replaces was
        # redundant with this same line, fix-r13 DS simplify#2).
        return rc
    return _resolve_unresolved_shapes(root, selected, out, env)


# fix-r12 (review MM boundary#1, P1): the rescue cell's inconclusive exit
# is a green exit, so a mutant whose reader reaches it through lost races
# would read as SURVIVED — a shape-dependent kill, not a guard hole. The
# cell prints the anchor below on that exit (tests.rs); a SURVIVED row
# whose negative log carries it is rerun once, and only a rerun that is
# again unresolvable passes (recorded as INVALID-SHAPE). fix-r13 widened
# the anchor to every lost-race mix (the fail-closed that redded Timeout
# evidence at the cap is gone — see the module docstring) and split the
# final exit code: 0 stays reserved for "every row KILLED", released
# rows (residual 8) exit 3, so a consumer reading only the exit code can
# no longer read "released without a kill" as green. Everything else
# keeps the suite's own verdict — `negative_suite.py` is M6-shared and
# stays untouched; only its published helpers are used.
UNRESOLVED_ANCHOR = '#1444 START-RACE-UNRESOLVED'
RERUN_STATES = ('KILLED', 'SURVIVED', 'INVALID', 'INVALID-SHAPE')


def _load_controls(out):
    try:
        return json.loads((out / 'summary.json').read_text())['controls']
    except (OSError, ValueError, KeyError, TypeError):
        # TypeError: a non-dict document (or a non-list `controls`) is
        # unreadable evidence like any other — fail closed, never a
        # traceback (fix-r13, review MM boundary#4).
        return None


def _has_unresolved_anchor(out, mutation):
    try:
        return UNRESOLVED_ANCHOR in (out / f'{mutation}-negative.log').read_text()
    except OSError:
        return False


def _resolve_unresolved_shapes(root, selected, out, env):
    rows = _load_controls(out)
    if rows is None:
        return 1
    by_mutation = {case.mutation: case for case in selected}
    changed = False
    for row in rows:
        if (row['status'] != 'SURVIVED'
                or row['mutation'] not in by_mutation
                or not _has_unresolved_anchor(out, row['mutation'])):
            continue
        case = by_mutation[row['mutation']]
        rerun = out / f'{case.mutation}-shape-rerun'
        try:
            rerun.mkdir(parents=False, exist_ok=False)
        except FileExistsError:
            # A leftover rerun directory cannot be reused (it would mix two
            # runs' evidence) and must not crash the rc ladder with a
            # traceback: the row keeps its SURVIVED verdict and the final
            # check fails closed on it (fix-r13, review MM boundary#3).
            print(f'{case.mutation}: shape-rerun directory already exists; '
                  'leaving the row SURVIVED', flush=True)
            continue
        rerun_rc = run_cases(root, [case], rerun, env, mutate, timeout=900,
                             script=__file__)
        if rerun_rc == 2:
            return 2
        rerun_row = next((item for item in (_load_controls(rerun) or [])
                          if item['mutation'] == case.mutation), None)
        if rerun_rc == 0:
            row.update(status='KILLED',
                       reason=row['reason'] + '; unresolvable shape rerun killed it')
        elif rerun_row is not None and rerun_row['status'] == 'SURVIVED' \
                and _has_unresolved_anchor(rerun, case.mutation):
            row.update(status='INVALID-SHAPE',
                       reason=row['reason'] + '; rerun also ended unresolvable')
        elif rerun_row is not None:
            row.update(status=rerun_row['status'],
                       reason=row['reason'] + '; shape rerun: ' + rerun_row['reason'])
        # A rerun whose evidence cannot be read leaves the row SURVIVED —
        # fail closed on unreadable rerun output.
        changed = True
    if not changed:
        return 1
    released = sum(row['status'] == 'INVALID-SHAPE' for row in rows)
    publish_summary(out, rows, states=RERUN_STATES)
    print('summary: ' + ', '.join(
        f'{state}={count}'
        for state, count in pending_reanchor.counts(rows, RERUN_STATES).items()),
        flush=True)
    if any(row['status'] not in ('KILLED', 'INVALID-SHAPE') for row in rows):
        return 1
    if released:
        # fix-r13 (reviews DS boundary#3 + MM concurrency#2/simplify#1/
        # boundary#2): rows released without a kill (residual 8) must not
        # share rc=0 with "every row killed" on the exit-code wire.
        print(f'released={released} row(s) passed without a kill '
              '(residual 8); exit 3', flush=True)
        return 3
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
