# anchor-selfcheck: unsupported unit-report helper for the CI preflight, no source replacement table
"""Machine-readable roster and execution report for the anchor preflight.

#2513 fix-r16 (DS conc#1 + DS conc#2 + MM simp#1): the Rust preflight used to
re-derive the roster from hand-parsed ``class X(Base):`` lines and the
execution statuses from CPython's human verbose formatting.  A multi-line
class header silently dropped a class from the roster (missed judgment), and
any report line containing ``__main__.Class.`` counted as an entry (false
pass).  This helper is the one python3 subprocess the preflight spawns; it
runs the script exactly as the preflight always did — same interpreter flags,
``runpy`` with ``__main__``, ``sys.argv`` passed through — and records the
structured products the judgment needs:

* roster — ``ast`` class declarations of the script with their base
  spellings, so a multi-line header is naturally correct;
* entries — every test outcome observed through ``unittest``'s own
  ``TestResult`` callbacks, which is the machine-readable form of the
  verbose report (printed text is never evidence);
* ran — the number of recorded outcomes, unittest's ``Ran N tests``.

Statuses: ``ok`` / ``fail`` / ``error`` / ``unexpected-success`` mean the
method executed; ``skipped`` and ``expected-failure`` do not.  SubTest
outcomes need no separate record: the parent method's own callback carries
the class coverage.

The JSON report goes to ``--report <path>``; stdout and stderr pass through
unchanged so the human report keeps flowing to the CI log.

Use: python3 -I -B negative_anchor_report.py --report <path> <script> [script args...]
"""
import ast
import json
import runpy
import sys
import unittest
from pathlib import Path

# The TestResult callback each outcome flows through, mapped to the status
# the judgment classifies. Patching the base class covers every runner the
# scripts construct (TextTestRunner's default result and custom subclasses
# that call super()).
CALLBACK_STATUSES = {
    'addSuccess': 'ok',
    'addFailure': 'fail',
    'addError': 'error',
    'addUnexpectedSuccess': 'unexpected-success',
    'addSkip': 'skipped',
    'addExpectedFailure': 'expected-failure',
}

_records = []


def _install_recorder():
    for name, status in CALLBACK_STATUSES.items():
        original = getattr(unittest.TestResult, name)

        def hook(result, test, *args, _original=original, _status=status, **kwargs):
            _records.append({'id': test.id(), 'status': _status})
            _original(result, test, *args, **kwargs)

        setattr(unittest.TestResult, name, hook)


def entry_class(identifier):
    """The class of a test id: ``module.Class.method`` (``__main__`` under
    runpy), with a trailing `` (subtest)`` part stripped.  A classless id
    yields ''."""
    parts = identifier.split(' (', 1)[0].split('.')
    return parts[-2] if len(parts) >= 2 else ''


def class_declarations(script):
    """Top-level class declarations with their base spellings, via ast.

    A ``class Name(Base):`` header spanning several lines is one ClassDef
    node here, where the pre-fix line scanner dropped the class."""
    tree = ast.parse(script.read_bytes())
    return [{'name': node.name,
             'bases': [ast.unparse(base) for base in node.bases]}
            for node in tree.body if isinstance(node, ast.ClassDef)]


def _publish(report_path, script, declarations):
    entries = [{'id': row['id'], 'class': entry_class(row['id']),
                'status': row['status']} for row in _records]
    report_path.write_text(json.dumps(
        {'script': script.name, 'roster': declarations,
         'entries': entries, 'ran': len(entries)}, indent=1) + '\n')


def main(argv):
    if len(argv) < 3 or argv[0] != '--report' or not argv[1] or not argv[2]:
        print('usage: negative_anchor_report.py --report <path> <script> [script args...]',
              file=sys.stderr)
        return 2
    report_path = Path(argv[1])
    script = Path(argv[2])
    if not script.is_file():
        print(f'no such script: {script}', file=sys.stderr)
        return 2
    declarations = class_declarations(script)
    _install_recorder()
    sys.argv = [str(script), *argv[3:]]
    try:
        runpy.run_path(str(script), run_name='__main__')
    except SystemExit as exit_code:
        # Propagate the script's own exit category (unittest.main's 0/1,
        # argparse's 2) after publishing the report, so the caller keeps the
        # exit-code contract it always had; a non-int code exits 1 exactly
        # like the interpreter does.
        _publish(report_path, script, declarations)
        code = exit_code.code
        if code is None:
            return 0
        return code if isinstance(code, int) else 1
    _publish(report_path, script, declarations)
    return 0


if __name__ == '__main__':
    raise SystemExit(main(sys.argv[1:]))
