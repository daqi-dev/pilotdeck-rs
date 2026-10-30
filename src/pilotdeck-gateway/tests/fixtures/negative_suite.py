# anchor-selfcheck: unsupported shared mutation suite runner, no mutation definitions
"""Run every selected control without treating harness failures as kills."""
from dataclasses import dataclass
import json
from pathlib import Path
import subprocess

import pending_reanchor as pending

from negative_process import mutating_sources, run_owned, wait_for_slot


@dataclass
class Case:
    mutation: str
    path: object
    old: str
    new: str
    test: str
    assertion: object
    scope: object = None


def passed(result, test):
    log = result.stdout + result.stderr
    return (result.returncode == 0 and f'test {test} ... ok' in log
            and 'test result: ok. 1 passed;' in log)


def classify(result, case):
    log = result.stdout + result.stderr
    if passed(result, case.test):
        return 'SURVIVED', 'bound test still passed'
    anchors = case.assertion if isinstance(case.assertion, list) else [case.assertion]
    if (result.returncode != 0 and 'test result: FAILED' in log
            and f'test {case.test} ... FAILED' in log and 'panicked at' in log
            and any(anchor in log for anchor in anchors)):
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


def publish_summary(out, results):
    """Publish the one final summary, atomically, after restore verification.

    ``record`` keeps every row in memory while the guarded window is open. An
    earlier KILLED row must never be readable from disk before
    ``mutating_sources`` restored and re-verified every source: a cancellation
    in that window would otherwise pair a stale all-green file with a nonzero
    exit. The payload shape is unchanged; the temp-and-rename write keeps a
    reader from observing a half-written JSON document (#166 r8).
    """
    target = Path(out) / 'summary.json'
    temporary = target.with_name(target.name + '.tmp')
    temporary.write_text(json.dumps({
        'controls': results,
        'counts': pending.counts(results),
    }, indent=2) + '\n')
    temporary.replace(target)


def run_cases(root, cases, out, env, mutate, *, timeout=900, baseline='HEAD', script=None):
    """Return an exit code; each case must pass before and after mutation."""
    results = []
    command = ['cargo', 'test', '--offline', '--manifest-path', 'src/Cargo.toml',
               '-p', 'pilotdeck-gateway', '--lib']

    def run(label, test):
        try:
            slot = wait_for_slot(root / 'scripts/cargo-slots.sh', timeout=900)
            (out / f'{label}.slots.log').write_text(slot.stdout + slot.stderr)
            # Exact binding still runs one test, including explicit TS/regeneration guards.
            result = run_owned(command + [test, '--', '--exact', '--include-ignored', '--nocapture'],
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
        print(f'{case.mutation}: {status}: {reason}', flush=True)

    # Where the guarded window raised, for the three-state exit contract:
    # 'enter' is a refusal to run (HEAD/baseline mismatch, busy lock, bad
    # arguments) and keeps the historical rc=1; 'restore'/'exit' are
    # unverified restores and must exit 2; 'body' is an in-window
    # precondition such as an expired anchor, also a refusal (#1076 fix-r1).
    phase = 'enter'
    try:
        registrations = pending.for_script(root, script)
        paths = list(dict.fromkeys(c.path for c in cases))
        if registrations:
            paths.append(root / pending.FIXTURES / pending.REGISTRY)
        with mutating_sources(root, paths, baseline=baseline) as originals:
            phase = 'body'
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
                    if not passed(run(case.mutation + '-positive', case.test), case.test):
                        reason = 'positive control did not pass the bound test'
                    else:
                        stage = 'negative'
                        case.path.write_text(changed)
                        status, reason = classify(run(case.mutation + '-negative', case.test), case)
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
                    if not passed(run(case.mutation + '-restored', case.test), case.test):
                        status, reason = 'INVALID', reason + '; restored bound test did not pass'
                except Exception as error:
                    status, reason = 'INVALID', reason + f'; restored: {type(error).__name__}: {error}'
                record(case, status, reason)
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
        if phase in ('restore', 'exit') or not isinstance(error, Exception):
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


def judge(result, prefix, targets, receipts, *, any_cell=False):
    """Require named FAILED and this run's exact assertion receipt, never prose."""
    if (result.returncode != 101 or not targets
            or len({test for test, _ in targets}) != len(targets)):
        return False, 'missing runtime failure or invalid targets', []
    # --show-output captures diagnostics until after each complete result line.
    # A receipt still cannot substitute for the named libtest failure.
    lines = set(result.stdout.splitlines())
    hits = [test for test, assertion in targets
            if f'test {prefix}{test} ... FAILED' in lines
            and receipts.matches(prefix + test, assertion)]
    unrelated = [test for test, _ in targets
                 if f'test {prefix}{test} ... FAILED' in lines and test not in hits]
    if unrelated:
        return False, f'declared failures lack current assertion receipts: {unrelated}', hits
    killed = bool(hits) if any_cell else len(hits) == len(targets)
    return killed, ('named assertions produced current nonce receipts' if killed else
                    'missing named FAILED line or current assertion receipt'), hits


def _write_new(path, text):
    with path.open('x') as stream:
        stream.write(text)


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
        raise SystemExit('usage: script --check | [--compile-only] NEW_OUTPUT_DIR [mutation ...]')
    selected = [row for row in rows if not args[1:] or row[0] in args[1:]]
    if not selected or (args[1:] and len(selected) != len(args[1:])):
        raise SystemExit('unknown/duplicate mutation')
    env = dict(os.environ)
    if not env.get('CARGO_TARGET_DIR'):
        raise SystemExit('CARGO_TARGET_DIR must be supplied by the caller')
    # Positive/compile/restored runs never inherit a stale protocol from a caller.
    env.pop(ReceiptRun.ENV, None)
    env.pop(ReceiptRun.PARENT_ENV, None)
    out = Path(args[0]).resolve()
    out.mkdir(parents=True, exist_ok=False)
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
        command = ['cargo', 'test', '--offline', '-p', 'pilotdeck-gateway', '--lib']
        if compile:
            command += ['--no-run']
        else:
            command += [prefix + targets(row)[0][0] if exact else prefix, '--',
                        '--show-output', '--format=pretty', '--test-threads=1']
            if exact:
                command.append('--exact')
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

    def positive(result):
        # Keep the previous matrix's pass/child cardinalities.
        return (result.returncode == 0
                and f'test result: ok. {passed_count} passed; 0 failed;' in result.stdout
                and result.stdout.count('PDRS_VERIFIED_CHILD ') == child_count)

    def execute(row, item, originals):
        name, source, old, new = row[:4]
        original = originals[source].decode()
        anchor(row, original)
        try:
            if not compile_only and not positive(run(name + '-positive', row, env)):
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
                killed, reason, hits = judge(result, prefix, targets(row), receipts,
                                             any_cell=name in any_cell)
                item.update(status='KILLED' if killed else
                            ('SURVIVED' if positive(result) else 'INVALID'),
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
            item['restoredPass'] = positive(run(name + '-restored', row, env))
            if not item['restoredPass']:
                item.update(status='INVALID', reason=item['reason'] + '; restored matrix failed')

    for row in selected:
        name, source, old, new = row[:4]
        item = dict(name=name, status='INVALID', reason='not run', compiled=False,
                    restoredPass=False, targets=targets(row))
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
        _write_new(out / f'{name}.result.json', json.dumps(item, indent=2) + '\n')
        print(f'{name}: {item["status"]}: {item["reason"]}', flush=True)
    counts = pending.counts(results, ('KILLED', 'SURVIVED', 'INVALID', 'COMPILED'))
    _write_new(out / 'summary.json', json.dumps({'controls': results, 'counts': counts}, indent=2) + '\n')
    print(json.dumps(counts), flush=True)
    success = 'COMPILED' if compile_only else 'KILLED'
    return int(any(row['status'] != success for row in results))
