#!/usr/bin/env python3
"""test-check-pr-gate.py — check-pr-gate 的「谁写谁不审」规则正负例（纯逻辑，不调 gh、不花模型钱）。

用法：python3 -I scripts/test-check-pr-gate.py   退出码 0 全过 · 1 有失败
"""
from __future__ import annotations

import importlib.util
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
spec = importlib.util.spec_from_file_location("gate", ROOT / "scripts" / "check-pr-gate.py")
gate = importlib.util.module_from_spec(spec)
assert spec.loader is not None
spec.loader.exec_module(gate)

BASE = "<!-- pr-gate:waiver 纯文档测试用例足够长 -->\nRefs #1\n"
GLM_BLOCK = ("<!-- pr-gate:v1 head=abc1234 roles=sibling:PASS,concurrency:PASS,boundary:PASS,simplify:PASS "
             "p0=0 p1=0 diff_lines=1 scope=full runner=zai model=glm-5.3 readonly=tools -->\nRefs #1\n")


def errs(body: str, ref: str) -> list[str]:
    return gate.check_reviewers(body, ref)


CASES = [
    # (说明, body, 分支, 是否应通过, 失败信息里应含)
    ("deepseek 写、codex+GLM 审", BASE + "<!-- reviewers: codex,GLM -->", "deepseek/a63-pr6", True, ""),
    ("codex 写、DeepSeek+GLM 审（别名 dsh）", BASE + "<!-- reviewers: dsh, glm -->", "codex/a64-pr11", True, ""),
    ("显式 author 覆盖无前缀分支", BASE + "<!-- author: Claude -->\n<!-- reviewers: codex,deepseek -->", "chore/x", True, ""),
    ("作者自审：deepseek 写、dsh 审", BASE + "<!-- reviewers: dsh,codex -->", "deepseek/a63-pr6", False, "谁写谁不审"),
    ("作者自审：别名 opus 归 claude", BASE + "<!-- reviewers: opus,codex -->", "claude/b2", False, "谁写谁不审"),
    ("只有一家", BASE + "<!-- reviewers: codex -->", "deepseek/x", False, "至少两家"),
    ("两个名字同一谱系", BASE + "<!-- reviewers: dsh,deepseek -->", "codex/x", False, "至少两家"),
    ("未知谱系", BASE + "<!-- reviewers: codex,gemini -->", "deepseek/x", False, "未知谱系"),
    ("缺 reviewers 标记", BASE, "deepseek/x", False, "reviewers"),
    ("两个 reviewers 标记", BASE + "<!-- reviewers: codex,glm -->\n<!-- reviewers: deepseek,glm -->", "claude/x", False, "恰好一个"),
    ("无前缀且无 author", BASE + "<!-- reviewers: codex,glm -->", "chore/x", False, "无法确定作者"),
    ("未知 author", BASE + "<!-- author: gemini -->\n<!-- reviewers: codex,glm -->", "chore/x", False, "不是已知谱系"),
    ("两个 author 标记", BASE + "<!-- author: codex -->\n<!-- author: glm -->\n<!-- reviewers: deepseek,claude -->", "chore/x", False, "只许一个"),
    ("GLM 写、四角色也用 GLM（zai）", GLM_BLOCK + "<!-- reviewers: codex,deepseek -->", "glm/a66-pr3", False, "runner=zai"),
    ("GLM 写、四角色用 GLM 以外：放行", GLM_BLOCK.replace("runner=zai model=glm-5.3", "runner=anthropic model=claude-sonnet-5-5")
     + "<!-- reviewers: codex,deepseek -->", "glm/a66-pr3", True, ""),
]


def main() -> int:
    fails = 0
    for name, body, ref, ok, want in CASES:
        e = errs(body, ref)
        good = (not e) if ok else (bool(e) and any(want in x for x in e))
        print(f"{'PASS' if good else 'FAIL'} {name} {e if not good else ''}")
        fails += not good
    # check() 整体也接上了：作者自审的 PR 过不了
    whole = gate.check(BASE + "<!-- reviewers: dsh,codex -->", "", "deepseek/x")
    good = any("谁写谁不审" in x for x in whole)
    print(f"{'PASS' if good else 'FAIL'} check() 整体包含评审方规则")
    fails += not good
    print(f"test-check-pr-gate: {'全部通过' if not fails else f'{fails} 项失败'}（{len(CASES) + 1} 例）")
    return 1 if fails else 0


if __name__ == "__main__":
    sys.exit(main())
