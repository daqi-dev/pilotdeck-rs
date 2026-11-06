# anchor-selfcheck: unsupported shared exact-source assertion helper, no source replacement table
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


def test_body(source, name):
    """The named Rust test function's body, cut at its own closing brace.

    A file-level needle cannot say *which* test observes something (#1401
    review, both lineages): once two tests mention the same string, deleting
    one observation still satisfied a whole-file check. Cutting forward to the
    next test attribute instead is not enough either -- it swallows every
    helper defined after the last test, and only recognising a bare
    ``#[test]``/``#[tokio::test]`` misses
    ``#[tokio::test(flavor = "multi_thread")]``. rustfmt puts a function's
    closing brace in column 0 and indents everything nested, so the first
    column-0 ``}`` (or a following test attribute, for input that never had
    one) ends the body.
    """
    start = re.search(
        rf'^(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?fn\s+{re.escape(name)}\s*[<(]',
        source, re.M)
    if start is None:
        raise LookupError(f'{name}: no such function')
    rest = source[start.end():]
    end = re.search(r'^(?:#\[(?:[\w:]+::)?test\b|\})', rest, re.M)
    if end is None:
        raise LookupError(f'{name}: unterminated function')
    return rest[:end.start()]
