# anchor-selfcheck: unsupported shared marker-scope predicate, no source replacement table
"""One definition of "this marker names a single assertion" (#1666 / #1734).

Two negative-control runners carried their own copy of this predicate and had
drifted apart: ``negative_panic.marker_is_scoped`` accepted markers the
``_negkit.contract`` copy refused (multi-line payloads and the ``: step``
suffix both name a scenario, not the target assertion). The same marker could
therefore pass one kit and be rejected by the other. Both now import this
module, so a future rule change lands in exactly one place.

The strictest of the two historical rule sets is kept: single line, no bare
``assertion`` / ``assertion `left == right`` prefix, no bare ``"<case>":``
label, no ``: step`` suffix, and no watchdog/timeout payload prefix.
"""
import re

# #813 fix-r8: a watchdog/timeout payload means the test hung; it is not an
# assertion failure. The prefixes are fixed so the judge can reject the payload
# without knowing which scenario stalled. Never begin such a message with a
# scenario name: prefix markers would otherwise match it.
HANG_PREFIXES = ('#813 gateway watchdog:', '#813 timeout:')

# `"<case>": ` accepts any failure of that scenario, including a different step
# or a hang. Trailing whitespace is part of the form.
_CASE_ONLY_MARKER = re.compile(r'^"[^"]+":\s*$')


def marker_is_scoped(marker):
    """Whether ``marker`` names one assertion instead of a class of failures.

    A bare scenario prefix is not evidence that the *target* assertion failed.
    Callers that know the target also pass ``target=`` to
    ``marker_kills_named_test``; this check is the fallback for every other
    consumer (#813 fix-r8, #1734).
    """
    return (isinstance(marker, str) and bool(marker.strip())
            and len(marker.splitlines()) == 1
            and marker != 'assertion'
            and not marker.startswith('assertion `left == right`')
            and _CASE_ONLY_MARKER.match(marker) is None
            and not marker.rstrip().endswith(': step')
            and not marker.startswith(HANG_PREFIXES))
