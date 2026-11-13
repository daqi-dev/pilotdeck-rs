# anchor-selfcheck: unsupported shared Rust binding-scan views, no source replacement table
"""One definition of the comment/literal views the binding checkers read (#1642).

native-management and native-registry-switch each carried a copy of the
comment stripper, and the literal half drifted: a scanner that keeps string
literals in the text a *candidate* scan reads lets a ``#[test] fn`` that only
exists inside a raw string stand in for a deleted bound test (a false PASS
for ``--check``). Both now read their views from here, so the next rule
change lands in exactly one place.

:func:`scanned_views` returns two same-length texts from a single pass:

``kept``
    comments blanked, string/char literals verbatim. This is the text an
    *assertion* is anchored in -- the assertion lives in a ``panic!`` literal
    payload, so blanking literals here would make every binding stale.
``structure``
    comments and literals both blanked (a literal becomes a same-length run
    of spaces). This is the text a *candidate* scan reads -- function
    heads, ``#[test]`` attributes and call edges must come from real code,
    never from prose inside a literal.

The two texts share every offset, so a match found in ``structure`` slices
the corresponding literal-bearing span out of ``kept``. The string/char
recognition (raw and C-string prefixes, hash delimiters) is
``negative_suite``'s own, and :func:`verify_structure_matches_shared_scanner`
proves the structure view cannot drift from ``negative_suite._rust_code_only``
(anything the scan cannot close raises, per the fail-closed scanning rule,
#449 / #1166).
"""
import re

from negative_suite import _CHAR_START, _STRING_START, _token_start, _rust_code_only

# #1642 fix-r4 (review P1-C/P1-E): a candidate must be a test function, so a
# deleted bound test cannot be stood in for by a same-named plain helper or by
# prose inside a string literal. `#[test]` / `#[tokio::test]` (with or without
# an argument list) are the two spellings the bound tests use; the check runs
# on the structure view, where a literal-carried `#[test]` is blank.
TEST_ATTRIBUTE = re.compile(r'#\[[ \t]*(?:tokio::)?test[ \t]*[\](]')


def scanned_views(text):
    """``(kept, structure)``: comments blanked in both, literals kept only in ``kept``.

    The two outputs have the same length and share every offset. Comments
    become one space; a string/char literal appears verbatim in ``kept`` and as
    a same-length blank run in ``structure``. Unterminated comments, strings
    and raw strings raise instead of guessing (fail-closed, #449 / #1166).
    """
    kept, structure = [], []
    i, length = 0, len(text)
    while i < length:
        if text.startswith('//', i):
            end = text.find('\n', i)
            i = length if end < 0 else end
            kept.append(' ')
            structure.append(' ')
            continue
        if text.startswith('/*', i):
            depth, i = 1, i + 2
            while depth:
                if text.startswith('/*', i):
                    depth, i = depth + 1, i + 2
                elif text.startswith('*/', i):
                    depth, i = depth - 1, i + 2
                elif i >= length:
                    raise ValueError('unterminated Rust block comment')
                else:
                    i += 1
            kept.append(' ')
            structure.append(' ')
            continue
        start = None
        if text[i] == '"' or (text[i] in 'rbc' and _token_start(text, i)):
            start = _STRING_START.match(text, i)
        if start is not None:
            hashes = start.group(1) or ''
            i = start.end()
            if hashes and 'r' not in start.group(0):
                raise ValueError('invalid Rust string prefix with raw hash delimiter')
            if 'r' in start.group(0):
                close = text.find('"' + hashes, i)
                if close < 0:
                    raise ValueError('unterminated Rust raw string')
                i = close + 1 + len(hashes)
            else:
                # A literal newline is legal inside a Rust string, so only
                # the end of the source can prove the literal unterminated.
                while True:
                    if i >= length:
                        raise ValueError('unterminated Rust string')
                    if text[i] == '"':
                        i += 1
                        break
                    i += 2 if text[i] == '\\' else 1
            literal = text[start.start():i]
            kept.append(literal)
            structure.append(' ' * len(literal))
            continue
        char = None
        if text[i] == "'" or (text[i] == 'b' and _token_start(text, i)):
            char = _CHAR_START.match(text, i)
        if char is not None:
            i = char.end()
            literal = text[char.start():i]
            kept.append(literal)
            structure.append(' ' * len(literal))
            continue
        kept.append(text[i])
        structure.append(text[i])
        i += 1
    return ''.join(kept), ''.join(structure)


def carries_test_attribute(structure, start):
    """True when the `fn` at ``start`` is preceded by a `#[test]`-family line.

    Run on the *structure* view: attributes and blank lines walk back from the
    `fn` (comments are already blank, so `#[cfg(unix)]` / `#[ignore = "..."]`
    between the test attribute and the `fn` keep counting); the first other
    line stops the walk. A `#[test]` inside a string literal is blanked here,
    so it cannot mark a plain function as a test.
    """
    for line in reversed(structure[:start].split('\n')):
        if not line.strip():
            continue
        if not line.lstrip().startswith('#['):
            return False
        if TEST_ATTRIBUTE.match(line.lstrip()):
            return True
    return False


def verify_structure_matches_shared_scanner(text):
    """Fail loudly if the structure view drifted from ``negative_suite``.

    ``_rust_code_only`` blanks each comment/literal to one space while the
    structure view blanks literals to a same-length run, so the two agree
    exactly after whitespace is collapsed. This is the read-only reuse that
    keeps the local two-view scanner honest: a future edit that stops
    recognising a literal shape makes this raise instead of quietly returning
    carried text to the candidate scan.
    """
    _kept, structure = scanned_views(text)
    if ''.join(structure.split()) != ''.join(_rust_code_only(text).split()):
        raise ValueError('#1642 structure view drifted from negative_suite._rust_code_only')
