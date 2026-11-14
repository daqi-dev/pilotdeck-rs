# anchor-selfcheck: unsupported shared subset-selection helper, no source replacement table
"""Subset selection shared by the gateway negative-control runners (#813 fix-r3).

A subset run shortens a host re-run; it is not a reason to skip the control that
is supposed to pass. The unmutated baseline therefore always runs first, and a
failing baseline makes the whole run INVALID before any mutant verdict counts.
"""
import os
import re
import sys
from pathlib import Path


def missing_target(environ=None):
    """True when ``CARGO_TARGET_DIR`` cannot be used: absent, empty or blank.

    An entry that requires a caller-owned build tree must refuse all three the
    same way. A presence-only check let a host that exported
    ``CARGO_TARGET_DIR=`` (or only spaces) through, and the shared target
    normalisation then rejected it *after* the evidence directory existed —
    exit 1 with a directory that reads like a finished run (#1666 fix-r7).
    """
    env = os.environ if environ is None else environ
    return not (env.get('CARGO_TARGET_DIR') or '').strip()


def blank_target(environ=None):
    """True when ``CARGO_TARGET_DIR`` is set but unusable (empty or spaces).

    Distinct from :func:`missing_target` for the entries that *default* the
    target when the variable is absent: absent keeps the shared default, while
    a present-but-blank value is a caller error that must refuse.
    """
    env = os.environ if environ is None else environ
    return 'CARGO_TARGET_DIR' in env and not env['CARGO_TARGET_DIR'].strip()


def unusable_target(environ=None):
    """The shared target rule's refusal message for ``CARGO_TARGET_DIR``, or None.

    ``missing_target`` is the presence half; this is the value half. The values
    are not re-listed here: they are delegated to
    ``negative_process.target_environment``, which is the one place that defines
    blank / dot / filesystem root / repository root / inside-``src`` (#1166).
    Re-listing them would rebuild exactly the family drift that let a dot target
    claim the evidence directory and then die on exit 1 in some entries while
    two others refused it (#1666 fix-r9).

    An *absent* variable is not a refusal here: the helper applies the shared
    default, so entries that require a caller-supplied tree still refuse
    "absent" with :func:`missing_target` themselves.
    """
    from negative_process import target_environment
    try:
        target_environment(Path(__file__).resolve().parents[4], environ)
    except RuntimeError as error:
        return str(error)
    return None


def refuse_unusable_target(environ=None):
    """Refuse a target value the shared rule rejects, before claiming any directory."""
    message = unusable_target(environ)
    if message:
        refuse(message)


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
    requested = list(requested)
    # A repeated name is a caller-side typo (`a a` for `a b`), not a subset.
    # Folding it silently turns a mistyped selection into a green run, which is
    # the same class of false green as an unknown name (#1666 / #1734).
    if len(set(requested)) != len(requested):
        raise ValueError(f'repeated control name in the selection: {requested}')
    requested = set(requested)
    unknown = requested - names
    if unknown:
        raise ValueError(f'unknown controls: {", ".join(sorted(unknown))}')
    return [row for row in rows
            if name(row) in always or not requested or name(row) in requested]


def registered(rows, expected_names, *, name=lambda row: row[0]):
    """Require the runtime table to equal a static roster, exactly.

    The denominator of a negative-control run must be a literal in the source,
    not ``len(table)``. With a runtime denominator, deleting a row silently
    shrinks the run: the summary says ``N/N killed`` for whatever survived, and
    a run that lost every row prints ``0/0 killed`` and exits 0 (#1666). This
    compares names in order and rejects duplicates, so a dropped, renamed,
    duplicated or reordered row is a startup refusal rather than a smaller
    green run.
    """
    names = [name(row) for row in rows]
    expected = list(expected_names)
    if not expected:
        raise ValueError('empty expected roster')
    if len(set(expected)) != len(expected):
        raise ValueError(f'duplicate names in expected roster: {expected}')
    if names != expected:
        missing = [item for item in expected if item not in names]
        extra = [item for item in names if item not in expected]
        raise ValueError(
            'control roster drifted from the static inventory'
            f' (missing={missing or "none"} extra={extra or "none"}'
            f' runtime={names} expected={expected})')
    return names


def claim_dir(path):
    """Claim the evidence directory; an existing one means "never started".

    A bare ``mkdir(exist_ok=False)`` raises ``FileExistsError``, which the exit
    code table reads as "the run happened and did not come out green" (1).
    Nothing was measured when the directory could not be claimed, so it is a
    refusal (2), and it must not be mistaken for a surviving mutant (#1734).
    """
    directory = Path(path).resolve()
    try:
        directory.mkdir(parents=True, exist_ok=False)
    except FileExistsError:
        refuse(f'output directory already exists: {directory}')
    except OSError as error:
        refuse(f'cannot create the output directory {directory}: {error}')
    return directory


def refuse(message):
    """Refuse to run: print the reason to stderr and exit 2.

    Exit 2 means "this run never started" -- an unknown control name, a
    control roster that no longer matches the static inventory, a locked or
    dirty tree, or an unreadable oracle. It is deliberately distinct from 1
    ("it ran and the result is not green"), so a reader can tell a false-green
    guard firing from a negative control that legitimately failed (#1734).

    A ``SystemExit`` carrying a *string* exits 1, so the message is written
    first and the code is passed explicitly as an int.

    #1087 fix-r5 (boundary #2 / sibling #2): a caller whose stderr is already
    dead must still exit 2 — an unguarded write would crash with rc=1 and read
    as "ran and was red". ``2>&-`` at exec time makes CPython set
    ``sys.stderr = None`` (AttributeError on write), a really closed stream
    object raises ValueError, and a dead fd raises OSError. The message is
    best-effort; the code is the contract.
    """
    try:
        sys.stderr.write(f'INVALID: {message}\n')
        sys.stderr.flush()
    except (AttributeError, OSError, ValueError):
        pass
    raise SystemExit(2)


def verdict_exit(rows, *, key='verdict', killed='KILLED'):
    """Exit code for a finished negative-control run.

    Success requires at least one row and every row KILLED. An empty selection
    (a mis-parse, a missing data mutation or a future branch that forgets to
    record) and any SURVIVED / INVALID / SKIPPED row are failures. In
    particular a run with zero KILLED can never return 0 (#818 fix-r3 P0).
    """
    statuses = [row[key] for row in rows]
    return 0 if statuses and all(status == killed for status in statuses) else 1
