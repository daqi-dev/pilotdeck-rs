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


def squeeze(source):
    """Collapse whitespace *between tokens*, leaving string literals intact.

    A multi-line ``negative_assert!(`` call is the same call as its one-line
    form, so a check that binds the condition, its polarity and the message
    together has to look past rustfmt's line breaks. Whitespace inside a
    literal is data (module docstring), so literals are copied verbatim:
    collapsing them would let a one-space needle match a two-space message
    (#1401 review P2). A space that only separated a bracket from its content
    is dropped too, so a call reads the same whether rustfmt kept it on one
    line or spread it over five.
    """
    out = []
    pending_space = False

    def flush(next_char):
        if pending_space and out and out[-1] not in '([' and next_char not in ')]':
            out.append(' ')

    i = 0
    while i < len(source):
        ch = source[i]
        if ch == '"':
            flush(ch)
            pending_space = False
            j = i + 1
            while j < len(source) and source[j] != '"':
                j += 2 if source[j] == '\\' else 1
            out.append(source[i:j + 1])
            i = j + 1
            continue
        if ch.isspace():
            pending_space = True
        else:
            flush(ch)
            pending_space = False
            out.append(ch)
        i += 1
    return ''.join(out)


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
