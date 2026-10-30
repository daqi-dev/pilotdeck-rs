# anchor-selfcheck: unsupported standalone runner contract, injected guard owns mutations
"""M6 runner lifecycle. No imports from legacy negative-control helpers.

Adapters supply a restoring/verifying context manager and execute(case, env).
For product sources that context MUST be negative_process.mutating_sources;
execute must compile each mutant and check positive/restored controls itself.
This module owns admission, verdicts and the only final-summary publication.
"""
from contextlib import contextmanager
from dataclasses import dataclass
import json
import os
from pathlib import Path
import re
import signal


# Reserved diagnostics emitted by the existing gateway watchdog/timeout probes.
# Data values containing these words are not themselves timeout evidence.
HANG_PREFIXES = ('#813 gateway watchdog:', '#813 timeout:')


@dataclass(frozen=True)
class Case:
    mutation: str
    test: str
    assertion: str
    # Explicit migration forms; new callers retain the strict default.
    kind: str = 'assertion'
    prelude: tuple = ()
    positive_tests: tuple = ()
    payload_pattern: str = ''


@dataclass
class RunResult:
    code: int
    document: dict
    error: str = ''


def _line(value, field):
    if not isinstance(value, str) or not value.strip() or len(value.splitlines()) != 1:
        raise ValueError(f'{field} must be one nonempty line')
    if any(char in value for char in '\r\n\x00'):
        raise ValueError(f'{field} must be one nonempty line')


def select_cases(cases, selected, oracle):
    table = tuple(cases)
    if not table:
        raise ValueError('empty case table')
    if not tuple(oracle):
        raise ValueError('zero-row oracle')
    for case in table:
        for field in ('mutation', 'test', 'assertion'):
            _line(getattr(case, field), field)
        if case.kind in ('assertion', 'message') and not marker_is_scoped(case.assertion):
            raise ValueError('message must identify a scoped assertion')
        if case.kind == 'assertion':
            if not re.fullmatch(r'#[0-9]+ \S[^\r\n]*', case.assertion):
                raise ValueError('assertion requires an issue-specific single-line marker')
        elif case.kind == 'message':
            if not case.payload_pattern:
                raise ValueError('legacy message requires its complete payload pattern')
            re.compile(case.payload_pattern)
        elif case.kind == 'location':
            if (not re.fullmatch(r'[^\s:]+\.rs:[1-9][0-9]*:[1-9][0-9]*', case.assertion)
                    or case.prelude != ('poison the leases mutex mid-turn',)):
                raise ValueError('location requires an exact source site and poison prelude')
        elif case.kind == 'positive':
            if (not case.positive_tests or len(set(case.positive_tests)) != len(case.positive_tests)
                    or any(not isinstance(test, str) or not test or re.search(r'\s', test)
                           for test in case.positive_tests)):
                raise ValueError('positive requires an explicit nonempty test inventory')
        else:
            raise ValueError('unknown case kind')
        if re.search(r'\s', case.test):
            raise ValueError('test requires an exact name without whitespace')
    names = [case.mutation for case in table]
    if len(set(names)) != len(names):
        raise ValueError('duplicate case name')
    choices = names if selected is None else list(selected)
    if not choices or len(set(choices)) != len(choices) or set(choices) - set(names):
        raise ValueError('selection must be nonempty, unique and known')
    wanted = set(choices)
    return table, tuple(case for case in table if case.mutation in wanted)


def environment(environ):
    """Require explicit locations, preserving the caller's cargo cache/target."""
    env = dict(environ)
    for name in ('CARGO_HOME', 'CARGO_TARGET_DIR', 'PDRS_BASELINE'):
        value = env.get(name)
        if not value or not Path(value).is_absolute():
            raise ValueError(f'{name} must be supplied as an absolute path')
        if name != 'CARGO_TARGET_DIR' and not Path(value).is_dir():
            raise ValueError(f'{name} must name an existing directory')
    env.setdefault('CARGO_PROFILE_DEV_DEBUG', 'line-tables-only')
    env.setdefault('CARGO_PROFILE_TEST_DEBUG', 'line-tables-only')
    return env


def claim_output(path):
    """Same #1060 admission rule: absent/empty directory, then O_EXCL claim.

    Claims are permanent evidence; no stale-PID probing, waiting or reclaim.
    Refusal never removes or renames any preexisting entry.
    """
    path = Path(path).resolve()
    path.mkdir(parents=True, exist_ok=True)
    if any(path.iterdir()):
        raise ValueError('output directory is not empty')
    with (path / '.claim').open('x', encoding='utf-8') as stream:
        stream.write(f'{os.getpid()}\n')
    return path


@contextmanager
def sigint_default():
    """Even a shell-backgrounded runner must unwind on SIGINT; restore caller."""
    previous = signal.signal(signal.SIGINT, signal.default_int_handler)
    try:
        yield
    finally:
        signal.signal(signal.SIGINT, previous)


def classify(result, case):
    """Narrow libtest --exact / --show-output contract, one test per process.

    Only a normally exited 101 with one named failure and its own first panic
    payload line can kill. A marker in stdout, a path, backtrace, another panic
    or a continuation line is insufficient. Adapters must propagate launch /
    timeout exceptions instead of fabricating a CompletedProcess.
    """
    raw = result.stdout + '\n' + result.stderr
    if result.returncode not in (0, 101):
        return 'INVALID', 'abnormal exit (signal, launch or non-test failure)', []
    lines = raw.splitlines()
    running = re.findall(r'^running (\d+) tests?$', raw, re.MULTILINE)
    named = re.findall(r'^test (\S+) \.\.\. (ok|FAILED|ignored)$', raw, re.MULTILINE)
    totals = re.findall(r'^test result: (ok|FAILED)\. (\d+) passed; (\d+) failed; '
                        r'(\d+) ignored;', raw, re.MULTILINE)
    if case.kind == 'positive':
        expected = set(case.positive_tests)
        passed = [test for test, status in named if status == 'ok']
        valid = (result.returncode == 0 and len(passed) == len(expected)
                 and set(passed) == expected and bool(totals)
                 and all(status in ('ok', 'ignored') for _, status in named)
                 and all(status == 'ok' and failed == '0' for status, _, failed, _ in totals)
                 and sum(int(count) for count in running) == len(named)
                 and sum(int(count) for _, count, _, _ in totals) == len(passed))
        return ('PASS', 'baseline passed', []) if valid else ('INVALID', 'baseline is not green', [])
    if running != ['1'] or len(named) != 1 or named[0][0] != case.test:
        return 'INVALID', 'missing exact single named test', []
    if (result.returncode == 0 and named == [(case.test, 'ok')]
            and totals == [('ok', '1', '0', '0')]):
        return 'SURVIVED', 'bound test still passed', []
    if (result.returncode != 101 or named != [(case.test, 'FAILED')]
            or totals != [('FAILED', '0', '1', '0')]):
        return 'INVALID', 'missing normal named test failure', []
    headers = [(index, re.fullmatch(r"thread '([^']+)'(?: \(\d+\))? panicked at .+:", line))
               for index, line in enumerate(lines) if ' panicked at ' in line]
    if case.kind in ('message', 'location'):
        return classify_legacy_panic(lines, headers, case)
    if len(headers) != 1 or headers[0][1] is None or headers[0][1][1] != case.test:
        return 'INVALID', 'panic is not exclusively owned by the named test', []
    index = headers[0][0]
    payload = lines[index + 1] if index + 1 < len(lines) else ''
    body = []
    for line in lines[index + 1:]:
        if re.match(r'^(?:stack backtrace:|note:|test result:|failures:|test \S+ \.\.\. |error: test failed)', line):
            break
        body.append(line)
    if any(line.lstrip().startswith(HANG_PREFIXES) for line in body):
        return 'INVALID', 'timeout/watchdog panic is not an assertion', []
    if payload != case.assertion and not re.fullmatch(
            r'assertion .+ failed: ' + re.escape(case.assertion), payload):
        return 'INVALID', 'missing exclusive first-line assertion marker', []
    return 'KILLED', 'named test failed at its assertion', [case.test]


def marker_is_scoped(marker):
    """One shared scope check for all migrated message bindings (#935)."""
    return (isinstance(marker, str) and bool(marker.strip())
            and len(marker.splitlines()) == 1
            and marker != 'assertion' and not marker.startswith('assertion `left == right`')
            and re.fullmatch(r'"[^"]+":\s*', marker) is None
            and not marker.rstrip().endswith(': step')
            and not marker.startswith(HANG_PREFIXES))


def classify_legacy_panic(lines, headers, case):
    """First-line scoped messages or the two recorded poison panic blocks.

    Unlike the old runner, no marker in a source path, continuation line or
    sibling panic counts. The location form also requires the unwrap payload.
    """
    expected = 2 if case.kind == 'location' else 1
    if (len(headers) != expected
            or any(match is None or match[1] != case.test for _, match in headers)):
        return 'INVALID', 'legacy panic ownership/count mismatch', []
    payloads = [lines[index + 1] if index + 1 < len(lines) else '' for index, _ in headers]
    for index, _ in headers:
        for line in lines[index + 1:]:
            if re.match(r"^(?:thread '|stack backtrace:|note:|test result:|failures:|test \S+ \.\.\. |error: test failed)", line):
                break
            if line.lstrip().startswith(HANG_PREFIXES):
                return 'INVALID', 'timeout/watchdog panic is not an assertion', []
    if case.kind == 'location':
        valid = (tuple(payloads[:-1]) == case.prelude
                 and payloads[-1] == 'called `Result::unwrap()` on an `Err` value: PoisonError { .. }'
                 and lines[headers[-1][0]].endswith(' panicked at ' + case.assertion + ':'))
    else:
        valid = (marker_is_scoped(case.assertion)
                 and re.fullmatch(case.payload_pattern, payloads[0]) is not None)
    if not valid:
        return 'INVALID', 'missing scoped legacy panic binding', []
    return 'KILLED', 'named test failed at its legacy binding', [case.test]


def _invalid(rows, reason):
    for row in rows:
        row.update(status='INVALID', reason=row['reason'] + '; ' + reason, hits=[])


def _document(rows, mode, selected, total):
    return {'mode': mode, 'selected': selected, 'total': total, 'controls': rows,
            'counts': {status: sum(row['status'] == status for row in rows)
                       for status in ('KILLED', 'SURVIVED', 'INVALID') +
                       (('PASS',) if any(row['status'] == 'PASS' for row in rows) else ())}}


def _revoke(output, identity):
    # Only our own inode is eligible. Never erase somebody else's file.
    for path in (output / 'summary.json.tmp', output / 'summary.json'):
        try:
            stat = path.lstat()
            if identity == (stat.st_dev, stat.st_ino):
                path.unlink()
        except FileNotFoundError:
            pass


def publish_summary(output, document, *, owner=None):
    """One atomic publication; revoke our inode even if rename then raises."""
    temporary = output / 'summary.json.tmp'
    target = output / 'summary.json'
    owner = {} if owner is None else owner
    try:
        with temporary.open('x', encoding='utf-8') as stream:
            stat = os.fstat(stream.fileno())
            owner['identity'] = (stat.st_dev, stat.st_ino)
            json.dump(document, stream, indent=2)
            stream.write('\n')
        temporary.replace(target)
    except BaseException:
        _revoke(output, owner.get('identity'))
        raise


def run(cases, output, *, guard, execute, oracle, selected=None, environ=None):
    """Run all selected cases and return 0 / pre-guard 1 / post-guard 2.

    guard() must restore AND verify on exit, including exceptional exits. The
    entered flag changes only after __enter__ returns. Fatal BaseExceptions
    revoke every row and suppress publication; ordinary per-case Exceptions
    record INVALID and continue. No summary exists on a fatal lifecycle error.
    """
    entered = False
    published = False
    rows = []
    mode, names, total = 'full', [], 0
    failure = ''
    owner = {}
    try:
        with sigint_default():
            table, chosen = select_cases(cases, selected, oracle)
            names, total = [case.mutation for case in chosen], len(table)
            mode = 'full' if len(chosen) == total else 'sample'
            rows = [dict(mutation=case.mutation, test=case.test, assertion=case.assertion,
                         status='INVALID', reason='not run', hits=[]) for case in chosen]
            env = environment(os.environ if environ is None else environ)
            out = claim_output(output)
            with guard():
                entered = True
                try:
                    for case, row in zip(chosen, rows):
                        try:
                            result = execute(case, env)
                            status, reason, hits = classify(result, case)
                            row.update(status=status, reason=reason, hits=hits)
                        except Exception as error:
                            row.update(status='INVALID', reason=f'{type(error).__name__}: {error}',
                                       hits=[])
                except BaseException as error:
                    # Retain this even if a broken guard suppresses it.
                    failure = f'{type(error).__name__}: {error}'
                    raise
            if not failure:
                publish_summary(out, _document(rows, mode, names, total), owner=owner)
                published = True
    except BaseException as error:
        failure = f'{type(error).__name__}: {error}'
    finally:
        # The exit code is derived here, never from SystemExit.code or type.
        if failure:
            try:
                if owner:
                    _revoke(out, owner.get('identity'))
            except BaseException as error:
                failure += f'; publication rollback: {type(error).__name__}: {error}'
            _invalid(rows, failure)
        success = published and not failure and bool(rows) and all(
            row['status'] in ('KILLED', 'PASS') for row in rows)
        code = 0 if success else (2 if entered else 1)
    return RunResult(code, _document(rows, mode, names, total), failure)
