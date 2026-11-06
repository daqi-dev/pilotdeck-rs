#!/usr/bin/env python3
"""`frame_attrib.py` 的最小自测（不依赖任何 trace 数据，纯合成记录）。

用法：
  python3 -I test_frame_attrib.py     # 退出码 0 = 全过；非 0 = 有用例失败

只覆盖判据里**纯函数**的部分（C2b 的插入形态、C5 的 norm 口径、C6 的 requestEvidence
比较）。C1–C4 需要真实 trace 与冻结比较器 G03，由真实数据复算 + 13 条 negctl 覆盖，
见 README「验证」。
"""
import importlib.util
import json
import os
import sys

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
check('norm 保留业务字段', '"used": 9534' in fa.norm(base))
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

# ---- is_the_gsf_insertion(): C2b ----
good = {'left': None,
        'right': {'kind': 'durable.status', 'event': 'gateway_submit_failed',
                  'statusKind': 'error', 'text': fa.GSF_TEXT}}
check('C2b 正确 gsf 新增 → ok', fa.is_the_gsf_insertion(good)[0])
check('C2b left 非 null（替换型）→ 否',
      not fa.is_the_gsf_insertion(dict(good, left={'kind': 'durable.status'}))[0])
check('C2b event 改名 → 否',
      not fa.is_the_gsf_insertion({'left': None, 'right': dict(good['right'], event='x')})[0])
check('C2b text 篡改 → 否',
      not fa.is_the_gsf_insertion({'left': None, 'right': dict(good['right'], text='x')})[0])

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
