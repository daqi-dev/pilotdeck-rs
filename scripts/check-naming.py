#!/usr/bin/env python3
"""命名规约机器检查（specs/README.md「忽略区目录名」+「worktree 与分支」）。

用法：
  python3 scripts/check-naming.py             # 全量（目录 + 本地分支名），存量按 baseline 冻结只减不增
  python3 scripts/check-naming.py --dirs-only # 只查目录名
  python3 scripts/check-naming.py --write-baseline  # 以当前违规集合重建豁免快照（收紧用）

规则（与 specs/README.md 同步维护，改一处必改另一处）：
  _build/ 顶层：wt-<issue号或阶段码>[-<短名>]  或  YYYYMMDD-HHMMSS[-<用途>]
  _tmp/  顶层：YYYYMMDD-<slug>
  分支名：<owner>/<内容>，owner ∈ {codex, docs, dev, chore, fix}；裸 main/release 豁免
  存量违规登记在 check-naming.baseline.txt，只减不增：清一项删一行，新增即 FAIL。
"""

import re
import subprocess
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
BASELINE = REPO / "scripts" / "check-naming.baseline.txt"

BUILD_DIR = re.compile(r"^(wt-[a-z0-9][a-z0-9-]*|\d{8}-\d{6}(-[a-z0-9][a-z0-9-]*)?)$")
TMP_DIR = re.compile(r"^\d{8}-[a-z0-9][a-z0-9-]*$")
TMP_EXEMPT = {"archive"}  # 历史豁免：只减不增
BRANCH_OWNER = re.compile(r"^(codex|docs|dev|chore|fix)/[a-z0-9][a-z0-9./_-]*$")
BRANCH_EXEMPT = {"main", "master", "release"}


def collect_bad() -> list[str]:
    bad = []
    for name, pat, exempt in (
        ("_build", BUILD_DIR, set()),
        ("_tmp", TMP_DIR, TMP_EXEMPT),
    ):
        d = REPO / name
        if not d.is_dir():
            continue
        for child in d.iterdir():
            if not child.is_dir():
                bad.append(f"{name}/ 裸文件: {child.relative_to(REPO)}")
            elif child.name not in exempt and not pat.match(child.name):
                bad.append(f"{name}/ 目录名不合规格式: {child.relative_to(REPO)}")
    try:
        out = subprocess.run(
            ["git", "branch", "--format", "%(refname:short)"],
            cwd=REPO, capture_output=True, text=True, check=True,
        ).stdout
    except Exception as e:  # noqa: BLE001 —— 无 git 或非仓库：目录检查仍有效
        print(f"跳过分支检查（{e}）", file=sys.stderr)
        out = ""
    for b in out.split():
        if b in BRANCH_EXEMPT:
            continue
        if not BRANCH_OWNER.match(b):
            bad.append(f"分支名 owner 不在白名单（codex|docs|dev|chore|fix）: {b}")
    return sorted(bad)


def load_baseline() -> set[str]:
    if BASELINE.exists():
        return {l.strip() for l in BASELINE.read_text().splitlines() if l.strip() and not l.startswith("#")}
    return set()


def main() -> int:
    bad = collect_bad()
    if "--dirs-only" in sys.argv:
        bad = [x for x in bad if not x.startswith("分支名")]
    if "--write-baseline" in sys.argv:
        BASELINE.write_text(
            "# 存量违规冻结快照（自动生成）——只减不增：清一项删一行，新增即 FAIL\n"
            + "\n".join(bad) + "\n"
        )
        print(f"baseline 已重建：{len(bad)} 项")
        return 0
    frozen = load_baseline()
    fresh = [x for x in bad if x not in frozen]
    gone = [x for x in frozen if x not in bad]
    if fresh:
        print("命名检查 FAIL（新增违规）：")
        for x in fresh:
            print(f"  - {x}")
        print("规范见 specs/README.md（忽略区目录名 / worktree 与分支）；存量豁免见 check-naming.baseline.txt")
        return 1
    if gone:
        print(f"baseline 中 {len(gone)} 项已消失，可运行 --write-baseline 收紧：")
        for x in gone:
            print(f"  - {x}")
    print(f"命名检查 PASS（存量冻结 {len(frozen) - len(gone)} 项）")
    return 0


if __name__ == "__main__":
    sys.exit(main())
