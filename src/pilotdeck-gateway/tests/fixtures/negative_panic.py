"""Libtest panic-block evidence shared by the gateway negative controls (#813).

A negative control is valid only when the *named* test failed and the marker
lives in that test's own panic block. The older ``marker in raw`` check also
matched another test's panic, ordinary stdout, and the named test's own
``test ... ok`` line, so a surviving mutant could be judged KILLED.

The slicing follows the #747 fix in ``turn-submit/negative.py``: libtest names
the panic thread after the test, and a name extending the requested test with
``::`` still belongs to the requested test/module.
"""


def panic_evidence(raw, used_test):
    """Return the panic blocks owned by ``used_test``."""
    blocks = []
    for segment in raw.split("thread '")[1:]:
        name = segment.split("'", 1)[0]
        if name == used_test or name.startswith(used_test + '::'):
            blocks.append(segment)
    return '\n'.join(blocks)


def marker_kills_named_test(raw, used_test, marker):
    """Require the marker inside the named test's panic block.

    ``marker`` may be a string or an iterable of alternative strings; an
    iterable is accepted so callers keep their existing ``a|b`` fixtures without
    weakening the scope.
    """
    evidence = panic_evidence(raw, used_test)
    markers = [marker] if isinstance(marker, str) else marker
    return any(candidate in evidence for candidate in markers)


def named_test_failed(raw, used_test):
    """libtest declares a test failed on its own ``test <name> ... FAILED`` line.

    A nested test under ``used_test`` also counts, mirroring
    :func:`panic_evidence`; callers normally pass one full test name.
    """
    suffix = ' ... FAILED'
    for line in raw.splitlines():
        if not line.startswith('test ') or not line.endswith(suffix):
            continue
        name = line[len('test '):-len(suffix)]
        if name == used_test or name.startswith(used_test + '::'):
            return True
    return False


def named_test_killed(raw, used_test, marker):
    """Strict #813 criterion: named test failed and marker is in its panic block."""
    return named_test_failed(raw, used_test) and marker_kills_named_test(raw, used_test, marker)
