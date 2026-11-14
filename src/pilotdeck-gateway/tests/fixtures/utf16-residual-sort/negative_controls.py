#!/usr/bin/env python3
"""#2131. Run after committing; OUT [NAME ...].

The five reachable JsString sort sites keep ICU4C's implicit weights for
isolated UTF-16 surrogates through `compare_utf16_preserving_surrogates`
instead of the folding `compare_utf16`. Each mutant reinstates the folded
comparator at exactly one site and must compile, fail that site's bound
replay assertion (the real TS gold cannot be reproduced by a folded
U+FFFD order), and pass again after restoration. All selected outcomes are
reported; --check-anchors mutates no source (it does take the tree lock,
so a lock file may appear at the tree root).
"""
import os
from pathlib import Path
import subprocess
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import (CARGO_SLOTS_WAIT_SECONDS, mutating_sources,
                               run_owned, tree_lock, wait_for_slot)
from negative_selection import (claim_dir, missing_target, refuse,
                                refuse_unusable_target, registered, select,
                                verdict_exit, withdraw_claim)
from negative_suite import (Case, classify, invalidate_completed, passed,
                            publish_summary)

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

# The denominator of a run is this static roster, not `len(cases)`: with a
# runtime denominator a deleted row shrinks the run silently and a table
# that lost every row reports `0/0 killed` and exits 0 (#1666). Renaming or
# dropping a mutant here is a deliberate edit that must update this tuple
# too, in the same order as the table.
EXPECTED_NAMES = (
    'registry-list-folded',
    'registry-unavailable-folded',
    'mcp-instructions-folded',
    'cache-tools-folded',
    'model-usage-folded',
)

# Both exit paths publish the same counts schema: a per-call literal would let
# the guard-failure and verdict paths drift apart for consumers.
SUMMARY_STATES = ('KILLED', 'SURVIVED', 'INVALID', 'COMPILED')


def main():
    # The roster is validated before any branch, so `--check-anchors` also
    # refuses roster drift: a renamed or dropped row with a stale
    # EXPECTED_NAMES must not print "anchors OK" (the sdk-mcp-session-tools
    # precedent). Roster, subset selection and target checks are refusals
    # (exit 2, "never started"), never mistaken for a failed control (1).
    if any(not isinstance(row, tuple) or len(row) != 6 for row in cases):
        refuse('#2131 malformed case row: every mutation needs 6 fields')
    try:
        registered(cases, EXPECTED_NAMES)
    except ValueError as error:
        refuse(str(error))
    if sys.argv[1:] == ['--check-anchors']:
        failures = []
        # Hold the tree lock while reading: a sibling mutating runner may
        # have a mutant on disk between its write and restore phases, which
        # would make this read-only check see `count == 0` spuriously.
        try:
            with tree_lock(root, timeout=60):
                for name, path, old, _, _, _ in cases:
                    count = path.read_text().count(old)
                    if count != 1:
                        failures.append(f'{name}: expected 1 anchor, got {count}')
        except (OSError, UnicodeDecodeError) as error:
            # TimeoutError is an OSError subclass: a busy lock, an unreadable
            # or non-UTF-8 anchor source is a refusal (2), not a traceback (1).
            refuse(f'#2131 check-anchors tree lock or read failed: {error}')
        print('\n'.join(failures) if failures else f'{len(cases)} anchors OK')
        return int(bool(failures))
    args = [argument for argument in sys.argv[1:] if argument != '--compile-only']
    compile_only = '--compile-only' in sys.argv[1:]
    if not args:
        refuse('usage: --check-anchors | [--compile-only] NEW_OUTPUT_DIR [mutation ...]')
    if args[0].startswith('--'):
        # `--check-anchors extra` must not become an output directory name.
        refuse(f'usage: unknown option {args[0]}')
    # No baseline row exists in this suite: each case carries its own
    # positive control, so the selector's baseline default is turned off.
    try:
        selected = select(cases, args[1:], always=())
    except ValueError as error:
        refuse(str(error))
    if missing_target():
        refuse('#2131 set CARGO_TARGET_DIR to the caller-owned build tree before running')
    refuse_unusable_target()
    out = claim_dir(args[0])
    env = os.environ.copy()
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
                   'pending_reanchor.py', 'negative_selection.py',
                   'negative_scope.py'))]
    def startup_refusal(error):
        # A guard that never started (dirty tree, source-lock wait exhausted)
        # is a startup refusal, not a red run: rc 2, and the empty directory
        # claim_dir made is withdrawn so a refusal leaves no evidence dir that
        # looks claimed (specs/negative-controls/README.md, the exit-code
        # four-way table and the "guards run before claim_dir" paragraph).
        # #3143 fix-r2 (DS sibling#1): the shared atomic withdrawal (rename
        # + rmtree in one helper, empty or not) -- the same migration the
        # four fix-r1 runners carry.
        withdraw_claim(out)
        refuse(f'guard refused to start: {error!r}')

    # Startup is everything the guard does before the first mutation; after
    # `started`, an interruption is a red run (INVALID rows, rc 1), never a
    # refusal. A delayed cancellation already leaves as SystemExit(128+signum)
    # and is re-raised by the isinstance check below; a KeyboardInterrupt from
    # the startup window still reaches `startup_refusal` (rc 2, empty OUT
    # withdrawn) because the handler catches BaseException
    # (specs/negative-controls/README.md, the exit-code four-way table, the
    # boundary rule and the delayed-signal contract paragraph).
    started = False
    try:
        with mutating_sources(root, guarded) as originals:
            started = True
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
        # The guard's delayed cancellation carries its own exit code
        # (SystemExit(128+signum)); it must survive this handler untouched.
        if isinstance(error, SystemExit):
            raise
        if not started:
            startup_refusal(error)
        reason = f'source guard: {type(error).__name__}: {error}'
        invalidate_completed(results, reason)
        for name, _path, _old, _new, test, marker in selected[len(results):]:
            results.append(dict(mutation=name, test=test, assertion=marker,
                                status='INVALID', reason=reason, compiled=False))
            print(f'{name}: INVALID: {reason}', flush=True)
    publish_summary(out, results, states=SUMMARY_STATES)
    return verdict_exit(results, key='status',
                        killed='COMPILED' if compile_only else 'KILLED')


if __name__ == '__main__':
    raise SystemExit(main())
