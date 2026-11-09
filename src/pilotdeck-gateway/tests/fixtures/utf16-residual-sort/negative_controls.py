#!/usr/bin/env python3
"""#2131. Run after committing; OUT [NAME ...].

The five reachable JsString sort sites keep ICU4C's implicit weights for
isolated UTF-16 surrogates through `compare_utf16_preserving_surrogates`
instead of the folding `compare_utf16`. Each mutant reinstates the folded
comparator at exactly one site and must compile, fail that site's bound
replay assertion (the real TS gold cannot be reproduced by a folded
U+FFFD order), and pass again after restoration. All selected outcomes are
reported; --check-anchors is read-only.
"""
import os
from pathlib import Path
import subprocess
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import (CARGO_SLOTS_WAIT_SECONDS, mutating_sources,
                               run_owned, tree_lock, wait_for_slot)
from negative_suite import Case, classify, passed, publish_summary

root = Path(__file__).resolve().parents[5]
REGISTRY = root / 'src/pilotdeck-gateway/src/runtime/tools/registry.rs'
CONTRIBUTIONS = root / 'src/pilotdeck-gateway/src/runtime/plugins/contributions.rs'
CACHE = root / 'src/pilotdeck-gateway/src/runtime/context/cache.rs'
STATS = root / 'src/pilotdeck-gateway/src/runtime/model/stats.rs'
PREFIX = 'runtime::plugins::utf16_residual_sort_tests::'
cases = [
    # ToolRegistry.listAll's tool sort (registry.rs, ToolRegistry.ts:118).
    ('registry-list-folded', REGISTRY,
     'collator.compare_utf16_preserving_surrogates(\n'
     '                name(a.as_ref()).units(),\n'
     '                name(b.as_ref()).units(),\n'
     '            )',
     'collator.compare_utf16(name(a.as_ref()).units(), name(b.as_ref()).units())',
     PREFIX + 'registry_list_all_order_matches_ts', '#2131 listAll order'),
    # ToolRegistry.listUnavailableEntries' diagnostic sort (registry.rs,
    # ToolRegistry.ts:179).
    ('registry-unavailable-folded', REGISTRY,
     'collator.compare_utf16_preserving_surrogates(\n'
     '                a.diagnostic.name.units(),\n'
     '                b.diagnostic.name.units(),\n'
     '            )',
     'collator.compare_utf16(a.diagnostic.name.units(), b.diagnostic.name.units())',
     PREFIX + 'registry_unavailable_entries_order_matches_ts',
     '#2131 listUnavailableEntries order'),
    # getAllMcpInstructions' server-name sort (contributions.rs,
    # PluginRuntime.ts:697).
    ('mcp-instructions-folded', CONTRIBUTIONS,
     'compare_utf16_preserving_surrogates(a.server_name.units(), b.server_name.units())',
     'compare_utf16(a.server_name.units(), b.server_name.units())',
     PREFIX + 'mcp_instructions_order_matches_ts', '#2131 getAllMcpInstructions order'),
    # buildCachePlan's tool sort (cache.rs, CachePlan.ts:44-46): the
    # fingerprint hashes the sorted tools, so a folded order changes it.
    ('cache-tools-folded', CACHE,
     'let order =\n'
     '                collator.compare_utf16_preserving_surrogates(left_name.units(), &right_name);',
     'let order = collator.compare_utf16(left_name.units(), &right_name);',
     PREFIX + 'cache_plan_fingerprint_matches_ts', '#2131 buildCachePlan fingerprint'),
    # modelUsageSnapshot's provider/model sort (stats.rs,
    # TokenStatsCollector.ts:209). The mixed case keeps distinct buckets, so
    # the folded comparator reorders it.
    ('model-usage-folded', STATS,
     'collator.compare_utf16_preserving_surrogates(left.units(), right.units())',
     'collator.compare_utf16(left.units(), right.units())',
     PREFIX + 'model_usage_provider_order_matches_ts', '#2131 modelUsageSnapshot order'),
]


def main():
    if sys.argv[1:] == ['--check-anchors']:
        failures = []
        # Hold the tree lock while reading: a sibling mutating runner may
        # have a mutant on disk between its write and restore phases, which
        # would make this read-only check see `count == 0` spuriously.
        with tree_lock(root):
            for name, path, old, _, _, _ in cases:
                count = path.read_text().count(old)
                if count != 1:
                    failures.append(f'{name}: expected 1 anchor, got {count}')
        print('\n'.join(failures) if failures else f'{len(cases)} anchors OK')
        return int(bool(failures))
    args = [argument for argument in sys.argv[1:] if argument != '--compile-only']
    compile_only = '--compile-only' in sys.argv[1:]
    if not args:
        raise SystemExit('usage: --check-anchors | [--compile-only] NEW_OUTPUT_DIR [mutation ...]')
    selected = [row for row in cases if not args[1:] or row[0] in args[1:]]
    if not selected or (args[1:] and len(selected) != len(args[1:])):
        raise SystemExit('#2131 unknown or duplicate mutation')
    env = os.environ.copy()
    if not env.get('CARGO_TARGET_DIR'):
        raise SystemExit('#2131 caller must supply CARGO_TARGET_DIR')
    out = Path(args[0]).resolve()
    out.mkdir(parents=True, exist_ok=False)
    command = ['cargo', 'test', '--offline', '--manifest-path', 'src/Cargo.toml',
               '-p', 'pilotdeck-gateway', '--lib']
    results = []

    def run(label, arguments, test, compile_only=False):
        try:
            slot = wait_for_slot(root / 'scripts/cargo-slots.sh', timeout=CARGO_SLOTS_WAIT_SECONDS)
            (out / f'{label}.slots.log').write_text(slot.stdout + slot.stderr)
            # No `--nocapture` and no `-q`: the classifier needs the named
            # per-test FAILED line and its panic payload intact (#938 r14).
            suffix = ['--no-run'] if compile_only else arguments
            result = run_owned(command + suffix, cwd=root, env=env, timeout=900)
            (out / f'{label}.log').write_text(result.stdout + result.stderr)
            return result
        except Exception as error:
            raw = ''
            if isinstance(error, subprocess.TimeoutExpired):
                for item in (error.stdout, error.stderr):
                    raw += item.decode(errors='replace') if isinstance(item, bytes) else item or ''
            with (out / f'{label}.log').open('a') as log:
                log.write(raw + f'\n{type(error).__name__}: {error}\n')
            raise

    fixture = Path(__file__).resolve().parent
    shared = fixture.parent
    guarded = [REGISTRY, CONTRIBUTIONS, CACHE, STATS,
               root / 'src/pilotdeck-gateway/src/runtime/plugins.rs',
               root / 'src/pilotdeck-gateway/tests/generator_provenance.rs',
               fixture / 'regression.rs', fixture / 'oracle.json',
               fixture / 'generate.mts', fixture / 'README.md', Path(__file__),
               *(shared / name for name in (
                   'negative_process.py', 'negative_suite.py', 'negative_panic.py',
                   'pending_reanchor.py'))]
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
                    if not compile_only and not passed(run(name + '-positive', arguments, test), test):
                        raise RuntimeError('positive control did not pass')
                    path.write_text(original.replace(old, new, 1))
                    if run(name + '-compile', arguments, test, compile_only=True).returncode != 0:
                        raise RuntimeError('mutant did not compile; never a kill')
                    row['compiled'] = True
                    if compile_only:
                        row.update(status='COMPILED', reason='mutant compiles; runtime check still required')
                    else:
                        status, reason = classify(run(name + '-negative', arguments, test),
                                                  Case(name, path, old, new, test, marker))
                        row.update(status=status, reason=reason)
                except Exception as error:
                    row.update(status='INVALID', reason=f'{type(error).__name__}: {error}')
                finally:
                    path.write_bytes(originals[path])
                try:
                    restored = run(name + '-restored', ['--no-run'] if compile_only else arguments, test)
                    if restored.returncode or (not compile_only and not passed(restored, test)):
                        raise RuntimeError('restored positive did not pass')
                except Exception as error:
                    row.update(status='INVALID', reason=row['reason'] + f'; restore: {error}')
                results.append(row)
                print(f'{name}: {row["status"]}: {row["reason"]}', flush=True)
    except BaseException as error:
        reason = f'source guard: {type(error).__name__}: {error}'
        for row in results:
            row.update(status='INVALID', reason=row['reason'] + '; ' + reason)
            row['compiled'] = False
        for name, _path, _old, _new, test, marker in selected[len(results):]:
            results.append(dict(mutation=name, test=test, assertion=marker,
                                status='INVALID', reason=reason, compiled=False))
            print(f'{name}: INVALID: {reason}', flush=True)
    publish_summary(out, results)
    expected = 'COMPILED' if compile_only else 'KILLED'
    return int(any(row['status'] != expected for row in results))


if __name__ == '__main__':
    raise SystemExit(main())
