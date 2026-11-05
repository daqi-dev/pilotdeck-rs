#!/usr/bin/env python3
"""任务书分段强制检查：大包必须按文件切段，每段可独立收货。

踩过的坑：2026-11-05 夜 #1316 跑满 7200 s 被 kill，16 个文件写完但 RESULT / 负控 /
final.patch 全缺；同一晚 #1334 #1336 #1370 #1328 #1339 同类超时（memory「glm-timeout-3600」）。
现有「每 30 分钟 WIP 提交」只防丢进度，不防「一整段跑不完」。所以把切段做成派单前的机检。

用法：
  python3 scripts/check-taskbook-segments.py <任务书> [<任务书> ...]
  python3 scripts/check-taskbook-segments.py --json <任务书>
  python3 scripts/check-taskbook-segments.py --explain <任务书>   # 只打印分类依据
  python3 scripts/check-taskbook-segments.py --print-skeleton     # 打印分段表骨架（派单被拒时贴进补充文件）
  python3 scripts/check-taskbook-segments.py --recent 24 [--root <主仓>]  # 扫最近 24 h 生成的任务书

判定（阈值依据见 _tmp/20261105-taskbook-segments/SEGMENTS.md 的对照表）：
  大包 = 任一条成立
    1) 任务书声明 `<!-- pdrs-scope: files=N lines=M est_min=K -->` 且 files ≥ 8 或 lines ≥ 1500 或 est_min > 90；
    2) 没有 scope 行时，正文出现「约/估 … 人日」→ 人日 × 480 min > 90（历史任务书的回退信号）；
    3) 仍判不出时，正文点名的 `*.rs` 路径 ≥ 8 个（回退启发式）。
  命中大包却缺分段表 / 表不全 / 有占位符 / 段时长 > 90 min / 缺续跑说明 → 退出 1 并逐项列出缺什么。

分段表要求（`## 分段交付` 一节里的 markdown 表，≥2 段）：
  列：段名 / 涉及文件 / 交付物（含该段自己的 RESULT 片段）/ 预估时长 / 段间依赖
  每段预估 ≤ 90 min；另需 `## 续跑` 一节，写明进度文件路径与「第 N 段失败从哪继续」。

退出码：0 通过（含「小包，无需分段」）· 1 命中大包但分段表缺失/不全 · 2 任务书读不了或 scope 行写坏 · 3 用法错误
"""

import json
import math
import re
import sys
from pathlib import Path

SCOPE_FILES = 8
SCOPE_LINES = 1500
SCOPE_MIN = 90          # 单段 / 单包允许的最大预估时长（min）
SEGMENT_MAX_MIN = 90
FALLBACK_RS_PATHS = 8
PERSON_DAY_MIN = 480    # 1 人日 ≈ 8 h；只用于把历史任务书里「约 N 人日」换算成分钟
PLACEHOLDER = re.compile(r"<待填>|<TODO>|\bTBD\b|\bTODO\b|xxx|\?\?\?")

SCOPE_LINE = re.compile(r"<!--\s*pdrs-scope:\s*(?P<body>[^>]*?)-->")
PERSON_DAY = re.compile(r"(?:约|估|预计|预计约)\s*([0-9]+(?:\.[0-9]+)?)\s*(?:[–\-~至]\s*([0-9]+(?:\.[0-9]+)?)\s*)?人[日天]")
RS_PATH = re.compile(r"[A-Za-z0-9_./\-]+\.rs\b")

SECTION = re.compile(r"^(#{2,3})\s+(.*)$")
WANT_COLS = {
    "seg": ("段",),
    "files": ("文件",),
    "deliver": ("交付物", "交付"),
    "est": ("预估", "预计", "时长"),
    "dep": ("依赖",),
}


def parse_scope(text):
    """返回 (scope_dict|None, error|None)。"""
    m = SCOPE_LINE.search(text)
    if not m:
        return None, None
    body = m.group("body").strip()
    out = {}
    for tok in re.split(r"[\s,;]+", body):
        if not tok:
            continue
        if "=" not in tok:
            return None, f"scope 行里有无法解析的片段：{tok!r}（应形如 files=16 lines=1124 est_min=180）"
        k, v = tok.split("=", 1)
        if k in ("files", "lines", "est_min", "modules"):
            if not re.fullmatch(r"[0-9]+", v):
                return None, f"scope 行 {k}={v!r} 不是整数"
            out[k] = int(v)
        else:
            out[k] = v
    if not any(k in out for k in ("files", "lines", "est_min")):
        return None, "scope 行至少要写 files= / lines= / est_min= 之一"
    return out, None


def rs_paths(text):
    return {p for p in RS_PATH.findall(text) if not p.startswith("http")}


def classify(text):
    """返回 dict：big / reason / metrics。"""
    scope, err = parse_scope(text)
    metrics = {"scope": scope, "scope_error": err, "rs_paths": len(rs_paths(text))}
    if err:
        return {"big": None, "reason": err, "metrics": metrics}
    if scope:
        triggers = []
        if scope.get("files", 0) >= SCOPE_FILES:
            triggers.append(f"files={scope['files']} ≥ {SCOPE_FILES}")
        if scope.get("lines", 0) >= SCOPE_LINES:
            triggers.append(f"lines={scope['lines']} ≥ {SCOPE_LINES}")
        if scope.get("est_min", 0) > SCOPE_MIN:
            triggers.append(f"est_min={scope['est_min']} > {SCOPE_MIN}")
        return {"big": bool(triggers), "reason": "；".join(triggers) or "scope 声明未达阈值", "metrics": metrics}
    m = PERSON_DAY.search(text)
    if m:
        val = max(float(m.group(1)), float(m.group(2)) if m.group(2) else 0.0)
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
    """取以 keyword 开头的 ## 一级小节的正文（到下一个 ## 为止）。"""
    lines = text.splitlines()
    out, grab, level = [], False, 0
    for line in lines:
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
    parts = [c.strip() for c in row.strip("|").split("|")]
    return parts


def parse_est(cell):
    m = re.search(r"([0-9]+(?:\.[0-9]+)?)\s*(min|分钟|m\b|h\b|小时)?", cell)
    if not m:
        return None
    v = float(m.group(1))
    unit = (m.group(2) or "min").strip()
    if unit.startswith("h") or unit == "小时":
        v *= 60
    return v


def check_segments(text):
    """返回问题列表（空 = 通过）。"""
    problems = []
    body = section_text(text, "分段交付")
    if body is None:
        return ["缺 `## 分段交付` 一节（大包必须按文件切段，模板见 ~/ws/pdrs-ops/templates/taskbook-segments.md）"]
    if PLACEHOLDER.search(body):
        problems.append("分段表里还有占位符（<待填> / TBD / TODO 等），须填实现值")
    rows = [cells(l) for l in body.splitlines()]
    rows = [r for r in rows if r]
    if not rows:
        return problems + ["`## 分段交付` 里没有表格（需 markdown 表：段名 / 涉及文件 / 交付物 / 预估时长 / 段间依赖）"]
    header, data = rows[0], []
    for r in rows[1:]:
        if all(re.fullmatch(r":?-{2,}:?", c) for c in r if c):
            continue
        data.append(r)
    joined = " ".join(header)
    missing_cols = [k for k, alts in WANT_COLS.items() if not any(a in joined for a in alts)]
    if missing_cols:
        problems.append("分段表缺列：" + "、".join({"seg": "段名", "files": "涉及文件", "deliver": "交付物",
                                                     "est": "预估时长", "dep": "段间依赖"}[k] for k in missing_cols))
    if len(data) < 2:
        problems.append(f"分段表只有 {len(data)} 段；大包至少要切成 2 段（每段可独立收货）")
    for i, r in enumerate(data, 1):
        if len(r) < len(header):
            problems.append(f"第 {i} 行字段数 {len(r)} < 表头 {len(header)}")
        est_cell = r[3] if len(r) > 3 else ""
        est = parse_est(est_cell)
        if est is None:
            problems.append(f"第 {i} 段预估时长读不出（{est_cell!r}）")
        elif est > SEGMENT_MAX_MIN:
            problems.append(f"第 {i} 段预估 {est:g} min > {SEGMENT_MAX_MIN} min（切成更小的段）")
        for j, c in enumerate(r):
            if not c and j < len(header):
                problems.append(f"第 {i} 段「{header[j]}」为空")
    resume = section_text(text, "续跑")
    if resume is None:
        problems.append("缺 `## 续跑` 一节（写清进度文件路径 + 第 N 段失败从哪继续）")
    elif not re.search(r"进度|progress", resume):
        problems.append("`## 续跑` 里没写进度文件路径（用 `_tmp/<日期>-issue-<N>/progress.md`）")
    return problems


SKELETON = """<!-- pdrs-scope: files=<待填> lines=<待填> modules=<待填> est_min=<待填> -->

## 分段交付（大包必填：每段一个可独立收货的交付）

| 段 | 涉及文件 | 交付物（含该段自己的 RESULT 片段） | 预估时长 | 段间依赖 |
|----|---------|-----------------------------------|---------|---------|
| S1 <段名> | `src/...` | `S1-RESULT.md` + `final.patch` 的 S1 部分 | 80 min | — |
| S2 <段名> | `src/...` | `S2-RESULT.md` + `final.patch` 的 S2 部分 | 80 min | S1 |

每段收工即可交付，不要求整包做完；段间不共写同一文件。

## 续跑

- 进度文件：`_tmp/<日期>-issue-<N>/progress.md`（每段开工 / 收工各追加一行 `S<n> start|done <时间> <HEAD>`）
- 第 N 段失败：读 progress.md 最后一个 done 段，先 `git apply` 该段 final.patch 分片，再从 N+1 段继续；不重做已 done 段。
"""


def check_file(path, as_json=False):
    try:
        text = Path(path).read_text(encoding="utf-8")
    except OSError as e:
        return {"file": str(path), "rc": 2, "verdict": "ERROR", "problems": [f"读不了：{e}"], "class": None}
    cls = classify(text)
    if cls["big"] is None:
        return {"file": str(path), "rc": 2, "verdict": "ERROR", "problems": [cls["reason"]], "class": cls}
    if not cls["big"]:
        return {"file": str(path), "rc": 0, "verdict": "OK", "problems": [], "class": cls}
    problems = check_segments(text)
    return {"file": str(path), "rc": 1 if problems else 0,
            "verdict": "FAIL" if problems else "OK", "problems": problems, "class": cls}


def find_recent(root, hours):
    import time
    cutoff = time.time() - hours * 3600
    out = []
    for p in sorted(Path(root).glob("_build/wt-*/_tmp/*/task.md")):
        try:
            if p.stat().st_mtime >= cutoff:
                out.append(p)
        except OSError:
            continue
    return out


def main(argv):
    args = list(argv[1:])
    as_json = "--json" in args
    explain = "--explain" in args
    quiet = "--quiet" in args
    if "--print-skeleton" in args:
        print(SKELETON, end="")
        return 0
    hours = None
    if "--recent" in args:
        i = args.index("--recent")
        try:
            hours = float(args[i + 1])
        except (IndexError, ValueError):
            print("用法：--recent <小时数>", file=sys.stderr)
            return 3
        del args[i:i + 2]
    root = "."
    if "--root" in args:
        i = args.index("--root")
        try:
            root = args[i + 1]
        except IndexError:
            print("用法：--root <主仓>", file=sys.stderr)
            return 3
        del args[i:i + 2]
    files = [a for a in args if not a.startswith("--")]
    if hours is not None:
        files = [str(p) for p in find_recent(root, hours)] + files
    if not files:
        print(__doc__.strip().splitlines()[0], file=sys.stderr)
        print("用法：check-taskbook-segments.py [--json] [--explain] [--recent N [--root DIR]] <任务书>...", file=sys.stderr)
        return 3

    results = [check_file(f, as_json) for f in files]
    if as_json:
        print(json.dumps(results, ensure_ascii=False, indent=2))
    elif explain:
        for r in results:
            c = r["class"] or {}
            print(f"{r['verdict']}\t{r['file']}\t大包={c.get('big')}\t依据：{c.get('reason')}")
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
    if any(r["rc"] == 1 for r in results):
        return 1
    if any(r["rc"] == 2 for r in results):
        return 2
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
