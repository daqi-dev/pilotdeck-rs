#!/usr/bin/env python3
"""#1019: the adopted-recording fence transfer as compile-time switches.

The mutations are ``crate::mutants::on`` branches in the production factory
(feature ``pdrs-mutants`` + ``PDRS_MUTANT``), so this control needs no source
replacement: the shared precompiled executor builds the feature-enabled
library test binary once, then runs the bound regression per switch row —
positive without a selector, negative with the switch selected, restored in
a fresh process — and reconciles the executed rows with this statically
audited table (#996 denominator check).

Each switch reverts one piece of the #1019 ordering contract, and each kill
lands on a dedicated assertion:
- ``adopt-open-before-mutex`` opens the recording before the process writer
  mutex is held; the exclusion test's opener counter then shows the rejected
  second factory did open, which the fence must have prevented.
- ``adopt-fence-dropped`` discards the transferred fence (the forbidden
  "just skip the lock"); the unlocked second writer then succeeds instead of
  being rejected.
- ``adopt-unfenced-recording`` accepts a recording with no fence; the
  rejection test then gets a session instead of the typed error.
- ``adopt-busy-untyped`` drops the SESSION_WRITER_BUSY mapping for the
  opener's lock contention; the busy test then fails on the error code.
- ``adopt-clone-not-retired`` skips retiring the recording's writer at the
  fence handoff; the retained opener clone then appends (a second sequence
  cache forking the chain) instead of the typed retirement rejection.
- ``adopt-deadline-not-rebased`` keeps the pre-construction absolute deadline
  for the recording branch's mutex wait; a slow drained snapshot then expires
  that budget into the try-once busy error.
- ``adopt-store-error-untyped`` drops the Node storage-error mapping for the
  opener's non-BUSY failures; the opener-failure test then fails on the
  exact Node code and message.

Usage: CARGO_TARGET_DIR=/abs/target python3 switch_negative.py NEW_OUTPUT_DIR [switch ...]
"""
import argparse
import os
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from _shared import mutant_switch

ROOT = Path(__file__).resolve().parents[5]
FIXTURES = ROOT / 'src/pilotdeck-gateway/tests/fixtures'
SWITCH_SOURCE = ROOT / 'src/pilotdeck-gateway/src/runtime/session/factory.rs'
TEST_SOURCE = ROOT / 'src/pilotdeck-gateway/src/runtime/session/factory_tests.rs'
PERSISTENCE_SOURCE = ROOT / 'src/pilotdeck-gateway/src/runtime/project/persistence.rs'
# (name, path, test, assertion): the switch site is counted in the target file.
# The kill must land on the dedicated #1019 assertion of each bound test.
switches = [
    ('adopt-open-before-mutex', SWITCH_SOURCE,
     'runtime::session::factory::tests::writer_adopt_excludes_second_factory_and_alias',
     '#1019 rejected second factory never opens the recording'),
    ('adopt-fence-dropped', SWITCH_SOURCE,
     'runtime::session::factory::tests::writer_adopt_fence_excludes_unlocked_writer',
     '#1019 adopted fence excludes an unlocked second writer'),
    ('adopt-unfenced-recording', SWITCH_SOURCE,
     'runtime::session::factory::tests::writer_adopt_rejects_unfenced_recording',
     '#1019 unfenced recording must not join a session'),
    ('adopt-busy-untyped', SWITCH_SOURCE,
     'runtime::session::factory::tests::writer_adopt_busy_is_typed_session_writer_busy',
     '#1019 opener lock contention is the typed busy error'),
    ('adopt-clone-not-retired', PERSISTENCE_SOURCE,
     'runtime::session::factory::tests::writer_adopt_retires_retained_opener_writer_clones',
     '#1019 retained opener writer clone cannot append after the handoff'),
    ('adopt-deadline-not-rebased', SWITCH_SOURCE,
     'runtime::session::factory::tests::writer_adopt_budget_excludes_slow_drained_snapshot',
     '#1019 slow drained snapshot retains the adopt path\'s busy wait budget'),
    ('adopt-store-error-untyped', SWITCH_SOURCE,
     'runtime::session::factory::tests::writer_adopt_opener_failure_releases_prepared',
     '#1019 opener failure keeps the node storage error message'),
]
GUARDED = [
    SWITCH_SOURCE,
    TEST_SOURCE,
    ROOT / 'src/pilotdeck-gateway/src/runtime/project/persistence.rs',
    ROOT / 'src/pilotdeck-gateway/src/storage/transcript_adapter.rs',
    ROOT / 'src/pilotdeck-gateway/src/storage/recorder.rs',
    ROOT / 'src/pilotdeck-gateway/src/storage/transcript.rs',
    ROOT / 'src/pilotdeck-gateway/src/mutants.rs',
    ROOT / 'src/pilotdeck-gateway/src/lib.rs',
    ROOT / 'src/pilotdeck-gateway/Cargo.toml',
    ROOT / 'src/Cargo.toml',
    ROOT / 'src/Cargo.lock',
    ROOT / 'src/pilotdeck-gateway/tests/common/baseline.rs',
    FIXTURES / '_shared/mutant_switch.py',
    FIXTURES / 'negative_suite.py',
    FIXTURES / 'negative_process.py',
    Path(__file__).resolve(),
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
                                 mutant_switch.clean_environment(os.environ),
                                 guarded=GUARDED, script=Path(__file__),
                                 requested=args.names)
    except (ValueError, OSError) as error:
        print(f'preflight: {error}', file=sys.stderr)
        return 2


if __name__ == '__main__':
    sys.exit(main())
