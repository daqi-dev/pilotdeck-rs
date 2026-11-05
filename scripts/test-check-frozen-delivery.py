#!/usr/bin/env python3
"""check-frozen-delivery.py 的用例（正控 + 负控）。

跑法：python3 -I scripts/test-check-frozen-delivery.py
用例都在临时 git 仓库里构造：一个 base 提交 + 一个交付提交。
负控必须让被测函数给出 False / block / 非零退出，不是「随便失败」。
"""

import hashlib
import importlib.util
import subprocess
import sys
import tempfile
from pathlib import Path

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
                               "scripts/parity/gone.py: hash change lacks exact "
                               "new event chain"])
        proc = delivery(Path(td), "parity", payload,
                        base_files=[("scripts/parity/gone.py", "OLD\n")],
                        removes=["scripts/parity/gone.py"])
        case("删除已登记成员、无删除链 → BLOCKED",
             proc.returncode == 1 and "BLOCKED 1 / READY 0" in proc.stdout,
             f"rc={proc.returncode} out={proc.stdout}")

    with tempfile.TemporaryDirectory() as td:
        payload = dict(parity_payload,
                       errors=[HASH_LIST,
                               "scripts/parity/gone.py: hash change lacks exact "
                               "new event chain"])
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
        # 运行期间工作树被改：两次 status 采样之间落盘的改动同样要阻塞。
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
        case("运行期间工作树被改 → BLOCKED",
             proc.returncode == 1 and "=== worktree === BLOCKED" in proc.stdout,
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
    case("函数体内同名局部变量不影响模块级清单（不误判二次绑定）",
         ui_registrable(UI_APPARATUS.replace(
             "SPEC_FILES = ('acceptance.md',)",
             "SPEC_FILES = ('acceptance.md',)\n"
             "def _local():\n    SPEC_FILES = ()\n    return SPEC_FILES\n"),
             UI_FROZEN) is not None)

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
    with tempfile.TemporaryDirectory() as td:
        # 子装置运行期间 HEAD 移动（并发 amend/commit）：证据与目标字节会跨两个
        # 提交，旧实现照样判 READY。装置源码在打印报告后提交一次。
        drift = ("import subprocess\n"
                 "subprocess.run(['git', 'commit', '--allow-empty', '-qm', 'drift'])\n")
        proc = delivery(Path(td), "ui-server-parity", {"verdict": "PASS", "errors": []},
                        source=drift)
        case("运行期间 HEAD 移动 → BLOCKED（快照不一致）",
             proc.returncode == 1 and "=== snapshot === BLOCKED" in proc.stdout,
             f"rc={proc.returncode} out={proc.stdout}")
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

    print(f"SUMMARY failed={fails}")
    return 1 if fails else 0


if __name__ == "__main__":
    sys.exit(run())
