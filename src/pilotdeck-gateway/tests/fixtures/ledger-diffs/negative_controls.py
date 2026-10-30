#!/usr/bin/env python3
"""#1088 source mutants; run after commit, never against an uncommitted baseline.

Python-only controls: each mutant must compile() before its bound unittest.
All results are collected, restoration is verified, then summary is published.
No Cargo build or second checkout is used. Own the tree exclusively: ordinary
checker readers must not run while negative data/source windows are active.
"""
import hashlib
import json
from pathlib import Path
import sys

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot

root = Path(__file__).resolve().parents[5]
mutations = [
    ('parity-empty-base', root / 'scripts/parity/check_frozen.py',
     'if base is not None:', 'if base:', 'test_real_cli', '#1088 real invalid base rejected'),
    ('ui-empty-base', root / 'scripts/ui-server-parity/check_frozen.py',
     'if base is not None:', 'if base:', 'test_real_cli', '#1088 real invalid base rejected'),
    ('ui-full-hash', root / 'scripts/ui-server-parity/check_frozen.py',
     'or hashlib.sha256(path.read_bytes()).hexdigest() != expected):', 'or False):',
     'test_full_file_hash', '#1088 full ledger hash must fail'),
    ('parity-numeric-history', root / 'scripts/parity/check_frozen.py',
     "ids.update(re.findall(r'^\\| ([1-9][0-9]*) \\|', old, re.M))", 'ids.update(())',
     'test_numeric_history', '#1088 valid ledger'),
    ('parity-grammar-drift', root / 'scripts/parity/check_frozen.py',
     "FIELDS = ('类别', '基线依据', 'Rust 行为', '范围', '机器判据', '批准记录')",
     "FIELDS = ('基线依据', '类别', 'Rust 行为', '范围', '机器判据', '批准记录')",
     'test_shared_grammar', '#1088 shared ledger grammar drift must be explicit'),
    ('ui-grammar-drift', root / 'scripts/ui-server-parity/check_frozen.py',
     "FIELDS = ('类别', '基线依据', 'Rust 行为', '范围', '机器判据', '批准记录')",
     "FIELDS = ('基线依据', '类别', 'Rust 行为', '范围', '机器判据', '批准记录')",
     'test_shared_grammar', '#1088 shared ledger grammar drift must be explicit'),
    ('parity-fields', root / 'scripts/parity/check_frozen.py',
     'if not sections.get(field):', 'if False:', 'test_fields', '#1088 invalid ledger must report'),
    ('parity-original', root / 'scripts/parity/check_frozen.py',
     'if original != match[1] or original not in ids:', 'if False:', 'test_corrections', '#1088 invalid ledger must report'),
    ('parity-history', root / 'scripts/parity/check_frozen.py',
     'if before != after or path.is_symlink():', 'if False:', 'test_append_only_bytes', '#1088 old ledger byte drift rejected'),
    ('parity-hash-row', root / 'scripts/parity/check_frozen.py',
     'if len(before_rows) != 1 or (before_rows != after_rows and not migration):',
     'if False:', 'test_hash_row', '#1088 legacy hash row byte drift rejected'),
    ('parity-entry-bytes', root / 'scripts/parity/check_frozen.py',
     "or path.read_bytes() != git(root, 'cat-file', 'blob', oid.decode(encoding='utf-8'))):",
     "or False):", 'test_existing_files', '#1088 changed entry rejected'),
    ('parity-full-hash', root / 'scripts/parity/check_frozen.py',
     'ok = digest.startswith(expected)', 'ok = True', 'test_full_file_hash', '#1088 full ledger hash must fail'),
    ('ui-fields', root / 'scripts/ui-server-parity/check_frozen.py',
     'if not sections.get(field):', 'if False:', 'test_fields', '#1088 invalid ledger must report'),
    ('ui-original', root / 'scripts/ui-server-parity/check_frozen.py',
     'if original != match[1] or original not in ids:', 'if False:', 'test_corrections', '#1088 invalid ledger must report'),
    ('ui-history', root / 'scripts/ui-server-parity/check_frozen.py',
     'if before != after or path.is_symlink():', 'if False:', 'test_append_only_bytes', '#1088 old ledger byte drift rejected'),
    ('ui-hash-row', root / 'scripts/ui-server-parity/check_frozen.py',
     'if len(before_rows) != 1 or before_rows != after_rows:', 'if False:', 'test_hash_row', '#1088 legacy hash row byte drift rejected'),
    ('ui-entry-bytes', root / 'scripts/ui-server-parity/check_frozen.py',
     "or path.read_bytes() != git(root, 'cat-file', 'blob', oid.decode(encoding='utf-8'))):",
     'or False):', 'test_existing_files', '#1088 changed entry rejected'),
]


def save(out, name, result):
    (out / (name + '.stdout')).write_text(result.stdout)
    (out / (name + '.stderr')).write_text(result.stderr)
    (out / (name + '.exit')).write_text(str(result.returncode) + '\n')


def main():
    out = Path(sys.argv[1]).resolve()
    out.mkdir(parents=True, exist_ok=False)
    selected = sys.argv[2:]
    chosen = [m for m in mutations if not selected or m[0] in selected]
    if selected and set(selected) != {m[0] for m in chosen}:
        raise SystemExit('#1088 unknown mutation name')
    results = []
    for name, path, old, new, test, assertion in chosen:
        kind = path.parent.name
        manifest = root / f'specs/{kind}/FROZEN.md'
        command = [sys.executable, '-I', str(Path(__file__).with_name('test_ledger.py')),
                   kind, '-v', 'Ledger.' + test]
        status, reason, compiled = 'INVALID', 'not run', False
        try:
            # Both checker and its self-hash must equal HEAD before mutation.
            with mutating_sources(root, [path, manifest]) as originals:
                text = originals[path].decode()
                if text.count(old) != 1:
                    raise RuntimeError(f'#1088 stale anchor {name}: expected 1, found {text.count(old)}')
                positive = run_owned(command, cwd=root, timeout=60)
                save(out, name + '-positive', positive)
                marker = f'{test} (__main__.Ledger.{test}) ... ok'
                if positive.returncode != 0 or marker not in positive.stdout + positive.stderr:
                    raise RuntimeError('#1088 bound positive did not pass')
                mutated = text.replace(old, new, 1)
                compile(mutated, str(path), 'exec')
                compiled = True
                path.write_text(mutated)
                digest = hashlib.sha256(path.read_bytes()).hexdigest()
                raw = originals[manifest].decode()
                original_hash = hashlib.sha256(originals[path]).hexdigest()
                if raw.count(original_hash) != 1:
                    raise RuntimeError('#1088 checker self-hash anchor must occur once')
                changed = raw.replace(original_hash, digest, 1)
                manifest.write_text(changed)
                negative = run_owned(command, cwd=root, timeout=60)
                save(out, name + '-negative', negative)
                log = negative.stdout + negative.stderr
                if negative.returncode == 0 and marker in log:
                    status, reason = 'SURVIVED', 'bound unittest still passed'
                elif (negative.returncode == 1 and f'FAIL: {test} (__main__.Ledger.{test})' in log
                      and 'AssertionError' in log and assertion in log and 'FAILED (failures=' in log
                      and 'ERROR:' not in log):
                    status, reason = 'KILLED', 'bound unittest failed at its #1088 assertion'
                else:
                    status, reason = 'INVALID', 'wrong failure, missing test, or setup/bootstrap failure'
            restored = run_owned(command, cwd=root, timeout=60)
            save(out, name + '-restored', restored)
            if restored.returncode != 0 or marker not in restored.stdout + restored.stderr:
                status, reason = 'INVALID', 'restored bound unittest did not pass'
        except Exception as error:
            status, reason = 'INVALID', str(error)
        results.append(dict(mutation=name, status=status, reason=reason, compiled=compiled))
        print(f'{name}: {status}: {reason}', flush=True)
    counts = {key: sum(row['status'] == key for row in results) for key in ('KILLED', 'SURVIVED', 'INVALID')}
    target = out / 'summary.json'
    temporary = out / 'summary.json.tmp'
    temporary.write_text(json.dumps({'controls': results, 'counts': counts}, ensure_ascii=False, indent=2) + '\n')
    temporary.replace(target)
    print(json.dumps(counts), flush=True)
    return int(any(row['status'] != 'KILLED' for row in results) or not results)


if __name__ == '__main__':
    raise SystemExit(main())
