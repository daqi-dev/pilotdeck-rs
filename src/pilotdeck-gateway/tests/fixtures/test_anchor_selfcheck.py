#!/usr/bin/env python3
# anchor-selfcheck: unsupported synthetic scanner controls, no product mutations
"""Synthetic scanner controls: only tiny disposable text fixtures are changed."""
import ast
from contextlib import redirect_stdout
import importlib.util
import io
from pathlib import Path
import sys
import tempfile
import textwrap
import unittest
from unittest.mock import patch

# -I excludes cwd and PYTHONPATH. Load exactly this adjacent checker, never a
# mutation runner or a module selected from the caller's environment.
PATH = Path(__file__).with_name('anchor_selfcheck.py')
SPEC = importlib.util.spec_from_file_location('anchor_selfcheck', PATH)
CHECK = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = CHECK
SPEC.loader.exec_module(CHECK)
PRODUCTION_FIXTURES = PATH.parent
PRODUCTION_UNSUPPORTED = {}
for _script in sorted(PRODUCTION_FIXTURES.rglob('*.py')):
    _reason = CHECK.read_marker(_script)
    if _reason is not None:
        PRODUCTION_UNSUPPORTED[_script.relative_to(PRODUCTION_FIXTURES).as_posix()] = _reason
PRODUCTION_UNSUPPORTED_SIGNALS = dict(CHECK.UNSUPPORTED_SIGNALS)
MARKER_PREFIX = '# anchor-selfcheck: unsupported'


class ScannerControls(unittest.TestCase):
    def setUp(self):
        scratch = Path(__file__).resolve().parents[4] / '_tmp'
        scratch.mkdir(exist_ok=True)
        self.tmp = tempfile.TemporaryDirectory(prefix='pdrs-anchor-selfcheck-', dir=scratch)
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name).resolve()
        self.fixtures = self.root / 'src/pilotdeck-gateway/tests/fixtures'
        self.fixtures.mkdir(parents=True)
        self.source = self.root / 'src/example.rs'
        self.source.write_text('before\n')
        self.script = self.fixtures / 'sample/negative.py'
        self.script.parent.mkdir()
        self.base = "from pathlib import Path\nroot = Path(__file__).resolve().parents[5]\nsource = root / 'src/example.rs'\n"
        self.write("mutants = [('sample', source, 'before', 'after')]\n")
        signals = patch.dict(CHECK.UNSUPPORTED_SIGNALS, {}, clear=True)
        signals.start()
        self.addCleanup(signals.stop)

    def write(self, body):
        self.script.write_text(self.base + body)

    @staticmethod
    def strip_marker(text):
        return ''.join(line for line in text.splitlines(keepends=True)
                       if not line.lstrip().startswith(MARKER_PREFIX))

    def header(self, reason):
        """Insert a #977 unsupported marker after an optional shebang."""
        lines = self.script.read_text().splitlines(keepends=True)
        index = 1 if lines and lines[0].startswith('#!') else 0
        lines.insert(index, f'{MARKER_PREFIX} {reason}\n')
        self.script.write_text(''.join(lines))

    def remove_header(self):
        self.script.write_text(self.strip_marker(self.script.read_text()))

    def marker_at_line(self, number, reason):
        """Write a scanner script whose marker is on one exact 1-based line."""
        lines = self.base.splitlines(keepends=True)
        while len(lines) < number - 1:
            lines.append('# filler\n')
        lines.insert(number - 1, f'{MARKER_PREFIX} {reason}\n')
        self.script.write_text(''.join(lines) + "raise RuntimeError('must not run')\n")

    def run_check(self, expected=0, *, env=None, anchors_only=True, label=''):
        output = io.StringIO()
        with redirect_stdout(output):
            code = CHECK.check(self.root, anchors_only=anchors_only, environ=env or {})
        report = output.getvalue()
        self.assertEqual(code, expected, f'#827 scanner exit: {label}\n{report}')
        if label:
            print(f'CONTROL {label} exit={code}\n{report}', end='')
        return report

    def test_missing_and_duplicate_anchor_fail_then_restore(self):
        self.run_check(label='original')
        self.source.write_text('BROKEN\n')
        self.assertIn('actual=0', self.run_check(1, label='deleted-anchor'))
        self.source.write_text('before before\n')
        self.assertIn('actual=2', self.run_check(1, label='duplicate-anchor'))
        self.source.write_text('before\n')
        self.run_check(label='restored')

    def test_bad_before_in_control_fails_then_restores(self):
        self.write("mutants = [('sample', source, 'BROKEN', 'after')]\n")
        self.assertIn('actual=0', self.run_check(1, label='changed-before'))
        self.write("mutants = [('sample', source, 'before', 'after')]\n")
        self.run_check(label='restored-before')

    def test_switch_table_counts_one_compiled_site(self):
        self.source.write_text('crate::mutants::on("sample")\n')
        self.write("switches = [('sample', source, 'sample_test', '#996 sample anchor')]\n")
        self.assertIn('CHECKED sample/negative.py: 1 anchors',
                      self.run_check(label='switch-table'))
        # Either assertion anchor of a golden test may prove the kill.
        self.write("switches = [('sample', source, 'sample_test',"
                   " ['#996 first outcome', '#996 second outcome'])]\n")
        self.run_check(label='switch-anchor-list')
        self.source.write_text('nothing here\n')
        self.assertIn('actual=0', self.run_check(1, label='missing-site'))
        self.source.write_text('crate::mutants::on("sample") crate::mutants::on("sample")\n')
        self.assertIn('actual=2', self.run_check(1, label='duplicate-site'))
        self.source.write_text('crate::mutants::on("sample")\n')
        self.assertIn('CHECKED sample/negative.py: 1 anchors',
                      self.run_check(label='restored'))

    def test_switch_rows_require_full_bindings_and_literals(self):
        self.source.write_text('crate::mutants::on("sample")\n')
        for body, message in (
            ("switches = [('sample', source, 'sample_test', '')]\n", 'invalid switch assertion'),
            ("switches = [('sample', source, '', '#996 anchor')]\n", 'invalid switch test'),
            ("switches = [('sample', source, 'sample_test', [])]\n", 'invalid switch assertion'),
            ("switches = [('sample', source, 'sample_test')]\n",
             'switches rows must be (name, path, test, assertion)'),
            ("switches = [('sample', source, 'sample_test', '#996 a', 'extra')]\n",
             'switches rows must be (name, path, test, assertion)'),
            ("switches = [('sample', source, 'sample_test', '#996 a'),"
             " ('sample', source, 'other', '#996 b')]\n", 'duplicate switch name'),
            ("switches = None\n", 'switches table must be a literal list'),
        ):
            with self.subTest(body=body):
                self.write(body)
                self.assertIn(message, self.run_check(1))
        self.write("switches = [('sample', source, 'sample_test', '#996 a')]\n")
        self.run_check(label='restored-bindings')

    def test_switch_table_updates_fail_closed(self):
        # #996 fix-r2: the table name is a whitelist; the single module-level
        # literal assignment is the only supported binding. r1's per-form
        # blacklist could not enumerate every escape, so any other Store/Del
        # shape or unlisted read now fails closed instead of silently
        # shrinking the audited rows below the executed table.
        self.source.write_text('crate::mutants::on("sample")\n')
        table = "switches = [('sample', source, 'sample_test', '#996 a')]\n"
        binding = 'the switches table must be the single module-level assignment'
        for update, message in (
            ("switches.append(('other', source, 'sample_test', '#996 b'))\n",
             'unsupported switches read'),
            ("switches.extend([('other', source, 'sample_test', '#996 b')])\n",
             'unsupported switches read'),
            ("switches.insert(0, ('other', source, 'sample_test', '#996 b'))\n",
             'unsupported switches read'),
            ("switches.pop()\n", 'unsupported switches read'),
            ("switches.remove('sample')\n", 'unsupported switches read'),
            ("switches.clear()\n", 'unsupported switches read'),
            ("switches.sort()\n", 'unsupported switches read'),
            # Subscript targets leave the inner Name in Load context, so these
            # fall through the read rejection; the bare forms hit the binding.
            ("del switches[0]\n", 'unsupported switches read'),
            ("switches[0] = ('other', source, 'sample_test', '#996 b')\n",
             'unsupported switches read'),
            ("switches[0][1] = source\n", 'unsupported switches read'),
            ("switches[:] = []\n", 'unsupported switches read'),
            ("switches += [('other', source, 'sample_test', '#996 b')]\n", binding),
            ("switches, extra = [], []\n", binding),
            ("(extra, *switches) = ([], [])\n", binding),
            ("switches: list = []\n", binding),
            ("if (switches := []):\n    pass\n", binding),
            # Last: a mutant that drops the binding rejection crashes on the
            # Delete node, so the earlier binding subTests must fail first.
            ("del switches\n", binding),
        ):
            with self.subTest(update=update):
                self.write(table + update)
                self.assertIn(message, self.run_check(1),
                              f'#996 r2 {update!r} must fail closed')
        self.write(table)
        self.assertIn('CHECKED sample/negative.py: 1 anchors',
                      self.run_check(label='restored-updates'),
                      '#996 r2 the restored table must check one anchor again')

    def test_switch_occurrences_outside_whitelist_fail_closed(self):
        # #996 fix-r2 review P1: r1's per-form blacklist silently passed
        # aliases (`s = switches; s.append`), indirect calls
        # (`getattr(switches, "pop")`, `list.clear(switches)`) and dynamic
        # rebinds (`setattr(sys.modules[__name__], "switches", [])`). The name
        # is now a whitelist: every occurrence that is not the declaration,
        # for-in, len() or an executor argument fails closed, including the
        # spellings no Name node ever sees (imports, keywords, literals) and
        # the dynamic module forms rejected outright in every checked script.
        self.source.write_text('crate::mutants::on("sample")\n')
        table = "switches = [('sample', source, 'sample_test', '#996 a')]\n"
        for escape, message in (
            ('s = switches\ns.append(("other", source, "sample_test", "#996 b"))\n',
             'unsupported switches read'),
            ('s = switches\ndel s[0]\n', 'unsupported switches read'),
            ('getattr(switches, "pop")(0)\n', 'unsupported switches read'),
            ('list.clear(switches)\n', 'unsupported switches read'),
            ('operator.setitem(switches, 0, None)\n', 'unsupported switches read'),
            ('consume(switches)\n', 'unsupported switches read'),
            ('import os as switches\n', 'unsupported switches named occurrence'),
            ('import switches\n', 'unsupported switches named occurrence'),
            ('from os import path as switches\n', 'unsupported switches named occurrence'),
            ('def helper(switches):\n    pass\n', 'unsupported switches named occurrence'),
            ('consume(switches=1)\n', 'unsupported switches named occurrence'),
            ("mirror = 'switches'\n", 'unsupported switches named occurrence'),
            ('namespace.switches = []\n', 'unsupported switches named occurrence'),
            ('global switches\n', 'unsupported switches named occurrence'),
            ('setattr(sys.modules[__name__], "switches", [])\n',
             'unsupported dynamic module access'),
            # A concatenated key never spells the name statically; only the
            # outright dynamic-form rejection can close these.
            ('setattr(sys.modules[__name__], "swi" + "tches", [])\n',
             'unsupported dynamic module access: setattr'),
            ('globals()["swi" + "tches"] = []\n',
             'unsupported dynamic module access: globals'),
            ('vars(object)["swi" + "tches"] = []\n',
             'unsupported dynamic module access: vars'),
            ('mirror = sys.modules[__name__]\n',
             'unsupported dynamic module access: sys.modules[...]'),
        ):
            with self.subTest(escape=escape):
                self.write(table + escape)
                self.assertIn(message, self.run_check(1),
                              f'#996 r2 {escape!r} must fail closed')
        self.write(table)
        self.assertIn('CHECKED sample/negative.py: 1 anchors',
                      self.run_check(label='restored-escapes'),
                      '#996 r2 the restored table must check one anchor again')

    def test_switch_whitelist_readonly_forms_stay_supported(self):
        # The positive side of the fix-r2 whitelist: the enumerated read-only
        # uses of the declared table must keep checking, or the scanner would
        # force schemata runners into unsupported markers.
        self.source.write_text('crate::mutants::on("sample")\n')
        table = "switches = [('sample', source, 'sample_test', '#996 a')]\n"
        reads = ('total = len(switches)\n'
                 'chosen = mutant_switch.selected(switches, [])\n'
                 "rows = [dict(zip(('name', 'path', 'test', 'anchor'), row))"
                 ' for row in switches]\n'
                 'for row in switches:\n    pass\n')
        self.write(table + reads)
        self.assertIn('CHECKED sample/negative.py: 1 anchors',
                      self.run_check(label='switch-whitelist'),
                      '#996 r2 the whitelisted read-only forms must stay supported')
        self.write(reads)
        self.assertIn('switches is read without its literal table declaration',
                      self.run_check(1, label='switch-read-without-table'),
                      '#996 r2 a read without the declaration must fail closed')
        self.write(table)
        self.assertIn('CHECKED sample/negative.py: 1 anchors',
                      self.run_check(label='switch-whitelist-restored'),
                      '#996 r2 the bare table must check one anchor again')

    def test_marker_scripts_cannot_hide_a_switch_table(self):
        self.source.write_text('crate::mutants::on("sample")\n')
        self.write("switches = [('sample', source, 'sample_test', '#996 anchor')]\n")
        self.header('exempts the runner')
        self.assertIn('exemption signals', self.run_check(1, label='hidden-switch-table'))
        self.remove_header()
        self.run_check(label='unhidden-switch-table')

    def test_switch_table_counts_alongside_replacement_tables(self):
        self.source.write_text('crate::mutants::on("sample") before\n')
        self.write("mutants = [('replace', source, 'before', 'after')]\n"
                   "switches = [('sample', source, 'sample_test', '#996 anchor')]\n")
        self.assertIn('CHECKED sample/negative.py: 2 anchors',
                      self.run_check(label='combined-tables'))

    def test_declared_occurrences_are_not_replace_limit(self):
        self.source.write_text('before before')
        self.write("ANCHOR_COUNTS = {'factory-ready-priority': 2}\nmutants = [('factory-ready-priority', source, 'before', 'after')]\nchanged = text.replace(before, after, 1)\n")
        self.run_check()
        self.source.write_text('before')
        self.assertIn('expected=2 actual=1', self.run_check(1))
        self.source.write_text('before before before')
        self.assertIn('actual=3', self.run_check(1))
        self.source.write_text('before before')
        self.run_check()

    def test_inline_count_and_invalid_counts(self):
        self.source.write_text('before before')
        self.write("mutants = [('sample', source, 'before', 'after', 2)]\n")
        self.run_check()
        for count in ['0', '-1', 'True', 'False', '1.5']:
            with self.subTest(count=count):
                self.write(f"mutants = [('sample', source, 'before', 'after', {count})]\n")
                self.assertIn('positive integer', self.run_check(1))
        self.write("ANCHOR_COUNTS = {'unused': 2}\nmutants = [('sample', source, 'before', 'after', 2)]\n")
        self.assertIn('unused ANCHOR_COUNTS', self.run_check(1))
        self.write("ANCHOR_COUNTS = {'sample': True}\nmutants = [('sample', source, 'before', 'after')]\n")
        self.assertIn('positive integers', self.run_check(1))
        self.write("ANCHOR_COUNTS = {'sample': 2}\nmutants = [('sample', source, 'before', 'after', 2)]\n")
        self.assertIn('duplicate count', self.run_check(1))

    def test_empty_counts_differ_from_missing_and_bool_counts_are_rejected(self):
        table = "mutants = [('sample', source, 'before', 'after')]\n"
        self.write(table)
        self.run_check(label='missing-counts-default-one')
        self.write('ANCHOR_COUNTS = {}\n' + table)
        self.assertIn('empty ANCHOR_COUNTS', self.run_check(1, label='empty-counts'))
        self.write("ANCHOR_COUNTS = {'sample': 1}\n" + table)
        self.run_check(label='nonempty-counts-restored')
        for count in ('True', 'False'):
            for inline in (True, False):
                with self.subTest(count=count, inline=inline):
                    self.write(table.replace("'after')", f"'after', {count})") if inline
                               else f"ANCHOR_COUNTS = {{'sample': {count}}}\n" + table)
                    self.assertIn('positive integer', self.run_check(1, label=f'bool-count-{inline}-{count}'))
                    self.write(table.replace("'after')", "'after', 1)") if inline
                               else "ANCHOR_COUNTS = {'sample': 1}\n" + table)
                    self.run_check(label='integer-count-restored')

    def test_regex_flags_are_used_and_invalid_regex_fails(self):
        self.source.write_text('BEGIN\nend')
        pattern = "mutants = [('sample', source, r'begin.*end', 'after')]\n"
        consumer = "changed, count = re.subn(pattern, replacement, content, flags=re.S | re.I)\n"
        self.write(pattern + consumer)
        self.run_check()
        self.write(pattern + consumer.replace('re.S | re.I', 're.S'))
        self.assertIn('actual=0', self.run_check(1))
        self.write(pattern.replace('begin.*end', '[') + consumer)
        self.run_check(1)
        self.write(pattern + consumer.replace('re.S | re.I', 'unknown_flags'))
        self.assertIn('unsupported regex flags', self.run_check(1))
        self.write(pattern + consumer)
        self.run_check()

    def test_re_escape_is_regex_not_literal(self):
        self.source.write_text('before()')
        self.write("mutants = [('sample', source, re.escape('before()'), 'after')]\nre.subn(pattern, replacement, content)\n")
        self.run_check()
        self.source.write_text('before')
        self.run_check(1)

    def test_sequential_regex_rewrite_requires_explicit_file_exception(self):
        name = 'runtime-spawn-panic/negative_controls.py'
        self.script.unlink()
        self.script = self.fixtures / name
        self.script.parent.mkdir()
        self.source.write_text('before\nreturn old\n')
        self.write("""import re
import os
assert os.environ.get('CARGO_TARGET_DIR')
mutants = [('clock-pending-snapshot', source, 'before', '')]
cases = []
for name, path, pattern, replacement in mutants:
    content = path.read_text()
    changed, count = re.subn(pattern, replacement, content, flags=re.S)
    if name == 'clock-pending-snapshot':
        changed, count = re.subn(re.escape('return old'), 'return new', changed)
    path.write_text(changed)
""")
        env = {'CARGO_TARGET_DIR': str(self.root / 'target')}
        self.assertIn('unsupported regex consumer', self.run_check(
            1, env=env, label='sequential-regex-unregistered'))
        self.assertIn(name, PRODUCTION_UNSUPPORTED)
        reason = PRODUCTION_UNSUPPORTED[name]
        self.assertTrue(reason.strip())
        self.header(reason)
        CHECK.UNSUPPORTED_SIGNALS[name] = PRODUCTION_UNSUPPORTED_SIGNALS[name]
        for anchors_only in (False, True):
            with self.subTest(anchors_only=anchors_only):
                report = self.run_check(env=env, anchors_only=anchors_only,
                                        label='sequential-regex-explicit-exception')
                self.assertIn(f'UNSUPPORTED {name}: {reason}', report)
                self.assertIn('checked=0 unsupported=1 anchors=0 errors=0', report)
        self.assertIn('MISSING_ENV CARGO_TARGET_DIR', self.run_check(
            1, anchors_only=False, label='sequential-regex-missing-environment'))
        self.remove_header()
        del CHECK.UNSUPPORTED_SIGNALS[name]
        self.assertIn('unsupported regex consumer', self.run_check(
            1, env=env, label='sequential-regex-exception-removed'))
        self.header(reason)
        CHECK.UNSUPPORTED_SIGNALS[name] = PRODUCTION_UNSUPPORTED_SIGNALS[name]
        self.run_check(env=env, anchors_only=False, label='sequential-regex-restored')
        other = self.fixtures / 'sample/negative.py'
        other.write_text(self.strip_marker(self.script.read_text()))
        self.assertIn('ERROR sample/negative.py: unsupported regex consumer',
                      self.run_check(1, env=env, label='sequential-regex-other-file'))
        other.unlink()
        self.run_check(env=env, anchors_only=False)
        self.assertEqual(self.source.read_text(), 'before\nreturn old\n')

    def test_heartbeat_implicit_source_checks_anchors(self):
        self.script.unlink()
        self.script = self.fixtures / 'edgeclaw-heartbeat/negative.py'
        self.script.parent.mkdir()
        self.write("helpers = source\ncases = [('same-role', 'before', 'after', 'assertion')]\n")
        self.assertIn('checked=1 unsupported=0 anchors=1 errors=0',
                      self.run_check(label='heartbeat-implicit-source'))
        self.source.write_text('BROKEN\n')
        self.assertIn('same-role: src/example.rs expected=1 actual=0',
                      self.run_check(1, label='heartbeat-missing-anchor'))
        self.source.write_text('before before\n')
        self.assertIn('expected=1 actual=2',
                      self.run_check(1, label='heartbeat-duplicate-anchor'))
        self.source.write_text('before\n')
        self.run_check(label='heartbeat-restored')

    def test_compiled_aliased_and_mixed_regex_consumers_fail_closed(self):
        self.source.write_text('[')
        table = "mutants = [('sample', source, '[', 'after')]\n"
        for consumer in [
            "compiled = re.compile(pattern)\ncompiled.sub(replacement, content)",
            "from re import sub\nsub(pattern, replacement, content)",
            "import re as regex\nregex.sub(pattern, replacement, content)",
            "substitute = re.subn\nsubstitute(pattern, replacement, content)",
        ]:
            with self.subTest(consumer=consumer):
                self.write(table + consumer)
                self.run_check(1)
        self.source.write_text('before text')
        self.write("mutants = [('sample', source, 'before.*', 'after')]\ncontent.replace(before, after)\nre.subn(pattern, replacement, content)\n")
        self.assertIn('mixed literal/regex', self.run_check(1))

    def move_script(self, relative):
        self.script.unlink()
        self.script = self.fixtures / relative
        self.script.parent.mkdir(parents=True, exist_ok=True)

    def test_background_helper_tests_require_an_explicit_exception(self):
        name = 'background-panic/test_negative_controls.py'
        self.move_script(name)
        self.write("def test_helper():\n    negative.mutate('probe', 'anchor', 'anchor', 'changed')\n")
        self.assertIn('no supported mutation anchors',
                      self.run_check(1, label='helper-tests-unregistered'))
        self.header(PRODUCTION_UNSUPPORTED[name])
        self.assertIn(f'UNSUPPORTED {name}:', self.run_check(label='helper-tests-registered'))
        self.remove_header()
        self.run_check(1, label='helper-tests-registration-removed')
        self.header(PRODUCTION_UNSUPPORTED[name])
        self.run_check(label='helper-tests-registration-restored')

    def test_llm_helper_exception_is_exact_and_signal_checked(self):
        name = 'edgeclaw-llm/test_controls.py'
        self.move_script(name)
        body = "def test_guard():\n    originals = {}\n    exec('pass')\n    cases = []\n"
        self.write(body)
        self.assertIn('snapshot dynamic write', self.run_check(1, label='#827 llm-helper unregistered'),
                      '#827 llm-helper must require registration')
        reason = PRODUCTION_UNSUPPORTED.get(name)
        self.assertEqual(reason, PRODUCTION_UNSUPPORTED['test_negative_process.py'],
                         '#827 llm-helper must use the shared guard test reason')
        CHECK.UNSUPPORTED_SIGNALS[name] = PRODUCTION_UNSUPPORTED_SIGNALS.get(name, set())
        for broken in ('missing-signals', 'extra-signal', 'removed-registration'):
            self.write(body)
            self.header(reason)
            self.assertIn('checked=0 unsupported=1 anchors=0 errors=0',
                          self.run_check(label='#827 llm-helper original'),
                          '#827 llm-helper registered signal must pass')
            if broken == 'missing-signals':
                CHECK.UNSUPPORTED_SIGNALS[name] = set()
            elif broken == 'extra-signal':
                self.write(body + "mutants = [('sample', source, 'before', 'after')]\n")
                self.header(reason)
            else:
                self.remove_header()
            self.run_check(1, label=f'#827 llm-helper {broken}')
            CHECK.UNSUPPORTED_SIGNALS[name] = PRODUCTION_UNSUPPORTED_SIGNALS.get(name, set())
            self.write(body)
            self.header(reason)
            self.run_check(label='#827 llm-helper restored')
        other = self.fixtures / 'renamed/test_controls.py'
        other.parent.mkdir()
        other.write_text(self.base + body)
        self.assertIn('ERROR renamed/test_controls.py: unsupported snapshot dynamic write',
                      self.run_check(1, label='#827 llm-helper renamed'),
                      '#827 llm-helper registration must not extend to another path')
        other.unlink()
        self.run_check(label='#827 llm-helper renamed restored')

    def test_dictionary_rows_and_new_keys_check_every_anchor(self):
        body = """mutants = {'first': (source, 'before', 'after')}
for suffix, old in [('a', 'before'), ('b', 'before')]:
    mutants[f'added-{suffix}'] = (source, old, old.replace('before', 'after'))
"""
        self.write(body)
        self.assertIn('anchors=3 errors=0', self.run_check(label='dictionary-original'))
        self.source.write_text('missing')
        self.assertIn('anchors=3 errors=3', self.run_check(1, label='dictionary-missing'))
        self.source.write_text('before before')
        self.assertIn('anchors=3 errors=3', self.run_check(1, label='dictionary-ambiguous'))
        self.source.write_text('before\n')
        self.run_check(label='dictionary-restored')
        for bad in [
            body + "mutants['first'] = (source, 'before', 'after')\n",
            body.replace("('b', 'before')", "('a', 'before')"),
            "mutants = {'first': (source, 'before', 'after'), 'first': (source, 'before', 'after')}\n",
        ]:
            self.write(bad)
            self.assertIn('duplicate mutation key', self.run_check(1, label='dictionary-overwrite'))
        for bad in [
            "mutants = {'first': dynamic_row()}\n",
            "mutants = {**other}\n",
            "mutants = {'': (source, 'before', 'after')}\n",
            "mutants = {1: (source, 'before', 'after')}\n",
            body + 'mutants.clear()\n',
            body + "mutants['first'] += (source, 'before', 'after')\n",
            body + "mutants = {'reset': (source, 'before', 'after')}\n",
            body + "mutants[unknown_key] = (source, 'before', 'after')\n",
            body + "del mutants['first']\n",
            body + 'del mutants\n',
            body + "mutants['extra']: tuple = (source, 'before', 'after')\n",
            "if unknown_condition:\n    mutants = {'first': (source, 'before', 'after')}\n",
            "if unknown_condition:\n    mutants = {}\nmutants['first'] = (source, 'before', 'after')\n",
            "for suffix in ['a', 'b']:\n    mutants = {}\n    mutants[suffix] = (source, 'before', 'after')\n",
            "mutants = {}\nwhile unknown_condition:\n    mutants['first'] = (source, 'before', 'after')\n",
            body.replace('    mutants[f', '    break\n    mutants[f'),
            body.replace('    mutants[f', '    if unknown_condition:\n        continue\n    mutants[f'),
        ]:
            self.write(bad)
            self.run_check(1, label='dictionary-unsupported-update')
        self.write(body)
        self.run_check(label='dictionary-format-restored')

    def test_host_pump_dictionary_uses_its_conditional_count_guard(self):
        self.move_script('host-event-pump/negative_controls.py')
        body = """path = source
mutations = {'idle': ('before', 'after', 'test'), 'wire': ('delivery', 'changed', 'test')}
name = sys.argv[2]
old, new, test = mutations[name]
assert content.count(old) == (2 if name == 'wire' else 1), name
"""
        self.source.write_text('before delivery delivery')
        self.write(body)
        self.assertIn('anchors=2 errors=0', self.run_check(label='pump-original'))
        for content, error in [('before delivery', 'expected=2 actual=1'),
                               ('before delivery delivery delivery', 'expected=2 actual=3'),
                               ('delivery delivery', 'expected=1 actual=0')]:
            self.source.write_text(content)
            self.assertIn(error, self.run_check(1, label='pump-bad-count'))
        self.source.write_text('before delivery delivery')
        self.run_check(label='pump-restored')
        # Read the script's expectation, do not duplicate a hardcoded wire=2.
        self.write(body.replace("2 if name", "3 if name"))
        self.assertIn('expected=3 actual=2', self.run_check(1, label='pump-changed-guard'))
        for guard in ['', 'assert content.count(old) == dynamic_count\n',
                      'assert content.count(old) != 2\n',
                      'assert content.count(old) == 2 == 2\n',
                      'assert content.count(old) == 0\n',
                      'assert content.count(old) == True\n',
                      'assert content.count(old) == 2\nassert content.count(old) == 2\n']:
            self.write(body[:body.index('assert')] + guard)
            self.run_check(1, label='pump-unsupported-guard')
        self.write(body)
        self.run_check(label='pump-guard-restored')
        self.write("ANCHOR_COUNTS = {'wire': 2}\n" + body)
        self.assertIn('duplicate count declaration', self.run_check(1, label='pump-duplicate-count-rule'))
        self.write(body)
        self.run_check(label='pump-count-rule-restored')

    def test_output_schema_bytes_preserve_exact_file_bytes(self):
        self.move_script('output-schema/negative.py')
        # Invalid UTF-8 and CRLF distinguish byte counting from text decoding.
        original = b'prefix\xff\r\nbefore\r\n'
        body = "MUTANTS = [('mask', b'\\xff\\r\\nbefore', b'after', True)]\n"
        self.source.write_bytes(original)
        self.write(body)
        self.assertIn('anchors=1 errors=0', self.run_check(label='bytes-original'))
        for content in [original.replace(b'\r\n', b'\n'), b'missing', original * 2]:
            self.source.write_bytes(content)
            self.run_check(1, label='bytes-wrong-anchor')
        self.source.write_bytes(original)
        self.run_check(label='bytes-restored')
        self.write(body.replace("b'\\xff\\r\\nbefore'", "b''"))
        self.assertIn('empty', self.run_check(1, label='bytes-empty-anchor'))
        self.write(body.replace("b'after'", "'after'"))
        self.assertIn('same string/byte type', self.run_check(1, label='bytes-text-replacement'))
        self.write("MUTANTS = [('mask', 'before', b'after', True)]\n")
        self.assertIn('same string/byte type', self.run_check(1, label='text-bytes-replacement'))
        self.write(body + 're.subn(pattern, replacement, content)\n')
        self.assertIn('literal consumers', self.run_check(1, label='bytes-regex-consumer'))
        self.write(body)
        self.run_check(label='bytes-format-restored')

    def test_sidecar_dictionary_metadata_and_generated_json_rows(self):
        self.move_script('sidecar-payload/negative.py')
        body = """mutants = {'embedded': (source, 'package', 'test', 'before', 'after')}
for route, test in [('wire', 'wire'), ('host', 'module')]:
    old = f'"name":"missing","route":"{route}"'
    mutants[f'omit-{route}'] = ('src/oracle.json', 'package', test, old, old.replace(route, 'omitted'))
"""
        oracle = self.root / 'src/oracle.json'
        original = '"name":"missing","route":"wire"\n"name":"missing","route":"host"'
        oracle.write_text(original)
        self.write(body)
        self.assertIn('anchors=3 errors=0', self.run_check(label='sidecar-original'))
        oracle.write_text(original.replace('wire', 'lost'))
        self.assertIn('omit-wire: src/oracle.json expected=1 actual=0',
                      self.run_check(1, label='sidecar-json-missing'))
        oracle.write_text(original)
        self.source.write_text('missing')
        self.assertIn('embedded: src/example.rs expected=1 actual=0',
                      self.run_check(1, label='sidecar-product-missing'))
        self.source.write_text('before\n')
        self.run_check(label='sidecar-restored')
        self.write(body.replace("'package', 'test', ", ''))
        self.run_check(1, label='sidecar-incomplete-metadata-row')
        self.write(body)
        self.run_check(label='sidecar-row-restored')

    def test_hooks_output_relative_source_and_concatenated_anchor(self):
        # host-factory/negative.py migrated to compile-time switches (#996);
        # hooks-output keeps the implicit relative-source shape.
        self.move_script('hooks-output/negative.py')
        body = """source = Path('src/example.rs')
LOOP = 'before'
mutants = [('lease', LOOP, 'after', 'test', 'failure'),
           ('deadline', 'timeout ' + LOOP, 'changed ' + LOOP, 'test', 'failure')]
"""
        self.source.write_text('timeout before')
        self.write(body)
        self.assertIn('anchors=2 errors=0', self.run_check(label='factory-original'))
        self.source.write_text('before')
        self.assertIn('deadline: src/example.rs expected=1 actual=0',
                      self.run_check(1, label='factory-dependency-missing'))
        self.source.write_text('timeout before before')
        self.assertIn('expected=1 actual=2', self.run_check(1, label='factory-ambiguous'))
        self.source.write_text('timeout before')
        self.run_check(label='factory-restored')
        self.write(body.replace("src/example.rs", '../outside.rs'))
        self.assertIn('outside repository', self.run_check(1, label='factory-outside-source'))
        self.write(body)
        self.run_check(label='factory-path-restored')

    def test_constants_paths_loops_and_extensions(self):
        self.write("""folder = source.parent
alias, label = [folder / 'example.rs', 'sample']
OLD = 'be' + 'fore'
mutants = [(label, alias, OLD, 'after')]
mutants += [('extra', source.with_name('example.rs'), OLD, 'after')]
for suffix, old in [('a', OLD), ('b', OLD)]:
    mutants.append((f'loop-{suffix}', source, old, 'after'))
mutants.extend([('extended', source, OLD, 'after')])
mutants += [*[(f'expanded-{suffix}', source, OLD, 'after') for suffix in ['c', 'd']]]
""")
        self.assertIn('anchors=7', self.run_check())
        self.source.write_text('wrong')
        self.assertIn('errors=7', self.run_check(1))

    def test_function_calls_dict_changes_and_runner_table(self):
        self.write("""mutants = [mutation('first', source, 'before', 'after'),
 dict(name='second', changes=[(source, 'before', 'after')])]
runner_mutations = [('third', source, 'before', 'after')]
""")
        self.assertIn('anchors=3', self.run_check())
        self.write(self.script.read_text()[len(self.base):].replace("'before'", "'missing'", 1))
        self.run_check(1)

    def test_after_dependencies_are_validated(self):
        self.write("""text = source.read_text()
changed = replace_once(text, 'before', 'after')
mutants = [('sample', source, text, changed)]
""")
        self.run_check()
        # The whole-file before still matches; only construction of after fails.
        self.source.write_text('renamed')
        self.assertIn('replace_once dependency', self.run_check(1, label='generated-after-anchor'))
        self.source.write_text('before')
        self.run_check()

    def test_dynamic_index_dependency_fails(self):
        self.write("""text = source.read_text()
part = text[:text.index('before')]
mutants = [('sample', source, text, part + 'after')]
""")
        self.run_check()
        self.source.write_text('missing')
        self.run_check(1)

    def test_snapshot_updates_and_set_inventory_are_interpreted(self):
        table = "mutants = [('sample', source, originals['example.rs'], 'after')]\n"
        prefix = "paths = {source}\noriginals = {p.name: p.read_text() for p in {p for p in paths} | {source}}\n"
        self.write(prefix + table)
        self.run_check(label='snapshot-set-inventory')
        update = "originals['example.rs'] = originals['example.rs'].replace('before', 'missing')\n"
        self.write(prefix + update + table)
        self.assertIn('actual=0', self.run_check(1, label='transformed-snapshot-before'))
        self.write(prefix + update + "originals = {'example.rs': source.read_text()}\n" + table)
        self.assertIn('unsupported snapshot binding', self.run_check(1, label='#827 second declaration'),
                      '#827 second declaration must be rejected')
        self.write(prefix.replace('{p for p in paths}', '{p for p in paths if condition}') + table)
        self.assertIn('unconditional single-loop', self.run_check(1, label='unknown-set-selection'))
        self.write(prefix + table)
        self.run_check()

    def test_snapshot_json_assignment_is_resolved_to_its_read_path(self):
        path = self.root / 'src/oracle.json'
        path.write_text('before')
        prefix = "originals = {}\noriginals['oracle.json'] = (root / 'src/oracle.json').read_text()\n"
        table = "mutants = [('sample', 'oracle.json', originals['oracle.json'], 'after')]\n"
        self.write(prefix + table)
        self.run_check(label='snapshot-json-path')
        self.write(prefix + "originals['oracle.json'] = 'missing'\n" + table)
        self.assertIn('unresolved source path', self.run_check(1, label='snapshot-json-read-removed'))
        self.write(prefix + table)
        self.run_check(label='snapshot-json-read-restored')

    def check_snapshot_binding(self, statement, label):
        for name in ('originals', 'snapshots'):
            for constructor in ('assign', 'with'):
                with self.subTest(name=name, constructor=constructor):
                    declaration = (f"{name} = {{'alias.rs': source.read_text()}}\n" if constructor == 'assign'
                                   else f"from negative_process import mutating_sources\nwith mutating_sources(root, [source]) as {name}:\n    pass\n")
                    table = "mutants = [('sample', source, 'before', 'after')]\n"
                    self.write(declaration + table)
                    self.run_check(label=f'#827 {label} {name} {constructor} original')
                    self.write(declaration + statement.replace('SNAPSHOT', name) + '\n' + table)
                    self.assertIn('unsupported snapshot binding', self.run_check(
                        1, label=f'#827 {label} {name} {constructor} rejected'),
                        f'#827 {label} must reject snapshot binding')
                    self.write(declaration + table)
                    self.run_check(label=f'#827 {label} {name} {constructor} restored')

    def test_snapshot_rejects_for_binding(self):
        self.check_snapshot_binding('for SNAPSHOT in [{}]:\n    pass', 'for-binding')

    def test_snapshot_rejects_walrus_binding(self):
        self.check_snapshot_binding('(SNAPSHOT := {})', 'walrus-binding')

    def test_snapshot_rejects_except_binding(self):
        self.check_snapshot_binding('try:\n    pass\nexcept Exception as SNAPSHOT:\n    pass', 'except-binding')

    def test_snapshot_rejects_import_binding(self):
        for statement in ('import os as SNAPSHOT', 'from os import path as SNAPSHOT', 'import SNAPSHOT'):
            self.check_snapshot_binding(statement, 'import-binding')

    def test_snapshot_rejects_definition_binding(self):
        for statement in ('def SNAPSHOT():\n    pass', 'async def SNAPSHOT():\n    pass',
                          'class SNAPSHOT:\n    pass', 'def helper(SNAPSHOT):\n    pass',
                          'lambda SNAPSHOT: None'):
            self.check_snapshot_binding(statement, 'definition-binding')

    def test_snapshot_rejects_comprehension_binding(self):
        for statement in ('[0 for SNAPSHOT in [{}]]', '{0 for SNAPSHOT in [{}]}',
                          '{0: 0 for SNAPSHOT in [{}]}', '(0 for SNAPSHOT in [{}])'):
            self.check_snapshot_binding(statement, 'comprehension-binding')

    def test_snapshot_rejects_match_binding(self):
        for pattern in ('SNAPSHOT', '[*SNAPSHOT]', "{'key': _, **SNAPSHOT}"):
            self.check_snapshot_binding(f'match {{}}:\n    case {pattern}:\n        pass', 'match-binding')

    @unittest.skipUnless(hasattr(ast, 'TypeVar'), 'Python 3.12+ type parameter syntax')
    def test_snapshot_rejects_type_parameter_binding(self):
        for parameter in ('SNAPSHOT', '*SNAPSHOT', '**SNAPSHOT'):
            self.check_snapshot_binding(f'type Alias[{parameter}] = int', 'type-parameter-binding')

    def test_snapshot_rejects_namespace_declaration(self):
        for statement in ('global SNAPSHOT', 'def helper():\n    global SNAPSHOT',
                          'def helper():\n    nonlocal SNAPSHOT'):
            self.check_snapshot_binding(statement, 'namespace-binding')

    def test_snapshot_rejects_second_assignment_or_with(self):
        for statement in ('SNAPSHOT = {}', 'with mutating_sources(root, [source]) as SNAPSHOT:\n    pass',
                          'with other() as SNAPSHOT:\n    pass', 'SNAPSHOT, other = {}, {}'):
            self.check_snapshot_binding(statement, 'second-binding')

    def test_snapshot_rejects_dynamic_writes_and_aliases(self):
        declaration = "originals = {'alias.rs': source.read_text()}\n"
        table = "mutants = [('sample', 'alias.rs', 'before', 'after')]\n"
        for statement in (
            "globals()['originals'] = {}", "locals()['originals'] = {}",
            "vars()['originals'] = {}", "setattr(module, 'originals', {})",
            "delattr(module, 'originals')", "exec('originals = {}')", "eval('globals()')",
            "module.__dict__['originals'] = {}", "alias = exec", "alias = getattr", "alias = builtins.setattr",
            'from builtins import globals as alias', 'from builtins import exec as alias',
            "getattr(builtins, 'exec')('originals = {}')",
            "getattr(builtins, 'globals')()['originals'] = {}",
        ):
            with self.subTest(statement=statement):
                self.write(declaration + table)
                self.run_check(label='#827 dynamic-write original')
                self.write(declaration + statement + '\n' + table)
                self.assertIn('unsupported snapshot dynamic write',
                              self.run_check(1, label='#827 dynamic-write rejected'),
                              '#827 dynamic-write capability must be rejected')
                self.write(declaration + table)
                self.run_check(label='#827 dynamic-write restored')

    def test_snapshot_allows_only_display_name_reflection(self):
        declaration = "originals = {'alias.rs': source.read_text()}\n"
        table = "mutants = [('sample', 'alias.rs', 'before', 'after')]\n"
        for expression in ("getattr(signature, '__name__')", "getattr(signature, '__name__', 'signature')"):
            self.write(declaration + 'label = ' + expression + '\n' + table)
            self.run_check(label='#827 display-name original')
            for bad in (expression.replace("'__name__'", "'exec'"),
                        expression.replace("'__name__'", 'attribute')):
                self.write(declaration + 'label = ' + bad + '\n' + table)
                self.assertIn('snapshot dynamic write', self.run_check(1, label='#827 display-name rejected'),
                              '#827 display-name must reject capability or unknown reflection')
            self.write(declaration + 'label = ' + expression + '\n' + table)
            self.run_check(label='#827 display-name restored')

    def check_snapshot_occurrences(self, statements, diagnostic, label, *,
                                   snapshot_values="'alias.rs': source.read_text()"):
        """Paired controls use the real scanner, never execute mutation scripts."""
        for name in ('originals', 'snapshots'):
            declaration = f"{name} = {{{snapshot_values}}}\n"
            table = f"mutants = [('sample', source, {name}['alias.rs'], 'after')]\n"
            for statement in statements:
                with self.subTest(name=name, statement=statement):
                    self.write(declaration + table)
                    self.run_check(label=f'#827 {label} original')
                    self.write(declaration + statement.replace('SNAPSHOT', name) + '\n' + table)
                    self.assertIn(diagnostic, self.run_check(1, label=f'#827 {label} rejected'),
                                  f'#827 {label} must reject the unsupported occurrence')
                    self.write(declaration + table)
                    self.run_check(label=f'#827 {label} restored')

    def test_snapshot_attribute_occurrences_fail_closed(self):
        self.check_snapshot_occurrences((
            "sys.modules[__name__].SNAPSHOT = {'alias.rs': 'other'}",
            'with x as mod.SNAPSHOT:\n    pass',
            'del mod.SNAPSHOT',
            'alias = mod.SNAPSHOT',
            'consume(mod.SNAPSHOT)',
            'mod.SNAPSHOT.clear()',
            'alias = sys.modules[__name__].SNAPSHOT; alias.clear()',
            "mod.SNAPSHOT['alias.rs'] = 'other'",
            'mod.SNAPSHOT += {}',
        ), 'unsupported snapshot attribute', 'attribute-occurrence')
        # A namespace reference is forbidden even without a bare Name binding.
        self.write('consume(module.originals)\nmutants = [("sample", source, "before", "after")]\n')
        self.assertIn('unsupported snapshot attribute', self.run_check(1, label='#827 unbound-attribute rejected'),
                      '#827 unbound-attribute must not require a bare snapshot Name')

    def test_snapshot_namespace_keys_fail_closed(self):
        self.check_snapshot_occurrences((
            "namespace['SNAPSHOT'] = {}",
            "del namespace['SNAPSHOT']",
            "consume(namespace['SNAPSHOT'])",
            "operator.setitem(namespace, 'SNAPSHOT', {})",
            "operator.attrgetter('SNAPSHOT')(module)",
            "key = 'SNAPSHOT'; namespace[key] = {}",
            "getattr(module, 'SNAPSHOT')",
            "setattr(module, 'SNAPSHOT', {})",
            "globals()['SNAPSHOT'] = {}",
            "vars(module)['SNAPSHOT'] = {}",
            "module.__dict__['SNAPSHOT'] = {}",
        ), 'unsupported snapshot dynamic write', 'namespace-key')

    def test_snapshot_reads_cannot_be_nested_write_targets(self):
        self.check_snapshot_occurrences((
            "SNAPSHOT['alias.rs'][0] = 'other'",
            "del SNAPSHOT['alias.rs'][0]",
            "SNAPSHOT['alias.rs'][0] += 'other'",
            "SNAPSHOT['alias.rs'].field = 'other'",
            "del SNAPSHOT['alias.rs'].field",
            "with x as SNAPSHOT['alias.rs'][0]:\n    pass",
        ), 'unsupported snapshot nested mapping write', 'nested-write')

    def test_snapshot_non_name_identifier_fields_fail_closed(self):
        self.check_snapshot_occurrences((
            'namespace.update(SNAPSHOT={})',
            'from module import SNAPSHOT as alias',
            'import SNAPSHOT as alias',
            'import package.SNAPSHOT as alias',
            'from package.SNAPSHOT import value as alias',
            'match value:\n    case Container(SNAPSHOT=alias):\n        pass',
        ), 'unsupported snapshot named occurrence', 'named-occurrence')

    def test_snapshot_call_arguments_and_unknown_uses(self):
        for name in ('originals', 'snapshots'):
            declaration = f"{name} = {{'alias.rs': source.read_text()}}\n"
            table = f"mutants = [('sample', source, {name}['alias.rs'], 'after')]\n"
            good = declaration + f'copy = dict({name})\nconsume(dict({name}))\n' + table
            for statement in (
                f'consume({name})', f'consume(data={name})', f'consume([{name}])',
                f'consume(consume({name}))', f'consume(data=consume({name}))',
                f'builtins.dict({name})', f'factory()({name})',
                f'dict({name}, other)', f'dict({name}, extra=1)', f'dict(data={name})',
                f'dict({name}, **{{}})', f'dict({name}, *[])',
                f'dict(*{name})', f'dict(**{name})', f'dict(*[{name}])',
            ):
                with self.subTest(name=name, statement=statement):
                    self.write(good)
                    self.run_check(label='#827 exact-dict original')
                    self.write(declaration + statement + '\n' + table)
                    self.assertIn('unsupported snapshot', self.run_check(1, label='#827 exact-dict rejected'),
                                  '#827 exact-dict must reject every other call shape')
                    self.write(good)
                    self.run_check(label='#827 exact-dict restored')
        self.check_snapshot_occurrences((
            'alias = SNAPSHOT', 'return_value = [SNAPSHOT]',
            'consume(*SNAPSHOT)', 'consume(**SNAPSHOT)',
            'SNAPSHOT()', 'for item in SNAPSHOT:\n    pass',
        ), 'mapping escape', 'unknown-occurrence')

    def test_snapshot_call_arguments_cannot_mutate_mapping(self):
        self.check_snapshot_occurrences((
            "import operator\noperator.setitem(SNAPSHOT, 'alias.rs', 'missing')",
            "from operator import setitem\nsetitem(SNAPSHOT, 'alias.rs', 'missing')",
            "dict.__setitem__(SNAPSHOT, 'alias.rs', 'missing')",
            "dict.update(SNAPSHOT, {'alias.rs': 'missing'})",
            'dict.clear(SNAPSHOT)',
            "def change(data):\n    data['alias.rs'] = 'missing'\nchange(SNAPSHOT)",
            "def change(data):\n    data['alias.rs'] = 'missing'\nchange(data=SNAPSHOT)",
        ), 'unsupported snapshot', 'call-mutation')

    def test_snapshot_read_chains_reject_mutating_methods(self):
        for name in ('originals', 'snapshots'):
            declaration = (f"import json\n{name} = {{'alias.rs': source.read_text(), "
                           "'nested': json.loads('{\"list\": []}')}\n")
            table = f"mutants = [('sample', source, {name}['alias.rs'], 'after')]\n"
            for statement in (
                "SNAPSHOT['nested'].update({'value': 'missing'})",
                "SNAPSHOT['nested']['list'].append('missing')",
                "SNAPSHOT['nested'].clear()",
                "SNAPSHOT['nested'].pop('list')",
                "SNAPSHOT['nested'].unknown_method()",
                "SNAPSHOT['nested']()",
                "SNAPSHOT.get('nested').update({'value': 'missing'})",
                "SNAPSHOT['alias.rs'].split().append('missing')",
                "SNAPSHOT['alias.rs'].split()[0].unknown_method()",
                "SNAPSHOT['nested'].field.clear()",
            ):
                with self.subTest(name=name, statement=statement):
                    self.write(declaration + table)
                    self.run_check(label='#827 read-method original')
                    self.write(declaration + statement.replace('SNAPSHOT', name) + '\n' + table)
                    self.assertIn('unsupported snapshot read method',
                                  self.run_check(1, label='#827 read-method rejected'),
                                  '#827 read-method must reject mutations at every chain depth')
                    self.write(declaration + table)
                    self.run_check(label='#827 read-method restored')

    def test_snapshot_read_chains_preserve_concrete_readonly_methods(self):
        for name in ('originals', 'snapshots'):
            declaration = f"{name} = {{'alias.rs': source.read_text(), 'bytes': source.read_bytes()}}\n"
            table = f"mutants = [('sample', source, {name}['alias.rs'], 'after')]\n"
            for expression in (
                "SNAPSHOT['bytes'].decode()", "SNAPSHOT['alias.rs'].count('before')",
                "SNAPSHOT['alias.rs'].encode()", "SNAPSHOT['alias.rs'].replace('before', 'after')",
                "SNAPSHOT['alias.rs'].split()", "SNAPSHOT['alias.rs'].strip()",
                "SNAPSHOT['bytes'].decode().split()[0].strip().count('before')",
                "SNAPSHOT.get('alias.rs').strip()",
            ):
                with self.subTest(name=name, expression=expression):
                    good = declaration + expression.replace('SNAPSHOT', name) + '\n' + table
                    self.write(good)
                    self.run_check(label='#827 readonly-chain original')
                    self.write(good.replace(expression.replace('SNAPSHOT', name),
                                            expression.replace('SNAPSHOT', name) + '.clear()'))
                    self.assertIn('unsupported snapshot read method',
                                  self.run_check(1, label='#827 readonly-chain rejected'),
                                  '#827 readonly-chain must not end validation at the first Call')
                    self.write(good)
                    self.run_check(label='#827 readonly-chain restored')

    def test_snapshot_dict_copy_checks_outer_read_chain(self):
        self.check_snapshot_occurrences((
            "dict(SNAPSHOT)['nested'].update({'x': 1})",
            "dict(SNAPSHOT)['nested'].clear()",
            "dict(SNAPSHOT)['nested'].unknown_method()",
            "mutator = dict(SNAPSHOT)['nested'].update; mutator({'x': 1})",
            "dict(SNAPSHOT)['nested']['list'].append('missing')",
            "dict(SNAPSHOT)['alias.rs'].strip().unknown_method()",
        ), 'unsupported snapshot read method', 'dict-outer-chain',
            snapshot_values="'alias.rs': source.read_text(), 'nested': {'list': []}")

    def test_snapshot_read_chain_checks_uncalled_terminal_attribute(self):
        self.check_snapshot_occurrences((
            "mutator = SNAPSHOT['nested'].update; mutator({'x': 1})",
            "mutator = SNAPSHOT.get('nested').update; mutator({'x': 1})",
            "mutator = SNAPSHOT['nested']['list'].append; mutator('missing')",
            "mutator = SNAPSHOT['nested'].field",
            "consume(SNAPSHOT['nested'].clear)",
            "mutator = SNAPSHOT['alias.rs'].strip().unknown_method",
            "mutator = SNAPSHOT.items().clear",
            "mutator = SNAPSHOT.keys().clear",
            "mutator = SNAPSHOT.values().clear",
        ), 'unsupported snapshot read method', 'terminal-attribute',
            snapshot_values="'alias.rs': source.read_text(), 'nested': {'list': []}")

    def test_snapshot_read_chain_preserves_terminal_readonly_forms(self):
        for name in ('originals', 'snapshots'):
            declaration = f"{name} = {{'alias.rs': source.read_text()}}\n"
            table = f"mutants = [('sample', source, {name}['alias.rs'], 'after')]\n"
            for expression in (
                "SNAPSHOT['alias.rs']", "dict(SNAPSHOT)['alias.rs']",
                "SNAPSHOT['alias.rs'].strip", "SNAPSHOT.get('alias.rs').strip",
                "dict(SNAPSHOT)['alias.rs'].strip", "dict(SNAPSHOT)['alias.rs'].strip()",
            ):
                with self.subTest(name=name, expression=expression):
                    expression = expression.replace('SNAPSHOT', name)
                    good = declaration + f'reader = {expression}\n' + table
                    self.write(good)
                    self.run_check(label='#827 terminal-readonly original')
                    self.write(declaration + f'reader = {expression}.clear\n' + table)
                    self.assertIn('unsupported snapshot read method',
                                  self.run_check(1, label='#827 terminal-readonly rejected'),
                                  '#827 terminal-readonly must check the final attribute')
                    self.write(good)
                    self.run_check(label='#827 terminal-readonly restored')

    def check_snapshot_dict_bindings(self, statements, label):
        for name in ('originals', 'snapshots'):
            declaration = f"{name} = {{'alias.rs': source.read_text()}}\n"
            table = "mutants = [('sample', source, 'before', 'after')]\n"
            for statement in statements:
                for copy in ('', f'copy = dict({name})\n'):
                    with self.subTest(name=name, statement=statement, copy=copy):
                        self.write(declaration + copy + table)
                        self.run_check(label=f'#827 {label} original')
                        for body in (statement + '\n' + declaration, declaration + statement + '\n'):
                            self.write(body + copy + table)
                            self.assertIn('unsupported snapshot dict binding',
                                          self.run_check(1, label=f'#827 {label} rejected'),
                                          f'#827 {label} must reject every explicit dict binding')
                        self.write(declaration + copy + table)
                        self.run_check(label=f'#827 {label} restored')
                        # The guard is snapshot-specific, independent of whether
                        # an exact dict(snapshot) call is actually present.
                        self.write(statement + '\n' + table)
                        self.run_check(label=f'#827 {label} no-snapshot')

    def test_snapshot_dict_binding_stores_fail_closed(self):
        self.check_snapshot_dict_bindings((
            'dict = f', 'dict: object = f', 'dict += other',
            'dict, other = pair', '(dict := f)',
            'for dict in values:\n    pass', 'with manager as dict:\n    pass',
            'items = [dict for dict in values]',
            'def dict():\n    pass', 'async def dict():\n    pass',
            'class dict:\n    pass', 'def helper(dict):\n    pass',
            'helper = lambda dict: None',
            'try:\n    pass\nexcept Exception as dict:\n    pass',
            'match value:\n    case dict:\n        pass',
            'match value:\n    case [*dict]:\n        pass',
            'match value:\n    case {**dict}:\n        pass',
            'global dict',
        ), 'dict-store-binding')

    def test_snapshot_dict_binding_deletions_fail_closed(self):
        self.check_snapshot_dict_bindings(('del dict',), 'dict-del-binding')

    def test_snapshot_dict_binding_imports_fail_closed(self):
        self.check_snapshot_dict_bindings((
            'import x as dict', 'import dict', 'import dict.submodule',
            'from x import value as dict', 'from x import dict',
            'from builtins import dict',
        ), 'dict-import-binding')

    def test_snapshot_transformed_reads_cannot_be_write_targets(self):
        self.check_snapshot_occurrences((
            "SNAPSHOT['alias.rs'].split()[0] = 'missing'",
            "del SNAPSHOT['alias.rs'].split()[0]",
            "SNAPSHOT.get('alias.rs').field = 'missing'",
        ), 'unsupported snapshot nested mapping write', 'chained-write')

    def test_snapshot_mapping_views_reject_method_chains(self):
        self.check_snapshot_occurrences((
            'SNAPSHOT.items().clear()', 'SNAPSHOT.keys().clear()', 'SNAPSHOT.values().clear()',
        ), 'unsupported snapshot read method', 'view-read-method')

    def test_snapshot_get_rejects_starred_arguments(self):
        for name in ('originals', 'snapshots'):
            declaration = f"{name} = {{'alias.rs': source.read_text()}}\npair = ('alias.rs', None)\n"
            table = f"mutants = [('sample', source, {name}['alias.rs'], 'after')]\n"
            good = declaration + f"{name}.get('alias.rs', None)\n" + table
            for arguments in ('*pair', "'alias.rs', *pair", "*pair, None"):
                with self.subTest(name=name, arguments=arguments):
                    self.write(good)
                    self.run_check(label='#827 get-starred original')
                    self.write(declaration + f'{name}.get({arguments})\n' + table)
                    self.assertIn('unsupported snapshot mapping method',
                                  self.run_check(1, label='#827 get-starred rejected'),
                                  '#827 get-starred must reject unpacked positional arguments')
                    self.write(good)
                    self.run_check(label='#827 get-starred restored')

    def test_snapshot_compatibility_mapping_calls_have_exact_shapes(self):
        declaration = "originals = {'alias.rs': source.read_text()}\n"
        table = "mutants = [('sample', source, originals['alias.rs'], 'after')]\n"
        for good, bad in (
            ('originals.items()', "originals.items('extra')"),
            ('originals.keys()', 'originals.keys(default=None)'),
            ('originals.values()', 'originals.values(None)'),
            ("originals.get('alias.rs')", "originals.get('alias.rs', None, 'extra')"),
            ("originals.get('alias.rs', None)", "originals.get('alias.rs', default=None)"),
        ):
            with self.subTest(good=good, bad=bad):
                self.write(declaration + good + '\n' + table)
                self.run_check(label='#827 compatibility-call original')
                self.write(declaration + bad + '\n' + table)
                self.assertIn('mapping method', self.run_check(1, label='#827 compatibility-call rejected'),
                              '#827 compatibility-call must match the complete modeled shape')
                self.write(declaration + good + '\n' + table)
                self.run_check(label='#827 compatibility-call restored')
        self.write(declaration + "alias = originals['alias.rs'] = 'other'\n" + table)
        self.assertIn('mapping escape or write', self.run_check(1, label='#827 chained-update rejected'),
                      '#827 chained-update must not inherit the single-target update exception')

    def test_snapshot_dictionary_unpacking_is_rejected_in_subscript_values(self):
        for name in ('originals', 'snapshots'):
            declaration = f"{name} = {{'alias.rs': source.read_text()}}\n"
            table = f"mutants = [('sample', source, {name}['alias.rs'], 'after')]\n"
            self.write(declaration + table)
            self.run_check(label='#827 dict-unpack original')
            self.write(declaration.replace("{'alias.rs': source.read_text()}",
                                           "{**{'alias.rs': source.read_text()}}") + table)
            self.assertIn('snapshot dictionary unpacking', self.run_check(1, label='#827 dict-unpack rejected'),
                          '#827 dict-unpack must fail through Unsupported rather than an uncaught exception')
            self.write(declaration + table)
            self.run_check(label='#827 dict-unpack restored')

    def test_snapshot_unique_with_preserves_existing_contexts(self):
        for constructor in ('mutating_sources', 'negative_process.mutating_sources'):
            for wrapper in ('{}', 'def main():\n{}', 'for iteration in [0]:\n{}'):
                body = f"with {constructor}(root, [source]) as snapshots:\n    pass\n"
                body = wrapper.format(body if wrapper == '{}' else ''.join('    ' + s for s in body.splitlines(True)))
                table = "mutants = [('sample', source, 'before', 'after')]\n"
                self.write('from negative_process import mutating_sources\nimport negative_process\n' + body + table)
                self.run_check(label='#827 unique-with original')
                self.write(body.replace(constructor + '(root, [source])', 'unknown()') + table)
                self.assertIn('unsupported snapshot binding', self.run_check(1, label='#827 unique-with unknown'),
                              '#827 unique-with unknown constructor must be rejected')
                self.write('from negative_process import mutating_sources\nimport negative_process\n' + body + table)
                self.run_check(label='#827 unique-with restored')

    def test_snapshot_capture_reads_only_declared_official_sources(self):
        declaration = ("from negative_process import mutating_sources\n"
                       "with mutating_sources(root, [source]) as snapshots:\n    pass\n")
        table = "mutants = [('sample', source, snapshots[source], b'after')]\n"
        self.write(declaration + table)
        self.run_check(label='#827 capture-read original')
        for broken, diagnostic in (
            (table, 'unresolved name snapshots'),
            (table + declaration, 'read before capture'),
            (declaration.replace('[source]', '[]') + table, 'example.rs'),
            (declaration.replace('from negative_process import mutating_sources',
                                 'from other import mutating_sources') + table, 'snapshot constructor'),
            (declaration.replace('from negative_process import mutating_sources',
                                 'def mutating_sources(*args):\n    yield {}') + table, 'snapshot constructor'),
            (declaration + 'mutating_sources = unknown\n' + table, 'snapshot constructor'),
            (declaration + 'negative_process.mutating_sources = unknown\n' + table, 'snapshot constructor'),
            (declaration.replace('root, [source]', 'root, [source], unknown=True') + table, 'snapshot constructor'),
            (declaration.replace('root, [source]', 'root') + table, 'snapshot constructor'),
            (declaration.replace('with mutating_sources', 'def helper():\n    with mutating_sources')
             .replace('    pass', '        pass') + table, 'across function scopes'),
        ):
            with self.subTest(diagnostic=diagnostic):
                self.write(broken)
                self.assertIn(diagnostic, self.run_check(1, label='#827 capture-read rejected'),
                              '#827 capture-read must stay bound to declared official source')
                self.write(declaration + table)
                self.run_check(label='#827 capture-read restored')

    def test_snapshot_assign_requires_unconditional_module_or_function_scope(self):
        declaration = "originals = {'alias.rs': source.read_text()}\n"
        table = "mutants = [('sample', source, 'before', 'after')]\n"
        for scope in ('', 'def main():\n', 'def arbitrary_name():\n', 'async def collect():\n'):
            good = scope + (textwrap.indent(declaration + table, '    ') if scope else declaration + table)
            for wrapper in ('if True:\n', 'for i in [0]:\n', 'class Scope:\n', 'with unknown():\n'):
                self.write(good)
                self.run_check(label='#827 declaration-scope original')
                bad = wrapper + textwrap.indent(declaration, '    ') + table
                self.write(scope + (textwrap.indent(bad, '    ') if scope else bad))
                self.assertIn('requires an unconditional module/function Assign',
                              self.run_check(1, label='#827 declaration-scope rejected'),
                              '#827 declaration-scope must reject conditional or class contexts')
                self.write(good)
                self.run_check(label='#827 declaration-scope restored')

    def test_snapshot_function_values_and_paths_stay_in_scope(self):
        for name in ('originals', 'snapshots'):
            for expression in ("{'alias.rs': source.read_text()}",
                               "{p.name: p.read_text() for p in [source]}"):
                key = 'alias.rs' if 'alias.rs' in expression else 'example.rs'
                declaration = f'{name} = {expression}\n'
                for target, before in ((repr(key), "'before'"), ('source', f'{name}[{key!r}]')):
                    table = f"mutants = [('sample', {target}, {before}, 'after')]\n"
                    good = 'def renamed():\n' + textwrap.indent(declaration + table, '    ')
                    for bad, diagnostic in (
                        ('def renamed():\n' + textwrap.indent(declaration, '    ') + table, 'unsupported snapshot'),
                        ('def renamed():\n' + textwrap.indent(declaration, '    ')
                         + 'def sibling():\n' + textwrap.indent(table, '    '), 'unsupported snapshot'),
                        ('def renamed():\n' + textwrap.indent(table + declaration, '    '), 'unresolved'),
                        (good + declaration, 'unsupported snapshot'),
                    ):
                        self.write(good)
                        self.run_check(label='#827 function-snapshot original')
                        self.write(bad)
                        self.assertIn(diagnostic, self.run_check(1, label='#827 function-snapshot rejected'),
                                      '#827 function-snapshot values and paths must retain local ownership')
                        self.write(good)
                        self.run_check(label='#827 function-snapshot restored')
        # A subscript update is resolved before named(); it must obey the same guard.
        declaration = "originals = {}\noriginals['alias.rs'] = source.read_text()\n"
        table = "mutants = [('sample', source, originals['alias.rs'], 'after')]\n"
        good = 'def collect():\n' + textwrap.indent(declaration + table, '    ')
        self.write(good)
        self.run_check(label='#827 function-update original')
        self.write('def collect():\n' + textwrap.indent(declaration, '    ') + table)
        self.assertIn('across function scopes', self.run_check(1, label='#827 function-update rejected'),
                      '#827 function-update must retain local ownership')
        self.write(good)
        self.run_check(label='#827 function-update restored')

    def test_snapshot_function_updates_cannot_cross_scopes(self):
        declaration = "originals = {'alias.rs': source.read_text()}\n"
        for scope in ('', 'def collect():\n'):
            for update in ("originals['alias.rs'] = source.read_text()\n", "originals.pop('unused.rs', None)\n"):
                for target, before in (("'alias.rs'", "'before'"), ('source', "originals['alias.rs']")):
                    table = f"mutants = [('sample', {target}, {before}, 'after')]\n"
                    body = declaration + update + table
                    good = scope + (textwrap.indent(body, '    ') if scope else body)
                    self.write(good)
                    self.run_check(label='#827 update-scope original')
                    body = declaration + 'def never_called():\n' + textwrap.indent(update, '    ') + table
                    self.write(scope + (textwrap.indent(body, '    ') if scope else body))
                    self.assertIn('snapshot update across function scopes',
                                  self.run_check(1, label='#827 update-scope rejected'),
                                  '#827 update-scope must not execute uncalled nested updates')
                    self.write(good)
                    self.run_check(label='#827 update-scope restored')

    def test_snapshot_empty_placeholder_is_a_generic_capture_binding(self):
        for filename in ('sample/negative.py', 'arbitrary/control.py'):
            self.move_script(filename)
            for name in ('originals', 'snapshots'):
                for constructor in ('mutating_sources', 'negative_process.mutating_sources'):
                    body = (f'from negative_process import mutating_sources\nimport negative_process\n{name} = {{}}\n'
                            f'with {constructor}(root, [source]) as {name}:\n'
                            f"    mutants = [('sample', source, {name}[source], b'after')]\n")
                    self.write(body)
                    self.run_check(label='#827 placeholder original')
                    # Change the observed snapshot value, keeping the real source intact.
                    self.write(body.replace(f'{name}[source]', f"{name}[source].decode().replace('before', 'missing')")
                               .replace("b'after'", "'after'"))
                    self.assertIn('expected=1 actual=0', self.run_check(1, label='#827 placeholder changed-value'),
                                  '#827 placeholder must resolve the capture value')
                    self.write(body)
                    self.run_check(label='#827 placeholder restored')

    def test_snapshot_placeholder_rejects_noncanonical_pairs(self):
        for name in ('originals', 'snapshots'):
            prefix = 'from negative_process import mutating_sources\n'
            initial = f'{name} = {{}}\n'
            capture = f'with mutating_sources(root, [source]) as {name}:\n    pass\n'
            table = "mutants = [('sample', source, 'before', 'after')]\n"
            good = prefix + initial + capture + table
            for pair in (
                initial.replace('{}', "{'key': 'value'}") + capture,
                initial + f"value = {name}['key']\n" + capture,
                initial + f"{name}['key'] = 'value'\n" + capture,
                initial + capture.replace('[source]', f"[\n    {name}['key']\n]"),
                initial + capture + initial,
                capture + initial,
                initial + capture.replace('mutating_sources(root, [source])', 'unknown()'),
                initial + 'if True:\n' + textwrap.indent(capture, '    '),
                'if True:\n' + textwrap.indent(initial, '    ') + capture,
                initial + 'def collect():\n' + textwrap.indent(capture, '    '),
                'def collect():\n' + textwrap.indent(initial + capture, '    '),
            ):
                self.write(good)
                self.run_check(label='#827 placeholder-pair original')
                self.write(prefix + pair + table)
                self.run_check(1, label='#827 placeholder-pair rejected')
                self.write(good)
                self.run_check(label='#827 placeholder-pair restored')
            self.write(good.replace('from negative_process import', 'from unknown import'))
            self.assertIn('snapshot constructor', self.run_check(1, label='#827 placeholder-constructor rejected'),
                          '#827 placeholder must retain official constructor validation')
            self.write(good)
            self.run_check(label='#827 placeholder-constructor restored')

    def decoded_anchor_view(self):
        (self.root / 'src/text.rs').write_text('before\n')
        return textwrap.dedent("""\
            from negative_process import mutating_sources
            text = root / 'src/text.rs'
            cases = [('sample', source, 'before', 'after', 'message')]
            messages = {'sample': 'message'}
            def anchors(texts):
                stale = []
                for name, path, before, _, _ in cases:
                    count = texts[path].count(before)
                    if count != 1:
                        stale.append((name, count))
                assert set(messages) == {case[0] for case in cases}, 'every mutant needs its own named assertion'
                if stale:
                    for name, count in stale: print(f'STALE ANCHOR {name}: {count} matches', file=sys.stderr)
                    raise SystemExit(2)
            with mutating_sources(root, [source, text]) as snapshots:
                originals = {path: snapshots[path].decode() for path in (source, text)}
                anchors(originals)
                mutants = [('view', source, originals[source].strip(), 'after')]
            """)

    def test_snapshot_decoded_view_exact_helper(self):
        body = self.decoded_anchor_view()
        self.write(body)
        self.run_check(label='#827 decoded-view original')
        self.write(body.replace("originals[source].strip()", "originals[source].strip().replace('before', 'missing')"))
        self.assertIn('expected=1 actual=0', self.run_check(1, label='#827 decoded-view changed-value'),
                      '#827 decoded-view must resolve captured bytes')
        self.write(body)
        self.run_check(label='#827 decoded-view restored')

    def test_snapshot_decoded_view_rejects_helper_or_binding_changes(self):
        good = self.decoded_anchor_view()
        changes = [
            good.replace('    stale = []', '    texts.clear()\n    stale = []'),
            good.replace('    stale = []', "    texts[source] = 'missing'\n    stale = []"),
            good.replace('    stale = []', '    leak(texts)\n    stale = []'),
            good.replace('    stale = []', '    return texts\n    stale = []'),
            good.replace('def anchors(texts):', '@decorator\ndef anchors(texts):'),
            good.replace('def anchors(texts):', 'def anchors(texts, extra=None):'),
            good + 'anchors = unknown\n',
            good + 'del anchors\n',
            good + 'anchors.__code__ = poison.__code__\n',
            good + 'callback = anchors\ncallback.__code__ = poison.__code__\n',
            good + 'replace_code(anchors)\n',
            good + 'module.anchors.__code__ = poison.__code__\n',
            good + "sys.modules[__name__].__setattr__('anchors', poison)\n",
            good + 'register(anchors=poison)\n',
            good.replace('anchors(originals)', 'anchors(originals, extra=True)'),
            good.replace('anchors(originals)', 'result = anchors(originals)'),
            good.replace('snapshots[path].decode()', 'snapshots[path]'),
            good.replace('snapshots[path].decode()', 'snapshots[path].decode(errors="ignore")'),
            good.replace('for path in (source, text)', 'for path in (source, text) if path'),
            good.replace('for path in (source, text)', 'for path in [source, text]'),
            good.replace('    anchors(originals)\n', '') + 'anchors(originals)\n',
            good.replace('    originals =', '    anchors(originals)\n    originals ='),
            good[:good.index('with mutating_sources')] + 'if True:\n'
                + textwrap.indent(good[good.index('with mutating_sources'):], '    '),
        ]
        for changed in changes:
            with self.subTest(changed=changed):
                self.write(good)
                self.run_check(label='#827 decoded-helper original')
                self.write(changed)
                self.assertIn('unsupported snapshot', self.run_check(1, label='#827 decoded-helper rejected'),
                              '#827 decoded-helper must reject unverified mapping consumers')
                self.write(good)
                self.run_check(label='#827 decoded-helper restored')

    def exiting_anchor_preflight(self):
        return textwrap.dedent("""\
            from negative_process import mutating_sources
            cases = [('sample', source, 'before', 'after', 'test')]
            all_cases = list(cases)
            if sys.argv[1:] == ['--check-anchors']:
                originals = {path: path.read_text() for path in {case[1] for case in cases}}
                stale = []
                for name, path, before, _after, _test in all_cases:
                    count = originals[path].count(before)
                    if count != 1:
                        stale.append((name, count, path.relative_to(root).as_posix()))
                if stale:
                    for name, count, relative in stale:
                        print(f'STALE ANCHOR {name}: {count} matches in {relative}')
                    print('STALE ANCHORS: negative.py is out of sync with its sources; refusing to run')
                    raise SystemExit(2)
                print(f'{len(all_cases)} anchors: ok')
                raise SystemExit(0)
            with mutating_sources(root, [source]) as originals:
                mutants = [('capture', source, originals[source].decode().strip(), 'after')]
            """)

    def test_snapshot_exiting_preflight_uses_official_capture(self):
        body = self.exiting_anchor_preflight()
        self.write(body)
        self.run_check(label='#827 exiting-preflight original')
        self.write(body.replace('originals[source].decode().strip()',
                                "originals[source].decode().strip().replace('before', 'missing')"))
        self.assertIn('expected=1 actual=0', self.run_check(1, label='#827 exiting-preflight changed-value'),
                      '#827 exiting-preflight must resolve later capture bytes')
        self.write(body)
        self.run_check(label='#827 exiting-preflight restored')

    def test_snapshot_exiting_preflight_rejects_other_control_flow(self):
        good = self.exiting_anchor_preflight()
        changes = [
            good.replace('    raise SystemExit(0)\n', ''),
            good.replace('    raise SystemExit(0)', '    if stale:\n        raise SystemExit(0)'),
            good.replace("['--check-anchors']", "['--unknown']"),
            good.replace('with mutating_sources', 'else:\n    pass\nwith mutating_sources'),
            good.replace('with mutating_sources', "old = originals[source]\nwith mutating_sources"),
            good.replace('with mutating_sources(root, [source])', 'with mutating_sources(root, [originals[source]])'),
            good + 'originals = {}\n',
            good.replace('from negative_process import', 'from other import'),
            good.replace('with mutating_sources', 'with unknown'),
            good.replace('    stale = []', '    originals.clear()\n    stale = []'),
            good.replace('    stale = []', '    leak(originals)\n    stale = []'),
            'if True:\n' + textwrap.indent(good, '    '),
        ]
        for changed in changes:
            with self.subTest(changed=changed):
                self.write(good)
                self.run_check(label='#827 preflight-shape original')
                self.write(changed)
                self.assertIn('unsupported snapshot', self.run_check(1, label='#827 preflight-shape rejected'),
                              '#827 preflight-shape must reject nonexclusive bindings')
                self.write(good)
                self.run_check(label='#827 preflight-shape restored')

    def test_snapshot_comprehension_tracks_real_paths(self):
        other = self.root / 'src/other/example.rs'
        other.parent.mkdir()
        other.write_text('before\n')
        for mapping in ('originals', 'snapshots'):
            for key, filename in (("p.name", 'example.rs'), ("p.stem + '.txt'", 'example.txt'),
                                  ("'src/example.rs'", 'src/example.rs')):
                for read in ('p.read_text()', "p.read_text().strip().replace('unused', 'ignored')",
                             'p.read_bytes().decode()'):
                    with self.subTest(mapping=mapping, key=key, read=read):
                        prefix = ("source = root / 'src'\nother = root / 'src/other/example.rs'\n"
                                  f"{mapping} = {{{key}: {read} for p in [other]}}\n")
                        table = f"mutants = [('sample', {filename!r}, 'before', 'after')]\n"
                        self.write(prefix + table)
                        self.run_check(label='#827 comprehension-path original')
                        anchors, _, _ = CHECK.extract(self.script, self.root)
                        self.assertEqual(len(anchors), 1, '#827 comprehension-path must produce one anchor')
                        self.assertEqual(anchors[0].path, other, '#827 comprehension-path must follow actual read')
                        other.write_text('missing\n')
                        self.assertIn('src/other/example.rs expected=1 actual=0',
                                      self.run_check(1, label='#827 comprehension-path wrong-origin'),
                                      '#827 comprehension-path must detect actual source drift')
                        other.write_text('before\n')
                        self.run_check(label='#827 comprehension-path restored')

    def test_snapshot_comprehension_rejects_ambiguous_or_missing_origins(self):
        prefix = "source = root / 'src'\npath = source / 'example.rs'\n"
        table = "mutants = [('sample', 'example.rs', 'before', 'after')]\n"
        good = "originals = {p.name: p.read_text() for p in [path]}\n"
        for bad, diagnostic in (
            ("originals = {p.name: p.read_text() for p in [path, path]}\n", 'duplicate comprehension keys'),
            ("originals = {p.name: p.read_text() for p in []}\n", 'missing snapshot key'),
            ("originals = {p.name: p.read_text() for p in [path] if True}\n", 'unconditional single-loop'),
            ("originals = {p.name: p.read_text() for p in [path] for q in [path]}\n", 'unconditional single-loop'),
            ("originals = {p.name: p.read_text() async for p in [path]}\n", 'unconditional single-loop'),
            ("originals = {p.name: 'before' for p in [path]}\n", 'unsupported snapshot origin'),
            ("originals = {**{'example.rs': path.read_text()}}\n", 'dictionary unpacking'),
        ):
            with self.subTest(diagnostic=diagnostic):
                self.write(prefix + good + table)
                self.run_check(label='#827 comprehension-origin original')
                self.write(prefix + bad + table)
                self.assertIn(diagnostic, self.run_check(1, label='#827 comprehension-origin rejected'),
                              '#827 comprehension-origin must reject unprovable origin')
                self.write(prefix + good + table)
                self.run_check(label='#827 comprehension-origin restored')

    def test_snapshot_multiple_mappings_do_not_guess_filename_owner(self):
        first = "originals = {'alias.rs': source.read_text()}\n"
        second = "snapshots = {'alias.rs': source.read_bytes()}\n"
        table = "mutants = [('sample', 'alias.rs', 'before', 'after')]\n"
        self.write(first + table)
        self.run_check(label='#827 mapping-owner original')
        self.write(first + second + table)
        self.assertIn('ambiguous snapshot mappings', self.run_check(1, label='#827 mapping-owner rejected'),
                      '#827 mapping-owner must not guess between mappings')
        self.write(second + table)
        self.run_check(label='#827 mapping-owner restored')
        self.write(second + "mutants = [('sample', source, snapshots['alias.rs'], b'after')]\n")
        self.run_check(label='#827 declared-byte-read original')
        self.write(second.replace('source.read_bytes()', "b'missing'")
                   + "mutants = [('sample', source, snapshots['alias.rs'], b'after')]\n")
        self.assertIn('actual=0', self.run_check(1, label='#827 declared-byte-read rejected'),
                      '#827 declared-byte-read must use the declared mapping value')
        self.write(second + table)
        self.run_check(label='#827 declared-byte-read restored')
        self.write("snapshots = {p.name: p.read_bytes() for p in [source]}\n"
                   + "mutants = [('sample', source, snapshots['example.rs'], b'after')]\n")
        self.run_check(label='#827 comprehension-byte-read original')
        self.write("snapshots = {p.name: p.read_bytes() for p in [source, source]}\n"
                   + "mutants = [('sample', source, snapshots['example.rs'], b'after')]\n")
        self.assertIn('duplicate comprehension keys', self.run_check(1, label='#827 comprehension-byte-read rejected'),
                      '#827 comprehension-byte-read must reject duplicate origins')
        self.write(second + table)
        self.run_check(label='#827 comprehension-byte-read restored')

    def test_snapshot_dictionary_uses_last_literal_key(self):
        other = self.root / 'src/other.rs'
        other.write_text('missing')
        prefix = "other = root / 'src/other.rs'\noriginals = {'alias.rs': source.read_text(), 'alias.rs': other.read_text()}\n"
        table = "mutants = [('sample', 'alias.rs', 'before', 'after')]\n"
        self.write(prefix + table)
        self.assertIn('src/other.rs expected=1 actual=0', self.run_check(1, label='#827 literal-last-key rejected'),
                      '#827 literal-last-key must use final value')
        other.write_text('before')
        self.run_check(label='#827 literal-last-key restored')

    def test_snapshot_unmodeled_writes_cannot_fall_back_to_basename(self):
        other = self.root / 'src/other.rs'
        other.write_text('missing')
        prefix = "other = root / 'src/other.rs'\noriginals = {'alias.rs': source.read_text()}\n"
        table = "mutants = [('sample', 'alias.rs', 'before', 'after')]\n"
        self.write(prefix + table)
        self.run_check(label='snapshot-write-original')
        for mapping in ('originals', 'snapshots', 'self.originals', 'self.snapshots'):
            for update in (
                f"{mapping}.update({{'alias.rs': other.read_text()}})",
                f"{mapping}.setdefault('alias.rs', other.read_text())",
                f"{mapping}.clear()", f"{mapping}.pop('alias.rs')", f"{mapping}.popitem()",
                f"{mapping}.__setitem__('alias.rs', 'before')", f"{mapping}.__delitem__('alias.rs')",
                f"{mapping}['alias.rs'] += 'before'", f"del {mapping}['alias.rs']",
                f"{mapping} |= {{'alias.rs': 'before'}}", f"del {mapping}",
                f"{mapping}: dict = {{'alias.rs': 'before'}}",
            ):
                with self.subTest(update=update):
                    self.write(prefix + update + '\n' + table)
                    self.assertIn('unsupported snapshot', self.run_check(1, label='snapshot-unmodeled-write'))
        for mapping in ('snapshots', 'self.originals', 'self.snapshots'):
            self.write(prefix + f"{mapping}['alias.rs'] = other.read_text()\n" + table)
            self.assertIn('unsupported snapshot', self.run_check(1, label='snapshot-unmodeled-assignment'))
        for mapping in ('self.originals', 'self.snapshots'):
            self.write(prefix + f"{mapping} = {{'alias.rs': other.read_text()}}\n" + table)
            self.assertIn('unsupported snapshot', self.run_check(1, label='snapshot-attribute-binding'))
        for mapping in ('originals', 'snapshots'):
            self.write(prefix + f"alias = {mapping}\nalias.update({{'alias.rs': other.read_text()}})\n" + table)
            self.assertIn('mapping escape', self.run_check(1, label='snapshot-alias-write'))
            self.write(prefix + f"shared = {{'alias.rs': source.read_text()}}\n{mapping} = shared\n" + table)
            self.assertIn('unsupported snapshot', self.run_check(1, label='snapshot-shared-initializer'))
        self.write(prefix + table)
        self.run_check(label='snapshot-write-restored')

    def test_snapshot_pop_only_allows_unused_key_removal(self):
        prefix = "originals = {'alias.rs': source.read_text(), 'unused.rs': source.read_text()}\n"
        table = "mutants = [('sample', 'alias.rs', 'before', 'after')]\n"
        self.write(prefix + "originals.pop('unused.rs', None)\n" + table)
        self.run_check(label='snapshot-unused-pop-original')
        self.write(prefix + "originals.pop('alias.rs', None)\n" + table)
        self.assertIn('removed snapshot key', self.run_check(1, label='snapshot-removed-target'))
        self.write(prefix + "originals.pop('alias.rs', None)\n" + table.replace("'before'", "originals['alias.rs']").replace("'alias.rs', originals", 'source, originals'))
        self.assertIn('removed snapshot key', self.run_check(1, label='snapshot-removed-value'))
        for removal in ("other = originals.pop('unused.rs', None)\n",
                        "originals.pop(key, None)\n", "originals.pop('unused.rs', 'value')\n",
                        "originals.pop('unused.rs', default=None)\n"):
            self.write(prefix + removal + table)
            self.assertIn('unsupported snapshot mapping method', self.run_check(1, label='snapshot-pop-unknown-shape'))
        self.write(prefix + "originals.pop('alias.rs', None)\noriginals['alias.rs'] = source.read_text()\n" + table)
        self.run_check(label='snapshot-pop-reassigned-restored')
        for statement in ("originals.pop('alias.rs', None); value = originals['alias.rs']\n",
                          "originals.pop('alias.rs', None); originals['alias.rs'] = source.read_text()\n",
                          "originals['alias.rs'] = source.read_text(); value = originals['alias.rs']\n"):
            self.write(prefix + statement + table)
            self.assertIn('same-line snapshot update', self.run_check(1, label='snapshot-same-line-order'))
        self.write(prefix + table)
        self.run_check(label='snapshot-order-restored')

    def test_snapshot_target_follows_rebased_transform_chain(self):
        other = self.root / 'src/other.rs'
        other.write_text(' before ')
        prefix = """other = root / 'src/other.rs'
originals = {'alias.rs': source.read_text()}
originals['alias.rs'] = other.read_text()
originals['alias.rs'] = originals['alias.rs'].strip()
originals['alias.rs'] = originals['alias.rs'].replace('unused', 'ignored')
"""
        table = "mutants = [('sample', 'alias.rs', originals['alias.rs'], 'after')]\n"
        self.write(prefix + table)
        anchors, _, _ = CHECK.extract(self.script, self.root)
        self.assertEqual(anchors[0].path, other, 'snapshot-transform-latest-read-path')
        self.assertEqual(anchors[0].before, 'before', 'snapshot-transform-current-value')
        self.run_check(label='snapshot-chain-original')
        # Keep the old source matching so a stale path would silently pass.
        table = "mutants = [('sample', 'alias.rs', 'before', 'after')]\n"
        self.write(prefix + table)
        other.write_text('missing')
        self.assertIn('src/other.rs expected=1 actual=0', self.run_check(1, label='snapshot-chain-wrong-source'))
        other.write_text(' before ')
        self.run_check(label='snapshot-chain-restored')
        self.write(prefix + "originals['alias.rs'] = 'before'\n" + table)
        self.assertIn('unsupported snapshot origin', self.run_check(1, label='snapshot-chain-lost-origin'))
        self.write(prefix + table)
        self.run_check(label='snapshot-chain-origin-restored')

    def test_environment_gaps_fail_strict_mode_only(self):
        self.write("mutants = [('sample', source, 'before', 'after')]\nassert os.environ.get('PDRS_BASELINE')\nassert env['CARGO_TARGET_DIR']\n")
        self.assertIn('MISSING_ENV PDRS_BASELINE', self.run_check(1, anchors_only=False, label='missing-environment'))
        self.run_check(anchors_only=False, env={'PDRS_BASELINE': 'fixture', 'CARGO_TARGET_DIR': 'fixture'})
        self.assertIn('MISSING_ENV', self.run_check(anchors_only=True))
        self.assertEqual(CHECK.environment_requirements(ast.parse("env.get('CARGO_TARGET_DIR', 'default')\nenv.setdefault('CARGO_HOME', 'default')\nenv.get('PDRS_CASE')")), set())
        self.assertEqual(CHECK.environment_requirements(ast.parse("env['CARGO_TARGET_DIR'] = 'scratch'\nenv['CARGO_TARGET_DIR']\nenv.setdefault('CARGO_HOME', 'default')\nenv.get('CARGO_HOME')")), set())
        self.assertEqual(CHECK.environment_requirements(ast.parse("os.getenv('PDRS_BASELINE')")), {'PDRS_BASELINE'})
        # A default in one mapping must not hide a caller read from another.
        self.assertEqual(CHECK.environment_requirements(ast.parse("env['CARGO_TARGET_DIR'] = 'scratch'\nos.environ['CARGO_TARGET_DIR']")), {'CARGO_TARGET_DIR'})
        self.assertEqual(CHECK.environment_requirements(ast.parse("if condition:\n env['PDRS_BASELINE'] = 'scratch'\nenv['PDRS_BASELINE']")), {'PDRS_BASELINE'})
        self.assertEqual(CHECK.environment_requirements(ast.parse("env['PDRS_BASELINE'] = env['PDRS_BASELINE']")), {'PDRS_BASELINE'})
        self.assertEqual(CHECK.environment_requirements(ast.parse("def one():\n env['CARGO_HOME'] = 'scratch'\ndef two():\n env.get('CARGO_HOME')")), {'CARGO_HOME'})
        self.assertEqual(CHECK.environment_requirements(ast.parse("env['PDRS_BASELINE'] = 'provided'\nenv = dict(os.environ)\nenv['PDRS_BASELINE']")), {'PDRS_BASELINE'})
        self.assertEqual(CHECK.environment_requirements(ast.parse("env['PDRS_BASELINE'] = 'provided'\ndef run(env):\n env['PDRS_BASELINE']")), {'PDRS_BASELINE'})
        self.assertEqual(CHECK.environment_requirements(ast.parse("env.setdefault('PDRS_BASELINE', '')\nenv['PDRS_BASELINE']")), {'PDRS_BASELINE'})

    def test_two_argument_environment_defaults_fail_then_restore(self):
        table = "mutants = [('sample', source, 'before', 'after')]\n"
        for reader in ('env.get', 'os.environ.get', 'os.getenv'):
            for default in ("''", 'None', 'False', '0', 'unknown_default'):
                with self.subTest(reader=reader, default=default):
                    self.write(table + f"{reader}('CARGO_TARGET_DIR', 'cache')\n")
                    self.run_check(anchors_only=False)
                    self.write(table + f"{reader}('CARGO_TARGET_DIR', {default})\n")
                    self.assertIn('MISSING_ENV CARGO_TARGET_DIR', self.run_check(
                        1, anchors_only=False, label=f'env-default-{reader}-{default}'))
                    self.run_check(anchors_only=False, env={'CARGO_TARGET_DIR': 'cache'}, label='caller-env-restored')
                    self.assertIn('MISSING_ENV', self.run_check(anchors_only=True))
                    self.write(table + f"{reader}('CARGO_TARGET_DIR', str(root / 'target'))\n")
                    self.run_check(anchors_only=False, label='default-path-restored')

    def test_getenv_keyword_default_and_per_read_fallback(self):
        table = "mutants = [('sample', source, 'before', 'after')]\n"
        self.write(table + "os.getenv('CARGO_HOME', default='cache')\n")
        self.run_check(anchors_only=False)
        self.write(table + "os.getenv('CARGO_HOME', default='')\n")
        self.assertIn('MISSING_ENV CARGO_HOME', self.run_check(1, anchors_only=False, label='empty-keyword-default'))
        self.write(table + "env.get('CARGO_HOME', 'cache')\nenv.get('CARGO_HOME', '')\n")
        self.assertIn('MISSING_ENV CARGO_HOME', self.run_check(1, anchors_only=False, label='fallback-is-not-assignment'))
        self.write(table + "env['CARGO_HOME'] = 'cache'\nenv.get('CARGO_HOME', '')\n")
        self.run_check(anchors_only=False, label='assigned-env-restored')

    def test_keyword_environment_keys_and_empty_setdefault_are_required(self):
        table = "mutants = [('sample', source, 'before', 'after')]\n"
        for function in ('os.getenv', 'os.environ.get', 'os.environ.setdefault'):
            default_name = 'value' if function.endswith('setdefault') else 'default'
            for key in ("'CARGO_TARGET_DIR'", "key='CARGO_TARGET_DIR'"):
                for default in ('', f', {default_name}=None', f", {default_name}=''", f', {default_name}=unknown'):
                    with self.subTest(function=function, key=key, default=default):
                        self.write(table + f'{function}({key}{default})\n')
                        self.assertIn('MISSING_ENV CARGO_TARGET_DIR', self.run_check(
                            1, anchors_only=False, label=f'keyword-environment-required-{function}'))
                        self.run_check(anchors_only=False, env={'CARGO_TARGET_DIR': 'supplied'},
                                       label='keyword-environment-supplied')
                self.write(table + f"{function}({key}, {default_name}='cache')\n")
                self.run_check(anchors_only=False, label='keyword-environment-default-restored')
        self.write(table + "os.environ.setdefault('CARGO_TARGET_DIR', '')\n")
        self.assertIn('MISSING_ENV CARGO_TARGET_DIR', self.run_check(
            1, anchors_only=False, label='empty-setdefault-required'))
        self.write(table + "os.environ.setdefault('CARGO_TARGET_DIR', 'cache')\nos.environ.get(key='CARGO_TARGET_DIR')\n")
        self.run_check(anchors_only=False, label='setdefault-assignment-restored')

    def test_unknown_script_and_partial_table_fail_closed(self):
        self.run_check()
        other = self.fixtures / 'sample/negative_new.py'
        other.write_text("print('would execute')\n")
        self.assertIn('no supported mutation anchors', self.run_check(1))
        other.unlink()
        self.write("mutants = [('sample', source, 'before', 'after'), dynamic_mutation()]\n")
        self.assertIn('unsupported mutation row', self.run_check(1))
        self.write("mutants = unknown_format()\n")
        self.assertIn('unsupported mutation table', self.run_check(1))

    def test_controls_and_arbitrary_filenames_are_discovered(self):
        hidden_source = self.root / 'src/hidden.rs'
        hidden_source.write_text('before\n')
        for filename in ('controls.py', 'mutations.py', 'unusual.py'):
            with self.subTest(filename=filename):
                destination = self.script.with_name(filename)
                destination.write_text(self.script.read_text().replace('src/example.rs', 'src/hidden.rs'))
                self.run_check(label=f'discover-{filename}')
                hidden_source.write_text('missing\n')
                report = self.run_check(1, label=f'broken-{filename}')
                self.assertIn(f'sample/{filename}', report)
                self.assertIn('actual=0', report)
                hidden_source.write_text('before\n')
                self.run_check(label=f'restored-{filename}')
                destination.unlink()
        other = self.fixtures / 'sample/runner.py'
        other.write_text("raise RuntimeError('must never execute')\n")
        self.assertIn('runner.py: no supported mutation anchors', self.run_check(1, label='unregistered-helper'))
        registered = f"{MARKER_PREFIX} synthetic helper without a table\nraise RuntimeError('must never execute')\n"
        other.write_text(registered)
        self.assertIn('UNSUPPORTED sample/runner.py: synthetic helper without a table',
                      self.run_check(label='registered-helper'))
        other.write_text("raise RuntimeError('must never execute')\n")
        self.assertIn('runner.py: no supported mutation anchors',
                      self.run_check(1, label='removed-helper'))
        other.write_text(registered)
        self.run_check(label='restored-helper')
        other.unlink()
        self.run_check()

    def test_table_rewrites_fail_but_selection_is_supported(self):
        table = "mutants = [('sample', source, 'before', 'after')]\n"
        self.write(table + "mutants = [item for item in mutants if item[0] in selected]\n")
        self.run_check()
        self.write(table + "mutants[0] = ('broken', source, 'MISSING', 'after')\n")
        self.assertIn('in-place mutation table', self.run_check(1))
        self.write(table + "mutants = [(name, path, 'MISSING', after) for name, path, before, after in mutants]\n")
        self.assertIn('unsupported mutation table comprehension', self.run_check(1))
        self.write(table + "mutants.clear()\n")
        self.assertIn('unsupported mutation table update', self.run_check(1))

    def test_unsupported_marker_is_explicit_and_fails_closed(self):
        self.write("raise RuntimeError('must not run')\n")
        self.header('synthetic unsupported format')
        self.assertIn('UNSUPPORTED sample/negative.py: synthetic unsupported format',
                      self.run_check(label='#977 marker-present'),
                      '#977 a marker with a reason must exempt its script')
        # #977 negative 1: a marker without a reason must not exempt the script.
        self.write("raise RuntimeError('must not run')\n")
        self.header('')
        self.assertIn('needs a nonempty reason',
                      self.run_check(1, label='#977 marker-empty-reason'),
                      '#977 an empty marker reason must fail closed')
        # #977 negative 2: a marker after the first 20 lines is not an exemption.
        self.marker_at_line(21, 'too late')
        report = self.run_check(1, label='#977 marker-after-limit')
        self.assertIn('must be within the first 20 lines, found line 21', report,
                      '#977 late marker must be reported with its line')
        # Line 20 is the last accepted position.
        self.marker_at_line(20, 'last allowed line')
        self.assertIn('UNSUPPORTED sample/negative.py: last allowed line',
                      self.run_check(label='#977 marker-at-limit'),
                      '#977 a marker on line 20 must be accepted')
        # #977 negative 3: a near-miss marker prefix is malformed, never ignored.
        self.write("raise RuntimeError('must not run')\n")
        self.script.write_text('# anchor-selfcheck: suported typo\n' + self.script.read_text())
        self.assertIn('malformed unsupported marker on line 1',
                      self.run_check(1, label='#977 malformed-marker'),
                      '#977 a malformed marker prefix must fail closed')
        # #977 negative 4: a second marker cannot smuggle another reason in.
        self.write("raise RuntimeError('must not run')\n")
        self.header('first reason is the only reason')
        lines = self.script.read_text().splitlines(keepends=True)
        self.script.write_text(''.join(lines[:2]) + f'{MARKER_PREFIX} second reason\n' + ''.join(lines[2:]))
        self.assertIn('duplicate unsupported marker on line 3',
                      self.run_check(1, label='#977 duplicate-marker'),
                      '#977 a duplicate marker must fail closed')
        # #977 negative 5: the remaining shared signals table cannot name a
        # script that does not exist (the old stale-allowlist failure).
        CHECK.UNSUPPORTED_SIGNALS['gone/negative.py'] = set()
        self.assertIn('stale UNSUPPORTED_SIGNALS entry: gone/negative.py',
                      self.run_check(1, label='#977 stale-signal-missing-script'),
                      '#977 a signal entry for a missing script must fail closed')
        del CHECK.UNSUPPORTED_SIGNALS['gone/negative.py']
        # read_marker itself rejects a missing script instead of returning None.
        self.script.unlink()
        with self.assertRaises(CHECK.Unsupported,
                              msg='#977 missing marker script must fail closed') as caught:
            CHECK.read_marker(self.script)
        self.assertIn('does not exist', str(caught.exception),
                      '#977 missing marker script must fail closed')
        self.write("raise RuntimeError('must not run')\n")
        self.header('restored')
        self.run_check(label='#977 restored')

    def test_marker_text_inside_a_string_is_not_a_comment(self):
        # #977 fix-r1 negative: marker text inside a triple-quoted string is
        # not a Python comment, so the pre-fix line scan wrongly exempted it.
        self.script.write_text(
            '"""Header example:\n# anchor-selfcheck: unsupported example only\n"""\n'
            + self.base + "raise RuntimeError('must not run')\n")
        report = self.run_check(1, label='#977 string-marker-ignored')
        self.assertIn('no supported mutation anchors found', report,
                      '#977 marker text inside a string must not exempt its script')
        # An inline comment after code on the same line is not a whole-line
        # marker either; only whole-line COMMENT tokens decide exemptions.
        self.script.write_text(
            self.base + "value = 1  # anchor-selfcheck: unsupported inline example\n"
            "raise RuntimeError('must not run')\n")
        report = self.run_check(1, label='#977 inline-marker-ignored')
        self.assertIn('no supported mutation anchors found', report,
                      '#977 an inline comment must not exempt its script')
        # The same string text stays inert once a real whole-line marker is
        # present; the comment counts, the string does not.
        self.script.write_text(
            '# anchor-selfcheck: unsupported real reason\n'
            '"""Header example:\n# anchor-selfcheck: unsupported example only\n"""\n'
            + self.base + "raise RuntimeError('must not run')\n")
        self.assertIn('UNSUPPORTED sample/negative.py: real reason',
                      self.run_check(label='#977 string-marker-plus-comment'),
                      '#977 a real comment marker must still exempt its script')
        # A script the tokenizer cannot even read fails closed as an error
        # entry instead of crashing the checker with an unhandled exception.
        self.script.write_text(self.base + '"""unterminated\n')
        report = self.run_check(1, label='#977 untokenizable-script')
        self.assertIn('not tokenizable', report,
                      '#977 an untokenizable script must fail closed')

    def test_missing_tracked_script_fails_closed_then_restores(self):
        # #977 fix-r1: an exempt script without UNSUPPORTED_SIGNALS registration
        # that disappears must fail the real check() entry, not only read_marker.
        self.write("raise RuntimeError('must not run')\n")
        self.header('no-signal exempt helper')
        anchors = self.fixtures / 'sample/anchors.py'
        anchors.write_text(self.base + "mutants = [('sample', source, 'before', 'after')]\n")
        inventory = patch.object(CHECK, 'tracked_fixtures',
                                 return_value={'sample/negative.py', 'sample/anchors.py'})
        with inventory:
            report = self.run_check(label='#977 tracked-present')
            self.assertIn('UNSUPPORTED sample/negative.py: no-signal exempt helper', report,
                          '#977 a tracked no-signal script is exempt while present')
            self.assertIn('CHECKED sample/anchors.py: 1 anchors', report,
                          '#977 a tracked anchor script is checked while present')
            # One script vanishes at a time: the survivor keeps discovery alive,
            # so the nonzero exit can only come from the missing-script rule.
            self.script.unlink()
            report = self.run_check(1, label='#977 tracked-missing')
            self.assertIn('missing fixture script: sample/negative.py', report,
                          '#977 a vanished tracked exempt script must fail closed')
            self.write("raise RuntimeError('must not run')\n")
            self.header('no-signal exempt helper')
            anchors.unlink()
            report = self.run_check(1, label='#977 tracked-anchors-missing')
            self.assertIn('missing fixture script: sample/anchors.py', report,
                          '#977 a vanished tracked anchor script must fail closed')
            anchors.write_text(self.base + "mutants = [('sample', source, 'before', 'after')]\n")
            self.assertIn('UNSUPPORTED sample/negative.py: no-signal exempt helper',
                          self.run_check(label='#977 tracked-restored'),
                          '#977 restoring the files must pass again')

    def test_unreadable_fixture_inventory_fails_closed(self):
        self.write("mutants = [('sample', source, 'before', 'after')]\n")
        with patch.object(CHECK, 'tracked_fixtures',
                          side_effect=CHECK.Unsupported('git ls-files failed: not a git checkout')):
            report = self.run_check(1, label='#977 inventory-unreadable')
        self.assertIn('fixture inventory: git ls-files failed: not a git checkout', report,
                      '#977 an unreadable git inventory must fail closed')

    def test_tracked_fixtures_reads_the_git_index(self):
        tracked = CHECK.tracked_fixtures(PATH.resolve().parents[4], PRODUCTION_FIXTURES.resolve())
        self.assertIn('anchor_selfcheck.py', tracked,
                      '#977 the checker itself must be a tracked fixture')
        self.assertIn('host-factory/peer.py', tracked,
                      '#977 a no-signal exempt script must stay in the git inventory')
        self.assertNotIn('sample/negative.py', tracked,
                         '#977 synthetic scratch fixtures are never tracked')
        self.assertTrue(all(not name.startswith('/') for name in tracked),
                        '#977 inventory names are relative to the fixtures directory')

    def test_each_production_marker_has_a_paired_removal_control(self):
        for name, reason in PRODUCTION_UNSUPPORTED.items():
            with self.subTest(name=name):
                self.assertTrue(reason.strip())
                body = (PATH.parent / name).read_text()
                self.assertIn(MARKER_PREFIX, body[:4096], f'{name} marker')
                self.script = self.fixtures / name
                self.script.parent.mkdir(parents=True, exist_ok=True)
                signals = PRODUCTION_UNSUPPORTED_SIGNALS.get(name, set())
                if signals:
                    CHECK.UNSUPPORTED_SIGNALS[name] = signals
                self.script.write_text(body)
                self.assertIn(f'UNSUPPORTED {name}: {reason}',
                              self.run_check(label=f'#977 marker-present-{name}'),
                              f'#977 {name} must be exempted by its own marker')
                self.script.write_text(self.strip_marker(body))
                self.run_check(1, label=f'#977 marker-removed-{name}')
                self.assertNotIn(f'UNSUPPORTED {name}:',
                                 self.run_check(1, label=f'#977 marker-removed-confirmed-{name}'),
                                 f'#977 {name} must stop being exempt without its marker')
                self.script.write_text(body)
                self.run_check(label=f'#977 marker-restored-{name}')
                print(f'MARKER {name}: present=0 removed=1 restored=0')
                self.script.unlink()
                CHECK.UNSUPPORTED_SIGNALS.pop(name, None)

    def test_helper_exemption_cannot_hide_mutation_signals(self):
        reason = 'helper without mutation definitions'
        self.write("raise RuntimeError('must not execute')\n")
        self.header(reason)
        self.run_check(label='signal-free-helper-original')
        bodies = [f"{table} = [('sample', source, 'before', 'after')]\n" for table in sorted(CHECK.TABLES)]
        bodies += ["mutation('sample', source, 'before', 'after')\n", "mutate('sample', source, 'before', 'after')\n"]
        for body in bodies:
            self.write(body)
            self.header(reason)
            self.assertIn('unsupported exemption signals', self.run_check(1, label='helper-hidden-mutation'))
        self.write("raise RuntimeError('must not execute')\n")
        self.header(reason)
        self.run_check(label='signal-free-helper-restored')

    def check_rebased_helper_registration(self, name, expected):
        label = '#827 rebased-helper ' + name
        self.assertIn(name, PRODUCTION_UNSUPPORTED, label + ' registration')
        self.assertTrue(PRODUCTION_UNSUPPORTED[name].strip(), label + ' reason')
        self.assertEqual(PRODUCTION_UNSUPPORTED_SIGNALS.get(name, set()), expected,
                         label + ' signals')
        self.move_script(name)
        body = (PATH.parent / name).read_text()
        self.script.write_text(body)
        if expected:
            CHECK.UNSUPPORTED_SIGNALS[name] = expected
        self.run_check(label=label + ' original')
        self.script.write_text(body + '\nmutation()\n')
        self.assertIn('unsupported exemption signals', self.run_check(1, label=label + ' added-signal'),
                      label + ' must reject additional mutation signals')
        self.script.write_text(body)
        if expected:
            CHECK.UNSUPPORTED_SIGNALS[name] = set()
            self.assertIn('unsupported exemption signals', self.run_check(1, label=label + ' missing-signal'),
                          label + ' must require every registered signal')
            CHECK.UNSUPPORTED_SIGNALS[name] = expected
        self.script.write_text(self.strip_marker(body))
        report = self.run_check(1, label=label + ' marker-removed')
        self.assertNotIn(f'UNSUPPORTED {name}:', report,
                         label + ' must require its in-script reason')
        self.script.write_text(body)
        self.run_check(label=label + ' restored')

    def test_rebased_negative_suite_registration(self):
        self.check_rebased_helper_registration('negative_suite.py', {'call:mutate'})

    def test_rebased_negative_suite_tests_registration(self):
        self.check_rebased_helper_registration('test_negative_suite.py', {'table:cases'})

    def test_rebased_failure_guard_verdict_registration(self):
        self.check_rebased_helper_registration('failure-guard/verdict.py', set())

    def test_rebased_failure_guard_verdict_tests_registration(self):
        self.check_rebased_helper_registration('failure-guard/test_verdict.py', set())

    def test_rebased_panic_evidence_tests_registration(self):
        self.check_rebased_helper_registration('test_negative_panic.py', {'table:cases'})

    def test_rebased_telemetry_runner_tests_registration(self):
        self.check_rebased_helper_registration('edgeclaw-llm-telemetry/test_controls.py', set())

    def test_rebased_cleanup_probe_registration(self):
        self.check_rebased_helper_registration('session-domain-cleanup/check_cleanup.py',
                                              {'table:CONTROLS', 'table:cases', 'table:controls'})

    def test_rebased_cleanup_launcher_registration(self):
        self.check_rebased_helper_registration('session-domain-cleanup/launcher.py', set())

    def test_registered_exemption_signals_are_exact_and_stale_checked(self):
        for name, signals in PRODUCTION_UNSUPPORTED_SIGNALS.items():
            with self.subTest(name=name):
                reason = PRODUCTION_UNSUPPORTED[name]
                self.move_script(name)
                CHECK.UNSUPPORTED_SIGNALS[name] = signals
                fragments = {signal: (f'{value} = []\n' if kind == 'table' else f'{value}()\n')
                             for signal in signals for kind, value in [signal.split(':')]}
                body = f'{MARKER_PREFIX} {reason}\n' + ''.join(fragments.values())
                self.write(body)
                self.run_check(label=f'exemption-signals-original-{name}')
                # An empty registration pins the absence of every signal; there
                # is no signal to remove or to detect without the entry.
                if signals:
                    for signal in signals:
                        self.write(f'{MARKER_PREFIX} {reason}\n'
                                   + ''.join(fragment for key, fragment in fragments.items() if key != signal))
                        self.assertIn('unsupported exemption signals',
                                      self.run_check(1, label='exemption-signal-removed'))
                    self.write(body + 'mutation()\n')
                    self.assertIn('unsupported exemption signals', self.run_check(1, label='exemption-signal-added'))
                    self.write(body)
                    del CHECK.UNSUPPORTED_SIGNALS[name]
                    self.assertIn('unsupported exemption signals', self.run_check(1, label='exemption-signal-unregistered'))
                    CHECK.UNSUPPORTED_SIGNALS[name] = signals
                else:
                    self.write(body + 'mutation()\n')
                    self.assertIn('unsupported exemption signals', self.run_check(1, label='exemption-signal-added'))
                # A registered signal with no marker is stale and fails closed.
                self.write(''.join(fragments.values()))
                self.assertIn('stale UNSUPPORTED_SIGNALS', self.run_check(1, label='exemption-signal-stale'))
                # So is a registered signal whose script is gone.
                self.script.unlink()
                self.assertIn('stale UNSUPPORTED_SIGNALS',
                              self.run_check(1, label='exemption-signal-missing-script'))
                self.write(body)
                self.run_check(label=f'exemption-signals-restored-{name}')
                del CHECK.UNSUPPORTED_SIGNALS[name]

    def test_each_row_exception_has_a_paired_stale_control(self):
        self.script.unlink()
        for (name, label), reason in CHECK.ROW_EXCEPTIONS.items():
            with self.subTest(name=name):
                self.assertTrue(reason.strip())
                self.script = self.fixtures / name
                self.script.parent.mkdir(parents=True, exist_ok=True)
                good = "('good', source, 'before', 'after')"
                oracle = self.root / 'src/oracle.json'
                oracle.write_text('{"hash": "' + 'a' * 64 + '"}')
                excluded = f"({label!r}, root / 'src/oracle.json', 'a' * 64, '0' * 64)"
                self.write(f'mutants = [{good}, {excluded}]\n')
                self.run_check()
                self.write(f'mutants = [{good}]\n')
                self.assertIn('stale row exceptions', self.run_check(1))
                self.write(f'mutants = [{good}, {excluded}]\n')
                self.run_check()
                print(f'ROW_EXCEPTION {name}/{label}: present=0 missing=1 restored=0')
                self.script.unlink()

    def test_sequential_rows_have_independent_stale_and_reason_checks(self):
        self.move_script('turn-gates/negative.py')
        rows = {name: f"({name!r}, source, unknown_scoped_anchor, 'after')"
                for filename, name in CHECK.SEQUENTIAL_ROWS if filename == 'turn-gates/negative.py'}
        good = "('good', source, 'before', 'after')"
        for name in rows:
            siblings = [row for other, row in rows.items() if other != name]
            body = 'mutants = [' + ', '.join([good, *siblings, rows[name]]) + ']\n'
            self.write(body)
            self.run_check(label=f'sequential-present-{name}')
            self.write('mutants = [' + ', '.join([good, *siblings]) + ']\n')
            self.assertIn('stale row exceptions', self.run_check(1, label=f'sequential-missing-{name}'))
            self.write(body)
            with patch.dict(CHECK.SEQUENTIAL_ROWS, {('turn-gates/negative.py', name): ''}):
                self.assertIn('empty sequential row reason', self.run_check(1, label='empty-sequential-reason'))
            self.run_check(label=f'sequential-restored-{name}')
        # A known row label in another script never inherits this exception.
        self.move_script('other/negative.py')
        self.write(body)
        self.assertIn('unresolved name', self.run_check(1, label='sequential-wrong-file'))

    def test_row_exceptions_validate_shape_and_search_all_sources(self):
        self.script.unlink()
        for (filename, name), reason in CHECK.ROW_EXCEPTIONS.items():
            with self.subTest(filename=filename):
                self.script = self.fixtures / filename
                self.script.parent.mkdir(parents=True, exist_ok=True)
                oracle = self.root / 'src/oracle.json'
                oracle.write_text('{"hash": "' + 'a' * 64 + '"}')
                prefix = "oracle = root / 'src/oracle.json'\nold = json.loads(oracle.read_text())['hash']\n"
                good = "('good', source, 'before', 'after')"
                exempt = f"({name!r}, oracle, old, '0' * 64)"
                table = f'mutants = [{good}, {exempt}]\n'
                self.write(prefix + table)
                self.run_check(label=f'json-exception-{filename}')
                for broken, error in (
                    (exempt.replace(', oracle,', ', source,'), 'JSON hash replacement'),
                    (exempt.replace(', old,', ", 'before',"), 'JSON hash replacement'),
                    (exempt.replace(", '0' * 64)", ', None)'), 'JSON hash replacement'),
                    (exempt.replace(', old,', ', unknown_before,'), 'unresolved name'),
                    (exempt.replace(", '0' * 64)", ')'), 'expected name/path/before/after'),
                ):
                    self.write(prefix + f'mutants = [{good}, {broken}]\n')
                    self.assertIn(error, self.run_check(1, label=f'bad-exception-shape-{filename}'))
                self.write(prefix + table)
                sibling = self.root / 'src/unrelated.rs'
                sibling.write_text('// ' + 'a' * 64)
                self.assertIn('exempted before occurs in source src/unrelated.rs', self.run_check(
                    1, label=f'exempted-source-anchor-{filename}'))
                sibling.unlink()
                self.run_check(label=f'exemption-restored-{filename}')
                oracle.write_text('{"hash": "' + 'b' * 64 + '"}')
                self.write(prefix + table.replace(', old,', ", 'a' * 64,"))
                self.assertIn('absent from JSON target', self.run_check(1, label='json-target-anchor-missing'))
                self.write(prefix + table)
                self.run_check(label='json-target-anchor-restored')
                with patch.dict(CHECK.ROW_EXCEPTIONS, {(filename, name): ''}):
                    self.assertIn('empty row exception reason', self.run_check(1))
                self.script.unlink()

    def test_function_scoped_row_is_checked_instead_of_exempted(self):
        self.script.unlink()
        self.script = self.fixtures / 'turn-runner/recording_negative.py'
        self.script.parent.mkdir()
        consumer = """for name, path, old, new in mutations:
    original = originals[path].decode()
    path.write_text(mutate(original, 'from_agent_input' if name == 'text-value' else None, [(old, new)], expected_count=ANCHOR_COUNTS.get(name, 1)))
"""
        table = "mutations = [('text-value', source, '.cloned()', '.map(convert)')]\n" + consumer
        original = '    fn previous() { value.cloned(); }\n    pub fn from_agent_input() { value.cloned(); }\n    async fn following() { value.cloned(); }\n'
        self.source.write_text(original)
        self.write(table)
        self.run_check(label='scoped-anchor-original')
        self.source.write_text(original.replace('from_agent_input() { value.cloned()', 'from_agent_input() { value.changed()'))
        self.assertIn('actual=0', self.run_check(1, label='scoped-anchor-missing'))
        self.source.write_text(original.replace('from_agent_input() { value.cloned()', 'from_agent_input() { value.cloned().cloned()'))
        self.assertIn('actual=2', self.run_check(1, label='scoped-anchor-duplicate'))
        self.write("ANCHOR_COUNTS = {'text-value': 2}\n" + table)
        self.assertIn('function-scoped count must be one', self.run_check(1, label='scoped-count-cannot-relax'))
        self.write(table)
        self.source.write_text(original.replace('previous', 'from_agent_input'))
        self.assertIn('invalid function-scoped anchor', self.run_check(1, label='duplicate-scope'))
        self.source.write_text(original.replace('from_agent_input', 'renamed'))
        self.assertIn('invalid function-scoped anchor', self.run_check(1, label='missing-scope'))
        self.source.write_text(original)
        for before in (' .cloned()', '.cloned()\n'):
            self.write(f"mutations = [('text-value', source, {before!r}, 'after')]\n" + consumer)
            self.assertIn('invalid function-scoped anchor', self.run_check(1, label='invalid-scoped-shape'))
        for bad in (
            table.replace("'from_agent_input' if name == 'text-value' else None", 'None'),
            table.replace('[(old, new)]', '[(other, new)]'),
            table.replace('originals[path]', 'originals[other]'),
            table.replace('in mutations:', 'in unknown:'),
            table + "mutate(original, None, [(old, new)])\n",
        ):
            self.write(bad)
            self.assertIn('function-scoped', self.run_check(1, label='changed-scoped-consumer'))
        self.write("mutations = [('renamed', source, 'previous', 'after')]\n")
        self.assertIn('stale scoped rows', self.run_check(1, label='stale-scope-rule'))
        self.write(table)
        self.run_check(label='scoped-anchor-restored')
        self.write(table.replace("'.cloned()', '.map(convert)'", "b'.cloned()', b'.map(convert)'"))
        self.assertIn('function-scoped anchors must be strings', self.run_check(1, label='scoped-bytes-rejected'))
        self.write(table)
        self.run_check(label='scoped-string-restored')

    def test_turn_submit_effective_message_anchor(self):
        self.script.unlink()
        self.script = self.fixtures / 'turn-submit/negative.py'
        self.script.parent.mkdir()
        table = "mutant_source = {}\nmutants = [('sample', 'before', 'after', None, None, None)]\n"
        self.write(table + "mutant_anchor = {'sample': ('message', 'specific panic')}\n")
        self.run_check(label='none-marker-with-override')
        for overrides in ('{}', "{'sample': ('message', None)}", "{'sample': ('message', '')}",
                          "{'sample': ('message', '  ')}", "{'sample': ('unknown', 'panic')}",
                          'None', '[]', "{'sample': 'message'}", "{'sample': ('message',)}"):
            self.write(table + f'mutant_anchor = {overrides}\n')
            self.assertIn('anchor', self.run_check(1, label=f'invalid-effective-anchor-{overrides}'))
        self.write(table.replace("'after', None, None, None", "'after', None, None")
                   + "mutant_anchor = {'sample': ('message', 'panic')}\n")
        self.assertIn('six turn-submit fields', self.run_check(1, label='incomplete-submit-row'))
        self.write(table.replace("'after', None, None, None", "'after', None, 'specific panic', None")
                   + 'mutant_anchor = {}\n')
        self.run_check(label='marker-fallback-restored')

    def test_turn_submit_location_span_start_and_workspace(self):
        self.script.unlink()
        self.script = self.fixtures / 'turn-submit/negative.py'
        self.script.parent.mkdir()
        before = 'unwrap_or_else(|poisoned| poisoned.into_inner())'
        table = f"mutant_source = {{}}\nmutants = [('sample', {before!r}, 'unwrap()', None, None, None)]\n"
        override = "mutant_anchor = {'sample': ('location', 'example.rs:1:1')}\n"
        self.source.write_text(before)
        self.write(table + override)
        self.assertIn('unsupported location helper', self.run_check(1, label='unverified-literal-location'))
        self.write(table.replace("'unwrap()'", "'prefix.unwrap()'") + override)
        self.assertIn('unwrap must start', self.run_check(1, label='shifted-unwrap-start'))
        self.source.write_text('prefix.' + before)
        self.write(table.replace(repr(before), repr('prefix.' + before)) + override)
        self.assertIn('unwrap must start', self.run_check(1, label='shifted-before-start'))
        self.source.write_text(before)
        outside = self.root / 'outside.rs'
        outside.write_text(before)
        self.write("source = root / 'outside.rs'\n" + table + override)
        self.assertIn('inside rust workspace', self.run_check(1, label='location-outside-rust'))
        self.write(table + override.replace("'example.rs:1:1'", 'None'))
        self.assertIn('unsupported location helper', self.run_check(1, label='unverified-derived-location'))
        self.write(table + "mutant_anchor = {'sample': ('message', 'specific panic')}\n")
        self.run_check(label='message-anchor-restored')

    def test_single_line_abort_anchor_is_unique(self):
        self.source.write_text('timer.abort();\n')
        self.write("mutations = [('skip-timer-abort', source, 'timer.abort();', '')]\n")
        self.run_check(label='timer-abort-original')
        self.source.write_text('timer.abort();\ntimer.abort();\n')
        self.assertIn('expected=1 actual=2', self.run_check(1, label='timer-abort-duplicate'))
        self.source.write_text('timer.abort();\n')
        self.run_check(label='timer-abort-restored')

    def submit_location_parts(self):
        before = 'unwrap_or_else(|poisoned| poisoned.into_inner())'
        helper = '''def unwrap_site(anchor_source, before, original):
    """The helper is inspected, never executed."""
    assert before, 'a location anchor needs the mutation span'
    text = original.decode()
    assert text.count(before) == 1
    head = text[:text.index(before)]
    return f'{anchor_source.relative_to(rust)}:{head.count(chr(10)) + 1}:{len(head) - head.rfind(chr(10))}'
'''
        setup = ("from negative_process import mutating_sources\nwith mutating_sources(root, [source]) as snapshots:\n    pass\n"
                 "rust = root / 'src'\nmutant_source = {}\noriginals = {source: snapshots[source]}\n"
                 "mutant_anchor = {'sample': ('location', None)}\n"
                 f"mutants = [('sample', {before!r}, 'unwrap()', None, None, None)]\n")
        consumer = '''selected = mutants
for name, before, after, case, marker, capability in selected:
    target = mutant_source.get(name, source)
    target_original = originals[target]
    used_test = mutant_test.get(name, test)
    anchor_kind, anchor = mutant_anchor.get(name, ('message', marker))
    if anchor_kind == 'location':
        anchor = unwrap_site(target, before, target_original)
'''
        return before, setup, helper, consumer

    def test_turn_submit_frozen_location_rejects_unique_span_line_drift(self):
        self.move_script('turn-submit/negative.py')
        before, setup, helper, consumer = self.submit_location_parts()
        body = (setup + helper + consumer).replace("('location', None)", "('location', 'example.rs:1:1')")
        self.source.write_text(before)
        self.write(body)
        self.run_check(label='literal-location-verified-original')
        # before is still unique; only its real source location has moved.
        self.source.write_text('// inserted line\n' + before)
        self.assertEqual(self.source.read_text().count(before), 1)
        self.assertIn('location anchor differs from span: expected=example.rs:2:1',
                      self.run_check(1, label='frozen-location-line-drift'))
        without_helper = body.replace(helper, '').replace('unwrap_site(target, before, target_original)', "'example.rs:1:1'")
        self.write(without_helper)
        self.assertIn('unsupported location helper', self.run_check(1, label='frozen-location-no-helper'))
        self.write(body.replace("'example.rs:1:1'", 'None'))
        self.run_check(label='derived-location-after-drift')
        self.source.write_text(before)
        self.write(body)
        self.run_check(label='literal-location-restored')

    def test_turn_submit_override_maps_reject_rebinding_and_mutation(self):
        self.move_script('turn-submit/negative.py')
        before, setup, helper, consumer = self.submit_location_parts()
        self.source.write_text(before)
        body = setup + helper + consumer
        self.write(body)
        self.run_check(label='override-maps-original')
        for mapping in ('mutant_anchor', 'mutant_source'):
            for suffix in (
                f'{mapping} = {{}}', f'def {mapping}():\n    pass',
                f'def unrelated({mapping}):\n    pass', f'from elsewhere import {mapping}',
                f'{mapping}.update({{}})', f"{mapping}['sample'] = None", f'del {mapping}',
                f"del {mapping}['sample']", f'{mapping} |= {{}}',
                f"{mapping}.setdefault('sample', None)",
                f"alias = {mapping}\nalias['sample'] = ('message', 'stale')",
                f"get_override = {mapping}.get",
                f'global {mapping}', f'def helper():\n    nonlocal {mapping}',
                f"match {{}}:\n    case {{**{mapping}}}:\n        pass",
            ):
                self.write(body + suffix + '\n')
                self.assertIn(mapping, self.run_check(1, label='override-map-rebound-after-consumer'))
            # A single declaration after its first consumer is no safer.
            declaration = next(line for line in setup.splitlines(True) if line.startswith(mapping + ' ='))
            self.write(body.replace(declaration, '') + declaration)
            self.assertIn('read before declaration', self.run_check(1, label='override-map-declared-after-read'))
            shared = declaration.replace(mapping + ' =', 'shared =') + f'{mapping} = shared\n'
            self.write(body.replace(declaration, shared) + "shared['sample'] = ('message', 'stale')\n")
            self.assertIn('literal mapping', self.run_check(1, label='override-map-shared-initializer'))
        self.write(body)
        self.run_check(label='override-maps-restored')

    def test_turn_submit_derived_location_is_bound_to_helper_and_source(self):
        self.move_script('turn-submit/negative.py')
        before, setup, helper, consumer = self.submit_location_parts()
        original = '// prefix\n    ' + before + '\n'
        self.source.write_text(original)
        body = setup + helper + consumer
        self.write(body)
        self.run_check(label='derived-location-original')
        static = CHECK.Static(self.script, self.root, ast.parse(self.script.read_text()))
        self.assertEqual(CHECK.derived_submit_location(static, 'sample', self.source, before, {}), 'example.rs:2:5')
        for bad, diagnostic in [
            (body.replace('head.count(chr(10)) + 1', 'head.count(chr(10)) + 2'), 'location helper'),
            (body.replace('    assert text.count(before) == 1\n', ''), 'location helper'),
            (body.replace('relative_to(rust)', 'relative_to(root)'), 'location helper'),
            (body.replace('def unwrap_site(', '@unknown_decorator\ndef unwrap_site('), 'location helper'),
            (body.replace('unwrap_site(target, before, target_original)', 'unwrap_site(source, before, target_original)'), 'location consumer'),
            (body.replace("if anchor_kind == 'location':", "if anchor_kind == 'message':"), 'location consumer'),
            (body.replace('in selected:', 'in other:'), 'location loop'),
            (body.replace('originals[target]', 'originals[source]'), 'location source binding'),
            (body.replace('target = mutant_source.get(name, source)', 'target = source'), 'location source binding'),
            (body.replace('snapshots[source]', "b'different bytes'"), 'location snapshot'),
            (body.replace("rust = root / 'src'", 'rust = root'), 'location helper workspace'),
            (body + 'unwrap_site(target, before, target_original)\n', 'location consumer'),
            (body.replace('selected = mutants', "unwrap_site = lambda *args: 'bogus.rs:0:0'\nselected = mutants"), 'location binding'),
            (body + "    anchor = 'bogus.rs:0:0'\n", 'location binding'),
            (body + "    anchor_kind = 'message'\n", 'location binding'),
            (body.replace('selected = mutants', 'from elsewhere import unwrap_site\nselected = mutants'), 'location binding'),
            (body + "def anchor():\n    return 'bogus.rs:0:0'\n", 'location binding'),
            (body + 'global anchor\n', 'location binding'),
            (body + 'def helper():\n    nonlocal anchor_kind\n', 'location binding'),
            (body + 'match {}:\n    case {**anchor}:\n        pass\n', 'location binding'),
            (body.replace("'unwrap()'", "'prefix.unwrap()'"), 'unwrap must start'),
            (setup + consumer, 'location helper'),
            (setup + helper, 'location consumer'),
        ]:
            self.write(bad)
            self.assertIn(diagnostic, self.run_check(1, label='derived-location-invalid-contract'))
        self.write(body)
        for content in ('missing', original * 2):
            self.source.write_text(content)
            self.assertIn('location span expected=1', self.run_check(1, label='derived-span-not-unique'))
        self.source.write_text(original)
        self.run_check(label='derived-location-restored')
        self.assertEqual(self.source.read_text(), original)

    def test_regex_json_exemption_cannot_hide_a_product_anchor(self):
        self.script.unlink()
        self.script = self.fixtures / 'background-panic/negative_controls.py'
        self.script.parent.mkdir()
        self.write("mutants = [('matrix', root / 'src/oracle.json', None, ('prefix', 'delete')), ('good', source, 'before', 'after')]\nre.subn(pattern, replacement, content, flags=re.S)\n")
        self.assertIn('NON_SOURCE', self.run_check())
        self.write(self.script.read_text()[len(self.base):].replace("root / 'src/oracle.json'", 'source'))
        self.assertIn('empty or non-string anchor', self.run_check(1))

    def test_missing_file_and_outside_path_fail(self):
        self.source.unlink()
        self.run_check(1)
        self.write("mutants = [('sample', root.parent / 'outside.rs', 'before', 'after')]\n")
        self.assertIn('outside repository', self.run_check(1))
        self.write("mutants = [('sample', unknown_path, 'before', 'after')]\n")
        self.assertIn('unresolved name', self.run_check(1))

    def test_syntax_empty_anchor_and_empty_inventory_fail(self):
        self.write('mutants = [')
        self.run_check(1)
        self.write("mutants = [('sample', source, '', 'after')]\n")
        self.assertIn('empty', self.run_check(1))
        self.script.unlink()
        self.assertIn('no negative scripts', self.run_check(1))

    def test_controls_are_not_executed_and_aliases_are_discovered(self):
        self.write("mutants = [('sample', source, 'before', 'after')]\nraise RuntimeError('must never execute')\nsource.write_text('DANGEROUS')\n")
        self.run_check()
        self.assertEqual(self.source.read_text(), 'before\n')
        alias = self.script.with_name('historic_negative.py')
        self.script.rename(alias)
        self.run_check()
        self.source.write_text('broken')
        self.run_check(1)


if __name__ == '__main__':
    unittest.main(verbosity=2)
