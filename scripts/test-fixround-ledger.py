#!/usr/bin/env python3
"""test-fixround-ledger.py — fixround-ledger.py 的配对测试（#3056 义务 8-10：幂等键 / 单写者 / 计数 / 机读分类）。

用法：python3 -I scripts/test-fixround-ledger.py   退出码 0 全过 · 1 有失败
"""
from __future__ import annotations

import importlib.util
import json
import os
import subprocess
import sys
import tempfile
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
        for key, want in (("#2692/ds/r11", "contention_first"), ("#2692/ds/r11", "contention_counted"),
                          ("#2692/ds/r11", "superseded"), ("#2692/ds/r12", "available"),
                          ("#2692/ds/r13", "unavailable_other"),
                          ("#2692/glm/r11", "contention_first"), ("#2692/glm/r12", "contention_counted"),
                          ("#2692/glm/r13", "available"), ("#2692/glm/r14", "unavailable_other")):
            if not any(ln.startswith(key + ":") and want in ln for ln in cp.stdout.splitlines()):
                fails.append(f"judge：{key} 应有分类 {want} 的行（输出：{cp.stdout}）")

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

    for f in fails:
        print("FAIL", f)
    print(f"test-fixround-ledger: {'全部通过' if not fails else f'{len(fails)} 项失败'}")
    return 1 if fails else 0


if __name__ == "__main__":
    sys.exit(main())
