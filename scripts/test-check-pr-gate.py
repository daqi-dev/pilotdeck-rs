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
    ("chore 前缀 + 只有未知 author", BASE + "<!-- author: gemini -->\n<!-- reviewers: codex,glm -->", "chore/x", False, "无法确定作者"),
    ("两个 author 标记取并集：都不得评审", BASE + "<!-- author: codex -->\n<!-- author: glm -->\n<!-- reviewers: glm,claude -->", "chore/x", False, "谁写谁不审"),
    ("两个 author 标记取并集：其余两家放行", BASE + "<!-- author: codex -->\n<!-- author: glm -->\n<!-- reviewers: deepseek,claude -->", "chore/x", True, ""),
    ("GLM 写、四角色也用 GLM（zai）", GLM_BLOCK + "<!-- reviewers: codex,deepseek -->", "glm/a66-pr3", False, "runner=zai"),
    ("author 声明丢掉前缀谱系也去不掉前缀（#234 式改写自述）", GLM_BLOCK + "<!-- author: claude -->\n<!-- reviewers: codex,deepseek -->", "glm/a68-p2", False, "runner=zai"),
    ("codex/ 分支 Claude 也改过：claude 不得评审", BASE + "<!-- author: codex,claude -->\n<!-- reviewers: claude,deepseek -->", "codex/a64-pr11", False, "谁写谁不审"),
    ("codex/ 分支 Claude 也改过：DeepSeek+GLM 放行", BASE + "<!-- author: codex,claude -->\n<!-- reviewers: deepseek,glm -->", "codex/a64-pr11", True, ""),
    ("author 与前缀一致：放行", BASE + "<!-- author: DeepSeek -->\n<!-- reviewers: codex,glm -->", "deepseek/x", True, ""),
    ("chore 前缀 + author 标记：放行", BASE + "<!-- author: claude -->\n<!-- reviewers: codex,deepseek -->", "chore/gate", True, ""),
    ("reviewers 正文含 > 不得被忽略（重复）", BASE + "<!-- reviewers: codex,deepseek -->\n<!-- reviewers: claude,glm > -->", "claude/x", False, "恰好一个"),
    ("author 正文含 > 不得被忽略", BASE + "<!-- author: claude > -->\n<!-- reviewers: claude,glm -->", "chore/x", False, "谁写谁不审"),
    ("空 author 标记", BASE + "<!-- author: -->\n<!-- reviewers: codex,glm -->", "chore/x", False, "无法确定作者"),
    ("只有逗号的 author 标记", BASE + "<!-- author: , , -->\n<!-- reviewers: codex,glm -->", "chore/x", False, "无法确定作者"),
    ("一个反引号开、三个反引号收也吞不掉作者", BASE + "`x\n<!-- author: codex,claude -->\ny```\n<!-- reviewers: claude,glm -->", "codex/r", False, "谁写谁不审"),
    ("代码里引用的 author 写法也算作者（只会更严）", BASE + "<!-- author: claude -->\n写法 `<!-- author: glm -->`\n<!-- reviewers: glm,deepseek -->", "chore/x", False, "谁写谁不审"),
    ("代码里引用的占位写法 A,B 不影响", BASE + "<!-- author: claude -->\n说明：写法是 `<!-- author: A,B -->`\n<!-- reviewers: codex,deepseek -->", "chore/x", True, ""),
    ("代码块里的 reviewers 不算标记", BASE + "```\n<!-- reviewers: claude,glm -->\n```\n<!-- reviewers: codex,deepseek -->", "claude/x", True, ""),
    ("注释里的 ``` 吞不掉后面的真 author", BASE + "<!-- explanation: ``` -->\n<!-- author: codex,claude -->\n<!-- end explanation: ``` -->\n<!-- reviewers: claude,glm -->", "codex/repro", False, "谁写谁不审"),
    ("~~~ 围栏里的标记不算", BASE + "~~~\n<!-- reviewers: claude,glm -->\n~~~\n<!-- reviewers: codex,deepseek -->", "claude/x", True, ""),
    ("双反引号行内代码里的标记不算", BASE + "<!-- author: claude -->\n写法 `` <!-- author: A,B --> ``\n<!-- reviewers: codex,deepseek -->", "chore/x", True, ""),
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
        # 署名不在末段（后面还有一段说明）也要识别
        subprocess.run([*g, "commit", "-q", "--allow-empty", "-m", "e\n\nCo-Authored-By: Claude <noreply@anthropic.com>\n\n（评审意见已处理）"], check=True)
        os.chdir(td)
        try:
            got3 = gate.trailer_families("HEAD~1", "HEAD")
        finally:
            os.chdir(cwd)
        subprocess.run([*g, "commit", "-q", "--allow-empty", "-m", "f\n\nCo-Authored-By: Bot\n <noreply@anthropic.com>"], check=True)
        os.chdir(td)
        try:
            got4 = gate.trailer_families("HEAD~1", "HEAD")
        finally:
            os.chdir(cwd)
        subprocess.run([*g, "commit", "-q", "--allow-empty", "-m", "h\n\nChange-By: ai\nAgent: opencode\nModel: glm-5.3\nCo-authored-by: 郑 <z@example.org>"], check=True)
        os.chdir(td)
        try:
            got5 = gate.trailer_families("HEAD~1", "HEAD")
        finally:
            os.chdir(cwd)
        good5 = got5 == {"glm"}
        print(f"{'PASS' if good5 else 'FAIL'} 规约标注 Agent:/Model: 也计入作者 {sorted(got5)}")
        fails += not good5
        good4 = got4 == {"claude"}
        print(f"{'PASS' if good4 else 'FAIL'} 折行署名（续行里的厂商域名）也识别 {sorted(got4)}")
        fails += not good4
        good3 = got3 == {"claude"}
        print(f"{'PASS' if good3 else 'FAIL'} 署名不在末段也识别 {sorted(got3)}")
        fails += not good3
        good = got2 == {"claude", "codex", "glm", "deepseek"}
        print(f"{'PASS' if good else 'FAIL'} 署名别名识别 {sorted(got2)}")
        fails += not good
        print(f"{'PASS' if bad_range_raised else 'FAIL'} 给了范围但读不到 → 抛错（fail-closed）")
        fails += not bad_range_raised
    for name, body, want in [
        ("代码里引用的 waiver 不算豁免", "说明：写法是 `<!-- pr-gate:waiver 纯文档更新足够长 -->`\nRefs #1\n<!-- reviewers: codex,deepseek -->", "缺角色审计结果块"),
        ("真结果块塞进围栏 = 缺结果块（拦）", "```\n" + GLM_BLOCK + "```\n<!-- reviewers: codex,deepseek -->", "缺角色审计结果块"),
        ("文档里围栏引用的示例结果块不当真块", "<!-- pr-gate:waiver 纯文档更新足够长 -->\nRefs #1\n```\n" + GLM_BLOCK.replace("Refs #1\n","") + "```\n<!-- reviewers: codex,deepseek -->", None),
        ("代码里引用的 Refs 不算 issue 关联", "<!-- pr-gate:waiver 纯文档更新足够长 -->\n```\nRefs #1\n```\n<!-- reviewers: codex,deepseek -->", "缺 issue 关联"),
    ]:
        e = gate.check(body, "", "glm/x" if "GLM_BLOCK" in name or "结果块" in name else "claude/x")
        good = (not e) if want is None else any(want in x for x in e)
        print(f"{'PASS' if good else 'FAIL'} {name} {e if not good else ''}")
        fails += not good
    with tempfile.TemporaryDirectory() as td:
        g = ["git", "-C", td, "-c", "user.email=t@t", "-c", "user.name=t"]
        r = lambda *x: subprocess.run([*g, *x], check=True, capture_output=True, text=True).stdout.strip()
        r("init", "-q", "-b", "main"); r("commit", "-q", "--allow-empty", "-m", "root")
        old_base = r("rev-parse", "HEAD")  # PR 创建时的 base.sha
        r("commit", "-q", "--allow-empty", "-m", "main later\n\nCo-Authored-By: GLM-5.3 <noreply@zhipu.com>")
        r("switch", "-q", "-c", "codex/x")  # 相当于 PR 已 rebase 到更新的 main
        r("commit", "-q", "--allow-empty", "-m", "pr\n\nCo-Authored-By: Codex <noreply@openai.com>")
        r("update-ref", "refs/remotes/origin/main", "main")
        cwd = os.getcwd(); os.chdir(td)
        try:
            stale = gate.trailer_families(old_base, "codex/x")
            tip = gate.trailer_families("origin/main", "codex/x")
        finally:
            os.chdir(cwd)
        good = stale == {"codex", "glm"} and tip == {"codex"}
        print(f"{'PASS' if good else 'FAIL'} 冻结 base 会多算 {sorted(stale)}；按目标分支 tip 只算 PR 自己的 {sorted(tip)}")
        fails += not good
    # 不依赖本仓的检出深度（CI 的 ci.yml 是浅检出、没有 origin/main）：自建临时仓
    with tempfile.TemporaryDirectory() as td:
        g = ["git", "-C", td, "-c", "user.email=t@t", "-c", "user.name=t"]
        r = lambda *x: subprocess.run([*g, *x], check=True, capture_output=True, text=True).stdout.strip()
        r("init", "-q", "-b", "main"); r("commit", "-q", "--allow-empty", "-m", "root")
        r("update-ref", "refs/remotes/origin/main", "main")
        r("switch", "-q", "-c", "codex/x"); r("commit", "-q", "--allow-empty", "-m", "pr\n\nCo-Authored-By: Codex <noreply@openai.com>")
        bf = Path(td) / "body.md"
        bf.write_text(BASE + "<!-- reviewers: deepseek,glm -->")
        env = dict(os.environ, PR_BASE_REF="main", PR_HEAD="HEAD", PR_HEAD_REF="codex/x")
        env.pop("PR_BASE_SHA", None)
        cp = subprocess.run([sys.executable, "-I", str(ROOT / "scripts" / "check-pr-gate.py"), "--body-file", str(bf)],
                            capture_output=True, text=True, env=env, cwd=td)
        good = cp.returncode == 0 and "提交署名谱系 ['codex']" in cp.stdout
        print(f"{'PASS' if good else 'FAIL'} 设了 PR_BASE_REF 时按 origin/<ref> 扫（rc={cp.returncode}）")
        fails += not good
    # 署名误识别负例：普通人的邮箱/缩写不算模型
    for line, want in [("David Smith <ds@example.org>", set()), ("Alex <alex@gpt.example.org>", set()),
                       ("Claude Opus 5.5 <noreply@anthropic.com>", {"claude"}), ("Bot <ci@openai.com>", {"codex"}),
                       ("opencode <glm-5.3@opencode>", {"glm"}), ("GLM-5.3 <noreply@zhipu.com>", {"glm"}),
                       ("alex@gpt.example.org", set()), ("ci@openai.com", {"codex"}),
                       ("OpenAI <noreply@example.org>", {"codex"}), ("Zhipu <noreply@example.org>", {"glm"}),
                       ("小助手 <bot@bigmodel.cn>, Codex <noreply@openai.com>", {"glm", "codex"}),
                       ("Assistant <a@z.ai> (pair)", {"glm"}), ("x <a@example.org> y <b@anthropic.com>", {"claude"})]:
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
    print(f"test-check-pr-gate: {'全部通过' if not fails else f'{fails} 项失败'}（{len(CASES) + 27} 例）")
    return 1 if fails else 0


if __name__ == "__main__":
    sys.exit(main())
