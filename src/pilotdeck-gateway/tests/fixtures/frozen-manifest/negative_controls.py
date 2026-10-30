#!/usr/bin/env python3
"""#1089 Python-only source mutants. Run only after coordinator commit.

All mutated sources and manifests must equal HEAD. No Cargo invocation, shadow
repository or HEAD exemption is used. Each candidate is compiled before tests;
all mutants run to a final KILLED/SURVIVED/INVALID summary.
"""
import json
from pathlib import Path
import sys

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot

root = Path(__file__).resolve().parents[5]
mutations = [
    ('parity-list-drift', root / 'scripts/parity/check_frozen.py',
     'if generated_manifest(root, raw) != raw:', 'if False:', 'test_hash_drift_and_manual_list', '#1089 source drift rejected'),
    ('parity-list-duplicate', root / 'scripts/parity/check_frozen.py',
     'if name in entries:', 'if False:', 'test_duplicate_parser', '#1089 duplicate parser rejected'),
    ('parity-event-fields', root / 'scripts/parity/check_frozen.py',
     'if len(values) != 1 or not values[0].strip():', 'if False:', 'test_event_fields', '#1089 event required field rejected'),
    ('parity-event-required', root / 'scripts/parity/check_frozen.py',
     'if name not in changed or current != target or remaining:', 'if False:', 'test_event_required_and_bound_to_hashes', '#1089 changed file without event rejected'),
    ('parity-event-existing', root / 'scripts/parity/check_frozen.py',
     'if event in existing:', 'if False:', 'test_event_required_and_bound_to_hashes', '#1089 existing event cannot cover new change'),
    ('parity-history', root / 'scripts/parity/check_frozen.py',
     'if (bp, bs) != (ap, ass):', 'if False:', 'test_history_and_existing_events', '#1089 historical prose drift rejected'),
    ('parity-old-pin', root / 'scripts/parity/check_frozen.py',
     'if not actual.startswith(pin):', 'if False:', 'test_base_failures', '#1089 corrupt baseline pin rejected'),
    ('gateway-parity-list-drift', root / 'scripts/gateway-parity/check_frozen.py',
     'if generated_manifest(root, raw) != raw:', 'if False:', 'test_hash_drift_and_manual_list', '#1089 source drift rejected'),
    ('gateway-parity-list-duplicate', root / 'scripts/gateway-parity/check_frozen.py',
     'if name in entries:', 'if False:', 'test_duplicate_parser', '#1089 duplicate parser rejected'),
    ('gateway-parity-event-fields', root / 'scripts/gateway-parity/check_frozen.py',
     'if len(values) != 1 or not values[0].strip():', 'if False:', 'test_event_fields', '#1089 event required field rejected'),
    ('gateway-parity-event-required', root / 'scripts/gateway-parity/check_frozen.py',
     'if name not in changed or current != target or remaining:', 'if False:', 'test_event_required_and_bound_to_hashes', '#1089 changed file without event rejected'),
    ('gateway-parity-event-existing', root / 'scripts/gateway-parity/check_frozen.py',
     'if event in existing:', 'if False:', 'test_event_required_and_bound_to_hashes', '#1089 existing event cannot cover new change'),
    ('gateway-parity-history', root / 'scripts/gateway-parity/check_frozen.py',
     'if (bp, bs) != (ap, ass):', 'if False:', 'test_history_and_existing_events', '#1089 historical prose drift rejected'),
    ('gateway-parity-old-pin', root / 'scripts/gateway-parity/check_frozen.py',
     'if not actual.startswith(pin):', 'if False:', 'test_base_failures', '#1089 corrupt baseline pin rejected'),
    ('ui-server-parity-list-drift', root / 'scripts/ui-server-parity/check_frozen.py',
     'if generated_manifest(root, raw) != raw:', 'if False:', 'test_hash_drift_and_manual_list', '#1089 source drift rejected'),
    ('ui-server-parity-list-duplicate', root / 'scripts/ui-server-parity/check_frozen.py',
     'if name in entries:', 'if False:', 'test_duplicate_parser', '#1089 duplicate parser rejected'),
    ('ui-server-parity-event-fields', root / 'scripts/ui-server-parity/check_frozen.py',
     'if len(values) != 1 or not values[0].strip():', 'if False:', 'test_event_fields', '#1089 event required field rejected'),
    ('ui-server-parity-event-required', root / 'scripts/ui-server-parity/check_frozen.py',
     'if name not in changed or current != target or remaining:', 'if False:', 'test_event_required_and_bound_to_hashes', '#1089 changed file without event rejected'),
    ('ui-server-parity-event-existing', root / 'scripts/ui-server-parity/check_frozen.py',
     'if event in existing:', 'if False:', 'test_event_required_and_bound_to_hashes', '#1089 existing event cannot cover new change'),
    ('ui-server-parity-history', root / 'scripts/ui-server-parity/check_frozen.py',
     'if (bp, bs) != (ap, ass):', 'if False:', 'test_history_and_existing_events', '#1089 historical prose drift rejected'),
    ('ui-server-parity-old-pin', root / 'scripts/ui-server-parity/check_frozen.py',
     'if not actual.startswith(pin):', 'if False:', 'test_base_failures', '#1089 corrupt baseline pin rejected'),
    ('parity-writer-head', root / 'scripts/parity/check_frozen.py',
     "if head is not None and data != git(root, 'show', f'{head}:{relative}'):",
     "if False and data != git(root, 'show', f'{head}:{relative}'):",
     'test_writer_head_binding', '#1089 dirty writer source rejected'),
    ('gateway-parity-writer-head', root / 'scripts/gateway-parity/check_frozen.py',
     "if head is not None and data != git(root, 'show', f'{head}:{relative}'):",
     "if False and data != git(root, 'show', f'{head}:{relative}'):",
     'test_writer_head_binding', '#1089 dirty writer source rejected'),
    ('ui-server-parity-writer-head', root / 'scripts/ui-server-parity/check_frozen.py',
     "if head is not None and data != git(root, 'show', f'{head}:{relative}'):",
     "if False and data != git(root, 'show', f'{head}:{relative}'):",
     'test_writer_head_binding', '#1089 dirty writer source rejected'),
    ('parity-cli-abbreviation', root / 'scripts/parity/check_frozen.py',
     'allow_abbrev=False', 'allow_abbrev=True', 'test_sibling_helpers_and_cli_abbreviations',
     '#1089 CLI abbreviations must not diverge from exact bootstrap dispatch'),
    ('gateway-parity-cli-abbreviation', root / 'scripts/gateway-parity/check_frozen.py',
     'allow_abbrev=False', 'allow_abbrev=True', 'test_sibling_helpers_and_cli_abbreviations',
     '#1089 CLI abbreviations must not diverge from exact bootstrap dispatch'),
    ('ui-server-parity-cli-abbreviation', root / 'scripts/ui-server-parity/check_frozen.py',
     'allow_abbrev=False', 'allow_abbrev=True', 'test_sibling_helpers_and_cli_abbreviations',
     '#1089 CLI abbreviations must not diverge from exact bootstrap dispatch'),
    ('parity-writer-dispatch', root / 'scripts/parity/check_frozen.py',
     'write_manifest(head=head)', 'write_manifest()', 'test_sibling_helpers_and_cli_abbreviations',
     '#1089 production writer requires HEAD binding'),
    ('gateway-parity-writer-dispatch', root / 'scripts/gateway-parity/check_frozen.py',
     'write_manifest(head=head)', 'write_manifest()', 'test_sibling_helpers_and_cli_abbreviations',
     '#1089 production writer requires HEAD binding'),
    ('ui-server-parity-writer-dispatch', root / 'scripts/ui-server-parity/check_frozen.py',
     'write_manifest(head=_boot_head)', 'write_manifest()', 'test_sibling_helpers_and_cli_abbreviations',
     '#1089 production writer requires HEAD binding'),
    ('parity-row-history', root / 'scripts/parity/check_frozen.py',
     "if name in after_tails):", "if False):", 'test_row_metadata', '#1089 table prose drift rejected'),
]


def save(out, name, result):
    for suffix, value in (('stdout', result.stdout), ('stderr', result.stderr),
                          ('exit', str(result.returncode) + '\n')):
        (out / (name + '.' + suffix)).write_text(value)


def main():
    out = Path(sys.argv[1]).resolve()
    out.mkdir(parents=True, exist_ok=False)
    selected = sys.argv[2:]
    chosen = [item for item in mutations if not selected or item[0] in selected]
    if selected and set(selected) != {item[0] for item in chosen}:
        raise SystemExit('#1089 unknown mutation name')
    results = []
    for name, path, before, after, test, assertion in chosen:
        command = [sys.executable, '-I', str(Path(__file__).with_name('test_manifest.py')),
                   path.parent.name, '-v', 'Manifest.' + test]
        marker = f'{test} (__main__.Manifest.{test}) ... ok'
        status, reason, compiled = 'INVALID', 'not run', False
        try:
            guarded = [path]
            if path.parent.name == 'gateway-parity':
                guarded.append(root / 'scripts/gateway-parity/common.py')
            with mutating_sources(root, guarded) as originals:
                text = originals[path].decode()
                if text.count(before) != 1:
                    raise RuntimeError(f'#1089 stale anchor {name}: expected 1, found {text.count(before)}')
                positive = run_owned(command, cwd=root, timeout=60)
                save(out, name + '-positive', positive)
                if positive.returncode != 0 or marker not in positive.stdout + positive.stderr:
                    raise RuntimeError('#1089 bound positive did not pass')
                mutated = text.replace(before, after, 1)
                compile(mutated, str(path), 'exec')
                compiled = True
                path.write_text(mutated)
                negative = run_owned(command, cwd=root, timeout=60)
                save(out, name + '-negative', negative)
                log = negative.stdout + negative.stderr
                if negative.returncode == 0 and marker in log:
                    status, reason = 'SURVIVED', 'bound unittest still passed'
                elif (negative.returncode == 1 and f'FAIL: {test} (__main__.Manifest.{test})' in log
                      and 'AssertionError' in log and assertion in log
                      and 'FAILED (failures=' in log and 'ERROR:' not in log):
                    status, reason = 'KILLED', 'bound #1089 assertion failed'
                else:
                    reason = 'wrong failure, missing test, or setup failure'
            restored = run_owned(command, cwd=root, timeout=60)
            save(out, name + '-restored', restored)
            if restored.returncode != 0 or marker not in restored.stdout + restored.stderr:
                status, reason = 'INVALID', 'restored positive failed'
        except Exception as error:
            status, reason = 'INVALID', str(error)
        results.append(dict(name=name, status=status, reason=reason, compiled=compiled))
        print(f'{name}: {status}: {reason}', flush=True)
    counts = {key: sum(row['status'] == key for row in results) for key in ('KILLED', 'SURVIVED', 'INVALID')}
    temporary = out / 'summary.json.tmp'
    temporary.write_text(json.dumps({'controls': results, 'counts': counts}, ensure_ascii=False, indent=2) + '\n')
    temporary.replace(out / 'summary.json')
    print(json.dumps(counts), flush=True)
    return int(not results or any(row['status'] != 'KILLED' for row in results))


if __name__ == '__main__':
    raise SystemExit(main())
