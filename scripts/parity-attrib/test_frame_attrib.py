#!/usr/bin/env python3
"""`frame_attrib.py` 的最小自测（不依赖任何 trace 数据，纯合成记录）。

用法：
  python3 -I test_frame_attrib.py     # 退出码 0 = 全过；非 0 = 有用例失败

只覆盖判据里**纯函数**的部分（C2b 的插入形态与字段集、C5 的 norm 口径、C6 的
requestEvidence 比较、C4 的 identity_ok、rounds() 的目录校验），外加装置自身的 sha256
pin。C1–C4 的真实数据判定与 13 条 negctl 不在这里，见 README「验证」。
"""
import contextlib
import hashlib
import importlib.util
import io
import json
import os
import sys
import tempfile

if not sys.flags.isolated:
    raise SystemExit('run with -I')

HERE = os.path.dirname(os.path.abspath(__file__))
spec = importlib.util.spec_from_file_location('frame_attrib', os.path.join(HERE, 'frame_attrib.py'))
fa = importlib.util.module_from_spec(spec)
spec.loader.exec_module(fa)

FAILS = []


def check(name, cond, detail=''):
    if cond:
        print(f'ok   {name}')
    else:
        print(f'FAIL {name} {detail}')
        FAILS.append(name)


def budget(**extra):
    rec = {'kind': 'context.budget', 'scenarioId': 'deadline', 'q': 'Return after the deadline',
           'used': 9534, 'total': 65536}
    rec.update(extra)
    return rec


# ---- norm(): C5 的比较键 ----
base = budget(invocationId='uuid-a', sequence=7)
check('norm 抹掉 invocationId', 'uuid-a' not in fa.norm(base))
check('norm 抹掉 sequence', '"sequence"' not in fa.norm(base))
check('norm 抹掉 requestEvidence（#1926 C5 豁免）',
      'requestEvidence' not in fa.norm(budget(requestEvidence={'total': 1})))
check('norm 保留业务字段', '"used":9534' in fa.norm(base))
check('norm 与键序无关', fa.norm({'kind': 'x', 'a': 1, 'b': 2}) == fa.norm({'b': 2, 'kind': 'x', 'a': 1}))
check('norm 抹掉 requestEvidence 后两侧同键',
      fa.norm(budget(requestEvidence={'total': 1})) == fa.norm(budget()))

# ---- check_request_evidence(): C6 ----
ts = [{'kind': 'model.request', 'attempt': 1}]
ru = [{'kind': 'model.request', 'attempt': 1}]

ok, msg = fa.check_request_evidence(ts, ru)
check('C6 两侧都无 ev → 通过且不比较', ok and '0 条' in msg, msg)

ok, msg = fa.check_request_evidence([dict(ts[0], requestEvidence={'a': 1, 'b': 2})],
                                    [dict(ru[0], requestEvidence={'b': 2, 'a': 1})])
check('C6 双侧 ev 内容相同（键序不同）→ 通过', ok and '1 条' in msg, msg)

ok, msg = fa.check_request_evidence([dict(ts[0], requestEvidence={'total': 1})],
                                    [dict(ru[0], requestEvidence={'total': 2})])
check('C6 双侧 ev 内容不同 → 失败', (not ok) and msg.startswith('C6 失败'), msg)

ok, msg = fa.check_request_evidence([dict(ts[0], requestEvidence={'total': 1})], ru)
check('C6 单侧有 ev → 通过并计残余（C5 残余，README 已登记）', ok and '单侧存在 1 条' in msg, msg)

ok, msg = fa.check_request_evidence(ts, [dict(ru[0], requestEvidence={'total': 1})])
check('C6 反方向单侧有 ev → 同样计残余', ok and '单侧存在 1 条' in msg, msg)

# 同名多帧：逐个配对，不能只比一条
multi_ts = [dict(ts[0], requestEvidence={'total': 1}), dict(ts[0], requestEvidence={'total': 2})]
multi_ru = [dict(ru[0], requestEvidence={'total': 2}), dict(ru[0], requestEvidence={'total': 1})]
ok, msg = fa.check_request_evidence(multi_ts, multi_ru)
check('C6 同键多帧按内容配对 → 通过（2 条都比了）', ok and '2 条' in msg, msg)

ok, msg = fa.check_request_evidence(multi_ts, [dict(ru[0], requestEvidence={'total': 2}),
                                               dict(ru[0], requestEvidence={'total': 3})])
check('C6 同键多帧有一条不同 → 失败', not ok, msg)

# identity 帧不参与
ident = {'kind': 'harness.proof', 'state': 'sidecar_identity', 'requestEvidence': {'x': 1}}
ok, msg = fa.check_request_evidence([dict(ts[0]), ident], [dict(ru[0])])
check('C6 跳过 sidecar identity 帧', ok and '0 条' in msg, msg)

# 被删掉的右侧 gsf 帧不参与（judge 传入 skip_rust）
gsf = {'kind': 'durable.status', 'event': 'gateway_submit_failed', 'requestEvidence': {'x': 1}}
ok, msg = fa.check_request_evidence([dict(ts[0])], [dict(ru[0]), gsf], skip_rust=frozenset({1}))
check('C6 跳过被删的 gsf 帧', ok and '单侧存在 0 条' in msg, msg)

# 同键两侧帧数不等：必须失败，不能 zip 静默截断（#1926 评审）
ok, msg = fa.check_request_evidence([dict(ts[0]), dict(ts[0])], [dict(ru[0])])
check('C6 同键帧数两侧不等 → 失败', (not ok) and '帧数两侧不等' in msg, msg)

# 同键多帧、一侧有 None：不能错位成「内容不同」（#1926 评审 P1：None 排序键导致配对错位）
# TS=[evA, evB]，Rust=[evA, None] → 只有 1 条可比，另 1 条是单侧残余
ok, msg = fa.check_request_evidence(
    [dict(ts[0], requestEvidence={'n': 1}), dict(ts[0], requestEvidence={'n': 2})],
    [dict(ru[0], requestEvidence={'n': 2}), dict(ru[0])])
check('C6 多帧含 None 不错位 → 通过且计 1 条残余',
      ok and '1 条两侧共有' in msg and '单侧存在 1 条' in msg, msg)

# 两侧都有 ev 但内容互不相同（各自都不在对方集合里）→ 仍必须失败
ok, msg = fa.check_request_evidence(
    [dict(ts[0], requestEvidence={'n': 1}), dict(ts[0], requestEvidence={'n': 2})],
    [dict(ru[0], requestEvidence={'n': 2}), dict(ru[0], requestEvidence={'n': 3})])
check('C6 多帧含一条内容不同 → 失败', not ok, msg)

# ---- is_the_gsf_insertion(): C2b ----
# 形状取自真实 G03 channel 差异（`_semantic_record` 投影只留这 5 个键）
def gsf_right(**extra):
    rec = {'kind': 'durable.status', 'agentScope': 'parent', 'event': 'gateway_submit_failed',
           'statusKind': 'error', 'text': fa.GSF_TEXT}
    rec.update(extra)
    return rec


good = {'left': None, 'right': gsf_right()}
check('C2b 正确 gsf 新增 → ok', fa.is_the_gsf_insertion(good)[0],
      fa.is_the_gsf_insertion(good)[1])
check('C2b left 非 null（替换型）→ 否',
      not fa.is_the_gsf_insertion(dict(good, left={'kind': 'durable.status'}))[0])
check('C2b event 改名 → 否',
      not fa.is_the_gsf_insertion({'left': None, 'right': dict(good['right'], event='x')})[0])
check('C2b text 篡改 → 否',
      not fa.is_the_gsf_insertion({'left': None, 'right': dict(good['right'], text='x')})[0])
# #1926 评审：C5/C6 都跳过这条 gsf，字段集只能在这里钉死
check('C2b 多带字段 → 否（不是 #6 形状）',
      not fa.is_the_gsf_insertion({'left': None, 'right': gsf_right(extra=1)})[0])
check('C2b 多带每轮随机键（invocationId）→ 否',
      not fa.is_the_gsf_insertion({'left': None, 'right': gsf_right(invocationId='u')})[0])
check('C2b 少字段 → 否',
      not fa.is_the_gsf_insertion(
          {'left': None, 'right': {k: v for k, v in gsf_right().items() if k != 'text'}})[0])
check('C2b agentScope 非 parent → 否',
      not fa.is_the_gsf_insertion({'left': None, 'right': gsf_right(agentScope='child')})[0])

# ---- 装置自身的护栏：判据被放宽必须显式改 pin，diff 里一眼可见 ----
# `scripts/parity/check_frozen.py` 只扫 `scripts/parity/**`，本装置在冻结范围之外，
# 因此用这条自 pin 替代冻结登记（#1926 评审 P2：判定装置不能没有任何机器护栏）。
PINNED_DEVICE_SHA256 = '5e3849fab60cf86bdec77371225805ff7c27325928311803df1300b7a05f018b'
with open(os.path.join(HERE, 'frame_attrib.py'), 'rb') as fh:
    actual_sha = hashlib.sha256(fh.read()).hexdigest()
check('装置自身 sha256 与 pin 一致（改判据必须显式 bump pin）',
      actual_sha == PINNED_DEVICE_SHA256, f'actual={actual_sha} pinned={PINNED_DEVICE_SHA256}')

# ---- identity_ok(): C4 ----
idrec = {'kind': 'harness.proof', 'state': 'sidecar_identity', 'side': 'rust',
         'sha256': 'a' * 64, 'expectedSha256': 'a' * 64, 'spawnObserved': True}
check('identity_ok 自洽 → 真', fa.identity_ok([idrec], 'rust')[0])
check('identity_ok sha 不自洽 → 假',
      not fa.identity_ok([dict(idrec, expectedSha256='b' * 64)], 'rust')[0])
check('identity_ok side 不符 → 假', not fa.identity_ok([idrec], 'ts')[0])
check('identity_ok 缺 sha256 → 假',
      not fa.identity_ok([{k: v for k, v in idrec.items() if k != 'sha256'}], 'rust')[0])
check('identity_ok 两条身份帧 → 假', not fa.identity_ok([idrec, idrec], 'rust')[0])

# ---- rounds(): 轮次目录发现 ----
with tempfile.TemporaryDirectory() as tdir:
    for name in ('ts-r1', 'ts-r10', 'ts-r2'):
        os.makedirs(os.path.join(tdir, name))
    check('rounds 按数字排序（r2 在 r10 前）',
          [os.path.basename(p) for p in fa.rounds(tdir, 'ts')] == ['ts-r1', 'ts-r2', 'ts-r10'])
    os.makedirs(os.path.join(tdir, 'ts-rX'))
    try:
        fa.rounds(tdir, 'ts')
        check('rounds 非数字后缀 → 报错（不静默跳过）', False)
    except SystemExit as exc:
        check('rounds 非数字后缀 → 报错（不静默跳过）', '不合规' in str(exc), str(exc))

# ---- judge(): C1–C6 编排（注入 G03 桩，不跑冻结比较器）----
# CI 只跑本自测，所以 judge() 这层「判据编排」必须有机检覆盖：#1926 评审 P2。
def proof(state):
    return {'kind': 'harness.proof', 'state': state}


def ident(side, **extra):
    rec = {'kind': 'harness.proof', 'state': 'sidecar_identity', 'side': side,
           'sha256': 'a' * 64, 'expectedSha256': 'a' * 64, 'spawnObserved': True}
    rec.update(extra)
    return rec


term = {'kind': 'terminal', 'outcome': 'failed'}
ts_ok = [ident('ts'), proof('result_unknown_fail_closed'), term]
ru_ok = [ident('rust'), proof('result_unknown_fail_closed'), term, gsf_right()]
stub = {'verdict': 'FAIL', 'triggered_rules': ['E10', 'E11', 'G03_channels'],
        'channel': {'differences': [{'left': None, 'right': gsf_right()}]}}


def j(ts, ru, per):
    return fa.judge('deadline', 'ts-dir', 'rust-dir', ts, ru, per=per)


check('judge 六条全满足 → ATTRIB_6', j(ts_ok, ru_ok, stub)[0] == 'ATTRIB_6', j(ts_ok, ru_ok, stub)[1])
check('judge G03 判 PASS → PASS',
      j(ts_ok, ru_ok, {'verdict': 'PASS'})[0] == 'PASS')
check('judge 规则集多一条 → NOT_6',
      j(ts_ok, ru_ok, dict(stub, triggered_rules=['E10', 'E11', 'G03_channels', 'trace_health']))[0] == 'NOT_6')
check('judge 语义差异 2 条 → NOT_6',
      j(ts_ok, ru_ok, dict(stub, channel={'differences': [{'left': None, 'right': gsf_right()},
                                                          {'left': None, 'right': gsf_right()}]}))[0] == 'NOT_6')
check('judge 场景越界 → NOT_6',
      fa.judge('sidecar_budget_limit', 'ts-dir', 'rust-dir', ts_ok, ru_ok, per=stub)[0] == 'NOT_6')
# C3：gsf 早于 fail_closed（分叉前移）
ru_early = [ident('rust'), gsf_right(), proof('result_unknown_fail_closed'), term]
check('judge gsf 早于 fail_closed → NOT_6', j(ts_ok, ru_early, stub)[0] == 'NOT_6')
# C4：身份帧 sha 不自洽
ru_badid = [ident('rust', expectedSha256='b' * 64), proof('result_unknown_fail_closed'),
            term, gsf_right()]
check('judge 身份自证失败 → NOT_6', j(ts_ok, ru_badid, stub)[0] == 'NOT_6')
# C5：删掉 gsf 后仍多一帧
ru_extra = [ident('rust'), proof('result_unknown_fail_closed'), term,
            {'kind': 'model.request', 'attempt': 1}, gsf_right()]
check('judge 多出无关帧 → NOT_6', j(ts_ok, ru_extra, stub)[0] == 'NOT_6')
# C6：两侧同名帧的 requestEvidence 内容不同
ts_ev = [ident('ts'), proof('result_unknown_fail_closed'),
         dict(term, requestEvidence={'n': 1})]
ru_ev = [ident('rust'), proof('result_unknown_fail_closed'),
         dict(term, requestEvidence={'n': 2}), gsf_right()]
check('judge requestEvidence 内容不同 → NOT_6', j(ts_ev, ru_ev, stub)[0] == 'NOT_6')

# ---- round_problem(): 轮次有效性闸门（对齐冻结 score.py）----
with tempfile.TemporaryDirectory() as tdir:
    d = os.path.join(tdir, 'ts-r1')
    os.makedirs(os.path.join(d, 'artifacts'))
    with open(os.path.join(d, 'run.json'), 'w') as fh:
        json.dump({'side': 'ts', 'valid_round': True, 'identity_ok': True,
                   'started_wall': 1762000000.0}, fh)
    with open(os.path.join(d, 'artifacts', 'summary.json'), 'w') as fh:
        json.dump({'blocked': []}, fh)
    check('round_problem 有效轮次 → None', fa.round_problem(d, 'ts') is None)
    with open(os.path.join(d, 'artifacts', 'summary.json'), 'w') as fh:
        json.dump({'blocked': ['deadline']}, fh)
    check('round_problem BLOCKED → 报原因', fa.round_problem(d, 'ts') is not None)
    os.remove(os.path.join(d, 'artifacts', 'summary.json'))
    check('round_problem 缺 summary.json → 报原因（blocked 未知不算未 blocked）',
          fa.round_problem(d, 'ts') is not None)
    # 恢复 summary.json：否则下面「缺 started_wall」会因 summary 缺失而通过，那条断言就没有
    # 判别力（去掉 started_wall 闸门也照样绿，#1926 评审 P2）。
    with open(os.path.join(d, 'artifacts', 'summary.json'), 'w') as fh:
        json.dump({'blocked': []}, fh)
    check('round_problem 恢复 summary 后有效轮次仍 → None（对照）',
          fa.round_problem(d, 'ts') is None)
    with open(os.path.join(d, 'run.json'), 'w') as fh:
        json.dump({'side': 'ts', 'valid_round': True, 'identity_ok': True}, fh)
    why = fa.round_problem(d, 'ts')
    check('round_problem 缺 started_wall → 报原因，且原因点名 started_wall（有判别力）',
          why is not None and 'started_wall' in why, f'why={why!r}')
    with open(os.path.join(d, 'run.json'), 'w') as fh:
        json.dump({'side': 'ts', 'valid_round': True, 'identity_ok': True,
                   'started_wall': 10 ** 400}, fh)
    why = fa.round_problem(d, 'ts')
    check('round_problem 超大整数 started_wall → 报原因（不抛 OverflowError）',
          why is not None and 'started_wall' in why, f'why={why!r}')
    with open(os.path.join(d, 'run.json'), 'w') as fh:
        json.dump({'side': 'ts', 'valid_round': False, 'identity_ok': True,
                   'started_wall': 1762000000.0}, fh)
    check('round_problem valid_round 非 true → 报原因', fa.round_problem(d, 'ts') is not None)

# ---- verdict_exit(): 退出码折叠（全 PASS 的批次不算归因证据）----
check('verdict_exit 有 NOT_6 → 2', fa.verdict_exit(10, 20, 3, False) == 2)
check('verdict_exit 批次不完整 → 6', fa.verdict_exit(10, 20, 0, True) == 6)
check('verdict_exit 全 PASS（n6=0）→ 7', fa.verdict_exit(100, 0, 0, False) == 7)
check('verdict_exit 正常归因 → 0', fa.verdict_exit(80, 20, 0, False) == 0)
check('verdict_exit 有未判定 → 8（比较器故障不得给 rc=0）', fa.verdict_exit(80, 20, 0, False, 1) == 8)
check('verdict_exit 未判定优先于批次不完整 → 8', fa.verdict_exit(80, 20, 0, True, 1) == 8)
check('verdict_exit 已有 NOT_6 时未判定不改变 2', fa.verdict_exit(80, 20, 1, False, 1) == 2)

# ---- case_mark(): 负控结果归类（UNKNOWN ≠ MISMATCH）----
check('case_mark 命中 → OK', fa.case_mark('NOT_6', 'NOT_6') == 'OK')
check('case_mark 实得 UNKNOWN 而期望别 → UNKNOWN（不算 MISMATCH）',
      fa.case_mark('UNKNOWN', 'NOT_6') == 'UNKNOWN')
check('case_mark 期望 UNKNOWN 且实得 UNKNOWN → OK', fa.case_mark('UNKNOWN', 'UNKNOWN') == 'OK')
check('case_mark 真判错 → MISMATCH', fa.case_mark('ATTRIB_6', 'NOT_6') == 'MISMATCH')

# ---- run(): 空对拍必须硬失败（P0：旧版 0 pair 时打印「无第二种差异」并 rc=0）----
with tempfile.TemporaryDirectory() as tdir:
    out, err = io.StringIO(), io.StringIO()
    with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
        rc = fa.run(tdir, 'deadline')
    check('run 空 root → rc≠0 且不给结论',
          rc != 0 and '无第二种差异' not in out.getvalue(), f'rc={rc} out={out.getvalue()[:80]!r}')

# ---- judge(): 比较器故障 = UNKNOWN，不是 NOT_6「反证命中」（#1926 评审 P1）----
_orig_g03 = fa.g03
try:
    fa.g03 = lambda *a: (None, None, 'g03 超时（>900s）')
    got, ev = fa.judge('deadline', '/no/ts', '/no/rust', [], [])
    check('judge g03 超时 → UNKNOWN（不当作 NOT_6）',
          got == 'UNKNOWN' and '未判定' in ev[0], f'got={got} ev={ev[0][:80]!r}')
    fa.g03 = lambda *a: ({}, {}, None)
    got, ev = fa.judge('deadline', '/no/ts', '/no/rust', [], [])
    check('judge g03 输出缺该场景 → UNKNOWN', got == 'UNKNOWN', f'got={got}')
finally:
    fa.g03 = _orig_g03

# ---- C0（进程内自报）：G03 说「没比成」→ UNKNOWN；trace_health 不误扩 ----
got, ev = fa.judge('deadline', '/x', '/y', [], [],
                   per={'verdict': 'FAIL', 'triggered_rules': ['trace_input'], 'error': 'bad record'})
check('judge G03 自报 trace_input → UNKNOWN', got == 'UNKNOWN' and 'C0' in ev[0], f'got={got}')
got, ev = fa.judge('deadline', '/x', '/y', [], [],
                   per={'verdict': 'FAIL', 'triggered_rules': ['trace_identity']})
check('judge G03 自报 trace_identity → UNKNOWN', got == 'UNKNOWN' and 'C0' in ev[0], f'got={got}')
got, ev = fa.judge('deadline', '/x', '/y', [], [],
                   per={'verdict': 'FAIL', 'triggered_rules': ['E10', 'E11', 'G03_channels', 'trace_health'],
                        'channel': {'differences': []}})
check('judge trace_health 仍判 NOT_6（C0 不误扩，negctl C/D 依赖它）',
      got == 'NOT_6' and 'C0' not in ev[0], f'got={got} ev={ev[0][:60]!r}')

# ---- run(): 端到端接线（全 PASS → 7；比较器故障 → 8），用桩替换 g03 ----
def _mk_round(root, name, side):
    d = os.path.join(root, name)
    os.makedirs(os.path.join(d, 'artifacts'))
    with open(os.path.join(d, 'run.json'), 'w') as fh:
        json.dump({'side': side, 'valid_round': True, 'identity_ok': True,
                   'started_wall': 1762000000.0}, fh)
    with open(os.path.join(d, 'artifacts', 'summary.json'), 'w') as fh:
        json.dump({'blocked': []}, fh)
    open(os.path.join(d, 'artifacts', 'deadline.pilotdeck-current-sidecar.jsonl'), 'w').close()


with tempfile.TemporaryDirectory() as tdir:
    _mk_round(tdir, 'ts-r1', 'ts')
    _mk_round(tdir, 'rust-r1', 'rust')
    _orig_g03 = fa.g03
    try:
        fa.g03 = lambda *a: ({'verdict': 'PASS', 'channel': {'differences': []},
                              'triggered_rules': []}, {}, None)
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            rc = fa.run(tdir, 'deadline')
        check('run 全 PASS 批次 → rc=7（钉住 run() 的接线，不只纯函数）',
              rc == 7 and '全部 PASS' in out.getvalue(), f'rc={rc}')
        fa.g03 = lambda *a: (None, None, 'g03 超时（>900s）')
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            rc = fa.run(tdir, 'deadline')
        txt = out.getvalue()
        check('run 比较器故障 → rc=8 且不打印「反证命中」',
              rc == 8 and '反证命中' not in txt and '未判定' in txt, f'rc={rc}')
    finally:
        fa.g03 = _orig_g03

# ---- is_gsf / is_identity ----
check('is_gsf 认 event', fa.is_gsf({'kind': 'durable.status', 'event': 'gateway_submit_failed'}))
check('is_gsf 不认别的 event', not fa.is_gsf({'kind': 'durable.status', 'event': 'turn_timeout'}))
check('is_identity 认 sidecar_identity',
      fa.is_identity({'kind': 'harness.proof', 'state': 'sidecar_identity'}))

print()
if FAILS:
    print(f'{len(FAILS)} 条失败：{FAILS}')
    sys.exit(1)
print('全部通过')
