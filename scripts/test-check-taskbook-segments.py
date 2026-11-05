#!/usr/bin/env python3
"""check-taskbook-segments.py 的用例（正控 + 负控）。

跑法：python3 -I scripts/test-check-taskbook-segments.py
每个用例是一份最小任务书；负控必须让检查器报出**对应那一条**，不是「随便失败」。
覆盖两轮评审打回的假 PASS / 假 FAIL 面：骨架原样贴入、scope 行写法、占位符变体、
估算单位/区间/加法、续跑路径的占位与否定写法、非 UTF-8 / BOM、Rust 泛型误伤。
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
case("续跑 留 {日期} 花括号占位", 1,
     BIG_SCOPE + SEGMENTS_OK.replace("_tmp/20261105-issue-100/progress.md", "_tmp/{日期}-issue-{N}/progress.md"),
     must=["进度文件路径"])
case("续跑 只写裸文件名", 1,
     BIG_SCOPE + SEGMENTS_OK.replace("_tmp/20261105-issue-100/progress.md", "progress.md"),
     must=["进度文件路径"])
case("续跑 写成否定句「无需 progress.md」", 1,
     BIG_SCOPE + SEGMENTS_OK.replace("`_tmp/20261105-issue-100/progress.md`（每段一行）", "无需 progress.md，整包重跑"),
     must=["进度文件路径"])
case("续跑 没写失败从哪继续", 1,
     BIG_SCOPE + SEGMENTS_OK.split("## 续跑")[0] + "## 续跑\n\n- 进度文件：`_tmp/20261105-issue-100/progress.md`\n",
     must=["第 N 段失败"])
case("占位符 <待填>", 1, BIG_SCOPE + SEGMENTS_OK.replace("`src/interaction.rs`", "<待填>"), must=["占位符"])
case("占位符 超长中文尖括号", 1,
     BIG_SCOPE + SEGMENTS_OK.replace("`S1-RESULT.md` + `final.patch` S1",
                                     "<请在这里填写本段涉及的全部文件路径与行数范围>"),
     must=["占位符"])
case("占位符 花括号 {待补}", 1, BIG_SCOPE + SEGMENTS_OK.replace("`src/interaction.rs`", "{待补}"), must=["占位符"])
case("占位符 单省略号 …", 1, BIG_SCOPE + SEGMENTS_OK.replace("`src/interaction.rs`", "…"), must=["占位符"])
case("占位符 ？？？", 1, BIG_SCOPE + SEGMENTS_OK.replace("`src/interaction.rs`", "？？？"), must=["占位符"])
case("Rust 泛型不是占位符（正控）", 0,
     BIG_SCOPE + SEGMENTS_OK.replace("`S1-RESULT.md` + `final.patch` S1",
                                     "`S1-RESULT.md`：新增 `Vec<InteractionEvent>` 序列化"))
case("骨架原样贴入（scope 占位）", 2, ctbs.SKELETON, must=["不是整数"])
case("骨架原样贴入（scope 已填，表未填）", 1,
     "<!-- pdrs-scope: files=16 lines=1124 est_min=180 -->\n" + ctbs.SKELETON.split("-->", 1)[1],
     must=["占位符"])
case("只有 1 段", 1,
     BIG_SCOPE + SEGMENTS_OK.replace(
         "| S2 HTTP 同域 | `src/native_http.rs` | `S2-RESULT.md` + `final.patch` S2 | 60 min | S1 |\n", ""),
     must=["只有 1 段"])
case("分段表整行为空", 1,
     BIG_SCOPE + SEGMENTS_OK.replace(
         "| S2 HTTP 同域 | `src/native_http.rs` | `S2-RESULT.md` + `final.patch` S2 | 60 min | S1 |",
         "| | | | | |\n| S2 HTTP 同域 | `src/native_http.rs` | `S2-RESULT.md` + `final.patch` S2 | 60 min | S1 |"),
     must=["整行为空"])
case("单 - 分隔行合法（正控）", 0,
     BIG_SCOPE + SEGMENTS_OK.replace("|----|---------|-----------------------------------|---------|---------|",
                                     "| - | - | - | - | - |"))
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
- 第 N 段失败：从 N+1 继续。
""", must=["120 min > 90 min"])
case("表头写 (h)：裸数字按小时算", 1,
     BIG_SCOPE + SEGMENTS_OK.replace("| 预估时长 |", "| 预估时长(h) |").replace("| 80 min |", "| 2 |"),
     must=["120 min > 90 min"])
case("估时单位「天」不许当 min", 1,
     BIG_SCOPE + SEGMENTS_OK.replace("| 80 min |", "| 1 天 |"), must=["读不出"])
case("估时区间取最大值", 1,
     BIG_SCOPE + SEGMENTS_OK.replace("| 80 min |", "| 80–120 min |"), must=["120 min > 90 min"])
case("估时加法求和（60 + 40）", 1,
     BIG_SCOPE + SEGMENTS_OK.replace("| 80 min |", "| 60 min + 40 min |"), must=["100 min > 90 min"])
case("估时连写求和（1 h 40 min）", 1,
     BIG_SCOPE + SEGMENTS_OK.replace("| 80 min |", "| 1 h 40 min |"), must=["100 min > 90 min"])
case("估时 1 h 30 min = 90 min（正控）", 0,
     BIG_SCOPE + SEGMENTS_OK.replace("| 80 min |", "| 1 h 30 min |"))
case("段名里的 S1 不参与估时", 0,
     BIG_SCOPE + SEGMENTS_OK.replace("| 80 min |", "| S3 分段 60 min |"))
case("双 scope 行：小的在前、大的在后", 1,
     "<!-- pdrs-scope: files=3 lines=200 est_min=60 -->\n<!-- pdrs-scope: files=16 -->\n# x\n",
     must=["分段交付"])
case("scope 大小写/全角冒号/下划线都认", 1,
     "<!-- PDRS_SCOPE： files=16 -->\n# x\n", must=["分段交付"])
case("scope 行含 <…> 占位符", 2, "<!-- pdrs-scope: files=<待填> -->\n# x\n", must=["不是整数"])
case("scope 行没收到 -->", 2, "<!-- pdrs-scope: files=16\n# x\n", must=["没写进", "收到"])
case("scope 标记写在注释外", 2, "pdrs-scope: files=16 est_min=300\n# x\n", must=["没写进"])
case("scope 注释写法不对（前缀有别的词）", 2, "<!-- note pdrs-scope files=16 -->\n# x\n", must=["写法不对"])
case("scope 行写坏", 2, "<!-- pdrs-scope: files=很多 -->\n# x\n", must=["不是整数"])
case("scope 行缺键", 2, "<!-- pdrs-scope: note=x -->\n# x\n", must=["至少要写"])
case("scope 声明未达阈值", 0,
     "<!-- pdrs-scope: files=3 lines=200 est_min=60 -->\n# x\n- `a.rs`\n- `b.rs`\n")
case("空文件不是「小包」", 2, "   \n\n", must=["空文件"])
case("纯 BOM 不是「小包」", 2, "﻿", must=["空文件"])
case("URL 里的 .rs 不计入回退路径数", 0,
     "# #1\n" + "\n".join(f"- https://example.com/m{i}/x.rs" for i in range(9)))
case("URL 后紧跟顿号枚举的 .rs 要计入", 1,
     "# #1\n- 见 https://example.com/a.md、" + "、".join(f"src/m{i}/x.rs" for i in range(8)), must=["分段交付"])


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

        # 非 UTF-8 任务书：必须 rc=2（读不了），不许 traceback 成 rc=1 冒充「分段表不合格」
        bad = Path(td) / "gbk.md"
        bad.write_bytes(b"\xff\xfe\x00bad")
        r = ctbs.check_file(bad)
        ok = r["rc"] == 2 and "读不了" in r["problems"][0]
        print(f"{'PASS' if ok else 'FAIL'}  非 UTF-8 任务书 → rc=2 读不了：rc={r['rc']}")
        fails += 0 if ok else 1

        # --recent 扫描期间树被回收：跳过，不报 ERROR
        r = ctbs.check_file(Path(td) / "gone.md", ignore_missing=True)
        ok = r is None
        print(f"{'PASS' if ok else 'FAIL'}  --recent 下文件消失 → 跳过：{r}")
        fails += 0 if ok else 1

    # 命令行契约
    for name, argv, expect in (
            ("--print-skeleton 带文件参数", ["x", "--print-skeleton", "task.md"], 3),
            ("--print-skeleton 带 --recent", ["x", "--print-skeleton", "--recent", "24"], 3),
            ("未知参数不许静默吞掉", ["x", "--jso", "task.md"], 3),
            ("--recent 缺小时数", ["x", "--recent"], 3),
    ):
        rc = ctbs.main(argv)
        ok = rc == expect
        print(f"{'PASS' if ok else 'FAIL'}  {name}：rc={rc}（期望 {expect}）")
        fails += 0 if ok else 1

    total = len(CASES) + 6
    print(f"SUMMARY cases={total} failed={fails}")
    return 1 if fails else 0


if __name__ == "__main__":
    sys.exit(run())
