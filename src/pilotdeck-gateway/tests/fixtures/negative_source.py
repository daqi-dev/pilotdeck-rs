"""Exact-source checks shared by negative-control runners (#813 fix-r4).

rustfmt may split a long ``assert!(...)`` across lines, so a preflight that
compares the whole file must ignore whitespace *between tokens*. Whitespace
inside a string literal is data, though: changing ``must match`` to
``must  match`` changes the assertion message even though ``str.split()`` sees
the same tokens.
"""
import re


def assertion_message_present(source, message):
    """``assert!(selected > 0, "<message>")`` with syntax-only whitespace.

    Whitespace around ``assert!``, the comparison, the comma and the closing
    parenthesis may differ (rustfmt). The literal itself is matched byte for
    byte, so a drift inside the message is rejected.
    """
    pattern = (
        r'assert!\s*\(\s*selected\s*>\s*0\s*,\s*"'
        + re.escape(message)
        + r'"\s*\)\s*;'
    )
    return re.search(pattern, source) is not None
