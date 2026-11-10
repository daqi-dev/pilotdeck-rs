# anchor-selfcheck: unsupported shared static binding table, no source replacement table
"""#1261 native-entry `--check` bindings, shared by both runners (#1989 item 6).

`negative_controls.py` and `early_exit_invalid.py` each carried their own copy
of the marker constants and the assertion needles. They had already drifted --
the scrub "unjudgeable" needle bound the whole guard and message in one script
and only `if !advertised { panic!(` in the other -- so a rewrite could keep one
runner green and silently unbind the other. The constants and every needle
shared by both live here; each runner keeps only what is its own (the mutation
table, `ANCHOR`, the `GATE` needle, the early-exit rows).

Every needle is the *whole* `negative_assert!` / `assert!` call after
`negative_source.squeeze` (syntax whitespace folded, literal bytes exact), so
the head, the condition with its polarity and the message are bound together:
rewriting a call as `assert!(true, "…")` or as a tuple no longer satisfies it
(#1989 item 1). The byte-bound and newline-guard needles come from the same
idea (#1989 items 2 and 3): the guard is bound *inside* `advertises`, so
short-circuiting it or changing its return action is a drift, not a variant.

`binding_errors` returns every failure instead of raising, so the unit test can
drive one drift at a time and assert the exact needle that notices it.
"""
from negative_source import squeeze, test_body

TEST = 'native_entry_process_rejects_inherited_authority'
SCRUB_TEST = 'native_entry_scrubs_inherited_process_environment'
SHAPE_PIN = 'native_listening_advertisement_shape'
OBSERVATION_PIN = 'native_listening_observation_needs_a_finished_line'
ADVERTISES = 'advertises'
OBSERVE_CHILD = 'observe_child_stderr'

MARKERS = [
    '#1261 native entry rejects inherited PILOTDECK_STORAGE_ROOT',
    '#1261 native entry rejects inherited PILOTDECK_SNAPSHOT_ROOT',
    '#1261 native entry rejects inherited PILOTDECK_MANAGED_CONFIG',
]
MARKER_TEMPLATE = '#1261 native entry rejects inherited {key}'
SCRUB_MARKER = '#1261 native entry scrubs inherited PILOTDECK_CONFIG_PATH'
KEYS = ('PILOTDECK_STORAGE_ROOT', 'PILOTDECK_SNAPSHOT_ROOT', 'PILOTDECK_MANAGED_CONFIG')
# The unmarked scrub early-exit panic: it makes an unjudgeable run INVALID
# instead of a spurious pass or a spurious kill.
SCRUB_EARLY_EXIT = ('#1261 native entry scrub child neither advertised listening '
                    'nor exited, cannot judge')
# The single call each test makes to observe the advertisement.
OBSERVE = 'observe_child_stderr(stderr)'
# The delegation under the shared bounds: both durations by name and in order,
# so swapping them at the call site is a drift (#1989 item 5).
DELEGATION = ('read_advertisement(stderr, CHILD_SILENCE_LIMIT, '
              'CHILD_OBSERVATION_BUDGET).await')
# The P0 fix itself, bound inside `advertises`: a short-circuited guard or a
# rewritten return action unbinds the whole "finished line" rule (#1989 item 3).
NEWLINE_GUARD = 'if !line.ends_with(b"\\n") { return false; }'


def reject_needles():
    """The three kill assertions of the rejects test, each a whole call."""
    return (
        (OBSERVE, 'listening advertisement observation'),
        (f'negative_assert!(!advertised, "{MARKER_TEMPLATE}");',
         'listening advertisement assertion'),
        (f'negative_assert!(!status.success(), "{MARKER_TEMPLATE}");',
         'child exit status assertion'),
        (f'negative_assert!(leftover.is_empty(), "{MARKER_TEMPLATE}");',
         'home leftover assertion'),
    )


def scrub_needles():
    """Everything the scrub test's verdict rests on."""
    return (
        ('Failed to parse default config YAML.',
         'inherited config parse observation'),
        (OBSERVE, 'scrub listening observation'),
        (f'negative_assert!(!leaked_parse, "{SCRUB_MARKER}");',
         'scrub inherited config assertion'),
        (f'if !advertised {{ panic!("{SCRUB_EARLY_EXIT}"); }}',
         'scrub unjudgeable branch'),
    )


def shape_needles():
    """The shape pin's two decisions and the cases that tell them apart.

    A fragment needle survives `assert!(true, "must count…")`; binding the
    whole call keeps the condition and its polarity (#1989 item 1).
    """
    return (
        ('assert!(advertises(line.as_bytes()), '
         '"must count as an advertisement: {line:?}");',
         'accepted advertisement assertion'),
        ('assert!(!advertises(line.as_bytes()), '
         '"must not count as an advertisement: {line:?}");',
         'rejected advertisement assertion'),
        (r'"native listening 127.0.0.1:4321 extra\n"',
         'trailing junk rejection'),
        (r'"native listening [::1]:4321\r\n"',
         'IPv6 advertisement acceptance'),
    )


def observation_needles():
    """The observation pin's branches, inputs and elapsed bounds, each bound whole."""
    return (
        # The inputs, not only the verdicts: a case whose bytes were trimmed
        # away keeps its assertion green against a different scenario.
        (r'&b"native listening 127.0.0.1:4321\n"[..]',
         'finished advertisement input'),
        (r'&b"native listening 127.0.0.1:4321"[..]',
         'EOF half-line input'),
        ("stream.resize(stream.len() + CHILD_STDERR_LIMIT * 2, b'1');",
         'unterminated line longer than the byte bound'),
        ('assert!(advertised, "finished advertisement: {collected:?}");',
         'finished advertisement case'),
        ('assert!(!advertised, "line cut by EOF: {collected:?}");',
         'EOF half-line case'),
        ('assert!(!advertised, "line cut by the byte bound: {} bytes", '
         'collected.len());',
         'byte bound half-line case'),
        ('assert!(collected.len() <= CHILD_STDERR_LIMIT, "collected {} bytes", '
         'collected.len());',
         'byte bound stops the buffer'),
        ('assert!(!advertised, "silence: {collected:?}");',
         'silence case'),
        ('assert!(!advertised, "read error: {collected:?}");',
         'read error case'),
        # The verdict alone would stay green with the evidence dropped: both
        # error and budget cases keep the bytes they read, and deleting the
        # evidence assertion is invisible to the other branches.
        ('assert!(collected.ends_with(b"127.0.0.1:4321"), "read error keeps '
         'the bytes read before it: {collected:?}");',
         'read error keeps the bytes read before it'),
        ('assert!(!advertised, "budget: {} bytes", collected.len());',
         'total budget case'),
        # The injections themselves, not just their verdicts: without the
        # injection the assertions would run against whatever reader the
        # previous case left behind and stay green (#1989 item 5).
        ('ErrorAfter::new(b"native listening 127.0.0.1:4321"),',
         'read error injection'),
        ('TalkativeLines::lines(3), budget, silence',
         'talkative child total budget'),
        ('assert!(collected.ends_with(b"noise from a child that never '
         'advertises\\n"), "the budget case read whole non-advertisement '
         'lines: {collected:?}");',
         'budget case reads whole lines'),
        # The elapsed bounds are what separate the 50 ms silence from the 5 s
        # budget; without them a swapped pair of bounds stays green (#1989 item
        # 5).
        ('assert!(elapsed < std::time::Duration::from_secs(1), '
         '"silence bound, not the total budget: {elapsed:?}");',
         'silence elapsed bound'),
        ('assert!(elapsed < std::time::Duration::from_secs(1), '
         '"the total budget ends a talkative child: {elapsed:?}");',
         'total budget elapsed bound'),
        # The two duration inputs themselves (#2102 P2-2): the elapsed bounds
        # above keep their word as long as the 1 s check sits far below the
        # budget, so shrinking `from_secs(5)` to 500 ms leaves every runtime
        # pin green and `timeout(left, ...)` losing `silence.min(left)` only
        # ever costs the budget -- the exact drift item 5 guards against.
        # Bound as the whole `let` statements, so a swapped pair of bounds is
        # red here too.
        ('let silence = std::time::Duration::from_millis(50);',
         'silence bound input'),
        ('let budget = std::time::Duration::from_secs(5);',
         'total budget input'),
    )


def _body(test_source, name):
    """`test_body` + `squeeze`, or LookupError for a missing function."""
    return squeeze(test_body(test_source, name))


def binding_errors(test_source):
    """Every shared binding that does not hold, as `what: detail` strings."""
    bodies = {}
    errors = []
    for name in (TEST, SCRUB_TEST, SHAPE_PIN, OBSERVATION_PIN, ADVERTISES,
                 OBSERVE_CHILD):
        try:
            bodies[name] = _body(test_source, name)
        except LookupError as error:
            errors.append(str(error))
            bodies[name] = ''
    rejects = bodies[TEST]
    scrub = bodies[SCRUB_TEST]
    shape = bodies[SHAPE_PIN]
    observation = bodies[OBSERVATION_PIN]
    guard = bodies[ADVERTISES]
    delegation = bodies[OBSERVE_CHILD]

    def require(haystack, needle, what):
        if needle not in haystack:
            errors.append(what)

    require(rejects, MARKER_TEMPLATE, f'marker template missing from {TEST}')
    for key in KEYS:
        require(rejects, f'"{key}"', f'key missing from {TEST}: {key}')
    for needle, what in reject_needles():
        require(rejects, needle, f'missing {what} from {TEST}')
    for needle, what in scrub_needles():
        require(scrub, needle, f'missing {what} from {SCRUB_TEST}')
    for needle, what in shape_needles():
        require(shape, needle, f'missing {what} from the shape pin')
    for needle, what in observation_needles():
        require(observation, needle, f'missing {what} from the observation pin')
    require(guard, NEWLINE_GUARD, f'missing the newline guard from {ADVERTISES}')
    require(delegation, DELEGATION,
            f'missing the bounded delegation from {OBSERVE_CHILD}')
    return errors
