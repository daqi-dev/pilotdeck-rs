#!/usr/bin/env python3
"""#1371: commit first, then compile and run every loop-stimulus mutation.

Usage: negative_controls.py --check
       negative_controls.py [--compile-only] NEW_OUTPUT_DIR [mutation ...]
Every mutation must compile, fail at its bound assertion, and pass again after
restoration. Source bytes must equal HEAD; no alternate baseline exists.

Unit mutations bind to `pilotdeck-transport` lib tests (plain `assert!` panic
payloads). Executable mutations bind to the *label* of the classified
`native_loop_stimulus.rs` assertion: the fixture records the first failing
assertion in `assertionFailure`, that label panics, and a different failure
only trips the generic oracle and stays INVALID.
"""
import argparse
import hashlib
import json
import os
import re
import sys
from dataclasses import dataclass
from pathlib import Path

ROOT = Path(__file__).resolve().parents[5]
FIXTURES = Path(__file__).resolve().parents[1]
LOOP = ROOT / 'src/pilotdeck-transport/src/loop_stimulus.rs'
CONTROLLED = ROOT / 'src/pilotdeck-transport/src/controlled_loop.rs'
COMPLETION = ROOT / 'src/pilotdeck-gateway/src/runtime/session/completion.rs'
ASSEMBLY = ROOT / 'src/pilotdeck-gateway/src/native_process/assembly.rs'
PROCESS_TEST = ROOT / 'src/pilotdeck-gateway/tests/native_loop_stimulus.rs'
PROCESS = Path(__file__).resolve().parent / 'process.mts'
sys.path.insert(0, str(FIXTURES))
from negative_process import mutating_sources, run_owned, wait_for_slot, tree_lock
from negative_panic import panic_evidence
from negative_suite import invalidate_completed, passed

EXECUTABLE = 'executable_native_loop_stimulus'

mutations = [
    # The bound is the safety release: without the deadline the loop waits
    # forever, so the watchdog test fails instead of expiring.
    ('outcome-bound-unbounded', LOOP,
     'if Instant::now() >= deadline {',
     'if false {',
     'loop_stimulus::tests::bound_expires_instead_of_hanging', '#1371 bound expires instead of hanging'),
    # A release must match the plan id: ignoring it lets a foreign release
    # end a loop that is still waiting.
    ('release-ignores-id', LOOP,
     '''let matches_id = value
                            .get("id")
                            .and_then(JsValue::as_string)
                            .is_some_and(|id| id.equals_str(&plan.id));''',
     'let matches_id = true;',
     'loop_stimulus::tests::release_matches_id_and_stage', '#1371 foreign releases are ignored'),
    # An armed directory without a plan stays inert; fabricating a plan turns
    # every ordinary turn of that process into a controlled one.
    ('armed-empty-not-inert', LOOP,
     'Ok(None) => State::Inert,',
     'Ok(None) => State::Planned(Plan { id: "default".into(), phase: Phase::Model, order: Order::TimeoutFirst, bound: DEFAULT_BOUND }),',
     'loop_stimulus::tests::missing_plan_is_inert', '#1371 no plan is inert'),
    # Only the `known` release may complete a turn: treating every release as
    # known fabricates a success for a result-unknown outcome, which the tool
    # cell reports as `#1371 tool phase unknown terminal`.
    ('unknown-release-treated-as-known', CONTROLLED,
     'if release.outcome.as_deref() == Some("known") {',
     'if true {',
     EXECUTABLE, '#1371 executable tool unknown terminal'),
    # The tool phase must publish the controlled_wait prefix; dropping it
    # loses the tool/model distinction the stimulus table depends on
    # (`#1371 tool phase controlled_wait prefix`).
    ('tool-prefix-omitted', CONTROLLED,
     'if plan.phase == Phase::Tool {',
     'if false {',
     EXECUTABLE, '#1371 executable tool controlled_wait prefix'),
    # A held return must keep the buffered completed unpublished: publishing
    # it before the release is exactly the ordering the cell forbids
    # (`#1371 return-pending completed is not published before the release`).
    ('completed-published-before-return', CONTROLLED,
     '        stimulus.publish("loop", "return", "block", None);',
     '''        stimulus.publish("loop", "final", "completed", None);
        stimulus.publish("loop", "return", "block", None);''',
     EXECUTABLE, '#1371 executable return-pending early final'),
    # The fence.complete witness is the only evidence that completion happened
    # before the late unknown; without it the collector cannot order the cell
    # (`#1371 no fence complete receipt for key loop-after-complete-run`).
    ('fence-complete-witness-removed', COMPLETION,
     '        pilotdeck_transport::loop_stimulus::witness_fence("complete", run_id.as_str());',
     '        let _ = run_id;',
     EXECUTABLE, '#1371 executable after-complete fence missing'),
    # A release without an id belongs to no plan. Turning the id test into a
    # wildcard lets any unnamed file release a waiting loop, which the
    # missing-id assertion forbids.
    ('release-missing-id-wildcard', LOOP,
     '''let matches_id = value
                            .get("id")
                            .and_then(JsValue::as_string)
                            .is_some_and(|id| id.equals_str(&plan.id));''',
     '''let matches_id = value
                            .get("id")
                            .and_then(JsValue::as_string)
                            .is_none_or(|id| id.equals_str(&plan.id));''',
     'loop_stimulus::tests::release_matches_id_and_stage', '#1371 releases without an id are ignored'),
    # The stage is part of the release identity: a return release must not
    # satisfy an outcome wait.
    ('release-ignores-stage', LOOP,
     '''                                .is_some_and(|value| value.equals_str(stage))''',
     '''                                .is_some_and(|_| true)''',
     'loop_stimulus::tests::release_matches_id_and_stage', '#1371 foreign stages are ignored'),
    # A bound must be a whole number of milliseconds: silently rounding a
    # fractional claim would record the cell under a bound nobody asked for.
    ('bound-fraction-accepted', LOOP,
     ' && ms.fract() == 0.0',
     '',
     'loop_stimulus::tests::malformed_plan_fails_closed', '#1371 unsupported bound fails closed'),
    # The unknown-first cell must prove the release won the race against the
    # turn timer. Delaying the release past the 400 ms timeout makes the timer
    # decide the turn, and the bounded assertion must catch it.
    ('unknown-first-release-after-timer', PROCESS,
     '''    await release(control, plan, 'outcome', 'unknown');
    const {events, at, terminal} = await pump;''',
     '''    await sleep(600);
    await release(control, plan, 'outcome', 'unknown');
    const {events, at, terminal} = await pump;''',
     EXECUTABLE, '#1371 executable unknown-first before timer'),
    # Only a resolved plan may suppress the automatic title request; arming a
    # directory that carries no plan is inert, and an inert entry must keep the
    # production title request (`#1371 inert turn keeps the title request`).
    ('title-suppressed-when-armed', ASSEMBLY,
     '''    let loop_planned = pilotdeck_transport::loop_stimulus::LoopStimulus::global()
        .is_some_and(|stimulus| stimulus.is_planned());''',
     '''    let loop_planned = loop_switch.is_some();''',
     EXECUTABLE, '#1371 executable inert title request'),
]


@dataclass
class Case:
    mutation: str
    path: object
    old: str
    new: str
    test: str
    assertion: object


STRING = re.compile(r'r#"(?:[^"]|"(?!#))"#|"(?:\\.|[^"\\])*"|\'(?:\\.|[^\'\\])*\'', re.S)
ANCHOR_FN = r'^\s*(?:pub(?:\([^)]*\))? )?(?:async )?fn {}\('


def strip_literals(line):
    """Replace every string/char literal with `""`, keeping the rest verbatim.

    Uses `finditer` rather than `Pattern.sub`: the shared anchor checker
    (anchor_selfcheck.regex_flags) refuses compiled/aliased regex consumers in
    a control script, and this file also uses literal `.replace()`.
    """
    out = []
    position = 0
    for match in STRING.finditer(line):
        out.append(line[position:match.start()])
        out.append('""')
        position = match.end()
    out.append(line[position:])
    return ''.join(out)


def fn_body(text, name):
    """Return the one definition of `name`, ending at its matching brace."""
    lines = text.splitlines()
    heads = [index for index, line in enumerate(lines)
             if re.match(ANCHOR_FN.format(re.escape(name)), line)]
    if len(heads) != 1:
        return ''
    head = heads[0]
    depth = 0
    opened = False
    for index in range(head, len(lines)):
        # String literals hold balanced braces here, but strip them anyway so a
        # later edit cannot make the brace count silently wrong.
        code = strip_literals(lines[index])
        depth += code.count('{') - code.count('}')
        opened = opened or '{' in code
        if opened and depth <= 0:
            return '\n'.join(lines[head:index + 1])
    return ''


def test_body(test):
    path = LOOP if test.startswith('loop_stimulus::tests::') else PROCESS_TEST
    return fn_body(path.read_text(), test.rsplit('::', 1)[-1])


def check_binding_scope():
    fixture = 'fn target() {\n}\nfn sibling() {\n    assert!(false, "only sibling");\n}\n'
    if 'only sibling' in fn_body(fixture, 'target'):
        raise RuntimeError('#1371 binding checker accepted sibling assertion')
    if 'only sibling' not in fn_body(fixture, 'sibling'):
        raise RuntimeError('#1371 binding checker cannot find a sibling assertion')


def classify(result, case):
    log = result.stdout + result.stderr
    if passed(result, case.test):
        return 'SURVIVED', 'bound test still passed'
    anchors = case.assertion if isinstance(case.assertion, list) else [case.assertion]
    # The named test's own failure plus its own panic payload is the only
    # accepted evidence; a sibling panic is not. The payload comes from the
    # shared #813 helper so this fixture cannot drift into a looser rule:
    # a block is attributable only to the named test (or, in a one-test run,
    # to a thread that test spawned itself).
    named = re.search(r'^test ' + re.escape(case.test) + r' \.\.\. FAILED$', log, re.MULTILINE)
    try:
        payload = panic_evidence(log, case.test)
    except ValueError:
        return 'INVALID', 'unattributable panic evidence (module path or ambiguous run)'
    if (result.returncode != 0 and named is not None and 'test result: FAILED' in log
            and 'could not compile' not in log
            and any(anchor in payload for anchor in anchors)):
        return 'KILLED', 'bound test failed at its assertion'
    return 'INVALID', 'missing bound runtime failure (build error, wrong panic, or missing test)'


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--check', action='store_true')
    parser.add_argument('--compile-only', action='store_true')
    parser.add_argument('output', nargs='?')
    parser.add_argument('selected', nargs='*')
    args = parser.parse_args()
    check_binding_scope()
    # #2795 fix-r4（评审 sibling P1 同族跟改，fix-r3 mcp-http-encoding 先例）：
    # 锚点预检也要持树锁读——兄弟变异运行器在自己的 write 与 restore 之间可能
    # 把变异体留在盘上，无锁读会把核对错在它不拥有的源上；锁忙 / 读失败是
    # 拒跑（2），不是宿主会读成 rc 1 的裸 traceback。本窗口只包只读预检面，
    # 与后续 mutating_sources 是顺序关系，锁先释放再重取，无同进程嵌套。
    try:
        with tree_lock(ROOT, timeout=60):
            stale = []
            for name, path, old, new, test, assertion in mutations:
                text = path.read_text()
                body = test_body(test)
                if (text.count(old) != 1 or old == new or
                        not body or f'"{assertion}"' not in body):
                    stale.append(name)
    except (TimeoutError, OSError, UnicodeDecodeError) as error:
        print(f'tree lock or anchor read failed: '
              f'{type(error).__name__}: {error}', file=sys.stderr)
        raise SystemExit(2)
    if stale:
        raise SystemExit('#1371 stale anchor/test/assertion bindings: ' + ', '.join(stale))
    if args.check:
        print(f'#1371 {len(mutations)} unique anchors and assertion bindings: OK')
        return 0
    selected = [row for row in mutations if not args.selected or row[0] in args.selected]
    if not selected or (args.selected and len(selected) != len(args.selected)):
        raise SystemExit('#1371 unknown or repeated mutation')
    if not args.output or not os.environ.get('CARGO_TARGET_DIR'):
        raise SystemExit('#1371 output and caller CARGO_TARGET_DIR required')
    output = Path(args.output).resolve()
    output.mkdir(parents=True, exist_ok=False)
    env = dict(os.environ)
    for key in ['PDRS_NEGATIVE_RECEIPTS', 'PDRS_NEGATIVE_PARENT_TEST', 'PDRS_MUTANT']:
        env.pop(key, None)
    # The sidecar sha proofs below compare a mutant build and a restored build
    # against one baseline build. Incremental compilation makes otherwise
    # identical sources produce different binaries, which would turn both
    # comparisons into noise; only the sidecar builds below run with it off,
    # so the far more numerous test builds keep their incremental cache.
    sidecar_env = dict(env)
    sidecar_env['CARGO_INCREMENTAL'] = '0'
    # The adjudicator and the shared apparatus decide KILLED/SURVIVED/INVALID
    # just as much as the product sources do: a loosened `panic_evidence` or
    # `passed` would turn a sibling panic into a recorded kill without any
    # product byte changing. Pin them, so `guardedSha256` names the whole ruler.
    guarded = [LOOP, CONTROLLED, COMPLETION, ASSEMBLY, PROCESS_TEST, PROCESS,
               Path(__file__).resolve(),
               FIXTURES / 'negative_process.py',
               FIXTURES / 'negative_panic.py',
               FIXTURES / 'negative_suite.py',
               ROOT / 'src/pilotdeck-gateway/tests/common/negative_receipt.rs',
               ROOT / 'src/pilotdeck-gateway/src/test_process.rs',
               ROOT / 'src/pilotdeck-transport/src/lib.rs',
               # The executable round runs this binary, and its startup is
               # where the plan is frozen: an unpinned edit here would change
               # what the whole matrix measures.
               ROOT / 'src/pilotdeck-transport/src/bin/pilotdeck-agent-loop-sidecar.rs']
    results = []
    guarded_sha256 = {}
    sidecar_sha256 = None

    def run(label, run_env, test, *, compile_only=False):
        slot = wait_for_slot(ROOT / 'scripts/cargo-slots.sh', timeout=1800)
        (output / f'{label}.slots.log').write_text(slot.stdout + slot.stderr)
        crate = 'pilotdeck-transport' if test.startswith('loop_stimulus::') else 'pilotdeck-gateway'
        command = ['cargo', 'test', '--offline', '--manifest-path', 'src/Cargo.toml', '-p', crate]
        command += ['--test', 'native_loop_stimulus'] if test == EXECUTABLE else ['--lib']
        command += ['--no-run'] if compile_only else [test, '--', '--exact', '--include-ignored']
        run_env = dict(run_env)
        if test == EXECUTABLE:
            run_env['PDRS_1371_PROCESS_OUTPUT'] = str(output / (label + '-process'))
        (output / f'{label}.command.json').write_text(json.dumps(command) + '\n')
        result = run_owned(command, cwd=ROOT, env=run_env, timeout=900)
        (output / f'{label}.log').write_text(result.stdout + result.stderr)
        return result

    def build_sidecar(label):
        """Rebuild the sidecar from the current source bytes under a cargo slot.

        Returns the binary's sha256 so the caller can state which bytes the
        executable round actually observed instead of assuming the rebuild
        landed.
        """
        slot = wait_for_slot(ROOT / 'scripts/cargo-slots.sh', timeout=1800)
        (output / f'{label}.slots.log').write_text(slot.stdout + slot.stderr)
        command = ['cargo', 'build', '--offline', '--manifest-path', 'src/Cargo.toml',
                   '-p', 'pilotdeck-transport', '--bin', 'pilotdeck-agent-loop-sidecar']
        (output / f'{label}.command.json').write_text(json.dumps(command) + '\n')
        built = run_owned(command, cwd=ROOT, env=sidecar_env, timeout=900)
        (output / f'{label}.log').write_text(built.stdout + built.stderr)
        if built.returncode:
            raise RuntimeError(f'{label}: sidecar build failed')
        sidecar = Path(env['CARGO_TARGET_DIR']) / 'debug/pilotdeck-agent-loop-sidecar'
        return hashlib.sha256(sidecar.read_bytes()).hexdigest()

    try:
        with mutating_sources(ROOT, guarded) as originals:
            guarded_sha256 = {str(path.relative_to(ROOT)): hashlib.sha256(originals[path]).hexdigest() for path in guarded}
            if any(row[4] == EXECUTABLE for row in selected):
                sidecar_sha256 = build_sidecar('sidecar-build')
            for name, path, old, new, test, assertion in selected:
                item = dict(name=name, test=test, assertion=assertion,
                            status='INVALID', reason='not run', compiled=False)
                rebuilt_sidecar = False
                try:
                    if not args.compile_only and not passed(run(name + '-positive', env, test), test):
                        raise RuntimeError('positive bound test failed')
                    path.write_text(originals[path].decode().replace(old, new, 1))
                    if test == EXECUTABLE and path in (LOOP, CONTROLLED):
                        # The sidecar must be rebuilt from the mutated source,
                        # or the executable observes the pristine transport.
                        # The sha proves the mutant binary differs from the
                        # baseline, so a no-op rebuild cannot pass as a kill.
                        mutant_sha256 = build_sidecar(name + '-sidecar')
                        item['mutantSidecar'] = mutant_sha256
                        if mutant_sha256 == sidecar_sha256:
                            raise RuntimeError('mutant sidecar bytes are unchanged')
                        rebuilt_sidecar = True
                    if run(name + '-compile', env, test, compile_only=True).returncode:
                        raise RuntimeError('mutant did not compile; not a runtime kill')
                    item['compiled'] = True
                    if args.compile_only:
                        item.update(status='COMPILED', reason='mutated source compiled')
                    else:
                        result = run(name + '-negative', env, test)
                        status, reason = classify(result, Case(name, path, old, new, test, assertion))
                        item.update(status=status, reason=reason)
                except Exception as error:
                    item.update(status='INVALID', reason=f'{type(error).__name__}: {error}')
                finally:
                    path.write_bytes(originals[path])
                try:
                    if not args.compile_only:
                        if test == EXECUTABLE and rebuilt_sidecar:
                            # The restored round must observe the pristine
                            # sidecar: an unchecked rebuild would leave the
                            # mutant binary in place and make `restoredPass`
                            # evidence for the wrong bytes.
                            restored_sha256 = build_sidecar(name + '-restored-sidecar')
                            item['restoredSidecar'] = restored_sha256
                            if restored_sha256 != sidecar_sha256:
                                raise RuntimeError(
                                    'restored sidecar bytes differ from the baseline build')
                        if not passed(run(name + '-restored', env, test), test):
                            raise RuntimeError('restored bound test failed')
                        item['restoredPass'] = True
                except Exception as error:
                    item.update(status='INVALID', reason=item['reason'] + f'; restored: {error}')
                results.append(item)
                print(f'{name}: {item["status"]}: {item["reason"]}', flush=True)
    except Exception as error:
        reason = f'source guard: {type(error).__name__}: {error}'
        # The guard restores and re-verifies every mutated file on exit. When it
        # raises there, the tree may be left mutated, so every row already
        # recorded as KILLED is untrustworthy: publish them as INVALID through
        # the one shared helper (#979) instead of a green summary from a broken
        # run. Filling only the rows that never ran would leave a false pass.
        invalidate_completed(results, reason)
        for name, _, _, _, test, assertion in selected[len(results):]:
            results.append(dict(name=name, test=test, assertion=assertion,
                                status='INVALID', reason=reason, compiled=False))
    counts = {state: sum(row['status'] == state for row in results)
              for state in ['KILLED', 'SURVIVED', 'INVALID', 'COMPILED']}
    temporary = output / 'summary.json.tmp'
    temporary.write_text(json.dumps(
        {'controls': results, 'counts': counts, 'guardedSha256': guarded_sha256,
         'sidecarSha256': sidecar_sha256}, indent=2) + '\n')
    temporary.replace(output / 'summary.json')
    print(json.dumps({'controls': results, 'counts': counts}), flush=True)
    success = 'COMPILED' if args.compile_only else 'KILLED'
    return int(any(row['status'] != success for row in results))


if __name__ == '__main__':
    sys.exit(main())
