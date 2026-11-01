#!/usr/bin/env python3
"""#132 HEAD-only product mutations; compile each, classify all, verify restore.

Run after the coordinator commits the patch. This script deliberately refuses
uncommitted/new sources. The caller supplies CARGO_TARGET_DIR; no alternate
repository or build directory is created. Use --check-anchors before commit.
"""
import os
from pathlib import Path
import sys

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot
from negative_suite import Case, classify, invalidate_completed, passed, publish_summary

root = Path(__file__).resolve().parents[5]
FIXTURE = Path(__file__).resolve().parent
PROJECT = root / 'src/pilotdeck-gateway/src/project_files.rs'
CATALOG = root / 'src/pilotdeck-gateway/src/model_catalog.rs'
SESSION = root / 'src/pilotdeck-gateway/src/session_model.rs'
COMPARISON = root / 'src/vendor/icu_collator/src/comparison.rs'
PREFIX = 'pairing::native_collation_tests::'
MATRIX = PREFIX + 'native_matrix_matches_node'
PAIRS = PREFIX + 'native_comparator_matches_node_all_pairs'
mutations = [
    ('code-unit-order', PROJECT,
     'collator.compare_utf16_preserving_surrogates(left.units(), right.units())',
     'left.units().cmp(right.units())', PAIRS, '#132 comparator raw weights'),
    ('replacement-fold', PROJECT,
     'collator.compare_utf16_preserving_surrogates(left.units(), right.units())',
     'collator.compare_utf16(left.units(), right.units())', MATRIX, '#132 matrix wire bytes'),
    ('pua-collision', COMPARISON,
     'CollationElement::new_implicit_from_code_point(u32::from(*surrogate))',
     'CollationElement::new_implicit_from_code_point(0xE000 + u32::from(*surrogate) - 0xD800)',
     PAIRS, '#132 comparator raw weights'),
    ('catalog-host-remains', CATALOG,
     '    items.sort_by(|a, b| node_compare_utf16(&collator, &a.0, &b.0));',
     '    if items.len() > 1 && items.iter().any(|(id, _)| crate::project_files::has_unpaired_surrogate(id)) {\n'
     '        return Err(config::Error { code: "JS_HOST_REQUIRED".into(), message: "".into(), diagnostics: vec![], name: "Error", requires_host: true });\n'
     '    }\n    items.sort_by(|a, b| node_compare_utf16(&collator, &a.0, &b.0));',
     MATRIX, '#132 matrix must terminate natively'),
    ('session-host-remains', SESSION,
     '    if entries.len() > 1 {\n        let collator',
     '    if entries.iter().any(|e| crate::project_files::has_unpaired_surrogate(field(e, "createdAt").as_string().unwrap())) {\n'
     '        let mut host = error("JS_HOST_REQUIRED", ""); host.requires_host = true; return Err(host);\n'
     '    }\n    if entries.len() > 1 {\n        let collator',
     MATRIX, '#132 matrix must terminate natively'),
    ('valid-pair-split', COMPARISON,
     'Ok(c) => length += c.len_utf16(),',
     'Ok(c) => { if c.len_utf16() == 2 { break; } length += 1; }',
     PAIRS, '#132 comparator raw weights'),
    ('french-secondary-forward', PROJECT,
     'collator.with_backward_second_level(true)',
     'collator.with_backward_second_level(false)',
     PAIRS, '#132 comparator raw weights'),
    ('identical-raw-units', COMPARISON,
     'return self.preserving_identical(left).cmp(&self.preserving_identical(right));',
     'return left.cmp(right);',
     PREFIX + 'preserving_identical_strength_keeps_normalization_and_ignorables',
     '#132 identical canonical equivalence'),
    ('identical-tie-dropped', COMPARISON,
     'if result == Ordering::Equal && self.options.strength() == Strength::Identical {',
     'if false && result == Ordering::Equal && self.options.strength() == Strength::Identical {',
     PREFIX + 'preserving_identical_strength_keeps_normalization_and_ignorables',
     '#132 identical retains primary ignorables'),
    ('french-script-fallback-lost', PROJECT,
     '&& locale.id.script.is_none()',
     '&& true', PAIRS, '#132 comparator raw weights'),
]


def main():
    if sys.argv[1:] == ['--check-anchors']:
        errors = []
        for name, path, old, new, _test, _marker in mutations:
            if path.read_text().count(old) != 1 or old == new:
                errors.append(f'{name}: expired or invalid anchor')
        print('\n'.join(errors) if errors else f'{len(mutations)} unique anchors checked')
        return int(bool(errors))
    args = [a for a in sys.argv[1:] if a != '--compile-only']
    compile_only = '--compile-only' in sys.argv[1:]
    if not args:
        raise SystemExit('usage: --check-anchors | [--compile-only] NEW_OUTPUT_DIR [mutation ...]')
    selected = [row for row in mutations if not args[1:] or row[0] in args[1:]]
    if not selected or (args[1:] and len(selected) != len(args[1:])):
        raise SystemExit('#132 unknown or duplicate mutation')
    env = dict(os.environ)
    if not env.get('CARGO_TARGET_DIR'):
        raise SystemExit('#132 caller must supply CARGO_TARGET_DIR')
    out = Path(args[0]).resolve()
    out.mkdir(parents=True, exist_ok=False)
    command = ['cargo', 'test', '--offline', '--manifest-path', 'src/Cargo.toml',
               '-p', 'pilotdeck-gateway', '--lib']
    results = []

    def run(label, arguments):
        slot = wait_for_slot(root / 'scripts/cargo-slots.sh', timeout=900)
        with (out / (label + '.slots.log')).open('x') as stream:
            stream.write(slot.stdout + slot.stderr)
        result = run_owned(command + arguments, cwd=root, env=env, timeout=900)
        with (out / (label + '.log')).open('x') as stream:
            stream.write(result.stdout + result.stderr)
        return result

    guarded = [PROJECT, CATALOG, SESSION, COMPARISON, FIXTURE / 'regression.rs',
               FIXTURE / 'oracle.json', FIXTURE / 'generate.mts', Path(__file__).resolve(),
               root / 'src/vendor/icu_collator/src/elements.rs',
               *(FIXTURE.parent / name for name in (
                   'negative_process.py', 'negative_suite.py', 'negative_panic.py', 'pending_reanchor.py'))]
    try:
        with mutating_sources(root, guarded) as originals:
            for name, path, old, new, test, marker in selected:
                row = dict(mutation=name, test=test, assertion=marker,
                           status='INVALID', reason='not run', compiled=False)
                arguments = [test, '--', '--exact']
                try:
                    original = originals[path].decode()
                    if original.count(old) != 1 or old == new:
                        raise ValueError('expired or invalid anchor')
                    if not compile_only and not passed(run(name + '-positive', arguments), test):
                        raise RuntimeError('positive control did not pass')
                    path.write_text(original.replace(old, new, 1))
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
                    path.write_bytes(originals[path])
                try:
                    restored = run(name + '-restored', ['--no-run'] if compile_only else arguments)
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
    publish_summary(out, results)
    expected = 'COMPILED' if compile_only else 'KILLED'
    return int(any(row['status'] != expected for row in results))


if __name__ == '__main__':
    raise SystemExit(main())
