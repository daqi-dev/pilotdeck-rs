# anchor-selfcheck: unsupported unit tests of the shared exact-source helper
"""Negative controls for the exact-literal preflight (#813 fix-r4).

Run from this directory: ``python3 -m unittest test_negative_source.py``.
The old preflight used ``' '.join(source.split())``, which also collapsed
whitespace *inside* the assertion message. A drifted literal therefore still
looked present. The helper may relax syntax whitespace only.
"""
import unittest

from negative_source import assertion_message_present, test_body

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


if __name__ == '__main__':
    unittest.main()
