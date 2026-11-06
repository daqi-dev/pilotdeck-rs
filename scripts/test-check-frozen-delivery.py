#!/usr/bin/env python3
"""check-frozen-delivery.py 的用例（正控 + 负控）。

跑法：python3 -I scripts/test-check-frozen-delivery.py
用例都在临时 git 仓库里构造：一个 base 提交 + 一个交付提交。
负控必须让被测函数给出 False / block / 非零退出，不是「随便失败」。
"""

import hashlib
import importlib.util
import os
import shutil
import signal
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path

# 用例会 importlib 加载真装置（scripts/parity/check_frozen.py）：不写 __pycache__，
# 否则 scripts/parity/ 下的 .pyc 会被装置自己判成「unregistered bytecode」。
sys.dont_write_bytecode = True

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("cfd", HERE / "check-frozen-delivery.py")
cfd = importlib.util.module_from_spec(spec)
spec.loader.exec_module(cfd)

TOOL = HERE / "check-frozen-delivery.py"
TIMEOUT = 120


def git(root, *args):
    return subprocess.run(["git", "-C", str(root), *args], capture_output=True,
                          text=True, check=True, timeout=TIMEOUT).stdout.strip()


def sha(text):
    return hashlib.sha256(text.encode()).hexdigest()


def init_repo(root):
    git(root, "init", "-q")
    git(root, "config", "user.email", "test@example.invalid")
    git(root, "config", "user.name", "test")
    # 非 ASCII 路径的负控依赖这个默认值；本机全局配成 false 时它会变成伪负控。
    git(root, "config", "core.quotePath", "true")


def commit(root, message):
    git(root, "add", "-A")
    git(root, "commit", "-qm", message, "--allow-empty")


def run_tool(*args):
    return subprocess.run(["python3", "-I", str(TOOL), *args],
                          capture_output=True, text=True, timeout=TIMEOUT)


def write(root, name, text):
    (root / name).parent.mkdir(parents=True, exist_ok=True)
    (root / name).write_text(text)


def scripted_git(seq):
    """按序返回预设 ``(code, out, err)`` 的 git 替身；用完即 StopIteration。"""
    def call(*_args, **_kwargs):
        return seq.pop(0)
    return call


def row(root, kind, path, digest, text=None):
    """在 base 的 FROZEN.md 里给 path 写一条清单行（注册起点是行，不是 blob）。"""
    write(root, f"specs/{kind}/FROZEN.md",
          f"# fake FROZEN\n\n{digest}  {path}\n")
    if text is not None:
        write(root, path, text)


# parity 的规格名单现在与装置源交叉核对（读不出即阻塞），所以假装置也必须带
# 与真装置同形的 ``manifest_inventory``，否则每条 parity 用例都只会撞上「不可判定」。
PARITY_MANIFEST = ("\n\ndef manifest_inventory(root):\n"
                   "    names = ('README.md', 'acceptance.md', 'normalization.md',\n"
                   "             'accepted-diffs.md', 'multimodal.md')\n"
                   "    return {name: name for name in names}\n")


def fake_apparatus(root, kind, payload, source=""):
    if kind == 'parity' and 'manifest_inventory' not in source:
        source += PARITY_MANIFEST
    # 夹具只负责吐一份固定报文。真 ui 装置体带着可执行的 bootstrap（临时仓里
    # 必然哈希准入失败），所以那一种 source 打印完就退出：分析侧读到的字节不变。
    guard = ("raise SystemExit(0)  # fixture: real bootstrap is analysis-only\n"
             if 'import sys as _boot_sys' in source else "")
    body = "import json\nprint(json.dumps(" + repr(payload) + "))\n" + guard + source
    write(root, f"scripts/{kind}/check_frozen.py", body)
    # 装置自身在 base 已有清单行：它改字节时走「改已登记文件」的路，不该被当成首登记。
    write(root, f"specs/{kind}/FROZEN.md",
          f"# fake FROZEN\n\n{sha(body)}  ../../scripts/{kind}/check_frozen.py\n")


GW_COMMON = "A2_SCRIPTS = ('check_frozen.py',)\n"

UI_APPARATUS = ("APPARATUS_PATHS = ('scripts/ui-server-parity', 'scripts/exp008', "
                "'specs/ui-server-parity')\n"
                "SPEC_FILES = ('acceptance.md',)\n"
                "EXP008_FILES = ('compare.py',)\n"
                "TOOLCHAIN_FILES = ()\n")

# 真装置的另一半：真实登记范围来自 check_frozen.py 的 SPEC 与 apparatus_files()，
# 预检会与 apparatus.py 的 APPARATUS_PATHS 交叉核对，两边不一致即「不可判定」。
# 前缀是真装置的隔离 bootstrap（``apparatus.py`` 就是它自己按 ``__file__`` 装进来
# 的，换一份字节/换目录即不可判定），``ROOT`` 同理；两者都由真文件钉住。
UI_BOOTSTRAP = "\n".join(cfd._UI_BOOTSTRAP) + "\n"
UI_FROZEN = (UI_BOOTSTRAP
             + "\nfrom apparatus import scan, SPEC_FILES, EXP008_FILES, toolchain_files\n"
             "from pathlib import Path\n"
             "\nROOT = Path(__file__).resolve().parents[2]\n"
             "SPEC = ROOT / 'specs/ui-server-parity'\n"
             "REQUIRED = set(SPEC_FILES)\n"
             "\n\ndef apparatus_files(root):\n"
             "    files = {Path(p) for p in scan(root / 'scripts/ui-server-parity')}\n"
             "    files.update(Path(p) for p in scan(root / 'scripts/exp008'))\n"
             "    return files\n"
             "\n\ndef required_files(root, folder):\n"
             "    return ({folder / name for name in REQUIRED} | apparatus_files(root)\n"
             "            | {root / 'scripts/exp008' / name for name in EXP008_FILES}\n"
             "            | set(toolchain_files(root)))\n")


# 真装置的 bootstrap（``scripts/gateway-parity/check_frozen.py``：``# >>>`` 到
# ``# <<<`` 之间，含冷路径的 ``_spec``/``module_from_spec``/``exec``）。外部值
# ``common.A2_SCRIPTS`` 只有在这整块逐行都是被评审过的那一份时才可信，所以夹具
# 直接取材于被测模型自己的常量，并由「真文件 == 常量」的正控钉住（fix-r19）。
GW_BOOTSTRAP = ("# >>> apparatus bootstrap\n"
                + "\n".join(cfd._GATEWAY_BOOTSTRAP)
                + "\n# <<< apparatus bootstrap\n")


def gw_source(*names):
    """假 gateway 装置源：与真装置同形的 ``manifest_inventory``。

    模板 ``f'scripts/gateway-parity/{name}'`` 遍历 ``A2_SCRIPTS``，a5-19 用内联
    元组模板，额外成员写成完整路径字面量，spec 名是裸名元组——预检按这些表达式
    重建清单，而不是把函数体里所有字符串常量都当成成员。
    """
    extra = "".join(f"    paths.append('scripts/gateway-parity/{name}')\n" for name in names)
    return (GW_BOOTSTRAP + "A2_SCRIPTS = common.A2_SCRIPTS\n"
            "\ndef manifest_inventory(root):\n"
            "    names = ('acceptance.md', 'normalization.md', 'accepted-diffs.md')\n"
            "    paths = [f'scripts/gateway-parity/{name}' for name in A2_SCRIPTS]\n"
            + extra +
            "    paths += [f'scripts/a5-19/{name}' for name in ('run.py',)]\n"
            "    return {name: name for name in names}\n")


def two_apparatus(root):
    """parity + 一只真能判 READY 的 gateway-parity。

    用来证明「某只装置被静默丢掉」之后剩下的那只仍会报 READY（假 READY 方向），
    而不是「一只都不剩」被另一条规则拦住 —— 后者会让负控因为错误的原因变红。
    """
    fake_apparatus(root, "parity", {"status": "PASS", "failures": []})
    fake_apparatus(root, "gateway-parity", {"status": "PASS", "failures": []},
                   gw_source())
    write(root, "scripts/gateway-parity/common.py", GW_COMMON)
    write(root, "scripts/a5-19/run.py", "")
    body = (root / "scripts/gateway-parity/check_frozen.py").read_text()
    write(root, "specs/gateway-parity/FROZEN.md",
          "# fake FROZEN\n\n" + sha(body)
          + "  ../../scripts/gateway-parity/check_frozen.py\n"
          + sha("") + "  ../../scripts/a5-19/run.py\n")


def gw_inventory(check_frozen_body, common=None):
    """建一棵只有 gateway 装置两半的树，返回 ``gateway_inventory`` 的结果。

    ``common`` 缺省为字面元组；``check_frozen_body`` 是 check_frozen.py 全文，
    夹具自己决定 bootstrap 走不走形。
    """
    with tempfile.TemporaryDirectory() as td:
        root = Path(td)
        init_repo(root)
        write(root, "scripts/gateway-parity/common.py", GW_COMMON if common is None else common)
        write(root, "scripts/gateway-parity/check_frozen.py", check_frozen_body)
        commit(root, "base")
        return cfd.gateway_inventory(root)


HASH_LIST = "hash list differs from generated inventory; run check_frozen.py --write"

# 真装置的原样报文（三份 check_frozen.py 都带这个括号；预检按整串匹配）。
LACKS_CHAIN_MSG = ("hash change lacks exact new event chain "
                   "(or has stale/duplicate event)")

EVENT_FIELDS = ("Calibration-Evidence: test fixture\n"
                "Independent-Reviews: A <a@example.invalid>; B <b@example.invalid>\n"
                "Approved-By: test\n"
                "Approval-Basis: test fixture\n")


def delivery(root, kind, payload, files=(), events=(), source="", dirty=None,
             base_files=(), moves=(), removes=()):
    """base 提交（含两侧装置）→ 交付提交（files/events/moves）→ 可选工作树脏改动。"""
    init_repo(root)
    fake_apparatus(root, kind, payload, source)
    for name, text in base_files:
        write(root, name, text)
    commit(root, "base")
    git(root, "update-ref", "refs/remotes/origin/main", git(root, "rev-parse", "HEAD"))
    for old, new in moves:
        git(root, "mv", old, new)
    for name in removes:
        git(root, "rm", "-q", name)
    for name, text in files:
        write(root, name, text)
    for name, text in events:
        write(root, name, text)
    commit(root, "delivery")
    for name, text in (dirty or ()):
        write(root, name, text)  # 只改工作树，不提交
    return run_tool("--root", str(root), "--base", "origin/main")


def run():
    fails = 0
    skips = 0

    def case(name, ok, detail=""):
        nonlocal fails
        print(f"{'PASS' if ok else 'FAIL'}  {name}{'' if ok else '  ← ' + detail}")
        if not ok:
            fails += 1

    if not sys.flags.isolated:
        print("FAIL  必须以 python3 -I 运行（工具本身也要求隔离模式）")
        return 1

    # ---------------------------------------------------------- chain rules
    with tempfile.TemporaryDirectory() as td:
        root = Path(td)
        init_repo(root)
        row(root, "fake", "scripts/fake/x.py", sha("A\n"), "A\n")
        commit(root, "base")
        base = git(root, "rev-parse", "HEAD")
        write(root, "scripts/fake/x.py", "B\n")
        write(root, "specs/fake/frozen/20260101-e1.md",
              f"Change: {sha('A\n')} {sha('B\n')} scripts/fake/x.py\n")
        commit(root, "delivery")

        edges, _, issues = cfd.event_evidence(root, "fake", base)
        case("event_evidence 收齐一条已提交的 Change 行",
             len(edges) == 1 and not issues, f"edges={edges} issues={issues}")
        case("单条边精确链通过",
             cfd.chain_backed(root, "fake", edges, "scripts/fake/x.py", base) is True)
        case("断链被拒（边指向别的 old）",
             cfd.chain_backed(root, "fake", [("e.md", "scripts/fake/x.py", sha("X\n"), sha("B\n"))],
                              "scripts/fake/x.py", base) is False)

        # 工作树里写一个新文件，HEAD 没有它：不能用它的字节补链。
        write(root, "scripts/fake/y.py", "Y\n")
        case("工作树里的未提交文件不能补链",
             cfd.chain_backed(root, "fake", edges, "scripts/fake/y.py", base) is False)

        # 重复边：旧实现「找到一条 old==base 的边」就放行（假 PASS）。
        write(root, "specs/fake/frozen/20260101-e2.md",
              f"Change: {sha('A\n')} {sha('B\n')} scripts/fake/x.py\n")
        commit(root, "duplicate edge")
        dup, _, _ = cfd.event_evidence(root, "fake", base)
        case("重复边被拒（旧实现会假 PASS）",
             len(dup) == 2 and cfd.chain_backed(root, "fake", dup, "scripts/fake/x.py", base) is False)

        # 未提交的事件文件不能当证据。
        write(root, "specs/fake/frozen/20260101-e3.md",
              f"Change: {sha('A\n')} {sha('B\n')} scripts/fake/z.py\n")
        _, _, uncommitted = cfd.event_evidence(root, "fake", base)
        case("未提交的事件文件报阻塞而不是当证据",
             any("not committed" in issue for issue in uncommitted), f"{uncommitted}")

        # HEAD 里没有该行、工作树补上：证据仍只取 HEAD（P0 回归）。
        # 独立仓库：后面还要用 base/edges 做分类用例。
    with tempfile.TemporaryDirectory() as td:
        root = Path(td)
        init_repo(root)
        write(root, "scripts/fake/x.py", "C\n")
        write(root, "specs/fake/frozen/20260101-e4.md", "Change: -\n")
        commit(root, "base")
        base2 = git(root, "rev-parse", "HEAD")
        write(root, "scripts/fake/x.py", "D\n")
        commit(root, "change without row")
        write(root, "specs/fake/frozen/20260101-e4.md",
              f"Change: {sha('C\n')} {sha('D\n')} scripts/fake/x.py\n")  # 只改工作树
        forged, _, _ = cfd.event_evidence(root, "fake", base2)
        case("工作树补的 Change 行不算证据（只读 HEAD）",
             cfd.chain_backed(root, "fake", forged, "scripts/fake/x.py", base2) is False,
             f"edges={forged}")

    with tempfile.TemporaryDirectory() as td:
        root = Path(td)
        init_repo(root)
        row(root, "fake", "scripts/ui-server-parity/x.py", sha("A\n"), "A\n")
        write(root, "scripts/ui-server-parity/apparatus.py", UI_APPARATUS)
        write(root, "scripts/ui-server-parity/check_frozen.py", UI_FROZEN)
        write(root, "specs/ui-server-parity/FROZEN.md",
              f"# fake FROZEN\n\n{sha('A\n')}  ../../scripts/ui-server-parity/x.py\n")
        commit(root, "base")
        base = git(root, "rev-parse", "HEAD")
        write(root, "scripts/ui-server-parity/x.py", "B\n")
        write(root, "specs/fake/frozen/20260101-e1.md",
              f"Change: {sha('A\n')} {sha('B\n')} scripts/ui-server-parity/x.py\n")
        commit(root, "delivery")
        edges, _, _ = cfd.event_evidence(root, "fake", base)

        # ---------------------------------------------------- classification
        ctx = {"root": root, "kind": "ui-server-parity", "mb": base,
               "edges": edges, "pending": set()}
        case("ui-server ../../ 旧 pin 路径可解析",
             cfd.existing_file(root, "ui-server-parity", "../../scripts/ui-server-parity/x.py") is True)
        case("invalid path/hash ../../ 判 ok",
             cfd.classify(ctx, "invalid path/hash: ../../scripts/ui-server-parity/x.py")[0] == "ok")
        case("unregistered/missing 有链背书判 ok",
             cfd.classify(ctx, "unregistered/missing file: scripts/ui-server-parity/x.py")[0] == "ok")
        case("unregistered/missing 无此文件判 block",
             cfd.classify(ctx, "unregistered/missing file: scripts/fake/absent.py")[0] == "block")
        case("重复边下 unregistered/missing 判 block",
             cfd.classify(dict(ctx, edges=dup),
                          "unregistered/missing file: scripts/ui-server-parity/x.py")[0] == "block")
        case("未提交文件不算 existing_file",
             cfd.existing_file(root, "parity", "../../scripts/fake/nope.py") is False)
        case("认不出的失败消息 fail-closed",
             cfd.classify(ctx, "totally unknown failure")[0] == "block")

    # ------------------------------------------- 首登记：parity / gateway
    parity_payload = {"hashes_consistent": False, "ok": False, "fully_registered": False,
                      "checked": {}, "notes": [], "errors": [HASH_LIST],
                      "unregistered_skipped": ["scripts/parity/new.py"],
                      "notice": "未登记文件暂跳过"}

    with tempfile.TemporaryDirectory() as td:
        proc = delivery(Path(td), "parity", parity_payload,
                        files=[("scripts/parity/new.py", "NEW\n")])
        case("parity 新文件无事件链 → BLOCKED（旧实现假 READY）",
             proc.returncode == 1 and "BLOCKED 1 / READY 0" in proc.stdout,
             f"rc={proc.returncode}")

    with tempfile.TemporaryDirectory() as td:
        proc = delivery(Path(td), "parity", parity_payload,
                        files=[("scripts/parity/new.py", "NEW\n")],
                        events=[("specs/parity/frozen/20260101-p.md",
                                 f"Change: - {sha('NEW\n')} scripts/parity/new.py\n"
                                 + EVENT_FIELDS)])
        case("parity 新文件有 Change: - <hash> 事件链 → READY",
             proc.returncode == 0 and "BLOCKED 0 / READY 1" in proc.stdout,
             f"rc={proc.returncode} out={proc.stdout}")

    with tempfile.TemporaryDirectory() as td:
        # manifests 只收 scripts/parity/ 与五个固定 spec 名：另一个顶层文档
        # --write 不会登记，预检不能拿它的链当可交付（第二枚 P0）。
        payload = dict(parity_payload, unregistered_skipped=["specs/parity/foo.md"],
                       errors=[HASH_LIST, "specs/parity/foo.md: hash change lacks exact "
                                          "new event chain"])
        proc = delivery(Path(td), "parity", payload,
                        files=[("specs/parity/foo.md", "FOO\n")],
                        events=[("specs/parity/frozen/20260101-q.md",
                                 f"Change: - {sha('FOO\n')} specs/parity/foo.md\n"
                                 + EVENT_FIELDS)])
        case("parity 清单外的 specs/ 新文件 → BLOCKED（--write 登记不了）",
             proc.returncode == 1 and "BLOCKED 1 / READY 0" in proc.stdout,
             f"rc={proc.returncode}")

    with tempfile.TemporaryDirectory() as td:
        # 清单名字自带 specs/ 前缀时住在两个被扫描目录之外：registrable 按原样
        # 认它（--write 会登记），首登记扫描也必须在场，否则两问不一致 → 假 READY。
        proc = delivery(Path(td), "parity", parity_payload,
                        source=PARITY_MANIFEST.replace("'multimodal.md')",
                                                       "'multimodal.md', 'specs/shared/glossary.md')"),
                        files=[("specs/shared/glossary.md", "GLOSSARY\n")])
        case("parity 清单指向两目录之外的路径、无事件链 → BLOCKED（扫描范围与 registrable 一致）",
             proc.returncode == 1 and "BLOCKED 1 / READY 0" in proc.stdout,
             f"rc={proc.returncode} out={proc.stdout}")

    with tempfile.TemporaryDirectory() as td:
        proc = delivery(Path(td), "parity", parity_payload,
                        source=PARITY_MANIFEST.replace("'multimodal.md')",
                                                       "'multimodal.md', 'specs/shared/glossary.md')"),
                        files=[("specs/shared/glossary.md", "GLOSSARY\n")],
                        events=[("specs/parity/frozen/20260101-g.md",
                                 f"Change: - {sha('GLOSSARY\n')} specs/shared/glossary.md\n"
                                 + EVENT_FIELDS)])
        case("parity 清单指向两目录之外的路径、有 Change: - <hash> → READY（端到端正控）",
             proc.returncode == 0 and "BLOCKED 0 / READY 1" in proc.stdout,
             f"rc={proc.returncode} out={proc.stdout}")

    with tempfile.TemporaryDirectory() as td:
        # 清单里有、树里没有的名字是装置自己的错（--write 连哈希都取不到），
        # 预检不能替它编一条「首登记」链去要求。
        root = Path(td)
        init_repo(root)
        write(root, "scripts/parity/check_frozen.py",
              PARITY_MANIFEST.replace("'multimodal.md')",
                                      "'multimodal.md', 'specs/shared/absent.md')"))
        commit(root, "base")
        base = git(root, "rev-parse", "HEAD")
        added, issues = cfd.unregistered_added(root, "parity", base)
        case("清单里不存在的文件不算首登记",
             added == [] and not any('absent.md' in issue for issue in issues),
             f'added={added} issues={issues}')

    with tempfile.TemporaryDirectory() as td:
        # 清单源认不出（names 不是字面量元组）：连「目录外该扫什么」都不可判定，
        # 必须自己出一条阻塞，而不是只靠每个文件的 registrable=None 兜住。
        proc = delivery(Path(td), "parity", parity_payload,
                        source="\nSPEC_NAMES = ('README.md', 'acceptance.md')\n"
                               "\ndef manifest_inventory(root):\n"
                               "    names = SPEC_NAMES\n"
                               "    return {name: name for name in names}\n")
        case("parity 清单源认不出 → BLOCKED 且报扫描范围不可判定",
             proc.returncode == 1 and "cannot tell parity's manifest_inventory names"
             in proc.stdout,
             f"rc={proc.returncode} out={proc.stdout}")

    # gateway 同源问题：清单成员落在三个扫描目录之外时，--write 照原样登记
    # （check_frozen.py 的 manifest_path 对任何 '../../' 名都出行），预检也必须看。
    GW_SHARED = gw_source().replace(
        "    return {name: name for name in names}\n",
        "    paths += ['specs/shared/g.md']\n"
        "    return {name: name for name in names}\n")

    with tempfile.TemporaryDirectory() as td:
        root = Path(td)
        init_repo(root)
        write(root, "scripts/gateway-parity/common.py", GW_COMMON)
        write(root, "scripts/gateway-parity/check_frozen.py", GW_SHARED)
        commit(root, "base")
        case("gateway 清单成员落在扫描目录之外仍可登记",
             cfd.registrable(root, "gateway-parity", "specs/shared/g.md") is True)

    with tempfile.TemporaryDirectory() as td:
        proc = delivery(Path(td), "gateway-parity",
                        {"status": "FAIL", "failures": [HASH_LIST]},
                        source=GW_SHARED,
                        files=[("specs/shared/g.md", "G\n")],
                        base_files=[("scripts/gateway-parity/common.py", GW_COMMON)])
        case("gateway 清单指向扫描目录之外的路径、无事件链 → BLOCKED",
             proc.returncode == 1 and "BLOCKED 1 / READY 0" in proc.stdout,
             f"rc={proc.returncode} out={proc.stdout}")

    with tempfile.TemporaryDirectory() as td:
        proc = delivery(Path(td), "gateway-parity",
                        {"status": "FAIL", "failures": [HASH_LIST]},
                        source=GW_SHARED,
                        files=[("specs/shared/g.md", "G\n")],
                        events=[("specs/gateway-parity/frozen/20260101-s.md",
                                 f"Change: - {sha('G\n')} specs/shared/g.md\n"
                                 + EVENT_FIELDS)],
                        base_files=[("scripts/gateway-parity/common.py", GW_COMMON)])
        case("gateway 清单指向扫描目录之外的路径、有 Change: - <hash> → READY",
             proc.returncode == 0 and "BLOCKED 0 / READY 1" in proc.stdout,
             f"rc={proc.returncode} out={proc.stdout}")

    with tempfile.TemporaryDirectory() as td:
        # 形参默认值不产生 Name(Store)：不回退 external，否则漏掉新成员。
        root = Path(td)
        init_repo(root)
        write(root, "scripts/gateway-parity/common.py", GW_COMMON)
        write(root, "scripts/gateway-parity/check_frozen.py",
              GW_BOOTSTRAP + "A2_SCRIPTS = common.A2_SCRIPTS\n"
              "\ndef manifest_inventory(root, A2_SCRIPTS=('check_frozen.py', 'extra.py')):\n"
              "    paths = [f'scripts/gateway-parity/{name}' for name in A2_SCRIPTS]\n"
              "    return {name: name for name in paths}\n")
        commit(root, "base")
        case("gateway A2_SCRIPTS 走形参默认值 → 不可判定（不回退外部值）",
             cfd.gateway_inventory(root) is None)

    with tempfile.TemporaryDirectory() as td:
        # 模块级遮蔽：check_frozen.py 自己 `A2_SCRIPTS = common.A2_SCRIPTS + (...)`
        # 时解释器用的是本模块扩展后的元组，回退 common 的旧元组就漏判。
        root = Path(td)
        init_repo(root)
        write(root, "scripts/gateway-parity/common.py", GW_COMMON)
        write(root, "scripts/gateway-parity/check_frozen.py",
              GW_BOOTSTRAP + "A2_SCRIPTS = common.A2_SCRIPTS + ('extra.py',)\n"
              "\ndef manifest_inventory(root):\n"
              "    paths = [f'scripts/gateway-parity/{name}' for name in A2_SCRIPTS]\n"
              "    return {name: name for name in paths}\n")
        commit(root, "base")
        case("gateway 模块级重绑 A2_SCRIPTS → 不可判定（不取 common 旧值）",
             cfd.gateway_inventory(root) is None)

    with tempfile.TemporaryDirectory() as td:
        # 正控：原样的 `A2_SCRIPTS = common.A2_SCRIPTS` 仍按 common 解析。
        root = Path(td)
        init_repo(root)
        write(root, "scripts/gateway-parity/common.py", GW_COMMON)
        write(root, "scripts/gateway-parity/check_frozen.py",
              GW_BOOTSTRAP + "A2_SCRIPTS = common.A2_SCRIPTS\n"
              "\ndef manifest_inventory(root):\n"
              "    paths = [f'scripts/gateway-parity/{name}' for name in A2_SCRIPTS]\n"
              "    return {name: name for name in paths}\n")
        commit(root, "base")
        case("gateway 原样 A2_SCRIPTS = common.A2_SCRIPTS → 正常解析",
             cfd.gateway_inventory(root) == {'scripts/gateway-parity/check_frozen.py'})

    with tempfile.TemporaryDirectory() as td:
        # `common` 被别的来源顶替时，external 的旧元组就不可信了。
        root = Path(td)
        init_repo(root)
        write(root, "scripts/gateway-parity/common.py", GW_COMMON)
        write(root, "scripts/gateway-parity/check_frozen.py",
              GW_BOOTSTRAP.replace("_load('common', _apparatus / 'common.py')", "other")
              + "A2_SCRIPTS = common.A2_SCRIPTS\n"
              "\ndef manifest_inventory(root):\n"
              "    paths = [f'scripts/gateway-parity/{name}' for name in A2_SCRIPTS]\n"
              "    return {name: name for name in paths}\n")
        commit(root, "base")
        case("gateway common 另有来源 → 不可判定", cfd.gateway_inventory(root) is None)

    with tempfile.TemporaryDirectory() as td:
        # 解包目标同样是 Name(Store)，但不是 `X = common.X` 那一条语句。
        root = Path(td)
        init_repo(root)
        write(root, "scripts/gateway-parity/common.py", GW_COMMON)
        write(root, "scripts/gateway-parity/check_frozen.py",
              GW_BOOTSTRAP + "A2_SCRIPTS, _unused = (common.A2_SCRIPTS, None)\n"
              "\ndef manifest_inventory(root):\n"
              "    paths = [f'scripts/gateway-parity/{name}' for name in A2_SCRIPTS]\n"
              "    return {name: name for name in paths}\n")
        commit(root, "base")
        case("gateway A2_SCRIPTS 走解包目标 → 不可判定",
             cfd.gateway_inventory(root) is None)

    with tempfile.TemporaryDirectory() as td:
        # `from other import *` 能把 A2_SCRIPTS 整个换掉，字面元组看着没动。
        root = Path(td)
        init_repo(root)
        write(root, "scripts/gateway-parity/common.py", "from other import *\n" + GW_COMMON)
        write(root, "scripts/gateway-parity/check_frozen.py",
              GW_BOOTSTRAP + "A2_SCRIPTS = common.A2_SCRIPTS\n"
              "\ndef manifest_inventory(root):\n"
              "    paths = [f'scripts/gateway-parity/{name}' for name in A2_SCRIPTS]\n"
              "    return {name: name for name in paths}\n")
        commit(root, "base")
        case("gateway common 里的 import * → 不可判定（清单可能被覆盖）",
             cfd.gateway_inventory(root) is None)


    # ---------------------------------------------------------------- r17 负控
    # 外部值只有在 bootstrap 两行都是唯一绑定时才可信；任何一种别的绑法都
    # 必须「不可判定」，否则解析器看到的值不是解释器用的值（假 READY）。
    case("gateway 正控：照原样 bootstrap → 可判定（否则下面六条只是恒 None）",
         gw_inventory(GW_BOOTSTRAP + "A2_SCRIPTS = common.A2_SCRIPTS\n"
                      "\ndef manifest_inventory(root):\n"
                      "    paths = [f'scripts/gateway-parity/{name}' for name in A2_SCRIPTS]\n"
                      "    return {name: name for name in paths}\n") is not None)
    case("gateway A2_SCRIPTS 没有模块级绑定 → 不可判定（不再默认取 common）",
         gw_inventory(GW_BOOTSTRAP
                      + "\ndef manifest_inventory(root):\n"
                      "    paths = [f'scripts/gateway-parity/{name}' for name in A2_SCRIPTS]\n"
                      "    return {name: name for name in paths}\n") is None)
    case("gateway 模块级 walrus 重绑 A2_SCRIPTS → 不可判定",
         gw_inventory(GW_BOOTSTRAP + "A2_SCRIPTS = common.A2_SCRIPTS\n"
                      "if (A2_SCRIPTS := ('check_frozen.py',)):\n"
                      "    pass\n"
                      "\ndef manifest_inventory(root):\n"
                      "    paths = [f'scripts/gateway-parity/{name}' for name in A2_SCRIPTS]\n"
                      "    return {name: name for name in paths}\n") is None)
    case("gateway 模块级 for 重绑 A2_SCRIPTS → 不可判定",
         gw_inventory(GW_BOOTSTRAP + "A2_SCRIPTS = common.A2_SCRIPTS\n"
                      "for A2_SCRIPTS in (('check_frozen.py',),):\n"
                      "    break\n"
                      "\ndef manifest_inventory(root):\n"
                      "    paths = [f'scripts/gateway-parity/{name}' for name in A2_SCRIPTS]\n"
                      "    return {name: name for name in paths}\n") is None)
    case("gateway 模块级 with ... as 重绑 A2_SCRIPTS → 不可判定",
         gw_inventory(GW_BOOTSTRAP + "A2_SCRIPTS = common.A2_SCRIPTS\n"
                      "with open(__file__) as A2_SCRIPTS:\n"
                      "    pass\n"
                      "\ndef manifest_inventory(root):\n"
                      "    paths = [f'scripts/gateway-parity/{name}' for name in A2_SCRIPTS]\n"
                      "    return {name: name for name in paths}\n") is None)
    case("gateway _load 取的不是 common.py → 不可判定",
         gw_inventory(GW_BOOTSTRAP.replace("_apparatus / 'common.py'",
                                           "_apparatus / 'other.py'")
                      + "A2_SCRIPTS = common.A2_SCRIPTS\n"
                      "\ndef manifest_inventory(root):\n"
                      "    paths = [f'scripts/gateway-parity/{name}' for name in A2_SCRIPTS]\n"
                      "    return {name: name for name in paths}\n") is None)
    case("gateway _load 换成别的 loader → 不可判定",
         gw_inventory(GW_BOOTSTRAP.replace("_load = _guard.load", "_load = other.load")
                      + "A2_SCRIPTS = common.A2_SCRIPTS\n"
                      "\ndef manifest_inventory(root):\n"
                      "    paths = [f'scripts/gateway-parity/{name}' for name in A2_SCRIPTS]\n"
                      "    return {name: name for name in paths}\n") is None)
    case("gateway _load 二次绑定 → 不可判定（不取首个）",
         gw_inventory(GW_BOOTSTRAP + "_load = _guard.load\n"
                      "A2_SCRIPTS = common.A2_SCRIPTS\n"
                      "\ndef manifest_inventory(root):\n"
                      "    paths = [f'scripts/gateway-parity/{name}' for name in A2_SCRIPTS]\n"
                      "    return {name: name for name in paths}\n") is None)
    case("gateway common 由 import 别名绑定 → 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "from other import thing as common\n"
                      "A2_SCRIPTS = common.A2_SCRIPTS\n"
                      "\ndef manifest_inventory(root):\n"
                      "    paths = [f'scripts/gateway-parity/{name}' for name in A2_SCRIPTS]\n"
                      "    return {name: name for name in paths}\n") is None)

    # ---------------------------------------------------------------- r18 负控
    # r17 的两处守卫都只看「拼写」：`/` 的左操作数、`_guard`/`_apparatus` 自身的
    # 绑定，被解释器换成别的路径仍会给出确定性答案（假 READY）。以下每条都保留
    # 合法 bootstrap，只替换一处，正控在上面。
    _gw_tail = ("A2_SCRIPTS = common.A2_SCRIPTS\n"
                "\ndef manifest_inventory(root):\n"
                "    paths = [f'scripts/gateway-parity/{name}' for name in A2_SCRIPTS]\n"
                "    return {name: name for name in paths}\n")
    case("gateway _load 指向别的目录（左操作数不是 _apparatus）→ 不可判定",
         gw_inventory(GW_BOOTSTRAP.replace("_apparatus / 'common.py'",
                                           "_evil / 'common.py'") + _gw_tail) is None)
    case("gateway _apparatus 换成别的路径 → 不可判定",
         gw_inventory(GW_BOOTSTRAP.replace(
             "_apparatus = _here.parent if _here.name == 'controls' else _here",
             "_apparatus = Path('scripts/evil')") + _gw_tail) is None)
    case("gateway _here 不是 __file__ 派生 → 不可判定",
         gw_inventory(GW_BOOTSTRAP.replace(
             "_here = Path(__file__).resolve().parent", "_here = Path.cwd()")
             + _gw_tail) is None)
    case("gateway _guard 二次绑定成别的对象 → 不可判定",
         gw_inventory(GW_BOOTSTRAP + "_guard = other\n" + _gw_tail) is None)
    case("gateway _guard_path 指向别的文件 → 不可判定",
         gw_inventory(GW_BOOTSTRAP.replace(
             "_guard_path = _apparatus / '_guard.py'",
             "_guard_path = _apparatus / '_other.py'") + _gw_tail) is None)
    # 夹具真实性：上面整组都靠 GW_BOOTSTRAP 与真装置逐行同形。真 bootstrap 一旦
    # 合法演进（改名 / 换写法 / 多一行绑定），夹具会照旧全绿而 gateway_inventory
    # 对真仓恒不可判定（假 BLOCK），或反过来漏掉一处新路径（假 READY）。所以这
    # 里钉的是整块相等，并让真文件本身过一遍模型（冷路径由此真被执行到）。
    _real_gw = (HERE / 'gateway-parity' / 'check_frozen.py').read_text(encoding='utf-8')
    case("真装置 gateway bootstrap 与模型常量整块一致",
         cfd._bootstrap_lines(_real_gw, cfd._GATEWAY_BOOTSTRAP_ANCHORS)
         == list(cfd._GATEWAY_BOOTSTRAP))
    case("模型对真 gateway check_frozen.py 判为可判定（冷路径被执行到）",
         cfd._bootstrap_ok(cfd._parse(_real_gw), cfd._GATEWAY_BOOTSTRAP_ANCHORS,
                           cfd._GATEWAY_BOOTSTRAP) is True)

    # ---------------------------------------------------------------- r19 负控
    # r18 的两处守卫仍只钉「拼写里的文件名」：冷路径真正喂给 loader 的 ``_spec``、
    # exec 的字节、``module_from_spec`` 的接收者，以及 ``Path``/``sys`` 的来源都
    # 不在模型里，保留 ``_guard_path`` 当诱饵即可让运行时用另一份文件而预检照旧
    # 给出确定性清单（假 READY）。r19 改成整块逐行比对，以下每条只改一处。
    case("gateway _spec 指向别的路径 → 不可判定",
         gw_inventory(GW_BOOTSTRAP.replace(
             "_spec = importlib.util.spec_from_file_location('_guard', _guard_path)",
             "_spec = importlib.util.spec_from_file_location("
             "'_guard', Path('/tmp/shared/_guard.py'))") + _gw_tail) is None)
    case("gateway exec 的字节换成别的来源 → 不可判定",
         gw_inventory(GW_BOOTSTRAP.replace(
             "exec(compile(_guard_source, str(_guard_path), 'exec'), _guard.__dict__)",
             "exec(compile(_evil.read_bytes(), str(_guard_path), 'exec'), "
             "_guard.__dict__)") + _gw_tail) is None)
    case("gateway module_from_spec 的接收者换成别的对象 → 不可判定",
         gw_inventory(GW_BOOTSTRAP.replace(
             "_guard = importlib.util.module_from_spec(_spec)",
             "_guard = _evil.module_from_spec(_spec)") + _gw_tail) is None)
    case("gateway _apparatus 无条件取 _here.parent（controls 变体冒充）→ 不可判定",
         gw_inventory(GW_BOOTSTRAP.replace(
             "_apparatus = _here.parent if _here.name == 'controls' else _here",
             "_apparatus = _here.parent") + _gw_tail) is None)
    case("gateway Path 不是从 pathlib 导入（from evil import Path）→ 不可判定",
         gw_inventory(GW_BOOTSTRAP.replace("from pathlib import Path",
                                           "from evil import Path") + _gw_tail) is None)
    case("gateway common 在块外二次绑定 → 不可判定",
         gw_inventory(GW_BOOTSTRAP + "common = other\n" + _gw_tail) is None)
    case("gateway 块外 global 重绑 common（函数体内赋值）→ 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "def _r():\n    global common\n"
                        "    common = _load('common', Path('/tmp/shared/common.py'))\n"
                        "_r()\n" + _gw_tail) is None)
    case("gateway 块外 except ... as common → 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "try:\n    pass\nexcept Exception as common:\n    pass\n"
                      + _gw_tail) is None)
    case("gateway 块外 del common → 不可判定",
         gw_inventory(GW_BOOTSTRAP + "del common\n" + _gw_tail) is None)
    case("gateway 块外写 common.A2_SCRIPTS（属性目标）→ 不可判定",
         gw_inventory(GW_BOOTSTRAP + "common.A2_SCRIPTS = ('evil.py',)\n"
                      + _gw_tail) is None)
    case("gateway 块外 sys.modules 注入 guard（下标目标）→ 不可判定",
         gw_inventory(GW_BOOTSTRAP + "sys.modules['_guard'] = None\n"
                      + _gw_tail) is None)
    case("gateway 块外 setattr(common, ...)（反射写）→ 不可判定",
         gw_inventory(GW_BOOTSTRAP + "setattr(common, 'A2_SCRIPTS', ('evil.py',))\n"
                      + _gw_tail) is None)
    case("第二个 bootstrap 起始锚点 → 不可判定",
         gw_inventory(GW_BOOTSTRAP + "# >>> apparatus bootstrap\n" + _gw_tail) is None)
    case("bootstrap 助手来源名单覆盖模型块的每个导入（增删都要红）",
         {name for name, _ in cfd._BOOTSTRAP_HELPERS}
         == cfd._import_names(cfd._parse('\n'.join(cfd._GATEWAY_BOOTSTRAP))))
    _real_ui = (HERE / 'ui-server-parity' / 'check_frozen.py').read_text(encoding='utf-8')
    case("真装置 ui bootstrap 与模型常量整块一致",
         cfd._bootstrap_lines(_real_ui, cfd._UI_BOOTSTRAP_ANCHORS)
         == list(cfd._UI_BOOTSTRAP))
    case("模型对真 ui check_frozen.py 的 bootstrap 与 ROOT 判为可判定",
         cfd._bootstrap_ok(cfd._parse(_real_ui), cfd._UI_BOOTSTRAP_ANCHORS,
                           cfd._UI_BOOTSTRAP) is True
         and cfd._file_parents(cfd._sole_binding(cfd._parse(_real_ui), 'ROOT')))

    # ---------------------------------------------------------------- r21 负控
    # r20 的 `_foreign_writes` 仍按「拼写」认写：写的是 `common` 这个名字才算。
    # 换个没被点名的入口（`common.__dict__`、`object.__setattr__`、把 sys 起个别名
    # 或从 sys 里单独取 modules）就绕过去了；默认参数里的 walrus 在模块作用域绑定，
    # 而 `_module_scope_nodes` 跳过了整个 def。每条只改一处，正控在上面。
    case("gateway 块外 common.__dict__.update(...) → 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "common.__dict__.update({'A2_SCRIPTS': ('evil.py',)})\n"
                      + _gw_tail) is None)
    case("gateway 块外先取出 common.__dict__ 再改 → 不可判定",
         gw_inventory(GW_BOOTSTRAP + "_d = common.__dict__\n"
                      "_d['A2_SCRIPTS'] = ('evil.py',)\n" + _gw_tail) is None)
    case("gateway 块外把 common.__dict__ 交给不认识的函数 → 不可判定",
         gw_inventory(GW_BOOTSTRAP + "evil_mod(common.__dict__)\n"
                      + _gw_tail) is None)
    case("gateway 块外 getattr(common, '__dict__') → 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "getattr(common, '__dict__')['A2_SCRIPTS'] = ('evil.py',)\n"
                      + _gw_tail) is None)
    case("gateway 块外 object.__setattr__(common, ...) → 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "object.__setattr__(common, 'A2_SCRIPTS', ('evil.py',))\n"
                      + _gw_tail) is None)
    case("gateway 块外 builtins.setattr(common, ...) → 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "import builtins\nbuiltins.setattr(common, 'A2_SCRIPTS', ())\n"
                      + _gw_tail) is None)
    case("gateway 块外 import sys as _s 后注入 _s.modules → 不可判定",
         gw_inventory(GW_BOOTSTRAP + "import sys as _s\n_s.modules['_guard'] = None\n"
                      + _gw_tail) is None)
    case("gateway 块外 from sys import modules as _m 后下标注入 → 不可判定",
         gw_inventory(GW_BOOTSTRAP + "from sys import modules as _m\n_m['_guard'] = None\n"
                      + _gw_tail) is None)
    case("gateway 块外 sys.modules.update(...) → 不可判定",
         gw_inventory(GW_BOOTSTRAP + "sys.modules.update({'_guard': None})\n"
                      + _gw_tail) is None)
    case("gateway 块外 sys.path.insert(...) → 不可判定",
         gw_inventory(GW_BOOTSTRAP + "sys.path.insert(0, '/tmp/shared')\n"
                      + _gw_tail) is None)
    case("gateway 块外 del common.A2_SCRIPTS（属性目标）→ 不可判定",
         gw_inventory(GW_BOOTSTRAP + "del common.A2_SCRIPTS\n" + _gw_tail) is None)
    case("gateway 默认参数里的 walrus 在模块作用域绑定 common → 不可判定",
         gw_inventory(GW_BOOTSTRAP + "def _f(_x=(common := None)):\n    pass\n"
                      + _gw_tail) is None)
    # 换页符：`str.splitlines` 把它当换行，`ast` 不当。块里多一个 `\f` 行会让
    # 行索引整体错位，块后第一行的写就被当成「块内」跳过（假 READY）。
    case("gateway 块内含换页符 + 块后第一行注入 sys.modules → 不可判定",
         gw_inventory(GW_BOOTSTRAP.replace("import tempfile", "\f\nimport tempfile")
                      + "sys.modules['_guard'] = None\n" + _gw_tail) is None)
    case("bootstrap 整块被塞进三引号字符串 → 不可判定（文字不是行为）",
         gw_inventory('_QUOTED = """\n' + GW_BOOTSTRAP + '"""\n' + _gw_tail) is None)
    # 行索引要跟 `ast` 的 lineno 同一套口径：tokenizer 把 `\f` 当空白、把单独的
    # `\r` 当换行，`str.splitlines` / `split('\n')` 两头都各错一半。
    case("块内换页符不产生额外行（不是 tokenizer 的换行）",
         cfd._source_lines("a = 1\n\f\nb = 2\n") == ["a = 1", "\f", "b = 2", ""])
    case("bootstrap 用 CR 行尾（tokenizer 当换行）→ 仍可判定",
         gw_inventory(GW_BOOTSTRAP.replace('\n', '\r')
                      + _gw_tail.replace('\n', '\r')) is not None)
    case("块外模块级 lambda 不误判、不崩溃 → 仍可判定",
         gw_inventory(GW_BOOTSTRAP + "_f = lambda: common\n" + _gw_tail) is not None)

    # ---------------------------------------------------------------- r22 负控
    # 名字匹配只管「按名字写」的那一半：把对象先取到别名里（`_c = common`）、
    # 交给一个形参（`def _swap(x): x.A = ...`）、把 sys 别名绑在函数体里、或者
    # 用 `from sys import *` / `operator.setitem` / `getattr(sys, 'modules')`
    # 换一种拼写，都能让 `Name(Store)` 计数与根名匹配同时落空。
    case("gateway 块外 _c = common 后按别名写属性 → 不可判定",
         gw_inventory(GW_BOOTSTRAP + "_c = common\n_c.A2_SCRIPTS = ('evil.py',)\n"
                      + _gw_tail) is None)
    case("gateway 块外 _d = common.__dict__ 后按别名写下标 → 不可判定",
         gw_inventory(GW_BOOTSTRAP + "_d = common.__dict__\n_d['A2_SCRIPTS'] = ()\n"
                      + _gw_tail) is None)
    case("gateway 块外 _m = sys.modules 后按别名注入 → 不可判定",
         gw_inventory(GW_BOOTSTRAP + "_m = sys.modules\n_m['_guard'] = None\n"
                      + _gw_tail) is None)
    case("gateway 块外 _m = sys.modules；_p = _m 二级别名 → 不可判定",
         gw_inventory(GW_BOOTSTRAP + "_m = sys.modules\n_p = _m\n_p['_guard'] = None\n"
                      + _gw_tail) is None)
    case("gateway 函数体内 import sys as _s 后注入 → 不可判定（表是进程级）",
         gw_inventory(GW_BOOTSTRAP
                      + "def _swap():\n    import sys as _s\n"
                        "    _s.modules['_guard'] = None\n_swap()\n" + _gw_tail) is None)
    # 别名导入这条由 `_alias_roots` 兜住（`_s` 从 `sys` 换名而来）；换成
    # `from sys import modules as _m` 时别名不重复种子名，只有 `_sys_roots`
    # 全树扫才看得见函数体里的这次导入。
    case("gateway 函数体内 from sys import modules as _m 后注入 → 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "def _swap():\n    from sys import modules as _m\n"
                        "    _m['_guard'] = None\n_swap()\n" + _gw_tail) is None)
    case("gateway 块外 from sys import * 后按裸名注入 → 不可判定",
         gw_inventory(GW_BOOTSTRAP + "from sys import *\nmodules['_guard'] = None\n"
                      + _gw_tail) is None)
    case("from sys import * 让两张表按裸名可达（导入表模型本身）",
         {'modules', 'path'} <= cfd._sys_roots(cfd._parse("from sys import *\n"))[1])
    case("gateway 块外 def _swap(x): x.A2_SCRIPTS = ... 后 _swap(common) → 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "def _swap(x):\n    x.A2_SCRIPTS = ('evil.py',)\n"
                        "_swap(common)\n" + _gw_tail) is None)
    case("gateway 块外 _f = setattr 后反射写 → 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "_f = setattr\n_f(common, 'A2_SCRIPTS', ('evil.py',))\n"
                      + _gw_tail) is None)
    case("gateway 块外 getattr(builtins, 'setattr')(...) → 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "import builtins\n"
                        "getattr(builtins, 'setattr')(common, 'A2_SCRIPTS', ())\n"
                      + _gw_tail) is None)
    case("gateway 块外 getattr(sys, 'modules') 后下标注入 → 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "getattr(sys, 'modules')['_guard'] = None\n" + _gw_tail) is None)
    case("gateway 块外 operator.setitem(sys.modules, ...) → 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "import operator\n"
                        "operator.setitem(sys.modules, '_guard', None)\n"
                      + _gw_tail) is None)
    # 另一端：放宽只放到「不是块绑定的对象、也不是 sys 的两张表」为止。
    case("块外 sys.argv.append(...) 不是在改导入表 → 仍可判定",
         gw_inventory(GW_BOOTSTRAP + "sys.argv.append('--x')\n" + _gw_tail) is not None)
    case("块外普通对象的 __class__ 读与 '__dict__' 字符串 → 仍可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "class _C:\n    pass\n"
                        "if _C().__class__ is _C:\n    _NOTE = 'use __dict__ here'\n"
                      + _gw_tail) is not None)
    case("块外 _s = sys.path 后 append 仍在改导入路径 → 不可判定",
         gw_inventory(GW_BOOTSTRAP + "_p = sys.path\n_p.append('/tmp/shared')\n"
                      + _gw_tail) is None)
    case("helper 来源：from evil import * 使名字来源不可判定",
         cfd._import_origin(cfd._parse("from pathlib import Path\n"
                                       "from evil import *\n"),
                            'Path', 'pathlib') is False)
    case("块外取回的函数直接调用（factory()(common, ...)）→ 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "factory()(common, 'A2_SCRIPTS', ('evil.py',))\n"
                      + _gw_tail) is None)
    case("清单函数里嵌套 def → 清单来源不可判定（作用域认不出就不猜）",
         gw_inventory(gw_source().replace(
             "    return {name: name for name in names}",
             "    def _helper():\n"
             "        extra = ('evil.md',)\n"
             "        return [f'scripts/a5-19/{name}' for name in extra]\n"
             "    return {name: name for name in names}")) is None)
    case("同名 def 只在嵌套作用域里出现也算二次定义 → 不可判定",
         cfd._function_node(cfd._parse(
             "def required_files(root):\n    pass\n"
             "class _A:\n    def required_files(self):\n        pass\n"),
             'required_files') is None)

    # ---------------------------------------------------------------- r23 负控
    # 别名闭包要覆盖每一种把对象换个名字的绑定形态，并且走遍所有作用域：函数体
    # 里的 `_c = common` 与模块级的那句等价；链式赋值、解包、`for` 目标、海象、
    # 改名导入都是同一件事。另一端由「按形参位置判定」守住：把值交给不碰它的
    # 那个形参不是写，`check_append_only(ROOT, base, notes)` 就是这种调用。
    case("gateway 函数体内 _c = common 后写属性 → 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "def _swap():\n    _c = common\n"
                        "    _c.A2_SCRIPTS = ('evil.py',)\n_swap()\n" + _gw_tail) is None)
    case("gateway 块外链式赋值 _c = _d = common → 不可判定",
         gw_inventory(GW_BOOTSTRAP + "_c = _d = common\n_d.A2_SCRIPTS = ()\n"
                      + _gw_tail) is None)
    case("gateway 块外解包 _c, _x = (common, None) → 不可判定",
         gw_inventory(GW_BOOTSTRAP + "_c, _x = (common, None)\n_c.A2_SCRIPTS = ()\n"
                      + _gw_tail) is None)
    case("gateway 块外 for 目标取到 common → 不可判定",
         gw_inventory(GW_BOOTSTRAP + "for _c in (common,):\n    _c.A2_SCRIPTS = ()\n"
                      + _gw_tail) is None)
    case("gateway 块外海象绑定 common → 不可判定",
         gw_inventory(GW_BOOTSTRAP + "if (_c := common):\n    _c.A2_SCRIPTS = ()\n"
                      + _gw_tail) is None)
    case("gateway 块外 from apparatus import common as _c → 不可判定",
         gw_inventory(GW_BOOTSTRAP + "from apparatus import common as _c\n"
                      "_c.A2_SCRIPTS = ()\n" + _gw_tail) is None)
    case("gateway 块外 _p = sys.path；_p.insert(...) → 不可判定",
         gw_inventory(GW_BOOTSTRAP + "_p = sys.path\n_p.insert(0, '/tmp/x')\n"
                      + _gw_tail) is None)
    case("gateway 块外 sys.meta_path.append(...) → 不可判定",
         gw_inventory(GW_BOOTSTRAP + "sys.meta_path.append(None)\n" + _gw_tail) is None)
    case("gateway 块外 from operator import setitem 后调用 → 不可判定",
         gw_inventory(GW_BOOTSTRAP + "from operator import setitem\n"
                      "setitem(sys.modules, '_guard', None)\n" + _gw_tail) is None)
    case("gateway 块外 from builtins import setattr as _f → 不可判定",
         gw_inventory(GW_BOOTSTRAP + "from builtins import setattr as _f\n"
                      "_f(common, 'A2_SCRIPTS', ())\n" + _gw_tail) is None)
    case("gateway 块外 getattr(common, <变量名>) → 不可判定",
         gw_inventory(GW_BOOTSTRAP + "_n = '__dict__'\ngetattr(common, _n)\n"
                      + _gw_tail) is None)
    case("形参就地改按位置判：改的是第二个形参 → 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "def _swap(_keep, x):\n    x.update({'A2_SCRIPTS': ()})\n"
                        "_swap(None, common)\n" + _gw_tail) is None)
    case("把 common 交给不碰它的那个形参 → 仍可判定（位置判定不放宽）",
         gw_inventory(GW_BOOTSTRAP
                      + "def _keep(notes, folder):\n    notes.append(folder)\n"
                        "_keep([], common)\n" + _gw_tail) is not None)
    case("形参就地改的函数只用非块对象调用 → 仍可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "def _swap(x):\n    x.update({'A2_SCRIPTS': ()})\n"
                        "_swap(object())\n" + _gw_tail) is not None)
    case("两跳传参：_a 把 common 交给就地改的形参 → 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "def _swap(x):\n    x.update({'A2_SCRIPTS': ()})\n"
                        "def _a(y):\n    _swap(y)\n_a(common)\n" + _gw_tail) is None)
    case("两跳传参按关键字对形参：common 落在被改的形参 → 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "def _swap(keep, x):\n    x.update({'A2_SCRIPTS': ()})\n"
                        "def _a(y):\n    _swap(x=y, keep=None)\n_a(common)\n"
                      + _gw_tail) is None)
    case("两跳传参按关键字对形参：改的形参没拿到 common → 仍可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "def _swap(keep, x):\n    x.update({'A2_SCRIPTS': ()})\n"
                        "def _a(y):\n    _swap(keep=y, x=None)\n_a(common)\n"
                      + _gw_tail) is not None)
    case("两跳传参：common 落在不被改的那个形参 → 仍可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "def _keep(notes, folder):\n    notes.append(folder)\n"
                        "def _a(y):\n    _keep([], y)\n_a(common)\n"
                      + _gw_tail) is not None)
    case("三跳传参（定义倒序）：链要跟到不动点才拦得住 → 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "def _a(y):\n    _b(y)\n"
                        "def _b(z):\n    _swap(z)\n"
                        "def _swap(x):\n    x.update({'A2_SCRIPTS': ()})\n"
                        "_a(common)\n" + _gw_tail) is None)

    # ---------------------------------------------------------------- r24 负控
    # r23 把「谁被改」和「实参落在哪个形参」分开了，但两侧的对位仍按字面拼写：
    # 循环/解包目标个数不匹配就整条丢掉、`*`/`**` 展开直接跳过、形参换个名字
    # 就地改就看不见、默认值里的对象不算传参、属性形式调用完全没查；`sys` 的
    # 模块名与表名混在一个集合里，`from sys import path as _p` 因此两边都算。
    case("gateway 块外 for 字面序列与目标个数不匹配 → 循环变量仍可能拿到 common",
         gw_inventory(GW_BOOTSTRAP
                      + "for _c in (common, common):\n    _c.A2_SCRIPTS = ()\n"
                      + _gw_tail) is None)
    case("gateway 块外 for 遍历 set 字面量（迭代顺序不由文件固定）→ 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "for _c, _x in {(None, common), (common, None)}:\n"
                        "    _x.A2_SCRIPTS = ()\n" + _gw_tail) is None)
    case("gateway 块外解包的星号目标也要算进去 → 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "_x, *_rest = (None, common, None)\n"
                        "_rest[0].A2_SCRIPTS = ()\n" + _gw_tail) is None)
    case("gateway from sys import path as _p 后就地改 → 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "from sys import path as _p\n_p.insert(0, 'evil')\n"
                      + _gw_tail) is None)
    case("gateway from sys import modules as _m 后就地改 → 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "from sys import modules as _m\n_m.update({'_guard': None})\n"
                      + _gw_tail) is None)
    case("gateway 调用点 * 展开把 common 递给被改形参 → 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "def _swap(x):\n    x.update({'A2_SCRIPTS': ()})\n"
                        "_swap(*(common,))\n" + _gw_tail) is None)
    case("gateway 调用点 ** 展开 → 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "def _swap(x):\n    x.update({'A2_SCRIPTS': ()})\n"
                        "_swap(**{'x': common})\n" + _gw_tail) is None)
    case("gateway 被调函数内换名后就地改 → 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "def _swap(x):\n    y = x\n    y.update({'A2_SCRIPTS': ()})\n"
                        "_swap(common)\n" + _gw_tail) is None)
    case("gateway 调用方换名后再传 → 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "def _swap(x):\n    x.update({'A2_SCRIPTS': ()})\n"
                        "def _a(y):\n    z = y\n    _swap(z)\n_a(common)\n"
                      + _gw_tail) is None)
    case("gateway 形参互相赋值（x = y; y = x）不挂起，改名后的写仍算数 → 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "def _swap(x, y):\n    x = y\n    y = x\n"
                        "    x.update({'A2_SCRIPTS': ()})\n"
                        "_swap(None, common)\n" + _gw_tail) is None)
    case("gateway 默认参数里带 common、调用点不传 → 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "def _swap(x=common):\n    x.update({'A2_SCRIPTS': ()})\n"
                        "_swap()\n" + _gw_tail) is None)
    case("gateway 以属性形式调用被改函数 → 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "class _C:\n    def _swap(self, x):\n"
                        "        x.update({'A2_SCRIPTS': ()})\n"
                        "_C()._swap(common)\n" + _gw_tail) is None)
    case("gateway 绑定方法的实例形参不叫 self，位置对位不能靠名字猜 → 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "class _C:\n    def _swap(m, x):\n"
                        "        x.update({'A2_SCRIPTS': ()})\n"
                        "_C()._swap(common)\n" + _gw_tail) is None)
    # 接收者分支：调用点看不出 `_swap` 是不是绑定方法，所以只要被看护对象出现在
    # 接收者位置就按「交出去了」算。这里钉的是模型的 fail-closed 回答，不是一条
    # 运行期真会跑的写法（模块对象上没有 `_swap`）。
    case("gateway 属性调用的接收者是被看护对象 → 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "def _swap(x):\n    x.update({'A2_SCRIPTS': ()})\n"
                        "common._swap(None)\n" + _gw_tail) is None)
    case("gateway 传递链上的属性调用也要按绑定方法对位 → 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "class _C:\n    def _swap(self, x):\n"
                        "        x.update({'A2_SCRIPTS': ()})\n"
                        "def _a(y):\n    _C()._swap(y)\n_a(common)\n"
                      + _gw_tail) is None)
    case("gateway 传递链上的 * 展开 → 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "def _swap(x):\n    x.update({'A2_SCRIPTS': ()})\n"
                        "def _a(y):\n    _swap(*y)\n_a((common,))\n"
                      + _gw_tail) is None)
    case("gateway 无主 lambda（直接当实参）里的就地改 → 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "sorted((common,), key=lambda o: o.update({'A2_SCRIPTS': ()}))\n"
                      + _gw_tail) is None)
    case("gateway 链式赋值把 lambda 绑到第二个名字 → 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "_f = _g = lambda x: x.update({'A2_SCRIPTS': ()})\n"
                        "_g(common)\n" + _gw_tail) is None)
    case("gateway 海象绑定的 lambda → 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "(_f := lambda x: x.update({'A2_SCRIPTS': ()}))(common)\n"
                      + _gw_tail) is None)
    case("gateway from sys import modules as _m 后解包传播表别名 → 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "from sys import modules as _m\n"
                        "_p, _q = _m, None\n"
                        "_p.update({'apparatus': None})\n" + _gw_tail) is None)
    case("gateway 解包右值是 set 字面量（迭代顺序不由文件固定）→ 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "_a, _b = {common, None}\n"
                        "_b.A2_SCRIPTS = ()\n" + _gw_tail) is None)
    case("gateway for 遍历 dict 字面量（键）→ 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "for _k in {common: None}:\n    _k.A2_SCRIPTS = ()\n"
                      + _gw_tail) is None)

    # ---------------------------------------------------------------- r26 负控
    # 复审把「字面量展开」这一族补齐：`*` / `**` 展开贡献的值文件里没写出来，
    # 解包右值是 dict 时拿到的是键；同族的两处过度拦截（属性调用的被调方其实
    # 不写、无主 lambda 只碰自己的行）也要能判「可判定」，否则合法交付被挡住。
    case("gateway for 遍历 * 展开的字面序列 → 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "_xs = (common,)\nfor _k in [*_xs]:\n"
                        "    _k.A2_SCRIPTS = ()\n" + _gw_tail) is None)
    case("gateway for 遍历 ** 展开的 dict 字面量 → 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "for _k in {**{common: 0}}:\n    _k.A2_SCRIPTS = ()\n"
                      + _gw_tail) is None)
    case("gateway 解包右值是 dict 字面量（拿到的是键）→ 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "_a, _b = {common: 1, 'x': 2}\n_a.A2_SCRIPTS = ()\n"
                      + _gw_tail) is None)
    case("gateway from sys import modules as _m 后解包 dict 字面量 → 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "from sys import modules as _m\n"
                        "_p, _q = {_m: 1, 'x': 2}\n"
                        "_p.update({'apparatus': None})\n" + _gw_tail) is None)
    case("gateway 属性调用 + 被看护默认参数、调用点不传 → 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "class _C:\n    def _swap(self, x=common):\n"
                        "        x.update({'A2_SCRIPTS': ()})\n"
                        "_C()._swap()\n" + _gw_tail) is None)
    case("gateway 绑定方法先取别名再调用 → 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "class _C:\n    def _swap(self, x):\n"
                        "        x.update({'A2_SCRIPTS': ()})\n"
                        "_g = _C()._swap\n_g(common)\n" + _gw_tail) is None)
    case("gateway 被调函数内解包形参后就地改 → 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "def _swap(x):\n    _a, _b = x, None\n"
                        "    _a.update({'A2_SCRIPTS': ()})\n_swap(common)\n"
                      + _gw_tail) is None)
    case("gateway 属性调用的被调方不写 → 仍可判定（不过度拦截）",
         gw_inventory(GW_BOOTSTRAP
                      + "class _C:\n    def _noop(self, x):\n        return x\n"
                        "def _a(y):\n    _C()._noop(y)\n_a(common)\n"
                      + _gw_tail) is not None)
    case("gateway 无主 lambda 只碰自己的行 → 仍可判定（不过度拦截）",
         gw_inventory(GW_BOOTSTRAP
                      + "sorted([{'k': 0}], key=lambda o: o.setdefault('k', 1))\n"
                      + _gw_tail) is not None)

    # ---------------------------------------------------------------- r27 负控
    # r26 的复审把三处收得过紧 / 过松：sys 模块名在解包时被从两张表里同时挤掉
    # （回归）；无主 lambda 的「所在调用点」被当成唯一调用点（partial 换壳后
    # 假通过）；接收者/属性名只按裸名匹配（`[_m][0].update(...)` /
    # `getattr([common][0], _n)` 绕过去）。另一头 `_C()._swap(*rows)` 这类干净
    # 展开不该再被判不可判定。
    case("gateway import sys as _s 后解包模块名 → 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "import sys as _s\n_p, _q = _s, 0\n"
                        "_p.modules['apparatus'] = None\n" + _gw_tail) is None)
    case("gateway 无主 lambda 被 partial 包一层再调用 → 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "from functools import partial\n"
                        "_f = partial(lambda o: o.update({'A2_SCRIPTS': ()}))\n"
                        "_f(common)\n" + _gw_tail) is None)
    case("gateway 绑定方法在容器里取别名 → 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "class _C:\n    def _swap(self, x):\n"
                        "        x.update({'A2_SCRIPTS': ()})\n"
                        "_g, _h = _C()._swap, None\n_g(common)\n" + _gw_tail) is None)
    case("gateway getattr 取绑定方法再调用 → 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "class _C:\n    def _swap(self, x):\n"
                        "        x.update({'A2_SCRIPTS': ()})\n"
                        "_g = getattr(_C(), '_swap')\n_g(common)\n" + _gw_tail) is None)
    case("gateway 被调函数内解包带星号目标 → 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "def _swap(x):\n    _a, *_b = None, x\n"
                        "    _b[0].update({'A2_SCRIPTS': ()})\n_swap(common)\n"
                      + _gw_tail) is None)
    case("gateway 被调函数内解包形参后属性写 → 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "def _swap(x):\n    _a, _b = x, None\n"
                        "    _a.A2_SCRIPTS = ()\n_swap(common)\n"
                      + _gw_tail) is None)
    case("gateway 表别名装进容器后就地改 → 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "_m = sys.modules\n[_m][0].update({'apparatus': None})\n"
                      + _gw_tail) is None)
    case("gateway getattr 的接收者装进容器 → 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "_n = A2_SCRIPTS\ngetattr([common][0], _n)\n"
                      + _gw_tail) is None)
    case("gateway 属性调用展开的是干净数据 → 仍可判定（不过度拦截）",
         gw_inventory(GW_BOOTSTRAP
                      + "class _C:\n    def _swap(self, x):\n"
                        "        x.update({'A2_SCRIPTS': ()})\n"
                        "rows = [{'k': 0}]\n_C()._swap(*rows)\n"
                      + _gw_tail) is not None)
    case("gateway 别名指向不写的绑定方法 → 仍可判定（不过度拦截）",
         gw_inventory(GW_BOOTSTRAP
                      + "class _C:\n    def _noop(self, x):\n        return x\n"
                        "_g = _C()._noop\n_g(common)\n" + _gw_tail) is not None)
    case("gateway 同名函数两条定义 + 零实参属性调用 → 不崩（按默认值判不可判定）",
         gw_inventory(GW_BOOTSTRAP
                      + "def _swap(x=common):\n    x.update({'A2_SCRIPTS': ()})\n"
                        "class _C:\n    def _swap(self, x=common):\n"
                        "        x.update({'A2_SCRIPTS': ()})\n_C()._swap()\n"
                      + _gw_tail) is None)
    case("gateway 无主 lambda 交给本文件 helper，由 helper 喂被看护对象 → 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "def _apply(f):\n    return f(common)\n"
                        "_apply(lambda o: o.update({'A2_SCRIPTS': ()}))\n"
                      + _gw_tail) is None)
    case("gateway 就地改的方法绑到名字后再调用 → 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "_g = common.update\n_g({'A2_SCRIPTS': ()})\n"
                      + _gw_tail) is None)
    case("gateway 绑定方法只给了被改形参的无关默认值 → 仍可判定（不过度拦截）",
         gw_inventory(GW_BOOTSTRAP
                      + "class _C:\n    def _swap(self, x, notes=common):\n"
                        "        x.update({'A2_SCRIPTS': ()})\n_C()._swap(1)\n"
                      + _gw_tail) is not None)
    case("gateway 属性调用展开的是干净字面量 → 仍可判定（不过度拦截）",
         gw_inventory(GW_BOOTSTRAP
                      + "class _C:\n    def _swap(self, x):\n"
                        "        x.update({'A2_SCRIPTS': ()})\n"
                        "obj = _C()\nobj._swap(*[1])\n" + _gw_tail) is not None)
    case("gateway 清单函数里出现 lambda → 不可判定",
         gw_inventory(GW_BOOTSTRAP + "A2_SCRIPTS = common.A2_SCRIPTS\n"
                      "\ndef manifest_inventory(root):\n"
                      "    _f = lambda name: name\n"
                      "    paths = [f'scripts/gateway-parity/{n}' for n in A2_SCRIPTS]\n"
                      "    return {name: name for name in paths}\n") is None)

    # ---------------------------------------------------------------- r28 负控
    # r27 的复审又放开 / 漏掉两条假 PASS 路径：被看护对象先在字面量容器里装一
    # 跳（`rows = [common]`、`_p = [common][0]`）再展开或就地改，以及用
    # `getattr(sys.modules, 'update')` 把就地改的方法名藏进字符串。另外三处是
    # 同一族的漏口（函数体内按裸名匹配接收者、消费者白名单名字被重绑、同名多
    # 定义的默认值被后一条覆盖），一条一个负控。
    case("gateway 字面量容器装一跳后按位置展开 → 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "def _swap(x):\n    x.update({'A2_SCRIPTS': ()})\n"
                        "rows = [common]\n_swap(*rows)\n" + _gw_tail) is None)
    case("gateway 字面量容器装一跳后按属性调用展开 → 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "class _C:\n    def _swap(self, x):\n"
                        "        x.update({'A2_SCRIPTS': ()})\n"
                        "rows = [common]\n_C()._swap(*rows)\n" + _gw_tail) is None)
    case("gateway dict 字面量装一跳后按 ** 展开 → 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "def _swap(x):\n    x.update({'A2_SCRIPTS': ()})\n"
                        "kw = {'x': common}\n_swap(**kw)\n" + _gw_tail) is None)
    case("gateway 字面量容器装 sys.modules 后展开 → 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "def _swap(x):\n    x.update({'apparatus': None})\n"
                        "rows = [sys.modules]\n_swap(*rows)\n" + _gw_tail) is None)
    case("gateway 从字面量容器取元素后就地改属性 → 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "_p = [common][0]\n_p.A2_SCRIPTS = ()\n" + _gw_tail) is None)
    case("gateway 从字面量容器取元素后就地改表 → 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "_m = [sys.modules][0]\n_m.update({'apparatus': None})\n"
                      + _gw_tail) is None)
    case("gateway getattr 取被看护表的方法名 → 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "_g = getattr(sys.modules, 'update')\n"
                        "_g({'apparatus': None})\n" + _gw_tail) is None)
    case("gateway getattr 取被看护对象的就地改方法 → 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "_g = getattr(common, 'update')\n_g({'A2_SCRIPTS': ()})\n"
                      + _gw_tail) is None)
    case("gateway getattr 的实参按 * 展开后取方法 → 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "class _C:\n    def _swap(self, x):\n"
                        "        x.update({'A2_SCRIPTS': ()})\n"
                        "_pair = (_C(), '_swap')\n_g = getattr(*_pair)\n_g(common)\n"
                      + _gw_tail) is None)
    case("gateway getattr 的属性名藏在变量里 → 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "class _C:\n    def _swap(self, x):\n"
                        "        x.update({'A2_SCRIPTS': ()})\n"
                        "_n = '_swap'\n_g = getattr(_C(), _n)\n_g(common)\n"
                      + _gw_tail) is None)
    case("gateway 被调函数内按字面量容器取接收者后就地改 → 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "def _swap(x):\n    [x][0].update({'A2_SCRIPTS': ()})\n"
                        "_swap(common)\n" + _gw_tail) is None)
    case("gateway 被调函数内按字面量容器取接收者后写属性 → 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "def _swap(x):\n    [x][0].A2_SCRIPTS = ()\n"
                        "_swap(common)\n" + _gw_tail) is None)
    case("gateway 被调函数内 del 形参下标（就地改）→ 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "def _swap(x):\n    del x['apparatus']\n"
                        "_swap(sys.modules)\n" + _gw_tail) is None)
    # 上面两条 `[x][0]` 只用到「字面量容器 + 形参」的直接拼写，拦它们的是
    # 接收者的包含匹配；下面两条才单独钉住 `_local_aliases` 的容器一跳（形参
    # 先绑到名字、再按位置取），去掉那条规则这两条会变成可判定。
    case("gateway 形参先装进字面量容器、按位置取就地改 → 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "def _swap(x):\n    rows = [x]\n"
                        "    rows[0].update({'A2_SCRIPTS': ()})\n"
                        "_swap(common)\n" + _gw_tail) is None)
    case("gateway 形参先装进字面量容器、取元素绑名字就地改 → 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "def _swap(x):\n    rows = [x]\n    _t = rows[0]\n"
                        "    _t.update({'A2_SCRIPTS': ()})\n"
                        "_swap(common)\n" + _gw_tail) is None)
    case("gateway 形参只装进容器不写 → 仍可判定（不过度拦截）",
         gw_inventory(GW_BOOTSTRAP
                      + "def _swap(x):\n    rows = [x]\n    return len(rows)\n"
                        "_swap(common)\n" + _gw_tail) is not None)
    # `sys` 在 bootstrap 块里绑过，`_alias_roots` 的容器一跳已经替 `_sys_aliases`
    # 挡了一层；只有「表从 from-import 来、名字不在被看护集合里」时，才轮到
    # `_container_sys_kinds` 单独兜底，所以这两条用 `from sys import modules`。
    case("gateway from-import 的 sys 表装进字面量容器后按位置就地改 → 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "from sys import modules as _m\nrows = [_m]\n"
                        "rows[0].update({'apparatus': None})\n" + _gw_tail) is None)
    case("gateway from-import 的 sys 表装进字面量容器后取元素就地改 → 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "from sys import modules as _m\n_m2 = [_m][0]\n"
                        "_m2.update({'apparatus': None})\n" + _gw_tail) is None)
    case("gateway 消费者名字被文件重绑后不再可信 → 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "def _apply(f):\n    return f(common)\n"
                        "sorted = _apply\n"
                        "sorted(lambda o: o.update({'A2_SCRIPTS': ()}))\n"
                      + _gw_tail) is None)
    case("gateway 同名函数两条定义、默认值前一条带被看护对象 → 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "def _swap(x=common):\n    x.update({'A2_SCRIPTS': ()})\n"
                        "_swap()\n"
                        "def _swap(x=0):\n    x.update({'A2_SCRIPTS': ()})\n"
                      + _gw_tail) is None)
    case("gateway 无主 lambda 嵌在另一个 lambda 里（外层是消费者）→ 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "def _apply(f):\n    return f(common)\n"
                        "sorted(rows, key=lambda o: _apply("
                        "lambda p: p.update({'A2_SCRIPTS': ()})))\n"
                      + _gw_tail) is None)
    case("gateway sys 模块名装进容器后取其 modules 表 → 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "import sys as _s\n"
                        "[_s][0].modules.update({'apparatus': None})\n"
                      + _gw_tail) is None)
    case("gateway 从容器取出的干净元素 → 仍可判定（不过度拦截）",
         gw_inventory(GW_BOOTSTRAP
                      + "_p = [{'k': 0}][0]\n_p.update({'k': 1})\n"
                      + _gw_tail) is not None)
    # r29：容器一跳在「解包目标」「容器深度」「容器上的取元素调用」以及
    # 「消费者名重绑」四处补齐。每条都对应 r29 的一条评审发现，去掉对应规则
    # 这一条会变成可判定。
    case("gateway 解包目标拿到字面量容器里的被看护对象 → 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "a, b = [common], 0\na[0].A2_SCRIPTS = ()\n"
                      + _gw_tail) is None)
    case("gateway 解包目标拿到字面量容器里的 sys 表 → 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "from sys import modules as _m\n_a, _b = [_m], 0\n"
                        "_a[0].update({'apparatus': None})\n" + _gw_tail) is None)
    case("gateway 解包目标拿到字面量容器里的 sys 模块 → 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "import sys\n_a, _b = [sys], 0\n"
                        "_a[0].modules['apparatus'] = None\n" + _gw_tail) is None)
    case("gateway 同一容器里既有 sys 模块又有 sys 表 → 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "import sys\nboth = [sys, sys.path]\n"
                        "both[1].append('/tmp/evil')\n" + _gw_tail) is None)
    case("gateway sys 表经 * 展开装进容器后就地改 → 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "from sys import modules as _m\nrows = [*[_m]]\n"
                        "rows[0].update({'apparatus': None})\n" + _gw_tail) is None)
    case("gateway sys 表经 ** 合并装进容器后就地改 → 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "from sys import modules as _m\nrows = {**{'x': _m}}\n"
                        "rows['x'].update({'apparatus': None})\n" + _gw_tail) is None)
    case("gateway sys 表套两层容器后就地改 → 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "from sys import modules as _m\nrows = [[_m]]\n"
                        "rows[0][0].update({'apparatus': None})\n" + _gw_tail) is None)
    case("gateway 推导式元素是被看护对象 → 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "rows = [common for _ in (0,)]\n"
                        "rows[0].A2_SCRIPTS = ()\n" + _gw_tail) is None)
    case("gateway 字面量容器相加后取元素 → 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "rows = [common] + [None]\nrows[0].A2_SCRIPTS = ()\n"
                      + _gw_tail) is None)
    case("gateway 容器构造调用返回的容器 → 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "rows = list([common])\nrows[0].A2_SCRIPTS = ()\n"
                      + _gw_tail) is None)
    case("gateway 字面量容器上的取元素调用 → 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "_p = {'k': common}.get('k')\n_p.A2_SCRIPTS = ()\n"
                      + _gw_tail) is None)
    # 正控：推导式只在 iterable 里读到被看护名时不该被跟随，容器构造/取元素
    # 调用只碰干净数据时也不该被跟随——过拦会把合法交付挡成不可判定。
    case("gateway 推导式只在 iterable 里读被看护名 → 仍可判定（不过度拦截）",
         gw_inventory(GW_BOOTSTRAP
                      + "names = [f'{n}' for n in A2_SCRIPTS]\nprint(names)\n"
                      + _gw_tail) is not None)
    case("gateway 取元素调用只碰干净数据 → 仍可判定（不过度拦截）",
         gw_inventory(GW_BOOTSTRAP
                      + "_p = {'k': 0}.get('k')\nprint(_p)\n" + _gw_tail) is not None)
    case("gateway 解包目标只有干净字面量 → 仍可判定（不过度拦截）",
         gw_inventory(GW_BOOTSTRAP
                      + "a, b = [1], 0\nprint(a, b)\n" + _gw_tail) is not None)
    case("gateway 消费者名被 match 捕获重绑 → 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "def _apply(f):\n    return f(common)\n"
                        "match _apply:\n    case _ as sorted:\n        pass\n"
                        "sorted(lambda o: o.update({'A2_SCRIPTS': ()}))\n"
                      + _gw_tail) is None)
    case("gateway 消费者名被函数形参遮蔽 → 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "def _apply(f):\n    return f(common)\n"
                        "def _outer(sorted):\n"
                        "    return sorted(lambda o: o.update({'A2_SCRIPTS': ()}))\n"
                        "_outer(_apply)\n" + _gw_tail) is None)
    case("gateway 消费者名经 builtins 属性写改绑 → 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "import builtins\ndef _apply(f):\n    return f(common)\n"
                        "builtins.sorted = _apply\n"
                        "sorted(lambda o: o.update({'A2_SCRIPTS': ()}))\n"
                      + _gw_tail) is None)
    case("gateway 消费者名经 __builtins__ 下标写改绑 → 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "def _apply(f):\n    return f(common)\n"
                        "__builtins__['sorted'] = _apply\n"
                        "sorted(lambda o: o.update({'A2_SCRIPTS': ()}))\n"
                      + _gw_tail) is None)
    # 取不到的反射名只在「后来真被喂了被看护对象」时算写：干净交付里
    # `getattr(list, _n)` 这种惯用法不该把整单判成不可判定。
    case("gateway 反射绑定没被喂被看护对象 → 仍可判定（不过度拦截）",
         gw_inventory(GW_BOOTSTRAP
                      + "_n = 'append'\n_f = getattr(list, _n)\nprint(_f)\n"
                      + _gw_tail) is not None)
    case("gateway 反射绑定后来喂了被看护对象 → 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "_n = '_swap'\n_g = getattr(_C(), _n)\n_g(common)\n"
                        "class _C:\n    def _swap(self, x):\n"
                        "        x.update({'A2_SCRIPTS': ()})\n"
                      + _gw_tail) is None)
    # r30：容器表达式的「另一半读不出来」「已跟踪容器再派生」「builtins 换名
    # 写」「for / 推导式目标」四种拼法补齐。每条都对应 r29 复审的一条发现。
    case("gateway 容器相加只有一半写得出来 → 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "rows = [common] + _x\nrows[0].A2_SCRIPTS = ()\n"
                      + _gw_tail) is None)
    case("gateway sys 表容器相加只有一半写得出来 → 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "from sys import modules as _m\n_a, _b = [_m] + _x, 0\n"
                        "_a[0].update({'apparatus': None})\n" + _gw_tail) is None)
    case("gateway 字典按 | 合并后被看护对象仍在 → 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "d = {'x': common} | _d\nd['x'].A2_SCRIPTS = ()\n"
                      + _gw_tail) is None)
    case("gateway 已跟踪容器与字面量相加 → 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "x = [common]\ny = x + [None]\ny[0].A2_SCRIPTS = ()\n"
                      + _gw_tail) is None)
    case("gateway 已跟踪容器的 copy() → 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "x = [common]\ny = x.copy()\ny[0].A2_SCRIPTS = ()\n"
                      + _gw_tail) is None)
    case("gateway 已跟踪容器交给 list() → 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "x = [common]\ny = list(x)\ny[0].A2_SCRIPTS = ()\n"
                      + _gw_tail) is None)
    case("gateway 对已跟踪容器做逐元素推导 → 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "x = [common]\ny = [e for e in x]\ny[0].A2_SCRIPTS = ()\n"
                      + _gw_tail) is None)
    case("gateway builtins 换名后改绑消费者 → 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "import builtins\n_b = builtins\n"
                        "def _apply(f):\n    return f(common)\n"
                        "_b.sorted = _apply\n"
                        "sorted(lambda o: o.update({'A2_SCRIPTS': ()}))\n"
                      + _gw_tail) is None)
    case("gateway 经 builtins.__dict__ 改绑消费者 → 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "import builtins\ndef _apply(f):\n    return f(common)\n"
                        "builtins.__dict__['sorted'] = _apply\n"
                        "sorted(lambda o: o.update({'A2_SCRIPTS': ()}))\n"
                      + _gw_tail) is None)
    case("gateway 经 __builtins__.update 改绑消费者 → 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "def _apply(f):\n    return f(common)\n"
                        "__builtins__.update({'sorted': _apply})\n"
                        "sorted(lambda o: o.update({'A2_SCRIPTS': ()}))\n"
                      + _gw_tail) is None)
    case("gateway for 目标重绑消费者 → 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "def _apply(f):\n    return f(common)\n"
                        "for sorted in _fns:\n"
                        "    sorted(lambda o: o.update({'A2_SCRIPTS': ()}))\n"
                      + _gw_tail) is None)
    case("gateway 推导式目标重绑消费者 → 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "def _apply(f):\n    return f(common)\n"
                        "_r = [sorted(lambda o: o.update({'A2_SCRIPTS': ()})) "
                        "for sorted in _fns]\nprint(_r)\n"
                      + _gw_tail) is None)
    # 正控：干净容器上的同名拼法不该被跟随。
    case("gateway 干净容器 copy/list/推导 → 仍可判定（不过度拦截）",
         gw_inventory(GW_BOOTSTRAP
                      + "x = [0]\n_a = x.copy()\n_b = list(x)\n"
                        "_c = [e for e in x]\nprint(_a, _b, _c)\n"
                      + _gw_tail) is not None)
    # r31：容器重复、把方法读成值、反射绑定再换名、模块名被容器换成表。
    case("gateway 容器重复后被看护对象仍在 → 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "rows = [common] * 3\nrows[0].A2_SCRIPTS = ()\n"
                      + _gw_tail) is None)
    case("gateway sys 表容器重复 → 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "from sys import modules as _m\nrows = [_m] * 2\n"
                        "rows[0].update({'apparatus': None})\n" + _gw_tail) is None)
    case("gateway 把 dict.update 读成值再喂被看护对象 → 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "class _C:\n    pass\n_g = dict.update\n"
                        "_g(common, {'A2_SCRIPTS': ()})\n" + _gw_tail) is None)
    case("gateway 反射取 dict.update 再喂被看护对象 → 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "class _C:\n    pass\n"
                        "_g = getattr(dict, 'update')\n"
                        "_g(common, {'A2_SCRIPTS': ()})\n" + _gw_tail) is None)
    case("gateway 反射绑定换名后仍喂被看护对象 → 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "class _C:\n    def _swap(self, x):\n"
                        "        x.update({'A2_SCRIPTS': ()})\n"
                        "_n = '_swap'\n_g = getattr(_C(), _n)\n_h = _g\n"
                        "_h(common)\n" + _gw_tail) is None)
    case("gateway 模块名被容器换成表后写表 → 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "import sys\nboth = sys\nboth = [sys.path]\n"
                        "both[0].append('/tmp/evil')\n" + _gw_tail) is None)
    # 正控：同样的拼法落在自己的数据上不该被跟随。
    case("gateway 自己的容器重复 / 自己的 dict.update → 仍可判定（不过度拦截）",
         gw_inventory(GW_BOOTSTRAP
                      + "class _C:\n    pass\n_d = {'k': 1}\n"
                        "_g = dict.update\n_g(_d, {'k': 2})\n"
                        "rows = [1] * 3\nprint(_d, rows)\n"
                      + _gw_tail) is not None)

    # r32：模块名再绑成表、星号解包、builtins 也走容器、把可调用对象存在属性
    # 上、for 目标取自被跟踪容器、集合运算、dict 视图、enumerate、把换名后的
    # 反射结果当参数交给消费者。每条都对应 r31 复审的一条发现，且都在探针
    # （/tmp/t1527-probe-r32.py）上先复现成 PASSES 再改。
    case("gateway 模块名随后绑成表（属性）→ 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "import sys\nboth = sys\nboth = sys.path\n"
                        "both.append('/tmp/evil')\n" + _gw_tail) is None)
    case("gateway 模块名随后绑成表（容器）→ 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "import sys\n_t = [sys.path]\nboth = sys\nboth = _t\n"
                        "both[0].append('/tmp/evil')\n" + _gw_tail) is None)
    case("gateway sys 表经星号解包换名 → 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "import sys\n_xs = (sys, sys.path)\n"
                        "both, _p = (*_xs,)\n_p.append('/tmp/evil')\n"
                      + _gw_tail) is None)
    case("gateway 模块名先绑表再被容器接走 → 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "import sys\n_p = sys.path\n_c = [_p]\n_x = _c\n"
                        "both = sys\nboth = _p\nboth = _x.copy()\n"
                        "both[0].append('/tmp/evil')\n" + _gw_tail) is None)
    case("gateway builtins 从容器里取出后改绑消费者 → 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "import builtins\ndef _apply(f):\n    return f(common)\n"
                        "_b = [builtins][0]\n_b.sorted = _apply\n"
                        "sorted(lambda o: o.update({'A2_SCRIPTS': ()}))\n"
                      + _gw_tail) is None)
    case("gateway builtins 经解包取出后改绑消费者 → 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "import builtins\ndef _apply(f):\n    return f(common)\n"
                        "_b, = (builtins,)\n_b.sorted = _apply\n"
                        "sorted(lambda o: o.update({'A2_SCRIPTS': ()}))\n"
                      + _gw_tail) is None)
    case("gateway 把 dict.update 存在属性上再喂被看护对象 → 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "class _C:\n    pass\n_o = _C()\n"
                        "_o.g = dict.update\n"
                        "_o.g(common, {'A2_SCRIPTS': ()})\n" + _gw_tail) is None)
    case("gateway for 目标取自被跟踪容器 → 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "x = [common]\nfor e in x:\n    e.A2_SCRIPTS = ()\n"
                      + _gw_tail) is None)
    case("gateway for 目标取自 sys 表容器 → 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "import sys\n_ml = [sys.path]\nfor t in _ml:\n"
                        "    t.append('/tmp/evil')\n" + _gw_tail) is None)
    case("gateway 集合差集后仍拿到被看护对象 → 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "s = {common} - _x\nnext(iter(s)).A2_SCRIPTS = ()\n"
                      + _gw_tail) is None)
    case("gateway 集合交集后仍拿到被看护对象 → 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "s = {common} & _x\nnext(iter(s)).A2_SCRIPTS = ()\n"
                      + _gw_tail) is None)
    case("gateway 集合对称差后仍拿到被看护对象 → 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "s = {common} ^ _x\nnext(iter(s)).A2_SCRIPTS = ()\n"
                      + _gw_tail) is None)
    case("gateway dict 视图再取出被看护对象 → 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "d = {'k': common}\nvs = d.values()\n"
                        "list(vs)[0].A2_SCRIPTS = ()\n" + _gw_tail) is None)
    case("gateway enumerate 后逐元素取出被看护对象 → 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "x = [common]\ny = enumerate(x)\n"
                        "_a, e = list(y)[0]\ne.A2_SCRIPTS = ()\n"
                      + _gw_tail) is None)
    case("gateway 换名后的反射结果当参数交给消费者 → 不可判定",
         gw_inventory(GW_BOOTSTRAP
                      + "_g = dict.update\n"
                        "map(_g, (common,), ({'A2_SCRIPTS': ()},))\n"
                      + _gw_tail) is None)
    # 正控：同样的拼法落在自己的数据上不该被跟随。
    case("gateway 自己的列表乘零 → 仍可判定（不过度拦截）",
         gw_inventory(GW_BOOTSTRAP + "rows = [1] * 0\nprint(rows)\n"
                      + _gw_tail) is not None)
    case("gateway 推导式元素与目标无关 → 仍可判定（不过度拦截）",
         gw_inventory(GW_BOOTSTRAP
                      + "x = [1]\nq = 2\ny = [q for e in x]\nprint(y)\n"
                      + _gw_tail) is not None)
    case("gateway for 遍历自己的列表 → 仍可判定（不过度拦截）",
         gw_inventory(GW_BOOTSTRAP
                      + "x = [1, 2]\nfor e in x:\n    print(e)\n"
                      + _gw_tail) is not None)

    # 白名单每个成员都要有配对的负控（名单漂移即红）与正控（干净数据不误拦）。
    for _consumer in sorted(cfd._LAMBDA_CONSUMERS):
        case(f"gateway 无主 lambda 交给 {_consumer} 且同时喂被看护对象 → 不可判定",
             gw_inventory(GW_BOOTSTRAP
                          + f"{_consumer}(common, lambda o: o.setdefault('k', 1))\n"
                          + _gw_tail) is None)
        case(f"gateway 无主 lambda 交给 {_consumer} 只碰自己的元素 → 仍可判定",
             gw_inventory(GW_BOOTSTRAP
                          + f"{_consumer}([{{'k': 0}}], "
                            "lambda o: o.setdefault('k', 1))\n"
                          + _gw_tail) is not None)

    with tempfile.TemporaryDirectory() as td:
        root = Path(td)
        init_repo(root)
        write(root, "scripts/parity/check_frozen.py", PARITY_MANIFEST)
        commit(root, "base")
        head = git(root, "rev-parse", "HEAD")
        case("parity 清单函数（无嵌套 def）→ spec 名单可判定",
             cfd.parity_spec_names(root, head) is not None)
        write(root, "scripts/parity/check_frozen.py",
              PARITY_MANIFEST.replace(
                  "    names = (",
                  "    _f = lambda x: x\n    names = ("))
        commit(root, "nested")
        case("parity 清单函数里嵌套 lambda → spec 名单不可判定",
             cfd.parity_spec_names(root, git(root, "rev-parse", "HEAD")) is None)

    with tempfile.TemporaryDirectory() as td:
        # 未跟踪草稿：真实闸门无感，--write 也不登记 → 预检不能误拦。
        payload = dict(parity_payload, unregistered_skipped=["scripts/parity/scratch.txt"])
        proc = delivery(Path(td), "parity", payload, dirty=[("scripts/parity/scratch.txt", "x\n")])
        case("未跟踪草稿不计首登记（不误拦）",
             proc.returncode == 0 and "BLOCKED 0 / READY 1" in proc.stdout,
             f"rc={proc.returncode}")

    with tempfile.TemporaryDirectory() as td:
        # 装置源名单新增一个 spec 文档：两套装置都不会自己报「缺行」（gateway 只
        # 说 hash list differs），首登记扫描必须覆盖 specs/<kind>/ 才拦得住。
        payload = {"status": "FAIL", "failures": [HASH_LIST]}
        proc = delivery(Path(td), "gateway-parity", payload,
                        source=gw_source().replace("'accepted-diffs.md')",
                                                   "'accepted-diffs.md', 'extra.md')"),
                        files=[("specs/gateway-parity/extra.md", "EXTRA\n")],
                        base_files=[("scripts/gateway-parity/common.py", GW_COMMON)])
        case("gateway 清单新增 spec 文档、无事件链 → BLOCKED（首登记不漏判）",
             proc.returncode == 1 and "BLOCKED 1 / READY 0" in proc.stdout,
             f"rc={proc.returncode} out={proc.stdout}")

    with tempfile.TemporaryDirectory() as td:
        payload = {"status": "FAIL", "failures": [HASH_LIST]}
        proc = delivery(Path(td), "gateway-parity", payload,
                        source=gw_source().replace("'accepted-diffs.md')",
                                                   "'accepted-diffs.md', 'extra.md')"),
                        files=[("specs/gateway-parity/extra.md", "EXTRA\n")],
                        events=[("specs/gateway-parity/frozen/20260101-x.md",
                                 f"Change: - {sha('EXTRA\n')} specs/gateway-parity/extra.md\n"
                                 + EVENT_FIELDS)],
                        base_files=[("scripts/gateway-parity/common.py", GW_COMMON)])
        case("gateway 清单新增 spec 文档、有 Change: - <hash> → READY",
             proc.returncode == 0 and "BLOCKED 0 / READY 1" in proc.stdout,
             f"rc={proc.returncode} out={proc.stdout}")

    with tempfile.TemporaryDirectory() as td:
        # parity 同向：源里新增 sixth.md 且文件是新加的，没有链就必须拦。
        proc = delivery(Path(td), "parity", dict(parity_payload),
                        source=PARITY_MANIFEST.replace("'multimodal.md')",
                                                       "'multimodal.md', 'sixth.md')"),
                        files=[("specs/parity/sixth.md", "SIXTH\n")])
        case("parity 清单新增 spec 文档、无事件链 → BLOCKED",
             proc.returncode == 1 and "BLOCKED 1 / READY 0" in proc.stdout,
             f"rc={proc.returncode} out={proc.stdout}")

    with tempfile.TemporaryDirectory() as td:
        proc = delivery(Path(td), "parity", dict(parity_payload),
                        source=PARITY_MANIFEST.replace("'multimodal.md')",
                                                       "'multimodal.md', 'sixth.md')"),
                        files=[("specs/parity/sixth.md", "SIXTH\n")],
                        events=[("specs/parity/frozen/20260101-y.md",
                                 f"Change: - {sha('SIXTH\n')} specs/parity/sixth.md\n"
                                 + EVENT_FIELDS)])
        case("parity 清单新增 spec 文档、有 Change: - <hash> → READY（端到端正控）",
             proc.returncode == 0 and "BLOCKED 0 / READY 1" in proc.stdout,
             f"rc={proc.returncode} out={proc.stdout}")

    with tempfile.TemporaryDirectory() as td:
        # 非 ASCII 路径：ls-tree 不带 -z 时 core.quotePath 会把它转义成 \344\270\255，
        # 按行比对就永远查不到 → registrable 误判 False（首登记漏判）。
        root = Path(td)
        init_repo(root)
        write(root, "scripts/parity/check_frozen.py", PARITY_MANIFEST)
        write(root, "scripts/parity/中文.py", "x = 1\n")
        commit(root, "base")
        case("非 ASCII 路径按字面匹配（ls-tree 必须 -z）",
             cfd.registrable(root, "parity", "scripts/parity/中文.py") is True)

    with tempfile.TemporaryDirectory() as td:
        proc = delivery(Path(td), "gateway-parity",
                        {"status": "FAIL", "failures": [HASH_LIST]},
                        files=[("scripts/gateway-parity/fixtures/new.json", "{}\n")],
                        source=gw_source("fixtures/new.json"),
                        base_files=[("scripts/gateway-parity/common.py", GW_COMMON)])
        case("gateway 清单成员无事件链 → BLOCKED",
             proc.returncode == 1 and "BLOCKED 1 / READY 0" in proc.stdout,
             f"rc={proc.returncode}")

    with tempfile.TemporaryDirectory() as td:
        # 装置源码没提到的文件不在清单里：--write 不登记，不能误拦。
        proc = delivery(Path(td), "gateway-parity",
                        {"status": "FAIL", "failures": [HASH_LIST]},
                        base_files=[("scripts/gateway-parity/common.py", GW_COMMON)],
                        source=gw_source("fixtures/other.json"),
                        files=[("scripts/a5-19/extra.mjs", "x\n")])
        case("gateway 清单外的 a5-19 新文件不误拦",
             proc.returncode == 0 and "BLOCKED 0 / READY 1" in proc.stdout,
             f"rc={proc.returncode}")

    with tempfile.TemporaryDirectory() as td:
        # 改名进装置目录：git 默认 rename detection 记成 R，--diff-filter=A 过滤掉，
        # 但 --write 会登记改名目的地 → 旧实现假 READY（第四轮 P0）。
        proc = delivery(Path(td), "gateway-parity",
                        {"status": "FAIL", "failures": [HASH_LIST]},
                        base_files=[("scripts/gateway-parity/fixtures/old.json", "{}\n"),
                                    ("scripts/gateway-parity/common.py", GW_COMMON)],
                        moves=[("scripts/gateway-parity/fixtures/old.json",
                                "scripts/gateway-parity/fixtures/new.json")],
                        source=gw_source("fixtures/new.json"))
        case("gateway 同前缀改名的目的地无事件链 → BLOCKED（旧实现假 READY）",
             proc.returncode == 1 and "BLOCKED 1 / READY 0" in proc.stdout,
             f"rc={proc.returncode} out={proc.stdout}")

    with tempfile.TemporaryDirectory() as td:
        proc = delivery(Path(td), "gateway-parity",
                        {"status": "FAIL", "failures": [HASH_LIST]},
                        base_files=[("scripts/gateway-parity/fixtures/old.json", "{}\n"),
                                    ("scripts/gateway-parity/common.py", GW_COMMON)],
                        moves=[("scripts/gateway-parity/fixtures/old.json",
                                "scripts/gateway-parity/fixtures/new.json")],
                        events=[("specs/gateway-parity/frozen/20260101-r.md",
                                 f"Change: - {sha('{}\n')} "
                                 "scripts/gateway-parity/fixtures/new.json\n"
                                 + EVENT_FIELDS)],
                        source=gw_source("fixtures/new.json"))
        case("gateway 改名目的地有 Change: - 事件链 → READY",
             proc.returncode == 0 and "BLOCKED 0 / READY 1" in proc.stdout,
             f"rc={proc.returncode} out={proc.stdout}")

    # --------------------------------- 既有文件被本分支加进清单＝首登记
    gw_payload = {"status": "FAIL", "failures": [HASH_LIST]}
    new_source = gw_source("fixtures/x.json")
    named = "import json\nprint(json.dumps(" + repr(gw_payload) + "))\n" + new_source

    with tempfile.TemporaryDirectory() as td:
        # x.json 在 merge-base 就存在、但当时没有清单行；本分支改 check_frozen.py
        # 把它加进清单 → --write 会加行，链必须从 '-' 起。旧实现只扫 diff 的 A，
        # 该文件不是 A → 只剩「hash list differs」→ 假 READY。
        proc = delivery(Path(td), "gateway-parity", gw_payload,
                        base_files=[("scripts/gateway-parity/fixtures/x.json", "{}\n"),
                                    ("scripts/gateway-parity/common.py", GW_COMMON)],
                        files=[("scripts/gateway-parity/check_frozen.py", named)])
        case("既有文件被加进清单、无事件链 → BLOCKED（旧实现假 READY）",
             proc.returncode == 1 and "BLOCKED 1 / READY 0" in proc.stdout,
             f"rc={proc.returncode} out={proc.stdout}")

    with tempfile.TemporaryDirectory() as td:
        # 正控：按真实闸门的语义写 Change: - <sha>（起点是清单行缺失，不是旧 blob）。
        proc = delivery(Path(td), "gateway-parity", gw_payload,
                        base_files=[("scripts/gateway-parity/fixtures/x.json", "{}\n"),
                                    ("scripts/gateway-parity/common.py", GW_COMMON)],
                        files=[("scripts/gateway-parity/check_frozen.py", named)],
                        events=[("specs/gateway-parity/frozen/20260101-s.md",
                                 f"Change: - {sha('{}\n')} "
                                 "scripts/gateway-parity/fixtures/x.json\n"
                                 + EVENT_FIELDS)])
        case("既有文件加进清单、Change: - 链 → READY（旧实现假 BLOCKED）",
             proc.returncode == 0 and "BLOCKED 0 / READY 1" in proc.stdout,
             f"rc={proc.returncode} out={proc.stdout}")

    with tempfile.TemporaryDirectory() as td:
        # 工作树的未提交改动 → 阻塞：子装置哈希的是工作树字节，HEAD 可以被遮住。
        proc = delivery(Path(td), "parity", parity_payload,
                        files=[("scripts/parity/new.py", "NEW\n")],
                        events=[("specs/parity/frozen/20260101-d.md",
                                 f"Change: - {sha('NEW\n')} scripts/parity/new.py\n"
                                 + EVENT_FIELDS)],
                        dirty=[("scripts/parity/new.py", "TAMPERED\n")])
        case("工作树有未提交的已跟踪改动 → BLOCKED",
             proc.returncode == 1 and "=== worktree === BLOCKED" in proc.stdout,
             f"rc={proc.returncode} out={proc.stdout}")

    # ------------------------------------------- gateway 可登记集取自装置源码
    with tempfile.TemporaryDirectory() as td:
        root = Path(td)
        init_repo(root)
        write(root, "scripts/gateway-parity/common.py",
              "A2_SCRIPTS = ('alpha.py', 'beta.py')\n"
              "A3_SCRIPTS = ('check_schema.py',)\n"
              "DATA_FILES = ('README.md', 'package.json')\n")
        write(root, "scripts/gateway-parity/check_frozen.py",
              GW_BOOTSTRAP + "A2_SCRIPTS = common.A2_SCRIPTS\n"
              "\ndef manifest_inventory(root):\n"
              "    paths = [f'scripts/gateway-parity/{name}' for name in A2_SCRIPTS]\n"
              "    paths += ['scripts/gateway-parity/fixtures/one.json']\n"
              "    return paths\n\n"
              "def something_else():\n    return 1\n")
        commit(root, "base")
        case("gateway A2 成员算首登记",
             cfd.registrable(root, "gateway-parity",
                             "scripts/gateway-parity/alpha.py") is True)
        case("gateway 清单内 fixtures 成员算首登记",
             cfd.registrable(root, "gateway-parity",
                             "scripts/gateway-parity/fixtures/one.json") is True)
        case("gateway DATA_FILES / A3 不算首登记（--write 不加行）",
             cfd.registrable(root, "gateway-parity",
                             "scripts/gateway-parity/README.md") is False
             and cfd.registrable(root, "gateway-parity",
                                 "scripts/gateway-parity/check_schema.py") is False)

    # ------------------------------------------- a5-19 模板臂 / 删除链 / 运行中脏树
    with tempfile.TemporaryDirectory() as td:
        root = Path(td)
        init_repo(root)
        write(root, "scripts/gateway-parity/common.py",
              "A2_SCRIPTS = ('alpha.py',)\n")
        write(root, "scripts/gateway-parity/check_frozen.py",
              GW_BOOTSTRAP + "A2_SCRIPTS = common.A2_SCRIPTS\n"
              "\ndef manifest_inventory(root):\n"
              "    paths = [f'scripts/gateway-parity/{name}' for name in A2_SCRIPTS]\n"
              "    paths += [f'scripts/a5-19/{name}' for name in ('run.py', 'six_cells.mts')]\n"
              "    return paths\n\n"
              "def something_else():\n    return 1\n")
        commit(root, "base")
        case("a5-19 模板臂（f'.../{name}'）算首登记（第六轮 P0）",
             cfd.registrable(root, "gateway-parity", "scripts/a5-19/run.py") is True)

    with tempfile.TemporaryDirectory() as td:
        # 分支把清单成员删掉：--write 删行，链收在 '-'（旧实现假 BLOCKED）。
        payload = dict(parity_payload,
                       errors=[HASH_LIST,
                               "scripts/parity/gone.py: " + LACKS_CHAIN_MSG])
        proc = delivery(Path(td), "parity", payload,
                        base_files=[("scripts/parity/gone.py", "OLD\n")],
                        removes=["scripts/parity/gone.py"])
        case("删除已登记成员、无删除链 → BLOCKED",
             proc.returncode == 1 and "BLOCKED 1 / READY 0" in proc.stdout,
             f"rc={proc.returncode} out={proc.stdout}")

    with tempfile.TemporaryDirectory() as td:
        payload = dict(parity_payload,
                       errors=[HASH_LIST,
                               "scripts/parity/gone.py: " + LACKS_CHAIN_MSG])
        body = "import json\nprint(json.dumps(" + repr(payload) + "))\n"
        proc = delivery(Path(td), "parity", payload,
                        base_files=[("scripts/parity/gone.py", "OLD\n"),
                                    ("specs/parity/FROZEN.md",
                                     "# fake FROZEN\n\n"
                                     f"{sha('OLD\n')}  ../../scripts/parity/gone.py\n"
                                     f"{sha(body)}  ../../scripts/parity/check_frozen.py\n")],
                        removes=["scripts/parity/gone.py"],
                        events=[("specs/parity/frozen/20260101-x.md",
                                 f"Change: {sha('OLD\n')} - scripts/parity/gone.py\n"
                                 + EVENT_FIELDS)])
        case("删除已登记成员、Change: <old> - 链 → READY（旧实现假 BLOCKED）",
             proc.returncode == 0 and "BLOCKED 0 / READY 1" in proc.stdout,
             f"rc={proc.returncode} out={proc.stdout}")

    with tempfile.TemporaryDirectory() as td:
        # 子装置改自己所在的树（相对路径 → 落到钉住快照里）：同一轮里兄弟装置的
        # 证据与目标字节就会跨两个树，必须阻塞。
        touch = ("open('scripts/gateway-parity/fixtures/touched.json', 'w').write('x')\n")
        gw = {"status": "PASS", "failures": []}
        body = "import json\nprint(json.dumps(" + repr(gw) + "))\n" + touch
        proc = delivery(Path(td), "gateway-parity", gw,
                        base_files=[("scripts/gateway-parity/fixtures/touched.json", "{}\n"),
                                    ("specs/gateway-parity/FROZEN.md",
                                     "# fake FROZEN\n\n"
                                     f"{sha('{}\n')}  ../../scripts/gateway-parity/fixtures/touched.json\n"
                                     f"{sha(body)}  ../../scripts/gateway-parity/check_frozen.py\n")],
                        source=touch)
        case("运行期间子装置改了自己所在的快照 → BLOCKED（快照不一致）",
             proc.returncode == 1 and "=== snapshot === BLOCKED" in proc.stdout,
             f"rc={proc.returncode} out={proc.stdout}")

    with tempfile.TemporaryDirectory() as td:
        # 快照隔离挡不住绝对路径回写被审树：`git status` 前后指纹必须报出差异，
        # 这是「运行预检不会改被审树任何文件」这条承诺的可验证证据（P1-E）。
        leak = (f"from pathlib import Path\n"
                f"(Path({str(Path(td))!r}) / "
                f"'scripts/gateway-parity/leak.json').write_text('x')\n")
        gw = {"status": "PASS", "failures": []}
        body = "import json\nprint(json.dumps(" + repr(gw) + "))\n" + leak
        proc = delivery(Path(td), "gateway-parity", gw,
                        base_files=[("specs/gateway-parity/FROZEN.md",
                                     "# fake FROZEN\n\n"
                                     f"{sha(body)}  ../../scripts/gateway-parity/check_frozen.py\n")],
                        source=leak)
        case("运行期间子装置回写被审树 → BLOCKED（status 指纹差异）",
             proc.returncode == 1
             and "=== worktree === BLOCKED" in proc.stdout
             and "the audited working tree changed during the run" in proc.stdout,
             f"rc={proc.returncode} out={proc.stdout}")

    with tempfile.TemporaryDirectory() as td:
        # 成员被移出清单、文件仍跟踪：--write 会删行、新目标取 '-'（与新清单的
        # new_hashes.get(name, '-') 一致），不是拿 HEAD 字节当目标。
        # 真实 pre-write 形态：清单行还在，装置只报 hash list differs；掉行要
        # 靠 dropped_rows 自己枚举（第七轮两家 P0/P1：手写 LACKS_CHAIN 报文是伪造对）。
        payload = {"status": "FAIL", "failures": [HASH_LIST]}
        common_src = "A2_SCRIPTS = (\n    'check_frozen.py', 'common.py',\n)\n"
        base_inventory = gw_source("fixtures/x.json")
        head_inventory = gw_source()
        base_body = "import json\nprint(json.dumps(" + repr(payload) + "))\n" + base_inventory
        head_body = "import json\nprint(json.dumps(" + repr(payload) + "))\n" + head_inventory
        frozen = ("# fake FROZEN\n\n"
                  f"{sha('{}\n')}  ../../scripts/gateway-parity/fixtures/x.json\n"
                  f"{sha(common_src)}  ../../scripts/gateway-parity/common.py\n"
                  f"{sha(base_body)}  ../../scripts/gateway-parity/check_frozen.py\n")
        proc = delivery(Path(td), "gateway-parity", payload, source=base_inventory,
                        base_files=[("scripts/gateway-parity/fixtures/x.json", "{}\n"),
                                    ("scripts/gateway-parity/common.py", common_src),
                                    ("specs/gateway-parity/FROZEN.md", frozen)],
                        files=[("scripts/gateway-parity/fixtures/x.json", '{"a": 1}\n'),
                               ("scripts/gateway-parity/check_frozen.py", head_body)],
                        events=[("specs/gateway-parity/frozen/20260101-t.md",
                                 f"Change: {sha('{}\n')} {sha('{\"a\": 1}\n')} "
                                 "scripts/gateway-parity/fixtures/x.json\n"
                                 + EVENT_FIELDS)])
        case("成员移出清单、旧→新链 → BLOCKED（应为 '-'）",
             proc.returncode == 1 and "BLOCKED 1 / READY 0" in proc.stdout,
             f"rc={proc.returncode} out={proc.stdout}")

    with tempfile.TemporaryDirectory() as td:
        # 真实 pre-write 形态：清单行还在，装置只报 hash list differs；掉行要
        # 靠 dropped_rows 自己枚举（第七轮两家 P0/P1：手写 LACKS_CHAIN 报文是伪造对）。
        payload = {"status": "FAIL", "failures": [HASH_LIST]}
        common_src = "A2_SCRIPTS = (\n    'check_frozen.py', 'common.py',\n)\n"
        base_inventory = gw_source("fixtures/x.json")
        head_inventory = gw_source()
        base_body = "import json\nprint(json.dumps(" + repr(payload) + "))\n" + base_inventory
        head_body = "import json\nprint(json.dumps(" + repr(payload) + "))\n" + head_inventory
        frozen = ("# fake FROZEN\n\n"
                  f"{sha('{}\n')}  ../../scripts/gateway-parity/fixtures/x.json\n"
                  f"{sha(common_src)}  ../../scripts/gateway-parity/common.py\n"
                  f"{sha(base_body)}  ../../scripts/gateway-parity/check_frozen.py\n")
        proc = delivery(Path(td), "gateway-parity", payload, source=base_inventory,
                        base_files=[("scripts/gateway-parity/fixtures/x.json", "{}\n"),
                                    ("scripts/gateway-parity/common.py", common_src),
                                    ("specs/gateway-parity/FROZEN.md", frozen)],
                        files=[("scripts/gateway-parity/fixtures/x.json", '{"a": 1}\n'),
                               ("scripts/gateway-parity/check_frozen.py", head_body)],
                        events=[("specs/gateway-parity/frozen/20260101-t.md",
                                 f"Change: {sha('{}\n')} - "
                                 "scripts/gateway-parity/fixtures/x.json\n"
                                 + EVENT_FIELDS)])
        case("成员移出清单、Change: <old> - 链 → READY",
             proc.returncode == 0 and "BLOCKED 0 / READY 1" in proc.stdout,
             f"rc={proc.returncode} out={proc.stdout}")

    # ui-server：SPEC_FILES 是从 apparatus.py 解析的字面量，成员被移出清单时
    # 终点同样取 '-'（第七轮两家 P1）。
    def ui_delivery(td, *, events):
        # 真实 pre-write 报文：成员移出 SPEC_FILES、行还在 → ui 装置报
        # `unexpected registered file: <绝对路径>`（check_frozen.py:276）。
        payload = {"verdict": "FAIL", "errors": [
            HASH_LIST,
            "unexpected registered file: "
            f"{Path(td).resolve()}/specs/ui-server-parity/multimodal.md"]}
        base_apparatus = UI_APPARATUS.replace(
            "'acceptance.md',)", "'acceptance.md', 'multimodal.md')")
        head_apparatus = UI_APPARATUS
        body = "import json\nprint(json.dumps(" + repr(payload) + "))\n"
        frozen = ("# fake FROZEN\n\n"
                  f"{sha('OLD\\n')}  ../../specs/ui-server-parity/multimodal.md\n"
                  f"{sha(base_apparatus)}  ../../scripts/ui-server-parity/apparatus.py\n"
                  f"{sha(body)}  ../../scripts/ui-server-parity/check_frozen.py\n")
        return delivery(Path(td), "ui-server-parity", payload,
                        source=UI_FROZEN,
                        base_files=[("specs/ui-server-parity/multimodal.md", "OLD\n"),
                                    ("scripts/ui-server-parity/apparatus.py", base_apparatus),
                                    ("specs/ui-server-parity/FROZEN.md", frozen)],
                        files=[("specs/ui-server-parity/multimodal.md", "NEW\n"),
                               ("scripts/ui-server-parity/apparatus.py", head_apparatus)],
                        events=events)

    with tempfile.TemporaryDirectory() as td:
        proc = ui_delivery(td, events=[("specs/ui-server-parity/frozen/20260101-u.md",
                                        f"Change: {sha('OLD\n')} - "
                                        "specs/ui-server-parity/multimodal.md\n"
                                        + EVENT_FIELDS)])
        case("ui-server 成员移出 SPEC_FILES、Change: <old> - 链 → READY",
             proc.returncode == 0 and "BLOCKED 0 / READY 1" in proc.stdout,
             f"rc={proc.returncode} out={proc.stdout}")

    with tempfile.TemporaryDirectory() as td:
        proc = ui_delivery(td, events=[("specs/ui-server-parity/frozen/20260101-u.md",
                                        f"Change: {sha('OLD\n')} {sha('NEW\n')} "
                                        "specs/ui-server-parity/multimodal.md\n"
                                        + EVENT_FIELDS)])
        case("ui-server 成员移出 SPEC_FILES、旧→新链 → BLOCKED",
             proc.returncode == 1 and "BLOCKED 1 / READY 0" in proc.stdout,
             f"rc={proc.returncode} out={proc.stdout}")

    with tempfile.TemporaryDirectory() as td:
        # 同名二次绑定：解释器生效的是末值，只认字面量首值会留下错误的 True。
        payload = {"status": "FAIL", "failures": [HASH_LIST]}
        twice = "A2_SCRIPTS = ('check_frozen.py',)\nA2_SCRIPTS += ('common.py',)\n"
        proc = delivery(Path(td), "gateway-parity", payload, source=gw_source(),
                        base_files=[("scripts/gateway-parity/common.py", twice)])
        case("清单源被二次赋值 → BLOCKED（不可判定，不取首个）",
             proc.returncode == 1 and "cannot tell" in proc.stdout,
             f"rc={proc.returncode} out={proc.stdout}")

    with tempfile.TemporaryDirectory() as td:
        # 同名函数定义两次：生效的是后一个，取第一个会模拟错清单。
        payload = {"status": "FAIL", "failures": [HASH_LIST]}
        twice = (gw_source()
                 + "\ndef manifest_inventory(root):\n"
                 "    return ['scripts/gateway-parity/other.py']\n")
        proc = delivery(Path(td), "gateway-parity", payload, source=twice,
                        base_files=[("scripts/gateway-parity/common.py", GW_COMMON)])
        case("manifest_inventory 定义两次 → BLOCKED（不可判定）",
             proc.returncode == 1 and "cannot tell" in proc.stdout,
             f"rc={proc.returncode} out={proc.stdout}")

    def gateway_tree(common_text):
        """建一棵 gateway 装置树（common.py 内容由调用方给），返回 (root, td)。"""
        td = tempfile.TemporaryDirectory()
        root = Path(td.name)
        init_repo(root)
        write(root, "scripts/gateway-parity/common.py", common_text)
        write(root, "scripts/gateway-parity/check_frozen.py", gw_source())
        commit(root, "base")
        return root, td

    # common.py 读不出时整棵装置都不可判定；注意 `A2_SCRIPTS = (` 是语法错，
    # `_parse` 就先短路了，所以另外补一条「能解析、但绑定不是字面元组」的夹具，
    # 才能把 scripts=None 真正送进 `_gateway_manifest`。
    root, td = gateway_tree("A2_SCRIPTS = (\n")
    try:
        case("gateway 的 common.py 语法错 → 不可判定（不崩、不猜）",
             cfd.gateway_inventory(root) is None
             and cfd.registrable(root, "gateway-parity",
                                 "scripts/gateway-parity/check_frozen.py") is None)
    finally:
        td.cleanup()

    root, td = gateway_tree("A2_SCRIPTS = tuple(EXTRA)\n")
    try:
        case("gateway 的 A2_SCRIPTS 不是字面元组 → 不可判定（None 不参与遍历）",
             cfd.gateway_inventory(root) is None
             and cfd.registrable(root, "gateway-parity",
                                 "scripts/gateway-parity/check_frozen.py") is None)
    finally:
        td.cleanup()

    root, td = gateway_tree("A2_SCRIPTS = ()\n")
    try:
        # 空元组是「已知的空清单」，不是「不知道」：不能像旧代码那样当假值退回。
        case("gateway 的 A2_SCRIPTS 为空元组 → 已知清单，脚本不可登记",
             cfd.gateway_inventory(root) is not None
             and cfd.registrable(root, "gateway-parity",
                                 "scripts/gateway-parity/check_frozen.py") is False)
    finally:
        td.cleanup()

    root, td = gateway_tree(GW_COMMON)
    try:
        # 三个 spec 名由装置源里的裸名常量派生（GATEWAY_SPEC_FILES 已删）：
        # 在名单里才可登记，不在名单里一律不可登记。
        case("gateway spec 名在装置源名单里 → 可登记",
             cfd.registrable(root, "gateway-parity",
                             "specs/gateway-parity/acceptance.md") is True)
        case("gateway spec 名不在装置源名单里 → 不可登记",
             cfd.registrable(root, "gateway-parity",
                             "specs/gateway-parity/other.md") is False)
    finally:
        td.cleanup()

    with tempfile.TemporaryDirectory() as td:
        # 装置两侧的目录清单必须一致：只有一侧改了就是「认不出」，不能按位置猜。
        root = Path(td)
        init_repo(root)
        write(root, "scripts/ui-server-parity/apparatus.py", UI_APPARATUS)
        write(root, "scripts/ui-server-parity/check_frozen.py",
              UI_FROZEN.replace("scan(root / 'scripts/exp008')",
                                "scan(root / 'scripts/exp007')"))
        commit(root, "base")
        case("ui 两侧清单源不一致 → 不可判定（不按位置猜）",
             cfd.registrable(root, "ui-server-parity",
                             "specs/ui-server-parity/acceptance.md") is None)
        case("ui 位置猜错的目录不参与登记",
             cfd.registrable(root, "ui-server-parity",
                             "scripts/ui-server-parity/x.py") is None)

    with tempfile.TemporaryDirectory() as td:
        # 交叉核对不能只靠 exp008 那一条：位置一侧换了目录名时，只有
        # 「两侧目录集合相等」这条能发现（否则按错目录判登记）。
        root = Path(td)
        init_repo(root)
        write(root, "scripts/ui-server-parity/apparatus.py", UI_APPARATUS)
        write(root, "scripts/ui-server-parity/check_frozen.py",
              UI_FROZEN.replace("scan(root / 'scripts/ui-server-parity')",
                                "scan(root / 'scripts/ui-server-parity-old')"))
        commit(root, "base")
        case("ui 位置一侧换了扫描目录名 → 不可判定",
             cfd.registrable(root, "ui-server-parity",
                             "specs/ui-server-parity/acceptance.md") is None)

    def ui_registrable(apparatus, frozen):
        """建一棵只有 ui 装置两半的树，返回 acceptance.md 的 registrable。"""
        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            init_repo(root)
            write(root, "scripts/ui-server-parity/apparatus.py", apparatus)
            write(root, "scripts/ui-server-parity/check_frozen.py", frozen)
            commit(root, "base")
            return cfd.registrable(root, "ui-server-parity",
                                   "specs/ui-server-parity/acceptance.md")

    def parity_registrable(names, path="specs/parity/README.md", names_expr=None):
        """按给定 ``names`` 元组建 parity 装置源，返回 path 的 registrable。

        ``names_expr`` 用来把绑定换成解析不出的形态（非字面量赋值）。
        """
        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            init_repo(root)
            value = names_expr or ("(" + ", ".join(repr(n) for n in names) + ")")
            body = ("\nSPEC_NAMES = ('placeholder.md',)\n"
                    "\ndef manifest_inventory(root):\n"
                    f"    names = {value}\n"
                    "    return {name: name for name in names}\n")
            write(root, "scripts/parity/check_frozen.py", body)
            commit(root, "base")
            return cfd.registrable(root, "parity", path)

    five = ('README.md', 'acceptance.md', 'normalization.md',
            'accepted-diffs.md', 'multimodal.md')
    case("parity 装置源名单 → README 可登记（正控）",
         parity_registrable(five) is True)
    # 清单只有装置源一份：源里删掉的名字立即不可登记（掉行才看得见），
    # 源里新增的名字立即要链（首登记才看得见）。字面量副本会让两向都静默。
    case("parity 源里删掉 multimodal.md → 该项不可登记（掉行不漏判）",
         parity_registrable(five[:4], "specs/parity/multimodal.md") is False)
    case("parity 源里新增 sixth.md → 该项要链（首登记不漏判）",
         parity_registrable(five + ('sixth.md',), "specs/parity/sixth.md") is True)
    case("parity 清单外的顶层文档仍不可登记",
         parity_registrable(five, "specs/parity/foo.md") is False)
    case("parity names 改成非字面量 → 不可判定（不退回副本）",
         parity_registrable(five, names_expr="SPEC_NAMES") is None)
    # 真装置的 manifest_path：已是 scripts/ specs/ 开头的名字原样登记，
    # 其余挂到 specs/parity/ 下。一律拼前缀会两种方向都判错。
    case("parity 带 specs/ 前缀的名字按原样登记",
         parity_registrable(five + ('specs/parity/sixth.md',),
                            "specs/parity/sixth.md") is True)
    case("parity 名字非规范（..） → 不可判定（--write 会拒）",
         parity_registrable(five + ('../escape.md',), "specs/parity/README.md") is None)
    with tempfile.TemporaryDirectory() as td:
        # names 先赋值再 +=：解释器看到六个名字，只认首个字面量会漏掉一个成员。
        root = Path(td)
        init_repo(root)
        body = ("\ndef manifest_inventory(root):\n"
                "    names = ('README.md', 'acceptance.md', 'normalization.md',\n"
                "             'accepted-diffs.md', 'multimodal.md')\n"
                "    names += ('sixth.md',)\n"
                "    return {name: name for name in names}\n")
        write(root, "scripts/parity/check_frozen.py", body)
        commit(root, "base")
        case("parity names 二次 += → 不可判定（不取首个字面量）",
             cfd.registrable(root, "parity", "specs/parity/README.md") is None)

    with tempfile.TemporaryDirectory() as td:
        # walrus 也是 Store，但值提取只认 Assign/AnnAssign。若此时回退到 external
        # （模块级 A2_SCRIPTS），解释器用函数内的新元组、解析器看到旧值 →
        # 新增成员漏判。Store 对不上就只能是「不可判定」。
        root = Path(td)
        init_repo(root)
        write(root, "scripts/gateway-parity/common.py", "A2_SCRIPTS = ('check_frozen.py',)\n")
        write(root, "scripts/gateway-parity/check_frozen.py",
              GW_BOOTSTRAP + "A2_SCRIPTS = common.A2_SCRIPTS\n"
              "\ndef manifest_inventory(root):\n"
              "    if (A2_SCRIPTS := ('check_frozen.py', 'extra.py')):\n"
              "        paths = [f'scripts/gateway-parity/{name}' for name in A2_SCRIPTS]\n"
              "        return {name: name for name in paths}\n"
              "    return {}\n")
        commit(root, "base")
        case("gateway A2_SCRIPTS 走 walrus 绑定 → 不可判定（不回退外部值）",
             cfd.gateway_inventory(root) is None)

    # 下面四条都是「认不出就阻塞」：任何一种静默退出都会让模型漂移变成假 READY。
    case("装置源解析不出（NUL 字节）→ 不可判定，不按子串猜",
         ui_registrable(UI_APPARATUS + "\x00", UI_FROZEN) is None)
    case("scan() 参数不是字面量目录 → 不可判定",
         ui_registrable(UI_APPARATUS,
                        UI_FROZEN.replace("scan(root / 'scripts/exp008')",
                                          "scan(root / target)")) is None)
    case("SPEC 绑定两次 → 不可判定（不取首个）",
         ui_registrable(UI_APPARATUS,
                        UI_FROZEN + "\nSPEC = ROOT / 'specs/ui-server-parity-alt'\n") is None)
    case("REQUIRED.discard(...) 就地改集合 → 不可判定",
         ui_registrable(UI_APPARATUS,
                        UI_FROZEN + "\nREQUIRED.discard('acceptance.md')\n") is None)
    case("check_frozen 里重绑 SPEC_FILES → 不可判定（不是导入的那份）",
         ui_registrable(UI_APPARATUS,
                        UI_FROZEN + "\nSPEC_FILES = SPEC_FILES[:-1]\n") is None)
    case("SPEC_FILES 从别处再导入一次 → 不可判定（来源不再是 apparatus）",
         ui_registrable(UI_APPARATUS,
                        UI_FROZEN + "\nfrom other import SPEC_FILES\n") is None)
    case("SPEC_FILES 从别处导入（唯一绑定） → 不可判定",
         ui_registrable(UI_APPARATUS,
                        UI_FROZEN.replace("from apparatus import",
                                          "from elsewhere import")) is None)
    case("apparatus.py 里 SPEC_FILES 又被导入一次 → 不可判定（字面量不再是全部）",
         ui_registrable(UI_APPARATUS + "\nfrom other import SPEC_FILES\n",
                        UI_FROZEN) is None)
    case("scan 调用带额外参数 → 不可判定（形状认不出）",
         ui_registrable(UI_APPARATUS,
                        UI_FROZEN.replace("scan(root / 'scripts/exp008')",
                                          "scan(root / 'scripts/exp008', extra)")) is None)
    case("多出一条认不出的 scan（属性调用）→ 不可判定（不在模型里的目录）",
         ui_registrable(
             UI_APPARATUS,
             UI_FROZEN.replace(
                 "    files.update(Path(p) for p in scan(root / 'scripts/exp008'))",
                 "    files.update(Path(p) for p in scan(root / 'scripts/exp008'))\n"
                 "    files.update(Path(p) for p in apparatus.scan(root / 'scripts/extra'))"))
         is None)
    case("scan 走属性调用（apparatus.scan）→ 不可判定",
         ui_registrable(UI_APPARATUS,
                        UI_FROZEN.replace("scan(root / 'scripts/exp008')",
                                          "apparatus.scan(root / 'scripts/exp008')")) is None)
    case("REQUIRED 偏离 SPEC_FILES（并集）→ 不可判定",
         ui_registrable(UI_APPARATUS,
                        UI_FROZEN.replace("REQUIRED = set(SPEC_FILES)",
                                          "REQUIRED = set(SPEC_FILES) | {'extra.md'}")) is None)
    case("REQUIRED 是字面量副本 → 不可判定（不认第二处清单）",
         ui_registrable(UI_APPARATUS,
                        UI_FROZEN.replace("REQUIRED = set(SPEC_FILES)",
                                          "REQUIRED = ('acceptance.md',)")) is None)
    case("EXP008 目录不在被扫描目录里 → 不可判定",
         ui_registrable(UI_APPARATUS,
                        UI_FROZEN.replace("{root / 'scripts/exp008' / name",
                                          "{root / 'scripts/exp009' / name")) is None)
    # 正控：同一夹具不改任何东西时必须能认出（否则上面四条只是「恒 None」）。
    case("两侧同形清单 → 可判定为登记范围",
         ui_registrable(UI_APPARATUS, UI_FROZEN) is True)
    # r17：四个名字都必须确实来自 apparatus 的那一份，别名/重定义都不算。
    # r18：夹具保留顶部那条合法导入再叠加一种绑定（旧夹具把合法导入一起换掉，
    # seen==0 就返回 None，测不出别名识别是否真的生效）。
    case("EXP008_FILES 改名别名绑到 SPEC_FILES → 不可判定",
         ui_registrable(UI_APPARATUS,
                        UI_FROZEN
                        + "\nfrom apparatus import EXP008_FILES as SPEC_FILES\n") is None)
    case("只有改名别名一条导入（无同名导入）→ 不可判定",
         ui_registrable(
             UI_APPARATUS,
             UI_FROZEN.replace("from apparatus import scan, SPEC_FILES, EXP008_FILES, "
                               "toolchain_files",
                               "from apparatus import scan, EXP008_FILES as SPEC_FILES, "
                               "EXP008_FILES, toolchain_files")) is None)
    case("合法导入 + import ... as SPEC_FILES → 不可判定",
         ui_registrable(UI_APPARATUS,
                        UI_FROZEN + "\nimport other as SPEC_FILES\n") is None)
    case("合法导入 + import ... as scan → 不可判定",
         ui_registrable(UI_APPARATUS,
                        UI_FROZEN + "\nimport other as scan\n") is None)
    case("相对导入 from .apparatus import ... → 不可判定",
         ui_registrable(
             UI_APPARATUS,
             UI_FROZEN.replace("from apparatus import scan, SPEC_FILES, EXP008_FILES, "
                               "toolchain_files",
                               "from .apparatus import scan, SPEC_FILES, EXP008_FILES, "
                               "toolchain_files")) is None)
    case("toolchain_files 从别处再导入一次 → 不可判定",
         ui_registrable(UI_APPARATUS,
                        UI_FROZEN + "\nfrom other import toolchain_files\n") is None)
    case("toolchain_files 被重新赋值 → 不可判定",
         ui_registrable(UI_APPARATUS,
                        UI_FROZEN + "\ntoolchain_files = ()\n") is None)
    case("scan 被同名 def 覆盖 → 不可判定",
         ui_registrable(UI_APPARATUS,
                        UI_FROZEN + "\ndef scan(paths):\n    return ()\n") is None)
    case("导入只出现在函数体里（模块级未绑）→ 不可判定",
         ui_registrable(UI_APPARATUS, UI_FROZEN.replace(
             "from apparatus import scan, SPEC_FILES, EXP008_FILES, toolchain_files",
             "") + "\ndef _later():\n"
             "    from apparatus import scan, SPEC_FILES, EXP008_FILES, toolchain_files\n"
             "    return scan, SPEC_FILES, EXP008_FILES, toolchain_files\n") is None)
    case("import scan（无 asname，顶层名即导入名）→ 不可判定",
         ui_registrable(UI_APPARATUS, UI_FROZEN + "\nimport scan\n") is None)
    case("import pkg.scan（点号形态只绑 pkg）→ 不算 scan 的绑定",
         ui_registrable(UI_APPARATUS, UI_FROZEN + "\nimport pkg.scan\n") is True)

    # ---------------------------------------------------------------- r19 ui 负控
    # apparatus.py 是 bootstrap 自己按 ``__file__`` 装进来的那一份，``ROOT`` 是
    # SPEC 的锚：两者只看拼写就仍能被换成别处（假 PASS/假 FAIL），所以整块比对。
    case("ui bootstrap 的 _boot_source 指向别处 → 不可判定",
         ui_registrable(UI_APPARATUS, UI_FROZEN.replace(
             "_boot_source = _boot_os.path.join(_boot_dir, 'apparatus.py')",
             "_boot_source = _boot_os.path.join('/tmp/shared', 'apparatus.py')")) is None)
    case("ui bootstrap 的 _boot_dir 换成 cwd → 不可判定",
         ui_registrable(UI_APPARATUS, UI_FROZEN.replace(
             "_boot_dir = _boot_os.path.dirname(_boot_os.path.abspath(__file__))",
             "_boot_dir = _boot_os.getcwd()")) is None)
    case("ui ROOT 不是 __file__ 派生（Path('/evil')）→ 不可判定",
         ui_registrable(UI_APPARATUS, UI_FROZEN.replace(
             "ROOT = Path(__file__).resolve().parents[2]",
             "ROOT = Path('/evil')")) is None)
    case("ui 的 Path 不是从 pathlib 导入（from evil import Path）→ 不可判定",
         ui_registrable(UI_APPARATUS, UI_FROZEN + "\nfrom evil import Path\n") is None)
    case("ui ROOT 用 parent.parent.parent 等价写法 → 不可判定（只认模型化的 parents[2]）",
         ui_registrable(UI_APPARATUS, UI_FROZEN.replace(
             "ROOT = Path(__file__).resolve().parents[2]",
             "ROOT = Path(__file__).resolve().parent.parent.parent")) is None)
    case("ui 块外二次绑定 _boot_dir → 不可判定",
         ui_registrable(UI_APPARATUS,
                        UI_FROZEN + "\n_boot_dir = '/tmp/shared'\n") is None)
    case("ui 块外 global 重绑 _boot_sys → 不可判定",
         ui_registrable(UI_APPARATUS, UI_FROZEN
                        + "\ndef _f():\n    global _boot_sys\n    _boot_sys = None\n") is None)
    case("ui 块外 sys.modules['apparatus'] 注入 → 不可判定",
         ui_registrable(UI_APPARATUS,
                        UI_FROZEN + "\nimport sys\nsys.modules['apparatus'] = None\n") is None)
    case("ui 第二个 preflight 结束锚点 → 不可判定",
         ui_registrable(UI_APPARATUS, UI_FROZEN
                        + "\n_boot_namespace['preflight'](_boot_dir)\n") is None)
    # ---------------------------------------------------------------- r21 ui 负控
    # gateway 侧的助手来源由 `_BOOTSTRAP_HELPERS` 钉住，ui 侧的五个别名导入此前
    # 没有对应名单：块后 `import other as _boot_types` 换掉 `_boot_module` 的来源
    # 也照样可判定。以下每条只改一处。
    case("ui 块外 import other as _boot_types → 不可判定（助手来源）",
         ui_registrable(UI_APPARATUS, UI_FROZEN + "\nimport other as _boot_types\n") is None)
    case("ui 块外 getattr(_boot_os, '__dict__') → 不可判定",
         ui_registrable(UI_APPARATUS, UI_FROZEN
                        + "\ngetattr(_boot_os, '__dict__')['curdir'] = '/evil'\n") is None)
    case("ui 块外 object.__setattr__(_boot_sys, ...) → 不可判定",
         ui_registrable(UI_APPARATUS, UI_FROZEN
                        + "\nobject.__setattr__(_boot_sys, 'modules', {})\n") is None)
    case("ui 块外 from sys import modules as _m 后下标注入 → 不可判定",
         ui_registrable(UI_APPARATUS, UI_FROZEN
                        + "\nfrom sys import modules as _m\n_m['apparatus'] = None\n") is None)
    case("ui 助手来源名单覆盖模型块的每个导入（增删都要红）",
         {name for name, _ in cfd._UI_BOOTSTRAP_HELPERS}
         == cfd._import_names(cfd._parse('\n'.join(cfd._UI_BOOTSTRAP))))
    case("函数体内同名局部变量不影响模块级清单（不误判二次绑定）",
         ui_registrable(UI_APPARATUS.replace(
             "SPEC_FILES = ('acceptance.md',)",
             "SPEC_FILES = ('acceptance.md',)\n"
             "def _local():\n    SPEC_FILES = ()\n    return SPEC_FILES\n"),
             UI_FROZEN) is not None)

    # ---------------------------------------------------------------- r22 ui 负控
    # gateway 侧 r22 补的「换名/换壳」形态要在 ui 侧同样钉住：块外别名、函数体里
    # 绑的 sys 别名、`from sys import *`、把模块对象交给形参，以及 `REQUIRED`
    # 的别名就地改。
    case("ui 块外 _b = _boot_sys 后按别名注入 → 不可判定",
         ui_registrable(UI_APPARATUS, UI_FROZEN
                        + "\n_b = _boot_sys\n_b.modules['apparatus'] = None\n") is None)
    case("ui 函数体内 import sys as _b 后注入 → 不可判定",
         ui_registrable(UI_APPARATUS, UI_FROZEN
                        + "\ndef _swap():\n    import sys as _b\n"
                          "    _b.modules['apparatus'] = None\n_swap()\n") is None)
    case("ui 块外 from sys import * 后按裸名注入 → 不可判定",
         ui_registrable(UI_APPARATUS, UI_FROZEN
                        + "\nfrom sys import *\nmodules['apparatus'] = None\n") is None)
    case("ui 块外 def _swap(m): m.preflight = ... 后 _swap(_boot_module) → 不可判定",
         ui_registrable(UI_APPARATUS, UI_FROZEN
                        + "\ndef _swap(m):\n    m.preflight = None\n"
                          "_swap(_boot_module)\n") is None)
    case("ui 块外 _r = REQUIRED 后就地改 → 不可判定",
         ui_registrable(UI_APPARATUS,
                        UI_FROZEN + "\n_r = REQUIRED\n_r.update({'evil.md'})\n") is None)
    case("ui 块外 getattr(REQUIRED, 'update') 就地改 → 不可判定",
         ui_registrable(UI_APPARATUS, UI_FROZEN
                        + "\ngetattr(REQUIRED, 'update')({'evil.md'})\n") is None)
    case("REQUIRED 的 getattr 就地改 → 清单来源不可判定（直接问读取器）",
         cfd._ui_required_names(cfd._parse(
             UI_FROZEN + "\ngetattr(REQUIRED, 'update')({'evil.md'})\n")) is None)
    case("ui 块外只读 REQUIRED（推导式里遍历）→ 仍可判定",
         ui_registrable(UI_APPARATUS, UI_FROZEN
                        + "\nNAMES = [name for name in REQUIRED]\n") is not None)

    # ---------------------------------------------------------------- r23 ui 负控
    # 与 gateway 侧同一批形态：sys 表先取别名再改、REQUIRED 的别名在函数体里就地
    # 改（`difference_update` 是 update 的另一种拼写），以及三处清单读取器遇到
    # 嵌套 def 时按「作用域认不出」阻塞。
    case("ui 块外 _p = _boot_sys.path 后 append → 不可判定",
         ui_registrable(UI_APPARATUS, UI_FROZEN
                        + "\n_p = _boot_sys.path\n_p.append('/tmp/x')\n") is None)
    case("ui 块外 _m = _boot_sys.modules 后 update → 不可判定",
         ui_registrable(UI_APPARATUS, UI_FROZEN
                        + "\n_m = _boot_sys.modules\n_m.update({'apparatus': None})\n") is None)
    # r28：`_sys_table` 的接收者改按包含匹配后，ui 侧的同一拼写也要拦得住。
    case("ui 块外 sys 模块名装进容器后取其 modules 表 → 不可判定",
         ui_registrable(UI_APPARATUS, UI_FROZEN
                        + "\nimport sys as _s\n"
                          "[_s][0].modules.update({'apparatus': None})\n") is None)
    case("ui 函数体内 _s = REQUIRED 后 difference_update → 不可判定",
         ui_registrable(UI_APPARATUS, UI_FROZEN
                        + "\ndef _r():\n    _s = REQUIRED\n"
                          "    _s.difference_update({'evil.md'})\n_r()\n") is None)
    case("ui 清单函数里嵌套 def → exp008 目录不可判定",
         cfd._ui_exp008_dir(cfd._parse(UI_FROZEN.replace(
             "    return ({folder / name for name in REQUIRED}",
             "    def _extra():\n        return None\n"
             "    return ({folder / name for name in REQUIRED}"))) is None)
    case("ui 扫描函数里嵌套 def → 扫描目录不可判定",
         cfd._ui_scan_dirs(cfd._parse(UI_FROZEN.replace(
             "    files = {Path(p) for p in scan(root / 'scripts/ui-server-parity')}",
             "    def _extra():\n        return None\n"
             "    files = {Path(p) for p in scan(root / 'scripts/ui-server-parity')}"))) is None)
    case("ui 扫描函数里嵌套 lambda → 扫描目录不可判定",
         cfd._ui_scan_dirs(cfd._parse(UI_FROZEN.replace(
             "    files = {Path(p) for p in scan(root / 'scripts/ui-server-parity')}",
             "    _f = lambda x: x\n"
             "    files = {Path(p) for p in scan(root / 'scripts/ui-server-parity')}"))) is None)

    # ---------------------------------------------------------------- r24 ui 负控
    # gateway 侧 r24 新增的绑定形态在 ui 侧同源（同一个 `_alias_roots`），这里
    # 只补三条最容易在 ui 侧单独走形的：链式赋值、解包（含星号目标）、for 目标
    # 个数不匹配；另外把 `sort`/`reverse` 这两种 list 就地改也算进改写名单。
    case("ui 块外链式赋值 _r = _q = REQUIRED 后就地改 → 不可判定",
         ui_registrable(UI_APPARATUS, UI_FROZEN
                        + "\n_r = _q = REQUIRED\n_q.update({'evil.md'})\n") is None)
    case("ui 块外解包（星号目标）拿到 REQUIRED → 不可判定",
         ui_registrable(UI_APPARATUS, UI_FROZEN
                        + "\n_x, *_rest = (None, REQUIRED)\n"
                          "_rest[0].update({'evil.md'})\n") is None)
    case("ui 块外 for 字面序列个数不匹配 → 循环变量仍可能拿到 REQUIRED",
         ui_registrable(UI_APPARATUS, UI_FROZEN
                        + "\nfor _r in (REQUIRED, REQUIRED):\n"
                          "    _r.update({'evil.md'})\n") is None)
    case("ui 块外 sys.path.sort() 重排导入解析顺序 → 不可判定",
         ui_registrable(UI_APPARATUS, UI_FROZEN
                        + "\n_boot_sys.path.sort()\n") is None)
    case("ui 块外 sys.path.reverse() 重排导入解析顺序 → 不可判定",
         ui_registrable(UI_APPARATUS, UI_FROZEN
                        + "\n_boot_sys.path.reverse()\n") is None)
    case("ui 块外以属性形式调用被改函数（实例形参不叫 self）→ 不可判定",
         ui_registrable(UI_APPARATUS, UI_FROZEN
                        + "\nclass _C:\n    def _swap(m, x):\n"
                          "        x.update({})\n"
                          "_C()._swap(_boot_manifest)\n") is None)
    case("ui 块外默认参数里带被看护对象、调用点不传 → 不可判定",
         ui_registrable(UI_APPARATUS, UI_FROZEN
                        + "\ndef _swap(x=_boot_manifest):\n"
                          "    x.update({})\n_swap()\n") is None)

    # ---------------------------------------------------------------- r26 ui 负控
    # gateway 侧 r26 把「交给函数」的判定从「实参根名」改成「实参里读到的名字」：
    # `_swap(*[REQUIRED])` / `_swap([REQUIRED])` 走展开与容器绕过去的两条同源。
    # 另把改名单的 `sort`/`reverse` 直接钉在常量上——`REQUIRED` 是 set，写不出
    # 一条真会跑的 `REQUIRED.reverse()` 夹具。
    case("ui 块外把 REQUIRED 展开后交给函数 → 不可判定",
         ui_registrable(UI_APPARATUS, UI_FROZEN
                        + "\n_swap(*[REQUIRED])\n") is None)
    case("ui 块外把 REQUIRED 装进容器再交给函数 → 不可判定",
         ui_registrable(UI_APPARATUS, UI_FROZEN
                        + "\n_swap([REQUIRED])\n") is None)
    case("ui 块外解包 list 字面量拿到 REQUIRED → 不可判定",
         ui_registrable(UI_APPARATUS, UI_FROZEN
                        + "\n_a, _b = [REQUIRED, None]\n"
                          "_a.update({'evil.md'})\n") is None)
    case("ui 块外把 REQUIRED 装进容器后就地改 → 不可判定",
         ui_registrable(UI_APPARATUS, UI_FROZEN
                        + "\n[REQUIRED][0].update({'evil.md'})\n") is None)
    case("改写名单含 sort / reverse（list 就地改）",
         {'sort', 'reverse'} <= cfd._MUTATOR_ATTRS)

    with tempfile.TemporaryDirectory() as td:
        # unexpected registered file 的两条负控：仓外绝对路径 / merge-base 无行。
        root = Path(td)
        init_repo(root)
        row(root, "ui-server-parity", "specs/ui-server-parity/multimodal.md", sha("OLD\n"), "OLD\n")
        write(root, "scripts/ui-server-parity/apparatus.py",
              UI_APPARATUS.replace("'acceptance.md',)", "'acceptance.md', 'multimodal.md')"))
        write(root, "scripts/ui-server-parity/check_frozen.py", UI_FROZEN)
        commit(root, "base")
        base = git(root, "rev-parse", "HEAD")
        write(root, "specs/ui-server-parity/multimodal.md", "NEW\n")
        write(root, "specs/ui-server-parity/frozen/20260101-v.md",
              f"Change: {sha('OLD\n')} {sha('NEW\n')} "
              "specs/ui-server-parity/multimodal.md\n" + EVENT_FIELDS)
        commit(root, "delivery")
        edges, _, _ = cfd.event_evidence(root, "ui-server-parity", base)
        ctx = {"root": root, "kind": "ui-server-parity", "mb": base,
               "edges": edges, "pending": set()}
        case("unexpected registered file 在仓外 → block",
             cfd.classify(ctx, "unexpected registered file: /etc/passwd")[0] == "block")
        case("unexpected registered file 在 base 无行 → block",
             cfd.classify(ctx, f"unexpected registered file: "
                               f"{root.resolve()}/specs/ui-server-parity/fresh.md")[0] == "block")
        case("unexpected registered file 有行且有链 → ok",
             cfd.classify(ctx, "unexpected registered file: "
                               f"{root.resolve()}/specs/ui-server-parity/multimodal.md")[0] == "ok")

    with tempfile.TemporaryDirectory() as td:
        # toolchain 文件按存在与否登记：删掉并写 Change: <old> - 才是合法交接。
        apparatus = UI_APPARATUS.replace("TOOLCHAIN_FILES = ()",
                                         "TOOLCHAIN_FILES = ('rust-toolchain',)")
        payload = {"status": "FAIL", "failures": [HASH_LIST]}
        body = "import json\nprint(json.dumps(" + repr(payload) + "))\n" + UI_FROZEN
        frozen = ("# fake FROZEN\n\n"
                  f"{sha('1.90.0\\n')}  ../../rust-toolchain\n"
                  f"{sha(apparatus)}  ../../scripts/ui-server-parity/apparatus.py\n"
                  f"{sha(body)}  ../../scripts/ui-server-parity/check_frozen.py\n")

        def toolchain_delivery(td, events):
            return delivery(Path(td), "ui-server-parity", payload,
                            base_files=[("rust-toolchain", "1.90.0\n"),
                                        ("scripts/ui-server-parity/apparatus.py", apparatus),
                                        ("specs/ui-server-parity/FROZEN.md", frozen)],
                            removes=["rust-toolchain"], events=events,
                            source=UI_FROZEN)

        proc = toolchain_delivery(td, [])
        case("toolchain 删除、无删除链 → BLOCKED",
             proc.returncode == 1 and "BLOCKED 1 / READY 0" in proc.stdout,
             f"rc={proc.returncode} out={proc.stdout}")

    with tempfile.TemporaryDirectory() as td:
        apparatus = UI_APPARATUS.replace("TOOLCHAIN_FILES = ()",
                                         "TOOLCHAIN_FILES = ('rust-toolchain',)")
        payload = {"status": "FAIL", "failures": [HASH_LIST]}
        body = "import json\nprint(json.dumps(" + repr(payload) + "))\n" + UI_FROZEN
        frozen = ("# fake FROZEN\n\n"
                  f"{sha('1.90.0\\n')}  ../../rust-toolchain\n"
                  f"{sha(apparatus)}  ../../scripts/ui-server-parity/apparatus.py\n"
                  f"{sha(body)}  ../../scripts/ui-server-parity/check_frozen.py\n")
        proc = delivery(Path(td), "ui-server-parity", payload,
                        base_files=[("rust-toolchain", "1.90.0\n"),
                                    ("scripts/ui-server-parity/apparatus.py", apparatus),
                                    ("specs/ui-server-parity/FROZEN.md", frozen)],
                        removes=["rust-toolchain"],
                        source=UI_FROZEN,
                        events=[("specs/ui-server-parity/frozen/20260101-t.md",
                                 f"Change: {sha('1.90.0\n')} - rust-toolchain\n"
                                 + EVENT_FIELDS)])
        case("toolchain 删除、Change: <old> - 链 → READY",
             proc.returncode == 0 and "BLOCKED 0 / READY 1" in proc.stdout,
             f"rc={proc.returncode} out={proc.stdout}")

    with tempfile.TemporaryDirectory() as td:
        # 清单来源不可读 → 无法判定是否可登记：必须阻塞，不能折算成 '-'。
        payload = {"status": "FAIL", "failures": [HASH_LIST]}
        body = "import json\nprint(json.dumps(" + repr(payload) + "))\n"
        frozen = ("# fake FROZEN\n\n"
                  f"{sha(body)}  ../../scripts/gateway-parity/check_frozen.py\n"
                  f"{sha('GONE\\n')}  ../../scripts/gateway-parity/common.py\n")
        proc = delivery(Path(td), "gateway-parity", payload,
                        base_files=[("scripts/gateway-parity/common.py", "GONE\n"),
                                    ("specs/gateway-parity/FROZEN.md", frozen)],
                        removes=["scripts/gateway-parity/common.py"],
                        events=[("specs/gateway-parity/frozen/20260101-n.md",
                                 f"Change: {sha('GONE\n')} - "
                                 "scripts/gateway-parity/common.py\n"
                                 + EVENT_FIELDS)])
        case("清单来源不可读 → BLOCKED（不可判定不折算成 '-'）",
             proc.returncode == 1 and "cannot tell whether --write still registers it"
             in proc.stdout,
             f"rc={proc.returncode} out={proc.stdout}")

    with tempfile.TemporaryDirectory() as td:
        # 装置源认不出 → 连「该扫哪些目录」都不可判定：扫描范围为空不能静默 READY。
        payload = {"verdict": "PASS", "errors": []}
        frozen = ("# fake FROZEN\n\n"
                  f"{sha('GONE\\n')}  ../../scripts/ui-server-parity/apparatus.py\n")
        proc = delivery(Path(td), "ui-server-parity", payload, source=UI_FROZEN,
                        base_files=[("scripts/ui-server-parity/apparatus.py", "GONE\n"),
                                    ("specs/ui-server-parity/FROZEN.md", frozen)])
        case("ui 清单源认不出 → BLOCKED（扫描范围不可判定不静默）",
             proc.returncode == 1 and "cannot tell ui-server-parity's inventory"
             in proc.stdout,
             f"rc={proc.returncode} out={proc.stdout}")

    with tempfile.TemporaryDirectory() as td:
        # SPEC_FILES 的成员住在 specs/ 目录下，也要落在首登记扫描范围里，
        # 否则 --write 加行、预检不索链。
        root = Path(td)
        init_repo(root)
        write(root, "scripts/ui-server-parity/apparatus.py",
              UI_APPARATUS.replace("'acceptance.md',)", "'acceptance.md', 'multimodal.md')"))
        write(root, "scripts/ui-server-parity/check_frozen.py", UI_FROZEN)
        write(root, "specs/ui-server-parity/multimodal.md", "NEW\n")
        write(root, "specs/ui-server-parity/FROZEN.md",
              "# fake FROZEN\n\n"
              f"{sha('A\n')}  ../../scripts/ui-server-parity/apparatus.py\n"
              f"{sha('B\n')}  ../../scripts/ui-server-parity/check_frozen.py\n")
        commit(root, "base")
        mb = git(root, "rev-parse", "HEAD")
        added, _ = cfd.unregistered_added(root, "ui-server-parity", mb)
        case("ui 新增的 SPEC_FILES 成员在扫描范围内（首登记不漏判）",
             'specs/ui-server-parity/multimodal.md' in added, f'added={added}')

    with tempfile.TemporaryDirectory() as td:
        # 注释掉的成员不算登记：按正则抽字面量会把它当「仍在清单」，掉行被漏报。
        payload = {"status": "FAIL", "failures": [HASH_LIST]}
        base_common = "A2_SCRIPTS = ('check_frozen.py', 'common.py', 'observer.py')\n"
        head_common = ("A2_SCRIPTS = ('check_frozen.py', 'common.py',\n"
                       "              # 'observer.py',  # retired)\n"
                       "              )\n")
        body = "import json\nprint(json.dumps(" + repr(payload) + "))\n"
        frozen = ("# fake FROZEN\n\n"
                  f"{sha('OBS\n')}  ../../scripts/gateway-parity/observer.py\n"
                  f"{sha(base_common)}  ../../scripts/gateway-parity/common.py\n"
                  f"{sha(body)}  ../../scripts/gateway-parity/check_frozen.py\n")
        base_files = [("scripts/gateway-parity/observer.py", "OBS\n"),
                      ("scripts/gateway-parity/common.py", base_common),
                      ("specs/gateway-parity/FROZEN.md", frozen)]
        event = ("specs/gateway-parity/frozen/20260101-c.md",
                 f"Change: {sha('OBS\n')} - scripts/gateway-parity/observer.py\n"
                 + EVENT_FIELDS)
        proc = delivery(Path(td), "gateway-parity", payload, base_files=base_files,
                        source=gw_source(),
                        files=[("scripts/gateway-parity/common.py", head_common)])
        case("清单里注释掉的成员、无掉行链 → BLOCKED",
             proc.returncode == 1 and "row dropped from the new manifest" in proc.stdout,
             f"rc={proc.returncode} out={proc.stdout}")

    with tempfile.TemporaryDirectory() as td:
        payload = {"status": "FAIL", "failures": [HASH_LIST]}
        base_common = "A2_SCRIPTS = ('check_frozen.py', 'common.py', 'observer.py')\n"
        head_common = ("A2_SCRIPTS = ('check_frozen.py', 'common.py',\n"
                       "              # 'observer.py',  # retired)\n"
                       "              )\n")
        body = "import json\nprint(json.dumps(" + repr(payload) + "))\n"
        frozen = ("# fake FROZEN\n\n"
                  f"{sha('OBS\n')}  ../../scripts/gateway-parity/observer.py\n"
                  f"{sha(base_common)}  ../../scripts/gateway-parity/common.py\n"
                  f"{sha(body)}  ../../scripts/gateway-parity/check_frozen.py\n")
        proc = delivery(Path(td), "gateway-parity", payload,
                        base_files=[("scripts/gateway-parity/observer.py", "OBS\n"),
                                    ("scripts/gateway-parity/common.py", base_common),
                                    ("specs/gateway-parity/FROZEN.md", frozen)],
                        source=gw_source(),
                        files=[("scripts/gateway-parity/common.py", head_common)],
                        events=[("specs/gateway-parity/frozen/20260101-c.md",
                                 f"Change: {sha('OBS\n')} - "
                                 "scripts/gateway-parity/observer.py\n"
                                 + EVENT_FIELDS)])
        case("清单里注释掉的成员、Change: <old> - 链 → READY",
             proc.returncode == 0 and "BLOCKED 0 / READY 1" in proc.stdout,
             f"rc={proc.returncode} out={proc.stdout}")

    with tempfile.TemporaryDirectory() as td:
        # 清单形态认不出（非字面量元组）：不可判定，不能猜成「仍登记」或「已掉行」。
        payload = {"status": "FAIL", "failures": [HASH_LIST]}
        base_common = "A2_SCRIPTS = ('check_frozen.py',)\n"
        head_common = "A2_SCRIPTS = tuple(['check_frozen.py'])\n"
        body = "import json\nprint(json.dumps(" + repr(payload) + "))\n"
        frozen = ("# fake FROZEN\n\n"
                  f"{sha(base_common)}  ../../scripts/gateway-parity/common.py\n"
                  f"{sha(body)}  ../../scripts/gateway-parity/check_frozen.py\n")
        proc = delivery(Path(td), "gateway-parity", payload,
                        base_files=[("scripts/gateway-parity/common.py", base_common),
                                    ("specs/gateway-parity/FROZEN.md", frozen)],
                        source=gw_source(),
                        files=[("scripts/gateway-parity/common.py", head_common)])
        case("清单形态认不出 → BLOCKED（不猜 True）",
             proc.returncode == 1 and "cannot tell" in proc.stdout,
             f"rc={proc.returncode} out={proc.stdout}")

    # ------------------------------------------------------- fail-closed
    with tempfile.TemporaryDirectory() as td, tempfile.TemporaryDirectory() as cd:
        # 运行期被审树 HEAD A→B→A 漂移：起止采样点都是 A（旧实现漏检），子装置
        # 若读运行时工作树就会看到 B 并给出与钉住证据不符的结论。子装置必须跑在
        # 钉住提交的一次性快照里：它读到的 marker 恒为 A。
        root, ctl = Path(td), Path(cd)
        init_repo(root)
        source = ("import json, time\n"
                  "from pathlib import Path\n"
                  f"ctl = Path({str(ctl)!r})\n"
                  "root = Path(__file__).resolve().parents[2]\n"
                  "(ctl / 'started').write_text('1')\n"
                  "deadline = time.time() + 60\n"
                  "while not (ctl / 'go').exists() and time.time() < deadline:\n"
                  "    time.sleep(0.01)\n"
                  "marker = (root / 'marker.txt').read_text().strip()\n"
                  "(ctl / 'read').write_text(marker)\n"
                  "print(json.dumps({'status': 'PASS' if marker == 'A' else 'FAIL',\n"
                  "                  'failures': [] if marker == 'A'\n"
                  "                              else ['unknown marker ' + marker]}))\n"
                  "while not (ctl / 'restored').exists() and time.time() < deadline:\n"
                  "    time.sleep(0.01)\n")
        root.joinpath("scripts/parity").mkdir(parents=True)
        write(root, "scripts/parity/check_frozen.py", source + PARITY_MANIFEST)
        write(root, "specs/parity/FROZEN.md",
              "# fake FROZEN\n\n"
              f"{sha(source + PARITY_MANIFEST)}  ../../scripts/parity/check_frozen.py\n")
        write(root, "marker.txt", "A\n")
        commit(root, "base")
        git(root, "update-ref", "refs/remotes/origin/main", git(root, "rev-parse", "HEAD"))
        commit(root, "delivery")
        a = git(root, "rev-parse", "HEAD")
        write(root, "marker.txt", "B\n")
        commit(root, "B")
        b = git(root, "rev-parse", "HEAD")
        git(root, "checkout", "-q", a)

        def flip_and_release():
            for _ in range(6000):
                if (ctl / "started").exists():
                    break
                time.sleep(0.005)
            else:
                (ctl / "error").write_text("child never started")
                return
            try:
                git(root, "checkout", "-q", b)
            except subprocess.CalledProcessError as error:
                (ctl / "error").write_text(f"checkout B: {error}")
                return
            (ctl / "go").write_text("1")
            for _ in range(6000):
                if (ctl / "read").exists():
                    break
                time.sleep(0.005)
            try:
                git(root, "checkout", "-q", a)
            except subprocess.CalledProcessError as error:
                (ctl / "error").write_text(f"checkout A: {error}")
                return
            (ctl / "restored").write_text("1")

        thread = threading.Thread(target=flip_and_release)
        thread.start()
        try:
            proc = run_tool("--root", str(root), "--base", "origin/main")
        finally:
            thread.join(timeout=90)
        seen = (ctl / "read").read_text().strip() if (ctl / "read").exists() else "<none>"
        error = (ctl / "error").read_text() if (ctl / "error").exists() else ""
        case("运行期 A→B→A 漂移 → 子装置读钉住快照（marker=A，READY）",
             proc.returncode == 0 and "BLOCKED 0 / READY 1" in proc.stdout
             and seen == "A" and not error,
             f"rc={proc.returncode} marker={seen} error={error} out={proc.stdout}")
    with tempfile.TemporaryDirectory() as td:
        # 事件文件内容也必须按起跑 SHA 读：HEAD 前进后写进去的行不能补链。
        root = Path(td)
        init_repo(root)
        write(root, "specs/parity/frozen/keep.md", "seed\n")
        commit(root, "mb")
        mb = git(root, "rev-parse", "HEAD")
        first = ("Change: " + "a" * 64 + " " + "b" * 64
                 + " scripts/parity/x.py\n" + EVENT_FIELDS)
        write(root, "specs/parity/frozen/20260101-p.md", first)
        commit(root, "delivery")
        pinned = git(root, "rev-parse", "HEAD")
        write(root, "specs/parity/frozen/20260101-p.md",
              first + "Change: " + "c" * 64 + " " + "d" * 64
              + " scripts/parity/y.py\n")
        commit(root, "later")
        cfd._PINNED_HEAD["sha"] = pinned
        try:
            edges, _, _ = cfd.event_evidence(root, "parity", mb)
        finally:
            cfd._PINNED_HEAD["sha"] = None
        case("事件文件内容钉住起跑 SHA（后提交的行不算证据）",
             edges == [("20260101-p.md", "scripts/parity/x.py", "a" * 64, "b" * 64)],
             f"edges={edges}")

    with tempfile.TemporaryDirectory() as td:
        # 内部读取钉住起跑 SHA：HEAD 已前进到 C，判定仍按 A（防 A→C→A 抖动）。
        root = Path(td)
        init_repo(root)
        write(root, "scripts/gateway-parity/common.py", "A2_SCRIPTS = ('alpha.py',)\n")
        write(root, "scripts/gateway-parity/check_frozen.py", gw_source())
        commit(root, "base")
        pinned = git(root, "rev-parse", "HEAD")
        write(root, "scripts/gateway-parity/common.py", "A2_SCRIPTS = ('beta.py',)\n")
        commit(root, "moved")
        cfd._PINNED_HEAD["sha"] = pinned
        try:
            at_pin = cfd.registrable(root, "gateway-parity",
                                     "scripts/gateway-parity/alpha.py")
            at_head = cfd.registrable(root, "gateway-parity",
                                      "scripts/gateway-parity/beta.py")
        finally:
            cfd._PINNED_HEAD["sha"] = None
        case("内部读取钉住起跑 SHA（不吃运行后的 HEAD）",
             at_pin is True and at_head is False, f"pin={at_pin} head={at_head}")

    with tempfile.TemporaryDirectory() as td:
        root = Path(td)
        proc = delivery(root, "parity", {"status": "FAIL", "failures": []})
        case("FAIL + 空失败列表 → BLOCKED（fail-closed）",
             proc.returncode == 1 and "BLOCKED 1 / READY 0" in proc.stdout,
             f"rc={proc.returncode}")
        proc = run_tool("--root", str(root), "--base", "no-such-ref")
        case("base 解析失败 → BLOCKED 1 / READY 0",
             proc.returncode == 1 and "BLOCKED 1 / READY 0" in proc.stdout,
             f"rc={proc.returncode}")

    # -------------------------------------------------- P0-A 已知报文整串匹配
    with tempfile.TemporaryDirectory() as td:
        ctx = {"root": Path(td), "kind": "parity", "mb": None, "edges": [],
               "pending": set(), "dropped": set()}
        case("hash list 报文原文仍判 ok",
             cfd.classify(ctx, cfd.HASH_LIST_MSG)[0] == "ok")
        for tail in ("; permission denied", "; corruption detected", " (retry)"):
            verdict = cfd.classify(ctx, cfd.HASH_LIST_MSG + tail)
            case(f"已知报文 + 未知后缀 {tail!r} → block（子串匹配会假 READY）",
                 verdict[0] == "block", f"{verdict}")

    # ---------------------------------- P0-B Pending 评审掩盖坏 / 多余 Change 行
    PENDING_FIELDS = ("Calibration-Evidence: test fixture\n"
                      "Independent-Reviews: Pending: 第二家未回\n"
                      "Approved-By: test fixture\n"
                      "Approval-Basis: test fixture\n")

    def pending_delivery(td, rows, files=()):
        payload = {"status": "FAIL", "failures": [HASH_LIST]}
        return delivery(Path(td), "parity", payload, files=list(files),
                        events=[("specs/parity/frozen/20260101-p.md",
                                 PENDING_FIELDS + "".join(row + "\n" for row in rows))])

    with tempfile.TemporaryDirectory() as td:
        proc = pending_delivery(td, ["Change: garbage-row"])
        case("Pending 下的畸形 Change 行 → BLOCKED（回填评审后真装置才炸）",
             proc.returncode == 1 and "invalid Change row" in proc.stdout,
             f"rc={proc.returncode} out={proc.stdout}")
    with tempfile.TemporaryDirectory() as td:
        proc = pending_delivery(td,
                                [f"Change: {sha('A\n')} {sha('A\n')} scripts/parity/x.py"],
                                files=[("scripts/parity/x.py", "A\n")])
        case("Pending 下的 old == new 行 → BLOCKED",
             proc.returncode == 1
             and "invalid Change path or equal hashes" in proc.stdout,
             f"rc={proc.returncode} out={proc.stdout}")
    with tempfile.TemporaryDirectory() as td:
        duplicate = f"Change: - {sha('B\n')} scripts/parity/x.py"
        proc = pending_delivery(td, [duplicate, duplicate],
                                files=[("scripts/parity/x.py", "B\n")])
        case("Pending 下的重复 Change 行 → BLOCKED",
             proc.returncode == 1 and "duplicate Change row" in proc.stdout,
             f"rc={proc.returncode} out={proc.stdout}")
    with tempfile.TemporaryDirectory() as td:
        proc = pending_delivery(td, [f"Change: {sha('A\n')} {sha('B\n')} "
                                     "scripts/parity/absent.py"])
        case("Pending 下的未使用边 → BLOCKED（--write 永远清不掉）",
             proc.returncode == 1 and "not part of an exact chain" in proc.stdout,
             f"rc={proc.returncode} out={proc.stdout}")
    with tempfile.TemporaryDirectory() as td:
        proc = pending_delivery(td, [f"Change: - {sha('B\n')} scripts/parity/x.py"],
                                files=[("scripts/parity/x.py", "B\n")])
        case("Pending 但行都合法 → READY（不回退成假 BLOCKED）",
             proc.returncode == 0 and "BLOCKED 0 / READY 1" in proc.stdout,
             f"rc={proc.returncode} out={proc.stdout}")

    # 真装置的掩盖机制：read_events 先查评审字段，Pending 直接抛家族错误，
    # Change 行循环根本走不到；回填评审后同一条坏行才报出来。
    real_spec = importlib.util.spec_from_file_location(
        "real_parity_cf", HERE / "parity/check_frozen.py")
    REAL_PARITY = importlib.util.module_from_spec(real_spec)
    real_spec.loader.exec_module(REAL_PARITY)
    with tempfile.TemporaryDirectory() as td:
        root = Path(td)
        (root / "specs/parity/frozen").mkdir(parents=True)
        event = root / "specs/parity/frozen/20260101-m.md"

        def real_failures(review, row):
            event.write_text("Calibration-Evidence: e\n"
                             f"Independent-Reviews: {review}\n"
                             "Approved-By: human\nApproval-Basis: basis\n"
                             + row + "\n")
            _, failures = REAL_PARITY.read_events(root)
            return failures

        masked = real_failures("Pending: 第二家未回", "Change: garbage-row")
        backfilled = real_failures("GLM: r1; MiniMax: r2", "Change: garbage-row")
        case("真实装置在 Change 行之前就因 Pending 报错（掩盖的来源）",
             len(masked) == 1 and "requires two distinct families" in masked[0],
             f"{masked}")
        case("真实装置回填评审后报 invalid Change row（预检必须提前拦）",
             any("invalid Change row" in item for item in backfilled), f"{backfilled}")

    # ----------------------------------- P1-D 真装置：已删除成员的合法交付
    SPEC_DOCS = ("README.md", "acceptance.md", "normalization.md",
                 "accepted-diffs.md", "multimodal.md")

    def real_parity_delivery(td, chain):
        root = Path(td)
        init_repo(root)
        (root / "scripts/parity").mkdir(parents=True)
        (root / "specs/parity").mkdir(parents=True)
        shutil.copyfile(HERE / "parity/check_frozen.py",
                        root / "scripts/parity/check_frozen.py")
        write(root, "scripts/parity/extra.py", "EXTRA = 1\n")
        write(root, "scripts/parity/helper.py", "HELPER = 1\n")
        for name in SPEC_DOCS:
            write(root, f"specs/parity/{name}", f"# {name}\n")
        write(root, "specs/parity/FROZEN.md",
              "# seed\n\n"
              f"| `README.md` | `{sha('# README.md\n')}` | generated | frozen/ |\n")
        commit(root, "seed")
        subprocess.run([sys.executable, "-I",
                        str(root / "scripts/parity/check_frozen.py"), "--write"],
                       cwd=str(root), check=True, capture_output=True, timeout=TIMEOUT)
        commit(root, "base")
        base = git(root, "rev-parse", "HEAD")
        git(root, "rm", "-q", "scripts/parity/extra.py")
        old = sha("EXTRA = 1\n") if chain else sha("WRONG\n")
        write(root, "specs/parity/frozen/20260101-del.md",
              "Calibration-Evidence: e\n"
              "Independent-Reviews: GLM: r1; MiniMax: r2\n"
              "Approved-By: human\nApproval-Basis: basis\n"
              f"Change: {old} - scripts/parity/extra.py\n")
        commit(root, "delivery")
        return run_tool("--root", str(root), "--base", base)

    with tempfile.TemporaryDirectory() as td:
        proc = real_parity_delivery(td, chain=True)
        case("真装置 + 已删除成员 + Change: <old> - 链 → READY（旧实现假 BLOCKED）",
             proc.returncode == 0 and "BLOCKED 0 / READY 1" in proc.stdout,
             f"rc={proc.returncode} out={proc.stdout}")
    with tempfile.TemporaryDirectory() as td:
        proc = real_parity_delivery(td, chain=False)
        case("真装置 + 已删除成员 + 错链 → BLOCKED",
             proc.returncode == 1 and "BLOCKED 1 / READY 0" in proc.stdout,
             f"rc={proc.returncode} out={proc.stdout}")

    # ------------------------------- P1-D 后半：中止报文要能重新证明登记两半
    APPARATUS_STUB = "import json\nprint(json.dumps({}))\n" + PARITY_MANIFEST
    with tempfile.TemporaryDirectory() as td:
        root = Path(td)
        init_repo(root)
        write(root, "scripts/parity/check_frozen.py", APPARATUS_STUB)
        write(root, "scripts/parity/x.py", "A\n")
        write(root, "specs/parity/FROZEN.md",
              "# fake FROZEN\n\n"
              f"{sha('A\n')}  ../../scripts/parity/x.py\n")
        commit(root, "base")
        base = git(root, "rev-parse", "HEAD")
        write(root, "scripts/parity/x.py", "B\n")
        write(root, "specs/parity/frozen/20260101-r.md",
              f"Change: {sha('A\n')} {sha('B\n')} scripts/parity/x.py\n" + EVENT_FIELDS)
        commit(root, "delivery")
        edges, _, issues = cfd.event_evidence(root, "parity", base)
        gaps = cfd.registration_gaps(root, "parity", base, edges, [])
        case("registration_gaps：链齐全时为空（中止报文可折算）",
             not issues and gaps == [], f"issues={issues} gaps={gaps}")
        # 同一棵树也是 P0-A 的前置条件：名字已登记、链齐全时 classify 会走
        # LACKS_CHAIN 分支并返回 ok，所以「原文 ok / 带尾巴 block」能区分
        # fullmatch 与 match（裸临时目录会让 registrable() 返回 None，
        # 两种匹配都落进 block，负控杀不死）。
        ctx = {"root": root, "kind": "parity", "mb": base, "edges": edges,
               "pending": set(), "dropped": set()}
        clean = "scripts/parity/x.py: " + LACKS_CHAIN_MSG
        case("lack-of-chain 原文（已登记、链齐全）→ ok",
             cfd.classify(ctx, clean)[0] == "ok", f"{cfd.classify(ctx, clean)}")
        case("lack-of-chain + 未知尾巴 → block（.match 会放行）",
             cfd.classify(ctx, clean + " and more")[0] == "block",
             f"{cfd.classify(ctx, clean + ' and more')}")
    with tempfile.TemporaryDirectory() as td:
        root = Path(td)
        init_repo(root)
        write(root, "scripts/parity/check_frozen.py", APPARATUS_STUB)
        write(root, "scripts/parity/x.py", "A\n")
        write(root, "specs/parity/FROZEN.md",
              "# fake FROZEN\n\n"
              f"{sha('A\n')}  ../../scripts/parity/x.py\n")
        commit(root, "base")
        base = git(root, "rev-parse", "HEAD")
        write(root, "scripts/parity/x.py", "B\n")
        commit(root, "delivery")
        gaps = cfd.registration_gaps(root, "parity", base, [], [])
        case("registration_gaps：改字节无链 → 非空（中止报文不得折算）",
             bool(gaps), f"gaps={gaps}")
    with tempfile.TemporaryDirectory() as td:
        # 只有 append-only 这一半能报出缺口：登记行全都可证（x.py 在交付里没动），
        # 变的是 merge-base 里已有的历史事件文件。少了 base_event_issues 这一半，
        # 中止报文就会被折算成可接受。
        root = Path(td)
        init_repo(root)
        write(root, "scripts/parity/check_frozen.py", APPARATUS_STUB)
        write(root, "scripts/parity/x.py", "A\n")
        write(root, "specs/parity/FROZEN.md",
              "# fake FROZEN\n\n"
              f"{sha('A\n')}  ../../scripts/parity/x.py\n")
        write(root, "specs/parity/frozen/20260101-old.md",
              f"Change: - {sha('A\n')} scripts/parity/x.py\n" + EVENT_FIELDS)
        commit(root, "base")
        base = git(root, "rev-parse", "HEAD")
        write(root, "specs/parity/frozen/20260101-old.md",
              f"Change: - {sha('A\n')} scripts/parity/x.py\n" + EVENT_FIELDS + "\nedited\n")
        commit(root, "delivery")
        gaps = cfd.registration_gaps(root, "parity", base, [], [])
        case("registration_gaps：历史事件被改写 → 非空（append-only 那一半）",
             any("append-only" in gap for gap in gaps), f"gaps={gaps}")

    # ================================================ fix-r2（codex / GLM）
    # P0-1：装置发现在钉 HEAD 之前读可变工作树 → 发现阶段的 A→B→A 漏检。
    # 垫片只把「工具第一次 rev-parse --verify HEAD」那一刻的树切回 A，
    # 此前的文件系统读看到的是 B（少了 parity 的 FROZEN.md）。
    GIT_SHIM = '''#!/usr/bin/env python3
import os, subprocess, sys
args = sys.argv[1:]
done = os.environ['CFD_SHIM_DONE']
if (not os.path.exists(done) and 'rev-parse' in args
        and '--verify' in args and 'HEAD' in args):
    open(done, 'w').close()
    subprocess.run([os.environ['CFD_SHIM_GIT'], '-C', os.environ['CFD_SHIM_ROOT'],
                    'checkout', '-q', os.environ['CFD_SHIM_A']],
                   check=False, capture_output=True)
os.execv(os.environ['CFD_SHIM_GIT'], [os.environ['CFD_SHIM_GIT']] + args)
'''
    UNCHAINED = "scripts/parity/x.py: " + LACKS_CHAIN_MSG
    with tempfile.TemporaryDirectory() as td:
        root = Path(td) / "repo"
        root.mkdir()
        init_repo(root)
        parity_src = ("import json\n"
                      "print(json.dumps({'status': 'FAIL', 'failures': "
                      + repr([UNCHAINED]) + "}))\n" + PARITY_MANIFEST)
        gw_src = ("import json\nprint(json.dumps({'status': 'PASS', 'failures': []}))\n"
                  + gw_source())
        write(root, "scripts/parity/check_frozen.py", parity_src)
        write(root, "scripts/gateway-parity/common.py", GW_COMMON)
        write(root, "scripts/gateway-parity/check_frozen.py", gw_src)
        write(root, "scripts/parity/x.py", "A\n")
        write(root, "specs/parity/FROZEN.md",
              "# fake FROZEN\n\n"
              f"{sha('A\n')}  ../../scripts/parity/x.py\n"
              f"{sha(parity_src)}  ../../scripts/parity/check_frozen.py\n")
        write(root, "specs/gateway-parity/FROZEN.md",
              "# fake FROZEN\n\n"
              f"{sha(gw_src)}  ../../scripts/gateway-parity/check_frozen.py\n")
        commit(root, "base")
        git(root, "update-ref", "refs/remotes/origin/main", git(root, "rev-parse", "HEAD"))
        write(root, "scripts/parity/x.py", "B\n")   # 改字节、无 Change 链
        commit(root, "delivery")
        head_a = git(root, "rev-parse", "HEAD")
        git(root, "rm", "-q", "specs/parity/FROZEN.md")   # B：发现阶段看不到 parity
        commit(root, "B: parity apparatus gone")
        head_b = git(root, "rev-parse", "HEAD")
        git(root, "checkout", "-q", head_b)               # 起跑时停在 B

        shim_dir = Path(td) / "shim"
        shim_dir.mkdir()
        shim = shim_dir / "git"
        shim.write_text(GIT_SHIM)
        shim.chmod(0o755)
        env = dict(os.environ)
        env.update(PATH=str(shim_dir) + os.pathsep + env["PATH"],
                   CFD_SHIM_DONE=str(Path(td) / "shim-done"),
                   CFD_SHIM_GIT=shutil.which("git"),
                   CFD_SHIM_ROOT=str(root), CFD_SHIM_A=head_a)
        proc = subprocess.run([sys.executable, "-I", str(TOOL),
                               "--root", str(root), "--base", "origin/main"],
                              capture_output=True, text=True, timeout=TIMEOUT, env=env)
        case("发现阶段 A→B→A → 装置集合取自钉住提交（parity 仍被审，旧实现假 READY）",
             proc.returncode == 1 and "=== parity === BLOCKED" in proc.stdout
             and "BLOCKED 1 / READY 1" in proc.stdout,
             f"rc={proc.returncode} out={proc.stdout}")
        case("发现阶段 A→B→A → 运行结束仍把树留在 A（不是被发现的 B）",
             git(root, "rev-parse", "HEAD") == head_a,
             f"head={git(root, 'rev-parse', 'HEAD')[:12]} a={head_a[:12]}")

    # P0-2：``A2 apparatus missing FROZEN hash:`` 的空列表 / 纯分隔符不得判可消除。
    with tempfile.TemporaryDirectory() as td:
        root = Path(td)
        init_repo(root)
        write(root, "seed.txt", "S\n")
        commit(root, "base")
        base = git(root, "rev-parse", "HEAD")
        ctx = {"root": root, "kind": "gateway-parity", "mb": base, "edges": [],
               "pending": set(), "dropped": set(), "snapshot": None}
        for label, message in (("空项", "A2 apparatus missing FROZEN hash: ,"),
                               ("纯空白", "A2 apparatus missing FROZEN hash:  "),
                               ("只有分隔符", "A2 apparatus missing FROZEN hash: , ,")):
            verdict = cfd.classify(ctx, message)
            case(f"A2 报文 {label}（空 hash 列表）→ block",
                 verdict[0] == "block", f"{verdict}")
        normal = cfd.classify(
            ctx, "A2 apparatus missing FROZEN hash: scripts/gateway-parity/new.py")
        case("A2 报文带真实名字、无链 → block（不是被空列表修复带偏的假 ok）",
             normal[0] == "block", f"{normal}")
        garbled = cfd.classify(
            ctx, "A2 apparatus missing FROZEN hash: scripts/gateway-parity/new file.py")
        case("A2 报文含非路径项（空格）→ block 且报 malformed（整串校验不放松）",
             garbled[0] == "block" and "malformed" in (garbled[1] or ""),
             f"{garbled}")

    # P1-G：parity 删 spec 文档时报裸名（表格键），掉行集合是 specs/parity/…。
    def parity_spec_drop(td, keep_in_inventory):
        root = Path(td)
        init_repo(root)
        names = ("README.md", "multimodal.md") if keep_in_inventory else ("README.md",)
        message = ("multimodal.md: [Errno 2] No such file or directory: "
                   "'specs/parity/multimodal.md'")
        src = ("import json\nprint(json.dumps({'status': 'FAIL', 'failures': "
               + repr([message]) + "}))\n"
               "\n\ndef manifest_inventory(root):\n"
               f"    names = {names!r}\n"
               "    return {name: name for name in names}\n")
        write(root, "scripts/parity/check_frozen.py", src)
        write(root, "specs/parity/multimodal.md", "# multimodal\n")
        write(root, "specs/parity/FROZEN.md",
              "# fake FROZEN\n\n"
              f"{sha(src)}  ../../scripts/parity/check_frozen.py\n"
              f"| `multimodal.md` | `{sha('# multimodal\n')}` | 2026-01-01 | seed |\n")
        commit(root, "base")
        git(root, "update-ref", "refs/remotes/origin/main", git(root, "rev-parse", "HEAD"))
        git(root, "rm", "-q", "specs/parity/multimodal.md")
        write(root, "specs/parity/frozen/20260101-m.md",
              f"Change: {sha('# multimodal\n')} - specs/parity/multimodal.md\n"
              + EVENT_FIELDS)
        commit(root, "delivery")
        return run_tool("--root", str(root), "--base", "origin/main")

    with tempfile.TemporaryDirectory() as td:
        proc = parity_spec_drop(td, keep_in_inventory=False)
        case("真删 spec 文档 + Change: <old> - 链 → READY（裸名报文，旧实现假 BLOCKED）",
             proc.returncode == 0 and "BLOCKED 0 / READY 1" in proc.stdout,
             f"rc={proc.returncode} out={proc.stdout}")
    with tempfile.TemporaryDirectory() as td:
        proc = parity_spec_drop(td, keep_in_inventory=True)
        case("删了仍在清单里的 spec 文档 → BLOCKED（归一不放过不该删的）",
             proc.returncode == 1 and "BLOCKED 1 / READY 0" in proc.stdout,
             f"rc={proc.returncode} out={proc.stdout}")

    # P1-C1：ui 的 ``unexpected registered file`` 报的是快照根下的绝对路径。
    with tempfile.TemporaryDirectory() as td:
        root = Path(td) / "repo"
        root.mkdir()
        init_repo(root)
        write(root, "scripts/ui-server-parity/apparatus.py", UI_APPARATUS)
        write(root, "scripts/ui-server-parity/check_frozen.py", UI_FROZEN)
        write(root, "rust-toolchain.toml", "[toolchain]\n")
        write(root, "specs/ui-server-parity/FROZEN.md",
              "# fake FROZEN\n\n"
              f"{sha('[toolchain]\n')}  ../../rust-toolchain.toml\n"
              f"{sha(UI_FROZEN)}  ../../scripts/ui-server-parity/check_frozen.py\n"
              f"{sha(UI_APPARATUS)}  ../../scripts/ui-server-parity/apparatus.py\n")
        commit(root, "base")
        base = git(root, "rev-parse", "HEAD")
        git(root, "rm", "-q", "rust-toolchain.toml")
        write(root, "specs/ui-server-parity/frozen/20260101-t.md",
              f"Change: {sha('[toolchain]\n')} - rust-toolchain.toml\n" + EVENT_FIELDS)
        commit(root, "delivery")
        edges, _, _ = cfd.event_evidence(root, "ui-server-parity", base)
        snapshot = Path(td) / "cfd-snapshot-fake" / "tree"
        ctx = {"root": root, "kind": "ui-server-parity", "mb": base, "edges": edges,
               "pending": set(), "dropped": {"rust-toolchain.toml"}, "snapshot": snapshot}
        reported = f"unexpected registered file: {snapshot}/rust-toolchain.toml"
        case("ui 报文用快照绝对路径 → ok（旧实现按被审树归一返 None 假 BLOCKED）",
             cfd.classify(ctx, reported)[0] == "ok", f"{cfd.classify(ctx, reported)}")
        case("ui 报文用被审树绝对路径 → 仍 ok",
             cfd.classify(ctx, f"unexpected registered file: {root}/rust-toolchain.toml")[0]
             == "ok")
        outside = cfd.classify(ctx, "unexpected registered file: /nowhere/rust-toolchain.toml")
        case("ui 报文指向两个根之外的绝对路径 → block",
             outside[0] == "block", f"{outside}")

    # P1-C2：写范围 —— 子装置运行窗口内新增的 worktree 记账**无法归属**（git
    # 不记录创建者，兄弟会话并发建树看起来一模一样），所以只报诊断 note，
    # 不再假 BLOCK；读不出记账才是阻塞。
    with tempfile.TemporaryDirectory() as td, tempfile.TemporaryDirectory() as extra:
        target = Path(extra) / "sneaky"
        payload = {"status": "PASS", "failures": []}
        leak = ("import subprocess\n"
                f"subprocess.run(['git', 'worktree', 'add', '--detach', {str(target)!r},\n"
                "                'HEAD'], capture_output=True)\n")
        # parity 夹具会自动把 ``PARITY_MANIFEST`` 追加到源尾，清单可解析、报文
        # 只吐一份；FROZEN.md 由 ``fake_apparatus`` 按同一份 body 钉住。
        proc = delivery(Path(td), "parity", payload, source=leak)
        case("子装置窗口内新增 worktree → 诊断 note（无法归属，不假 BLOCK）",
             proc.returncode == 0
             and "appeared while an apparatus ran" in proc.stdout
             and "cannot be attributed" in proc.stdout,
             f"rc={proc.returncode} out={proc.stdout}")

    # P1（codex r2b-3）：窗口采样读不出记账时不得静默跳过验证。
    case("window_verdict：前后任一侧读不出 → block（不静默跳过）",
         cfd.window_verdict(None, set())[0] is not None
         and cfd.window_verdict({'/a'}, None)[0] is not None)
    wblock, wnote = cfd.window_verdict({'/a'}, {'/a', '/sibling'})
    case("window_verdict：无法归属的新增只记 note，不阻",
         wblock is None and '/sibling' in (wnote or ''), f"{wblock} / {wnote}")
    case("window_verdict：无变化 → 全 None",
         cfd.window_verdict({'/a'}, {'/a'}) == (None, None))

    # P1（codex r2b-1）：清理未真删净时 holder 必须留在登记里（atexit 可重试），
    # 且主流程要把它当阻塞，而不是把残留降级成「别人的 note」。
    case("holder_verdict：无残留 → None", cfd.holder_verdict([]) is None)
    case("holder_verdict：仍有 holder → 阻塞消息点名路径",
         "/kept" in (cfd.holder_verdict([(Path('/repo'), Path('/kept'))]) or ''))

    # P1-C2（续）：SIGTERM 落在快照存活期间也必须清干净（atexit + 信号转
    # SystemExit）。走真实入口 ``main()``：子装置故意 sleep 30 s，父进程看到
    # 属于本仓的 ``cfd-snapshot-*`` 后给工具发 SIGTERM，再核退出码与泄漏
    # （快照目录 / .git/worktrees 记账）。直接调 install_sigterm_cleanup 的写法
    # 杀不死「main 忘了装处理器」这一变异，所以必须走入口。
    with tempfile.TemporaryDirectory() as td:
        root = Path(td) / "repo"
        root.mkdir()
        init_repo(root)
        slow = ("import time\n"
                "print('started', flush=True)\n"
                "time.sleep(30)\n")
        fake_apparatus(root, "parity", {"status": "PASS", "failures": []}, slow)
        commit(root, "base")
        git(root, "update-ref", "refs/remotes/origin/main", git(root, "rev-parse", "HEAD"))
        before = set(Path(tempfile.gettempdir()).glob("cfd-snapshot-*"))
        proc = subprocess.Popen([sys.executable, "-I", str(TOOL), "--root", str(root),
                                 "--base", "origin/main"],
                                stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
                                start_new_session=True)
        pgid = os.getpgid(proc.pid)
        holder = None
        deadline = time.time() + 60
        while time.time() < deadline and proc.poll() is None:
            listed_now = git(root, "worktree", "list", "--porcelain")
            fresh = [p for p in set(Path(tempfile.gettempdir()).glob("cfd-snapshot-*"))
                     - before if str(p / "tree") in listed_now]
            if fresh:
                holder = sorted(fresh)[0]
                break
            time.sleep(0.2)
        if holder is not None:
            proc.terminate()
        try:
            _, err = proc.communicate(timeout=60)
        except subprocess.TimeoutExpired:
            proc.kill()
            _, err = proc.communicate()
        try:
            os.killpg(pgid, signal.SIGKILL)   # 收掉还在 sleep 的子装置
        except (ProcessLookupError, PermissionError):
            pass
        listed = git(root, "worktree", "list", "--porcelain")
        leaked = holder is not None and (holder.exists()
                                         or str(holder / "tree") in listed)
        case("真实入口收到 SIGTERM → 退出码 143 且快照与 worktree 记账都不留",
             proc.returncode == 143 and holder is not None and not leaked,
             f"rc={proc.returncode} holder={holder} leaked={leaked} "
             f"err={err.strip()[-200:]}")

    # P2：``git worktree add`` 失败时 git 可能已经写下 .git/worktrees 记账，
    # 失败路径必须只清掉**本 holder** 的记账；仓级 ``git worktree prune`` 会顺手
    # 删掉别的会话的陈旧记账，不在本次写范围内。
    with tempfile.TemporaryDirectory() as td:
        root = Path(td) / "repo"
        root.mkdir()
        init_repo(root)
        write(root, "seed.txt", "S\n")
        commit(root, "base")
        head = git(root, "rev-parse", "HEAD")
        real_git = cfd.git
        entry = root / ".git" / "worktrees" / "tree"
        other = root / ".git" / "worktrees" / "someone-else"
        other.mkdir(parents=True)
        (other / "gitdir").write_text(str(Path(td) / "elsewhere" / ".git") + "\n")
        calls = []

        def failing_git(_root, *args, **_kwargs):
            calls.append(args)
            if args[:2] == ("rev-parse", "--git-common-dir"):
                return 0, ".git\n", ""
            if args[:2] == ("worktree", "add"):
                # git 先写记账再 checkout：失败时 entry 已存在，gitdir 指向本 holder。
                entry.mkdir(parents=True, exist_ok=True)
                (entry / "gitdir").write_text(str(Path(args[4]) / ".git") + "\n")
                return 1, "", "boom"
            if args[:2] == ("worktree", "remove"):
                return 1, "", "no such worktree"
            return 0, "", ""
        try:
            cfd.git = failing_git
            snapshot, why = cfd.create_snapshot(root, head)
        finally:
            cfd.git = real_git
        case("快照创建失败 → 返回 None 且只清本 holder 的 .git/worktrees 记账",
             snapshot is None and why == "boom" and not entry.exists()
             and other.exists()
             and ("worktree", "prune") not in calls
             and not cfd._SNAPSHOT_HOLDERS,
             f"snapshot={snapshot} why={why} entry={entry.exists()} "
             f"other={other.exists()} calls={calls} holders={cfd._SNAPSHOT_HOLDERS}")

    # P2：ROW_BARE 只认 64 位 hex，16 位截断哈希是散文不是清单行。
    with tempfile.TemporaryDirectory() as td:
        root = Path(td)
        init_repo(root)
        write(root, "specs/parity/FROZEN.md",
              "# fake FROZEN\n\n0123456789abcdef  specs/parity/x.md\n")
        commit(root, "base")
        rows = cfd.manifest_rows(root, "parity", git(root, "rev-parse", "HEAD"))
        case("ROW_BARE：16 位 hex 行不算清单行（真装置也只认 64 位）",
             rows == set(), f"rows={rows}")

    # P2：ROW_TABLE 的收紧也要有负控（真 fullmatch 要求尾单元格与纯哈希反引号）。
    with tempfile.TemporaryDirectory() as td:
        root = Path(td)
        init_repo(root)
        write(root, "specs/parity/FROZEN.md",
              "# fake FROZEN\n\n"
              "| `scripts/parity/a.py` | `0123456789abcdef` |\n"
              "| `scripts/parity/b.py` | `0123456789abcdef junk` | note |\n"
              "| `scripts/parity/c.py` | `0123456789abcdef…` | note |\n")
        commit(root, "base")
        rows = cfd.manifest_rows(root, "parity", git(root, "rev-parse", "HEAD"))
        case("ROW_TABLE：缺尾单元格 / 哈希反引号带杂尾是散文，合法省略号行才算",
             rows == {"scripts/parity/c.py"}, f"rows={rows}")

    # P2：ROW_BARE 用 re.ASCII —— 真装置是 bytes 正则，CJK 行名在那边就是散文。
    with tempfile.TemporaryDirectory() as td:
        root = Path(td)
        init_repo(root)
        write(root, "specs/gateway-parity/FROZEN.md",
              "# fake FROZEN\n\n" + sha("x") + "  中文/名.py\n")
        commit(root, "base")
        rows = cfd.manifest_rows(root, "gateway-parity", git(root, "rev-parse", "HEAD"))
        case("ROW_BARE：CJK 行名按真装置（bytes \\w）算散文", rows == set(), f"rows={rows}")

    # P1（boundary）：``ls-tree -z`` 按字节解析 —— 非 UTF-8 目录名被解码成 U+FFFD
    # 后 exists_at 会问一个不存在的名字，装置被静默跳过（假 READY 方向）。
    with tempfile.TemporaryDirectory() as td:
        root = Path(td)
        raw = b'specs/caf\xe9'
        kind = b'caf\xe9'.decode('utf-8', 'surrogateescape')
        real_bytes, real_exists = cfd.git_bytes, cfd.exists_at
        seen = []
        cfd.git_bytes = lambda *_a, **_k: (0, b'040000 tree ' + b'0' * 40 + b'\t' + raw
                                           + b'\0', b'')
        cfd.exists_at = lambda _r, _c, path: (seen.append(path), True)[1]
        try:
            found, invalid = cfd.discover(root, 'f' * 40)
        finally:
            cfd.git_bytes, cfd.exists_at = real_bytes, real_exists
        case("discover 按字节读 ls-tree：非 UTF-8 目录名不被静默跳过",
             found == [(kind, root / 'scripts' / kind / 'check_frozen.py')]
             and invalid == []
             and all('�' not in path for path in seen),
             f"found={found} invalid={invalid} seen={seen}")

    # P1（codex）：runner 查询自身出错（非 1）不能当成「没有 runner」——否则
    # 普通文件顶替的装置会被跳过。1 是 git 的「该树里没有这个路径」（2.54 实测），
    # 其余非零是查询失败。
    with tempfile.TemporaryDirectory() as td:
        root = Path(td)
        real_git = cfd.git
        try:
            for code, want in ((128, None), (1, False)):
                cfd.git = scripted_git([(0, "deadbeef", ""), (code, "", "boom")])
                got = cfd.exists_at(root, "deadbeef",
                                    "scripts/x/check_frozen.py")
                case(f"exists_at：git 退出码 {code} → {want!r}",
                     got is want, f"code={code} got={got!r}")
        finally:
            cfd.git = real_git

    # P2（GLM sibling）：同源的 base 事件存在性查询没跟着立规矩——任何非零
    # 都折进「历史事件被改写」，瞬时 git 故障会假 BLOCK 且文案诬指 append-only。
    with tempfile.TemporaryDirectory() as td:
        root = Path(td)
        real_git = cfd.git
        try:
            cfd.git = scripted_git([
                (0, "100644 blob deadbeef\tspecs/fake/frozen/20260101-e1.md\0", ""),
                (0, "", ""),        # pinned_head()^{commit} 解析成功
                (128, "", "boom"),
            ])
            issues = cfd.base_event_issues(root, "fake", "HEAD")
        finally:
            cfd.git = real_git
        case("base_event_issues：rev-parse 非 1 退出码 → 报「查不出」而非「事件被改写」",
             len(issues) == 1 and "cannot tell whether the base event survives"
             in issues[0], f"{issues}")
        real_git = cfd.git
        try:
            cfd.git = scripted_git([
                (0, "100644 blob deadbeef\tspecs/fake/frozen/20260101-e1.md\0", ""),
                (128, "", "bad HEAD"),
            ])
            issues = cfd.base_event_issues(root, "fake", "HEAD")
        finally:
            cfd.git = real_git
        case("base_event_issues：pinned rev 解析不出 → 报「解析不了」而非「事件被改写」",
             len(issues) == 1 and "cannot resolve" in issues[0], f"{issues}")

    # P0（两家独立撞上）：``specs/<alias>`` 是符号链接（120000 blob）时，discover
    # 以前按「不是 tree」静默 continue：该装置根本不跑，父预检报 READY 而子装置
    # 明明是 FAIL。目录型非 tree 条目（符号链接 / 子模块）一律 BLOCK，并在报告里
    # 点名条目与模式；普通文件（specs/README.md）不算别名，另见下方正控。
    with tempfile.TemporaryDirectory() as td:
        root = Path(td)
        init_repo(root)
        fake_apparatus(root, "parity", {"status": "PASS", "failures": []})
        write(root, "seed.txt", "S\n")
        commit(root, "base")
        git(root, "update-ref", "refs/remotes/origin/main", git(root, "rev-parse", "HEAD"))
        (root / "specs" / "alias").symlink_to("parity")
        commit(root, "delivery")
        mode = git(root, "ls-tree", "HEAD", "--", "specs/alias").split()[0]
        proc = run_tool("--root", str(root), "--base", "origin/main")
        case("specs/<alias> 是符号链接（120000 blob）→ BLOCKED 并点名 alias",
             mode == "120000" and proc.returncode == 1
             and "=== discovery === BLOCKED" in proc.stdout
             and "specs/alias" in proc.stdout and "120000" in proc.stdout
             and "=== parity === READY" not in proc.stdout
             and "BLOCKED 1 / READY 0" in proc.stdout,
             f"mode={mode} rc={proc.returncode} out={proc.stdout}")

    # P1（sibling）：``specs/<alias>`` 被普通文件顶替（坏合并删目录留文件）时，
    # ``scripts/<alias>/check_frozen.py`` 还在；旧实现按普通文件静默 continue，
    # 这个装置就整只不跑 —— 与符号链接同一条假 READY 路径。
    with tempfile.TemporaryDirectory() as td:
        root = Path(td)
        init_repo(root)
        two_apparatus(root)
        commit(root, "base")
        git(root, "update-ref", "refs/remotes/origin/main",
            git(root, "rev-parse", "HEAD"))
        shutil.rmtree(root / "specs" / "parity")
        write(root, "specs/parity", "not a directory\n")
        commit(root, "delivery")
        mode = git(root, "ls-tree", "HEAD", "--", "specs/parity").split()[0]
        runner = (root / "scripts" / "parity" / "check_frozen.py").exists()
        proc = run_tool("--root", str(root), "--base", "origin/main")
        case("specs/<alias> 被普通文件顶替（runner 仍在）→ BLOCKED 并点名",
             mode == "100644" and runner and proc.returncode == 1
             and "=== discovery === BLOCKED" in proc.stdout
             and "specs/parity" in proc.stdout and "100644" in proc.stdout
             and "=== parity === READY" not in proc.stdout
             and "=== gateway-parity === READY" not in proc.stdout
             and "BLOCKED 1 / READY 0" in proc.stdout,
             f"mode={mode} runner={runner} rc={proc.returncode} out={proc.stdout}")

    # P2（两家评审）：子模块（160000 commit）是另一条目录型非 tree 路径，也要有
    # 负控，不能只测符号链接。
    with tempfile.TemporaryDirectory() as td:
        root = Path(td)
        init_repo(root)
        two_apparatus(root)
        commit(root, "base")
        base = git(root, "rev-parse", "HEAD")
        git(root, "update-ref", "refs/remotes/origin/main", base)
        git(root, "update-index", "--add", "--cacheinfo",
            f"160000,{base},specs/sub")
        git(root, "commit", "-qm", "delivery")
        mode = git(root, "ls-tree", "HEAD", "--", "specs/sub").split()[0]
        proc = run_tool("--root", str(root), "--base", "origin/main")
        case("specs/<alias> 是子模块（160000 commit）→ BLOCKED 并点名",
             mode == "160000" and proc.returncode == 1
             and "=== discovery === BLOCKED" in proc.stdout
             and "specs/sub" in proc.stdout and "160000" in proc.stdout
             and "BLOCKED 1 / READY 0" in proc.stdout,
             f"mode={mode} rc={proc.returncode} out={proc.stdout}")

    # 正控：``specs/README.md`` 是普通文件（100644 blob），不可能装 FROZEN.md，
    # 收紧后不得把它当非法条目把正常路径打红。
    with tempfile.TemporaryDirectory() as td:
        root = Path(td)
        init_repo(root)
        fake_apparatus(root, "parity", {"status": "PASS", "failures": []})
        write(root, "specs/README.md", "specs index\n")
        commit(root, "base")
        git(root, "update-ref", "refs/remotes/origin/main", git(root, "rev-parse", "HEAD"))
        proc = run_tool("--root", str(root), "--base", "origin/main")
        case("specs/README.md（普通 100644 文件）不算装置别名 → parity 仍 READY",
             proc.returncode == 0 and "=== parity === READY" in proc.stdout
             and "BLOCKED 0 / READY 1" in proc.stdout,
             f"rc={proc.returncode} out={proc.stdout}")

    # P2（sibling）：钉住提交里没有的装置目录（只在工作树里，未跟踪）不得被审 ——
    # 杀掉「discover 仍读文件系统」的变异体。
    with tempfile.TemporaryDirectory() as td:
        root = Path(td) / "repo"
        root.mkdir()
        init_repo(root)
        write(root, "seed.txt", "S\n")
        fake_apparatus(root, "parity", {"status": "PASS", "failures": []})
        commit(root, "base")
        git(root, "update-ref", "refs/remotes/origin/main", git(root, "rev-parse", "HEAD"))
        write(root, "specs/fake/FROZEN.md",
              "# fake FROZEN\n\n" + sha("S\n") + "  ../../seed.txt\n")
        write(root, "scripts/fake/check_frozen.py",
              "import json\nprint(json.dumps({'status': 'PASS', 'failures': []}))\n")
        proc = run_tool("--root", str(root), "--base", "origin/main")
        case("工作树里多出的未跟踪装置（钉住提交没有）不被审",
             "=== fake ===" not in proc.stdout and "=== parity ===" in proc.stdout
             and proc.returncode == 0,
             f"rc={proc.returncode} out={proc.stdout}")

    # P1（codex/GLM 两家独立撞上）：并发兄弟建 worktree 不得假 BLOCK，本进程
    # 留下的记账必须拦。
    case("scope_verdict：本进程快照记账残留 → block",
         cfd.scope_verdict({'/a'}, {'/a', '/snap/tree'}, '/snap/tree')[0] is not None)
    verdict_block, verdict_note = cfd.scope_verdict({'/a'}, {'/a', '/other/wt'}, None)
    case("scope_verdict：非本进程新增条目 → 只记 note 不阻",
         verdict_block is None and '/other/wt' in (verdict_note or ''),
         f"{verdict_block} / {verdict_note}")
    case("scope_verdict：worktree list 读不出 → block",
         cfd.scope_verdict(None, set(), None)[0] is not None)
    case("scope_verdict：无变化 → 全 None",
         cfd.scope_verdict({'/a'}, {'/a'}, '/x') == (None, None))
    with tempfile.TemporaryDirectory() as td:
        target = Path(td) / "snap"
        target.mkdir()
        link = Path(td) / "link"
        link.symlink_to(target)
        linked = cfd.scope_verdict(set(), {str(link)}, str(target))[0]
        case("scope_verdict：同一个目录的符号链接拼写仍算自家记账（→ block）",
             linked is not None, f"{linked}")

    # P1（codex）：清理被 SIGTERM 打断时 holder 必须仍在登记里，atexit 才能重试。
    with tempfile.TemporaryDirectory() as td:
        root = Path(td) / "repo"
        root.mkdir()
        init_repo(root)
        holder = Path(td) / "cfd-snapshot-x"
        (holder / "tree").mkdir(parents=True)
        real_git = cfd.git
        raised = kept = False
        cfd._SNAPSHOT_HOLDERS.append((root, holder))

        def interrupted(_root, *_args, **_kwargs):
            raise SystemExit(143)
        try:
            cfd.git = interrupted
            try:
                cfd.drop_holder(root, holder)
            except SystemExit:
                raised = True
            kept = (root, holder) in cfd._SNAPSHOT_HOLDERS
        finally:
            cfd.git = real_git
            cfd._SNAPSHOT_HOLDERS.clear()
        case("drop_holder 被 SystemExit 打断 → holder 仍登记（atexit 可重试）",
             raised and kept and holder.exists(), f"raised={raised} kept={kept}")

    # P1（codex r2b-1）：清理**报告成功但没真删净**（rmtree 静默失败）时，登记
    # 不得摘除 —— 否则 atexit 无从重试，残留还被 scope_verdict 当成别人的 note。
    with tempfile.TemporaryDirectory() as td:
        root = Path(td) / "repo"
        root.mkdir()
        init_repo(root)
        holder = Path(td) / "cfd-snapshot-y"
        (holder / "tree").mkdir(parents=True)
        real_git, real_rmtree = cfd.git, shutil.rmtree
        cfd._SNAPSHOT_HOLDERS.append((root, holder))

        def no_remove(_root, *args, **_kwargs):
            if args[:2] == ("worktree", "remove"):
                return 1, "", "no such worktree"
            if args[:2] == ("rev-parse", "--git-common-dir"):
                return 0, ".git\n", ""
            return 0, "", ""
        try:
            cfd.git = no_remove
            shutil.rmtree = lambda *_a, **_k: None      # 静默失败
            cleaned = cfd.drop_holder(root, holder)
            kept = (root, holder) in cfd._SNAPSHOT_HOLDERS
        finally:
            cfd.git, shutil.rmtree = real_git, real_rmtree
            cfd._SNAPSHOT_HOLDERS.clear()
        case("清理未真删净 → drop_holder 返回 False 且 holder 仍登记",
             cleaned is False and kept and holder.exists(),
             f"cleaned={cleaned} kept={kept}")

    # P1（codex r2b-1）：``gitdir`` 与 target 是同一目录的等价拼写（符号链接，
    # macOS 上 /tmp 与 /private/tmp 就是这种）时，本 holder 的记账仍要删掉。
    with tempfile.TemporaryDirectory() as td:
        root = Path(td) / "repo"
        root.mkdir()
        init_repo(root)
        real = Path(td) / "real-snap"
        real.mkdir()
        link = Path(td) / "link-snap"
        link.symlink_to(real)
        entry = root / ".git" / "worktrees" / "tree"
        entry.mkdir(parents=True)
        (entry / "gitdir").write_text(str(link / "tree" / ".git") + "\n")
        removed = cfd.drop_admin_entry(root, real / "tree")
        case("drop_admin_entry：等价拼写的 gitdir 仍匹配并删掉本 holder 记账",
             removed is True and not entry.exists(),
             f"removed={removed} exists={entry.exists()}")

    # P1（codex r2b-1）：common-dir 读不出时不得假装删干净。
    with tempfile.TemporaryDirectory() as td:
        root = Path(td) / "repo"
        root.mkdir()
        init_repo(root)
        real_git = cfd.git
        try:
            cfd.git = lambda *_a, **_k: (1, "", "boom")
            removed = cfd.drop_admin_entry(root, Path(td) / "snap")
        finally:
            cfd.git = real_git
        case("drop_admin_entry：记账读不出 → 返回 False（不当作已清）",
             removed is False, f"removed={removed}")

    # P1（两家独立撞上）：单条 ``.git/worktrees/<id>/gitdir`` 不可读时不能当成
    # 「已清」—— 旧实现 _recorded_gitdir 返回 None，与 wanted 不等，于是
    # admin_entry_present 报 False、drop_holder 摘了登记，而记账仍在盘上。
    with tempfile.TemporaryDirectory() as td:
        root = Path(td) / "repo"
        root.mkdir()
        init_repo(root)
        holder = Path(td) / "cfd-snapshot-z"
        target = holder / "tree"
        target.mkdir(parents=True)
        entry = root / ".git" / "worktrees" / "broken"
        entry.mkdir(parents=True)        # 没有 gitdir 文件 → read_text 抛 OSError
        cfd._SNAPSHOT_HOLDERS.append((root, holder))
        try:
            recorded = cfd._recorded_gitdir(entry)
            present = cfd.admin_entry_present(root, target)
            dropped = cfd.drop_holder(root, holder)
            kept = (root, holder) in cfd._SNAPSHOT_HOLDERS
        finally:
            cfd._SNAPSHOT_HOLDERS.clear()
        case("gitdir 不可读 → admin_entry_present 不得报 False、drop_holder 不摘登记",
             recorded is cfd._UNREADABLE_GITDIR and present is None
             and dropped is False and kept and entry.exists(),
             f"recorded={recorded!r} present={present!r} dropped={dropped!r} "
             f"kept={kept} entry_exists={entry.exists()}")

    # P1（boundary）：``gitdir`` 可读但为空 / 全是空白（非原子写入被截断）时，
    # 旧实现 _real_path('') 得到 cwd、与 wanted 不等 → 变成「证明不是我们的」，
    # drop_holder 照摘登记。相对路径不是坏数据：``git worktree add
    # --relative-paths``（git 2.54 实测写 ``../../../../wt/.git``）也合法，
    # 必须按条目目录解析，否则这类仓库的清理会被永久 BLOCK。
    with tempfile.TemporaryDirectory() as td:
        results = []
        for idx, payload in enumerate(("", "   \n", "/tmp/cfd-snapshot-abc/tr\n",
                                       "bad\x00path/.git\n", "file:///x/.git\n")):
            root = Path(td) / f"repo{idx}"
            root.mkdir()
            init_repo(root)
            holder = Path(td) / f"cfd-snapshot-e{idx}"
            target = holder / "tree"
            target.mkdir(parents=True)
            entry = root / ".git" / "worktrees" / f"tree{idx}"
            entry.mkdir(parents=True)
            (entry / "gitdir").write_text(payload)
            recorded = cfd._recorded_gitdir(entry)
            present = cfd.admin_entry_present(root, target)
            cfd._SNAPSHOT_HOLDERS.append((root, holder))
            try:
                dropped = cfd.drop_holder(root, holder)
                kept = (root, holder) in cfd._SNAPSHOT_HOLDERS
            finally:
                cfd._SNAPSHOT_HOLDERS.clear()
            results.append((payload.strip(), recorded is cfd._UNREADABLE_GITDIR,
                            present, dropped, kept, entry.exists()))
        case("gitdir 为空 / 空白 / 截断 / NUL / URL → 不可读，不报 False、不摘登记",
             all(row[1] and row[2] is None and row[3] is False and row[4]
                 and row[5] for row in results), f"{results}")

    # 反向：``://`` 也可能出现在合法目录名里，只有前导 scheme 才算非路径。
    with tempfile.TemporaryDirectory() as td:
        root = Path(td) / "repo"
        root.mkdir()
        init_repo(root)
        entry = root / ".git" / "worktrees" / "urlpath"
        entry.mkdir(parents=True)
        (entry / "gitdir").write_text(
            str(Path(td) / "cache://v1" / "wt" / ".git") + "\n")
        recorded = cfd._recorded_gitdir(entry)
        case("gitdir 路径中部含 ://（合法目录名）→ 仍可读，不误判不可读",
             recorded is not cfd._UNREADABLE_GITDIR, f"recorded={recorded!r}")
        # 单字母 scheme（`x://host/.git`）同样是 URL，正则不能要求 scheme 至少
        # 两个字符（codex P2）。
        (entry / "gitdir").write_text("x://host/.git\n")
        case("gitdir 单字母 scheme URL → 不可读",
             cfd._recorded_gitdir(entry) is cfd._UNREADABLE_GITDIR, "")

    with tempfile.TemporaryDirectory() as td:
        root = Path(td) / "repo"
        root.mkdir()
        init_repo(root)
        holder = Path(td) / "cfd-snapshot-rel"
        target = holder / "tree"
        target.mkdir(parents=True)
        entry = root / ".git" / "worktrees" / "tree"
        entry.mkdir(parents=True)
        relative = os.path.relpath(str(target / ".git"), str(entry))
        (entry / "gitdir").write_text(relative + "\n")
        recorded = cfd._recorded_gitdir(entry)
        present = cfd.admin_entry_present(root, target)
        cfd._SNAPSHOT_HOLDERS.append((root, holder))
        try:
            dropped = cfd.drop_holder(root, holder)
            kept = (root, holder) in cfd._SNAPSHOT_HOLDERS
        finally:
            cfd._SNAPSHOT_HOLDERS.clear()
        case("相对 gitdir（git --relative-paths）按条目目录解析 → 仍认作自家记账",
             recorded == cfd._real_path(target / ".git") and present is True
             and dropped is True and not kept and not entry.exists(),
             f"relative={relative!r} recorded={recorded!r} present={present!r} "
             f"dropped={dropped!r} kept={kept} entry_exists={entry.exists()}")

    # 上面那条用例用 os.path.relpath 手工合成 gitdir，与实现共享同一假设
    # （GLM boundary P2：若 git 的相对基不是 `.git/worktrees/<id>/`，两边会
    # 一致地错而测试仍绿）。这里驱动真实 git 落一次盘对拍。``--relative-paths``
    # 要 git ≥ 2.48，老 git 打 SKIP 并跳过——跳过不是通过。
    with tempfile.TemporaryDirectory() as td:
        root = Path(td) / "repo"
        root.mkdir()
        init_repo(root)
        write(root, "seed.txt", "S\n")
        commit(root, "base")
        real_target = Path(td) / "wt-real"
        made = subprocess.run(
            ["git", "-C", str(root), "worktree", "add", "--relative-paths",
             "--detach", str(real_target), "HEAD"],
            capture_output=True, text=True, timeout=TIMEOUT)
        if made.returncode != 0:
            err = (made.stderr or "").strip()
            # 只有「这个 git 不认识该选项」才允许跳过；磁盘满 / 权限 / 其它
            # 失败都是真失败，跳过会把「没验到」伪装成通过（codex P2）。
            unsupported = (("relative-paths" in err or "relative_paths" in err)
                           and ("unknown option" in err.lower()
                                or "unrecognized" in err.lower()
                                or err.startswith("usage:")))
            if unsupported:
                skips += 1
                print("SKIP  真实 git --relative-paths 建树（本机 git 不支持该选项）")
            else:
                case("真实 git --relative-paths 建树", False, err)
        else:
            entry = next(iter((root / ".git" / "worktrees").iterdir()))
            payload = (entry / "gitdir").read_text().strip()
            recorded = cfd._recorded_gitdir(entry)
            want = cfd._real_path(real_target / ".git")
            case("真实 git --relative-paths 写出的相对 gitdir → 解析到 <worktree>/.git",
                 recorded == want and not Path(payload).is_absolute(),
                 f"payload={payload!r} recorded={recorded!r} want={want!r}")

    # P2（codex）：可读匹配 + 不可读条目并存时，可读的那条就是证据 → True；
    # 只有持久坏条目 → None，且阻塞文案点名该条目路径（不再指认自家快照未清）。
    with tempfile.TemporaryDirectory() as td:
        root = Path(td) / "repo"
        root.mkdir()
        init_repo(root)
        target = Path(td) / "snap" / "tree"
        target.mkdir(parents=True)
        good = root / ".git" / "worktrees" / "tree"
        good.mkdir(parents=True)
        (good / "gitdir").write_text(str(target / ".git") + "\n")
        (root / ".git" / "worktrees" / "broken").mkdir()
        cfd._UNVERIFIED_ADMIN_ENTRIES.clear()
        present = cfd.admin_entry_present(root, target)
        case("可读匹配 + 不可读条目并存 → True（不因旁人坏条目阻塞）",
             present is True, f"present={present!r}")
        shutil.rmtree(good)
        cfd._UNVERIFIED_ADMIN_ENTRIES.clear()
        present = cfd.admin_entry_present(root, target)
        named = cfd.holder_verdict([(root, Path(td) / "snap")]) or ""
        cfd._UNVERIFIED_ADMIN_ENTRIES.clear()
        case("只有持久坏条目 → None，且阻塞文案点名该条目路径",
             present is None and "broken" in named,
             f"present={present!r} named={named!r}")

    # 短退避：一次重扫就能消掉的瞬态不可读（兄弟正在 add）不该报 None。
    with tempfile.TemporaryDirectory() as td:
        root = Path(td) / "repo"
        root.mkdir()
        init_repo(root)
        target = Path(td) / "snap" / "tree"
        target.mkdir(parents=True)
        real_scan = cfd._scan_admin
        calls = []

        def flaky(admin, wanted):
            calls.append(1)
            return (None, ["broken"]) if len(calls) == 1 else (False, [])

        cfd._scan_admin = flaky
        cfd._UNVERIFIED_ADMIN_ENTRIES.discard("broken")
        try:
            present = cfd.admin_entry_present(root, target)
        finally:
            cfd._scan_admin = real_scan
        case("瞬态不可读条目：退避重扫后按干净处理（不假 BLOCK、不入诊断集）",
             present is False and len(calls) == 2
             and "broken" not in cfd._UNVERIFIED_ADMIN_ENTRIES,
             f"present={present!r} calls={len(calls)} "
             f"named={sorted(cfd._UNVERIFIED_ADMIN_ENTRIES)}")

    # P1（codex）：两个根嵌套时取**最深**前缀，先命中被审树会把快照路径归一错。
    with tempfile.TemporaryDirectory() as td:
        outer, inner = Path(td) / "outer", Path(td) / "outer" / "inner"
        inner.mkdir(parents=True)
        ctx_nested = {"root": outer, "kind": "ui-server-parity", "snapshot": inner}
        nested = cfd.repo_relative_any(ctx_nested, str(inner / "a.md"))
        plain = cfd.repo_relative_any({"root": outer, "kind": "ui-server-parity"},
                                      str(outer / "a.md"))
        case("repo_relative_any：嵌套根取最深前缀，非嵌套不变",
             nested == "a.md" and plain == "a.md", f"nested={nested} plain={plain}")

    # P1（codex）：截断位置正好落在**更早的 `/.git` 组件**上时，前缀本身以
    # `/.git` 结尾，长度判据拦不住：TMPDIR 位于 `/x/.git/folder` 时，
    # `/x/.git/folder/h/tree/.git` 截成 `/x/.git` 仍「可读且以 /.git 结尾」，
    # 与 wanted 不等 → 被当成「证明不是我们的」→ 摘登记、残留当干净。
    # 判据：recorded 是 wanted 的严格祖先 → 只能是自家路径的撕裂写，按不可读。
    with tempfile.TemporaryDirectory() as td:
        root = Path(td) / "repo"
        root.mkdir()
        init_repo(root)
        holder = Path(td) / "parent" / ".git" / "folder" / "cfd-snapshot-torn"
        target = holder / "tree"
        target.mkdir(parents=True)
        entry = root / ".git" / "worktrees" / "treeTorn"
        entry.mkdir(parents=True)
        (entry / "gitdir").write_text(str(Path(td) / "parent" / ".git") + "\n")
        recorded = cfd._recorded_gitdir(entry)
        verdict = (cfd._admin_match(recorded, cfd._real_path(target / ".git"))
                   if recorded is not cfd._UNREADABLE_GITDIR else "unreadable")
        present = cfd.admin_entry_present(root, target)
        cfd._SNAPSHOT_HOLDERS.append((root, holder))
        try:
            dropped = cfd.drop_holder(root, holder)
            kept = (root, holder) in cfd._SNAPSHOT_HOLDERS
        finally:
            cfd._SNAPSHOT_HOLDERS.clear()
        case("gitdir 截断在更早的 /.git 组件（自家路径的严格祖先）→ 不当作旁人记账",
             verdict == "torn" and present is None and dropped is False
             and kept and entry.exists(),
             f"verdict={verdict} recorded={recorded!r} present={present!r} "
             f"dropped={dropped!r} kept={kept} entry_exists={entry.exists()}")
        case("_admin_match：等式 / 旁人 / 祖先 三分",
             cfd._admin_match("/a/tree/.git", "/a/tree/.git") == "ours"
             and cfd._admin_match("/a/other/.git", "/a/tree/.git") == "other"
             and cfd._admin_match("/a/.git", "/a/.git/x/tree/.git") == "torn",
             "ok")

    # 祖先若是**文件**，就是另一只真 worktree 的 gitdir（例如 TMPDIR 落在同仓
    # 另一只 worktree 里），不能判撕裂，否则那里每次运行都永久 BLOCK（GLM P3）。
    with tempfile.TemporaryDirectory() as td:
        outer = Path(td) / "wt"
        outer.mkdir()
        (outer / ".git").write_text("gitdir: /elsewhere\n")
        case("_admin_match：严格祖先是文件（真 worktree 的 gitdir）→ other",
             cfd._admin_match(str(outer / ".git"),
                              str(outer / "tmp" / "h" / "tree" / ".git")) == "other",
             "")

    # P2（GLM sibling）：`_still_there` 的 EACCES 分支零覆盖——stat 失败必须
    # 按「未证已删」处理，否则 holder 目录还在盘上却被摘登记（`Path.exists()`
    # 对 EACCES 也答 False，正是 drop_admin_entry 修过的同类洞）。
    with tempfile.TemporaryDirectory() as td:
        locked = Path(td) / "locked"
        holder = locked / "cfd-snapshot-stat"
        (holder / "tree").mkdir(parents=True)
        os.chmod(locked, 0o000)
        try:
            exists = holder.exists()
            still = cfd._still_there(holder)
        finally:
            os.chmod(locked, 0o700)
        case("holder 的 stat 失败（EACCES）→ _still_there 判未证已删（exists() 会答 False）",
             exists is False and still is True,
             f"exists={exists} still_there={still}")

    print(f"SUMMARY failed={fails} skipped={skips}")
    return 1 if fails else 0


if __name__ == "__main__":
    sys.exit(run())
