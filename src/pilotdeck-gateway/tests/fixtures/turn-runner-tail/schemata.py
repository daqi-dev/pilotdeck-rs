#!/usr/bin/env python3
"""#996: turn-runner-tail controls as compile-time switches.

The #742/#936 text-replacement negative_controls.py is deleted; each mutation
is now a ``crate::mutants::on`` branch compiled into the feature-enabled
library test binary. CARGO_TARGET_DIR and PDRS_BASELINE must be supplied.
Use --check for a read-only anchor preflight, or NEW_OUTPUT_DIR [switch ...].
"""
import argparse
import os
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from _shared import mutant_switch

ROOT = Path(__file__).resolve().parents[5]
SRC = ROOT / 'src/pilotdeck-gateway/src/runtime/turn'
TAIL = SRC / 'runner_tail.rs'
ARTIFACTS = SRC / 'runner_artifacts.rs'
RUNNER = SRC / 'runner.rs'
LIVE = SRC / 'live.rs'
PROMPT = SRC / 'runner_prompt.rs'
# #2228 DS r2 P1: tail-empty-blocks binds a test in runner_prompt_tests.rs,
# which runner.rs assembles through runner_tests.rs (`#[path]` chain); the
# binding inherits via `use super::*` from both parents.
RUNNER_TESTS = SRC / 'runner_tests.rs'
PROMPT_TESTS = SRC / 'runner_prompt_tests.rs'
RUNNER_NOTICE_TESTS = SRC / 'runner_notice_tests.rs'
# Assembled by live.rs itself (`#[path]` at the `mod tail_tests` boundary).
LIVE_TAIL_TESTS = SRC / 'live_tail_tests.rs'
# The regeneration switch lives at the library-test call site: the shared
# baseline module is compiled into integration-test crates as well, where the
# library's switches do not exist.
TAIL_TESTS = SRC / 'runner_tail_tests.rs'
BASELINE = ROOT / 'src/pilotdeck-gateway/tests/common/baseline.rs'
UNIT = 'runtime::turn::runner::tail::tests::'
LIVE_UNIT = 'runtime::turn::live::tail_tests::'
# (name, path, test, assertion): the switch site is counted in the target file.
switches = [
    ('tail-title-after-dispose', TAIL, UNIT + 'title_start_after_dispose',
     '#742 title'),
    ('tail-nonregular-file', ARTIFACTS, UNIT + 'collector_policy',
     '#742 collector'),
    ('tail-env-dot-boundary', ARTIFACTS, UNIT + 'collector_policy',
     '#742 collector'),
    ('tail-root-symlink', ARTIFACTS, UNIT + 'collector_policy',
     '#742 collector'),
    ('tail-listing-first-overwrite', TAIL, UNIT + 'listing_two_turns',
     '#742 tail'),
    ('tail-host-prefix-constant', RUNNER,
     'runtime::turn::live::tests::host_lifecycle_set_is_complete', '#936'),
    ('tail-delete-suggestion', TAIL, UNIT + 'suggestion_on', '#742 tail'),
    ('tail-drop-suggestion-error-diagnostic', TAIL,
     UNIT + 'suggestion_failure_injected', '#742 tail'),
    ('tail-suggestion-empty-assistant', TAIL, UNIT + 'suggestion_on',
     '#742 tail'),
    ('tail-artifact-omitted', TAIL, UNIT + 'files_created', '#742 tail'),
    ('tail-artifact-status', TAIL, UNIT + 'files_aborted', '#742 tail'),
    ('tail-artifact-disabled', TAIL, UNIT + 'files_collection_disabled',
     '#742 tail'),
    ('tail-listing-disabled', TAIL, UNIT + 'listing_two_turns', '#742 tail'),
    ('tail-listing-utf16-limit', TAIL, UNIT + 'listing_utf16_limit',
     '#742 tail'),
    ('tail-listing-first', TAIL, UNIT + 'listing_two_turns', '#742 tail'),
    ('tail-listing-last', TAIL, UNIT + 'listing_two_turns', '#742 tail'),
    ('tail-listing-model', TAIL, UNIT + 'listing_empty_model', '#742 tail'),
    ('tail-snapshot-marker', TAIL, UNIT + 'files_none', '#742 tail'),
    ('tail-snapshot-mutates-memory', TAIL, UNIT + 'files_none', '#742 tail'),
    ('tail-title-disabled', TAIL, UNIT + 'title_before', '#742 tail'),
    ('tail-title-aborted-write', TAIL, UNIT + 'title_abort_ignored',
     '#742 tail'),
    ('tail-title-provenance', TAIL, UNIT + 'title_before', '#742 tail'),
    ('tail-title-sequence', TAIL, UNIT + 'title_before', '#742 tail'),
    ('tail-title-locked-port', TAIL,
     UNIT + 'title_port_reentrance_and_detach_race', '#742 title'),
    ('tail-title-detach-race', TAIL,
     UNIT + 'title_port_reentrance_and_detach_race', '#742 title'),
    ('tail-metadata-owner', TAIL,
     UNIT + 'metadata_write_retains_owner_and_serial_lock', '#742 metadata'),
    ('tail-metadata-serial', TAIL,
     UNIT + 'metadata_write_retains_owner_and_serial_lock', '#742 metadata'),
    ('tail-tool-source', ARTIFACTS, UNIT + 'files_explicit_path', '#742 tail'),
    ('tail-tool-observation', ARTIFACTS, UNIT + 'files_created', '#742 tail'),
    ('tail-same-file-hash', ARTIFACTS, UNIT + 'collector_policy',
     '#742 collector'),
    ('tail-overlap-diff', ARTIFACTS, UNIT + 'collector_policy',
     '#742 collector'),
    ('tail-sensitive-file', ARTIFACTS, UNIT + 'collector_policy',
     '#742 collector'),
    ('tail-hard-internal', ARTIFACTS, UNIT + 'collector_policy',
     '#742 collector'),
    ('tail-allowed-input', ARTIFACTS, UNIT + 'collector_policy',
     '#742 collector'),
    ('tail-readonly-tool', ARTIFACTS, UNIT + 'collector_policy',
     '#742 collector'),
    ('tail-denied-tool', ARTIFACTS, UNIT + 'collector_policy',
     '#742 collector'),
    ('tail-env-example', ARTIFACTS, UNIT + 'collector_policy',
     '#742 collector'),
    ('tail-known-depth', ARTIFACTS, UNIT + 'collector_policy',
     '#742 collector'),
    ('tail-artifact-size', ARTIFACTS, UNIT + 'files_created', '#742 tail'),
    ('tail-artifact-operation', ARTIFACTS, UNIT + 'files_updated', '#742 tail'),
    ('tail-artifact-id', ARTIFACTS, UNIT + 'files_created', '#742 tail'),
    ('tail-live-tail-wiring', LIVE, LIVE_UNIT + 'live_tail_progress_and_drop',
     '#742 live tail'),
    ('tail-live-tool-observation', LIVE,
     LIVE_UNIT + 'live_tail_progress_and_drop', '#742 live tail'),
    ('tail-empty-blocks', PROMPT,
     'runtime::turn::runner::tests::prompts::blocks_empty', '#936 empty blocks'),
    ('tail-host-prompt-rejection', LIVE,
     'runtime::turn::live::tests::host_lifecycle_set_is_complete', '#936'),
    ('tail-regeneration', TAIL_TESTS, UNIT + 'regenerates_identically',
     '#742 tail fixture regeneration'),
]
FIXTURES = ROOT / 'src/pilotdeck-gateway/tests/fixtures'
GUARDED = [
    TAIL,
    ARTIFACTS,
    RUNNER,
    LIVE,
    PROMPT,
    RUNNER_TESTS,
    PROMPT_TESTS,
    TAIL_TESTS,
    # #2228 DS r3 P1 / zai r3 P1 (independent hit): the LIVE_UNIT bindings run
    # through live.rs's #[path] assembly of live_tail_tests.rs — the
    # live_tail_progress_and_drop test body lives there and inherits via
    # `use super::*`, and its setup() runs live-session/peer.py as a python3
    # subprocess — so concurrent edits to either during a run must be caught
    # by the source guard.
    LIVE_TAIL_TESTS,
    FIXTURES / 'live-session/peer.py',
    # #2228 DS r3 P1: the row bindings bake turn-runner-tail/oracle.json into
    # the binary (include_str! via runner_tail_tests.rs's row()) and the
    # tail-regeneration binding runs turn-runner-tail/generate.mts through
    # tests/common/baseline.rs, so concurrent edits to either during a run
    # must be caught by the source guard.
    FIXTURES / 'turn-runner-tail/oracle.json',
    FIXTURES / 'turn-runner-tail/generate.mts',
    # #2228 DS r4 P1: the tail-empty-blocks binding's row() bakes
    # turn-prompt-hooks/oracle.json into the binary (runner_prompt_tests.rs:161
    # include_str! via row()), so a concurrent edit during a run must be caught
    # by the source guard; generate.mts stays unregistered because no binding
    # here regenerates that fixture.
    FIXTURES / 'turn-prompt-hooks/oracle.json',
    # #2228 DS r6 sibling P2 / MM r6 concurrency P2 (same class as the r4
    # entry above): RUNNER_TESTS also assembles runner_notice_tests.rs
    # (`#[path]` at runner_tests.rs:18-19), whose row() bakes
    # turn-start-notices/oracle.json into the same binary
    # (runner_notice_tests.rs:46 include_str!); generate.mts stays
    # unregistered for the same reason as the prompt twin above.
    RUNNER_NOTICE_TESTS,
    FIXTURES / 'turn-start-notices/oracle.json',
    # #2228 DS r3 P2 / zai r3 P2 (independent hit): the top of the turn chain —
    # runtime/turn/mod.rs declares the runner/live modules the bindings
    # resolve through, and runtime/mod.rs declares turn itself, capping the
    # chain at the same declarator depth the project chain registers.
    ROOT / 'src/pilotdeck-gateway/src/runtime/turn/mod.rs',
    ROOT / 'src/pilotdeck-gateway/src/runtime/mod.rs',
    BASELINE,
    # #2228 MM r6 boundary P1 / DS r6 sibling+concurrency P2 (two families,
    # graded P1): baseline.rs unconditionally compiles its #[path] submodule
    # tests/common/regeneration_masks.rs, whose include_str! bakes the shared
    # mask table; the tail-regeneration row's compare()
    # (runner_tail_tests.rs:521 → baseline.rs:348) executes that code over that
    # table, so both are runtime judges of this fixture's verdict.
    ROOT / 'src/pilotdeck-gateway/tests/common/regeneration_masks.rs',
    FIXTURES / '_shared/regeneration-masks.tsv',
    ROOT / 'src/pilotdeck-gateway/src/mutants.rs',
    ROOT / 'src/pilotdeck-gateway/src/lib.rs',
    ROOT / 'src/pilotdeck-gateway/Cargo.toml',
    ROOT / 'src/Cargo.toml',
    ROOT / 'src/Cargo.lock',
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
                                 guarded=GUARDED, prefix='', script=Path(__file__),
                                 requested=args.names)
    except (ValueError, OSError) as error:
        print(f'preflight: {error}', file=sys.stderr)
        return 2


if __name__ == '__main__':
    sys.exit(main())
