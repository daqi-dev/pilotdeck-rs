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

判定（阈值依据见 scripts/README.md「check-taskbook-segments.py」一节，原始对照表在
scripts/README.md（阈值依据；一次性论证原件在 _tmp/20261105-taskbook-segments/SEGMENTS.md，回收后以 README 为准））：
  大包 = 任一条成立
    1) 任务书声明 `<!-- pdrs-scope: files=N lines=M est_min=K -->`（可多条，任一条达阈值即大包）
       且 files ≥ 8 或 lines ≥ 1500 或 est_min > 90；
    2) 没有可解析的 scope 行时，正文出现「约/估 … 人日」→ 人日 × 480 min > 90（历史任务书的回退信号，
       多条取最大值）；
    3) 仍判不出时，正文点名的 `*.rs` 路径 ≥ 8 个（回退启发式）。
  scope 行写坏（占位符 / 非整数 / 没写阈值键 / 没收到 `-->` / 没写进 `<!-- -->`）→ 退出 2，不静默退回启发式。
  代码围栏（``` / ~~~）里的内容一律不算数：贴「上一单的示例分段表」不能当成本包的分段表，
  里面残留的 `<段名>` 也不该把合规任务书打回。

分段表要求（`## 分段交付` 一节里的 markdown 表，≥2 段）：
  列：段名 / 涉及文件 / 交付物（含该段自己的 RESULT 片段）/ 预估时长 / 段间依赖（列序不限，按表头定位）
  每段预估 ≤ 90 min：单位认 min/分钟/m/h/小时，区间（`–`/`~`/`至`/`/`/`上限`）取最大值，
  明确加法（`+`/`加`）或连写（`1 h 30 min`、`1h30m`）按总时长相加（加法优先于「最多 / 上限」措辞）；不认的单位按「读不出」报错；
  另需 `## 续跑` 一节：具体的进度文件路径（带目录的 `…progress.md` / `…进度.md`，不许留 `<…>` / `{…}` / `…`，
  不许写成「无需 / 不用 …」）+「第 N 段失败从哪继续」的最小学样（失败 / 继续 / 接续 / 恢复 / resume）。

退出码：0 通过（含「小包，无需分段」，`--recent` 没扫到也算 0）· 1 命中大包但分段表缺失/不全 ·
2 任务书读不了、为空、读取期间被改写或 scope 行写坏 · 3 用法错误
（多个文件混合时 2 优先于 1，ERROR 比 FAIL 更需要人看）
"""

import json
import math
import re
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
ANGLE = re.compile(r"<([^<>\n]{0,200})>")
BRACE = re.compile(r"\{([^{}\n]{0,80})\}")
PLACEHOLDER_WORDS = {
    "n", "n+1", "n-1", "nn", "tbd", "todo", "xxx", "yyy", "???", "待填", "待补", "空",
    "date", "name", "dir", "path", "head", "sha", "reason", "file", "id", "num", "idx", "x",
}
# 裸省略号 / 问号 / 英文占位词（CJK 前后没有 \b，用显式 ASCII 环视）
BARE_PLACEHOLDER = re.compile(r"\.\.\.|…|。。。|\?\?\?|？？？|(?<![A-Za-z])(?:tbd|todo|xxx)(?![A-Za-z])", re.I)


def _looks_placeholder(content):
    c = content.strip().strip("`'\"")
    if not c:
        return True
    if CJK.search(c):
        return True
    return c.lower() in PLACEHOLDER_WORDS


def placeholder_in(text):
    """返回第一个占位符字面量；没有返回 None。"""
    for m in ANGLE.finditer(text):
        if _looks_placeholder(m.group(1)):
            return m.group(0)
    for m in BRACE.finditer(text):
        if _looks_placeholder(m.group(1)):
            return m.group(0)
    # 裸占位符前先把尖括号/花括号整段抹掉：`<https://…>`、`Vec<u8>` 里的 `…` 不是占位符
    masked = ANGLE.sub(lambda m: " " * len(m.group(0)), text)
    masked = BRACE.sub(lambda m: " " * len(m.group(0)), masked)
    m = BARE_PLACEHOLDER.search(masked)
    return m.group(0) if m else None


# ---- scope 行 --------------------------------------------------------------
# 容忍大小写、`pdrs-scope` / `pdrs_scope` / `pdrs scope`、半角与全角冒号、半角与全角连字符、冒号前后空格。
DASHES = r"[\s_\-－—‐‑]"
SCOPEISH = re.compile(rf"pdrs{DASHES}*scope", re.I)
COMMENT = re.compile(r"<!--(?P<body>.*?)-->", re.S)
SCOPE_BODY = re.compile(rf"^\s*pdrs{DASHES}*scope\s*[:：]?\s*(?P<rest>.*)$", re.I)
PERSON_DAY = re.compile(r"(?:约|估|预计|预计约)\s*([0-9]+(?:\.[0-9]+)?)\s*(?:[–\-~至]\s*([0-9]+(?:\.[0-9]+)?)\s*)?人[日天]")
# URL 只吃 ASCII URL 字符：中文顿号/逗号/分号紧跟其后时不许被整串吞掉（否则后面的 .rs 枚举漏计）
URL = re.compile(r"[A-Za-z][A-Za-z0-9+.-]*://[A-Za-z0-9./%#?=&_~:@!$'()*+-]+")
RS_PATH = re.compile(r"[A-Za-z0-9_./\-]+\.rs\b")
# 代码围栏：整段（含内容）抹成空行，行数不变
FENCE = re.compile(r"(?ms)^[ \t]*(?P<f>```+|~~~+)[^\n]*\n.*?(?:^[ \t]*(?P=f)[^\n]*$|\Z)")

SECTION = re.compile(r"^(#{2,3})\s+(.*)$")
WANT_COLS = {
    "seg": ("段",),
    "files": ("文件",),
    "deliver": ("交付物", "交付"),
    "est": ("预估", "预计", "时长"),
    "dep": ("依赖",),
}
COL_NAMES = {"seg": "段名", "files": "涉及文件", "deliver": "交付物", "est": "预估时长", "dep": "段间依赖"}


def strip_fences(text):
    """抹掉代码围栏里的内容（保留行数）。围栏里的示例表既不算合规，也不该被当占位符打回。"""
    return FENCE.sub(lambda m: "\n" * m.group(0).count("\n"), text)


def fence_unclosed(text):
    """有开栏但没有闭合行（FENCE 匹配到了文件尾）：此时围栏后的正文会被整段抹掉，不能静默判小包。"""
    for m in FENCE.finditer(text):
        lines = m.group(0).rstrip("\n").splitlines()
        if len(lines) < 2 or not lines[-1].lstrip().startswith(m.group("f")):
            return True
    return False


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
        k, v = k.strip(), v.strip()
        if k in ("files", "lines", "est_min", "modules"):
            # 位数上限：Python 3.11+ 的 int() 对 >4300 位会抛 ValueError（崩溃冒充 FAIL）
            if not re.fullmatch(r"[0-9]{1,18}", v):
                return None, f"scope 行 {k}={v[:40]!r} 不是整数（不许留占位符）"
            out[k] = int(v)
        else:
            out[k] = v
    if not any(k in out for k in ("files", "lines", "est_min")):
        return None, "scope 行至少要写 files= / lines= / est_min= 之一"
    return out, None


def parse_scopes(text):
    """返回 (scopes, errors)。多条 scope 行全部解析；任一条写坏都要报出来。

    「写了 scope 但没写成合法注释」比「没写 scope」危险得多：静默退回启发式会把一个
    est_min=300 的大包当小包放行（#1316 类）。所以下面三种都算写坏（rc 2）：
      ① 出现 `pdrs-scope` 字样但不在 `<!-- -->` 里；② 注释没收到 `-->`；③ 注释体解析不出阈值键。
    """
    errors, scopes = [], []
    spans = []
    for m in COMMENT.finditer(text):
        spans.append(m.span())
        body = m.group("body")
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
    # 注释外的 scope 字样（含没收到 --> 的半截注释）
    for m in SCOPEISH.finditer(text):
        if not any(a <= m.start() < b for a, b in spans):
            line = text.count("\n", 0, m.start()) + 1
            errors.append(f"第 {line} 行的 scope 标记没写进 `<!-- -->` 注释（或注释没收到 `-->`），"
                          "不能静默忽略——写坏了按「读不了」处理")
    return scopes, errors


def rs_paths(text):
    text = URL.sub(" ", text)  # URL 里的 .rs 不算任务目标
    return {p for p in RS_PATH.findall(text) if not p.startswith("//") and "://" not in p}


def classify(text):
    """返回 dict：big(True/False/None) / reason / metrics。"""
    scopes, errors = parse_scopes(text)
    metrics = {"scopes": scopes, "scope_errors": errors, "rs_paths": len(rs_paths(text))}
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
    for m in PERSON_DAY.finditer(text):
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


def is_separator(row):
    """GFM 分隔行：`---` / `:--:` / 单 `-` 都合法；全空行不算分隔行（那是坏行）。"""
    filled = [c for c in row if c]
    return bool(filled) and all(re.fullmatch(r":?-+:?", c) for c in filled)


EST_TOKEN = re.compile(r"(?<![A-Za-z0-9.])([0-9]+(?:\.[0-9]+)?)\s*([A-Za-z一-鿿]*)")
# 小时+分钟的连写（`1h40m` / `1h45min` / `1h30`）：直接切 token 时后一个数字会被 lookbehind
# 挡掉、只读成 60 min。先把「N 小时 M 分」整体改写成两个带单位的 token 再切。
COMPACT_UNIT = re.compile(
    r"(?<![0-9.])(?P<h>[0-9]+)\s*(?:hours?|hrs?|h|小时|时)\s*(?P<m>[0-9]{1,2})\s*"
    r"(?:minutes?|mins?|min|m|分钟|分)?(?!\s*[0-9])", re.I)
# 区间 / 上限语义取最大值：`80–120 min`、`80/90 min`、`80 min（上限 90 min）`
RANGE_MARK = re.compile(r"[–—~～/]|至|到|上限|不超过|最多|≤|(?<=\d)\s*-\s*(?=\d)")
# 只有 `+` / `＋` / `加` 是明确加法；`、，,` 是列表分隔符，`含` 常是「含联调」、`和` 常是
# 「S1 和 S2」——把这些当加法会把 `60–80 min，含联调` 加成 140（假 FAIL）。
ADD_MARK = re.compile(r"\+|＋|加")
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
    for tok in re.findall(r"[a-z一-鿿]+", header_cell.lower()):
        if tok in UNIT_HOUR or tok == "工时":
            return "h"
        if tok in UNIT_DAY or tok in ("人日", "工作日"):
            return "d"     # _unit_minutes 会判「读不出」
        if tok in UNIT_MIN:
            return ""
    return ""


def parse_est(cell, default_unit=""):
    """从「预估时长」单元格取分钟数。

    规则：区间/上限（`80–120 min`、`80/90 min`、`上限 90 min`）取最大值；
    并列或连写（`60 min + 40 min`、`1 h 30 min`、`1h30m`）求和——此前一律取 max，
    把「60 + 40 = 100 min」读成 60 min 放行；单值按自身单位换算。
    认不出的单位返回 None（报「读不出」），不许默默当 min。
    """
    cell = COMPACT_UNIT.sub(lambda m: f"{m.group('h')} h {m.group('m')} min", cell)
    toks = [(float(m.group(1)), m.group(2).lower()) for m in EST_TOKEN.finditer(cell)]
    if not toks:
        return None
    # 无单位的数字：先看它后面最近的显式单位，没有再看前面，最后用表头暗示的单位
    resolved = []
    for i, (v, u) in enumerate(toks):
        if not u:
            after = next((t[1] for t in toks[i + 1:] if t[1]), "")
            before = next((t[1] for t in reversed(toks[:i]) if t[1]), "")
            u = after or before or default_unit
        mins = _unit_minutes(v, u)
        if mins is None:
            return None
        resolved.append(mins)
    if len(resolved) == 1:
        return resolved[0]
    if ADD_MARK.search(cell):   # `+` / `加` 是明确加法：后面再写「最多 / 上限」也要求和
        return sum(resolved)
    if RANGE_MARK.search(cell):
        return max(resolved)    # 区间 / 上限：按最大值判（「60–80 min，含联调」里的逗号不算并列）
    return sum(resolved)


NEGATION = re.compile(r"不要|不用|不必|无需|无须|不许|不能|不需要|别写|勿写")
RESUME_FAIL_WORDS = ("失败", "继续", "接续", "恢复", "重跑", "resume")
# 路径允许中文（`…/进度.md`、`…/评审记录/progress.md`）；把 `<>{}}…` 也吃进来，占位形态才能整串识别
PATH_TOKEN = re.compile(r"[A-Za-z0-9_./㐀-鿿<>{}…-]+\.md")
NEG_WINDOW = 30         # 只看路径所在分句（路径前 30 字符里最后一个句读之后）：
                        # 「不需要在这里写 progress.md」命中，「…progress.md（不要提交进 git）」不命中


def resume_problems(resume):
    """`## 续跑` 的最小检查：具体进度文件路径 + 「第 N 段失败从哪继续」的学样。"""
    problems = []
    path, bad_paths = None, []
    for line in resume.splitlines():
        for m in PATH_TOKEN.finditer(line):
            p = m.group(0)
            name = p.rsplit("/", 1)[-1]
            if not re.search(r"progress|进度", name):
                continue
            bad = None
            if any(ch in p for ch in "{}<>…") or "..." in p:
                bad = "还是占位符"
            elif "/" not in p:
                bad = "只有文件名、没有目录"
            elif p.startswith("//"):
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
    if not any(w in resume.lower() for w in RESUME_FAIL_WORDS):
        problems.append("`## 续跑` 里没写「第 N 段失败从哪继续」（要有 失败 / 继续 / 接续 / 恢复 / 重跑 之一的说明）")
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
            for r in rows[1:]:
                if is_separator(r):
                    continue
                if not any(r):
                    problems.append(f"分段表第 {len(data) + 1} 行整行为空（不许用空行占位）")
                    continue
                data.append(r)
            col = {}
            for key, alts in WANT_COLS.items():
                for i, h in enumerate(header):
                    if any(a in h for a in alts):
                        col[key] = i
                        break
            missing_cols = [COL_NAMES[k] for k in COL_NAMES if k not in col]
            if missing_cols:
                problems.append("分段表缺列：" + "、".join(missing_cols))
            if len(data) < 2:
                problems.append(f"分段表只有 {len(data)} 段；大包至少要切成 2 段（每段可独立收货）")
            default_unit = _header_unit(header[col["est"]]) if "est" in col else ""
            for i, r in enumerate(data, 1):
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
                        problems.append(f"第 {i} 段预估时长读不出（{est_cell!r}；只认 min / 分钟 / h / 小时）")
                    elif est > SEGMENT_MAX_MIN:
                        problems.append(f"第 {i} 段预估 {est:g} min > {SEGMENT_MAX_MIN} min（切成更小的段）")
            if placeholder_in(body) and not any("占位符" in p for p in problems):
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


def _err(path, detail):
    return {"file": str(path), "rc": 2, "problems": [detail], "class": None}


def check_file(path, ignore_missing=False):
    p = Path(path)
    try:
        st1 = p.stat()
    except FileNotFoundError:
        if ignore_missing:      # --recent 扫描期间树被回收：跳过，不报 ERROR
            return None
        return _err(path, f"读不了：找不到 {path}")
    except OSError as e:
        return _err(path, f"读不了：{e}")
    if st1.st_size > MAX_BYTES:
        return _err(path, f"任务书 {st1.st_size} 字节超过 {MAX_BYTES}（不整文件读进内存；先删生成物）")
    # 限长读：stat 与 read 之间文件继续增长时不至于把几百 MB 读进内存（TOCTOU）
    try:
        with p.open("rb") as f:
            data = f.read(MAX_BYTES + 1)
    except FileNotFoundError:
        if ignore_missing:
            return None
        return _err(path, f"读不了：找不到 {path}")
    except OSError as e:
        return _err(path, f"读不了：{e}")
    if len(data) > MAX_BYTES:
        return _err(path, f"任务书超过 {MAX_BYTES} 字节（不整文件读进内存；先删生成物）")
    try:
        text = data.decode("utf-8-sig")
    except UnicodeDecodeError as e:
        return _err(path, f"读不了：{e}")
    # 读后复核：挡住 stat→read→stat 之间发生的写入（写方正在写的瞬间）。
    # 写方停顿 > FRESH_SEC 后再续写的半截任务书挡不住（窗口内没有新写），见 find_recent 的说明。
    try:
        st2 = p.stat()
    except OSError:
        st2 = None
    if st2 is not None and (st2.st_size != st1.st_size or st2.st_mtime != st1.st_mtime):
        # 不静默跳过：跳过等于「本轮没检查也算过」（fail-open）。写方补完后重跑即可。
        return _err(path, "任务书在读取过程中被改写（可能还在写），稍后重跑")
    if all(ch.isspace() or ch in "﻿\x00" for ch in text):
        return _err(path, "任务书是空文件（不是「小包」）")
    if fence_unclosed(text):
        return _err(path, "代码围栏没有闭合（``` 开了没收）；围栏后的 scope / 文件清单会被整段吞掉，先补齐再跑")
    body = strip_fences(text)
    if not body.strip():
        return _err(path, "任务书去掉代码围栏后没有正文（不是「小包」）")
    cls = classify(text)   # 分类用原文：围栏里列的文件清单 / scope 行也算数（fail-safe 方向）
    if cls["big"] is None:
        return {"file": str(path), "rc": 2, "problems": [cls["reason"]], "class": cls}
    if not cls["big"]:
        return {"file": str(path), "rc": 0, "problems": [], "class": cls}
    problems = check_segments(body)
    return {"file": str(path), "rc": 1 if problems else 0,
            "problems": problems, "class": cls}


def _is_dispatch_copy(p):
    """`remote-run/stage|artifacts` 里是同一份任务书的派发副本，`.prev-*` 是旧轮次备份：都不重复检查。"""
    return any(part == "remote-run" or part.startswith(".prev") for part in p.parts[:-1])


def find_recent(root, hours):
    """扫最近 N 小时生成的任务书。

    刚写 FRESH_SEC 秒内的文件跳过：写方不是原子写时，读到半截 scope 行会误报 rc 2（假 ERROR）。
    文件在扫描与读取之间被删（worktree 回收）时静默跳过——那种「读不了」不是任务书的问题。
    check_file 里的「读后复核」只挡 stat→read→stat 这个毫秒级窗口内的写入；
    **写方停顿超过 FRESH_SEC 再续写**时窗口内没有新写，半截内容仍会被判成「小包」——
    这条边界没堵（要堵得靠写方原子落盘或任务书完成标记），别把它当成已解决。
    mtime 在未来的（时钟偏移）不跳过，否则它在墙钟追上之前永远扫不到。
    """
    import time
    now = time.time()
    cutoff = now - hours * 3600
    out = []
    # 递归扫：修复 / 续跑 / 重生成轮次的任务书在 _tmp/<单>/fix-rN/、resume*/、regen*/ 下一层，
    # 只认一层会把它们整类漏掉（round-start 9d 兜底轮对这批假 PASS）。
    try:
        candidates = sorted(Path(root).glob("_build/wt-*/_tmp/**/task.md"))
    except OSError:
        return out      # 目录在迭代中被并发回收：按空处理，别抛 traceback（rc 1 会冒充「大包不合格」）
    for p in candidates:
        if _is_dispatch_copy(p):
            continue
        try:
            st = p.stat()
        except OSError:
            continue
        if st.st_mtime < cutoff or 0 <= now - st.st_mtime < FRESH_SEC:
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
    root = "."
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
            i += 2
            continue
        if a == "--root":
            if i + 1 >= len(args):
                return usage("--root 需要目录")
            root = args[i + 1]
            i += 2
            continue
        if not a.startswith("--"):
            rest.append(a)
        i += 1
    if hours is not None and not Path(root).is_dir():
        return usage(f"--root 不是目录：{root}")
    scanned = [str(p) for p in find_recent(root, hours)] if hours is not None else []
    files = list(dict.fromkeys(scanned + rest))   # 同一个文件既被扫到又被点名时不检查两遍
    if not files:
        if hours is not None:      # 合法空扫描：没有新任务书不算用法错误
            print(f"（--recent {hours:g}h 在 {root} 下没扫到任务书；刚写不到 {FRESH_SEC} s 的会跳过）",
                  file=sys.stderr)
            if as_json:
                print("[]")        # 按行解析 JSON 的调用方不能收到一行文本 SUMMARY
            elif not quiet:        # --quiet 只压正文；SUMMARY 照打，「没扫到」与「都通过」要能分辨
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
