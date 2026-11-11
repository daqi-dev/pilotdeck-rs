# anchor-selfcheck: unsupported shared exact-source assertion helper, no source replacement table
"""Exact-source checks shared by negative-control runners (#813 fix-r4).

rustfmt may split a long ``assert!(...)`` across lines, so a preflight that
compares the whole file must ignore whitespace *between tokens*. Whitespace
inside a string literal is data, though: changing ``must match`` to
``must  match`` changes the assertion message even though ``str.split()`` sees
the same tokens.

#2303 (条 1/20): the two hand-rolled Rust lexers converged here. The single
span scanner below carries the #2064 refusal rules (comments and literals
blanked for the declaration view, unterminated shapes fail closed, hash
delimiters without a legal raw prefix refused); ``negative_suite`` forwards
to it, and ``squeeze`` keeps literal bytes verbatim while dropping comments.
"""
import re

# #2303 (条 2): the function-qualifier words every fn-head matcher accepts.
# One shared list, so ``const fn`` / ``unsafe fn`` / ``extern "C" fn`` cannot
# be found by one matcher and missed by another (the pre-convergence state:
# four sites kept narrow pub/async-only copies). ``default`` is kept for the
# specialized-default spelling ``default fn`` (negative_suite r10口径).
FN_QUALIFIERS = ('async', 'const', 'unsafe', 'extern', 'pub', 'default')


def fn_qualifier_run(sep):
    """A regex for one function qualifier, e.g. ``pub`` / ``unsafe`` / ``extern "C"``.

    ``sep`` is the separator required *after* the qualifier word -- ``\\s+``
    for whole-file code views, ``[ \\t]+`` for line-anchored matchers whose
    spans must not cross a newline. The parenthesised visibility form
    (``pub(crate)``) and the quoted ABI (``extern "C"``) are qualifier-shaped
    too; a plain word boundary alone would not accept them.
    """
    words = '|'.join(FN_QUALIFIERS)
    return rf'(?:(?:{words}){sep}|pub\([^)]*\){sep}|extern{sep}"[^"]*"{sep})'


# The fn form's left boundary (#2064 r10 + #2303 条 19): ``fn`` must stand as
# its own word -- a non-word in front of it (so ``asyncfn case_a(``, one
# identifier whose qualifier is glued onto ``fn``, is refused) and real
# whitespace behind it (so ``fncase_a(`` is a longer identifier, not a
# declaration). The qualifier run lives where it is load-bearing: the
# line-anchored heads (``test_body`` below and the method-head matchers in the
# fixture runners), where ``^`` makes ``async fn first`` miss a bare ``fn``.
FN_LEFT = r'(?<!\w)fn\s+'

# The table form's left boundary (#2303 条 9): a row must be preceded by a
# table separator (``{``, ``,`` or a newline, modulo spaces/tabs). A bare
# word boundary let a match-arm binding pattern (``Ok(case_a) =>``) stand in
# for a declared row.
TABLE_LEFT = r'(?<=[{,\n])[ \t]*'

# #2064 r11 + #2303 条 15: the string-prefix table. ``c``/``cr`` are legal
# Rust C-string prefixes and ``b``/``br`` byte-string prefixes; ``rb`` is NOT
# a legal prefix (legal raw prefixes are exactly ``r``/``br``/``cr``), so it
# was removed -- it used to borrow the ``r`` and pass the raw-string gate.
_STRING_START = re.compile(r'(cr|c|br|r|b)?(#*)"')
# #2064 r11: ``'\u{…}'`` / ``'\xNN'`` are single-char literals whose escape
# bodies the old pattern could not close, so their quotes leaked into the
# code view and could pair into bogus ``'x'`` matches eating neighbouring code.
_CHAR_START = re.compile(r"b?'(?:\\u\{[0-9A-Fa-f_]+\}|\\x[0-9A-Fa-f]{2}|\\.|[^\\'])'")
_WORD = re.compile(r'\w')

# Legal raw-string prefixes, exactly (#2303 条 15): ``rb#"` used to pass the
# old ``'r' in prefix`` judgment and be blanked as a raw string.
_RAW_PREFIXES = ('cr', 'br', 'r')


def _token_start(source, index):
    """True when ``index`` is not glued to the tail of an identifier."""
    return index == 0 or not _WORD.match(source[index - 1])


def _lex(source):
    """One lexical pass: [(kind, start, end)] covering the whole source.

    ``kind`` is ``'code'``, ``'comment'`` or ``'literal'``; the spans are
    ordered and contiguous. The scan knows only the lexical shapes Rust
    itself guarantees; anything it cannot close -- an unterminated string,
    raw string or block comment -- raises instead of guessing, per the
    fail-closed scanning rule (#449 / #1166: a hand-rolled guess that
    mis-lexes is worse than refusing). An unterminated char literal is *not*
    a raise: it may be a lifetime or a loop label, so it stays code.

    #2064 r4 + #2303 条 15: a raw hash delimiter with no legal raw prefix
    (``c#"…"#`` / ``b#"…"#``) is not legal Rust -- rustc 1.99.0 rejects it
    with ``error: prefix `c` is unknown`` -- so it is refused instead of
    guessing a plain string whose inner quote would return the carried text
    to the code view. A ``#`` that *does* reach code position followed
    directly by ``"`` and preceded by a word character (``cb#"`` / ``rc#"``
    / ``cc#"`` / ``rb#"``, the multi-letter prefixes the table above cannot
    match) is refused the same way: legal Rust only ever writes ``#`` as
    ``#[``/``#!`` or inside a raw-string/raw-identifier prefix, so this
    shape is unreachable in compilable source (rustc: unknown prefix).
    """
    spans = []
    code_start = 0
    i, length = 0, len(source)

    def emit(kind, start, end):
        nonlocal code_start
        if start > code_start:
            spans.append(('code', code_start, start))
        spans.append((kind, start, end))
        code_start = end

    while i < length:
        if source.startswith('//', i):
            end = source.find('\n', i)
            emit('comment', i, length if end < 0 else end)
            i = length if end < 0 else end
            continue
        if source.startswith('/*', i):
            depth, j = 1, i + 2
            while depth:
                if source.startswith('/*', j):
                    depth, j = depth + 1, j + 2
                elif source.startswith('*/', j):
                    depth, j = depth - 1, j + 2
                elif j >= length:
                    raise ValueError('unterminated Rust block comment')
                else:
                    j += 1
            emit('comment', i, j)
            i = j
            continue
        if (source[i] == '#' and i + 1 < length and source[i + 1] == '"'
                and i and _WORD.match(source[i - 1])):
            raise ValueError('unknown Rust string prefix before a raw hash delimiter')
        start = None
        if source[i] == '"' or (source[i] in 'rbc' and _token_start(source, i)):
            start = _STRING_START.match(source, i)
        if start is not None:
            prefix, hashes = start.group(1) or '', start.group(2) or ''
            j = start.end()
            if hashes and prefix not in _RAW_PREFIXES:
                raise ValueError('invalid Rust string prefix with raw hash delimiter')
            if prefix in _RAW_PREFIXES:
                close = source.find('"' + hashes, j)
                if close < 0:
                    raise ValueError('unterminated Rust raw string')
                j = close + 1 + len(hashes)
            else:
                # A literal newline is legal inside a Rust string, so only
                # the end of the source can prove the literal unterminated.
                while True:
                    if j >= length:
                        raise ValueError('unterminated Rust string')
                    if source[j] == '"':
                        j += 1
                        break
                    j += 2 if source[j] == '\\' else 1
            emit('literal', i, j)
            i = j
            continue
        char = None
        if source[i] == "'" or (source[i] == 'b' and _token_start(source, i)):
            char = _CHAR_START.match(source, i)
        if char is not None:
            emit('literal', i, char.end())
            i = char.end()
            continue
        i += 1
    if code_start < length:
        spans.append(('code', code_start, length))
    return spans


def rust_code_only(source):
    """Return ``source`` with every comment and literal replaced by a space.

    #2064 r10: a binding that a comment or a string literal satisfies is a
    binding that audits nothing (``// fn case_a(`` passed the old matcher), so
    the noise carriers are blanked before any form is matched. The fail-closed
    rules of :func:`_lex` apply. Whitespace is preserved: the fn form's
    qualifier boundary needs it (#2303 条 19), and the table form's separator
    (#2303 条 9) reads the newline a row starts on.
    """
    return ''.join(source[start:end] if kind == 'code' else ' '
                   for kind, start, end in _lex(source))


def squeeze(source):
    """Collapse whitespace *between tokens*, leaving literals and comments' role intact.

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

    #2303 (条 1/13): the scan is the shared one (``_lex``), so the #2064
    refusal rules apply here too -- an unterminated literal, a raw hash
    delimiter without a legal raw prefix (``c#"…"#``) or an unknown
    multi-letter prefix (``rb#"…"#``) raises instead of being guessed into
    a plain string that returns the carried bytes to the code view.
    """
    out = []
    state = {'pending_space': False}

    def flush(next_char):
        if state['pending_space'] and out and out[-1] not in '([' and next_char not in ')]':
            out.append(' ')
        state['pending_space'] = False

    for kind, start, end in _lex(source):
        if kind == 'code':
            for ch in source[start:end]:
                if ch.isspace():
                    state['pending_space'] = True
                else:
                    flush(ch)
                    out.append(ch)
        elif kind == 'literal':
            flush(source[start])
            out.append(source[start:end])
        else:
            # Dropped, but it still separates: `let x = 1; /* c */ let y = 2;`
            # must stay two statements, not become `let x = 1;let y = 2;`.
            state['pending_space'] = True
    return ''.join(out)


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


def assertion_marker_present(source, marker):
    """A stale-binding marker must live in code, never inside a comment.

    The binding audits this backs used to scan the raw text, so a marker
    quoted inside a comment -- a line the compiler ignores -- kept the
    binding green after the real assertion was deleted (#2303 条 5: the
    comment carrier). The marker is matched against :func:`squeeze` instead:
    comments are dropped while literal bytes stay verbatim, so the marker
    must be carried by code or by an actual string literal. The fail-closed
    rules of ``_lex`` apply unchanged -- an unterminated literal or an
    illegal raw hash prefix raises instead of scanning past it.
    """
    return marker in squeeze(source)


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

    #2303 (条 2): the head accepts every shared qualifier word (``const fn``
    / ``unsafe fn`` / ``extern "C" fn`` / ``pub(crate) fn``), from the same
    list the other matchers use. #2303 (条 8): a line-start ``fn name()`` that
    lives inside a block comment or a string literal is not a declaration --
    the head must start in code, per the #2064 r10 carrier rule.
    """
    head = re.compile(r'^(?:' + fn_qualifier_run(r'\s+') + r'*)fn\s+'
                      + re.escape(name) + r'\s*[<(]', re.M)
    noise = [(start, end) for kind, start, end in _lex(source) if kind != 'code']
    found = None
    for match in head.finditer(source):
        if not any(start <= match.start() < end for start, end in noise):
            found = match
            break
    if found is None:
        raise LookupError(f'{name}: no such function')
    rest = source[found.end():]
    end = re.search(r'^(?:#\[(?:[\w:]+::)?test\b|\})', rest, re.M)
    if end is None:
        raise LookupError(f'{name}: unterminated function')
    return rest[:end.start()]


def declares_function(source, short):
    """True when ``source`` declares ``short`` as a function (the fn form only).

    #2303 (条 3): ``declares_test`` also accepts the table and macro forms, so
    a caller that means a real ``fn`` declaration (the panic-marker audit)
    pins the fn form through this entry instead of being satisfied by a
    ``name =>`` row.
    """
    if not short:
        raise ValueError('declares_function needs a short name')
    return re.search(FN_LEFT + re.escape(short) + r'\s*\(',
                     rust_code_only(source)) is not None
