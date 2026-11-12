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

  4. 修复轮字段（#3056，长尾收口卡「机检落点」）：正文第一个非空行含 `fix-r<N>` 即判为修复轮 PR
     （提交标题 `fix(...) #N fix-rM：…`，PR 正文首行即标题回显；正文不回显标题则漏判——fail-open，
     与「防漏跑不防伪造」同一威胁模型）。修复轮 PR 的每个结果块须带 `fixtype=<枚举式|结构性|不适用>`
     （字面三值，子类字样一律写「结构性」）；N≥4 另须正文恰一个 `<!-- pdrs-residual: <非空> -->`
     （无上一轮写字面值「无上一轮」，空白与占位符不算，围栏 / 引用块里的示例不算声明——口径与任务书侧
     check-taskbook-segments.py 同一正本）。豁免轮（waiver）的修法类型写在豁免标记内
     （`fixtype=<三值>`，值后须空格或结尾分隔），豁免理由长度不计 fixtype 词汇；标记内声明了 fixtype
     就查值（声明即查，是否必填看修复轮判定）。
  5. `fixtype=不适用` 的合法性按 diff 级路径检查（#3056 义务 4）：以 `scripts/ci_scope.py` 的
     docs_only 分类为唯一口径，用 --base/--head/--repo 复算 `git diff` 路径集，只碰白名单路径才放行；
     缺 --base/--head/--repo、git 失败、diff 为空、分类器加载失败一律 fail-closed 拒收（宁拒不漏）。
  6. 多块一致（#3056 G）：修复轮 PR 贴多个结果块凑双谱系/分批/复跑时，全块须同一 head=，
     或全块声明同一 patch= 指纹——不同头且不同指纹的块不得拼作「末轮评审覆盖齐」。

输入：--body-file FILE ｜ --pr N（调 gh pr view，只读）｜ 环境变量 PR_BODY（CI 用，避免 shell 插值）
      --head / PR_HEAD、--base / PR_BASE（CI 传 PR 的 head 与 base SHA；补丁指纹在 --repo 里算）
      --repo（算补丁指纹与「不适用」路径检查的 git 仓库，须含 base 与 head 两个提交）
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

# #3056 修复轮字段（长尾收点卡「机检落点」）：修法类型只接受字面三值。
FIXTYPE_VALUES = ("枚举式", "结构性", "不适用")
# PR 侧修复轮识别：正文第一个非空行含 fix-r<N>（词边界防 fix-r10 匹配成 fix-r1）。
# 边界用 ASCII 字符集而不是 \w：Python 的 \w 含 CJK，中文紧贴（「修复fix-r5：」「fix-r5轮」）时
# \w 边界把整轮判没 → fixtype/residual/N≥4 全跳、闸门假 PASS（DS boundary P0，Python 3.14 实测
# re.match(r"\w", "复") 为 True）。中文不是 fix-r 的词组成部分，ASCII 边界足够。
FIXROUND_RE = re.compile(r"(?<![A-Za-z0-9_-])fix-r(\d+)(?![A-Za-z0-9_-])")
RESIDUAL_MARKER = re.compile(r"^[ \t]*<!--\s*pdrs-residual:(.*?)-->[ \t]*$", re.M)
# 值域 = 字面三值的字符集：紧贴的中文标点/后续文字不并进值（DS boundary#3）——
# 「fixtype=结构性，纯文档」里值停在 结构性，「，纯文档」由 check() 的值后终止检查拒收（判缺/非法）。
WAIVER_FIXTYPE = re.compile(r"fixtype=([" + re.escape("".join(FIXTYPE_VALUES)) + "]+)")


def fix_round_from_body(body: str):
    """正文第一个非空行里的 fix-r<N>；没有则 None。首行不回显提交标题会漏判（fail-open，见 docstring 规则 4）。"""
    for line in body.split("\n"):
        s = line.strip()
        if s:
            m = FIXROUND_RE.search(s)
            return int(m.group(1)) if m else None
    return None


def _kv(attrs: str) -> dict:
    """注释键值全量解析（#200）：`key=value` 空白分隔，同键多次收集成列表（调用方判重复）。"""
    kv: dict[str, list[str]] = {}
    for tok in attrs.split():
        k, sep, v = tok.partition("=")
        if sep:
            kv.setdefault(k, []).append(v)
    return kv


def _load_ci_scope():
    """懒加载同目录 ci_scope.py（「不适用」判定的唯一口径，不复制其路径白名单）；任何失败返回 None（fail-closed）。"""
    try:
        import importlib.util
        p = os.path.join(os.path.dirname(os.path.abspath(__file__)), "ci_scope.py")
        spec = importlib.util.spec_from_file_location("pdrs_ci_scope_3056", p)
        if spec is None or spec.loader is None:
            return None
        mod = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(mod)
        return mod
    except Exception:  # noqa: BLE001 —— 加载失败即 fail-closed，由调用方拒收
        return None


def _load_taskbook():
    """懒加载同目录 check-taskbook-segments.py（占位符判定 placeholder_in 与围栏屏蔽 strip_fences 的正本，
    不复制实现——DS sibling#1/boundary#2）；任何失败返回 None（调用方 fail-closed 拒收）。
    该模块自身只做模块级常量装配（含惰性加载本文件取 FIXTYPE_VALUES），加载无副作用。"""
    try:
        import importlib.util
        p = os.path.join(os.path.dirname(os.path.abspath(__file__)), "check-taskbook-segments.py")
        spec = importlib.util.spec_from_file_location("pdrs_taskbook_for_gate_3056", p)
        if spec is None or spec.loader is None:
            return None
        mod = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(mod)
        return mod
    except Exception:  # noqa: BLE001 —— 加载失败即 fail-closed，由调用方拒收
        return None


def residual_errors(body: str, fix_round) -> list[str]:
    """N≥4 的 pdrs-residual 检查（blocks 与 waiver 两分支共用，不再各写一份）。

    口径与任务书侧 check-taskbook-segments.check_fix_fields 同一正本：围栏 / 引用块 / 缩进块里的
    示例不算声明（strip_fences 后再找标记），占位符不算答（placeholder_in）。任务书模块加载失败
    按 fail-closed 拒收（宁拒不漏，与 _load_ci_scope 同款）。"""
    if fix_round is None or fix_round < 4:
        return []
    tb = _load_taskbook()
    if (tb is None or not hasattr(tb, "strip_fences") or not hasattr(tb, "placeholder_in")
            or not hasattr(tb, "RESIDUAL_NONE")):
        return ["pdrs-residual 口径模块（check-taskbook-segments.py）加载失败（fail-closed 拒收）"]
    errs: list[str] = []
    found = RESIDUAL_MARKER.findall(tb.strip_fences(body))
    if not found:
        errs.append(f"修复轮 fix-r{fix_round}（N≥4）PR 正文缺 <!-- pdrs-residual: 上轮残差判读 -->（无上一轮写字面值「无上一轮」）")
    elif len(found) > 1:
        errs.append("pdrs-residual 标记出现多次（至多一次）")
    else:
        content = found[0].strip()
        if not content:
            errs.append("pdrs-residual 内容是空白串（无上一轮写字面值「无上一轮」，空答不算答）")
        elif content != tb.RESIDUAL_NONE:
            hit = tb.placeholder_in(content)
            if hit:
                errs.append(f"pdrs-residual 内容还是占位符：{hit[:80]!r}（写实际判读；无上一轮写字面值「无上一轮」）")
    return errs


def check_na_path(base: str, repo: str, head: str) -> list[str]:
    """`fixtype=不适用` 的 diff 级路径检查（#3056 义务 4）：ci_scope 语义为唯一口径，fail-closed。"""
    if not (base and head and repo):
        return ["fixtype=不适用 但缺 --base/--head/--repo，无法按 ci_scope 语义复算 diff 路径（fail-closed 拒收）"]
    mod = _load_ci_scope()
    if mod is None or not hasattr(mod, "docs_only_for_range"):
        return ["fixtype=不适用 但 ci_scope.py 加载失败（fail-closed 拒收）"]
    if not mod.docs_only_for_range(repo, base, head):
        return ["fixtype=不适用 与 diff 路径矛盾：改动不限于文档白名单（ci_scope 唯一口径）——触及代码路径不得写不适用"]
    return []


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
    kv = _kv(attrs)
    one = {k: v[0] for k, v in kv.items() if len(v) == 1}
    for k in ("head", "roles", "readonly", "runner", "model"):
        if len(kv.get(k, [])) != 1:
            errs.append(f"结果块键 {k} 须恰好出现一次（现 {len(kv.get(k, []))} 次）；重跑 run-review-roles.py")
    if len(kv.get("patch", [])) > 1:
        errs.append("结果块键 patch 至多出现一次；重跑 run-review-roles.py")
    # #3056 义务 E：fixtype= 键校验——只要声明就须是字面三值（是否必填看 check() 的修复轮判定）
    if len(kv.get("fixtype", [])) > 1:
        errs.append("结果块键 fixtype 至多出现一次；重跑 run-review-roles.py")
    for v in kv.get("fixtype", []):
        if v not in FIXTYPE_VALUES:
            errs.append(f"结果块 fixtype={v} 不是字面三值（枚举式 / 结构性 / 不适用）；「结构派生」等子类字样一律写「结构性」")
    if one.get("readonly", "tools") != "tools":
        errs.append(f"结果块 readonly={one['readonly']}：只接受 readonly=tools（执行层工具白名单）")
    # codex（官方 GPT 谱系，ops 侧 `codex exec` 入口）不在 run-review-roles.py 的
    # runner 表里，但当 zai/anthropic/deepseek/minimax 全是作者线时它是可用的非作者
    # 谱系（2026-11-06 GLM 作者单实测）。这里只登记组合，四项 PASS、head/patch 与
    # 角色齐全的判据一条不减。
    prefix = {"zai": "glm-", "anthropic": "claude-", "minimax": "MiniMax-",
              "deepseek": "deepseek-", "codex": "gpt-"}
    rn, md = one.get("runner"), one.get("model", "")
    # 前缀后至少要有一个字符：`model=gpt-` 这种空后缀不算登记过的谱系（#1599 r4 评审 P3）。
    if rn is not None and (rn not in prefix or len(md) <= len(prefix[rn])
                           or not md.startswith(prefix[rn])):
        errs.append(f"结果块 runner={rn} model={md} 不是已知组合（zai→glm-*，anthropic→claude-*，minimax→MiniMax-*，deepseek→deepseek-*，codex→gpt-*）")
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


def check(body: str, head: str = "", patch: str = "", base: str = "", repo: str = "") -> list[str]:
    errs: list[str] = []
    # 换行归一到 \n：CRLF 正文的行尾 \r 会让 RESIDUAL_MARKER 的行尾锚定匹配不上（假 FAIL）；
    # 任务书侧 check-taskbook-segments.py 的换行归一是先例。后续所有正则都吃归一后的正文。
    body = body.replace("\r\n", "\n").replace("\r", "\n")
    blocks = list(MARKER.finditer(body))
    fix_round = fix_round_from_body(body)
    w = WAIVER.search(body)
    if blocks:
        # 分批（--paths）评审可贴多个结果块：逐块校验，任一无效即拒
        for n, m in enumerate(blocks, 1):
            tag = f"结果块 {n}/{len(blocks)}：" if len(blocks) > 1 else ""
            errs.extend(tag + e for e in check_block(m.group(1), head, patch))
        # ---- #3056 修复轮字段（义务 E/G；详见 docstring 规则 4-6）----
        kvs = [_kv(m.group(1)) for m in blocks]
        declared = [v for kv in kvs for v in kv.get("fixtype", [])]
        if fix_round is not None:
            for n, kv in enumerate(kvs, 1):
                if not kv.get("fixtype"):
                    tag = f"结果块 {n}/{len(blocks)}：" if len(blocks) > 1 else "结果块："
                    errs.append(tag + f"修复轮（fix-r{fix_round}）须带 fixtype=<枚举式|结构性|不适用>；重跑 run-review-roles.py --fixtype …")
            # G：多块凑双谱系/分批/复跑须同头或同一补丁指纹（#3056 阻塞前置）
            if len(blocks) > 1:
                heads = [kv.get("head", [""])[0] for kv in kvs]
                patches = [kv.get("patch", [""])[0] for kv in kvs]
                if not (len(set(heads)) == 1 or (all(patches) and len(set(patches)) == 1)):
                    errs.append(f"修复轮多块不一致（#3056 G）：块头 {heads} 各不相同且补丁指纹 "
                                f"{['缺' if not p else p[:12] for p in patches]} 不统一——分批/复跑拼块须覆盖同一 head 或同一 patch 指纹")
            # N≥4：上轮残差判读（PR 正文，恰一个、非空白、非占位符、围栏内示例不算声明）
            errs.extend(residual_errors(body, fix_round))
        # 义务 4：fixtype=不适用 的 diff 级路径检查（声明即查，与轮数判定无关——
        # 「不适用」本身断言了 docs-only diff，该断言可复算就必须复算）
        if any(v == "不适用" for v in declared):
            errs.extend(check_na_path(base, repo, head))
    elif w:
        reason = w.group(1).strip()
        # 声明即查（MM sibling#7；与结果块侧 :check_block 的 fixtype 校验同口径）：标记内声明了
        # fixtype 就查值，是否必填才看修复轮判定——非修复轮写 fixtype=结构派生 / 不适用 也不再静默放行。
        m2 = WAIVER_FIXTYPE.search(reason)
        fv = m2.group(1) if m2 else None
        if fix_round is not None and not m2:
            # 义务 3：waiver 轮载体——修法类型写在豁免标记内；豁免理由长度不计 fixtype 词汇
            errs.append(f"修复轮（fix-r{fix_round}）的豁免标记须携带修法类型：fixtype=<枚举式|结构性|不适用>")
        if m2:
            # 值域已收紧到三值字符集：紧贴文字并不进值，但值后若还跟着非空白（「结构性，纯文档」
            # 「结构派生」）说明写的不是字面三值载体——判缺/非法而不是把尾巴并进值放行（DS boundary#3）
            tail = reason[m2.end():m2.end() + 1]
            if fv not in FIXTYPE_VALUES or (tail and not tail.isspace()):
                why = "值后不得紧跟其它文字（用空格或结尾分隔）" if tail and not tail.isspace() \
                    else "「结构派生」等子类字样一律写「结构性」"
                errs.append(f"豁免 fixtype={fv}{tail if tail and not tail.isspace() else ''} 不是字面三值载体"
                            f"（枚举式 / 结构性 / 不适用；{why}）")
        if fix_round is not None:
            # N≥4：上轮残差判读（同 blocks 分支口径：恰一个、非空白、非占位符、围栏内示例不算声明）
            errs.extend(residual_errors(body, fix_round))
        plain = WAIVER_FIXTYPE.sub("", reason).strip()
        if len(plain) < 8:
            errs.append("豁免理由太短（≥8 字，不计 fixtype 词汇）")
        else:
            print(f"check-pr-gate: 豁免——{plain}")
        if fv == "不适用":
            errs.extend(check_na_path(base, repo, head))
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
    ap.add_argument("--base", default=os.environ.get("PR_BASE", ""), help="PR base SHA；与 --head 同给时算当前补丁指纹（head 不等时比对）并复算 fixtype=不适用 的 diff 路径")
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
    errs = check(body, a.head, patch, a.base, a.repo)
    for e in errs:
        print(f"check-pr-gate: FAIL {e}")
    if not errs:
        print("check-pr-gate: PASS")
    return 1 if errs else 0


if __name__ == "__main__":
    sys.exit(main())
