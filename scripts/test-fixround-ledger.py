#!/usr/bin/env python3
"""test-fixround-ledger.py — fixround-ledger.py 的配对测试（#3056 义务 8-10：幂等键 / 单写者 / 计数 / 机读分类）。

用法：python3 -I scripts/test-fixround-ledger.py   退出码 0 全过 · 1 有失败
"""
from __future__ import annotations

import contextlib
import importlib.util
import io
import json
import os
import subprocess
import sys
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
LEDGER = ROOT / "scripts" / "fixround-ledger.py"


def main() -> int:
    spec = importlib.util.spec_from_file_location("flg", LEDGER)
    flg = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    spec.loader.exec_module(flg)
    fails: list[str] = []
    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td)
        # 幂等键格式：正例（issue 号一位/多位、谱系含连字符数字）与负例（缺 #、大写谱系、r0、缺 r、尾缀）
        for key in ("#2692/ds/r11", "#1/zai/r1", "#3056/glm-5-3/r12", "#9/mm/r9"):
            if not flg.key_ok(key):
                fails.append(f"validate-key：{key} 应为合法键")
        for key in ("2692/ds/r11", "#2692/DS/r11", "#2692/ds/r0", "#2692/ds/11", "#2692/ds/r11x",
                    "#2692//r11", "#2692/ds/r", "  #2692/ds/r11"):
            if flg.key_ok(key):
                fails.append(f"validate-key：{key!r} 应为非法键")

        def run(*args: str) -> subprocess.CompletedProcess:
            return subprocess.run([sys.executable, "-I", str(LEDGER), *args],
                                  capture_output=True, text=True, env=dict(os.environ))

        # register + count：同键**同形态**先登记者为准，后到标 superseded 不计数（义务 8）
        led = tmp / "sub" / "ledger.jsonl"
        cp = run("register", str(led), "#2692/ds/r11", "--status", "ERROR", "--contention", "--note", "首次 429")
        if cp.returncode != 0 or "superseded" in cp.stdout:
            fails.append(f"register：首次登记应成功且不标 superseded（rc={cp.returncode}、{cp.stdout}）")
        # count 与 judge 同口径（#3056 fix-r1 P1）：首次争抢（contention_first）是排除段，不计入停手计数
        cp = run("count", str(led))
        if cp.returncode != 0 or "#2692/ds" in cp.stdout:
            fails.append(f"count：首次争抢不计入停手计数（rc={cp.returncode}、{cp.stdout}）")
        # 同键补跑（--rerun）参与判重：不盲标 superseded；补跑后仍争抢 = contention_counted，计入
        cp = run("register", str(led), "#2692/ds/r11", "--status", "ERROR", "--contention", "--rerun", "--note", "补跑仍 429")
        if cp.returncode != 0 or "superseded" in cp.stdout:
            fails.append(f"register：同键补跑不应标 superseded（rc={cp.returncode}、{cp.stdout}）")
        cp = run("count", str(led))
        if cp.returncode != 0 or "#2692/ds: 1" not in cp.stdout:
            fails.append(f"count：补跑后仍争抢应计入（contention_counted；rc={cp.returncode}、{cp.stdout}）")
        # 同键同形态真重复：后到仍写入但标 superseded，计数不动（义务 8）
        cp = run("register", str(led), "#2692/ds/r11", "--status", "ERROR", "--contention", "--rerun", "--note", "重复评论")
        if cp.returncode != 0 or "superseded" not in cp.stdout:
            fails.append(f"register：同键同形态重复应标 superseded（rc={cp.returncode}、{cp.stdout}）")
        recs = [json.loads(x) for x in led.read_text().splitlines()]
        if len(recs) != 3 or recs[0].get("superseded") or recs[1].get("superseded") or recs[2].get("superseded") is not True:
            fails.append(f"register：行序与 superseded 标记（{[r.get('superseded') for r in recs]}）")
        cp = run("count", str(led))
        if cp.returncode != 0 or "#2692/ds: 1" not in cp.stdout:
            fails.append(f"count：真重复登记不推计数（rc={cp.returncode}、{cp.stdout}）")

        # PASS（available）不是失败，不推阈值；第二次计入类失败 → rc 1（判据 5 停手信号）。
        # fix-r5 修 5（sibling#1）：非争抢**首登**失败是悬置（suspended，不计入亦不排除）——
        # 这里 r13 是补跑后仍失败（--rerun → unavailable_other，计入），悬置形态另在下方专测
        cp = run("register", str(led), "#2692/ds/r12", "--status", "PASS")
        if cp.returncode != 0:
            fails.append(f"register：新轮号应正常登记（rc={cp.returncode}）")
        cp = run("count", str(led))
        if cp.returncode != 0 or "#2692/ds: 1" not in cp.stdout:
            fails.append(f"count：PASS（available）不计入停手计数（rc={cp.returncode}、{cp.stdout}）")
        run("register", str(led), "#2692/ds/r13", "--status", "ERROR", "--rerun")
        cp = run("count", str(led))
        if cp.returncode != 1 or "#2692/ds: 2" not in cp.stdout or "停手" not in cp.stdout:
            fails.append(f"count：同链×谱系计入类失败累计 2 应 rc=1 并给停手信号（rc={cp.returncode}、{cp.stdout}）")
        # 不同链 / 不同谱系分开计（各 1 条计入类失败，互不触发）
        run("register", str(led), "#2692/mm/r11", "--status", "ERROR", "--rerun")
        run("register", str(led), "#1953/ds/r40", "--status", "ERROR", "--rerun")
        cp = run("count", str(led))
        if cp.returncode != 1 or "#1953/ds: 1" not in cp.stdout or "#2692/mm: 1" not in cp.stdout:
            fails.append(f"count：链×谱系应各自累计（{cp.stdout}）")

        # judge（义务 10）：resource_contention × rerun 两字段区分「首次争抢（排除）」与「补跑后仍争抢（计入）」；
        # fix-r5 修 5：非争抢首登失败判 suspended（悬置）——glm/r14 即该形态
        run("register", str(led), "#2692/glm/r11", "--status", "ERROR", "--contention")
        run("register", str(led), "#2692/glm/r12", "--status", "ERROR", "--contention", "--rerun")
        run("register", str(led), "#2692/glm/r13", "--status", "PASS")
        run("register", str(led), "#2692/glm/r14", "--status", "ERROR")
        cp = run("judge", str(led))
        jlines = [ln for ln in cp.stdout.splitlines() if ln.startswith("#")]
        # 修 3（MM sibling#1 P2）：judge 与 count 同一键归并——每键恰一行，不再逐条输出同键多行；
        # ds/r11 三条（首登争抢 + 补跑争抢 + 重复补跑）归并成 contention_counted（计入类最严 + 补跑），
        # superseded 行不再出现
        if len(jlines) != len({ln.split(":", 1)[0] for ln in jlines}):
            fails.append(f"judge：按键归并后同键不得多行（{jlines}）")
        if any(ln.startswith("#2692/ds/r11:") and ("contention_first" in ln or "superseded" in ln) for ln in jlines):
            fails.append(f"judge：ds/r11 归并后不应再出现 contention_first / superseded 行（{jlines}）")
        for key, want in (("#2692/ds/r11", "contention_counted"), ("#2692/ds/r12", "available"),
                          ("#2692/ds/r13", "unavailable_other"), ("#2692/mm/r11", "unavailable_other"),
                          ("#1953/ds/r40", "unavailable_other"),
                          ("#2692/glm/r11", "contention_first"), ("#2692/glm/r12", "contention_counted"),
                          ("#2692/glm/r13", "available"), ("#2692/glm/r14", "suspended")):
            if not any(ln.startswith(key + ":") and want in ln for ln in jlines):
                fails.append(f"judge：{key} 应有分类 {want} 的行（输出：{cp.stdout}）")
        # 归并行标注补跑 / 首登（钉「·键归并取最严」标记本身，不与分类文案里的「补跑」词混淆）
        if not any(ln.startswith("#2692/ds/r11:") and "补跑·键归并" in ln for ln in jlines):
            fails.append(f"judge：ds/r11 归并行应标注补跑形态（{jlines}）")
        if not any(ln.startswith("#2692/glm/r11:") and "首登·键归并" in ln for ln in jlines):
            fails.append(f"judge：glm/r11 行应标注首登形态（{jlines}）")

        # 坏行 fail-closed：count / judge / register 遇无法解析的行一律 rc 2，绝不跳过
        bad = tmp / "bad.jsonl"
        bad.write_text('{"key": "#1/ds/r1", "status": "ERROR"}\nnot-json\n', encoding="utf-8")
        for cmd in ("count", "judge"):
            cp = run(cmd, str(bad))
            if cp.returncode != 2 or "fail-closed" not in cp.stderr:
                fails.append(f"{cmd}：坏行应 rc=2 fail-closed（rc={cp.returncode}、{cp.stderr.strip()[:80]}）")
        # register 坏行同样 fail-closed（#3056 fix-r1 item 7：不再裸 json.loads 崩栈，
        # traceback rc 1 会冒充「count 触发阈值」），且拒追加、台账不被改动
        cp = run("register", str(bad), "#1/ds/r2", "--status", "ERROR")
        if cp.returncode != 2 or "Traceback" in cp.stderr or "fail-closed" not in cp.stderr:
            fails.append(f"register：坏行应 rc=2 fail-closed 不崩栈（rc={cp.returncode}、{cp.stderr.strip()[:80]}）")
        if len(bad.read_text().splitlines()) != 2:
            fails.append("register：坏行时应拒追加（台账不被改动）")
        # 合法 JSON 但非对象 / 缺合法 key：同样 rc 2
        bad.write_text('{"key": "#1/ds/r1"}\n[1,2]\n', encoding="utf-8")
        cp = run("register", str(bad), "#1/ds/r2", "--status", "ERROR")
        if cp.returncode != 2 or "Traceback" in cp.stderr:
            fails.append(f"register：非对象行应 rc=2 不崩栈（rc={cp.returncode}、{cp.stderr.strip()[:80]}）")
        # 非 UTF-8 字节：register 与 count 都 rc 2（解码不留「replace」静默吞字节的口子）
        bad.write_bytes(b'{"key": "#1/ds/r1"}\n\xff\xfe\n')
        for args in (("count", str(bad)), ("register", str(bad), "#1/ds/r2", "--status", "ERROR")):
            cp = run(*args)
            if cp.returncode != 2 or "Traceback" in cp.stderr:
                fails.append(f"{args[0]}：非 UTF-8 台账应 rc=2 fail-closed（rc={cp.returncode}、{cp.stderr.strip()[:80]}）")
        bad.write_text('{"key": "no-sharp/ds/r1"}\n', encoding="utf-8")
        cp = run("count", str(bad))
        if cp.returncode != 2:
            fails.append(f"count：缺合法 key 的行应 rc=2（rc={cp.returncode}）")

        # 非法键：register 与 validate-key 都拒绝
        cp = run("register", str(led), "2692/ds/r11", "--status", "PASS")
        if cp.returncode != 2:
            fails.append(f"register：非法键应 rc=2（rc={cp.returncode}）")
        cp = run("validate-key", "#2692/ds/r0")
        if cp.returncode != 2:
            fails.append(f"validate-key：r0 应 rc=2（rc={cp.returncode}）")

        # 单写者（义务 9）：8 个进程并发登记同键，落盘行数=8、未 superseded 恰 1（flock + O_APPEND 不丢行不双计）
        led2 = tmp / "race" / "ledger.jsonl"
        procs = [subprocess.Popen([sys.executable, "-I", str(LEDGER), "register", str(led2), "#2692/ds/r11",
                                   "--status", "ERROR", "--contention"], stdout=subprocess.DEVNULL,
                                  stderr=subprocess.DEVNULL, env=dict(os.environ)) for _ in range(8)]
        for p in procs:
            if p.wait() != 0:
                fails.append("race：并发登记进程不应失败")
        lines = [json.loads(x) for x in led2.read_text().splitlines()]
        if len(lines) != 8:
            fails.append(f"race：8 路并发应落 8 行不丢行（实为 {len(lines)}）")
        if sum(1 for r in lines if not r.get("superseded")) != 1:
            fails.append(f"race：并发同键应恰 1 条计数（实为 {sum(1 for r in lines if not r.get('superseded'))}）")
        cp = run("count", str(led2))
        if cp.returncode != 0 or "#2692/ds" in cp.stdout:
            fails.append(f"race：并发同键均首次争抢，count 不计入（{cp.stdout}）")

        # 修 1（MM concurrency#1 P1）负控：count 按键归并——同键「首次失败 + 补跑失败」只计 1、
        # 不触发停手；同链×谱系两个不同键各一次失败才计 2、rc=1（fix-r5 修 5 后首登失败是悬置，
        # 「首登+补跑失败」归并取计入类最严 = 补跑失败 unavailable_other）
        led3 = tmp / "merge" / "ledger.jsonl"
        run("register", str(led3), "#7/ds/r1", "--status", "ERROR", "--note", "首登失败（悬置）")
        run("register", str(led3), "#7/ds/r1", "--status", "ERROR", "--rerun", "--note", "补跑仍失败")
        cp = run("count", str(led3))
        if cp.returncode != 0 or "#7/ds: 1" not in cp.stdout or "停手" in cp.stdout:
            fails.append(f"count：同键首登+补跑失败应按键归并只计 1、rc=0（rc={cp.returncode}、{cp.stdout}）")
        run("register", str(led3), "#7/ds/r2", "--status", "ERROR", "--rerun", "--note", "另一轮补跑仍失败")
        cp = run("count", str(led3))
        if cp.returncode != 1 or "#7/ds: 2" not in cp.stdout or "停手" not in cp.stdout:
            fails.append(f"count：不同键各一次计入类失败应计 2 触发停手 rc=1（rc={cp.returncode}、{cp.stdout}）")

        # 修 2（MM concurrency#1 P1 = DS boundary#3 + MM sibling#2 三撞）交错序列：同键
        # 「补跑后仍失败（unavailable_other，计入类）+ 后到补跑 PASS」→ 归并按计入类取最严，仍计 1——
        # 补跑翻绿不得抹掉已计入的失败（判据 5 停手信号漏报）。负控口径：归并若改回
        # 「补跑无条件优先」，本断言即红（count 变 0 行、#8/ds 无输出）。
        # fix-r5 修 5 形态适配：首登失败现在是悬置（不计入），翻绿不抹失败的不变量改钉在
        # 计入类（补跑后仍失败）形态上——不变量本身不变。
        led5 = tmp / "interleave" / "ledger.jsonl"
        run("register", str(led5), "#8/ds/r1", "--status", "ERROR", "--rerun", "--note", "补跑仍失败")
        run("register", str(led5), "#8/ds/r1", "--status", "PASS", "--rerun", "--note", "再补跑 PASS")
        cp = run("count", str(led5))
        if cp.returncode != 0 or "#8/ds: 1" not in cp.stdout:
            fails.append(f"count：同键补跑计入类失败 + 后到 PASS 归并后仍计 1（rc={cp.returncode}、{cp.stdout}）")
        cp = run("judge", str(led5))
        if not any(ln.startswith("#8/ds/r1:") and "unavailable_other" in ln and "补跑·键归并" in ln
                   for ln in cp.stdout.splitlines()):
            fails.append(f"judge：同键补跑失败 + 后到 PASS 应归并到 unavailable_other（补跑形态）：{cp.stdout}")
        # 对照组：首登是排除段（contention_first）+ 补跑 PASS → 同档内补跑优先，不计入
        led6 = tmp / "interleave2" / "ledger.jsonl"
        run("register", str(led6), "#8/mm/r1", "--status", "ERROR", "--contention", "--note", "首登争抢")
        run("register", str(led6), "#8/mm/r1", "--status", "PASS", "--rerun", "--note", "补跑成功")
        cp = run("count", str(led6))
        if cp.returncode != 0 or "#8/mm" in cp.stdout:
            fails.append(f"count：首登排除段 + 补跑 PASS 应不计入（rc={cp.returncode}、{cp.stdout}）")
        cp = run("judge", str(led6))
        if not any(ln.startswith("#8/mm/r1:") and "available" in ln and "补跑·键归并" in ln
                   for ln in cp.stdout.splitlines()):
            fails.append(f"judge：首登争抢 + 补跑 PASS 应归并到 available（补跑形态）：{cp.stdout}")

        # 修 2 写入侧（#3056 fix-r4，MM sibling#1 P1）：判重含类别与形态——同键同形态的后到记录，
        # 只有已有记录不轻于它（_merge_rank 同一序）才标 superseded。两向都要：
        # 向一：先 PASS 后同形态计入类失败 → 失败**不得**被 superseded 抹掉（停手信号不漏报）。
        # fix-r5 修 5 形态适配：计入类失败须带 --rerun（首登失败是悬置，本就计不进——不变量钉在计入类上）
        led10 = tmp / "wside1" / "ledger.jsonl"
        run("register", str(led10), "#11/ds/r1", "--status", "PASS", "--note", "先 PASS")
        cp = run("register", str(led10), "#11/ds/r1", "--status", "ERROR", "--rerun", "--note", "后到补跑失败")
        if cp.returncode != 0 or "superseded" in cp.stdout:
            fails.append(f"register：同形态先 PASS 后计入类失败，失败不应标 superseded（rc={cp.returncode}、{cp.stdout}）")
        wrecs = [json.loads(x) for x in led10.read_text().splitlines()]
        if len(wrecs) != 2 or wrecs[1].get("superseded") is not False:
            fails.append(f"register：先 PASS 后失败，失败行 superseded 应为 False（{[r.get('superseded') for r in wrecs]}）")
        cp = run("count", str(led10))
        if cp.returncode != 0 or "#11/ds: 1" not in cp.stdout:
            fails.append(f"count：先 PASS 后计入类失败，停手计数应见该失败（rc={cp.returncode}、{cp.stdout}）")
        # 向二：先计入类失败（补跑仍失败）后同形态 PASS → 后到 PASS 被更严的已有失败压回 superseded
        #（补跑翻绿不抹失败，r3 修 2 的归并口径落到写入侧；fix-r5 修 5 形态适配同向一）
        led11 = tmp / "wside2" / "ledger.jsonl"
        run("register", str(led11), "#11/mm/r1", "--status", "ERROR", "--rerun", "--note", "先补跑失败")
        cp = run("register", str(led11), "#11/mm/r1", "--status", "PASS", "--rerun", "--note", "后到 PASS")
        if cp.returncode != 0 or "superseded" not in cp.stdout:
            fails.append(f"register：同形态先失败后 PASS，PASS 应标 superseded（rc={cp.returncode}、{cp.stdout}）")
        cp = run("count", str(led11))
        if cp.returncode != 0 or "#11/mm: 1" not in cp.stdout:
            fails.append(f"count：先失败后同形态 PASS，计数仍为 1 不因翻绿清零（rc={cp.returncode}、{cp.stdout}）")

        # 修 5（fix-r5，sibling#1 P2）悬置态：非争抢首登失败 = suspended（不计入亦不排除）。
        # 负控口径：若 classify 不加悬置（旧码把首登非争抢失败判 unavailable_other 直接计入），
        # 下面「2 个悬置键 → rc 0」即红（旧码 rc=1 假触发停手）——卡面「最迟须在下一次
        # 派轮前补跑，逾期未补才计 1 次」的机器可读落地
        led13 = tmp / "susp" / "ledger.jsonl"
        run("register", str(led13), "#21/ds/r1", "--status", "ERROR", "--note", "首登失败未补跑")
        run("register", str(led13), "#21/ds/r2", "--status", "ERROR", "--note", "另一轮首登失败")
        cp = run("count", str(led13))
        if cp.returncode != 0 or "#21/ds" in cp.stdout or "悬置 2 键" not in cp.stdout or "停手" in cp.stdout:
            fails.append(f"count：2 个悬置键不得计入停手计数（rc={cp.returncode}、{cp.stdout}）")
        # fix-r10（DS r9 sibling#2 P2a）：suspended 支的分裂计数也要断言——只查「悬置 2 键」
        # 时变异 n_susp=0（恒打印首登失败 0）仍绿＝显示回归无判据
        if "首登失败未补跑 2" not in cp.stdout or "首次争抢未补跑 0" not in cp.stdout:
            fails.append(f"count：2 个 suspended 键应分列首登失败 2 / 首次争抢 0（{cp.stdout}）")
        cp = run("judge", str(led13))
        if not any(ln.startswith("#21/ds/r1:") and "suspended" in ln for ln in cp.stdout.splitlines()):
            fails.append(f"judge：首登非争抢失败应判 suspended（{cp.stdout}）")
        # fix-r9（DS r5 P2a）：contention_first（首次争抢排除段）与 suspended 同为「须补跑」的
        # 悬置形态——只含首次争抢的台账也要在 count 输出「待补跑」可见性（旧码只打印
        # suspended，首次争抢的补跑义务被藏进台账＝卡面判据 5 的运营面缺口）
        led13b = tmp / "cont-first" / "ledger.jsonl"
        run("register", str(led13b), "#31/zai/r7", "--status", "ERROR", "--contention", "--note", "首次 429")
        cp = run("count", str(led13b))
        if (cp.returncode != 0 or "#31/zai" in cp.stdout or "悬置 1 键" not in cp.stdout
                or "首次争抢未补跑 1" not in cp.stdout or "首登失败未补跑 0" not in cp.stdout):
            fails.append(f"count：只含 contention_first 时也应输出悬置可见性（rc={cp.returncode}、{cp.stdout}）")
        cp = run("judge", str(led13b))
        if not any(ln.startswith("#31/zai/r7:") and "contention_first" in ln for ln in cp.stdout.splitlines()):
            fails.append(f"judge：首次争抢应判 contention_first（{cp.stdout}）")
        # 悬置轮补跑 PASS → available（有效块）：round 结束即不悬置
        led14 = tmp / "susp2" / "ledger.jsonl"
        run("register", str(led14), "#22/mm/r1", "--status", "ERROR", "--note", "首登失败悬置")
        run("register", str(led14), "#22/mm/r1", "--status", "PASS", "--rerun", "--note", "补跑 PASS")
        cp = run("count", str(led14))
        if cp.returncode != 0 or "#22/mm" in cp.stdout:
            fails.append(f"count：悬置轮补跑 PASS 应判 available 不计入（rc={cp.returncode}、{cp.stdout}）")
        # 悬置轮补跑仍失败 → unavailable_other（计入）；同键两形态只计 1
        run("register", str(led14), "#22/mm/r2", "--status", "ERROR")
        run("register", str(led14), "#22/mm/r2", "--status", "ERROR", "--rerun")
        cp = run("count", str(led14))
        if cp.returncode != 0 or "#22/mm: 1" not in cp.stdout:
            fails.append(f"count：悬置轮补跑仍失败应计 1（rc={cp.returncode}、{cp.stdout}）")
        # 悬置轮逾期未补（--overdue）→ overdue_counted 计入；与另一计入类失败 → rc 1 停手
        run("register", str(led14), "#22/mm/r3", "--status", "ERROR", "--rerun", "--note", "补跑仍失败")
        cp = run("count", str(led14))
        if cp.returncode != 1 or "#22/mm: 2" not in cp.stdout or "停手" not in cp.stdout:
            fails.append(f"count：悬置+补跑失败+再一计入类 = 2 应 rc=1（rc={cp.returncode}、{cp.stdout}）")
        # 悬置 → 逾期升级：后到 --overdue 不得被判 dup 压回（升级必须落得下来），归并取计入类
        led15 = tmp / "overdue" / "ledger.jsonl"
        run("register", str(led15), "#23/glm/r1", "--status", "ERROR", "--note", "首登失败悬置")
        cp = run("register", str(led15), "#23/glm/r1", "--status", "ERROR", "--overdue", "--note", "逾期未补")
        if cp.returncode != 0 or "superseded" in cp.stdout:
            fails.append(f"register：悬置→逾期升级不应标 superseded（rc={cp.returncode}、{cp.stdout}）")
        cp = run("count", str(led15))
        if cp.returncode != 0 or "#23/glm: 1" not in cp.stdout:
            fails.append(f"count：逾期未补应计 1（rc={cp.returncode}、{cp.stdout}）")
        cp = run("judge", str(led15))
        if not any(ln.startswith("#23/glm/r1:") and "overdue_counted" in ln for ln in cp.stdout.splitlines()):
            fails.append(f"judge：逾期未补应判 overdue_counted（{cp.stdout}）")
        # 反向：已 overdue 的键被后到的悬置形态登记 → 判 dup 标 superseded（计入类不因后到被抹）
        cp = run("register", str(led15), "#23/glm/r1", "--status", "ERROR", "--note", "后到的悬置形态")
        if cp.returncode != 0 or "superseded" not in cp.stdout:
            fails.append(f"register：已 overdue 不得被悬置形态降级（rc={cp.returncode}、{cp.stdout}）")
        cp = run("count", str(led15))
        if cp.returncode != 0 or "#23/glm: 1" not in cp.stdout:
            fails.append(f"count：逾期计入不因后到悬置登记清零（rc={cp.returncode}、{cp.stdout}）")

        # 修 3.2（DS boundary#2 P2）：空 / 纯空白 / 不存在的台账路径 fail-closed rc 2——
        # 静默当空台账计会把「路径写错」混进「真的没有失败」，rc 1/0/2 三态就失真
        for cmd, badpath in (("count", ""), ("judge", ""), ("count", "   "), ("judge", "   ")):
            cp = run(cmd, badpath)
            if cp.returncode != 2 or "fail-closed" not in cp.stderr:
                fails.append(f"{cmd}：空/纯空白台账路径应 rc=2 fail-closed（rc={cp.returncode}、{cp.stderr.strip()[:60]}）")
        cp = run("count", str(tmp / "nope" / "nope.jsonl"))
        if cp.returncode != 2 or "不存在" not in cp.stderr:
            fails.append(f"count：不存在的台账应 rc=2 且文案含「不存在」（rc={cp.returncode}、{cp.stderr.strip()[:60]}）")
        cp = run("judge", str(tmp / "nope2.jsonl"))
        if cp.returncode != 2 or "不存在" not in cp.stderr:
            fails.append(f"judge：不存在的台账应 rc=2 且文案含「不存在」（rc={cp.returncode}、{cp.stderr.strip()[:60]}）")

        # 修 1（fix-r5，boundary#1 P1）四腿退出码契约：输出通道失败**不改写语义退出码**——
        # count 阈值已成立（计入类 ≥ 2 → 语义 rc 1）时，无论 stdout 管道运行期死（大输出 EPIPE）
        # 还是读端在写者起跑前就关（小输出首条 print 即 EPIPE，旧码收尾 flush 撞死管 → rc 120，
        # 实测留档 fix-r5 RESULT），rc 1 停手信号都必须原样送达。腿 1（正常）由上方全部用例覆盖。
        # 腿 2：大输出 + 运行期 EPIPE——20000 条链各 2 个计入键（count 输出 = 每链一行 ≈290 KiB，
        # 远超 64 KiB 管道缓冲，且阈值已成立 → 语义 rc 1）；读者读 3 行即关，写端必撞 EPIPE
        big = tmp / "bigpipe" / "ledger.jsonl"
        big.parent.mkdir(parents=True, exist_ok=True)
        with big.open("w", encoding="utf-8") as bf:
            for i in range(1, 20001):
                for r in ("r1", "r2"):
                    bf.write(json.dumps({"key": f"#{i}/ds/{r}", "ts": "t", "status": "ERROR",
                                         "resource_contention": False, "rerun": True, "overdue": False,
                                         "superseded": False, "note": ""}) + "\n")
        pp = subprocess.Popen([sys.executable, "-I", str(LEDGER), "count", str(big)],
                              stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
                              env=dict(os.environ))
        for _ in range(3):
            pp.stdout.readline()
        pp.stdout.close()
        pipe_rc = pp.wait()
        pipe_err = pp.stderr.read()
        if pipe_rc != 1 or "Traceback" in pipe_err or "stdout 已断" not in pipe_err:
            fails.append(f"修1-腿2：大输出管道早关应保住语义 rc=1 并告警一次（rc={pipe_rc}、{pipe_err.strip()[:80]}）")
        # 腿 3：小输出 + 读端在写者起跑前即关（Popen 后立刻关我们这一端；写者解释器启动需数十
        # 毫秒，首条 print(flush=True) 必撞 EPIPE）——旧码 rc 120 的原始复现形态
        small = tmp / "smallpipe" / "ledger.jsonl"
        small.parent.mkdir(parents=True, exist_ok=True)
        with small.open("w", encoding="utf-8") as sf:
            for k in ("#19/ds/r1", "#19/ds/r2"):
                sf.write(json.dumps({"key": k, "ts": "t", "status": "ERROR",
                                     "resource_contention": False, "rerun": True, "overdue": False,
                                     "superseded": False, "note": ""}) + "\n")
        ps = subprocess.Popen([sys.executable, "-I", str(LEDGER), "count", str(small)],
                              stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
                              env=dict(os.environ))
        ps.stdout.close()
        small_rc = ps.wait()
        small_err = ps.stderr.read()
        if small_rc != 1 or "Traceback" in small_err or "Exception ignored" in small_err:
            fails.append(f"修1-腿3：小输出读端早关应保住语义 rc=1（旧码 rc=120）（rc={small_rc}、"
                         f"{small_err.strip()[:80]}）")
        # 腿 4：stderr 启动即关（2>&-）——count 成功路径的 stdout 必须纯净（boundary#3 实测：
        # sys.stderr 为 None 时 print(file=None) 会落 stdout 污染机器可读通道），rc 语义原样
        cp = subprocess.run(["bash", "-c", 'exec "$0" -I "$1" count "$2" 2>&-',
                             sys.executable, str(LEDGER), str(small)],
                            capture_output=True, text=True, env=dict(os.environ))
        want_lines = ["#19/ds: 2  <-- 停手信号",
                      "fixround-ledger: 触发停手阈值（判据 5 通道兜底）：同链×谱系键归并后计入类失败累计 ≥ 2，停手转人拍板"]
        if cp.returncode != 1 or cp.stdout.splitlines() != want_lines or "Traceback" in cp.stderr:
            fails.append(f"修1-腿4：stderr 启动即关时 stdout 应纯净且 rc=1（rc={cp.returncode}、"
                         f"{cp.stdout!r}、{cp.stderr[:60]!r}）")
        # 修 3（fix-r5，boundary#3 P2）真实路径负控：stderr 启动即关 + **失败路径** register——
        # 旧负控用 makedirs 失败路径，rc 2 由错误路径给出、与 print 失败无关，没走到它声称的路径；
        # 本负控断言「本应进 stderr 的 fail-closed 文案在 stderr 死时被显式跳过、不落 stdout」
        blocker2 = tmp / "blocker2"
        blocker2.write_text("x", encoding="utf-8")
        cp = subprocess.run(["bash", "-c", 'exec "$0" -I "$1" register "$2" "$3" --status PASS 2>&-',
                             sys.executable, str(LEDGER), str(blocker2 / "l.jsonl"), "#1/ds/r1"],
                            capture_output=True, text=True, env=dict(os.environ))
        if cp.returncode != 2 or cp.stdout != "" or "Traceback" in cp.stderr:
            fails.append(f"修3：stderr 死时 fail-closed 文案应显式跳过、stdout 保持空（rc={cp.returncode}、"
                         f"stdout={cp.stdout[:60]!r}、stderr={cp.stderr[:60]!r}）")
        # 修 3 行为负控（进程内）：stderr 运行期死（write 即抛 OSError）——emit 捕获后接 devnull，
        # read_lines 的坏路径文案丢给死流、不崩栈、SystemExit(2) 照常（monkeypatch 后还原：
        # fd 2 会被 emit 接去 devnull，先 os.dup 备份原 stderr，测毕 dup2 还原——不能吃掉测试自己的 stderr）
        real_se = sys.stderr
        saved_fd2 = os.dup(2)

        class _DeadSE:
            def write(self, *a, **k):
                raise OSError("EBADF（模拟）")

            def flush(self):
                raise OSError("EBADF（模拟）")

        try:
            sys.stderr = _DeadSE()
            try:
                flg.read_lines(str(tmp / "nope3.jsonl"))
                fails.append("修3：stderr 运行期死时 read_lines 坏路径不应成功")
            except SystemExit as e:
                if e.code != 2:
                    fails.append(f"修3：stderr 运行期死时坏路径应 SystemExit(2)（code={e.code}）")
        finally:
            sys.stderr = real_se
            os.dup2(saved_fd2, 2)
            os.close(saved_fd2)

        # 修 5 负控：落盘失败 fail-closed（rc 2、干净文案不崩栈），短写/异常回滚不留半行
        blocker = tmp / "blocker"
        blocker.write_text("x", encoding="utf-8")
        cp = run("register", str(blocker / "led.jsonl"), "#1/ds/r1", "--status", "ERROR")
        if cp.returncode != 2 or "Traceback" in cp.stderr or "fail-closed" not in cp.stderr:
            fails.append(f"register：父目录是普通文件应 rc=2 干净拒跑（rc={cp.returncode}、{cp.stderr.strip()[:80]}）")
        adir = tmp / "adir"
        adir.mkdir()
        cp = run("register", str(adir), "#1/ds/r1", "--status", "ERROR")
        if cp.returncode != 2 or "Traceback" in cp.stderr or "fail-closed" not in cp.stderr:
            fails.append(f"register：台账路径是目录应 rc=2 干净拒跑（rc={cp.returncode}、{cp.stderr.strip()[:80]}）")
        # 写入中途 OSError：回滚到写前长度，半行不落盘（进程内直调 register，monkeypatch os.write）
        led4 = tmp / "short" / "ledger.jsonl"
        led4.parent.mkdir(parents=True, exist_ok=True)
        led4.write_text("", encoding="utf-8")
        real_write = os.write

        def _boom(fd: int, data: bytes) -> int:
            real_write(fd, data[:5])  # 先落 5 字节半行再炸，验证回滚
            raise OSError("ENOSPC（模拟）")

        try:
            flg.os.write = _boom
            rc = flg.register(str(led4), "#1/ds/r1", "ERROR", False, False, False, "")
        finally:
            flg.os.write = real_write
        if rc != 2 or led4.read_bytes() != b"":
            fails.append(f"register：写入中途 OSError 应 rc=2 且回滚半行（rc={rc}、剩 {led4.read_bytes()!r}）")

        # 短写不抛异常（每次只写 3 字节）：循环补写整行，count 仍能正常解析
        #（fix-r5 修 5：带 --rerun 使其计入，与断言「#2/ds: 1」对齐）
        try:
            flg.os.write = lambda fd, data: real_write(fd, data[:3])
            rc = flg.register(str(led4), "#2/ds/r1", "ERROR", False, True, False, "")
        finally:
            flg.os.write = real_write
        if rc != 0:
            fails.append(f"register：短写应循环补写成功（rc={rc}）")
        cp = run("count", str(led4))
        if cp.returncode != 0 or "#2/ds: 1" not in cp.stdout or "fail-closed" in cp.stderr:
            fails.append(f"count：短写补写后台账应是整行可解析（rc={cp.returncode}、{cp.stdout}、{cp.stderr.strip()[:60]}）")

        # 修 4（DS boundary#4）：register 的 flock / fstat OSError 收进干净文案 rc 2——
        # 留在 try 外会以 traceback rc 1 逃出，冒充「count 触发阈值」（rc 1 是停手专属信号）
        led7 = tmp / "oserr" / "ledger.jsonl"
        led7.parent.mkdir(parents=True, exist_ok=True)
        real_flock = flg.fcntl.flock

        def _flock_boom(fd: int, op: int) -> None:
            raise OSError("EWOULDBLOCK（模拟）")

        try:
            flg.fcntl.flock = _flock_boom
            rc = flg.register(str(led7), "#3/ds/r1", "ERROR", False, False, False, "")
        finally:
            flg.fcntl.flock = real_flock
        if rc != 2 or led7.exists() and led7.read_bytes():
            fails.append(f"register：flock OSError 应 rc=2 干净拒跑不落半行（rc={rc}、剩 {led7.read_bytes()!r}）")
        # 写前长度的 fstat 抬错（第 2 次调用：第 1 次是 register 持锁内读快照的 fstat）
        led8 = tmp / "oserr2" / "ledger.jsonl"
        led8.parent.mkdir(parents=True, exist_ok=True)
        real_fstat = os.fstat
        calls = {"n": 0}

        def _fstat_nth_boom(fd: int):
            calls["n"] += 1
            if calls["n"] >= 2:
                raise OSError("EIO（模拟）")
            return real_fstat(fd)

        try:
            flg.os.fstat = _fstat_nth_boom
            rc = flg.register(str(led8), "#3/ds/r2", "ERROR", False, False, False, "")
        finally:
            flg.os.fstat = real_fstat
        if rc != 2 or led8.read_bytes():
            fails.append(f"register：写前 fstat OSError 应 rc=2 不落半行（rc={rc}、剩 {led8.read_bytes()!r}）")

        # 修 5（MM concurrency#2）：count / judge 读路径共享锁——慢写者冒烟 + 不取锁的行为负控。
        # 真并发负控的构造：写者子进程持 LOCK_EX 写半行后睡 3 s 再补写整行；读者（count 子进程）
        # 必须在共享锁上等写者放锁再快照——不取锁会立刻读到残行，坏行 fail-closed 连坐 rc 2。
        # （#3056 fix-r4 修 3.6：删「读源码文本找 fcntl.flock/LOCK_SH」的静态断言——
        # 源码里写不写锁字样证明不了运行时真取锁，改由下面的不取锁行为负控钉住。）
        led9 = tmp / "readerlock" / "ledger.jsonl"
        led9.parent.mkdir(parents=True, exist_ok=True)
        ready = tmp / "half-done"
        slow = tmp / "slowwriter.py"
        slow.write_text(
            "import fcntl, os, sys, time\n"
            "fd = os.open(sys.argv[1], os.O_WRONLY | os.O_CREAT | os.O_APPEND, 0o644)\n"
            "fcntl.flock(fd, fcntl.LOCK_EX)\n"
            "os.write(fd, b'{\"key\": \"#9/ds/r1\", \"rerun\": true, \"sta')\n"
            "open(sys.argv[2], 'w').close()\n"
            "time.sleep(3)\n"
            "os.write(fd, b'tus\": \"ERROR\"}\\n')\n"
            "fcntl.flock(fd, fcntl.LOCK_UN)\n"
            "os.close(fd)\n", encoding="utf-8")
        pw = subprocess.Popen([sys.executable, "-I", str(slow), str(led9), str(ready)],
                              stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, env=dict(os.environ))
        try:
            for _ in range(200):
                if ready.exists():
                    break
                time.sleep(0.05)
            if not ready.exists():
                fails.append("修5：慢写者未就绪（ready 标记 10 s 内未出现）")
            else:
                cp = run("count", str(led9))
                if cp.returncode != 0 or "#9/ds: 1" not in cp.stdout:
                    fails.append(f"修5：写者持锁补写期间 count 应等锁读整行（rc={cp.returncode}、"
                                 f"{cp.stdout.strip()[:60]}、{cp.stderr.strip()[:60]}）")
                # 修 3.3（DS concurrency#1 + MM P3 同向）：等锁不静默——写者持锁期间读者
                # 抢不到 LOCK_SH，须先往 stderr 打一行「等待台账锁」再转阻塞
                #（对齐 check_outcome_parity.py:141-143 惯例，观测上区分「等锁」与「死锁」）
                if "等待台账锁" not in cp.stderr:
                    fails.append(f"修3.3：等锁应先往 stderr 打一行「等待台账锁」（{cp.stderr.strip()[:60]}）")
        finally:
            pw.wait(timeout=60)

        # 修 3.6 行为负控：猴补 flock 为 no-op（锁整体失效）后，慢写者持锁补写半行的窗口内
        # read_lines 会读到残行——坏行 fail-closed SystemExit(2) 且文案含「不是 JSON」。
        # （静态断言只能证明源码里有锁字样，证明不了运行时真取锁；此负控红 = 锁是唯一防线。）
        led12 = tmp / "noreadlock" / "ledger.jsonl"
        led12.parent.mkdir(parents=True, exist_ok=True)
        ready2 = tmp / "half-done2"
        slow2 = tmp / "slowwriter2.py"
        slow2.write_text(
            "import fcntl, os, sys, time\n"
            "fd = os.open(sys.argv[1], os.O_WRONLY | os.O_CREAT | os.O_APPEND, 0o644)\n"
            "fcntl.flock(fd, fcntl.LOCK_EX)\n"
            "os.write(fd, b'{\"key\": \"#12/ds/r1\", \"sta')\n"
            "open(sys.argv[2], 'w').close()\n"
            "time.sleep(3)\n"
            "os.write(fd, b'tus\": \"ERROR\"}\\n')\n"
            "fcntl.flock(fd, fcntl.LOCK_UN)\n"
            "os.close(fd)\n", encoding="utf-8")
        pw2 = subprocess.Popen([sys.executable, "-I", str(slow2), str(led12), str(ready2)],
                               stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, env=dict(os.environ))
        try:
            for _ in range(200):
                if ready2.exists():
                    break
                time.sleep(0.05)
            if not ready2.exists():
                fails.append("修3.6：慢写者未就绪（ready 标记 10 s 内未出现）")
            else:
                real_flock2 = flg.fcntl.flock
                _buf = io.StringIO()
                try:
                    flg.fcntl.flock = lambda fd, op: 0
                    try:
                        with contextlib.redirect_stderr(_buf):
                            flg.read_lines(str(led12))
                        fails.append("修3.6：不取锁读半行不应成功（read_lines 未抛）")
                    except SystemExit as e:
                        if e.code != 2 or "不是 JSON" not in _buf.getvalue():
                            fails.append(f"修3.6：不取锁读半行应 SystemExit(2) 且文案含「不是 JSON」"
                                         f"（code={e.code}、{_buf.getvalue()[:60]}）")
                finally:
                    flg.fcntl.flock = real_flock2
        finally:
            pw2.wait(timeout=60)

    for f in fails:
        print("FAIL", f)
    print(f"test-fixround-ledger: {'全部通过' if not fails else f'{len(fails)} 项失败'}")
    return 1 if fails else 0


if __name__ == "__main__":
    sys.exit(main())
