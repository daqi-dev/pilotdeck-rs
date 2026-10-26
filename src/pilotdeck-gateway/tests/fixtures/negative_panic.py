"""Libtest panic-block evidence shared by the gateway negative controls (#813).

A negative control is valid only when the *named* test failed and the marker
lives in that test's own panic block. The older ``marker in raw`` check also
matched another test's panic, ordinary stdout, and the named test's own
``test ... ok`` line, so a surviving mutant could be judged KILLED.

Caller preconditions (all four consumers):

* one libtest invocation per process, with ``--exact``. The ``<unnamed>``
  widening requires exactly one test in that invocation. A module-level caller
  may pass several full names explicitly, but then ``single_failed_test_run`` is
  false and only the named blocks count;
* the argument is a full test name as libtest prints it, never a module path;
* the marker is an assertion message owned by the named test.

Names are compared exactly. A name is never widened to its submodules: a
``pkg::tests::named::nested`` panic is not evidence for ``pkg::tests::named``.
Callers need no implicit module expansion; those that judge a module pass the
full test names explicitly (a string, or an iterable of strings). A module path
such as ``pkg::tests`` raises ``ValueError`` instead of silently matching its
descendants (#813 fix-r2).

A test may assert on a thread it spawned itself. libtest names those threads
``<unnamed>``, so the assertion panic is not in the named test's block (#813
fix-r1). Such a block is accepted only when the named test is the one libtest
declared FAILED *and* this process ran exactly one test (``running 1 test``
plus ``1 failed``). With one test in the process, any ``<unnamed>`` thread can
only have been spawned by that test; with two or more tests the block cannot be
attributed and stays rejected.

Known limitation (#813 fix-r2, P2-2): libtest streams a detached thread's panic
*after* the named test's ``... FAILED`` line -- the recorded host logs even show
it after ``test result:`` -- so "the block is before FAILED" cannot separate a
panic inside the test body from one after the test returned. The single-test
guard is therefore still a heuristic: a detached thread that outlives the test
and panics with the same marker is accepted. Keep every marker an assertion
message unique to the named test, and treat a new detached-thread consumer as a
new analysis, not as a free widening.
"""
import re


def _full_test_names(used_test):
    """Return explicit full test names; reject a bare module path.

    Module-level callers must name every test they mean. Silently treating a
    module path as a prefix is exactly what #813 fix-r2 removed.
    """
    names = [used_test] if isinstance(used_test, str) else list(used_test)
    if not names:
        raise ValueError('at least one full test name is required')
    for name in names:
        if not isinstance(name, str) or not name:
            raise ValueError(f'not a full test name: {name!r}')
        # libtest names are function paths. A trailing `tests` segment is the
        # module path callers used to pass by mistake.
        if name.rsplit('::', 1)[-1] == 'tests':
            raise ValueError(
                f'{name!r} looks like a module path, not a full test name; '
                'pass the full test names explicitly')
    return names


def _observed_test_names(raw):
    """Every name libtest printed on a ``test <name> ... <status>`` line."""
    names = []
    for line in raw.splitlines():
        match = re.match(r'^test (\S+) \.\.\. (?:ok|FAILED|ignored|measured)$', line)
        if match:
            names.append(match.group(1))
    return names


def _reject_observed_module_paths(raw, names):
    """Raise when ``raw`` proves a name is only a prefix of observed tests."""
    observed = _observed_test_names(raw)
    for name in names:
        if name in observed:
            continue
        nested = sorted({seen for seen in observed if seen.startswith(name + '::')})
        if nested:
            raise ValueError(
                f'{name!r} is a module path, not a full test name; '
                f'observed tests: {", ".join(nested)}')


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
    """Return the panic blocks attributable to the full test name(s)."""
    names = _full_test_names(used_test)
    _reject_observed_module_paths(raw, names)
    include_unnamed = (any(named_test_failed(raw, name) for name in names)
                       and single_failed_test_run(raw))
    blocks = []
    for segment in raw.split("thread '")[1:]:
        name = segment.split("'", 1)[0]
        if name in names:
            blocks.append(segment)
        elif include_unnamed and name == '<unnamed>':
            blocks.append(segment)
    return '\n'.join(blocks)


def marker_kills_named_test(raw, used_test, marker):
    """Require the marker inside a full test's own panic block.

    ``used_test`` is one full name or an explicit iterable of full names.
    ``marker`` may be a string or an iterable of alternative strings; an
    iterable is accepted so callers keep their existing ``a|b`` fixtures without
    weakening the scope.
    """
    evidence = panic_evidence(raw, used_test)
    markers = [marker] if isinstance(marker, str) else marker
    return any(candidate in evidence for candidate in markers)


def named_test_failed(raw, used_test):
    """libtest declares one of the full test names failed on its own status line.

    Exact equality only: a nested test under ``used_test`` never counts. Callers
    that judge a module pass every full name explicitly.
    """
    names = set(_full_test_names(used_test))
    _reject_observed_module_paths(raw, names)
    suffix = ' ... FAILED'
    for line in raw.splitlines():
        if not line.startswith('test ') or not line.endswith(suffix):
            continue
        if line[len('test '):-len(suffix)] in names:
            return True
    return False


def named_test_killed(raw, used_test, marker):
    """Strict #813 criterion: a full test failed and marker is in its panic block.

    With an explicit list, the marker must belong to one of the listed tests
    that actually failed; a passing list member cannot lend its block.
    """
    names = _full_test_names(used_test)
    failed = [name for name in names if named_test_failed(raw, name)]
    return bool(failed) and marker_kills_named_test(raw, failed, marker)
