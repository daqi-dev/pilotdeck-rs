#!/usr/bin/env python3
"""Temporary source mutations; each must turn a timing assertion red, then restore.

The negative controls for the shared multi-sample timing budgets in
``tests/common/timing.rs`` (issue #802). Each mutation restores a historical
quadratic or constant-factor implementation, or breaks the halved-input
premise; the ratio, the per-sample absolute cap, or the premise assertion must
catch it, then the restored tree must pass the same test. Run with no other
writers/builds in this worktree.

Release mode, because the mutants must stay observable without debug-only
checks. The red runs use the fixture's default sample count (three rounds, five
for the reparented test), exactly like the green runs and CI, and an inherited
``PDRS_TIMING_SAMPLES`` is dropped so no caller export can shrink them: a
forced one-sample red run would never prove that the median criterion CI
judges can turn red (#978 first review). Every mutant here is equally slow in
every round, so the median of the paired ratios stays high and the per-sample
cap is never divided by the sample count; more samples only redden them
further. ``CARGO_TARGET_DIR`` must
come from the caller; a private build directory would hide slot contention
from cargo-slots.sh.

Every mutation is executed and recorded as KILLED, SURVIVED or INVALID (with a
reason); the script exits non-zero unless all of them are KILLED. A non-zero
exit from an unrelated unwrap, a compile error or a timeout is INVALID: the
mutant must fail at the dedicated message named in its ``expects`` mapping. A
mutation may name more than one acceptable dedicated line (``any``) when two
issue-specific guards can stop it first; every token in ``all`` must still be
present, so the accepted line stays bound to this test.

#1087 fix-r1: the rows stay in memory for the whole guarded window and one
final ``summary.json`` is published by a single rename only after
``mutating_sources`` restored and byte-verified every mutated source; the old
per-row ``write_text`` publish left an all-KILLED summary readable while the
restore was still unverified. A cancellation (``BaseException``) voids every
already-recorded row and every row that never ran, publishes that INVALID
summary, and then fails closed: a ``SystemExit`` is normalized to 2 (256/-256
truncate to a zero POSIX status, a string code exits 1 — #1059 fix-r2), a
plain exception exits 1, and KeyboardInterrupt/Breaking signals are re-raised.
"""
import difflib
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from _negkit import non_head
from negative_panic import named_test_killed, panic_evidence
from negative_process import (mutating_sources, run_owned, wait_for_slot,
                              write_source_atomic)
from negative_selection import claim_dir, missing_target, refuse, refuse_unusable_target, registered, select, verdict_exit

root = Path(__file__).resolve().parents[5]

# Each mutation names the test whose timing assertion it must turn red.
LIB_DEEP_FRAME = 'runtime::tools::builtin::web::html_compat::tests::deep_frame_stack_select_end_tags_stay_within_linear_budget'
mutations = [
    # The pre-9511fb790 bottom-up frame scan: every `</select>` walked the whole
    # open frame stack from the bottom, over the 100k template frames below the
    # select, so N templates x M selects was quadratic. (A .rev() scan stays
    # O(1) here: the select frame sits on top when it closes.)
    ('frame-stack-rescan', 'src/pilotdeck-gateway/src/runtime/tools/builtin/web/html_compat.rs',
     '    fn contains(&self, context: Context) -> bool {\n'
     '        match context {\n'
     '            Context::Select => self.select_frames > 0,\n'
     '            Context::Template => self.template_frames > 0,\n'
     '        }\n'
     '    }',
     '    fn contains(&self, context: Context) -> bool {\n'
     '        self.frames.iter().any(|frame| frame.context == context)\n'
     '    }',
     ['--lib', LIB_DEEP_FRAME],
     dict(all=('8 MiB rewrite', 'did not scale from'))),
    # The pre-4fbb5f78b rewalk of every moved subtree. Linear with the subtree
    # size; the retained absolute cap catches it, and its cache-unfriendly
    # constant can also cross the 8x ratio band on a loaded host. The sink
    # keeps the walk observable in release.
    ('moved-subtree-rewalk', 'src/pilotdeck-gateway/src/runtime/tools/builtin/web/html_dom.rs',
     '    fn attach_height(&mut self, parent: Id, child: Id) {\n'
     '        self.change_child_height(parent, 0, self.nodes[child].height);\n'
     '        self.update_height(parent);\n'
     '    }',
     '    fn attach_height(&mut self, parent: Id, child: Id) {\n'
     '        let mut visited = 0usize;\n'
     '        let mut work = vec![child];\n'
     '        while let Some(id) = work.pop() {\n'
     '            visited += 1;\n'
     '            work.extend(self.children(id));\n'
     '            if let Kind::Element {\n'
     '                template: Some(fragment),\n'
     '                ..\n'
     '            } = self.nodes[id].kind\n'
     '            {\n'
     '                work.push(fragment);\n'
     '            }\n'
     '        }\n'
     '        if visited > (usize::MAX >> 1) {\n'
     '            self.exceeded = true;\n'
     '        }\n'
     '        self.change_child_height(parent, 0, self.nodes[child].height);\n'
     '        self.update_height(parent);\n'
     '    }',
     ['--test', 'html_markdown', 'reparented_wide_subtrees_stay_fast'],
     # This linear-but-slow rewalk can stay inside the 22.6x band (local probe
     # measured 16.0x) and be stopped by the 5 s per-sample cap instead; both
     # lines carry #873 and are that test's timing guards.
     dict(all=('#873',), any=('did not scale from', 'per-sample budget'))),
    # The pre-rope join materialized both sides, so joining N siblings moved
    # the whole accumulated text every time. Byte-identical output; only the
    # scaling ratio and the cap turn red.
    ('materializing-join', 'src/pilotdeck-gateway/src/runtime/tools/builtin/web/html_markdown_buffer.rs',
     '        right.leading = 0;\n'
     '        self.concat(left, right)\n'
     '    }\n'
     '    pub fn indent',
     '        right.leading = 0;\n'
     '        let joined = self.flatten(left)? + &self.flatten(right)?;\n'
     '        self.text_owned(joined)\n'
     '    }\n'
     '    pub fn indent',
     ['--test', 'html_markdown', 'large_shallow_pages_match_real_ts_bytes_and_stay_fast'],
     # The quadratic join's ratio was diluted to about 2.7–2.8x on the host by the
     # CPU worker's repeated provenance (a ~3.8 s constant per sample). After
     # #978 fix-r2 the worker times conversion only, so the same mutant reds
     # the 2^1.5 = 2.83x band (local: 38.58 s / 9.68 s = 3.99x). The per-sample cap stays
     # as this test's anti-hang backstop; accept either dedicated #978 line so
     # a host that trips one first still binds the mutant to this test.
     dict(all=('#978',), any=('did not scale from', 'per-sample budget'))),
    # The same quadratic join on the reparented wide page. The 2 MiB probe
    # (256 KiB/2 MiB, 8x) reds the 22.6x band (linear ~8x) in ~12 s. The
    # 8 MiB full sample is SIGKILLed by host memory pressure (inferred) before
    # its own assertion, which is why the test carries the scaled probe.
    ('materializing-join-reparent',
     'src/pilotdeck-gateway/src/runtime/tools/builtin/web/html_markdown_buffer.rs',
     '        right.leading = 0;\n'
     '        self.concat(left, right)\n'
     '    }\n'
     '    pub fn indent',
     '        right.leading = 0;\n'
     '        let joined = self.flatten(left)? + &self.flatten(right)?;\n'
     '        self.text_owned(joined)\n'
     '    }\n'
     '    pub fn indent',
     ['--test', 'html_markdown', 'reparented_wide_subtrees_stay_fast'],
     # The probe normally reds the #873 ratio band; accept the same test's
     # per-sample cap too, so a host that trips that guard first still binds
     # the mutant to a dedicated timing line.
     dict(all=('#873',), any=('did not scale from', 'per-sample budget'))),
    # A broken "half": quartering the dominant repeat leaves the halved input
    # at ~0.25x the full bytes, so the doubling premise of the scaling ratio
    # no longer holds. The premise assertion must catch this before any
    # timing is measured (the ratio itself would also redden, but later and
    # for the wrong reason: a quarter input reports ~4x growth).
    ('halving-unsound', 'src/pilotdeck-gateway/tests/html_markdown.rs',
     '            rule["repeat"] = (target / 2).into();',
     '            rule["repeat"] = (target / 4).into();',
     ['--test', 'html_markdown', 'large_shallow_pages_match_real_ts_bytes_and_stay_fast'],
     dict(all=('#978', 'halved recipe'))),
]
# #1666: static denominator; a deleted row must not shrink the run silently and
# an emptied table must never print `0/0 killed` and exit 0.
EXPECTED_NAMES = (
    'frame-stack-rescan', 'moved-subtree-rewalk', 'materializing-join',
    'materializing-join-reparent', 'halving-unsound',
)


def run_timing_test(target, extra_env, timeout):
    env = os.environ.copy()
    # Both runs must use this fixture's default sample count whatever the
    # caller exported: an inherited ``PDRS_TIMING_SAMPLES=1`` would silently
    # put the red run back on the one-sample path CI never takes (#978 second
    # review). ``extra_env`` can still override it explicitly.
    env.pop('PDRS_TIMING_SAMPLES', None)
    env.update(extra_env)
    wait_for_slot(root / 'scripts/cargo-slots.sh')
    return run_owned(['cargo', 'test', '--release', '--locked', '--offline',
                      '--manifest-path', 'src/Cargo.toml', '-p', 'pilotdeck-gateway',
                      *target, '--', '--test-threads=1'],
                     cwd=root, env=env, timeout=timeout)


TIMING_MARKERS = ('did not scale from', 'per-sample budget', 'halved recipe')


def outcome(result, test, expects=None):
    """A KILLED mutant must fail the named test at its dedicated message.

    A non-zero exit from an unrelated unwrap, a compile error or a timeout is
    INVALID, never KILLED: the mutant must bind to the assertion under test.
    ``expects`` is a mapping with ``all`` tokens (required) and optional ``any``
    alternatives (exactly one dedicated line is enough).

    #1087 fix-r2 (boundary P1): the failure line is taken from the tested
    test's own panic payload (``panic_evidence``), and the kill additionally
    requires the shared strict #813 judge — that test's own line-anchored
    FAILED status line. The old whole-log scan also matched a sibling test's
    panic, captured stdout and build output — the false-kill shapes the twin
    controls in test_negative_controls.py pin.
    """
    expects = expects or {}
    required = expects.get('all', ())
    alternatives = expects.get('any', ())
    log = result.stdout + result.stderr
    evidence = panic_evidence(log, test)
    failure = next((line.strip() for line in evidence.split('\n')
                    if any(marker in line for marker in TIMING_MARKERS)), '')
    matched = (all(token in failure for token in required)
               and (not alternatives
                    or any(token in failure for token in alternatives)))
    # #1087 fix-r4 (same-family sweep): the kill requires libtest's own
    # FAILED summary line too — an aborted run that already printed the
    # named test's FAILED line and payload (the ``--nocapture`` SIGKILL
    # shape) is not a kill. A timeout is already INVALID above (the
    # caller's TimeoutExpired branch), so this only closes the SIGKILL /
    # crash shape. #1087 fix-r5 (simplify #2): the FAILED summary requirement
    # moved into the shared named_test_killed default (require_summary=True).
    killed = (bool(failure) and matched
              and named_test_killed(log, test, failure))
    return log, killed, failure


def record(summary, entry):
    """Append a row in memory; the disk summary is published once, later.

    The outcome of an earlier row must never be readable as the final result
    while the guarded restore is still unverified (#1087 fix-r1): the old
    per-row ``write_text`` here is what left an all-KILLED summary on disk when
    a cancellation killed the run mid-suite.
    """
    summary.append(entry)
    reason = f" ({entry['reason']})" if entry['reason'] else ''
    print(f"{entry['name']}: {entry['outcome']}{reason}", flush=True)


def write_summary(out, summary):
    """Publish the one final summary atomically (#1087 fix-r1 / #1061).

    This is called exactly once per normal run, after every guarded file was
    restored and byte-verified, and once from the cancellation handler
    (INVALID). A reader that checks "the file exists" must never observe a
    half-written JSON document, so the payload goes to a sibling temp file and
    is renamed over ``summary.json``.
    """
    target = out / 'summary.json'
    tmp = out / 'summary.json.tmp'
    tmp.write_text(json.dumps(summary, ensure_ascii=False, indent=2) + '\n')
    tmp.replace(target)


def main(argv=None):
    argv = list(sys.argv if argv is None else argv)
    # #1734: caller-owned preconditions are checked before the evidence directory
    # exists, so "never started" (2) really leaves nothing behind.
    if missing_target():  # #1666: absent, empty or blank, one shared predicate
        refuse('CARGO_TARGET_DIR must come from the caller; refusing a private build directory')
    refuse_unusable_target()  # #1666 fix-r9: dot / root / src are unusable too

    # Local rehearsals of an uncommitted fix point this at ``git stash create``,
    # the same pre-commit escape hatch used by the other fixtures' controls
    # (live-recording, turn-runner, network-timing, ...). The host runs the
    # default HEAD, i.e. the committed tree.
    # #1540: a fully-green run against that pinned baseline is NON-HOST EVIDENCE
    # (the host gate only trusts HEAD), so it exits 42 instead of 0.
    baseline_source, baseline, baseline_sha = non_head.gate(root)
    selected = argv[2:]
    # #1666: ordinary raises, so `python3 -O` cannot strip these guards.
    try:
        registered(mutations, EXPECTED_NAMES)
        # `mutations` stays the frozen literal so `anchor_selfcheck.py` can read it
        # statically; the requested subset is a separate name.
        run_rows = select(mutations, selected, always=())
    except ValueError as error:
        refuse(str(error))
    out = claim_dir(argv[1])
    summary = []

    try:
        with mutating_sources(root, sorted({root / filename for _, filename, _, _, _, _ in run_rows}),
                              baseline=baseline) as snapshots:
            for name, filename, old, new, target, expects in run_rows:
                path = root / filename
                original = snapshots[path]
                before = hashlib.sha256(original).hexdigest()
                content = original.decode()
                occurrences = content.count(old)
                if occurrences != 1:
                    raise ValueError(f'{name}: anchor occurs {occurrences} times (expected 1)')
                changed = content.replace(old, new, 1)
                (out / f'{name}.patch').write_text(''.join(difflib.unified_diff(
                    content.splitlines(True), changed.splitlines(True),
                    fromfile=filename, tofile=filename)))
                test = target[-1]
                entry = dict(name=name, sourceSha256=before, outcome='INVALID', reason='')
                mutated = False

                try:
                    # Pristine tree first: the same assertion must pass before the
                    # mutant is applied.
                    result = run_timing_test(target, {}, timeout=1200)
                    log, _, failure = outcome(result, test)
                    (out / f'{name}.green.log').write_text(log)
                    if result.returncode != 0:
                        raise RuntimeError(
                            f'pristine tree did not pass: {failure or result.returncode}')
                    entry['greenPassed'] = True

                    # #1087 fix-r1: every guarded-source write in the window is
                    # one atomic rename (#1903 rule, shared with
                    # ``mutating_sources``) — a truncating ``write_text`` killed
                    # mid-write leaves an empty or half-written product source.
                    write_source_atomic(root, path, changed.encode(),
                                        mode=path.stat().st_mode & 0o7777)
                    mutated = True
                    # Same sample count as CI: this red run must exercise the median
                    # criterion, not a degenerate one-sample path (#978 first review).
                    # The helper collects every round before it asserts, so the red run
                    # now costs what the green run does times the mutant's slowdown;
                    # the per-call bound stays at this fixture's original 1800 s, which
                    # covers the slowest mutant (a quadratic join, tens of seconds per
                    # 8 MiB sample) with room to spare and keeps a hung mutant INVALID
                    # instead of blocking the chain.
                    result = run_timing_test(target, {}, timeout=1800)
                    log, killed, failure = outcome(result, test, expects)
                    (out / f'{name}.red.log').write_text(log)
                    entry.update(returncode=result.returncode, failure=failure)
                    if result.returncode == 0:
                        entry.update(outcome='SURVIVED', reason='mutant test passed')
                    elif not killed:
                        entry.update(outcome='INVALID',
                                     reason='failed without the expected dedicated message')
                    else:
                        entry['outcome'] = 'KILLED'
                except subprocess.TimeoutExpired as error:
                    entry.update(outcome='INVALID',
                                 reason=f'timed out after {error.timeout}s')
                except Exception as error:
                    entry.update(outcome='INVALID',
                                 reason=f'{type(error).__name__}: {error}')
                finally:
                    if mutated:
                        # Same #1087 fix-r1 window: the in-loop restore is also one
                        # rename, never a truncating write_bytes.
                        write_source_atomic(root, path, original,
                                           mode=path.stat().st_mode & 0o7777)
                        if hashlib.sha256(path.read_bytes()).hexdigest() != before:
                            entry.update(outcome='INVALID', reason='restore failed')

                # Restored tree: the test must come back green before the next mutant,
                # so a broken restoration cannot masquerade as a killed mutant.
                if entry['outcome'] == 'KILLED':
                    try:
                        result = run_timing_test(target, {}, timeout=1200)
                        log, _, failure = outcome(result, test)
                        (out / f'{name}.restored.log').write_text(log)
                        if result.returncode == 0:
                            entry['restoredPassed'] = True
                        else:
                            entry.update(
                                outcome='INVALID',
                                reason=f'restored tree did not pass: '
                                       f'{failure or result.returncode}')
                    except Exception as error:
                        entry.update(outcome='INVALID',
                                     reason=f'restored run: {type(error).__name__}: {error}')

                # #1087 fix-r1: the row stays in memory only; the summary is
                # published once, after the guard has verified every byte.
                record(summary, entry)
    except BaseException as error:
        reason = f'runner: {type(error).__name__}: {error}'
        # #1087 fix-r1: a cancellation or a guard failure voids every
        # already-recorded row in memory, then publishes INVALID. The disk
        # never held a final summary before this point (record() is memory
        # only), so an interrupt inside this publish — even a second SIGINT —
        # leaves the pre-existing bytes untouched instead of an all-KILLED
        # file; a missing/unchanged summary plus a non-zero exit is the
        # fail-closed signal.
        for entry in summary:
            entry['outcome'] = 'INVALID'
            entry['reason'] = (f"{entry['reason']}; voided: {reason}"
                               if entry['reason'] else f"voided: {reason}")
        for name, *_ in run_rows[len(summary):]:
            record(summary, dict(name=name, sourceSha256=None, outcome='INVALID',
                                 reason=reason))
        write_summary(out, summary)
        print(f'voided {len(summary)} row(s): {reason}',
              file=sys.stderr, flush=True)
        # The INVALID summary is on disk, so a SystemExit is normalized instead
        # of judged by its raw code: 256/-256 truncate to a zero process
        # status on POSIX and a string code exits 1, none of which may decide
        # how a coordinator reads this run (#1059 fix-r2).
        if isinstance(error, SystemExit):
            raise SystemExit(2) from error
        if not isinstance(error, Exception):
            raise
        return 1
    # Reaching here means mutating_sources restored every guarded file and
    # verified it against the baseline. Only now may a final summary exist.
    write_summary(out, summary)
    killed = [entry for entry in summary if entry['outcome'] == 'KILLED']
    for entry in summary:
        reason = f" ({entry['reason']})" if entry['reason'] else ''
        print(f"{entry['name']}: {entry['outcome']}{reason}")
    print(f'{len(killed)}/{len(summary)} killed; every source restored byte-for-byte')
    # #1666: a zero-row run is never success (`0/0 killed` used to exit 0).
    # #1540: green but non-HEAD is NON-HOST EVIDENCE and exits 42, never 0.
    return non_head.non_head_exit(baseline_source, baseline_sha,
                                  code=verdict_exit(summary, key='outcome'),
                                  evidence=out)


if __name__ == '__main__':
    raise SystemExit(main())
