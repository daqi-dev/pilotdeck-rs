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


def git(root, *args):
    return subprocess.run(["git", "-C", str(root), *args], capture_output=True,
                          text=True, check=True).stdout.strip()


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
                          capture_output=True, text=True)


def fake_apparatus(root, kind, payload):
    (root / f"specs/{kind}").mkdir(parents=True, exist_ok=True)
    (root / f"specs/{kind}/FROZEN.md").write_text("# fake FROZEN\n")
    (root / f"scripts/{kind}").mkdir(parents=True, exist_ok=True)
    (root / f"scripts/{kind}/check_frozen.py").write_text(
        "import json\nprint(json.dumps(" + repr(payload) + "))\n")


HASH_LIST = "hash list differs from generated inventory; run check_frozen.py --write"


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
        (root / "scripts/fake").mkdir(parents=True)
        (root / "scripts/fake/x.py").write_text("A\n")
        commit(root, "base")
        base = git(root, "rev-parse", "HEAD")
        (root / "scripts/fake/x.py").write_text("B\n")
        event = root / "specs/fake/frozen/20260101-e1.md"
        event.parent.mkdir(parents=True)
        event.write_text(f"Change: {sha('A\n')} {sha('B\n')} scripts/fake/x.py\n")
        commit(root, "delivery")

        edges, _, issues = cfd.event_evidence(root, "fake", base)
        case("event_evidence 收齐一条已提交的 Change 行",
             len(edges) == 1 and not issues, f"edges={edges} issues={issues}")
        case("单条边精确链通过",
             cfd.chain_backed(root, edges, "scripts/fake/x.py", base) is True)
        case("断链被拒（边指向别的 old）",
             cfd.chain_backed(root, [("e.md", "scripts/fake/x.py", sha("X\n"), sha("B\n"))],
                              "scripts/fake/x.py", base) is False)
        case("工作树未提交的字节不能补链",
             cfd.chain_backed(root, edges, "scripts/fake/y.py", base) is False)

        # 重复边：旧实现「找到一条 old==base 的边」就放行（假 PASS）。
        (root / "specs/fake/frozen/20260101-e2.md").write_text(
            f"Change: {sha('A\n')} {sha('B\n')} scripts/fake/x.py\n")
        commit(root, "duplicate edge")
        dup, _, _ = cfd.event_evidence(root, "fake", base)
        case("重复边被拒（旧实现会假 PASS）",
             len(dup) == 2 and cfd.chain_backed(root, dup, "scripts/fake/x.py", base) is False)

        # 未提交的事件文件不能当证据。
        (root / "specs/fake/frozen/20260101-e3.md").write_text(
            f"Change: {sha('A\n')} {sha('B\n')} scripts/fake/z.py\n")
        _, _, uncommitted = cfd.event_evidence(root, "fake", base)
        case("未提交的事件文件报阻塞而不是当证据",
             any("not committed" in issue for issue in uncommitted), f"{uncommitted}")

        # ---------------------------------------------------- classification
        ctx = {"root": root, "kind": "ui-server-parity", "mb": base,
               "edges": edges, "pending": set()}
        case("ui-server ../../ 旧 pin 路径可解析",
             cfd.existing_file(root, "ui-server-parity", "../../scripts/fake/x.py") is True)
        case("invalid path/hash ../../ 判 ok",
             cfd.classify(ctx, "invalid path/hash: ../../scripts/fake/x.py")[0] == "ok")
        case("unregistered/missing 有链背书判 ok",
             cfd.classify(ctx, "unregistered/missing file: scripts/fake/x.py")[0] == "ok")
        case("unregistered/missing 无此文件判 block",
             cfd.classify(ctx, "unregistered/missing file: scripts/fake/absent.py")[0] == "block")
        case("重复边下 unregistered/missing 判 block",
             cfd.classify(dict(ctx, edges=dup),
                          "unregistered/missing file: scripts/fake/x.py")[0] == "block")
        case("未提交文件不算 existing_file",
             cfd.existing_file(root, "parity", "../../scripts/fake/nope.py") is False)
        case("认不出的失败消息 fail-closed",
             cfd.classify(ctx, "totally unknown failure")[0] == "block")

    # ------------------------------------------- 首登记：parity / gateway
    def delivery(root, kind, payload, new_files, events):
        init_repo(root)
        fake_apparatus(root, kind, payload)
        (root / f"scripts/{kind}/keep.py").write_text("keep\n")
        commit(root, "base")
        git(root, "update-ref", "refs/remotes/origin/main", git(root, "rev-parse", "HEAD"))
        for name, text in new_files.items():
            (root / name).parent.mkdir(parents=True, exist_ok=True)
            (root / name).write_text(text)
        for name, text in events.items():
            (root / name).parent.mkdir(parents=True, exist_ok=True)
            (root / name).write_text(text)
        commit(root, "delivery")
        return run_tool("--root", str(root), "--base", "origin/main")

    parity_payload = {"hashes_consistent": False, "ok": False, "fully_registered": False,
                      "checked": {}, "notes": [], "errors": [HASH_LIST],
                      "unregistered_skipped": ["scripts/parity/new.py"],
                      "notice": "未登记文件暂跳过"}

    with tempfile.TemporaryDirectory() as td:
        proc = delivery(Path(td), "parity", parity_payload,
                        {"scripts/parity/new.py": "NEW\n"}, {})
        case("parity 新文件无事件链 → BLOCKED（旧实现假 READY）",
             proc.returncode == 1 and "BLOCKED 1 / READY 0" in proc.stdout,
             f"rc={proc.returncode}")

    with tempfile.TemporaryDirectory() as td:
        proc = delivery(Path(td), "parity", parity_payload,
                        {"scripts/parity/new.py": "NEW\n"},
                        {"specs/parity/frozen/20260101-p.md":
                         f"Change: - {sha('NEW\n')} scripts/parity/new.py\n"
                         "Calibration-Evidence: test fixture\n"
                         "Independent-Reviews: A <a@example.invalid>; B <b@example.invalid>\n"
                         "Approved-By: test\n"
                         "Approval-Basis: test fixture\n"})
        case("parity 新文件有 Change: - <hash> 事件链 → READY",
             proc.returncode == 0 and "BLOCKED 0 / READY 1" in proc.stdout,
             f"rc={proc.returncode} out={proc.stdout}")

    with tempfile.TemporaryDirectory() as td:
        proc = delivery(Path(td), "gateway-parity",
                        {"status": "FAIL", "failures": [HASH_LIST]},
                        {"scripts/gateway-parity/fixtures/new.json": "{}\n"}, {})
        case("gateway 新增清单成员无事件链 → BLOCKED",
             proc.returncode == 1 and "BLOCKED 1 / READY 0" in proc.stdout,
             f"rc={proc.returncode}")

    # ------------------------------------------------------- fail-closed
    with tempfile.TemporaryDirectory() as td:
        root = Path(td)
        proc = delivery(root, "parity", {"status": "FAIL", "failures": []}, {}, {})
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
