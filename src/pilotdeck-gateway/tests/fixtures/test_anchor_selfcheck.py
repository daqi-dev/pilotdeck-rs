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

    def write(self, body):
        self.script.write_text(self.base + body)

    def run_check(self, expected=0, *, env=None, anchors_only=True, label=''):
        output = io.StringIO()
        with redirect_stdout(output):
            code = CHECK.check(self.root, anchors_only=anchors_only, environ=env or {})
        report = output.getvalue()
        self.assertEqual(code, expected, report)
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
        for count in ['0', '-1', 'True', '1.5']:
            with self.subTest(count=count):
                self.write(f"mutants = [('sample', source, 'before', 'after', {count})]\n")
                self.assertIn('positive integer', self.run_check(1))
        self.write("ANCHOR_COUNTS = {'unused': 2}\nmutants = [('sample', source, 'before', 'after', 2)]\n")
        self.assertIn('unused ANCHOR_COUNTS', self.run_check(1))
        self.write("ANCHOR_COUNTS = {'sample': True}\nmutants = [('sample', source, 'before', 'after')]\n")
        self.assertIn('positive integers', self.run_check(1))
        self.write("ANCHOR_COUNTS = {'sample': 2}\nmutants = [('sample', source, 'before', 'after', 2)]\n")
        self.assertIn('duplicate count', self.run_check(1))

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
        for anchors_only in (False, True):
            with self.subTest(anchors_only=anchors_only):
                report = self.run_check(env=env, anchors_only=anchors_only,
                                        label='sequential-regex-explicit-exception')
                self.assertIn(f'UNSUPPORTED {name}: {reason}', report)
                self.assertIn('checked=0 unsupported=1 anchors=0 errors=0', report)
        self.assertIn('MISSING_ENV CARGO_TARGET_DIR', self.run_check(
            1, anchors_only=False, label='sequential-regex-missing-environment'))
        del CHECK.UNSUPPORTED[name]
        self.assertIn('unsupported regex consumer', self.run_check(
            1, env=env, label='sequential-regex-exception-removed'))
        CHECK.UNSUPPORTED[name] = reason
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
                script.write_text("raise RuntimeError('must not run')\n")
                CHECK.UNSUPPORTED[name] = reason
                self.run_check()
                script.unlink()
                self.assertIn('stale/empty', self.run_check(1))
                script.write_text("raise RuntimeError('must not run')\n")
                self.run_check()
                print(f'ALLOWLIST {name}: present=0 missing=1 restored=0')
                script.unlink()
                del CHECK.UNSUPPORTED[name]

    def test_each_row_exception_has_a_paired_stale_control(self):
        self.script.unlink()
        for (name, label), reason in CHECK.ROW_EXCEPTIONS.items():
            with self.subTest(name=name):
                self.assertTrue(reason.strip())
                self.script = self.fixtures / name
                self.script.parent.mkdir(parents=True, exist_ok=True)
                good = "('good', source, 'before', 'after')"
                siblings = ', '.join(f"({other!r}, source, unknown_scoped_anchor, 'after')"
                                     for file, other in CHECK.ROW_EXCEPTIONS
                                     if file == name and other != label)
                excluded = f"({label!r}, source, unknown_scoped_anchor, 'after')"
                good = ', '.join(part for part in (good, siblings) if part)
                self.write(f'mutants = [{good}, {excluded}]\n')
                self.run_check()
                self.write(f'mutants = [{good}]\n')
                self.assertIn('stale row exceptions', self.run_check(1))
                self.write(f'mutants = [{good}, {excluded}]\n')
                self.run_check()
                print(f'ROW_EXCEPTION {name}/{label}: present=0 missing=1 restored=0')
                self.script.unlink()

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
