#!/usr/bin/env python3
"""test-check-pr-gate.py — check-pr-gate 的「谁写谁不审」规则正负例（纯逻辑，不调 gh、不花模型钱）。

用法：python3 -I scripts/test-check-pr-gate.py   退出码 0 全过 · 1 有失败
"""
from __future__ import annotations

import importlib.util
import json
import os
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
spec = importlib.util.spec_from_file_location("gate", ROOT / "scripts" / "check-pr-gate.py")
gate = importlib.util.module_from_spec(spec)
assert spec.loader is not None
spec.loader.exec_module(gate)

BASE = "<!-- pr-gate:waiver 纯文档测试用例足够长 -->\nRefs #1\n"
GLM_BLOCK = ("<!-- pr-gate:v1 head=abc1234 roles=sibling:PASS,concurrency:PASS,boundary:PASS,simplify:PASS "
             "p0=0 p1=0 diff_lines=1 scope=full runner=zai model=glm-5.3 readonly=tools -->\nRefs #1\n")


def errs(body: str, ref: str, trailers=None) -> list[str]:
    return gate.check_reviewers(body, ref, trailers)


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
    ("未知 author", BASE + "<!-- author: gemini -->\n<!-- reviewers: codex,glm -->", "chore/x", False, "未知谱系"),
    ("两个 author 标记", BASE + "<!-- author: codex -->\n<!-- author: glm -->\n<!-- reviewers: deepseek,claude -->", "chore/x", False, "只许一个"),
    ("GLM 写、四角色也用 GLM（zai）", GLM_BLOCK + "<!-- reviewers: codex,deepseek -->", "glm/a66-pr3", False, "runner=zai"),
    ("author 声明丢掉前缀谱系（#234 式改写自述）", GLM_BLOCK + "<!-- author: claude -->\n<!-- reviewers: codex,deepseek -->", "glm/a68-p2", False, "只能加不能减"),
    ("codex/ 分支 Claude 也改过：claude 不得评审", BASE + "<!-- author: codex,claude -->\n<!-- reviewers: claude,deepseek -->", "codex/a64-pr11", False, "谁写谁不审"),
    ("codex/ 分支 Claude 也改过：DeepSeek+GLM 放行", BASE + "<!-- author: codex,claude -->\n<!-- reviewers: deepseek,glm -->", "codex/a64-pr11", True, ""),
    ("author 与前缀一致：放行", BASE + "<!-- author: DeepSeek -->\n<!-- reviewers: codex,glm -->", "deepseek/x", True, ""),
    ("chore 前缀 + author 标记：放行", BASE + "<!-- author: claude -->\n<!-- reviewers: codex,deepseek -->", "chore/gate", True, ""),
    ("reviewers 正文含 > 不得被忽略（重复）", BASE + "<!-- reviewers: codex,deepseek -->\n<!-- reviewers: claude,glm > -->", "claude/x", False, "恰好一个"),
    ("author 正文含 > 不得被忽略（未知谱系）", BASE + "<!-- author: gemini > -->\n<!-- reviewers: codex,glm -->", "chore/x", False, "未知谱系"),
    ("空 author 标记", BASE + "<!-- author: -->\n<!-- reviewers: codex,glm -->", "chore/x", False, "空的"),
    ("只有逗号的 author 标记", BASE + "<!-- author: , , -->\n<!-- reviewers: codex,glm -->", "chore/x", False, "空的"),
    ("前缀大小写与空白", BASE + "<!-- reviewers: codex,glm -->", " DeepSeek/x", True, ""),
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
    for name, body, ok, want in [
        ("署名 Claude 未声明：Claude 评审被拒", BASE + "<!-- reviewers: claude,deepseek -->", False, "含提交署名"),
        ("署名 Claude：anthropic 四角色被拒", GLM_BLOCK.replace("runner=zai model=glm-5.3", "runner=anthropic model=claude-sonnet-5-5")
         + "<!-- reviewers: deepseek,glm -->", False, "runner=anthropic"),
        ("署名 Claude：DeepSeek+GLM 评审放行", BASE + "<!-- reviewers: deepseek,glm -->", True, ""),
    ]:
        e = errs(body, "codex/a64-pr11", {"codex", "claude"})
        good = (not e) if ok else (bool(e) and any(want in x for x in e))
        print(f"{'PASS' if good else 'FAIL'} {name} {e if not good else ''}")
        fails += not good
    with tempfile.TemporaryDirectory() as td:
        g = ["git", "-C", td, "-c", "user.email=t@t", "-c", "user.name=t"]
        subprocess.run([*g, "init", "-q"], check=True)
        subprocess.run([*g, "commit", "-q", "--allow-empty", "-m", "base"], check=True)
        subprocess.run([*g, "commit", "-q", "--allow-empty", "-m", "x\n\nCo-Authored-By: Codex <noreply@openai.com>"], check=True)
        subprocess.run([*g, "commit", "-q", "--allow-empty", "-m", "y\n\nCo-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"], check=True)
        cwd = os.getcwd()
        os.chdir(td)
        try:
            got = gate.trailer_families("HEAD~2", "HEAD")
        finally:
            os.chdir(cwd)
        good = got == {"codex", "claude"}
        print(f"{'PASS' if good else 'FAIL'} trailer_families 从真实提交读出 {sorted(got)}")
        fails += not good
        # 别名：Sonnet / Anthropic 域名 / GPT / opencode / zhipu 都要识别
        for msg in ["a\n\nCo-Authored-By: Sonnet <noreply@anthropic.com>", "b\n\nCo-Authored-By: GPT <x@openai.com>",
                    "c\n\nCo-Authored-By: opencode <glm-5.3@opencode>", "d\n\nCo-Authored-By: DeepSeek <d@x>"]:
            subprocess.run([*g, "commit", "-q", "--allow-empty", "-m", msg], check=True)
        os.chdir(td)
        try:
            got2 = gate.trailer_families("HEAD~4", "HEAD")
            bad_range_raised = False
            try:
                gate.trailer_families("deadbeefdeadbeef", "HEAD")
            except gate.TrailerScanError:
                bad_range_raised = True
        finally:
            os.chdir(cwd)
        good = got2 == {"claude", "codex", "glm", "deepseek"}
        print(f"{'PASS' if good else 'FAIL'} 署名别名识别 {sorted(got2)}")
        fails += not good
        print(f"{'PASS' if bad_range_raised else 'FAIL'} 给了范围但读不到 → 抛错（fail-closed）")
        fails += not bad_range_raised
    # 署名误识别负例：普通人的邮箱/缩写不算模型
    for line, want in [("David Smith <ds@example.org>", set()), ("Alex <alex@gpt.example.org>", set()),
                       ("Claude Opus 5.5 <noreply@anthropic.com>", {"claude"}), ("Bot <ci@openai.com>", {"codex"}),
                       ("opencode <glm-5.3@opencode>", {"glm"}), ("GLM-5.3 <noreply@zhipu.com>", {"glm"}),
                       ("alex@gpt.example.org", set()), ("ci@openai.com", {"codex"}),
                       ("OpenAI <noreply@example.org>", {"codex"}), ("Zhipu <noreply@example.org>", {"glm"})]:
        got = gate.trailer_family_of(line)
        good = got == want
        print(f"{'PASS' if good else 'FAIL'} 署名识别「{line}」→ {sorted(got)}")
        fails += not good
    # --pr 时 --head 不能替换目标 PR 的署名范围
    with tempfile.TemporaryDirectory() as td:
        payload = Path(td) / "pr.json"
        payload.write_text(json.dumps({"body": BASE + "<!-- reviewers: claude,deepseek -->", "headRefName": "codex/x",
                                       "headRefOid": "1111111111111111111111111111111111111111", "baseRefOid": "2222222"}))
        fake = Path(td) / "gh"
        fake.write_text(f"#!/bin/sh\ncat '{payload}'\n")
        fake.chmod(0o755)
        env = dict(os.environ, PATH=f"{td}:{os.environ['PATH']}")
        env.pop("GITHUB_ACTIONS", None)
        cp = subprocess.run([sys.executable, "-I", str(ROOT / "scripts" / "check-pr-gate.py"), "--pr", "1", "--head", ""],
                            capture_output=True, text=True, env=env)
        good = cp.returncode == 1 and "不一致" in cp.stdout
        print(f"{'PASS' if good else 'FAIL'} --pr 配 --head '' 被拒（rc={cp.returncode}）")
        fails += not good
    # CI 缺范围守卫
    env = dict(os.environ, GITHUB_ACTIONS="true", PR_HEAD_REF="codex/x")
    env.pop("PR_BASE_SHA", None); env.pop("PR_HEAD", None)
    with tempfile.NamedTemporaryFile("w", suffix=".md", delete=False) as f:
        f.write(BASE + "<!-- reviewers: deepseek,glm -->")
    cp = subprocess.run([sys.executable, "-I", str(ROOT / "scripts" / "check-pr-gate.py"), "--body-file", f.name],
                        capture_output=True, text=True, env=env)
    os.unlink(f.name)
    good = cp.returncode == 2
    print(f"{'PASS' if good else 'FAIL'} CI 缺 PR_BASE_SHA/PR_HEAD 退 2（rc={cp.returncode}）")
    fails += not good
    # --pr 模式用目标 PR 的分支，不继承环境 PR_HEAD_REF（codex 评审 P1 的复现）
    with tempfile.TemporaryDirectory() as td:
        fake = Path(td) / "gh"
        payload = Path(td) / "pr.json"  # 不经 shell echo：macOS sh 的 echo 会把 JSON 里的 \\n 转义成真换行
        payload.write_text(json.dumps({"body": BASE + "<!-- reviewers: claude,glm -->", "headRefName": "claude/fork"}))
        fake.write_text(f"#!/bin/sh\ncat '{payload}'\n")
        fake.chmod(0o755)
        env = dict(os.environ, PATH=f"{td}:{os.environ['PATH']}", PR_HEAD_REF="codex/other-pr")
        env.pop("GITHUB_ACTIONS", None)  # 本用例测的是 --pr 取目标分支；CI 缺范围守卫另有用例
        cp = subprocess.run([sys.executable, "-I", str(ROOT / "scripts" / "check-pr-gate.py"), "--pr", "1"], capture_output=True, text=True, env=env)
        good = cp.returncode == 1 and "谁写谁不审" in cp.stdout
        print(f"{'PASS' if good else 'FAIL'} --pr 用目标 PR 分支、不继承 PR_HEAD_REF（rc={cp.returncode}）")
        fails += not good
    # check() 整体也接上了：作者自审的 PR 过不了
    whole = gate.check(BASE + "<!-- reviewers: dsh,codex -->", "", "deepseek/x")
    good = any("谁写谁不审" in x for x in whole)
    print(f"{'PASS' if good else 'FAIL'} check() 整体包含评审方规则")
    fails += not good
    print(f"test-check-pr-gate: {'全部通过' if not fails else f'{fails} 项失败'}（{len(CASES) + 20} 例）")
    return 1 if fails else 0


if __name__ == "__main__":
    sys.exit(main())
