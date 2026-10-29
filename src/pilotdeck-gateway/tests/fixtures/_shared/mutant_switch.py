# anchor-selfcheck: unsupported shared precompiled-switch executor, no mutation definitions
"""#996: compile once, then execute every selected switch in its own process.

Shared by the fixture runners whose negative controls are compile-time
``crate::mutants::on`` switches instead of working-tree text replacement
(host-factory, session-domain, edgeclaw-service-facade). The selector
(``PDRS_MUTANT``) is an output of this executor and is never inherited from
the caller. Sources are never written: mutation happens inside the compiled
test binary, so the tree lock only excludes other negative runners.
"""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import time
from types import SimpleNamespace

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import run_owned, tree_lock, wait_for_slot
from negative_suite import classify, invalidate_completed, passed

SELECTOR = 'PDRS_MUTANT'
STATES = ('KILLED', 'SURVIVED', 'INVALID')


def clean_environment(env):
    """No stale selector or receipt protocol may reach a fresh child."""
    result = dict(env)
    result.pop(SELECTOR, None)
    result.pop('PDRS_FACADE_ENCODER_MUTANT', None)
    result.pop('PDRS_NEGATIVE_RECEIPTS', None)
    result.pop('PDRS_NEGATIVE_PARENT_TEST', None)
    return result


def selector_environment(env, name):
    """A clean environment with one switch selected, or none for a control."""
    result = clean_environment(env)
    if name is not None:
        result[SELECTOR] = name
    return result


def raw_timeout(error):
    """Decode the partial output a killed child still wrote."""
    return ''.join(part.decode(errors='replace') if isinstance(part, bytes) else part or ''
                   for part in (error.stdout, error.stderr)) + f'\nTIMEOUT: {error}\n'


def counts(rows):
    return {state: sum(row['status'] == state for row in rows) for state in STATES}


def publish_summary(output, rows, started, build_seconds):
    """Publish counts and timing atomically after every accounted row."""
    document = {'controls': rows, 'counts': counts(rows), 'timing': {
        'build_seconds': build_seconds, 'total_seconds': time.monotonic() - started}}
    pending = output / 'summary.json.tmp'
    pending.write_text(json.dumps(document, indent=2) + '\n')
    pending.replace(output / 'summary.json')


def artifact(result):
    """The one feature-enabled pilotdeck_gateway library test executable."""
    if result.returncode:
        raise RuntimeError('precompile failed; see build.log')
    executables = set()
    for line in result.stdout.splitlines():
        try:
            item = json.loads(line)
        except json.JSONDecodeError:
            continue
        target = item.get('target', {})
        if (item.get('reason') == 'compiler-artifact'
                and target.get('name') == 'pilotdeck_gateway'
                and target.get('kind') == ['lib']
                and item.get('profile', {}).get('test') is True
                and item.get('executable')
                and 'pdrs-mutants' in item.get('features', [])):
            executables.add(item['executable'])
    if len(executables) != 1:
        raise RuntimeError(f'expected one feature-enabled library test executable, got {len(executables)}')
    binary = Path(executables.pop()).resolve()
    target_dir = Path(os.environ['CARGO_TARGET_DIR']).resolve()
    if not binary.is_relative_to(target_dir) or not binary.is_file():
        raise RuntimeError('test executable is absent or outside caller CARGO_TARGET_DIR')
    return binary


def precompile(root, env, output, *, build_timeout=900):
    """Build the feature-enabled library test binary once and record it."""
    gate = wait_for_slot(root / 'scripts/cargo-slots.sh', timeout=900)
    (output / 'build.slots.log').write_text(gate.stdout + gate.stderr)
    command = ['cargo', 'test', '--offline', '--locked',
               '--manifest-path', str(root / 'src/Cargo.toml'),
               '-p', 'pilotdeck-gateway', '--lib', '--features', 'pdrs-mutants',
               '--no-run', '--message-format=json']
    begin = time.monotonic()
    build_seconds = 0.0
    try:
        result = run_owned(command, cwd=root, env=env, timeout=build_timeout)
        (output / 'build.log').write_text(result.stdout + result.stderr)
        binary = artifact(result)
    except subprocess.TimeoutExpired as error:
        (output / 'build.log').write_text(raw_timeout(error))
        raise
    finally:
        build_seconds = time.monotonic() - begin
    (output / 'binary.json').write_text(json.dumps({
        'path': str(binary), 'sha256': hashlib.sha256(binary.read_bytes()).hexdigest(),
        'command': command,
    }, indent=2) + '\n')
    return binary, build_seconds


def switch_site(name):
    """The compiled call every switch row must have exactly one of."""
    return f'::mutants::on("{name}")'


def as_tests(value):
    """A bound test is one name or a nonempty list; lists are all-cells rows."""
    if isinstance(value, str) and value:
        return [value]
    if (isinstance(value, list) and value
            and all(isinstance(item, str) and item for item in value)):
        return list(value)
    raise ValueError(f'invalid test binding: {value!r}')


def preflight(rows):
    """Every switch must exist exactly once in its bound source file."""
    def bound(value):
        if isinstance(value, str):
            return bool(value)
        return (isinstance(value, list) and value
                and all(isinstance(item, str) and item for item in value))

    errors = []
    seen = set()
    for row in rows:
        name = row['name']
        if name in seen:
            errors.append(f'{name}: duplicate switch identity')
        seen.add(name)
        if row['path'].read_text().count(switch_site(name)) != 1:
            errors.append(f'{name}: expected exactly one compiled switch site')
        for key in ('test', 'anchor'):
            if not bound(row.get(key)):
                errors.append(f'{name}: missing {key} binding')
        # All-cells rows bind one assertion per test, so no cell is judged by
        # another cell's anchor.
        tests, anchors = row.get('test'), row.get('anchor')
        if isinstance(tests, list) and not (isinstance(anchors, list)
                                            and len(anchors) == len(tests)):
            errors.append(f'{name}: list tests require an aligned assertion list')
    if errors:
        raise ValueError('\n'.join(errors))


def selected(rows, requested):
    """Selection must be known, unique and nonempty."""
    known = {row['name'] for row in rows}
    if len(requested) != len(set(requested)) or set(requested) - known:
        raise ValueError('unknown or duplicate switch name')
    return [row for row in rows if not requested or row['name'] in requested]


def execute_suite(selected_rows, invoke, verdict=None, prefix=''):
    """One bad row must not hide later rows; invoke owns bounded children.

    A row's test binding is one name or a nonempty list. List rows are
    all-cells bindings: every test must pass in the positive and restored
    phases and fail at its own assertion under mutation.
    """
    if verdict is None:
        def verdict(result, row):
            return classify(result, SimpleNamespace(test=row['test'], assertion=row['anchor']))

    def phase(name, phase_name, tests, selector, row):
        """Invoke each bound test once; return the names that did not pass."""
        failed = []
        for index, test in enumerate(tests):
            label = (f'{name}-{phase_name}' if len(tests) == 1
                     else f'{name}-{phase_name}-{index + 1}of{len(tests)}')
            if not passed(invoke(label, test, selector, row), prefix + test):
                failed.append(test)
        return failed

    for row in selected_rows:
        status, reason = 'INVALID', ''
        try:
            name, witness = row['name'], row.get('witness')
            tests = as_tests(row['test'])
            witnesses = as_tests(witness) if witness else []
            if phase(name, 'positive', tests, None, row):
                reason = 'positive control did not pass the bound test'
            elif witness and phase(name, 'witness-positive', witnesses, None, row):
                reason = 'witness test did not pass before mutation'
            else:
                witness_failed = bool(witness
                                      and phase(name, 'witness-negative', witnesses, name, row))
                anchors = row.get('anchor')
                aligned = (len(tests) > 1 and isinstance(anchors, list)
                           and len(anchors) == len(tests))
                verdicts = [
                    verdict(
                        invoke(
                            f'{name}-negative' if len(tests) == 1
                            else f'{name}-negative-{index + 1}of{len(tests)}',
                            test, name, row),
                        dict(row, test=prefix + test,
                             anchor=anchors[index] if aligned else anchors))
                    for index, test in enumerate(tests)]
                unaccounted = [(test, state, why)
                               for test, (state, why) in zip(tests, verdicts)
                               if state != 'KILLED']
                if not unaccounted:
                    status, reason = 'KILLED', 'bound test failed at its assertion'
                else:
                    status = unaccounted[0][1]
                    reason = '; '.join(f'{test}: {why}'
                                      for test, _state, why in unaccounted)
                if witness_failed:
                    status, reason = 'INVALID', reason + '; witness test failed under mutation'
        except Exception as error:
            status, reason = 'INVALID', f'{type(error).__name__}: {error}'
        # Fresh process, no selector: also run after exceptions and failed positives.
        try:
            if phase(row['name'], 'restored', as_tests(row['test']), None, row):
                status, reason = 'INVALID', reason + '; restored bound test did not pass'
        except Exception as error:
            status, reason = 'INVALID', reason + f'; restored: {type(error).__name__}: {error}'
        yield {key: row[key] for key in ('name', 'test', 'anchor')} | {
            'status': status, 'reason': reason}


def denominator_drift(root, script, rows, requested=(), declared=None):
    """#996 fix-r4: reconcile the executed switch names with the declaration.

    The static whitelist and the import-time audit both stop at module scope,
    so execution-entry code (``main()``) can still shrink or grow the table the
    executor actually consumes. The only unforgeable denominator is therefore
    compared here, after the run, using the checker's own extraction
    (anchor_selfcheck.static_switch_names): an explicit selection is compared
    with the requested names, and no requested name may exceed the declared
    table (selected() already bounds the runtime side). Returns a drift
    description, or None when the executed set matches the denominator.

    #996 fix-r5: entries whose declaration is not a switches table (the
    edgeclaw-service-facade legacy mutation tables) pass their statically
    extracted ``declared`` set (anchor_selfcheck.static_mutant_names) instead;
    a runtime table the executed rows could already have shrunk is never a
    denominator, and ``script`` is then only the documented declaring source.
    """
    import anchor_selfcheck  # fixtures root; already on sys.path here.

    executed = {row['name'] for row in rows}
    if declared is None:
        declared = anchor_selfcheck.static_switch_names(script, root)
    expected = set(requested) or declared
    missing = sorted(expected - executed)
    extra = sorted((executed | set(requested)) - declared)
    if missing or extra:
        return (f'switch denominator drift: the executed table has {len(executed)} '
                f'switch names, the declared table has {len(declared)}; '
                f'missing={missing}, extra={extra}')
    return None


def enforce_denominator(root, script, rows, requested=(), declared=None):
    """#996 fix-r5: drift invalidates the whole suite, in every runner shape.

    Both the shared switches-table run() and the facade's own execution
    entries finish with this check, so no runner can publish a shrunken or
    forged success regardless of which table shape it consumes.

    #996 fix-r6: a table emptied whole at runtime leaves no executed row to
    invalidate, and ``any()`` over an empty row list reads False, so the drift
    used to reach neither the summary nor (for the callers without a
    ``not rows`` guard) the exit code. The suite-level row keeps the INVALID
    reason in the published summary and fails every caller's final ``any()``,
    so no entry re-derives the empty-table check on its own.
    """
    drift = denominator_drift(root, script, rows, requested, declared)
    if drift:
        # The audited table and the executed table disagree: no row
        # of this run counts as a valid control outcome.
        for row in rows:
            row.update(status='INVALID',
                       reason=f'{row["reason"]}; {drift}' if row['reason'] else drift)
        if not rows:
            rows.append({'name': 'switch-denominator', 'test': '-', 'anchor': '-',
                         'status': 'INVALID', 'reason': drift})
        print(drift, file=sys.stderr)
    return drift


def record_sources(root, output, guarded):
    """Hash guarded inputs; working-tree hashes, not HEAD equivalence."""
    hashes = {str(path.relative_to(root)): hashlib.sha256(path.read_bytes()).hexdigest()
              for path in sorted(guarded)}
    head = run_owned(['git', 'rev-parse', 'HEAD'], cwd=root, timeout=30)
    if head.returncode:
        raise RuntimeError('cannot record source HEAD')
    (output / 'source.json').write_text(json.dumps({
        'head': head.stdout.strip(), 'sha256': hashes,
        'note': 'working-tree source hashes; not a claim of committed HEAD equivalence',
    }, indent=2) + '\n')
    return hashes


def verify_unchanged(root, hashes):
    """The precompiled run never edits sources; prove it before exiting."""
    if any(hashlib.sha256((root / name).read_bytes()).hexdigest() != digest
           for name, digest in hashes.items()):
        raise RuntimeError('sources changed during precompiled run')


def run(root, output, selected_rows, env, *, guarded, script=None, requested=(),
        prefix='', test_timeout=240, build_timeout=900, verdict=None):
    """Execute every selected switch; return the process exit code.

    With ``script`` (the declaring execution entry) the run finishes with the
    #996 fix-r4 denominator check (through the shared enforce_denominator):
    the executed switch names must match the statically declared ones (or the
    explicitly requested selection), or the whole suite is reported INVALID
    with the difference and exits nonzero.
    """
    started = time.monotonic()
    build_seconds = 0.0
    rows = []
    try:
        with tree_lock(root, timeout=900):
            hashes = record_sources(root, output, guarded)
            binary, build_seconds = precompile(root, env, output, build_timeout=build_timeout)

            def invoke(label, test, selector, row):
                # Exact binding still runs one test, including explicit TS guards.
                command = [str(binary), prefix + test, '--exact', '--nocapture', '--include-ignored']
                begin = time.monotonic()
                try:
                    result = run_owned(command, cwd=root / 'src/pilotdeck-gateway',
                                       env=selector_environment(env, selector),
                                       timeout=test_timeout)
                except subprocess.TimeoutExpired as error:
                    (output / (label + '.log')).write_text(raw_timeout(error))
                    raise
                (output / (label + '.log')).write_text(result.stdout + result.stderr)
                (output / (label + '.json')).write_text(json.dumps({
                    'command': command, 'selector': selector,
                    'returncode': result.returncode, 'seconds': time.monotonic() - begin,
                }, indent=2) + '\n')
                return result

            for row in execute_suite(selected_rows, invoke, verdict, prefix):
                rows.append(row)
                publish_summary(output, rows, started, build_seconds)
                print(f"{row['name']}: {row['status']}: {row['reason']}", flush=True)
            verify_unchanged(root, hashes)
            if script is not None:
                enforce_denominator(root, script, rows, requested)
    except Exception as error:
        reason = f'runner: {type(error).__name__}: {error}'
        # #1037: a guard or verification failure (record_sources, the exit
        # verify_unchanged, the denominator check) must revoke every already
        # completed row through the one shared implementation (#979), never a
        # second copy; otherwise an all-KILLED summary publishes a false pass.
        invalidate_completed(rows, reason)
        for row in selected_rows[len(rows):]:
            rows.append({key: row[key] for key in ('name', 'test', 'anchor')} | {
                'status': 'INVALID', 'reason': reason})
        print(reason, file=sys.stderr)
    publish_summary(output, rows, started, build_seconds)
    print('summary: ' + ', '.join(f'{state}={count}' for state, count in counts(rows).items()),
          flush=True)
    return int(not rows or any(row['status'] != 'KILLED' for row in rows))
