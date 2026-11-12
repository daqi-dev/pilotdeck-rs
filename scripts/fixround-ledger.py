#!/usr/bin/env python3
"""fixround-ledger.py — 长尾卡判据 5 的计数登记台账（#3056：登记载体、单写者、幂等键与机读分类）。

判据 5（state/decisions-20261110-longtail-closure-rule.md）的运营精度归本脚本：

  1. **幂等键**（义务 8）：登记键 = 链 × 谱系 × 轮号，形如 `#2692/ds/r11`（issue 号 / 谱系 / fix-rN 的 N）。
     同键重复登记**先登记者为准**：后到的登记仍写入（供审计）但标 `superseded=true`，计数不计。
  2. **单写者与原子累加**（义务 9）：flock + O_APPEND 追加写（board.md:791 跨进程丢行改 O_APPEND 的先例），
     读-判重-写在同一把锁内完成，多进程并发登记不丢行。
  3. **计数**：`count` 按链×谱系累计未 superseded 的登记条数；任一链×谱系 ≥ 2 时退出码 1
     （判据 5 停手阈值的机读信号，人据此停修转结构性/转人，本脚本不停任何东西）。
  4. **争抢二义收敛**（义务 10，MM 建议）：资源争抢类排除**仅覆盖首次**；补跑后仍无有效块即按前提③计入。
     落地：登记行带 `resource_contention`（run-review-roles.py 的 rec 字段，SlotTimeout / 429 配额 /
     执行级 TIMEOUT 为 true）与 `rerun`（--rerun 补跑标记）两个机读字段，`judge` 据此分类：
       - contention_first：首次争抢（排除段，不计入，须补跑）
       - contention_counted：补跑后仍争抢（前提③，计入）
       - available：非争抢且 PASS（有效块）
       - unavailable_other：非争抢且非 PASS（看 detail 人判，不自动排除也不计入争抢类）

登记行是单行 JSON（键固定：key/ts/status/resource_contention/rerun/superseded/note），坏行 fail-closed
（count / judge 遇无法解析的行退出码 2，绝不跳过——跳过会把丢数据吞成计数偏低）。

用法：
  python3 -I scripts/fixround-ledger.py validate-key '#2692/ds/r11'
  python3 -I scripts/fixround-ledger.py register <台账文件> '#2692/ds/r11' --status ERROR --contention [--rerun] [--note …]
  python3 -I scripts/fixround-ledger.py count <台账文件>
  python3 -I scripts/fixround-ledger.py judge <台账文件>
退出码：0 正常 · 1 count 触发阈值（≥2）· 2 输入/格式错误
"""
from __future__ import annotations

import argparse
import datetime as dt
import fcntl
import json
import os
import re
import sys

# 幂等键：链（#<issue>）× 谱系（小写字母开头的字母数字连字符）× 轮号（r 后正整数，r0 不是轮）
KEY_RE = re.compile(r"^#(\d+)/([a-z][a-z0-9-]*)/r([1-9]\d*)$")
COUNT_THRESHOLD = 2  # 判据 5：同链×谱系累计第 2 次即停手信号


def key_ok(key: str) -> bool:
    return bool(KEY_RE.fullmatch(key))


def _chain_lineage(key: str) -> tuple[str, str]:
    m = KEY_RE.fullmatch(key)
    assert m is not None
    return m.group(1), m.group(2)


def read_lines(path: str) -> list[dict]:
    """读台账；坏行（非 JSON / 缺 key / 键非法）fail-closed 退出码 2（跳过会把丢数据吞成计数偏低）。"""
    out: list[dict] = []
    if not os.path.exists(path):
        return out
    with open(path, encoding="utf-8") as f:
        for n, line in enumerate(f, 1):
            line = line.strip()
            if not line:
                continue
            try:
                rec = json.loads(line)
            except ValueError:
                print(f"fixround-ledger: {path}:{n} 不是 JSON 行（fail-closed，不跳过）：{line[:120]}", file=sys.stderr)
                raise SystemExit(2)
            if not isinstance(rec, dict) or not isinstance(rec.get("key"), str) or not key_ok(rec["key"]):
                print(f"fixround-ledger: {path}:{n} 登记行缺合法 key（fail-closed，不跳过）：{line[:120]}", file=sys.stderr)
                raise SystemExit(2)
            out.append(rec)
    return out


def register(path: str, key: str, status: str, contention: bool, rerun: bool, note: str) -> int:
    if not key_ok(key):
        print(f"fixround-ledger: 登记键非法：{key}（形如 #2692/ds/r11，见 --help）", file=sys.stderr)
        return 2
    os.makedirs(os.path.dirname(os.path.abspath(path)), exist_ok=True)
    # 单写者：flock 排他 + O_APPEND 追加（读-判重-写都在锁内；board.md:791 先例）
    fd = os.open(path, os.O_RDWR | os.O_CREAT | os.O_APPEND, 0o644)
    try:
        fcntl.flock(fd, fcntl.LOCK_EX)
        existing = os.pread(fd, os.fstat(fd).st_size or 1, 0).decode("utf-8", "replace") if os.fstat(fd).st_size else ""
        dup = any(ln.strip() and json.loads(ln).get("key") == key for ln in existing.split("\n") if ln.strip())
        # 后到重复：仍写入（审计可见）但标 superseded，count 不计——先登记者为准
        rec = {"key": key, "ts": dt.datetime.now().isoformat(timespec="seconds"), "status": status,
               "resource_contention": contention, "rerun": rerun, "superseded": dup,
               "note": note}
        os.write(fd, (json.dumps(rec, ensure_ascii=False) + "\n").encode("utf-8"))
        tag = "（重复键，标 superseded 不计数——先登记者为准）" if dup else ""
        print(f"fixround-ledger: 登记 {key} status={status} contention={contention} rerun={rerun}{tag}")
        return 0
    finally:
        fcntl.flock(fd, fcntl.LOCK_UN)
        os.close(fd)


def count(path: str) -> int:
    recs = read_lines(path)
    tally: dict[tuple[str, str], int] = {}
    for rec in recs:
        if rec.get("superseded"):
            continue
        tally[_chain_lineage(rec["key"])] = tally.get(_chain_lineage(rec["key"]), 0) + 1
    rc = 0
    for (chain, lineage), n in sorted(tally.items()):
        flag = "  <-- 停手信号" if n >= COUNT_THRESHOLD else ""
        print(f"#{chain}/{lineage}: {n}{flag}")
        if n >= COUNT_THRESHOLD:
            rc = 1
    print(f"fixround-ledger: {'触发停手阈值（判据 5）：同链×谱系累计 ≥ 2' if rc else '未触发阈值'}")
    return rc


def judge(path: str) -> int:
    recs = read_lines(path)
    for rec in recs:
        key = rec["key"]
        if rec.get("superseded"):
            cls = "superseded（重复登记，不计）"
        elif rec.get("resource_contention") and not rec.get("rerun"):
            cls = "contention_first（首次争抢：排除段不计入，须补跑）"
        elif rec.get("resource_contention") and rec.get("rerun"):
            cls = "contention_counted（补跑后仍争抢：按前提③计入）"
        elif rec.get("status") == "PASS":
            cls = "available（有效块）"
        else:
            cls = "unavailable_other（非争抢失败：看 detail 人判）"
        print(f"{key}: {cls}")
    return 0


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    vk = sub.add_parser("validate-key", help="校验幂等键格式")
    vk.add_argument("key")
    rg = sub.add_parser("register", help="登记一轮（flock + O_APPEND；同键后到标 superseded）")
    rg.add_argument("path")
    rg.add_argument("key")
    rg.add_argument("--status", default="ERROR", help="该轮评审状态（run-review-roles 的 summary 判定，如 PASS/ERROR/TIMEOUT）")
    rg.add_argument("--contention", action="store_true", help="该轮属资源争抢（SlotTimeout / 429 配额 / 执行级 TIMEOUT）")
    rg.add_argument("--rerun", action="store_true", help="该轮是补跑（--rerun；首次争抢补跑后仍争抢按前提③计入）")
    rg.add_argument("--note", default="", help="备注（跟踪单评论链接等）")
    ct = sub.add_parser("count", help="按链×谱系累计（≥2 退出码 1，判据 5 停手信号）")
    ct.add_argument("path")
    jd = sub.add_parser("judge", help="逐条分类：available / contention_first / contention_counted / unavailable_other")
    jd.add_argument("path")
    a = ap.parse_args()
    if a.cmd == "validate-key":
        if key_ok(a.key):
            print(f"fixround-ledger: {a.key} 合法")
            return 0
        print(f"fixround-ledger: {a.key} 非法（形如 #2692/ds/r11）", file=sys.stderr)
        return 2
    if a.cmd == "register":
        return register(a.path, a.key, a.status, a.contention, a.rerun, a.note)
    if a.cmd == "count":
        return count(a.path)
    return judge(a.path)


if __name__ == "__main__":
    sys.exit(main())
