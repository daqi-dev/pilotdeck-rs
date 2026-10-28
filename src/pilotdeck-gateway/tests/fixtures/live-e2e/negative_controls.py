#!/usr/bin/env python3
"""Temporary apparatus mutations; require a selftest assertion failure, then restore.

Run with no other writers in this worktree. Evidence goes in a new output
folder; original source bytes and hashes are verified after every mutation.

Every row turns off one observable the selftest asserts on (an injection, an
expectation value, the HOME isolation, a middleman recording/reaping
property, the type-strict comparator, the verified:false report contract)
and names the dedicated ``#969`` message that must go red, so a surviving
mutant can never pass for evidence. A failure without the bound message
(crash, wrong assertion) is INVALID, never a kill — and so is any outcome
that relies on the driver's 60s watchdog instead of a dedicated assertion
(the cancel row re-proves this: the mock bounds its hang, the turn ends
without the abort, and the driver's own cancel assertion names the failure).

The baseline row is the positive control: text-passthrough and
cancel-passthrough must pass with no mutation applied; the cancel mutant
additionally re-proves its own cell green before the mutation is written,
because a selected single mutant skips the baseline row.

Three rows run attack_suite.py instead of a selftest case: the comparator
mutations are unit-level (forged driver results fed to verify_caught), so
their negative control is the attack suite reporting the matching ESCAPED
line. Every row that ran the selftest also has its report.json checked:
each case cell must carry an explicit verified field (README contract).

Source mutations require the committed HEAD. No alternate baseline/dirty-source
escape is accepted. The explicit no-abort runtime hook can run before commit.
"""
import difflib
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot
from cancel_evidence import abort_evidence_ok as validate_abort

root = Path(__file__).resolve().parents[5]
source = Path(__file__).resolve().parent
out = Path(sys.argv[1]).resolve()
out.mkdir(parents=True, exist_ok=False)

# The case(s) each row runs, and the exact assertion message that has to
# appear in the run log; anything else (timeout, crash, unrelated error) does
# not count as a kill.
TESTS = {
    'baseline': (['text-passthrough', 'cancel-passthrough'], None),
    'mitm-drop-disabled': (['text-drop'], 'complete evidence.frames'),
    'mitm-duplicate-disabled': (['text-duplicate'], 'complete evidence'),
    'mitm-early-final-disabled': (['text-early-final'], 'complete evidence.frames'),
    'mitm-double-delivery': (['text-passthrough'], 'complete evidence'),
    'driver-tool-payload-forged': (['tool-passthrough'], 'complete evidence'),
    'driver-abort-never-fires': (['cancel-passthrough'],
        'cancel window must fire the client-side abort after the first model event'),
    'driver-home-leaked': (['text-passthrough'], 'driver HOME must be the case sandbox home'),
    'mitm-record-stops-on-close': (['text-passthrough'], 'MITM must keep recording after the host closes the output pipe'),
    'mitm-delivered-lying': (['text-passthrough'], 'frames after the host closed the pipe must be recorded as not delivered'),
    'mitm-eof-reap-disabled': (['text-passthrough'], 'MITM must exit after host stdin EOF even when the child ignores SIGTERM'),
    'mitm-group-reap-blind': (['text-passthrough'], 'MITM must kill the TERM-ignoring descendant that outlives the leader'),
    'comparator-type-blind': ([], 'ESCAPED bool-for-int-count'),
    'comparator-keys-blind': ([], 'ESCAPED extra-terminal-key'),
    'comparator-length-blind': ([], 'ESCAPED events-truncated-together'),
    'report-failure-verified-dropped': ([], 'ESCAPED failing-case-carries-verified-false'),
}
ATTACK_ROWS = ('comparator-type-blind', 'comparator-keys-blind',
               'comparator-length-blind', 'report-failure-verified-dropped')

mutations = [
    ('baseline', None, None, None),
    ('mitm-drop-disabled', 'stdio_mitm.py',
     'forward_count = 0 if "drop" in actions else 2 if "duplicate" in actions else 1',
     'forward_count = 1 if "drop" in actions else 2 if "duplicate" in actions else 1'),
    ('mitm-duplicate-disabled', 'stdio_mitm.py',
     '2 if "duplicate" in actions else 1', '1 if "duplicate" in actions else 1'),
    ('mitm-early-final-disabled', 'stdio_mitm.py',
     'if "early_final" in actions:', 'if False and "early_final" in actions:'),
    ('mitm-double-delivery', 'stdio_mitm.py',
     'forward_count = 0 if "drop" in actions else 2 if "duplicate" in actions else 1',
     'forward_count = 2 if direction == "s2h" and frame.get("kind") == "event" else 1'),
    ('driver-tool-payload-forged', 'driver.mts',
     "const text = readFileSync(path, 'utf8');", "const text = 'forged';"),
    # An explicit runtime mutation hook exercises the real TS path without
    # rewriting a dirty worktree. It is forbidden in the golden generator.
    ('driver-abort-never-fires', None, None, None),
    ('driver-home-leaked', 'selftest.py',
     '        "HOME": str(home),', '        "HOME": os.environ.get("HOME", str(home)),'),
    ('mitm-record-stops-on-close', 'stdio_mitm.py',
     '            written_to_host = deliver_to_host(encoded, forward_count) if forward_count else 0',
     '            written_to_host = deliver_to_host(encoded, forward_count) if forward_count else 0\n'
     '            if not mitm.forward["s2h"]:\n                break'),
    ('mitm-delivered-lying', 'stdio_mitm.py',
     '            mitm.settle(record, written_to_host > 0)',
     '            mitm.settle(record, forward_count > 0)'),
    ('mitm-eof-reap-disabled', 'stdio_mitm.py',
     '        reap_after_host_eof(child)', '        pass  # eof reap disabled'),
    ('mitm-group-reap-blind', 'stdio_mitm.py',
     '        if process.poll() is not None and not group_alive(process.pid):',
     '        if process.poll() is not None:'),
    ('comparator-type-blind', 'selftest.py',
     '    if type(actual) is not type(expected):', '    if False and type(actual) is not type(expected):'),
    ('comparator-keys-blind', 'selftest.py',
     '        for key in sorted(set(expected) | set(actual)):',
     '        for key in sorted(set(expected) & set(actual)):'),
    ('comparator-length-blind', 'selftest.py',
     '        if len(actual) != len(expected):', '        if False and len(actual) != len(expected):'),
    ('report-failure-verified-dropped', 'selftest.py',
     '    except (SelftestError, DeadlineExceeded) as error:\n'
     '        return {"case": name, "failed": str(error), "verified": False}',
     '    except (SelftestError, DeadlineExceeded) as error:\n'
     '        return {"case": name, "failed": str(error)}'),
]
selected = sys.argv[2:]
if selected:
    mutations = [mutation for mutation in mutations if mutation[0] in selected]
    assert len(mutations) == len(selected), 'unknown mutation name'
env = os.environ.copy()
assert env.get('PDRS_BASELINE'), 'PDRS_BASELINE must point at the read-only TS baseline root'
assert env.get('CARGO_TARGET_DIR'), '#969 negative: caller must set CARGO_TARGET_DIR'
baseline = 'HEAD'


def run_selftest(name, cases):
    return run_owned([sys.executable, str(source / 'selftest.py'),
                      str(out / f'{name}-evidence'), '--cases', *cases],
                     cwd=root, env=env, timeout=300)


def run_row(name):
    """Run the row's named check: a selftest case run or the attack suite."""
    if name in ATTACK_ROWS:
        return run_owned([sys.executable, '-B', str(source / 'attack_suite.py'), '--report', str(out / f'{name}-attacks.json')],
                         cwd=root, env=env, timeout=120)
    if name == 'driver-abort-never-fires':
        env['LIVE_E2E_TEST_NO_ABORT'] = '1'
        try:
            return run_selftest(name, TESTS[name][0])
        finally:
            env.pop('LIVE_E2E_TEST_NO_ABORT', None)
    return run_selftest(name, TESTS[name][0])


def abort_evidence_ok():
    return validate_abort(out / 'driver-abort-never-fires-evidence' / 'cancel-passthrough')


def report_contract_ok(name):
    """Every report case cell carries an explicit verdict (README contract)."""
    path = out / f'{name}-evidence' / 'report.json'
    try:
        report = json.loads(path.read_text())
    except (OSError, ValueError):
        return False, 'missing/unreadable report.json'
    entries = report.get('cases')
    if not isinstance(entries, list) or not entries:
        return False, f'cases={entries!r}'
    for entry in entries:
        if 'verified' not in entry:
            return False, f'{entry.get("case")!r} has no verified field'
        if entry['verified'] is not True and not entry.get('failed'):
            return False, f'{entry.get("case")!r} failed without a message'
    return True, 'every case cell carries an explicit verdict'


def attack_escaped(name, bound):
    try:
        rows = json.loads((out / f'{name}-attacks.json').read_text())
        matches = [row for row in rows if row.get('name') == bound.removeprefix('ESCAPED ')]
        return len(matches) == 1 and matches[0].get('caught') is False
    except (OSError, ValueError, TypeError):
        return False


def target_observed(name):
    if name.startswith('mitm-') and name in {
        'mitm-record-stops-on-close', 'mitm-delivered-lying',
        'mitm-eof-reap-disabled', 'mitm-group-reap-blind'}:
        return True  # Each direct check already binds its full dedicated assertion.
    if name in {'driver-abort-never-fires', 'driver-home-leaked'}:
        return True  # Dedicated driver assertion, cancel also checks structured completion.
    try:
        case = out / f'{name}-evidence' / TESTS[name][0][0]
        result = json.loads((case / 'driver-result.json').read_text())
        frames = json.loads((case / 'frames.json').read_text())['s2h']
        if name == 'driver-tool-payload-forged':
            return result['error'] is None and result['toolResults'] == [{'id':'call-live-e2e-read', 'text':'forged'}]
        if name == 'mitm-double-delivery':
            return result['error'] == 'Sidecar event sequence mismatch: expected 1, got 0.'
        if name == 'mitm-duplicate-disabled':
            return result['error'] is None and result['terminal'] == {
                'type':'success', 'stopReason':'completed', 'turns':1, 'messageCount':2}
        action = 'drop' if name == 'mitm-drop-disabled' else 'early_final'
        rows = [r for r in frames if r['action'] == [action]]
        if len(rows) != 1 or result['error'] is not None:
            return False
        return (rows[0]['delivered'] is True if action == 'drop'
                else rows[0]['frame']['final'] is False)
    except (OSError, ValueError, TypeError, KeyError):
        return False


def bound_in_report(name, bound):
    try:
        report = json.loads((out / f'{name}-evidence' / 'report.json').read_text())
        for cell in report['cases']:
            if cell['case'] in TESTS[name][0] and cell.get('verified') is False and bound in cell.get('failed', ''):
                return True
        direct = {'mitm-record-stops-on-close': 'mitm-pipe-close',
                  'mitm-delivered-lying': 'mitm-pipe-close',
                  'mitm-eof-reap-disabled': 'mitm-eof-reap',
                  'mitm-group-reap-blind': 'mitm-eof-reap-group'}
        value = report['mitmChecks'].get(direct.get(name))
        return isinstance(value, str) and bound in value
    except (OSError, ValueError, KeyError, TypeError):
        return False


summary = []
# Run every mutation before concluding anything; the verdict comes last.
with mutating_sources(root, sorted({source / filename for _, filename, _, _ in mutations if filename}),
                      baseline=baseline) as snapshots:
    for name, filename, old, new in mutations:
        path = source / filename if filename else None
        original = snapshots[path] if path else None
        before = hashlib.sha256(original).hexdigest() if original else None
        content = original.decode() if original else None
        changed = None
        if old:
            changed = content.replace(old, new, 1)
            (out / f'{name}.patch').write_text(''.join(difflib.unified_diff(
                content.splitlines(True), changed.splitlines(True),
                fromfile=filename, tofile=filename)))
        status, detail, rc = 'INVALID', 'row did not produce a verdict', None
        try:
            bound = TESTS[name][1]
            if old and content.count(old) != 1:
                raise RuntimeError(f'anchor count {content.count(old)} for {name}, expected 1')
            # The cancel mutant first re-proves its own cell green with the
            # pristine sources: a selected single mutant skips the baseline.
            if name not in ATTACK_ROWS and name != 'baseline':
                precheck = run_selftest(f'{name}-precheck', TESTS[name][0])
                (out / f'{name}-precheck.log').write_text(precheck.stdout + precheck.stderr)
                if precheck.returncode != 0:
                    raise RuntimeError(
                        f'positive control failed before mutation: rc={precheck.returncode}')
            if changed is not None:
                path.write_text(changed)
                if path.suffix == '.py':
                    compile(changed, str(path), 'exec')
                else:
                    syntax = run_owned(['node', '--import', str(Path(env['PDRS_BASELINE']) / 'node_modules/tsx/dist/loader.mjs'),
                                        str(source / 'syntax.mts'), str(path)], cwd=root, env=env, timeout=30)
                    if syntax.returncode != 0:
                        raise RuntimeError('mutant TS syntax invalid: ' + syntax.stderr)
            try:
                result = run_row(name)
            except subprocess.TimeoutExpired:
                result = None
                status, detail = 'INVALID', 'row timed out'
            if result is not None:
                rc = result.returncode
                log = result.stdout + result.stderr
                (out / f'{name}.log').write_text(log)
                if name == 'baseline':
                    # Positive control: the same runner path must pass.
                    status = 'KILLED' if rc == 0 else 'SURVIVED'
                    detail = f'rc={rc}'
                elif name in ATTACK_ROWS:
                    if rc == 0:
                        status, detail = 'SURVIVED', 'attack suite caught nothing with the mutant applied'
                    elif rc == 1 and attack_escaped(name, bound):
                        status, detail = 'KILLED', 'bound attack escaped in structured report'
                    else:
                        status, detail = 'INVALID', f'rc={rc} without the bound escape line'
                elif rc == 0:
                    status, detail = 'SURVIVED', 'selftest passed with the mutant applied'
                elif rc == 1 and bound in log and bound_in_report(name, bound) and target_observed(name):
                    if name == 'driver-abort-never-fires':
                        if 'turn did not finish within 60s' in log:
                            status, detail = 'INVALID', 'killed by the driver watchdog, not the cancel assertion'
                        else:
                            ok, why = abort_evidence_ok()
                            status, detail = ('KILLED', why) if ok else ('INVALID', f'missing structured evidence: {why}')
                    else:
                        status, detail = 'KILLED', 'bound message present'
                else:
                    status = 'INVALID'
                    detail = f'rc={rc} without the bound message' if rc == 1 else f'rc={rc}'
            if result is not None and name not in ATTACK_ROWS:
                ok, why = report_contract_ok(name)
                if not ok:
                    status, detail = 'INVALID', f'failed report contract: {why}'
        except Exception as error:
            # Record and continue: the verdict must come after every row ran.
            status, detail, rc = 'INVALID', f'row error: {error}', None
        finally:
            if original is not None:
                path.write_bytes(original)
                assert hashlib.sha256(path.read_bytes()).hexdigest() == before
            summary.append(dict(name=name, status=status, detail=detail,
                                returncode=rc, bound=TESTS[name][1] if name in TESTS else None))
            (out / 'summary.json').write_text(json.dumps(summary, ensure_ascii=False, indent=2) + '\n')
            print(f'{name}: {status} ({detail})', flush=True)
    print(f'{sum(1 for row in summary if row["status"] == "KILLED")}/{len(mutations)} killed; '
          f'every source restored byte-for-byte', flush=True)
not_killed = [f'{row["name"]}: {row["status"]} ({row["detail"]})'
              for row in summary if row['status'] != 'KILLED']
assert not not_killed, f'rows that did not reproduce their bound failure: {not_killed}'
