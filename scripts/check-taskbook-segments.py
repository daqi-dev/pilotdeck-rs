#!/usr/bin/env python3
"""任务书分段强制检查：大包必须按文件切段，每段可独立收货。

踩过的坑：2026-11-05 夜 #1316 跑满 7200 s 被 kill，16 个文件写完但 RESULT / 负控 /
final.patch 全缺；同一晚 #1334 #1336 #1370 #1328 #1339 同类超时。
现有「每 30 分钟 WIP 提交」只防丢进度，不防「一整段跑不完」。所以把切段做成派单前的机检。

用法：
  python3 scripts/check-taskbook-segments.py <任务书> [<任务书> ...]
  python3 scripts/check-taskbook-segments.py --json <任务书>
  python3 scripts/check-taskbook-segments.py --explain <任务书>   # 只打印分类依据
  python3 scripts/check-taskbook-segments.py --print-skeleton     # 打印分段表骨架（派单被拒时贴进补充文件）
  python3 scripts/check-taskbook-segments.py --recent 24 [--root <主仓>]  # 扫最近 24 h 生成的任务书

判定（阈值细节的唯一正本在 scripts/README.md「check-taskbook-segments.py」一节；
一次性论证原件在 _tmp/20261105-taskbook-segments/SEGMENTS.md，回收后以 README 为准）：
  大包 = 任一条成立
    1) 任务书声明 `<!-- pdrs-scope: files=N lines=M est_min=K -->`（可多条，任一条达阈值即大包）
       且 files ≥ 8 或 lines ≥ 1500 或 est_min > 90；
    2) 没有可解析的 scope 行时，出现「约/大约/估计/预计/估算/估/工作量/共计/合计/总计/共 [标点] N 人日」
       → 人日 × 480 min > 90
       （历史任务书的回退信号，多条取最大值）；
    3) 仍判不出时，点名的 `*.rs` 路径 ≥ 8 个（回退启发式）。
  scope 行只认**围栏外的正文**；人日与 .rs 回退看原文（围栏里出现的证据也算，宁可判大）——
  围栏里贴的示例 scope 既不参与分类，也不会把后面的回退信号短路成「小包」。
  scope 行写坏（占位符 / 非整数 / 没写阈值键 / 没收到 `-->` / 没写进 `<!-- -->`）→ 退出 2，不静默退回启发式。
  代码围栏（``` / ~~~）里的分段表与占位符不算数：贴「上一单的示例分段表」不能当成本包的分段表，
  里面残留的 `<段名>` 也不该把合规任务书打回。

分段表要求（`## 分段交付` 一节里的 markdown 表，≥2 段）：
  列：段名 / 涉及文件 / 交付物（含该段自己的 RESULT 片段）/ 预估时长 / 段间依赖（列序不限，按表头定位）
  每段预估 ≤ 90 min：单位认 min/分钟/m/h/小时，区间（`–`/`~`/`至`/`/`/`上限`）取最大值，
  明确加法（`+`/`＋`/另加/再加/再补）按子表达式相加、每段内部再取 max（`40–50 min + 20 min` = 70，
  裸「增加 / 加班」不算加法），连写（`1 h 30 min`、`1h30m`）也相加；表头写 `(h)` / `(hrs)` / `工时` 时
  裸数字按小时算，`(人日)` / `工作日` 报「读不出」；不认的单位按「读不出」报错；
  另需 `## 续跑` 一节：具体的进度文件路径（带目录的 `…progress.md` / `…进度.md`，不许留 `<…>` / `{…}` / `…`，
  不许写成「无需 / 不用 / 不会 …」这类否定句，也不许写 URL 片段）+「第 N 段失败从哪继续」的最小学样
  （失败 / 继续 / 接续 / 恢复 / resume；同样不许写在否定句里）。

退出码：0 通过（含「小包，无需分段」，`--recent` 扫过但没扫到也算 0）· 1 命中大包但分段表缺失/不全 ·
2 任务书读不了、为空、读取期间被改写、scope 行写坏，或 `--recent` 的递归扫描中断（「扫不了」不许
冒充「没扫到」）· 3 用法错误（`--root` 只与 `--recent` 同用）
（多个文件混合时 2 优先于 1，ERROR 比 FAIL 更需要人看）
"""

import bisect
import json
import math
import os
import re
import stat
import sys
from pathlib import Path

SCOPE_FILES = 8
SCOPE_LINES = 1500
SCOPE_MIN = 90          # 单包允许的最大预估时长（min）
SEGMENT_MAX_MIN = 90
FALLBACK_RS_PATHS = 8
PERSON_DAY_MIN = 480    # 1 人日 ≈ 8 h；只用于把历史任务书里「约 N 人日」换算成分钟
MAX_BYTES = 2 * 1024 * 1024  # 任务书不可能这么大；超了按「读不了」处理，不整文件读进内存
FRESH_SEC = 10          # --recent 跳过「刚写几秒」的文件：写方不是原子写时读到半截会误判

# ---- 占位符 ----------------------------------------------------------------
# 只认「像占位符」的尖括号/花括号内容：含中日韩字，或是已知占位词（N / 日期 / 待填 …）。
# 这样 `Vec<u8>`、`Result<(), E>`、`<br>`、`<https://…>` 这些仓库常态不会被误判（假 FAIL 会拒派合规任务书）。
CJK = re.compile(r"[㐀-鿿]")
# 不设长度上限：上限之外的内容既不算占位符、又不会被遮蔽 → 超长的 `{请填写…}` 静默通过（假 PASS），
# 超长的 `<https://…/a...>` 里的 `…` 又会被裸占位规则命中（假 FAIL）。两头都要堵，改成无上限。
ANGLE = re.compile(r"<([^<>\n]*)>")
BRACE = re.compile(r"\{([^{}\n]*)\}")
# 全角尖括号（`＜待填＞`）与中文方括号（`【待填】`）：Unicode 归一化不做，直接加同义模式。
FW_ANGLE = re.compile(r"[＜〈]([^＜＞〈〉\n]*)[＞〉]")
# 中文方括号里可能是正文（`【重要】`），只认明确的占位词，不套「含中文即占位」
CJK_BRACKET = re.compile(r"【([^【】\n]*)】")
# 只认明确的占位词：单字「待」「略」会把 `【策略】` / `【等待】` / `【忽略】` 这类正文判成占位符。
PLACEHOLDER_HINT = re.compile(r"待填|待补|待定|待完善|待补充|填写|占位|省略号|略去|tbd|todo|xxx", re.I)
PLACEHOLDER_WORDS = {
    "n", "n+1", "n-1", "nn", "tbd", "todo", "xxx", "yyy", "???", "待填", "待补", "空",
    "date", "name", "dir", "path", "head", "sha", "reason", "file", "id", "num", "idx", "x",
}
# 裸省略号 / 问号 / 英文占位词（CJK 前后没有 \b，用显式 ASCII 环视）
BARE_PLACEHOLDER = re.compile(r"\.\.\.|…|。。。|\?\?\?|？？？|待填|待补|待定|"
                              r"(?<![A-Za-z])(?:tbd|todo|xxx)(?![A-Za-z])", re.I)


def _looks_placeholder(content):
    c = content.strip().strip("`'\"")
    if not c:
        return True
    if CJK.search(c):
        return True
    if set(c) <= set(".．。…·"):   # `<...>` / `{…}` / `<。。。>`：纯点号就是占位
        return True
    return c.lower() in PLACEHOLDER_WORDS


def _looks_placeholder_cjk_bracket(content):
    """`【…】` 里只有明确的占位词才算占位：`【重要】`、`【S1】` 这类正文不误伤。"""
    c = content.strip()
    if not c:
        return True
    if PLACEHOLDER_HINT.search(c):
        return True
    if set(c) <= set(".．。…·"):
        return True
    return c.lower() in PLACEHOLDER_WORDS


def placeholder_in(text):
    """返回第一个占位符字面量；没有返回 None。"""
    for m in ANGLE.finditer(text):
        if _looks_placeholder(m.group(1)):
            return m.group(0)
    for m in BRACE.finditer(text):
        # 空花括号只在**整格就是 `{}`** 时算占位：`format!("{}", x)` 里的 `{}` 是代码，
        # 误判会把合规任务书打回（假 FAIL）。
        if not m.group(1).strip() and text.strip() != m.group(0):
            continue
        # Rust 内联命名参数（`format!("{x}")`）：花括号紧贴引号、内容是单个标识符，**且这个引号在
        # 表达式上下文里**（前一个字符是 `(` `,` `=` `[` `{` `+`）才按代码放行。
        # 只看「紧贴引号」会把正文里的 `按 "{name}" 填`、`"{file}"`、`"{N}"` 一并当代码跳过——
        # 这三个词正在占位词表里，交付物格写成引号裹占位词就假 PASS。
        # 字符串前缀（`f"{name}"` / `rb"{path}"`）也是表达式上下文：前一个字符是 ASCII 标识符字符。
        if (re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]*", m.group(1).strip())
                and m.start() > 1 and text[m.start() - 1] in "\"'"
                and (text[m.start() - 2] in "(,=[{+"
                     or (text[m.start() - 2].isascii()
                         and (text[m.start() - 2].isalnum() or text[m.start() - 2] == "_")))
                and m.end() < len(text) and text[m.end()] in "\"'"):
            continue
        if _looks_placeholder(m.group(1)):
            return m.group(0)
    for m in FW_ANGLE.finditer(text):
        if _looks_placeholder(m.group(1)):
            return m.group(0)
    for m in CJK_BRACKET.finditer(text):
        if _looks_placeholder_cjk_bracket(m.group(1)):
            return m.group(0)
    # 裸占位符前先把尖括号/花括号整段抹掉：`<https://…>`、`Vec<u8>` 里的 `…` 不是占位符
    masked = ANGLE.sub(lambda m: " " * len(m.group(0)), text)
    masked = BRACE.sub(lambda m: " " * len(m.group(0)), masked)
    masked = FW_ANGLE.sub(lambda m: " " * len(m.group(0)), masked)
    masked = CJK_BRACKET.sub(lambda m: " " * len(m.group(0)), masked)
    m = BARE_PLACEHOLDER.search(masked)
    return m.group(0) if m else None


# ---- scope 行 --------------------------------------------------------------
# 容忍大小写、`pdrs-scope` / `pdrs_scope` / `pdrs scope`、半角与全角冒号、半角与全角连字符、冒号前后空格。
DASHES = r"[\s_\-－—‐‑]"
SCOPEISH = re.compile(rf"pdrs{DASHES}*scope", re.I)
COMMENT = re.compile(r"<!--(?P<body>.*?)-->", re.S)
# 行内代码（`` `…` `` / ``` ``…`` ```）是第四种「贴示例」形态：示例 scope 写在反引号里同样不是本包声明。
# 只对 scope 解析屏蔽（人日 / `.rs` 回退、续跑路径仍看原文——反引号里常是真实路径）。
# 反引号串长度必须配对（CommonMark）：修前只认单反引号，``` ``…`` ``` 里的示例 scope 照样被当前置声明。


def _backtick_runs(text):
    """所有极大反引号串 [(start, length)]。"""
    n, runs, i = len(text), [], 0
    while i < n:
        a = text.find("`", i)
        if a < 0:
            break
        j = a
        while j < n and text[j] == "`":
            j += 1
        runs.append((a, j - a))
        i = j
    return runs


def mask_inline_code(text):
    """把行内代码整段抹成空格（换行保留）。没有配对串的反引号按字面量处理。

    先建「长度 → 起点」表再二分找闭合串：修前每个开串都往后重扫一遍，长度 1..k 的开串
    全不闭合时是 O(k·n)（2 MiB 病态输入实测 53 s，派单闸门跟着卡住）。
    """
    runs = _backtick_runs(text)
    by_len = {}
    for st, ln in runs:
        by_len.setdefault(ln, []).append(st)
    out = list(text)
    consumed = 0                                 # 已经抹掉的区间末尾：里面的开串不再当开串
    for a, run in runs:
        if a < consumed:
            continue
        pos = by_len[run]
        k = bisect.bisect_right(pos, a)          # 闭合串必须在开串之后
        if k >= len(pos):
            continue                             # 没有闭合：这串反引号是字面量
        close = pos[k]
        for x in range(a, close + run):
            if out[x] != "\n":
                out[x] = " "
        consumed = close + run
    return "".join(out)
SCOPE_BODY = re.compile(rf"^\s*pdrs{DASHES}*scope\s*[:：]?\s*(?P<rest>.*)$", re.I)
# 关键字与数字之间允许标点（`工作量：3 人日` / `预计,2 人日`）；「估计」要单列（裸「估」后跟「计」不匹配）。
PERSON_DAY = re.compile(r"(?:约|大约|估计|预计约|预计|估算|估|工作量|共计|合计|总计|共)\s*[:：,，]?\s*"
                        r"([0-9]+(?:\.[0-9]+)?)\s*(?:个)?\s*(?:[–\-~至]\s*([0-9]+(?:\.[0-9]+)?)\s*)?人[日天]")
# URL 只吃 ASCII URL 字符：中文顿号/逗号/分号紧跟其后时不许被整串吞掉（否则后面的 .rs 枚举漏计）
URL = re.compile(r"[A-Za-z][A-Za-z0-9+.-]*://[A-Za-z0-9./%#?=&_~:@!$'()*+-]+")
RS_PATH = re.compile(r"[A-Za-z0-9_./\-]+\.rs\b")
# 代码围栏（CommonMark）：闭合行须同字符、长度 **≥** 开栏、独占一行（只许前后空白）。
# 逐行扫（FENCE_OPEN + _fence_spans），正则写不干净：修前要求闭合串与开栏串**完全相等**，
# ```` ``` ```` 开、```` ```` ```` 收的标准写法被判「围栏没有闭合」→ 整份任务书 rc 2（假 ERROR）。
FENCE_OPEN = re.compile(r"[ \t]*(?P<f>`{3,}|~{3,})(?P<info>.*)$")
# 引用块与缩进代码块是 Markdown 的另两种「贴示例」形态：里面出现的 scope 同样是示例，不是本包声明。
# 只抹成空格（行数、偏移不变），人日 / `.rs` 回退仍看原文（宁可判大）。
QUOTED = re.compile(r"(?m)^[ \t]*>.*$")
# 缩进代码块按 CommonMark 要求「前置空行」，且列表续行不算：`1. 进度文件：` 下一行缩进 4 格的路径是正文，
# 一刀切会把它抹掉 → 续跑路径整段消失（假 FAIL）。
INDENTED_HEAD = re.compile(r"(?: {4,}|\t)")
CODE_INDENT = 4
# 列表标记 + 其后的空格：决定「列表项内容列」，缩进代码块的门槛按它 + 4 算
LIST_ITEM = re.compile(r"[ \t]*(?:[-*+]|[0-9]{1,9}[.)])[ \t]+")


def _blank_indented(text):
    """抹掉缩进代码块：前置空行（或文档开头）且缩进达到「当前容器 + 4」。

    列表项里的段落只比标记多 2–3 格，`1. 进度文件：` + 空行 + 4 格路径在 CommonMark 里仍是正文
    （松散列表的段落）。一刀切按 4 格抹会把合规任务书的进度路径抹掉（假 FAIL），所以上一个非空行
    是列表项时，门槛抬到「标记内容列 + 4」（`- ` = 6、`1. ` = 7）。
    """
    out, in_block, need = [], True, CODE_INDENT
    for line in text.splitlines(keepends=True):
        body = line.rstrip("\n")
        head = body[:len(body) - len(body.lstrip(" \t"))]
        if in_block and len(head.expandtabs(4)) >= need:
            out.append(" " * len(body) + line[len(body):])
            continue
        out.append(line)
        if not body.strip():
            in_block = True           # 空行之后可能是缩进代码块（门槛沿用上一个非空行）
            continue
        in_block = False
        m = LIST_ITEM.match(body)
        need = len(m.group(0).expandtabs(4)) + CODE_INDENT if m else CODE_INDENT
    return "".join(out)

SECTION = re.compile(r"^(#{2,3})\s+(.*)$")
# 顺序有讲究：`段间依赖` 里也有「段」，dep 必须先认领，seg 才不会绑到依赖列。
# 每个 key 先按别名优先级找（`段名` 精确 > `段`），且同一列不许被两个 key 认领。
WANT_COLS = {
    "dep": ("依赖",),
    "seg": ("段名", "段"),
    "files": ("文件",),
    "deliver": ("交付物", "交付"),
    "est": ("预估", "预计", "时长"),
}
COL_NAMES = {"seg": "段名", "files": "涉及文件", "deliver": "交付物", "est": "预估时长", "dep": "段间依赖"}


def _fence_spans(text):
    """返回代码围栏跨度 [(start, end, closed)]，按出现顺序。

    闭合行 = 同字符、长度 ≥ 开栏串、独占一行（只许前后空白）。没有闭合行的一直吃到文件尾，
    `closed=False`（调用方据此判「围栏没有闭合」，不能静默把后半段抹掉）。
    """
    lines = text.split("\n")
    starts, pos = [], 0
    for ln in lines:
        starts.append(pos)
        pos += len(ln) + 1
    spans, i = [], 0
    while i < len(lines):
        m = FENCE_OPEN.match(lines[i])
        # 反引号围栏的 info string 里不许再出现反引号（CommonMark），否则这行是行内代码不是围栏
        if not m or (m.group("f")[0] == "`" and "`" in m.group("info")):
            i += 1
            continue
        ch, n = m.group("f")[0], len(m.group("f"))
        close = re.compile(r"[ \t]*" + re.escape(ch) + "{" + str(n) + r",}[ \t]*")
        j = i + 1
        while j < len(lines) and not close.fullmatch(lines[j]):
            j += 1
        if j < len(lines):
            spans.append((starts[i], starts[j] + len(lines[j]), True))
            i = j + 1
        else:
            spans.append((starts[i], len(text), False))
            break
    return spans


_NON_NL = re.compile(r"[^\n]")


def strip_fences(text):
    """抹掉代码围栏 / 引用块 / 缩进代码块里的内容（保留行数与偏移）。

    围栏里贴的是示例：既不算合规分段表，也不该被当占位符打回。引用块（`> …`）与 4 空格缩进块
    是另外两种贴示例的标准写法——修前它们里的示例 scope 会被当成本包声明，把 9 个 `.rs` 的包
    短路成小包 rc 0（与 fix-r6 的围栏示例同一条失效类）。
    """
    spans = _fence_spans(text)
    if spans:                       # 一次建缓冲、逐跨切片替换、末尾只拼一次：修前每跨整串重建
        buf = list(text)            # （`text[:a] + 掩码 + text[b:]`）是 O(跨度数 × n)，病态输入分钟级
        for a, b, _closed in spans:
            buf[a:b] = _NON_NL.sub(" ", text[a:b])
        text = "".join(buf)
    blank = lambda m: " " * len(m.group(0))   # noqa: E731 —— 行内不换行，行数不变
    out = QUOTED.sub(blank, text)
    return _blank_indented(out)


def fence_unclosed(text):
    """有开栏但没有闭合行（一直吃到文件尾）：此时围栏后的正文会被整段抹掉，不能静默判小包。"""
    return any(not closed for _a, _b, closed in _fence_spans(text))


def _parse_scope_body(body):
    """解析一条 scope 行；返回 (dict, error)。"""
    body = body.strip()
    if not body:
        return None, "scope 行为空（应形如 files=16 lines=1124 est_min=180）"
    out = {}
    for tok in re.split(r"[\s,;，；]+", body):
        if not tok:
            continue
        if "=" not in tok and "＝" not in tok:
            return None, f"scope 行里有无法解析的片段：{tok!r}（应形如 files=16 lines=1124 est_min=180）"
        k, v = re.split(r"[=＝]", tok, maxsplit=1)
        # 键名大小写不敏感、`-` 等同 `_`（`EST_MIN` / `est-min` 都认）；**认不出的键一律算写坏**：
        # 旧实现把未知键原样存进 dict，`files=2 estmins=300` 里那个 300 min 的声明被静默吞掉 →
        # 300 min 的包判成小包 rc 0（假 PASS），与「scope 写坏不许静默」相反。
        k, v = k.strip().lower().replace("-", "_"), v.strip()
        if k in out:
            return None, (f"scope 行里 {k} 写了两次（后写的会把先写的阈值吞掉，按「读不了」处理）")
        if k in ("files", "lines", "est_min", "modules"):
            # 位数上限：Python 3.11+ 的 int() 对 >4300 位会抛 ValueError（崩溃冒充 FAIL）
            if not re.fullmatch(r"[0-9]{1,18}", v):
                return None, f"scope 行 {k}={v[:40]!r} 不是整数（不许留占位符）"
            out[k] = int(v)
        else:
            return None, (f"scope 行里有认不出的键 {k[:40]!r}（只认 files / lines / est_min / modules，"
                          "键名大小写与 `-`/`_` 不敏感；写错键等于把阈值吞掉，按「读不了」处理）")
    if not any(k in out for k in ("files", "lines", "est_min")):
        return None, "scope 行至少要写 files= / lines= / est_min= 之一"
    return out, None


def _iter_comments(text):
    """线性扫 `<!-- … -->`，产出 (start, end, body)。body=None 表示没收到 `-->`。

    修前用 `COMMENT`（非贪婪 `.*?` + DOTALL）：每个 `<!--` 都要往后找到 `-->` 或文件尾，
    50 万个未闭合 `<!--`（2 MiB）就是 O(n²)，闸门挂死。
    """
    i = 0
    while True:
        a = text.find("<!--", i)
        if a < 0:
            return
        b = text.find("-->", a + 4)
        if b < 0:
            yield a, len(text), None
            return
        yield a, b + 3, text[a + 4:b]
        i = b + 3


def _indent_kind(text, pos):
    """pos 所在行的缩进性质：`"flat"`（顶格）/ `"example"`（列表项下贴的示例）/ `"prose"`（判不准）。

    `- scope 写法示例：` 下一行缩进 4 格的 `<!-- pdrs-scope … -->` 修前被当成本包声明，
    一条 files=2 的示例就把后面 9 个 `.rs` 的包短路成小包 rc 0（假 PASS）。声明必须顶格写。
    修前切 `text[行首:pos]` 判空：无换行的长行上 `nl` 为空、行首恒为 0，每条 scope 都拷一遍前缀，
    单行 2 MiB 任务书退化成 O(n²)（实测 6.5 s，README 承诺 <1 s）。反向逐字符扫到非空白即返回，
    只在「整段前缀确实全是空白」时才切那一小段，总代价 O(前缀)，全文件线性。

    **前置空行的缩进代码块在 `strip_fences` 里已经抹掉**，走到这里的缩进行只剩两种：
      ① 列表项下面贴的示例（上一个非空行是 `- …` / `1. …`）——按示例忽略；
      ② 段落懒续行（正文下一行缩进 4 格）——CommonMark 里是正文、不是代码块，作者写了 scope
         却被静默丢掉，启发式回退把 est_min=300 的包当小包放行（gate14 GLM P1 实测：
         `# t\\n一些正文\\n    <!-- pdrs-scope: files=6 est_min=300 -->` → scopes=[] rc 0 假 PASS）。
         判不准的一律报 `"prose"`，由调用方报 rc 2（既不静默忽略也不静默采纳）。
    回看上一个非空行最多 4096 字符（够越过任意正常空行段），超了按 `"prose"` 处理。
    """
    i = pos
    while i > 0:
        ch = text[i - 1]
        if ch == "\n":
            break
        if not ch.isspace():
            return "flat"       # 行首到 pos 里有正文：不是缩进行
        i -= 1
    if len(text[i:pos].expandtabs(4)) < CODE_INDENT:
        return "flat"           # 缩进不到 4 格：仍按顶格声明算
    # 往回找上一个非空行，看它是不是列表项
    j, budget = i - 1, 4096
    while j > 0 and budget > 0:
        a = text.rfind("\n", max(0, j - budget), j) + 1
        if text[a:j].strip():
            return "example" if LIST_ITEM.match(text[a:j]) else "prose"
        budget -= j - a + 1
        j = a - 1
    return "prose"              # 到文档开头都是空行：判不准（正常任务书不会这样）


def _line_indent_example(text, pos):
    """缩进行是不是「列表项下贴的示例」——只有这一种可以静默忽略。"""
    return _indent_kind(text, pos) == "example"


def parse_scopes(text):
    """返回 (scopes, errors)。多条 scope 行全部解析；任一条写坏都要报出来。

    「写了 scope 但没写成合法注释」比「没写 scope」危险得多：静默退回启发式会把一个
    est_min=300 的大包当小包放行（#1316 类）。所以下面三种都算写坏（rc 2）：
      ① 出现 `pdrs-scope` 字样但不在 `<!-- -->` 里；② 注释没收到 `-->`；③ 注释体解析不出阈值键。
    """
    errors, scopes = [], []
    spans = []
    nl = [i for i, ch in enumerate(text) if ch == "\n"]
    for a, b, body in _iter_comments(text):
        if body is None:
            continue            # 没收到 `-->`：交给下面的 SCOPEISH 兜底报错（带行号）
        spans.append((a, b))    # 先登记：缩进示例**在**注释里，不能反被报成「没写进注释」
        kind = _indent_kind(text, a)
        if kind == "example":
            continue            # 列表项下贴的示例，不是本包声明
        if kind == "prose":
            line = bisect.bisect_right(nl, a) + 1
            errors.append(f"第 {line} 行的 scope 缩进在正文下面（既不是代码块里的示例、也不是顶格声明）："
                          "要当声明请顶格写，要当示例请放进列表项或空行之后的缩进块")
            continue
        sm = SCOPE_BODY.match(body)
        if not sm:
            if SCOPEISH.search(body):
                errors.append("scope 注释写法不对（应形如 `<!-- pdrs-scope: files=16 est_min=180 -->`）")
            continue
        if "\n" in body.strip():
            errors.append("scope 行必须写在一行里（跨行的注释解析不了阈值）")
            continue
        s, err = _parse_scope_body(sm.group("rest"))
        if err:
            errors.append(err)
        else:
            scopes.append(s)
    # 注释外的 scope 字样（含没收到 --> 的半截注释）。归属判定用二分，不逐条扫 spans：
    # 修前 `any(a <= pos < b for a, b in spans)` 在 N 条注释 × M 条 scope 时是 O(N·M)，
    # 2 MiB 的病态任务书能把派单闸门挂死（实测 >120 s 不返回）。
    starts = [a for a, _b in spans]
    for m in SCOPEISH.finditer(text):
        pos = m.start()
        k = bisect.bisect_right(starts, pos) - 1
        if k >= 0 and spans[k][0] <= pos < spans[k][1]:
            continue
        if _line_indent_example(text, pos):
            continue
        line = bisect.bisect_right(nl, pos) + 1
        if _indent_kind(text, pos) == "prose":
            errors.append(f"第 {line} 行的 scope 缩进在正文下面（既不是代码块里的示例、也不是顶格声明）："
                          "要当声明请顶格写，要当示例请放进列表项或空行之后的缩进块")
            continue
        errors.append(f"第 {line} 行的 scope 标记没写进 `<!-- -->` 注释（或注释没收到 `-->`），"
                      "不能静默忽略——写坏了按「读不了」处理")
    return scopes, errors


def rs_paths(text):
    text = URL.sub(" ", text)  # URL 里的 .rs 不算任务目标
    return {p for p in RS_PATH.findall(text) if not p.startswith("//") and "://" not in p}


def classify(text, fallback_text=None):
    """返回 dict：big(True/False/None) / reason / metrics。

    `text` 找 scope 声明——调用方传**去掉代码围栏**的正文：围栏里贴的是示例，不是本包的声明，
    否则一段示例 `<!-- pdrs-scope: files=2 -->` 会把后面的回退信号整段短路（假 PASS）。
    `fallback_text`（默认同 text）找人日与 .rs 路径：回退方向取「宁可判大」，围栏里出现的证据也算数。
    """
    fb = text if fallback_text is None else fallback_text
    scopes, errors = parse_scopes(mask_inline_code(text))
    metrics = {"scopes": scopes, "scope_errors": errors, "rs_paths": len(rs_paths(fb))}
    if errors:
        return {"big": None, "reason": "；".join(dict.fromkeys(errors)), "metrics": metrics}
    if scopes:
        triggers = []
        for s in scopes:
            if s.get("files", 0) >= SCOPE_FILES:
                triggers.append(f"files={s['files']} ≥ {SCOPE_FILES}")
            if s.get("lines", 0) >= SCOPE_LINES:
                triggers.append(f"lines={s['lines']} ≥ {SCOPE_LINES}")
            if s.get("est_min", 0) > SCOPE_MIN:
                triggers.append(f"est_min={s['est_min']} > {SCOPE_MIN}")
        if triggers:
            return {"big": True, "reason": "；".join(dict.fromkeys(triggers)), "metrics": metrics}
        return {"big": False, "reason": "scope 声明未达阈值", "metrics": metrics}
    # 人日估算可能写多条（前置准备 + 主体）：取最大值，不能只看第一条（前小后大会漏判大包）
    best = None
    for m in PERSON_DAY.finditer(fb):
        val = max(float(m.group(1)), float(m.group(2)) if m.group(2) else 0.0)
        if not math.isfinite(val):
            return {"big": None, "reason": "人日估算数值过大/非法，读不出", "metrics": metrics}
        best = val if best is None else max(best, val)
    if best is not None:
        est_f = best * PERSON_DAY_MIN
        if not math.isfinite(est_f):   # 原始值有限但 ×480 溢出成 inf：math.ceil 会抛 OverflowError
            return {"big": None, "reason": "人日估算数值过大/非法，读不出", "metrics": metrics}
        est = int(math.ceil(est_f))
        metrics["person_day"] = best
        metrics["est_min"] = est
        return {"big": est > SCOPE_MIN,
                "reason": f"人日估算 {best:g} → est_min={est} {'>' if est > SCOPE_MIN else '≤'} {SCOPE_MIN}",
                "metrics": metrics}
    n = metrics["rs_paths"]
    return {"big": n >= FALLBACK_RS_PATHS,
            "reason": f"回退启发式：点名 .rs 路径 {n} 个 {'≥' if n >= FALLBACK_RS_PATHS else '<'} {FALLBACK_RS_PATHS}",
            "metrics": metrics}


def section_text(text, keyword):
    """取以 keyword 开头的 ## 一级小节的正文（到下一个同级或更高级标题为止）。"""
    out, grab, level = [], False, 0
    for line in text.splitlines():
        m = SECTION.match(line)
        if m:
            if grab and len(m.group(1)) <= level:
                break
            if not grab and keyword in m.group(2):
                grab, level = True, len(m.group(1))
                continue
        if grab:
            out.append(line)
    return "\n".join(out) if grab else None


def cells(row):
    row = row.strip()
    if not row.startswith("|"):
        return None
    return [c.strip() for c in row.strip("|").split("|")]


def _col_hit(alt, header_cell):
    """列别名匹配：单字别名（`段`）只认整格，其余按子串。

    `段` 按子串会把「分段文件」认成段名列，文件列再无列可绑 → 合规分段表被判「缺列：涉及文件」。
    """
    return header_cell.strip() == alt if len(alt) <= 1 else alt in header_cell


def is_separator(row):
    """GFM 分隔行：`---` / `:--:` / 单 `-` 都合法；全空行不算分隔行（那是坏行）。"""
    filled = [c for c in row if c]
    return bool(filled) and all(re.fullmatch(r":?-+:?", c) for c in filled)


EST_TOKEN = re.compile(r"(?<![A-Za-z0-9.])([0-9]+(?:\.[0-9]+)?)\s*([A-Za-z一-鿿]*)")
# 小时+分钟的连写（`1h40m` / `1h45min` / `1h30`）：直接切 token 时后一个数字会被 lookbehind
# 挡掉、只读成 60 min。先把「N 小时 M 分」整体改写成两个带单位的 token 再切。
COMPACT_UNIT = re.compile(
    r"(?<![0-9.])(?P<h>[0-9]+(?:\.[0-9]+)?)\s*(?:hours?|hrs?|h|小时|时)\s*(?P<m>[0-9]{1,4})\s*"
    r"(?:minutes?|mins?|min|m|分钟|分)?(?!\s*[0-9])", re.I)
# 区间 / 上限语义取最大值：`80–120 min`、`80/90 min`、`80 min（上限 90 min）`
RANGE_MARK = re.compile(r"[–—~～/]|至|到|上限|不超过|最多|≤|(?<=\d)\s*-\s*(?=\d)")
# 只有 `+` / `＋` / `另加` / `再加` / `再补` 是明确加法；`、，,` 是列表分隔符，`含` 常是「含联调」、
# `和` 常是「S1 和 S2」——把这些当加法会把 `60–80 min，含联调` 加成 140（假 FAIL）。
# 裸「加」是「增加 / 加班」的子串（`60–80 min（增加 20 min 联调）`），不当加法。
ADD_MARK = re.compile(r"\+|＋|另加|再加|再补")
BREAK_CHARS = "（(，,；;"     # 回看窗口的边界：括注开头 / 句读
MAX_EST_CELL = 65536        # 单个估时格子 64 KiB 上限：正常格子 < 100 字符
UNIT_MIN = ("", "min", "mins", "minute", "minutes", "m", "分", "分钟")
UNIT_HOUR = ("h", "hr", "hrs", "hour", "hours", "时", "小时", "个小时")
UNIT_DAY = ("d", "day", "days", "天", "日")


def _unit_minutes(v, unit):
    if unit in UNIT_MIN:
        return v
    if unit in UNIT_HOUR:
        return v * 60
    return None  # 「天 / 日 / d」等未支持单位：宁可报「读不出」，也不许当 min 放行


def _header_unit(header_cell):
    """表头里的单位提示：`预估时长(h)` / `（hrs）` / `（h)` / `/h`。

    与单元格同一张单位表（此前只硬编码 `(h|hr|小时)`，`(hrs)` 的裸数字被当 min 放行）。
    认不出的写法按 min —— 只有括号/斜杠里的独立词才算，`预估时长` 里的「时」不算。
    """
    low = header_cell.lower()
    # 连续中文会被 findall 并成一个 token（`预估工时` != `工时`），所以中文单位按子串判。
    if "人日" in low or "人天" in low or "工作日" in low:
        return "d"         # _unit_minutes 会判「读不出」
    if "工时" in low or "人时" in low or "人小时" in low:
        return "h"
    for tok in re.findall(r"[a-z一-鿿]+", low):
        if tok in UNIT_HOUR:
            return "h"
        if tok in UNIT_DAY:
            return "d"     # _unit_minutes 会判「读不出」
        if tok in UNIT_MIN:
            return ""
    # 认得出的「时长类」表头按 min；**认不出的返回 None**：裸数字不许默默当 min 放行
    # （`预估人天` 曾被读成 min，2 人天=960 min 的段判成 2 min 通过）。
    if any(w in low for w in ("时长", "时间", "耗时", "估时", "分钟")):
        return ""
    return None


def _paren_inside(s):
    """每个位置 `pos` 是否落在同一行的 `（…）` / `(…)` 里（与逐次 rfind 的旧写法同义）。

    旧写法对每个候选位置各做一次 `rfind(o, 0, pos)` + 无上界的 `find(c, start)`：单个长格子
    O(n²)（实测 "60 min + 1 " × 16000 = 176 KB 要 0.42 s，2 MiB 单格分钟级）。这里一次正扫出表，
    查询 O(1)。闭括号恰在 pos 上时不算「在括号里」（旧写法 `end > pos` 不成立）。
    """
    inside = bytearray(len(s))
    for o, c in (("（", "）"), ("(", ")")):
        open_at = -1
        for i, ch in enumerate(s):
            inside[i] |= open_at >= 0     # 只看 [0, i)
            if ch == o:
                open_at = i
            elif ch == c and open_at >= 0:
                open_at = -1
    return inside


def parse_est(cell, default_unit=""):
    """从「预估时长」单元格取分钟数。

    规则：区间/上限（`80–120 min`、`80/90 min`、`上限 90 min`）取最大值；
    并列或连写（`60 min + 40 min`、`1 h 30 min`、`1h30m`）求和——此前一律取 max，
    把「60 + 40 = 100 min」读成 60 min 放行；单值按自身单位换算。
    认不出的单位返回 None（报「读不出」），不许默默当 min。
    """
    if len(cell) > MAX_EST_CELL:      # 单格 64 KiB 以上不是估时：直接判「读不出」（也挡住病态输入）
        return None
    # 千分位分隔符（半角 / 全角）先去掉：`1,000 min` 会被 EST_TOKEN 拆成 `1`（无单位）与 `000`（单位 min），
    # 后面的单位再借给 `1`、求和成 1 min → 1000 min 的段 rc 0（假 PASS）。
    # 分组宽度认 3 位与 4 位：只认 3 位时 `1,0000 min`（中文万位分组）逗号留着 → 同样读成 1 min（假 PASS）。
    cell = re.sub(r"(?<=[0-9])[,，](?=[0-9]{3,4}(?![0-9]))", "", cell)
    cell = COMPACT_UNIT.sub(lambda m: f"{m.group('h')} h {m.group('m')} min", cell)
    # 明确加法先切段：`40–50 min + 20 min` 是 max(40,50)+20=70，不是 40+50+20=110。
    # 括注里的 `+` 是列表（`80 min（S1 + S2 各 40 min）`），切开后后半段只剩被跳过的数字 → 整格「读不出」。
    # 文字加法词（另加 / 再加 / 再补）写在括注里仍然是加法；只有裸 `+` 在括注里才算列表。
    inside = _paren_inside(cell)
    cut = [m.span() for m in ADD_MARK.finditer(cell)
           if not inside[m.start()] or m.group(0) not in ("+", "＋")]
    parts, prev = [], 0
    for a, b in cut:
        parts.append(cell[prev:a])
        prev = b
    parts.append(cell[prev:])
    parts = [x for x in parts if x.strip()]
    if len(parts) > 1:
        total, seen = 0, False
        for part in parts:
            got = parse_est(part, default_unit)   # 每段内部再按区间 / 并列规则算
            if got is None and EST_TOKEN.search(part):
                return None                        # 段里有数字但读不出单位：整格报「读不出」
            if got is not None:
                total += got
                seen = True
        return total if seen else None
    # 括注里的数字多半是说明（`90 min（含 10 min 缓冲）`、`80 min（2 个文件）`、`（S1 和 S2 各 30 min）`），
    # 既不是并列也不是区间：`含 / 各 / 其中 / 包括 / 内含 / 外加` 之后的数字不计入（`不含` 除外），「（…）」里单位不是
    # 时长数字的跳过而不是整格报「读不出」（旧行为会把 90 min 的合规段判成 100 → 拒派）。
    # 回看窗口的边界字符位置表：每个数字 6 次 rfind 是 O(n²)（纯数字长格上尤其明显），
    # 一次列出再用二分取「最近的一个」。
    breaks = sorted(i for i, ch in enumerate(cell) if ch in BREAK_CHARS)
    toks = []
    for m in EST_TOKEN.finditer(cell):
        # 回看窗口到「最近的括注开头 / 句读」为止，不再固定 4 字符：`90 min（含双段联调 10 min）` 里
        # `含` 与 `10` 隔 6 字符，旧窗口看不到 → 10 被计入求和 → 100 > 90 → 合规段被拒（假 FAIL）。
        k = bisect.bisect_left(breaks, m.start())
        head = cell[(breaks[k - 1] if k else -1) + 1:m.start()]
        # `不含 / 未含 / 无含 / 除…含` 是**排除**：那段时间不在主估时里，必须计入（`90 min（不含 30 min 测试）`
        # 真值 120）；写成裸 `[含各]` 会把否定句也跳过 → 边界上的段假 PASS。
        if re.search(r"(?<![不未无除])含|各|其中|包括|内含|外加", head):
            continue
        toks.append((float(m.group(1)), m.group(2).lower(), m.start()))
    if not toks:
        return None
    # 无单位的数字：先看它后面最近的显式单位，没有再看前面，最后用表头暗示的单位
    resolved = []
    for i, (v, u, pos) in enumerate(toks):
        if not u:
            after = next((t[1] for t in toks[i + 1:] if t[1]), "")
            before = next((t[1] for t in reversed(toks[:i]) if t[1]), "")
            u = after or before or default_unit
        if u is None:
            return None    # 表头认不出单位、格子里也没写：报「读不出」，不许当 min 放行
        u = re.sub(r"(?:以内|之内|以下|左右|上下)$", "", u)   # `分钟以内` 的限定词不是单位的一部分
        u = u.strip("至到")   # `60 至 80 分钟`：区间词被并进单位，剥掉后按无单位处理（后面按区间取 max）
        mins = _unit_minutes(v, u)
        if mins is None:
            if inside[pos]:
                continue    # `（2 个文件）` 这类括注不是时长，跳过；整格没有时长数字时才报「读不出」
            return None
        resolved.append((mins, pos))
    if not resolved:
        return None
    if len(resolved) == 1:
        return resolved[0][0]
    if RANGE_MARK.search(cell):
        # 区间 / 上限取最大值（「60–80 min，含联调」里的逗号不算并列）。**区间的每一侧自己可能
        # 就是连写**：`1h30m–1h45m` 是 max(90, 105)=105。一律 `max(所有 token)` 只取到 60，
        # 105 min 的段被当成 60 → rc 0 假 PASS（gate14 DeepSeek P0 实测）。按区间符号的位置分组，
        # 组内求和、组间取最大；纯区间 `80–120 min` 每组一个数，行为不变。
        # 分组边界 = 区间符号 + 句读 / 括注开头：`1h30m–1h45m` 两侧各自求和（105）；
        # `60–80 min（增加 20 min 联调）` 里括注里的 20 是另一句说明，不能并进右侧（仍取 80）。
        marks = sorted([m.start() for m in RANGE_MARK.finditer(cell)] + breaks)
        groups = {}
        for mins, pos in resolved:
            k = bisect.bisect_right(marks, pos)   # pos 落在第几个区间符号之后
            groups[k] = groups.get(k, 0) + mins
        return max(groups.values())
    return sum(mins for mins, _pos in resolved)


NEGATION = re.compile(r"不要|不用|不必|无需|无须|不许|不能|不会|不需要|别写|勿写")
RESUME_FAIL_WORDS = ("失败", "继续", "接续", "恢复", "重跑", "resume")
# 路径允许中文（`…/进度.md`、`…/评审记录/progress.md`）；把 `<>{}}…` 也吃进来，占位形态才能整串识别
PATH_TOKEN = re.compile(r"[A-Za-z0-9_./㐀-鿿<>{}…＜＞｛｝【】〈〉-]+\.md")
NEG_WINDOW = 30         # 只看路径所在分句（路径前 30 字符里最后一个句读之后）：
                        # 「不需要在这里写 progress.md」命中，「…progress.md（不要提交进 git）」不命中


def _looks_url(p):
    """`https://…` 的片段之外，`example.com/2026/progress.md` 这类无 scheme URL 也不算本地路径。"""
    if "://" in p:
        return True
    first = p.split("/", 1)[0]
    return bool(re.fullmatch(r"[A-Za-z0-9-]+(?:\.[A-Za-z0-9-]+)*\.[A-Za-z]{2,}", first))


def resume_problems(resume):
    """`## 续跑` 的最小检查：具体进度文件路径 + 「第 N 段失败从哪继续」的学样。"""
    problems = []
    path, bad_paths = None, []
    for line in resume.splitlines():
        for m in PATH_TOKEN.finditer(line):
            p = m.group(0)
            name = p.rsplit("/", 1)[-1]
            if not re.search(r"progress|进度", name, re.I):   # `PROGRESS.md` / `Progress.md` 同样是进度文件
                continue
            bad = None
            if any(ch in p for ch in "{}<>…＜＞｛｝【】〈〉") or "..." in p:
                bad = "还是占位符"
            elif "/" not in p:
                bad = "只有文件名、没有目录"
            elif p.startswith("//") or _looks_url(p):
                bad = "是 URL 里的片段，不是本地路径"
            elif NEGATION.search(re.split(r"[，。；、：,;]", line[max(0, m.start() - NEG_WINDOW):m.start()])[-1]):
                bad = "写在否定句里"
            if bad:
                bad_paths.append(f"{p}（{bad}）")
            elif path is None:
                path = p
    if not path:
        detail = f"；看到的不合规路径：{'、'.join(bad_paths)}" if bad_paths else ""
        problems.append("`## 续跑` 里没写具体的进度文件路径（要带目录的实例路径，如 "
                        "`_tmp/20261105-issue-1316/progress.md`；不许留 <…> / {…} / …，"
                        f"不许只写文件名，也不许写在「无需 / 不用」这类否定句里）{detail}")
    positive = []
    for line in resume.splitlines():
        # 路径 / 行内代码里的 `resume` / 进度 不算「第 N 段失败从哪继续」的说明：
        # `- 进度文件：_tmp/x/resume-r1/progress.md` 修前命中英文关键字，整节只写路径也 PASS。
        bare = PATH_TOKEN.sub(lambda m: " " * len(m.group(0)), line)
        bare = re.sub(r"`[^`\n]*`", lambda m: " " * len(m.group(0)), bare)
        for clause in re.split(r"[，。；、：,;]", bare):
            if not NEGATION.search(clause):
                positive.append(clause)
    if not any(w in "\n".join(positive).lower() for w in RESUME_FAIL_WORDS):
        problems.append("`## 续跑` 里没写「第 N 段失败从哪继续」（要有 失败 / 继续 / 接续 / 恢复 / 重跑 / resume 之一的说明）")
    return problems


def check_segments(text):
    """返回问题列表（空 = 通过）。缺表与缺续跑要一起报，免得派单人改两遍。"""
    problems = []
    body = section_text(text, "分段交付")
    if body is None:
        problems.append("缺 `## 分段交付` 一节（大包必须按文件切段，模板见 ~/ws/pdrs-ops/templates/taskbook-segments.md）")
    else:
        rows = [cells(line) for line in body.splitlines()]
        rows = [r for r in rows if r is not None]
        header = rows[0] if rows else None
        data = []
        if header is None:
            problems.append("`## 分段交付` 里没有表格（需 markdown 表：段名 / 涉及文件 / 交付物 / 预估时长 / 段间依赖）")
        else:
            for j, r in enumerate(rows[1:]):
                # GFM 分隔行只能紧贴表头（第 2 行）；后面的全 `-` 行是没填的段行，不许整行跳过。
                if j == 0 and is_separator(r):
                    continue
                if not any(r):
                    problems.append(f"分段表第 {len(data) + 1} 行整行为空（不许用空行占位）")
                    continue
                data.append(r)
            col, used = {}, set()
            for key, alts in WANT_COLS.items():
                for alt in alts:   # 别名按优先级试；`段名` 命中前不会退回 `段`
                    hit = next((i for i, h in enumerate(header)
                                if i not in used and _col_hit(alt, h)), None)
                    if hit is not None:
                        col[key], _ = hit, used.add(hit)
                        break
            missing_cols = [COL_NAMES[k] for k in COL_NAMES if k not in col]
            if missing_cols:
                problems.append("分段表缺列：" + "、".join(missing_cols))
            if len(data) < 2:
                problems.append(f"分段表只有 {len(data)} 段；大包至少要切成 2 段（每段可独立收货）")
            default_unit = _header_unit(header[col["est"]]) if "est" in col else ""
            owner = {}
            for i, r in enumerate(data, 1):
                if "files" in col:      # 段间不共写同一文件：共写 = 两段不能各自独立收货
                    for tok_ in FILE_TOKEN.findall(r[col["files"]] if col["files"] < len(r) else ""):
                        key = tok_[2:] if tok_.startswith("./") else tok_
                        if key in owner and owner[key] != i:
                            problems.append(f"第 {i} 段与第 {owner[key]} 段共写同一文件 {key}"
                                            "（段间不共写同一文件，否则不能独立收货）")
                        else:
                            owner.setdefault(key, i)
                if len(r) < len(header):
                    problems.append(f"第 {i} 行字段数 {len(r)} < 表头 {len(header)}")
                for key, idx in col.items():
                    cell = r[idx] if idx < len(r) else ""
                    if not cell:
                        problems.append(f"第 {i} 段「{COL_NAMES[key]}」为空")
                        continue
                    hit = placeholder_in(cell)
                    if hit:
                        problems.append(f"第 {i} 段「{COL_NAMES[key]}」还是占位符：{hit!r}")
                if "est" in col:
                    est_cell = r[col["est"]] if col["est"] < len(r) else ""
                    est = parse_est(est_cell, default_unit)
                    if est is None:
                        problems.append(f"第 {i} 段预估时长读不出（{_clip(est_cell)!r}；"
                                        "只认 min / 分钟 / h / 小时）")
                    elif est > SEGMENT_MAX_MIN:
                        problems.append(f"第 {i} 段预估 {est:g} min > {SEGMENT_MAX_MIN} min（切成更小的段）")
            table_text = "\n".join(x for x in body.splitlines() if x.lstrip().startswith("|"))
            if placeholder_in(table_text) and not any("占位符" in p for p in problems):
                problems.append("分段表里还有占位符（<…> / {…} / … / TBD / TODO），须填实现值")
    resume = section_text(text, "续跑")
    if resume is None:
        problems.append("缺 `## 续跑` 一节（写清进度文件路径 + 第 N 段失败从哪继续）")
    else:
        problems.extend(resume_problems(resume))
    return problems


SKELETON = """<!-- pdrs-scope: files=<待填> lines=<待填> modules=<待填> est_min=<待填> -->

## 分段交付（大包必填：每段一个可独立收货的交付）

| 段 | 涉及文件 | 交付物（含该段自己的 RESULT 片段） | 预估时长 | 段间依赖 |
|----|---------|-----------------------------------|---------|---------|
| S1 <段名> | `src/...` | `S1-RESULT.md` + `final.patch` 的 S1 分片 | 80 min | — |
| S2 <段名> | `src/...` | `S2-RESULT.md` + `final.patch` 的 S2 分片 | 80 min | S1 |

每段收工即可交付，不要求整包做完；段间不共写同一文件。

## 续跑

- 进度文件：`_tmp/<日期>-issue-<N>/progress.md`（每段开工 / 收工各追加一行 `S<n> start|done <时间> <HEAD>`）
- 第 N 段失败：读 progress.md 最后一个 done 段，先 `git apply` 该段 final.patch 分片，再从 N+1 段继续；不重做已 done 段。
"""


# 分段表「涉及文件」列里的文件路径（按扩展名认，避免把 `S1` 这类段名当文件）
# 只认代码扩展名时 `Cargo.lock` / `events.proto` / `001.sql` / `a.html` 不算文件 → 两段共写照样放行（假 PASS）。
FILE_TOKEN = re.compile(
    r"[A-Za-z0-9_./-]*[A-Za-z0-9_-]\."
    r"(?:rs|py|ts|tsx|js|mjs|md|json|toml|ya?ml|sh|txt|lock|proto|sql|html|css)")


def _clip(text, n=80):
    """报错里嵌原文时截断：整格可能上万字符，原样塞进问题列表会把输出撑爆。"""
    return text if len(text) <= n else text[:n] + "…"


def _err(path, detail):
    return {"file": str(path), "rc": 2, "problems": [detail], "class": None}


def check_file(path, ignore_missing=False):
    p = Path(path)
    # 先解析成字面路径：任务书本身可以是符号链接，但 open 时不再跟随末级链接。
    try:
        real = Path(os.path.realpath(p, strict=True))
    except FileNotFoundError:
        if ignore_missing:      # --recent 扫描期间树被回收：跳过，不报 ERROR
            return None
        return _err(path, f"读不了：找不到 {path}")
    except OSError as e:
        return _err(path, f"读不了：{e}")
    try:
        st1 = real.stat()
    except FileNotFoundError:
        if ignore_missing:
            return None
        return _err(path, f"读不了：找不到 {path}")
    except OSError as e:
        return _err(path, f"读不了：{e}")
    if not stat.S_ISREG(st1.st_mode):
        # FIFO / 设备文件在这里 open 会无限阻塞（没有写端），派单闸门会整条挂住。
        return _err(path, "不是普通文件（FIFO / 设备文件），不读")
    if st1.st_size > MAX_BYTES:
        return _err(path, f"任务书 {st1.st_size} 字节超过 {MAX_BYTES}（不整文件读进内存；先删生成物）")
    # 用 fd 复核，不再 stat 路径：stat 与 open 之间路径被换成 FIFO 会永久阻塞（闸门挂死），
    # 同 size/mtime 的 rename 换入又会让「读后复核」失效。O_NONBLOCK 让换入的 FIFO 能 open 成功、
    # 由 fstat 判类型拒掉；O_NOFOLLOW 拒绝末级再被换成符号链接。做法与 scripts/cli-parity/common.py 的 open_plain 一致。
    flags = os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_NONBLOCK", 0)
    try:
        fd = os.open(real, flags)
    except FileNotFoundError:
        if ignore_missing:
            return None
        return _err(path, f"读不了：找不到 {path}")
    except OSError as e:
        return _err(path, f"读不了：{e}")
    try:
        fst = os.fstat(fd)
        if not stat.S_ISREG(fst.st_mode):
            return _err(path, "不是普通文件（FIFO / 设备文件），不读")
        if (fst.st_dev, fst.st_ino) != (st1.st_dev, st1.st_ino):
            return _err(path, "任务书在检查过程中被替换（inode 变了），稍后重跑")
        if fst.st_size > MAX_BYTES:
            return _err(path, f"任务书超过 {MAX_BYTES} 字节（不整文件读进内存；先删生成物）")
        # 限长读：打开后文件继续增长时不至于把几百 MB 读进内存
        with os.fdopen(fd, "rb", closefd=False) as f:
            data = f.read(MAX_BYTES + 1)
        fst2 = os.fstat(fd)
    except OSError as e:
        return _err(path, f"读不了：{e}")
    finally:
        os.close(fd)
    if len(data) > MAX_BYTES:
        return _err(path, f"任务书超过 {MAX_BYTES} 字节（不整文件读进内存；先删生成物）")
    try:
        text = data.decode("utf-8-sig")
    except UnicodeDecodeError as e:
        return _err(path, f"读不了：{e}")
    # 换行归一到 \n：CRLF 任务书的闭合围栏行尾有 \r，`[ \t]*$` 匹配不上 → 围栏判「没闭合」/ 判「已闭合」
    # 全看哪条正则先撞上，同一份文件两套结论。归一后行数不变，偏移只在 \r 处缩短（后续不再用旧偏移）。
    text = text.replace("\r\n", "\n").replace("\r", "\n")
    # 读后复核：挡住 open→read 之间发生的写入（写方正在写的瞬间），以及读完路径上又被换掉的替换。
    # 写方停顿 > FRESH_SEC 后再续写的半截任务书挡不住（窗口内没有新写），见 find_recent 的说明。
    if fst2.st_size != fst.st_size or fst2.st_mtime != fst.st_mtime:
        # 不静默跳过：跳过等于「本轮没检查也算过」（fail-open）。写方补完后重跑即可。
        return _err(path, "任务书在读取过程中被改写（可能还在写），稍后重跑")
    try:
        st2 = real.stat()
    except OSError:
        st2 = None
    if st2 is None:
        # 读完之后文件没了（被删 / 被 remove 后重建）：不能拿旧内容当结论。
        if ignore_missing:
            return None
        return _err(path, f"读不了：读到一半文件被删了（{path}），稍后重跑")
    if (st2.st_dev, st2.st_ino) != (fst.st_dev, fst.st_ino) or st2.st_size != fst.st_size \
            or st2.st_mtime != fst.st_mtime:
        return _err(path, "任务书在读取过程中被替换 / 改写（路径上的文件已不是读到的那个），稍后重跑")
    if all(ch.isspace() or ch in "﻿\x00" for ch in text):
        return _err(path, "任务书是空文件（不是「小包」）")
    if fence_unclosed(text):
        return _err(path, "代码围栏没有闭合（``` 开了没收）；围栏后的 scope / 文件清单会被整段吞掉，先补齐再跑")
    body = strip_fences(text)
    if not body.strip():
        return _err(path, "任务书去掉代码围栏后没有正文（不是「小包」）")
    # scope 只认正文（围栏里的示例不算声明）；人日 / .rs 回退用原文（围栏里的证据也算，fail-safe）
    cls = classify(body, text)
    if cls["big"] is None:
        return {"file": str(path), "rc": 2, "problems": [cls["reason"]], "class": cls}
    if not cls["big"]:
        return {"file": str(path), "rc": 0, "problems": [], "class": cls}
    problems = check_segments(body)
    return {"file": str(path), "rc": 1 if problems else 0,
            "problems": problems, "class": cls}


def _is_dispatch_copy(p):
    """`remote-run/stage|artifacts` 里是同一份任务书的派发副本，`.prev-*` 是旧轮次备份：都不重复检查。

    只看 `_tmp` 之后的路径段：`--root` 前缀里正好有叫 `remote-run` 的目录时不能把全部任务书跳掉。
    """
    parts = p.parts
    try:
        i = len(parts) - 1 - parts[::-1].index("_tmp")   # 取**最后**一个：--root 前缀里也可能叫 _tmp
    except ValueError:
        return False
    return any(part == "remote-run" or part.startswith("remote-run.") or part.startswith(".prev")
               for part in parts[i + 1:-1])


class ScanError(Exception):
    """扫描根下的递归 glob 抛 OSError：区分「扫描失败」（rc 2）与「扫到 0 个」（rc 0）。"""


def _iter_taskbooks(base):
    """自己走目录找 `_build/wt-*/_tmp/**/task.md`；任何目录级 OSError 都抛 ScanError。

    修前用 `Path.glob("_build/wt-*/_tmp/**/task.md")`：pathlib 对 EACCES / 并发删除**静默跳过**
    （本机实测：`chmod 000` 的 `_tmp` 下任务书整类消失，`files=1` rc 0），与 README 承诺的
    「递归扫描中断 → rc 2」相反。这里逐层 `os.scandir` / `os.walk(onerror=...)`，错误不吞。
    """
    out = []
    try:
        with os.scandir(base) as it:
            entries = list(it)
    except FileNotFoundError:
        return out          # 没有 _build/：合法空扫描
    except OSError as e:
        raise ScanError(f"扫 {base} 中断：{e}") from e
    wts = []
    for e in entries:       # is_dir() 自己也会抛 OSError（目录刚被回收 / 权限）：不能崩成 traceback（rc 1）
        try:
            if e.is_dir():
                wts.append(e.path)
        except FileNotFoundError:
            continue
        except OSError as err:
            raise ScanError(f"扫 {e.path} 中断：{err}") from err
    wts.sort()

    def onerror(e):
        # 目录在遍历中被回收（FileNotFoundError）不是任务书的问题，跳过即可；
        # 权限拒绝 / IO 错误则一律中断——那正是「扫不了却被当成没扫到」的入口。
        if isinstance(e, FileNotFoundError):
            return
        raise ScanError(f"递归扫 {getattr(e, 'filename', base)} 中断：{e}")

    for wt in wts:
        if not os.path.basename(wt).startswith("wt-"):
            continue
        for dirpath, dirs, files in os.walk(os.path.join(wt, "_tmp"), onerror=onerror):
            # `os.walk` 默认 followlinks=False：`_tmp` 之下被换成符号链接的子目录**静默不下降**，
            # 里面的任务书整类消失且不报错（又变回「扫不了 = 没扫到」，与 fix-r8 同一个失效类）。
            # 不跟随（防环），也不许静默漏：扫到符号链接目录就中断，让派单侧按 rc 2 处理。
            for d in dirs:
                p = os.path.join(dirpath, d)
                if os.path.islink(p):
                    raise ScanError(f"递归扫到符号链接目录 {p}：不跟随会静默漏掉里面的任务书，"
                                    "请换成真实目录或删掉链接")
            if "task.md" in files:
                out.append(Path(dirpath) / "task.md")
    return sorted(out)


def find_recent(root, hours):
    """扫最近 N 小时生成的任务书。

    刚写 FRESH_SEC 秒内的文件跳过：写方不是原子写时，读到半截 scope 行会误报 rc 2（假 ERROR）。
    文件在扫描与读取之间被删（worktree 回收）时静默跳过——那种「读不了」不是任务书的问题。
    check_file 里的「读后复核」只挡 stat→read→stat 这个毫秒级窗口内的写入；
    **写方停顿超过 FRESH_SEC 再续写**时窗口内没有新写，半截内容仍会被判成「小包」——
    这条边界没堵（要堵得靠写方原子落盘或任务书完成标记），别把它当成已解决。
    为了堵「扫描开始后才创建」，mtime 比 `now - FRESH_SEC` 新的一律跳过（含**未来**的）：
    时钟偏移的文件会晚几轮才扫到，宁可漏一轮也不假 PASS。
    """
    import time
    now = time.time()
    cutoff = now - hours * 3600
    out = []
    # 递归扫：修复 / 续跑 / 重生成轮次的任务书在 _tmp/<单>/fix-rN/、resume*/、regen*/ 下一层，
    # 只认一层会把它们整类漏掉（round-start 9d 兜底轮对这批假 PASS）。
    # 不用 pathlib 的 glob：它会**静默吞掉** EACCES / ENOENT（实测 chmod 000 的 `_tmp` 下任务书
    # 整类消失、rc 0），那样「扫不了」又变回「没扫到」。自己 scandir / walk，每层 OSError 都算中断。
    candidates = _iter_taskbooks(Path(root) / "_build")
    for p in candidates:
        if _is_dispatch_copy(p):
            continue
        try:
            st = p.stat()
        except FileNotFoundError:
            continue        # 扫描与 stat 之间被回收：那份任务书不在了，不是「读不了」
        except OSError as e:
            raise ScanError(f"读 {p} 的 stat 失败：{e}") from e
        # 单边判：只要 mtime 比 `now - FRESH_SEC` 新就跳过（含**未来**的）。旧写法 `0 <= now - mtime`
        # 对扫描开始后才创建的文件（mtime > now）恒为假，半截内容照样被读成「小包」——正是 FRESH_SEC
        # 要挡的窗口。代价：时钟偏移到未来的文件会晚几轮才扫到；宁可漏一轮也不假 PASS。
        if st.st_mtime < cutoff or st.st_mtime > now - FRESH_SEC:
            continue
        out.append(p)
    return out


def usage(msg=None):
    if msg:
        print(msg, file=sys.stderr)
    print("用法：check-taskbook-segments.py [--json] [--explain] [--quiet] [--recent N [--root DIR]] <任务书>...",
          file=sys.stderr)
    return 3


def main(argv):
    args = list(argv[1:])
    flags = {"--json", "--explain", "--quiet", "--print-skeleton"}
    unknown = [a for a in args if a.startswith("--") and a not in flags | {"--recent", "--root"}]
    if unknown:
        return usage(f"未知参数：{' '.join(unknown)}")
    as_json = "--json" in args
    explain = "--explain" in args
    quiet = "--quiet" in args
    if "--print-skeleton" in args:
        if [a for a in args if not a.startswith("--")]:
            return usage("--print-skeleton 不能与任务书参数同用（会让人误以为文件被检查过）")
        print(SKELETON, end="")
        return 0
    hours = None
    root, root_given = ".", False
    rest, i = [], 0
    while i < len(args):
        a = args[i]
        if a == "--recent":
            if i + 1 >= len(args):
                return usage("--recent 需要小时数")
            try:
                hours = float(args[i + 1])
            except ValueError:
                return usage("--recent <小时数>")
            if not (math.isfinite(hours) and hours > 0):
                return usage("--recent <小时数> 要是 > 0 的有限数（nan / inf / 0 / 负数不行）")
            if hours * 3600 <= FRESH_SEC:   # 窗口比新鲜度阈值还短：什么都扫不到，别冒充「通过」
                return usage(f"--recent {hours:g}h 的窗口（{hours * 3600:g} s）不大于新鲜度阈值 "
                             f"{FRESH_SEC} s，任何文件都会被跳过；给个更大的窗口")
            i += 2
            continue
        if a == "--root":
            if i + 1 >= len(args):
                return usage("--root 需要目录")
            root, root_given = args[i + 1], True
            i += 2
            continue
        if not a.startswith("--"):
            rest.append(a)
        i += 1
    if root_given and hours is None:
        return usage("--root 只与 --recent 同用（显式任务书按给定路径检查，不按 root 解析）")
    if hours is not None and not Path(root).is_dir():
        return usage(f"--root 不是目录：{root}")
    scanned = []
    if hours is not None:
        try:
            scanned = [str(p) for p in find_recent(root, hours)]
        except ScanError as e:
            print(f"扫描中断：{e}（不是「没扫到」；本轮兜底没生效）", file=sys.stderr)
            return 2
    files = list(dict.fromkeys(scanned + rest))   # 同一个文件既被扫到又被点名时不检查两遍
    if not files:
        if hours is not None:      # 合法空扫描：没有新任务书不算用法错误
            print(f"（--recent {hours:g}h 在 {root} 下没扫到任务书；刚写不到 {FRESH_SEC} s 的会跳过）",
                  file=sys.stderr)
            if as_json:
                print("[]")        # 按行解析 JSON 的调用方不能收到一行文本 SUMMARY
            elif not quiet and not explain:   # --quiet 只压正文；SUMMARY 照打，「没扫到」与「都通过」要能分辨
                print("SUMMARY files=0 ok=0 fail=0 error=0")
            return 0
        return usage()

    # ignore_missing 只给 find_recent 扫出来的文件：显式点名的任务书不见了要报 ERROR，
    # 否则「没检查」会被当成「通过」（--recent 与显式参数同用时最危险）。
    from_scan, explicit = set(scanned), set(rest)
    results = []
    for f in files:
        # 显式点名的文件永不放行「不存在」：即便它同时也被扫描到
        r = check_file(f, ignore_missing=f in from_scan and f not in explicit)
        if r is not None:
            results.append(r)
    if as_json:
        print(json.dumps(results, ensure_ascii=False, indent=2))
    elif explain:
        names = {0: "OK", 1: "FAIL", 2: "ERROR"}
        for r in results:
            c = r["class"] or {}
            print(f"{names.get(r['rc'], '?')}\t{r['file']}\t大包={c.get('big')}\t依据：{c.get('reason')}")
    elif not quiet:
        for r in results:
            c = r["class"] or {}
            if r["rc"] == 0:
                print(f"OK    {r['file']}（{'大包，分段表合规' if c.get('big') else '小包，无需分段'}；{c.get('reason')}）")
            elif r["rc"] == 2:
                print(f"ERROR {r['file']}：{'；'.join(r['problems'])}")
            else:
                print(f"FAIL  {r['file']}：命中大包（{c.get('reason')}）但任务书不合格")
                for p in r["problems"]:
                    print(f"      - {p}")
    if not as_json and not explain:   # --quiet 只压每个文件的行；SUMMARY 照打，「没扫到」与「都通过」要能分辨
        n_fail = sum(1 for r in results if r["rc"] == 1)
        n_err = sum(1 for r in results if r["rc"] == 2)
        n_ok = sum(1 for r in results if r["rc"] == 0)
        print(f"SUMMARY files={len(results)} ok={n_ok} fail={n_fail} error={n_err}")
    if any(r["rc"] == 2 for r in results):
        return 2   # ERROR 比 FAIL 更需要人看：批量里混着时不许被 rc=1 吞掉
    if any(r["rc"] == 1 for r in results):
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
