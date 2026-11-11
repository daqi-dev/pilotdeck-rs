#!/usr/bin/env python3
"""#2294 HEAD-only controls: compile, fail at the bound assertion, restore.

Usage: negative_controls.py --check
       negative_controls.py [--compile-only] NEW_OUT [mutation ...]
Commit the tests and this script first. CARGO_TARGET_DIR is caller-owned.
Every selected row is reported, including anchor/build/source-guard failures.
`--check` proves each mutation anchor is unique and that the hand-copied
manifest numbers still describe their artifacts (check_manifest, the
cron-schedule #1509 P2-2d depth): README group-table counts vs oracle.json,
the README Rows list vs this table, every oracle scenario bound to a replay
in the Rust sources, every row's bound test actually defined there. It cannot
prove the row's assertion string ever reaches a panic payload, so a full run
is the only proof that a row can go KILLED.

Rows bind one mutation to one lib test (run with `--exact`); the kill evidence
is that test's own panic payload carrying its `#2294` message. The six rows
are the prev-plan §5 targets: the lock gate removed from the gate chain
(g10), the stop order swapped (cron no longer drains before Always-On), the
reload rebuilding an unchanged family, describe advertising the always-on
family with no manager bound, and a fired tick keeping the discovery lock;
plus the #2566 re-arm row: a failing watcher start swapped ahead of its slot
store, pinning the dead watcher in the slot so ensure never re-arms.
The bound replays need no Node: they read the committed oracle bytes, so
every row runs on both platforms.
"""
from pathlib import Path
import json
import os
import re
import subprocess
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot
from negative_suite import Case, classify, declares_test, passed, bind_module_file

root = Path(__file__).resolve().parents[5]
gates = root / 'src/pilotdeck-gateway/src/runtime/always_on/gates.rs'
automation = root / 'src/pilotdeck-gateway/src/native_process/assembly/automation.rs'
capabilities = root / 'src/pilotdeck-gateway/src/native_process/capabilities.rs'
scheduler = root / 'src/pilotdeck-gateway/src/runtime/always_on/scheduler.rs'
methods_always_on = root / 'src/pilotdeck-gateway/src/native_process/methods/always_on.rs'
# 修复轮 r3 P2（simplify）：绑定测试到源文件的映射从 bound_test_source 的
# 深度 1/2 猜路径启发式换成 #1890 共享装置 bind_module_file 的显式
# (prefix, path) 清单——未列出的前缀、多余的 `::` 层级、空短名都
# fail-closed 报错，不再是「找不到文件就返回 None」的软失败。
bound_modules = (
    ('runtime::always_on::scheduler::tests::', scheduler),
    ('runtime::always_on::gates::tests::', gates),
    ('native_process::assembly::automation::tests::', automation),
    ('native_process::methods::always_on::tests::', methods_always_on),
)
oracle = root / 'src/pilotdeck-gateway/tests/fixtures/always-on-2294/oracle.json'
GATES_REPLAY = 'runtime::always_on::gates::tests::replays_the_gate_chain_golden'
B2_REPLAY = 'native_process::assembly::automation::tests::b2_start_stop_order'
B6_REPLAY = 'native_process::assembly::automation::tests::b6_reload_cron_only'
NOT_ADVERTISED = (
    'native_process::methods::always_on::tests::'
    'always_on_methods_are_not_advertised_without_a_manager'
)
T1_REPLAY = 'runtime::always_on::scheduler::tests::t1_fires_with_state_and_lock_bookkeeping'
REARM = 'runtime::always_on::scheduler::tests::a_failed_watch_start_re_arms_after_the_root_returns'
mutations = [
    # The gate chain's lock gate is removed, so g10's held lock answers ok
    # instead of blocked/lock_busy (a held discovery lock no longer short-
    # circuits the chain — the "period arrived but the gate opened anyway"
    # failure the plan's first target names).
    ('gate-chain-lock-gate-removed', gates,
     '    if *lock_held {\n'
     '        return blocked(GateBlockReason::LockBusy);\n'
     '    }\n'
     '\n'
     '    if fresh.is_empty() {\n',
     '    if fresh.is_empty() {\n',
     GATES_REPLAY,
     '#2294 g10-lock-held: the gate chain answer matches the TS baseline'),
    # stopGeneration stops Always-On before cron, so b2's lifecycle log ends
    # ao.stop, cron.stop instead of the reverse startup order the TS comment
    # pins ("producer dependencies drain in reverse startup order").
    ('stop-order-swapped', automation,
     '        if changes.cron_changed\n'
     '            && let Some(cron) = &generation.cron\n'
     '            && let Err(error) = cron.clone().stop().await\n'
     '        {\n'
     '            failures.push(error);\n'
     '        }\n'
     '        if changes.always_on_changed\n'
     '            && let Some(always_on) = &generation.always_on\n'
     '            && let Err(error) = always_on.clone().stop().await\n'
     '        {\n'
     '            failures.push(error);\n'
     '        }\n',
     '        if changes.always_on_changed\n'
     '            && let Some(always_on) = &generation.always_on\n'
     '            && let Err(error) = always_on.clone().stop().await\n'
     '        {\n'
     '            failures.push(error);\n'
     '        }\n'
     '        if changes.cron_changed\n'
     '            && let Some(cron) = &generation.cron\n'
     '            && let Err(error) = cron.clone().stop().await\n'
     '        {\n'
     '            failures.push(error);\n'
     '        }\n',
     B2_REPLAY,
     '#2294 lifecycle order'),
    # stage() rebuilds the unchanged family: the reuse guard is inverted, so a
    # cron-only reload constructs a second Always-On provider (b6's factory
    # counts and log both diverge; the factory counts assert first).
    ('reload-rebuilds-unchanged-family', automation,
     '            Some(previous) if !changes.always_on_changed => previous.always_on.clone(),',
     '            Some(previous) if changes.always_on_changed => previous.always_on.clone(),',
     B6_REPLAY,
     '#2294 factory counts'),
    # The wired probe answers true without a manager, so describe advertises
    # the three always-on methods on a gateway whose control is not bound —
    # the false capability claim the not-advertised test exists to kill.
    ('advertise-without-manager', capabilities,
     'fn always_on(requests: &Requests) -> bool {\n'
     '    requests.always_on.is_some()\n'
     '}\n',
     'fn always_on(_requests: &Requests) -> bool {\n'
     '    true\n'
     '}\n',
     NOT_ADVERTISED,
     '#2294 always_on_apply must not be advertised without an always-on manager'),
    # A fired tick keeps the discovery lock, so t1's lockAfter stays true
    # against the golden's false (the bookkeeping half of the fire path).
    ('fired-tick-keeps-the-lock', scheduler,
     '                if result.outcome == "no_plan" {\n'
     '                    self.ensure_dormancy_watcher();\n'
     '                }\n'
     '                release_discovery_lock(&self.deps.paths);\n',
     '                if result.outcome == "no_plan" {\n'
     '                    self.ensure_dormancy_watcher();\n'
     '                }\n',
     T1_REPLAY,
     '#2294 t1-fired: the full fire path matches the TS baseline'),
    # #2566: the failing watcher start runs before the slot store, so the
    # synchronous on_error -> dispose_watcher() takes from an empty slot and
    # the dead watcher is stored after — the is_some() early return makes
    # the failed start permanent (rearm's slot-empty assert kills it).
    ('watch-start-before-slot-store', scheduler,
     '        *self\n'
     '            .shared\n'
     '            .watcher\n'
     '            .lock()\n'
     '            .unwrap_or_else(|e| e.into_inner()) = Some(watcher.clone());\n'
     '        watcher.start();\n',
     '        watcher.start();\n'
     '        *self\n'
     '            .shared\n'
     '            .watcher\n'
     '            .lock()\n'
     '            .unwrap_or_else(|e| e.into_inner()) = Some(watcher.clone());\n',
     REARM,
     '#2294 rearm: on_error disposes the failed watcher out of the slot'),
]


def check_anchor(row, original):
    if original.count(row[2]) != 1 or row[2] == row[3]:
        raise ValueError(f'{row[0]}: stale or unchanged anchor (count={original.count(row[2])})')


readme = Path(__file__).with_name('README.md')


def check_manifest():
    """Every hand-copied number must equal the artifact it describes.

    The README's group-table counts and `Rows:` list, and the `--exact` test
    name of each mutation, are hand-maintained copies of oracle.json / this
    file. A copy that drifts is invisible until the cell or row it names is
    deleted, so `--check` derives them from the sources (#2488, the
    cron-schedule #1509 P2-2d depth).
    """
    errors = []
    manifest = json.loads(oracle.read_text())
    # 修复轮 r3 P2（boundary）：空 oracle 让下面每一段核对都空洞通过——
    # 总数为零、分组表与 Rows 为空、场景绑定无内容——所以先拒绝空清单
    # 本身，--check 对被清空的 oracle.json 不再空心绿灯。
    if not manifest:
        return ['oracle.json: the scenario manifest is empty']
    counts = {}
    for entry in manifest:
        counts[entry['id'][0]] = counts.get(entry['id'][0], 0) + 1
    text = readme.read_text()
    # The prose total ("the 58 scenario records") and the group table's
    # cells column: each row's backticked tokens name the oracle prefixes
    # it covers (`g*` -> prefix g, `l1` -> prefix l), so the count must
    # equal their summed cells and the covered prefixes must be exactly
    # the oracle's — a deleted group row cannot silently pass.
    total = re.search(r'the (\d+) scenario records', text)
    if not total:
        errors.append('README: no scenario-records total in the oracle paragraph')
    elif int(total.group(1)) != len(manifest):
        errors.append(f'README: says {total.group(1)} scenario records, oracle has {len(manifest)}')
    covered = []
    for line in text.splitlines():
        row = re.match(r'^\|((?:\s*`[^`]+`,?)+)\s*\| (\d+) \|', line)
        if not row:
            continue
        tokens = re.findall(r'`([^`]+)`', row.group(1))
        expected = sum(counts.get(token.rstrip('*')[0], 0) for token in tokens)
        covered.extend(token.rstrip('*')[0] for token in tokens)
        if expected != int(row.group(2)):
            errors.append(f'README: {" ".join(tokens)} says {row.group(2)} cells, oracle has {expected}')
    if sorted(covered) != sorted(counts):
        errors.append(f'README: group table covers {sorted(set(covered))}, oracle has {sorted(counts)}')
    # The list lives in its own paragraph (`Rows: `a`, `b`, …`), so scope the
    # scan to that paragraph: prose elsewhere in the README that mentions the
    # list would otherwise be swept into `listed` and compared as rows.
    blocks = [block for block in re.split(r'\n\s*\n', text) if 'Rows:' in block]
    listed = (re.findall(r'`([a-z0-9-]+)`', blocks[0].split('Rows:', 1)[1])
              if len(blocks) == 1 else [])
    if len(blocks) != 1:
        errors.append(f'README: expected exactly one Rows paragraph, found {len(blocks)}')
    expected_rows = [row[0] for row in mutations]
    if listed != expected_rows:
        errors.append(f"README: Rows list {listed} != mutations {expected_rows}")
    # Every scenario must be bound to a replay: the oracle id quoted in the
    # always-on module or the assembly half that replays it.
    replay = ''.join(p.read_text() for p in sorted(
        (root / 'src/pilotdeck-gateway/src/runtime/always_on').glob('*.rs')))
    replay += automation.read_text()
    for entry in manifest:
        if f'"{entry["id"]}"' not in replay:
            errors.append(f'{entry["id"]}: no bound replay in the always-on sources')
    # 修复轮 r3 P2（simplify）：bind_module_file 显式列出 (prefix, path)，
    # 绑定到唯一文件；未列出的前缀 / 多余 `::` 层级 / 空短名 fail-closed。
    bound_sources = {path: path.read_text() for _, path in bound_modules}
    for row in mutations:
        ident = row[4].rsplit('::', 1)[-1]
        try:
            source, short = bind_module_file(row[4], bound_modules)
            # 双谱系评审 P2（DS+MM simplify 两家撞上）：绑定测试存在性复用共享装置
            # negative_suite.declares_test（#2064 r10 硬化版：注释/字符串先抹白、左
            # 边界、三拼写折叠），不自写裸正则——注释里的 `fn name(` 满足裸正则，
            # 满足不了 declares_test，--check 不再空心通过。
            # #2303（条 4）：declares_test 的 fail-closed 拒绝（ValueError）随
            # bind_module_file 走同一 per-row 收账，不再裸穿成 traceback。
            if not declares_test(bound_sources[source], short):
                errors.append(f'{row[0]}: bound test {short} is not defined in {source.name}')
        except ValueError as error:
            errors.append(f'{row[0]}: {error}')
    return errors


def main():
    args = sys.argv[1:]
    if args == ['--check']:
        errors = []
        for row in mutations:
            try:
                check_anchor(row, row[1].read_text())
            except Exception as error:
                errors.append(str(error))
        errors.extend(check_manifest())
        print('\n'.join(errors) if errors else
              f'#2294 {len(mutations)} unique anchors and fixture manifest checked')
        return int(bool(errors))
    compile_only = '--compile-only' in args
    args = [arg for arg in args if arg != '--compile-only']
    if not args:
        raise SystemExit('provide NEW_OUT and optional mutation names, or --check')
    names = args[1:]
    selected = [row for row in mutations if not names or row[0] in names]
    if not selected or (names and len(selected) != len(names)):
        raise SystemExit('unknown or duplicate mutation')
    env = dict(os.environ)
    if not env.get('CARGO_TARGET_DIR'):
        raise SystemExit('CARGO_TARGET_DIR must be supplied by the caller')
    out = Path(args[0]).resolve()
    out.mkdir(parents=True, exist_ok=False)
    summary = []
    guarded = list(dict.fromkeys([gates, automation, capabilities, scheduler, oracle,
        Path(__file__).resolve(),
        Path(__file__).resolve().parents[1] / 'negative_process.py',
        Path(__file__).resolve().parents[1] / 'negative_suite.py',
        Path(__file__).resolve().parents[1] / 'negative_panic.py']))

    def save(name, text):
        with (out / name).open('x') as stream:
            stream.write(text)

    def run(label, test, *, compile=False):
        slot = wait_for_slot(root / 'scripts/cargo-slots.sh', timeout=600)
        save(label + '.slots.log', slot.stdout + slot.stderr)
        # negative_suite needs libtest's named result lines, including positives.
        command = ['cargo', 'test', '--offline', '-p', 'pilotdeck-gateway', '--lib']
        if compile:
            command.append('--no-run')
        else:
            command += [test, '--', '--exact', '--show-output']
        save(label + '.command.json', json.dumps(command) + '\n')
        try:
            result = run_owned(command, cwd=root / 'src', env=env, timeout=300)
        except Exception as error:
            raw = ''
            if isinstance(error, subprocess.TimeoutExpired):
                raw = ''.join(value.decode(errors='replace') if isinstance(value, bytes)
                              else value or '' for value in (error.stdout, error.stderr))
            save(label + '.log', raw + f'\n{type(error).__name__}: {error}\n')
            raise
        save(label + '.log', result.stdout + result.stderr)
        return result

    for row in selected:
        name, path, old, new, test, assertion = row
        item = dict(name=name, test=test, assertion=assertion,
                    status='INVALID', reason='not executed', compiled=False)
        try:
            # No caller-supplied baseline, dirty-file snapshot, or alternate git.
            with mutating_sources(root, guarded) as originals:
                original = originals[path].decode()
                check_anchor(row, original)
                if not compile_only and not passed(run(name + '-positive', test), test):
                    raise RuntimeError('bound positive control did not pass')
                try:
                    path.write_text(original.replace(old, new, 1))
                    if run(name + '-compile', test, compile=True).returncode != 0:
                        raise RuntimeError('mutant failed compilation; not a runtime kill')
                    item['compiled'] = True
                    if compile_only:
                        item.update(status='COMPILED', reason='cargo test --no-run passed')
                    else:
                        case = Case(name, path, old, new, test, assertion)
                        status, reason = classify(run(name + '-negative', test), case)
                        item.update(status=status, reason=reason)
                finally:
                    # Bytes come only from the HEAD-validated guard; it also
                    # restores and verifies every guarded source on exit.
                    path.write_bytes(originals[path])
                if not passed(run(name + '-restored', test), test):
                    raise RuntimeError('bound restored control did not pass')
        except Exception as error:
            item.update(status='INVALID', reason=f'{type(error).__name__}: {error}')
        summary.append(item)
        print(f"{name}: {item['status']}: {item['reason']}", flush=True)
    save('summary.json', json.dumps(summary, indent=2) + '\n')
    counts = {state: sum(row['status'] == state for row in summary)
              for state in ('KILLED', 'SURVIVED', 'INVALID', 'COMPILED', 'SKIPPED')}
    print('summary: ' + ', '.join(f'{key}={value}' for key, value in counts.items()), flush=True)
    success = 'COMPILED' if compile_only else 'KILLED'
    return int(not summary
               or not counts[success]
               or any(row['status'] not in (success, 'SKIPPED') for row in summary))


if __name__ == '__main__':
    raise SystemExit(main())
