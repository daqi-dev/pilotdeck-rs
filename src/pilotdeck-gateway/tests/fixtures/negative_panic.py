"""Libtest panic-block evidence shared by the gateway negative controls (#813).

A negative control is valid only when the *named* test failed and the marker
lives in that test's own panic block. The older ``marker in raw`` check also
matched another test's panic, ordinary stdout, and the named test's own
``test ... ok`` line, so a surviving mutant could be judged KILLED.

The slicing follows the #747 fix in ``turn-submit/negative.py``: libtest names
the panic thread after the test, and a name extending the requested test with
``::`` still belongs to the requested test/module.

A test may assert on a thread it spawned itself. libtest names those threads
``<unnamed>``, so the assertion panic is not in the named test's block (#813
fix-r1). Such a block is accepted only when the named test is the one libtest
declared FAILED *and* this process ran exactly one test (``running 1 test``
plus ``1 failed``). With one test in the process, any ``<unnamed>`` thread can
only have been spawned by that test; with two or more tests the block cannot be
attributed and stays rejected.
"""
import re


def single_failed_test_run(raw):
    """``running 1 test`` and ``1 failed`` in the same libtest invocation.

    Requiring exactly one of each line also rejects concatenated runs, whose
    counts would otherwise be ambiguous.
    """
    running = re.findall(r'^running (\d+) tests?$', raw, re.MULTILINE)
    failed = re.findall(r'^test result: FAILED\. \d+ passed; (\d+) failed;', raw,
                        re.MULTILINE)
    return running == ['1'] and failed == ['1']


def panic_evidence(raw, used_test):
    """Return the panic blocks attributable to ``used_test``."""
    include_unnamed = named_test_failed(raw, used_test) and single_failed_test_run(raw)
    blocks = []
    for segment in raw.split("thread '")[1:]:
        name = segment.split("'", 1)[0]
        if name == used_test or name.startswith(used_test + '::'):
            blocks.append(segment)
        elif include_unnamed and name == '<unnamed>':
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
