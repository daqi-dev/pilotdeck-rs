#!/usr/bin/env python3
"""Retirement 轮逐方法 outcome / 错误码对照装置（issue #1477）。

判据（P1–P8）、阈值（0 处不一致）与容差全部登记于
``specs/gateway-parity/diffs/A8.6-RETIREMENT-OUTCOME-PARITY-001.md``，先于任何
Rust retirement 轮登记（S1 判据提交，body 含 ``criteria-commit: yes``）。本文件是
这些规则的唯一实现：

  emit  -- 从 TS 参考轮（k=5）重生成 / 复核金样 ts-oracle.json
  check -- 拿一轮或多轮 retirement 轮的 methods.json + frames.jsonl 对照金样

退出码：0 PASS / 1 FAIL / 2 输入或环境不可用（fail-closed）。rc2 的完整成因清单见
``README.md``「Rust retirement 轮怎么调用」与差异条目的「机器判据」——缺文件、JSON
解析失败、缺字段、金样条目数 != 101、金样与登记的 gated 集不符、缺 ``-I``、
``emit`` 写金样失败等，一律绝不当 PASS。
"""
from __future__ import annotations

import sys

if not sys.flags.isolated:
    print(f'run with -I: python3 -I {__file__}', file=sys.stderr)
    raise SystemExit(2)

import argparse
import hashlib
import json
import os
import tempfile
from pathlib import Path

# 同族装置口径（scripts/gateway-parity/*、scripts/parity/* 均成对设置）：不写也
# **不读**缓存字节码。只设 dont_write_bytecode 挡不住读取已有的
# __pycache__/*.pyc（PEP 552 unchecked-hash 可直接执行），而本目录会被负控以
# spec_from_file_location 再加载一次——陈旧字节码会让「跑的是当前源码」不成立。
try:
    sys.pycache_prefix = tempfile.mkdtemp(prefix='pdrs-retirement-pyc-')
except OSError:
    # 没有可写临时目录（只读环境 / 临时目录被清理）也不能让判据起不来：指向一个
    # **核实不存在**的目录，效果等价于「读不到任何缓存」，写仍被 dont_write_bytecode
    # 挡住。启动依赖不得先于任何判定（评审 P0）。名字带 16 位随机字节并在冲突时
    # 重试——固定的 ``.no-pycache-<pid>`` 可预测，等于给 unchecked-hash pyc 留了
    # 一个可投毒的位置（评审 P1）。
    base = os.path.dirname(os.path.abspath(__file__))
    for _ in range(8):
        candidate = os.path.join(
            base, f'.no-pycache-{os.getpid()}-{os.urandom(8).hex()}')
        if not os.path.lexists(candidate):
            break
    else:
        # 入口失败一律 rc2（输入不可用）；`SystemExit('...')` 返回的是 **1**，会被
        # 下游当成「判据 FAIL」而不是「跑不了」。
        print('check_outcome_parity: cannot reserve a fresh pycache prefix',
              file=sys.stderr)
        raise SystemExit(2)
    sys.pycache_prefix = candidate
sys.dont_write_bytecode = True

EXIT_PASS, EXIT_FAIL, EXIT_INPUT = 0, 1, 2

# —— 登记事实（diffs/A8.6-RETIREMENT-OUTCOME-PARITY-001.md「基线依据」）——
SCHEMA = 'retirement-outcome-parity/ts-oracle v1'
BASELINE_REVISION = 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85'
NODE_VERSION = 'v22.22.0'
EXPECTED_METHOD_COUNT = 101
CAPABILITY_UNAVAILABLE = 'CAPABILITY_UNAVAILABLE'
# P5：errorCode == CAPABILITY_UNAVAILABLE 的方法集合必须恰好等于这 6 个
# （TS 基线在无凭据配置下就是如此答；这是登记值，不是豁免）。
REGISTERED_UNAVAILABLE_METHODS = frozenset({
    'manager_browsers', 'memory_list', 'memory_wipe',
    'snapshot_get', 'snapshot_list', 'snapshot_restore',
})
# —— 显式延期（#2143，diffs/A8.6-RETIREMENT-OUTCOME-PARITY-001-R1.md）——
# 这三个 always-on 方法按 2026-11-05 人定「只做核心 agent 功能，always-on 等增值
# 模块暂缓」尚未实现：Rust 无条件回 CAPABILITY_UNAVAILABLE（#1313），而 TS 基线的
# 真实回答是 **成功信封** `ok:true` + payload `error.code`
# （#2143 实测 cc7c4e6 / Node v22.22.0：cycle_not_found / not_configured /
# not_configured）。金样没写错，所以这是**延期**不是「金样过期」，更不是豁免：
# 容忍范围只有一格 —— 该方法 `outcome == 'error'` 且
# `errorCode == CAPABILITY_UNAVAILABLE` 对应金样 `ok`。Rust 若回别的码、或将来
# 回 `ok` 之外的第三种形状，P3/P4 **照常 FAIL**；P1/P2/P6/P7/P8 对这三个方法
# 照常核；P3/P4/P5 对其余 98 个方法逐字节不变。**逐方法显式列出，不许前缀匹配。**
DEFERRED_UNAVAILABLE_METHODS = frozenset({
    'always_on_apply', 'always_on_abort', 'always_on_rerun_plan',
})
DEFERRED_REASON = (
    'always-on 三方法按 2026-11-05 人定暂缓实现（A7.8-ALWAYS-ON-DEFERRED-001）；'
    'Rust 如实回 CAPABILITY_UNAVAILABLE，TS 基线实测回 ok:true 信封。'
    '登记：diffs/A8.6-RETIREMENT-OUTCOME-PARITY-001-R1.md（延期，非豁免）')
KNOWN_OUTCOMES = ('ok', 'error')
KNOWN_SHAPES = ('request', 'stream')


# 本装置的金样是第 5 份「101 方法」登记表；冻结 fixture 是四源集合的落点，
# 二者必须逐名同形（否则单边漂移会让 P1 的期望集失效：漏跑新方法时
# missing=∅ 仍 PASS —— 假 PASS；反向漂移则假 FAIL）。
FROZEN_METHODS = (Path(__file__).resolve().parent.parent
                  / 'gateway-parity' / 'fixtures' / 'retirement-methods.json')


class InputError(Exception):
    """输入不可用 —— fail-closed，退出码 2，绝不当 PASS。"""


def error_value(response):
    """response 的 ``error`` 字段原值：缺省与 ``null`` 都等同「无 error」。

    serde 未 skip 成功响应时常见显式 null；把它判成「携带 error」会对合法
    Rust 轮假 FAIL。但**只有 null / 缺省**才是无 error：字符串、数组、数字
    （例如 ``error: String`` 的 ``"boom"`` 或 ``error: Vec`` 的 ``[]``）都是
    线缆异常，不得被「不是 dict 就当没有」过滤成假 PASS。
    """
    return response.get('error')


def error_object(response):
    """response 携带的错误对象；非 dict 的 ``error`` 值返回 None。

    调用方判「是否携带 error」必须用 :func:`error_value`（``is not None``），
    不能只看本函数的返回值——否则非 dict 的非法值会静默通过。
    """
    error = error_value(response)
    return error if isinstance(error, dict) else None


def violation(rule, round_name, method, detail):
    return {'rule': rule, 'round': round_name, 'method': method, 'detail': detail}


def sha256_file(path):
    digest = hashlib.sha256()
    with open(path, 'rb') as handle:
        for chunk in iter(lambda: handle.read(1 << 20), b''):
            digest.update(chunk)
    return digest.hexdigest()


def read_json(path):
    try:
        text = Path(path).read_text(encoding='utf-8')
    except (OSError, UnicodeDecodeError) as error:
        # UnicodeDecodeError ⊂ ValueError 但 ⊄ OSError：不显式接住就会 traceback
        # 成 rc=1「判据 FAIL」，与登记口径 rc=2「输入不可用」矛盾。
        raise InputError(f'cannot read {path}: {error}') from error
    try:
        return json.loads(text)
    except ValueError as error:
        raise InputError(f'cannot parse {path}: {error}') from error


def load_rows(round_dir):
    """一轮的 methods.json → 行列表（严格校验，缺字段即输入不可用）。"""
    path = Path(round_dir) / 'driver' / 'methods.json'
    data = read_json(path)
    if not isinstance(data, dict) or not isinstance(data.get('methods'), list):
        raise InputError(f'{path}: expected an object with a "methods" list')
    rows = []
    for index, row in enumerate(data['methods']):
        if not isinstance(row, dict):
            raise InputError(f'{path}: row {index} is not an object')
        for field in ('method', 'outcome', 'shape'):
            value = row.get(field)
            if not isinstance(value, str) or not value:
                raise InputError(f'{path}: row {index} lacks string field {field}')
        # errorCode 进 P7 的稳定性集合，非哈希类型会在 analyze_stability 里
        # 抛 TypeError（rc1 traceback）；events 走 ``>= 1``，JSON true 会被
        # 当成 1。两者都在入口 fail-closed 成 rc2。
        code = row.get('errorCode')
        if code is not None and (not isinstance(code, str) or not code):
            raise InputError(
                f'{path}: row {index} has a non-string errorCode {code!r}')
        events = row.get('events')
        if events is not None and (not isinstance(events, int) or isinstance(events, bool)):
            raise InputError(
                f'{path}: row {index} has a non-integer events {events!r}')
        rows.append(row)
    return rows, path


def load_frames(round_dir):
    """一轮的 frames.jsonl → 帧列表。只按 "\\n" 切行（splitlines 会在
    U+2028 / U+2029 / U+0085 / \\x0b / \\x0c 处把一条 JSON 记录切成两半）。"""
    path = Path(round_dir) / 'driver' / 'frames.jsonl'
    try:
        text = Path(path).read_text(encoding='utf-8')
    except (OSError, UnicodeDecodeError) as error:
        raise InputError(f'cannot read {path}: {error}') from error
    lines = text.split('\n')
    if lines and lines[-1] == '':
        lines = lines[:-1]
    frames = []
    for number, line in enumerate(lines, 1):
        if not line.strip():
            raise InputError(f'{path}: blank line {number}')
        try:
            frame = json.loads(line)
        except ValueError as error:
            raise InputError(f'{path}: line {number}: {error}') from error
        if not isinstance(frame, dict):
            raise InputError(f'{path}: line {number} is not an object')
        frames.append(frame)
    return frames, path


def index_wire(frames, path):
    """线缆索引：方法 → send id、id → response 帧列表、final:true 事件帧 id 集。

    响应按列表保留：同 id 出现多条 response 是线缆异常（重放/重试重复落盘），
    由 P6 判 FAIL；dict 的 last-wins 会让「先错码后对码」的形状假 PASS。
    """
    send_ids = {}
    responses = {}
    finals = set()
    for frame in frames:
        wire = frame.get('frame')
        if not isinstance(wire, dict):
            continue  # close 等记账行，无 frame 载荷
        direction, kind = frame.get('direction'), wire.get('type')
        if direction == 'send' and kind == 'request':
            identifier, method = wire.get('id'), wire.get('method')
            if not isinstance(identifier, str) or not isinstance(method, str):
                raise InputError(f'{path}: send request frame without id/method')
            send_ids.setdefault(method, []).append(identifier)
        elif direction == 'receive' and kind == 'response':
            identifier = wire.get('id')
            if not isinstance(identifier, str):
                raise InputError(f'{path}: response frame without id')
            responses.setdefault(identifier, []).append(wire)
        elif direction == 'receive' and kind == 'event' and wire.get('final') is True:
            identifier = wire.get('id')
            if not isinstance(identifier, str):
                raise InputError(f'{path}: final event frame without id')
            finals.add(identifier)
    return send_ids, responses, finals


def deferred_match(method, row, golden):
    """该行是否落在**已登记延期**的那一格：金样 ok，Rust 回 CAPABILITY_UNAVAILABLE。

    只有这一格被容忍；调用方不得把本函数当「该方法不用判」的开关——P1/P2/P6/P7/P8
    对延期方法照常核，本函数只为 P3/P4 的逐行比对开一格。P5 是集合规则，按方法集合
    扣除（`DEFERRED_UNAVAILABLE_METHODS`），口径由金样侧 fail-closed 保证一致。
    """
    return (method in DEFERRED_UNAVAILABLE_METHODS
            and golden is not None
            and golden['outcome'] == 'ok' and golden['errorCode'] is None
            and row['outcome'] == 'error'
            and row.get('errorCode') == CAPABILITY_UNAVAILABLE)


def analyze_round(round_name, rows, wire, oracle_index):
    """单轮 P1–P6、P8 判分；P7 跨轮，见 analyze_stability。"""
    send_ids, responses, finals = wire
    violations = []
    stats = {'round': round_name, 'rows': len(rows),
             'executed': 0, 'ok': 0, 'error': 0, 'timeout': 0,
             # 延期命中必须显式可见：计数 + 逐方法，不允许静默跳过。
             'deferred': 0, 'deferredMethods': []}
    seen = set()
    unavailable = set()
    # 一个请求 id 只能属于一个方法：共用 id 时两条方法会读同一条 response，
    # 线缆无法归属（实测：两方法共 id + 单条 response 可整轮 PASS）。
    id_owners = {}
    for owner, identifiers in send_ids.items():
        for identifier in identifiers:
            id_owners.setdefault(identifier, []).append(owner)
    for row in rows:
        method, outcome, shape = row['method'], row['outcome'], row['shape']
        if method in seen:
            violations.append(violation(
                'P1-COVERAGE', round_name, method,
                'duplicate method row (P8 fail-closed: duplicates are never PASS)'))
        seen.add(method)
        if outcome == 'timeout':
            stats['timeout'] += 1
            violations.append(violation(
                'P1-COVERAGE', round_name, method,
                'timeout outcome does not count as executed and is itself FAIL'))
            golden = oracle_index.get(method)
            if golden is not None and golden['outcome'] != outcome:
                violations.append(violation(
                    'P3-OUTCOME', round_name, method,
                    f'outcome {outcome!r} != golden {golden["outcome"]!r} '
                    '(a timeout is never an accepted outcome)'))
            continue
        if outcome not in KNOWN_OUTCOMES:
            violations.append(violation(
                'P8-FAIL-CLOSED', round_name, method,
                f'unknown outcome value {outcome!r} (expected one of {KNOWN_OUTCOMES})'))
            continue
        stats['executed'] += 1
        stats[outcome] += 1
        if outcome == 'error' and row.get('errorCode') == CAPABILITY_UNAVAILABLE:
            unavailable.add(method)
        golden = oracle_index.get(method)
        if golden is None:
            violations.append(violation(
                'P1-COVERAGE', round_name, method,
                'method outside the registered 101-method execution set'))
        else:
            deferred = deferred_match(method, row, golden)
            if deferred:
                stats['deferred'] += 1
                stats['deferredMethods'].append(method)
            if shape != golden['shape']:
                violations.append(violation(
                    'P2-SHAPE', round_name, method,
                    f'shape {shape!r} != golden {golden["shape"]!r}'))
            if outcome != golden['outcome'] and not deferred:
                violations.append(violation(
                    'P3-OUTCOME', round_name, method,
                    f'outcome {outcome!r} != golden {golden["outcome"]!r} '
                    '(no availability-ratio discounting)'))
            if deferred:
                pass  # 已计入 stats['deferred']；P4 只对这一格不比对
            elif outcome == 'error':
                code = row.get('errorCode')
                if not isinstance(code, str) or not code:
                    violations.append(violation(
                        'P4-ERRORCODE', round_name, method,
                        'error row without a non-empty errorCode'))
                elif code != golden['errorCode']:
                    violations.append(violation(
                        'P4-ERRORCODE', round_name, method,
                        f'errorCode {code!r} != golden {golden["errorCode"]!r} '
                        '(case-sensitive, per-method, no code-family approximation)'))
            elif row.get('errorCode') is not None:
                violations.append(violation(
                    'P4-ERRORCODE', round_name, method,
                    f'ok row must not carry errorCode (got {row.get("errorCode")!r})'))
        # P6 对每一行已执行方法都核（不依赖金样）。
        identifiers = send_ids.get(method, [])
        if not identifiers:
            violations.append(violation(
                'P6-WIRE-CODE', round_name, method, 'missing send frame'))
        elif len(identifiers) > 1:
            violations.append(violation(
                'P6-WIRE-CODE', round_name, method,
                f'{len(identifiers)} send frames for one method'))
        else:
            identifier = identifiers[0]
            matching = responses.get(identifier, [])
            shared = sorted(name for name in id_owners.get(identifier, []) if name != method)
            if shared:
                violations.append(violation(
                    'P6-WIRE-CODE', round_name, method,
                    f'request id {identifier} is also used by {shared}: responses '
                    'cannot be attributed to a single method'))
            if len(matching) > 1:
                # 与 send 侧对称：一个请求只允许一条 response。重放/重试重复
                # 落盘会让 last-wins 掩盖「先错码后对码」的线缆不一致。
                violations.append(violation(
                    'P6-WIRE-CODE', round_name, method,
                    f'{len(matching)} response frames for one id (duplicate or '
                    'replayed response; each request must receive exactly one)'))
            elif outcome == 'error':
                if not matching:
                    violations.append(violation(
                        'P6-WIRE-CODE', round_name, method, 'missing response frame'))
                else:
                    response = matching[0]
                    raw_error = error_value(response)
                    error = error_object(response)
                    wire_code = error.get('code') if error else None
                    if response.get('ok') is not False:
                        violations.append(violation(
                            'P6-WIRE-CODE', round_name, method,
                            'response.ok is not false for an error outcome'))
                    if error is None:
                        detail = ('response frame without an error object'
                                  if raw_error is None else
                                  f'response error must be an object (got {raw_error!r})')
                        violations.append(violation(
                            'P6-WIRE-CODE', round_name, method, detail))
                    else:
                        if wire_code != row.get('errorCode'):
                            violations.append(violation(
                                'P6-WIRE-CODE', round_name, method,
                                f'wire error.code {wire_code!r} != '
                                f'outcome.errorCode {row.get("errorCode")!r}'))
                        message = error.get('message')
                        if not isinstance(message, str) or not message:
                            violations.append(violation(
                                'P6-WIRE-CODE', round_name, method,
                                'empty error.message on the wire'))
            elif shape == 'stream':
                if matching:
                    # stream 的收尾帧可省（TS 基线只有 final 事件），但一旦存在
                    # 就必须是成功响应：final:true 事件 + ok:false response 的
                    # 矛盾线缆不得 PASS（实测可整轮 PASS）。
                    response = matching[0]
                    if response.get('ok') is not True:
                        violations.append(violation(
                            'P6-WIRE-CODE', round_name, method,
                            'stream ok with a response whose ok is not true'))
                    if error_value(response) is not None:
                        violations.append(violation(
                            'P6-WIRE-CODE', round_name, method,
                            'stream ok with a response carrying a non-null error '
                            f'({error_value(response)!r}); must be absent or null'))
                if identifier not in finals:
                    violations.append(violation(
                        'P6-WIRE-CODE', round_name, method,
                        'stream ok without a final:true event frame'))
                events = row.get('events')
                if not isinstance(events, int) or events < 1:
                    violations.append(violation(
                        'P6-WIRE-CODE', round_name, method,
                        f'stream ok with events < 1 (got {events!r})'))
            else:
                if not matching:
                    violations.append(violation(
                        'P6-WIRE-CODE', round_name, method, 'missing response frame'))
                else:
                    response = matching[0]
                    if response.get('ok') is not True:
                        violations.append(violation(
                            'P6-WIRE-CODE', round_name, method,
                            'response.ok is not true for an ok outcome'))
                    if error_value(response) is not None:
                        violations.append(violation(
                            'P6-WIRE-CODE', round_name, method,
                            'ok response carries a non-null error '
                            f'({error_value(response)!r}); must be absent or null'))
    executed_names = {row['method'] for row in rows if row['outcome'] in KNOWN_OUTCOMES}
    for method in sorted(set(oracle_index) - executed_names):
        violations.append(violation(
            'P1-COVERAGE', round_name, method,
            'missing from the executed set (registered 101)'))
    # P5：unavailable 集合 ⊆ 登记的 6 个 gated ∪ 已登记延期的 3 个 always-on，
    # 且 6 个 gated 必须**双向恰等**（集合外答 unavailable 即 FAIL，gated 之内答
    # 非 unavailable 也 FAIL；99/101 unavailable 必须 FAIL）。
    # 延期那 3 个答 unavailable 是登记过的既知差异（上面已计入 stats['deferred']），
    # 不是「集合外答 unavailable」，故从集合减法里扣除。
    for method in sorted(unavailable - REGISTERED_UNAVAILABLE_METHODS
                         - DEFERRED_UNAVAILABLE_METHODS):
        violations.append(violation(
            'P5-UNAVAILABLE-SET', round_name, method,
            'answers CAPABILITY_UNAVAILABLE outside the registered gated set '
            '(the old all-or-nothing UNIFORM_UNAVAILABLE semantics is NOT the '
            'Rust-round criteria)'))
    for method in sorted(REGISTERED_UNAVAILABLE_METHODS - unavailable):
        violations.append(violation(
            'P5-UNAVAILABLE-SET', round_name, method,
            'registered gated method did not answer CAPABILITY_UNAVAILABLE'))
    return violations, stats


def analyze_stability(per_round_values):
    """P7：同一轮次集合内每方法 (outcome, errorCode) 逐轮相同。"""
    violations = []
    for method in sorted(per_round_values):
        values = per_round_values[method]
        distinct = {value for value in values.values()}
        if len(distinct) > 1:
            drift = ', '.join(f'{name}={value[0]}/{value[1]}'
                              for name, value in sorted(values.items()))
            rounds = ', '.join(sorted(values))
            violations.append(violation(
                'P7-ROUND-STABILITY', rounds, method,
                f'(outcome, errorCode) drift across rounds: {drift}'))
    return violations


def load_frozen_methods(path=FROZEN_METHODS):
    """冻结 fixture 的 method -> shape 表（101 条）。"""
    data = read_json(path)
    methods = data.get('methods') if isinstance(data, dict) else None
    if not isinstance(methods, list):
        raise InputError(f'{path}: frozen method fixture lacks a methods list')
    shapes = {}
    for entry in methods:
        if not isinstance(entry, dict) or not isinstance(entry.get('method'), str):
            raise InputError(f'{path}: malformed frozen method entry')
        shapes[entry['method']] = entry.get('shape')
    return shapes


def load_oracle(path):
    """金样加载 + 严格校验（fail-closed：任何不符合登记形状即退出码 2）。"""
    data = read_json(path)
    if not isinstance(data, dict):
        raise InputError(f'{path}: oracle is not an object')
    if data.get('schema') != SCHEMA:
        raise InputError(f'{path}: unexpected schema {data.get("schema")!r}')
    baseline = data.get('baseline')
    if not isinstance(baseline, dict) or baseline.get('revision') != BASELINE_REVISION:
        raise InputError(f'{path}: baseline.revision must be {BASELINE_REVISION}')
    rounds = data.get('rounds')
    if not isinstance(rounds, dict):
        raise InputError(f'{path}: missing rounds metadata')
    k = rounds.get('k')
    if not isinstance(k, int) or isinstance(k, bool) or k < 1:
        raise InputError(f'{path}: rounds.k must be a positive integer')
    if not isinstance(rounds.get('dirs'), list) or len(rounds['dirs']) != k:
        raise InputError(f'{path}: rounds.dirs must list k round names')
    files = rounds.get('files')
    if not isinstance(files, list) or len(files) != k:
        raise InputError(f'{path}: rounds.files must list one entry per round')
    for entry in files:
        if (not isinstance(entry, dict) or not isinstance(entry.get('round'), str)
                or not isinstance(entry.get('methodsSha256'), str)
                or not isinstance(entry.get('framesSha256'), str)):
            raise InputError(f'{path}: malformed rounds.files entry')
    methods = data.get('methods')
    if not isinstance(methods, list) or len(methods) != EXPECTED_METHOD_COUNT:
        raise InputError(
            f'{path}: oracle must hold exactly {EXPECTED_METHOD_COUNT} rows, '
            f'got {len(methods) if isinstance(methods, list) else "non-list"}')
    index = {}
    for position, row in enumerate(methods):
        if not isinstance(row, dict):
            raise InputError(f'{path}: methods[{position}] is not an object')
        method = row.get('method')
        if not isinstance(method, str) or not method:
            raise InputError(f'{path}: methods[{position}] lacks a method name')
        if method in index:
            raise InputError(f'{path}: duplicate oracle method {method!r}')
        shape, outcome = row.get('shape'), row.get('outcome')
        if shape not in KNOWN_SHAPES:
            raise InputError(f'{path}: {method}: shape {shape!r} not in {KNOWN_SHAPES}')
        if outcome not in KNOWN_OUTCOMES:
            raise InputError(f'{path}: {method}: outcome {outcome!r} not in {KNOWN_OUTCOMES}')
        error_code = row.get('errorCode')
        wire_code = row.get('wireCode')
        if outcome == 'error':
            if not isinstance(error_code, str) or not error_code:
                raise InputError(f'{path}: {method}: error row without errorCode')
            if wire_code != error_code:
                raise InputError(f'{path}: {method}: wireCode {wire_code!r} != errorCode')
        else:
            if error_code is not None:
                raise InputError(f'{path}: {method}: ok row must not carry errorCode')
            if wire_code is not None:
                raise InputError(f'{path}: {method}: ok row must not carry wireCode')
        index[method] = {'shape': shape, 'outcome': outcome, 'errorCode': error_code}
    unavailable = {method for method, row in index.items()
                    if row['outcome'] == 'error' and row['errorCode'] == CAPABILITY_UNAVAILABLE}
    if unavailable != REGISTERED_UNAVAILABLE_METHODS:
        raise InputError(
            f'{path}: oracle unavailable set {sorted(unavailable)} does not match '
            f'the registered gated set {sorted(REGISTERED_UNAVAILABLE_METHODS)}')
    # 延期集合的登记前提（fail-closed）：金样里必须是 `ok`（TS 基线的真实回答，
    # #2143 实测），且与 gated 集不相交、全部在 101 条里。任一漂移说明登记与数据
    # 打架：此时继续跑会让延期变成「对未知形状的静默放行」。
    overlap = sorted(DEFERRED_UNAVAILABLE_METHODS & REGISTERED_UNAVAILABLE_METHODS)
    if overlap:
        raise InputError(f'{path}: deferred methods {overlap} also belong to the '
                         'registered gated set')
    for method in sorted(DEFERRED_UNAVAILABLE_METHODS):
        row = index.get(method)
        if row is None:
            raise InputError(f'{path}: deferred method {method!r} is missing from '
                             f'the {EXPECTED_METHOD_COUNT}-method golden')
        if row['outcome'] != 'ok' or row['errorCode'] is not None:
            raise InputError(
                f'{path}: deferred method {method!r} is registered as '
                f'{row["outcome"]}/{row["errorCode"]!r}; the deferral only covers '
                'the TS baseline answering ok (a different golden row is a registry '
                'drift, not something to tolerate)')
    assert_matches_frozen({method: row['shape'] for method, row in index.items()},
                          str(path))
    return data, index


def assert_matches_frozen(shapes, label):
    """方法集（名 + shape）必须与冻结 fixture 逐条相同，否则 rc2。"""
    frozen = load_frozen_methods()
    if shapes == frozen:
        return
    missing = sorted(set(frozen) - set(shapes))
    extra = sorted(set(shapes) - set(frozen))
    differ = sorted(name for name in set(frozen) & set(shapes)
                    if frozen[name] != shapes[name])
    raise InputError(
        f'{label}: method set differs from the frozen fixture {FROZEN_METHODS} '
        f'(missing={missing[:5]}, extra={extra[:5]}, shapeMismatch={differ[:5]})')


def load_round_bundle(round_dir):
    rows, methods_path = load_rows(round_dir)
    frames, frames_path = load_frames(round_dir)
    wire = index_wire(frames, frames_path)
    return {'dir': Path(round_dir), 'rows': rows, 'frames': frames, 'wire': wire,
            'methods_sha256': sha256_file(methods_path),
            'frames_sha256': sha256_file(frames_path)}


def ensure_unique_round_names(bundles):
    """P7 以轮次目录名为键：重名轮次会静默合并，跨轮漂移检测被架空。"""
    names = [bundle['dir'].name for bundle in bundles]
    duplicates = sorted({name for name in names if names.count(name) > 1})
    if duplicates:
        raise InputError(
            f'duplicate round directory names {duplicates}: rounds are keyed by '
            'directory name in P7; pass distinct round directories')


def stability_values(bundles):
    values = {}
    for bundle in bundles:
        for row in bundle['rows']:
            if row['outcome'] in KNOWN_OUTCOMES:
                values.setdefault(row['method'], {})[bundle['dir'].name] = \
                    (row['outcome'], row.get('errorCode'))
    return values


def emit_report(report):
    print(json.dumps(report, ensure_ascii=False, indent=2))
    return EXIT_PASS if report['status'] == 'PASS' else EXIT_FAIL


def run_emit(args):
    bundles = [load_round_bundle(path) for path in args.round]
    if len(bundles) < 1:
        raise InputError('emit needs at least one --round')
    ensure_unique_round_names(bundles)
    # 用第一轮自身做自洽金样，跑全套单轮判分（P1/P5/P6/重复行 + 未知值）。
    self_index = {row['method']: {'shape': row['shape'], 'outcome': row['outcome'],
                                  'errorCode': row.get('errorCode')}
                  for row in bundles[0]['rows']}
    if len(self_index) != EXPECTED_METHOD_COUNT:
        raise InputError(
            f'first round holds {len(self_index)} methods, expected {EXPECTED_METHOD_COUNT}')
    # 金样只能从 TS 基线的真实回答派生：延期方法的金样行必须是 `ok`。若拿一轮
    # 「延期方法答 CAPABILITY_UNAVAILABLE」的数据去 emit，延期分支会让它自洽通过，
    # 从而把 Rust 差异写进金样 —— 这里 fail-closed 挡住。
    for method in sorted(DEFERRED_UNAVAILABLE_METHODS):
        row = self_index.get(method)
        if row is None:
            raise InputError(f'emit: first round lacks deferred method {method!r}')
        if row['outcome'] != 'ok' or row['errorCode'] is not None:
            raise InputError(
                f'emit: {method!r} answered {row["outcome"]}/{row["errorCode"]!r} in '
                'the first round; the golden must come from the TS baseline where '
                'it answers ok (a deferred divergence is never a golden row)')
    # 生成侧也核：金样是第 5 份登记表，生成时同样不得与冻结 fixture 漂移。
    assert_matches_frozen({row['method']: row['shape'] for row in bundles[0]['rows']},
                          str(bundles[0]['dir']))
    violations = []
    stats = []
    for bundle in bundles:
        round_violations, round_stats = analyze_round(
            bundle['dir'].name, bundle['rows'], bundle['wire'], self_index)
        violations.extend(round_violations)
        stats.append(round_stats)
    violations.extend(analyze_stability(stability_values(bundles)))
    if violations:
        return emit_report({
            'status': 'FAIL',
            'command': 'emit',
            'reason': 'reference rounds violate the registered rules; '
                      'a golden sample cannot be derived from them',
            'rounds': stats, 'violations': violations,
            'violationsCount': len(violations)})
    first = bundles[0]
    wire_codes = {}
    send_ids, responses, _ = first['wire']
    for row in first['rows']:
        method = row['method']
        identifier = send_ids[method][0]
        if row['outcome'] == 'error':
            # 单帧已在 analyze_round 里强制（重复 response 即 FAIL），此处取唯一一帧。
            error = responses[identifier][0].get('error')
            wire_codes[method] = error.get('code') if isinstance(error, dict) else None
        else:
            wire_codes[method] = None
    payload = {
        'schema': SCHEMA,
        'baseline': {'revision': BASELINE_REVISION, 'node': NODE_VERSION,
                     'suite': 'retirement'},
        'rounds': {
            'k': len(bundles),
            'dirs': [bundle['dir'].name for bundle in bundles],
            'files': [{'round': bundle['dir'].name,
                       'methodsSha256': bundle['methods_sha256'],
                       'framesSha256': bundle['frames_sha256']}
                      for bundle in bundles],
        },
        'methods': [{'method': row['method'], 'shape': row['shape'],
                     'outcome': row['outcome'], 'errorCode': row.get('errorCode'),
                     'wireCode': wire_codes[row['method']]}
                    for row in first['rows']],
    }
    target = Path(args.oracle)
    digest = None
    # 写金样失败同样是「输入/环境不可用」（rc 2），不是判据 FAIL（rc 1）：`--oracle=`
    # （`Path('') == Path('.')`）、不可写路径、目标是目录都会抛 OSError / ValueError，
    # 不接住就是裸 traceback → rc1。
    try:
        target.parent.mkdir(parents=True, exist_ok=True)
        # 原子发布（与负控 summary.json 同一口径）：中断不留截断的金样。tmp 名取
        # 不可预测的（`<name>.<pid>.tmp` 在世界可写目录里可被预置符号链接，写入会
        # 跟随它截断目标）；任何失败都先删 tmp，再按输入不可用收口（两家评审 r10）。
        handle, tmp_name = tempfile.mkstemp(
            dir=target.parent, prefix=target.name + '.', suffix='.tmp')
        tmp = Path(tmp_name)
        try:
            with os.fdopen(handle, 'w', encoding='utf-8') as stream:
                stream.write(json.dumps(payload, ensure_ascii=False, indent=2) + '\n')
            os.replace(tmp, target)
        except BaseException:
            tmp.unlink(missing_ok=True)
            raise
        # 摘要也算写金样的一部分：读到 OSError 同样不是判据 FAIL。
        digest = sha256_file(target)
    except (OSError, ValueError) as error:
        raise InputError(f'cannot write oracle {args.oracle!r}: {error}') from error
    print(json.dumps({'status': 'PASS', 'command': 'emit', 'oracle': str(target),
                      'methods': len(payload['methods']),
                      'sha256': digest,
                      'rounds': stats}, ensure_ascii=False, indent=2))
    return EXIT_PASS


def run_check(args):
    _, oracle_index = load_oracle(args.oracle)
    bundles = [load_round_bundle(path) for path in args.round]
    ensure_unique_round_names(bundles)
    violations = []
    stats = []
    for bundle in bundles:
        round_violations, round_stats = analyze_round(
            bundle['dir'].name, bundle['rows'], bundle['wire'], oracle_index)
        violations.extend(round_violations)
        stats.append(round_stats)
    violations.extend(analyze_stability(stability_values(bundles)))
    report = {
        'status': 'PASS' if not violations else 'FAIL',
        'command': 'check',
        'oracle': {'path': str(args.oracle), 'methods': len(oracle_index)},
        # 已登记延期（不是豁免）：显式列出方法集与理由，逐轮命中数见 rounds[].deferred。
        'deferred': {'methods': sorted(DEFERRED_UNAVAILABLE_METHODS),
                     'reason': DEFERRED_REASON,
                     'registration': 'specs/gateway-parity/diffs/'
                                     'A8.6-RETIREMENT-OUTCOME-PARITY-001-R1.md'},
        'rounds': stats,
        # 单轮时 P7 是空判（每个方法只有一项，distinct 恒为 1）：PASS 不等于
        # 「跨轮稳定已核」。报告显式给出评估轮数，下游不得把 1 轮当 P7 证据。
        'p7Rounds': len(bundles),
        'p7Evaluated': len(bundles) > 1,
        'violations': violations,
        'violationsCount': len(violations),
    }
    return emit_report(report)


def main(argv=None):
    parser = argparse.ArgumentParser(
        description='Retirement outcome-parity checker (#1477); rules P1-P8 are '
                    'registered in specs/gateway-parity/diffs/'
                    'A8.6-RETIREMENT-OUTCOME-PARITY-001.md')
    sub = parser.add_subparsers(dest='command', required=True)
    for name, runner in (('emit', run_emit), ('check', run_check)):
        subparser = sub.add_parser(name)
        subparser.add_argument('--oracle', required=True,
                                help='ts-oracle.json path (emit writes, check reads)')
        subparser.add_argument('--round', action='append', required=True,
                               help='round directory (driver/methods.json + '
                                    'driver/frames.jsonl); repeatable')
        subparser.set_defaults(runner=runner)
    args = parser.parse_args(argv)
    try:
        return args.runner(args)
    except InputError as error:
        print(json.dumps({'status': 'INPUT_ERROR', 'command': args.command,
                          'error': str(error)}, ensure_ascii=False, indent=2))
        return EXIT_INPUT


if __name__ == '__main__':
    sys.exit(main())
