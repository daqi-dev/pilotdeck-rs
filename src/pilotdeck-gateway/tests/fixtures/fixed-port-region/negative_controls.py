#!/usr/bin/env python3
"""#2461/#2462/#2463 controls for the fixed-port guard family in
`native_process::channels_tests.rs`: the guard's three silent failure modes
must stay observable — the watcher's cap arm must speak before releasing, the
close probe must read only a refusal as closed, and `console_tail` must not
panic on a multi-byte cut.

OUT [mutation ...], --compile-only OUT, --check (also --check-anchors).
The shared runner owns source restoration, offline cargo, nonce receipts,
patch evidence, pending registrations and the complete result summary.

Rows 1-2 pin the #2461 cap arm: deleting the emit or emptying the port list
each leaves the cap observable through exactly one named assertion. Row 3
recreates the pre-#2461-r3 release: dropping the flock synchronously inside
`Drop` while the listener still accepts — the `.all(|_| true)` shortcut was
considered and rejected here because its kill rides on a thread race between
the watcher's release and the main thread's `try_lock`, while the synchronous
release is dropped before `Drop` returns and therefore kills deterministically.
Rows 4-6 pin the #2462 judgment: any error read as a refusal, an accepted
connect read as closed, and a refusal that no longer matches each turn the
same four-shape control red by name. Rows 7-9 pin the #2463 byte window: the
panicking slice, the dropped alignment, and the halved window. Row 10 pins
the #2461 fix-r7 deadline alignment: the watcher cap tied back to the claim
wait recreates the pre-r7 race where the waiter always expired first.

Prefix '' with every cell a full module path (same widening precedent as
`channel-loader-startup`), exact mode, one cell per row: the mutation must
fail its declared assertion outright, never partially.
"""
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_suite import matrix_main

root = Path(__file__).resolve().parents[5]
source = root / 'src/pilotdeck-gateway/src/native_process/channels_tests.rs'
# #2461 fix-r6: the shared fixed-port classifier lives here now; rows 4-6
# mutate it (see REFUSED_GUARD / ACCEPTED_ARM above).
classifier = root / 'src/pilotdeck-gateway/tests/common/fixed_port_probe.rs'

# #2461 row 1: the cap arm's emit, deleted. The release still happens, so only
# the emission count can notice the diagnostic is gone.
EMIT_LINE = '            emit(&fixed_port_timeout_diagnostic(&probes, cap));\n'

# #2461 row 2: the not-refused list's join, emptied. The diagnostic still fires
# once and still names the cap, but no longer the port.
JOIN_PORTS = '        not_refused.join("; ")\n'

# #2461 row 3: the hand-off's send, replaced by the synchronous release. The
# watcher thread still spawns and exits on its failed recv; the flock is free
# the moment `Drop` returns, above a listener that still accepts.
SEND_BLOCK = (
    '                if let Err(returned) = hand_off.send(files) {\n'
    '                    drop(returned.0);\n'
    '                }\n'
)

# #2462 rows 4+6: the ConnectionRefused guard. Row 4 collapses it into a
# catch-all refusal; row 6 re-aims it at a kind no refusal ever carries.
# #2461 fix-r6: the classifier moved to the shared tests-only module
# (`tests/common/fixed_port_probe.rs`), so the anchors follow it there; the
# kill cell stays the same nail test (it calls the shared classifier now).
REFUSED_GUARD = (
    '        Err(error) if error.kind() == std::io::ErrorKind::ConnectionRefused => {\n'
    '            FixedPortProbe::Refused\n'
    '        }\n'
)

# #2462 row 5: the Ok arm, reclassified as a refusal (the shared generic form
# carries the accepted payload).
ACCEPTED_ARM = '        Ok(value) => FixedPortProbe::Accepted(value),\n'

# #2463 rows 7+8: the alignment chain — the boundary-safe `get` plus the
# `.or_else` forward-alignment. Row 7 reverts it to the panicking byte slice;
# row 8 drops the `.or_else`, so a multi-byte cut falls to the named fallback
# and the payload dies with it. The multi-line anchor keeps it distinct from
# the test's own saturating_sub.
GET_MATCH = (
    '    let start = console.len().saturating_sub(4096);\n'
    '    match console\n'
    '        .get(start..)\n'
    '        .or_else(|| (1..=3).find_map(|delta| console.get(start + delta..)))\n'
    '    {\n'
)

# The un-aligned shape row 8 replaces the chain with: a plain `get` whose
# `None` arm is the named fallback (the payload is then dropped wholesale).
GET_WITHOUT_ALIGNMENT = (
    '    let start = console.len().saturating_sub(4096);\n'
    '    match console.get(start..) {\n'
)

# The halved window of row 9: the boundary-aligned tail is no longer the
# last 4096 bytes.
GET_MATCH_HALVED = (
    '    let start = console.len().saturating_sub(2048);\n'
    '    match console\n'
    '        .get(start..)\n'
    '        .or_else(|| (1..=3).find_map(|delta| console.get(start + delta..)))\n'
    '    {\n'
)

# #2461 fix-r7 row 10: the watcher cap's derivation. Dividing by one ties the
# cap back to the full claim wait — the pre-r7 shape where the waiter's
# deadline (starting when it begins waiting) always expired before the cap
# arm (starting when the region drops) could release the flock.
WATCHER_CAP = (
    'const FIXED_PORT_WATCHER_CAP: Duration = '
    'Duration::from_secs(FIXED_PORT_CLAIM_WAIT_SECS / 2);\n'
)
WATCHER_CAP_TIED = (
    'const FIXED_PORT_WATCHER_CAP: Duration = '
    'Duration::from_secs(FIXED_PORT_CLAIM_WAIT_SECS / 1);\n'
)

# The cap-arm cell: the diagnostic is exactly one line and carries the port.
CAP_COUNT = ('native_process::channels_tests::fixed_port_watcher_cap_releases_through_the_named_diagnostic',
             '#2461 the cap arm emits exactly one named diagnostic')
CAP_PORT = ('native_process::channels_tests::fixed_port_watcher_cap_releases_through_the_named_diagnostic',
            '#2461 the cap diagnostic names the not-refused port')
# The ordering cell: the flock must still be held above a live listener.
ORDERING_HELD = ('native_process::channels_tests::fixed_port_region_releases_only_after_its_listener_closes',
                 '#2461 the flock is still held while the listener accepts connections')
# The #2462 cells: the four-shape judgment, driven without real fd exhaustion.
PROBE_REFUSED = ('native_process::channels_tests::only_a_refusal_proves_the_fixed_port_closed',
                 '#2462 a refused probe is the only shape that proves the port closed')
PROBE_ACCEPTED = ('native_process::channels_tests::only_a_refusal_proves_the_fixed_port_closed',
                  '#2462 an accepted connect is a named failure')
PROBE_OTHER = ('native_process::channels_tests::only_a_refusal_proves_the_fixed_port_closed',
               '#2462 a non-refused error kind is a named failure')
# The #2463 cells: the panic, the dropped alignment, and the window.
TAIL_NO_PANIC = ('native_process::channels_tests::console_tail_cuts_inside_a_multibyte_character_without_panicking',
                 '#2463 console_tail must not panic when the 4 KiB cut lands inside a character')
TAIL_ALIGNED = ('native_process::channels_tests::console_tail_cuts_inside_a_multibyte_character_without_panicking',
                '#2463 the multi-byte cut forward-aligns instead of dropping the tail')
TAIL_WINDOW = ('native_process::channels_tests::console_tail_takes_the_last_4096_bytes',
               '#2463 the tail of a boundary-aligned console is its last 4096 bytes')
# The fix-r7 cell: the invariant itself, stated where the constants live.
CLAIM_OUTLIVES = ('native_process::channels_tests::fixed_port_claim_wait_strictly_outlives_the_watcher_cap',
                  '#2461 the claim wait strictly outlives the watcher cap, so the cap arm can still save a waiter that arrived first')

MUTATIONS = [
    # The emit is deleted: the cap arm releases through the diagnostic that
    # no longer exists, so the control's own count goes to zero.
    ('watcher-cap-diagnostic-dropped', source, EMIT_LINE,
     '            // #2461 negative control: the cap diagnostic is dropped\n',
     [CAP_COUNT]),
    # The not-refused list joins nothing: one diagnostic without the port.
    ('watcher-cap-diagnostic-portless', source, JOIN_PORTS,
     '        String::new()\n',
     [CAP_PORT]),
    # The hand-off becomes the synchronous release: the flock is free before
    # `Drop` returns, with the listener still accepting — the exact ordering
    # the cell exists to keep dead, deterministically (see the docstring).
    ('watcher-releases-inside-drop', source, SEND_BLOCK,
     '                drop(files);\n',
     [ORDERING_HELD]),
    # Every error becomes a refusal: EMFILE stops being a named failure, which
    # is precisely the false PASS #2462 was filed for.
    ('close-probe-reads-any-error-as-refused', classifier, REFUSED_GUARD,
     '        Err(_error) => FixedPortProbe::Refused,\n',
     [PROBE_OTHER]),
    # An accepted connect becomes closed: the still-open listener passes.
    ('close-probe-reads-accepted-as-closed', classifier, ACCEPTED_ARM,
     '        Ok(_value) => FixedPortProbe::Refused,\n',
     [PROBE_ACCEPTED]),
    # The guard no longer matches a refusal: the only closed-port proof turns
    # into a named failure.
    ('close-probe-refusal-never-matches', classifier, REFUSED_GUARD,
     '        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {\n'
     '            FixedPortProbe::Refused\n'
     '        }\n',
     [PROBE_REFUSED]),
    # The alignment chain reverts to the byte slice: the multi-byte cut panics
    # the message builder instead of aligning.
    ('console-tail-panics-on-multibyte-cut', source, GET_MATCH,
     '    let start = console.len().saturating_sub(4096);\n'
     '    match Some(&console[start..]) {\n',
     [TAIL_NO_PANIC]),
    # The `.or_else` alignment is dropped: a multi-byte cut falls to the
    # named fallback and the payload is dropped wholesale.
    ('console-tail-alignment-dropped', source, GET_MATCH,
     GET_WITHOUT_ALIGNMENT,
     [TAIL_ALIGNED]),
    # The window is halved: the boundary-aligned tail is no longer the last
    # 4096 bytes.
    ('console-tail-window-halved', source, GET_MATCH,
     GET_MATCH_HALVED,
     [TAIL_WINDOW]),
    # The cap ties back to the full claim wait: the pre-r7 race where the
    # waiter's deadline always expired before the cap arm could release the
    # flock, so the sibling failed `#2301` by name instead of being saved.
    ('watcher-cap-equals-claim-wait', source, WATCHER_CAP,
     WATCHER_CAP_TIED,
     [CLAIM_OUTLIVES]),
]


def main():
    args = ['--check'] if sys.argv[1:] == ['--check-anchors'] else sys.argv[1:]
    # No `any_cell`: every mutation must fail its declared cell outright.
    # Prefix '': every cell is a full module path (see the docstring).
    return matrix_main(root, __file__, '', MUTATIONS,
                       passed_count=1, child_count=0, paths=[source, classifier],
                       exact=True, args=args)


if __name__ == '__main__':
    raise SystemExit(main())
