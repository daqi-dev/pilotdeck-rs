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


def fake_apparatus(root, kind, payload, source=""):
    body = "import json\nprint(json.dumps(" + repr(payload) + "))\n" + source
    write(root, f"scripts/{kind}/check_frozen.py", body)
    # 装置自身在 base 已有清单行：它改字节时走「改已登记文件」的路，不该被当成首登记。
    write(root, f"specs/{kind}/FROZEN.md",
          f"# fake FROZEN\n\n{sha(body)}  ../../scripts/{kind}/check_frozen.py\n")


GW_COMMON = "A2_SCRIPTS = ('check_frozen.py',)\n"

UI_APPARATUS = ("APPARATUS_PATHS = ('scripts/ui-server-parity', 'scripts/exp008', "
                "'specs/ui-server-parity')\n"
                "SPEC_FILES = ('acceptance.md',)\n"
                "TOOLCHAIN_FILES = ()\n")

# 真装置的另一半：真实登记范围来自 check_frozen.py 的 SPEC 与 apparatus_files()，
# 预检会与 apparatus.py 的 APPARATUS_PATHS 交叉核对，两边不一致即「不可判定」。
UI_FROZEN = ("\nSPEC = ROOT / 'specs/ui-server-parity'\n"
             "\n\ndef apparatus_files(root):\n"
             "    files = {Path(p) for p in scan(root / 'scripts/ui-server-parity')}\n"
             "    files.update(Path(p) for p in scan(root / 'scripts/exp008'))\n"
             "    return files\n")


def gw_source(*names):
    """假 gateway 装置源：与真装置同形的 ``manifest_inventory``。

    模板 ``f'scripts/gateway-parity/{name}'`` 遍历 ``A2_SCRIPTS``，a5-19 用内联
    元组模板，额外成员写成完整路径字面量，spec 名是裸名元组——预检按这些表达式
    重建清单，而不是把函数体里所有字符串常量都当成成员。
    """
    extra = "".join(f"    paths.append('scripts/gateway-parity/{name}')\n" for name in names)
    return ("\ndef manifest_inventory(root):\n"
            "    names = ('acceptance.md', 'normalization.md', 'accepted-diffs.md')\n"
            "    paths = [f'scripts/gateway-parity/{name}' for name in A2_SCRIPTS]\n"
            + extra +
            "    paths += [f'scripts/a5-19/{name}' for name in ('run.py',)]\n"
            "    return {name: name for name in names}\n")


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
        # 未跟踪草稿：真实闸门无感，--write 也不登记 → 预检不能误拦。
        payload = dict(parity_payload, unregistered_skipped=["scripts/parity/scratch.txt"])
        proc = delivery(Path(td), "parity", payload, dirty=[("scripts/parity/scratch.txt", "x\n")])
        case("未跟踪草稿不计首登记（不误拦）",
             proc.returncode == 0 and "BLOCKED 0 / READY 1" in proc.stdout,
             f"rc={proc.returncode}")

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
              "def manifest_inventory(root):\n"
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
              "def manifest_inventory(root):\n"
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
