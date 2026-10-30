#!/usr/bin/env python3
"""#1120 Python harness mutations, run after coordinator commits new sources.

No product source is changed. --check validates every exact anchor and Python
compilation without mutation. Real runs require HEAD equality, use the shared
restoration guard, and report every KILLED/SURVIVED/INVALID before exiting.
"""
import json
import os
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot

root = Path(__file__).resolve().parents[5]
source = root / 'scripts/smoke/edgeclaw.py'
mutants = [
    ('any-fact', source, 'all(marker.casefold() in text for marker in fact["all"])',
     'any(marker.casefold() in text for marker in fact["all"])',
     'test_multitoken_fact_requires_all_markers', '#1120 conjunctive fact'),
    ('metadata-as-memory', source, 'durable = "\\n".join([r["content"] for r in records + summary["files"]]',
     'durable = "\\n".join([observation.get("query", "")] + [r["content"] for r in records + summary["files"]]',
     'test_input_trace_and_l0_cannot_create_hits', '#1120 metadata is not durable content'),
    ('trace-as-recall', source, 'context = recall.get("systemContext")',
     'context = recall.get("trace", recall.get("systemContext"))',
     'test_input_trace_and_l0_cannot_create_hits', '#1120 trace is not injected context'),
    ('uncapped-output', source, 'body["max_tokens"] = min(cap, MAX_OUTPUT_TOKENS)',
     'body["max_tokens"] = cap',
     'test_native_payload_limits', '#1120 output cap'),
    ('budget-and', source, 'self.count >= MAX_REQUESTS or self.cell_count >= MAX_REQUESTS_PER_CELL',
     'self.count >= MAX_REQUESTS and self.cell_count >= MAX_REQUESTS_PER_CELL',
     'test_relay_budget_and_raw_failure_retention', '#1120 per-cell budget guard'),
    ('drop-raw-response', source, 'record["response_text"] = data.decode("utf-8", "replace")',
     'record["response_text"] = ""',
     'test_relay_budget_and_raw_failure_retention', '#1120 invalid model bytes retained'),
    ('partial-as-complete', source, '"planned": REPETITIONS, "observed": len(valid)',
     '"planned": REPETITIONS, "observed": REPETITIONS',
     'test_incomplete_groups_never_get_success_rate', '#1120 observed denominator'),
    ('overwrite-evidence', source, 'with path.open("x", encoding="utf-8") as out:',
     'with path.open("w", encoding="utf-8") as out:',
     'test_isolation_and_exclusive_evidence', '#1120 exclusive evidence guard'),
]


def check(original):
    for name, _path, old, new, _test, _marker in mutants:
        if original.count(old) != 1:
            raise ValueError(f'#1120 STALE ANCHOR {name}: expected 1, got {original.count(old)}')
        compile(original.replace(old, new, 1), str(source), 'exec')


def main():
    if sys.argv[1:] == ['--check']:
        check(source.read_text())
        print(f'#1120 anchors and Python compilation PASS: {len(mutants)} mutants')
        return 0
    if len(sys.argv) != 2:
        raise SystemExit('usage: negative.py --check | <exclusive-output>')
    if not os.environ.get('CARGO_TARGET_DIR'):
        raise SystemExit('#1120 caller must set CARGO_TARGET_DIR (never substituted)')
    output = Path(sys.argv[1]).resolve()
    if not output.is_relative_to(root / '_tmp'):
        raise SystemExit('#1120 output must be in this tree _tmp')
    output.mkdir(parents=True, exist_ok=False)
    results = []
    try:
        with mutating_sources(root, [source]) as originals:
            original = originals[source].decode()
            check(original)  # stale anchors abort, never become SURVIVED
            for name, _path, old, new, test, marker in mutants:
                row = dict(name=name, test=test, status='INVALID', reason='not executed', compiled=False)
                command = [sys.executable, '-I', '-B', str(Path(__file__).with_name('test_smoke.py')),
                           f'Controls.{test}']

                def run(label):
                    result = run_owned(command, cwd=root, env=dict(os.environ), timeout=60)
                    with (output / f'{name}-{label}.log').open('x') as log:
                        log.write(result.stdout + result.stderr)
                    return result

                try:
                    before = run('positive')
                    if before.returncode != 0 or 'Ran 1 test' not in before.stderr or '\nOK\n' not in before.stderr:
                        raise ValueError('#1120 named positive control failed or absent')
                    changed = original.replace(old, new, 1)
                    compile(changed, str(source), 'exec')
                    row['compiled'] = True
                    source.write_text(changed)
                    result = run('mutant')
                    log = result.stdout + result.stderr
                    if result.returncode == 0:
                        row.update(status='SURVIVED', reason='bound test still passes')
                    elif (f'FAIL: {test} ' in log and 'Ran 1 test' in log
                          and any('AssertionError:' in line and marker in line for line in log.splitlines())):
                        row.update(status='KILLED', reason='named test failed at bound #1120 assertion')
                    else:
                        row['reason'] = 'missing bound runtime failure; syntax/import/errors do not kill'
                except Exception as exc:
                    row.update(status='INVALID', reason=str(exc))
                finally:
                    source.write_text(original)
                try:
                    after = run('restored')
                    if after.returncode != 0 or '\nOK\n' not in after.stderr:
                        row.update(status='INVALID', reason='restored control did not pass')
                except Exception as exc:
                    row.update(status='INVALID', reason=f'restored: {exc}')
                results.append(row)
    except Exception as exc:
        for row in results:
            row.update(status='INVALID', reason=f'HEAD/restore guard: {exc}')
        results.append(dict(name='source-guard', status='INVALID', reason=str(exc)))
    # Publish only after the shared guard has verified restoration.
    temporary = output / 'summary.json.tmp'
    with temporary.open('x') as stream:
        json.dump(results, stream, indent=2)
    temporary.replace(output / 'summary.json')
    for row in results:
        print(f"{row['name']}: {row['status']} — {row['reason']}")
    return 0 if len(results) == len(mutants) and all(r['status'] == 'KILLED' for r in results) else 1


if __name__ == '__main__':
    raise SystemExit(main())
