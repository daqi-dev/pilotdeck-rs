#!/usr/bin/env python3
# anchor-selfcheck: unsupported in-memory unittest coverage; no source replacements
"""#1190 read-only adapter, pure gates and evidence initialization regressions."""
import contextlib
import io
import inspect
import json
import os
from pathlib import Path
import stat
import sys
import types
import unittest
from unittest.mock import MagicMock, Mock, patch

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parent))
import meta_controls as meta
import negative_controls as gate


class DriftTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.raw = gate.read_frozen()
        cls.baseline = meta.load_inputs(cls.raw)

    def test_pure_checks_do_not_read_files(self):
        with patch.object(os, 'open', side_effect=AssertionError('#1190 pure: os.open')), \
                patch.object(Path, 'read_text', side_effect=AssertionError('#1190 pure: read_text')):
            self.assertFalse(self.baseline.errors(), '#1190 pure: real inputs must pass')
            rows = meta.run_scenarios(self.baseline, meta.SCENARIOS)
        self.assertTrue(all(row['status'] == 'KILLED' for row in rows),
                        '#1190 pure: in-memory scenarios must all be killed')

    def test_scan_globs_have_one_source(self):
        sample = {'20261101-1190-extra.md': 'canonical require', 'ignored.md': ''}
        self.assertTrue(set(gate.doc_counts({}, sample)) == {'20261101-1190-extra.md'},
                        '#1190 globs: event must be selected')
        with patch.object(gate, 'SCAN_GLOBS', ('ignored.md',)):
            self.assertTrue(set(gate.doc_counts({}, sample)) == {'ignored.md'},
                            '#1190 globs: selection must use the shared constant')
            self.assertTrue('frozen event 20261101-1190-extra.md matches no doc scan glob'
                            in gate.check_scan_set(sample),
                            '#1190 globs: reverse coverage must use the same selection')

    def test_all_rows_run_after_invalid_or_survived(self):
        with patch.object(meta, 'check_scenario', side_effect=[
                ValueError('unrelated'), AssertionError('#1190 empty-state-bound: expected drift: missing'),
                'specific drift', AssertionError('unrelated panic')]):
            rows = meta.run_scenarios(self.baseline, meta.SCENARIOS[:4])
        self.assertTrue([row['status'] for row in rows] == ['INVALID', 'SURVIVED', 'KILLED', 'INVALID'],
                        '#1190 aggregate: every selected row must run')

    def test_real_frozen_is_unchanged(self):
        before = gate.read_frozen()
        self.assertTrue(all(row['status'] == 'KILLED' for row in
                            meta.run_scenarios(self.baseline, meta.SCENARIOS)),
                        '#1190 unchanged: all scenarios must finish')
        self.assertTrue(gate.read_frozen() == before,
                        '#1190 unchanged: real frozen bytes must be identical')

    def test_nonregular_entries_are_rejected_before_open(self):
        for kind in (stat.S_IFLNK, stat.S_IFDIR, stat.S_IFIFO, stat.S_IFSOCK,
                     stat.S_IFCHR, stat.S_IFBLK):
            with self.subTest(kind=kind), \
                    patch.object(os, 'open', return_value=80) as opened, \
                    patch.object(os, 'close'), \
                    patch.object(os, 'listdir', return_value=['entry']), \
                    patch.object(os, 'stat', return_value=types.SimpleNamespace(st_mode=kind)), \
                    patch.object(os, 'fstat', return_value=types.SimpleNamespace(st_mode=stat.S_IFREG)), \
                    patch.object(os, 'fdopen', return_value=MagicMock()):
                with self.assertRaisesRegex(ValueError, '#1190 FROZEN INPUT: nonregular entry entry',
                                            msg='#1190 nonregular: reject before child open'):
                    gate.read_frozen()
                self.assertTrue(opened.call_count == 4,
                                '#1190 nonregular: no child descriptor may open')

    def test_descriptor_type_is_rechecked(self):
        stream = Mock()
        stream.__enter__ = Mock(return_value=stream)
        stream.__exit__ = Mock(return_value=False)
        stream.fileno.return_value = 81
        with patch.object(os, 'open', return_value=80), patch.object(os, 'close'), \
                patch.object(os, 'listdir', return_value=['entry']), \
                patch.object(os, 'stat', return_value=types.SimpleNamespace(st_mode=stat.S_IFREG)), \
                patch.object(os, 'fstat', return_value=types.SimpleNamespace(st_mode=stat.S_IFIFO)), \
                patch.object(os, 'fdopen', return_value=stream):
            with self.assertRaisesRegex(ValueError, '#1190 FROZEN INPUT: nonregular opened entry entry',
                                        msg='#1190 descriptor: recheck opened type'):
                gate.read_frozen()
        self.assertFalse(stream.read.called, '#1190 descriptor: never read a raced special file')

    def test_open_flags_and_regular_bytes(self):
        stream = io.BytesIO(b'raw\r\nbytes')
        stream.fileno = lambda: 81
        with patch.object(os, 'open', return_value=80) as opened, patch.object(os, 'close'), \
                patch.object(os, 'listdir', return_value=['entry']), \
                patch.object(os, 'stat', return_value=types.SimpleNamespace(st_mode=stat.S_IFREG)) as info, \
                patch.object(os, 'fstat', return_value=types.SimpleNamespace(st_mode=stat.S_IFREG)), \
                patch.object(os, 'fdopen', return_value=stream):
            self.assertTrue(gate.read_frozen() == {'entry': b'raw\r\nbytes'},
                            '#1190 reader: preserve exact bytes')
        for call in opened.call_args_list:
            self.assertTrue(call.args[1] & os.O_NOFOLLOW,
                            '#1190 reader: every open must reject symlinks')
            self.assertFalse(call.args[1] & (os.O_WRONLY | os.O_RDWR | os.O_CREAT | os.O_TRUNC),
                             '#1190 reader: every open must be read-only')
        self.assertTrue(opened.call_args_list[-1].args[1] & os.O_NONBLOCK,
                        '#1190 reader: raced FIFO must not block')
        self.assertTrue(info.call_args.kwargs.get('follow_symlinks') is False,
                        '#1190 reader: entry stat must not follow symlinks')

    def test_directory_errors_fail_closed(self):
        with patch.object(os, 'open', side_effect=OSError('no-follow refusal')), \
                patch.object(sys, 'argv', ['negative_controls.py', '--check-anchors']), \
                contextlib.redirect_stderr(io.StringIO()) as errors:
            self.assertTrue(gate.main() == 2, '#1190 directory: input failure must reject CLI')
        self.assertIn('#1190 registration input: no-follow refusal', errors.getvalue(),
                      '#1190 directory: named input refusal required')

    def test_alternate_directory_is_not_an_option(self):
        with patch.object(sys, 'argv', ['negative_controls.py', '--frozen-dir', '/unused']), \
                patch.object(gate, 'read_frozen', return_value=self.raw) as reader, \
                contextlib.redirect_stderr(io.StringIO()) as errors:
            with self.assertRaises(SystemExit, msg='#1190 CLI: alternate directory must reject') as caught:
                gate.main()
        self.assertTrue(caught.exception.code == 2 and not reader.called,
                        '#1190 CLI: reject override before input loading')
        self.assertIn('unrecognized arguments: --frozen-dir', errors.getvalue(),
                      '#1190 CLI: no alternate frozen directory')

    def test_readonly_tripwire_invalidates_success(self):
        with patch.object(sys, 'argv', ['meta_controls.py']), \
                patch.object(gate, 'read_frozen', side_effect=[self.raw, {}]), \
                contextlib.redirect_stdout(io.StringIO()) as output:
            self.assertTrue(meta.main() == 1, '#1190 tripwire: drift must reject run')
        import json
        rows = json.loads(output.getvalue())['controls']
        self.assertTrue(all(row['status'] == 'INVALID' for row in rows),
                        '#1190 tripwire: revoke every successful row')
        self.assertTrue(all('#1190 real-frozen-unchanged' in row['reason'] for row in rows),
                        '#1190 tripwire: bind byte-drift diagnostic')

    def test_early_errors_have_fresh_evidence_and_continue(self):
        source = Mock()
        rows = [('first', source, 'absent', ''), ('second', source, 'pass', 'invalid !')]
        recorded = []
        with patch.object(sys, 'argv', ['negative_controls.py', '--output', '/unused', '--source', '/unused']), \
                patch.object(gate, 'MUTATIONS', rows), \
                patch.object(gate, 'BOUND', {name: (('case', 'rule'),) for name, *_ in rows}), \
                patch.object(gate, 'UNAFFECTED', {name: ('case', 'rule') for name, *_ in rows}), \
                patch.object(gate, 'check_bindings', return_value=[]), \
                patch.object(gate, 'read_frozen', return_value=self.raw), \
                patch.object(gate, 'read_documents', return_value={}), \
                patch.object(gate, 'check_docs', return_value=[]), \
                patch.object(gate, 'new_output_directory', return_value=MagicMock()), \
                patch.object(gate, 'mutating_sources', return_value=contextlib.nullcontext({source: b'pass\n'})), \
                patch.object(gate, 'publish_summary', side_effect=lambda _out, data: recorded.extend(data)), \
                contextlib.redirect_stdout(io.StringIO()):
            try:
                code = gate.main()
            except Exception as error:
                self.fail(f'#1190 evidence: early errors escaped: {type(error).__name__}: {error}')
            self.assertTrue(code == 1, '#1190 evidence: early errors must stay row-local')
        self.assertTrue(len(recorded) == 2, '#1190 evidence: must continue after early error')
        expected = {'baseline': [], 'mutant': [], 'unaffected': None, 'restored': None}
        self.assertTrue(all(row['evidence'] == expected for row in recorded),
                        '#1190 evidence: early errors need initialized empty evidence')
        self.assertTrue(recorded[0]['evidence'] is not recorded[1]['evidence'],
                        '#1190 evidence: never carry previous row evidence')
        self.assertTrue('anchor expired' in recorded[0]['reason'] and 'SyntaxError' in recorded[1]['reason'],
                        '#1190 evidence: exercise anchor and compile failures')


def negative_checks():
    """Compile function variants in memory, bind exact unittest names, never write source."""
    variants = (
        ('skip-entry-type', 'read_frozen',
         'if not stat.S_ISREG(os.stat(name, dir_fd=descriptor,\n'
         '                                       follow_symlinks=False).st_mode):', 'if False:',
         'test_nonregular_entries_are_rejected_before_open', '#1190 nonregular: reject before child open'),
        ('skip-opened-type', 'read_frozen',
         'if not stat.S_ISREG(os.fstat(stream.fileno()).st_mode):', 'if False:',
         'test_descriptor_type_is_rechecked', '#1190 descriptor: recheck opened type'),
        ('follow-root-link', 'read_frozen',
         'os.open(ROOT, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)',
         'os.open(ROOT, os.O_RDONLY | os.O_DIRECTORY)',
         'test_open_flags_and_regular_bytes', '#1190 reader: every open must reject symlinks'),
        ('blocking-child', 'read_frozen',
         'os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK', 'os.O_RDONLY | os.O_NOFOLLOW',
         'test_open_flags_and_regular_bytes', '#1190 reader: raced FIFO must not block'),
        ('follow-entry-stat', 'read_frozen', 'follow_symlinks=False', 'follow_symlinks=True',
         'test_open_flags_and_regular_bytes', '#1190 reader: entry stat must not follow symlinks'),
        ('duplicate-globs', 'doc_counts', 'for pattern in SCAN_GLOBS',
         "for pattern in ('*-1190.md', '*-1190-*.md')",
         'test_scan_globs_have_one_source', '#1190 globs: selection must use the shared constant'),
        ('allow-alternate-directory', 'main',
         '    args = parser.parse_args()',
         "    parser.add_argument('--frozen-dir')\n    args = parser.parse_args()",
         'test_alternate_directory_is_not_an_option', '#1190 CLI: alternate directory must reject'),
        ('skip-readonly-tripwire', 'meta.main', 'if gate.read_frozen() != before:', 'if False:',
         'test_readonly_tripwire_invalidates_success', '#1190 tripwire: drift must reject run'),
        ('accept-unrelated-assertion', 'meta.run_scenarios',
         "'SURVIVED' if str(error).startswith(row['assertion']) else 'INVALID'", "'SURVIVED'",
         'test_all_rows_run_after_invalid_or_survived', '#1190 aggregate: every selected row must run'),
        ('late-evidence', 'main',
         "                evidence = {'baseline': [], 'mutant': [], 'unaffected': None,\n"
         "                            'restored': None}\n", '',
         'test_early_errors_have_fresh_evidence_and_continue', '#1190 evidence: early errors must stay row-local'),
    )
    DriftTests.setUpClass()
    results = []
    for name, function, before, after, method, marker in variants:
        row = {'mutation': name, 'test': f'test_meta_controls.DriftTests.{method}',
               'assertion': marker, 'status': 'INVALID', 'compiled': False}
        try:
            module = meta if function.startswith('meta.') else gate
            function = function.removeprefix('meta.')
            text = inspect.getsource(getattr(module, function))
            if text.count(before) != 1:
                raise ValueError(f'#1190 {name}: stale in-memory anchor')
            namespace = {}
            exec(compile(text.replace(before, after, 1), f'<#1190 {name}>', 'exec'),
                 module.__dict__, namespace)
            changed = namespace[function]
            row['compiled'] = True
            positive = unittest.TestResult()
            DriftTests(method).run(positive)
            if not positive.wasSuccessful():
                raise ValueError('#1190 positive failed: ' + str(positive.errors + positive.failures))
            negative = unittest.TestResult()
            with patch.object(module, function, changed), contextlib.redirect_stdout(io.StringIO()):
                DriftTests(method).run(negative)
            failures = negative.errors + negative.failures
            row['raw'] = [detail for _test, detail in failures]
            if negative.wasSuccessful():
                row.update(status='SURVIVED', reason='named test passed under variant')
            elif (not negative.errors and negative.failures and
                  all(any('AssertionError' in line and marker in line for line in detail.splitlines())
                      for _test, detail in negative.failures)):
                row.update(status='KILLED', reason='named test failed with its exclusive assertion')
            else:
                row['reason'] = 'unrelated exception or missing bound assertion'
            restored = unittest.TestResult()
            DriftTests(method).run(restored)
            if not restored.wasSuccessful():
                raise ValueError('#1190 restored test failed')
        except Exception as error:
            row.update(status='INVALID', reason=f'{type(error).__name__}: {error}')
        results.append(row)
    print(json.dumps({'controls': results}, indent=2))
    return 0 if all(row['status'] == 'KILLED' for row in results) else 1


if __name__ == '__main__':
    if sys.argv[1:] == ['--negative']:
        raise SystemExit(negative_checks())
    unittest.main(verbosity=2)
