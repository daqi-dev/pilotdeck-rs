"""Check the new negative runner without modifying any product source."""
import ast
import contextlib
import io
from pathlib import Path
import sys
import tempfile
from types import SimpleNamespace
import unittest

here = Path(__file__).resolve().parent
source = here.parents[2] / 'src/runtime/memory/edgeclaw/llm/transport.rs'
module = ast.parse((here / 'negative.py').read_text())
parts = [node for node in module.body if
         isinstance(node, ast.FunctionDef) and node.name in {'anchors', 'run'} or
         isinstance(node, ast.Assign) and any(isinstance(t, ast.Name) and t.id == 'cases' for t in node.targets)]
namespace = {'sys': sys, 'os': __import__('os')}
exec(compile(ast.Module(body=parts, type_ignores=[]), str(here / 'negative.py'), 'exec'), namespace)


class Controls(unittest.TestCase):
    def test_anchor_positive(self):
        namespace['anchors'](source.read_text())

    def test_stale_and_duplicate_anchors_exit_2(self):
        original = source.read_text()
        before = namespace['cases'][0][1]
        for changed in [original.replace(before, 'removed-anchor', 1), original + before]:
            with self.subTest(changed=len(changed)), contextlib.redirect_stderr(io.StringIO()) as output:
                with self.assertRaises(SystemExit) as error:
                    namespace['anchors'](changed)
                self.assertEqual(error.exception.code, 2)
                self.assertIn('STALE ANCHOR', output.getvalue())
                self.assertNotIn('SURVIVED', output.getvalue())

    def test_classification_requires_runtime_test(self):
        namespace.update(root=here, command=['cargo'], wait_for_slot=lambda _: SimpleNamespace(stdout='', stderr=''))
        cases = [
            (0, 'running 1 test\ntest result: ok.', 'PASS'),
            (101, 'running 1 test\ntest result: FAILED.', 'KILLED'),
            (101, 'could not compile\nrunning 1 test\ntest result: FAILED.', 'INVALID'),
            (1, 'running 0 tests\ntest result: FAILED.', 'INVALID'),
            (0, 'running 0 tests\ntest result: ok.', 'INVALID'),
        ]
        with tempfile.TemporaryDirectory(prefix='pdrs-llm-negative-check-') as tmp:
            namespace['out'] = Path(tmp)
            for index, (status, log, expected) in enumerate(cases):
                namespace['run_owned'] = lambda *_, status=status, log=log, **__: SimpleNamespace(returncode=status, stdout=log, stderr='')
                self.assertEqual(namespace['run'](str(index), 'wire_matrix'), expected)


if __name__ == '__main__':
    unittest.main()
