# anchor-selfcheck: unsupported shared mutation suite runner, no mutation definitions
"""Run every selected control without treating harness failures as kills."""
from dataclasses import dataclass
import itertools
import json
import os
from pathlib import Path
import re
import subprocess

import pending_reanchor as pending

from negative_panic import named_test_failed, panic_evidence
from negative_process import mutating_sources, run_owned, wait_for_slot
from negative_selection import (claim_dir, missing_target, refuse,
                                refuse_unusable_target)


@dataclass
class Case:
    mutation: str
    path: object
    old: str
    new: str
    test: str
    assertion: object
    scope: object = None


def short_test(test):
    """The last component of a Rust test identity, for evidence file names."""
    return test.rsplit('::', 1)[-1]


def bind_module_file(test, modules):
    """Bind a full test identity to the one file that declares its module.

    ``modules`` is an ordered iterable of ``(prefix, path)`` pairs; the longest
    /most specific prefix has to come first (``forwarding::`` sits inside
    ``stream_tests::``). #1890: six audit implementations had drifted copies of
    this rule, two of them scanned a concatenation of several files, so a
    same-named short test in a sibling module could satisfy the binding. This
    returns ``(path, short)`` and rejects the two shapes that cannot be exact:
    a prefix that is not listed at all, and a further ``::`` level or an empty
    short name under a listed prefix (an empty short name matches ``fn(`` and
    ``=>`` in any compacted source, so it silently audits nothing).
    """
    for prefix, path in modules:
        if not test.startswith(prefix):
            continue
        short = test[len(prefix):]
        if not short:
            raise ValueError('bound test has an empty short name: ' + test)
        if '::' in short:
            raise ValueError('bound test has an extra module level: ' + test)
        return path, short
    raise ValueError('bound test module not audited: ' + test)


_FN_QUALIFIERS = ('async', 'const', 'unsafe', 'extern', 'pub', 'default')

# #2064 r10: the ``fn`` form's left boundary. A plain word boundary would
# refuse ``async fn case_a`` (compaction glues the qualifier onto ``fn``), so
# the boundary whitelists exactly the qualifier words -- and only as whole
# words: ``run_fncase_a(`` and ``xpubfncase_a(`` stay refused because their
# qualifier-looking suffix does not start at a word boundary of its own.
_FN_LEFT = r'(?<!\w)(?:(?:' + '|'.join(_FN_QUALIFIERS) + r'))*'

# #2064 r11: ``c``/``cr`` are legal Rust C-string prefixes; leaving them out
# let a raw C string's inner quote close a bogus plain string and return the
# carried text to the code view (an audit hole, not a crash). ``cr`` must be
# tried before ``c`` or the ``r`` is left behind as code.
_STRING_START = re.compile(r'(?:cr|c|br|rb|r|b)?(#*)"')
# #2064 r11: ``'\u{…}'`` / ``'\xNN'`` are single-char literals whose escape
# bodies the old pattern could not close, so their quotes leaked into the code
# view and could pair into bogus ``'x'`` matches eating neighbouring code.
_CHAR_START = re.compile(r"b?'(?:\\u\{[0-9A-Fa-f_]+\}|\\x[0-9A-Fa-f]{2}|\\.|[^\\'])'")
_WORD = re.compile(r'\w')


def _token_start(source, index):
    """True when ``index`` is not glued to the tail of an identifier."""
    return index == 0 or not _WORD.match(source[index - 1])


def _rust_code_only(source):
    """Return ``source`` with every comment and literal replaced by a space.

    #2064 r10: a binding that a comment or a string literal satisfies is a
    binding that audits nothing (``// fn case_a(`` passed the old matcher), so
    the noise carriers are blanked before any form is matched. The scan is a
    single pass that knows only the lexical shapes Rust itself guarantees;
    anything it cannot close -- an unterminated string, char or comment --
    raises instead of guessing, per the fail-closed scanning rule (#449 /
    #1166: a hand-rolled guess that mis-lexes is worse than refusing).
    """
    code = []
    i, length = 0, len(source)
    while i < length:
        if source.startswith('//', i):
            end = source.find('\n', i)
            i = length if end < 0 else end
            code.append(' ')
            continue
        if source.startswith('/*', i):
            depth, i = 1, i + 2
            while depth:
                if source.startswith('/*', i):
                    depth, i = depth + 1, i + 2
                elif source.startswith('*/', i):
                    depth, i = depth - 1, i + 2
                elif i >= length:
                    raise ValueError('unterminated Rust block comment')
                else:
                    i += 1
            code.append(' ')
            continue
        start = None
        if source[i] == '"' or (source[i] in 'rbc' and _token_start(source, i)):
            start = _STRING_START.match(source, i)
        if start is not None:
            hashes = start.group(1) or ''
            i = start.end()
            # #2064 r4: a raw hash delimiter with no ``r``-bearing prefix
            # (``c#"…"#``) is not legal Rust -- rustc 1.99.0 rejects it with
            # ``error: prefix `c` is unknown`` -- so refuse it instead of
            # guessing a plain string whose inner quote would return the
            # carried text to the code view (#449 / #1166 fail-closed rule).
            if hashes and 'r' not in start.group(0):
                raise ValueError('invalid Rust string prefix with raw hash delimiter')
            if 'r' in start.group(0):
                close = source.find('"' + hashes, i)
                if close < 0:
                    raise ValueError('unterminated Rust raw string')
                i = close + 1 + len(hashes)
            else:
                # A literal newline is legal inside a Rust string, so only
                # the end of the source can prove the literal unterminated.
                while True:
                    if i >= length:
                        raise ValueError('unterminated Rust string')
                    if source[i] == '"':
                        i += 1
                        break
                    i += 2 if source[i] == '\\' else 1
            code.append(' ')
            continue
        char = None
        if source[i] == "'" or (source[i] == 'b' and _token_start(source, i)):
            char = _CHAR_START.match(source, i)
        if char is not None:
            i = char.end()
            code.append(' ')
            continue
        code.append(source[i])
        i += 1
    return ''.join(code)


def declares_test(source, short, macro=None):
    """True when ``source`` declares ``short`` as a function or a table row.

    An empty ``short`` is refused outright (#2064): it degenerates the table
    form to a bare ``=>`` that any arrow at a boundary satisfies (a tuple
    match arm), so a mistyped row would audit nothing while ``--check``
    stayed green. Comments and string/char literals are blanked before
    matching (#2064 r10): a binding that a comment carrier satisfies is
    equally hollow. Whitespace is stripped so the three drifted spellings
    (``'fn ' + name``, ``'fn' + name``, ``name + ' =>'``) collapse into one
    rule, and every form requires a left boundary: a longer identifier that
    merely ends in ``short`` -- ``run_fncase_a(`` or ``xcase_a =>`` -- must
    not satisfy the binding. Compaction glues the function qualifiers onto
    ``fn`` (``async fn foo`` becomes ``asyncfnfoo``), so the ``fn`` form's
    boundary whitelists exactly the qualifier words, and only as whole words.
    ``macro`` accepts the table form ``macro!(short, ...)`` used by the
    live-stream cases.
    """
    if not short:
        raise ValueError('declares_test needs a short name')
    compact = ''.join(_rust_code_only(source).split())
    escaped = re.escape(short)
    if re.search(_FN_LEFT + r'fn' + escaped + r'\(', compact):
        return True
    if re.search(r'(?<!\w)' + escaped + '=>', compact):
        return True
    return bool(macro) and re.search(
        r'(?<!\w)' + re.escape(macro) + r'\(' + escaped + ',', compact) is not None


def case_labels(cases):
    """One unique evidence stem per Case, computed across the whole run.

    #1413 fix-r4: #1365 fix-r3 deduplicated labels only inside one mutation
    group, so a multi-test row's derived stem (``<row>-<short>``) could equal
    another row's bare mutation name. Both Cases then wrote the same
    ``-positive``/``-negative``/``-restored`` files: the second silently
    replaced the first's evidence while the summary still counted every Case.
    Uniqueness is now global. Single-Case rows keep their historical bare
    mutation name because existing evidence paths depend on it; a Case that
    would collide falls back to its whole test identity plus an ordinal.
    """
    groups = {}
    for case in cases:
        groups.setdefault(case.mutation, []).append(case)

    proposed = {}
    for mutation, group in groups.items():
        if len(group) == 1:
            proposed[id(group[0])] = mutation
            continue
        shorts = [short_test(case.test) for case in group]
        unique = len(set(shorts)) == len(shorts)
        for case, short in zip(group, shorts):
            base = short if unique else case.test.replace('::', '.')
            proposed[id(case)] = f'{mutation}-{base}'

    # Single-Case bare names are admitted first: they are the historical
    # paths. Every later stem is admitted only if no earlier Case claimed it.
    ordered = ([case for case in cases if len(groups[case.mutation]) == 1]
               + [case for case in cases if len(groups[case.mutation]) > 1])
    labels = {}
    taken = set()
    for case in ordered:
        stem = proposed[id(case)]
        if stem in taken:
            stem = f'{case.mutation}-{case.test.replace("::", ".")}'
        base = stem
        ordinal = 1
        while stem in taken:
            ordinal += 1
            stem = f'{base}-{ordinal}'
        labels[id(case)] = stem
        taken.add(stem)
    return labels


def passed(result, test):
    log = result.stdout + result.stderr
    # `--nocapture` can interleave a test's stdout between the name and the
    # status word (#938 r14), so accept the exact name but not an arbitrary
    # suffix after `... ok`.
    named_ok = re.search(r'^test ' + re.escape(test) + r' \.\.\..* ok$', log,
                         re.MULTILINE)
    # #1637: a bound test that skipped *itself* also prints the marker line
    # `PDRS_TEST_SKIPPED <short-name>: skipped, …` (tests/common/skip_line.rs
    # `TEST_SKIPPED_MARKER`, written straight to the stderr handle so the
    # harness capture cannot hide it) and exits green. That is not a passing
    # positive control: on this host the row certified nothing, and counting
    # it as a pass is the vacuous positive #1637 closes. Only whole-test
    # self-skips carry the marker; a test that ran and skipped one scenario
    # of many has no marker line and still passes on its own merits.
    self_skipped = re.search(r'^PDRS_TEST_SKIPPED ' + re.escape(short_test(test)) + r'\b',
                             log, re.MULTILINE)
    return (result.returncode == 0 and named_ok is not None
            and self_skipped is None
            and 'test result: ok. 1 passed;' in log)


def classify(result, case):
    log = result.stdout + result.stderr
    if passed(result, case.test):
        return 'SURVIVED', 'bound test still passed'
    anchors = case.assertion if isinstance(case.assertion, list) else [case.assertion]
    # #818/#819/#930: only the named test's own FAILED line plus its own panic
    # payload can kill. A sibling test's panic, ordinary stdout, or the named
    # test's `... ok` line is not evidence.
    evidence = panic_evidence(log, case.test)
    if (result.returncode != 0 and named_test_failed(log, case.test)
            and 'test result: FAILED' in log
            and 'could not compile' not in log and evidence
            # #818 fix-r4: a list is a set of alternatives: one mutation can
            # fail at one of several assertions (or a panic stops the test
            # before the later one), so any single bound marker kills.
            and any(anchor in evidence for anchor in anchors)):
        return 'KILLED', 'bound test failed at its assertion'
    return 'INVALID', 'missing bound runtime failure (build error, wrong panic, or missing test)'


def invalidate_completed(results, reason):
    """A failed source guard invalidates every row already recorded.

    The guard restores and re-verifies every mutated file on context exit. When
    it raises there, a previously recorded KILLED row is no longer trustworthy:
    the tree may not be restored. Leaving it green would publish a false pass
    (#979 P0), so this is the single implementation shared by ``run_cases`` and
    the SQLite-header executor. Revoke optional compilation receipts too
    (#1037), keeping the original schema of runners that do not publish a
    ``compiled`` field.
    """
    for row in results:
        row.update(status='INVALID', reason=row['reason'] + '; ' + reason)
        if 'compiled' in row:
            row['compiled'] = False


def publish_summary(out, results, states=('KILLED', 'SURVIVED', 'INVALID')):
    """Publish the one final summary, atomically, after restore verification.

    ``states`` widens the machine-readable ``counts`` block for a caller that
    records its own platform-absent rows (native-management's ``SKIPPED``);
    the default keeps every existing caller's payload byte-identical.

    ``record`` keeps every row in memory while the guarded window is open. An
    earlier KILLED row must never be readable from disk before
    ``mutating_sources`` restored and re-verified every source: a cancellation
    in that window would otherwise pair a stale all-green file with a nonzero
    exit. The payload shape is unchanged; the temp-and-rename write keeps a
    reader from observing a half-written JSON document (#166 r8).
    """
    _publish_json(Path(out) / 'summary.json',
                  {'controls': results, 'counts': pending.counts(results, states)},
                  overwrite=True)


def run_cases(root, cases, out, env, mutate, *, timeout=900, baseline='HEAD', script=None,
              labels=None):
    """Return an exit code; each case must pass before and after mutation.

    ``labels`` may carry stems computed over a wider Case set (the equivalent
    measurement shares this output directory), so evidence files stay unique
    across both phases; by default they are computed from ``cases`` alone.
    """
    results = []
    command = ['cargo', 'test', '--offline', '--manifest-path', 'src/Cargo.toml',
               '-p', 'pilotdeck-gateway', '--lib']

    # #1365 fix-r3 P1: one mutation row may bind several tests, one ``Case``
    # each. Naming the evidence by the mutation alone made every later Case
    # overwrite the earlier Case's positive/negative/restored logs, so a
    # multi-test row kept raw output for its last test only while the summary
    # still reported one row per test — an evidence gap under a green count.
    # Multi-Case rows therefore carry a per-Case component in the log stem and
    # in the printed verdict; single-Case rows keep their historical names so
    # existing evidence paths stay valid. #1413 fix-r4 moved uniqueness out to
    # ``case_labels``, which deduplicates globally instead of per row.
    labels = case_labels(cases) if labels is None else dict(labels)

    def stem(case):
        return labels[id(case)]

    def run(label, test):
        try:
            slot = wait_for_slot(root / 'scripts/cargo-slots.sh', timeout=900)
            (out / f'{label}.slots.log').write_text(slot.stdout + slot.stderr)
            # Exact binding still runs one test, including explicit TS/regeneration guards.
            # No `--nocapture`: it interleaves test stdout with the
            # `test <name> ... ok` status line (#938 r14), which would make the
            # exact named-test judge miss a real FAILED line.
            result = run_owned(command + [test, '--', '--exact', '--include-ignored'],
                               cwd=root, env=env, timeout=timeout)
            (out / f'{label}.log').write_text(result.stdout + result.stderr)
            return result
        except Exception as error:
            raw = ''
            if isinstance(error, subprocess.TimeoutExpired):
                for value in (error.stdout, error.stderr):
                    raw += value.decode(errors='replace') if isinstance(value, bytes) else value or ''
            with (out / f'{label}.log').open('a') as log:
                log.write(raw + f'\n{type(error).__name__}: {error}\n')
            raise

    def record(case, status, reason):
        # Memory only: the final summary is published after the guarded
        # restore/re-verify window has closed successfully (#1076).
        results.append({'mutation': case.mutation, 'test': case.test,
                        'assertion': case.assertion, 'status': status, 'reason': reason})
        print(f'{stem(case)}: {status}: {reason}', flush=True)

    # Where the guarded window raised, for the three-state exit contract:
    # 'enter' is a refusal to run (HEAD/baseline mismatch, busy lock, bad
    # arguments) and keeps the historical rc=1; 'restore'/'exit' are
    # unverified restores and must exit 2; 'body' is an in-window
    # precondition such as an expired anchor, also a refusal (#1076 fix-r1).
    # ``body_error`` remembers the object the guarded body raised. A guard that
    # raises while unwinding raises a *different* object even though ``phase``
    # still says 'body'; that is an unverified restore and must exit 2
    # (#1076 fix-r2). A stale anchor refused by a guard that then restores
    # cleanly keeps the historical rc=1.
    phase = 'enter'
    body_error = None
    try:
        registrations = pending.for_script(root, script)
        paths = list(dict.fromkeys(c.path for c in cases))
        if registrations:
            paths.append(root / pending.FIXTURES / pending.REGISTRY)
        with mutating_sources(root, paths, baseline=baseline) as originals:
            phase = 'body'
            try:
                for case in cases:
                    entry = pending.match(registrations, case.mutation)
                    if entry is not None:
                        record(case, pending.SKIPPED, entry.reason())
                        continue
                    status, reason, stage = 'INVALID', '', 'anchor'
                    try:
                        changed = mutate(originals[case.path].decode(), case.scope,
                                         [(case.old, case.new)])
                        stage = 'positive'
                        if not passed(run(stem(case) + '-positive', case.test), case.test):
                            reason = 'positive control did not pass the bound test'
                        else:
                            stage = 'negative'
                            case.path.write_text(changed)
                            status, reason = classify(run(stem(case) + '-negative', case.test), case)
                    except Exception as error:
                        status, reason = 'INVALID', f'{stage}: {type(error).__name__}: {error}'
                    finally:
                        # Restore before every successor, also on timeout or bad anchors.
                        # The enclosing guard independently restores and verifies on exit.
                        phase = 'restore'
                        case.path.write_bytes(originals[case.path])
                        if case.path.read_bytes() != originals[case.path]:
                            raise RuntimeError(f'byte restoration failed: {case.path}')
                        phase = 'body'
                    try:
                        if not passed(run(stem(case) + '-restored', case.test), case.test):
                            status, reason = 'INVALID', reason + '; restored bound test did not pass'
                    except Exception as error:
                        status, reason = 'INVALID', reason + f'; restored: {type(error).__name__}: {error}'
                    record(case, status, reason)
            except BaseException as error:
                # Keep the refusal/cancellation object so the handler below can
                # tell it apart from a failure raised by the guard exit.
                body_error = error
                raise
            # The body is done; an exception from here on comes from the
            # guard's own restore/re-verify on context exit.
            phase = 'exit'
    except BaseException as error:
        # A baseline/lock/restoration failure — or a cancellation delivered by
        # the guard's restore/re-verify window — makes every recorded row
        # untrustworthy. Void them in memory, account for unrun cases, publish
        # the INVALID summary, and exit 2. No final summary existed before this
        # point, so an interrupt during this publish leaves no summary at all
        # (missing is fail-closed), never the stale all-KILLED file (#1076).
        reason = f'source guard: {type(error).__name__}: {error}'
        invalidate_completed(results, reason)
        for case in cases[len(results):]:
            record(case, 'INVALID', reason)
        publish_summary(out, results)
        # A raw SystemExit code must not decide the process status: 256/-256
        # truncate to zero on POSIX and a string code exits 1. Re-raise the
        # normalized code; a KeyboardInterrupt returns 2 from run_cases.
        if isinstance(error, SystemExit):
            raise SystemExit(2) from error
        # #1076 fix-r1 exit contract: only an unverified restore or a
        # cancellation-invalidated run may use rc=2. A refusal to run before
        # the guard opened (HEAD/baseline mismatch, busy lock, bad argument)
        # keeps the historical rc=1 that callers already assert
        # (``pending-reanchor/test_controls.py`` HEAD-failure exit).
        # #1076 fix-r2: when the body already raised, a *different* exception
        # here can only come from the guard's own restore/re-verify while
        # unwinding, so the run is void even though ``phase`` is 'body'.
        if (phase in ('restore', 'exit') or not isinstance(error, Exception)
                or (body_error is not None and error is not body_error)):
            return 2
        return 1
    # Reaching here means mutating_sources restored every source and verified
    # it against the baseline. Only now may a final summary exist.
    publish_summary(out, results)
    counts = pending.counts(results)
    print('summary: ' + ', '.join(f'{state}={count}' for state, count in counts.items()), flush=True)
    return int(not results or any(row['status'] != 'KILLED' for row in results))


# #946 matrix protocol. The legacy Case API above belongs to a separate migration.
# Only libtest's named FAILED lines remain textual; panic/diagnostic prose is
# never evidence. Each run owns new files, keyed by (full test, exact assertion).
class ReceiptRun:
    ENV = 'PDRS_NEGATIVE_RECEIPTS'
    PARENT_ENV = 'PDRS_NEGATIVE_PARENT_TEST'

    def __init__(self, out, label, targets):
        import secrets
        from pathlib import Path
        self.directory = Path(out).resolve() / (label + '.receipts')
        self.directory.mkdir(exist_ok=False)
        if not targets or len({test for test, _ in targets}) != len(targets):
            raise ValueError('nonempty targets with unique test identities required')
        self.nonce = secrets.token_hex(32)
        self.paths = {target: self.directory / f'{index}.receipt'
                      for index, target in enumerate(targets)}
        self.config = {'nonce': self.nonce, 'targets': [
            {'test': test, 'assertion': assertion, 'path': str(path)}
            for (test, assertion), path in self.paths.items()]}
        _write_new(self.directory / 'run.json', json.dumps(self.config, indent=2) + '\n')

    def environment(self, env):
        result = dict(env)
        result.pop(self.PARENT_ENV, None)
        result[self.ENV] = json.dumps(self.config)
        return result

    def matches(self, test, assertion):
        import os
        import stat
        path = self.paths.get((test, assertion))
        if path is None:
            return False
        try:
            # No symlink/device/FIFO, and no unbounded reads of corrupt output.
            fd = os.open(path, os.O_RDONLY | os.O_NONBLOCK | os.O_NOFOLLOW)
            with os.fdopen(fd, 'rb') as stream:
                if not stat.S_ISREG(os.fstat(stream.fileno()).st_mode):
                    return False
                return stream.read(65) == self.nonce.encode('ascii')
        except OSError:
            return False


def cell_states(result, prefix, targets, receipts):
    """Per-target observation states behind one verdict.

    Never a verdict on its own and never written to the result row: ``judge``
    owns both. The row stays inside the schema registered with
    ``_negkit/compare_verdicts.py``, whose unknown-field check rejects any new
    key and would turn every ``matrix_main`` summary into a false FAIL
    (#1579 P1). Kept as a separate function so the three-value ``judge``
    contract sixteen fixtures import stays untouched.
    """
    if not targets or len({test for test, _ in targets}) != len(targets):
        return []
    # --show-output captures diagnostics until after each complete result line.
    # A receipt still cannot substitute for the named libtest failure.
    lines = set(result.stdout.splitlines())
    cells = []
    for test, assertion in targets:
        failed = f'test {prefix}{test} ... FAILED' in lines
        if failed and receipts.matches(prefix + test, assertion):
            cells.append({'test': test, 'status': 'KILLED'})
        elif failed:
            cells.append({'test': test, 'status': 'NO_RECEIPT'})
        elif f'test {prefix}{test} ... ok' in lines:
            cells.append({'test': test, 'status': 'OK'})
        else:
            cells.append({'test': test, 'status': 'NOT_RUN'})
    return cells


def judge(result, prefix, targets, receipts, *, any_cell=False, require_all_ran=False):
    """Require named FAILED and this run's exact assertion receipt, never prose.

    ``require_all_ran`` pins the exact-mode contract: a command built from only
    the first declared target still runs and kills, so the row would collect a
    verdict for cells that never executed (#1579 P1). Every declared target
    must then show either its named FAILED plus receipt or its named ``ok``
    line. Prefix mode keeps the old leniency: an ``any_cell`` row may declare
    cells that an early return makes unreachable (#879b), and those never
    appear as ``ok``.
    """
    cells = cell_states(result, prefix, targets, receipts)
    if result.returncode != 101 or not cells:
        return False, 'missing runtime failure or invalid targets', []
    hits = [cell['test'] for cell in cells if cell['status'] == 'KILLED']
    unrelated = [cell['test'] for cell in cells if cell['status'] == 'NO_RECEIPT']
    if unrelated:
        return False, f'declared failures lack current assertion receipts: {unrelated}', hits
    if require_all_ran:
        missing = [cell['test'] for cell in cells if cell['status'] == 'NOT_RUN']
        if missing:
            return False, f'declared targets never ran: {missing}', hits
    killed = bool(hits) if any_cell else len(hits) == len(targets)
    return killed, ('named assertions produced current nonce receipts' if killed else
                    'missing named FAILED line or current assertion receipt'), hits


def _write_new(path, text):
    with path.open('x') as stream:
        stream.write(text)


_PUBLISH_SERIAL = itertools.count()


def _publish_json(path, payload, *, overwrite=False):
    """Publish one JSON evidence file atomically; refuse by default.

    `_write_new` creates the file empty and then writes, so a reader that
    tails the evidence directory can observe a half-written document. Both
    write paths share this one implementation:

    * `matrix_main` publishes into a directory it created exclusively, so the
      historical `open('x')` refusal is kept — `os.link` installs the finished
      document or fails with `FileExistsError`, never replacing a file that
      appeared while the temporary one was being written.
    * `publish_summary` (the `run_cases` path) re-publishes into an existing
      output directory, so it opts into `overwrite=True` and gets the same
      temp-file write with a `replace` (#166 r8).

    The temp name carries the pid and a module-level serial, so two runs that
    publish into one directory can never install each other's bytes or unlink
    each other's temp file (#1579 r5: a fixed `<name>.tmp` let the writer of
    one run `replace()` the half-written temp of another). The serial is a
    module counter, not process-global state: two *instances* of this module
    inside one process would both start at 0, so run each publish from one
    import (#1694).

    `os.link` needs a filesystem that supports hard links; where it does not
    (exFAT, some FUSE/SMB mounts, some container volumes) the publish fails
    loudly with the path and the reason instead of a bare `OSError`, and it
    still never falls back to a non-atomic install (#1694).
    """
    temporary = path.with_name(f'.{path.name}.{os.getpid()}.{next(_PUBLISH_SERIAL)}.tmp')
    try:
        temporary.write_text(json.dumps(payload, indent=2) + '\n')
        if overwrite:
            temporary.replace(path)
        else:
            try:
                os.link(temporary, path)
            except FileExistsError:
                raise
            except OSError as error:
                raise OSError(
                    f'cannot publish {path}: the evidence directory does not support '
                    f'hard links ({error.strerror or error}); use a filesystem that does, '
                    'or pass overwrite=True into a directory you own') from error
    finally:
        try:
            temporary.unlink()
        except OSError:
            # Never mask the publish error with a cleanup error; a leaked
            # hidden temp is inert (nothing reads dotfiles in the evidence dir).
            pass


# The `controls[]` row schema published to `_negkit/compare_verdicts.py`
# (#1579 P1). Its `normalize` rejects any key outside the registered whitelist
# and exits 2, so a summary that grows a field becomes un-comparable — a false
# FAIL for every consumer. Rows are therefore built only from these fields;
# new evidence must be registered with the comparator first.
RESULT_ROW_FIELDS = ('name', 'status', 'reason', 'compiled', 'restoredPass', 'targets', 'hits')


def result_row(name, *, status='INVALID', reason='not run', compiled=False,
               restored_pass=False, targets=(), hits=None):
    """One `controls[]` row, restricted to :data:`RESULT_ROW_FIELDS`.

    ``hits`` stays absent until a verdict observes it: the comparator
    normalizes a missing ``hits`` to ``null`` and treats ``null`` and ``[]`` as
    different, so writing ``[]`` on a row that never reached ``judge`` would
    turn an old/new comparison of the same run into a false NOT EQUIVALENT.
    """
    row = {'name': name, 'status': status, 'reason': reason,
           'compiled': compiled, 'restoredPass': restored_pass,
           'targets': [list(target) for target in targets]}
    if hits is not None:
        row['hits'] = list(hits)
    return row


def matrix_main(root, script, prefix, rows, *, passed_count, child_count,
                paths, any_cell=(), exact=False, args=None):
    """HEAD-only matrix controls; compile, run, restore, account for every row.

    Rows keep the fixture's declarative anchors for anchor_selfcheck. Six-field
    rows bind a list of cells to a message prefix, or one test to an exact
    assertion; five-field rows already contain (test, assertion) pairs.
    """
    import os
    import sys
    import difflib
    from pathlib import Path
    args = list(sys.argv[1:] if args is None else args)
    compile_only = '--compile-only' in args
    args = [arg for arg in args if arg != '--compile-only']

    def targets(row):
        if len(row) == 5:
            return list(row[4])
        tests, assertion = row[4:]
        if isinstance(tests, str):
            return [(tests, assertion)]
        return [(test, assertion + ' ' + test.removesuffix('_cwd').replace('_', '-'))
                for test in tests]

    def anchor(row, original):
        if original.count(row[2]) != 1 or row[2] == row[3] or not targets(row):
            raise ValueError(f'{row[0]}: stale/invalid anchor or empty targets')

    registrations = pending.for_script(root, script)
    if args == ['--check']:
        errors = []
        for row in rows:
            if pending.match(registrations, row[0]) is not None:
                errors.append(f'{row[0]}: {pending.SKIPPED}')
                continue
            try:
                anchor(row, row[1].read_text())
            except Exception as error:
                errors.append(str(error))
        print('\n'.join(errors) if errors else f'{len(rows)} unique anchors checked')
        return int(bool(errors))
    if not args:
        refuse('usage: script --check | [--compile-only] NEW_OUTPUT_DIR [mutation ...]')
    selected = [row for row in rows if not args[1:] or row[0] in args[1:]]
    if not selected or (args[1:] and len(selected) != len(args[1:])):
        # #1734: a mistyped selection never ran anything, so it is the same
        # "never started" refusal (2) as the target guard below -- not a run
        # that came out red.
        refuse('unknown/duplicate mutation')
    env = dict(os.environ)
    if missing_target(env):
        # #1734: the same "never started" taxonomy as the migrated entries.
        # `SystemExit('<string>')` carried the message but exited 1, which the
        # host triage table reads as "ran and did not pass".
        refuse('CARGO_TARGET_DIR must be supplied by the caller')
    refuse_unusable_target(env)  # #1666 fix-r10: dot / root / src are unusable too
    # Positive/compile/restored runs never inherit a stale protocol from a caller.
    env.pop(ReceiptRun.ENV, None)
    env.pop(ReceiptRun.PARENT_ENV, None)
    out = claim_dir(args[0])  # #1734: an existing directory is "never started" (2)
    fixture_root = Path(__file__).resolve().parent
    guarded = list(dict.fromkeys([*paths, Path(script).resolve(), Path(__file__).resolve(),
        fixture_root / 'negative_process.py', fixture_root.parent / 'common/negative_receipt.rs',
        root / 'src/pilotdeck-gateway/src/test_process.rs']))
    if registrations:
        guarded.append(root / pending.FIXTURES / pending.REGISTRY)
    results = []

    def run(label, row, run_env, *, compile=False):
        slot = wait_for_slot(root / 'scripts/cargo-slots.sh', timeout=600, interval=15)
        _write_new(out / f'{label}.slots.log', slot.stdout + slot.stderr)
        # No -q: cargo forwards it to libtest as --quiet, and the classifier
        # needs the named per-test lines even where --format=pretty would have
        # restored them (#273 fix-r1 lesson, made a standing rule by #942).
        # No `-q`: cargo forwards it to libtest as `--quiet`, which replaces
        # the per-test `test <name> ... FAILED` line that `judge()` requires
        # with `<name> --- FAILED` (#818 fix-r3).
        base = ['cargo', 'test', '--offline', '-p', 'pilotdeck-gateway', '--lib']
        libtest = ['--', '--show-output', '--format=pretty', '--test-threads=1']
        if compile:
            command = base + ['--no-run']
        elif exact:
            # Cargo takes one TESTNAME positionally; libtest takes several.
            # Every declared cell therefore travels after `--`, and `--exact`
            # pins each filter to its full name. Passing only targets(row)[0]
            # left the other cells unexecuted while the row was still recorded
            # as killed (#1579 P1); judge() now also refuses a row whose
            # declared targets produced no named result line.
            command = (base + libtest + ['--exact']
                       + [prefix + test for test, _ in targets(row)])
        else:
            command = base + [prefix] + libtest
        _write_new(out / f'{label}.command.json', json.dumps({
            'command': command, 'cwd': str(root / 'src'),
            'CARGO_TARGET_DIR': run_env['CARGO_TARGET_DIR']}, indent=2) + '\n')
        try:
            result = run_owned(command, cwd=root / 'src', env=run_env, timeout=900)
        except Exception as error:
            _write_new(out / f'{label}.error.txt', f'{type(error).__name__}: {error}\n')
            raise
        _write_new(out / f'{label}.stdout', result.stdout)
        _write_new(out / f'{label}.stderr', result.stderr)
        return result

    def positive(result, row):
        # Prefix mode keeps the matrix-wide cardinality. Exact mode runs exactly
        # the row's declared cells, so its pass count is that row's cell count
        # (#1579: the old first-target shortcut also pinned the count at one).
        expected = len(targets(row)) if exact else passed_count
        return (result.returncode == 0
                and f'test result: ok. {expected} passed; 0 failed;' in result.stdout
                and result.stdout.count('PDRS_VERIFIED_CHILD ') == child_count)

    def execute(row, item, originals):
        name, source, old, new = row[:4]
        original = originals[source].decode()
        anchor(row, original)
        try:
            if not compile_only and not positive(run(name + '-positive', row, env), row):
                raise RuntimeError('positive matrix did not pass with all child receipts')
            changed = original.replace(old, new, 1)
            _write_new(out / f'{name}.patch', ''.join(difflib.unified_diff(
                original.splitlines(True), changed.splitlines(True),
                fromfile=str(source.relative_to(root)), tofile=str(source.relative_to(root)))))
            source.write_text(changed)
            if run(name + '-compile', row, env, compile=True).returncode != 0:
                raise RuntimeError('mutant did not compile; never a kill')
            item['compiled'] = True
            if compile_only:
                item.update(status='COMPILED', reason='mutant cargo test --no-run passed')
            else:
                receipts = ReceiptRun(out, name + '-negative',
                                      [(prefix + t, a) for t, a in targets(row)])
                result = run(name + '-negative', row, receipts.environment(env))
                killed, reason, hits = judge(
                    result, prefix, targets(row), receipts,
                    any_cell=name in any_cell, require_all_ran=exact)
                item.update(status='KILLED' if killed else
                            ('SURVIVED' if positive(result, row) else 'INVALID'),
                            reason=reason, hits=hits)
        except Exception as error:
            item.update(status='INVALID', reason=f'{type(error).__name__}: {error}')
        finally:
            # Restore inside the guard before the positive check. The
            # guard also verifies every protected byte on context exit.
            source.write_bytes(originals[source])
            if source.read_bytes() != originals[source]:
                raise RuntimeError('source restoration failed')
        if not compile_only:
            item['restoredPass'] = positive(run(name + '-restored', row, env), row)
            if not item['restoredPass']:
                item.update(status='INVALID', reason=item['reason'] + '; restored matrix failed')

    for row in selected:
        name, source, old, new = row[:4]
        item = result_row(name, targets=targets(row))
        try:
            with mutating_sources(root, guarded) as originals:
                entry = pending.match(registrations, name)
                if entry is not None:
                    item.update(status=pending.SKIPPED, reason=entry.reason())
                else:
                    execute(row, item, originals)
        except Exception as error:
            item.update(status='INVALID', reason=f'{type(error).__name__}: {error}')
        results.append(item)
        _publish_json(out / f'{name}.result.json', item)
        print(f'{name}: {item["status"]}: {item["reason"]}', flush=True)
    counts = pending.counts(results, ('KILLED', 'SURVIVED', 'INVALID', 'COMPILED'))
    _publish_json(out / 'summary.json', {'controls': results, 'counts': counts})
    print(json.dumps(counts), flush=True)
    success = 'COMPILED' if compile_only else 'KILLED'
    return int(any(row['status'] != success for row in results))
