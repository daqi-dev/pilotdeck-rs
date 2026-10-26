"""Check the new negative runner without modifying any product source."""
import ast
import contextlib
import io
from pathlib import Path
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

here = Path(__file__).resolve().parent
source = here.parents[2] / 'src/runtime/memory/edgeclaw/llm/transport.rs'
reader = here.parents[2] / 'src/fetch_redirect.rs'
module = ast.parse((here / 'negative.py').read_text())
parts = [node for node in module.body if
         isinstance(node, ast.FunctionDef) and node.name in {'anchors', 'run', 'classify'} or
         isinstance(node, ast.Assign) and any(isinstance(t, ast.Name) and t.id in {'cases', 'messages'} for t in node.targets)]
namespace = {'source': source, 'reader': reader, 'sys': sys, 'os': __import__('os'), 're': __import__('re')}
exec(compile(ast.Module(body=parts, type_ignores=[]), str(here / 'negative.py'), 'exec'), namespace)


class Controls(unittest.TestCase):
    def test_anchor_positive(self):
        namespace['anchors'](source.read_text())

    def test_stale_and_duplicate_anchors_exit_2(self):
        originals = {path: path.read_text() for path in (source, reader)}
        for name, path, before, _, _ in namespace['cases']:
            original = originals[path]
            for changed in [original.replace(before, 'removed-anchor', 1), original + before]:
                contents = dict(originals)
                contents[path] = changed
                with self.subTest(name=name, changed=len(changed)), \
                     patch.object(Path, 'read_text', autospec=True, side_effect=contents.__getitem__), \
                     contextlib.redirect_stderr(io.StringIO()) as output:
                    with self.assertRaises(SystemExit) as error:
                        namespace['anchors'](contents[source])
                    self.assertEqual(error.exception.code, 2)
                    self.assertIn(f'STALE ANCHOR {name}:', output.getvalue())
                    self.assertNotIn('SURVIVED', output.getvalue())

    def test_classification_requires_named_assertion(self):
        namespace.update(root=here, command=['cargo'], wait_for_slot=lambda _: SimpleNamespace(stdout='', stderr=''))
        positive = 'running 1 test\ntest wire_matrix ... ok\ntest result: ok. 1 passed; 0 failed;'
        killed = "running 1 test\ntest wire_matrix ... FAILED\n---- wire_matrix stdout ----\nthread 'wire_matrix' (123) panicked at file.rs:1:2:\nassertion `left == right` failed: expected marker\n left: 1\nright: 2\n\nfailures:\n    wire_matrix\ntest result: FAILED. 0 passed; 1 failed;"
        cases = [
            (0, positive, 'PASS'), (101, killed, 'KILLED'),
            (101, 'running 1 test\ntest result: FAILED.', 'INVALID'),
            (101, killed.replace('wire_matrix', 'unrelated'), 'INVALID'),
            (101, killed.replace('expected marker', 'loopback listen: Permission denied'), 'INVALID'),
            (101, killed.replace("thread 'wire_matrix'", "thread 'unrelated'"), 'INVALID'),
            (101, killed.replace('expected marker', 'wrong assertion').replace('left: 1', 'left: expected marker'), 'INVALID'),
            (101, 'could not compile\n' + killed, 'INVALID'),
            (101, killed.replace('running 1 test', 'running 0 tests'), 'INVALID'),
            (0, positive.replace('wire_matrix', 'unrelated'), 'INVALID'),
            (-9, killed, 'INVALID'), (1, killed, 'INVALID'),
        ]
        with tempfile.TemporaryDirectory(prefix='pdrs-llm-negative-check-') as tmp:
            namespace['out'] = Path(tmp)
            for index, (status, log, expected) in enumerate(cases):
                namespace['run_owned'] = lambda *_, status=status, log=log, **__: SimpleNamespace(returncode=status, stdout=log, stderr='')
                with self.subTest(index=index):
                    self.assertEqual(namespace['run'](str(index), 'wire_matrix', 'expected marker'), expected)


if __name__ == '__main__':
    unittest.main()
