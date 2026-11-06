#!/usr/bin/env python3
"""Retirement outcome-parity 负控执行器（issue #1477 §6.2，N1–N17）。

只做数据变异：从 TS 参考数据加载轮次，内存里改 methods / frames，写到输出目录下
的临时轮次目录，再以子进程跑 ``check_outcome_parity.py check``。**不改任何源码
文本**（不触碰仓库任何被跟踪文件，也就不进文本锚点体系）。

判据分两步（§6.3）：
  1. 注入是否落地 —— 落盘文件重新解析后核对被注入维度确实变了（没落地记 INVALID）；
  2. 归因是否落到被注入对象 —— 期望规则 fire 且 violation 点名被注入的方法
     （规则名可能因该轮自有违规而 fire，那种情形不算 KILLED）。

逐条记 KILLED / SURVIVED / INVALID + 原因，**全部跑完再汇总**（不遇第一个失败就
中止）；任一非 KILLED 即非零退出；``summary.json`` 原子发布（先写 ``*.tmp`` 再
``rename``，缺失即失败）。

退出码与 checker 同口径：0 全 KILLED / 1 有非 KILLED / 2 输入或环境不可用
（缺 ``-I``、checker 加载失败、输出目录写不进去）——rc2 绝不能被读成「判据 FAIL」。
"""
from __future__ import annotations

import os
import sys

# ---------------------------------------------------------------------------
# 启动期第一件事：失效同一输出目录里上一轮的 ``summary.json``。
#
# 下游以「summary.json 存在」为「负控已跑完」的证据，所以**任何**早退路径都不得
# 把上一轮的 PASS 留在盘上：缺 ``-I``、checker 导入失败、argparse 报错（拼错 /
# 缺值 / 多余 token）、读数据失败、preflight 非 0。因此这段只依赖 ``os`` / ``sys``，
# 放在模块体最前面，早于 ``-I`` 断言、早于 ``load_checker()``、早于 pycache 目录
# 设置（那一步在无可写临时目录时也可能退出）。
#
# 输出目录只从**显式** ``--output`` 取：解析器 ``allow_abbrev=False``，选项名
# 精确匹配，解析前即可无歧义地取出。不按「哪些 token 可能是位置参数」猜候选——
# 未知选项带不带值在解析前不可判定，猜错的代价是删掉别人的证据（评审 P0，误删同样
# 算丢数据）；没写 ``--output`` 就一个都不删。
#
# 帮助路径同样失效（**不再猜会走哪条分支**）：只要命令里显式写了输出目录，无论
# argparse 之后是跑、是 ``-h``、还是报错，那个目录的旧 PASS 都已报废。跳过帮助等于
# 把「哪些形状算帮助」再猜一遍（``-hh`` / ``--help=x`` / 被带值选项吃掉），而猜错的
# 方向都是留旧 PASS 冒充本轮（两家评审 P0）。
#
# 值 = 紧跟 ``--output`` 的**那个** token；它若不是值（见下），就算这次 ``--output``
# 缺值、不再向后找（``--output --data DIR`` 里的 DIR 是 ``--data`` 的值，argparse 在
# 第一个 ``--output`` 上就报错）。三种 token 不是值：
#   ① 本解析器认识的选项拼写（``allow_abbrev=False``，精确匹配）；
#   ② **两个 CPython 版本都判成选项**的形状（``-`` 后是 ``-`` 或字母、且不含空格：
#      ``-hh`` / ``-h=x`` / ``-x`` / ``--he`` / ``-e5``）——3.12 与 3.14 实测
#      ``--output <token>`` 都报 ``expected one argument``，当值只会去删同名目录；
#   ③ 空 token（``Path('') == Path('.')``，见 ``main`` 的空值判断）。
# 两版结论**相反**的只有「``-`` 后紧跟数字或点」这一小撮（``-1x`` / ``-1e3``：3.12
# 当选项、3.14 当值），一律按值——漏收的方向是留旧 PASS 冒充本轮，误删只是少一份可
# 复算的旧结果，代价不对称。所有显式目录**逐个都试一遍**再退出，前一个不可删不短路
# 后面的。
# ---------------------------------------------------------------------------

VALUE_OPTIONS = ('--output', '--data', '--oracle')
HELP_OPTIONS = ('-h', '--help')
# 选项拼写的唯一来源：``main`` 的解析器用 ``PARSER_OPTIONS`` 建、``is_known_option``
# 用 ``KNOWN_OPTIONS`` 判，两者在 ``build_parser`` 里断言相等（评审 P2：手抄副本无守卫）。
KNOWN_OPTIONS = VALUE_OPTIONS + HELP_OPTIONS


def tmp_owner_pid(name):
    """``summary.json.<pid>.tmp`` 里的 pid；不是这个形状返回 None。"""
    prefix, suffix = 'summary.json.', '.tmp'
    if not (name.startswith(prefix) and name.endswith(suffix)):
        return None
    middle = name[len(prefix):-len(suffix)]
    return int(middle) if middle.isdigit() else None


def pid_alive(pid):
    """该 pid 是否仍是活进程（判断 tmp 是不是**另一个仍在跑**的执行器的）。"""
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except OverflowError:
        # 名字里的「pid」大到超出 C int（人为植入的 tmp）：不是活进程，按垃圾处理。
        # 不接住会让模块体的失效段整段抛异常 → rc1 traceback 且后面的目录不再失效。
        return False
    except OSError:
        return True
    return True


def is_own_summary_artifact(name):
    """只认本执行器自己的产物名：精确 ``summary.json``（含旧版 ``summary.json.tmp``）
    与 ``summary.json.<pid>.tmp``。

    早先的 ``glob('summary.json*')`` 会把 ``summary.json.backup`` /
    ``summary.json.notes`` 这类既有文件一起 ``unlink``——失效旧结果不得顺手删掉
    别人的证据（评审 P0）。
    """
    return name in ('summary.json', 'summary.json.tmp') or tmp_owner_pid(name) is not None


def invalidate_stale_summary(out_root):
    """删掉该目录里上一轮的 ``summary.json`` 与残留 tmp。

    只动本执行器自己的产物名；另一个**仍在跑**的执行器的
    ``summary.json.<pid>.tmp`` 不删（删掉它会让对方的原子发布 ``replace`` 抛
    ``FileNotFoundError``；并发互斥的残余登记在跟进单）。目录不存在时**不创建**。
    """
    root = os.fspath(out_root)
    try:
        if not os.path.exists(root):
            return True  # 新目录：没有旧结果可失效，也不必替调用方创建
        if not os.path.isdir(root):
            print(f'negative_controls: {root} is not a directory', file=sys.stderr)
            return False
        entries = sorted(os.listdir(root))
    except OSError as error:
        print(f'negative_controls: cannot use {root}: {error}', file=sys.stderr)
        return False
    removed = []
    for name in entries:
        path = os.path.join(root, name)
        if not (os.path.isfile(path) and is_own_summary_artifact(name)):
            continue
        owner = tmp_owner_pid(name)
        if owner is not None and owner != os.getpid() and pid_alive(owner):
            print(f'negative_controls: keeping live {name} (pid {owner} runs)',
                  file=sys.stderr)
            continue
        try:
            os.unlink(path)
        except OSError as error:
            print(f'negative_controls: cannot remove {path}: {error}', file=sys.stderr)
            return False
        removed.append(name)
    if removed:
        print(f'removed stale {", ".join(removed)}', file=sys.stderr)
    return True


def is_known_option(token):
    """本解析器认识的选项拼写（``allow_abbrev=False``，精确匹配）。取值规则见模块头。"""
    return any(token == option or token.startswith(option + '=')
               for option in KNOWN_OPTIONS)


def is_certainly_option(token):
    """**3.12 与 3.14 都会判成选项**的形状，因此不当 ``--output`` 的值。理由见模块头。

    ``-hh`` / ``-h=x`` / ``-x`` / ``--he`` / ``--1`` / ``-e5`` 实测两版都是选项
    （``--output <token>`` 报 ``expected one argument``）；当值只会去失效一个同名目录
    （Codex r10 P0）。含空格的两版都当值（``-x y``）；``-`` 开头的负数仍是值；两版
    **结论相反**的 ``-1x`` / ``-1e3`` / ``-1_000`` 按值（见模块头的代价不对称）。
    """
    if not token.startswith('-') or len(token) == 1:
        return False
    if ' ' in token:
        return False
    second = token[1]
    if second == '-':
        return True
    if second == '.':
        # ``-.5`` 两版都是值；``-.`` / ``-..`` 两版都是选项
        return not (len(token) > 2 and token[2].isdigit())
    return not second.isdigit()


def early_outputs(argv):
    """解析前从原始 argv 取**所有**显式写出的非空 ``--output`` 值。

    只认两字不差的 ``--output`` / ``--output=``：不猜缩写、不猜位置参数——那些猜测
    正是前几轮误删别人证据的来源（评审 P0）。

    值 = **紧跟** ``--output`` 的那个 token，且它不是已知选项、不是两版都判成选项的
    形状（``is_certainly_option``）、也不是空值；否则视为这次 ``--output`` 缺值，
    **不再向后找**（``--output --data DIR`` 的 DIR 属于 ``--data``）。已知选项不当值，
    但要**重新按 token 处理一遍**：``--output --output=DIR`` 的 DIR 仍被收集（argparse
    在第一个 ``--output`` 上报错，DIR 是这条命令行里显式点名的）。空值（``--output=``
    / ``--output ''``）不收集——``Path('')`` 就是当前工作目录，绝不能把 CWD 当输出
    目录失效（两家评审：会删掉调用者目录里的 summary）；``main`` 另行判参数错误 rc2。

    重复写多次时**每个非空值都收集**：argparse 只有最后一个生效，但这条命令终究没跑完，
    被显式点名的目录都不该留旧 PASS。
    """
    values = []
    expecting = False
    for token in argv:
        if token == '--':
            break
        if expecting:
            expecting = False
            if (token and not is_known_option(token)
                    and not is_certainly_option(token)):
                values.append(token)
                continue
            # 不是值：不收集，落下去按选项再处理这个 token。
        if token == '--output':
            expecting = True
        elif token.startswith('--output='):
            inline = token[len('--output='):]
            if inline:
                values.append(inline)
    return values


# 只在作为脚本运行时失效：被 import 时不得拿宿主的 sys.argv 去删任何东西
# （评审 P0：导入模块即删除调用者证据）。这段仍早于 ``-I`` 断言与
# ``load_checker()``——那些启动依赖一失败就到不了失效，旧 PASS 会冒充本轮。
# **逐个都试一遍再退出**：前一个目录不可删（如 ``/dev/null``）不能让后面显式点名的
# 目录跳过失效（两家评审：短路会让后者旧 PASS 冒充本轮）。
if __name__ == '__main__':
    _failures = [path for path in early_outputs(sys.argv[1:])
                 if not invalidate_stale_summary(path)]
    if _failures:
        raise SystemExit(2)

if not sys.flags.isolated:
    print(f'negative_controls: run with -I: python3 -I {__file__}', file=sys.stderr)
    raise SystemExit(2)

import argparse
import importlib.util
import json
import subprocess
import tempfile
from pathlib import Path

# 同族装置口径（scripts/gateway-parity/*、scripts/parity/* 均成对设置）：不写也
# **不读**缓存字节码。只有 dont_write_bytecode 挡不住读取已有的
# __pycache__/check_outcome_parity.*.pyc（PEP 552 unchecked-hash 可直接执行），
# 而本脚本正是用 spec_from_file_location 再加载 checker——陈旧字节码会让负控的
# KILLED/INVALID 归因跑的不是当前源码。
try:
    sys.pycache_prefix = tempfile.mkdtemp(prefix='pdrs-retirement-pyc-')
except OSError:
    # 没有可写临时目录也不能让执行器起不来（起不来就到不了「失效旧 summary」）：
    # 指向一个**核实不存在**的目录即「读不到缓存」。名字带 16 位随机字节并在
    # 冲突时重试——固定的 ``.no-pycache-<pid>`` 可能被预先植入 unchecked-hash
    # pyc（评审 P1：可预测路径等于可投毒缓存）。
    base = os.path.dirname(os.path.abspath(__file__))
    for _ in range(8):
        candidate = os.path.join(
            base, f'.no-pycache-{os.getpid()}-{os.urandom(8).hex()}')
        if not os.path.lexists(candidate):
            break
    else:
        # 入口失败一律 rc2（输入不可用），与 `-I` 断言同一口径；`SystemExit('...')`
        # 返回的是 **1**（消息走 stderr），会被下游当成「判据 FAIL」而不是「跑不了」。
        print('negative_controls: cannot reserve a fresh pycache prefix', file=sys.stderr)
        raise SystemExit(2)
    sys.pycache_prefix = candidate
sys.dont_write_bytecode = True

HERE = Path(__file__).resolve().parent
CHECKER = HERE / 'check_outcome_parity.py'
DEFAULT_DATA = Path('_tmp/20261106-issue-1477/r1/data')
DEFAULT_ORACLE = HERE / 'ts-oracle.json'
INJECTED_MESSAGE = 'injected by negative controls for retirement-outcome-parity'
ALT_CODE = 'UPLOAD_NOT_FOUND'
CHECK_TIMEOUT_SECONDS = 600


def load_checker():
    """以显式路径加载判据实现（-I 隔离模式下 sys.path 不含脚本目录）。

    checker 缺失 / 不可读 / **加载期任何异常**（含 `SyntaxError`：坏合并、半写入、
    手改出错）都等于本执行器跑不了（rc 2），不是「判据 FAIL」（rc 1）：裸 traceback
    会被下游读成后者。`SystemExit` 不拦——checker 自己的 `-I` / pycache 断言就是 rc2。
    """
    spec = importlib.util.spec_from_file_location('check_outcome_parity', CHECKER)
    module = importlib.util.module_from_spec(spec)
    try:
        spec.loader.exec_module(module)
    except Exception as error:
        print(f'negative_controls: cannot load checker {CHECKER}: {error!r}',
              file=sys.stderr)
        raise SystemExit(2) from error
    if Path(module.__file__).resolve() != CHECKER:
        print(f'negative_controls: loaded checker {module.__file__} is not {CHECKER}',
              file=sys.stderr)
        raise SystemExit(2)
    return module


CHECK = load_checker()
# 判据侧常量的唯一登记值（不在负控里再硬编码一份，避免常量漂移后测错对象）。
CAPABILITY_UNAVAILABLE = CHECK.CAPABILITY_UNAVAILABLE


class OutputError(Exception):
    """输出目录写不进去（只读目录 / 同名文件 / 父件是普通文件 / 目标是目录）。

    环境不可用按入口级失败收口 rc2；裸 OSError 冒成 traceback 是 rc1，会被下游读成
    「某个控件 SURVIVED 的判据 FAIL」（两家评审 r10）。
    """


def write_output(path, text):
    """写盘失败一律折成 ``OutputError``（rc2），不当判据 FAIL。"""
    try:
        Path(path).write_text(text, encoding='utf-8')
    except (OSError, ValueError) as error:
        raise OutputError(f'cannot write {path}: {error}') from error


def read_round(data_dir, name):
    # 只按 "\n" 切行（共享实现，见 check_outcome_parity.load_frames）。
    rows, _ = CHECK.load_rows(Path(data_dir) / name)
    frames, _ = CHECK.load_frames(Path(data_dir) / name)
    return [dict(row) for row in rows], [json.loads(json.dumps(frame)) for frame in frames]


def write_round(out_dir, rows, frames):
    driver = Path(out_dir) / 'driver'
    try:
        driver.mkdir(parents=True, exist_ok=True)
    except OSError as error:
        raise OutputError(f'cannot create {driver}: {error}') from error
    write_output(driver / 'methods.json',
                 json.dumps({'methods': rows}, ensure_ascii=False) + '\n')
    write_output(driver / 'frames.jsonl',
                 '\n'.join(json.dumps(frame, ensure_ascii=False)
                           for frame in frames) + '\n')


def reread_round(out_dir):
    """落盘后重读（注入落地核对的读法与判据实现共享）。"""
    rows, _ = CHECK.load_rows(out_dir)
    frames, _ = CHECK.load_frames(out_dir)
    return rows, frames


def find_row(rows, method):
    matches = [row for row in rows if row['method'] == method]
    if len(matches) != 1:
        raise AssertionError(f'expected exactly one row for {method}, got {len(matches)}')
    return matches[0]


def send_id(frames, method):
    for frame in frames:
        wire = frame.get('frame')
        if (frame.get('direction') == 'send' and isinstance(wire, dict)
                and wire.get('type') == 'request' and wire.get('method') == method):
            return wire['id']
    raise AssertionError(f'no send frame for {method}')


def response_frame(frames, identifier):
    for frame in frames:
        wire = frame.get('frame')
        if (frame.get('direction') == 'receive' and isinstance(wire, dict)
                and wire.get('type') == 'response' and wire.get('id') == identifier):
            return frame
    return None


def rewrite_response(frames, identifier, body):
    frame = response_frame(frames, identifier)
    if frame is None:
        raise AssertionError(f'no response frame for {identifier}')
    frame['frame'] = body
    frame['raw'] = json.dumps(body, ensure_ascii=False)


def error_response(identifier, code, message):
    return {'type': 'response', 'id': identifier, 'ok': False,
            'error': {'code': code, 'message': message}}


def ok_response(identifier):
    return {'type': 'response', 'id': identifier, 'ok': True,
            'result': {'injected': True}}


def set_error(rows, frames, method, code):
    row = find_row(rows, method)
    row['outcome'] = 'error'
    row['errorCode'] = code
    row['error'] = INJECTED_MESSAGE
    rewrite_response(frames, send_id(frames, method), error_response(send_id(frames, method), code, INJECTED_MESSAGE))


def set_ok(rows, frames, method):
    row = find_row(rows, method)
    row['outcome'] = 'ok'
    row.pop('errorCode', None)
    row.pop('error', None)
    rewrite_response(frames, send_id(frames, method), ok_response(send_id(frames, method)))


def drop_response(frames, identifier):
    frames[:] = [frame for frame in frames
                 if not (frame.get('direction') == 'receive'
                         and isinstance(frame.get('frame'), dict)
                         and frame['frame'].get('type') == 'response'
                         and frame['frame'].get('id') == identifier)]


class Control:
    def __init__(self, cid, title, expected_rules, probe, build, landed, extra=None):
        self.id = cid
        self.title = title
        self.expected_rules = set(expected_rules)
        self.probe = probe
        self.build = build          # () -> {round_name: (rows, frames)}
        self.landed = landed       # {round_name: (rows, frames)} -> (bool, reason)
        self.extra = extra or {}    # 归因加严项：N7 双方码、N11 漂移轮次


def build_controls(base_rows, base_frames, round2_rows, round2_frames):
    ok_request = next(row['method'] for row in base_rows
                      if row['outcome'] == 'ok' and row['shape'] == 'request')
    error_probe = next(row['method'] for row in base_rows
                       if row['outcome'] == 'error'
                       and row.get('errorCode') != CAPABILITY_UNAVAILABLE)
    ok_rows = [row['method'] for row in base_rows if row['outcome'] == 'ok']
    # N10 保留的两个 ok 方法必须在线上有 response 帧（stream ok 只有 final
    # 事件，rewrite_response 会找不到帧 → 控件 INVALID）。
    request_ok = [row['method'] for row in base_rows
                  if row['outcome'] == 'ok' and row['shape'] == 'request']
    assert len(request_ok) >= 3, f'need three request-shaped ok rows, got {request_ok}'
    stream_ok = [row['method'] for row in base_rows
                 if row['outcome'] == 'ok' and row['shape'] == 'stream']
    assert stream_ok, 'baseline has no stream ok method; N14 needs one'
    # 保留 stream ok（它没有 response 帧可改写）与一个 request ok → 恰 99 个被改写。
    keep_ok = stream_ok[:1] + request_ok[:1] if stream_ok else request_ok[:2]
    assert len(keep_ok) == 2, f'need exactly two kept ok rows, got {keep_ok}'
    shape_probe = request_ok[2]
    n10_probe = next(row['method'] for row in base_rows if row['method'] not in keep_ok)
    first_row = base_rows[0]['method']
    outcome_probe = ok_rows[2]
    gated = 'memory_list'
    assert gated in CHECK.REGISTERED_UNAVAILABLE_METHODS
    assert find_row(base_rows, gated)['errorCode'] == CAPABILITY_UNAVAILABLE
    assert ALT_CODE != CHECK.CAPABILITY_UNAVAILABLE

    def clone(*samples):
        return [([dict(row) for row in rows], [json.loads(json.dumps(frame)) for frame in frames])
                for rows, frames in samples]

    def one(mutate):
        def build():
            rows, frames = clone((base_rows, base_frames))[0]
            mutate(rows, frames)
            return {'round-01': (rows, frames)}
        return build

    def n5():
        rows, frames = clone((base_rows, base_frames))[0]
        rows[:] = [row for row in rows if row['method'] != first_row]
        return {'round-01': (rows, frames)}

    def n6():
        rows, frames = clone((base_rows, base_frames))[0]
        rows.append(dict(find_row(rows, first_row)))
        return {'round-01': (rows, frames)}

    def n10():
        rows, frames = clone((base_rows, base_frames))[0]
        for row in rows:
            if row['method'] not in keep_ok:
                identifier = send_id(frames, row['method'])
                row['outcome'] = 'error'
                row['errorCode'] = CAPABILITY_UNAVAILABLE
                row['error'] = INJECTED_MESSAGE
                rewrite_response(frames, identifier,
                                 error_response(identifier, CAPABILITY_UNAVAILABLE, INJECTED_MESSAGE))
        return {'round-01': (rows, frames)}

    def n14():
        rows, frames = clone((base_rows, base_frames))[0]
        identifier = send_id(frames, stream_ok[0])
        bad = error_response(identifier, ALT_CODE, INJECTED_MESSAGE)
        # 规则允许 stream ok 已有一条 ok:true 的 response；那种形状下必须**改写**
        # 既有帧（追加会让落地检查先看到成功帧 → 控件 INVALID）。
        existing = [frame for frame in frames
                    if frame.get('direction') == 'receive'
                    and isinstance(frame.get('frame'), dict)
                    and frame['frame'].get('type') == 'response'
                    and frame['frame'].get('id') == identifier]
        if existing:
            existing[0]['frame'] = bad
            existing[0]['raw'] = json.dumps(bad, ensure_ascii=False)
            del existing[1:]
        else:
            frames.append({'direction': 'receive',
                           'raw': json.dumps(bad, ensure_ascii=False), 'frame': bad})
        return {'round-01': (rows, frames)}

    def n15():
        rows, frames = clone((base_rows, base_frames))[0]
        identifier, other = send_id(frames, request_ok[0]), send_id(frames, request_ok[1])
        for frame in frames:
            wire = frame.get('frame')
            if (frame.get('direction') == 'send' and isinstance(wire, dict)
                    and wire.get('method') == request_ok[1]):
                wire['id'] = identifier
                frame['raw'] = json.dumps(wire, ensure_ascii=False)
        drop_response(frames, other)
        return {'round-01': (rows, frames)}

    def landed_n14():
        def check(bundles):
            rows, frames = bundles['round-01']
            row = find_row(rows, stream_ok[0])
            if row['outcome'] != 'ok':
                return False, f'{stream_ok[0]} outcome changed to {row["outcome"]!r}'
            matching = [frame for frame in frames
                        if frame.get('direction') == 'receive'
                        and isinstance(frame.get('frame'), dict)
                        and frame['frame'].get('type') == 'response'
                        and frame['frame'].get('id') == send_id(frames, stream_ok[0])]
            if len(matching) != 1 or (matching[0]['frame'] or {}).get('ok') is not False:
                return False, 'contradictory error response did not land'
            return True, ''
        return check

    def n16():
        rows, frames = clone((base_rows, base_frames))[0]
        identifier = send_id(frames, request_ok[0])
        replay = ok_response(identifier)
        frames.append({'direction': 'receive',
                       'raw': json.dumps(replay, ensure_ascii=False), 'frame': replay})
        return {'round-01': (rows, frames)}

    def n17():
        rows, frames = clone((base_rows, base_frames))[0]
        method = shape_probe
        original = next(frame for frame in frames
                        if frame.get('direction') == 'send'
                        and isinstance(frame.get('frame'), dict)
                        and frame['frame'].get('method') == method)
        frames.append(json.loads(json.dumps(original)))
        return {'round-01': (rows, frames)}

    def landed_count(direction, kind, method):
        """被注入方法在同向同类型帧里的条数必须恰为 expected。"""
        def build(expected):
            def check(bundles):
                _, frames = bundles['round-01']
                matching = [frame for frame in frames
                            if frame.get('direction') == direction
                            and isinstance(frame.get('frame'), dict)
                            and frame['frame'].get('type') == kind
                            and (frame['frame'].get('method') == method
                                 if direction == 'send'
                                 else frame['frame'].get('id') == send_id(frames, method))]
                if len(matching) != expected:
                    return False, (f'{len(matching)} {direction}/{kind} frames for '
                                   f'{method}, expected {expected}')
                return True, ''
            return check
        return build

    def landed_n15():
        def check(bundles):
            rows, frames = bundles['round-01']
            first, second = request_ok[0], request_ok[1]
            id1, id2 = send_id(frames, first), send_id(frames, second)
            if id1 != id2:
                return False, f'ids differ: {first}={id1} {second}={id2}'
            matching = [frame for frame in frames
                        if frame.get('direction') == 'receive'
                        and isinstance(frame.get('frame'), dict)
                        and frame['frame'].get('id') == id1]
            if len(matching) != 1:
                return False, f'{len(matching)} receive frames for the shared id, expected 1'
            return True, ''
        return check

    def n11():
        first = clone((base_rows, base_frames))[0]
        second = clone((round2_rows, round2_frames))[0]
        set_error(second[0], second[1], error_probe, ALT_CODE)
        return {'round-01': first, 'round-02': second}

    def landed_row(field, expected, method):
        def check(bundles):
            row = find_row(bundles['round-01'][0], method)
            if row.get(field) != expected:
                return False, f'{method}.{field} is {row.get(field)!r}, expected {expected!r}'
            return True, ''
        return check

    def landed_wire(code, method):
        def check(bundles):
            identifier = send_id(bundles['round-01'][1], method)
            response = response_frame(bundles['round-01'][1], identifier)
            wire_code = ((response['frame'].get('error') or {}).get('code')
                         if response else None)
            if wire_code != code:
                return False, f'wire code for {method} is {wire_code!r}, expected {code!r}'
            return True, ''
        return check

    def landed_no_response(method):
        def check(bundles):
            identifier = send_id(bundles['round-01'][1], method)
            if response_frame(bundles['round-01'][1], identifier) is not None:
                return False, f'response frame for {method} still present'
            return True, ''
        return check

    def landed_absent(method):
        def check(bundles):
            if any(row['method'] == method for row in bundles['round-01'][0]):
                return False, f'{method} row still present'
            return True, ''
        return check

    def landed_duplicate(method):
        def check(bundles):
            names = [row['method'] for row in bundles['round-01'][0]]
            if names.count(method) != 2:
                return False, f'{method} appears {names.count(method)} times, expected 2'
            return True, ''
        return check

    def landed_n10():
        def check(bundles):
            rows = bundles['round-01'][0]
            unavailable = [row['method'] for row in rows
                           if row['outcome'] == 'error'
                           and row.get('errorCode') == CAPABILITY_UNAVAILABLE]
            expected = len(rows) - len(keep_ok)
            if len(unavailable) != expected:
                return False, f'{len(unavailable)} unavailable rows, expected {expected}'
            kept = sorted(row['method'] for row in rows if row['outcome'] == 'ok')
            if kept != sorted(keep_ok):
                return False, f'kept ok rows are {kept}, expected {sorted(keep_ok)}'
            return True, ''
        return check

    def landed_n11():
        def check(bundles):
            first = find_row(bundles['round-01'][0], error_probe)
            second = find_row(bundles['round-02'][0], error_probe)
            if first.get('errorCode') == second.get('errorCode'):
                return False, 'round-02 code did not drift from round-01'
            identifier = send_id(bundles['round-02'][1], error_probe)
            response = response_frame(bundles['round-02'][1], identifier)
            wire_code = ((response['frame'].get('error') or {}).get('code')
                         if response else None)
            if wire_code != second.get('errorCode'):
                return False, 'round-02 wire not synced to the drifted code'
            return True, ''
        return check

    return [
        Control('N1', f'ok 方法 {ok_request} 改 error（线缆同步）',
                ['P3-OUTCOME'], ok_request,
                one(lambda rows, frames: set_error(rows, frames, ok_request, ALT_CODE)),
                landed_row('outcome', 'error', ok_request)),
        Control('N2', f'error 方法 {error_probe} 的 errorCode 改 {ALT_CODE}（线缆同步）',
                ['P4-ERRORCODE'], error_probe,
                one(lambda rows, frames: set_error(rows, frames, error_probe, ALT_CODE)),
                landed_row('errorCode', ALT_CODE, error_probe)),
        Control('N3', f'非 gated ok 方法 {ok_request} 改 CAPABILITY_UNAVAILABLE（线缆同步）',
                ['P5-UNAVAILABLE-SET'], ok_request,
                one(lambda rows, frames: set_error(rows, frames, ok_request, CAPABILITY_UNAVAILABLE)),
                landed_row('errorCode', CAPABILITY_UNAVAILABLE, ok_request)),
        Control('N4', f'gated 方法 {gated} 改 ok（线缆同步）',
                ['P5-UNAVAILABLE-SET'], gated,
                one(lambda rows, frames: set_ok(rows, frames, gated)),
                landed_row('outcome', 'ok', gated)),
        Control('N5', f'删掉方法行 {first_row}',
                ['P1-COVERAGE'], first_row, n5, landed_absent(first_row)),
        Control('N6', f'复制方法行 {first_row}（同名两行）',
                ['P1-COVERAGE'], first_row, n6, landed_duplicate(first_row)),
        Control('N7', f'线缆 error.code 与 outcome 码不一致（{error_probe}）',
                ['P6-WIRE-CODE'], error_probe,
                one(lambda rows, frames: rewrite_response(
                    frames, send_id(frames, error_probe),
                    error_response(send_id(frames, error_probe), ALT_CODE, INJECTED_MESSAGE))),
                landed_wire(ALT_CODE, error_probe),
                {'probeCode': find_row(base_rows, error_probe)['errorCode']}),
        Control('N8', f'删掉 {error_probe} 的 response 帧',
                ['P6-WIRE-CODE'], error_probe,
                one(lambda rows, frames: drop_response(frames, send_id(frames, error_probe))),
                landed_no_response(error_probe)),
        Control('N9', f'{ok_request} 的 outcome 改 timeout',
                ['P1-COVERAGE', 'P3-OUTCOME'], ok_request,
                one(lambda rows, frames: find_row(rows, ok_request).update(
                    outcome='timeout', error='injected timeout')),
                landed_row('outcome', 'timeout', ok_request)),
        Control('N10', '99/101 改 CAPABILITY_UNAVAILABLE（线缆同步，保留 2 个 ok）',
                ['P5-UNAVAILABLE-SET'], n10_probe, n10, landed_n10()),
        Control('N11', f'第 2 轮 {error_probe} 的码与第 1 轮不同（其余不变）',
                ['P7-ROUND-STABILITY'], error_probe, n11, landed_n11(),
                {'driftRound': 'round-02'}),
        Control('N12', f'{shape_probe} 的 shape 改 stream（线缆不动）',
                ['P2-SHAPE'], shape_probe,
                one(lambda rows, frames: find_row(rows, shape_probe).update(shape='stream')),
                landed_row('shape', 'stream', shape_probe)),
        Control('N13', f'{outcome_probe} 的 outcome 改成未登记值',
                ['P8-FAIL-CLOSED'], outcome_probe,
                one(lambda rows, frames: find_row(rows, outcome_probe).update(
                    outcome='skipped', error='injected unknown outcome')),
                landed_row('outcome', 'skipped', outcome_probe)),
        Control('N14', f'stream ok 方法 {stream_ok[0]} 加一条同 id 错误 response',
                ['P6-WIRE-CODE'], stream_ok[0], n14, landed_n14()),
        Control('N15', f'{request_ok[0]} 与 {request_ok[1]} 共用同一请求 id（只留一条 response）',
                ['P6-WIRE-CODE'], request_ok[0], n15, landed_n15()),
        Control('N16', f'{request_ok[0]} 的 response 复制一条（同 id 两条 response）',
                ['P6-WIRE-CODE'], request_ok[0], n16,
                landed_count('receive', 'response', request_ok[0])(2)),
        Control('N17', f'{shape_probe} 的 send 帧复制一条（一个方法两条 send）',
                ['P6-WIRE-CODE'], shape_probe, n17,
                landed_count('send', 'request', shape_probe)(2)),
    ]


def preflight_base_rounds(data_root, oracle, names):
    """变异前先确认参考轮本身 PASS（否则负控的归因无意义）。

    逐轮之外再跑一次**跨轮合并**：单轮的 P7 是空判（`p7Evaluated: false`），
    N11 注入的「轮间漂移」只有在未注入时这对轮次本来不漂移的前提下才能归因给
    注入；否则参考数据自带的漂移会送出一个假 KILLED。
    """
    invocations = [(name, [str(Path(data_root) / name)]) for name in names]
    invocations.append(('+'.join(names), [str(Path(data_root) / name) for name in names]))
    for label, paths in invocations:
        command = [sys.executable, '-I', str(CHECKER), 'check', '--oracle', str(oracle)]
        for path in paths:
            command += ['--round', path]
        try:
            result = subprocess.run(
                command, capture_output=True, text=True,
                timeout=CHECK_TIMEOUT_SECONDS, start_new_session=True)
        except subprocess.TimeoutExpired:
            print(f'preflight {label}: TIMEOUT after {CHECK_TIMEOUT_SECONDS}s',
                  file=sys.stderr)
            return 2
        print(f'preflight {label}: rc={result.returncode}', file=sys.stderr)
        if result.returncode != 0:
            print(result.stdout[:2000], result.stderr[:2000], file=sys.stderr)
            return result.returncode or 1
    return 0


def run_control(control, out_root, oracle):
    out_dir = Path(out_root) / control.id
    record = {'id': control.id, 'title': control.title, 'probe': control.probe,
              'expectedRules': sorted(control.expected_rules), 'verdict': None,
              'reason': None, 'firedRules': None, 'rc': None}
    try:
        bundles = control.build()
    except Exception as error:  # noqa: BLE001 - 记录为 INVALID，不中止其余控件
        record.update(verdict='INVALID', reason=f'build failed: {error!r}')
        return record
    for name, (rows, frames) in bundles.items():
        write_round(out_dir / name, rows, frames)
    # 旧产物不留在目录里冒充本轮输出（缺失即失败）。
    try:
        (out_dir / 'check-output.json').unlink(missing_ok=True)
    except OSError as error:
        raise OutputError(
            f'cannot remove stale {out_dir / "check-output.json"}: {error}') from error
    # 第一步：注入是否落地（重读落盘文件核对）。
    try:
        reread = {name: reread_round(out_dir / name) for name in bundles}
        landed, reason = control.landed({name: reread[name] for name in bundles})
    except Exception as error:  # noqa: BLE001
        record.update(verdict='INVALID', reason=f'mutated files unreadable: {error!r}')
        return record
    if not landed:
        record.update(verdict='INVALID', reason=f'injection did not land: {reason}')
        return record
    # 第二步：跑判据，归因核对。
    command = [sys.executable, '-I', str(CHECKER), 'check', '--oracle', str(oracle)]
    for name in sorted(bundles):
        command += ['--round', str(out_dir / name)]
    try:
        result = subprocess.run(command, capture_output=True, text=True,
                                timeout=CHECK_TIMEOUT_SECONDS, start_new_session=True)
    except subprocess.TimeoutExpired:
        record.update(verdict='INVALID',
                      reason=f'checker timed out after {CHECK_TIMEOUT_SECONDS}s')
        return record
    record['rc'] = result.returncode
    write_output(out_dir / 'check-output.json', result.stdout)
    if result.stderr.strip():
        record['stderr'] = result.stderr.strip()[:2000]
    if result.returncode == 0:
        record.update(verdict='SURVIVED',
                      reason='checker returned PASS on mutated data')
        return record
    if result.returncode != 1:
        record.update(verdict='INVALID',
                      reason=f'checker exit {result.returncode} (input unusable?): '
                             f'{result.stdout.strip()[:400]}')
        return record
    try:
        report = json.loads(result.stdout)
        violations = report.get('violations', [])
    except ValueError as error:
        record.update(verdict='INVALID', reason=f'cannot parse checker output: {error}')
        return record
    fired = sorted({violation['rule'] for violation in violations})
    record['firedRules'] = fired
    matched = [violation for violation in violations
               if violation['rule'] in control.expected_rules
               and violation.get('method') == control.probe]
    # 期望规则必须**全部**点名被注入方法（并集非空即算 KILLED 会让回归失效的
    # 规则被另一条规则遮住，例如 N9 的 P3）。
    if {violation['rule'] for violation in matched} != control.expected_rules:
        matched = []
    if control.id == 'N7' and matched:
        # N7 必须点名双方码（outcome 码与线缆码都在 detail 里）。
        probe_code = control.extra['probeCode']
        if not all(ALT_CODE in violation['detail'] and probe_code in violation['detail']
                   for violation in matched):
            matched = []
    if control.id == 'N11' and matched:
        # N11 必须点名漂移轮次。
        drift_round = control.extra['driftRound']
        if not any(drift_round in f"{violation['round']} {violation['detail']}"
                   for violation in matched):
            matched = []
    if matched:
        record.update(verdict='KILLED', reason='expected rule fired naming the injected method')
    else:
        record.update(verdict='SURVIVED',
                      reason=f'expected {sorted(control.expected_rules)} naming '
                             f'{control.probe}; fired rules: {fired}')
    return record


def build_parser():
    """解析器选项表由 ``PARSER_OPTIONS`` 驱动，并断言与 ``KNOWN_OPTIONS`` 一致。

    两处各自硬编码时，「加一个选项但漏改预扫描集合」会让该选项被当成 ``--output``
    的值去失效同名目录，漂移无人发现（评审 P2）。这里让漂移在启动时 rc2 暴露。
    """
    options = (
        (('--output',), {'required': True,
                         'help': 'output directory for mutated rounds + summary'}),
        (('--data',), {'default': str(DEFAULT_DATA),
                       'help': 'TS reference data root (default: %(default)s)'}),
        (('--oracle',), {'default': str(DEFAULT_ORACLE),
                         'help': 'ts-oracle.json (default: %(default)s)'}),
    )
    parser = argparse.ArgumentParser(
        allow_abbrev=False,
        description='Negative controls N1-N17 for the retirement outcome-parity '
                    'checker (#1477). Data-only mutations; no source text is touched.')
    # 输出目录走**显式**选项、不做位置参数：解析前就能无歧义取值（见模块头）。
    for flags, kwargs in options:
        parser.add_argument(*flags, **kwargs)
    declared = tuple(flags[0] for flags, _ in options)
    if declared != VALUE_OPTIONS:
        parser.error(f'KNOWN_OPTIONS out of sync with the parser: '
                     f'parser={declared} scanner={VALUE_OPTIONS}')
    return parser


def main(argv=None):
    parser = build_parser()
    raw_argv = list(sys.argv[1:] if argv is None else argv)
    args = parser.parse_args(raw_argv)

    # 模块体已按 --output 失效过一次（覆盖 argparse 自己 rc=2 早退的路径）；
    # 这里按解析结果再做一次，是本进程的权威失效点（幂等）。
    #
    # 空值必须当参数错误：``Path('') == Path('.')``，否则 ``--output=`` 会把**当前
    # 工作目录**当输出目录（删掉那里旧 summary，再把变异轮与 summary 写进去）。
    # argparse 本身接受 ``--output=``（得到 ``''``），这个判断补上它。
    if not args.output:
        parser.error('--output must not be empty')
    out_root = Path(args.output)
    if not invalidate_stale_summary(out_root):
        return 2
    data = Path(args.data)
    try:
        base_rows, base_frames = read_round(data, 'round-01')
        round2_rows, round2_frames = read_round(data, 'round-02')
        # 前置：未变异的参考轮必须本来 PASS。否则「期望规则 fire」可能是既有违规，
        # 归因就不成立（例如 --data 指向的轮本就有 P4）。逐轮之外还要跑一次
        # 跨轮合并：单轮 P7 是空判，N11 的「跨轮漂移」归因必须先排除参考轮对
        # 本身已漂移。
        preflight = preflight_base_rounds(data, Path(args.oracle),
                                          ('round-01', 'round-02'))
        if preflight != 0:
            print('negative_controls: 未变异的参考轮已不 PASS 金样，负控无法归因；'
                  '先修数据或金样', file=sys.stderr)
            return 2
        controls = build_controls(base_rows, base_frames, round2_rows, round2_frames)
    except CHECK.InputError as error:
        print(f'negative_controls: 输入不可用：{error}', file=sys.stderr)
        return 2
    except Exception as error:  # noqa: BLE001 - 控件构造失败同样按输入不可用收口
        print(f'negative_controls: cannot assemble controls: {error!r}', file=sys.stderr)
        return 2

    records = []
    try:
        for control in controls:  # 全部跑完再汇总，不遇败即停
            record = run_control(control, out_root, Path(args.oracle))
            records.append(record)
            print(f"{control.id}: {record['verdict']} ({record['reason']})")

        counts = {verdict: sum(1 for record in records if record['verdict'] == verdict)
                  for verdict in ('KILLED', 'SURVIVED', 'INVALID')}
        summary = {'status': 'PASS' if counts['SURVIVED'] == 0
                   and counts['INVALID'] == 0 else 'FAIL',
                   'controls': records, 'counts': counts,
                   'oracle': str(args.oracle), 'data': str(args.data)}
        target = out_root / 'summary.json'
        # tmp 名带 pid：同目录并发两次执行时不能共用同一个 tmp（先完成者的 rename
        # 会把另一进程写了一半的内容发布成最终文件）。
        tmp = out_root / f'summary.json.{os.getpid()}.tmp'
        write_output(tmp, json.dumps(summary, ensure_ascii=False, indent=2) + '\n')
        try:
            tmp.replace(target)  # 原子发布：先写 *.tmp 再 replace
        except OSError as error:
            raise OutputError(f'cannot publish {target}: {error}') from error
    except OutputError as error:
        # 写盘失败是「跑不了」（rc2），不是「某个控件的判据 FAIL」（rc1）。
        print(f'negative_controls: 输出目录不可用：{error}', file=sys.stderr)
        return 2
    print(json.dumps({'status': summary['status'], 'counts': counts,
                      'summary': str(target)}, ensure_ascii=False))
    return 0 if summary['status'] == 'PASS' else 1


if __name__ == '__main__':
    sys.exit(main())
