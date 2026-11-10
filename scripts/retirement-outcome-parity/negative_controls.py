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
（缺 ``-I``、checker 加载失败、输出目录写不进去、argv 连宽松孪生解析器都分不了类）
——rc2 绝不能被读成「判据 FAIL」。
"""
from __future__ import annotations

try:
    import argparse
except Exception:  # noqa: BLE001 - 连标准库都起不来：走下面的保守字面扫描（只漏不多删）
    argparse = None
import os
import sys

# ---------------------------------------------------------------------------
# 启动期第一件事：失效同一输出目录里上一轮的 ``summary.json``。
#
# 下游以「summary.json 存在」为「负控已跑完」的证据，所以**任何**早退路径都不得
# 把上一轮的 PASS 留在盘上：缺 ``-I``、checker 导入失败、argparse 报错（拼错 /
# 缺值 / 多余 token）、读数据失败、preflight 非 0。因此这段放在模块体最前面，早于
# ``-I`` 断言、早于 ``load_checker()``、早于 pycache 目录设置（那一步在无可写临时
# 目录时也可能退出）；只依赖 ``os`` / ``sys`` / ``argparse``（标准库，无副作用）。
#
# 帮助路径同样失效（**不猜会走哪条分支**）：只要命令里显式写了输出目录，无论
# argparse 之后是跑、是 ``-h``、还是报错，那个目录的旧 PASS 都已报废。
#
# **token 分类交给 argparse 自己，不再手抄规则。** 前几轮把「哪个 token 算
# ``--output`` 的值」写成镜像 argparse 的判定层（候选集 / 帮助判定 / 负数正则 /
# 「两版都判成选项的形状」），每加一层都在另一版 Python 或另一种拼写上漏收，而漏收
# 的方向是留旧 PASS 冒充本轮、猜错的方向是删掉别人的证据（Codex/DeepSeek 连续
# P0）。现在的做法是**问 argparse 本人**：用**同一张选项表**（``PARSER_OPTIONS``）
# 建一个宽松孪生解析器（``build_classifier``），把所有选项的 ``nargs`` 放宽成
# ``'?'``、``--output`` 改成 ``action='append'``，未知选项由 ``parse_known_args``
# 收进 extras——它不会因为「缺值」报错，但**分类与真实解析器逐 token 一致**，
# 包括只有运行中的这版 CPython 才有的行为（``-1x`` / ``-1e3``：3.12 是选项、
# 3.14 是值）与短选项内联（``'-h x'`` 是 ``-h`` 带内联值，不是目录名）。
# 实测矩阵见 ``README.md``「值怎么取」与 ``_tmp/20261106-issue-1477/verify-fix-r12/``。
#
# 空值不收集（``Path('') == Path('.')``，见 ``main`` 的空值判断）；重复写多次时
# **每个非空值都收集**（argparse 只有一个生效，但这条命令终究没跑完，被显式点名的
# 目录都不该留旧 PASS），所有目录**逐个都试一遍**再退出，前一个不可删不短路后面的。
# 没有 ``--output`` 就一个都不删；``--`` 之后的不算（真实解析器把那里当位置参数，
# 本执行器没有位置参数、直接 rc2），孪生解析器同样不收集。
# ---------------------------------------------------------------------------

# 选项表的**唯一来源**：解析前分类（``build_classifier``）与正式解析
# （``build_parser``）都从这里建，加选项不会漏改其中一侧（Codex r11 P2）。
PARSER_OPTIONS = (
    ('output', ('--output',), 'output directory for mutated rounds + summary'),
    ('data', ('--data',), 'TS reference data root (default: %(default)s)'),
    ('oracle', ('--oracle',), 'ts-oracle.json (default: %(default)s)'),
)
HELP_OPTIONS = ('-h', '--help')

# 字面扫描（分类不可用时的回退）要判断「下一个 token 会不会被 argparse 当成选项」，
# 这一判断依赖**已注册的选项名**：精确同名是选项、``--output=x`` 的 ``=`` 之前是选项、
# ``-h x`` 是短选项内联（都先于「含空格就是值」）。从同一张表推出来，不另抄一份
# （Codex r15 P1：手抄漏掉「含空格」与「末尾换行」两类，文档的残余断言因此不成立）。
_OPTION_FLAGS = frozenset(
    [*HELP_OPTIONS]
    + [flag for _key, flags, _help in PARSER_OPTIONS for flag in flags]
)
_SHORT_OPTION_FLAGS = tuple(
    flag for flag in _OPTION_FLAGS if len(flag) == 2 and flag[1] != '-'
)


def tmp_owner_pid(name):
    """``summary.json.<pid>.tmp`` 里的 pid；不是这个形状返回 None。"""
    prefix, suffix = 'summary.json.', '.tmp'
    if not (name.startswith(prefix) and name.endswith(suffix)):
        return None
    middle = name[len(prefix):-len(suffix)]
    if not middle.isdigit():
        return None
    try:
        return int(middle)
    except ValueError:
        # isdigit() 为真但 int() 拒绝（superscript ``²``、>4300 位数字串，3.11+ 上限）：
        # 当成「不是我们的产物」而不是抛异常——异常会让模块体失效段整段崩掉（rc1），
        # 后面的 --output 目录不再失效，旧 PASS 冒充本轮（DeepSeek r11 P1）。
        return None


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


def _output_lock_guard_fd(out_root):
    """以非阻塞 flock 探测输出目录锁，供模块体失效段用（评审 r7 P2）。

    真正的互斥仍在 `check_outcome_parity.output_lock`（run_locked 里拿）；这里只回答
    「现在**敢不敢**删这个目录里已发布的 summary.json」。返回 ``(fd, held)``：
    fd 为 None 时 held 一定为 True——**fail-closed**：fcntl 缺失 / 锁路径打不开 /
    探测本身出错都当被占，宁可不删也绝不删掉一个活进程刚发布的结果。
    拿到锁时 fd 保持持有、由调用方在删完后释放：删除期间持锁，别的执行器不可能
    同时往这个目录发布新 summary（否则「探测空闲→对方发布→我们再删」的窗口里
    还是会误删）。
    """
    path = os.path.join(os.fspath(out_root), '.retirement-outcome-parity.lock')
    # 与 check_outcome_parity.output_lock 同一口径打开（评审 r8 P2）：O_RDONLY|
    # O_CREAT|O_NOFOLLOW|O_NONBLOCK + fchmod 0644 + S_ISREG。分叉会让「守卫」与
    # 「真锁」对同一个目录各执一词：守卫用 0600 建出锁文件会挡住跨账号共享目录里
    # 后来者的 O_RDONLY；守卫不带 O_NONBLOCK 的 O_RDONLY 在预置同名 FIFO 上挂死
    # （这段在模块体最前面，挂死等于整个执行器连同 CI job 一起起不来）。
    nonblock = getattr(os, 'O_NONBLOCK', None)
    nofollow = getattr(os, 'O_NOFOLLOW', None)
    if nonblock is None or nofollow is None:
        return None, True
    try:
        fd = os.open(path, os.O_RDONLY | os.O_CREAT | nofollow | nonblock
                     | getattr(os, 'O_CLOEXEC', 0), 0o644)
    except (OSError, ValueError):
        return None, True
    try:
        import fcntl
        import stat as stat_module
        info = os.fstat(fd)
        if not stat_module.S_ISREG(info.st_mode):
            # 目录 / FIFO / 设备都不是锁文件：与真锁同一判法，fail-closed 当被占。
            os.close(fd)
            return None, True
        try:
            os.fchmod(fd, 0o644)
        except OSError:  # pragma: no cover - 同真锁：权限修复失败不影响互斥语义
            pass
        try:
            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except OSError:
            os.close(fd)
            return None, True
    except Exception:  # noqa: BLE001 - fcntl 缺失 / 平台不支持：fail-closed 当被占
        os.close(fd)
        return None, True
    return fd, False


def _release_output_lock_guard(fd):
    try:
        import fcntl
        fcntl.flock(fd, fcntl.LOCK_UN)
    except Exception:  # noqa: BLE001 - 释放失败只剩 close 兜底
        pass
    os.close(fd)


def invalidate_stale_summary(out_root, respect_output_lock=False):
    """删掉该目录里上一轮的 ``summary.json`` 与残留 tmp。

    只动本执行器自己的产物名；另一个**仍在跑**的执行器的
    ``summary.json.<pid>.tmp`` 不删（删掉它会让对方的原子发布 ``replace`` 抛
    ``FileNotFoundError``）。目录不存在时**不创建**。

    ``respect_output_lock=True``（模块体早退路径用）：先非阻塞探测输出目录锁，
    锁被**另一个进程**占着时不删已发布的 ``summary.json``（那是持锁方刚发布的
    结果，删掉就是丢证据），只回收死 pid 的残留 tmp；探测到空闲才持锁删除。
    锁文件名与 `check_outcome_parity.LOCK_BASENAME` 同源常量（字面一致）。
    """
    root = os.fspath(out_root)
    skip_published = False
    guard_fd = None
    if respect_output_lock:
        guard_fd, skip_published = _output_lock_guard_fd(out_root)
    try:
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
            if skip_published and name == 'summary.json':
                # 输出目录锁被别的进程占着：这个 summary.json 是持锁方（或它之前
                # 那轮）的现行结果，失效它归 run_locked 拿到锁之后的权威失效负责。
                print(f'negative_controls: output lock held; keeping {name}',
                      file=sys.stderr)
                continue
            owner = tmp_owner_pid(name)
            if owner is not None and owner != os.getpid() and pid_alive(owner):
                print(f'negative_controls: keeping live {name} (pid {owner} runs)',
                      file=sys.stderr)
                continue
            try:
                os.unlink(path)
            except FileNotFoundError:
                # 并发失效 / 别的进程先删了同一批条目：目标已达成（不在盘上），
                # 不算失败（评审 r7 P2：返回 False 会让本进程被当成 rc2 早退）。
                continue
            except OSError as error:
                print(f'negative_controls: cannot remove {path}: {error}', file=sys.stderr)
                return False
            removed.append(name)
        if removed:
            print(f'removed stale {", ".join(removed)}', file=sys.stderr)
        return True
    finally:
        if guard_fd is not None:
            _release_output_lock_guard(guard_fd)


def build_classifier():
    """只做 token 分类的**宽松孪生**解析器：与 ``build_parser`` 同一张选项表。

    与真实解析器的差别只在「永不因为形状报错」，不在「怎么分类」：

    * 每个带值选项 ``nargs='?'`` + ``action='append'``——``--output --data DIR``
      这类形状不会中止，``--output`` 收 None，重复写时**每个**值都留下；
    * ``parse_known_args``——未知选项 / 多余 token 落 extras，不抛 SystemExit；
    * ``-h`` / ``--help`` 必须按真实解析器注册：否则 ``'-h x'``（短选项 + 内联值）
      会被当成普通位置参数当值（Codex r11 P0）。但**不能**用 ``store_true``——那样
      ``--help=x`` / ``-h=x`` 会抛 ``ignored explicit argument``，孪生解析器整个失败，
      后面的 ``--output`` 就不再失效（留旧 PASS）。所以 help 也放宽成
      ``nargs='?'`` + ``action='append'``：只借它做 token 分类，值被收进 ``namespace.help``
      不影响 ``--output`` 的收集。

    分类本身完全由运行中的这版 argparse 决定（``_parse_optional`` 的正数/负数/含空格/
    短选项内联规则），所以跨版本差异（``-1x``：3.12 选项、3.14 值）自动跟随，不需要
    镜像，也不会随版本漂移（前几轮 P0 的根因）。
    """
    parser = argparse.ArgumentParser(add_help=False, allow_abbrev=False)
    parser.add_argument(*HELP_OPTIONS, nargs='?', action='append')
    for _key, flags, _help in PARSER_OPTIONS:
        parser.add_argument(*flags, nargs='?', action='append')
    return parser


def early_outputs(argv):
    """解析前取**所有**显式写出的非空 ``--output`` 值，返回 ``(values, ok)``。

    分类全部交给 argparse（见 ``build_classifier``）：不手抄规则、不猜缩写、不猜
    位置参数——那些猜测正是前几轮误删别人证据 / 留旧 PASS 的来源（两家评审 P0）。

    ``ok=False`` 表示**分类不可用**（argparse 导入失败，或宽松孪生解析器在某种形状上
    抛 ``ArgumentError`` / ``SystemExit``）：调用方改走 ``literal_outputs`` 的保守
    字面扫描——只认 ``--output 值`` / ``--output=值``，且只在下一个 token 是**值**时才
    消费：非 ``-`` 开头 / ``-`` 本身 / 含空格三类直接算值（含空格这类要在**没先命中
    选项分支**时才算——``-h x`` / ``--output=x y`` 已被选项分支截获）；**只有负数那一
    类**是保守子集，按两版正则**形状**的交集取值（数字表取本解释器）。**只漏不多删**——然后按
    输入不可用 rc2 退出。扫不到的形状登记在差异条目「已知边界」⑤。

    空值（``--output=`` / ``--output ''``）不收集：``Path('')`` 就是当前工作目录，
    绝不能把 CWD 当输出目录失效（两家评审：会删掉调用者目录里的 summary）；
    ``main`` 另行判参数错误 rc2。
    """
    if argparse is None:
        print('negative_controls: cannot import argparse; using literal --output scan',
              file=sys.stderr)
        return [], False
    try:
        namespace, _extras = build_classifier().parse_known_args(argv)
    except (argparse.ArgumentError, SystemExit) as error:
        print(f'negative_controls: cannot classify argv: {error!r}', file=sys.stderr)
        return [], False
    return [value for value in (namespace.output or []) if value], True


# 只在作为脚本运行时失效：被 import 时不得拿宿主的 sys.argv 去删任何东西
# （评审 P0：导入模块即删除调用者证据）。这段仍早于 ``-I`` 断言与
# ``load_checker()``——那些启动依赖一失败就到不了失效，旧 PASS 会冒充本轮。
# **逐个都试一遍再退出**：前一个目录不可删（如 ``/dev/null``）不能让后面显式点名的
# 目录跳过失效（两家评审：短路会让后者旧 PASS 冒充本轮）。
def _decimal_digits(text):
    """Unicode **十进制**数字串（Nd 类）。

    argparse 的负数正则 ``^-\\d+$|^-\\d*\\.\\d+$`` 用的是 Python 的 Unicode ``\\d``，
    等价于 ``str.isdecimal()``（Nd），**不是** ``str.isdigit()``——``'²'`` 的 ``isdigit()``
    为真、``isdecimal()`` 为假，argparse 也不认它。``-١``（阿拉伯-印度数字）、
    ``-１``（全角）在两版里都是值（Codex r14 P1）。

    Unicode 表跟着**跑着的那个解释器**走（3.12 是 Unicode 15、3.14 是 Unicode 16），
    这正是要的：``-𐵀``（U+10D40）在 3.14 是十进制数字、同版本 argparse 也当值，在 3.12
    不是、同版本 argparse 也当选项——逐版本自洽，所以「两版都收」的交集随 Unicode 版本
    移动，方向仍是只漏不多删（Codex r16 P1）。
    """
    return bool(text) and all(char.isdecimal() for char in text)


def looks_like_negative_number(token):
    """argparse 的**负数字面量**判定（``_negative_number_matcher``），两版取交集。

    真实解析器没有「长得像负数」的选项（只有 ``-h`` 与三条 ``--`` 长选项），所以
    ``_has_negative_number_optionals`` 是空集，``-1`` / ``-1.5`` / ``-.5`` 在**值位置**
    被 argparse 当值（3.12 与 3.14 实测一致，见
    ``_tmp/20261106-issue-1477/verify-fix-r15/probe-312.txt`` 与 ``probe-314.txt``）。
    这里手写以免在「标准库都导入不了」的回退路径上再依赖 ``re``；``\\d`` 取 Unicode
    十进制数字（``str.isdecimal``），与**同版本** argparse 一致。

    只覆盖**两版正则形状都认**的 token：3.12 的 ``^-\\d+$|^-\\d*\\.\\d+$`` 是整串匹配，
    3.14 换成只锚开头的 ``-\\.?\\d``，所以 ``-1x`` / ``-0x1f`` 这类 3.14 是值、3.12 是
    选项——本函数一律判**不是值**，只漏不多删（已知边界⑤）。``\\d`` 的 Unicode 表按
    运行版本取（见 ``_decimal_digits``），故新码点如 ``-𐵀`` 在 3.14 判值、3.12 判选项，
    与各自版本的 argparse 一致。
    """
    if not token.startswith('-') or token.startswith('--'):
        return False
    body = token[1:]
    if '.' in body:
        whole, _, fraction = body.partition('.')
        return (not whole or _decimal_digits(whole)) and _decimal_digits(fraction)
    return _decimal_digits(body)


def _negative_value(token):
    """两版 argparse 都判成负数的 token（含 3.12 的 ``$`` 允许一个末尾换行）。"""
    if looks_like_negative_number(token):
        return True
    # 3.12 的正则用 ``$``，它在**末尾那一个换行之前**也匹配，于是 ``'-1\\n'`` /
    # ``'-١\\n'`` 是值；3.14 的 ``-\\.?\\d`` 只锚开头，同样认。两版都当值（Codex r15 P1）。
    # 两个换行不行（``$`` 只跨一个），``'\\r\\n'`` 也不行（``\\d`` 不吃 ``\\r``）——
    # 交给 ``looks_like_negative_number`` 判定剥掉一个 ``\\n`` 之后的部分即可。
    return (token.endswith('\n') and not token.endswith('\n\n')
            and looks_like_negative_number(token[:-1]))


def _is_literal_value(token):
    """该 token 在**值位置**是否必被 argparse 当成值（负数一类是保守子集，其余逐条等价）。

    逐条对着 argparse ``_parse_optional`` 返回 ``None`` 的条件写：**除负数匹配那一步
    外**，分类分支在当前注册选项下**逐条等价**——只有「负数字面量」与「含空格」两步
    相对源码对调，两步都返回「值」故不影响结论；真正会改变结论的顺序照抄：
    ``'-h x'`` 先命中短选项内联、``'--output=x y'`` 先命中 ``=`` 拆分，两者都在
    「含空格就是值」之前；``'-1 x'`` 则是含空格的值（Codex r15 P1）。
    源码里 ``_get_option_tuples`` 那步在 ``allow_abbrev=False`` 下对长选项退化为空，
    短选项一侧由已注册短选项前缀覆盖，故省略（Grok r16 P3）。

    **负数匹配是保守子集**，不是等价：按两版正则**形状**的交集取值（3.12 整串、
    3.14 只锚开头），所以 ``-1x`` 在 3.14 是值、这里不收——那正是已知边界⑤登记的残余
    （Codex r17 P1）。数字表取**本解释器**的 Unicode 表，故与同版本 argparse 一致。
    """
    if not token or token[0] != '-':
        return True
    if token in _OPTION_FLAGS:
        return False                                   # 精确同名选项（含 -h / --help）
    if len(token) == 1:
        return True                                    # 单个 '-'
    head, separator, _tail = token.partition('=')
    if separator and head in _OPTION_FLAGS:
        return False                                   # --output=... 的 '=' 之前是选项
    if any(token.startswith(flag) for flag in _SHORT_OPTION_FLAGS):
        return False                                   # '-h x' / '-h1'：短选项内联
    return ' ' in token or _negative_value(token)


def literal_outputs(argv):
    """**保守**失效清单：只认字面 ``--output 值`` / ``--output=值``。

    仅在分类不可用时使用（argparse 连导入都失败，或宽松孪生解析器报错）：模块体
    失效段不能因此整段跳过，否则上一轮的 PASS 留在盘上，下游按「文件存在」读成本轮
    通过。方向是**只漏不多删**：

    * 内联写法 ``--output=值`` 没有歧义——``=`` 之后整段都是值（哪怕是 ``-old``），
      所以**照收**；
    * 分隔写法 ``--output 值`` 只在下一个 token **必为值**时才算值：不以 ``-`` 开头、
      ``-`` 本身、含 ASCII 空格的 token（argparse 的 ``if ' ' in arg_string: return
      None``，两版同源；``--bogus x`` 也是值）、argparse 的负数字面量（``-1`` /
      ``-1.5`` / ``-.5`` / ``-١``，含 3.12 ``$`` 允许的**一个末尾换行**：``'-1\\n'`` 两版
      都是值）——而 ``-h`` / ``-h x``（短选项内联）/ ``--data`` / ``--output=x`` / 裸
      ``--`` 在任何一版里都是选项（判定顺序见 ``_is_literal_value``）；
    * 下一个 token 是选项 / 裸 ``--`` / 另一个 ``--output=...`` 时**不消费**它，
      留到下一轮按选项再判——否则 ``--output --output=X`` 会漏收、``--output --
      --output=X`` 会越过终止符误删位置参数同名目录（Codex r13 P0×2）；
    * 遇到裸 ``--`` 立即停止（之后的 ``--output`` 只是位置参数，argparse 也不收）；
    * 仍会漏的形状只剩**两版负数正则形状不一致**的那些（3.14 的 ``-\\.?\\d`` 只锚开头、
      3.12 是整串 ``^-\\d+$|^-\\d*\\.\\d+$``，于是 ``-1x`` / ``-1e3`` / ``-1.`` /
      ``-٣.٤.٥`` 在 3.14 是值、3.12 是选项；``'-1\\n\\n'`` 同理）——残余精确说是三条同时
      成立：前缀规则认 · 整串规则不认 · **token 无 ASCII 空格**（``-1 x`` 满足前两条，
      却因含空格在两版都是值且这里也收，故不是残余）。``\\d`` 取**本解释器**的 Unicode
      表，故它与同版本 argparse 天然一致。漏掉等价于「不删」，不会误删别人证据
      （已知边界⑤）。除此之外的判定与 ``_parse_optional`` 逐条对齐，证据：
      ``_tmp/20261106-issue-1477/verify-fix-r16/``。
    """
    values = []
    index = 0
    while index < len(argv):
        token = argv[index]
        if token == '--':
            break
        if token == '--output':
            # 只有**真的消费掉一个值**才前进两位；下一个 token 是选项 / `--` / 另一个
            # `--output=...` 时必须留到下一轮按选项再处理，否则
            # `--output --output=X`（漏收）与 `--output -- --output=X`（越过终止符、
            # 误删位置参数同名目录）都会发生（Codex r13 P0×2）。
            if index + 1 < len(argv):
                value = argv[index + 1]
                if value and _is_literal_value(value):
                    values.append(value)
                    index += 2
                    continue
            index += 1
            continue
        if token.startswith('--output='):
            # 内联写法没有歧义：argparse 一定把 `=` 之后整段当值（哪怕是 `-old`），
            # 所以这里**不再按 `-` 前缀过滤**（Grok r13 P0）。
            value = token[len('--output='):]
            if value:
                values.append(value)
        index += 1
    return values


if __name__ == '__main__':
    _outputs, _classified = early_outputs(sys.argv[1:])
    if not _classified:
        # argparse 起不来时**不猜复杂形状**，但字面写出的输出目录仍必须失效，
        # 否则旧 PASS 冒充本轮（Codex r12 P0）。清完按入口失败 rc2 退出。
        _outputs = literal_outputs(sys.argv[1:])
    # 模块体失效在输出目录锁**之外**：加 respect_output_lock 探测，锁被别人占着时
    # 只回收死 pid tmp、不删已发布的 summary.json（run_locked 拿到锁后有权威失效）。
    _failures = [path for path in _outputs
                 if not invalidate_stale_summary(path, respect_output_lock=True)]
    if _failures or not _classified:
        raise SystemExit(2)

if not sys.flags.isolated:
    print(f'negative_controls: run with -I: python3 -I {__file__}', file=sys.stderr)
    raise SystemExit(2)

import atexit
import importlib.util
import json
import shutil
import stat
import subprocess
import tempfile
from pathlib import Path

# 同族装置口径（scripts/gateway-parity/*、scripts/parity/* 均成对设置）：不写也
# **不读**缓存字节码。只有 dont_write_bytecode 挡不住读取已有的
# __pycache__/check_outcome_parity.*.pyc（PEP 552 unchecked-hash 可直接执行），
# 而本脚本正是用 spec_from_file_location 再加载 checker——陈旧字节码会让负控的
# KILLED/INVALID 归因跑的不是当前源码。
# #2452：自建前缀注册 atexit 清理（rmtree 容错，同三个装置入口与探针）——本模块
# 被 self_check 的预算求值在进程内 exec（lru_cache 单例，每进程一次），此前每次
# exec 各泄漏一个常驻空临时目录且把宿主的 sys.pycache_prefix 顶掉；目录由
# mkdtemp 独占创建（mode 700）且 dont_write_bytecode 下恒为空，退出时 rmtree 只会
# 删掉自己的空目录。OSError 回退路径核实不存在，不注册。
try:
    sys.pycache_prefix = tempfile.mkdtemp(prefix='pdrs-retirement-pyc-')
    atexit.register(shutil.rmtree, sys.pycache_prefix, ignore_errors=True)
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
# 参考轮数据**入库**在 `fixtures/`（#1809 条目 1）：干净检出即可跑，不再依赖未跟踪的
# `_tmp/20261106-issue-1477/r1/data`。默认值相对**脚本位置**而不是 cwd——以前的相对
# 默认值在别的工作目录下直接 `[Errno 2]`，换机 / CI 上跑不了。`--data` 仍可覆盖。
DEFAULT_DATA = HERE / 'fixtures'
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
# 控件名册同源读 check_outcome_parity（评审 r7 P1；#2378 升级）：执行器拿到
# controls 后核 **ID 全集**（不是条数——条数拦不住改名：一重复一缺失时条数不变，
# 名册不等），控件集被删空 / 删部分 / 改名时**空真不得判 PASS**——rc2 收口，不发布
# summary。r9 合并补全：正控（#2143 PC1–PC2）同一守卫——build_positives 被删空时
# positiveCounts 全 0，`BROKEN == 0 and INVALID == 0` 照样空真判 PASS。
EXPECTED_CONTROL_IDS = CHECK.EXPECTED_CONTROL_IDS
EXPECTED_POSITIVE_IDS = CHECK.EXPECTED_POSITIVE_IDS
EXPECTED_CONTROLS = CHECK.EXPECTED_CONTROLS
EXPECTED_POSITIVES = CHECK.EXPECTED_POSITIVES


class OutputError(Exception):
    """输出目录写不进去（只读目录 / 同名文件 / 父件是普通文件 / 目标是目录）。

    环境不可用按入口级失败收口 rc2；裸 OSError 冒成 traceback 是 rc1，会被下游读成
    「某个控件 SURVIVED 的判据 FAIL」（两家评审 r10）。
    """


_NOFOLLOW = getattr(os, 'O_NOFOLLOW', 0)
_O_DIRECTORY = getattr(os, 'O_DIRECTORY', 0)
_DIR_FD_OK = all(entry in os.supports_dir_fd
                 for entry in (os.open, os.mkdir, os.unlink, os.rename))


def require_safe_writes():
    """平台给不出「不跟随链接的打开」就拒跑，**不静默退回跟随链接**。

    ``getattr(os, 'O_NOFOLLOW', 0)`` 这类回退在缺该标志的平台上会把保护悄悄摘掉，
    而 ``O_TRUNC`` 仍然在——"从不跟随符号链接"就变成一句空话（两家评审 r13）。
    """
    if not (_NOFOLLOW and _O_DIRECTORY and _DIR_FD_OK):
        raise OutputError('this platform cannot open directories without following '
                          'symlinks (needs O_NOFOLLOW / O_DIRECTORY / dir_fd); '
                          'refusing to write')


def _descend(dir_fd, part):
    """在**已持有的**目录 fd 下打开（缺则创建）子目录，绝不跟随链接；成功即接手 fd。

    一次只走**一个**路径部件：带分隔符或 ``.`` / ``..`` 的部件直接拒绝——``dir_fd``
    只对单个名字生效，带分隔符的部件会按路径重新解析、中间链接照样被跟随（Codex r14
    实测 ``'N1/child'`` 写穿到链接目标）。当前控制 ID 是代码内固定的 ``N1``–``N17``，
    没有外部输入能走到这里；这道守卫是给未来接口留的 fail-closed。
    """
    separators = {sep for sep in ('/', os.sep, os.altsep) if sep}
    if (not part or part in ('.', '..') or '\0' in part
            or any(sep in part for sep in separators)):
        raise OutputError(f'refusing non-single-component output path part {part!r}')
    for first in (True, False):
        try:
            child = os.open(part, os.O_RDONLY | _O_DIRECTORY | _NOFOLLOW, dir_fd=dir_fd)
        except FileNotFoundError as error:
            if not first:
                # 建完到第二次 open 之间目录被移走：仍是环境不可用，必须收口成 rc2；
                # 裸 raise 会冒成 traceback rc1，被下游读成「某个控件 SURVIVED」（Codex r14 P1）。
                raise OutputError(f'cannot open {part!r} as a real directory: it disappeared '
                                  f'after it was created: {error}') from error
            try:
                os.mkdir(part, 0o700, dir_fd=dir_fd)
            except FileExistsError:
                pass
            except OSError as error:
                raise OutputError(f'cannot create {part!r} in the output directory: '
                                  f'{error}') from error
            continue
        except OSError as error:
            raise OutputError(f'cannot open {part!r} as a real directory: {error}') from error
        os.close(dir_fd)
        return child
    raise AssertionError('unreachable')


def open_output_dir(root, parts=()):
    """逐段 ``O_NOFOLLOW|O_DIRECTORY`` 打开 ``root/parts``（缺则创建），返回持有的 fd。

    后续写盘全部相对这个 **fd**（``dir_fd``），不再按路径重新解析：两次「检查」与
    「使用」之间把中间目录换成符号链接的窗口因此不存在——先 ``is_symlink()`` 再按路径
    ``open`` 只挡得住静态预置的链接，父目录被换掉的竞态照样写穿（两家评审 r13）。
    ``root`` 是调用方给的输出目录，允许它本身是链接（macOS 的 ``/tmp`` 就是）；
    它的**祖先**按路径解析、不在本执行器的威胁模型内（登记在差异条目「已知边界」⑥）。
    """
    require_safe_writes()
    try:
        dir_fd = os.open(root, os.O_RDONLY | _O_DIRECTORY)
    except FileNotFoundError:
        # 调用方给的根目录不存在就建出来（同旧行为：`--output a/b/c` 允许父目录一起建）。
        # 根本身的祖先不在威胁模型内——它是调用方点名的目录。
        try:
            os.makedirs(root, exist_ok=True)
            dir_fd = os.open(root, os.O_RDONLY | _O_DIRECTORY)
        except (OSError, ValueError) as error:
            raise OutputError(f'cannot create output directory {root}: {error}') from error
    except (OSError, ValueError) as error:
        raise OutputError(f'cannot open output directory {root}: {error}') from error
    try:
        for part in parts:
            dir_fd = _descend(dir_fd, part)
        return dir_fd
    except BaseException:
        os.close(dir_fd)
        raise


def write_at(dir_fd, name, text, exclusive=False):
    """相对 ``dir_fd`` 写一个文件：``O_NOFOLLOW|O_NONBLOCK`` + ``S_ISREG`` + 失败一律折成
    ``OutputError``（rc2）。

    ``O_NOFOLLOW`` 让预置的 ``.../methods.json -> ~/.bashrc`` 直接 ``ELOOP`` 失败，
    而不是以本进程权限截断目标（DeepSeek r11 P1）。``O_NONBLOCK`` + ``S_ISREG``
    （评审 r7 P2）挡**预置 FIFO**：无读者端的 FIFO 会让裸 ``O_WRONLY`` open 永久
    阻塞（整个执行器连同 CI job 一起挂死），带读者端的 FIFO 则 open 立即成功、
    只有 ``fstat`` 后的 ``S_ISREG`` 判定能拒。``exclusive`` 再叠 ``O_EXCL``，
    用于原子发布的 tmp。``UnicodeEncodeError`` / 嵌入 NUL 的路径是 ``ValueError``
    而不是 ``OSError``，不接住就会冒成 traceback rc1（两家评审 r13）。
    """
    flags = os.O_WRONLY | os.O_CREAT | os.O_TRUNC | _NOFOLLOW | os.O_NONBLOCK
    if exclusive:
        flags |= os.O_EXCL
    try:
        path_fd = os.open(name, flags, 0o600, dir_fd=dir_fd)
    except (OSError, ValueError) as error:
        raise OutputError(f'cannot write {name}: {error}') from error
    # fstat 的是**要写入的那个 fd 本身**（fd 不可变），不是再走一次路径名——
    # 带读者端的 FIFO 在上面 open 成功，只有这里的 S_ISREG 能拒。
    try:
        if not stat.S_ISREG(os.fstat(path_fd).st_mode):
            os.close(path_fd)
            raise OutputError(
                f'cannot write {name}: not a regular file (planted FIFO / '
                'device / socket); refusing to write')
    except OSError as error:
        raise OutputError(f'cannot write {name}: {error}') from error
    try:
        with os.fdopen(path_fd, 'w', encoding='utf-8') as handle:
            handle.write(text)
    except (OSError, ValueError) as error:
        raise OutputError(f'cannot write {name}: {error}') from error


def remove_at(dir_fd, name):
    """相对 ``dir_fd`` 删一个文件（不存在不算错）。``unlink`` 不跟随链接，删的是链接本身。"""
    try:
        os.unlink(name, dir_fd=dir_fd)
    except FileNotFoundError:
        return
    except OSError as error:
        raise OutputError(f'cannot remove {name}: {error}') from error


def read_round(data_dir, name):
    # 只按 "\n" 切行（共享实现，见 check_outcome_parity.load_frames）。
    rows, _ = CHECK.load_rows(Path(data_dir) / name)
    frames, _ = CHECK.load_frames(Path(data_dir) / name)
    return [dict(row) for row in rows], [json.loads(json.dumps(frame)) for frame in frames]


def write_round(out_root, control_id, name, rows, frames):
    driver = open_output_dir(out_root, (control_id, name, 'driver'))
    try:
        write_at(driver, 'methods.json',
                 json.dumps({'methods': rows}, ensure_ascii=False) + '\n')
        write_at(driver, 'frames.jsonl',
                 '\n'.join(json.dumps(frame, ensure_ascii=False)
                           for frame in frames) + '\n')
    finally:
        os.close(driver)


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
    # 凡靠「答 unavailable 必须被点名」归因的控件，探针都不能落在 P5 会做集合减法的
    # gated 集里（`REGISTERED_UNAVAILABLE_METHODS`）——P5 只对集合外答 unavailable
    # 点名，探针落在集合内，注入就被吞掉、控件从 KILLED 退化成 SURVIVED（假 FAIL）。
    # 例外是**故意**落在集合内的控件：N4（gated，靠 P5 的另一侧点名），它不由本
    # 过滤器选探针。原 #2143 r2/r3 实测：gated 方法顶到首行的参考数据下，改前
    # N3/N10 SURVIVED、rc=1。A8.6-...-001-R2 撤回复期时保留这一普适加固
    # （去掉延期集那一半，见 diffs/A8.6-RETIREMENT-OUTCOME-PARITY-001-R2.md）。
    swallowed = CHECK.REGISTERED_UNAVAILABLE_METHODS
    ok_request = next(row['method'] for row in base_rows
                      if row['outcome'] == 'ok' and row['shape'] == 'request'
                      and row['method'] not in swallowed)
    assert ok_request not in swallowed, (
        f'probe {ok_request} sits in the gated set; N1/N3/N9 attribution '
        'would be swallowed by the P5 set subtraction')
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
    # N10 的注入是「把 keep_ok 之外的行全改成 CAPABILITY_UNAVAILABLE」，归因靠 P5 点名
    # 探针；探针必须同样避开 `swallowed`（见函数开头），否则注入对它是空操作。
    n10_probe = next(row['method'] for row in base_rows
                     if row['method'] not in keep_ok
                     and row['method'] not in swallowed)
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


def reference_rounds(data_root):
    """``--data`` 下所有轮次目录（含 ``driver/methods.json``），按名字排序。

    早期版本把 preflight 写死成 ``round-01`` / ``round-02``：``--data`` 换目录后
    ``round-03..05`` 的 PASS 不在自检范围（#1809 条目 5）。参考数据切片入库后 5 轮
    都可复现，改成**全量发现**——参考轮里只要有一轮本来就不 PASS 金样，注入后的
    违规就不能归因给注入。少于 2 轮直接 rc2：N11 的跨轮漂移归因需要成对的轮次。
    """
    root = Path(data_root)
    try:
        names = sorted(entry.name for entry in root.iterdir()
                       if (entry / 'driver' / 'methods.json').is_file())
    except OSError as error:
        raise CHECK.InputError(
            f'cannot list reference rounds under {root}: {error}') from error
    if len(names) < 2:
        raise CHECK.InputError(
            f'{root}: need at least 2 reference rounds for the cross-round '
            f'preflight, found {names}')
    return tuple(names)


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
    # #2573 双谱系评审 P2（DS+MM sibling 两家撞上）：输出侧故障与 build 同为
    # 「这条控件没跑成」，run_positive 已收口，run_control 的同形调用不收会让
    # 同一故障在正控路径局部化、在负控路径整批 rc2——对称收 INVALID + return。
    try:
        for name, (rows, frames) in bundles.items():
            write_round(out_root, control.id, name, rows, frames)
    except Exception as error:  # noqa: BLE001 - 记录为 INVALID，不中止其余控件
        record.update(verdict='INVALID', reason=f'write round failed: {error!r}')
        return record
    # 旧产物不留在目录里冒充本轮输出（缺失即失败）。相对**持有的**控制目录 fd 操作，
    # 不按路径重新解析（父目录被换成链接时不误删别处文件）。
    try:
        control_fd = open_output_dir(out_root, (control.id,))
        try:
            remove_at(control_fd, 'check-output.json')
        finally:
            os.close(control_fd)
    except Exception as error:  # noqa: BLE001 - 记录为 INVALID，不中止其余控件
        record.update(verdict='INVALID', reason=f'cannot prepare the output dir: {error!r}')
        return record
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
    # #2573：同 run_positive——check 输出落盘失败收 INVALID，不穿出控件循环。
    try:
        control_fd = open_output_dir(out_root, (control.id,))
        try:
            write_at(control_fd, 'check-output.json', result.stdout)
        finally:
            os.close(control_fd)
    except Exception as error:  # noqa: BLE001 - 记录为 INVALID，不中止其余控件
        record.update(verdict='INVALID', reason=f'cannot record the check output: {error!r}')
        return record
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


def run_positive(positive, out_root, oracle):
    """跑一条正控：HELD（断言成立）/ BROKEN（应 PASS 却没 PASS 或报告丢字段）/ INVALID。"""
    out_dir = Path(out_root) / positive.id
    record = {'id': positive.id, 'title': positive.title, 'probe': positive.probe,
              'verdict': None, 'reason': None, 'rc': None}
    try:
        rows, frames = positive.build()
    except Exception as error:  # noqa: BLE001 - 记录为 INVALID，不中止其余控件
        record.update(verdict='INVALID', reason=f'build failed: {error!r}')
        return record
    # #2573：输出侧故障（write_round / open_output_dir / remove_at / write_at 抛
    # OutputError——磁盘满、目录不可开）与 build 同为「这条正控没跑成」，同样局部收
    # INVALID + return；让它穿出 for positive 循环会被 main() 外层收成 rc2「输入
    # 不可用」，正控记录不发布、summary 缺 positiveCounts（正控注册数现为 0，此
    # 路径在 A8.6 复活正控前不可达，先收口）。
    try:
        write_round(out_root, positive.id, 'round-01', rows, frames)
    except Exception as error:  # noqa: BLE001 - 记录为 INVALID，不中止其余控件
        record.update(verdict='INVALID', reason=f'write round failed: {error!r}')
        return record
    try:
        control_fd = open_output_dir(out_root, (positive.id,))
        try:
            remove_at(control_fd, 'check-output.json')
        finally:
            os.close(control_fd)
    except Exception as error:  # noqa: BLE001 - 记录为 INVALID，不中止其余控件
        record.update(verdict='INVALID', reason=f'cannot prepare the output dir: {error!r}')
        return record
    command = [sys.executable, '-I', str(CHECKER), 'check',
               '--oracle', str(oracle), '--round', str(out_dir / 'round-01')]
    try:
        result = subprocess.run(command, capture_output=True, text=True,
                                timeout=CHECK_TIMEOUT_SECONDS, start_new_session=True)
    except subprocess.TimeoutExpired:
        record.update(verdict='INVALID',
                      reason=f'checker timed out after {CHECK_TIMEOUT_SECONDS}s')
        return record
    record['rc'] = result.returncode
    # #2573：同上——check 输出落盘失败收 INVALID，不穿出控件循环。
    try:
        control_fd = open_output_dir(out_root, (positive.id,))
        try:
            write_at(control_fd, 'check-output.json', result.stdout)
        finally:
            os.close(control_fd)
    except Exception as error:  # noqa: BLE001 - 记录为 INVALID，不中止其余控件
        record.update(verdict='INVALID', reason=f'cannot record the check output: {error!r}')
        return record
    if result.stderr.strip():
        record['stderr'] = result.stderr.strip()[:2000]
    try:
        report = json.loads(result.stdout)
    except ValueError as error:
        record.update(verdict='INVALID', reason=f'cannot parse checker output: {error}')
        return record
    if result.returncode not in (0, 1):
        # 与 run_control 同口径：rc2（输入/环境不可用）是「没跑成」，不是判据 FAIL，
        # 更不能记成「应 PASS 的性质不成立」。
        record.update(verdict='INVALID',
                      reason=f'checker exit {result.returncode} (input unusable?): '
                             f'{result.stdout.strip()[:400]}')
        return record
    if result.returncode == 1:
        violations = report.get('violations') if isinstance(report, dict) else None
        reason = 'checker exit 1 (expected PASS)'
        if violations:
            reason += f'; violations {violations[:3]}'
        record.update(verdict='BROKEN', reason=reason)
        return record
    try:
        ok, reason = positive.verify(report)
    except Exception as error:  # noqa: BLE001
        record.update(verdict='INVALID', reason=f'verify failed: {error!r}')
        return record
    record.update(verdict='HELD' if ok else 'BROKEN', reason=reason)
    return record


# 判决词表只此一份（r3 sibling#2 / r4 sibling#1）：登记在 check_outcome_parity.py
# 的单一登记点（与 EXPECTED_* 同处），本执行器的 fold / tally / 期望 counts 与
# self_check 的期望 counts 全部同源读这里。各自写字面量时，只给一侧加值而漏改
# 另一侧，词表外判决不进任何计数键，回到 #2378 的空真形态。
NEGATIVE_VERDICTS = CHECK.NEGATIVE_VERDICTS
POSITIVE_VERDICTS = CHECK.POSITIVE_VERDICTS


def fold_verdict(record, vocabulary):
    """#2378：词表外判决一律记 INVALID，不静默丢弃。

    此前汇总只数 ``record['verdict'] == verdict``，判决落不进词表（含 None）时
    不进任何键：``SURVIVED == 0 and INVALID == 0`` 空真判 PASS。折叠在
    **record 本身**上做（不只改计数）：summary 的 controls / positives 条目
    直接显示 INVALID 与原因，审计可见；原判决与原原因保留在 reason 里。
    """
    verdict = record.get('verdict')
    if verdict not in vocabulary:
        record['verdict'] = 'INVALID'
        record['reason'] = (f'verdict {verdict!r} is outside the vocabulary '
                            f'{tuple(vocabulary)}; recorded as INVALID (#2378); '
                            f'previous reason: {record.get("reason")!r}')
    return record


def tally(records, vocabulary):
    """按词表计数（fold_verdict 之后所有判决都已在词表内，直数即可）。"""
    return {verdict: sum(1 for record in records if record['verdict'] == verdict)
            for verdict in vocabulary}


def build_parser():
    """正式解析器：选项表与 ``build_classifier`` **同源**（``PARSER_OPTIONS``）。

    两处各自硬编码时，「加一个选项但漏改预扫描集合」会让该选项被当成 ``--output``
    的值去失效同名目录，漂移无人发现（Codex r10 P2）；只比每组第一个拼写同样漏掉
    别名（r11 P2）。同源之后这两类漂移在结构上不存在——``build_classifier`` 用
    ``flags`` 全套注册，``_parse_optional`` 对别名一视同仁。
    """
    parser = argparse.ArgumentParser(
        allow_abbrev=False,
        description='Negative controls N1-N17 for the retirement outcome-parity '
                    'checker (#1477). Data-only mutations; no source text is touched.')
    defaults = {'data': str(DEFAULT_DATA), 'oracle': str(DEFAULT_ORACLE)}
    for key, flags, help_text in PARSER_OPTIONS:
        kwargs = {'help': help_text}
        if key == 'output':
            # 输出目录走**显式**选项、不做位置参数：解析前就能无歧义取值（见模块头）。
            kwargs['required'] = True
        else:
            kwargs['default'] = defaults[key]
        parser.add_argument(*flags, **kwargs)
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
    # 输出目录互斥（#1809 条目 4）：整轮运行（含权威失效、控件构造、逐条写
    # `N*/driver/*`、发布 summary）都在同一把 flock 下。被占时**阻塞等待**（理由见
    # `check_outcome_parity.output_lock`），等锁提示走 stderr，不静默跳过。
    # 锁在**权威失效之前**拿到：否则后到者可能在先到者写 summary 的窗口里读到半成品。
    try:
        with CHECK.output_lock(out_root):
            return run_locked(args, out_root)
    except CHECK.InputError as error:
        # 拿不到锁 / 锁路径不可用是「跑不了」（rc2），不是某个控件的判据 FAIL（rc1）。
        print(f'negative_controls: 输出目录不可用：{error}', file=sys.stderr)
        return 2


def run_locked(args, out_root):
    if not invalidate_stale_summary(out_root):
        return 2
    data = Path(args.data)
    try:
        base_rows, base_frames = read_round(data, 'round-01')
        round2_rows, round2_frames = read_round(data, 'round-02')
        # 前置：未变异的参考轮必须本来 PASS。否则「期望规则 fire」可能是既有违规，
        # 归因就不成立（例如 --data 指向的轮本就有 P4）。逐轮之外还要跑一次
        # 跨轮合并：单轮 P7 是空判，N11 的「跨轮漂移」归因必须先排除参考轮对
        # 本身已漂移。#1809 条目 5：覆盖 `--data` 下**全部**轮次，不写死 round-01/02。
        preflight = preflight_base_rounds(data, Path(args.oracle),
                                          reference_rounds(data))
        if preflight != 0:
            print('negative_controls: 未变异的参考轮已不 PASS 金样，负控无法归因；'
                  '先修数据或金样', file=sys.stderr)
            return 2
        controls = build_controls(base_rows, base_frames, round2_rows, round2_frames)
        # A8.6-R1 的延期正控 PC1–PC2 已随 #2294 按反转条款撤回（A8.6-…-R2），
        # 正控注册数归 0（EXPECTED_POSITIVES = 0）。条数守卫机制保留：将来再登记
        # 正控时在 controls 旁重建装配并核数，这里不得再空真放行。
        positives = []
    except CHECK.InputError as error:
        print(f'negative_controls: 输入不可用：{error}', file=sys.stderr)
        return 2
    except Exception as error:  # noqa: BLE001 - 控件构造失败同样按输入不可用收口
        print(f'negative_controls: cannot assemble controls: {error!r}', file=sys.stderr)
        return 2
    control_ids = [control.id for control in controls]
    if sorted(control_ids) != sorted(EXPECTED_CONTROL_IDS):
        # 控件 ID 名册与登记值不符（被删空 / 删部分 / 改名 / 重复 / 多了未知条目）：
        # #2378 拒绝而非缩小——按条数拦不住改名（一重复一缺失时条数不变），空集会让
        # 下面的 `SURVIVED == 0 and INVALID == 0` 空真判 PASS、rc=0（评审 r7 P1）。
        # 排序后逐元素比对，重复条目（名册同长、一条出现两次）也会不等。
        print('negative_controls: assembled control ids '
              f'{sorted(control_ids)} do not match the registered roster '
              f'{sorted(EXPECTED_CONTROL_IDS)} (#2378); refusing to judge an '
              'unexpected control set', file=sys.stderr)
        return 2
    positive_ids = [positive.id for positive in positives]
    if sorted(positive_ids) != sorted(EXPECTED_POSITIVE_IDS):
        # 正控集同一守卫（r9 合并补全，#2143；#2378 升级为 ID 名册）：删空
        # build_positives 时 positiveCounts 全 0，status 仍 PASS——「应当 PASS
        # 的性质」静默失保。
        print('negative_controls: assembled positive ids '
              f'{sorted(positive_ids)} do not match the registered roster '
              f'{sorted(EXPECTED_POSITIVE_IDS)} (#2378); refusing to judge an '
              'unexpected positive set', file=sys.stderr)
        return 2

    records = []
    positive_records = []
    try:
        for control in controls:  # 全部跑完再汇总，不遇败即停
            record = run_control(control, out_root, Path(args.oracle))
            records.append(record)
            print(f"{control.id}: {record['verdict']} ({record['reason']})")
        for positive in positives:
            record = run_positive(positive, out_root, Path(args.oracle))
            positive_records.append(record)
            print(f"{positive.id}: {record['verdict']} ({record['reason']})")

        # 汇总前核**实际跑出的 ids**（#2378）：构造后的名册守卫之外再核一次记录侧，
        # 拒绝而非缩小——记录侧缺一条 / 重复一条（run_control 丢记录、未来的早退
        # 分支）同样不得靠剩下的记录全绿判 PASS。rc2 收口，不发布 summary。
        run_control_ids = [record['id'] for record in records]
        if sorted(run_control_ids) != sorted(EXPECTED_CONTROL_IDS):
            print('negative_controls: recorded control ids '
                  f'{sorted(run_control_ids)} do not match the registered roster '
                  f'{sorted(EXPECTED_CONTROL_IDS)} (#2378); refusing to summarize '
                  'an unexpected control set', file=sys.stderr)
            return 2
        run_positive_ids = [record['id'] for record in positive_records]
        if sorted(run_positive_ids) != sorted(EXPECTED_POSITIVE_IDS):
            print('negative_controls: recorded positive ids '
                  f'{sorted(run_positive_ids)} do not match the registered roster '
                  f'{sorted(EXPECTED_POSITIVE_IDS)} (#2378); refusing to summarize '
                  'an unexpected positive set', file=sys.stderr)
            return 2

        # 词表外判决先折叠成 INVALID（#2378），再计数：不折叠的话这类判决不进任何
        # 键，SURVIVED/INVALID 双 0 空真判 PASS。
        records = [fold_verdict(record, NEGATIVE_VERDICTS)
                   for record in records]
        positive_records = [fold_verdict(record, POSITIVE_VERDICTS)
                            for record in positive_records]
        counts = tally(records, NEGATIVE_VERDICTS)
        positive_counts = tally(positive_records, POSITIVE_VERDICTS)
        # #2378：三张 counts 断言为期望值——名册 + 折叠之后 KILLED/HELD 必须恰好
        # 等于名册条数，SURVIVED / INVALID / BROKEN 必须为 0。这不再是「两个零」的
        # 空真式判定：任何一侧缺一条（未跑 / 未记 / 词表外）都让 counts 不等。
        expected_counts = dict.fromkeys(NEGATIVE_VERDICTS, 0)
        expected_counts['KILLED'] = EXPECTED_CONTROLS
        expected_positive_counts = dict.fromkeys(POSITIVE_VERDICTS, 0)
        expected_positive_counts['HELD'] = EXPECTED_POSITIVES
        summary = {'status': 'PASS' if counts == expected_counts
                   and positive_counts == expected_positive_counts else 'FAIL',
                   'controls': records, 'positives': positive_records,
                   'counts': counts, 'positiveCounts': positive_counts,
                   'oracle': str(args.oracle), 'data': str(args.data)}
        target = out_root / 'summary.json'
        # tmp 名带 pid：同目录并发两次执行时不能共用同一个 tmp（先完成者的 rename
        # 会把另一进程写了一半的内容发布成最终文件）。
        tmp = f'summary.json.{os.getpid()}.tmp'
        out_fd = open_output_dir(out_root)
        try:
            remove_at(out_fd, tmp)  # 只删链接自己/残留，绝不跟随
            write_at(out_fd, tmp, json.dumps(summary, ensure_ascii=False, indent=2) + '\n',
                     exclusive=True)
            try:
                # 原子发布：先写 *.tmp 再 replace，两步都相对**同一个** fd。
                os.replace(tmp, 'summary.json', src_dir_fd=out_fd, dst_dir_fd=out_fd)
            except OSError as error:
                remove_at(out_fd, tmp)  # 失败路径不把半成品留在目录里
                raise OutputError(f'cannot publish {target}: {error}') from error
        finally:
            os.close(out_fd)
    except OutputError as error:
        # 写盘失败是「跑不了」（rc2），不是「某个控件的判据 FAIL」（rc1）。
        print(f'negative_controls: 输出目录不可用：{error}', file=sys.stderr)
        return 2
    print(json.dumps({'status': summary['status'], 'counts': counts,
                      'positiveCounts': positive_counts,
                      'summary': str(target)}, ensure_ascii=False))
    return 0 if summary['status'] == 'PASS' else 1


if __name__ == '__main__':
    sys.exit(main())
