#!/usr/bin/env python3
"""#2378 负控：retirement-outcome-parity 负控汇总的静态名册 / 分母断言。

Mutates the pure-Python ruler (scripts/retirement-outcome-parity/) in place
under ``mutating_sources``; every mutant must make the executor either refuse
with the #2378 roster message (rc=2, no summary published) or fold the
out-of-vocabulary verdict to INVALID and FAIL (rc=1). The unmutated baseline —
a full real-data round over the committed fixtures — must PASS first; a red
baseline makes every mutant INVALID, mirroring the apparatus's own preflight
(未变异的参考轮必须本来 PASS，否则无法归因).

No cargo is involved anywhere in this suite: the mutation targets are the
apparatus python files, and the measured command is ``python3 -I``. The shared
slot helpers (``run_owned`` / ``wait_for_slot``) therefore do not apply; only
the shared source-mutation guard is used. Post-commit run only: the fix this
suite guards must be at HEAD, because ``mutating_sources`` byte-compares the
targets against HEAD and refuses a dirty tree.
"""
import json
import signal
import subprocess
import sys
import tempfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import defer_signal_exit, mutating_sources
from negative_selection import claim_dir, refuse, registered, select, verdict_exit


def terminate_cleanly(signum, _frame):
    """Unwind on SIGTERM/SIGHUP/SIGQUIT so the finally blocks restore the
    sources; the default disposition would leave a mutant in the tree and
    every later fixture refusing to run (r3 concurrency#1, mirror of
    same-json-surfaces' form)."""
    if defer_signal_exit(signum):
        return
    raise SystemExit(128 + signum)


def install_terminate_handlers():
    signal.signal(signal.SIGTERM, terminate_cleanly)
    if hasattr(signal, 'SIGHUP'):
        signal.signal(signal.SIGHUP, terminate_cleanly)
    if hasattr(signal, 'SIGQUIT'):
        signal.signal(signal.SIGQUIT, terminate_cleanly)

# 一轮全量负控（preflight + 17 负控 + 0 正控，每个都是 python3 -I 子进程）的
# 外层上限，对齐 self_check 的 NEGCTL_TIMEOUT_SECONDS 语义（内部最坏和的封顶）。
ROUND_TIMEOUT_SECONDS = 1800

root = Path(__file__).resolve().parents[5]
apparatus = root / 'scripts' / 'retirement-outcome-parity'

# 每个变异体独立作用于原始字节（先还原再下一个），不累积。
# 判杀标准绑定 #2378 守卫的专属输出（refuse 文案 / 折叠 reason），不拿任意
# 非零 rc 充数——rc=2 也可能是「输入不可用」，必须命中名册拒绝的原文。
# 登记值适配 main 现状（rebase 后）：负控 N1–N17、正控空集——A8.6-R1 的 PC1–PC2
# 与 N18 已随 #2294 按反转条款撤回（A8.6-…-R2），正控侧变异改为「名册虚增」。
mutations = [
    ('baseline', None, None, None),
    # 名册删一条（负控侧）：EXPECTED_CONTROL_IDS 少 N17 → 执行器仍构造 17 条，
    # 名册比对不等 → rc=2「拒绝而非缩小」，不发布 summary。
    ('roster-drop-n17', apparatus / 'check_outcome_parity.py',
     "'N11', 'N12', 'N13', 'N14', 'N15', 'N16', 'N17'))",
     "'N11', 'N12', 'N13', 'N14', 'N15', 'N16'))"),
    # 名册虚增一条（正控侧）：EXPECTED_POSITIVE_IDS 凭空加 PC1 → 执行器实际
    # 构造 0 个正控，名册比对不等 → 同一守卫 rc=2。
    ('roster-extra-pc1', apparatus / 'check_outcome_parity.py',
     "EXPECTED_POSITIVE_IDS = frozenset()",
     "EXPECTED_POSITIVE_IDS = frozenset(('PC1',))"),
    # 条数守卫的盲区：N5 改名成 N4（一重复一缺失，条数不变 17）→ 只有 ID 名册
    # 比对能拒——#2378 正文点名的假 PASS 形态。
    ('roster-rename-n5', apparatus / 'negative_controls.py',
     "Control('N5', ", "Control('N4', "),
    # 词表外判决：run_control 记 'MISSED' → 汇总前必须折叠成 INVALID（reason 保留
    # 原判决可审计）、counts 整字典不等期望值 → status FAIL rc=1。没有折叠时
    # 'MISSED' 不进任何键，「两个零」空真判 PASS（issue 正文现象）。
    ('verdict-out-of-vocabulary', apparatus / 'negative_controls.py',
     "verdict='KILLED', reason='expected rule fired naming the injected method'",
     "verdict='MISSED', reason='expected rule fired naming the injected method'"),
]
# #1666/#2378 静态名册：len(mutations) 不是分母，删行必须让本执行器自己拒跑
# （negative_selection.registered 与网关家族同源）。
EXPECTED_NAMES = (
    'baseline', 'roster-drop-n17', 'roster-extra-pc1',
    'roster-rename-n5', 'verdict-out-of-vocabulary',
)
try:
    registered(mutations, EXPECTED_NAMES)
    run_rows = select(mutations, sys.argv[2:])
except ValueError as error:
    refuse(str(error))
out = claim_dir(sys.argv[1])
runner = apparatus / 'negative_controls.py'


def summary_state(path):
    """「内容 + 文件是否发布」的观测（返回序对齐 judge 签名：text 在前），真实
    循环与合成自检共用一份（r5 boundary#1 / sibling#2）：只测 judge 分支而绕过
    这里的取数接线，`is_file()` 被改回内容真值（0 字节已发布被当「未发布」）时
    合成自检照绿。已发布的 0 字节文件返回 ('', True)——发布即证据，不看内容真值。
    """
    published = path.is_file()
    return (path.read_text(encoding='utf-8') if published else None), published


def judge(name, rc, log, summary_text, summary_published):
    """单条判杀：全部绑定到 #2378 守卫的专属可观测输出。"""
    if name == 'baseline':
        # 正控：未变异的一轮必须照旧 PASS（尺子改动要真实数据复算）。
        # 正控注册数现为 0（A8.6-R2 撤回），positiveCounts 仍为三键全 0 的整字典。
        if rc == 0 and summary_text:
            summary = json.loads(summary_text)
            if (summary.get('status') == 'PASS'
                    and summary.get('counts') == {'KILLED': 17, 'SURVIVED': 0, 'INVALID': 0}
                    and summary.get('positiveCounts') == {'HELD': 0, 'BROKEN': 0, 'INVALID': 0}):
                return 'KILLED', 'unmutated full round still PASSes with exact counts'
        return 'SURVIVED', 'baseline full round must PASS before mutants count'
    if name in ('roster-drop-n17', 'roster-extra-pc1', 'roster-rename-n5'):
        # 三条名册变异都必须被「拒绝而非缩小」：rc=2 + 名册拒绝原文 + 名册侧
        # 涉及的那个 id 出现在两侧清单里（防止任意 rc=2 冒充判杀）。
        named = {'roster-drop-n17': 'N17', 'roster-extra-pc1': 'PC1',
                 'roster-rename-n5': 'N5'}[name]
        side = ('assembled positive ids' if name == 'roster-extra-pc1'
                else 'assembled control ids')
        if (rc == 2 and side in log and 'do not match the registered roster' in log
                and named in log and not summary_published):
            return 'KILLED', '#2378 roster refusal fired (rc=2, no summary published)'
        if summary_published:
            # 「拒绝＝不留下看起来像跑完的证据」是判杀契约的一半（r3 boundary#1）。
            # 判据用「文件是否发布」（r4 boundary#1），不用内容真值——已发布的
            # 0 字节 summary.json 也是跑过的证据，`not summary_text` 会放过它。
            return 'SURVIVED', (f'roster refusal published a summary anyway '
                                f'(refusal must leave no evidence of a run)')
        return 'SURVIVED', (f'expected rc=2 with the #2378 roster refusal naming '
                            f'{named}, got rc={rc}')
    # verdict-out-of-vocabulary：17 条全折叠 INVALID → 整字典不等 → FAIL rc=1。
    if rc != 1:
        return 'SURVIVED', f'expected rc=1 (folded INVALID fails the exact counts), got rc={rc}'
    if not summary_text:
        return 'SURVIVED', 'no summary published for the FAIL run'
    summary = json.loads(summary_text)
    if summary.get('status') != 'FAIL':
        return 'SURVIVED', f"status {summary.get('status')!r} must be FAIL"
    if summary.get('counts') != {'KILLED': 0, 'SURVIVED': 0, 'INVALID': 17}:
        return 'SURVIVED', f"unexpected counts: {summary.get('counts')}"
    reasons = [entry.get('reason') or '' for entry in summary.get('controls') or []]
    if len(reasons) != 17 or not all("outside the vocabulary" in reason
                                     and "'MISSED'" in reason for reason in reasons):
        return 'SURVIVED', 'every folded record must name the foreign verdict'
    return 'KILLED', 'out-of-vocabulary verdicts folded to INVALID, status FAIL'


rows = []
install_terminate_handlers()
# 运行时 pin（r4 sibling#2）：处理器装上才算数——删掉上面的调用点，getsignal
# 仍是默认处置，这里直接红（same-json-surfaces 三件套的等价物：那边是源码
# 计数断言 + 自变异，本夹具调用点 0 缩进锚点难写，直接验生效更强）。
# 信号常量直接引用（CI 修复轮）：getattr(signal, name) 会被锚点自检按
# 「unsupported snapshot dynamic write: getattr」拒收（anchor_selfcheck.py
# 快照禁用符号表），本夹具只在 POSIX 跑，SIGHUP/SIGQUIT 恒存在、无需容缺。
for _sig_name, _sig in (('SIGTERM', signal.SIGTERM),
                        ('SIGHUP', signal.SIGHUP),
                        ('SIGQUIT', signal.SIGQUIT)):
    if signal.getsignal(_sig) is not terminate_cleanly:
        print(f'self-check: terminate handler for {_sig_name} not installed',
              file=sys.stderr)
        raise SystemExit(2)
# judge 合成自检（r4 boundary#2 + sibling#3、r5 boundary#1/sibling#2）：「拒绝
# 不得留下跑完的证据」分支不能只靠读码论证，且判据输入必须走真实取数接线
# summary_state（在临时目录里摆出三形：不存在 / 0 字节 / JSON）——只测 judge
# 分支而绕过接线，`is_file()` 被改回内容真值时这里照绿。三形钉红绿两侧：
# 未发布 → KILLED；已发布（0 字节与 JSON 两形）→ SURVIVED。
_refusal_log = ('negative_controls: assembled control ids [...] do not match '
                'the registered roster [...] (#2378); refusing to summarize '
                'an unexpected control set (N5)')
with tempfile.TemporaryDirectory(prefix='negctl-roster-selfcheck-') as _tmp:
    _tmp = Path(_tmp)
    _missing = summary_state(_tmp / 'absent-summary.json')
    (_tmp / 'empty-summary.json').write_bytes(b'')
    _empty = summary_state(_tmp / 'empty-summary.json')
    (_tmp / 'json-summary.json').write_text('{"status": "PASS"}', encoding='utf-8')
    _json = summary_state(_tmp / 'json-summary.json')
    if _missing != (None, False) or _empty != ('', True) or _json[1] is not True:
        print(f'self-check: summary_state wrong: missing={_missing!r} '
              f'empty={_empty!r} json={_json!r}', file=sys.stderr)
        raise SystemExit(2)
    if judge('roster-rename-n5', 2, _refusal_log, *_missing)[0] != 'KILLED':
        print('self-check: a clean refusal must judge KILLED', file=sys.stderr)
        raise SystemExit(2)
    for _state in (_empty, _json):
        if judge('roster-rename-n5', 2, _refusal_log, *_state)[0] != 'SURVIVED':
            print('self-check: a refusal that published a summary must judge '
                  'SURVIVED', file=sys.stderr)
            raise SystemExit(2)
# 词表同源 pin（r5 sibling#1）：单一登记点的两个消费者若改回字面量副本（即便
# 暂同值）当时无红、要到最长 1800 s 的负控轮后才可能暴露——这里源码计数
# `= CHECK.NEGATIVE_VERDICTS` 别名恰一处，改回字面量即计数归零直接红。
for _pinned in (apparatus / 'negative_controls.py', apparatus / 'self_check.py'):
    _src = _pinned.read_text(encoding='utf-8')
    for _vocab in ('NEGATIVE_VERDICTS', 'POSITIVE_VERDICTS'):
        if _src.count(f'{_vocab} = CHECK.{_vocab}') != 1:
            print(f'self-check: {_pinned.name} lost the CHECK-sourced '
                  f'{_vocab} alias (exactly one expected)', file=sys.stderr)
            raise SystemExit(2)
with mutating_sources(root, sorted({path for _name, path, _old, _new in run_rows
                                    if path is not None})) as snapshots:
    # 锚点过期必须报错退出，绝不记成 SURVIVED：变异前先核每个锚在当前源里恰一次。
    stale = [(name, snapshots[path].decode().count(old))
             for name, path, old, _new in run_rows if path is not None
             if snapshots[path].decode().count(old) != 1]
    if stale:
        for name, count in stale:
            print(f'STALE ANCHOR {name}: {count} matches', file=sys.stderr)
        raise SystemExit(2)
    for name, path, old, new in run_rows:
        out_dir = out / name
        out_dir.mkdir()
        try:
            if path is not None:
                path.write_text(snapshots[path].decode().replace(old, new, 1),
                                encoding='utf-8', newline='')
            result = subprocess.run(
                [sys.executable, '-I', str(runner), '--output', str(out_dir)],
                capture_output=True, text=True, timeout=ROUND_TIMEOUT_SECONDS,
                start_new_session=True)
            rc, log = result.returncode, result.stdout + result.stderr
            summary_text, summary_published = summary_state(out_dir / 'summary.json')
            status, reason = judge(name, rc, log, summary_text, summary_published)
        except subprocess.TimeoutExpired:
            rc, log, status, reason = (None, '', 'INVALID',
                                       f'round timed out after {ROUND_TIMEOUT_SECONDS}s')
        except Exception as error:  # noqa: BLE001 - 记录为 INVALID，不中止其余变异
            rc, log, status, reason = (None, '', 'INVALID',
                                       f'{type(error).__name__}: {error}')
        finally:
            if path is not None:
                path.write_bytes(snapshots[path])
        rows.append(dict(name=name, status=status, reason=reason, rc=rc))
        (out / f'{name}.log').write_text(log + f'\nexit={rc}\n', encoding='utf-8')
        (out / 'summary.json').write_text(
            json.dumps(rows, ensure_ascii=False, indent=2) + '\n', encoding='utf-8')
        print(f'{name}: {status} ({reason})', flush=True)
killed = sum(1 for row in rows if row['status'] == 'KILLED')
print(f'{killed}/{len(run_rows)} killed; every source restored byte-for-byte', flush=True)
sys.exit(verdict_exit(rows, key='status'))
