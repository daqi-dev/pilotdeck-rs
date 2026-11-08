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
    byte, so a drift inside the message is rejected. The match runs on
    ``squeeze(source)``, which drops comments while keeping literal bytes, so a
    commented-out copy of the assertion -- a line the compiler ignores -- does
    not count as present (#2102 P2-1).
    """
    pattern = (
        r'assert!\s*\(\s*selected\s*>\s*0\s*,\s*"'
        + re.escape(message)
        + r'"\s*\)\s*;'
    )
    return re.search(pattern, squeeze(source)) is not None


def _line_comment_end(source, start):
    """End of a ``//`` comment: the newline stays code, as it separates."""
    end = source.find('\n', start + 2)
    return len(source) if end < 0 else end


def _block_comment_end(source, start):
    """End of a (possibly nested) ``/* ... */`` comment.

    Rust block comments nest; stopping at the first ``*/`` would end the
    literal early and expose comment text as code.
    """
    depth = 0
    i = start
    while i < len(source):
        if source.startswith('/*', i):
            depth += 1
            i += 2
        elif source.startswith('*/', i):
            depth -= 1
            i += 2
            if depth == 0:
                return i
        else:
            i += 1
    return len(source)


def _string_end(source, start):
    """End of a ``"..."`` literal, honouring backslash escapes."""
    i = start + 1
    while i < len(source):
        if source[i] == '\\':
            i += 2
        elif source[i] == '"':
            return i + 1
        else:
            i += 1
    return len(source)


def _raw_string_end(source, start):
    """End of a ``r"…"`` / ``r#"…"#`` / ``br…`` / ``cr…`` literal, or None.

    ``r#ident`` is a raw identifier, not a literal: the prefix only counts
    when a quote follows the hash run. At most 255 hashes are allowed.
    """
    for prefix in ('br', 'cr', 'r'):
        if not source.startswith(prefix, start):
            continue
        i = start + len(prefix)
        hashes = 0
        while i < len(source) and source[i] == '#' and hashes < 255:
            hashes += 1
            i += 1
        if i >= len(source) or source[i] != '"':
            continue
        closing = '"' + '#' * hashes
        end = source.find(closing, i + 1)
        return len(source) if end < 0 else end + len(closing)
    return None


def _char_literal_end(source, start):
    """End of a ``'x'`` / ``'\\n'`` / ``'\\u{1F600}'`` literal, or None.

    A lifetime or loop label (``'a``, ``'_``, ``'outer:``) has no closing
    quote, so it stays code and its surrounding whitespace still collapses.
    """
    i = start + 1
    if i >= len(source) or source[i] == "'":
        return None
    if source[i] == '\\':
        i += 2
        while i < len(source) and i - start <= 12 and source[i] != "'":
            i += 1
        return i + 1 if i < len(source) and source[i] == "'" else None
    i += 1
    return i + 1 if i < len(source) and source[i] == "'" else None


def _literal_end(source, start):
    """The end of any literal beginning at ``start``, or None for code."""
    ch = source[start]
    if ch == '"':
        return _string_end(source, start)
    raw = _raw_string_end(source, start)
    if raw is not None:
        return raw
    if ch == 'b' and source.startswith('b"', start):
        return _string_end(source, start + 1)
    if ch == 'b' and source.startswith("b'", start):
        return _char_literal_end(source, start + 1)
    if ch == 'c' and source.startswith('c"', start):
        return _string_end(source, start + 1)
    if ch == "'":
        return _char_literal_end(source, start)
    return None


def squeeze(source):
    """Collapse whitespace *between tokens*, leaving literals and comments intact.

    A multi-line ``negative_assert!(`` call is the same call as its one-line
    form, so a check that binds the condition, its polarity and the message
    together has to look past rustfmt's line breaks. Whitespace inside a
    literal is data (module docstring), so literals are copied verbatim:
    collapsing them would let a one-space needle match a two-space message
    (#1401 review P2). A space that only separated a bracket from its content
    is dropped too, so a call reads the same whether rustfmt kept it on one
    line or spread it over five.

    Literal boundaries come from a lexical scan, not quote counting: a
    character literal such as ``'"'`` used to open a string, shifting every
    later quote and letting a two-space message satisfy a one-space needle
    (#1401 review P2 / #1989 item 4). Comments are scanned too, and dropped:
    they separate tokens but carry no code, so bytes a needle is looking for
    cannot hide inside a commented-out assertion (#1989 item 4).
    """
    out = []
    pending_space = False

    def flush(next_char):
        if pending_space and out and out[-1] not in '([' and next_char not in ')]':
            out.append(' ')

    i = 0
    while i < len(source):
        ch = source[i]
        if ch.isspace():
            pending_space = True
            i += 1
            continue
        comment = False
        if source.startswith('//', i):
            end, comment = _line_comment_end(source, i), True
        elif source.startswith('/*', i):
            end, comment = _block_comment_end(source, i), True
        else:
            end = _literal_end(source, i)
        if end is None:
            flush(ch)
            pending_space = False
            out.append(ch)
            i += 1
            continue
        if comment:
            # Dropped, but it still separates: `let x = 1; /* c */ let y = 2;`
            # must stay two statements, not become `let x = 1;let y = 2;`.
            pending_space = True
            i = end
            continue
        flush(ch)
        pending_space = False
        out.append(source[i:end])
        i = end
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
