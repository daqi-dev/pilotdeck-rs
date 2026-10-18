#!/usr/bin/env python3
"""check-pr-gate.py — 合并闸门第 2、3 步的机器检查：PR 描述里必须有角色审计结果块（或写明理由的豁免）与 issue 关联。

只查「做没做、做全没做」，不查结论对不对（那是双谱系评审的事）；结果块可被手写伪造，本脚本防的是**漏跑**。

规则：
  1. 有 `<!-- pr-gate:v1 ... roles=a:S,b:S,... -->` 结果块（scripts/run-review-roles.py 生成），
     且 sibling / concurrency / boundary / simplify 四个角色都在、都是 PASS；
     或有 `<!-- pr-gate:waiver <理由≥8字> -->`（纯文档/纯状态 PR 用，理由会打印出来供评审看）。
  2. 有 issue 关联：`Closes|Fixes|Resolves #N`（合并即关单，GitHub 定顺序）或 `Refs #N`（一单多 PR，单保持开），
     或 `<!-- no-issue <理由> -->`。
  3. 谁写谁不审（CLAUDE.md「当前协作与持续推进」）：PR 描述有 `<!-- reviewers: A,B -->`，列出双谱系评审的两家；
     作者谱系（可以多家）＝ 分支前缀谱系 ∪ `<!-- author: A,B -->` 声明 ∪ 提交 Co-Authored-By 署名（CI 给了 PR_BASE_SHA 时扫描）：
       - 前缀是 codex/ glm/ deepseek/ claude/ 时，author 声明必须包含它（只能加不能减，防改写自述）；
       - 其它前缀（docs/ chore/ fix/ dev/）必须写 author，否则 FAIL；
       - 署名扫描是非自述信号：协调者用 Claude 提交 codex 的产物也会带 Claude 署名，此时 Claude 同样不得评审。
     评审至少两家不同谱系、都不是作者；角色审计结果块的 runner 谱系（zai→GLM、anthropic→Claude）也不得是作者。
     谱系名不区分大小写，dsh/ds→deepseek，gpt→codex，opus/sonnet→claude，zai→glm。

输入：--body-file FILE ｜ --pr N（调 gh pr view，只读）｜ 环境变量 PR_BODY（CI 用，避免 shell 插值）；
      分支名：--head-ref 或环境变量 PR_HEAD_REF（--pr 时自动取）
退出码：0 通过 · 1 不通过 · 2 输入错误
"""
from __future__ import annotations

import argparse
import os
import re
import subprocess
import sys

REQUIRED_ROLES = ("sibling", "concurrency", "boundary", "simplify")
MARKER = re.compile(r"<!--\s*pr-gate:v1\b([^>]*)-->")
WAIVER = re.compile(r"<!--\s*pr-gate:waiver\s+(.+?)\s*-->", re.S)
ISSUE_LINK = re.compile(r"\b(?:close[sd]?|fix(?:e[sd])?|resolve[sd]?|refs)\s+#\d+\b", re.I)
NO_ISSUE = re.compile(r"<!--\s*no-issue\s+(\S.*?)\s*-->", re.S)
# 取完整注释正文再校验（不能用 [^>]：正文含 > 时整条会被静默忽略）
REVIEWERS = re.compile(r"<!--\s*reviewers\s*:(.*?)-->", re.I | re.S)
AUTHOR = re.compile(r"<!--\s*author\s*:(.*?)-->", re.I | re.S)
FAMILY = {"codex": "codex", "gpt": "codex", "glm": "glm", "zai": "glm", "deepseek": "deepseek", "dsh": "deepseek",
          "ds": "deepseek", "claude": "claude", "opus": "claude", "sonnet": "claude", "anthropic": "claude"}
BRANCH_FAMILY = {"codex": "codex", "glm": "glm", "deepseek": "deepseek", "claude": "claude"}


def family(name: str) -> str | None:
    return FAMILY.get(name.strip().lower())


# 署名识别分两处看：名字部分（< 之前）按模型名单词匹配；邮箱只认已知厂商域名。
# 不拆整条邮箱：普通人的 ds@example.org、alex@gpt.example.org 不能算成模型作者。
TRAILER_NAME_EXTRA = {"opencode": "glm", "openai": "codex", "zhipu": "glm"}
NAME_SKIP = {"ds"}  # 太短，人名缩写常见
VENDOR_DOMAINS = {"anthropic.com": "claude", "openai.com": "codex", "deepseek.com": "deepseek",
                  "zhipu.com": "glm", "zhipuai.cn": "glm", "bigmodel.cn": "glm", "z.ai": "glm", "opencode": "glm"}


def trailer_family_of(line: str) -> set[str]:
    m = re.match(r"\s*([^<]*?)\s*(?:<([^>]*)>)?\s*$", line)
    name, email = (m.group(1), m.group(2) or "") if m else (line, "")
    if not email and re.fullmatch(r"\S+@\S+", name.strip()):
        name, email = "", name.strip()  # 裸邮箱：只按域名判，不拆词
    out = set()
    words = {k: v for k, v in {**FAMILY, **TRAILER_NAME_EXTRA}.items() if k not in NAME_SKIP}
    for tok in re.findall(r"[a-z0-9]+", name.lower()):
        if tok in words:
            out.add(words[tok])
    domain = email.lower().rsplit("@", 1)[-1] if "@" in email else ""
    for d, f in VENDOR_DOMAINS.items():
        if domain == d or domain.endswith("." + d):
            out.add(f)
    return out


class TrailerScanError(Exception):
    pass


def trailer_families(base: str, head: str) -> set[str]:
    """base..head 提交的 Co-Authored-By 署名对应的谱系。给了范围却读不到 → 抛 TrailerScanError（fail-closed）。"""
    if not base or not head:
        return set()
    # 从 merge-base 起算：PR rebase 过 main 时 base.sha 可能是旧点，直接 base..head 会把 main 上别家的提交算进来
    mb = subprocess.run(["git", "merge-base", base, head], capture_output=True, text=True)
    if mb.returncode != 0:
        raise TrailerScanError(mb.stderr.strip()[:200] or "merge-base 失败")
    # 扫完整提交消息里任意位置的 Co-Authored-By 行：git 的 trailer 解析只认末段，署名后再跟一段说明就会漏
    cp = subprocess.run(["git", "log", f"{mb.stdout.strip()}..{head}", "--format=%B%x00"], capture_output=True, text=True)
    if cp.returncode != 0:
        raise TrailerScanError(cp.stderr.strip()[:200])
    out = set()
    for m in re.finditer(r"(?im)^\s*co-authored-by\s*:\s*(.+)$", cp.stdout):
        out |= trailer_family_of(m.group(1))
    return out


CODE = re.compile(r"```.*?```|`[^`\n]*`", re.S)


def strip_code(body: str) -> str:
    """去掉行内代码与代码块：文档里引用标记写法（如 `<!-- author: A,B -->`）不算标记。"""
    return CODE.sub("", body)


def check_reviewers(body: str, head_ref: str, trailers: set[str] | None = None) -> list[str]:
    errs: list[str] = []
    raw, body = body, strip_code(body)  # 标记声明去掉代码引用再看；结果块与 check() 同口径读原文
    am = [x.strip() for x in AUTHOR.findall(body)]
    if len(am) > 1:
        return [f"有 {len(am)} 个 author 标记；只许一个（多家写成 author: A,B）"]
    branch = BRANCH_FAMILY.get(head_ref.strip().split("/", 1)[0].lower()) if "/" in head_ref else None
    names = [x for x in re.split(r"[,\s]+", am[0]) if x] if am else []
    unknown = [x for x in names if family(x) is None]
    if am and not names:
        errs.append("author 标记是空的；写出至少一个谱系（codex/GLM/DeepSeek/Claude），或删掉标记")
    if unknown:
        errs.append(f"author 里有未知谱系 {unknown}（可选 codex/GLM/DeepSeek/Claude）")
    declared = {family(x) for x in names if family(x)}
    if branch and am and branch not in declared:
        errs.append(f"author 声明 {sorted(declared)} 没包含分支前缀谱系（{branch}）；只能加不能减")
    authors = declared | ({branch} if branch else set()) | (trailers or set())
    if not branch and not am:
        errs.append(f"无法确定作者谱系：分支「{head_ref or '未知'}」前缀不是 codex/glm/deepseek/claude，须写 <!-- author: X -->")
    rm = REVIEWERS.findall(body)
    if len(rm) != 1:
        errs.append(f"须恰好一个 <!-- reviewers: A,B --> 标记（现 {len(rm)} 个），列出双谱系评审的两家")
        return errs
    rnames = [x for x in re.split(r"[,\s]+", rm[0].strip()) if x]
    fams = [family(x) for x in rnames]
    bad = [n for n, f in zip(rnames, fams) if f is None]
    if bad:
        errs.append(f"reviewers 里有未知谱系 {bad}（可选 codex/GLM/DeepSeek/Claude）")
    known = {f for f in fams if f}
    if len(known) < 2:
        errs.append(f"评审须至少两家不同谱系，现为 {sorted(known) or '无'}")
    clash = sorted(authors & known)
    if clash:
        src = "（含提交署名）" if trailers and set(clash) & trailers else ""
        errs.append(f"作者谱系 {clash}{src} 出现在评审里（谁写谁不审）")
    for m in MARKER.finditer(raw):
        rn = re.search(r"(?:^|\s)runner=(\S+)", m.group(1))
        if rn and family(rn.group(1)) in authors:
            errs.append(f"角色审计用的 runner={rn.group(1)} 与作者同谱系（{family(rn.group(1))}）；改用 --runner 另一家重跑")
    return errs


def check_block(attrs: str, head: str) -> list[str]:
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
            errs.append(f"结果块 head={bhead} 与当前 PR head={head} 不符（审计结果属于旧提交）；重跑 run-review-roles.py")
    return errs


def check(body: str, head: str = "", head_ref: str = "", trailers: set[str] | None = None) -> list[str]:
    errs: list[str] = []
    blocks = list(MARKER.finditer(body))
    w = WAIVER.search(body)
    if blocks:
        # 分批（--paths）评审可贴多个结果块：逐块校验，任一无效即拒
        for n, m in enumerate(blocks, 1):
            tag = f"结果块 {n}/{len(blocks)}：" if len(blocks) > 1 else ""
            errs.extend(tag + e for e in check_block(m.group(1), head))
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
    errs.extend(check_reviewers(body, head_ref, trailers))
    return errs


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    g = ap.add_mutually_exclusive_group()
    g.add_argument("--body-file")
    g.add_argument("--pr")
    ap.add_argument("--head", default=None, help="PR 当前 head SHA；给定时与结果块 head= 比对（默认：--pr 取目标 PR；否则环境 PR_HEAD）")
    ap.add_argument("--head-ref", default=None, help="PR 分支名，用来推断作者谱系（默认：--pr 取目标 PR 的分支；否则取环境变量 PR_HEAD_REF）")
    a = ap.parse_args()
    a.head_explicit = a.head is not None
    base = None
    if a.body_file:
        body = open(a.body_file, encoding="utf-8").read()
    elif a.pr:
        cp = subprocess.run(["gh", "pr", "view", a.pr, "--json", "body,headRefName,headRefOid,baseRefOid"], capture_output=True, text=True)
        if cp.returncode != 0:
            print(f"check-pr-gate: gh 失败：{cp.stderr.strip()}", file=sys.stderr)
            return 2
        import json
        pr = json.loads(cp.stdout)
        body = pr.get("body") or ""
        # --pr 一律用目标 PR 自己的分支，不继承环境里别的 PR 的 PR_HEAD_REF（防用他人分支判作者）
        a.head_ref = a.head_ref if a.head_ref is not None else pr.get("headRefName", "")
        # 署名范围也一律取目标 PR 的，不继承环境里别的 PR 的 PR_HEAD / PR_BASE_SHA
        target = pr.get("headRefOid", "")
        if a.head_explicit and target and not (a.head and (target.startswith(a.head) or a.head.startswith(target))):
            print(f"check-pr-gate: FAIL --head={a.head!r} 与目标 PR 的 head {target[:12]} 不一致；--pr 模式一律按目标 PR 扫署名")
            return 1
        a.head = target or (a.head or "")
        base = pr.get("baseRefOid", "")
        subprocess.run(["git", "fetch", "-q", "origin", base, a.head], capture_output=True)
    elif "PR_BODY" in os.environ:
        body = os.environ["PR_BODY"]
    else:
        print("check-pr-gate: 需要 --body-file / --pr / 环境变量 PR_BODY", file=sys.stderr)
        return 2
    if a.head_ref is None:
        a.head_ref = os.environ.get("PR_HEAD_REF", "")
    if a.head is None:
        a.head = os.environ.get("PR_HEAD", "")
    if base is None:
        base = os.environ.get("PR_BASE_SHA", "")
    if os.environ.get("GITHUB_ACTIONS") == "true" and not (base and a.head):
        print("check-pr-gate: CI 缺 PR_BASE_SHA / PR_HEAD，无法扫提交署名（fail-closed）", file=sys.stderr)
        return 2
    try:
        trailers = trailer_families(base, a.head)
    except TrailerScanError as e:
        print(f"check-pr-gate: FAIL 读不到提交署名 {base[:8]}..{a.head[:8]}（{e}）；给了范围就必须扫成功，不降级放行")
        return 1
    if base and a.head:
        print(f"check-pr-gate: 提交署名谱系 {sorted(trailers) or '无'}（计入作者）")
    else:
        print("check-pr-gate: 本地未给提交范围，未扫署名（CI 会扫）", file=sys.stderr)
    errs = check(body, a.head, a.head_ref, trailers)
    for e in errs:
        print(f"check-pr-gate: FAIL {e}")
    if not errs:
        print("check-pr-gate: PASS")
    return 1 if errs else 0


if __name__ == "__main__":
    sys.exit(main())
