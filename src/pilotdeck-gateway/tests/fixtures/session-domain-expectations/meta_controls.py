#!/usr/bin/env python3
# anchor-selfcheck: unsupported pure in-memory drift scenarios; no source replacements
"""#1190 defensive quality hardening: exercise drift gates entirely in memory.

No fixture files, alternate directories, temporary directories, source writes
or cleanup. Each corruption binds a named check and its specific diagnostic;
exceptions or unrelated diagnostics are INVALID. Run every selected scenario
before summarizing. JSON goes to stdout; the caller owns evidence redirection.
--self-test also proves a bypassed gate fails the bound assertion, and compares
read-only real frozen/ bytes before and after all scenarios.
"""
import argparse
import copy
from dataclasses import dataclass
import json
from pathlib import Path
import sys
from unittest.mock import patch

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parent))
import negative_controls as gate


@dataclass
class Inputs:
    expectations: dict
    calibration: dict
    require_rows: list
    bound: dict
    unaffected: dict
    documents: dict
    frozen: dict

    def counts(self):
        return gate.derived_counts(self.expectations, self.calibration, self.require_rows)

    def errors(self):
        return (gate.check_bindings(self.expectations, self.require_rows,
                                    self.bound, self.unaffected)
                + gate.check_docs(self.counts(), self.documents, self.frozen)
                + gate.check_scan_set(self.frozen))


def load_inputs(raw):
    return Inputs(gate.panel_table('EXPECTATION_CASES'), gate.panel_table('CALIBRATION_CASES'),
                  copy.deepcopy(gate.MUTATIONS), copy.deepcopy(gate.BOUND),
                  copy.deepcopy(gate.UNAFFECTED), gate.read_documents(), gate.decode_frozen(raw))


STATE = 'drop-registry-state-require'
FIXTURE = '20261101-1190-r99.md'
RENAME = '20261101-1190r2.md'
SCENARIOS = (
    'delete-state-mutation-row', 'empty-state-bound', 'malformed-state-bound',
    'non-sequence-state-bound', 'malformed-state-mutation-row',
    'duplicate-state-mutation-row', 'readme-stale-count', 'readme-stale-count-ge',
    'unscanned-freeze-event', 'delete-known-freeze-event', 'rename-known-freeze-event',
    'shrink-scan-set',
)


def check_scenario(name, baseline):
    """Assert one corruption is rejected; a weakened gate fails this exact check."""
    data = copy.deepcopy(baseline)
    original_errors = baseline.errors()
    if original_errors:
        raise ValueError(f'#1190 {name}: positive gate failed: {original_errors}')
    row = next(row for row in data.require_rows if row[0] == STATE)
    if name == 'delete-state-mutation-row':
        data.require_rows.remove(row)
        marker = f'BOUND names unknown mutation {STATE}'
    elif name == 'empty-state-bound':
        data.bound[STATE] = ()
        marker = f'BOUND[{STATE}] is empty'
    elif name == 'malformed-state-bound':
        data.bound[STATE] = (('expectations-rust-registry-state',),)
        marker = (f'{STATE}: BOUND entry is not a (case, rule) pair of nonempty '
                  "strings: ('expectations-rust-registry-state',)")
    elif name == 'non-sequence-state-bound':
        data.bound[STATE] = 42
        marker = f'BOUND[{STATE}] is not a sequence of (case, rule) pairs: 42'
    elif name == 'malformed-state-mutation-row':
        data.require_rows[data.require_rows.index(row)] = row[:-1]
        marker = ('MUTATIONS row is not a (name, source, old, new) row with a '
                  f'nonempty name: {row[:-1]!r}')
    elif name == 'duplicate-state-mutation-row':
        data.require_rows.append(row)
        marker = f'MUTATIONS names are not unique: {STATE}'
    elif name in ('readme-stale-count', 'readme-stale-count-ge', 'unscanned-freeze-event'):
        count = baseline.counts()['mutationRows']
        phrase = '七条' if name != 'readme-stale-count-ge' else '7 个'
        # The historical counterexample is seven; if the table grows to seven,
        # refuse the stale test instead of mistaking a valid phrase for a kill.
        if count == 7:
            raise ValueError(f'#1190 {name}: stale-count fixture is no longer stale')
        label = FIXTURE if name == 'unscanned-freeze-event' else 'README'
        documents = data.frozen if label == FIXTURE else data.documents
        documents[label] = f'本夹具钉死了{phrase} canonical require。\n'
        marker = (f"{label}:1: '{phrase}' pins 7 but derived mutationRows is {count} "
                  '(runtime-derived counts, recorded to derived-counts.json)')
    elif name in ('delete-known-freeze-event', 'shrink-scan-set'):
        target = gate.KNOWN_EVENTS[0]
        if name == 'delete-known-freeze-event':
            del data.frozen[target]
        else:
            data.frozen.clear()
        marker = f'event file {target} is missing from the doc scan set'
    elif name == 'rename-known-freeze-event':
        data.frozen[RENAME] = data.frozen.pop(gate.KNOWN_EVENTS[1])
        marker = f'frozen event {RENAME} matches no doc scan glob'
    else:
        raise ValueError(f'#1190 unknown scenario: {name}')
    errors = data.errors()
    message = f'#1190 {name}: expected drift: {marker}'
    if marker not in errors:
        if errors:
            raise ValueError(f'{message}; unrelated errors: {errors}')
        raise AssertionError(message)
    if baseline.errors() or data is baseline:
        raise ValueError(f'#1190 {name}: baseline changed')
    return marker


def run_scenarios(baseline, selected):
    results = []
    for name in selected:
        row = {'mutation': name, 'test': f'meta_controls.check_scenario[{name}]',
               'assertion': f'#1190 {name}: expected drift:', 'status': 'INVALID'}
        try:
            row['diagnostic'] = check_scenario(name, baseline)
            row.update(status='KILLED', reason='bound drift diagnostic; unchanged baseline passes')
        except AssertionError as error:
            row.update(status='SURVIVED' if str(error).startswith(row['assertion']) else 'INVALID',
                       reason=str(error))
        except Exception as error:
            row['reason'] = f'{type(error).__name__}: {error}'
        results.append(row)
    return results


def self_test(baseline):
    """Every bound check must fail if its gate is replaced by an always-pass gate."""
    results = []
    for name in SCENARIOS:
        target = ('check_bindings' if name in SCENARIOS[:6] else
                  'check_docs' if name in SCENARIOS[6:9] else 'check_scan_set')
        row = {'mutation': f'bypass-{name}', 'test': f'meta_controls.check_scenario[{name}]',
               'assertion': f'#1190 {name}: expected drift:', 'status': 'INVALID'}
        try:
            with patch.object(gate, target, return_value=[]):
                try:
                    check_scenario(name, baseline)
                except AssertionError as error:
                    if not str(error).startswith(row['assertion']):
                        raise ValueError(f'wrong assertion: {error}') from error
                    row.update(status='KILLED', reason=f'AssertionError: {error}')
                else:
                    row.update(status='SURVIVED', reason='bypassed gate passed its bound assertion')
            check_scenario(name, baseline)
        except Exception as error:
            row.update(status='INVALID', reason=f'{type(error).__name__}: {error}')
        results.append(row)
    return results


def main():
    parser = argparse.ArgumentParser(description=__doc__, allow_abbrev=False)
    parser.add_argument('--only', choices=SCENARIOS, action='append')
    parser.add_argument('--self-test', action='store_true')
    args = parser.parse_args()
    if args.only and (args.self_test or len(set(args.only)) != len(args.only)):
        parser.error('--only cannot repeat or combine with --self-test')
    results = []
    try:
        before = gate.read_frozen()
        baseline = load_inputs(before)
        results = run_scenarios(baseline, args.only or SCENARIOS)
        if args.self_test:
            results.extend(self_test(baseline))
        if gate.read_frozen() != before:
            raise ValueError('#1190 real-frozen-unchanged: real frozen/ bytes changed')
    except Exception as error:
        for row in results:
            row.update(status='INVALID', reason=f'input verification: {type(error).__name__}: {error}')
        results.append({'mutation': 'real-frozen-unchanged', 'status': 'INVALID',
                        'reason': f'{type(error).__name__}: {error}'})
    counts = {state: sum(row['status'] == state for row in results)
              for state in ('KILLED', 'SURVIVED', 'INVALID')}
    print(json.dumps({'controls': results, 'counts': counts}, indent=2, ensure_ascii=False))
    return 0 if results and counts['KILLED'] == len(results) else 1


if __name__ == '__main__':
    raise SystemExit(main())
