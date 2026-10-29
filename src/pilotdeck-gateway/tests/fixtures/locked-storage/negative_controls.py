#!/usr/bin/env python3
"""#1027: HEAD-only controls; compile every mutant before its exact runtime test.

CARGO_TARGET_DIR must be supplied. Use --check for read-only anchor validation;
use [--compile-only] NEW_OUTPUT_DIR [mutation ...] after committing the patch.
The shared runner uses mutating_sources, run_owned and wait_for_slot, restores
within the guard and reports all mutations before returning a nonzero failure.
"""
from pathlib import Path
import sys

root = Path(__file__).resolve().parents[5]
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot
from negative_suite import matrix_main

source = root / 'src/pilotdeck-gateway/src/runtime/session/factory_tests.rs'
prefix = 'runtime::session::factory::tests::'
test = 'writer_factory_rejects_already_locked_storage'
mutations = [
    ('skip-locked-wait', source,
     'tokio::task::yield_now().await; // #1027 retry only while locked',
     'return; // #1027 mutation: skip the release wait',
     test, '#1027 lock release wait must remain pending while store is retained'),
    ('ignore-observed-unlock', source,
     'return; // #1027 lock release observed',
     'tokio::task::yield_now().await; // #1027 mutation: ignore observed unlock',
     test, '#1027 dropped store eventually releases its ledger lock'),
]

if __name__ == '__main__':
    raise SystemExit(matrix_main(root, __file__, prefix, mutations,
                                passed_count=1, child_count=0,
                                paths=[source], exact=True))
