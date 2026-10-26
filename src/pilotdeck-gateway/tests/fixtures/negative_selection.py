"""Subset selection shared by the gateway negative-control runners (#813 fix-r3).

A subset run shortens a host re-run; it is not a reason to skip the control that
is supposed to pass. The unmutated baseline therefore always runs first, and a
failing baseline makes the whole run INVALID before any mutant verdict counts.
"""


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
