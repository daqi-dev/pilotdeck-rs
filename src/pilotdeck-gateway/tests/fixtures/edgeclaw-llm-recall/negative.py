#!/usr/bin/env python3
"""Reversible product mutations for #765 retrieval-side LLM calls.

Every case must compile and fail the bound ``#765 ...`` assertion in the named
test. Anchors are checked against the baseline bytes before anything runs: a
stale anchor is a harness defect, so it prints ``STALE ANCHOR`` and exits 2
(never SURVIVED). A surviving case does not stop the run; every selected case
executes, then the script prints a KILLED / SURVIVED / INVALID summary and
exits 1 when anything is not KILLED.

``mutating_sources`` refuses to run unless the mutated sources match HEAD, so
use ``--check-anchors`` before committing changes and run the mutants once
those changes are committed:

    CARGO_TARGET_DIR=/abs/target python3 negative.py OUT_DIR
    CARGO_TARGET_DIR=/abs/target python3 negative.py OUT_DIR source-priority
    python3 negative.py --check-anchors
    python3 negative.py --list

Never run this in a shadow repository or alternate source tree.
"""
import json
import os
from pathlib import Path
import itertools
import re
import subprocess
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_panic import named_test_killed
from negative_process import (detach_dead_streams, mutating_sources,
                              run_owned, unique_summary_tmp,
                              wait_for_slot, write_source_atomic)

root = Path(__file__).resolve().parents[5]
recall = root / 'src/pilotdeck-gateway/src/runtime/memory/edgeclaw/llm/recall.rs'
transport = root / 'src/pilotdeck-gateway/src/runtime/memory/edgeclaw/llm/transport.rs'

# Per-invocation serial for the label-log run token below.
_RUN_SERIAL = itertools.count(1)

cases = [
    # Lower `general_local` below `workspace_external`, so the older local
    # project loses the fallback tie-break.
    # oracle: select-fallback-priority expects LocalOlder.
    ('source-priority', recall,
     '''        Some(value)
            if value.equals_str("general_local")
                || value.equals_str("workspace_external_mirror") =>
        {
            2.0
        }
        Some(value) if value.equals_str("workspace_external") => 1.0,''',
     '''        Some(value)
            if value.equals_str("general_local")
                || value.equals_str("workspace_external_mirror") =>
        {
            1.0
        }
        Some(value) if value.equals_str("workspace_external") => 3.0,''',
     'replay_matrix'),
    # Replace `localeCompare` with raw UTF-16 order: "a" then outranks "B".
    # oracle: select-fallback-updated-locale expects UpperB.
    ('fallback-updated', recall,
     '''    match node_compare_utf16(recall_collator(), &right.updated_at, &left.updated_at) {
        Ordering::Less => -1.0,
        Ordering::Equal => 0.0,
        Ordering::Greater => 1.0,
    }''',
     '''    match right.updated_at.units().cmp(left.updated_at.units()) {
        Ordering::Less => -1.0,
        Ordering::Equal => 0.0,
        Ordering::Greater => 1.0,
    }''',
     'replay_matrix'),
    # Do not trim the model id before matching; a padded id no longer matches.
    # oracle: select-match-whitespace-id expects project p1 without a fallback reason.
    ('selected-id-trim', recall,
     '        .map(|value| JsString::from_utf16(js_trim_units(value.units()).to_vec()))',
     '        .map(|value| value.clone())',
     'replay_matrix'),
    # Swap the two allowEmpty default reasons.
    # oracle: select-outside-allow-empty expects the outside-shortlist text.
    ('allow-empty-reason', recall,
     '                            JsString::from("Model returned a project id outside the shortlist.")',
     '                            JsString::from("Model returned no matching project.")',
     'replay_matrix'),
    # Send only 199 manifest entries; the 200th recorded prompt entry is lost.
    # oracle: manifest-200 records exactly 200.
    ('manifest-slice', recall,
     '                    .take(200)',
     '                    .take(199)',
     'replay_matrix'),
    # Drop the upper Math.min(5, ...) clamp for the manifest limit.
    # oracle: manifest-limit-over asks for limit 5 while the input is 9.
    ('manifest-limit', recall,
     '    js_max(1.0, js_min(5.0, limit.unwrap_or(5.0)))',
     '    js_max(1.0, limit.unwrap_or(5.0))',
     'replay_matrix'),
    # Use the last three, not four, recent messages of the decide prompt.
    # oracle: decide-recent-slice drops the fourth retained message.
    ('decide-recent-slice', recall,
     '''fn recent_message_pairs(messages: &[MemoryMessage]) -> Vec<JsValue> {
    let start = messages.len().saturating_sub(4);''',
     '''fn recent_message_pairs(messages: &[MemoryMessage]) -> Vec<JsValue> {
    let start = messages.len().saturating_sub(3);''',
     'replay_matrix'),
    # Same off-by-one for the user-message strings used by the other prompts.
    # oracle: select-recent-user-slice drops "message 2".
    ('user-recent-slice', recall,
     '''fn recent_user_texts(messages: &[MemoryMessage]) -> Vec<JsValue> {
    let start = messages.len().saturating_sub(4);''',
     '''fn recent_user_texts(messages: &[MemoryMessage]) -> Vec<JsValue> {
    let start = messages.len().saturating_sub(3);''',
     'replay_matrix'),
    # Render a missing sourceType as "none" instead of "unknown".
    # oracle: select-source-unknown prompt source_type.
    ('project-source-unknown', recall,
     '            None => JsValue::String("unknown".into()),',
     '            None => JsValue::String("none".into()),',
     'replay_matrix'),
    # Feed the fallback reason the project id instead of the project name.
    # This keeps the production expression shape (`fallback_name`) and only
    # changes the bound value, so the mutant compiles; the named replay case
    # still fails on the fallback reason text it pins.
    # oracle: select-outside-fallback expects "Fallback selected Alpha".
    ('fallback-reason-name', recall,
     '''        let (fallback_id, fallback_name) =
            (fallback.project_id.clone(), fallback.project_name.clone());''',
     '''        let (fallback_id, fallback_name) =
            (fallback.project_id.clone(), fallback.project_id.clone());''',
     'replay_matrix'),
    # Off-by-one on the manifest description bound (200 -> 201).
    # oracle: manifest-description-truncate prompt description.
    ('manifest-description-bound', recall,
     '        JsValue::String(truncate_for_prompt(&entry.description, 200.0)),',
     '        JsValue::String(truncate_for_prompt(&entry.description, 201.0)),',
     'replay_matrix'),
    # Off-by-one on the shortlist description bound (180 -> 181).
    # oracle: select-description-truncate prompt description.
    ('shortlist-description-bound', recall,
     '        JsValue::String(truncate_for_prompt(&project.description, 180.0)),',
     '        JsValue::String(truncate_for_prompt(&project.description, 181.0)),',
     'replay_matrix'),
    # Off-by-one on the project-selection reason bound (220 -> 221).
    # oracle: select-reason-truncate result length.
    ('reason-bound', recall,
     '    Some(truncate_for_prompt(reason, 220.0))',
     '    Some(truncate_for_prompt(reason, 221.0))',
     'replay_matrix'),
    # JSON.stringify(..., null, 2): double the indentation width.
    # oracle: every recorded request body.
    ('pretty-indent', recall,
     "    out.extend(std::iter::repeat_n(' ', depth * 2));",
     "    out.extend(std::iter::repeat_n(' ', depth * 4));",
     'replay_matrix'),
    # P1-1: warning text is the TS prefix plus `String(error)`. Each of the
    # three catch blocks is bound to its own dedicated message.
    ('warning-gate', recall,
     '"[clawxmemory] file memory gate fallback: "',
     '"[clawxmemory] file memory gate fallback!: "',
     'fallback_warnings_match_ts'),
    ('warning-project', recall,
     '"[clawxmemory] file memory project selection fallback: "',
     '"[clawxmemory] file memory project selection fallback!: "',
     'fallback_warnings_match_ts'),
    ('warning-selection', recall,
     '"[clawxmemory] file memory selection fallback: "',
     '"[clawxmemory] file memory selection fallback!: "',
     'fallback_warnings_match_ts'),
    # `String(error)` is `name: message`, not `message` alone.
    ('warning-error-string', recall,
     '    units.extend_from_slice(error.js_string().units());',
     '    units.extend_from_slice(error.message.units());',
     'fallback_warnings_match_ts'),
    # P1-1 order: TS emits the errored debug record before the fallback warning.
    ('warning-order', transport,
     '''        debug(JsValue::Object(record));
        result''',
     '''        if result.is_ok() {
            debug(JsValue::Object(record));
        }
        result''',
     'fallback_warnings_match_ts'),
    # P1-1 separation: the warning must not be folded into the debug record.
    ('warning-in-debug', transport,
     '''        debug(JsValue::Object(record));
        result''',
     '''        if result.is_err() {
            record.insert(
                "errorMessage".into(),
                JsValue::String(JsString::from("[clawxmemory] file memory")),
            );
        }
        debug(JsValue::Object(record));
        result''',
     'fallback_warnings_match_ts'),
    # Reachable ±0: TS `!==` treats +0 and -0 as equal. `total_cmp` does not,
    # so it bypasses the score tie-break and the plus-zero entry wins.
    ('sort-total-cmp', recall,
     '''    if right.exact != left.exact {
        return right.exact - left.exact;
    }''',
     '''    match right.exact.total_cmp(&left.exact) {
        Ordering::Equal => {}
        Ordering::Greater => return 1.0,
        Ordering::Less => return -1.0,
    }''',
     'js_fallback_sort_matches_ts'),
    # Reachable exact direction: descending exact is the first tier. Flipping
    # it lets the lower-exact entry win and must fail the hand test's exact
    # assertion (the ±0 case has equal exact values, so it still passes).
    ('sort-exact-order', recall,
     '''    if right.exact != left.exact {
        return right.exact - left.exact;
    }''',
     '''    if right.exact != left.exact {
        return left.exact - right.exact;
    }''',
     'js_fallback_sort_matches_ts'),
    # Reachable ties: `Array#sort` treats a 0 comparator result as equal and
    # keeps the stable input order. Dropping the +0/-0 fold returns Less for a
    # full tie, so the stable pair flips. (The old NaN-domain flavour of this
    # mutant no longer has a NaN assertion to kill: #765 fix-r4 removed NaN
    # from `js_fallback_sort_matches_ts` because the producer cannot emit it.)
    ('sort-nan-order', recall,
     '''fn js_sort_order(delta: f64) -> Ordering {
    if delta.is_nan() || delta == 0.0 {
        Ordering::Equal
    } else if delta < 0.0 {
        Ordering::Less
    } else {
        Ordering::Greater
    }
}''',
     '''fn js_sort_order(delta: f64) -> Ordering {
    if delta > 0.0 {
        Ordering::Greater
    } else {
        Ordering::Less
    }
}''',
     'js_fallback_sort_matches_ts'),
    # P1-3: sort the reachable producer domain (exact 0/2,
    # score = exact*10 + matched tokens) descending. The TS gold sample is the
    # fixture-driven check, so a wrong direction must fail it at the first
    # fixture case whose maximum is not the first element.
    ('sort-reachable-order', recall,
     '''    if right.score != left.score {
        return right.score - left.score;
    }''',
     '''    if right.score != left.score {
        return left.score - right.score;
    }''',
     'js_fallback_sort_matches_ts_reachable_domain'),
]

EXPECTED = {
    'source-priority': '#765 result select-fallback-priority',
    'fallback-updated': '#765 result select-fallback-updated-locale',
    'selected-id-trim': '#765 result select-match-whitespace-id',
    'allow-empty-reason': '#765 result select-outside-allow-empty',
    'manifest-slice': '#765 requests manifest-200',
    'manifest-limit': '#765 result manifest-limit-over',
    'decide-recent-slice': '#765 requests decide-recent-slice',
    'user-recent-slice': '#765 requests select-recent-user-slice',
    'project-source-unknown': '#765 requests select-source-unknown',
    'fallback-reason-name': '#765 result select-outside-fallback',
    'manifest-description-bound': '#765 requests manifest-description-truncate',
    'shortlist-description-bound': '#765 requests select-description-truncate',
    'reason-bound': '#765 result select-reason-truncate',
    'pretty-indent': '#765 requests decide-project',
    'warning-gate': '#765 gate warning',
    'warning-project': '#765 project selection warning',
    'warning-selection': '#765 selection warning',
    'warning-error-string': '#765 gate warning',
    'warning-order': '#765 warning order',
    'warning-in-debug': '#765 warning must not enter debug 0',
    'sort-total-cmp': '#765 minus zero and plus zero are equal',
    'sort-exact-order': '#765 reachable exact order',
    'sort-nan-order': '#765 full tie preserves input order',
    'sort-reachable-order': '#765 reachable-domain sort first mismatch',
}


def classify(log, returncode, expected, test):
    # #1087 fix-r2: the kill criterion is the shared strict #813 judge — the
    # named test's own FAILED status line plus the bound assertion inside
    # that test's own panic payload (negative_panic judges both). The old
    # whole-log `expected in log` also matched a sibling test's panic,
    # captured stdout and build output — the false-kill shapes the twin
    # controls in test_negative.py pin. Defined before the argv dispatch so
    # the twin controls can import the module and drive it directly.
    if returncode is None:
        return 'INVALID', 'no exit code'
    if 'could not compile' in log:
        return 'INVALID', 'compiler failure'
    if returncode != 0 and 'test result: FAILED.' in log and 'running 1 test' in log:
        if named_test_killed(log, test, expected):
            return 'KILLED', expected
        return 'INVALID', f'failed without the bound assertion {expected}'
    if returncode == 0 and 'test result: ok.' in log:
        return 'SURVIVED', 'test stayed green'
    return 'INVALID', 'unclassified output'


names = [case[0] for case in cases]
assert len(names) == len(set(names)), 'duplicate control name'
all_cases = list(cases)
if sys.argv[1:] == ['--list']:
    print('\n'.join(names))
    raise SystemExit(0)
if sys.argv[1:] == ['--check-anchors']:
    # The preflight reads into `current`, not `originals` (#1087 fix-r3,
    # boundary #1): the guarded window below now sits under a cancellation
    # try, so the `with mutating_sources(...) as originals` capture is no
    # longer a module-level statement and anchor_selfcheck can no longer
    # exempt this preflight's `originals` binding from its one-declaration
    # rule. Keeping the snapshot name unique to the capture keeps the
    # preflight readable by the static checker instead of unsupported.
    current = {path: path.read_text() for path in {case[1] for case in cases}}
    stale = []
    for name, path, before, _after, _test in all_cases:
        count = current[path].count(before)
        if count != 1:
            stale.append((name, count, path.relative_to(root).as_posix()))
    if stale:
        try:
            for name, count, relative in stale:
                print(f'STALE ANCHOR {name}: {count} matches in {relative}')
            print('STALE ANCHORS: negative.py is out of sync with its sources; refusing to run')
        except (OSError, ValueError):
            # #1087 fix-r5 (boundary #1 family): same rule as the in-window
            # refusal — a dead stdout must not rewrite this exit code.
            pass
        raise SystemExit(2)
    try:
        print(f'{len(all_cases)} anchors: ok')
    except (OSError, ValueError):
        pass
    raise SystemExit(0)

out = Path(sys.argv[1]).resolve() if len(sys.argv) > 1 else None


def _refuse_summary(reason):
    """Invalidate a reused directory's stale summary (#1087 fix-r4, concurrency #3).

    The evidence directory is claimed with ``mkdir(exist_ok=True)`` so a host
    re-run can land in the same path; an exit-2 "never started" refusal
    must not leave the previous run's green ``summary.json`` readable
    there as this run's evidence. The marker is one rename, carries no
    verdict rows, and leaves a fresh directory untouched — before the first
    run there is nothing to invalidate.

    #1087 fix-r5 (boundary #3): a failing marker write must not turn the
    refusal into a traceback (rc=1, "ran and was red") — the reason is
    reported best-effort and the caller keeps its exit-2 code either way.
    """
    if out is None or not (out / 'summary.json').exists():
        return
    try:
        tmp = unique_summary_tmp(out)
        tmp.write_text(json.dumps({'refused': reason}, indent=2) + '\n')
        tmp.replace(out / 'summary.json')
    except OSError as error:
        try:
            print(f'could not invalidate the reused summary: {error}',
                  file=sys.stderr)
        except (OSError, ValueError):
            pass


def _refuse(message):
    """#1087 fix-r5 (concurrency #1): an early check is a refusal, not a crash.

    The old ``assert`` traceback exited 1 — "ran and was red" — and left a
    reused directory's previous green summary readable as this run's
    evidence. Now the stale summary is invalidated by one marker rename and
    the run exits 2 ("never started"), the same contract the in-window
    stale-anchor refusal keeps.
    """
    _refuse_summary(f'{message}: run refused')
    try:
        print(f'INVALID: {message}', file=sys.stderr)
    except (OSError, ValueError):
        # #1087 fix-r5 (boundary #1): stderr may be dead (`2>&-` gives
        # sys.stderr=None, a closed stream raises); the code is the contract.
        pass
    raise SystemExit(2)


if out is None:
    _refuse('usage: negative.py OUT_DIR [NAME ...] | negative.py --list | --check-anchors')
selected = sys.argv[2:]
unknown = set(selected) - set(names)
if unknown:
    _refuse(f'unknown controls: {sorted(unknown)}')
if selected:
    cases = [case for case in cases if case[0] in selected]
if not cases:
    _refuse('no controls selected')

target = os.environ.get('CARGO_TARGET_DIR')
if not target:
    _refuse('negative.py requires CARGO_TARGET_DIR from the caller')
out.mkdir(parents=True, exist_ok=True)
run_token = f'{os.getpid()}.{next(_RUN_SERIAL)}'
paths = sorted({case[1] for case in cases})

env = dict(os.environ)
env['CARGO_TARGET_DIR'] = target
command = ['cargo', 'test', '--manifest-path', str(root / 'src/Cargo.toml'),
           '-p', 'pilotdeck-gateway', '--test', 'edgeclaw_llm_recall']
results = []
# #1087 fix-r3 (boundary #1): bound before the guarded window so the
# cancellation handler can always publish, even when the abort happened
# before the baseline run recorded anything.
baseline = {'name': 'baseline', 'outcome': 'INVALID', 'returncode': None}


def write_summary(current_baseline):
    """Publish the one final summary atomically (#1087 fix-r3, boundary #1).

    Called exactly once per run, after ``mutating_sources`` restored and
    byte-verified every guarded source — or from the cancellation handler,
    after the same guard unwound. The old per-row truncating ``write_text``
    left a partial all-KILLED summary on disk when a cancellation killed the
    run mid-suite, and an interrupt inside the write could leave an empty or
    half-written JSON document.
    """
    tmp = unique_summary_tmp(out)
    tmp.write_text(json.dumps(
        {'baseline': current_baseline, 'cases': results}, indent=2) + '\n')
    tmp.replace(out / 'summary.json')


def _void_pending(error):
    """Void the recorded rows and publish one INVALID summary (boundary #1).

    A cancellation or crash inside the guarded window is not a verdict: the
    rows recorded so far are voided, the remaining selected cases are
    recorded INVALID, and the single summary is published after the guard
    restored the tree. The process then exits 3 — deliberately different
    from 2 ("never started", the stale-anchor refusal below) so a reader can
    tell a cancelled run from one that never ran.
    """
    reason = f'runner: {type(error).__name__}: {error}'
    for row in results:
        row['outcome'] = 'INVALID'
        row['reason'] = (f"{row['reason']}; voided: {reason}"
                         if row['reason'] else f"voided: {reason}")
    for name, _path, before, after, test in cases[len(results):]:
        results.append({'name': name, 'test': test, 'outcome': 'INVALID',
                        'reason': reason, 'returncode': None,
                        'anchor': before, 'replacement': after})
    write_summary(baseline)
    try:
        print(f'voided {len(results)} row(s): {reason}', file=sys.stderr, flush=True)
    except (OSError, ValueError):
        # #1087 fix-r4 (concurrency #2): the summary is already published, so
        # a closed stderr must not replace the exit code below — "aborted = 3"
        # is the only contract left to keep. #1087 fix-r5: ValueError covers
        # an in-process closed stream, OSError the real dead fd.
        pass
    if isinstance(error, (OSError, ValueError)):
        # #1087 fix-r5 (concurrency #2): a stream-failure abort detaches the
        # dead fds first, so the interpreter-shutdown flush cannot rewrite
        # the exit code below to 120 ("Exception ignored in:").
        detach_dead_streams()
    if isinstance(error, SystemExit):
        # Normalize: 128+signum and any other raw code must not decide how a
        # coordinator reads this run (a 256/-256 code truncates to a zero
        # process status on POSIX).
        raise SystemExit(3) from error
    if not isinstance(error, Exception):
        raise
    raise SystemExit(3) from error



try:
    with mutating_sources(root, paths) as originals:
        stale = []
        # Only the selected cases own snapshot bytes; scanning `all_cases` here
        # would look up files that a name-filtered run never snapshotted
        # (`negative.py OUT_DIR source-priority` -> KeyError on transport.rs).
        for name, path, before, _after, _test in cases:
            count = originals[path].decode().count(before)
            if count != 1:
                stale.append((name, path.relative_to(root).as_posix(), count))
        if stale:
            report = '\n'.join(
                f'STALE ANCHOR {name}: {count} matches in {relative}'
                for name, relative, count in stale) + '\n'
            (out / f'stale-anchors.{run_token}.log').write_text(report)
            try:
                print(report, end='', flush=True)
                print('STALE ANCHORS: negative.py is out of sync with its sources; refusing to run',
                      flush=True)
            except (OSError, ValueError):
            # #1087 fix-r5 (boundary #1 family): a dead stdout must not
            # turn this exit-2 refusal into an in-window abort (the
            # report is already on disk in the stale-anchors log).
                pass
            raise SystemExit(2)
        baseline_ok = False
        try:
            try:
                slots = wait_for_slot(root / 'scripts/cargo-slots.sh')
                slots_text = slots.stdout + slots.stderr
                run = run_owned(command + ['replay_matrix', '--', '--exact'],
                                cwd=root, env=env, timeout=600)
                log = slots_text + run.stdout + run.stderr
                returncode = run.returncode
            except subprocess.TimeoutExpired as error:
                log = (error.stdout or '') + (error.stderr or '')
                returncode = None
            (out / f'baseline.{run_token}.log').write_text(log)
            passed = re.search(r'^test result: ok\. \d+ passed; 0 failed;', log, re.MULTILINE)
            baseline_ok = returncode == 0 and passed is not None
            baseline = {'name': 'baseline', 'outcome': 'PASS' if baseline_ok else 'FAIL',
                        'returncode': returncode}
            print(f'baseline: {"PASS" if baseline_ok else "FAIL"} exit={returncode}', flush=True)
            if not baseline_ok:
                print('baseline failed; mutants not run', flush=True)
                raise SystemExit(1)

            for name, path, before, after, test in cases:
                slots_text = ''
                log = ''
                returncode = None
                try:
                    slots = wait_for_slot(root / 'scripts/cargo-slots.sh')
                    slots_text = slots.stdout + slots.stderr
                    # Snapshot bytes come only from mutating_sources; the mutation
                    # itself is a single exact anchor replacement.
                    original = originals[path].decode()
                    # #1087 fix-r2: every guarded-source write in the window is
                    # one atomic rename (#1903 rule, shared with
                    # ``mutating_sources``) — a truncating ``write_bytes`` killed
                    # mid-write leaves an empty or half-written product source.
                    write_source_atomic(root, path,
                                         original.replace(before, after).encode(),
                                         mode=path.stat().st_mode & 0o7777)
                    try:
                        run = run_owned(
                            command + [test, '--', '--exact'],
                            cwd=root, env=env, timeout=600)
                        log = run.stdout + run.stderr
                        returncode = run.returncode
                    finally:
                        # Same #1087 fix-r2 window: the in-loop restore is also
                        # one rename, never a truncating write_bytes.
                        write_source_atomic(root, path, originals[path],
                                            mode=path.stat().st_mode & 0o7777)
                except subprocess.TimeoutExpired as error:
                    log = (error.stdout or '') + (error.stderr or '')
                    returncode = None
                except Exception as error:  # noqa: BLE001 - report, never hide a cell
                    log = f'{type(error).__name__}: {error}\n'
                    returncode = None
                outcome, reason = classify(log, returncode, EXPECTED[name], test)
                if outcome == 'KILLED' and not baseline_ok:
                    outcome, reason = 'INVALID', 'baseline was not green'
                (out / f'{name}.{run_token}.log').write_text(
                    'COMMAND ' + ' '.join(command + [test, '--', '--exact']) + '\n' +
                    slots_text + log + f'\nexit={returncode}\nexpected={EXPECTED[name]}\n')
                results.append({'name': name, 'test': test, 'outcome': outcome,
                                'reason': reason, 'returncode': returncode,
                                'anchor': before, 'replacement': after})
                print(f'{name}: {outcome} ({reason}) exit={returncode}', flush=True)
        finally:
            for path, data in originals.items():
                # Same #1087 fix-r2 window: the final restore is also one rename,
                # never a truncating write_bytes.
                write_source_atomic(root, path, data,
                                    mode=path.stat().st_mode & 0o7777)
except SystemExit as error:
    if error.code == 1:
        # The deliberate baseline failure: the guard already restored and
        # byte-verified the tree while unwinding the with-block; publish the
        # baseline row and keep the "ran and failed" exit code.
        write_summary(baseline)
        raise
    if error.code == 2:
        # The deliberate stale-anchor refusal: nothing was measured, so
        # nothing is published. The directory claim above allows a host
        # re-run into the same path, so a previous run's green summary is
        # invalidated before the refusal propagates — it must not stay
        # readable as this run's evidence (#1087 fix-r4, concurrency #3).
        _refuse_summary('stale anchors: negative.py is out of sync with its sources')
        raise
    _void_pending(error)
except BaseException as error:  # noqa: BLE001 - void, publish, then re-raise
    _void_pending(error)
# Reaching here means mutating_sources restored every guarded file and
# verified it against the baseline bytes. Only now may a summary exist.
write_summary(baseline)

killed = [row['name'] for row in results if row['outcome'] == 'KILLED']
survived = [row['name'] for row in results if row['outcome'] == 'SURVIVED']
invalid = [row['name'] for row in results if row['outcome'] == 'INVALID']
print(f'{len(results)} mutants: KILLED {len(killed)} / SURVIVED {len(survived)} / INVALID {len(invalid)}')
if survived:
    print('SURVIVED: ' + ', '.join(survived))
if invalid:
    print('INVALID: ' + ', '.join(invalid))
raise SystemExit(0 if baseline['outcome'] == 'PASS' and not survived and not invalid else 1)
