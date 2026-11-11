# anchor-selfcheck: unsupported unit tests of the shared exact-source helper
"""Negative controls for the exact-literal preflight (#813 fix-r4).

Run from this directory: ``python3 -m unittest test_negative_source.py``.
The old preflight used ``' '.join(source.split())``, which also collapsed
whitespace *inside* the assertion message. A drifted literal therefore still
looked present. The helper may relax syntax whitespace only.
"""
import unittest

from negative_source import (assertion_marker_present, assertion_message_present,
                              declares_function, squeeze, test_body)

MESSAGE = 'gateway case filter must match at least one scenario'
SPLIT = f'''
fn filter_case() {{
    assert!(
        selected > 0,
        "{MESSAGE}"
    );
}}
'''
SINGLE = f'fn filter_case() {{ assert!(selected > 0, "{MESSAGE}"); }}\n'


class ExactLiteralPreflight(unittest.TestCase):
    def test_rustfmt_line_breaks_are_accepted(self):
        self.assertTrue(assertion_message_present(SPLIT, MESSAGE))

    def test_single_line_is_accepted(self):
        self.assertTrue(assertion_message_present(SINGLE, MESSAGE))

    def test_double_space_inside_the_literal_is_rejected(self):
        # This is the reviewer's drift: the old flattening saw "must match".
        drifted = SINGLE.replace('must match', 'must  match')
        self.assertFalse(assertion_message_present(drifted, MESSAGE))
        # ... while the old normalization would have accepted it.
        self.assertIn(f'assert!(selected > 0, "{MESSAGE}");',
                      ' '.join(drifted.split()))

    def test_newline_inside_the_literal_is_rejected(self):
        drifted = SINGLE.replace('must match', 'must\nmatch')
        self.assertFalse(assertion_message_present(drifted, MESSAGE))

    def test_missing_message_is_rejected(self):
        self.assertFalse(assertion_message_present(SINGLE, MESSAGE + ' (typo)'))

    def test_syntax_whitespace_does_not_hide_a_missing_assert(self):
        self.assertFalse(assertion_message_present('fn other() {}', MESSAGE))

    def test_a_line_commented_out_assertion_is_not_present(self):
        # #2102 P2-1: the raw-source regex accepted the whole line commented
        # out. A comment the compiler ignores is not the assertion, and
        # `squeeze` drops it before the match.
        commented = (
            'fn filter_case() {\n'
            f'    // assert!(selected > 0, "{MESSAGE}");\n'
            '}\n'
        )
        self.assertFalse(assertion_message_present(commented, MESSAGE))

    def test_a_block_commented_out_assertion_is_not_present(self):
        commented = (
            f'fn filter_case() {{ /* assert!(selected > 0, "{MESSAGE}"); */ }}\n'
        )
        self.assertFalse(assertion_message_present(commented, MESSAGE))


class TestBodySegmentation(unittest.TestCase):
    """#1401 review: a needle must be bound to the test that observes it."""

    SOURCE = '''
#[tokio::test(flavor = "multi_thread")]
async fn first() {
    assert!(a);
}

#[test]
fn second() {
    assert!(b);
}

fn helper_after_the_last_test() {
    assert!(a);
}
'''

    def test_body_stops_at_the_functions_own_brace(self):
        # Both tests mention ``assert!(a)``; a file-level check would pass
        # with the first test's observation deleted.
        body = test_body(self.SOURCE, 'first')
        self.assertIn('assert!(a);', body)
        self.assertNotIn('assert!(b);', body)

    def test_non_bare_test_attributes_do_not_widen_the_body(self):
        # `#[tokio::test(flavor = ...)]` must end the previous body: the old
        # `^#\\[(?:tokio::)?test\\]$` separator missed it.
        self.assertNotIn('assert!(b);', test_body(self.SOURCE, 'first'))

    def test_helper_after_the_last_test_is_not_part_of_it(self):
        self.assertNotIn('helper_after_the_last_test',
                         test_body(self.SOURCE, 'second'))

    def test_missing_or_unterminated_function_is_reported(self):
        with self.assertRaises(LookupError):
            test_body(self.SOURCE, 'absent')
        with self.assertRaises(LookupError):
            test_body('async fn open() {\n    let x = 1;\n', 'open')

    def test_every_shared_qualifier_word_resolves_the_head(self):
        # #2303 (条 2): ``const fn`` / ``unsafe fn`` / ``extern "C" fn`` /
        # ``pub(crate) fn`` all declare a test body; the old narrow pattern
        # only knew pub/async and stranded the rest as LookupError.
        for head in ('const fn', 'unsafe fn', 'extern "C" fn',
                     'pub(crate) fn', 'default fn'):
            with self.subTest(head=head):
                source = f'{head} scoped() {{\n    assert!(a);\n}}\n'
                self.assertIn('assert!(a);', test_body(source, 'scoped'))

    def test_a_block_comment_fn_head_is_not_a_declaration(self):
        # #2303 (条 8): a line-start ``fn name()`` inside a block comment (or
        # a string literal) is not the function the body is cut from; the head
        # must start in code, per the #2064 r10 carrier rule.
        commented = (
            '/* a commented-out decoy:\nfn first() {\n    assert!(decoy);\n}\n*/\n'
            'fn first() {\n    assert!(a);\n}\n'
        )
        body = test_body(commented, 'first')
        self.assertIn('assert!(a);', body,
                      '#2303 the body must come from the head in code')
        self.assertNotIn('decoy', body,
                         '#2303 a block-comment fn head must not be taken as the declaration')
        carried = 'let decoy = "\nfn first() {\n    assert!(decoy);\n}\n";\nfn first() {\n    assert!(a);\n}\n'
        self.assertNotIn('decoy', test_body(carried, 'first'),
                         '#2303 a string-literal fn head must not be taken as the declaration')


class SqueezeKeepsLiteralsAndCallShapes(unittest.TestCase):
    """#1401 review P2: bind a whole ``negative_assert!`` call, not a fragment.

    The needle says which assertion it bound (head, condition, polarity and
    message), so a call rewritten as a plain tuple no longer satisfies it.
    """

    def test_a_multiline_call_reads_as_its_one_line_form(self):
        spread = (
            '        negative_assert!(\n'
            '            !leaked_parse,\n'
            '            "#1261 native entry scrubs inherited X"\n'
            '        );\n'
        )
        self.assertIn(
            'negative_assert!(!leaked_parse, "#1261 native entry scrubs '
            'inherited X");',
            squeeze(spread))

    def test_whitespace_inside_a_literal_is_kept(self):
        # Collapsing the literal would let a one-space needle match a
        # two-space message.
        squeezed = squeeze('assert!(a, "must  match");')
        self.assertIn('"must  match"', squeezed)
        self.assertNotIn('"must match"', squeezed)

    def test_a_rewritten_tuple_is_not_the_assertion(self):
        rewritten = squeeze('let _ = (!leaked_parse, "message");')
        self.assertNotIn('negative_assert!(', rewritten)

    def test_an_escaped_quote_does_not_end_the_literal(self):
        squeezed = squeeze(r'assert!(a, "a \"quoted\" word");')
        self.assertIn(r'"a \"quoted\" word"', squeezed)


class SqueezeIsLexicalNotQuoteCounting(unittest.TestCase):
    """#1989 item 4: literal boundaries come from a lexical scan.

    Counting ``"`` characters made every literal after an unbalanced quote read
    as string *content*, so the two-space message in the #1401 review collision
    was folded into a one-space one and satisfied a one-space needle. Each test
    here is a construct that carries a quote inside a non-string literal or
    inside a comment; the message that follows must keep its bytes.
    """

    DRIFTED = 'assert!(advertised, "#1261  native entry rejects inherited X");'

    def assertMessageKeepsItsBytes(self, source):
        squeezed = squeeze(source)
        self.assertIn('"#1261  native entry rejects inherited X"', squeezed)
        self.assertNotIn('"#1261 native entry rejects inherited X"', squeezed)

    def test_a_character_literal_holding_a_quote(self):
        # The collision from the issue body, end to end.
        self.assertMessageKeepsItsBytes('let c = \'"\';\n' + self.DRIFTED)

    def test_a_character_literal_escape(self):
        self.assertMessageKeepsItsBytes("let c = '\\\"';\n" + self.DRIFTED)

    def test_a_byte_character_literal(self):
        self.assertMessageKeepsItsBytes("let c = b'\"';\n" + self.DRIFTED)

    def test_a_lifetime_is_code_not_a_character_literal(self):
        # `'a` has no closing quote; a scanner that assumes one would eat the
        # following code up to the next apostrophe.
        self.assertMessageKeepsItsBytes(
            "fn f<'a>(x: &'a str) -> &'a str { x }\n" + self.DRIFTED)

    def test_a_raw_string_holding_a_quote(self):
        self.assertMessageKeepsItsBytes('let s = r#"a " b"#;\n' + self.DRIFTED)

    def test_a_raw_identifier_is_not_a_raw_string(self):
        squeezed = squeeze('let r#type = 1;\nlet x = "a  b";')
        self.assertIn('r#type', squeezed)
        self.assertIn('"a  b"', squeezed)

    def test_a_quote_inside_a_line_comment(self):
        self.assertMessageKeepsItsBytes('// an odd " quote\n' + self.DRIFTED)

    def test_a_quote_inside_a_nested_block_comment(self):
        self.assertMessageKeepsItsBytes(
            '/* outer /* inner " */ still " comment */\n' + self.DRIFTED)

    def test_comment_markers_inside_a_string_are_content(self):
        squeezed = squeeze('let s = "// not a comment /* nor this */";\nlet x = 1;')
        self.assertIn('"// not a comment /* nor this */"', squeezed)
        self.assertIn('let x = 1;', squeezed)

    def test_a_commented_out_assertion_is_not_the_assertion(self):
        # The needle must not be satisfiable by a line the compiler ignores.
        squeezed = squeeze(
            '// assert!(advertised, "#1261 native entry rejects inherited X");\n'
            'let x = 1;')
        self.assertNotIn('assert!(advertised', squeezed)

    def test_a_block_comment_still_separates_tokens(self):
        self.assertEqual('let x = 1; let y = 2;',
                         squeeze('let x = 1; /* c */ let y = 2;'))

    def test_syntax_whitespace_still_collapses(self):
        spread = 'assert!(\n        !x,\n        "msg"\n    );'
        self.assertEqual('assert!(!x, "msg");', squeeze(spread))


class SqueezeRefusesIllegalPrefixShapes(unittest.TestCase):
    """#2303 (条 13): the #2064 r4 refusal rules apply to ``squeeze`` too.

    The two lexers converged, so a raw hash delimiter without a legal raw
    prefix (``c#"…"#`` / ``b#"…"#``) and an unknown multi-letter prefix
    (``rb#"…"#``) raise here as well, instead of the old behaviour: the
    mis-lexed ``#`` became code, the inner quote opened a bogus plain string
    and the carried bytes returned to the code view.
    """

    def test_a_hash_delimiter_without_a_raw_prefix_is_refused(self):
        with self.assertRaises(ValueError,
                                msg='#2303 squeeze must refuse c#"…"#'):
            squeeze('let s = c#"x"fn case_a(){"#;')
        with self.assertRaises(ValueError,
                                msg='#2303 squeeze must refuse b#"…"#'):
            squeeze('let s = b#"x"fn case_a(){"#;')

    def test_an_unknown_multi_letter_prefix_is_refused(self):
        with self.assertRaises(ValueError,
                                msg='#2303 squeeze must refuse rb#"…"#'):
            squeeze('let s = rb#"x"fn case_a(){"y"#;')

    def test_legal_prefixed_raw_literals_keep_their_bytes(self):
        # The positive side: every legal prefix shape stays one literal,
        # bytes verbatim, so a needle inside it is not folded by syntax
        # whitespace collapsing.
        for literal, source in (
                ('br#"must  match"#', 'let s = br#"must  match"#;'),
                ('cr#"must  match"#', 'let s = cr#"must  match"#;'),
                ('r#"must  match"#', 'let s = r#"must  match"#;'),
                ('b"must  match"', 'let s = b"must  match";'),
                ('c"must  match"', 'let s = c"must  match";')):
            with self.subTest(literal=literal):
                self.assertIn(literal, squeeze(source))


class DeclaresFunctionTests(unittest.TestCase):
    """#2303 (条 3): the fn-form-only entry behind ``declares_test``."""

    def test_the_fn_form_is_accepted_and_pinned(self):
        self.assertTrue(declares_function('async fn case_a() {', 'case_a'),
                        '#2303 the fn form must satisfy declares_function')
        self.assertTrue(declares_function('pub fn case_a() {', 'case_a'),
                        '#2303 a pub fn declaration must satisfy declares_function')

    def test_the_table_and_macro_forms_do_not_satisfy_it(self):
        self.assertFalse(declares_function('matrix_tests! { case_a => "row" }', 'case_a'),
                         '#2303 a table row must not satisfy the fn-only form')
        self.assertFalse(declares_function('case!(case_a,);', 'case_a'),
                         '#2303 a macro row must not satisfy the fn-only form')


class AssertionMarkerTests(unittest.TestCase):
    """#2303 (条 5): the shared comment-proof marker matcher for binding audits."""

    MARKER = '"#749 close budget covers accepted bytes"'

    def test_a_string_literal_carries_the_marker(self):
        carried = 'fn close_budget() {\n    assert!(kept, "#749 close budget covers accepted bytes");\n}\n'
        self.assertTrue(assertion_marker_present(carried, self.MARKER),
                        '#2303 a string literal must carry the marker')

    def test_a_line_comment_carrier_does_not_satisfy_the_binding(self):
        # Delete the assertion, keep a commented-out copy of it: the raw-text
        # scan this matcher replaced stayed green (the comment carrier).
        commented = ('fn close_budget() {}\n'
                     '// assert!(kept, "#749 close budget covers accepted bytes");\n')
        self.assertFalse(assertion_marker_present(commented, self.MARKER),
                         '#2303 a line comment must not carry the assertion marker')

    def test_a_block_comment_carrier_does_not_satisfy_the_binding(self):
        commented = ('fn close_budget() {}\n'
                     '/* assert!(kept, "#749 close budget covers accepted bytes"); */\n')
        self.assertFalse(assertion_marker_present(commented, self.MARKER),
                         '#2303 a block comment must not carry the assertion marker')

    def test_a_marker_missing_entirely_is_rejected(self):
        self.assertFalse(assertion_marker_present('fn close_budget() {}\n', self.MARKER),
                        '#2303 a missing marker must be refused')

    def test_a_marker_split_by_a_format_placeholder_is_not_the_quoted_needle(self):
        # The quoted needle demands the marker as one contiguous literal; a
        # formatted message broken across `{...}` pieces does not satisfy it.
        formatted = 'fn close_budget() {\n    assert!(kept, "#749 {}");\n}\n'
        self.assertFalse(assertion_marker_present(formatted, self.MARKER),
                         '#2303 a formatted message is not the quoted marker needle')

    def test_the_fail_closed_scan_rules_apply(self):
        with self.assertRaises(ValueError, msg='#2303 an unterminated literal must refuse the scan'):
            assertion_marker_present('let s = "unterminated\n', self.MARKER)


if __name__ == '__main__':
    unittest.main()
