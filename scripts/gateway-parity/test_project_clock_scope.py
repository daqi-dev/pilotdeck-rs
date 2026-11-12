"""Project-summary createdAt scope: fixed TS shape and approved path compatibility."""
# >>> apparatus bootstrap (verbatim in every A2 entry; test_safety checks it)
# -I keeps PYTHONPATH, user site and the script directory out of module
# resolution.  It must run before every other standard-library import: a
# shadowed argparse/pathlib would otherwise execute before this check.  ``sys``
# is builtin; os is imported only after the frozen-module preflight below.
import sys

if not sys.flags.isolated:
    raise SystemExit(f'run with -I: python3 -I {__file__}')

if sys.version_info < (3, 11) or 'os' not in sys.modules:
    raise SystemExit('os must be preloaded from CPython 3.11+ frozen modules')
import os

_boot_dir = os.path.dirname(os.path.realpath(__file__))
_boot_dir = os.path.dirname(_boot_dir) if os.path.basename(_boot_dir) == 'controls' else _boot_dir
# Identity, not spelling: a case-insensitive alias, a symlinked spelling or a
# mixed ``//`` form names this very directory while the string differs
# (#1033). Nonexistent sys.path entries skip; an empty entry means cwd.
if any(
        os.path.isdir(entry or os.curdir)
        and any(os.path.isdir(root)
                and os.path.samefile(entry or os.curdir, root)
                for root in (_boot_dir, os.path.join(_boot_dir, 'controls')))
        for entry in sys.path):
    raise SystemExit('apparatus directory on sys.path; run the entry file directly with -I')

import importlib.util
from pathlib import Path
import tempfile

# -B only stops cache writes, so redirect the bytecode prefix too (fix-r5).
sys.pycache_prefix = tempfile.mkdtemp(prefix='pdrs-gateway-pyc-')
sys.dont_write_bytecode = True
# CPython has already compiled the CLI entry before it can acquire a lease.
# Capture that executing object, not a later read of the restored entry file.
_apparatus_code = sys._getframe().f_code

# Load the one guard by explicit path, never sys.path: a same-named package or
# extension module beside the source would win before the scan (fix-r6 P1).
# Inline on purpose: importing the guard is what this block defends.
_here = Path(__file__).resolve().parent
_apparatus = _here.parent if _here.name == 'controls' else _here
_guard_path = _apparatus / '_guard.py'
_guard = sys.modules.get('_guard')
if not (getattr(_guard, '__file__', None)
        and Path(_guard.__file__).resolve() == _guard_path
        and isinstance(getattr(_guard, '_apparatus_source', None), bytes)):
    _spec = importlib.util.spec_from_file_location('_guard', _guard_path)
    _guard = importlib.util.module_from_spec(_spec)
    sys.modules['_guard'] = _guard
    _guard_source = _guard_path.read_bytes()
    try:
        exec(compile(_guard_source, str(_guard_path), 'exec'), _guard.__dict__)
    except BaseException:
        sys.modules.pop('_guard', None)
        raise
    _guard._apparatus_source = _guard_source
if Path(_guard.__file__).resolve() != _guard_path:
    raise RuntimeError(f'_guard: loaded {_guard.__file__}, expected {_guard_path}')

_load = _guard.load
_has_git_checkout = _guard._has_git_checkout
_verify_cli_imports = _guard.verify_cli_imports
run_entry = _guard.run_entry
common = _load('common', _apparatus / 'common.py')
# <<< apparatus bootstrap
import atexit
import shutil
import unittest

atexit.register(shutil.rmtree, sys.pycache_prefix, ignore_errors=True)

compare = _load('compare', _apparatus / 'compare.py')
Normalizer = compare.Normalizer


def summary(path_field='fullPath', **changes):
    # Fixed cc7c4e6 src/web/client/protocol.ts: WebProjectSummary and
    # src/web/server/listProjects.ts: summarizeProject. The path spelling is
    # an approved compatibility shape, not a field emitted by that TS source.
    return dict({'projectKey': '/project', 'name': 'project',
                 path_field: '/project', 'sessionCount': 0, 'createdAt': 1000},
                **changes)


class ProjectCreatedAtScope(unittest.TestCase):
    def assert_created_at_exact(self, value):
        self.assertFalse(Normalizer.clock_field(value, 'createdAt'))
        self.assertNotEqual(Normalizer().value(value),
                            Normalizer().value(dict(value, createdAt=2000)))

    def test_fixed_ts_empty_and_populated_summaries_normalize_created_at(self):
        for extra in ({}, {'sessionCount': 3, 'lastActivity': 1500},
                      {'sessionCount': 3, 'lastActivity': 1500.25}):
            for created_at in (1000, 1000.25):
                value = summary(createdAt=created_at, **extra)
                with self.subTest(value=value):
                    self.assertEqual(Normalizer().value(value)['createdAt'], '<wall-clock>')
                    self.assertEqual(Normalizer().value({'projects': [value]}),
                                     Normalizer().value({'projects': [dict(value, createdAt=2000)]}))

    def test_approved_path_compatibility_is_distinct_from_fixed_ts_shape(self):
        for extra in ({}, {'sessionCount': 2, 'lastActivity': 1500},
                      {'fullPath': '/project'}):
            value = summary('path', **extra)
            with self.subTest(value=value):
                self.assertEqual(Normalizer().value(value)['createdAt'], '<wall-clock>')
                self.assertEqual(Normalizer().value(value),
                                 Normalizer().value(dict(value, createdAt=2000)))

    def test_each_required_field_is_needed(self):
        for path_field in ('fullPath', 'path'):
            value = summary(path_field)
            for missing in ('projectKey', 'name', path_field, 'sessionCount'):
                with self.subTest(path_field=path_field, missing=missing):
                    self.assert_created_at_exact({key: item for key, item in value.items()
                                                  if key != missing})

    def test_absent_optional_clock_is_not_invented(self):
        value = summary()
        del value['createdAt']
        self.assertFalse(Normalizer.clock_field(value, 'createdAt'))
        self.assertEqual(Normalizer().value(value), value)

    def test_extra_business_fields_do_not_hide_created_at_changes(self):
        for path_field in ('fullPath', 'path'):
            for extra, item in (('runAt', 10), ('nextRunAt', 20), ('durationMs', 30),
                                ('usage', {'totalTokens': 40}), ('budget', 50),
                                ('note', 'business object'), ('unknown', None)):
                with self.subTest(path_field=path_field, extra=extra):
                    self.assert_created_at_exact(summary(path_field, **{extra: item}))

    def test_identity_and_path_fields_require_strings(self):
        for path_field in ('fullPath', 'path'):
            for field in ('projectKey', 'name', path_field):
                for invalid in (None, 1, True, [], {}):
                    with self.subTest(path_field=path_field, field=field, invalid=invalid):
                        self.assert_created_at_exact(summary(path_field, **{field: invalid}))
        # Both path spellings may be present; neither may bypass type checking.
        for invalid_field in ('path', 'fullPath'):
            value = summary(path='/project')
            value[invalid_field] = None
            with self.subTest(invalid_field=invalid_field):
                self.assert_created_at_exact(value)

    def test_session_count_requires_a_nonnegative_integer(self):
        for path_field in ('fullPath', 'path'):
            for invalid in (None, '1', False, True, [], {}, -1, 0.5, 3.0,
                            float('nan'), float('inf'), float('-inf')):
                with self.subTest(path_field=path_field, invalid=invalid):
                    self.assert_created_at_exact(summary(path_field, sessionCount=invalid))

    def test_optional_activity_requires_a_finite_number(self):
        for path_field in ('fullPath', 'path'):
            for invalid in (None, '1', False, True, [], {},
                            float('nan'), float('inf'), float('-inf')):
                with self.subTest(path_field=path_field, invalid=invalid):
                    self.assert_created_at_exact(summary(path_field, lastActivity=invalid))

    def test_created_at_requires_a_finite_json_number(self):
        for path_field in ('fullPath', 'path'):
            for invalid in (None, '2026-01-01T00:00:00Z', False, True, [], {},
                            float('nan'), float('inf'), float('-inf')):
                value = summary(path_field, createdAt=invalid)
                with self.subTest(path_field=path_field, invalid=invalid):
                    self.assertFalse(Normalizer.clock_field(value, 'createdAt'))
                    actual = Normalizer().value(value)['createdAt']
                    # Check type as well: Python considers True equal to 1.
                    self.assertIs(type(actual), type(invalid))
                    if type(invalid) is float and invalid != invalid:
                        self.assertNotEqual(actual, actual)
                    else:
                        self.assertEqual(actual, invalid)

    def test_summary_identity_and_count_differences_remain_visible(self):
        for path_field in ('fullPath', 'path'):
            for field, changed in (('projectKey', '/other'), ('name', 'other'),
                                   (path_field, '/other'), ('sessionCount', 1)):
                value = summary(path_field)
                with self.subTest(path_field=path_field, field=field):
                    self.assertNotEqual(Normalizer().value(value),
                                        Normalizer().value(dict(value, **{field: changed})))

    def test_other_project_time_fields_do_not_gain_normalization(self):
        for field in ('updatedAt', 'startedAt', 'completedAt', 'acceptedAt',
                      'runAt', 'nextRunAt', 'durationMs', 'budget'):
            value = summary(**{field: 1})
            with self.subTest(field=field):
                self.assertNotEqual(Normalizer().value(value),
                                    Normalizer().value(dict(value, **{field: 2})))

    def test_existing_project_last_activity_scope_is_unchanged(self):
        # This historical predicate is deliberately independent of the new
        # createdAt summary boundary, including its path spelling support.
        for path_field in ('path', 'fullPath'):
            value = {'projectKey': '/project', 'name': 'project',
                     path_field: '/project', 'lastActivity': 1, 'runAt': 10}
            with self.subTest(path_field=path_field):
                self.assertEqual(Normalizer().value(value),
                                 Normalizer().value(dict(value, lastActivity=2)))
        value = {'projectKey': '/project', 'lastActivity': 1}
        self.assertNotEqual(Normalizer().value(value),
                            Normalizer().value(dict(value, lastActivity=2)))

    def test_other_existing_created_at_containers_are_unchanged(self):
        for value in (
            {'entryId': 'entry', 'sequence': 1, 'turnId': 'turn', 'type': 'model_request'},
            {'taskId': 'task', 'schedule': {'kind': 'once'}, 'message': 'hello', 'status': 'active'},
            {'requestId': 'request', 'ownerId': 'owner', 'kind': 'question', 'toolCallId': 'tool'},
        ):
            with self.subTest(value=value):
                self.assertEqual(Normalizer().value(dict(value, createdAt=1000)),
                                 Normalizer().value(dict(value, createdAt=2000)))


if __name__ == '__main__':
    unittest.main()
