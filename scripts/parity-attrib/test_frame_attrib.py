#!/usr/bin/env python3
"""`frame_attrib.py` 的最小自测（不依赖任何真实 trace 数据，纯合成记录）。

用法：
  python3 -I test_frame_attrib.py     # 退出码 0 = 全过；非 0 = 有用例失败

覆盖装置里**可纯合成复现**的部分：C2b 的插入形态与字段集、C5 的 norm 口径、C6 的
requestEvidence 比较、C4 的 identity_ok、`rounds()` 的目录校验、`round_problem()` 轮次闸门、
`judge()` 的 C0–C6 编排（注入 G03 桩，不跑冻结比较器）、`verdict_exit()` 退出码折叠、
`case_mark()` 负控归类、`write_trace()` 原子写、`build_cases()` 的基线前置守卫、
`negctl()` 的端到端接线（0 / 3 / 4 / 8 / 9 五个退出码，含 BaselineUnusable→rc=3 与
轮次目录名不合规→rc=3）、清理兜底（atexit 删半成品证据目录）与断管道下 rc 不被改写，
以及装置自身的 sha256 pin。15 条负控在**真实数据**上的判定不在这里，见 README「验证记录」。
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
PINNED_DEVICE_SHA256 = 'd41b4c6a2e5a5bb7fe011389782367e9ef2c0d3efed51ae3c957fb523df12bae'
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

# ---- case_mark(): 负控结果归类（未判定 ≠ MISMATCH，且 UNKNOWN 必须点名自报规则）----
check('case_mark 命中 → OK', fa.case_mark('NOT_6', 'NOT_6') == 'OK')
check('case_mark 实得 UNKNOWN 而期望别 → UNJUDGED（不算 MISMATCH）',
      fa.case_mark('UNKNOWN', 'NOT_6') == 'UNJUDGED')
check('case_mark 期望 UNKNOWN 且自报 trace_identity → OK',
      fa.case_mark('UNKNOWN', 'UNKNOWN', ('trace_identity',)) == 'OK')
check('case_mark 期望 UNKNOWN 且自报 trace_input → OK',
      fa.case_mark('UNKNOWN', 'UNKNOWN', ('trace_input',)) == 'OK')
check('case_mark 期望 UNKNOWN 但成因是进程级故障（自报集为空）→ UNJUDGED',
      fa.case_mark('UNKNOWN', 'UNKNOWN', ()) == 'UNJUDGED')
# 成因只认结构化集合：证据行里的字面量不再参与判定，进程级故障的 stderr 污染不了它。
check('case_mark 不看证据行文案（只有文本、没有结构化成因 → UNJUDGED）',
      fa.case_mark('UNKNOWN', 'UNKNOWN') == 'UNJUDGED')
check('case_mark 真判错 → MISMATCH', fa.case_mark('ATTRIB_6', 'NOT_6') == 'MISMATCH')

# ---- C0 的自报规则名必须仍与冻结件一致（漂移即 C0 静默失效，错向复发）----
for rel in ('scripts/parity/g03_compare.py', 'scripts/parity/score.py'):
    with open(os.path.join(fa.REPO, rel), encoding='utf-8') as fh:
        src = fh.read()
    check(f'{rel} 仍含这两个自报规则名（C0 的成因正本）',
          all(name in src for name in fa.G03_UNRUN_RULES), sorted(fa.G03_UNRUN_RULES))
check('G03_UNRUN_RULES 不含 trace_health（它是产品行为差异，不是「没比成」）',
      'trace_health' not in fa.G03_UNRUN_RULES)

# ---- write_trace(): 证据文件必须原子发布（半截 jsonl 会让证据目录本身作废）----
with tempfile.TemporaryDirectory() as tdir:
    dest = os.path.join(tdir, 'x.jsonl')
    fa.write_trace(dest, [{'a': 1}, {'b': 2}])
    with open(dest, encoding='utf-8') as fh:
        check('write_trace 正常写出全部记录', fh.read() == '{"a": 1}\n{"b": 2}\n')
    check('write_trace 不留临时文件', sorted(os.listdir(tdir)) == ['x.jsonl'], os.listdir(tdir))
    before = open(dest, encoding='utf-8').read()
    try:
        # 第 2 条不可序列化：非原子写会先 open('w') 截断目标再逐条写，留下半截证据；
        # 原子写要求目标文件保持原样。
        fa.write_trace(dest, [{'a': 1}, {'bad': {1, 2}}])
        check('write_trace 序列化失败 → 抛异常', False)
    except TypeError:
        check('write_trace 序列化失败 → 目标文件保持原样（不落半截证据）',
              open(dest, encoding='utf-8').read() == before, open(dest, encoding='utf-8').read())
        check('write_trace 序列化失败 → 不留临时文件', sorted(os.listdir(tdir)) == ['x.jsonl'],
              os.listdir(tdir))

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
    got, ev, cause = fa.judge('deadline', '/no/ts', '/no/rust', [], [])
    check('judge g03 超时 → UNKNOWN（不当作 NOT_6）',
          got == 'UNKNOWN' and '未判定' in ev[0], f'got={got} ev={ev[0][:80]!r}')
    check('judge g03 超时的证据行归属 C0（与 README 判据表同口径）',
          'C0' in ev[0], f'ev={ev[0][:80]!r}')
    check('judge g03 超时的结构化成因为空（进程级故障 ≠ C0 自报）', cause == (), f'cause={cause}')
    fa.g03 = lambda *a: (None, None, 'g03 无法执行：[Errno 24] Too many open files')
    got, ev, cause = fa.judge('deadline', '/no/ts', '/no/rust', [], [])
    check('judge g03 起不来（OSError）→ UNKNOWN，不是装置崩溃',
          got == 'UNKNOWN' and '未判定' in ev[0] and cause == (), f'got={got}')
    fa.g03 = lambda *a: ({}, {}, None)
    got, ev, _ = fa.judge('deadline', '/no/ts', '/no/rust', [], [])
    check('judge g03 输出缺该场景 → UNKNOWN', got == 'UNKNOWN' and 'C0' in ev[0], f'got={got}')
finally:
    fa.g03 = _orig_g03

# ---- C0（进程内自报）：G03 说「没比成」→ UNKNOWN；trace_health 不误扩 ----
got, ev, cause = j(ts_ok, ru_ok, {'verdict': 'FAIL', 'triggered_rules': ['trace_input'],
                                   'error': 'bad record'})
check('judge G03 自报 trace_input → UNKNOWN', got == 'UNKNOWN' and 'C0' in ev[0], f'got={got}')
check('judge G03 自报 trace_input → 结构化成因点名 trace_input', cause == ('trace_input',), f'cause={cause}')
got, ev, cause = j(ts_ok, ru_ok, {'verdict': 'FAIL', 'triggered_rules': ['trace_identity']})
check('judge G03 自报 trace_identity → UNKNOWN', got == 'UNKNOWN' and 'C0' in ev[0], f'got={got}')
check('judge G03 自报 trace_identity → 结构化成因点名 trace_identity',
      cause == ('trace_identity',), f'cause={cause}')
got, ev, cause = j(ts_ok, ru_ok,
                    {'verdict': 'FAIL', 'triggered_rules': ['E10', 'E11', 'G03_channels', 'trace_health'],
                     'channel': {'differences': []}})
check('judge trace_health 仍判 NOT_6（C0 不误扩，negctl C/D 依赖它）',
      got == 'NOT_6' and 'C0' not in ev[0] and cause == (), f'got={got} ev={ev[0][:60]!r}')
# `error: null` 不能把字面量 None 拼进证据行（GitHub 上的 G03 结果可能带 null）
got, ev, _ = j(ts_ok, ru_ok, {'verdict': 'FAIL', 'triggered_rules': ['trace_input'], 'error': None})
check('judge 自报 error 为 null → 证据行不出现 "None"',
      got == 'UNKNOWN' and 'None' not in ev[0], f'ev={ev[0][:80]!r}')

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

# ---- build_cases(): 变异点缺失必须是 BaselineUnusable（rc=3），不是裸异常 ----
def _gsf_rec():
    return {'kind': 'durable.status', 'agentScope': 'parent', 'event': 'gateway_submit_failed',
            'statusKind': 'error', 'text': fa.GSF_TEXT}


def _base(side, with_gsf):
    """合成一对基线：rust 侧就是 ts 侧多出那一条 gsf（#6 的形状）。"""
    recs = [dict(ident(side), sequence=0),
            {'kind': 'harness.proof', 'state': fa.FAIL_CLOSED, 'sequence': 1},
            {'kind': 'context.budget', 'used': 1, 'total': 2, 'sequence': 2},
            {'kind': 'model.request', 'attempt': 1, 'sequence': 3},
            {'kind': 'side_effect.state', 'sequence': 4},
            {'kind': 'terminal', 'outcome': 'failed', 'q': 'x', 'sequence': 5},
            {'kind': 'operation.terminal', 'outcome': 'result_unknown',
             'lastAppliedSequence': 5, 'sequence': 6}]
    if with_gsf:
        recs.append(dict(_gsf_rec(), sequence=7))
    return recs


_rich_ts = _base('ts', False)
_rich_ru = _base('rust', True)
check('build_cases 富基线 → 15 条用例、全部 5 元组',
      len(fa.build_cases(_rich_ts, _rich_ru)) == 15
      and all(len(c) == 5 for c in fa.build_cases(_rich_ts, _rich_ru)))
check('build_cases 不改基线（变异只作用于副本）',
      fa.build_cases(_rich_ts, _rich_ru) is not None
      and _rich_ru[7] == dict(_gsf_rec(), sequence=7))


def _expect_unusable(label, ts, ru, needle):
    try:
        fa.build_cases(ts, ru)
        check(label, False, '没有抛 BaselineUnusable')
    except fa.BaselineUnusable as exc:
        check(label, needle in str(exc), f'exc={exc}')


# 无 identity 帧的 TS 基线：G03 允许这种输入（上游捕获早于 identity 包装），
# 但 N 用例需要一帧可改 —— 旧代码是裸 next() → StopIteration → rc=1 traceback。
_expect_unusable('build_cases ts 基线缺 identity 帧 → BaselineUnusable（不是 StopIteration）',
                 [r for r in _rich_ts if not fa.is_identity(r)], _rich_ru, 'sidecar_identity')
# 空 TS trace：旧代码在 N 的裸 next() 上崩（StopIteration），O 的 out[0] 会 IndexError。
# 这里最早命中的是 L/M 的 context.budget 前置 —— 重点是**任何情况下都是 BaselineUnusable**。
_expect_unusable('build_cases ts 基线为空 → BaselineUnusable（不是 IndexError/StopIteration）',
                 [], _rich_ru, 'context.budget')
# 首条记录没有 sequence：O 的变异会是空操作（期望 UNKNOWN 落空 → 假 MISMATCH）。
_expect_unusable('build_cases 首条记录缺 sequence → BaselineUnusable（避免空变异）',
                 [ident('ts')] + _rich_ts[1:], _rich_ru, 'sequence')
# 缺 context.budget：L/M 的变异点不存在。
_expect_unusable('build_cases 缺 context.budget → BaselineUnusable',
                 [r for r in _rich_ts if r.get('kind') != 'context.budget'], _rich_ru, 'context.budget')
# E 的变异点（operation.terminal / outcome=result_unknown）缺失：旧写法是静默空操作，
# 用例退化成第二个 A → 假 MISMATCH → rc=4「判据被证伪」（与 #1968 同向）。
_expect_unusable('build_cases 缺 operation.terminal(result_unknown) → BaselineUnusable（E 不空变异）',
                 _rich_ts,
                 [r for r in _rich_ru
                  if not (r.get('kind') == 'operation.terminal'
                          and r.get('outcome') == 'result_unknown')],
                 'operation.terminal')
# H 的变异点（side_effect.state）缺失：过滤型变异同样会静默空操作。
_expect_unusable('build_cases 缺 side_effect.state → BaselineUnusable（H 不空变异）',
                 _rich_ts,
                 [r for r in _rich_ru if r.get('kind') != 'side_effect.state'],
                 'side_effect.state')
# 缺 gsf：A/B/C/G 都依赖它，且 negctl 不再有第二处前置检查（只此一条 rc=3 路径）。
_expect_unusable('build_cases 缺 gsf → BaselineUnusable（唯一一处前置，不再重复检查）',
                 _rich_ts, [r for r in _rich_ru if not fa.is_gsf(r)], 'gsf')

# ---- negctl(): 端到端接线（rc 必须由真实循环折叠出来，不只纯函数）----
# 判据：线序 A/B/C/D/E/F/G/H/I/J/K/L/M 依用例名桩掉 G03 —— 这是仓库内唯一能在 CI 复现
# 负控 rc 折叠的地方（#1969 评审 P2：rc=8 分支此前没有任何端到端断言）。
def _g03_stub(mapping, default):
    def stub(left, right, scenario):
        name = os.path.basename(os.path.dirname(left))
        return mapping.get(name, default), {}, None
    return stub


_per_attrib6 = {'verdict': 'FAIL', 'triggered_rules': ['E10', 'E11', 'G03_channels'],
                'channel': {'differences': [{'left': None, 'right': gsf_right()}]}}
_per_not6 = {'verdict': 'FAIL', 'triggered_rules': ['E10', 'E11', 'G03_channels', 'trace_health'],
             'channel': {'differences': []}}
_per_selfreport = {'verdict': 'FAIL', 'triggered_rules': ['trace_input'], 'error': 'bad'}
_ok_map = {'A-baseline-attrib6': _per_attrib6, 'L-single-side-requestEvidence': _per_attrib6,
           'N-identity-broken-g03-selfreport': {**_per_selfreport,
                                                'triggered_rules': ['trace_identity']},
           'O-record-missing-sequence-g03-input-error': _per_selfreport}

with tempfile.TemporaryDirectory() as tdir:
    rounds_dir = os.path.join(tdir, 'rounds')
    for side, recs in (('ts', _rich_ts), ('rust', _rich_ru)):
        name = f'{side}-r1'
        d = os.path.join(rounds_dir, name)
        os.makedirs(os.path.join(d, 'artifacts'))
        with open(os.path.join(d, 'run.json'), 'w') as fh:
            json.dump({'side': side, 'valid_round': True, 'identity_ok': True,
                       'started_wall': 1762000000.0}, fh)
        with open(os.path.join(d, 'artifacts', 'summary.json'), 'w') as fh:
            json.dump({'blocked': []}, fh)
        fa.write_trace(os.path.join(d, 'artifacts',
                                    'deadline.pilotdeck-current-sidecar.jsonl'), recs)

    _saved = fa.g03
    try:
        # ① 全部按期望命中 → rc=0
        fa.g03 = _g03_stub(_ok_map, _per_not6)
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            rc = fa.negctl(rounds_dir, 'deadline', os.path.join(tdir, 'out-ok'))
        check('negctl 15 条全按期望命中 → rc=0', rc == 0 and out.getvalue().count('OK') == 15,
              f'rc={rc}')

        # ② 只有一条未判定（G03 超时）→ rc=8，且打印「未判定」汇总
        fa.g03 = (lambda inner: (lambda l, r, s: (None, None, 'g03 超时（>900s）')
                                 if 'B-event-renamed' in l else inner(l, r, s)))(
            _g03_stub(_ok_map, _per_not6))
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            rc = fa.negctl(rounds_dir, 'deadline', os.path.join(tdir, 'out-unjudged'))
        check('negctl 一条未判定 → rc=8 且打印未判定汇总（不折成 MISMATCH）',
              rc == 8 and '未判定：1 条' in out.getvalue(), f'rc={rc}')

        # ③ N 的 UNKNOWN 若来自进程级故障（不是自报）→ 不能算命中，仍 rc=8
        fa.g03 = (lambda inner: (lambda l, r, s: (None, None, 'g03 超时（>900s）')
                                 if 'N-identity' in l else inner(l, r, s)))(
            _g03_stub(_ok_map, _per_not6))
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            rc = fa.negctl(rounds_dir, 'deadline', os.path.join(tdir, 'out-cause'))
        check('negctl 期望 UNKNOWN 但成因非自报 → UNJUDGED / rc=8（不记 OK）',
              rc == 8 and 'UNJUDGED' in out.getvalue(), f'rc={rc}')

        # ④ MISMATCH 与未判定并存 → rc=4（MISMATCH 优先），但未判定汇总仍要打印
        #    （让 A 的期望落空 → 真 MISMATCH；B 变成装置故障 → 未判定）
        fa.g03 = (lambda inner: (lambda l, r, s: (None, None, 'g03 超时（>900s）')
                                 if 'B-event-renamed' in l else inner(l, r, s)))(
            _g03_stub({**_ok_map, 'A-baseline-attrib6': _per_not6}, _per_not6))
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            rc = fa.negctl(rounds_dir, 'deadline', os.path.join(tdir, 'out-mismatch'))
        text4 = out.getvalue()
        check('negctl MISMATCH + 未判定 → rc=4 且未判定汇总仍打印（信息不丢）',
              rc == 4 and '未判定：1 条' in text4, f'rc={rc}')
        # 汇总行内的 rc 必须是**真正的**退出码：写成 rc=8 会让只读日志的审计者把
        # 「判据被证伪」读成「装置故障」（r2 boundary 评审 P2）。
        check('negctl MISMATCH + 未判定 → 汇总行报 rc=4（不硬编码 rc=8）',
              '（rc=4：' in text4 and '（rc=8）' not in text4, f'out={text4[-200:]!r}')

        # ⑥ 循环中途崩溃 → rc=9 且删掉半成品证据目录（不与「目录已存在」rc=1 撞码）
        def _boom(*a):
            raise RuntimeError('模拟 ENOSPC / EIO')
        fa.g03 = _boom
        out_9 = os.path.join(tdir, 'out-crash')
        out = io.StringIO()
        err = io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            rc = fa.negctl(rounds_dir, 'deadline', out_9)
        check('negctl 循环中途崩溃 → rc=9（不是 rc=1，不与「已存在」撞码）',
              rc == 9 and 'rc=9' in out.getvalue(), f'rc={rc}')
        check('negctl 循环中途崩溃 → 半成品证据目录已删除（不留残缺证据）',
              not os.path.exists(out_9))

        # ⑤ 基线轮次无效 → rc=3（不是「判据被证伪」的 rc=4；#1968 的机检版）
        for name in ('run.json',):
            with open(os.path.join(rounds_dir, 'rust-r1', name), 'w') as fh:
                json.dump({'side': 'rust', 'valid_round': False, 'identity_ok': True,
                           'started_wall': 1762000000.0}, fh)
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            rc = fa.negctl(rounds_dir, 'deadline', os.path.join(tdir, 'out-invalid'))
        check('negctl 基线轮次无效 → rc=3（基线不可用，不折成 MISMATCH）',
              rc == 3 and '基线不可用' in out.getvalue(), f'rc={rc}')
        # 同一批数据 run() 仍然硬失败（rc=5）：两者口径一致，都不拿无效轮次下结论
        out = io.StringIO()
        with contextlib.redirect_stderr(io.StringIO()):
            rc_run = fa.run(rounds_dir, 'deadline')
        check('同一批无效轮次 run() 也是硬失败 → rc=5（与 negctl rc=3 同向）',
              rc_run == 5, f'rc={rc_run}')
    finally:
        fa.g03 = _saved

# ---- 证据目录必须原子独占（O_EXCL 语义，不是 check-then-act）----
with tempfile.TemporaryDirectory() as tdir:
    rounds_dir = os.path.join(tdir, 'rounds')
    for side, recs in (('ts', _rich_ts), ('rust', _rich_ru)):
        d = os.path.join(rounds_dir, f'{side}-r1')
        os.makedirs(os.path.join(d, 'artifacts'))
        with open(os.path.join(d, 'run.json'), 'w') as fh:
            json.dump({'side': side, 'valid_round': True, 'identity_ok': True,
                       'started_wall': 1762000000.0}, fh)
        with open(os.path.join(d, 'artifacts', 'summary.json'), 'w') as fh:
            json.dump({'blocked': []}, fh)
        fa.write_trace(os.path.join(d, 'artifacts',
                                    'deadline.pilotdeck-current-sidecar.jsonl'), recs)

    def _negctl_quiet(outdir):
        with contextlib.redirect_stdout(io.StringIO()):
            try:
                return fa.negctl(rounds_dir, 'deadline', outdir)
            except SystemExit as exc:
                return f'SystemExit: {exc}'

    taken = os.path.join(tdir, 'taken')
    os.makedirs(taken)
    got = _negctl_quiet(taken)
    check('--negctl-out 已存在 → 拒绝（不覆盖证据）',
          isinstance(got, str) and '可独占新建' in got and '已存在' in got, f'got={got!r}')
    # 悬空符号链接：`os.path.exists()` 返回 False（旧实现会穿过去写目标），
    # `os.makedirs()` 抛 FileExistsError（新实现拒绝）。这条钉住「create 即 check」。
    dangling = os.path.join(tdir, 'dangling')
    os.symlink(os.path.join(tdir, 'no-such-target'), dangling)
    got = _negctl_quiet(dangling)
    check('--negctl-out 是悬空符号链接 → 拒绝（旧 exists() 判 False 会穿透写入）',
          isinstance(got, str) and '可独占新建' in got and '已存在' in got, f'got={got!r}')
    check('悬空符号链接的写入没有被创建（没有穿透）',
          not os.path.exists(os.path.join(tdir, 'no-such-target')))

    # 空串是 falsy：旧写法 `if args.negctl_out:` 会静默改跑 run()，调用方按 negctl 的 rc 表
    # 去读 run 的 rc。空串必须显式报错（落到 makedirs 的失败分支）。
    _argv = sys.argv
    try:
        sys.argv = ['frame_attrib.py', rounds_dir, 'deadline', '--negctl-out', '']
        with contextlib.redirect_stdout(io.StringIO()):
            try:
                got = fa.main()
            except SystemExit as exc:
                got = f'SystemExit: {exc}'
    finally:
        sys.argv = _argv
    check('--negctl-out "" → 显式报错，不静默落进 run()',
          isinstance(got, str) and '可独占新建' in got, f'got={got!r}')

# ---- r2 评审的增量：BaselineUnusable→rc=3 端到端、目录名不合规、清理与断管道 ----
with tempfile.TemporaryDirectory() as tdir:
    # ① 基线缺变异点（rust 侧无 side_effect.state）→ build_cases 抛 BaselineUnusable
    #    → negctl 折叠成 rc=3，**不是** rc=9 traceback、也不是 rc=4 假 MISMATCH。
    rounds_dir = os.path.join(tdir, 'rounds')
    for side, recs in (('ts', _rich_ts),
                       ('rust', [r for r in _rich_ru if r.get('kind') != 'side_effect.state'])):
        d = os.path.join(rounds_dir, f'{side}-r1')
        os.makedirs(os.path.join(d, 'artifacts'))
        with open(os.path.join(d, 'run.json'), 'w') as fh:
            json.dump({'side': side, 'valid_round': True, 'identity_ok': True,
                       'started_wall': 1762000000.0}, fh)
        with open(os.path.join(d, 'artifacts', 'summary.json'), 'w') as fh:
            json.dump({'blocked': []}, fh)
        fa.write_trace(os.path.join(d, 'artifacts',
                                    'deadline.pilotdeck-current-sidecar.jsonl'), recs)
    out = io.StringIO()
    with contextlib.redirect_stdout(out):
        rc = fa.negctl(rounds_dir, 'deadline', os.path.join(tdir, 'out-unusable'))
    check('negctl 基线缺变异点 → rc=3 且目录未创建（BaselineUnusable 接线）',
          rc == 3 and '基线不可用' in out.getvalue()
          and not os.path.exists(os.path.join(tdir, 'out-unusable')), f'rc={rc}')

    # ② 轮次目录名不合规：`rounds()` 抛 SystemExit(str)（stdlib 语义 → 进程退 1），
    #    negctl 必须把它折成 rc=3（数据不可用），否则与「证据目录已存在」rc=1 撞码。
    os.makedirs(os.path.join(rounds_dir, 'ts-rX', 'artifacts'))
    out = io.StringIO()
    with contextlib.redirect_stdout(out):
        rc = fa.negctl(rounds_dir, 'deadline', os.path.join(tdir, 'out-badname'))
    check('negctl 轮次目录名不合规 → rc=3（不是 rc=1「目录已存在」）',
          rc == 3 and '基线不可用' in out.getvalue(), f'rc={rc}')

# ③ write_trace 发布后的权限：mkstemp 默认 0600，归档后换账号复核会读不到 → G03 假「未判定」。
with tempfile.TemporaryDirectory() as tdir:
    p = os.path.join(tdir, 'trace.jsonl')
    fa.write_trace(p, [{'kind': 'a'}])
    mode = os.stat(p).st_mode & 0o777
    check('write_trace 发布后权限含组/其他可读（0644，换账号复核可读）',
          mode == 0o644, f'mode={oct(mode)}')

# ④ atexit 兜底：建好目录、没跑完就被信号打断（SystemExit 绕过显式清理）时也要删干净。
with tempfile.TemporaryDirectory() as tdir:
    d = os.path.join(tdir, 'pending')
    os.makedirs(d)
    fa._PENDING_OUTDIRS.add(d)
    fa._cleanup_pending_outdirs()
    check('atexit 清理：未跑完的证据目录被删掉且销账',
          not os.path.exists(d) and d not in fa._PENDING_OUTDIRS)

# ⑤ rc=9 的收尾输出遇断管道（`… | head -1`）：不得抛出，否则异常逃出 except 块把 rc 改成 1。
class _BrokenOut:
    def write(self, _s):
        raise BrokenPipeError('broken pipe')

    def flush(self):
        raise BrokenPipeError('broken pipe')

    def fileno(self):
        raise OSError('no fileno')


_saved_out = sys.stdout
try:
    sys.stdout = _BrokenOut()
    fa._report_crash(RuntimeError('模拟崩溃'), '/nonexistent/pdrs-1968')
    broke_ok = True
except BaseException as exc:  # noqa: BLE001 —— 这里就是要它「什么都不抛」
    broke_ok = False
    broke_detail = repr(exc)
finally:
    sys.stdout = _saved_out
check('rc=9 收尾输出遇断管道不抛出（rc 不被改写成 1）',
      broke_ok, '' if broke_ok else broke_detail)

# ⑥ _PipeSafe：读端提前退出时 write/flush 都不抛，并把 fd 1 静音（否则退出码会被改写）。
_silenced = []
_real_silence = fa._silence_stdout
fa._silence_stdout = lambda: _silenced.append(1)
try:
    _w = fa._PipeSafe(_BrokenOut())
    _wrote = _w.write('x' * 10)
    _w.flush()
    pipe_ok, pipe_detail = True, ''
except BaseException as exc:  # noqa: BLE001 —— 这里就是要它「什么都不抛」
    pipe_ok, pipe_detail = False, repr(exc)
finally:
    fa._silence_stdout = _real_silence
check('_PipeSafe 吞掉断管道错误并静音 fd 1（write 返回长度、flush 不抛）',
      pipe_ok and _wrote == 10 and len(_silenced) == 2,
      pipe_detail or f'wrote={_wrote} silenced={len(_silenced)}')

# ⑦ 真实进程 + 短读者：输出 2000 行、读一行就关管道，退出码必须是真实 rc=3 —— 而不是
#    CPython 退出 flush 失败的 120（缓冲）或无缓冲时的 1（改前实测见 README「验证记录」）。
import subprocess  # noqa: E402 —— 只这一处需要起真进程

with tempfile.TemporaryDirectory() as tdir:
    rounds_dir = os.path.join(tdir, 'rounds')
    os.makedirs(rounds_dir)
    for _i in range(1, 2001):
        os.makedirs(os.path.join(rounds_dir, f'ts-r{_i}'))
    for flags, tag in (([], '默认缓冲'), (['-u'], '-u 无缓冲')):
        p = subprocess.Popen(
            [sys.executable, '-I', *flags, os.path.join(HERE, 'frame_attrib.py'),
             rounds_dir, 'deadline', '--negctl-out', os.path.join(tdir, f'out-{len(flags)}')],
            stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
        first = p.stdout.readline()
        p.stdout.close()
        try:
            rc = p.wait(timeout=120)
        except subprocess.TimeoutExpired:
            p.kill()
            rc = 'timeout'
        check(f'输出接短读者（{tag}，读一行即关管道）时 rc 是真实值 3，不是 120 / 1',
              rc == 3 and b'rc=3' in first, f'rc={rc} first={first[:80]!r}')

print()
if FAILS:
    print(f'{len(FAILS)} 条失败：{FAILS}')
    sys.exit(1)
print('全部通过')
