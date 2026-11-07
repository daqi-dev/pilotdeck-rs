#!/usr/bin/env python3
# anchor-selfcheck: unsupported #1540 meta-controls compile in-memory verdict mutants, no source writes
"""Remove each #1540 protection, compile, run the bound unittest, then restore.

No git worktree, product mutation, cargo target or HEAD-guard bypass is used:
every mutant is an in-memory copy of the shared verdict helper compiled from
its untouched file, so this control never writes the source tree. Each variant
first proves the bound test passes on the pristine module, then that removing
exactly one protection turns that same test red at its dedicated #1540 line,
then that the pristine module passes again; only the middle run may fail, and
only at the named assertion. Stale anchors and unrelated exceptions are
INVALID, never SURVIVED or KILLED; every variant runs even if an earlier one
is INVALID, and the script exits non-zero unless all of them are KILLED.
"""
from contextlib import redirect_stderr, redirect_stdout
import io
import json
from pathlib import Path
import sys
import tempfile
from types import ModuleType
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parent))
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_selection import refuse
import non_head
import test_non_head as tests


# (name, unique old text, replacement, bound class, bound test, exact message)
VARIANTS = (
    ('exit-constant', 'NON_HEAD_EXIT = 42', 'NON_HEAD_EXIT = 0',
     'VerdictTests', 'test_non_head_green_marks_and_exits_42',
     '#1540 a green non-HEAD run exits the dedicated code 42'),
    ('head-passthrough', "if baseline_source == 'HEAD':\n        return 0",
     "if baseline_source == 'HEAD':\n        return 42",
     'VerdictTests', 'test_head_green_exits_zero_without_side_effects',
     '#1540 a HEAD green run still exits 0'),
    ('failure-passthrough', 'if code != 0:\n        return code',
     'if False:\n        return code',
     'VerdictTests', 'test_failure_keeps_its_code',
     '#1540 a failing run keeps its own exit code'),
    ('marker-print',
     "print(f'NON-HOST EVIDENCE (非宿主证据): baseline {detail}; '\n"
     "          f'the host gate only trusts HEAD evidence; exit {NON_HEAD_EXIT}',\n"
     "          flush=True)",
     'pass',
     'VerdictTests', 'test_non_head_green_marks_and_exits_42',
     '#1540 the stdout marker names the baseline and the code'),
    ('marker-exit-code',
     "f'the host gate only trusts HEAD evidence; exit {NON_HEAD_EXIT}'",
     "'the host gate only trusts HEAD evidence; exit 0'",
     'VerdictTests', 'test_non_head_green_marks_and_exits_42',
     '#1540 the stdout marker names the baseline and the code'),
    ('provenance-gate', 'if evidence is not None:', 'if False:',
     'VerdictTests', 'test_non_head_green_marks_and_exits_42',
     '#1540 the provenance record is published for the host gate'),
    ('provenance-rename', "temporary.replace(Path(evidence) / 'baseline.json')",
     'pass',
     'VerdictTests', 'test_non_head_green_marks_and_exits_42',
     '#1540 the provenance record is published for the host gate'),
    ('provenance-direct-write', "temporary = Path(evidence) / 'baseline.json.tmp'",
     "temporary = Path(evidence) / 'baseline.json'",
     'VerdictTests', 'test_non_head_green_marks_and_exits_42',
     '#1540 the provenance record is published through a temp file'),
    ('provenance-fields', "'hostEvidence': False,", "'hostEvidence': True,",
     'VerdictTests', 'test_non_head_green_marks_and_exits_42',
     '#1540 baseline.json records the non-host provenance'),
    ('ignored-detail', "'ignores (its guard always requires HEAD); the run cannot '",
     "'requires nothing at all); the run cannot '",
     'VerdictTests', 'test_ignored_message_names_the_ignore',
     '#1540 the ignored marker says the run cannot certify the request'),
    ('gate-verify', "'rev-parse', '--verify', override + '^{commit}'],",
     "'rev-parse', override + '^{commit}'],",
     'GateTests', 'test_override_resolves_and_announces',
     '#1540 the override is resolved inside the repo, once'),
    ('gate-announce',
     "print(f'NON-HEAD BASELINE: {sha} via PDRS_NEGATIVE_BASELINE; '\n"
     "              'the host gate only trusts HEAD evidence', flush=True)",
     'pass',
     'GateTests', 'test_override_resolves_and_announces',
     '#1540 a non-HEAD run announces itself before any mutant runs'),
    ('gate-ignored-announce',
     "print(f'NON-HEAD BASELINE REQUEST: {sha} via PDRS_NEGATIVE_BASELINE; '\n"
     "              'this suite ignores the override and always guards HEAD; '\n"
     "              'the host gate only trusts HEAD evidence', flush=True)",
     'pass',
     'GateTests', 'test_ignored_override_announces_the_ignore',
     '#1540 the ignored flavor must say the guard stayed on HEAD'),
    ('gate-refusal', 'if resolved.returncode != 0:', 'if False:',
     'GateTests', 'test_unresolvable_override_refuses',
     '#1540 a garbage override never starts'),
    ('empty-override', 'if not override:', 'if override is None:',
     'GateTests', 'test_empty_override_means_head',
     '#1540 an empty override is unset, not a revision'),
)


class BoundResult(unittest.TextTestResult):
    def __init__(self, *args, **kwargs):
        super().__init__(*args, **kwargs)
        self.assertions = []

    def addFailure(self, test, error):
        super().addFailure(test, error)
        self.assertions.append((test.id(), error))


def run_bound(class_name, method, module, output, label):
    prior = tests.non_head
    tests.non_head = module
    stream = io.StringIO()
    try:
        with redirect_stdout(stream), redirect_stderr(stream):
            result = unittest.TextTestRunner(
                stream=stream, verbosity=2, resultclass=BoundResult).run(
                unittest.TestSuite([getattr(tests, class_name)(method)]))
    finally:
        tests.non_head = prior
    with (output / (label + '.log')).open('x') as log:
        log.write(stream.getvalue())
    return result


def controls(output, selected=None):
    output.mkdir(parents=True, exist_ok=False)
    source = Path(non_head.__file__).read_text(encoding='utf-8')
    chosen = [item for item in VARIANTS if selected is None or item[0] in selected]
    if not chosen or (selected is not None and set(selected) - {item[0] for item in chosen}):
        refuse('empty/unknown variant selection: '
               + ', '.join(item[0] for item in VARIANTS))
    rows = []
    for name, before, after, class_name, method, marker in chosen:
        identity = f'test_non_head.{class_name}.{method}'
        row = dict(mutation=name, test=identity, assertion=marker,
                  status='INVALID', reason='not run', hits=[])
        module_name = '_negkit_non_head_mutant'
        try:
            if source.count(before) != 1:
                raise ValueError(f'stale anchor: expected 1, got {source.count(before)}: {before!r}')
            code = compile(source.replace(before, after, 1),
                           str(Path(non_head.__file__)), 'exec')
            module = ModuleType(module_name)
            module.__file__ = non_head.__file__
            sys.modules[module_name] = module
            exec(code, module.__dict__)
            positive = run_bound(class_name, method, non_head, output, name + '-positive')
            negative = run_bound(class_name, method, module, output, name + '-negative')
            restored = run_bound(class_name, method, non_head, output, name + '-restored')
            healthy = positive.wasSuccessful() and restored.wasSuccessful()
            bound = (negative.testsRun == 1 and not negative.errors
                     and len(negative.assertions) == 1
                     and negative.assertions[0][0] == identity
                     and issubclass(negative.assertions[0][1][0], AssertionError)
                     and str(negative.assertions[0][1][1]).endswith(' : ' + marker))
            if healthy and bound:
                row.update(status='KILLED',
                           reason='compiled; exact bound assertion failed; restored passed',
                           hits=[identity])
            elif healthy and negative.wasSuccessful():
                row.update(status='SURVIVED', reason='bound test still passed')
            else:
                row['reason'] = 'positive/restored failure or unrelated mutant failure; see raw logs'
        except Exception as error:
            row['reason'] = f'{type(error).__name__}: {error}'
        finally:
            sys.modules.pop(module_name, None)
        rows.append(row)
        print(f'{name}: {row["status"]}: {row["reason"]}', flush=True)
    document = {'mode': 'full' if len(chosen) == len(VARIANTS) else 'sample',
                'selected': [row['mutation'] for row in rows],
                'total': len(VARIANTS), 'controls': rows}
    temporary = output / 'summary.json.tmp'
    temporary.write_text(json.dumps(document, ensure_ascii=False, indent=2) + '\n',
                         encoding='utf-8')
    temporary.replace(output / 'summary.json')
    print('summary: ' + ', '.join(f'{status}={sum(row["status"] == status for row in rows)}'
                                  for status in ('KILLED', 'SURVIVED', 'INVALID')))
    return int(any(row['status'] != 'KILLED' for row in rows))


def main():
    import argparse
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('output', nargs='?', type=Path)
    parser.add_argument('variants', nargs='*')
    args = parser.parse_args()
    if args.output is not None:
        return controls(args.output, args.variants or None)
    with tempfile.TemporaryDirectory(prefix='negkit-non-head-controls-') as directory:
        return controls(Path(directory) / 'evidence')


if __name__ == '__main__':
    raise SystemExit(main())
