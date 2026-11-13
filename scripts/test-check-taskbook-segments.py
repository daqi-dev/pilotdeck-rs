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
import shutil
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
# 修 3②（MM2 sibling#2）：`\.rs` 右边界是 ASCII 环视——中文紧贴（「x.rs是本包要改的」）仍要数到
# 该路径（\b 两侧都是 \w 会漏计 → 卡阈值 8 时大包被判小包，假 PASS）；.rsx 仍不算
case("大包 rs 路径回退 CJK 紧贴（9 个）", 1,
     "# #1\n" + "\n".join(f"- src/m{i}/x.rs是本包要改的" for i in range(9)), must=["分段交付"])
case("rs 路径回退：.rsx 不算（9 个，正控）", 0,
     "# #1\n" + "\n".join(f"- src/m{i}/x.rsx" for i in range(9)))
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
# GFM 允许每一行省略行首竖线；丢掉这些行 = 该行的估时 / 占位符 / 列数不再校验 → 假 PASS（DS gate16 P1#2）
_NO_PIPE_S2 = "S2 HTTP 同域 | `src/native_http.rs` | `S2-RESULT.md` + `final.patch` S2 | 120 min | S1"
case("省略行首竖线的数据行仍校验估时（假 PASS）", 1,
     BIG_SCOPE + SEGMENTS_OK.replace(TABLE.splitlines()[2], _NO_PIPE_S2), must=["120 min > 90 min"])
case("省略行首竖线的数据行仍查占位符", 1,
     BIG_SCOPE + SEGMENTS_OK.replace(TABLE.splitlines()[2], _NO_PIPE_S2.replace("`src/native_http.rs`", "<待填>")),
     must=["占位符"])
case("整张表都省行首竖线（正控）", 0,
     BIG_SCOPE + SEGMENTS_OK.replace(TABLE, "\n".join(
         ln.strip().strip("|").strip() for ln in TABLE.splitlines())))
case("表头也省行首竖线时仍校验估时（假 PASS）", 1,
     BIG_SCOPE + SEGMENTS_OK.replace("| 80 min |", "| 120 min |").replace(TABLE, "\n".join(
         ln.strip().strip("|").strip() for ln in TABLE.splitlines())), must=["120 min > 90 min"])
# 表头省行首竖线、数据行保留竖线的混写（gate17 GLM boundary P1）：主扫按「第一条带首竖线的行」
# 会把数据行当表头 → 合规表缺全部列 + 只有 1 段 → 假 FAIL（拒派合规任务书）
_MIXED = TABLE.splitlines()
_MIXED[0] = _MIXED[0].strip().strip("|").strip()          # 只有表头省行首竖线
MIXED_HEADER_TABLE = "\n".join(_MIXED) + "\n"
case("表头省行首竖线、数据行保留（正控）", 0,
     BIG_SCOPE + "## 分段交付\n\n" + MIXED_HEADER_TABLE + RESUME)
case("表头省行首竖线、数据行保留时仍校验估时", 1,
     BIG_SCOPE + "## 分段交付\n\n" + MIXED_HEADER_TABLE.replace("| 80 min |", "| 120 min |") + RESUME,
     must=["120 min > 90 min"])
case("锚到的表头绑不出五列时退回带首竖线的真表头（正控）", 0,
     BIG_SCOPE + """## 分段交付

段 | 涉及文件
---|---
| 段 | 涉及文件 | 交付物 | 预估时长 | 段间依赖 |
|---|---|---|---|---|
| S1 | `src/a.rs` | `S1-RESULT.md` | 60 min | — |
| S2 | `src/b.rs` | `S2-RESULT.md` | 60 min | S1 |
""" + RESUME)
case("表头省行首竖线、分隔行保留竖线（正控）", 0,
     BIG_SCOPE + "## 分段交付\n\n" + "\n".join([_MIXED[0], "|" + _MIXED[1]] + _MIXED[2:]) + "\n" + RESUME)
case("省略行首竖线但列数不符 → 报读不出，不静默丢", 1,
     BIG_SCOPE + SEGMENTS_OK.replace(TABLE.splitlines()[2],
                                     "S2 HTTP 同域 | `src/native_http.rs` | 120 min | S1"),
     must=["读不出 5 列"])
case("表后空行之后的正文不算表行（正控）", 0,
     BIG_SCOPE + "## 分段交付\n\n" + TABLE + "\n每段收工即可交付，段间不共写同一文件。\n" + RESUME)
case("表后正文含竖线也不当表行（正控）", 0,
     BIG_SCOPE + "## 分段交付\n\n" + TABLE + "\n口径：估时按 min | 小时换算，超 90 min 再切。\n" + RESUME)
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
case("scope 行只有未知键", 2, "<!-- pdrs-scope: note=x -->\n# x\n", must=["认不出的键"])
case("scope 行只有 modules（没阈值键）", 2, "<!-- pdrs-scope: modules=2 -->\n# x\n", must=["至少要写"])
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


# ---- fix-r8：第七轮闸门（DeepSeek boundary P0×2 / P1×1，GLM concurrency P1×1）--------------------
case("scope 键拼错（estmins）算写坏", 2,
     "<!-- pdrs-scope: files=2 estmins=300 -->\n# x\n", must=["认不出的键"])
case("scope 键大写 EST_MIN 照认（不许吞阈值）", 1,
     "<!-- pdrs-scope: files=2 EST_MIN=300 -->\n# x\n", must=["est_min=300"])
case("scope 键 est-min 连字符照认", 1,
     "<!-- pdrs-scope: files=2 est-min=300 -->\n# x\n", must=["est_min=300"])
case("占位符 全角尖括号 ＜待填＞", 1,
     BIG_SCOPE + SEGMENTS_OK.replace("`src/interaction.rs`", "＜待填＞"), must=["占位符"])
case("占位符 中文方括号【待填】", 1,
     BIG_SCOPE + SEGMENTS_OK.replace("`src/interaction.rs`", "【待填】"), must=["占位符"])
case("占位符 裸 待填", 1,
     BIG_SCOPE + SEGMENTS_OK.replace("`src/interaction.rs`", "待填"), must=["占位符"])
case("【重要】这类正文不算占位符（正控）", 0,
     BIG_SCOPE + SEGMENTS_OK.replace("`S1-RESULT.md` + `final.patch` S1", "`S1-RESULT.md`（【重要】先跑 fmt）"))
# 骨架 `_tmp/<日期>-issue-<N>/progress.md` 去掉尖括号后既无 `{}<>…` 也有 `/` → 旧规则判成实例路径
# rc 0 假 PASS（gate17 GLM boundary P0）
case("续跑 裸 N 段 _tmp/…-issue-N（假 PASS）", 1,
     BIG_SCOPE + SEGMENTS_OK.replace("_tmp/20261105-issue-100/progress.md", "_tmp/20261106-issue-N/progress.md"),
     must=["路径段 'N' 是没替换的占位"])
case("续跑 裸 N 独占一段 _tmp/N/", 1,
     BIG_SCOPE + SEGMENTS_OK.replace("_tmp/20261105-issue-100/progress.md", "_tmp/N/progress.md"),
     must=["路径段 'N' 是没替换的占位"])
case("续跑 N- 前缀段 N-issue/", 1,
     BIG_SCOPE + SEGMENTS_OK.replace("_tmp/20261105-issue-100/progress.md", "_tmp/N-issue/progress.md"),
     must=["路径段 'N' 是没替换的占位"])
case("续跑 目录名含 n 但不是单字母段（正控）", 0,
     BIG_SCOPE + SEGMENTS_OK.replace("_tmp/20261105-issue-100/progress.md",
                                     "_tmp/20261105-issue-1316/native/progress.md"))
case("续跑 全角占位 _tmp/＜日期＞-issue-＜N＞", 1,
     BIG_SCOPE + SEGMENTS_OK.replace("_tmp/20261105-issue-100/progress.md",
                                     "_tmp/＜日期＞-issue-＜N＞/progress.md"),
     must=["进度文件路径"])
case("估时 90 min（含 10 min 缓冲）= 90（正控）", 0,
     BIG_SCOPE + SEGMENTS_OK.replace("| 80 min |", "| 90 min（含 10 min 缓冲） |"))
case("估时 100 min（含 10 min 缓冲）= 100", 1,
     BIG_SCOPE + SEGMENTS_OK.replace("| 80 min |", "| 100 min（含 10 min 缓冲） |"), must=["100 min > 90 min"])
case("估时 80 min（2 个文件）= 80（正控）", 0,
     BIG_SCOPE + SEGMENTS_OK.replace("| 80 min |", "| 80 min（2 个文件） |"))
case("估时 60 min（S1 和 S2 各 30 min）= 60（正控）", 0,
     BIG_SCOPE + SEGMENTS_OK.replace("| 80 min |", "| 60 min（S1 和 S2 各 30 min） |"))
case("估时 加法仍求和：40 min + 60 min（含 10 min 缓冲）= 100", 1,
     BIG_SCOPE + SEGMENTS_OK.replace("| 80 min |", "| 40 min + 60 min（含 10 min 缓冲） |"),
     must=["100 min > 90 min"])


# ---- fix-r9：第八轮闸门（GLM boundary P0×1 引用/缩进示例 scope；P2×1 【策略】误判）----------
NINE_RS = "\n".join(f"- src/m{i}.rs" for i in range(1, 10))
case("引用块里的示例 scope 不算声明", 1,
     "# t\n\n> <!-- pdrs-scope: files=2 lines=80 est_min=30 -->\n\n" + NINE_RS + "\n",
     must=["分段交付"])
case("缩进代码块里的示例 scope 不算声明", 1,
     "# t\n\n    <!-- pdrs-scope: files=2 lines=80 est_min=30 -->\n\n" + NINE_RS + "\n",
     must=["分段交付"])
case("引用块里的写坏 scope 也不假 ERROR（回退照走）", 1,
     "# t\n\n> <!-- pdrs-scope: files=很多 -->\n\n" + NINE_RS + "\n", must=["分段交付"])
case("【策略】不是占位符（正控）", 0,
     BIG_SCOPE + SEGMENTS_OK.replace("`S1-RESULT.md` + `final.patch` S1", "`S1-RESULT.md`（见【策略】附件）"))
case("【等待】不是占位符（正控）", 0,
     BIG_SCOPE + SEGMENTS_OK.replace("`S1-RESULT.md` + `final.patch` S1", "`S1-RESULT.md`（【等待】上游合入）"))
case("【待补充】仍是占位符", 1,
     BIG_SCOPE + SEGMENTS_OK.replace("`S1-RESULT.md` + `final.patch` S1", "【待补充】"), must=["占位符"])


# ---- fix-r10：第九轮闸门（两家 P0×2 / P1×4 / P2×5，全部先实测复现）-----------------------------
TEN_RS = "\n".join(f"- src/m{i}.rs" for i in range(1, 10))
case("行内代码里的示例 scope 不算声明（两家同报 P0）", 1,
     "# t\n\n写法示例：`<!-- pdrs-scope: files=2 lines=80 est_min=30 -->`\n\n" + TEN_RS + "\n",
     must=["分段交付"])
case("行内代码里的写坏 scope 也不假 ERROR（回退照走）", 1,
     "# t\n\n写法：`<!-- pdrs-scope: files=很多 -->`\n\n" + TEN_RS + "\n", must=["分段交付"])
case("续跑路径写在反引号里仍认（行内代码只对 scope 屏蔽）", 0,
     BIG_SCOPE + SEGMENTS_OK.replace("| 80 min |", "| 40 min |"))
case("紧连写 1h100min = 160 min（旧实现只读 60，P0）", 1,
     BIG_SCOPE + SEGMENTS_OK.replace("| 80 min |", "| 1h100min |"), must=["160 min > 90 min"])
case("紧连写 1h130m = 190 min", 1,
     BIG_SCOPE + SEGMENTS_OK.replace("| 80 min |", "| 1h130m |"), must=["190 min > 90 min"])
case("紧连写 1h40m = 100 min", 1,
     BIG_SCOPE + SEGMENTS_OK.replace("| 80 min |", "| 1h40m |"), must=["100 min > 90 min"])
case("超长花括号占位符（>80 字）仍判占位（P0）", 1,
     BIG_SCOPE + SEGMENTS_OK.replace("`S1-RESULT.md` + `final.patch` S1",
                                     "{" + "请填写本段全部文件、改动行号与验收命令以及相关说明" * 4 + "}"),
     must=["占位符"])
case("超长 autolink 里的 … 不算占位符（正控）", 0,
     BIG_SCOPE + SEGMENTS_OK.replace("`S1-RESULT.md` + `final.patch` S1",
                                     "`S1-RESULT.md`，见 <https://example.com/" + "a" * 240 + "...>"))
case("表头 预估人天 + 裸数字不许当 min（P1）", 1,
     BIG_SCOPE + SEGMENTS_OK.replace("| 预估时长 |", "| 预估人天 |").replace("| 80 min |", "| 2 |"),
     must=["读不出"])
case("表头 预估人时 + 裸数字按小时（P1）", 1,
     BIG_SCOPE + SEGMENTS_OK.replace("| 预估时长 |", "| 预估人时 |").replace("| 80 min |", "| 2 |"),
     must=["120 min > 90 min"])
case("区间 60 至 80 分钟 = 80（正控，P1 假 FAIL）", 0,
     BIG_SCOPE + SEGMENTS_OK.replace("| 80 min |", "| 60 至 80 分钟 |"))
case("区间 60 到 100 分钟 = 100（P1）", 1,
     BIG_SCOPE + SEGMENTS_OK.replace("| 80 min |", "| 60 到 100 分钟 |"), must=["100 min > 90 min"])
case("括注「含」距数字 >4 字符仍不计入（P1 假 FAIL）", 0,
     BIG_SCOPE + SEGMENTS_OK.replace("| 80 min |", "| 90 min（含双段联调 10 min） |"))
case("括注里的 + 是列表不是加法（P1 假 FAIL）", 0,
     BIG_SCOPE + SEGMENTS_OK.replace("| 80 min |", "| 80 min（S1 + S2 各 40 min） |"))
case("重复 scope 键后写覆盖前者（P2 假 PASS）", 2,
     "<!-- pdrs-scope: files=16 est_min=180 files=2 -->\n# x\n", must=["写了两次"])
case("正文里的 …… 不是表格占位符（P2 假 FAIL）", 0,
     BIG_SCOPE + SEGMENTS_OK.replace("## 分段交付（大包必填）\n\n",
                                     "## 分段交付（大包必填）\n\n先做 A 再做 B……\n\n"))
case("列表续行里缩进 4 格的进度路径仍认（P2 假 FAIL）", 0,
     BIG_SCOPE + """## 分段交付

| 段 | 涉及文件 | 交付物 | 预估时长 | 段间依赖 |
|----|---------|--------|---------|---------|
| S1 | `src/a.rs` | `S1-RESULT.md` | 40 min | — |
| S2 | `src/b.rs` | `S2-RESULT.md` | 40 min | S1 |

## 续跑

1. 进度文件：
    `_tmp/20261105-issue-100/progress.md`
2. 第 1 段失败从第 2 段继续。
""")
case("全 - 数据行不许当分隔行跳过（P2 假 PASS）", 1,
     BIG_SCOPE + SEGMENTS_OK.replace("| S2 HTTP 同域 |", "| - | - | - | - | - |\n| S2 HTTP 同域 |"),
     must=["读不出"])
case("关闭围栏带尾随文本 = 未闭合（P2 假 PASS）", 2,
     "# t\n\n```\n``` x\n<!-- pdrs-scope: files=2 lines=80 est_min=30 -->\n\n" + TEN_RS + "\n",
     must=["围栏"])

# ---- fix-r11 第二批：第十轮闸门（GLM boundary P0×2 P1×1 P2×1 / DeepSeek boundary P0×1 P2×2）-----
case("CRLF 任务书的围栏照常闭合（P0 假 PASS）", 1,
     "# t\r\n\r\n```\r\n<!-- pdrs-scope: files=2 lines=80 est_min=30 -->\r\n```\r\n\r\n"
     "<!-- pdrs-scope: files=16 est_min=180 -->\r\n", must=["分段交付"])
case("闭合围栏长于开栏也算闭合（P0 假 ERROR）", 0,
     "# t\n\n```\n<!-- pdrs-scope: files=16 -->\n````\n\n正文。\n")
case("双反引号行内代码里的示例 scope 不算声明（P0 假 PASS）", 1,
     "# t\n\n写法：``<!-- pdrs-scope: files=2 lines=80 est_min=30 -->``\n\n" + TEN_RS + "\n",
     must=["分段交付"])
case("列表续行缩进 4 格的裸示例 scope 不算声明（P1 假 PASS）", 1,
     "# t\n\n- scope 写法示例：\n    <!-- pdrs-scope: files=2 lines=80 est_min=30 -->\n\n"
     + TEN_RS + "\n", must=["分段交付"])
case("人日带量词「个」（P2 假 PASS）", 1,
     "# #1\n约 2 个人日\n\n## 要做\n1. x\n", must=["分段交付"])
case("空花括号 `format!(\"{}\", x)` 不是占位符（P2 假 FAIL）", 0,
     BIG_SCOPE + SEGMENTS_OK.replace("`S1-RESULT.md` + `final.patch` S1",
                                     "`S1-RESULT.md`，改 `format!(\"{}\", x)` 调用"))
case("整格就是 {} 仍算占位符（正控）", 1,
     BIG_SCOPE + SEGMENTS_OK.replace("`S1-RESULT.md` + `final.patch` S1", "{}"),
     must=["占位符"])

# ---- fix-r11 第四批（GLM 第十一轮 concurrency P0×2、sibling P2 / P3，另修 patch14 引入的回归）----
# patch14 重写 _blank_indented 时丢了「空行之后 in_block = True」：中段的缩进代码块不再抹掉，
# 里面的示例 scope 被当成真声明把大包短路成小包（实测修前 rc 0，假 PASS）。
case("中段缩进代码块里的示例 scope 不算声明（回归，P0 假 PASS）", 1,
     "# t\n\n正文一行。\n\n    <!-- pdrs-scope: files=2 lines=80 est_min=30 -->\n\n"
     + TEN_RS + "\n", must=["分段交付"])
# 续跑路径不在反引号里时，只有 PATH_TOKEN 静音那一条能挡住路径里的 resume（旧的负控只测到反引号那条）
# 只有 _blank_indented 拦得住：中段缩进块里的示例分段行若没被抹掉，会被当成本包的表行（120 min > 90）
case("中段缩进块里的示例分段行不算数（回归，P0 假 PASS）", 0,
     BIG_SCOPE + SEGMENTS_OK.replace("""## 续跑""", """写法示例（上一单）：

    | 段 | 涉及文件 | 交付物 | 预估时长 | 段间依赖 |
    |----|---------|--------|---------|---------|
    | S1 | `src/x.rs` | `X.md` | 120 min | — |

## 续跑"""))
case("裸进度路径里带 resume 也不算失败续跑说明（P0 假 PASS）", 1,
     BIG_SCOPE + SEGMENTS_OK.replace("`_tmp/20261105-issue-100/progress.md`",
                                     "_tmp/20261105-issue-100/resume-r1/progress.md")
     .replace("- 第 N 段失败：读 progress.md 最后一个 done 段，从 N+1 继续。\n", ""),
     must=["第 N 段失败"])
case("两段共写同一文件必须报（sibling P2 漏判）", 1,
     BIG_SCOPE + SEGMENTS_OK.replace("`src/native_http.rs`", "`src/interaction.rs`"),
     must=["共写同一文件"])

# ---- fix-r11 第三批（DeepSeek 第十一轮 boundary P0×2 P2×5 P3×2，全部先复现原始 rc）----------
case("千分位 1,000 min 不许读成 1 min（P0 假 PASS）", 1,
     BIG_SCOPE + SEGMENTS_OK.replace("| 80 min |", "| 1,000 min |"), must=["1000 min > 90 min"])
case("全角千分位 1，000 min 同样（P0）", 1,
     BIG_SCOPE + SEGMENTS_OK.replace("| 80 min |", "| 1，000 min |"), must=["1000 min > 90 min"])
case("进度路径里带 resume 不算「失败从哪继续」（P0 假 PASS）", 1,
     BIG_SCOPE + SEGMENTS_OK.replace("_tmp/20261105-issue-100/progress.md",
                                     "_tmp/20261105-issue-100/resume-r1/progress.md")
     .replace("- 第 N 段失败：读 progress.md 最后一个 done 段，从 N+1 继续。\n", ""),
     must=["第 N 段失败"])
case("松散列表：空行 + 4 格缩进的进度路径仍是正文（P2 假 FAIL）", 0,
     BIG_SCOPE + SEGMENTS_OK.replace("- 进度文件：`_tmp/20261105-issue-100/progress.md`（每段一行）",
                                     "- 进度文件：\n\n    `_tmp/20261105-issue-100/progress.md`"))
case("Rust 内联命名参数 format!(\"{x}\") 不是占位符（P2 假 FAIL）", 0,
     BIG_SCOPE + SEGMENTS_OK.replace("`S1-RESULT.md` + `final.patch` S1",
                                     "`S1-RESULT.md`，改 `format!(\"{x}\")`"))
case("分段文件 + 段 的表头按整格绑列（P2 假 FAIL）", 0,
     BIG_SCOPE + """## 分段交付

| 分段文件 | 段 | 交付物 | 预估时长 | 段间依赖 |
|---------|----|--------|---------|---------|
| `src/a.rs` | S1 | `S1-RESULT.md` | 40 min | — |
| `src/b.rs` | S2 | `S2-RESULT.md` | 40 min | S1 |

## 续跑

- 进度文件：`_tmp/20261105-issue-100/progress.md`（每段一行）
- 第 N 段失败：读 progress.md 最后一个 done 段，从 N+1 继续。
""")

# ---- fix-r12（gate12：GLM boundary P1×2 P2×1 + DeepSeek boundary P0×1 P1×1 P2×1，全部先复现原始 rc）----
case("小数小时连写 1.5h30m = 120 min（P0 假 PASS）", 1,
     BIG_SCOPE + SEGMENTS_OK.replace("| 80 min |", "| 1.5h30m |"), must=["120 min > 90 min"])
case("小数小时连写 1.5h40m = 130 min（P0）", 1,
     BIG_SCOPE + SEGMENTS_OK.replace("| 80 min |", "| 1.5h40m |"), must=["130 min > 90 min"])
case("小数小时连写 1.5h100min = 190 min（P0）", 1,
     BIG_SCOPE + SEGMENTS_OK.replace("| 80 min |", "| 1.5h100min |"), must=["190 min > 90 min"])
case("括注「不含」是排除不是包含：90 + 30 = 120 min（P0 假 PASS）", 1,
     BIG_SCOPE + SEGMENTS_OK.replace("| 80 min |", "| 90 min（不含 30 min 测试） |"),
     must=["120 min > 90 min"])
case("括注「未含」同样计入", 1,
     BIG_SCOPE + SEGMENTS_OK.replace("| 80 min |", "| 60 min（未含 40 min 联调） |"),
     must=["100 min > 90 min"])
case("括注「含」仍是包含：90 min 不变", 0,
     BIG_SCOPE + SEGMENTS_OK.replace("| 80 min |", "| 90 min（含 30 min 测试） |"))
case("万位分组 1,0000 min 不许读成 1 min（P1 假 PASS）", 1,
     BIG_SCOPE + SEGMENTS_OK.replace("| 80 min |", "| 1,0000 min |"), must=["10000 min > 90 min"])
case("全角万位分组 1，0000 min 同样（P1）", 1,
     BIG_SCOPE + SEGMENTS_OK.replace("| 80 min |", "| 1，0000 min |"), must=["10000 min > 90 min"])
case("引号裹占位词 \"{name}\" 仍是占位符（P2 假 PASS）", 1,
     BIG_SCOPE + SEGMENTS_OK.replace("`src/interaction.rs`", "按 \"{name}\" 填"), must=["占位符"])
case("引号裹占位词 \"{file}\" 仍是占位符（P2 假 PASS）", 1,
     BIG_SCOPE + SEGMENTS_OK.replace("`src/interaction.rs`", "按 \"{file}\" 填"), must=["占位符"])
case("引号裹占位词 \"{N}\" 仍是占位符（P2 假 PASS）", 1,
     BIG_SCOPE + SEGMENTS_OK.replace("`src/interaction.rs`", "按 \"{N}\" 填"), must=["占位符"])
case("字符串前缀 f\"{name}\" 仍放行（第十轮豁免别被收紧掉）", 0,
     BIG_SCOPE + SEGMENTS_OK.replace("`src/interaction.rs`", "`f\"{name}\"`"))
case("字符串前缀 rb\"{path}\" 同样放行", 0,
     BIG_SCOPE + SEGMENTS_OK.replace("`src/interaction.rs`", "`rb\"{path}\"`"))
case("表达式上下文里的命名参数 format!(\"{idx}\") 仍放行（别矫枉过正）", 0,
     BIG_SCOPE + SEGMENTS_OK.replace("`S1-RESULT.md` + `final.patch` S1",
                                     "`S1-RESULT.md`，改 `format!(\"{idx}\")`"))
case("Cargo.lock 两段共写也要报（P2 假 PASS）", 1,
     BIG_SCOPE + SEGMENTS_OK.replace("`src/interaction.rs`", "`Cargo.lock`")
     .replace("`src/native_http.rs`", "`Cargo.lock`"), must=["共写同一文件"])
case(".jsonl fixture 两段共写也要报（gate16 sibling P2）", 1,
     BIG_SCOPE + SEGMENTS_OK.replace("`src/interaction.rs`", "`src/fixtures/events.jsonl`")
     .replace("`src/native_http.rs`", "`src/fixtures/events.jsonl`"), must=["共写同一文件"])
# gate18 GLM boundary P0：`strip("|")` 把 `|| a | b | …` 的粘写双竖线一并剥掉，GFM 空首格塌缩、
# 整行左移一位 → 6 格行与 5 列表头对齐，非合规任务书 rc 0
case("行首双竖线的空段名不许塌缩（假 PASS）", 1,
     BIG_SCOPE + SEGMENTS_OK.replace(
         "| S2 HTTP 同域 | `src/native_http.rs` | `S2-RESULT.md` + `final.patch` S2 | 60 min | S1 |",
         "|| S2 HTTP 同域 | `src/native_http.rs` | `S2-RESULT.md` + `final.patch` S2 | 60 min | S1 |"),
     # GFM 里 `||` 就是「空首格 + 5 格」= 6 格：按列数不符报出来（整行不校验），
     # 修前 strip("|") 把它塌成 5 格、段名读成次格 → 假 PASS（gate18 GLM P0）。
     must=["字段数 6"])
# gate18 MiniMax boundary：`FILE_TOKEN` 交替最左优先，`events.jsonl` 被 `js` 吃成 `events.js`
# → `.json` 与 `.jsonl` 两个不同文件撞键，合规任务书被判共写（假 FAIL，文件名也是错的）
case("四处扩展名最左优先：.json 与 .jsonl 分属两段不算共写（正控）", 0,
     BIG_SCOPE + SEGMENTS_OK.replace("`src/interaction.rs`", "`src/fixtures/events.json`")
     .replace("`src/native_http.rs`", "`src/fixtures/events.jsonl`"))
case("同一 .jsonl 两段共写的报错要写全扩展名（不是 events.js）", 1,
     BIG_SCOPE + SEGMENTS_OK.replace("`src/interaction.rs`", "`src/fixtures/events.jsonl`")
     .replace("`src/native_http.rs`", "`src/fixtures/events.jsonl`"),
     must=["events.jsonl（段间不共写"])
case(".ts 与 .tsx 分属两段不算共写（正控）", 0,
     BIG_SCOPE + SEGMENTS_OK.replace("`src/interaction.rs`", "`src/a.ts`")
     .replace("`src/native_http.rs`", "`src/a.tsx`"))
# gate18 MiniMax boundary P1：共写键不归一（`SRC/A.rs` vs `src/a.rs` 是同一个文件）→ 规则被绕过
case("同一文件大小写不同写法也算共写（假 PASS）", 1,
     BIG_SCOPE + SEGMENTS_OK.replace("`src/interaction.rs`", "`src/a.rs`")
     .replace("`src/native_http.rs`", "`SRC/A.rs`"), must=["共写同一文件"])
case("x/../x/a.rs 与 x/a.rs 归一到同一键（假 PASS）", 1,
     BIG_SCOPE + SEGMENTS_OK.replace("`src/interaction.rs`", "`x/a.rs`")
     .replace("`src/native_http.rs`", "`x/../x/a.rs`"), must=["共写同一文件"])
case("不同文件不算共写（正控：src/a.rs 与 src/b.rs）", 0,
     BIG_SCOPE + SEGMENTS_OK.replace("`src/interaction.rs`", "`src/a.rs`"))

# ---- fix-r13：gate14（DeepSeek boundary P0×1 + GLM boundary P1×1）---------------------------------
# 区间两侧各自连写时，`max(所有 token)` 只取到 60：105 min 的段被当成 60 → rc 0 假 PASS
case("区间两侧各自连写：1h30m–1h45m = max(90,105) = 105（P0 假 PASS）", 1,
     BIG_SCOPE + SEGMENTS_OK.replace("| 80 min |", "| 1h30m–1h45m |"), must=["105 min > 90 min"])
case("小数小时 + 区间：1.5h–1h45m = 105（P0）", 1,
     BIG_SCOPE + SEGMENTS_OK.replace("| 80 min |", "| 1.5h–1h45m |"), must=["105 min > 90 min"])
case("段落懒续行里的缩进 scope 不许静默忽略（P1 假 PASS）", 2,
     "# t\n一些正文\n    <!-- pdrs-scope: files=6 est_min=300 -->\n\n## 要做\n1. 改 src/a.rs\n",
     must=["缩进在正文下面"])
case("列表项下贴的缩进示例仍静默忽略（正控）", 0,
     "# t\n\n- scope 写法示例：\n    <!-- pdrs-scope: files=2 lines=80 est_min=30 -->\n\n正文\n")
case("文档第一行就是列表项 + 缩进示例（正控，gate16 GLM P2）", 0,
     "- scope 写法示例：\n    <!-- pdrs-scope: files=2 lines=80 est_min=30 -->\n\n## 要做\n1. 改 src/a.rs\n")
case("大写 PROGRESS.md 也算进度文件（P3 假 FAIL）", 0,
     BIG_SCOPE + SEGMENTS_OK.replace("_tmp/20261105-issue-100/progress.md",
                                     "_tmp/20261105-issue-100/PROGRESS.md"))

# ---- fix-r14：gate15（DeepSeek boundary P0×1 P1×1 + GLM sibling P2×1）------------------------------
# 长正文（≥4096 字符）后面缩进的 scope：回看窗口里没有换行，旧代码把文档开头当「上一个非空行」
LONG_PROSE = "目标：" + "很长的正文" * 900
case("超长正文行后面的缩进 scope 也不许静默忽略（P0 假 PASS）", 2,
     "- 目标：x\n" + LONG_PROSE + "\n    <!-- pdrs-scope: files=6 est_min=300 -->\n\n## 要做\n1. 改 src/a.rs\n",
     must=["缩进在正文下面"])
case("括注「不包含」是排除：90 + 30 = 120（P1 假 PASS）", 1,
     BIG_SCOPE + SEGMENTS_OK.replace("| 80 min |", "| 90 min（不包含 30 min 联调） |"),
     must=["120 min > 90 min"])
case("括注「不包括」同样计入（P1）", 1,
     BIG_SCOPE + SEGMENTS_OK.replace("| 80 min |", "| 90 min（不包括 30 min 测试） |"),
     must=["120 min > 90 min"])
case("括注「不计入」是明确排除，仍不计入（正控）", 0,
     BIG_SCOPE + SEGMENTS_OK.replace("| 80 min |", "| 90 min（不计入 30 min 缓冲） |"))
case("全角花括号占位符（P2 假 PASS，两家同报）", 1,
     BIG_SCOPE + SEGMENTS_OK.replace("`src/interaction.rs`", "｛请填写本段涉及的文件｝"),
     must=["占位符"])

# ---- fix-r15：gate16 GLM boundary P1（围栏前导空白不限缩进，两头都错）------------------------------
case("缩进代码块里的孤 ``` 不是围栏开栏（P1 假 ERROR）", 1,
     "# t\n\n写法示例（上一单）：\n\n    ```\n\n正文结束。\n\n" + TEN_RS + "\n",
     must=["分段交付"])
case("顶格围栏里嵌 4 格缩进围栏不提前闭合外层（P1 假 PASS）", 1,
     "# t\n\n```\n示例：\n\n    ```\n<!-- pdrs-scope: files=2 lines=80 est_min=30 -->\n"
     "    ```\n```\n\n" + TEN_RS + "\n", must=["分段交付"])
case("3 格缩进的围栏仍按围栏处理（正控：围栏里的 scope 不算声明）", 0,
     "# t\n\n   ```\n<!-- pdrs-scope: files=16 est_min=180 -->\n   ```\n\n正文\n")

# ---- fix-r18：gate19（MiniMax boundary P0×1 P2×5 P3×1 + GLM boundary P1×1）------------------------
# 行中的 scope 注释（正文后面贴一条示例）修前按「顶格声明」收下，`if scopes:` 短路掉人日 / `.rs`
# 两路回退 → 9 个 `.rs` 的包 rc 0 小包（实测）。声明必须独占行首。
case("行中贴的示例 scope 不许短路回退（9 个 .rs 仍判大包，P0 假 PASS）", 2,
     "# t\n\n写法参考 <!-- pdrs-scope: files=2 lines=80 est_min=30 --> 这样写\n\n" + TEN_RS + "\n",
     must=["同一行"])
case("行中 scope 后面紧跟第二条（同一行多个）也报（P0 同类）", 2,
     "<!-- pdrs-scope: files=2 est_min=30 --> <!-- pdrs-scope: files=3 est_min=40 -->\n\n"
     + TEN_RS + "\n", must=["同一行"])
case("独占一行的顶格 scope 仍算声明（正控）", 1,
     "# t\n\n<!-- pdrs-scope: files=16 est_min=180 -->\n\n正文\n", must=["分段交付"])
# 小节标题按「整节名」认：`为什么要分段交付` 抢在真节前面会把合规任务书判成缺节（假 FAIL）；
# `分段交付的背景` 里贴上一单的旧表会顶替本包（假 PASS）。
case("小节标题带说明后缀仍认（正控：分段交付（大包必填））", 0,
     BIG_SCOPE + SEGMENTS_OK.replace("## 分段交付", "## 分段交付（大包必填）"))
case("「为什么要分段交付」不抢真节（P1 假 FAIL）", 0,
     BIG_SCOPE + SEGMENTS_OK.replace("## 分段交付",
                                     "## 为什么要分段交付\n\n因为 #1316 跑满 7200 s。\n\n## 分段交付"))
case("「分段交付的背景」里的旧表不算本包（P1 假 PASS）", 1,
     BIG_SCOPE + SEGMENTS_OK.replace("## 分段交付", "## 分段交付的背景"))
case("「续跑说明」不算 ## 续跑（假 PASS）", 1,
     BIG_SCOPE + SEGMENTS_OK.replace("## 续跑", "## 续跑说明"), must=["续跑"])

# 合规路径与骨架占位并存：坏的那条修前被静默丢掉（派单里原样留着骨架也 rc 0）
case("续跑里占位路径与实例路径并存仍要报（P2 fail-open）", 1,
     BIG_SCOPE + SEGMENTS_OK.replace(
         "- 进度文件：`_tmp/20261105-issue-100/progress.md`（每段一行）",
         "- 进度文件：`_tmp/20261105-issue-100/progress.md`（实例）；骨架是 `_tmp/<日期>-issue-<N>/progress.md`"),
     must=["占位"])
# 表头写 `预计用时`：与 `预估时长` 同类，裸数字按 min（修前「读不出」→ 合规任务书假 FAIL）
case("表头「预计用时」+ 裸数字按 min（P2 假 FAIL）", 0,
     BIG_SCOPE + SEGMENTS_OK.replace("| 预估时长 |", "| 预计用时 |")
     .replace("| 80 min |", "| 80 |").replace("| 60 min |", "| 60 |"))
# `预期 2 人日` / `预计需要 2 人日`：人日回退的关键词表漏了它们 → 大包被判小包（假 PASS）
case("「预期 2 人日」也算人日证据（P2 假 PASS）", 1,
     "# t\n\n预期 2 人日\n\n1. 改 `src/a.rs`\n2. 改 `src/b.rs`\n3. 改 `src/c.rs`\n",
     must=["分段交付"])
case("「预计需要 2 人日」也算（P2 假 PASS）", 1,
     "# t\n\n预计需要 2 人日\n\n1. 改 `src/a.rs`\n2. 改 `src/b.rs`\n3. 改 `src/c.rs`\n",
     must=["分段交付"])
case("「预期 0.1 人日」不达阈值（正控：小包）", 0,
     "# t\n\n预期 0.1 人日\n\n1. 改 `src/a.rs`\n2. 改 `src/b.rs`\n3. 改 `src/c.rs`\n")
# 共写检测的扩展名白名单补齐（`.xml` 修前整类不可见 → 两段共写不报，假 PASS）
case("两段共写 a.xml 要报（P2 假 PASS）", 1,
     BIG_SCOPE + SEGMENTS_OK.replace("`src/interaction.rs`", "`config/a.xml`")
     .replace("`src/native_http.rs`", "`config/a.xml`"), must=["共写"])
case("大小写不同的 A.RS 与 a.rs 是同一文件（P2 假 PASS）", 1,
     BIG_SCOPE + SEGMENTS_OK.replace("`src/interaction.rs`", "`SRC/A.RS`")
     .replace("`src/native_http.rs`", "`src/a.rs`"), must=["共写"])
case("无扩展名的 Dockerfile 共写要报（P2 假 PASS）", 1,
     BIG_SCOPE + SEGMENTS_OK.replace("`src/interaction.rs`", "`deploy/Dockerfile`")
     .replace("`src/native_http.rs`", "`deploy/Dockerfile`"), must=["共写"])
case("两个不同的 xml 不是共写（正控）", 0,
     BIG_SCOPE + SEGMENTS_OK.replace("`src/interaction.rs`", "`config/a.xml`")
     .replace("`src/native_http.rs`", "`config/b.xml`"))

# ---- fix-r19：gate20（GLM boundary P0×1 P2×4；MiniMax boundary P0×1 P1×1 P2×2）------------------
# 行内代码的反引号修前跨段落配对：正文里一个笔误的未闭合反引号会和后面段落的反引号配成一对，
# 把中间的顶格 scope 整段抹成空格 → 既解析不到也报不出错，退到 `.rs` 回退（0 个）判小包。
case("杂散未闭合反引号不许吞掉后面段落里的真 scope（P0 假 PASS）", 1,
     "# t\n\n一些正文，笔误：按`下面写法统一\n\n<!-- pdrs-scope: files=16 est_min=300 -->\n\n"
     "## 要做\n1. 改 `src/a.py`\n", must=["分段交付"])
case("同一段落里的行内代码仍屏蔽 scope（正控）", 0,
     "# t\n\n正文 `示例 <!-- pdrs-scope: files=2 est_min=30 --> 这样` 结束\n\n## 要做\n1. 改 `src/a.rs`\n")
# 首条数据行全 `-`（没填的段行）被当成第二条分隔行整行丢掉：占位符 / 空值 / 估时全不校验
case("第一条数据行全是 - 也要查估时（P0 假 PASS）", 1,
     BIG_SCOPE + SEGMENTS_OK.replace(TABLE.splitlines()[2],
                                     "| - | - | - | - | - |"), must=["读不出"])
case("第一条数据行全是 - 且带占位符也要报（P0 同类）", 1,
     BIG_SCOPE + SEGMENTS_OK.replace(TABLE.splitlines()[2],
                                     "| - | - | - | <待填> | - |"), must=["占位符"])

# ---- fix-r20：gate21（GLM boundary P1×4 P2×3；MiniMax boundary P1×1 P2×1）---------------------
# 同一节里示例表（五列齐全、≥2 行）排在本包真表前面：修前只看第一张 → 真表整张不校验（假 PASS）
_EXAMPLE = ("| 段 | 涉及文件 | 交付物 | 预估时长 | 段间依赖 |\n"
            "|----|---------|--------|---------|---------|\n"
            "| S1 示例 | `src/example.rs` | `S1-RESULT.md` | 30 min | — |\n"
            "| S2 示例 | `src/example2.rs` | `S2-RESULT.md` | 30 min | S1 |\n")
case("示例表在前、真表在后：真表的 300 min 仍要抓（P1 假 PASS）", 1,
     BIG_SCOPE + "## 分段交付\n\n" + _EXAMPLE + "\n" + TABLE.replace("| 80 min |", "| 300 min |") + RESUME,
     must=["300 min > 90 min"])
case("示例表在前、真表在后（合规正控）", 0,
     BIG_SCOPE + "## 分段交付\n\n" + _EXAMPLE + "\n" + TABLE + RESUME)
# 多于表头的行：错位后恰好读到一个像估时的格子，真估时 300 min 再没人看（假 PASS）
case("数据行多一列且错位格像估时（假 PASS）", 1,
     BIG_SCOPE + SEGMENTS_OK.replace(
         TABLE.splitlines()[2],
         "| S1 交互方法 | `src/interaction.rs` | `S1-RESULT.md` | 60 min | 300 min | — |"),
     must=["字段数 6"])
# 漏写竖线的行：GFM 在这里结束表格，该行的 300 min 修前静默不校验
case("分段表中间漏写竖线（P3 假 PASS）", 1,
     BIG_SCOPE + SEGMENTS_OK.replace(
         "| S2 HTTP 同域 | `src/native_http.rs` | `S2-RESULT.md` + `final.patch` S2 | 60 min | S1 |",
         "S2 HTTP 同域 `src/native_http.rs` `S2-RESULT.md` 300 min S1"),
     must=["没有竖线"])
# 表后紧跟标题（无空行）不算漏写竖线：GFM 里标题本来就结束表格
case("表后紧跟标题不算漏写竖线（正控）", 0,
     BIG_SCOPE + "## 分段交付\n\n" + TABLE + RESUME)
# GFM 转义竖线 `\|` 是格内字面竖线：修前多切一格 → 合规任务书报「读不出」（假 FAIL）
_ESC = TABLE.replace("| S2 HTTP 同域 |", "| S2 \\| 存储 |")
case("转义竖线 \\| 不切列（正控）", 0, BIG_SCOPE + "## 分段交付\n\n" + _ESC + RESUME)
case("转义竖线 \\| 之后 120 min 仍要抓", 1,
     BIG_SCOPE + "## 分段交付\n\n" + _ESC.replace("| 60 min |", "| 120 min |") + RESUME,
     must=["120 min > 90 min"])
# 路径段的裸占位词：修前只认单字母 N/M/K，`xxx` / `NUM` 照当实例路径
case("续跑路径段 xxx 是占位（P1 假 PASS）", 1,
     BIG_SCOPE + SEGMENTS_OK.replace("_tmp/20261105-issue-100/progress.md", "_tmp/xxx/progress.md"),
     must=["占位"])
case("续跑路径段 issue-NUM 是占位（P1 假 PASS）", 1,
     BIG_SCOPE + SEGMENTS_OK.replace("_tmp/20261105-issue-100", "_tmp/issue-NUM"),
     must=["占位"])
case("续跑路径段是正常日期目录（正控）", 0,
     BIG_SCOPE + SEGMENTS_OK.replace("_tmp/20261105-issue-100", "_tmp/20261105-issue-1316"))
# URL 里的 resume 不是「第 N 段失败从哪继续」的说明
case("续跑只写含 resume 的 URL（P2 假 PASS）", 1,
     BIG_SCOPE + SEGMENTS_OK.split("## 续跑")[0]
     + "## 续跑\n\n- 进度文件：`_tmp/20261105-issue-100/progress.md`\n- 参考：https://example.com/resume\n",
     must=["第 N 段失败"])
# 正文里给词加引号不是 Rust 命名参数
case("正文 (\"{file}\") 仍算占位（P2 假 PASS）", 1,
     BIG_SCOPE + SEGMENTS_OK.replace("`src/interaction.rs`", '("{file}")'),
     must=["占位符"])
case("format!(\"{name}\") 是代码不缺占位（正控）", 0,
     BIG_SCOPE + SEGMENTS_OK.replace("`src/interaction.rs`", 'format!("{name}")'))
# 列表项 + 空行 + 4 格缩进 scope：CommonMark 里是松散列表项正文，不是「贴的示例」
case("列表项 + 空行 + 缩进 scope 不许静默忽略（P2 假 PASS）", 2,
     "- 目标：\n\n    <!-- pdrs-scope: files=6 est_min=300 -->\n\n## 要做\n1. x\n",
     must=["缩进"])
case("列表项紧贴缩进 scope 仍是示例（正控）", 0,
     "- 目标：\n    <!-- pdrs-scope: files=2 est_min=30 -->\n\n## 要做\n1. 改 `src/a.rs`\n")
# 估时乘法：`4 × 40 min` 是 160，修前读成 44 ≤ 90（假 PASS）
case("估时乘法 4 × 40 min 不许读成 44（P1 假 PASS）", 1,
     BIG_SCOPE + SEGMENTS_OK.replace("| 80 min |", "| 4 × 40 min |"), must=["160 min > 90 min"])
case("估时乘法 2 × 30 min 合规（正控）", 0,
     BIG_SCOPE + SEGMENTS_OK.replace("| 80 min |", "| 2 × 30 min |"))

# parse_est 数值直判：只断言 rc 抓不到「值算错但方向仍对」的回归
PARSE_EST = [
    ("80 min（2 个文件）", "", 80.0),
    ("60 min（S1 和 S2 各 30 min）", "", 60.0),
    ("90 min（含双段联调 10 min）", "", 90.0),
    ("80 min（S1 + S2 各 40 min）", "", 80.0),
    ("40 min + 60 min（含 10 min 缓冲）", "", 100.0),
    ("60 min + 40 min（最多再补 20 min）", "", 120.0),
    ("1h100min", "", 160.0),
    ("1h130m", "", 190.0),
    ("1.5h30m", "", 120.0),
    ("1.5h40m", "", 130.0),
    ("1.5h100min", "", 190.0),
    ("90 min（不含 30 min 测试）", "", 120.0),
    ("60 min（未含 40 min 联调）", "", 100.0),
    ("90 min（含 30 min 测试）", "", 90.0),
    ("90 min（不包含 30 min 联调）", "", 120.0),
    ("90 min（不包括 30 min 测试）", "", 120.0),
    ("90 min（未包含 20 min）", "", 110.0),
    ("90 min（不计入 30 min 缓冲）", "", 90.0),
    ("90 min（需要包含 30 min 联调）", "", 90.0),
    ("90 min（不需要包含 30 min 联调）", "", 120.0),
    ("80 min（20 min 联调不计入）", "", 80.0),
    ("1h30m–1h45m", "", 105.0),
    ("1.5h–1h45m", "", 105.0),
    ("80–120 min", "", 120.0),
    ("1,0000 min", "", 10000.0),
    ("1，0000 min", "", 10000.0),
    ("1 h 100 min", "", 160.0),
    ("60 至 80 分钟", "", 80.0),
    ("60 到 80 分钟", "", 80.0),
    ("60-80 分钟", "", 80.0),
    ("1.5 小时", "", 90.0),
    ("80 min（外加 20 min 联调）", "", 100.0),   # 外加是加法，不是「包含说明」（MiniMax P1）
    ("80 min 外加 20 min", "", 100.0),
    ("60 min 或 90 min", "", 90.0),               # 或 / 、是二选一 / 列表，取 max 不求和（GLM P2）
    ("60 min、90 min", "", 90.0),
    ("80 min，涉及 2 个文件", "", 80.0),          # 句读后的「数字 + 计数量词」不是估时（GLM P2）
    ("80 min；另含缓冲说明 3 项", "", 80.0),
    ("2", "h", 120.0),
    ("2", "d", None),
    ("2", None, None),
    ("4 × 40 min", "", 160.0),      # 乘法连写（gate21 GLM P1）
    ("2 × 30 min", "", 60.0),
    ("4x40 min", "", 160.0),
    ("1h12345m", "", 12405.0),      # 分钟部不再限 4 位（gate21 GLM P3）
]

CLI_CASES = [
        ("--print-skeleton 带文件参数", ["x", "--print-skeleton", "task.md"], 3),
        ("--print-skeleton 带 --recent", ["x", "--print-skeleton", "--recent", "24"], 3),
        ("未知参数不许静默吞掉", ["x", "--jso", "task.md"], 3),
        ("--recent 缺小时数", ["x", "--recent"], 3),
        ("--recent nan 是用法错误", ["x", "--recent", "nan", "task.md"], 3),
        ("--recent 0 是用法错误", ["x", "--recent", "0", "task.md"], 3),
        ("--recent 窗口短于新鲜度阈值是用法错误", ["x", "--recent", "0.001", "task.md"], 3),
        ("--recent 负数 是用法错误", ["x", "--recent", "-1", "task.md"], 3),
        ("--recent --root 不是目录是用法错误", ["x", "--recent", "24", "--root", "/nonexistent-pdrs-xyz"], 3),
        ("--root 不带 --recent 是用法错误", ["x", "--root", ".", "task.md"], 3),
        ("--recent 写了两次是用法错误（last-wins 会漏扫）",
         ["x", "--recent", "24", "--recent", "24"], 3),
        ("--root 写了两次是用法错误（last-wins 会漏扫）",
         ["x", "--recent", "24", "--root", ".", "--root", "."], 3),
        ("--json 与 --explain 同用是用法错误（explain 被静默吞掉）",
         ["x", "--json", "--explain", "task.md"], 3),
        ("--print-skeleton 与 --json 同用是用法错误（输出不是 JSON）",
         ["x", "--print-skeleton", "--json"], 3),
        ("--quiet 与 --explain 同用是用法错误（quiet 被静默忽略）",
         ["x", "--quiet", "--explain", "task.md"], 3),
        ("--json 与 --quiet 同用是用法错误（SUMMARY 语义不明）",
         ["x", "--json", "--quiet", "task.md"], 3),
]



# ---- 修复轮字段（#3056 长尾收口卡「机检落点」）：路径段 fix-rN 才触发 ------------
# 载荷都是小包文本（无 scope 声明、无 9 个 .rs）：字段检查须在 big/small 分支之前生效（义务 2）。
FIX_TASKBOOK = "# #9 fix(gateway) 修复轮\n\n## 要做\n1. 改 `src/a.rs` 补测试\n"
FIXTYPE_ENUM = "<!-- pdrs-fixtype: 枚举式 词表封闭性：两类词表各自全文遍历，无第三种写法 -->\n"
FIXTYPE_STRUCT = "<!-- pdrs-fixtype: 结构性 词表核验：测试真实遍历的词表 12/12 项全部覆盖 -->\n"
FIXTYPE_NA = "<!-- pdrs-fixtype: 不适用 -->\n"
RESIDUAL_OK = "<!-- pdrs-residual: 上轮的洞是边界暴露：仅修饰窗差一词，修法本身成立 -->\n"
RESIDUAL_NONE = "<!-- pdrs-residual: 无上一轮 -->\n"

FIX_CASES = []


def fixcase(name, expect_rc, text, must=None, rnd="fix-r5"):
    FIX_CASES.append((name, expect_rc, rnd, text, must or []))


fixcase("修复轮正控：枚举式（附边界论证）", 0, FIX_TASKBOOK + FIXTYPE_ENUM + RESIDUAL_OK)
fixcase("修复轮正控：结构性（附词表核验）", 0, FIX_TASKBOOK + FIXTYPE_STRUCT + RESIDUAL_OK)
fixcase("修复轮正控：不适用（任务书侧不查路径）", 0, FIX_TASKBOOK + FIXTYPE_NA + RESIDUAL_OK)
fixcase("修复轮缺 fixtype 字段", 1, FIX_TASKBOOK + RESIDUAL_OK, must=["pdrs-fixtype"])
fixcase("修复轮子类字样：结构派生", 1,
        FIX_TASKBOOK + "<!-- pdrs-fixtype: 结构派生 已覆盖全部词表 -->\n" + RESIDUAL_OK,
        must=["字面三值", "结构派生"])
fixcase("枚举式无边界论证", 1, FIX_TASKBOOK + "<!-- pdrs-fixtype: 枚举式 -->\n" + RESIDUAL_OK,
        must=["缺论证"])
fixcase("fixtype 值是空白串", 1, FIX_TASKBOOK + "<!-- pdrs-fixtype:    -->\n" + RESIDUAL_OK,
        must=["空白"])
fixcase("fixtype 标记出现两次", 1, FIX_TASKBOOK + FIXTYPE_ENUM + FIXTYPE_STRUCT + RESIDUAL_OK,
        must=["至多一次"])
fixcase("fixtype 论证是占位符", 1, FIX_TASKBOOK + "<!-- pdrs-fixtype: 枚举式 <边界论证> -->\n" + RESIDUAL_OK,
        must=["占位符"])
fixcase("fix-r5（N≥4）缺 residual", 1, FIX_TASKBOOK + FIXTYPE_ENUM, must=["pdrs-residual"])
fixcase("residual 内容是空白串", 1, FIX_TASKBOOK + FIXTYPE_ENUM + "<!-- pdrs-residual:   -->\n",
        must=["空白"])
fixcase("residual 字面值 无上一轮（正控）", 0, FIX_TASKBOOK + FIXTYPE_ENUM + RESIDUAL_NONE)
fixcase("residual 内容是占位符", 1, FIX_TASKBOOK + FIXTYPE_ENUM + "<!-- pdrs-residual: <上轮残差判读> -->\n",
        must=["占位符"])
fixcase("fix-r3（N<4）不要求 residual（正控）", 0, FIX_TASKBOOK + FIXTYPE_ENUM, rnd="fix-r3")
fixcase("fix-r3 仍要求 fixtype", 1, FIX_TASKBOOK, must=["pdrs-fixtype"], rnd="fix-r3")
# fix-r3 P1-3（MM boundary#2）：接续轮 b 后缀——`fix-r11b` 算第 11 轮（真实目录
# _tmp/20261108-archive-wt-1809/20261026-issue-1809/fix-r11b/task.md 即此形），字段义务
# （fixtype 必填 / N≥4 residual）不得整段跳过；`fix-r5x`（非 b 后缀）仍不是修复轮
# （与 PR 侧 test-review-roles.py 的 fix-r5x 负控同词表，单源 FIXROUND_RE）。
fixcase("fix-r11b 接续轮算第 11 轮：字段齐（正控）", 0,
        FIX_TASKBOOK + FIXTYPE_STRUCT + RESIDUAL_OK, rnd="fix-r11b")
fixcase("fix-r11b 接续轮仍要求 fixtype", 1, FIX_TASKBOOK, must=["pdrs-fixtype"], rnd="fix-r11b")
fixcase("fix-r11b（N≥4）缺 residual", 1, FIX_TASKBOOK + FIXTYPE_ENUM, must=["pdrs-residual"], rnd="fix-r11b")
fixcase("fix-r5x 不是修复轮：字段不检查（正控）", 0,
        FIX_TASKBOOK + FIXTYPE_ENUM + RESIDUAL_OK, rnd="fix-r5x")
# fix-r8 boundary P1（MM r7 评审）：超长 fix-r 段（≥19 位）不得静默判非修复轮——
# 同一 token 在 PR 侧 check() 被 _overflow_err fail-closed 拒收，任务书侧旧口径
# fullmatch 失败 → None → fixtype/residual 义务整段跳过＝两闸门同输入结论相反。
# 修复＝镜像 fail-closed：check_file 报「读不了」rc=2（与 非UTF-8/FIFO 同款拒收通道）。
fixcase("超长 fix-r 段（≥19 位）：fail-closed 读不了，不静默跳过字段义务", 2,
        FIX_TASKBOOK, must=["读不了", "位界"], rnd="fix-r" + "1" * 19)
fixcase("围栏里的 fixtype 不算声明", 1, FIX_TASKBOOK + "```\n" + FIXTYPE_ENUM + "```\n" + RESIDUAL_OK,
        must=["pdrs-fixtype"])
fixcase("大包修复轮：分段 + 字段齐（正控）", 0, BIG_SCOPE + SEGMENTS_OK + FIXTYPE_STRUCT + RESIDUAL_OK)
fixcase("大包修复轮：缺分段表（字段齐）", 1, BIG_SCOPE + FIXTYPE_ENUM + RESIDUAL_OK, must=["分段交付"])
fixcase("大包修复轮：分段齐但缺字段", 1, BIG_SCOPE + SEGMENTS_OK, must=["pdrs-fixtype"])
case("非修复轮带 fixtype/residual 标记：不检查（正控）", 0, SMALL + FIXTYPE_ENUM + RESIDUAL_OK)


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

        # 修复轮字段用例：任务书写进 _tmp/<单>/fix-rN/task.md（路径段即修复轮信号，与 ops
        # dispatch-fix 的生成位置同构）；载荷是小包文本——字段检查须先于 big/small 分支。
        for name, expect, rnd, text, must in FIX_CASES:
            p = Path(td) / "_tmp" / "20261105-issue-9" / rnd / "task.md"
            p.parent.mkdir(parents=True, exist_ok=True)
            p.write_text(text, encoding="utf-8")
            r = ctbs.check_file(p)
            out = "；".join(r["problems"])
            ok = r["rc"] == expect and all(s in out for s in must)
            print(f"{'PASS' if ok else 'FAIL'}  {name}：rc={r['rc']}（期望 {expect}）{'' if ok else ' ← ' + out}")
            if not ok:
                fails += 1

        # 独立检查的条数用**实际打印的行数**数，不手写常数：chmod 000（root 下权限位无效）这类
        # 分支会整块跳过而不打印，常数让 SUMMARY 虚报（gate21 MiniMax boundary P3 实测：root 下
        # 报 303、实际只跑 302）。
        import io
        _manual = {"n": 0}
        _real_stdout = sys.stdout

        class _CountedOut(io.TextIOBase):
            def write(self, s):
                _manual["n"] += s.count("\n")
                return _real_stdout.write(s)

            def flush(self):
                _real_stdout.flush()

        sys.stdout = _CountedOut()

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
        def _boom_scandir(path):
            raise PermissionError(13, "Permission denied", str(path))

        real_scandir = os.scandir
        os.scandir = _boom_scandir
        try:
            try:
                ctbs.find_recent(td, 24)
                ok = False
            except ctbs.ScanError:
                ok = True
        finally:
            os.scandir = real_scandir
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

        # 真实权限拒绝：chmod 000 的 _tmp 下任务书必须让扫描「中断」而不是静默消失
        if os.name == "posix" and os.geteuid() != 0:   # root 下 chmod 000 拦不住，用例会假红
            blocked = Path(td) / "_build" / "wt-9998" / "_tmp"
            blocked.mkdir(parents=True)
            (blocked / "task.md").write_text(SMALL, encoding="utf-8")
            os.utime(blocked / "task.md", (time.time() - 3600, time.time() - 3600))
            os.chmod(blocked, 0o000)
            try:
                try:
                    ctbs.find_recent(td, 24)
                    ok = False
                except ctbs.ScanError:
                    ok = True
                finally:
                    os.chmod(blocked, 0o755)
            except Exception:
                os.chmod(blocked, 0o755)
                raise
            print(f"{'PASS' if ok else 'FAIL'}  真实 chmod 000 的目录 → ScanError（不许静默漏扫）")
            fails += 0 if ok else 1

        # ---- fix-r11：stat 与 open 之间换入 FIFO / 换 inode 的窗口（concurrency P1/P2）----
        # 1) 任务书本身是符号链接：先解析成字面路径再 open，照读不误
        real_md = Path(td) / "link-real.md"
        real_md.write_text(SMALL, encoding="utf-8")
        link_md = Path(td) / "link.md"
        if not link_md.exists():
            link_md.symlink_to(real_md)
        r = ctbs.check_file(link_md)
        ok = r is not None and r["rc"] == 0
        print(f"{'PASS' if ok else 'FAIL'}  符号链接任务书照读（rc 0）：rc={r and r['rc']}")
        fails += 0 if ok else 1

        # 2) 打开标志必须带 O_NOFOLLOW | O_NONBLOCK（FIFO 换入不再挂死）
        seen = {}
        real_open = os.open

        def spy_open(path, flags, *a, **kw):
            seen["flags"] = flags
            return real_open(path, flags, *a, **kw)

        ctbs.os.open = spy_open
        try:
            ctbs.check_file(real_md)
        finally:
            ctbs.os.open = real_open
        need = os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_NONBLOCK", 0)
        ok = seen.get("flags") == need
        print(f"{'PASS' if ok else 'FAIL'}  open 标志带 O_NOFOLLOW|O_NONBLOCK：{seen.get('flags')!r}")
        fails += 0 if ok else 1

        # 3) 读到一半路径上的文件被换成同 size/mtime 的另一份 → rc 2（旧实现按路径 stat 会放行）
        victim = Path(td) / "swap.md"
        victim.write_text(SMALL, encoding="utf-8")
        other = Path(td) / "swap-other.md"
        other.write_text(SMALL, encoding="utf-8")
        st = victim.stat()
        os.utime(other, ns=(st.st_atime_ns, st.st_mtime_ns))
        real_fdopen = os.fdopen

        def spy_fdopen(fd, *a, **kw):
            os.replace(other, victim)          # 读之前把路径换成另一份（inode 变了，size/mtime 不变）
            return real_fdopen(fd, *a, **kw)

        ctbs.os.fdopen = spy_fdopen
        try:
            r = ctbs.check_file(victim)
        finally:
            ctbs.os.fdopen = real_fdopen
        ok = r is not None and r["rc"] == 2 and "替换" in r["problems"][0]
        detail = (r["problems"] or [""])[0][:30] if r else ""
        print(f"{'PASS' if ok else 'FAIL'}  读中路径被同 size/mtime 换掉 → rc=2：rc={r and r['rc']} {detail}")
        fails += 0 if ok else 1

        # 4) stat 与 open 之间换成 FIFO：必须靠 O_NONBLOCK + fstat 判类型 rc 2（不阻塞）。
        #    换入点必须在 os.open **之前**：旧用例在 fdopen 时才换，fd 仍指向原普通文件，
        #    删掉 S_ISREG 判定照样 PASS（断言只看 rc，没测到声称的那条路径）。
        fifo_md = Path(td) / "swapfifo.md"
        fifo_md.write_text(SMALL, encoding="utf-8")
        real_open2 = os.open
        target = os.path.realpath(fifo_md)

        def spy_open2(path, flags, *a, **kw):
            if str(path) == target:
                if fifo_md.exists():
                    fifo_md.unlink()
                os.mkfifo(fifo_md)
            return real_open2(path, flags, *a, **kw)

        ctbs.os.open = spy_open2
        try:
            r = ctbs.check_file(fifo_md)
        finally:
            ctbs.os.open = real_open2
        ok = (r is not None and r["rc"] == 2 and "不是普通文件" in r["problems"][0])
        detail = (r["problems"] or [""])[0][:20] if r else ""
        print(f"{'PASS' if ok else 'FAIL'}  stat→open 之间换成 FIFO → rc=2 非普通文件："
              f"rc={r and r['rc']} {detail}")
        fails += 0 if ok else 1

        # 5) mask_inline_code 也是线性的：长度 1..2000 的反引号开串全不闭合 = 2 MiB 病态输入，
        #    修前逐串后扫 O(k·n) 实测 53 s
        bad_ticks = "".join("`" * i + "a" for i in range(1, 2001))
        t0 = time.monotonic()
        masked = ctbs.mask_inline_code(bad_ticks)
        dt = time.monotonic() - t0
        ok = (dt < 5.0 and masked.count("a") == 2000
              and ctbs.mask_inline_code("`a` `b`") == " " * 7          # 两段各自整体抹掉
              and ctbs.mask_inline_code("x `no close") == "x `no close")  # 没闭合按字面量
        print(f"{'PASS' if ok else 'FAIL'}  mask_inline_code 线性（2 MiB 病态输入 {dt:.2f}s）："
              f"未闭合按字面量保留={masked.count('a') == 2000}")
        fails += 0 if ok else 1

        # 6) strip_fences 线性：2 MiB 全闭合围栏（262144 跨）修前每跨整串重建，实测 32.66 s
        big_fences = "```\n```\n" * 262144
        t0 = time.monotonic()
        no_fence = ctbs.strip_fences(big_fences)
        dt = time.monotonic() - t0
        ok = (dt < 5.0 and no_fence.strip() == ""
              and no_fence.count("\n") == big_fences.count("\n"))
        print(f"{'PASS' if ok else 'FAIL'}  strip_fences 线性（2 MiB / 26 万跨 {dt:.2f}s）："
              f"全抹平={no_fence.strip() == ''} 行数不变={no_fence.count(chr(10)) == big_fences.count(chr(10))}")
        fails += 0 if ok else 1

        # 7) parse_est 超长格子：2 MiB 单格修前 44.19 s（O(n²)），现在按 MAX_EST_CELL 直接判「读不出」
        t0 = time.monotonic()
        got = ctbs.parse_est("60 min + 1 " * 174762)
        dt = time.monotonic() - t0
        ok = got is None and dt < 1.0
        print(f"{'PASS' if ok else 'FAIL'}  parse_est 2 MiB 单格 {dt:.2f}s → {got}（须 <1 s 且读不出）")
        fails += 0 if ok else 1

        # 8) 读不出时的报错不嵌整格原文：否则问题列表随格子尺寸膨胀（2 MiB 格子 → 2 MiB stdout）
        long_est = BIG_SCOPE + SEGMENTS_OK.replace("| 80 min |", "| " + "x" * 5000 + " |")
        probs = ctbs.check_segments(long_est)
        worst = max((len(x) for x in probs), default=0)
        ok = worst < 400 and any("读不出" in x for x in probs)
        print(f"{'PASS' if ok else 'FAIL'}  长格子报错截断（最长问题 {worst} 字符）：读不出={any('读不出' in x for x in probs)}")
        fails += 0 if ok else 1

        # 5) --explain 的空扫描也不许打文本 SUMMARY（按行解析的调用方不认）
        empty3 = Path(td) / "emptyroot3"
        empty3.mkdir()
        buf = io.StringIO()
        with redirect_stdout(buf), redirect_stderr(io.StringIO()):
            rc = ctbs.main(["x", "--recent", "24", "--root", str(empty3), "--explain"])
        ok = rc == 0 and "SUMMARY" not in buf.getvalue()
        print(f"{'PASS' if ok else 'FAIL'}  --explain 空扫描不打 SUMMARY：rc={rc} 输出={buf.getvalue()!r}")
        fails += 0 if ok else 1

        # 6) scope / 注释扫描必须是线性：修前 2 MiB 病态输入 O(n²)，派单闸门挂死（实测 >120 s）
        sc1, err1 = ctbs.parse_scopes("<!--" * 500000)                      # 2 MiB 全未闭合注释
        # 下面两条都**没有换行**：整份文档是一行。修前每条 scope 都切一遍 text[行首:pos]（行首恒 0），
        # 1.56 MB 实测 1.54 s、2.03 MB 2.6 s（O(n²)）。绝对秒数在满载机器上会飘（load 29 实测同一条 0.72–1.51 s），
        # 所以判「输入翻倍、耗时 < 3 倍」：修前是 4 倍（平方），修后 ~2 倍（线性），与机器负载无关。
        one = "<!--pdrs-scope files=2 -->" * 78000      # 2.03 MB / 7.8 万条，全挤在一行
        # 挤在一行的第 2 条起都是「注释和正文同一行」→ rc 2（fix-r18 的 P0 规则），只算 1 条声明；
        # 每条独占一行时 7.8 万条都是真声明。两种形态都要线性。
        # 每档取 3 次的最小值：满载机器上单次会被抢占（实测同一档 0.35 / 0.9 / 1.2 s 都出现过），
        # 最小值才反映算法本身；平方实现在最小耗时上照样是 4 倍。
        def _best(text, times=4):
            best = float("inf")
            res = None
            for _ in range(times):
                t0 = time.monotonic()
                res = ctbs.parse_scopes(text)
                best = min(best, time.monotonic() - t0)
            return res, best
        (sc2, err2), d1 = _best(one)
        (sc2b, err2b), d2 = _best(one * 2)              # 4.06 MB / 15.6 万条
        ratio = d2 / max(d1, 0.02)
        # 阈值 3.5：满载（四路 gate 同时在跑）实测同一条曾到 3.02 误红；平方实现是 4 倍以上，
        # 3.5 仍分得开「线性（~2 倍）」与「平方（~4 倍）」，不改变判据语义。
        (sc3, err3), _d3 = _best(one.replace(" -->", " -->\n"))            # 每条独占一行
        ok = (ratio < 3.5 and len(sc2) == 1 and len(err2) == 77999
              and len(sc2b) == 1 and len(err2b) == 155999
              and len(sc3) == 78000 and not err3
              and ctbs.parse_scopes("<!-- pdrs-scope: files=2")[1])   # 未闭合注释里的 scope 仍要报错
        print(f"{'PASS' if ok else 'FAIL'}  scope 扫描线性（2.03 MB {d1:.2f}s → 4.06 MB {d2:.2f}s，"
              f"比值 {ratio:.1f} <3.5；单行={len(sc2)} 条+{len(err2)} 报错，逐行={len(sc3)} 条）：err1={len(err1)}")
        fails += 0 if ok else 1

        # 7) scandir entry 的 is_dir() 抛 OSError 也要 ScanError，不许 traceback 成 rc 1（GLM P3）
        class _BoomEntry:
            path = str(Path(td) / "_build" / "wt-boom")

            def is_dir(self):
                raise PermissionError(13, "Permission denied", self.path)

        class _BoomScan:
            def __enter__(self):
                return iter([_BoomEntry()])

            def __exit__(self, *a):
                return False

        real_scandir3 = os.scandir
        os.scandir = lambda _p: _BoomScan()
        try:
            try:
                ctbs._iter_taskbooks(Path(td) / "_build")
                ok = False
            except ctbs.ScanError:
                ok = True
            except Exception:      # 别的异常（PermissionError 直接漏出）也是 FAIL，不许把整轮测试打断
                ok = False
        finally:
            os.scandir = real_scandir3
        print(f"{'PASS' if ok else 'FAIL'}  scandir entry.is_dir() 抛 OSError → ScanError（不 traceback）")
        fails += 0 if ok else 1

        # 8) `_tmp` 之下的符号链接子目录：`os.walk` 默认不跟随，修前静默漏掉里面的任务书（rc 0 假「没扫到」）
        symroot = Path(td) / "_build" / "wt-sym" / "_tmp" / "issue-sym"
        (symroot / "real").mkdir(parents=True, exist_ok=True)
        (symroot / "real" / "task.md").write_text("# t\n", encoding="utf-8")
        try:
            os.symlink(symroot / "real", symroot / "link")
            made_link = True
        except (OSError, NotImplementedError):   # 平台不支持符号链接：跳过，不算失败
            made_link = False
        ok = True
        if made_link:
            try:
                ctbs._iter_taskbooks(Path(td) / "_build")
                ok = False                        # 静默漏 = 假 PASS
            except ctbs.ScanError:
                ok = True
        shutil.rmtree(Path(td) / "_build" / "wt-sym", ignore_errors=True)   # 别影响后面扫同一 root 的用例
        print(f"{'PASS' if ok else 'FAIL'}  _tmp 下符号链接目录 → ScanError（不静默漏）：建链={made_link}")
        fails += 0 if ok else 1

        # 9) 派发副本 / 旧轮次备份**本身**是符号链接：按 _is_dispatch_copy 的口径跳过，不该整轮中断
        target = Path(td) / "_shared" / "remote-run-target"
        (target / "stage").mkdir(parents=True, exist_ok=True)
        (target / "stage" / "task.md").write_text("# t\n", encoding="utf-8")
        droot = Path(td) / "_build2" / "wt-1" / "_tmp" / "issue-1"
        droot.mkdir(parents=True, exist_ok=True)
        ok = True
        try:
            os.symlink(target, droot / "remote-run")
        except (OSError, NotImplementedError):   # 平台不支持符号链接：跳过，不算失败
            ok = "skip"
        if ok is True:
            try:
                got = [str(p) for p in ctbs._iter_taskbooks(Path(td) / "_build2")]
                ok = len(got) == 0      # 跳过副本，不报错也不算「扫到」
            except ctbs.ScanError:
                ok = False
        shutil.rmtree(Path(td) / "_build2", ignore_errors=True)
        print(f"{'SKIP' if ok == 'skip' else ('PASS' if ok else 'FAIL')}"
              "  remote-run 目录是符号链接时跳过（不中断整轮扫描）")
        fails += 0 if (ok is True or ok == "skip") else 1

        # 10) 大小写变体 `Task.md`：macOS 默认文件系统上同一个名字，字符串精确比较会整类漏扫
        croot = Path(td) / "_build3" / "wt-1" / "_tmp" / "issue-1"
        croot.mkdir(parents=True, exist_ok=True)
        (croot / "Task.md").write_text("# t\n", encoding="utf-8")
        got = [p.name for p in ctbs._iter_taskbooks(Path(td) / "_build3")]
        shutil.rmtree(Path(td) / "_build3", ignore_errors=True)
        ok = got == ["Task.md"] or got == ["task.md"]   # 大小写不敏感 fs 上写进去读出来可能同名
        print(f"{'PASS' if ok else 'FAIL'}  Task.md 大小写变体不整类漏扫：{got}")
        fails += 0 if ok else 1

        # 11) `.preview/` 不是 `.prev-*` 备份目录：裸前缀会把正常目录整段跳掉
        ok = (not ctbs._is_dispatch_copy(Path("_build/wt-1/_tmp/i/.preview/task.md"))
              and ctbs._is_dispatch_copy(Path("_build/wt-1/_tmp/i/.prev-r2/task.md"))
              and ctbs._is_dispatch_copy(Path("_build/wt-1/_tmp/i/remote-run/stage/task.md")))
        print(f"{'PASS' if ok else 'FAIL'}  .preview 不当备份跳过、.prev-r2 与 remote-run 仍跳过")
        fails += 0 if ok else 1

        # 12) 字面三值单源（#3056 fix-r1 item 13，DS sibling#8）：任务书侧常量必须派生自
        # check-pr-gate.py（同一集合），且赋值行不得再写一份字面值（改回复制会在加值/改名后漂移：
        # 一侧放行、另一侧永久拒收，假 FAIL）
        _gspec = importlib.util.spec_from_file_location("gate_for_taskbook_pairing", HERE / "check-pr-gate.py")
        _gate = importlib.util.module_from_spec(_gspec)
        _gspec.loader.exec_module(_gate)
        _assign = [ln for ln in (HERE / "check-taskbook-segments.py").read_text(encoding="utf-8").splitlines()
                   if ln.startswith("FIXTYPE_VALUES = ")]
        ok = (ctbs.FIXTYPE_VALUES == _gate.FIXTYPE_VALUES and len(_assign) == 1
              and "_gate_module()" in _assign[0])
        print(f"{'PASS' if ok else 'FAIL'}  FIXTYPE_VALUES 派生自 check-pr-gate（同一集合，不复制第二份）")
        fails += 0 if ok else 1

        # 13) residual 标记正则单源（#3056 fix-r2 修 7，DS simplify#1 / MM2 sibling#5）：
        # 任务书侧 RESIDUAL_RE 必须派生自 check-pr-gate.py 的 RESIDUAL_MARKER（同一 pattern 与
        # flags），且赋值行不得再写 re.compile 拷贝——两份同字面拷贝会在改标记语法时一侧判「缺」、
        # 另一侧认声明（任务书侧与 PR 侧对同一份文本给出相反结论，假 FAIL / 假 PASS 分歧）
        _r_assign = [ln for ln in (HERE / "check-taskbook-segments.py").read_text(encoding="utf-8").splitlines()
                     if ln.startswith("RESIDUAL_RE = ")]
        ok = (ctbs.RESIDUAL_RE.pattern == _gate.RESIDUAL_MARKER.pattern
              and ctbs.RESIDUAL_RE.flags == _gate.RESIDUAL_MARKER.flags and len(_r_assign) == 1
              and "_gate_module()" in _r_assign[0])
        print(f"{'PASS' if ok else 'FAIL'}  RESIDUAL_RE 派生自 check-pr-gate 的 RESIDUAL_MARKER（同一正则，不复制第二份）")
        fails += 0 if ok else 1

        # 14) N≥4 门槛单源（#3056 fix-r2，DS r2 boundary#3 P2）：任务书侧 RESIDUAL_MIN_ROUND 必须
        # 派生自 check-pr-gate.py（同一数值），且赋值行不得再写字面量 4——两份字面量会在改阈值时
        # 漂移，一个闸门要 residual、另一个不要，同一 PR 两边结论相反（假 PASS / 假 FAIL 分歧）
        _m_assign = [ln for ln in (HERE / "check-taskbook-segments.py").read_text(encoding="utf-8").splitlines()
                     if ln.startswith("RESIDUAL_MIN_ROUND = ")]
        ok = (ctbs.RESIDUAL_MIN_ROUND == _gate.RESIDUAL_MIN_ROUND and len(_m_assign) == 1
              and "_gate_module()" in _m_assign[0])
        print(f"{'PASS' if ok else 'FAIL'}  RESIDUAL_MIN_ROUND 派生自 check-pr-gate（同一门槛，不复制第二份）")
        fails += 0 if ok else 1

        # 15) 轮号词表单源（#3056 fix-r3 P1-3，MM boundary#2；fix-r4 P2-4，DS concurrency#5 +
        # boundary#11 双撞）：任务书侧 FIX_ROUND_PART 必须派生自 check-pr-gate.py 的 FIXROUND_RE，
        # 且赋值行不得再写 re.compile 拷贝——两份定义会在 b 后缀 / 位界改动时一侧认接续轮
        # 另一侧不认（该轮族的 fixtype/residual/多块一致整段跳过＝假 PASS）。断言用
        # .pattern+.flags 而非 `is`：两侧对象来自两次独立 exec_module，identity 只靠 CPython
        # re._cache 的实现细节偶然成立，缓存淘汰 / re.purge() 即假 FAIL。
        _f_assign = [ln for ln in (HERE / "check-taskbook-segments.py").read_text(encoding="utf-8").splitlines()
                     if ln.startswith("FIX_ROUND_PART = ")]
        ok = (ctbs.FIX_ROUND_PART.pattern == _gate.FIXROUND_RE.pattern
              and ctbs.FIX_ROUND_PART.flags == _gate.FIXROUND_RE.flags and len(_f_assign) == 1
              and "_gate_module()" in _f_assign[0] and "re.compile" not in _f_assign[0])
        print(f"{'PASS' if ok else 'FAIL'}  FIX_ROUND_PART 派生自 check-pr-gate 的 FIXROUND_RE（pattern+flags 同一，不复制第二份）")
        fails += 0 if ok else 1

        # 16) 超长 fix-r 段两侧同结论（#3074 fix-r8 boundary P1，MM r7 评审）：任务书侧
        # FIX_ROUND_OVER 派生自 check-pr-gate 的 FIXROUND_OVER（单源）；同一 token
        # （fix-r+19 位）在 PR 侧 _overflow_err 报错、任务书侧 fix_round_from_path 抛
        # FixRoundOverflow（调用方转 rc 2「读不了」）——两闸门对同一输入不得一个拒收
        # 一个静默放行（字段族漂移＝假 PASS）。正常段照常返回轮号（5）。
        _o_assign = [ln for ln in (HERE / "check-taskbook-segments.py").read_text(encoding="utf-8").splitlines()
                     if ln.startswith("FIX_ROUND_OVER = ")]
        _tok = "fix-r" + "1" * 19
        try:
            ctbs.fix_round_from_path(f"_tmp/20261114-issue-3056/{_tok}/task.md")
            raised = False
        except ctbs.FixRoundOverflow:
            raised = True
        ok = (ctbs.FIX_ROUND_OVER.pattern == _gate.FIXROUND_OVER.pattern
              and ctbs.FIX_ROUND_OVER.flags == _gate.FIXROUND_OVER.flags
              and len(_o_assign) == 1 and "_gate_module()" in _o_assign[0]
              and raised
              and ctbs.fix_round_from_path("_tmp/20261114-issue-3056/fix-r5/task.md") == 5
              and _gate._overflow_err(f"fix(scripts) #1 {_tok}: x", "提交标题") is not None)
        print(f"{'PASS' if ok else 'FAIL'}  超长 fix-r 段两侧同结论：PR 侧 _overflow_err 拒收、"
              f"任务书侧抛 FixRoundOverflow（rc 2 读不了），FIX_ROUND_OVER 单源派生")
        fails += 0 if ok else 1


    # 命令行契约
    sys.stdout = _real_stdout          # 恢复：下面的 CLI 契约与 PARSE_EST 用各自的 len() 计数
    for name, argv, expect in CLI_CASES:
        rc = ctbs.main(argv)
        ok = rc == expect
        print(f"{'PASS' if ok else 'FAIL'}  {name}：rc={rc}（期望 {expect}）")
        fails += 0 if ok else 1

    # 命令行契约用 len(CLI_CASES) 数；FIX_CASES 在自己的计数器建立之前打印（上面 run() 开头），
    # 所以显式计入（#3056 fix-r1 item 11，DS sibling#6：SUMMARY cases= 少报 19 条）；
    # 「线性铺开」的独立检查用实际打印行数（_manual["n"]），跳过的分支不计数。
    total = len(CASES) + len(FIX_CASES) + _manual["n"] + len(CLI_CASES) + len(PARSE_EST)
    # parse_est 直接断言数值：`rc=0` 抓不到「算成 82 但没超 90」这类回归
    for cell, unit, want in PARSE_EST:
        got = ctbs.parse_est(cell, unit)
        ok = got == want
        print(f"{'PASS' if ok else 'FAIL'}  parse_est({cell!r}, {unit!r}) = {got}（期望 {want}）")
        fails += 0 if ok else 1

    print(f"SUMMARY cases={total} failed={fails}")
    return 1 if fails else 0


if __name__ == "__main__":
    sys.exit(run())
