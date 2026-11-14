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
import atexit
import contextlib
import hashlib
import json
import os
import shutil
import stat
import tempfile
from pathlib import Path

# `fcntl` 只在 POSIX 有。Windows 缺 `O_NOFOLLOW` 时本装置本来就拒跑（差异条目
# 已知边界⑳，平台支持另立单），这里同样按「跑不了」收口成 rc2，不做降级。
try:
    import fcntl
except ImportError:  # pragma: no cover - 非 POSIX 平台
    fcntl = None

# 输出目录互斥（#1809 条目 4）。同一个输出根被两个进程同时写时，
# `N*/driver/*` 会被原地交错截断，审计者可据此拼出「N10 KILLED」而本轮并没有
# 真正跑完任何控件。锁文件放在被写的目录**里面**：跟着目录走，不需要调用方
# 另记一个路径；名字带点前缀，不与控件目录 `N1..N17` / `summary.json` 冲突。
LOCK_BASENAME = '.retirement-outcome-parity.lock'

# 同族装置口径（scripts/gateway-parity/*、scripts/parity/* 均成对设置）：不写也
# **不读**缓存字节码。只设 dont_write_bytecode 挡不住读取已有的
# __pycache__/*.pyc（PEP 552 unchecked-hash 可直接执行），而本目录会被负控以
# spec_from_file_location 再加载一次——陈旧字节码会让「跑的是当前源码」不成立。
# #2452：自建前缀注册 atexit 清理（rmtree 容错）——本模块被 self_check /
# negative_controls 在进程内 exec，也被 self_check 的三个子进程探针按 spec 加载；
# 此前每次加载各泄漏一个常驻空临时目录（$TMPDIR 无界累积）。目录由 mkdtemp 独占
# 创建（mode 700）且 dont_write_bytecode 下恒为空，退出时 rmtree 只会删掉自己的
# 空目录。OSError 回退路径核实不存在，不注册。
try:
    sys.pycache_prefix = tempfile.mkdtemp(prefix='pdrs-retirement-pyc-')
    atexit.register(shutil.rmtree, sys.pycache_prefix, ignore_errors=True)
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

# 负控件名册（N1–N17 全集）与正控件名册（当前空集）：本文件是三件套共享的
# **单一登记点**——negative_controls 的执行器（名册 / 条数守卫）与 self_check 的
# check_negctl（钉 KILLED / HELD 数与 ID 名册）都从这里读，不各抄一份。评审
# r7 P1：控件集被删到 0 条时执行器自己判 PASS rc=0 的空真；r9 合并补全：正控
# 同样需要守卫——build_positives 被删空时 positiveCounts 全 0，status 仍 PASS。
# #2378：**条数拦不住改名**（N5 改名成已有 ID 后一重复一缺失、条数不变）——
# 登记值从条数升级为字面 ID 全集，set(实际 ids) != 名册即拒绝（rc2），
# 绝不缩小分母。派生条数仅供既有消费者（self_check 的 retirement_budget_rows）
# 继续用；新代码一律比对 ID 名册，不比条数。A8.6-R1 的延期正控 PC1–PC2 与
# 负控 N18 已随 #2294 always-on 落地按反转条款撤回
# （diffs/A8.6-RETIREMENT-OUTCOME-PARITY-001-R2.md），名册随之收敛；将来
# 再登记正控时改 EXPECTED_POSITIVE_IDS 即可。
EXPECTED_CONTROL_IDS = frozenset((
    'N1', 'N2', 'N3', 'N4', 'N5', 'N6', 'N7', 'N8', 'N9', 'N10',
    'N11', 'N12', 'N13', 'N14', 'N15', 'N16', 'N17'))
EXPECTED_POSITIVE_IDS = frozenset()
EXPECTED_CONTROLS = len(EXPECTED_CONTROL_IDS)
EXPECTED_POSITIVES = len(EXPECTED_POSITIVE_IDS)

# 判决词表（r4 sibling#1）：单一登记点，执行器（negative_controls.py）与
# self_check.py 同源读 CHECK 取这份，不各抄字面量——词表是「折叠 + 计数 +
# 期望 counts」三方共用的键集，任何一侧漏改都会让词表外判决重新落进
# 「两个零」空真（#2378 正文的形态）。
NEGATIVE_VERDICTS = ('KILLED', 'SURVIVED', 'INVALID')
POSITIVE_VERDICTS = ('HELD', 'BROKEN', 'INVALID')

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
KNOWN_OUTCOMES = ('ok', 'error')
KNOWN_SHAPES = ('request', 'stream')


# 本装置的金样是第 5 份「101 方法」登记表；冻结 fixture 是四源集合的落点，
# 二者必须逐名同形（否则单边漂移会让 P1 的期望集失效：漏跑新方法时
# missing=∅ 仍 PASS —— 假 PASS；反向漂移则假 FAIL）。
FROZEN_METHODS = (Path(__file__).resolve().parent.parent
                  / 'gateway-parity' / 'fixtures' / 'retirement-methods.json')


class InputError(Exception):
    """输入不可用 —— fail-closed，退出码 2，绝不当 PASS。"""


@contextlib.contextmanager
def output_lock(root):
    """对 ``root`` 目录加**独占** flock（跨进程，释放即解锁）。

    * 被占时**阻塞等待**，不是失败：并发跑两次是本装置的合法用法（CI 与本机各一次、
      或审计者边跑边看），明确失败会让人误判成数据问题；等锁期间在 stderr 打一行
      提示，观测上不静默。
    * 锁文件 ``<root>/.retirement-outcome-parity.lock`` 用 ``O_NOFOLLOW`` 打开：
      预置成指向别处的符号链接直接 ``ELOOP`` 失败（与负控 `driver/*` 同一写盘模型），
      绝不顺着链接去锁别人。
    * ``flock`` 锁是**打开文件描述**的属性，不经由 fd 继承传给子进程时不生效；
      本装置只在进程内直接调用，不经 sh -c。
    * 缺 ``fcntl``（非 POSIX）或 ``O_NOFOLLOW`` 时按 rc2 收口——不静默退化成「没锁」。
    """
    if fcntl is None:  # pragma: no cover - 非 POSIX 平台
        raise InputError('flock is unavailable on this platform; refusing to write '
                         f'{root} without output-directory mutual exclusion')
    if not hasattr(os, 'O_NOFOLLOW'):  # pragma: no cover - 非 POSIX 平台
        raise InputError('O_NOFOLLOW is unavailable on this platform; refusing to '
                         f'lock {root} (see diffs/A8.6-RETIREMENT-OUTCOME-PARITY-001.md)')
    root = Path(root)
    try:
        root.mkdir(parents=True, exist_ok=True)
        path = root / LOCK_BASENAME
        # `O_RDONLY` 足够：`flock` 只要求一个打开的文件描述，不看读写模式。用只读打开
        # 是为了让**别人**建在共享临时目录（`/tmp`、`$TMPDIR`）里的 0644 锁文件也能被
        # 本进程取到——`O_RDWR` 会在跨账号共享目录上直接 `EACCES`，把「等锁」变成
        # 「跑不了」。`O_NOFOLLOW` 挡住预置成指向别处的链接。
        # `O_NONBLOCK`：预置同名 **FIFO** 时，不带它的只读打开会一直等写端（`O_NOFOLLOW`
        # 拦不住 FIFO），整个装置挂死；加它后 FIFO 打开立即返回，再由下面的 `fstat`
        # 判成非普通文件 → rc2。普通文件上 `O_NONBLOCK` 是空操作。
        fd = os.open(path, os.O_RDONLY | os.O_CREAT | os.O_NOFOLLOW | os.O_NONBLOCK
                     | getattr(os, 'O_CLOEXEC', 0), 0o644)
    except OSError as error:
        raise InputError(f'cannot open lock {root / LOCK_BASENAME}: {error}') from error
    try:
        info = os.fstat(fd)
        if not stat.S_ISREG(info.st_mode):
            # 目录 / FIFO / 设备：都不是锁文件。目录虽然 `flock` 也能锁，但那是另一套
            # 语义（谁建的目录谁以为在被锁），一律拒跑更安全。
            raise InputError(f'lock {path} is not a regular file '
                             f'(mode {info.st_mode:o}); refusing to use it as a lock')
    except BaseException:
        os.close(fd)
        raise
    locked = False
    try:
        # 创建时的 `0o644` 会被 umask 折损（`umask 077` 得 0600），跨账号共享临时目录里
        # 后来者再 `O_RDONLY` 打开就 `EACCES`。显式 `fchmod` 一次，让注释里「共享目录
        # 也能等锁」的承诺真正成立。`fchmod` 失败不改变互斥语义（同账号场景照常工作），
        # 因此只吞掉，不把「能锁但改不了权限」误判成输入不可用。
        try:
            os.fchmod(fd, 0o644)
        except OSError:  # pragma: no cover - 极少数 FS 不支持 fchmod
            pass
        try:
            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            print(f'retirement-outcome-parity: waiting for output lock {path} '
                  '(held by another process)', file=sys.stderr)
            try:
                fcntl.flock(fd, fcntl.LOCK_EX)
            except OSError as error:
                raise InputError(f'cannot take lock {path}: {error}') from error
        except OSError as error:
            # `ENOLCK` / `EOPNOTSUPP`（异构 FS 上 flock 不可用）与「缺 fcntl」「缺
            # O_NOFOLLOW」同一口径：rc2「跑不了」，不是裸 traceback 冒充判据 FAIL。
            raise InputError(f'cannot take lock {path}: {error}') from error
        locked = True
        yield path
    finally:
        # 只在**真的取到锁**之后解锁：取锁失败时 `LOCK_UN` 也会以同一个 errno 失败，
        # 抛出来就是裸 OSError 覆盖上面刚折好的 InputError（负控入口只捕 InputError）。
        # 清理本身的失败一律吞掉——`close(fd)` 无论如何都会释放 flock。
        if locked:
            try:
                fcntl.flock(fd, fcntl.LOCK_UN)
            except OSError:  # pragma: no cover - 解锁失败只影响提示，不影响正确性
                pass
        try:
            os.close(fd)
        except OSError:  # pragma: no cover - 同上
            pass


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


def analyze_round(round_name, rows, wire, oracle_index):
    """单轮 P1–P6、P8 判分；P7 跨轮，见 analyze_stability。"""
    send_ids, responses, finals = wire
    violations = []
    stats = {'round': round_name, 'rows': len(rows),
             'executed': 0, 'ok': 0, 'error': 0, 'timeout': 0}
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
            if shape != golden['shape']:
                violations.append(violation(
                    'P2-SHAPE', round_name, method,
                    f'shape {shape!r} != golden {golden["shape"]!r}'))
            if outcome != golden['outcome']:
                violations.append(violation(
                    'P3-OUTCOME', round_name, method,
                    f'outcome {outcome!r} != golden {golden["outcome"]!r} '
                    '(no availability-ratio discounting)'))
            if outcome == 'error':
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
    # P5：unavailable 集合恰好等于登记的 6 个（集合外答 unavailable 即 FAIL，
    # 6 个之内答非 unavailable 也 FAIL；99/101 unavailable 必须 FAIL）。
    for method in sorted(unavailable - REGISTERED_UNAVAILABLE_METHODS):
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


def publish_json(target, payload):
    """金样的原子发布（tmp 写入 + `os.replace`，与负控 summary.json 同一口径）。

    fix-r15（DS r14 sibling#3 / MM sibling#1 / MM boundary#1 三撞，P2a）：从
    run_emit 提到模块级，供 self_check 的发布失败孪生直接驱动——三份 check_frozen
    的同款不变量已有 frozen-manifest 夹具的孪生+负控，emit 侧此前是裸的。tmp 名取
    不可预测的（`<name>.<pid>.tmp` 在世界可写目录里可被预置符号链接，写入会跟随
    它截断目标）；任何失败都先删 tmp，再按输入不可用收口（两家评审 r10）。
    """
    handle, tmp_name = tempfile.mkstemp(
        dir=target.parent, prefix=target.name + '.', suffix='.tmp')
    tmp = Path(tmp_name)
    try:
        with os.fdopen(handle, 'w', encoding='utf-8') as stream:
            stream.write(json.dumps(payload, ensure_ascii=False, indent=2) + '\n')
        os.replace(tmp, target)
    except BaseException:
        # fix-r14（DS r13 sibling#2 P2a，与 g03/check_frozen r13-r14 同族）：清理
        # unlink 是唯一吞掉的错——失败路径清理错不得顶掉原发布错（否则外层
        # InputError 报的是清理失败而非 os.replace 的真实失败，归因被掩蔽）。
        from contextlib import suppress
        with suppress(OSError):
            tmp.unlink(missing_ok=True)
        raise


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
        # 与负控共用同一把输出目录锁（`output_lock`）：并发两次 `emit` 写同一目录
        # 时后到者等前一个发布完，不会读到半写的金样再 `os.replace` 覆盖。
        # 原子发布（与负控 summary.json 同一口径）：中断不留截断的金样。tmp 名取
        # 不可预测的（`<name>.<pid>.tmp` 在世界可写目录里可被预置符号链接，写入会
        # 跟随它截断目标）；任何失败都先删 tmp，再按输入不可用收口（两家评审 r10）。
        with output_lock(target.parent):
            publish_json(target, payload)
            # 摘要也算写金样的一部分：读到 OSError 同样不是判据 FAIL。必须在**锁内**
            # 算——出了锁再读，排队的下一个 `emit` 可能已 `os.replace` 成自己的产物，
            # 报告的 `sha256` 就会归因到别人的写入。
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
