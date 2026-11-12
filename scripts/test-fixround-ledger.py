#!/usr/bin/env python3
"""test-fixround-ledger.py — fixround-ledger.py 的配对测试（#3056 义务 8-10：幂等键 / 单写者 / 计数 / 机读分类）。

用法：python3 -I scripts/test-fixround-ledger.py   退出码 0 全过 · 1 有失败
"""
from __future__ import annotations

import importlib.util
import inspect
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

        # PASS（available）不是失败，不推阈值；第二次计入类失败 → rc 1（判据 5 停手信号）
        cp = run("register", str(led), "#2692/ds/r12", "--status", "PASS")
        if cp.returncode != 0:
            fails.append(f"register：新轮号应正常登记（rc={cp.returncode}）")
        cp = run("count", str(led))
        if cp.returncode != 0 or "#2692/ds: 1" not in cp.stdout:
            fails.append(f"count：PASS（available）不计入停手计数（rc={cp.returncode}、{cp.stdout}）")
        run("register", str(led), "#2692/ds/r13", "--status", "ERROR")
        cp = run("count", str(led))
        if cp.returncode != 1 or "#2692/ds: 2" not in cp.stdout or "停手" not in cp.stdout:
            fails.append(f"count：同链×谱系计入类失败累计 2 应 rc=1 并给停手信号（rc={cp.returncode}、{cp.stdout}）")
        # 不同链 / 不同谱系分开计（各 1 条计入类失败，互不触发）
        run("register", str(led), "#2692/mm/r11", "--status", "ERROR")
        run("register", str(led), "#1953/ds/r40", "--status", "ERROR")
        cp = run("count", str(led))
        if cp.returncode != 1 or "#1953/ds: 1" not in cp.stdout or "#2692/mm: 1" not in cp.stdout:
            fails.append(f"count：链×谱系应各自累计（{cp.stdout}）")

        # judge（义务 10）：resource_contention × rerun 两字段区分「首次争抢（排除）」与「补跑后仍争抢（计入）」
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
                          ("#2692/glm/r13", "available"), ("#2692/glm/r14", "unavailable_other")):
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
        # 不触发停手；同链×谱系两个不同键各一次失败才计 2、rc=1
        led3 = tmp / "merge" / "ledger.jsonl"
        run("register", str(led3), "#7/ds/r1", "--status", "ERROR", "--note", "首登失败")
        run("register", str(led3), "#7/ds/r1", "--status", "ERROR", "--rerun", "--note", "补跑仍失败")
        cp = run("count", str(led3))
        if cp.returncode != 0 or "#7/ds: 1" not in cp.stdout or "停手" in cp.stdout:
            fails.append(f"count：同键首登+补跑失败应按键归并只计 1、rc=0（rc={cp.returncode}、{cp.stdout}）")
        run("register", str(led3), "#7/ds/r2", "--status", "ERROR", "--note", "另一轮失败")
        cp = run("count", str(led3))
        if cp.returncode != 1 or "#7/ds: 2" not in cp.stdout or "停手" not in cp.stdout:
            fails.append(f"count：不同键各一次失败应计 2 触发停手 rc=1（rc={cp.returncode}、{cp.stdout}）")

        # 修 2（MM concurrency#1 P1 = DS boundary#3 + MM sibling#2 三撞）交错序列：同键
        # 「首登 unavailable_other（计入类失败）+ 补跑 PASS」→ 归并按计入类取最严，仍计 1——
        # 补跑翻绿不得抹掉已计入的失败（判据 5 停手信号漏报）。负控口径：归并若改回
        # 「补跑无条件优先」，本断言即红（count 变 0 行、#8/ds 无输出）。
        led5 = tmp / "interleave" / "ledger.jsonl"
        run("register", str(led5), "#8/ds/r1", "--status", "ERROR", "--note", "首登失败")
        run("register", str(led5), "#8/ds/r1", "--status", "PASS", "--rerun", "--note", "补跑 PASS")
        cp = run("count", str(led5))
        if cp.returncode != 0 or "#8/ds: 1" not in cp.stdout:
            fails.append(f"count：同键首登计入类失败 + 补跑 PASS 归并后仍计 1（rc={cp.returncode}、{cp.stdout}）")
        cp = run("judge", str(led5))
        if not any(ln.startswith("#8/ds/r1:") and "unavailable_other" in ln and "首登·键归并" in ln
                   for ln in cp.stdout.splitlines()):
            fails.append(f"judge：同键首登失败 + 补跑 PASS 应归并到 unavailable_other（首登形态）：{cp.stdout}")
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
            rc = flg.register(str(led4), "#1/ds/r1", "ERROR", False, False, "")
        finally:
            flg.os.write = real_write
        if rc != 2 or led4.read_bytes() != b"":
            fails.append(f"register：写入中途 OSError 应 rc=2 且回滚半行（rc={rc}、剩 {led4.read_bytes()!r}）")

        # 短写不抛异常（每次只写 3 字节）：循环补写整行，count 仍能正常解析
        try:
            flg.os.write = lambda fd, data: real_write(fd, data[:3])
            rc = flg.register(str(led4), "#2/ds/r1", "ERROR", False, False, "")
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
            rc = flg.register(str(led7), "#3/ds/r1", "ERROR", False, False, "")
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
            rc = flg.register(str(led8), "#3/ds/r2", "ERROR", False, False, "")
        finally:
            flg.os.fstat = real_fstat
        if rc != 2 or led8.read_bytes():
            fails.append(f"register：写前 fstat OSError 应 rc=2 不落半行（rc={rc}、剩 {led8.read_bytes()!r}）")

        # 修 5（MM concurrency#2）：count / judge 读路径共享锁——静态断言 + 慢写者冒烟。
        # 真并发负控的构造：写者子进程持 LOCK_EX 写半行后睡 3 s 再补写整行；读者（count 子进程）
        # 必须在共享锁上等写者放锁再快照——不取锁会立刻读到残行，坏行 fail-closed 连坐 rc 2。
        rsrc = inspect.getsource(flg.read_lines)
        if "fcntl.flock" not in rsrc or "LOCK_SH" not in rsrc:
            fails.append("修5：read_lines 读路径须取共享锁（LOCK_SH）与写者互斥")
        led9 = tmp / "readerlock" / "ledger.jsonl"
        led9.parent.mkdir(parents=True, exist_ok=True)
        ready = tmp / "half-done"
        slow = tmp / "slowwriter.py"
        slow.write_text(
            "import fcntl, os, sys, time\n"
            "fd = os.open(sys.argv[1], os.O_WRONLY | os.O_CREAT | os.O_APPEND, 0o644)\n"
            "fcntl.flock(fd, fcntl.LOCK_EX)\n"
            "os.write(fd, b'{\"key\": \"#9/ds/r1\", \"sta')\n"
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
        finally:
            pw.wait(timeout=60)

    for f in fails:
        print("FAIL", f)
    print(f"test-fixround-ledger: {'全部通过' if not fails else f'{len(fails)} 项失败'}")
    return 1 if fails else 0


if __name__ == "__main__":
    sys.exit(main())
