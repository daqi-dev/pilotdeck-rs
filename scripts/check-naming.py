#!/usr/bin/env python3
"""命名规约机器检查（specs/README.md「忽略区目录名」+「worktree 与分支」）。

用法：
  python3 scripts/check-naming.py             # 全量（目录 + 本地分支名），存量按 baseline 冻结只减不增
  python3 scripts/check-naming.py --dirs-only # 只查目录名（禁止与 --write-baseline 同用）
  python3 scripts/check-naming.py --write-baseline  # 收紧 baseline：新集合必须是旧集合的子集，否则拒绝

规则（与 specs/README.md 同步维护，改一处必改另一处）：
  _build/ 顶层：wt-<issue号或阶段码>[-<短名>]  或  YYYYMMDD-HHMMSS[-<用途>]（共享缓存白名单除外）
  _tmp/  顶层：YYYYMMDD-<slug>
  分支名：<owner>/<内容>（全小写），owner ∈ {codex, glm, deepseek, claude, docs, dev, chore, fix}；裸 main/release 豁免
  存量违规登记在 check-naming.baseline.txt，只减不增：清一项删一行（或 --write-baseline 收紧），新增即 FAIL。
"""

import re
import subprocess
import sys
from pathlib import Path

SCRIPT_DIR = Path(__file__).resolve().parent
BASELINE = SCRIPT_DIR / "check-naming.baseline.txt"

BUILD_DIR = re.compile(r"^(wt-[a-z0-9][a-z0-9-]*|\d{8}-\d{6}(-[a-z0-9][a-z0-9-]*)?)$")
TMP_DIR = re.compile(r"^\d{8}-[a-z0-9][a-z0-9-]*$")
TMP_EXEMPT = {"archive"}  # 历史豁免：只减不增
# 长期共享缓存/工具目录，非遗留违规（规约白名单）
BUILD_EXEMPT = {"cargo-home", "fe-parity"}
FILE_EXEMPT = {".DS_Store"}  # macOS 系统文件，随时再生，豁免
BRANCH_OWNER = re.compile(r"^(codex|glm|deepseek|claude|docs|dev|chore|fix)/[a-z0-9][a-z0-9./_-]*$")
BRANCH_EXEMPT = {"main", "master", "release"}


def repo_root() -> Path:
    """定位主仓根（worktree 里跑时 _build/_tmp 在主仓，不在 worktree）。"""
    try:
        out = subprocess.run(
            ["git", "rev-parse", "--path-format=absolute", "--git-common-dir"],
            capture_output=True, text=True, check=True,
        ).stdout.strip()
        return Path(out).parent
    except Exception:  # noqa: BLE001 —— 无 git 时退回脚本所在目录
        return SCRIPT_DIR.parent


def collect_bad(root: Path) -> tuple[list[str], list[str]]:
    """返回 (违规列表, 跳过原因列表)。"""
    bad, skipped = [], []
    for name, pat, exempt in (
        ("_build", BUILD_DIR, BUILD_EXEMPT),
        ("_tmp", TMP_DIR, TMP_EXPT := TMP_EXEMPT),
    ):
        d = root / name
        if not d.is_dir():
            skipped.append(f"{name}/ 不存在（CI/非主仓环境），跳过")
            continue
        for child in d.iterdir():
            if not child.is_dir():
                if child.name not in FILE_EXEMPT:
                    bad.append(f"{name}/ 裸文件: {child.relative_to(root)}")
            elif child.name not in exempt and not pat.match(child.name):
                bad.append(f"{name}/ 目录名不合规格式: {child.relative_to(root)}")
    try:
        out = subprocess.run(
            ["git", "for-each-ref", "refs/heads", "--format", "%(refname:short)"],
            capture_output=True, text=True, check=True, cwd=root,
        ).stdout
    except FileNotFoundError:
        skipped.append("无 git 可执行文件，跳过分支检查")
        out = ""
    for b in filter(None, out.splitlines()):
        if b in BRANCH_EXEMPT:
            continue
        if not BRANCH_OWNER.match(b):
            bad.append(f"分支名 owner 不在白名单（codex|glm|deepseek|claude|docs|dev|chore|fix）: {b}")
    return sorted(bad), skipped


def load_baseline() -> set[str]:
    if BASELINE.exists():
        return {l.strip() for l in BASELINE.read_text().splitlines() if l.strip() and not l.startswith("#")}
    return set()


def main() -> int:
    write_baseline = "--write-baseline" in sys.argv
    dirs_only = "--dirs-only" in sys.argv
    if write_baseline and dirs_only:
        print("FAIL：--dirs-only 与 --write-baseline 禁止同用（会静默丢弃分支存量）")
        return 1

    bad, skipped = collect_bad(repo_root())
    if dirs_only:
        bad = [x for x in bad if not x.startswith("分支名")]
    for s in skipped:
        print(f"WARN: {s}", file=sys.stderr)

    if write_baseline:
        old = load_baseline()
        fresh = [x for x in bad if x not in old]
        if old and fresh:
            print("FAIL：--write-baseline 只许收紧（新集合 ⊆ 旧集合），以下为新增违规，拒绝写入：")
            for x in fresh:
                print(f"  - {x}")
            return 1
        BASELINE.write_text(
            "# 存量违规冻结快照（只减不增：清一项删一行，或本脚本 --write-baseline 收紧）\n"
            + "\n".join(bad) + "\n"
        )
        print(f"baseline 已更新：{len(bad)} 项（原 {len(old)} 项）")
        return 0

    frozen = load_baseline()
    fresh = [x for x in bad if x not in frozen]
    # 冻结项中不在当前违规集合者 = 已清理，按「只减不增」应收紧
    gone = [x for x in frozen if x not in bad]
    if fresh:
        print("命名检查 FAIL（新增违规）：")
        for x in fresh:
            print(f"  - {x}")
        print("规范见 specs/README.md（忽略区目录名 / worktree 与分支）；存量豁免见 check-naming.baseline.txt")
        return 1
    if gone:
        print(f"FAIL：baseline 中 {len(gone)} 项已消失——按「只减不增」应收紧（运行 --write-baseline）：")
        for x in gone:
            print(f"  - {x}")
        return 1
    print(f"命名检查 PASS（存量冻结 {len(frozen)} 项）")
    return 0


if __name__ == "__main__":
    sys.exit(main())
