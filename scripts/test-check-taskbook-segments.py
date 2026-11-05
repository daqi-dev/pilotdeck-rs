#!/usr/bin/env python3
"""check-taskbook-segments.py 的用例（正控 + 负控）。

跑法：python3 -I scripts/test-check-taskbook-segments.py
每个用例是一份最小任务书；负控必须让检查器报出**对应那一条**，不是「随便失败」。
覆盖三轮评审打回的假 PASS / 假 FAIL 面：骨架原样贴入、scope 行写法、占位符变体、
估算单位/区间/加法/连写、续跑路径的占位与否定写法、代码围栏、非 UTF-8 / BOM / 全角空白、
Rust 泛型与 autolink 误伤、--recent 与显式参数的退出码。
"""

import importlib.util
import json
import os
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

TABLE = """| 段 | 涉及文件 | 交付物（含该段自己的 RESULT 片段） | 预估时长 | 段间依赖 |
|----|---------|-----------------------------------|---------|---------|
| S1 交互方法 | `src/interaction.rs` | `S1-RESULT.md` + `final.patch` S1 | 80 min | — |
| S2 HTTP 同域 | `src/native_http.rs` | `S2-RESULT.md` + `final.patch` S2 | 60 min | S1 |
"""

RESUME = """
## 续跑

- 进度文件：`_tmp/20261105-issue-100/progress.md`（每段一行）
- 第 N 段失败：读 progress.md 最后一个 done 段，从 N+1 继续。
"""

SEGMENTS_OK = "## 分段交付（大包必填）\n\n" + TABLE + RESUME

CASES = []


def case(name, expect_rc, text, must=None):
    CASES.append((name, expect_rc, text, must or []))


case("小包：不要求分段", 0, SMALL)
case("大包 scope 无分段表", 1, BIG_SCOPE, must=["分段交付"])
case("大包 scope + 合规分段表", 0, BIG_SCOPE + SEGMENTS_OK)
case("大包人日回退（约 2 人日）", 1, "# #1 估 2 人日\n\n## 要做\n1. x\n", must=["分段交付"])
case("人日多条取最大（0.1 + 2）", 1,
     "# #1\n约 0.1 人日（前置准备）\n后续约 2 人日\n", must=["分段交付"])
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
case("续跑 写 进度.md（正控）", 0,
     BIG_SCOPE + SEGMENTS_OK.replace("_tmp/20261105-issue-100/progress.md", "_tmp/20261105-issue-100/进度.md"))
case("续跑 中文目录（正控）", 0,
     BIG_SCOPE + SEGMENTS_OK.replace("_tmp/20261105-issue-100/progress.md",
                                     "_tmp/20261105-issue-100/评审记录/progress.md"))
case("续跑 路径后括号里写「不要」（正控）", 0,
     BIG_SCOPE + SEGMENTS_OK.replace("（每段一行）", "（不要提交进 git）"))
case("占位符 <待填>", 1, BIG_SCOPE + SEGMENTS_OK.replace("`src/interaction.rs`", "<待填>"), must=["占位符"])
case("占位符 超长中文尖括号", 1,
     BIG_SCOPE + SEGMENTS_OK.replace("`S1-RESULT.md` + `final.patch` S1",
                                     "<请在这里填写本段涉及的全部文件路径与行数范围>"),
     must=["占位符"])
case("占位符 花括号 {待补}", 1, BIG_SCOPE + SEGMENTS_OK.replace("`src/interaction.rs`", "{待补}"), must=["占位符"])
case("占位符 花括号 {file}", 1, BIG_SCOPE + SEGMENTS_OK.replace("`src/interaction.rs`", "{file}"), must=["占位符"])
case("占位符 单省略号 …", 1, BIG_SCOPE + SEGMENTS_OK.replace("`src/interaction.rs`", "…"), must=["占位符"])
case("占位符 ？？？", 1, BIG_SCOPE + SEGMENTS_OK.replace("`src/interaction.rs`", "？？？"), must=["占位符"])
case("Rust 泛型不是占位符（正控）", 0,
     BIG_SCOPE + SEGMENTS_OK.replace("`S1-RESULT.md` + `final.patch` S1",
                                     "`S1-RESULT.md`：新增 `Vec<InteractionEvent>` 序列化"))
case("autolink 里的 … 不是占位符（正控）", 0,
     BIG_SCOPE + SEGMENTS_OK.replace("`S1-RESULT.md` + `final.patch` S1",
                                     "`S1-RESULT.md`，见 <https://example.com/…>"))
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
case("表头写 (hrs)：裸数字按小时算", 1,
     BIG_SCOPE + SEGMENTS_OK.replace("| 预估时长 |", "| 预估时长(hrs) |").replace("| 80 min |", "| 2 |"),
     must=["120 min > 90 min"])
case("表头混排括号（h)：裸数字按小时算", 1,
     BIG_SCOPE + SEGMENTS_OK.replace("| 预估时长 |", "| 预估时长（h) |").replace("| 80 min |", "| 2 |"),
     must=["120 min > 90 min"])
case("表头「预估时长」里的「时」不算小时单位（正控）", 0, BIG_SCOPE + SEGMENTS_OK)
case("估时单位「天」不许当 min", 1,
     BIG_SCOPE + SEGMENTS_OK.replace("| 80 min |", "| 1 天 |"), must=["读不出"])
case("估时区间取最大值", 1,
     BIG_SCOPE + SEGMENTS_OK.replace("| 80 min |", "| 80–120 min |"), must=["120 min > 90 min"])
case("估时区间带说明「含联调」取最大（正控）", 0,
     BIG_SCOPE + SEGMENTS_OK.replace("| 80 min |", "| 60–80 min，含联调 |"))
case("估时 80/90 min 取最大（正控）", 0,
     BIG_SCOPE + SEGMENTS_OK.replace("| 80 min |", "| 80/90 min |"))
case("估时上限写法取最大", 1,
     BIG_SCOPE + SEGMENTS_OK.replace("| 80 min |", "| 60 min（上限 120 min） |"),
     must=["120 min > 90 min"])
case("估时加法求和（60 + 40）", 1,
     BIG_SCOPE + SEGMENTS_OK.replace("| 80 min |", "| 60 min + 40 min |"), must=["100 min > 90 min"])
case("估时连写求和（1 h 40 min）", 1,
     BIG_SCOPE + SEGMENTS_OK.replace("| 80 min |", "| 1 h 40 min |"), must=["100 min > 90 min"])
case("估时紧连写 1h40m", 1,
     BIG_SCOPE + SEGMENTS_OK.replace("| 80 min |", "| 1h40m |"), must=["100 min > 90 min"])
case("估时紧连写 1h45min", 1,
     BIG_SCOPE + SEGMENTS_OK.replace("| 80 min |", "| 1h45min |"), must=["105 min > 90 min"])
case("估时 1h30m = 90 min（正控）", 0,
     BIG_SCOPE + SEGMENTS_OK.replace("| 80 min |", "| 1h30m |"))
case("估时 1 h 30 min = 90 min（正控）", 0,
     BIG_SCOPE + SEGMENTS_OK.replace("| 80 min |", "| 1 h 30 min |"))
case("段名里的 S1 不参与估时", 0,
     BIG_SCOPE + SEGMENTS_OK.replace("| 80 min |", "| S3 分段 60 min |"))
case("示例表写在围栏里不算数", 1,
     BIG_SCOPE + "## 分段交付\n\n```\n" + TABLE + "```\n" + RESUME, must=["没有表格"])
case("围栏里的模板占位符不误伤（正控）", 0,
     BIG_SCOPE + SEGMENTS_OK + "\n```\n| S1 <段名> | `src/...` | <待填> | 80 min | — |\n```\n")
case("双 scope 行：小的在前、大的在后", 1,
     "<!-- pdrs-scope: files=3 lines=200 est_min=60 -->\n<!-- pdrs-scope: files=16 -->\n# x\n",
     must=["分段交付"])
case("scope 大小写/全角冒号/下划线都认", 1,
     "<!-- PDRS_SCOPE： files=16 -->\n# x\n", must=["分段交付"])
case("scope 全角连字符也认", 1,
     "<!-- pdrs－scope: files=16 -->\n# x\n", must=["分段交付"])
case("scope 行含 <…> 占位符", 2, "<!-- pdrs-scope: files=<待填> -->\n# x\n", must=["不是整数"])
case("scope 行没收到 -->", 2, "<!-- pdrs-scope: files=16\n# x\n", must=["没写进", "收到"])
case("scope 标记写在注释外", 2, "pdrs-scope: files=16 est_min=300\n# x\n", must=["没写进"])
case("scope 注释写法不对（前缀有别的词）", 2, "<!-- note pdrs-scope files=16 -->\n# x\n", must=["写法不对"])
case("scope 行写坏", 2, "<!-- pdrs-scope: files=很多 -->\n# x\n", must=["不是整数"])
case("scope 超长数字不崩溃", 2, "<!-- pdrs-scope: files=" + "9" * 40 + " -->\n# x\n", must=["不是整数"])
case("scope 行缺键", 2, "<!-- pdrs-scope: note=x -->\n# x\n", must=["至少要写"])
case("scope 声明未达阈值", 0,
     "<!-- pdrs-scope: files=3 lines=200 est_min=60 -->\n# x\n- `a.rs`\n- `b.rs`\n")
case("空文件不是「小包」", 2, "   \n\n", must=["空文件"])
case("纯 BOM 不是「小包」", 2, "﻿", must=["空文件"])
case("只含全角空格不是「小包」", 2, "　　\n　", must=["空文件"])
case("整份都是代码围栏不是「小包」", 2, "```\n# #1\n```\n", must=["围栏"])
case("明确加法优先于「最多」措辞（60 + 40 + 20）", 1,
     BIG_SCOPE + SEGMENTS_OK.replace("| 80 min |", "| 60 min + 40 min（最多再补 20 min） |"),
     must=["120 min > 90 min"])
case("区间里的「和」不是加法（正控）", 0,
     BIG_SCOPE + SEGMENTS_OK.replace("| 80 min |", "| 60–80 min（S1 和 S2 联调） |"))
case("估时 1h30（连写无分单位）= 90（正控）", 0,
     BIG_SCOPE + SEGMENTS_OK.replace("| 80 min |", "| 1h30 |"))
case("续跑 写 URL 里的 progress.md 不算", 1,
     BIG_SCOPE + SEGMENTS_OK.replace("_tmp/20261105-issue-100/progress.md",
                                     "https://example.com/2026/progress.md"),
     must=["进度文件路径"])
case("围栏里列 9 个 .rs 仍算大包", 1,
     "# #1\n```\n" + "\n".join(f"- src/m{i}.rs" for i in range(9)) + "\n```\n", must=["分段交付"])
case("人日数值有限但 ×480 溢出 → rc=2 读不出（不许 traceback）", 2,
     "# t\n\n约 1" + "0" * 306 + " 人日\n", must=["读不出"])
case("围栏开了没收口 → rc=2（不许把后面的 scope 整段吞掉）", 2,
     "# t\n```text\n忘写闭合\n\n<!-- pdrs-scope: files=12 lines=900 est_min=200 -->\n\n## 要做\n1. x\n",
     must=["围栏没有闭合"])
case("URL 里的 .rs 不计入回退路径数", 0,
     "# #1\n" + "\n".join(f"- https://example.com/m{i}/x.rs" for i in range(9)))
case("URL 后紧跟顿号枚举的 .rs 要计入", 1,
     "# #1\n- 见 https://example.com/a.md、" + "、".join(f"src/m{i}.rs" for i in range(8)), must=["分段交付"])
case("URL 后紧跟分号枚举的 .rs 要计入", 1,
     "# #1\n- 见 https://example.com/a.md;" + ";".join(f"src/m{i}.rs" for i in range(8)), must=["分段交付"])

case("围栏里的示例 scope 不许短路回退（9 个 .rs 仍判大包）", 1,
     "# #1\n```\n<!-- pdrs-scope: files=2 lines=80 est_min=30 -->\n```\n"
     + "\n".join(f"- src/m{i}.rs" for i in range(9)), must=["分段交付"])
case("围栏里的大值示例 scope 不把小包判成大包（正控）", 0,
     SMALL + "\n```\n<!-- pdrs-scope: files=16 lines=1124 est_min=180 -->\n```\n")
case("围栏里写坏的示例 scope 不再假 ERROR（正控）", 0,
     SMALL + "\n```\n<!-- pdrs-scope: files=<待填> -->\n```\n")
case("表头「预估工时」+ 裸数字按小时算", 1,
     BIG_SCOPE + SEGMENTS_OK.replace("| 预估时长 |", "| 预估工时 |").replace("| 80 min |", "| 2 |"),
     must=["120 min > 90 min"])
case("表头「预估人日」+ 裸数字不许当 min", 1,
     BIG_SCOPE + SEGMENTS_OK.replace("| 预估时长 |", "| 预估人日 |").replace("| 80 min |", "| 1 |"),
     must=["读不出"])
case("占位符 <...>（ASCII 三点）", 1,
     BIG_SCOPE + SEGMENTS_OK.replace("`src/interaction.rs`", "<...>"), must=["占位符"])
case("占位符 {...}（ASCII 三点）", 1,
     BIG_SCOPE + SEGMENTS_OK.replace("`src/interaction.rs`", "{...}"), must=["占位符"])
case("区间 + 明确加法：max(40,50)+20 = 70（正控）", 0,
     BIG_SCOPE + SEGMENTS_OK.replace("| 80 min |", "| 40–50 min + 20 min |"))
case("区间 + 明确加法超过上限：max(60,80)+20 = 100", 1,
     BIG_SCOPE + SEGMENTS_OK.replace("| 80 min |", "| 60–80 min + 20 min |"), must=["100 min > 90 min"])
case("裸「增加」不是加法：60–80 min（增加 20 min 联调）取 80（正控）", 0,
     BIG_SCOPE + SEGMENTS_OK.replace("| 80 min |", "| 60–80 min（增加 20 min 联调） |"))
case("续跑写「无需恢复」不算交代失败续跑", 1,
     BIG_SCOPE + SEGMENTS_OK.replace("第 N 段失败：读 progress.md 最后一个 done 段，从 N+1 继续。",
                                     "无需恢复，每段独立做完即可。"), must=["第 N 段"])
case("续跑写「不会失败」不算交代失败续跑", 1,
     BIG_SCOPE + SEGMENTS_OK.replace("第 N 段失败：读 progress.md 最后一个 done 段，从 N+1 继续。",
                                     "每段独立，不会失败。"), must=["第 N 段"])
case("续跑写无 scheme 的 URL 不算本地进度文件", 1,
     BIG_SCOPE + SEGMENTS_OK.replace("_tmp/20261105-issue-100/progress.md",
                                     "example.com/2026/progress.md"), must=["进度文件路径"])
case("人日回退认「工作量 N 人日」", 1, "# t\n\n工作量 3 人日\n", must=["分段交付"])
case("人日回退认「共计 N 人日」", 1, "# t\n\n共计 2 人日\n", must=["分段交付"])

case("人日回退容标点：「工作量：3 人日」", 1, "# t\n\n工作量：3 人日\n", must=["分段交付"])
case("人日回退认「估计 2 人日」", 1, "# t\n\n估计 2 人日\n", must=["分段交付"])
case("估时「90 分钟以内」= 90（正控）", 0,
     BIG_SCOPE + SEGMENTS_OK.replace("| 80 min |", "| 90 分钟以内 |"))
case("估时「120 min以内」按 120 判", 1,
     BIG_SCOPE + SEGMENTS_OK.replace("| 80 min |", "| 120 min以内 |"), must=["120 min > 90 min"])
case("列序换了（依赖在前）时空段名照样报", 1,
     BIG_SCOPE + """## 分段交付

| 段间依赖 | 段名 | 涉及文件 | 交付物 | 预估时长 |
|---------|------|---------|--------|---------|
| — |  | `src/a.rs` | `S1-RESULT.md` | 80 min |
| S1 | S2 b | `src/b.rs` | `S2-RESULT.md` | 60 min |

## 续跑

- 进度文件：`_tmp/20261105-issue-100/progress.md`
- 第 N 段失败：从 N+1 继续。
""", must=["为空"])


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

        # FIFO 冒充 task.md：不许 open 阻塞（挂起级），直接 rc 2
        fifo = Path(td) / "fifo.md"
        os.mkfifo(fifo)
        r = ctbs.check_file(fifo)
        ok = r is not None and r["rc"] == 2 and "不是普通文件" in r["problems"][0]
        print(f"{'PASS' if ok else 'FAIL'}  FIFO 不是普通文件 → rc=2 不阻塞：rc={r and r['rc']}")
        fails += 0 if ok else 1

        # --recent 扫描期间树被回收：跳过，不报 ERROR
        r = ctbs.check_file(Path(td) / "gone.md", ignore_missing=True)
        ok = r is None
        print(f"{'PASS' if ok else 'FAIL'}  --recent 下文件消失 → 跳过：{r}")
        fails += 0 if ok else 1

        # --recent 与显式参数混用：显式文件不存在必须 rc=2，不许被 ignore_missing 吞掉
        rc = ctbs.main(["x", "--recent", "24", "--root", td, "typo.md"])
        ok = rc == 2
        print(f"{'PASS' if ok else 'FAIL'}  --recent + 显式缺文件 → rc=2：rc={rc}")
        fails += 0 if ok else 1

        # 合法空扫描：没扫到任务书不算用法错误
        rc = ctbs.main(["x", "--recent", "24", "--root", td, "--quiet"])
        ok = rc == 0
        print(f"{'PASS' if ok else 'FAIL'}  --recent 空扫描 → rc=0：rc={rc}")
        fails += 0 if ok else 1

        # --recent 递归扫：修复轮的 fix-rN 下一层要扫得到；remote-run 派发副本不重复扫
        import time
        base = Path(td) / "_build" / "wt-9999" / "_tmp" / "20261105-issue-9999"
        deep = base / "fix-r1"
        deep.mkdir(parents=True)
        stage = base / "remote-run" / "stage"
        stage.mkdir(parents=True)
        for f in (deep / "task.md", stage / "task.md"):
            f.write_text("# t\n", encoding="utf-8")
            os.utime(f, (time.time() - 3600, time.time() - 3600))
        got = [str(x) for x in ctbs.find_recent(td, 24)]
        ok = len(got) == 1 and got[0].endswith("fix-r1/task.md")
        print(f"{'PASS' if ok else 'FAIL'}  --recent 扫到 fix-r1 深度且跳过 remote-run 副本：{got}")
        fails += 0 if ok else 1

        # 空扫描 + --json：按行解析 JSON 的调用方不能收到文本 SUMMARY
        import io
        from contextlib import redirect_stderr, redirect_stdout
        empty = Path(td) / "emptyroot"
        empty.mkdir()
        buf = io.StringIO()
        with redirect_stdout(buf), redirect_stderr(io.StringIO()):
            rc = ctbs.main(["x", "--recent", "24", "--root", str(empty), "--json"])
        ok = rc == 0 and buf.getvalue().strip() == "[]"
        print(f"{'PASS' if ok else 'FAIL'}  --recent 空扫描 --json 打 []：rc={rc} 输出={buf.getvalue()!r}")
        fails += 0 if ok else 1

        # _is_dispatch_copy 只看 _tmp 之后：--root 前缀里叫 remote-run 也不能把全部任务书跳掉
        ok = (ctbs._is_dispatch_copy(Path("/srv/remote-run/repo/_build/wt-1/_tmp/x/task.md")) is False
              and ctbs._is_dispatch_copy(Path("/srv/_build/wt-1/_tmp/x/remote-run/stage/task.md")) is True
              and ctbs._is_dispatch_copy(Path("/srv/_build/wt-1/_tmp/x/.prev-1/task.md")) is True
              and ctbs._is_dispatch_copy(Path("/srv/_tmp/remote-run/repo/_build/wt-1/_tmp/x/task.md")) is False)
        print(f"{'PASS' if ok else 'FAIL'}  _is_dispatch_copy 只看 _tmp 之后的路径段")
        fails += 0 if ok else 1

        # 非空扫描的 --json 必须能被 json.loads；--explain 不许再打文本 SUMMARY
        real = Path(td) / "real-small.md"
        real.write_text(SMALL, encoding="utf-8")
        buf = io.StringIO()
        with redirect_stdout(buf), redirect_stderr(io.StringIO()):
            rc = ctbs.main(["x", str(real), "--json"])
        try:
            got = json.loads(buf.getvalue())
            ok = rc == 0 and isinstance(got, list) and len(got) == 1 and got[0]["rc"] == 0
        except Exception:
            ok = False
        print(f"{'PASS' if ok else 'FAIL'}  非空 --json 可 json.loads：rc={rc} 输出={buf.getvalue()[:60]!r}")
        fails += 0 if ok else 1
        buf = io.StringIO()
        with redirect_stdout(buf), redirect_stderr(io.StringIO()):
            rc = ctbs.main(["x", str(real), "--explain"])
        ok = rc == 0 and "SUMMARY" not in buf.getvalue() and "OK" in buf.getvalue()
        print(f"{'PASS' if ok else 'FAIL'}  --explain 不打 SUMMARY：rc={rc} 输出={buf.getvalue()[:60]!r}")
        fails += 0 if ok else 1

        # mtime 在未来的文件也要跳过（扫描开始后才创建的任务书 mtime > now）
        fut = Path(td) / "_build" / "wt-9997" / "_tmp" / "issue-9997" / "task.md"
        fut.parent.mkdir(parents=True)
        fut.write_text("# t\n", encoding="utf-8")
        os.utime(fut, (time.time() + 60, time.time() + 60))
        got = [str(x) for x in ctbs.find_recent(td, 24)]
        ok = not any(x.endswith("issue-9997/task.md") for x in got)
        print(f"{'PASS' if ok else 'FAIL'}  未来 mtime 的文件被 FRESH_SEC 跳过：{got}")
        fails += 0 if ok else 1

        # find_recent 自己的 OSError → ScanError（不是 return 空列表）
        class _BoomPath:
            def __init__(self, root):
                self.root = root

            def glob(self, pat):
                raise OSError("模拟：权限拒绝")

        real_path = ctbs.Path
        ctbs.Path = _BoomPath
        try:
            try:
                ctbs.find_recent(td, 24)
                ok = False
            except ctbs.ScanError:
                ok = True
        finally:
            ctbs.Path = real_path
        print(f"{'PASS' if ok else 'FAIL'}  find_recent 遇 OSError 抛 ScanError")
        fails += 0 if ok else 1

        # 扫描中断（glob 抛 OSError）必须 rc=2，不许与「没扫到」同观感
        real_find = ctbs.find_recent
        def boom(root, hours):
            raise ctbs.ScanError("模拟：权限拒绝")
        ctbs.find_recent = boom
        err = io.StringIO()
        try:
            with redirect_stdout(io.StringIO()), redirect_stderr(err):
                rc = ctbs.main(["x", "--recent", "24", "--root", td])
        finally:
            ctbs.find_recent = real_find
        ok = rc == 2 and "扫描中断" in err.getvalue()
        print(f"{'PASS' if ok else 'FAIL'}  扫描中断 → rc=2：rc={rc} stderr={err.getvalue().strip()[:40]!r}")
        fails += 0 if ok else 1

    # 命令行契约
    for name, argv, expect in (
            ("--print-skeleton 带文件参数", ["x", "--print-skeleton", "task.md"], 3),
            ("--print-skeleton 带 --recent", ["x", "--print-skeleton", "--recent", "24"], 3),
            ("未知参数不许静默吞掉", ["x", "--jso", "task.md"], 3),
            ("--recent 缺小时数", ["x", "--recent"], 3),
            ("--recent nan 是用法错误", ["x", "--recent", "nan", "task.md"], 3),
            ("--recent 0 是用法错误", ["x", "--recent", "0", "task.md"], 3),
            ("--recent 负数 是用法错误", ["x", "--recent", "-1", "task.md"], 3),
            ("--recent --root 不是目录是用法错误", ["x", "--recent", "24", "--root", "/nonexistent-pdrs-xyz"], 3),
            ("--root 不带 --recent 是用法错误", ["x", "--root", ".", "task.md"], 3),
    ):
        rc = ctbs.main(argv)
        ok = rc == expect
        print(f"{'PASS' if ok else 'FAIL'}  {name}：rc={rc}（期望 {expect}）")
        fails += 0 if ok else 1

    total = len(CASES) + 22   # 13 条独立检查（非 UTF-8 / FIFO / 树被回收 / --recent+显式缺文件 / 空扫描 / 递归扫 / 空扫描 --json / 派发副本判定 / 非空 --json / --explain / 未来 mtime / 扫描中断异常 / 扫描中断 rc）+ 9 条命令行契约
    print(f"SUMMARY cases={total} failed={fails}")
    return 1 if fails else 0


if __name__ == "__main__":
    sys.exit(run())
