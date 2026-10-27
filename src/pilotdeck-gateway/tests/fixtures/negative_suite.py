"""Run every selected control without treating harness failures as kills."""
from dataclasses import dataclass
import json
import re
import subprocess

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


def failed_tests(stdout, prefix):
    """Failed names from the outer libtest ``failures:`` list only.

    With ``--nocapture`` (or parallel workers) a test's own output can land
    between the test name and its verdict, so ``... FAILED`` must not be
    required to stay adjacent to the name (#879). The indented list after the
    run is the one spelling libtest guarantees. Only the outer run's final
    list counts (#889): a bounded worker runs its own libtest, and its
    ``failures:`` list only ever surfaces embedded in a parent panic message
    on stderr, just like a panic echoing indented cell names, so both are
    excluded by reading stdout's last ``failures:`` section - which libtest
    prints after every test's own output.
    """
    lines = stdout.splitlines()
    headers = [i for i, line in enumerate(lines) if line == 'failures:']
    if not headers:
        return set()
    names = set()
    for line in lines[headers[-1] + 1:]:
        if not line.startswith('    '):
            if line:
                break  # the summary line ends the outer failures list
            continue
        name = line.strip()
        if name.startswith(prefix):
            names.add(name)
    return names


def case_name(test):
    """Matrix cells name their dedicated assertion after the dashed case id."""
    return test.removesuffix('_cwd').replace('_', '-')


def panic_blocks(stderr, prefix, test, worker):
    """A parent test's own panic blocks; the embedded worker's stays inside.

    With ``--nocapture`` panics go to stderr and a parent's message embeds
    its bounded worker's whole captured output, so the worker's own panic
    line must not end the parent's block. A block runs from the test's
    ``thread '<name>' panicked at`` line up to the next panic of any other
    thread, or the end of stderr.
    """
    name = re.escape(prefix + test)
    embedded = re.escape(prefix + worker)
    pattern = (rf"thread '{name}'(?: \(\d+\))? panicked at.*?"
               rf"(?=\nthread '(?!{embedded}')[^']+'(?: \(\d+\))? panicked at|\Z)")
    return re.findall(pattern, stderr, re.S)


# Lines that end a panic payload rather than belong to it: the backtrace
# note and libtest's own result/report output. Shared shape with the #813
# ``negative_panic`` rule that only the message body is evidence.
_PANIC_TAIL = re.compile(
    r'^(?:'
    r'stack backtrace:'
    r'|note: Some details are omitted'
    r'|note: run with `RUST_BACKTRACE'
    r'|test result:'
    r'|failures:'
    r'|test \S+ \.\.\. (?:ok|FAILED|ignored|measured)'
    r'|error: test failed'
    r'|running \d+ tests?$'
    r')'
)

# std's assert_eq/assert_ne print the custom message on the payload's first
# line, after the fixed diagnostic; every other dedicated message (assert!,
# expect) starts a payload line of its own.
_ASSERTION_PREFIXES = ('assertion `left == right` failed: ',
                       'assertion `left != right` failed: ')


def worker_payloads(block, prefix, worker):
    """The embedded worker's own stderr panic payloads inside a parent block.

    A parent's message embeds the worker's whole captured output, and
    run_case splices it as stdout then stderr (test_process.rs:137), so the
    worker's stdout - where a whole old panic diagnostic can be echoed
    back - ends at the worker's own libtest summary line. Only panics after
    the block's last ``test result:`` line are the worker's real stderr; a
    block without one embeds no completed worker run and yields no payload
    (#889 fix-r3). A marker in worker stdout or a plain diagnostic is not
    evidence (#889 fix-r2): only the body of the worker's own panic is.
    The ``panicked at <loc>:`` header line and everything after the
    backtrace note are dropped, mirroring the shared #813 ``negative_panic``
    rule.
    """
    lines = block.splitlines()
    summaries = [i for i, line in enumerate(lines) if line.startswith('test result:')]
    if not summaries:
        return []
    payloads = []
    for segment in '\n'.join(lines[summaries[-1] + 1:]).split("\nthread '")[1:]:
        if segment.split("'", 1)[0] != prefix + worker:
            continue
        at = segment.find(' panicked at ')
        if at < 0:
            continue
        body = segment[at + len(' panicked at '):]
        newline = body.find('\n')
        if newline < 0:
            continue
        kept = []
        for line in body[newline + 1:].splitlines():
            if _PANIC_TAIL.match(line):
                break
            kept.append(line)
        payloads.append('\n'.join(kept))
    return payloads


def _scoped_to_assertion(payload, message):
    """Whether the payload carries ``message`` as an assertion's own text.

    The message must start a payload line - bare for ``assert!``/``expect()``,
    or after std's ``assertion `left == right` failed: `` diagnostic - so an
    echo inside the indented ``left:``/``right:`` value lines never counts.
    """
    return any(line.startswith(message)
               or any(line.startswith(f'{prefix}{message}')
                      for prefix in _ASSERTION_PREFIXES)
               for line in payload.splitlines())


def target_hits(result, prefix, targets, worker, *, every_block=True, refused=None):
    """Split declared ``(test, dedicated message)`` targets into hits/unrelated.

    A target counts as hit only when the outer ``failures:`` list names it
    and every one of its own panic blocks carries its dedicated message in
    every one of the embedded worker's own stderr panic payloads; a
    declared failure whose worker died elsewhere - a plain diagnostic
    echoing the marker, a setup ``unwrap()``, an old panic echoed on worker
    stdout, or the marker echoed in assertion values - is unrelated, never
    a kill (#889 fix-r2, fix-r3). A block whose payloads disagree - one
    carrying the message, another not - is conflicting evidence: the
    target is refused (kept in ``unrelated`` and appended to ``refused``
    when the caller supplies a list), never judged by any single payload
    (#889 fix-r3). A matrix run has exactly one block per test, so the
    every-block requirement stays fail-closed when a foreign panic echoes
    this test's panic-start line (#889).

    ``every_block=False`` is for single-test ``--exact`` runners: their
    parent legitimately re-panics after ``catch_unwind`` without the
    worker's captured output (metadata-process-cwd's ``#879 child matrix
    failed`` wrapper), and with no other test in the process no foreign
    echo exists - so one own block with the worker's payload decides.
    """
    if not targets:
        raise ValueError('at least one declared target is required')
    failed = failed_tests(result.stdout, prefix)
    hit, unrelated = [], []
    for test, message in targets:
        if f'{prefix}{test}' not in failed:
            continue
        blocks = panic_blocks(result.stderr, prefix, test, worker)
        carries, conflicts = [], []
        for block in blocks:
            carrying = [_scoped_to_assertion(payload, message)
                        for payload in worker_payloads(block, prefix, worker)]
            carries.append(bool(carrying) and all(carrying))
            conflicts.append(any(carrying) and not all(carrying))
        if any(conflicts):
            unrelated.append(test)
            if refused is not None:
                refused.append(test)
        elif blocks and (all(carries) if every_block else any(carries)):
            hit.append(test)
        else:
            unrelated.append(test)
    return hit, unrelated


def cell_hits(result, prefix, tests, marker, worker, *, refused=None):
    """Matrix cells compose their dedicated message from marker and case id."""
    return target_hits(result, prefix,
                       [(test, f'{marker} {case_name(test)}') for test in tests],
                       worker, refused=refused)


def judge(result, prefix, tests, marker, worker, *, any_cell=False):
    """Classify a matrix negative run: ``(killed, reason, hits)``.

    ``any_cell`` accepts a partial kill: one declared cell red with its
    dedicated message is a kill even while a sibling declared cell is
    structurally unreachable for that mutation (the #879b stats
    ``absolute-guard`` run relies on this). A declared cell with
    conflicting worker payloads vetoes the whole run instead (#889 fix-r3):
    no single payload ever decides.
    """
    log = result.stdout + result.stderr
    if result.returncode == 0:
        return False, 'assertion did not fail', []
    if 'error[' in log or 'could not compile' in log:
        return False, 'compile failure', []
    if result.returncode != 101 or 'panicked at' not in log:
        return False, 'missing named runtime failure (build error, wrong panic, or missing test)', []
    refused = []
    hit, unrelated = cell_hits(result, prefix, tests, marker, worker, refused=refused)
    if refused:
        return False, f'conflicting worker payloads refused: {refused}', hit
    if unrelated:
        return False, f'declared cells failed without the dedicated marker: {unrelated}', hit
    missed = [test for test in tests if test not in hit]
    if any_cell:
        if hit:
            return True, 'a declared cell failed at its dedicated assertion', hit
        return False, f'no declared cell was red with the marker: {missed}', hit
    if len(hit) == len(tests):
        return True, 'declared cells failed at their dedicated assertions', hit
    return False, f'declared cells stayed green: {missed}', hit


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


def run_cases(root, cases, out, env, mutate, *, timeout=900, baseline='HEAD'):
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
        results.append({'mutation': case.mutation, 'test': case.test,
                        'assertion': case.assertion, 'status': status, 'reason': reason})
        counts = {state: sum(r['status'] == state for r in results)
                  for state in ('KILLED', 'SURVIVED', 'INVALID')}
        (out / 'summary.json').write_text(json.dumps(
            {'controls': results, 'counts': counts}, indent=2) + '\n')
        print(f'{case.mutation}: {status}: {reason}', flush=True)

    try:
        with mutating_sources(root, list(dict.fromkeys(c.path for c in cases)),
                              baseline=baseline) as originals:
            for case in cases:
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
                    case.path.write_bytes(originals[case.path])
                    if case.path.read_bytes() != originals[case.path]:
                        raise RuntimeError(f'byte restoration failed: {case.path}')
                try:
                    if not passed(run(case.mutation + '-restored', case.test), case.test):
                        status, reason = 'INVALID', reason + '; restored bound test did not pass'
                except Exception as error:
                    status, reason = 'INVALID', reason + f'; restored: {type(error).__name__}: {error}'
                record(case, status, reason)
    except Exception as error:
        # A baseline/lock/restoration failure makes further mutation unsafe.
        # Still account for every selected case and never leave an all-green summary.
        reason = f'source guard: {type(error).__name__}: {error}'
        for row in results:
            row.update(status='INVALID', reason=row['reason'] + '; ' + reason)
        for case in cases[len(results):]:
            record(case, 'INVALID', reason)
        if len(results) == len(cases):
            (out / 'summary.json').write_text(json.dumps({
                'controls': results,
                'counts': {'KILLED': 0, 'SURVIVED': 0, 'INVALID': len(results)},
            }, indent=2) + '\n')
    counts = {state: sum(r['status'] == state for r in results)
              for state in ('KILLED', 'SURVIVED', 'INVALID')}
    print('summary: ' + ', '.join(f'{state}={count}' for state, count in counts.items()), flush=True)
    return int(not results or any(row['status'] != 'KILLED' for row in results))
