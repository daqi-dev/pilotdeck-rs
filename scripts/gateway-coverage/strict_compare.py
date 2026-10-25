"""Type-strict comparison helpers shared by the two coverage validators.

``true == 1`` and ``101.0 == 101`` are false PASSes for an audit ledger: a
bool/float retype must never satisfy an int pin.  Both ``check_ledger.py`` and
``check_coverage_manifest.py`` import these, so the two rulers cannot drift
apart on comparison semantics.  Pure stdlib; safe under ``python3 -I`` once the
importing script has put its own directory on ``sys.path``.
"""


def strict_equal(left, right):
    """Type-strict deep equality; bool/int/float drift never compares equal."""
    if type(left) is not type(right):
        return False
    if isinstance(left, dict):
        return left.keys() == right.keys() and all(
            strict_equal(left[key], right[key]) for key in left)
    if isinstance(left, list):
        return len(left) == len(right) and all(
            strict_equal(a, b) for a, b in zip(left, right))
    return left == right


def strict_contains(values, target):
    return any(strict_equal(value, target) for value in values)
