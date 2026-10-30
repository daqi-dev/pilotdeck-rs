#!/usr/bin/env python3
"""#1082 compiled controls for the same-rootDir two-generation SQLite matrix.

Same executor contract as `edgeclaw-native-default/negative.py`: compile every
switch once with `pdrs-mutants`, then run the bound positive, negative and
restored cases through `negative_suite` verdicts. No source rewriting, no
alternate HEAD and no second target directory. The bound tests live in the
library module `storage::same_root_tests` (the deterministic in-flight writer
seam is `cfg(test)` and not visible to the integration-test crate), so this
fixture owns its prefix instead of extending the default fixture's list.
"""
import argparse
import os
from pathlib import Path
import sys
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from _shared import mutant_switch

ROOT = Path(__file__).resolve().parents[5]
FIXTURES = ROOT / 'src/pilotdeck-gateway/tests/fixtures'
STORAGE = ROOT / 'src/pilotdeck-gateway/src/runtime/memory/edgeclaw/storage.rs'
TESTS = ROOT / 'src/pilotdeck-gateway/src/runtime/memory/edgeclaw/same_root_tests.rs'
INTEGRATION = ROOT / 'src/pilotdeck-gateway/tests/edgeclaw_native_default.rs'
PREFIX = 'runtime::memory::edgeclaw::storage::'
SERVICE = ROOT / 'src/pilotdeck-gateway/src/runtime/memory/edgeclaw/service/mod.rs'
FACTORY = ROOT / 'src/pilotdeck-gateway/src/runtime/memory/edgeclaw/factory.rs'
PROVIDER = ROOT / 'src/pilotdeck-gateway/src/runtime/memory/edgeclaw/provider.rs'
switches = [
    ('native-same-root-close-unserialized', STORAGE,
     'same_root_tests::same_root_close_serializes_with_in_flight_write',
     '#1082 explicit close must observe Close-specific lock contention'),
    ('native-same-root-reentrant-lock-blocking', STORAGE,
     'same_root_tests::same_root_drop_from_locked_callback_completes',
     '#1082 callback Drop must not reacquire its own process lock'),
    ('native-same-root-inner-releases-owner', STORAGE,
     'same_root_tests::same_root_reentrant_guards_preserve_outer_owner',
     '#1082 inner guard preserves outer ownership'),
    ('native-same-root-stale-thread-owner', STORAGE,
     'same_root_tests::same_root_reentrant_guards_preserve_outer_owner',
     '#1082 outer guard releases thread ownership'),
    ('native-same-root-drop-unserialized', STORAGE,
     'same_root_tests::same_root_drop_serializes_with_in_flight_write',
     '#1082 implicit close must observe Drop-specific lock contention'),
    ('native-same-root-lock-bypassed', STORAGE,
     'same_root_tests::same_root_read_during_in_flight_write_does_not_error',
     '#1082 serialized read must see committed row'),
    ('native-same-root-lock-not-shared', STORAGE,
     'same_root_tests::same_root_first_write_serializes_with_old_generation_write',
     '#1082 new first write must serialize with old writer'),
    ('native-same-root-stage-unserialized', STORAGE,
     'same_root_tests::same_root_stage_serializes_with_in_flight_write',
     '#1082 new stage must serialize with old writer'),
    ('native-same-root-lock-poison-fatal', STORAGE,
     'same_root_tests::same_root_lock_survives_writer_panic',
     '#1082 shared write lock poisoned'),
    ('native-same-root-busy-wait', STORAGE,
     'same_root_tests::same_root_stage_needing_migration_fails_immediately_against_external_writer',
     '#1082 cross-process stage still fails immediately'),
    ('native-same-root-dead-locks-retained', STORAGE,
     'same_root_tests::same_root_registry_reclaims_dead_paths_and_preserves_live_locks',
     '#1082 registry must reclaim dead stage paths'),
    ('native-same-root-live-locks-pruned', STORAGE,
     'same_root_tests::same_root_registry_reclaims_dead_paths_and_preserves_live_locks',
     '#1082 registry must preserve live lock identity'),
    ('native-same-root-peer-write-lost', STORAGE,
     'same_root_tests::same_root_generation_matches_ts_matrix',
     '#1082 serial row exists'),
    ('workspace-state-key', SERVICE,
     'same_root_lifecycle_tests::staged_flush_stage',
     '#1082 lifecycle matrix staged-flush-stage'),
    ('native-close-noop', FACTORY,
     'same_root_lifecycle_tests::cancelled_release_flush_write',
     '#1082 lifecycle matrix cancelled-release-flush-write'),
    ('provider-empty-diagnostic', PROVIDER,
     'same_root_lifecycle_tests::cancelled_retrieve_write',
     '#1082 lifecycle matrix cancelled-retrieve-write'),
    ('native-same-root-flush-not-indexed', STORAGE,
     'same_root_lifecycle_tests::disposing_flush_write',
     '#1082 lifecycle matrix disposing-flush-write'),
]

GUARDED = [
    STORAGE, TESTS, INTEGRATION, PROVIDER,
    ROOT / 'src/pilotdeck-gateway/src/runtime/memory/edgeclaw/same_root_lifecycle_tests.rs',
    ROOT / 'src/pilotdeck-gateway/src/runtime/project.rs',
    FIXTURES / 'edgeclaw-native-default/observe.mts',
    FIXTURES / '_shared/baseline.mts',
    FIXTURES / 'anchor_selfcheck.py',
    Path(__file__).with_name('lifecycle.mts'),
    ROOT / 'src/pilotdeck-gateway/src/runtime/memory/edgeclaw/service/mod.rs',
    ROOT / 'src/pilotdeck-gateway/src/runtime/memory/edgeclaw/factory.rs',
    ROOT / 'src/pilotdeck-gateway/src/runtime/memory/mod.rs',
    ROOT / 'src/pilotdeck-gateway/src/mutants.rs',
    ROOT / 'src/pilotdeck-gateway/src/lib.rs',
    ROOT / 'src/pilotdeck-gateway/Cargo.toml', ROOT / 'src/Cargo.toml', ROOT / 'src/Cargo.lock',
    FIXTURES / '_shared/mutant_switch.py', FIXTURES / 'negative_suite.py',
    FIXTURES / 'negative_process.py',
    Path(__file__), Path(__file__).with_name('oracle.json'),
    Path(__file__).with_name('generate.mts'), Path(__file__).with_name('observe.mts'),
    Path(__file__).with_name('README.md'),
]

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--check', action='store_true')
    parser.add_argument('output', nargs='?')
    parser.add_argument('names', nargs='*')
    args = parser.parse_args()
    try:
        rows = [dict(zip(('name', 'path', 'test', 'anchor'), row)) for row in switches]
        chosen = mutant_switch.selected(rows, args.names)
        mutant_switch.preflight(rows)
        if args.check:
            if args.output or args.names:
                raise ValueError('--check does not take output or names')
            print(f'{len(rows)} switch anchors and bindings checked')
            return 0
        if not args.output or not os.environ.get('CARGO_TARGET_DIR'):
            raise ValueError('NEW_OUTPUT_DIR and caller CARGO_TARGET_DIR are required')
        output = Path(args.output).resolve()
        output.mkdir(parents=True, exist_ok=False)
        return mutant_switch.run(ROOT, output, chosen,
            mutant_switch.clean_environment(os.environ), guarded=GUARDED,
            prefix=PREFIX, script=Path(__file__), requested=args.names)
    except Exception as error:
        print(f'{type(error).__name__}: {error}', file=sys.stderr)
        return 2

if __name__ == '__main__':
    sys.exit(main())
