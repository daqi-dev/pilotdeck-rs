#!/usr/bin/env python3
"""check-pr-gate.py — 合并闸门第 2、3 步的机器检查：PR 描述里必须有角色审计结果块（或写明理由的豁免）与 issue 关联。

只查「做没做、做全没做」，不查结论对不对（那是双谱系评审的事）；结果块可被手写伪造，本脚本防的是**漏跑**。

规则：
  1. 有 `<!-- pr-gate:v1 ... roles=a:S,b:S,... -->` 结果块（scripts/run-review-roles.py 生成），
     且 sibling / concurrency / boundary / simplify 四个角色都在、都是 PASS；
     或有 `<!-- pr-gate:waiver <理由≥8字> -->`（纯文档/纯状态 PR 用，理由会打印出来供评审看）。
  2. 有 issue 关联：`Closes|Fixes|Resolves #N`（合并即关单，GitHub 定顺序）或 `Refs #N`（一单多 PR，单保持开），
     或 `<!-- no-issue <理由> -->`。

  3. 结果块 head= 须等于 PR 当前 head；不等时，只有块里的 patch=（审计时 `git diff base...head` 的补丁指纹）
     与当前 PR 补丁指纹（`git diff <PR base>...<PR head>`）相同才沿用——纯 rebase、内容逐字不变的提交链
     不必重审（同 Gerrit trivial rebase 保留评审）；任何一行不同、上下文变了、或算不出指纹，都按旧提交拒收。

输入：--body-file FILE ｜ --pr N（调 gh pr view，只读）｜ 环境变量 PR_BODY（CI 用，避免 shell 插值）
      --head / PR_HEAD、--base / PR_BASE（CI 传 PR 的 head 与 base SHA；补丁指纹在 --repo 里算）
退出码：0 通过 · 1 不通过 · 2 输入错误
"""
from __future__ import annotations

import argparse
import hashlib
import os
import re
import subprocess
import sys

REQUIRED_ROLES = ("sibling", "concurrency", "boundary", "simplify")
MARKER = re.compile(r"<!--\s*pr-gate:v1\b([^>]*)-->")
WAIVER = re.compile(r"<!--\s*pr-gate:waiver\s+(.+?)\s*-->", re.S)
ISSUE_LINK = re.compile(r"\b(?:close[sd]?|fix(?:e[sd])?|resolve[sd]?|refs)\s+#\d+\b", re.I)
NO_ISSUE = re.compile(r"<!--\s*no-issue\s+(\S.*?)\s*-->", re.S)
# 补丁指纹的 diff 参数全部写死：不受本机 / CI 的 diff.renames、diff.algorithm、前缀、颜色、外部 diff 配置影响
FINGERPRINT_DIFF = ["diff", "--no-color", "--no-ext-diff", "--no-textconv", "--no-renames", "--unified=3",
                    "--diff-algorithm=myers", "--src-prefix=a/", "--dst-prefix=b/"]


def patch_fingerprint(repo: str, base: str, head: str) -> str:
    """`git diff base...head` 的 `git patch-id --verbatim`：不含行号，含改动行、上下文与空白，rebase 后内容不变则不变。

    已知边界（威胁模型「防漏跑不防伪造」内接受）：patch-id 忽略行号，同一文件里把改动挪到 3 行上下文逐字相同的
    重复块处，指纹不变——只能刻意构造，不会由 rebase 或正常修改产生。

    用 --verbatim 而非 --stable：--stable 会剥掉空白，只改缩进（Python / YAML 里改语义）的提交指纹不变，会假沿用
    （2026-10-25 实测）；文件模式、新增空文件、删除、二进制改动都会改变指纹（同日实测）。

    run-review-roles.py 写块与本闸门比对共用这一个函数。空 diff 或 git 失败返回 ''（调用方据此不沿用）。
    """
    cp = subprocess.run(["git", "-C", repo, *FINGERPRINT_DIFF, f"{base}...{head}"], capture_output=True)
    if cp.returncode != 0 or not cp.stdout:
        return ""
    pid = subprocess.run(["git", "-C", repo, "patch-id", "--verbatim"], input=cp.stdout, capture_output=True)
    lines = pid.stdout.splitlines()
    # 整个 diff 只应得到一行「<patch-id> <0…0>」；多行、少字段、非 40 hex 都按算不出处理（fail closed）
    if pid.returncode != 0 or len(lines) != 1 or len(lines[0].split()) != 2:
        return ""
    word = lines[0].split()[0]
    if not re.fullmatch(rb"[0-9a-f]{40}", word):
        return ""
    # 二进制（含 .gitattributes 标 -diff 的文本）只渲染成「Binary files … differ」，内容是否进指纹取决于
    # patch-id 是否哈希 index 行——git 2.54 实测会，但不是跨版本契约。把这些文件改动前后的完整 blob id 并入指纹，
    # 内容决定指纹，与 git 版本无关；没有二进制改动时指纹就是 patch-id 本身。
    # 与主 diff 用同一组写死参数。git 2.54 实测 --numstat 本就不走 textconv（防御性统一，防版本差异）
    num = subprocess.run(["git", "-C", repo, *FINGERPRINT_DIFF, "--numstat", "-z", f"{base}...{head}"],
                         capture_output=True)
    if num.returncode != 0:
        return ""
    binary = [rec.split(b"\t", 2)[2] for rec in num.stdout.split(b"\0") if rec.startswith(b"-\t-\t")]
    if not binary:
        return word.decode()
    raw = subprocess.run(["git", "-C", repo, *FINGERPRINT_DIFF, "--raw", "--no-abbrev", f"{base}...{head}",
                          "--", *[":(literal)" + p.decode("utf-8", "surrogateescape") for p in binary]],
                         capture_output=True)
    if raw.returncode != 0 or not raw.stdout:
        return ""
    return hashlib.sha1(word + b"\n" + raw.stdout).hexdigest()


def check_block(attrs: str, head: str, patch: str = "") -> list[str]:
    errs: list[str] = []
    # 注释内全文直接 findall：roles=...\s+...\s+... 多键值里 \S+ 会在首个空格截断（P1 修复）
    pairs = re.findall(r"(\w+):([A-Z_]+)", attrs)
    roles = dict(pairs)
    dup = sorted({r for r, _ in pairs if [x for x, _ in pairs].count(r) > 1})
    if dup:
        errs.append(f"结果块角色重复 {dup}（后出现的会遮蔽前面的状态）")
    for r in REQUIRED_ROLES:
        if r not in roles:
            errs.append(f"结果块缺角色 {r}")
        elif roles[r] != "PASS":
            errs.append(f"角色 {r} 状态 {roles[r]}（只有 PASS 计入闸门；PARTIAL/TIMEOUT 须重跑或分批）")
    # #200：按完整键解析（空白分隔的 key=value），每个键恰好一次
    kv: dict[str, list[str]] = {}
    for tok in attrs.split():
        k, sep, v = tok.partition("=")
        if sep:
            kv.setdefault(k, []).append(v)
    one = {k: v[0] for k, v in kv.items() if len(v) == 1}
    for k in ("head", "roles", "readonly", "runner", "model"):
        if len(kv.get(k, [])) != 1:
            errs.append(f"结果块键 {k} 须恰好出现一次（现 {len(kv.get(k, []))} 次）；重跑 run-review-roles.py")
    if len(kv.get("patch", [])) > 1:
        errs.append("结果块键 patch 至多出现一次；重跑 run-review-roles.py")
    if one.get("readonly", "tools") != "tools":
        errs.append(f"结果块 readonly={one['readonly']}：只接受 readonly=tools（执行层工具白名单）")
    prefix = {"zai": "glm-", "anthropic": "claude-"}
    rn, md = one.get("runner"), one.get("model", "")
    if rn is not None and (rn not in prefix or not md.startswith(prefix[rn])):
        errs.append(f"结果块 runner={rn} model={md} 不是已知组合（zai→glm-*，anthropic→claude-*）")
    # P1（boundary 审 #1）：head= 必须与当前 PR head 比对，防「推新提交后旧 PASS 块照样过闸」
    if head:
        bhead = one.get("head", "")  # 完整键、恰一次（nothead= 不算，重复 head 已在上面拒）
        short, full = sorted((bhead, head), key=len)
        if not re.fullmatch(r"[0-9a-f]{7,40}", bhead):
            errs.append(f"结果块 head=「{bhead}」不是 commit SHA（外部 diff 占位不得过闸）；重跑 run-review-roles.py")
        elif not full.startswith(short):
            bpatch = one.get("patch", "")
            # 只有审过整个 PR 的块（scope=full）才可沿用：--paths 分批块的指纹是整个 PR 的，但它只审了一部分
            if one.get("scope") != "full":
                errs.append(f"结果块 head={bhead} 与当前 PR head={head} 不符，且 scope={one.get('scope', '缺')} 不是 full，"
                            "分批块不沿用；重跑 run-review-roles.py")
            elif patch and re.fullmatch(r"[0-9a-f]{40}", bpatch) and bpatch == patch:
                print(f"check-pr-gate: 结果块 head={bhead} ≠ PR head={head}，但补丁指纹相同（patch={patch[:12]}），沿用")
            else:
                why = ("块内无 patch= 指纹" if not kv.get("patch") else "块内 patch= 不是 40 位指纹"
                       if not re.fullmatch(r"[0-9a-f]{40}", bpatch) else "当前补丁指纹算不出" if not patch
                       else f"补丁指纹不同（块 {bpatch[:12]} ≠ 当前 {patch[:12]}）")
                errs.append(f"结果块 head={bhead} 与当前 PR head={head} 不符（审计结果属于旧提交；{why}）；重跑 run-review-roles.py")
    return errs


def check(body: str, head: str = "", patch: str = "") -> list[str]:
    errs: list[str] = []
    blocks = list(MARKER.finditer(body))
    w = WAIVER.search(body)
    if blocks:
        # 分批（--paths）评审可贴多个结果块：逐块校验，任一无效即拒
        for n, m in enumerate(blocks, 1):
            tag = f"结果块 {n}/{len(blocks)}：" if len(blocks) > 1 else ""
            errs.extend(tag + e for e in check_block(m.group(1), head, patch))
    elif w:
        reason = w.group(1).strip()
        if len(reason) < 8:
            errs.append("豁免理由太短（≥8 字）")
        else:
            print(f"check-pr-gate: 豁免——{reason}")
    else:
        errs.append("缺角色审计结果块（python3 scripts/run-review-roles.py 生成 summary.md，贴进 PR）或 pr-gate:waiver 豁免")
    if not ISSUE_LINK.search(body) and not NO_ISSUE.search(body):
        errs.append("缺 issue 关联：Closes/Fixes #N（合并即关）或 Refs #N（单保持开），或 <!-- no-issue 理由 -->")
    return errs


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    g = ap.add_mutually_exclusive_group()
    g.add_argument("--body-file")
    g.add_argument("--pr")
    ap.add_argument("--head", default=os.environ.get("PR_HEAD", ""), help="PR 当前 head SHA；给定时与结果块 head= 比对")
    ap.add_argument("--base", default=os.environ.get("PR_BASE", ""), help="PR base SHA；与 --head 同给时算当前补丁指纹，供 head 不等时比对")
    ap.add_argument("--repo", default=".", help="算补丁指纹的 git 仓库（须含 base 与 head 两个提交）")
    a = ap.parse_args()
    if a.body_file:
        body = open(a.body_file, encoding="utf-8").read()
    elif a.pr:
        cp = subprocess.run(["gh", "pr", "view", a.pr, "--json", "body", "--jq", ".body"], capture_output=True, text=True)
        if cp.returncode != 0:
            print(f"check-pr-gate: gh 失败：{cp.stderr.strip()}", file=sys.stderr)
            return 2
        body = cp.stdout
    elif "PR_BODY" in os.environ:
        body = os.environ["PR_BODY"]
    else:
        print("check-pr-gate: 需要 --body-file / --pr / 环境变量 PR_BODY", file=sys.stderr)
        return 2
    patch = patch_fingerprint(a.repo, a.base, a.head) if a.head and a.base else ""
    errs = check(body, a.head, patch)
    for e in errs:
        print(f"check-pr-gate: FAIL {e}")
    if not errs:
        print("check-pr-gate: PASS")
    return 1 if errs else 0


if __name__ == "__main__":
    sys.exit(main())
