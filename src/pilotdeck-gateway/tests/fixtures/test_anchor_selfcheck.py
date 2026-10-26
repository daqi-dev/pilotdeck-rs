#!/usr/bin/env python3
"""Synthetic scanner controls: only tiny disposable text fixtures are changed."""
import ast
from contextlib import redirect_stdout
import importlib.util
import io
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

# -I excludes cwd and PYTHONPATH. Load exactly this adjacent checker, never a
# mutation runner or a module selected from the caller's environment.
PATH = Path(__file__).with_name('anchor_selfcheck.py')
SPEC = importlib.util.spec_from_file_location('anchor_selfcheck', PATH)
CHECK = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = CHECK
SPEC.loader.exec_module(CHECK)
PRODUCTION_UNSUPPORTED = dict(CHECK.UNSUPPORTED)
PRODUCTION_UNSUPPORTED_SIGNALS = dict(CHECK.UNSUPPORTED_SIGNALS)


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
        self.allowlist = patch.dict(CHECK.UNSUPPORTED, {}, clear=True)
        self.allowlist.start()
        self.addCleanup(self.allowlist.stop)
        signals = patch.dict(CHECK.UNSUPPORTED_SIGNALS, {}, clear=True)
        signals.start()
        self.addCleanup(signals.stop)

    def write(self, body):
        self.script.write_text(self.base + body)

    def run_check(self, expected=0, *, env=None, anchors_only=True, label=''):
        output = io.StringIO()
        with redirect_stdout(output):
            code = CHECK.check(self.root, anchors_only=anchors_only, environ=env or {})
        report = output.getvalue()
        self.assertEqual(code, expected, f'{label}\n{report}')
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
        CHECK.UNSUPPORTED[name] = reason
        CHECK.UNSUPPORTED_SIGNALS[name] = PRODUCTION_UNSUPPORTED_SIGNALS[name]
        for anchors_only in (False, True):
            with self.subTest(anchors_only=anchors_only):
                report = self.run_check(env=env, anchors_only=anchors_only,
                                        label='sequential-regex-explicit-exception')
                self.assertIn(f'UNSUPPORTED {name}: {reason}', report)
                self.assertIn('checked=0 unsupported=1 anchors=0 errors=0', report)
        self.assertIn('MISSING_ENV CARGO_TARGET_DIR', self.run_check(
            1, anchors_only=False, label='sequential-regex-missing-environment'))
        del CHECK.UNSUPPORTED[name]
        del CHECK.UNSUPPORTED_SIGNALS[name]
        self.assertIn('unsupported regex consumer', self.run_check(
            1, env=env, label='sequential-regex-exception-removed'))
        CHECK.UNSUPPORTED[name] = reason
        CHECK.UNSUPPORTED_SIGNALS[name] = PRODUCTION_UNSUPPORTED_SIGNALS[name]
        self.run_check(env=env, anchors_only=False, label='sequential-regex-restored')
        other = self.fixtures / 'sample/negative.py'
        other.write_text(self.script.read_text())
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
        CHECK.UNSUPPORTED[name] = PRODUCTION_UNSUPPORTED[name]
        self.assertIn(f'UNSUPPORTED {name}:', self.run_check(label='helper-tests-registered'))
        del CHECK.UNSUPPORTED[name]
        self.run_check(1, label='helper-tests-registration-removed')
        CHECK.UNSUPPORTED[name] = PRODUCTION_UNSUPPORTED[name]
        self.run_check(label='helper-tests-registration-restored')

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

    def test_host_factory_relative_source_and_concatenated_anchor(self):
        self.move_script('host-factory/negative.py')
        body = """SOURCE = Path('src/example.rs')
LOOP = 'before'
MUTATIONS = [('lease', LOOP, 'after', 'test', 'failure'),
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
        self.run_check(label='rebound-snapshot-restored')
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

    def test_snapshot_rebinding_discards_stale_path_assignments(self):
        other = self.root / 'src/other.rs'
        other.write_text('before')
        self.write("""other = root / 'src/other.rs'
originals = {'alias.rs': source.read_text()}
originals['alias.rs'] = source.read_text()
originals = {'alias.rs': other.read_text()}
mutants = [('sample', 'alias.rs', 'before', 'after')]
""")
        self.run_check(label='rebound-path-original')
        other.write_text('missing')
        self.assertIn('src/other.rs expected=1 actual=0', self.run_check(1, label='rebound-path-anchor-missing'))
        other.write_text('before')
        self.run_check(label='rebound-path-restored')

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
            self.assertIn('mapping initializer', self.run_check(1, label='snapshot-shared-initializer'))
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
        CHECK.UNSUPPORTED['sample/runner.py'] = 'synthetic helper without a table'
        self.assertIn('UNSUPPORTED sample/runner.py', self.run_check(label='registered-helper'))
        other.unlink()
        self.assertIn('stale/empty UNSUPPORTED', self.run_check(1, label='removed-helper'))
        del CHECK.UNSUPPORTED['sample/runner.py']
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

    def test_allowlist_is_explicit_and_stale_entries_fail(self):
        self.write("raise RuntimeError('must not run')\n")
        CHECK.UNSUPPORTED['sample/negative.py'] = 'synthetic unsupported format'
        self.assertIn('UNSUPPORTED sample/negative.py: synthetic unsupported format', self.run_check())
        CHECK.UNSUPPORTED['gone/negative.py'] = 'stale entry'
        self.assertIn('stale/empty', self.run_check(1))
        del CHECK.UNSUPPORTED['gone/negative.py']
        CHECK.UNSUPPORTED['sample/negative.py'] = ''
        self.run_check(1)

    def test_each_file_exception_has_a_paired_missing_file_control(self):
        for name, reason in PRODUCTION_UNSUPPORTED.items():
            with self.subTest(name=name):
                self.assertTrue(reason.strip())
                script = self.fixtures / name
                script.parent.mkdir(parents=True, exist_ok=True)
                signals = PRODUCTION_UNSUPPORTED_SIGNALS.get(name, set())
                body = ''.join(f'{value} = []\n' if kind == 'table' else f'{value}()\n'
                               for kind, value in (signal.split(':') for signal in sorted(signals)))
                body += "raise RuntimeError('must not run')\n"
                script.write_text(body)
                CHECK.UNSUPPORTED[name] = reason
                if signals:
                    CHECK.UNSUPPORTED_SIGNALS[name] = signals
                self.run_check(label=f'allowlist-present-{name}')
                script.unlink()
                self.assertIn('stale/empty', self.run_check(1, label=f'allowlist-missing-{name}'))
                script.write_text(body)
                self.run_check(label=f'allowlist-restored-{name}')
                print(f'ALLOWLIST {name}: present=0 missing=1 restored=0')
                script.unlink()
                del CHECK.UNSUPPORTED[name]
                CHECK.UNSUPPORTED_SIGNALS.pop(name, None)

    def test_helper_exemption_cannot_hide_mutation_signals(self):
        name = 'sample/negative.py'
        CHECK.UNSUPPORTED[name] = 'helper without mutation definitions'
        self.write("raise RuntimeError('must not execute')\n")
        self.run_check(label='signal-free-helper-original')
        bodies = [f"{table} = [('sample', source, 'before', 'after')]\n" for table in sorted(CHECK.TABLES)]
        bodies += ["mutation('sample', source, 'before', 'after')\n", "mutate('sample', source, 'before', 'after')\n"]
        for body in bodies:
            self.write(body)
            self.assertIn('unsupported exemption signals', self.run_check(1, label='helper-hidden-mutation'))
        self.write("raise RuntimeError('must not execute')\n")
        self.run_check(label='signal-free-helper-restored')

    def test_registered_exemption_signals_are_exact_and_stale_checked(self):
        for name, signals in PRODUCTION_UNSUPPORTED_SIGNALS.items():
            with self.subTest(name=name):
                self.move_script(name)
                CHECK.UNSUPPORTED[name] = PRODUCTION_UNSUPPORTED[name]
                CHECK.UNSUPPORTED_SIGNALS[name] = signals
                fragments = {signal: (f'{value} = []\n' if kind == 'table' else f'{value}()\n')
                             for signal in signals for kind, value in [signal.split(':')]}
                body = ''.join(fragments.values())
                self.write(body)
                self.run_check(label=f'exemption-signals-original-{name}')
                for signal in signals:
                    self.write(''.join(fragment for key, fragment in fragments.items() if key != signal))
                    self.assertIn('unsupported exemption signals', self.run_check(1, label='exemption-signal-removed'))
                self.write(body + 'mutation()\n')
                self.assertIn('unsupported exemption signals', self.run_check(1, label='exemption-signal-added'))
                self.write(body)
                del CHECK.UNSUPPORTED_SIGNALS[name]
                self.assertIn('unsupported exemption signals', self.run_check(1, label='exemption-signal-unregistered'))
                CHECK.UNSUPPORTED_SIGNALS[name] = signals
                del CHECK.UNSUPPORTED[name]
                self.assertIn('stale UNSUPPORTED_SIGNALS', self.run_check(1, label='exemption-signal-stale'))
                CHECK.UNSUPPORTED[name] = PRODUCTION_UNSUPPORTED[name]
                self.run_check(label=f'exemption-signals-restored-{name}')
                del CHECK.UNSUPPORTED[name]
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
        setup = ("rust = root / 'src'\nmutant_source = {}\noriginals = {source: snapshots[source]}\n"
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
