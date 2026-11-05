#!/usr/bin/env python3
"""check-taskbook-segments.py 的用例（正控 + 负控）。

跑法：python3 -I scripts/test-check-taskbook-segments.py
每个用例是一份最小任务书；负控必须让检查器报出**对应那一条**，不是「随便失败」。
覆盖评审打回的四个假 PASS 面：骨架原样贴入、scope 行含 `<…>`、估算单位/区间、换列表头。
"""

import importlib.util
import sys
import tempfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("ctbs", HERE / "check-taskbook-segments.py")
ctbs = importlib.util.module_from_spec(spec)
spec.loader.exec_module(ctbs)

SMALL = """# #100 feat(test) 小包

## 要做
1. 改 `src/a.rs`
2. 补测试

## 交付前自查
照模板。
"""

BIG_SCOPE = """<!-- pdrs-scope: files=16 lines=1124 modules=2 est_min=180 -->
# #1316 feat(gateway) 大包

## 要做
1. 装配会话域
"""

SEGMENTS_OK = """## 分段交付（大包必填）

| 段 | 涉及文件 | 交付物（含该段自己的 RESULT 片段） | 预估时长 | 段间依赖 |
|----|---------|-----------------------------------|---------|---------|
| S1 交互方法 | `src/interaction.rs` | `S1-RESULT.md` + `final.patch` S1 | 80 min | — |
| S2 HTTP 同域 | `src/native_http.rs` | `S2-RESULT.md` + `final.patch` S2 | 60 min | S1 |

## 续跑

- 进度文件：`_tmp/20261105-issue-100/progress.md`（每段一行）
- 第 N 段失败：读 progress.md 最后一个 done 段，从 N+1 继续。
"""

CASES = []


def case(name, expect_rc, text, must=None):
    CASES.append((name, expect_rc, text, must or []))


case("小包：不要求分段", 0, SMALL)
case("大包 scope 无分段表", 1, BIG_SCOPE, must=["分段交付"])
case("大包 scope + 合规分段表", 0, BIG_SCOPE + SEGMENTS_OK)
case("大包人日回退（约 2 人日）", 1, "# #1 估 2 人日\n\n## 要做\n1. x\n", must=["分段交付"])
case("大包 rs 路径回退（9 个）", 1,
     "# #1\n" + "\n".join(f"- src/m{i}/x.rs" for i in range(9)), must=["分段交付"])
case("段时长 > 90 min", 1, BIG_SCOPE + SEGMENTS_OK.replace("| 80 min |", "| 120 min |"), must=["120 min > 90 min"])
case("缺 续跑 一节", 1, BIG_SCOPE + SEGMENTS_OK.split("## 续跑")[0], must=["续跑"])
case("续跑 写「无需进度文件」", 1,
     BIG_SCOPE + SEGMENTS_OK.split("## 续跑")[0] + "## 续跑\n\n- 无需进度文件，重跑即可。\n",
     must=["进度文件路径"])
case("续跑 留 <N> 占位符", 1,
     BIG_SCOPE + SEGMENTS_OK.replace("_tmp/20261105-issue-100/progress.md", "_tmp/<日期>-issue-<N>/progress.md"),
     must=["进度文件路径"])
case("占位符 <待填>", 1, BIG_SCOPE + SEGMENTS_OK.replace("`src/interaction.rs`", "<待填>"), must=["占位符"])
case("骨架原样贴入（scope 占位）", 2, ctbs.SKELETON, must=["不是整数"])
case("骨架原样贴入（scope 已填，表未填）", 1,
     "<!-- pdrs-scope: files=16 lines=1124 est_min=180 -->\n" + ctbs.SKELETON.split("-->", 1)[1],
     must=["占位符"])
case("只有 1 段", 1,
     BIG_SCOPE + SEGMENTS_OK.replace(
         "| S2 HTTP 同域 | `src/native_http.rs` | `S2-RESULT.md` + `final.patch` S2 | 60 min | S1 |\n", ""),
     must=["只有 1 段"])
case("缺列（没有依赖列）", 1, BIG_SCOPE + SEGMENTS_OK.replace("| 段间依赖 |", "| 备注 |"),
     must=["缺列"])
case("换列表头：预估在第 2 列，120 min 仍要抓到", 1,
     BIG_SCOPE + """## 分段交付

| 段 | 预估时长 | 涉及文件 | 交付物 | 段间依赖 |
|----|---------|---------|--------|---------|
| S1 a | 120 min | `src/a.rs` | `S1-RESULT.md` | — |
| S2 b | 60 min | `src/b.rs` | `S2-RESULT.md` | S1 |

## 续跑

- 进度文件：`_tmp/20261105-issue-100/progress.md`
""", must=["120 min > 90 min"])
case("估时单位「天」不许当 min", 1,
     BIG_SCOPE + SEGMENTS_OK.replace("| 80 min |", "| 1 天 |"), must=["读不出"])
case("估时区间取最大值", 1,
     BIG_SCOPE + SEGMENTS_OK.replace("| 80 min |", "| 80–120 min |"), must=["120 min > 90 min"])
case("双 scope 行：小的在前、大的在后", 1,
     "<!-- pdrs-scope: files=3 lines=200 est_min=60 -->\n<!-- pdrs-scope: files=16 -->\n# x\n",
     must=["分段交付"])
case("scope 行含 <…> 占位符", 2, "<!-- pdrs-scope: files=<待填> -->\n# x\n", must=["不是整数"])
case("scope 行没收到 -->", 2, "<!-- pdrs-scope: files=16\n# x\n", must=["收口"])
case("scope 行写坏", 2, "<!-- pdrs-scope: files=很多 -->\n# x\n", must=["不是整数"])
case("scope 行缺键", 2, "<!-- pdrs-scope: note=x -->\n# x\n", must=["至少要写"])
case("scope 声明未达阈值", 0,
     "<!-- pdrs-scope: files=3 lines=200 est_min=60 -->\n# x\n- `a.rs`\n- `b.rs`\n")
case("空文件不是「小包」", 2, "   \n\n", must=["空文件"])
case("URL 里的 .rs 不计入回退路径数", 0,
     "# #1\n" + "\n".join(f"- https://example.com/m{i}/x.rs" for i in range(9)))


def run():
    fails = 0
    with tempfile.TemporaryDirectory() as td:
        for name, expect, text, must in CASES:
            p = Path(td) / "task.md"
            p.write_text(text, encoding="utf-8")
            r = ctbs.check_file(p)
            out = "；".join(r["problems"]) + " " + ((r["class"] or {}).get("reason") or "")
            ok = r["rc"] == expect and all(s in out for s in must)
            print(f"{'PASS' if ok else 'FAIL'}  {name}：rc={r['rc']}（期望 {expect}）{'' if ok else ' ← ' + out}")
            if not ok:
                fails += 1
    # --print-skeleton 不许与文件参数同用（否则调用方会以为文件被检查过）
    rc = ctbs.main(["x", "--print-skeleton", "task.md"])
    ok = rc == 3
    print(f"{'PASS' if ok else 'FAIL'}  --print-skeleton 带文件参数：rc={rc}（期望 3）")
    fails += 0 if ok else 1
    total = len(CASES) + 1
    print(f"SUMMARY cases={total} failed={fails}")
    return 1 if fails else 0


if __name__ == "__main__":
    sys.exit(run())
