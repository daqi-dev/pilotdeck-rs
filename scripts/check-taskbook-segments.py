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

退出码：0 通过（含「小包，无需分段」，`--recent` 扫过但没扫到也算 0）· 1 命中大包但分段表缺失/不全，
或修复轮任务书缺必填字段 ·
2 任务书读不了、为空、读取期间被改写、scope 行写坏、路径段 fix-r 后 ≥19 位数字（位界 {1,18} 外，
与 PR 侧 check-pr-gate 同一结论 fail-closed，fix-r8），或 `--recent` 的递归扫描中断（「扫不了」不许
冒充「没扫到」）· 3 用法错误（`--root` 只与 `--recent` 同用）
（多个文件混合时 2 优先于 1，ERROR 比 FAIL 更需要人看）

修复轮字段（#3056 长尾收口卡「机检落点」，置于 big/small 分支之前——小包同受检）：
任务书路径段全匹配 fix-r<N>[b] 即修复轮（b 为接续轮后缀，算第 N 轮；ops dispatch-fix 生成于
_tmp/<日期>-issue-<N>/fix-rN[/b]/task.md；词表与 gate 侧 FIXROUND_RE 单源），
须含恰好一条 `<!-- pdrs-fixtype: 枚举式|结构性|不适用 [论证] -->`（独占一行；值只认字面三值，
「结构派生 / 自动派生」等子类字样一律写「结构性」；枚举式附边界论证、结构性附词表核验，空白 /
占位符不算）；fix-rN 的 N≥4 另须 `<!-- pdrs-residual: <非空判读> -->`（恰好一次；无上一轮时允许
字面值「无上一轮」，空答不算答）。「不适用」的 diff 级路径合法性（只碰文档类路径，白名单只有
state/board.md）由 PR 侧 scripts/check-pr-gate.py 按 scripts/ci_scope.py 的分类语义判定——任务书
先于实现存在，本检查器不做路径判定。非修复轮任务书带这些标记不报错（模板 / 骨架可能含示例）。
"""

import bisect
import json
import math
import os
import posixpath
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
# 全角花括号（`｛待填｝`）：`## 续跑` 的路径侧早就把 `｛｝` 当占位，分段表侧没有同义模式 →
# `｛请填写本段涉及的文件｝` 整格判不出占位（两家 boundary 各自独立报到，fail-open）。
FW_BRACE = re.compile(r"｛([^｛｝\n]*)｝")
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


def _code_string_context(text, i):
    """`"{x}"` 的左引号前一个字符是 `text[i]`：它在**代码**语境里吗（而不是正文里给词加引号）？

    修前 `( , = [ { +` 一律放行：正文里写 `("{file}")`（前面是裸 `(`）也被当成 `format!` 命名参数
    跳过 → 占位漏判（假 PASS，gate21 GLM boundary P2 实测）。ASCII `(` 再要求它本身是一次调用：
    前面是标识符或 `!`（`format!(` / `println!(`）。
    """
    ch = text[i]
    if ch in "=,{[+":
        return True
    if ch == "(":
        return i > 0 and (text[i - 1] == "!"
                          or (text[i - 1].isascii() and (text[i - 1].isalnum() or text[i - 1] == "_")))
    # 字符串前缀（`f"{name}"` / `rb"{path}"`）：前一个字符是 ASCII 标识符字符
    return ch.isascii() and (ch.isalnum() or ch == "_")


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
                and _code_string_context(text, m.start() - 2)
                and m.end() < len(text) and text[m.end()] in "\"'"):
            continue
        if _looks_placeholder(m.group(1)):
            return m.group(0)
    for m in FW_ANGLE.finditer(text):
        if _looks_placeholder(m.group(1)):
            return m.group(0)
    for m in FW_BRACE.finditer(text):
        if _looks_placeholder(m.group(1)):
            return m.group(0)
    for m in CJK_BRACKET.finditer(text):
        if _looks_placeholder_cjk_bracket(m.group(1)):
            return m.group(0)
    # 裸占位符前先把尖括号/花括号整段抹掉：`<https://…>`、`Vec<u8>` 里的 `…` 不是占位符
    masked = ANGLE.sub(lambda m: " " * len(m.group(0)), text)
    masked = BRACE.sub(lambda m: " " * len(m.group(0)), masked)
    masked = FW_ANGLE.sub(lambda m: " " * len(m.group(0)), masked)
    masked = FW_BRACE.sub(lambda m: " " * len(m.group(0)), masked)
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


# 段落边界（CommonMark）：空行与 ATX 标题行都断开段落，行内代码不能跨段配对。
# 修前只在整篇里按「同长度反引号串」配对：正文里一个笔误的未闭合 `` ` `` 会和**后面任意段落**的
# 反引号配成一对，把中间顶格的 `<!-- pdrs-scope: files=16 est_min=300 -->` 整段抹成空格 ——
# scope 既解析不到也报不出错，静默退回人日 / `.rs` 启发式（gate20 GLM boundary P0 实测：
# 杂散反引号 + 顶格 scope + 后面任意 `src/a.py` → rc 0「小包」，正是 #1316 那类假 PASS）。
PARA_BREAK = re.compile(r"\n[ \t]*\n|^ {0,3}#{1,6}\s", re.M)


def mask_inline_code(text):
    """把行内代码整段抹成空格（换行保留）。没有配对串的反引号按字面量处理。

    配对**不跨段落**：段内未闭合的反引号按字面量处理（CommonMark 同），否则一个笔误会吞掉
    后面段落里的真声明。先建「长度 → 起点」表再二分找闭合串：修前每个开串都往后重扫一遍，
    长度 1..k 的开串全不闭合时是 O(k·n)（2 MiB 病态输入实测 53 s，派单闸门跟着卡住）。
    """
    runs = _backtick_runs(text)
    by_len = {}
    for st, ln in runs:
        by_len.setdefault(ln, []).append(st)
    breaks = [m.start() for m in PARA_BREAK.finditer(text)]
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
        bk = bisect.bisect_right(breaks, a)
        if bk < len(breaks) and close >= breaks[bk]:
            continue                             # 闭合串在下一个段落：跨段不配对，按字面量处理
        for x in range(a, close + run):
            if out[x] != "\n":
                out[x] = " "
        consumed = close + run
    return "".join(out)
# fix-r19（#3074，DS/MM r17+r18 增量双谱系评审双撞）：`\s*[:：]?\s*` 相邻星号形（同一
# 字符类可被两个量词同时匹配）改为语言等价的 `\s*(?:[:：]\s*)?`（W* C? W* ≡ W* (C W*)?，
# r18 ECHO_HEAD 同款；不用 possessive/原子组——要 python≥3.11，此处全版本可用）。
# 但光改冒号槽还不够：残余的 `\s*`（冒号后那组）× 贪心 `(?P<rest>.*)$` 同样二次方
# （`\s*` 给回 × `.*` 全线重扫，实测 N=65514 失败形 7.8 s／a^N 形 >10 s——本机复测见
# _tmp/20261114-3074-fixr19/scope-v2-probe.out）。rest 首字符钉非空白：改前正则的
# 报告匹配里 `\s*` 贪心吃满 → rest 起点后首字符必非空白（或 rest 空）；给回空白只会
# 让 rest 起点同行右移/移到更早行，而 `$` 只看 rest 末端 → 同败，语言与 rest 组值
# 不变（字母表全枚举 1,567,636 串零差异），失败路径每步回溯 O(1)＝线性（实测 0.0028 s）。
# `:526` 的「body 含换行须报错」判据不受影响：跨行桥形照常命中（`\s*` 含 \n）才走到
# 该报错，v2 前后同判（见上面探针形状清单 `pdrs-scope:\nfiles=2` 等）。
SCOPE_BODY = re.compile(rf"^\s*pdrs{DASHES}*scope\s*(?:[:：]\s*)?(?P<rest>(?:[^\s\n][^\n]*)?)$", re.I)
# 关键字与数字之间允许标点（`工作量：3 人日` / `预计,2 人日`）；「估计」要单列（裸「估」后跟「计」不匹配）。
# `需(要|投入)?` 跟着关键词：`预计需要 2 人日` / `共需投入 3 人日` 修前接不上数字，人日证据整路丢掉
# → 退到 `.rs` 回退把大包判成小包（gate19 MiniMax boundary P2）。
# `预期` 与 `预计` 同义，一并收（此前只认后者）。
# fix-r19（#3074，同 SCOPE_BODY 的相邻星号反模式收族）：两处 `\s*[:：,，]?\s*` /
# `\s*(?:个)?\s*` 改语言等价的 `\s*(?:[:：,，]\s*)?` / `\s*(?:个\s*)?`（W* C? W* ≡
# W* (C W*)?，标点后空白只归一处；失败路径每步回溯 O(1)＝线性，实测 N=65514 失败形
# 0.0014 s / 0.0023 s——改前 0.97 s @16000 起二次方，见 _tmp/20261114-3074-fixr19/）。
PERSON_DAY = re.compile(r"(?:约|大约|估计|预计约|预计|预期|估算|估|工作量|共计|合计|总计|共)"
                        r"(?:需(?:要|投入)?)?\s*(?:[:：,，]\s*)?"
                        r"([0-9]+(?:\.[0-9]+)?)\s*(?:个\s*)?(?:[–\-~至]\s*([0-9]+(?:\.[0-9]+)?)\s*)?人[日天]")
# URL 只吃 ASCII URL 字符：中文顿号/逗号/分号紧跟其后时不许被整串吞掉（否则后面的 .rs 枚举漏计）
URL = re.compile(r"[A-Za-z][A-Za-z0-9+.-]*://[A-Za-z0-9./%#?=&_~:@!$'()*+-]+")
# `\.rs` 的右边界不用 \b（#3056 fix-r2 修 3②，MM2 sibling#2）：Python 的 \w 含 CJK，「src/a.rs是文件」
# 这类中文紧贴写法 \b 两侧都是 \w → 数不到该路径 → 卡在回退阈值 8 时大包被判小包（假 PASS）。
# 与 :96 BARE_PLACEHOLDER 同款 ASCII 环视。
RS_PATH = re.compile(r"[A-Za-z0-9_./\-]+\.rs(?![A-Za-z0-9_])")
# 代码围栏（CommonMark）：闭合行须同字符、长度 **≥** 开栏、独占一行（只许前后空白）。
# 逐行扫（FENCE_OPEN + _fence_spans），正则写不干净：修前要求闭合串与开栏串**完全相等**，
# ```` ``` ```` 开、```` ```` ```` 收的标准写法被判「围栏没有闭合」→ 整份任务书 rc 2（假 ERROR）。
# 前导空白最多 3 格（CommonMark）：4 格及以上的 ``` 是**缩进代码块里的字面内容**，不是围栏。
# 不限缩进时两头都错：缩进块里引一行孤 ``` 会被当开栏、永不闭合 → 假 ERROR（rc 2）；
# 顶格围栏里嵌 4 格缩进围栏时，内层提前闭合外层，夹在中间的示例 scope 泄漏成真声明 → 假 PASS。
FENCE_OPEN = re.compile(r" {0,3}(?P<f>`{3,}|~{3,})(?P<info>.*)$")
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
        close = re.compile(r" {0,3}" + re.escape(ch) + "{" + str(n) + r",}[ \t]*")
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
            # 行首到 pos 里有正文：scope 注释和正文在同一行（`写法参考 <!-- pdrs-scope … --> 这样写`）。
            # 修前当「顶格声明」收下：一条 files=2 的示例把 `if scopes:` 之后的人日 / `.rs` 回退
            # 整段短路 → 9 个 `.rs` 的包 rc 0 小包（gate19 MiniMax boundary P0 实测）。
            return "inline"
        i -= 1
    if len(text[i:pos].expandtabs(4)) < CODE_INDENT:
        return "flat"           # 缩进不到 4 格：仍按顶格声明算
    # 往回找上一个非空行，看它是不是列表项
    j, budget, blank_seen = i - 1, 4096, False
    while j > 0 and budget > 0:
        lo = max(0, j - budget)
        nl_at = text.rfind("\n", lo, j)
        if nl_at < 0:
            # 回看窗口里连一个换行都没有。两种情形要分开（gate16 GLM P2 实测）：
            #   ① 窗口已经缩到文档开头（lo == 0）：上一行就是**第一行**，它的行首看得见，
            #      `- scope 写法示例：` 开头 + 下一行 4 格示例 scope 应当判 example；
            #   ② 窗口还没到开头（lo > 0）：上一行本身比窗口还长，看不到行首就判不出
            #      「列表项 / 正文」，必须按判不准处理——取 `a = nl_at + 1 = 0` 会把文档开头
            #      整段当「上一个非空行」，文档以 `- ` 开头时长正文后面的缩进 scope 被静默丢掉
            #      （gate15 DeepSeek P0 实测：4503 字符正文 → scopes=[] rc 0 假 PASS）。
            if lo > 0:
                return "prose"
            first = text[0:j]
            if not first.strip():
                return "prose"
            if not LIST_ITEM.match(first):
                return "prose"
            return "prose" if blank_seen else "example"
        a = nl_at + 1
        if text[a:j].strip():
            if not LIST_ITEM.match(text[a:j]):
                return "prose"
            # 列表项与缩进 scope 之间隔着空行时，CommonMark 里那是**松散列表项的正文**（内容列 + 2），
            # 不是代码块也不是「贴的示例」：静默忽略会把一条 files=6 est_min=300 的声明丢掉 →
            # 启发式回退判小包 rc 0（假 PASS，gate21 GLM boundary P2 实测）。
            # 只有紧贴列表项（无空行）的缩进 scope 才是示例。
            return "prose" if blank_seen else "example"
        blank_seen = True
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
        if kind == "inline":
            line = bisect.bisect_right(nl, a) + 1
            errors.append(f"第 {line} 行的 scope 注释和正文在同一行（多半是贴示例）："
                          "要当声明请顶格独占一行，要当示例请放进行内代码 / 围栏 / 列表项下的缩进示例")
            continue
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


SECTION_TAIL = re.compile(r"^[\s（(：:·—–\-]|$")


def _section_matches(title, keyword):
    """标题是否就是这一节：`分段交付` / `分段交付（大包必填）` / `续跑（每段一行）` 算，
    `为什么要分段交付`（抢在真节前面 → 假 FAIL）、`分段交付的背景`（贴旧表顶替 → 假 PASS）不算。
    允许前面的编号前缀（`1.` / `一、`），关键词后面只许是行尾、空白或标点。"""
    t = re.sub(r"^(?:\d+[.、)]|[（(]?[一二三四五六七八九十]+[)）、.])\s*", "", title.strip())
    if not t.startswith(keyword):
        return False
    return bool(SECTION_TAIL.match(t[len(keyword):]))


def section_text(text, keyword):
    """取以 keyword 开头的 ## 一级小节的正文（到下一个同级或更高级标题为止）。"""
    out, grab, level = [], False, 0
    for line in text.splitlines():
        m = SECTION.match(line)
        if m:
            if grab and len(m.group(1)) <= level:
                break
            if not grab and _section_matches(m.group(2), keyword):
                grab, level = True, len(m.group(1))
                continue
        if grab:
            out.append(line)
    return "\n".join(out) if grab else None


ESCAPED_PIPE = "\x00"   # GFM 转义竖线 `\|` 的临时替身（任务书正文里不会有 NUL）


def _split_row(row):
    """按竖线切格。**只剥一根**行首 / 行尾竖线：`strip("|")` 会把 `|| a | b | …` 的粘写双竖线
    一并剥掉，GFM 的空首格塌缩、整行左移一位——6 格行被当成 5 格与表头对齐，段名读成次格、
    估时列对齐到别的格子，非合规任务书 rc 0（gate18 GLM boundary P0）。

    GFM 的 `\\|` 是**格内字面竖线**：切之前换成 NUL 占位、切完还原。修前直接按 `|` 切，
    `| S1 \\| 网关 | a.rs | S1.md | 80 min | — |` 多切一格、后续列整体右移 → 合规任务书
    报「预估时长读不出 `S1.md`」（假 FAIL，gate21 GLM boundary P1 实测）。
    """
    r = row.strip().replace("\\|", ESCAPED_PIPE)
    if r.startswith("|"):
        r = r[1:]
    if r.endswith("|"):
        r = r[:-1]
    return [c.strip().replace(ESCAPED_PIPE, "|") for c in r.split("|")]


def cells(row, width=None):
    """一行表格 → 格子列表；读不出返回 None。

    GFM 允许**每一行都省略行首竖线**（`S2 | a.rs | S2.md | 120 min | S1`），表头与数据行可以混着写。
    只认行首竖线会把这类行静默丢掉：该行的估时 / 占位符 / 字段数都不再校验 → 假 PASS（DS gate16 P1#2）。
    省略行首竖线的行按「列数等于表头」认；列数不符的由调用方报错，不许悄悄丢。
    """
    row = row.strip()
    if row.startswith("|"):
        return _split_row(row)
    if width is None or "|" not in row:
        return None
    parts = _split_row(row)
    return parts if len(parts) == width else None


def _col_hit(alt, header_cell):
    """列别名匹配：单字别名（`段`）只认整格，其余按子串。

    `段` 按子串会把「分段文件」认成段名列，文件列再无列可绑 → 合规分段表被判「缺列：涉及文件」。
    """
    return header_cell.strip() == alt if len(alt) <= 1 else alt in header_cell


def is_separator(row):
    """GFM 分隔行：`---` / `:--:` / 单 `-` 都合法；全空行不算分隔行（那是坏行）。"""
    filled = [c for c in row if c]
    return bool(filled) and all(re.fullmatch(r":?-+:?", c) for c in filled)


def _header_index(raw):
    """分段表表头行号：优先「下一行是分隔行」的那一行——GFM 每行都可省行首竖线，表头与数据行还能
    混写（表头省、数据行保留），只认「第一条带首竖线的行」会把数据行当表头 → 合规表假 FAIL。
    锚不到再退到第一条带首竖线的行（与旧行为一致）。"""
    for i in range(len(raw) - 1):
        if "|" not in raw[i] or "|" not in raw[i + 1]:
            continue
        if is_separator(_split_row(raw[i + 1])):
            return i
    return next((i for i, line in enumerate(raw) if cells(line) is not None), None)


def _bind_cols(header):
    """表头 → {列键: 下标}。别名按优先级试；`段名` 命中前不会退回 `段`。"""
    col, used = {}, set()
    for key, alts in WANT_COLS.items():
        for alt in alts:
            hit = next((i for i, h in enumerate(header) if i not in used and _col_hit(alt, h)), None)
            if hit is not None:
                col[key], _ = hit, used.add(hit)
                break
    return col


EST_TOKEN = re.compile(r"(?<![A-Za-z0-9.])([0-9]+(?:\.[0-9]+)?)\s*([A-Za-z一-鿿]*)")
# 小时+分钟的连写（`1h40m` / `1h45min` / `1h30`）：直接切 token 时后一个数字会被 lookbehind
# 挡掉、只读成 60 min。先把「N 小时 M 分」整体改写成两个带单位的 token 再切。
COMPACT_UNIT = re.compile(
    r"(?<![0-9.])(?P<h>[0-9]+(?:\.[0-9]+)?)\s*(?:hours?|hrs?|h|小时|时)\s*(?P<m>[0-9]{1,9})\s*"
    r"(?:minutes?|mins?|min|m|分钟|分)?(?!\s*[0-9])", re.I)
# 区间 / 上限语义取最大值：`80–120 min`、`80/90 min`、`80 min（上限 90 min）`
# `或` / `、` 是「二选一 / 列表」：`60 min 或 90 min` 修前落进求和分支读成 150 > 90 → 合规段被拒
# （假 FAIL，gate20 GLM boundary P2 实测）。按并列取最大值与 `/` 同待遇。
RANGE_MARK = re.compile(r"[–—~～/、]|至|到|上限|不超过|最多|或|≤|(?<=\d)\s*-\s*(?=\d)")
# 只有 `+` / `＋` / `另加` / `再加` / `再补` 是明确加法；`、，,` 是列表分隔符，`含` 常是「含联调」、
# `和` 常是「S1 和 S2」——把这些当加法会把 `60–80 min，含联调` 加成 140（假 FAIL）。
# 裸「加」是「增加 / 加班」的子串（`60–80 min（增加 20 min 联调）`），不当加法。
ADD_MARK = re.compile(r"\+|＋|另加|再加|再补|外加")
# 乘法连写：`4 × 40 min` 是 160 min。修前 `×` / `*` 谁都认不出，`4`（无单位）借后面 `40 min` 的单位
# 读成 4、与 40 求和成 44 ≤ 90 → 160 min 的段 rc 0（假 PASS，gate21 GLM boundary P1 实测）。
# `4x40`（字母 x 夹在两个数字之间）同待遇；折叠成单值再走后面的区间 / 求和规则。
MUL_MARK = re.compile(r"(?<![\d.])(\d+(?:\.\d+)?)\s*(?:[×＊*]|[xX](?=\s*\d))\s*(\d+(?:\.\d+)?)")
BREAK_CHARS = "（(，,；;"     # 回看窗口的边界：括注开头 / 句读
# 括注里「包含」类说明词：它们后面的数字不计入主估时。**否定式不算包含**——否定词在词前 4 字符内
# （`不含` / `不包含` / `未包含` / `不需要包括` / `除…含`）时那段时间必须计入。
# 窗口取 4 而不是 3：`不需要包含` 里 `不` 与 `含` 隔着 `需要包` 三字；窗口 3 只能看到 `需`，于是
# 只能把 `需` 也当否定词，反过来把**肯定的** `需要包含 30 min` 判成否定式（120 min，假 FAIL）。
# 「外加」**不是**包含说明：`80 min（外加 20 min 联调）` 是 100 min，按包含跳过会读成 80
# （假 PASS，gate20 MiniMax boundary P1 实测；同一措辞不加括注时却是 100，两种写法结论相反）。
# 它归 ADD_MARK。
INCLUDE_WORDS = re.compile(r"含|各|其中|包括|内含")
INCLUDE_NEG = re.compile(r"[不未无除没]")
EXCLUDE_WORDS = re.compile(r"不计入|不算")
INCLUDE_LOOKBACK = 4


def _skips_number(text, lo, hi):
    """`text[lo:hi]` 这段说明文字是否让紧跟着的数字不计入主估时。

    用 pos/endpos 在原文上搜、不切前缀：切前缀是 O(len) × token 数，64 KiB 的单格会退化成平方。
    """
    if EXCLUDE_WORDS.search(text, lo, hi):     # `（不计入 30 min 缓冲）` 是明确的排除说明
        return True
    for m in INCLUDE_WORDS.finditer(text, lo, hi):
        if not INCLUDE_NEG.search(text[max(lo, m.start() - INCLUDE_LOOKBACK):m.start()]):
            return True
    return False


def _excludes_after(text, end):
    """数字**后面**的「不计入 / 不算」同样生效：`80 min（20 min 联调不计入）` 是中文自然语序。

    只看同一括注 / 同一句读之内（`）` `)` `，` `,` `；` `;` 为止），最多 40 字符，保持线性。
    """
    hi, limit = end, min(len(text), end + 40)
    while hi < limit and text[hi] not in "）)，,；;":
        hi += 1
    return bool(EXCLUDE_WORDS.search(text, end, hi))


# 非时长量词开头的单位（`2 个文件` / `3 项` / `2 处`）：是数量说明，不是估时。只收明确是计数的量词，
# `人日` 这类真时长单位不在内（仍判「读不出」）。
NON_DURATION_UNIT = re.compile(r"^(?:个|项|文件|段|处|次|条|份|张|行|块|名|位)")
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
    认不出的返回 None（裸数字随后报「读不出」，不当 min 放行）—— 只有括号/斜杠里的独立词才算，
    `预估时长` 里的「时」不算，`(人日)` / `人天` / `工作日` 也返回 None。
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
    # `用时` 与 `时长 / 耗时` 同类（都是时长），裸数字按 min；`工作量` / `投入` **不在此列**：
    # 裸数字可能是人日，按 min 收会低估（gate19 MiniMax boundary P2 里建议也按 min 认，
    # 但低估是假 PASS 方向，宁可留「读不出」——见 SEGMENTS.md §6 第 23 条）。
    if any(w in low for w in ("时长", "时间", "耗时", "估时", "分钟", "用时")):
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
            if ch == c and open_at >= 0:
                open_at = -1
                continue                  # 闭括号自身不算「在括号里」（与 docstring / 旧 rfind 写法一致）
            inside[i] |= open_at >= 0     # 只看 [0, i)
            if ch == o:
                open_at = i
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
    for _ in range(4):      # 连乘 `2 × 3 × 4 min` 逐轮折叠；4 轮足够，剩下的走后面的「读不出」
        folded = MUL_MARK.sub(lambda m: f"{float(m.group(1)) * float(m.group(2)):g}", cell)
        if folded == cell:
            break
        cell = folded
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
        lo = (breaks[k - 1] if k else -1) + 1
        # `不含 / 不包含 / 未包含 / 不包括 / 除…含` 是**排除**：那段时间不在主估时里，必须计入
        # （`90 min（不含 30 min 测试）` 真值 120）；写成裸 `[含各]` 会把否定句也跳过 → 边界上的段假 PASS。
        # 否定判定要覆盖 2–3 字的写法：只写 `(?<![不未无除])含` 时 `不包含` 的 `含` 前一字符是 `包`，
        # 仍被当成包含（gate15 DeepSeek P1 实测：`不包含` / `不包括` / `未包含` 三种都读成 90，真值 120）。
        # 后置的「不计入」只对**括注里**的数字生效：`80 min（20 min 联调不计入）` 里 20 不算、80 还要算，
        # 对 80 也做前瞻会把整格判成没有估时（实测 None）。
        in_paren = k > 0 and cell[breaks[k - 1]] in "（("
        if _skips_number(cell, lo, m.start()) or (in_paren and _excludes_after(cell, m.end())):
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
            # `（2 个文件）` 这类括注不是时长；括注**外**、句读后面同样常见的说明
            # （`80 min，涉及 2 个文件` / `80 min；另含缓冲说明 3 项`）也跳过——修前整格判
            # 「读不出」→ 拒派合规任务书（gate20 GLM boundary P2 实测）。
            if inside[pos] or NON_DURATION_UNIT.match(u):
                continue    # 整格没有时长数字时才报「读不出」
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
# 路径段里的裸占位：骨架 `_tmp/<日期>-issue-<N>/progress.md` 去掉尖括号就是 `…-issue-N`，
# 整串既没有 `{}<>…` 也有 `/`，旧规则判成实例路径 → rc 0 假 PASS（gate17 GLM boundary P0）。
# 单字母之外还要认多字母占位词（`xxx` / `TBD` / `NUM` / `DATE` / `N1`）：修前 `_tmp/xxx/progress.md`
# 与 `_tmp/issue-NUM/progress.md` 照当实例路径（rc 0 假 PASS，gate21 GLM boundary P1 实测）。
# 词表与格子侧同源（PLACEHOLDER_WORDS / BARE_PLACEHOLDER），两处不再各写一份。
# CJK 段（`评审记录`、`进度`）放行：仓里真有中文目录名。
PATH_PLACEHOLDER_PART = re.compile(r"[NnMmKk]\d*")
# 词表与格子侧同源，但**只取明确的占位词**：`x` / `name` / `dir` / `file` / `path` / `head` / `sha`
# 在真实路径里是可能的目录名（`_tmp/x/progress.md`），当占位会假 FAIL 拒派合规任务书。
PATH_PLACEHOLDER_WORDS = {w for w in PLACEHOLDER_WORDS
                          if w in {"n", "nn", "num", "idx", "id", "date", "tbd", "todo", "xxx", "yyy"}}


def _path_placeholder(p):
    """返回路径里第一个裸占位段（`N` / `issue-N` / `xxx` / `NUM`），没有返回 None。"""
    for seg in p.split("/"):
        for part in re.split(r"[-_.]", seg):
            if PATH_PLACEHOLDER_PART.fullmatch(part) or part.lower() in PATH_PLACEHOLDER_WORDS:
                return part
    return None


def _looks_url(p):
    """`https://…` 的片段之外，`example.com/2026/progress.md` 这类无 scheme URL 也不算本地路径。"""
    if "://" in p:
        return True
    first = p.split("/", 1)[0]
    return bool(re.fullmatch(r"[A-Za-z0-9-]+(?:\.[A-Za-z0-9-]+)*\.[A-Za-z]{2,}", first))


def resume_problems(resume):
    """`## 续跑` 的最小检查：具体进度文件路径 + 「第 N 段失败从哪继续」的学样。"""
    problems = []
    path, bad_paths, hard_bad = None, [], []
    for line in resume.splitlines():
        for m in PATH_TOKEN.finditer(line):
            p = m.group(0)
            name = p.rsplit("/", 1)[-1]
            if not re.search(r"progress|进度", name, re.I):   # `PROGRESS.md` / `Progress.md` 同样是进度文件
                continue
            bad = None
            ph = _path_placeholder(p)
            if any(ch in p for ch in "{}<>…＜＞｛｝【】〈〉") or "..." in p:
                bad = "还是占位符"
            elif ph:
                bad = f"路径段 {ph!r} 是没替换的占位"
            elif "/" not in p:
                bad = "只有文件名、没有目录"
            elif p.startswith("//") or _looks_url(p):
                bad = "是 URL 里的片段，不是本地路径"
            elif NEGATION.search(re.split(r"[，。；、：,;]", line[max(0, m.start() - NEG_WINDOW):m.start()])[-1]):
                bad = "写在否定句里"
            if bad:
                bad_paths.append(f"{_clip(p)}（{bad}）")
                if "占位" in bad:
                    hard_bad.append(f"{_clip(p)}（{bad}）")
            elif path is None:
                path = p
    if hard_bad:
        # 合规路径与占位路径并存时修前静默丢掉坏的那条：骨架 `_tmp/<日期>-issue-<N>/progress.md`
        # 原样留在派单里也 rc 0（gate19 MiniMax boundary P2 实测）。只把**占位符**这一类升成硬问题：
        # 「读 progress.md 最后一个 done 段」这种裸文件名的正文引用是常态写法，不能因此假 FAIL。
        problems.append("`## 续跑` 里的进度文件路径还留着占位：" + "、".join(hard_bad)
                        + "（要写带目录的实例路径，骨架只许出现在示例 / 围栏里）")
    if not path:
        detail = f"；看到的不合规路径：{'、'.join(bad_paths)}" if bad_paths else ""
        problems.append("`## 续跑` 里没写具体的进度文件路径（要带目录的实例路径，如 "
                        "`_tmp/20261105-issue-1316/progress.md`；不许留 <…> / {…} / …，"
                        "也不许留裸 `N` 这类没替换的路径段，"
                        f"不许只写文件名，也不许写在「无需 / 不用」这类否定句里）{detail}")
    positive = []
    for line in resume.splitlines():
        # 路径 / 行内代码里的 `resume` / 进度 不算「第 N 段失败从哪继续」的说明：
        # `- 进度文件：_tmp/x/resume-r1/progress.md` 修前命中英文关键字，整节只写路径也 PASS。
        bare = PATH_TOKEN.sub(lambda m: " " * len(m.group(0)), line)
        bare = URL.sub(lambda m: " " * len(m.group(0)), bare)   # URL 里的 `resume` 不是说明（gate21 GLM P2）
        bare = re.sub(r"`[^`\n]*`", lambda m: " " * len(m.group(0)), bare)
        for clause in re.split(r"[，。；、：,;]", bare):
            if not NEGATION.search(clause):
                positive.append(clause)
    if not any(w in "\n".join(positive).lower() for w in RESUME_FAIL_WORDS):
        problems.append("`## 续跑` 里没写「第 N 段失败从哪继续」（要有 失败 / 继续 / 接续 / 恢复 / 重跑 / resume 之一的说明）")
    return problems


def _table_starts(raw):
    """分段小节里**每一张**表的表头行号（GFM 表 = 表头 + 紧贴的分隔行）。

    表体里也可能出现「表头 + 分隔行」的形态，两种要分开（都不能一刀切）：
      ① 整行 `-` 的**数据行** + 下一行数据：`_split_row` 认成表头，其实只是没填的段行——
         当成新表头会把真表切成两半、前面几行全丢（gate21 修 P0 时自测撞到）；
      ② 表头省竖线的混写：两列小表后面紧贴真表头——真表头绑出的列更多，是真表头。
    判据：候选表头绑出的列数**多于**当前表头才另起一张表，否则按表体里的数据行处理。
    """
    out, cur, i, n = [], None, 0, len(raw)
    while i < n - 1:
        cand = ("|" in raw[i] and "|" in raw[i + 1]
                and not is_separator(_split_row(raw[i]))
                and is_separator(_split_row(raw[i + 1])))
        if cand:
            if cur is None or len(_bind_cols(_split_row(raw[i]))) > len(_bind_cols(_split_row(raw[cur]))):
                out.append(i)
                cur = i
        elif cur is not None and not raw[i].strip():
            cur = None               # 空行结束当前表
        i += 1
    return out


# GFM 里这些行开头会结束表格；表后紧跟标题 / 列表 / 围栏 / 引用 / 注释是常态，不算「漏写竖线」
TABLE_BREAK = re.compile(r"^ {0,3}(?:#{1,6}\s|[-*+][ \t]|\d{1,9}[.)][ \t]|>|```|~~~|<!--)")


def _table_rows(raw, hidx, starts):
    """表头之后的表行：空行 / 下一张表的表头 / 别的块级开头处结束。

    返回 (rows, stray)。stray 是表区里第一条**没有竖线**的正文行：GFM 在这里结束表格，
    但作者多半是漏写竖线——静默丢掉等于该行的估时 / 占位符 / 共写都不再校验
    （假 PASS，gate21 MiniMax boundary P3 实测：`S2 b.rs y.md 300 min S1`）。
    """
    nxt = next((s for s in starts if s > hidx), len(raw))
    rows, stray = [], None
    for idx in range(hidx + 1, nxt):
        line = raw[idx]
        if not line.strip():
            break
        if "|" not in line:
            if TABLE_BREAK.match(line):
                break
            if stray is None:
                stray = (idx, line)
            continue
        rows.append(line)
    return rows, stray


def check_segments(text):
    """返回问题列表（空 = 通过）。缺表与缺续跑要一起报，免得派单人改两遍。"""
    problems = []
    body = section_text(text, "分段交付")
    if body is None:
        problems.append("缺 `## 分段交付` 一节（大包必须按文件切段，模板见 ~/ws/pdrs-ops/templates/taskbook-segments.md）")
    else:
        raw = body.splitlines()
        starts = _table_starts(raw)
        hidx = _header_index(raw)
        if hidx is not None and hidx not in starts:
            starts.append(hidx)          # 兜底锚到的表头（下一行不是分隔行）
        if not starts:
            problems.append("`## 分段交付` 里没有表格（需 markdown 表：段名 / 涉及文件 / 交付物 / 预估时长 / 段间依赖）")
        else:
            # 同一节里的**每一张**表都要校验：示例表（五列齐全、≥2 行）排在本包真表前面时，修前
            # 只看第一张 → 真表整张不校验，300 min 的段 / 共写 / 占位符全不查、rc 0（假 PASS，
            # gate21 MiniMax boundary P1 实测）。列数少于 4 的表（如「段 | 说明」）不当分段表。
            starts = sorted(set(starts))
            tables = []
            for s in starts:
                header = _split_row(raw[s])
                rows, stray = _table_rows(raw, s, starts)
                tables.append({"start": s, "header": header, "col": _bind_cols(header),
                               "rows": rows, "stray": stray})
            # 主表 = 绑出列最多的那张（同分取靠后的：示例表通常排在本包真表前面）：
            # 缺列与段数只按主表判，别让一张两列的小表把真表挤掉。
            main = max(tables, key=lambda t: (len(t["col"]), t["start"]))
            missing_cols = [COL_NAMES[k] for k in COL_NAMES if k not in main["col"]]
            if missing_cols:
                problems.append("分段表缺列：" + "、".join(missing_cols))
            owner, seen, table_text = {}, 0, []
            for t in tables:
                if len(t["col"]) < 4:      # 不像分段表：只看不判（段名 / 说明这种小表）
                    continue
                width, col, header = len(t["header"]), t["col"], t["header"]
                default_unit = _header_unit(header[col["est"]]) if "est" in col else ""
                table_text.extend([raw[t["start"]]] + t["rows"])
                first_row = True
                for line in t["rows"]:
                    r = cells(line, width)
                    if r is None:
                        problems.append(f"分段表有一行读不出 {width} 列（行首竖线可省，但列数须与表头一致）：{_clip(line.strip())!r}")
                        continue
                    # GFM 分隔行只能紧贴表头（第 2 行）；后面的全 `-` 行是没填的段行，不许整行跳过。
                    # 修前判据是 `not data and is_separator(r)`：第一条数据行若是全 `-`（没填的段行），
                    # data 仍为空 → 被当第二条分隔行整行丢掉，该行的占位符 / 空值 / 估时全不校验
                    # （gate20 MiniMax boundary P0 实测：三段表的首行全 `-` → rc 0）。
                    if first_row and is_separator(r):
                        first_row = False
                        continue
                    first_row = False
                    if not any(r):
                        problems.append(f"分段表第 {seen + 1} 行整行为空（不许用空行占位）")
                        continue
                    seen += 1
                    i = seen
                    if len(r) != width:
                        # 多于表头的行修前照收：`r[col["est"]]` 读到的不是估时格、`r[col["files"]]`
                        # 不是文件格 → 错位格恰好像估时就把真估时 / 真共写掩盖过去（假 PASS，
                        # gate21 两家 boundary 各自报到）。整行不校验，只报列数。
                        problems.append(f"第 {i} 行字段数 {len(r)} ≠ 表头 {width}"
                                        "（列对不齐，这一行的估时 / 文件都不校验）")
                        continue
                    if "files" in col:      # 段间不共写同一文件：共写 = 两段不能各自独立收货
                        files_cell = r[col["files"]]
                        for tok_ in FILE_TOKEN.findall(files_cell) + BARE_FILE.findall(files_cell):
                            key = _file_key(tok_)
                            if key in owner and owner[key] != i:
                                problems.append(f"第 {i} 段与第 {owner[key]} 段共写同一文件 {_clip(tok_)}"
                                                "（段间不共写同一文件，否则不能独立收货）")
                            else:
                                owner.setdefault(key, i)
                    for key, idx in col.items():
                        cell = r[idx]
                        if not cell:
                            problems.append(f"第 {i} 段「{COL_NAMES[key]}」为空")
                            continue
                        hit = placeholder_in(cell)
                        if hit:
                            problems.append(f"第 {i} 段「{COL_NAMES[key]}」还是占位符：{_clip(hit)!r}")
                    if "est" in col:
                        est_cell = r[col["est"]]
                        est = parse_est(est_cell, default_unit)
                        if est is None:
                            problems.append(f"第 {i} 段预估时长读不出（{_clip(est_cell)!r}；"
                                            "只认 min / 分钟 / h / 小时）")
                        elif est > SEGMENT_MAX_MIN:
                            problems.append(f"第 {i} 段预估 {est:g} min > {SEGMENT_MAX_MIN} min（切成更小的段）")
                if t["stray"]:
                    idx, line = t["stray"]
                    problems.append(f"分段表第 {idx - t['start']} 行没有竖线，表格在这里断了"
                                    f"（漏写 `|`？这一行之后的估时 / 占位符都不校验）：{_clip(line.strip())!r}")
            if seen < 2:
                problems.append(f"分段表只有 {seen} 段；大包至少要切成 2 段（每段可独立收货）")
            if placeholder_in("\n".join(table_text)) and not any("占位符" in p for p in problems):
                problems.append("分段表里还有占位符（<…> / {…} / … / TBD / TODO），须填实现值")
    resume = section_text(text, "续跑")
    if resume is None:
        problems.append("缺 `## 续跑` 一节（写清进度文件路径 + 第 N 段失败从哪继续）")
    else:
        problems.extend(resume_problems(resume))
    return problems


# ---- 修复轮字段（#3056 长尾收口卡「机检落点」） ------------------------------
# 修复轮信号 = 路径段全匹配 fix-r<N>（含接续轮 b 后缀，ops dispatch-fix 生成于
# _tmp/<日期>-issue-<N>/fix-r<M>[/b]/task.md）。
# 内容识别不可靠：任务书正文经常引用 fix-rN 字样（如本检查器的文档），不能当轮次信号。
# 轮号词表单源（#3056 fix-r3 P1-3，MM boundary#2）：旧本文件手抄 `fix-r(\d+)` 与 PR 侧
# FIXROUND_RE 是两份定义——b 后缀（fix-r11b，board.md:1660/:1642/:1981/:1984 在案）在
# 任务书侧 fullmatch 失败、PR 侧词尾环视不认，两侧同时静默跳过该轮族的字段义务＝假 PASS。
# 派生 check-pr-gate.py 的 FIXROUND_RE（b 算第 N 轮、只认 b、位界 {1,18}；fix-r5x 仍是
# 非修复轮——PR 侧负控钉死），fullmatch 于路径段等价于词边界匹配。


def _gate_module():
    """懒加载同目录 check-pr-gate.py 取字面三值常量（#3056 fix-r1，DS sibling#8）：run-review-roles.py
    的 _gate_module() 是先例——三值改名 / 加值时任务书闸门与 PR 闸门用同一对象，不再各写一份
    （各写一份会在改名后一侧放行另一侧永久拒收，假 FAIL）。加载失败让位后续 FIXTYPE_RE 检查报错
    （fail-closed：拿不到正本就不做静默放行）。"""
    import importlib.util
    p = os.path.join(os.path.dirname(os.path.abspath(__file__)), "check-pr-gate.py")
    spec = importlib.util.spec_from_file_location("pdrs_gate_for_taskbook_3056", p)
    if spec is None or spec.loader is None:
        print(f"check-taskbook-segments: 加载不了 {p}（字面三值的正本），fail-closed 退出", file=sys.stderr)
        raise SystemExit(2)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


# 修法类型只接受字面三值（卡「收口证明义务」节）；「结构派生/自动派生」等子类字样一律写 结构性。
# 常量不复制：就是 check-pr-gate.py 的同一对象（见 _gate_module；配对测试断言 is 同一）。
FIXTYPE_VALUES = _gate_module().FIXTYPE_VALUES
# 轮号词表不复制：PR 侧 check-pr-gate.py 的 FIXROUND_RE 是源（fix-r3 P1-3，见上方段落注释；
# fix_round_from_path 用 fullmatch，环视在串首/尾恒真，等价词边界匹配）。
FIX_ROUND_PART = _gate_module().FIXROUND_RE
# fix-r8 boundary P1（MM r7 评审，协调者实测复现 _tmp/20261114-3074-mm-p1-probe/）：
# 位界 {1,18} 经单源派生到任务书侧后，超长 fix-r 段（≥19 位）fullmatch 失败 → 旧
# fix_round_from_path 静默返回 None（判非修复轮，fixtype/residual 义务整段跳过＝假 PASS），
# 而同一 token 在 PR 侧 check() 被 _overflow_err fail-closed 拒收——两闸门同输入结论相反。
# 镜像 fail-closed：超长段派生 PR 侧 FIXROUND_OVER（单源，配对测试断言 is 同一），
# fix_round_from_path 命中即抛 FixRoundOverflow（「读不了」），调用方按 rc 2 拒收。
FIX_ROUND_OVER = _gate_module().FIXROUND_OVER


class FixRoundOverflow(Exception):
    """路径段 fix-r 后 ≥19 位数字（位界 {1,18} 外）——fail-closed，报「读不了」而非
    静默判非修复轮（与 PR 侧 _overflow_err 同一 token 同一结论，MM r8 boundary P1）。"""
# 标记须独占一行（行首允许缩进，`-->` 后不许再有正文）：正文行内引用的示例标记不算声明
FIXTYPE_RE = re.compile(r"^[ \t]*<!--\s*pdrs-fixtype:(.*?)-->[ \t]*$", re.M)
# residual 标记正则不复制：PR 侧 check-pr-gate.py 的 RESIDUAL_MARKER 是源（#3056 fix-r2 修 7，
# DS simplify#1 / MM2 sibling#5）——两份同字面拷贝会在改标记语法时一侧判「缺」另一侧认声明。
# 配对测试断言两处 pattern 一致（同 FIXTYPE_VALUES 的单源做法）。
RESIDUAL_RE = _gate_module().RESIDUAL_MARKER
# N≥4 门槛单源（#3056 fix-r2，DS r2 boundary#3 P2）：此前本文件与 check-pr-gate.py 各写一份
# 字面量 4，改一侧不改另一侧会让任务书闸门与 PR 闸门对同一修复轮给出相反结论（一侧要
# residual、另一侧不要）。派生自 check-pr-gate（同 FIXTYPE_VALUES 的手法，配对测试断言）。
RESIDUAL_MIN_ROUND = _gate_module().RESIDUAL_MIN_ROUND
RESIDUAL_NONE = "无上一轮"   # 字面值：没有上一轮时允许（空答不算答）


def fix_round_from_path(path):
    """路径里第一个全匹配 fix-r<N> 的段 → N；不是修复轮返回 None。

    fix-r8 boundary P1（MM r7 评审）：命中超长 fix-r 段（≥19 位数字）时抛
    FixRoundOverflow（fail-closed「读不了」），不静默返回 None——位界外的段
    FIX_ROUND_PART 本就 fullmatch 不上，旧口径会让同一 token 在 PR 侧被拒收、
    任务书侧静默跳过整段修复轮义务。fix-r9 P2（MM boundary#2）：超长检查**先全
    路径扫完**再找合法段——旧口径逐段「先查超长、再查合法、合法即 return」，
    合法段（fix-r5）排在超长段前面时后者永不检查＝短路放行；两遍扫后任一超长
    段都触发 rc 2。"""
    parts = Path(path).parts
    for part in parts:      # 第一遍：超长段全查（合法段在前不得短路）
        if FIX_ROUND_OVER.fullmatch(part):
            raise FixRoundOverflow(part)
    for part in parts:      # 第二遍：首个合法段定轮号
        m = FIX_ROUND_PART.fullmatch(part)
        if m:
            return int(m.group(1))
    return None


def check_fix_fields(body, fix_round):
    """修复轮任务书的必填字段检查（#3056）。body 须是去掉围栏后的正文（围栏里的示例不算声明）。

    - `<!-- pdrs-fixtype: 枚举式|结构性|不适用 [论证] -->`：恰好一次；值须是字面三值；
      枚举式附边界论证、结构性附词表核验（非空白、非占位符）；不适用在任务书侧不查路径
      （任务书先于实现存在），diff 级合法性归 PR 侧 check-pr-gate.py（ci_scope 语义唯一口径）。
    - fix-rN（N≥4）另须 `<!-- pdrs-residual: <非空> -->`：恰好一次、非空白、非占位符；
      字面值「无上一轮」允许。
    非修复轮（fix_round is None）返回空列表——字段存在也不报错（模板 / 骨架可能含示例）。
    """
    if fix_round is None:
        return []
    problems: list[str] = []
    marks = FIXTYPE_RE.findall(body)
    if not marks:
        problems.append("修复轮任务书缺修法类型字段 `<!-- pdrs-fixtype: 枚举式|结构性|不适用 … -->`"
                         "（三选一字面值；枚举式附边界论证、结构性附词表核验）")
    elif len(marks) > 1:
        problems.append(f"pdrs-fixtype 标记出现 {len(marks)} 次（至多一次）")
    else:
        inner = marks[0].strip()
        if not inner:
            problems.append("pdrs-fixtype 的值是空白串（三选一：枚举式 / 结构性 / 不适用；空答不算答）")
        else:
            parts = inner.split(None, 1)
            value, arg = parts[0], (parts[1].strip() if len(parts) > 1 else "")
            if value not in FIXTYPE_VALUES:
                problems.append(f"修法类型「{value}」不是字面三值（枚举式 / 结构性 / 不适用）；"
                                 "「结构派生 / 自动派生」等子类字样一律写「结构性」")
            elif value in ("枚举式", "结构性"):
                if not arg:
                    problems.append(f"修法类型「{value}」缺论证（枚举式附边界论证、结构性附词表核验；空白不算）")
                else:
                    hit = placeholder_in(arg)
                    if hit:
                        problems.append(f"修法类型「{value}」的论证还是占位符：{_clip(hit)!r}（写实际论证）")
    if fix_round >= RESIDUAL_MIN_ROUND:
        marks = RESIDUAL_RE.findall(body)
        if not marks:
            problems.append(f"fix-r{fix_round}（N≥{RESIDUAL_MIN_ROUND}）任务书缺上轮残差判读 `<!-- pdrs-residual: … -->`"
                            f"（上一轮的洞是边界暴露还是修法本身缺陷；{RESIDUAL_NONE}时写字面值「{RESIDUAL_NONE}」）")
        elif len(marks) > 1:
            problems.append(f"pdrs-residual 标记出现 {len(marks)} 次（至多一次）")
        else:
            content = marks[0].strip()
            if not content:
                problems.append("pdrs-residual 内容是空白串（空答不算答；"
                                f"无上一轮时写字面值「{RESIDUAL_NONE}」）")
            elif content != RESIDUAL_NONE:
                hit = placeholder_in(content)
                if hit:
                    problems.append(f"pdrs-residual 内容还是占位符：{_clip(hit)!r}（写实际判读）")
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

# 骨架是填空题不是答案：修复轮字段段用占位形态写，原样贴进修复轮任务书会被打回（缺字段 / 占位符）。
SKELETON += """修复轮任务书（路径在 `_tmp/<日期>-issue-<N>/fix-rN/task.md`）另须两件（#3056 长尾收口卡）：

`<!-- pdrs-fixtype: <枚举式|结构性|不适用> <论证> -->` —— 三选一字面值：`枚举式` 附边界论证
（词表封闭性论证，或反向负例清单证明修饰窗/边界不吞真发现）；`结构性` 附词表核验（须核到测试真实
遍历的词表）；`不适用` 仅当 diff 只触及文档类路径（以 scripts/ci_scope.py 的分类语义为唯一口径，
其白名单只有 `state/board.md` 一个精确路径）。

`<!-- pdrs-residual: <上轮残差判读> -->` —— fix-rN 的 N≥4 必填：上一轮的洞是边界暴露还是修法
本身缺陷；无上一轮时写字面值 `无上一轮`。
"""


# 分段表「涉及文件」列里的文件路径（按扩展名认，避免把 `S1` 这类段名当文件）
# 只认代码扩展名时 `Cargo.lock` / `events.proto` / `001.sql` / `a.html` 不算文件 → 两段共写照样放行（假 PASS）。
# `jsonl` 单列：`src/` 下有 50+ 个 `.jsonl` fixture，两段各追加同一份也照样放行（gate16 sibling P2）。
# 交替按**最长优先**排（`jsonl` 在 `json` 前、`json` 在 `js` 前、`tsx` 在 `ts` 前），外加右边界：
# Python `re` 是最左优先，`events.jsonl` 会被 `js` 吃成 `events.js`——`.json` 与 `.jsonl` 两个
# 不同文件的键撞成一个，报「共写 events.js」（假 FAIL，报错里的文件名也是错的）。
FILE_TOKEN = re.compile(
    r"[A-Za-z0-9_./-]*[A-Za-z0-9_-]\."
    r"(?:jsonl|json|js|tsx|ts|mjs|cjs|rs|py|md|toml|ya?ml|sh|txt|lock|proto|sql|html|css"
    r"|xml|svg|ipynb|patch|diff|c|h|cc|cpp|hpp|go|java|kt|rb|php|swift|csv|tsv|ini|cfg"
    r"|conf|properties|gradle|env)"
    r"(?![A-Za-z0-9])", re.IGNORECASE)   # `SRC/A.RS` 与 `src/a.rs` 是同一个文件：键已 casefold，别在识别处漏掉
# 无扩展名的常见构建文件（`Dockerfile` 两段各改一次同样是共写）
# 目录前缀要一起收（`deploy/Dockerfile` 与 `ci/Dockerfile` 是两个文件，键要带目录）
BARE_FILE = re.compile(r"(?<![A-Za-z0-9_./-])(?:[A-Za-z0-9_./-]*/)?"
                       r"(?:Dockerfile|Containerfile|Makefile|Justfile)"
                       r"(?![A-Za-z0-9_])", re.IGNORECASE)


def _file_key(tok_):
    """共写判定键：`./` 前缀、`x/../x/a.rs` 这类同义写法与大小写都归一——APFS 默认大小写不敏感，
    `SRC/A.rs` 与 `src/a.rs` 就是同一个文件，不归一会让「段间不共写同一文件」被绕过（fail-open）。
    报错仍打原文，不暴露归一后的键。"""
    key = tok_[2:] if tok_.startswith("./") else tok_
    return posixpath.normpath(key).casefold()


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
    # 修复轮字段检查置于 big/small 分支之前：小包（不分段的修复轮）同受检（#3056 义务 2）。
    # fix-r8 boundary P1：超长 fix-r 路径段（≥19 位）按「读不了」fail-closed（rc 2），
    # 不静默判非修复轮跳过字段义务——与 PR 侧 check-pr-gate._overflow_err 同一 token
    # 同一结论（两闸门对同一输入不得相反）。
    try:
        fix_round = fix_round_from_path(p)
    except FixRoundOverflow as e:
        return _err(p, f"读不了：路径段「{e}」是 fix-r 后 ≥19 位数字——超出轮号位界 "
                       "{1,18}，与 PR 侧 check-pr-gate 同一结论 fail-closed 拒收；"
                       "请核对路径（正常修复轮号远小于 19 位）")
    fix_problems = check_fix_fields(body, fix_round)
    if not cls["big"]:
        if fix_problems:
            return {"file": str(path), "rc": 1, "problems": fix_problems, "class": cls}
        return {"file": str(path), "rc": 0, "problems": [], "class": cls}
    problems = check_segments(body) + fix_problems
    return {"file": str(path), "rc": 1 if problems else 0,
            "problems": problems, "class": cls}


# `.prev-*` 是旧轮次备份目录；用词界而不是裸前缀：`.preview/` 这类正常目录不该被当成备份跳掉
PREV_BACKUP = re.compile(r"\.prev(?:$|[-_.])")


def _is_dispatch_part(part):
    return part == "remote-run" or part.startswith("remote-run.") or bool(PREV_BACKUP.match(part))


def _is_dispatch_copy(p):
    """`remote-run/stage|artifacts` 里是同一份任务书的派发副本，`.prev-*` 是旧轮次备份：都不重复检查。

    只看 `_tmp` 之后的路径段：`--root` 前缀里正好有叫 `remote-run` 的目录时不能把全部任务书跳掉。
    """
    parts = p.parts
    try:
        i = len(parts) - 1 - parts[::-1].index("_tmp")   # 取**最后**一个：--root 前缀里也可能叫 _tmp
    except ValueError:
        return False
    return any(_is_dispatch_part(part) for part in parts[i + 1:-1])


def _is_dispatch_dir(p):
    """目录**本身**就是派发副本 / 备份（判断符号链接要不要跳过时用，末段也算进来）。"""
    parts = p.parts
    try:
        i = len(parts) - 1 - parts[::-1].index("_tmp")
    except ValueError:
        return False
    return any(_is_dispatch_part(part) for part in parts[i + 1:])


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
                    # 派发副本 / 旧轮次备份本来就该跳过（`_is_dispatch_copy` 的口径）：这种软链
                    # 不增强制中断，否则一条 `remote-run -> 共享产物` 的链就让整轮扫描 rc 2
                    # （gate21 MiniMax boundary P3 实测）。其余软链仍中断，不静默漏。
                    if _is_dispatch_dir(Path(p)):
                        continue
                    raise ScanError(f"递归扫到符号链接目录 {p}：不跟随会静默漏掉里面的任务书，"
                                    "请换成真实目录或删掉链接")
            # `task.md` 按大小写不敏感比：macOS 默认文件系统上 `Task.md` 与 `task.md` 是同一个名字，
            # 字符串精确比较会把整类任务书漏掉，rc 0 冒充「都通过」（gate21 GLM boundary P3）。
            names = {n.casefold(): n for n in files}
            if "task.md" in names:
                out.append(Path(dirpath) / names["task.md"])
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
    if as_json and explain:
        # 修前 if/elif 顺序把 --explain 静默吞掉：调用方按 --explain 想要判定依据却只拿到 JSON
        return usage("--json 与 --explain 不能同用（一个只出 JSON、一个只打判定依据，选一个）")
    modes = [n for n, on in (("--json", as_json), ("--explain", explain), ("--quiet", quiet)) if on]
    if len(modes) > 1:
        # 修前 if/elif 顺序把后面的模式静默吞掉：`--quiet --explain` 照打逐文件行、`--json --quiet`
        # 只出 JSON 却没有 SUMMARY。输出模式互斥，选一个（gate21 MiniMax boundary P3）。
        return usage(f"{' 与 '.join(modes)} 不能同用（输出模式互斥，选一个）")
    if "--print-skeleton" in args:
        if [a for a in args if not a.startswith("--")]:
            return usage("--print-skeleton 不能与任务书参数同用（会让人误以为文件被检查过）")
        if modes:
            # 修前只打骨架、rc 0：按 `--json` 解析的调用方拿到非 JSON
            return usage(f"--print-skeleton 不能与 {' '.join(modes)} 同用（它只打模板文本）")
        print(SKELETON, end="")
        return 0
    hours = None
    root, root_given = ".", False
    seen_opt = set()
    rest, i = [], 0
    while i < len(args):
        a = args[i]
        if a in ("--recent", "--root"):
            if a in seen_opt:   # last-wins 会把前一个 root 下的任务书整批漏扫，且 rc 0 冒充「扫过」
                return usage(f"{a} 写了两次：后一次会静默覆盖前一次，不猜你的意思")
            seen_opt.add(a)
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
