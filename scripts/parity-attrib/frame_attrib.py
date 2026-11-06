#!/usr/bin/env python3
"""#1811 选项 A：帧级归因 + 反证判据（只读，不改任何冻结件、不改尺子）。

用法（必须 -I）：
  python3 -I frame_attrib.py <rounds 目录> <场景>
  python3 -I frame_attrib.py <rounds 目录> <场景> --negctl-out <独占新建目录>
  rounds 目录下同时含 ts-r1..ts-rN 与 rust-r1..rust-rN（parity-full 的落盘布局）。

判据（这就是「#6 归因」的可机检形式，逐 pair 判）：
  C1 场景 ∈ {deadline, deadline_during_tool}（accepted-diffs #6 的登记范围）
  C2 冻结比较器 G03 对该 pair 报出的语义差异集 **恰好** 是 1 条：
       left = null，right.kind = durable.status，right.event = gateway_submit_failed，
       right.statusKind = error，right.text = TS 宿主常量
     —— 多一条、少一条、换成别的 event、或 left 非 null（替换而非新增）都不算 #6。
  C3 该 gsf 记录在右侧 trace 里恰好 1 条、在左侧 0 条，且它的序号严格晚于
     harness.proof{state=result_unknown_fail_closed}。
  C4 右侧 sidecar 是 Rust 侧（identity 帧自证），且 sidecar crate 内无 status / 持久化写路径
     （由本脚本外部的源码 grep 提供，见 README 与 attr-grep.txt）。
  C5 删掉右侧那条 gsf 后，两侧帧集合（多重集）必须完全相同。
     **豁免 `requestEvidence`**：对齐冻结口径 `specs/parity/normalization.md:32`
     「G03 与原版严格 comparator 都去掉 requestEvidence 后再比语义」与
     `scripts/parity/g03_compare.py` 的 `pop("requestEvidence")`（两处）。
     残余（README 已写明）：C5 因此**不再**覆盖「单侧多出 requestEvidence」这一类差异。
  C6 两侧同名帧**都存在** `requestEvidence` 时，规范化（sort_keys + 紧凑）后必须逐字节相同。
     单侧存在不在 C6 覆盖范围（那是 C5 的残余，见 README）；两侧都不存在则不适用。

反证（「不是 #6 会长什么样」）：C1–C6 任一不满足即判 NOT_6，脚本以 rc=2 退出。
negctl 子命令用真实 trace 的变异副本证明这些判据不是空话；其中
`L-single-side-requestEvidence` 期望 ATTRIB_6 —— 它证明 C5 的豁免**确实生效**，
是已登记的残余而不是漏判（见 README「C5 残余」）。
"""
import argparse
import glob
import json
import os
import shutil
import subprocess
import sys
from collections import Counter

if not sys.flags.isolated:
    raise SystemExit('run with -I')


def _repo_root():
    """从脚本所在目录向上找含 scripts/parity/g03_compare.py 的仓库根。

    装置同时存在于 `_tmp/`（历史副本）与 `scripts/parity-attrib/`（入仓副本），
    两份副本都在仓库内，向上必能命中。**找不到就报错**：绝不回退到硬编码的机器路径，
    否则被审树与比较器可能来自不同 HEAD，归因结论不可信（#1926 评审 P1）。
    """
    here = os.path.dirname(os.path.abspath(__file__))
    cur = here
    while True:
        if os.path.isfile(os.path.join(cur, 'scripts', 'parity', 'g03_compare.py')):
            return cur
        parent = os.path.dirname(cur)
        if parent == cur:
            raise SystemExit(f'repo root not found: 从 {here} 向上没有 scripts/parity/g03_compare.py')
        cur = parent


REPO = _repo_root()
G03 = f'{REPO}/scripts/parity/g03_compare.py'
SCENARIOS = ('deadline', 'deadline_during_tool')
FAIL_CLOSED = 'result_unknown_fail_closed'
GSF_TEXT = ('AgentLoop terminal outcome is result_unknown and host reconciliation '
            'found no final result.')
# #6 产物那条 gsf 在冻结比较器 channel 差异里的**投影形状**（真实数据实测：G03 的
# `_semantic_record` 只留这几个键，q/scenarioId/invocationId/sequence 都在投影时就被丢掉）。
# C5/C6 都跳过这一帧，所以它的字段集必须单独钉死：多带或少带字段同样不是 #6。
GSF_FIELDS = frozenset({'kind', 'agentScope', 'event', 'statusKind', 'text'})
G03_TIMEOUT = 900  # 秒；冻结比较器单次要跑全部场景，病态输入不许把装置挂死
_G03_CACHE = {}


_LOAD_CACHE = {}


def load(path):
    """读 jsonl trace。同一路径在一个进程里只解析一次（20 轮 → 400 pair 时会重复读同 40 个文件）。"""
    if path not in _LOAD_CACHE:
        with open(path) as fh:
            _LOAD_CACHE[path] = [json.loads(l) for l in fh if l.strip()]
    return _LOAD_CACHE[path]


def is_gsf(rec):
    return rec.get('kind') == 'durable.status' and rec.get('event') == 'gateway_submit_failed'


def is_identity(rec):
    return rec.get('kind') == 'harness.proof' and rec.get('state') == 'sidecar_identity'


def g03(left_dir, right_dir, scenario):
    """调冻结比较器。同一对目录只跑一次（run() 每 pair 要用 G03 列两次，缓存免重复 spawn）。"""
    key = (left_dir, right_dir, scenario)
    if key in _G03_CACHE:
        return _G03_CACHE[key]
    try:
        out = subprocess.run([sys.executable, '-I', G03, left_dir, right_dir],
                             capture_output=True, text=True, timeout=G03_TIMEOUT)
    except subprocess.TimeoutExpired:
        result = (None, None, f'g03 超时（>{G03_TIMEOUT}s）')
        _G03_CACHE[key] = result
        return result
    try:
        data = json.loads(out.stdout)
    except ValueError:
        result = (None, None, f'g03 输出不可解析 rc={out.returncode} {out.stderr[:200]}')
        _G03_CACHE[key] = result
        return result
    result = (data['per_scenario'].get(scenario, {}), data, None)
    _G03_CACHE[key] = result
    return result


def is_the_gsf_insertion(diff):
    if diff.get('left') is not None:
        return False, 'left 非 null（是替换/改动，不是新增）'
    right = diff.get('right')
    if not isinstance(right, dict):
        return False, 'right 不是记录'
    if not is_gsf(right):
        return False, (f'新增记录不是 gsf（kind={right.get("kind")} '
                       f'event={right.get("event")}）')
    if right.get('statusKind') != 'error':
        return False, f'新增记录 statusKind={right.get("statusKind")}'
    if right.get('text') != GSF_TEXT:
        return False, f'新增记录 text 不是 TS 宿主常量：{right.get("text")!r}'
    if right.get('agentScope') != 'parent':
        return False, f'新增记录 agentScope={right.get("agentScope")}（#6 只在 parent 流）'
    # 投影形状必须恰好是 #6 那 5 个键：C5/C6 都会跳过这条 gsf，多带/少带字段只有这里能抓。
    keys = set(right)
    extra, missing = keys - GSF_FIELDS, GSF_FIELDS - keys
    if extra or missing:
        return False, (f'新增记录字段集不是 #6 形状（多 {sorted(extra)} / 少 {sorted(missing)}）')
    return True, 'ok'


def canon(value):
    """与冻结尺子 `g03_compare.py` 的 `canon()` 同口径：sort_keys + 紧凑 + ensure_ascii=False。

    紧凑分隔符是**口径**不是格式偏好：装置若与尺子不同，「逐字节相同」的结论不可比（#1926 评审）。
    """
    return json.dumps(value, sort_keys=True, ensure_ascii=False, separators=(',', ':'))


def norm(rec):
    """C5 的比较键：抹掉每轮随机的 invocationId、插入导致的 sequence 右移，
    以及 requestEvidence（冻结口径两处都 pop 掉它，见模块 docstring）。"""
    return canon({k: v for k, v in rec.items()
                  if k not in ('invocationId', 'sequence', 'requestEvidence')})


def ev_norm(value):
    """C6 的比较键：规范化后逐字节比较（键序不算差异，值算）。"""
    return canon(value)


def identity_ok(recs, side):
    """C4 / 计数表共用：sidecar 身份帧自证成立（恰 1 条、side 对、sha 自洽、spawn 观测到）。"""
    ident = [r for r in recs if is_identity(r)]
    if len(ident) != 1:
        return False, None
    rec = ident[0]
    ok = (rec.get('side') == side
          and isinstance(rec.get('sha256'), str) and rec.get('sha256')
          and rec.get('sha256') == rec.get('expectedSha256')
          and rec.get('spawnObserved') is True)
    return ok, rec


def request_evidence_index(recs, skip=frozenset()):
    """按 C5 比较键归组，记录每帧的 requestEvidence（可能为 None）。"""
    idx = {}
    for i, rec in enumerate(recs):
        if is_identity(rec) or i in skip:
            continue
        idx.setdefault(norm(rec), []).append(rec.get('requestEvidence'))
    return idx


def check_request_evidence(ts_recs, rust_recs, skip_rust=frozenset()):
    """C6：两侧**都存在** requestEvidence 的同名帧，规范化后必须逐字节相同。

    返回 (ok, 证据行)。单侧存在属于 C5 的已登记残余（README），C6 不覆盖、也不算失败。
    同名帧以 C5 的比较键（`norm()`，已抹掉 requestEvidence）归组；同组内按规范化内容排序后
    一一配对，因此同键多帧时也能比较到每一个 requestEvidence。
    """
    ts_idx = request_evidence_index(ts_recs)
    ru_idx = request_evidence_index(rust_recs, skip=skip_rust)
    compared = residual = 0
    for key in ts_idx.keys() & ru_idx.keys():
        # 同键两侧帧数不等说明 C5 的归组前提已被破坏：静默 zip 截断会漏比尾部（#1926 评审）。
        if len(ts_idx[key]) != len(ru_idx[key]):
            return False, (f'C6 失败：同键帧数两侧不等（TS {len(ts_idx[key])} / '
                           f'Rust {len(ru_idx[key])}）：' + key[:300])
        # 只把**两侧都存在**的 requestEvidence 放进多重集比内容；单侧存在计入残余。
        # 不能按「有无」排序后 zip：None 的排序键与内容串不可比，None 位置不同就会错位，
        # 把单侧存在误判成内容不同（#1926 评审 P1）。
        left = Counter(ev_norm(v) for v in ts_idx[key] if v is not None)
        right = Counter(ev_norm(v) for v in ru_idx[key] if v is not None)
        common = left & right
        compared += sum(common.values())
        only_left, only_right = left - common, right - common
        if only_left and only_right:
            return False, ('C6 失败：两侧共有的 requestEvidence 内容不同：'
                           + (sorted(only_left)[0][:300] + ' || ' + sorted(only_right)[0][:300]))
        residual += sum(only_left.values()) + sum(only_right.values())
    return True, (f'C6 ok：{compared} 条两侧共有的 requestEvidence 规范化后逐字节相同'
                  f'（单侧存在 {residual} 条，属 C5 残余，C6 不覆盖）')


def judge(scenario, ts_dir, rust_dir, ts_recs, rust_recs, per=None):
    """返回 (结论, 证据行列表)。结论 ∈ {PASS, ATTRIB_6, NOT_6}。

    `per` 可注入该场景的 G03 结果（`per_scenario[scenario]`）：自测用桩数据覆盖 C1–C6
    的编排层，不必真跑冻结比较器；生产调用传 None，由 `g03()` 现算。
    """
    ev = []
    if scenario not in SCENARIOS:
        return 'NOT_6', [f'C1 失败：场景 {scenario} 不在 #6 登记范围 {SCENARIOS}']
    if per is None:
        per, _, err = g03(ts_dir, rust_dir, scenario)
        if per is None:
            return 'NOT_6', [f'C2 失败：{err}']
    diffs = per.get('channel', {}).get('differences') or []
    verdict = per.get('verdict')
    rules = sorted(per.get('triggered_rules') or [])
    if verdict == 'PASS':
        return 'PASS', [f'C2：G03 判 PASS，语义差异 0 条']
    # C2a：规则集必须恰好是 #6 的那三条。#6 只会多一条 durable.status，
    #      因此任何别的规则（E6 终态不符、trace_health、E9 settlement…）都是另一种缺陷。
    if rules != ['E10', 'E11', 'G03_channels']:
        return 'NOT_6', [f'C2a 失败：triggered_rules={rules}，不是 #6 的 [E10,E11,G03_channels]']
    if len(diffs) != 1:
        return 'NOT_6', [f'C2b 失败：G03 语义差异 {len(diffs)} 条（#6 只允许 1 条）：'
                         + json.dumps(diffs, ensure_ascii=False)[:600]]
    ok, why = is_the_gsf_insertion(diffs[0])
    if not ok:
        return 'NOT_6', [f'C2b 失败：唯一差异不是 gsf 新增 —— {why}', json.dumps(diffs[0], ensure_ascii=False)]
    ev.append('C2a ok：triggered_rules 恰好 [E10,E11,G03_channels]')
    ev.append('C2b ok：G03 语义差异恰好 1 条 = 右侧新增 gateway_submit_failed（left=null）')
    ts_gsf = [i for i, r in enumerate(ts_recs) if is_gsf(r)]
    ru_gsf = [i for i, r in enumerate(rust_recs) if is_gsf(r)]
    if len(ru_gsf) != 1 or len(ts_gsf) != 0:
        return 'NOT_6', [f'C3 失败：gsf 计数 ts={len(ts_gsf)} rust={len(ru_gsf)}（要求 0/1）']
    fc = next((i for i, r in enumerate(rust_recs)
               if r.get('kind') == 'harness.proof' and r.get('state') == FAIL_CLOSED), None)
    if fc is None:
        return 'NOT_6', ['C3 失败：右侧 trace 里没有 result_unknown_fail_closed 帧']
    if ru_gsf[0] <= fc:
        return 'NOT_6', [f'C3 失败：gsf 序号 {ru_gsf[0]} 不晚于 fail_closed 帧 {fc}（分叉前移）']
    ev.append(f'C3 ok：gsf 计数 ts=0 rust=1，位于第 {ru_gsf[0]} 帧，晚于 fail_closed 帧 {fc}')
    ident_ok, ident = identity_ok(rust_recs, 'rust')
    if not ident_ok:
        return 'NOT_6', ['C4 失败：右侧 sidecar 身份自证不成立']
    ev.append(f'C4 ok：右侧 sidecar 身份自证 sha256={str(ident.get("sha256"))[:12]}…，spawnObserved=true')
    # C5：把右侧那一条 gsf 删掉后，两侧的帧**集合**必须完全相同（逐条多重集相等）。
    #     抹掉 invocationId（每轮随机 UUID，G03 自己也归一化）、sequence（插入后整段右移，
    #     顺序不变性已由 C2a 的 G03 规则与 C3 的插入位置覆盖），以及 requestEvidence
    #     （冻结口径 normalization.md:32 与 g03_compare.py 两处都 pop 掉它，见 #1926）。
    #     用集合而不是逐位，是因为 #1841 的 context.budget 相对顺序偏离已被 G03 判为可接受；
    #     逐位比对会把它误报成「不是 #6」，那是把装置口径问题混进产品归因。
    ts_cmp = sorted(norm(r) for r in ts_recs if not is_identity(r))
    ru_cmp = sorted(norm(r) for i, r in enumerate(rust_recs) if not is_identity(r) and i != ru_gsf[0])
    if ts_cmp != ru_cmp:
        only_ts = [x for x in ts_cmp if x not in ru_cmp]
        only_ru = [x for x in ru_cmp if x not in ts_cmp]
        return 'NOT_6', [f'C5 失败：删掉那条 gsf 后两侧帧集合仍不等 '
                         f'(仅 TS {len(only_ts)} 条 / 仅 Rust {len(only_ru)} 条)：'
                         + (only_ts[:1] + only_ru[:1]).__str__()[:500]]
    ev.append(f'C5 ok：删掉右侧第 {ru_gsf[0]} 帧后，两侧各 {len(ts_cmp)} 帧的集合完全相同'
              '（口径已豁免 requestEvidence）')
    # C6：两侧**都存在** requestEvidence 的同名帧，规范化后必须逐字节相同。
    #     单侧存在属于 C5 的已登记残余（README），C6 不覆盖、也不算失败。
    c6_ok, c6_msg = check_request_evidence(ts_recs, rust_recs, skip_rust=frozenset({ru_gsf[0]}))
    if not c6_ok:
        return 'NOT_6', [c6_msg]
    ev.append(c6_msg)
    return 'ATTRIB_6', ev


def rounds(root, side):
    """`<root>/<side>-r<N>` 轮次目录，按 N 升序。目录名不合规即报错（不静默跳过）。"""
    dirs = [p for p in glob.glob(f'{root}/{side}-r*') if os.path.isdir(p)]
    bad = [p for p in dirs if not os.path.basename(p).rsplit('-r', 1)[1].isdigit()]
    if bad:
        raise SystemExit(f'轮次目录名不合规（应为 {side}-r<N>）：{sorted(bad)}')
    return sorted(dirs, key=lambda p: int(os.path.basename(p).rsplit('-r', 1)[1]))


def round_problem(d, side):
    """轮次有效性（与冻结 `scripts/parity/score.py:146-158` 同口径）。

    返回 None 表示有效，否则返回原因。没有这道闸门时：被 BLOCKED / 只跑了半截的轮次会被
    G03 追加 `trace_identity` 规则 → 判成 NOT_6（把「轮次无效」误报成「不是 #6」），或者
    直接因 trace 缺失/截断抛异常中断整个分析（#1926 评审 P1）。
    """
    try:
        with open(os.path.join(d, 'run.json')) as fh:
            ledger = json.load(fh)
    except (OSError, ValueError) as exc:
        return f'run.json 不可读：{exc}'
    if ledger.get('side') != side:
        return f'run.json side={ledger.get("side")!r}，不是 {side!r}'
    if ledger.get('valid_round') is not True:
        return 'run.json valid_round 不为 true'
    if ledger.get('identity_ok') is not True:
        return 'run.json identity_ok 不为 true'
    sp = os.path.join(d, 'artifacts', 'summary.json')
    if os.path.isfile(sp):
        try:
            with open(sp) as fh:
                summary = json.load(fh)
        except (OSError, ValueError) as exc:
            return f'summary.json 不可读：{exc}'
        if summary.get('blocked'):
            return f'summary.json blocked={summary.get("blocked")!r}'
    return None


def collect_rounds(root, side, trace):
    """筛出有效轮次；无效轮次逐条列原因，不静默参与判定。"""
    good, bad = [], []
    for d in rounds(root, side):
        why = round_problem(d, side)
        if why is None:
            try:
                load(os.path.join(d, 'artifacts', trace))
            except (OSError, ValueError) as exc:
                why = f'trace 不可读：{exc}'
        (bad.append((os.path.basename(d), why)) if why else good.append(d))
    return good, bad


def run(root, scenario):
    trace = f'{scenario}.pilotdeck-current-sidecar.jsonl'
    ts_all, ts_bad = collect_rounds(root, 'ts', trace)
    ru_all, ru_bad = collect_rounds(root, 'rust', trace)
    # 空对拍必须**硬失败**：0 对时循环不执行、total=0，旧版会打印「所有 FAIL 都可归因」
    # 并以 rc=0 退出 —— 那是尺子假 PASS（#1926 评审 P0）。
    if not ts_all or not ru_all:
        print(f'## 失败：有效轮次不足（ts {len(ts_all)} 轮 / rust {len(ru_all)} 轮），无法归因：{root}',
              file=sys.stderr)
        for side, bad in (('ts', ts_bad), ('rust', ru_bad)):
            for name, why in bad:
                print(f'   {side}/{name}: {why}', file=sys.stderr)
        return 5
    print(f'# 场景 {scenario}')
    print(f'# rounds = {root}')
    print()
    print('## 每轮 gsf / requestEvidence 计数（原件 trace 直读）')
    print(f'{"round":10s} {"n_frames":>8s} {"gsf":>4s} {"ev":>4s} {"ident":>6s}  durable.status events')
    for side, dirs in (('ts', ts_all), ('rust', ru_all)):
        for d in dirs:
            recs = load(os.path.join(d, 'artifacts', trace))
            st = [f'{r.get("event")}/{r.get("statusKind")}' for r in recs if r.get('kind') == 'durable.status']
            iok = identity_ok(recs, side)[0]
            nev = sum(1 for r in recs if r.get('requestEvidence') is not None)
            print(f'{os.path.basename(d):10s} {len(recs):8d} {sum(1 for r in recs if is_gsf(r)):4d} '
                  f'{nev:4d} {str(iok):>6s}  {st}')
    print()
    print('## 逐 pair 判定')
    print(f'{"pair":16s} {"G03":6s} {"判定":9s} 证据')
    npass = n6 = not6 = 0
    bad = []
    for td in ts_all:
        for rd in ru_all:
            ts_recs = load(os.path.join(td, 'artifacts', trace))
            ru_recs = load(os.path.join(rd, 'artifacts', trace))
            verdict, ev = judge(scenario, td, rd, ts_recs, ru_recs)
            name = f'{os.path.basename(td)} v {os.path.basename(rd)}'
            per, _, _ = g03(td, rd, scenario)
            g = (per or {}).get('verdict')
            print(f'{name:16s} {str(g):6s} {verdict:9s} {ev[0]}')
            for line in ev[1:]:
                print(f'{"":16s} {"":6s} {"":9s} {line}')
            if verdict == 'PASS':
                npass += 1
            elif verdict == 'ATTRIB_6':
                n6 += 1
            else:
                not6 += 1
                bad.append((name, ev))
    total = npass + n6 + not6
    print()
    print(f'## 汇总：{total} pair 中 PASS={npass} · 归因 #6={n6} · 不能归因={not6}')
    if total == 0:  # 兜底：对拍为空时绝不打印「全部可归因」（尺子假 PASS）
        print('## 失败：0 pair 被判，拒绝给出结论', file=sys.stderr)
        return 5
    if not6:
        print('## 反证命中（不能归因 #6）：')
        for name, ev in bad:
            print(' ', name, ev)
        return 2
    if ts_bad or ru_bad:
        # 批次不完整：结论只覆盖有效轮次，不得作为验收证据（fail-closed，不静默通过）。
        print(f'## 警告：{len(ts_bad) + len(ru_bad)} 个轮次无效，已排除：')
        for side, bad_rounds in (('ts', ts_bad), ('rust', ru_bad)):
            for name, why in bad_rounds:
                print(f'   {side}/{name}: {why}')
        print('## 结论仅覆盖上面列出的有效轮次；含无效轮次的批次不算验收证据。')
        return 6
    print('## 结论：该场景所有 FAIL 的语义差异集恰好是 #6 那一条，无第二种差异。')
    return 0


# ---------- negctl：用真实 trace 的变异副本证明判据不是空话 ----------

def negctl(root, scenario, outdir):
    trace = f'{scenario}.pilotdeck-current-sidecar.jsonl'
    # 证据目录独占新建：README 已声明「不得覆写」，代码必须同样设防，否则并发重跑会
    # rmtree 掉另一个进程正在读的用例（假 MISMATCH），历史证据也被覆盖（#1926 评审 P2）。
    if os.path.exists(outdir):
        raise SystemExit(f'--negctl-out 必须独占新建：{outdir} 已存在（不覆盖证据）')
    ts_dirs, ru_dirs = rounds(root, 'ts'), rounds(root, 'rust')
    if not ts_dirs or not ru_dirs:
        print(f'negctl 需要至少一对轮次目录，{root} 下 ts={len(ts_dirs)} rust={len(ru_dirs)}')
        return 3
    ts_dir = ts_dirs[0]
    ru_dir = ru_dirs[0]
    base_ts = load(os.path.join(ts_dir, 'artifacts', trace))
    base_ru = load(os.path.join(ru_dir, 'artifacts', trace))
    if not any(is_gsf(r) for r in base_ru):
        print(f'negctl 需要一对 FAIL（含 gsf）作基线，{ru_dir} 没有 gsf')
        return 3

    def write_case(name, ts_recs, ru_recs):
        d = os.path.join(outdir, name)
        for side, recs, src in (('ts', ts_recs, ts_dir), ('rust', ru_recs, ru_dir)):
            dst = os.path.join(d, f'{side}-r1', 'artifacts')
            if os.path.isdir(os.path.join(d, f'{side}-r1')):
                shutil.rmtree(os.path.join(d, f'{side}-r1'))
            shutil.copytree(src, os.path.join(d, f'{side}-r1'))
            with open(os.path.join(dst, trace), 'w') as fh:
                for rec in recs:
                    fh.write(json.dumps(rec, ensure_ascii=False) + '\n')
        return os.path.join(d, 'ts-r1'), os.path.join(d, 'rust-r1')

    cases = []
    # A 基线：原样 → 应判 ATTRIB_6
    cases.append(('A-baseline-attrib6', base_ts, base_ru, 'ATTRIB_6'))
    # B 换成别的 event 名 → NOT_6
    ru_b = [dict(r) for r in base_ru]
    for r in ru_b:
        if is_gsf(r):
            r['event'] = 'gateway_submit_aborted'
    cases.append(('B-event-renamed', base_ts, ru_b, 'NOT_6'))
    # C 多写一条 → NOT_6
    ru_c = list(base_ru)
    idx = next(i for i, r in enumerate(ru_c) if is_gsf(r))
    ru_c.insert(idx + 1, ru_c[idx])
    cases.append(('C-duplicated', base_ts, ru_c, 'NOT_6'))
    # D 分叉前移：把 gsf 挪到 fail_closed 之前 → NOT_6
    ru_d = [dict(r) for r in base_ru]
    g = next(i for i, r in enumerate(ru_d) if is_gsf(r))
    fc = next(i for i, r in enumerate(ru_d) if r.get('kind') == 'harness.proof' and r.get('state') == FAIL_CLOSED)
    rec = ru_d.pop(g)
    ru_d.insert(fc, rec)
    cases.append(('D-moved-before-fail-closed', base_ts, ru_d, 'NOT_6'))
    # E 替换型：把右侧终态 outcome 改掉（left 非 null）→ NOT_6
    ru_e = [dict(r) for r in base_ru]
    for r in ru_e:
        if r.get('kind') == 'operation.terminal' and r.get('outcome') == 'result_unknown':
            r['outcome'] = 'failed'
            break
    cases.append(('E-terminal-substituted', base_ts, ru_e, 'NOT_6'))
    # F 场景越界：把场景名换成未登记场景 → NOT_6
    cases.append(('F-scenario-out-of-scope', base_ts, base_ru, 'NOT_6', 'sidecar_budget_limit'))
    # G 载荷篡改：event 名对但 text 换成别的话 → NOT_6
    ru_g = [dict(r) for r in base_ru]
    for r in ru_g:
        if is_gsf(r):
            r['text'] = 'AgentLoop terminal outcome is result_unknown.'
    cases.append(('G-text-tampered', base_ts, ru_g, 'NOT_6'))
    # H 静默少一帧（G03 的偏序比较器看不见的那种）→ 只有 C5 能抓 → NOT_6
    ru_h = [r for r in base_ru if r.get('kind') != 'side_effect.state']
    cases.append(('H-frame-dropped', base_ts, ru_h, 'NOT_6'))
    # I 与 #6 无关地多一帧（重复一条 model.request）→ C5 抓 → NOT_6
    ru_i = list(base_ru)
    mi = next(i for i, r in enumerate(ru_i) if r.get('kind') == 'model.request')
    ru_i.insert(mi + 1, dict(ru_i[mi]))
    cases.append(('I-unrelated-extra-frame', base_ts, ru_i, 'NOT_6'))
    # J/K 冻结比较器看不见的字段改动（偏序规则不覆盖），只能靠 C5 的集合比对抓
    def mutate_field(recs, kind, field, val):
        out = [dict(r) for r in recs]
        for r in out:
            if r.get('kind') == kind and field in r:
                r[field] = val
                return out
        raise SystemExit(f'negctl 变异点不存在：{kind}.{field}')

    cases.append(('J-field-q-changed', base_ts,
                  mutate_field(base_ru, 'terminal', 'q', 'X'), 'NOT_6'))
    cases.append(('K-lastApplied-changed', base_ts,
                  mutate_field(base_ru, 'operation.terminal', 'lastAppliedSequence', 99), 'NOT_6'))

    # L/M 绑定 #1926 的新增 C5 豁免与 C6。
    def put_ev(recs, value):
        out = [dict(r) for r in recs]
        for r in out:
            if r.get('kind') == 'context.budget':
                r['requestEvidence'] = value
                return out
        raise SystemExit('negctl 变异点不存在：context.budget（需要带该帧的基线）')

    # L 单侧多出 requestEvidence：C5 已豁免 → 期望 ATTRIB_6。
    #   这是**故意保留的残余**（README「C5 残余」），负控的价值在于盯住豁免真的生效，
    #   而不是证明装置能抓它；若哪天它变回 NOT_6，说明口径又被改严了，需同步文档。
    cases.append(('L-single-side-requestEvidence', base_ts,
                  put_ev(base_ru, {'accountingContract': 'negctl/v1', 'total': 1}), 'ATTRIB_6'))
    # M 双侧都有但内容不同：C6 必须抓到 → NOT_6
    cases.append(('M-requestEvidence-content-differs',
                  put_ev(base_ts, {'accountingContract': 'negctl/v1', 'total': 1}),
                  put_ev(base_ru, {'accountingContract': 'negctl/v1', 'total': 2}), 'NOT_6'))

    ok = True
    for case in cases:
        name, ts_recs, ru_recs, want = case[0], case[1], case[2], case[3]
        sc = case[4] if len(case) > 4 else scenario
        td, rd = write_case(name, ts_recs, ru_recs)
        got, ev = judge(sc, td, rd, ts_recs, ru_recs)
        mark = 'OK' if got == want else 'MISMATCH'
        if got != want:
            ok = False
        print(f'negctl {name:32s} 期望={want:9s} 实得={got:9s} {mark}  {ev[0][:110]}')
    return 0 if ok else 4


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('root')
    ap.add_argument('scenario')
    ap.add_argument('--negctl-out', default=None)
    args = ap.parse_args()
    if args.negctl_out:
        return negctl(args.root, args.scenario, args.negctl_out)
    return run(args.root, args.scenario)


if __name__ == '__main__':
    sys.exit(main())
