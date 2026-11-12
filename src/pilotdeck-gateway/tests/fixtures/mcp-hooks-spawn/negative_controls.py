#!/usr/bin/env python3
"""After coordinator commit: bounded #808 mutations, no private build directory.

#1087: the kill judge is the shared ``negative_panic`` criterion — the bound
test's own ``FAILED`` status line plus the anchor inside that test's own panic
payload. The run filter is a bare suffix (no ``--exact``), so the full
module-qualified name is resolved from the run's own status lines and must be
unique; an ambiguous or absent match, or a marker printed to stdout or another
test's panic, is never a kill (fail-closed INVALID, the cargo-side form of
#818). Every mutant runs to the end: rows are recorded KILLED / SURVIVED /
INVALID with a reason, the summary.json is published once after the last
restore verification, and the shared ``negative_selection.verdict_exit``
contract keeps exit 0 only for a nonempty all-KILLED run. #1087 fix-r1: that
publish is one atomic rename (``write_summary``), never a truncating
``write_text`` — the runner, its twin controls in ``test_negative_controls.py``
and the mutation table stay at module level.
"""
import argparse, json, os, re, sys
from pathlib import Path
root = Path(__file__).resolve().parents[5]
sys.path.insert(0, str(root / 'src/pilotdeck-gateway/tests/fixtures'))
from negative_panic import named_test_killed
from negative_process import (mutating_sources, run_owned, wait_for_slot,
                             write_source_atomic)
from negative_selection import verdict_exit
from negative_suite import passed

client = 'src/pilotdeck-gateway/src/runtime/mcp/client.rs'
hooks = 'src/pilotdeck-gateway/src/runtime/async_hooks.rs'
mutants = [
 ('start-clear', client, r'(impl ConnectCompletion.*?)(state.connect = None;)', r'\1let _ = &state;', 'start_spawn_failure_matrix', 'failure clears cached attempt'),
 ('start-drop', client, r'(impl Drop for ConnectCompletion.*?if )self.sender.is_some\(\)', r'\1false', 'start_spawn_drop_matrix', 'guard publishes an outcome'),
 ('start-identity', client, r'if state\s*\.connect\s*\.as_ref\(\)\s*\.is_some_and\(\|current\| current.same_channel\(&self.mine\)\)', 'if true', 'start_failure_keeps_replacement', 'old failure preserves replacement'),
 ('delivery-clear', hooks, r'\*self\s*\.pending\s*\.delivery\s*\.lock\(\)\s*\.unwrap_or_else\(std::sync::PoisonError::into_inner\) = None;', 'let _ = &self.pending;', 'delivery_spawn_failure_matrix', 'failure clears cached attempt'),
 ('delivery-drop', hooks, r'(impl Drop for DeliveryCompletion.*?if )self.sender.is_some\(\)', r'\1false', 'delivery_spawn_drop_matrix', 'guard publishes an outcome'),
 ('delivery-prepoll', hooks, r'(    let mut completion = DeliveryCompletion \{.*?\n    \};)\n    tokio::spawn\(async move \{', r'    tokio::spawn(async move {\n\1', 'delivery_spawn_drop_matrix', 'guard publishes an outcome'),
 ('delivery-poison', hooks, r'\*self\s*\.pending\s*\.delivery\s*\.lock\(\)\s*\.unwrap_or_else\(std::sync::PoisonError::into_inner\) = None;', 'if let Ok(mut slot) = self.pending.delivery.lock() { *slot = None; }', 'delivery_poison_failure', 'poisoned delivery cleared'),
 ('submit-clear', hooks, r'\*self\s*\.pending\s*\.delivery\s*\.lock\(\)\s*\.unwrap_or_else\(std::sync::PoisonError::into_inner\) = None;', 'let _ = &self.pending;', 'submit_sink_panic_clears_delivery', 'submit panic clears delivery'),
]


def save(out, name, value):
    with (out / name).open('x') as f: f.write(value)


def run(out, name, test):
    slots = wait_for_slot(root / 'scripts/cargo-slots.sh')
    save(out, name + '.slots.log', slots.stdout + slots.stderr)
    return run_owned(['cargo', 'test', '--manifest-path', 'src/Cargo.toml', '-p', 'pilotdeck-gateway', '--lib', test], cwd=root, env=os.environ.copy(), timeout=600)


def full_names(log, test):
    """Full libtest names whose last segment is the bare filter ``test``.

    The run filters by substring (no ``--exact``), so the status lines carry
    the module-qualified names; the suffix resolves to exactly one of them or
    the row is fail-closed INVALID (#1087).
    """
    names = set()
    for line in log.split('\n'):
        match = re.match(r'^test (\S+) \.\.\. (?:ok|FAILED|ignored)$', line)
        if match and (match.group(1) == test
                      or match.group(1).endswith('::' + test)):
            names.add(match.group(1))
    return sorted(names)


def classify(result, test, anchor):
    """KILLED only on the bound test's own FAILED line plus its panic payload."""
    log = result.stdout + result.stderr
    names = full_names(log, test)
    if len(names) != 1:
        return 'INVALID', (f'cannot resolve the bound test name: '
                            f'{len(names)} matches for {test}')
    if passed(result, names[0]):
        return 'SURVIVED', 'bound test still passed'
    # #1087 fix-r5 (simplify #2): the FAILED summary requirement moved
    # into the shared named_test_killed default (require_summary=True).
    if (result.returncode != 0
            and named_test_killed(log, names[0], anchor)):
        return 'KILLED', 'bound test failed at its assertion'
    return 'INVALID', 'missing bound runtime failure (build error, wrong panic, or missing anchor)'


def write_summary(out, results):
    """Publish the one final summary atomically (#1087 fix-r1 / #1903 rule).

    The publish already happens only after the last restore verification; the
    remaining #1087 fix-r1 half is the write channel itself — a truncating
    ``write_text`` killed mid-write leaves an empty or half-written
    summary.json, so the payload goes to a sibling temp file and is renamed
    into place in one atomic step.
    """
    tmp = out / 'summary.json.tmp'
    tmp.write_text(json.dumps(results, indent=2) + '\n')
    # ``os.rename`` (not ``Path.replace``/``os.replace``): this runner also
    # regex-mutates sources, and the anchor selfcheck reads ANY ``.replace``
    # attribute call as a literal string consumer mixed with the regex one,
    # refusing the whole script. ``os.rename`` is the same single atomic
    # rename on POSIX (it silently replaces an existing target), and this
    # runner family is POSIX-only — ``negative_process`` needs ``flock``.
    os.rename(tmp, out / 'summary.json')


def main(argv=None):
    argv = list(sys.argv if argv is None else argv)
    if not os.environ.get('CARGO_TARGET_DIR'):
        sys.exit('CARGO_TARGET_DIR must be supplied by the caller')
    p = argparse.ArgumentParser()
    p.add_argument('output')
    p.add_argument('--only', action='append')
    p.add_argument('--check-patterns', action='store_true')
    a = p.parse_args(argv[1:])
    out = Path(a.output).resolve()
    out.mkdir(parents=True, exist_ok=False)
    baseline = os.environ.get('NEGATIVE_BASELINE') or 'HEAD'
    selected = mutants
    if a.only:
        unknown = set(a.only) - {m[0] for m in mutants}
        if unknown:
            sys.exit(f'unknown mutants: {sorted(unknown)}')
        selected = [m for m in mutants if m[0] in a.only]
    results = []
    for name, relative, pattern, replacement, test, anchor in selected:
        path = root / relative
        if a.check_patterns:
            _, n = re.subn(pattern, replacement, path.read_text(), flags=re.S)
            if n != 1:
                sys.exit(f'stale pattern {name}: {n} matches in {relative}')
            print(f'{name}: unique source pattern; test={test}; anchor={anchor}', flush=True)
            continue
        status, reason = 'INVALID', 'not executed'
        try:
            with mutating_sources(root, [path], baseline=baseline) as original:
                changed, n = re.subn(pattern, replacement, original[path].decode(), flags=re.S)
                if n != 1:
                    status, reason = 'INVALID', f'stale pattern: {n} matches in {relative}'
                else:
                    # #1087 fix-r2: every guarded-source write in the window
                    # is one atomic rename (#1903 rule, shared with
                    # ``mutating_sources``) — a truncating ``write_text``
                    # killed mid-write leaves an empty or half-written
                    # product source.
                    write_source_atomic(
                        root, path, changed.encode(),
                        mode=path.stat().st_mode & 0o7777)
                    result = run(out, name, test)
                    save(out, name + '.log', result.stdout + result.stderr)
                    status, reason = classify(result, test, anchor)
            restored = run(out, name + '-restored', test)
            save(out, name + '-restored.log', restored.stdout + restored.stderr)
            restored_names = full_names(restored.stdout + restored.stderr, test)
            if (restored.returncode != 0 or len(restored_names) != 1
                    or not passed(restored, restored_names[0])):
                status, reason = 'INVALID', reason + '; restored run not green'
        except Exception as error:
            status, reason = 'INVALID', f'{type(error).__name__}: {error}'
        print(f'{name}: {status}: {reason}', flush=True)
        results.append({'mutation': name, 'test': test, 'assertion': anchor,
                        'status': status, 'reason': reason})
    if a.check_patterns:
        return 0
    counts = {state: sum(row['status'] == state for row in results)
              for state in ('KILLED', 'SURVIVED', 'INVALID')}
    print(json.dumps(counts), flush=True)
    write_summary(out, results)
    return verdict_exit(results, key='status')


if __name__ == '__main__':
    raise SystemExit(main())
