# anchor-selfcheck: unsupported unit tests of the shared exact-source helper
"""Negative controls for the exact-literal preflight (#813 fix-r4).

Run from this directory: ``python3 -m unittest test_negative_source.py``.
The old preflight used ``' '.join(source.split())``, which also collapsed
whitespace *inside* the assertion message. A drifted literal therefore still
looked present. The helper may relax syntax whitespace only.
"""
import unittest

from negative_source import assertion_message_present, squeeze, test_body

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


if __name__ == '__main__':
    unittest.main()
