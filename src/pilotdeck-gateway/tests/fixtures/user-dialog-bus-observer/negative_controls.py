#!/usr/bin/env python3
"""#2887: the #1445 user-dialog assembly connectivity claim as a compiled switch.

`build_user_dialog_bus` (native_process/assembly/user_dialog.rs) builds the
production bus with `GatewayUserDialogBus::connected`, and
`production_bus_is_connected_without_an_observer` claimed in prose that
reverting to `GatewayUserDialogBus::new(runtime, None)` (observer-coupled
connectivity) turns it red. Two non-author reviews of the #1445 family found
the claim was never registered as an executable control, so nothing automatic
stopped the comment and the code from drifting apart — exactly the gap the
mcp-resolver-lease fixture closed for #2157. This fixture registers it: the
switch is a compiled `crate::mutants::on` read in that assembly file; the
source is never rewritten, so the executor precompiles once with
`--features pdrs-mutants` and runs every phase in its own process.

Judged through the #946 nonce receipt protocol (receipts=True): the mutation
only counts as killed on the bound test's named FAILED line plus this run's
assertion receipt, never on prose or a sibling failure. The bound assertion is
the single message-carrying failure point on the test's path (the
`unwrap_or_else(into_inner)` lock-poison recovery cannot fail first). The
startup contract is the 2026-11 hardened shape: the EXPECTED_NAMES roster is
checked with the shared `negative_selection.registered` in every mode,
`--check` takes no output directory and no names (an empty-string positional is
still a positional), unknown/repeated names and unusable caller targets are
refused with rc 2 before the evidence directory is claimed (`claim_dir`),
a stale anchor refuses the run instead of recording SURVIVED, and the exit
verdict is the shared `negative_selection.verdict_exit` over the published
summary rows. CARGO_TARGET_DIR must be supplied and passes the shared #1166
target rule; the admitted target is normalized to its repo-root-anchored
absolute value and written back before the run, so the shared artifact() judge
reads the directory cargo actually built in.

Usage: negative_controls.py --check
       negative_controls.py NEW_OUTPUT_DIR [switch ...]
"""
import argparse
import json
import os
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from _shared import mutant_switch
from negative_process import target_environment, tree_lock
from negative_selection import (claim_dir, missing_target, refuse,
                                 refuse_unusable_target, registered, verdict_exit)

ROOT = Path(__file__).resolve().parents[5]
FIXTURES = ROOT / 'src/pilotdeck-gateway/tests/fixtures'
ASSEMBLY = ROOT / 'src/pilotdeck-gateway/src/native_process/assembly/user_dialog.rs'
# (name, path, test, assertion): the switch site is counted in the target file.
# With the switch selected the assembly reverts to the legacy observer-coupled
# constructor, so the bound assertion — the bus is live without a renderer
# change observer — fails on the disconnected bus.
switches = [
    ('user-dialog-bus-observer-coupled', ASSEMBLY,
     ('native_process::assembly::user_dialog::tests::'
      'production_bus_is_connected_without_an_observer'),
     '#1445 production assembly must build a connected bus'),
]
EXPECTED_NAMES = ('user-dialog-bus-observer-coupled',)
# #2133: neg-for-diff.sh finds the tables a diff needs by the literal
# `dir/file` of every changed file (`git grep -F 'fixtures/negative_suite.py'`).
# The imported shared helpers and the fixture modules their import chain
# reaches are keys this table builds from bare file names, so a change under
# `fixtures/` would schedule no mutation at all without these anchors; the
# rows and this tuple must agree (fixtures/test_negative_discovery_anchors.py).
DISCOVERY_ANCHORS = (
    '_shared/mutant_switch.py',
    'fixtures/negative_process.py',
    'fixtures/negative_suite.py',
    'fixtures/negative_selection.py',
    'fixtures/negative_panic.py',
    'fixtures/negative_scope.py',
    'fixtures/pending_reanchor.py',
    'fixtures/anchor_selfcheck.py',
)
GUARDED = [
    ASSEMBLY,
    # The bound test's judgement semantics live in the bus constructors and
    # the module chain up to the assembly; concurrent edits to any of them
    # during a run must be caught by the source guard (the mcp-resolver-lease
    # GUARDED precedent).
    ROOT / 'src/pilotdeck-gateway/src/user_dialog/bus.rs',
    ROOT / 'src/pilotdeck-gateway/src/user_dialog/mod.rs',
    ROOT / 'src/pilotdeck-gateway/src/native_process.rs',
    ROOT / 'src/pilotdeck-gateway/src/native_process/assembly.rs',
    # Workers re-execute with env_clear; the selector forwarding and the
    # receipt recording live here and must not change under a running suite.
    ROOT / 'src/pilotdeck-gateway/tests/common/negative_receipt.rs',
    ROOT / 'src/pilotdeck-gateway/src/mutants.rs',
    ROOT / 'src/pilotdeck-gateway/src/lib.rs',
    ROOT / 'src/pilotdeck-gateway/Cargo.toml',
    ROOT / 'src/Cargo.toml',
    ROOT / 'src/Cargo.lock',
    FIXTURES / '_shared/mutant_switch.py',
    FIXTURES / 'negative_suite.py',
    FIXTURES / 'negative_process.py',
    FIXTURES / 'negative_selection.py',
    Path(__file__).resolve(),
]


def stale_names():
    """Rows whose bound test / assertion literal no longer holds in the source.

    A stale anchor is a "never started" refusal (rc 2), never a SURVIVED row:
    the switch could compile and the bound test could still pass for an
    unrelated reason, and a host would read that as the control being green.
    """
    stale = []
    text = ASSEMBLY.read_text(encoding='utf-8')
    for name, _path, test, assertion in switches:
        if '{' in assertion or '}' in assertion:
            stale.append(f'{name} (bound assertion is a format! template)')
            continue
        function = test.rsplit('::', 1)[-1]
        if (text.count(f'"{assertion}"') != 1
                or text.count(f'fn {function}') != 1
                or text.count('GatewayUserDialogBus::connected(') != 1
                or text.count('GatewayUserDialogBus::new(') != 1):
            stale.append(name)
    return stale


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--check', action='store_true')
    parser.add_argument('output', nargs='?')
    parser.add_argument('names', nargs='*')
    args = parser.parse_args()
    # #2495 fix-r2 argv contract: --check's usage line (no output directory,
    # no names) is machine-checked, not just documented. `is not None`, not
    # truthiness: an empty-string positional (an unset shell variable
    # unquoted) is still a positional.
    if args.check and (args.output is not None or args.names):
        refuse('#2887 --check takes no output directory and no switch names')
    # The runtime table must equal the static roster in every mode; a row
    # dropped from `switches` cannot silently shrink the denominator. The
    # audit whitelist (anchor_selfcheck.switch_table_rows) reads `switches`
    # only through this for-in, so the roster is compared over the row dicts
    # the executor itself consumes.
    rows = [dict(zip(('name', 'path', 'test', 'anchor'), row)) for row in switches]
    try:
        registered(rows, EXPECTED_NAMES, name=lambda row: row['name'])
    except ValueError as error:
        refuse(f'#2887 {error}')
    # Caller-side name errors are refused from argv alone, before any guard
    # reads live sources or claims the evidence directory.
    if not args.check:
        unknown = sorted(set(args.names) - {row[0] for row in switches})
        if unknown:
            refuse(f'#2887 unknown switch names: {unknown}')
        repeated = sorted({name for name in args.names
                           if args.names.count(name) > 1})
        if repeated:
            refuse(f'#2887 repeated switch names: {repeated}')
    chosen = [row for row in rows if not args.names or row['name'] in args.names]
    try:
        mutant_switch.preflight(rows)
    except ValueError as error:
        refuse(f'#2887 {error}')
    if not args.check:
        # The target is admitted by the one shared rule before the evidence
        # directory is claimed: absent, empty, blank and dot are all "never
        # started" (rc 2), and the normalized absolute value is written back
        # so the shared artifact() judge reads the directory cargo builds in.
        if not args.output:
            refuse('#2887 an output directory is required')
        if missing_target():
            refuse('#2887 set CARGO_TARGET_DIR to the caller-owned build tree')
        refuse_unusable_target()
        try:
            os.environ['CARGO_TARGET_DIR'] = target_environment(
                ROOT, os.environ)['CARGO_TARGET_DIR']
        except RuntimeError as error:
            refuse(f'#2887 {error}')
    # The stale scan reads the working tree, so it takes the same tree lock
    # the executor below takes (a concurrent sibling with a mutant compiled
    # from a mid-edit source must not be read back as a false stale, and a
    # busy lock is itself the same refusal).
    try:
        with tree_lock(ROOT, timeout=60):
            stale = stale_names()
    except (TimeoutError, OSError, UnicodeDecodeError) as error:
        refuse('#2887 tree lock or stale read failed: '
               f'{type(error).__name__}: {error}')
    if stale:
        refuse('#2887 stale anchor/test/assertion bindings: ' + ', '.join(stale))
    if args.check:
        print(f'#2887 {len(rows)} switch anchors and bindings checked '
              f'(EXPECTED_NAMES roster matched)')
        return 0
    output = claim_dir(args.output)
    code = mutant_switch.run(ROOT, output, chosen,
                             mutant_switch.clean_environment(os.environ),
                             guarded=GUARDED, prefix='', script=Path(__file__),
                             requested=args.names, receipts=True)
    # The exit contract is the shared verdict over the published rows, not
    # the executor's own integer: zero rows is never 0 and any non-KILLED
    # row fails, re-derived from the one file a host reads.
    try:
        summary = json.loads((output / 'summary.json').read_text(encoding='utf-8'))
        return verdict_exit(summary['controls'], key='status')
    except (OSError, ValueError, KeyError) as error:
        print(f'#2887 published summary unreadable: {type(error).__name__}: {error}',
              file=sys.stderr)
        return code


if __name__ == '__main__':
    sys.exit(main())
