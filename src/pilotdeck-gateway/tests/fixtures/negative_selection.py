"""Subset selection shared by the gateway negative-control runners (#813 fix-r3).

A subset run shortens a host re-run; it is not a reason to skip the control that
is supposed to pass. The unmutated baseline therefore always runs first, and a
failing baseline makes the whole run INVALID before any mutant verdict counts.
"""
import re


def baseline_passed(raw, test_name):
    """The named test ran and libtest reported it green.

    This is the verdict for the unmutated copy of every selected target test
    (#813 fix-r4). It requires the exact ``test <name> ... ok`` line and a
    zero-failure result, so a compile error or a sibling test's pass cannot
    stand in for the target.
    """
    passed = re.search(r'^test ' + re.escape(test_name) + r' \.\.\. ok$', raw,
                       re.MULTILINE)
    result = re.search(r'^test result: ok\. (\d+) passed; 0 failed;', raw,
                       re.MULTILINE)
    return (passed is not None and result is not None
            and int(result.group(1)) >= 1
            and re.search(r'^test result: FAILED', raw, re.MULTILINE) is None)


def select(controls, requested, *, name=lambda row: row[0], always=('baseline',)):
    """Return the always-run rows plus the requested controls, in input order.

    ``requested`` is an empty sequence for a full run. Names are compared
    exactly; an unknown name raises instead of silently producing a shorter
    run. The old runners only asserted that the filtered list was non-empty, so
    one valid name hid every typo and any non-empty subset dropped the
    baseline.
    """
    rows = list(controls)
    always = tuple(always)
    names = {name(row) for row in rows}
    missing = [item for item in always if item not in names]
    if missing:
        raise ValueError(f'missing baseline control(s): {", ".join(missing)}')
    requested = set(requested)
    unknown = requested - names
    if unknown:
        raise ValueError(f'unknown controls: {", ".join(sorted(unknown))}')
    return [row for row in rows
            if name(row) in always or not requested or name(row) in requested]
