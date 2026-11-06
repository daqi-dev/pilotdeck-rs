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

if not sys.flags.isolated:
    raise SystemExit('run with -I')


def _repo_root():
    """从脚本所在目录向上找含 scripts/parity/g03_compare.py 的仓库根。

    装置同时存在于 `_tmp/`（历史副本）与 `scripts/parity-attrib/`（入仓副本），
    硬编码 /Users/da/ws/pilotdeck-rs 会把两份副本绑死在同一棵树上。
    """
    here = os.path.dirname(os.path.abspath(__file__))
    cur = here
    while True:
        if os.path.isfile(os.path.join(cur, 'scripts', 'parity', 'g03_compare.py')):
            return cur
        parent = os.path.dirname(cur)
        if parent == cur:
            return '/Users/da/ws/pilotdeck-rs'
        cur = parent


REPO = _repo_root()
G03 = f'{REPO}/scripts/parity/g03_compare.py'
SCENARIOS = ('deadline', 'deadline_during_tool')
FAIL_CLOSED = 'result_unknown_fail_closed'
GSF_TEXT = ('AgentLoop terminal outcome is result_unknown and host reconciliation '
            'found no final result.')


def load(path):
    with open(path) as fh:
        return [json.loads(l) for l in fh if l.strip()]


def is_gsf(rec):
    return rec.get('kind') == 'durable.status' and rec.get('event') == 'gateway_submit_failed'


def is_identity(rec):
    return rec.get('kind') == 'harness.proof' and rec.get('state') == 'sidecar_identity'


def g03(left_dir, right_dir, scenario):
    out = subprocess.run([sys.executable, '-I', G03, left_dir, right_dir],
                         capture_output=True, text=True)
    try:
        data = json.loads(out.stdout)
    except ValueError:
        return None, None, f'g03 输出不可解析 rc={out.returncode} {out.stderr[:200]}'
    return data['per_scenario'].get(scenario, {}), data, None


def is_the_gsf_insertion(diff):
    if diff.get('left') is not None:
        return False, 'left 非 null（是替换/改动，不是新增）'
    right = diff.get('right')
    if not isinstance(right, dict):
        return False, 'right 不是记录'
    if right.get('kind') != 'durable.status':
        return False, f'新增记录 kind={right.get("kind")}'
    if right.get('event') != 'gateway_submit_failed':
        return False, f'新增记录 event={right.get("event")}'
    if right.get('statusKind') != 'error':
        return False, f'新增记录 statusKind={right.get("statusKind")}'
    if right.get('text') != GSF_TEXT:
        return False, f'新增记录 text 不是 TS 宿主常量：{right.get("text")!r}'
    return True, 'ok'


def norm(rec):
    """C5 的比较键：抹掉每轮随机的 invocationId、插入导致的 sequence 右移，
    以及 requestEvidence（冻结口径两处都 pop 掉它，见模块 docstring）。"""
    return json.dumps({k: v for k, v in rec.items()
                       if k not in ('invocationId', 'sequence', 'requestEvidence')},
                      ensure_ascii=False, sort_keys=True)


def ev_norm(value):
    """C6 的比较键：规范化后逐字节比较（键序不算差异，值算）。"""
    return json.dumps(value, ensure_ascii=False, sort_keys=True)


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
        left = sorted(ts_idx[key], key=lambda v: ev_norm(v) if v is not None else '')
        right = sorted(ru_idx[key], key=lambda v: ev_norm(v) if v is not None else '')
        for lv, rv in zip(left, right):
            if lv is not None and rv is not None:
                compared += 1
                if ev_norm(lv) != ev_norm(rv):
                    return False, (f'C6 失败：两侧共有的 requestEvidence 内容不同 '
                                   f'({len(ev_norm(lv))} vs {len(ev_norm(rv))} 字节)：'
                                   + (ev_norm(lv)[:300] + ' || ' + ev_norm(rv)[:300]))
            elif (lv is None) != (rv is None):
                residual += 1
    return True, (f'C6 ok：{compared} 条两侧共有的 requestEvidence 规范化后逐字节相同'
                  f'（单侧存在 {residual} 条，属 C5 残余，C6 不覆盖）')


def judge(scenario, ts_dir, rust_dir, ts_recs, rust_recs):
    """返回 (结论, 证据行列表)。结论 ∈ {PASS, ATTRIB_6, NOT_6}。"""
    ev = []
    if scenario not in SCENARIOS:
        return 'NOT_6', [f'C1 失败：场景 {scenario} 不在 #6 登记范围 {SCENARIOS}']
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
    ident = [r for r in rust_recs if is_identity(r)]
    if not (len(ident) == 1 and ident[0].get('side') == 'rust'
            and ident[0].get('sha256') == ident[0].get('expectedSha256')
            and ident[0].get('spawnObserved') is True):
        return 'NOT_6', ['C4 失败：右侧 sidecar 身份自证不成立']
    ev.append(f'C4 ok：右侧 sidecar 身份自证 sha256={ident[0].get("sha256")[:12]}…，spawnObserved=true')
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
    dirs = [p for p in glob.glob(f'{root}/{side}-r*') if os.path.isdir(p)]
    return sorted(dirs, key=lambda p: int(os.path.basename(p).rsplit('-r', 1)[1]))


def run(root, scenario):
    ts_all, ru_all = rounds(root, 'ts'), rounds(root, 'rust')
    trace = f'{scenario}.pilotdeck-current-sidecar.jsonl'
    print(f'# 场景 {scenario}')
    print(f'# rounds = {root}')
    print()
    print('## 每轮 gsf / requestEvidence 计数（原件 trace 直读）')
    print(f'{"round":10s} {"n_frames":>8s} {"gsf":>4s} {"ev":>4s} {"ident":>6s}  durable.status events')
    for side, dirs in (('ts', ts_all), ('rust', ru_all)):
        for d in dirs:
            recs = load(os.path.join(d, 'artifacts', trace))
            st = [f'{r.get("event")}/{r.get("statusKind")}' for r in recs if r.get('kind') == 'durable.status']
            ident = [r for r in recs if is_identity(r)]
            iok = (len(ident) == 1 and ident[0].get('side') == side
                   and ident[0].get('sha256') == ident[0].get('expectedSha256')
                   and ident[0].get('spawnObserved') is True)
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
    if not6:
        print('## 反证命中（不能归因 #6）：')
        for name, ev in bad:
            print(' ', name, ev)
        return 2
    print('## 结论：该场景所有 FAIL 的语义差异集恰好是 #6 那一条，无第二种差异。')
    return 0


# ---------- negctl：用真实 trace 的变异副本证明判据不是空话 ----------

def negctl(root, scenario, outdir):
    trace = f'{scenario}.pilotdeck-current-sidecar.jsonl'
    ts_dir = rounds(root, 'ts')[0]
    ru_dir = rounds(root, 'rust')[0]
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
