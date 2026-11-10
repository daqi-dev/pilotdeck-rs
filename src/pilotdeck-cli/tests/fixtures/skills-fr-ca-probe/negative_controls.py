#!/usr/bin/env python3
"""#2140 HEAD-only CLI skills-collator controls: --check | --check-anchors |
--self-check | [--compile-only] NEW_OUTPUT_DIR [mutation ...].

Run after the coordinator commits the patch. This script deliberately refuses
uncommitted/new sources (mutating_sources proves every byte against HEAD), so
the full KILLED/SURVIVED verdicts are produced on the host; --check /
--check-anchors work pre-commit. The caller supplies CARGO_TARGET_DIR; no
alternate repository or build directory is created. Startup refusals
(unknown option/mutation, roster drift, unusable target, existing output
directory) exit 2 = "never started", never a traceback that reads as a failed
control (#1734 / #1666 fix-r7).
"""
import io
import os
import signal
import sys
from pathlib import Path

sys.dont_write_bytecode = True
root = Path(__file__).resolve().parents[5]
sys.path.insert(0, str(root / 'src/pilotdeck-gateway/tests/fixtures'))
from negative_process import (defer_signal_exit, mutating_sources, run_owned,
                              wait_for_slot, write_source_atomic)
from negative_selection import (claim_dir, missing_target, refuse,
                                 refuse_unusable_target, registered,
                                 verdict_exit)
from negative_suite import Case, classify, invalidate_completed, passed, publish_summary

SKILLS = root / 'src/pilotdeck-cli/src/skills.rs'
PROBE_JSON = root / 'src/pilotdeck-cli/tests/fixtures/skills-fr-ca-probe.json'
# pilotdeck-cli is a binary-only package, so the unit tests in
# `src/skills.rs` run under the `pilotdeck` bin target, not `--lib`.
TEST = 'skills::tests::fr_ca_secondary_matches_recorded_node_probe'
mutations = [
    # Dropping the fr-CA backward-secondary repair makes the default collator
    # order accents at the secondary level (generic fr), against the recorded
    # fr_CA.UTF-8 row's sign 1 for `coté` vs `côte`.
    ('fr-ca-repair-dropped', SKILLS,
     'collator.with_backward_second_level(true)', 'collator',
     TEST, '#2140 fr_CA.UTF-8: "coté" vs "côte"'),
    # Widening the repair to every `fr` language (no CA region) reorders the
    # fr_FR.UTF-8 row, which Node orders like generic fr (sign -1).
    ('fr-ca-region-widened', SKILLS,
     '                && locale\n                    .id\n                    .region\n'
     '                    .is_some_and(|region| region.as_str() == "CA")\n',
     '',
     TEST, '#2140 fr_FR.UTF-8: "coté" vs "côte"'),
    # Losing the Latn script gate applies the same repair widening as above to
    # the default collator order for the fr_Latn_CA.UTF-8 row (sign 1).
    # `Option::is_none_or` still returns true for the script-less fr_CA row,
    # so only the explicit-script rows move (gateway twin: native-collation's
    # french-script-fallback-lost). fix-r2: the replacement keeps the twin's
    # `|_|` form -- replacing only the inner comparison would leave `script`
    # unused, and a host that compiles with `-D warnings` would fail the
    # mutant build and read the whole suite as INVALID (grep-etxtbsy README
    # precedent), so the anchor covers the whole closure instead.
    ('fr-ca-script-lost', SKILLS,
     '.is_none_or(|script| script.to_string() == "Latn")',
     '.is_none_or(|_| false)',
     TEST, '#2140 fr_Latn_CA.UTF-8: "coté" vs "côte"'),
    # Widening the Latn gate to every script repairs the fr_Cyrl_CA.UTF-8 row
    # (sign -1, no repair), the direction the #2170 gateway test pins
    # (gateway twin: native-collation's french-script-widened).
    ('fr-ca-script-widened', SKILLS,
     '.is_none_or(|script| script.to_string() == "Latn")',
     '.is_none_or(|_| true)',
     TEST, '#2140 fr_Cyrl_CA.UTF-8: "coté" vs "côte"'),
    # #2170 (DS r1 sibling#5): the `resolved` audit in skills.rs had no
    # paired mutation. Rewriting the fixture's recorded locale for the Cyrl
    # row must turn that audit red — the audit panics when the leading
    # resolved subtag ("en") differs from the row language ("fr")
    # (fix-r5: equality, not starts_with).
    ('probe-resolved-rewritten', PROBE_JSON,
     '"resolved": "fr-Cyrl-CA"', '"resolved": "en-US"',
     TEST, '#2170 fr_Cyrl_CA.UTF-8: resolved'),
    # #2170 fix-r4 (DS fix-r3 sibling#1): the empty-language audit assert
    # had no paired mutation. Emptying the fr_CA row's language subtag must
    # turn that assert red; deleting the assert from skills.rs would leave
    # this row INVALID (the pair replay still fails, but the panic payload
    # lacks the marker — classified non-KILLED either way).
    ('probe-language-emptied', PROBE_JSON,
     '"lang": "fr_CA.UTF-8"', '"lang": "_CA.UTF-8"',
     TEST, '#2170 _CA.UTF-8: probe row needs a language subtag'),
    # #2170 fix-r6 (DS r7 sibling#2): the fix-r5 equality audit needs a
    # mutation a prefix comparison would let through — poisoning the fr_FR
    # row's resolved to "frr-FR" keeps starts_with("fr") true but fails the
    # first-subtag equality, so reverting skills.rs to the prefix form
    # leaves this row SURVIVED (the pair replay also passes).
    ('probe-resolved-prefix-poisoned', PROBE_JSON,
     '"resolved": "fr-FR"', '"resolved": "frr-FR"',
     TEST, '#2170 fr_FR.UTF-8: resolved'),
]
# fix-r1 (DS simplify#1, #1666): the run denominator must be a literal, not
# `len(mutations)` — a dropped row would silently shrink the run.
EXPECTED_NAMES = (
    'fr-ca-repair-dropped',
    'fr-ca-region-widened',
    'fr-ca-script-lost',
    'fr-ca-script-widened',
    'probe-resolved-rewritten',
    'probe-language-emptied',
    'probe-resolved-prefix-poisoned',
)


def check_roster():
    try:
        registered(mutations, EXPECTED_NAMES)
    except ValueError as error:
        refuse(str(error))


def row_error(name, count, old, new, test, marker):
    """One validator for BOTH anchor paths (fix-r3, DS r3 sibling#1 /
    simplify#1 / boundary#1: the run loop used to check only count and
    old == new, so an empty marker survived into classify(), where
    `'' in evidence` matches ANY panic -- a kill for the wrong reason)."""
    if count != 1 or old == new:
        return f'{name}: stale/invalid anchor: expected=1 actual={count}'
    if not test or not marker:
        return f'{name}: empty test or marker binding'
    return None


def check_anchors(stdout=None, stderr=None):
    """Exit non-zero on any expired anchor; an empty manifest never passes."""
    stdout = sys.stdout if stdout is None else stdout
    stderr = sys.stderr if stderr is None else stderr
    errors = []
    # fix-r3 (anthropic sibling#1): count is deduplicated per (path, old)
    # anchor, mirroring the gateway twin — the table shares anchors (the two
    # script rows bind the same `is_none_or` closure).
    seen, duplicates = set(), []
    for name, path, old, new, test, marker in mutations:
        key = (str(path), old)
        if key in seen:
            duplicates.append(name)
        seen.add(key)
        count = path.read_text(encoding='utf-8').count(old)
        error = row_error(name, count, old, new, test, marker)
        if error:
            errors.append(error)
    for error in errors:
        print(error, file=stdout)
    if not mutations:
        print('#2140 anchors: empty mutation manifest; refusing to pass', file=stderr)
        return 2
    print(f'#2140 anchors: {len(seen)} unique anchors checked ({len(mutations)} rows)',
          file=stdout)
    if duplicates:
        print('rows sharing an anchor: ' + ', '.join(duplicates), file=stdout)
    return int(bool(errors))


def self_check():
    """Negative control for this gate: an empty manifest must fail closed (#1364 P0-a)."""
    if not mutations:
        print('self-check FAILED: the shipped mutation manifest is empty', file=sys.stderr)
        return 1
    saved = list(mutations)
    out, err = io.StringIO(), io.StringIO()
    try:
        mutations.clear()
        status = check_anchors(stdout=out, stderr=err)
    finally:
        mutations[:] = saved
    if status == 0:
        print('self-check FAILED: an empty manifest passed the anchor gate', file=sys.stderr)
        return 1
    if 'empty mutation manifest' not in err.getvalue():
        print('self-check FAILED: empty manifest did not report the explicit error',
              file=sys.stderr)
        return 1
    # fix-r3 (DS r3 boundary#2, #2728): the empty test/marker branch itself
    # needs a negative control -- deleting row_error's second clause must turn
    # self-check red. The criterion binds the branch's OWN message: matching
    # only the row name could be satisfied by a stale-anchor error, passing
    # for the wrong reason (a stale first-row anchor makes this arm refuse,
    # so the branch is only verified on a table whose first anchor is live).
    first = mutations[0]
    out2 = io.StringIO()
    try:
        mutations[0] = (*first[:5], '')
        status2 = check_anchors(stdout=out2)
    finally:
        mutations[0] = first
    if status2 == 0 or 'empty test or marker binding' not in out2.getvalue():
        print('self-check FAILED: an empty marker binding passed the anchor gate',
              file=sys.stderr)
        return 1
    print(f'self-check: empty manifest fails closed (rc={status}); '
          f'empty marker binding fails closed (rc={status2}); '
          f'shipped manifest still has {len(mutations)} anchors: ok')
    return 0


def terminate_cleanly(signum, _frame):
    """Unwind on SIGTERM/SIGHUP/SIGQUIT so the finally blocks restore the
    sources; the default disposition would leave a mutant in the tree
    (mirror of same-json-surfaces' #892 r23 form)."""
    if defer_signal_exit(signum):
        return
    raise SystemExit(128 + signum)


def install_terminate_handlers():
    signal.signal(signal.SIGTERM, terminate_cleanly)
    if hasattr(signal, 'SIGHUP'):
        signal.signal(signal.SIGHUP, terminate_cleanly)
    if hasattr(signal, 'SIGQUIT'):
        signal.signal(signal.SIGQUIT, terminate_cleanly)


def main():
    args = sys.argv[1:]
    if args == ['--self-check']:
        return self_check()
    if args in (['--check'], ['--check-anchors']):
        check_roster()
        return check_anchors()
    compile_only = '--compile-only' in args
    args = [arg for arg in args if arg != '--compile-only']
    for arg in args:
        if arg.startswith('-'):
            # fix-r1 (DS boundary#1, P1): an unknown `--flag` would otherwise
            # be taken for the output directory and run the whole mutation
            # suite instead of refusing.
            refuse(f'#2140 unknown option: {arg}')
    if not args:
        # fix-r2 (DS boundary#2/simplify#4): a string SystemExit is rc=1 and
        # would read as "ran and did not come out green" (#1734).
        refuse('usage: --check | --check-anchors | --self-check | [--compile-only] '
               'NEW_OUTPUT_DIR [mutation ...]')
    check_roster()
    selected = [row for row in mutations if not args[1:] or row[0] in args[1:]]
    if not selected or (args[1:] and len(selected) != len(args[1:])):
        refuse('#2140 unknown or duplicate mutation')
    if missing_target():
        refuse('#2140 caller must supply CARGO_TARGET_DIR')
    refuse_unusable_target()
    # fix-r2 (DS concurrency#3): installed before claiming the evidence
    # directory, so a signal in that window cannot strand an empty claimed
    # directory that only a human can remove.
    install_terminate_handlers()
    out = claim_dir(args[0])
    command = ['cargo', 'test', '--offline', '--manifest-path', 'src/Cargo.toml',
               '-p', 'pilotdeck-cli', '--bin', 'pilotdeck']
    results = []

    def run(label, arguments):
        slot = wait_for_slot(root / 'scripts/cargo-slots.sh', timeout=900)
        with (out / (label + '.slots.log')).open('x') as stream:
            stream.write(slot.stdout + slot.stderr)
        result = run_owned(command + arguments, cwd=root, env=env, timeout=900)
        with (out / (label + '.log')).open('x') as stream:
            stream.write(result.stdout + result.stderr)
        return result

    # #1885: the guard set carries exactly the files a control rewrites;
    # mutating_sources restores each guarded path atomically on exit (#1903),
    # and leftovers anywhere else stay detected by the whole-src HEAD scan
    # (_assert_workspace_sources).
    guarded = sorted({row[1] for row in mutations}, key=str)
    env = dict(os.environ)
    try:
        with mutating_sources(root, guarded) as originals:
            for name, path, old, new, test, marker in selected:
                row = dict(mutation=name, test=test, assertion=marker,
                           status='INVALID', reason='not run', compiled=False)
                arguments = [test, '--', '--exact']
                try:
                    original = originals[path].decode()
                    # fix-r3: shared validator with check_anchors -- an empty
                    # marker must refuse HERE too, not only in --check mode.
                    error = row_error(name, original.count(old), old, new, test, marker)
                    if error:
                        raise ValueError(error)
                    if not compile_only and not passed(run(name + '-positive', arguments), test):
                        raise RuntimeError('positive control did not pass')
                    # #1903: a truncating write here would leave a half-file
                    # on SIGKILL; the rename publish never does.
                    write_source_atomic(root, path,
                                        original.replace(old, new, 1).encode())
                    if run(name + '-compile', ['--no-run']).returncode != 0:
                        raise RuntimeError('mutant did not compile; never a kill')
                    row['compiled'] = True
                    if compile_only:
                        row.update(status='COMPILED', reason='mutant compiles; runtime check still required')
                    else:
                        status, reason = classify(run(name + '-negative', arguments),
                                                  Case(name, path, old, new, test, marker))
                        row.update(status=status, reason=reason)
                except Exception as error:
                    row.update(status='INVALID', reason=f'{type(error).__name__}: {error}')
                finally:
                    write_source_atomic(root, path, originals[path])
                try:
                    restored = run(name + '-restored',
                                   ['--no-run'] if compile_only else arguments)
                    if restored.returncode or (not compile_only and not passed(restored, test)):
                        raise RuntimeError('restored positive did not pass')
                except Exception as error:
                    row.update(status='INVALID', reason=row['reason'] + f'; restore: {error}')
                results.append(row)
                print(f'{name}: {row["status"]}: {row["reason"]}', flush=True)
    except BaseException as error:
        reason = f'source guard: {type(error).__name__}: {error}'
        invalidate_completed(results, reason)
        for name, _path, _old, _new, test, marker in selected[len(results):]:
            results.append(dict(mutation=name, test=test, assertion=marker,
                                status='INVALID', reason=reason, compiled=False))
            print(f'{name}: INVALID: {reason}', flush=True)
        if isinstance(error, (SystemExit, KeyboardInterrupt)):
            # fix-r1 (DS boundary#5): a signal unwinding must keep its exit
            # code instead of being reported as "ran and did not come out
            # green"; the INVALID rows above still record what was abandoned.
            raise
    publish_summary(out, results,
                    states=('KILLED', 'SURVIVED', 'INVALID', 'COMPILED'))
    return verdict_exit(results, key='status',
                        killed='COMPILED' if compile_only else 'KILLED')


if __name__ == '__main__':
    raise SystemExit(main())
