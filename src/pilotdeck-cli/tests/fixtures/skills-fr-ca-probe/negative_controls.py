#!/usr/bin/env python3
"""#2140 HEAD-only CLI skills-collator controls: --check-anchors |
[--compile-only] NEW_OUTPUT_DIR [mutation ...].

Run after the coordinator commits the patch. This script deliberately refuses
uncommitted/new sources (mutating_sources proves every byte against HEAD), so
the full KILLED/SURVIVED verdicts are produced on the host; --check-anchors
works pre-commit. The caller supplies CARGO_TARGET_DIR; no alternate
repository or build directory is created.
"""
import os
from pathlib import Path
import sys

sys.dont_write_bytecode = True
root = Path(__file__).resolve().parents[5]
sys.path.insert(0, str(root / 'src/pilotdeck-gateway/tests/fixtures'))
from negative_process import mutating_sources, run_owned, wait_for_slot
from negative_suite import Case, classify, invalidate_completed, passed, publish_summary

SKILLS = root / 'src/pilotdeck-cli/src/skills.rs'
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
]


def check_anchors():
    """Exit non-zero on any expired anchor; an empty manifest never passes."""
    errors = []
    for name, path, old, new, _test, _marker in mutations:
        count = path.read_text().count(old)
        if count != 1 or old == new:
            errors.append(f'{name}: stale/invalid anchor: expected=1 actual={count}')
    for error in errors:
        print(error)
    if not mutations:
        print('#2140 anchors: empty mutation manifest; refusing to pass', file=sys.stderr)
        return 2
    print(f'#2140 anchors: {len(mutations) - len(errors)}/{len(mutations)} valid')
    return int(bool(errors))


def main():
    if sys.argv[1:] == ['--check-anchors']:
        return check_anchors()
    args = [arg for arg in sys.argv[1:] if arg != '--compile-only']
    compile_only = '--compile-only' in sys.argv[1:]
    if not args:
        raise SystemExit('usage: --check-anchors | [--compile-only] '
                         'NEW_OUTPUT_DIR [mutation ...]')
    selected = [row for row in mutations if not args[1:] or row[0] in args[1:]]
    if not selected or (args[1:] and len(selected) != len(args[1:])):
        raise SystemExit('#2140 unknown or duplicate mutation')
    env = dict(os.environ)
    if not env.get('CARGO_TARGET_DIR'):
        raise SystemExit('#2140 caller must supply CARGO_TARGET_DIR')
    out = Path(args[0]).resolve()
    out.mkdir(parents=True, exist_ok=False)
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

    shared = root / 'src/pilotdeck-gateway/tests/fixtures'
    guarded = [SKILLS, Path(__file__).resolve(),
               shared / 'negative_process.py', shared / 'negative_suite.py',
               shared / 'negative_panic.py']
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
    publish_summary(out, results)
    expected = 'COMPILED' if compile_only else 'KILLED'
    return int(any(row['status'] != expected for row in results))


if __name__ == '__main__':
    raise SystemExit(main())
