#!/usr/bin/env python3
"""#1354/#2510 guarded main-entry routing mutation; commit first, never bypass HEAD.

NEW_OUTPUT [mutation ...]. --check validates anchors without changing sources.
Every mutant must compile, kill its exact bound test, and pass after
restoration. The bound tests split over two cargo targets: the unit matrix
in `native_process::tests` runs with `--lib`, and the routing observations
spawn the real `CARGO_BIN_EXE_pilotdeck-gateway` child, so they run with
`--test mode_routing` (that target rebuilds the gateway binary, which is
where main.rs's routing lives). Unbound observations (launch failures,
deadlines without an advertisement, stderr pipe errors) stay INVALID: a slow
host is never evidence. The #2510 rows below are the measured counterweight,
not an exception to that rule: their mutants break the shared advertisement
predicate itself (`tests/common/native_entry.rs`), so the deadline expect
they bind fires only in a run whose own positive control already saw the
unmutated child advertise — the mutant, not the host, is what the deadline
indicts.

The self-managed runner keeps the shared verdict invariants
(specs/negative-controls/README.md): the denominator is the static
EXPECTED_NAMES roster checked at startup, refusals exit 2 ("never started"),
the mutant is published with one atomic rename, and the closing exit code
comes from negative_selection.verdict_exit.
"""
import difflib
import os
import re
import signal
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import (defer_signal_exit, mutating_sources, run_owned,
                              tree_lock, wait_for_slot, write_source_atomic)
from negative_selection import (claim_dir, missing_target, refuse,
                                refuse_unusable_target, registered, select,
                                verdict_exit)
from negative_suite import Case, classify, passed, publish_summary

ROOT = Path(__file__).resolve().parents[5]
NATIVE_PROCESS = ROOT / 'src/pilotdeck-gateway/src/native_process.rs'
MAIN = ROOT / 'src/pilotdeck-gateway/src/main.rs'
UNIT_TESTS = ROOT / 'src/pilotdeck-gateway/src/native_process/tests.rs'
INTEGRATION = ROOT / 'src/pilotdeck-gateway/tests/mode_routing.rs'
# The shared constants both sibling tests observe through (#2510 item 1):
# the 8 mode rows never touch this file, so a drifted NATIVE_LISTENING or a
# flipped `advertises` judgment had no mutant that could see it.
COMMON = ROOT / 'src/pilotdeck-gateway/tests/common/native_entry.rs'
UNIT_TEST = 'native_process::tests::gateway_mode_config_matrix'
ENV_TEST = 'native_process::tests::explicit_options_and_environment'
# The lib-target bound tests run `--lib <name>`; everything else is an
# integration test of the routing target.
LIB_TESTS = (UNIT_TEST, ENV_TEST)

mutations = [
    # #1303 option A is the pinned default: an unset config item must keep
    # the proxy routing. Flipping the None arm routes the default entry to
    # native, so the unit matrix's first assertion dies.
    ('mode-default-native', NATIVE_PROCESS,
     '    match mode {\n'
     '        None => Ok(false),\n'
     '        Some(value) => match value.to_str() {\n'
     '            Some("proxy") => Ok(false),\n'
     '            Some("native") => Ok(true),',
     '    match mode {\n'
     '        None => Ok(true),\n'
     '        Some(value) => match value.to_str() {\n'
     '            Some("proxy") => Ok(false),\n'
     '            Some("native") => Ok(true),',
     UNIT_TEST, '#1354 unset gateway mode keeps the proxy default (#1303 A)'),
    # `native` must route to the native composition; ignoring it turns every
    # config-driven native start into a proxy start (the rollback target
    # only, with no way forward).
    ('mode-env-ignored', NATIVE_PROCESS,
     '            Some("native") => Ok(true),',
     '            Some("native") => Ok(false),',
     UNIT_TEST, '#1354 explicit native mode'),
    # Fail closed: any other value is rejected at startup. Accepting the
    # invalid arm silently falls through to the proxy default.
    ('mode-invalid-accepted', NATIVE_PROCESS,
     '            _ => {\n'
     '                Err("gateway mode (PILOTDECK_GATEWAY_MODE) must be exactly proxy or native.".into())\n'
     '            }',
     '            _ => Ok(false),',
     UNIT_TEST, '#1354 invalid gateway mode'),
    # The empty string is a value, not an unset key: it must stay rejected
    # (#1369 shape), not degrade into the default.
    ('mode-empty-accepted', NATIVE_PROCESS,
     '        Some(value) => match value.to_str() {\n'
     '            Some("proxy") => Ok(false),',
     '        Some(value) if value.is_empty() => Ok(false),\n'
     '        Some(value) => match value.to_str() {\n'
     '            Some("proxy") => Ok(false),',
     UNIT_TEST, '#1354 invalid gateway mode'),
    # The per-invocation `--native` argument outranks the config item. With
    # the flip, a `proxy` config item swallows the explicit flag, so the
    # argv precedence test's parser-signature assertion dies.
    ('mode-argv-precedence-flipped', MAIN,
     '    if explicit_native || mode_native {',
     '    if mode_native {',
     'argv_native_wins_over_the_proxy_mode_config',
     '#1354 --native must beat the proxy config item'),
    # The mode gate is judged before the argument's routing, so an invalid
    # value fails closed even with `--native` present (#1369 shape).
    # Restoring the short-circuit lets the explicit flag smuggle a typo in
    # the config item past startup, so the bypass test's gate-message
    # assertion dies.
    ('mode-argv-bypasses-gate', MAIN,
     '    let mode_native = pilotdeck_gateway::native_process::gateway_mode(\n'
     '        std::env::var_os("PILOTDECK_GATEWAY_MODE").as_deref(),\n'
     '    )?;\n'
     '    if explicit_native || mode_native {',
     '    let mode_native = !explicit_native\n'
     '        && pilotdeck_gateway::native_process::gateway_mode(\n'
     '            std::env::var_os("PILOTDECK_GATEWAY_MODE").as_deref(),\n'
     '        )?;\n'
     '    if explicit_native || mode_native {',
     'invalid_mode_config_fails_closed_even_with_native_argv',
     '#1354 --native must not bypass the mode gate'),
    # The config item's routing must hand the native options parser the
    # whole command line — `first` included. Dropping it makes the first
    # option's value the leading key, so the env-routed native child fails
    # at parse and never advertises.
    ('mode-env-argv-drop', MAIN,
     '    if explicit { None } else { first }.into_iter().chain(rest)',
     '    let _ = (explicit, first);\n'
     '    rest',
     'mode_config_routes_the_native_entry',
     '#1354 native never advertised listening'),
    # The entry-scope mode item must not sink into the native subtree
    # (#1354): renaming the removal to a dead key keeps
    # PILOTDECK_GATEWAY_MODE in the child environment, so the environment
    # pin's entry-scope marker dies.
    ('mode-env-sinks', NATIVE_PROCESS,
     '    inherited.remove("PILOTDECK_GATEWAY_MODE");',
     '    inherited.remove("PILOTDECK_MODE_SINK_UNUSED");',
     ENV_TEST, '#1354 entry-scope mode item not inherited'),
    # #2510 item 1: the shared advertisement phrase had no drift binding.
    # Pointing it one trailing space off makes the predicate unable to match
    # the child's real line, so the routing observation never resolves and the
    # 120 s advertisement budget ends at its marked deadline expect (the
    # same case's positive control proves the child did advertise).
    ('native-listening-flipped', COMMON,
     'pub const NATIVE_LISTENING: &str = "native listening";',
     'pub const NATIVE_LISTENING: &str = "native listening ";',
     'mode_config_routes_the_native_entry',
     '#1354 native advertisement deadline'),
    # #2510 item 1: the predicate's own judgment had no drift binding. Flipping
    # the address-shape polarity rejects the real advertisement (a non-empty,
    # whitespace-free address now reads as "not an advertisement"), so the
    # observation loops past every line and the same marked deadline fires.
    ('advertises-judgment-flipped', COMMON,
     '        Some(Some(address)) => !address.is_empty() && !address.contains(char::is_whitespace),',
     '        Some(Some(address)) => address.is_empty() || address.contains(char::is_whitespace),',
     'mode_config_routes_the_native_entry',
     '#1354 native advertisement deadline'),
]

# #1666: the denominator is this literal roster, not `len(mutations)`. With a
# runtime denominator a deleted row shrinks the run silently and a table that
# lost every row reports `0/0 killed` and exits 0. Renaming or dropping a
# mutant here is a deliberate edit that must update this tuple too, in the
# same order as the table.
EXPECTED_NAMES = (
    'mode-default-native',
    'mode-env-ignored',
    'mode-invalid-accepted',
    'mode-empty-accepted',
    'mode-argv-precedence-flipped',
    'mode-argv-bypasses-gate',
    'mode-env-argv-drop',
    'mode-env-sinks',
    'native-listening-flipped',
    'advertises-judgment-flipped',
)


def function_body(name, text):
    """One top-level function's source, cut at its column-zero `}`.

    Signatures may span lines (`native_child_serves` does), so the opening
    paren is only the entry point and the body ends at the first column-zero
    closing brace — a sibling's assertion cannot satisfy the binding.
    """
    match = re.search(r'^(?:async )?fn ' + re.escape(name) + r'\(', text, re.M)
    if not match:
        return ''
    end = text.find('\n}', match.start())
    return text[match.start():end + 2] if end >= 0 else ''


def test_body(test, texts=None):
    """The bound test's own source body, cut at its column-zero `}`."""
    if texts is None:
        texts = {UNIT_TESTS: UNIT_TESTS.read_text(), INTEGRATION: INTEGRATION.read_text()}
    path = UNIT_TESTS if test.startswith('native_process::tests::') else INTEGRATION
    return function_body(test.rsplit('::', 1)[-1], texts[path])


def binding_text(test, texts=None):
    """The bound test's body plus the helper bodies it names.

    A routing observation asserts through `native_child_serves` (and sets up
    through `native_fixtures`): the marker that the mutant kills lives in
    the helper's own panic, not in the test's body. The kill judge still
    requires the marker inside the *named test's* panic block (a helper
    panic runs on the named test's own thread), so this widening only binds
    `--check` against marker drift; every helper named here is one the named
    test actually calls.
    """
    if texts is None:
        texts = {UNIT_TESTS: UNIT_TESTS.read_text(), INTEGRATION: INTEGRATION.read_text()}
    body = test_body(test, texts)
    if not body:
        return ''
    combined = [body]
    path = UNIT_TESTS if test.startswith('native_process::tests::') else INTEGRATION
    for helper in re.findall(r'^(?:async )?fn (\w+)\(', texts[path], re.M):
        if re.search(r'\b' + re.escape(helper) + r'\s*\(', body):
            helper_body = test_body(helper, texts)
            if helper_body:
                combined.append(helper_body)
    return '\n'.join(combined)


def check_errors():
    """The `--check` verdict: every anchor and marker binding is intact."""
    # #2795 fix-r4（评审 sibling P1 同族跟改，fix-r3 mcp-http-encoding 先例）：
    # 锚点核对（含 test_body / binding_text 的字典推导裸读）也要持树锁读
    # ——兄弟变异运行器在自己的 write 与 restore 之间可能把变异体留在盘上
    # （native_process/tests.rs 是 ≥5 个 runner 的共享变异目标），无锁读会把
    # 核对错在它不拥有的源上；锁忙 / 读失败是拒跑（2）。本窗口只包只读核对
    # 面，与后续 mutating_sources 是顺序关系，锁先释放再重取，无同进程嵌套。
    try:
        with tree_lock(ROOT, timeout=60):
            errors = []
            for name, path, old, new, test, assertion in mutations:
                text = path.read_text()
                body = binding_text(test)
                if text.count(old) != 1:
                    errors.append(f'{name}: anchor occurs {text.count(old)} times')
                if old == new:
                    errors.append(f'{name}: anchor equals replacement')
                if not body:
                    errors.append(f'{name}: bound test {test} not found exactly once')
                elif assertion not in body:
                    errors.append(f'{name}: marker not in the bound test body')
            return errors
    except (TimeoutError, OSError, UnicodeDecodeError) as error:
        refuse(f'tree lock or anchor read failed: '
               f'{type(error).__name__}: {error}')


def terminate_cleanly(signum, _frame):
    """Unwind on SIGTERM/SIGHUP/SIGQUIT so the finally blocks restore the
    sources; the default disposition would leave a mutant in the tree
    (mirror of same-json-surfaces' handler)."""
    if defer_signal_exit(signum):
        return
    raise SystemExit(128 + signum)


def install_terminate_handlers():
    signal.signal(signal.SIGTERM, terminate_cleanly)
    if hasattr(signal, 'SIGHUP'):
        signal.signal(signal.SIGHUP, terminate_cleanly)
    if hasattr(signal, 'SIGQUIT'):
        signal.signal(signal.SIGQUIT, terminate_cleanly)


def main():
    # The roster is validated before any branch, so the `--check` static
    # gate (CI) refuses roster drift — a renamed or dropped row with a
    # stale `EXPECTED_NAMES` must not print "anchors OK" (the
    # sdk-mcp-session-tools precedent). The roster and the subset selection
    # are both ordinary raises, so a typo is refused as "never started"
    # (exit 2), never mistaken for a failed control (exit 1).
    try:
        registered(mutations, EXPECTED_NAMES)
    except ValueError as error:
        refuse(str(error))
    if sys.argv[1:2] == ['--check']:
        errors = check_errors()
        if errors:
            raise SystemExit('#1354 ' + '; '.join(errors))
        print(f'#1354 {len(mutations)} mutation anchors OK')
        return 0
    if len(sys.argv) < 2:
        raise SystemExit('usage: negative_controls.py --check | NEW_OUTPUT [mutation ...]')
    # No baseline row exists in this suite: the suite's own positive controls
    # run per case, so the selector's baseline default is turned off here.
    # `select` returns the table's own rows (tuples); the runner below
    # speaks `Case`, whose field order is the table's column order.
    try:
        run_rows = [Case(*row) for row in select(mutations, sys.argv[2:], always=())]
    except ValueError as error:
        refuse(str(error))
    if missing_target():
        refuse('set CARGO_TARGET_DIR to the caller-owned build tree before running')
    refuse_unusable_target()
    out = claim_dir(sys.argv[1])
    install_terminate_handlers()
    env = dict(os.environ)
    env.update(CARGO_PROFILE_DEV_DEBUG='line-tables-only',
               CARGO_PROFILE_TEST_DEBUG='line-tables-only', LANG='en_US.UTF-8')
    env.pop('LC_ALL', None)
    env.pop('LC_MESSAGES', None)
    base = ['cargo', 'test', '--offline', '--manifest-path', 'src/Cargo.toml',
            '-p', 'pilotdeck-gateway']
    results = []

    def run(label, case, args):
        # The bound test's cargo target: the routing observations spawn the
        # real binary, so they run the integration target (which rebuilds
        # the gateway binary with the mutated main.rs); the unit matrix runs
        # the lib target.
        command = list(base)
        command += (['--lib', case.test] if case.test in LIB_TESTS
                    else ['--test', 'mode_routing', case.test])
        slot = wait_for_slot(ROOT / 'scripts/cargo-slots.sh', timeout=900)
        (out / (label + '.slots.log')).write_text(slot.stdout + slot.stderr)
        result = run_owned(command + args, cwd=ROOT, env=env, timeout=900)
        (out / (label + '.log')).write_text(result.stdout + result.stderr)
        return result

    # One whole-run window, the family convention (`settings`,
    # `native-process`): the tree lock is taken once around every case, so a
    # sibling suite serializes against this run instead of interleaving
    # between its cases.
    guarded = [NATIVE_PROCESS, MAIN, UNIT_TESTS, INTEGRATION, COMMON,
               Path(__file__).resolve(),
               Path(__file__).resolve().parents[1] / 'negative_process.py',
               Path(__file__).resolve().parents[1] / 'negative_suite.py',
               Path(__file__).resolve().parents[1] / 'negative_selection.py']
    with mutating_sources(ROOT, guarded) as originals:
        for case in run_rows:
            row = dict(mutation=case.mutation, test=case.test, assertion=case.assertion,
                       status='INVALID', reason='', compiled=False)
            args = ['--', '--exact']
            try:
                try:
                    original = originals[case.path].decode()
                    if original.count(case.old) != 1 or case.old == case.new:
                        raise RuntimeError(f'stale anchor: {case.mutation}')
                    changed = original.replace(case.old, case.new, 1)
                    if not passed(run(case.mutation + '-positive', case, args), case.test):
                        raise RuntimeError('positive control failed')
                    (out / (case.mutation + '.patch')).write_text(''.join(difflib.unified_diff(
                        original.splitlines(True), changed.splitlines(True),
                        fromfile=str(case.path), tofile=str(case.path))))
                    # One atomic rename, never a truncating in-place write: a
                    # SIGKILL in the write-back window must not leave a
                    # half-written source behind (#1903).
                    write_source_atomic(ROOT, case.path, changed.encode())
                    compiled = run(case.mutation + '-compile', case, ['--no-run'])
                    row['compiled'] = compiled.returncode == 0
                    if not row['compiled']:
                        raise RuntimeError('mutant did not compile')
                    row['status'], row['reason'] = classify(
                        run(case.mutation + '-negative', case, args), case)
                finally:
                    # Restore per case, the `native-process` family's own
                    # sequence: the whole-run context below is only the
                    # backstop (a crash between cases), not the per-case
                    # restore — the next case's positive control must see
                    # the original bytes.
                    write_source_atomic(ROOT, case.path, originals[case.path])
                if not passed(run(case.mutation + '-restored', case, args), case.test):
                    raise RuntimeError('restored positive control failed')
            except Exception as error:
                row.update(status='INVALID', reason=f'{type(error).__name__}: {error}')
            results.append(row)
            print(f"{case.mutation}: {row['status']}: {row['reason']}", flush=True)
    publish_summary(out, results)
    return verdict_exit(results, key='status')


if __name__ == '__main__':
    raise SystemExit(main())
