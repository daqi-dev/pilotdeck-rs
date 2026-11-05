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
_tmp/20261105-taskbook-segments/SEGMENTS.md）：
  大包 = 任一条成立
    1) 任务书声明 `<!-- pdrs-scope: files=N lines=M est_min=K -->`（可多条，任一条达阈值即大包）
       且 files ≥ 8 或 lines ≥ 1500 或 est_min > 90；
    2) 没有可解析的 scope 行时，正文出现「约/估 … 人日」→ 人日 × 480 min > 90（历史任务书的回退信号）；
    3) 仍判不出时，正文点名的 `*.rs` 路径 ≥ 8 个（回退启发式）。
  scope 行写坏（占位符 / 非整数 / 没写阈值键 / 没收到 `-->` / 没写进 `<!-- -->`）→ 退出 2，不静默退回启发式。

分段表要求（`## 分段交付` 一节里的 markdown 表，≥2 段）：
  列：段名 / 涉及文件 / 交付物（含该段自己的 RESULT 片段）/ 预估时长 / 段间依赖（列序不限，按表头定位）
  每段预估 ≤ 90 min：单位认 min/分钟/m/h/小时，区间（`–`/`~`/`至`）取最大值，
  并列（`+`/`、`/和`/`含` 或 `1 h 30 min` 这种连写）按总时长相加；不认的单位按「读不出」报错；
  另需 `## 续跑` 一节：具体的进度文件路径（带目录的 `…progress.md` / `…进度.md`，不许留 `<…>` / `{…}` / `…`）
  + 「第 N 段失败从哪继续」的最小学样（失败 / 继续 / 接续 / 恢复 / resume）。

退出码：0 通过（含「小包，无需分段」）· 1 命中大包但分段表缺失/不全 · 2 任务书读不了、
为空或 scope 行写坏 · 3 用法错误（多个文件混合时 2 优先于 1，ERROR 比 FAIL 更需要人看）
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

# ---- 占位符 ----------------------------------------------------------------
# 只认「像占位符」的尖括号/花括号内容：含中日韩字，或是已知占位词（N / 日期 / 待填 …）。
# 这样 `Vec<u8>`、`Result<(), E>`、`<br>`、`<https://…>` 这些仓库常态不会被误判（假 FAIL 会拒派合规任务书）。
CJK = re.compile(r"[㐀-鿿]")
ANGLE = re.compile(r"<([^<>\n]{0,200})>")
BRACE = re.compile(r"\{([^{}\n]{0,80})\}")
PLACEHOLDER_WORDS = {
    "n", "n+1", "n-1", "nn", "tbd", "todo", "xxx", "yyy", "???", "待填", "待补", "空",
    "date", "name", "dir", "path", "head", "sha", "reason",
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
    m = BARE_PLACEHOLDER.search(text)
    return m.group(0) if m else None


# ---- scope 行 --------------------------------------------------------------
# 容忍大小写、`pdrs-scope` / `pdrs_scope` / `pdrs scope`、半角与全角冒号、冒号前后空格。
SCOPEISH = re.compile(r"pdrs[\s_-]*scope", re.I)
COMMENT = re.compile(r"<!--(?P<body>.*?)-->", re.S)
SCOPE_BODY = re.compile(r"^\s*pdrs[\s_-]*scope\s*[:：]?\s*(?P<rest>.*)$", re.I)
PERSON_DAY = re.compile(r"(?:约|估|预计|预计约)\s*([0-9]+(?:\.[0-9]+)?)\s*(?:[–\-~至]\s*([0-9]+(?:\.[0-9]+)?)\s*)?人[日天]")
# URL 只吃 ASCII URL 字符：中文顿号/逗号紧跟其后时不许被整串吞掉（否则后面的 .rs 枚举漏计）
URL = re.compile(r"[A-Za-z][A-Za-z0-9+.-]*://[A-Za-z0-9./%#?=&_~:@!$'()*+,;-]+")
RS_PATH = re.compile(r"[A-Za-z0-9_./\-]+\.rs\b")

SECTION = re.compile(r"^(#{2,3})\s+(.*)$")
WANT_COLS = {
    "seg": ("段",),
    "files": ("文件",),
    "deliver": ("交付物", "交付"),
    "est": ("预估", "预计", "时长"),
    "dep": ("依赖",),
}
COL_NAMES = {"seg": "段名", "files": "涉及文件", "deliver": "交付物", "est": "预估时长", "dep": "段间依赖"}


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
        k = k.strip()
        if k in ("files", "lines", "est_min", "modules"):
            if not re.fullmatch(r"[0-9]+", v.strip()):
                return None, f"scope 行 {k}={v.strip()!r} 不是整数（不许留占位符）"
            out[k] = int(v.strip())
        else:
            out[k] = v.strip()
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
    m = PERSON_DAY.search(text)
    if m:
        val = max(float(m.group(1)), float(m.group(2)) if m.group(2) else 0.0)
        if not math.isfinite(val):
            return {"big": None, "reason": "人日估算数值过大/非法，读不出", "metrics": metrics}
        est = int(math.ceil(val * PERSON_DAY_MIN))
        metrics["person_day"] = val
        metrics["est_min"] = est
        return {"big": est > SCOPE_MIN,
                "reason": f"人日估算 {val:g} → est_min={est} {'>' if est > SCOPE_MIN else '≤'} {SCOPE_MIN}",
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
RANGE_MARK = re.compile(r"[–—~～]|至|到|(?<=\d)\s*-\s*(?=\d)")
SUM_MARK = re.compile(r"\+|＋|、|，|,|加|和|含|另")
UNIT_MIN = ("", "min", "mins", "minute", "minutes", "m", "分", "分钟")
UNIT_HOUR = ("h", "hr", "hrs", "hour", "hours", "时", "小时", "个小时")
UNIT_DAY = ("d", "day", "days", "天", "日")


def _unit_minutes(v, unit):
    if unit in UNIT_MIN:
        return v
    if unit in UNIT_HOUR:
        return v * 60
    return None  # 「天 / 日 / d」等未支持单位：宁可报「读不出」，也不许当 min 放行


def parse_est(cell, default_unit=""):
    """从「预估时长」单元格取分钟数。

    规则：区间（`80–120 min`）取最大值；并列（`60 min + 40 min`、`1 h 30 min`）求和——
    此前一律取 max，把「60 + 40 = 100 min」读成 60 min 放行；单值按自身单位换算。
    认不出的单位返回 None（报「读不出」），不许默默当 min。
    """
    toks = [(float(m.group(1)), m.group(2).lower()) for m in EST_TOKEN.finditer(cell)]
    if not toks:
        return None
    # 无单位的数字：先看它后面最近的显式单位，没有再看前面，最后用表头暗示的单位
    units = [u for _, u in toks if u]
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
    if RANGE_MARK.search(cell) and not SUM_MARK.search(cell):
        return max(resolved)
    return sum(resolved)


NEGATION = re.compile(r"不要|不用|不必|无需|无须|不许|不能|不需要|别写|勿写")
RESUME_FAIL_WORDS = ("失败", "继续", "接续", "恢复", "重跑", "resume")
PATH_TOKEN = re.compile(r"[A-Za-z0-9_./-]+\.md")


def resume_problems(resume):
    """`## 续跑` 的最小检查：具体进度文件路径 + 「第 N 段失败从哪继续」的学样。"""
    problems = []
    path, bad_paths = None, []
    for line in resume.splitlines():
        if line.strip().startswith("```"):
            continue
        # 整行带占位符（`<日期>` / `{N}` / `…`）就不是实例路径：整行不认，免得正则从
        # `_tmp/<日期>-issue-<N>/progress.md` 里截出 `/progress.md` 当合规路径（假 PASS 面）。
        line_ph = placeholder_in(line)
        for m in PATH_TOKEN.finditer(line):
            p = m.group(0)
            name = p.rsplit("/", 1)[-1]
            if not re.search(r"progress|进度", name):
                continue
            bad = None
            if line_ph is not None:                            # 整行带占位符
                bad = f"整行含占位符 {line_ph!r}"
            elif "/" not in p:                                 # 裸 `progress.md`：没写目录
                bad = "只有文件名、没有目录"
            elif any(ch in p for ch in "{}<>…") or "..." in p:  # 占位形态
                bad = "还是占位符"
            elif NEGATION.search(line):                         # 「无需 progress.md」
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
            default_unit = ""
            if "est" in col:
                est_header = header[col["est"]].lower()
                if re.search(r"\((h|hr|小时)\)|（(h|hr|小时)）", est_header):
                    default_unit = "h"   # 表头写「预估时长(h)」时，格内裸数字按小时算
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


def check_file(path, ignore_missing=False):
    p = Path(path)
    try:
        if p.stat().st_size > MAX_BYTES:
            return {"file": str(path), "rc": 2,
                    "problems": [f"任务书 {p.stat().st_size} 字节超过 {MAX_BYTES}（不整文件读进内存；先删生成物）"],
                    "class": None}
        text = p.read_text(encoding="utf-8-sig")
    except FileNotFoundError:
        if ignore_missing:      # --recent 扫描期间树被回收：跳过，不报 ERROR
            return None
        return {"file": str(path), "rc": 2, "problems": [f"读不了：找不到 {path}"], "class": None}
    except (OSError, UnicodeDecodeError) as e:
        return {"file": str(path), "rc": 2, "problems": [f"读不了：{e}"], "class": None}
    if not text.strip("﻿\x00 \t\r\n\f\v"):
        return {"file": str(path), "rc": 2, "problems": ["任务书是空文件（不是「小包」）"], "class": None}
    cls = classify(text)
    if cls["big"] is None:
        return {"file": str(path), "rc": 2, "problems": [cls["reason"]], "class": cls}
    if not cls["big"]:
        return {"file": str(path), "rc": 0, "problems": [], "class": cls}
    problems = check_segments(text)
    return {"file": str(path), "rc": 1 if problems else 0,
            "problems": problems, "class": cls}


def find_recent(root, hours):
    """扫最近 N 小时生成的任务书。

    刚写几秒的文件跳过：写方不是原子写时，读到半截 scope 行会误报 rc 2（假 ERROR）。
    文件在扫描与读取之间被删（worktree 回收）时静默跳过——那种「读不了」不是任务书的问题。
    """
    import time
    now = time.time()
    cutoff = now - hours * 3600
    out = []
    for p in sorted(Path(root).glob("_build/wt-*/_tmp/*/task.md")):
        try:
            st = p.stat()
        except OSError:
            continue
        if st.st_mtime < cutoff or now - st.st_mtime < 10 or st.st_size == 0:
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
    files = rest
    if hours is not None:
        files = [str(p) for p in find_recent(root, hours)] + files
    if not files:
        return usage()

    results = [r for r in (check_file(f, ignore_missing=hours is not None) for f in files) if r is not None]
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
