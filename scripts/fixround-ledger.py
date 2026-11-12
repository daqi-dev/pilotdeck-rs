#!/usr/bin/env python3
"""fixround-ledger.py — 长尾卡判据 5 的计数登记台账（#3056：登记载体、单写者、幂等键与机读分类）。

判据 5（state/decisions-20261110-longtail-closure-rule.md）的运营精度归本脚本：

  1. **幂等键**（义务 8）：登记键 = 链 × 谱系 × 轮号，形如 `#2692/ds/r11`（issue 号 / 谱系 / fix-rN 的 N）。
     同键**同形态**（都首次 / 都补跑）的后到登记，已有同形态记录**不轻于它**（classify 同一序）才标
     `superseded=true`、计数不计（后到登记仍写入供审计）；**计入类失败不因后到被抹**——同键
     「先 PASS 后计入类失败」失败必须保留可见（写入侧与 _merged_by_key 归并同源，#3056 fix-r4
     修 2，MM sibling#1 P1：判据 5 停手信号不漏报）；同键补跑（`--rerun`）是新一轮尝试，参与判重、
     不盲标 superseded——否则补跑后仍争抢的 `contention_counted` 永不可达（#3056 fix-r1 MM concurrency#1）。
  2. **单写者与原子累加**（义务 9）：flock + O_APPEND 追加写（board.md:791 跨进程丢行改 O_APPEND 的先例），
     读-判重-写在同一把锁内完成，快照直接读已持锁的 fd（#3056 fix-r4 修 3.4，MM concurrency#1 P2：
     不再另开第二个 `O_CREAT` fd——外部 rm/mv 换路径时会新建空 inode，登记行写进已 unlink 的
     旧 inode 而彻底消失），多进程并发登记不丢行；`count` / `judge` 的读路径同样取共享锁
     （LOCK_SH）快照——不锁会在写者短写循环补写半行的窗口读到残行，坏行 fail-closed 把正常
     并发读误报成台账损坏（#3056 fix-r3 修 5，MM concurrency#2）；等锁不静默（LOCK_NB 试探、
     抢不到先往 stderr 打一行「等待台账锁」再转阻塞，对齐 check_outcome_parity.py:141-143 惯例，
     #3056 fix-r4 修 3.3）。
  3. **计数（按键归并）**：`count` 与 `judge` 同一分类（classify 单一实现）且**同一归并**
     （_merged_by_key 单一实现，每键一行/一次）。计数单位是**键**（= 链×谱系×轮号）：先按键归并
     ——每个键只贡献一次，**取该键最严形态：计入类失败（contention_counted / unavailable_other）
     优先于 available，同档内补跑优先于首登**（补跑 PASS 抹不掉同键已计入的失败——判据 5 的
     停手信号不因补跑翻绿而漏报，#3056 fix-r3 修 2，MM concurrency#1 P1 = DS boundary#3 =
     MM sibling#2 三撞），再按链×谱系汇总；同轮「首登失败 + 补跑失败」两条只计 1，防单轮
     顶满阈值的假停手（#3056 fix-r2 MM concurrency#1 P1）。计入停手阈值的失败类 =
     contention_counted（补跑后仍争抢，前提③）+ unavailable_other（非争抢失败）；不计入 =
     contention_first（首次争抢排除段，须补跑）、superseded（重复登记）、available（PASS 有效块——
     PASS 不是失败）。任一链×谱系键归并后计入类 ≥ 2 时退出码 1（判据 5 停手阈值的机读信号，
     人据此停修转结构性/转人，本脚本不停任何东西）。
  4. **争抢二义收敛**（义务 10，MM 建议）：资源争抢类排除**仅覆盖首次**；补跑后仍无有效块即按前提③计入。
     落地：登记行带 `resource_contention`（run-review-roles.py 的 rec 字段，SlotTimeout / 429 配额 /
     执行级 TIMEOUT 为 true）与 `rerun`（--rerun 补跑标记）两个机读字段，`judge` 按键归并后逐键分类
     （归并规则同上；行内标注归并记录是补跑还是首登）：
       - superseded：同键同形态重复登记（不计，归并时弃）
       - contention_first：首次争抢（排除段，不计入，须补跑）
       - contention_counted：补跑后仍争抢（前提③，计入）
       - available：非争抢且 PASS（有效块，不计入——PASS 不是失败）
       - unavailable_other：非争抢且非 PASS（看 detail 人判，不自动排除，计入）

登记行是单行 JSON（键固定：key/ts/status/resource_contention/rerun/superseded/note），坏行 fail-closed
（register / count / judge 遇无法解析或非 UTF-8 的行一律退出码 2，绝不跳过——跳过会把丢数据吞成计数
偏低；register 侧同口径，不再裸 json.loads 崩栈用 rc 1 冒充「触发阈值」）。空 / 纯空白 / 不存在的
台账路径同样 fail-closed 退出码 2，不静默当空台账计（#3056 fix-r4 修 3.2，DS boundary#2 P2：
判据 5 的停手信号靠 rc 区分三态，「路径写错」不得混进「真的没有失败」）。告警与输出自身的
print 失败（stdout 管道早关 → BrokenPipeError、stderr 关闭 → EBADF）也收成干净退出码 2
（#3056 fix-r4 修 3.1，MM sibling#2 P2）——rc 1 只留给判据 5 停手信号，不以 traceback 冒充。

用法：
  python3 -I scripts/fixround-ledger.py validate-key '#2692/ds/r11'
  python3 -I scripts/fixround-ledger.py register <台账文件> '#2692/ds/r11' --status ERROR --contention [--rerun] [--note …]
  python3 -I scripts/fixround-ledger.py count <台账文件>
  python3 -I scripts/fixround-ledger.py judge <台账文件>
退出码：0 正常 · 1 count 触发阈值（≥2）· 2 输入/格式错误（含 print 路径 OSError）
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


def _parse_records(path: str, text: str) -> list[dict]:
    """逐行解析台账文本；坏行（非 JSON / 非对象 / 缺 key / 键非法）fail-closed 退出码 2
    （跳过会把丢数据吞成计数偏低）。read_lines 与 register 共用——register 侧同口径，
    不再裸 json.loads 崩栈（traceback rc 1 会冒充「count 触发阈值」）。"""
    out: list[dict] = []
    for n, line in enumerate(text.split("\n"), 1):
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


def read_lines(path: str) -> list[dict]:
    """count / judge 的读路径：先取共享锁（LOCK_SH）再快照（#3056 fix-r3 修 5，MM concurrency#2）——
    写者是 register 的短写循环（flock LOCK_EX），读者不锁会在补写半行的窗口读到残行，
    坏行 fail-closed 把正常并发读误报成台账损坏。锁内快照读完即释放；读/锁/解码失败
    fail-closed 退出码 2（不跳过）。空 / 纯空白 / 不存在的路径同样 fail-closed rc 2（fix-r4
    修 3.2，DS boundary#2 P2）——静默当空台账计会把「路径写错」混进「真的没有失败」，
    判据 5 的停手信号靠 rc 区分三态，宁可显式。"""
    if not path or not path.strip():
        print(f"fixround-ledger: 台账路径为空或纯空白（fail-closed，不静默当空台账计）：{path!r}", file=sys.stderr)
        raise SystemExit(2)
    try:
        fd = os.open(path, os.O_RDONLY)
    except FileNotFoundError:
        print(f"fixround-ledger: 台账不存在（fail-closed，不静默当空台账计）：{path}", file=sys.stderr)
        raise SystemExit(2)
    except OSError as e:
        print(f"fixround-ledger: {path} 读不了（fail-closed）：{e}", file=sys.stderr)
        raise SystemExit(2)
    try:
        # 等锁不静默（#3056 fix-r4 修 3.3，DS concurrency#1 + MM P3 同向）：先 LOCK_NB 试探，
        # 抢不到先往 stderr 打一行「等待台账锁」再转阻塞——对齐 scripts/retirement-outcome-parity/
        # check_outcome_parity.py:141-143「等锁期间在 stderr 打一行提示，观测上不静默」的仓内惯例；
        # 写者（register 的 LOCK_EX）被 SIGSTOP / 大台账慢写卡住时，观测上可区分「等锁」与「死锁」
        try:
            fcntl.flock(fd, fcntl.LOCK_SH | fcntl.LOCK_NB)
        except OSError:
            print(f"fixround-ledger: 等待台账锁：{path}", file=sys.stderr)
            fcntl.flock(fd, fcntl.LOCK_SH)
        size = os.fstat(fd).st_size
        text = (os.pread(fd, size, 0) if size else b"").decode("utf-8")
    except UnicodeDecodeError as e:
        print(f"fixround-ledger: {path} 不是 UTF-8（fail-closed）：{e}", file=sys.stderr)
        raise SystemExit(2)
    except OSError as e:
        print(f"fixround-ledger: {path} 读不了（fail-closed）：{e}", file=sys.stderr)
        raise SystemExit(2)
    finally:
        # 解锁/关闭失败不改变已定的判定（快照已在手；进程退出即释放锁），只告警，不以 traceback 逃出
        for _fn, _what in ((lambda: fcntl.flock(fd, fcntl.LOCK_UN), "解锁"),
                           (lambda: os.close(fd), "关闭")):
            try:
                _fn()
            except OSError as e:
                print(f"fixround-ledger: {path} {_what}失败（不影响已定结果）：{e}", file=sys.stderr)
    return _parse_records(path, text)


def classify(rec: dict) -> str:
    """count 与 judge 共用的机读分类（单一实现——两命令口径由同一函数保证，不再各说各话）。"""
    if rec.get("superseded"):
        return "superseded"
    if rec.get("resource_contention") and not rec.get("rerun"):
        return "contention_first"
    if rec.get("resource_contention") and rec.get("rerun"):
        return "contention_counted"
    if rec.get("status") == "PASS":
        return "available"
    return "unavailable_other"


CLASS_LABEL = {
    "superseded": "superseded（同键同形态重复登记，不计）",
    "contention_first": "contention_first（首次争抢：排除段不计入，须补跑）",
    "contention_counted": "contention_counted（补跑后仍争抢：按前提③计入）",
    "available": "available（有效块：PASS 不是失败，不计入停手计数）",
    "unavailable_other": "unavailable_other（非争抢失败：看 detail 人判，计入）",
}
# 计入判据 5 停手阈值的失败类；contention_first（首次争抢排除段）/ superseded（重复登记）/
# available（PASS 有效块）不计——count 与 judge 共用 classify，口径只有这一份
COUNTED_CLASSES = ("contention_counted", "unavailable_other")


def register(path: str, key: str, status: str, contention: bool, rerun: bool, note: str) -> int:
    if not key_ok(key):
        print(f"fixround-ledger: 登记键非法：{key}（形如 #2692/ds/r11，见 --help）", file=sys.stderr)
        return 2
    # 单写者：flock 排他 + O_APPEND 追加（读-判重-写都在锁内；board.md:791 先例）。
    # makedirs/os.open 也走 fail-closed（#3056 fix-r2 修 5）：父目录是普通文件 / 路径是目录等
    # OSError 一律干净文案拒跑（rc 2），不让 traceback 冒充「count 触发阈值」（rc 1 专属停手信号）
    try:
        os.makedirs(os.path.dirname(os.path.abspath(path)), exist_ok=True)
        fd = os.open(path, os.O_RDWR | os.O_CREAT | os.O_APPEND, 0o644)
    except OSError as e:
        print(f"fixround-ledger: {path} 打不开（fail-closed）：{e}", file=sys.stderr)
        return 2
    # 取锁的 OSError 同样走干净文案 rc 2（#3056 fix-r3 修 4，DS boundary#4）：留在 try 外会以
    # traceback rc 1 逃出，冒充「count 触发阈值」（rc 1 是判据 5 停手的专属信号）
    try:
        fcntl.flock(fd, fcntl.LOCK_EX)
    except OSError as e:
        print(f"fixround-ledger: {path} 加锁失败（fail-closed）：{e}", file=sys.stderr)
        try:
            os.close(fd)
        except OSError:
            pass
        return 2
    try:
        # 快照直接用已持锁的 fd（os.pread 按显式偏移读，O_APPEND 只影响写）——不再另开第二个
        # `O_CREAT` fd 读快照（#3056 fix-r4 修 3.4，MM concurrency#1 P2）：外部 rm/mv 换掉台账
        # 路径时第二个 fd 会新建空 inode，登记行写进已 unlink 的旧 inode、在可见台账里彻底消失
        # （丢数据 + 停手信号漏报）；顺带消掉「持锁期间另开并关闭同文件第二个 fd」的跨平台
        # flock 语义依赖（同一进程两个 fd 抢 flock 会自锁死的旧顾虑一并不复存在）。
        # UTF-8 严格解码：decode('replace') 会把坏字节静默吞成计数偏低，这里宁可拒读（rc 2）。
        try:
            size = os.fstat(fd).st_size
            text = (os.pread(fd, size, 0) if size else b"").decode("utf-8")
        except UnicodeDecodeError as e:
            print(f"fixround-ledger: {path} 不是 UTF-8（fail-closed）：{e}", file=sys.stderr)
            return 2
        except OSError as e:
            print(f"fixround-ledger: {path} 读不了（fail-closed）：{e}", file=sys.stderr)
            return 2
        existing = _parse_records(path, text)
        # 判重含形态与类别（#3056 fix-r4 修 2，MM sibling#1 P1）：同键**同形态**（都首次 / 都补跑）的
        # 后到记录，只有当已有同形态记录**不轻于它**（_merge_rank 同一序——与归并侧同源，
        # classify 单一实现）才标 superseded。计入类失败（contention_counted / unavailable_other）
        # 不因后到被抹——写入侧与 _merged_by_key「取该键最严」不变量对齐，同键「先 PASS 后
        # 计入类失败」失败保留可见（判据 5 停手信号不漏报）；「先失败后同形态 PASS」仍被更严的
        # 已有失败压回 superseded（补跑翻绿不抹失败，r3 修 2 口径落到写入侧）。同键补跑是新一轮
        # 尝试，参与判重、不盲标 superseded——盲标会把 contention_counted（补跑后仍争抢）
        # 永远压回 superseded，义务 10 的两分对同轮补跑不可达（#3056 fix-r1 MM concurrency#1）
        rec = {"key": key, "ts": dt.datetime.now().isoformat(timespec="seconds"), "status": status,
               "resource_contention": contention, "rerun": rerun, "superseded": False,
               "note": note}
        dup = any(r.get("key") == key and bool(r.get("rerun")) == rerun
                  and _rank_raw(r) <= _merge_rank(rec) for r in existing)
        rec["superseded"] = dup
        # 追加写整行（#3056 fix-r2 修 5）：短写循环补写；中途 OSError 回滚到写前长度（仍在锁内），
        # 不留半行 JSON 让后续 count/register 的坏行 fail-closed 连坐。写前长度的 fstat 与取锁同款
        # 收进 OSError → 干净文案 rc 2（修 4），不以 traceback rc 1 冒充停手信号
        payload = (json.dumps(rec, ensure_ascii=False) + "\n").encode("utf-8")
        try:
            end = os.fstat(fd).st_size
        except OSError as e:
            print(f"fixround-ledger: {path} 取写前长度失败（fail-closed）：{e}", file=sys.stderr)
            return 2
        try:
            done = 0
            while done < len(payload):
                done += os.write(fd, payload[done:])
        except OSError as e:
            try:
                os.ftruncate(fd, end)
            except OSError:
                pass
            print(f"fixround-ledger: {path} 写入失败已回滚（fail-closed）：{e}", file=sys.stderr)
            return 2
        tag = "（重复键，标 superseded 不计数——已有同形态记录不轻于本次）" if dup else ""
        print(f"fixround-ledger: 登记 {key} status={status} contention={contention} rerun={rerun}{tag}")
        return 0
    finally:
        # 解锁/关闭失败不改变已定结果（写已落盘或已回滚；进程退出即释放锁），只告警，不以 traceback 逃出
        try:
            fcntl.flock(fd, fcntl.LOCK_UN)
        except OSError as e:
            print(f"fixround-ledger: {path} 解锁失败（不影响已定结果）：{e}", file=sys.stderr)
        try:
            os.close(fd)
        except OSError as e:
            print(f"fixround-ledger: {path} 关闭失败（不影响已定结果）：{e}", file=sys.stderr)


def _merged_by_key(recs: list[dict]) -> list[dict]:
    """按键归并（#3056 fix-r2 修 1；fix-r3 修 2 改「按计入类取最严」）：每个键至多贡献一条
    记录——superseded（同键同形态重复登记）弃掉；**计入类（contention_counted /
    unavailable_other）优先于 available**——同键「首登计入类失败 + 补跑 PASS」归并后仍计入，
    补跑翻绿抹不掉已计入的失败（判据 5 停手信号不漏报，MM concurrency#1 P1 = DS boundary#3 =
    MM sibling#2 三撞）；**同档内补跑（rerun）优先于首登**——同轮「首登失败 + 补跑失败」
    取补跑形态，两条只计 1，逐条累加会让单轮顶满停手阈值（假停手）。同形态的重复登记按类别判
    superseded（#3056 fix-r4 修 2：已有同形态记录不轻于本次才标，计入类失败不因后到被抹），
    同键计入类失败可能有多条非 superseded 记录——归并同严取先到，每键仍恰出一条，不影响计数。"""
    chosen: dict[str, dict] = {}
    for rec in recs:
        if rec.get("superseded"):
            continue
        cur = chosen.get(rec["key"])
        if cur is None or (_merge_rank(rec) < _merge_rank(cur)):
            chosen[rec["key"]] = rec
    return list(chosen.values())


def _merge_rank(rec: dict) -> tuple[int, int]:
    """归并序：计入类（最严）排前，同档内补跑排前——_merged_by_key 与 judge / count 共用。"""
    return (0 if classify(rec) in COUNTED_CLASSES else 1, 0 if rec.get("rerun") else 1)


def _rank_raw(rec: dict) -> tuple[int, int]:
    """判重用的原始类别序（#3056 fix-r4 修 2）：剥掉行上已有的 superseded 标记再算 _merge_rank——
    判重看类别本身，不看「上一条曾被判重复」（被抹过的失败再补登，仍要能与原失败对上类别）。"""
    return _merge_rank({k: v for k, v in rec.items() if k != "superseded"})


def count(path: str) -> int:
    # 先按键归并（每键一次、计入类最严优先、同档补跑优先），再按链×谱系汇总——逐条累加会把
    # 同轮「首登失败 + 补跑失败」计 2，单轮即顶满停手阈值（#3056 fix-r2 修 1，MM concurrency#1 P1）
    tally: dict[tuple[str, str], int] = {}
    for rec in _merged_by_key(read_lines(path)):
        # 与 judge 同一分类（classify）：只计失败类（contention_counted / unavailable_other）。
        # 首次争抢（contention_first）是排除段、superseded 是重复登记、available 是 PASS——都不计，
        # 否则「一次真失败 + 一次资源不足」就假触发停手（#3056 fix-r1 DS concurrency#4/DS sibling#3）
        if classify(rec) not in COUNTED_CLASSES:
            continue
        tally[_chain_lineage(rec["key"])] = tally.get(_chain_lineage(rec["key"]), 0) + 1
    rc = 0
    for (chain, lineage), n in sorted(tally.items()):
        flag = "  <-- 停手信号" if n >= COUNT_THRESHOLD else ""
        print(f"#{chain}/{lineage}: {n}{flag}")
        if n >= COUNT_THRESHOLD:
            rc = 1
    print(f"fixround-ledger: {'触发停手阈值（判据 5）：同链×谱系键归并后计入类失败累计 ≥ 2' if rc else '未触发阈值'}")
    return rc


def judge(path: str) -> int:
    # 与 count 同一键归并（#3056 fix-r3 修 3，MM sibling#1 P2）：逐条输出会让同键多行、
    # 两个命令口径不一致；归并后每键一行（取最严形态，行内标注该键归并记录是补跑还是首登）
    for rec in _merged_by_key(read_lines(path)):
        form = "补跑" if rec.get("rerun") else "首登"
        print(f"{rec['key']}: {CLASS_LABEL[classify(rec)]}［{form}·键归并取最严］")
    return 0


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    vk = sub.add_parser("validate-key", help="校验幂等键格式")
    vk.add_argument("key")
    rg = sub.add_parser("register", help="登记一轮（flock + O_APPEND；同键同形态、已有记录不轻于本次的后到标 superseded；计入类失败不因后到被抹；补跑参与判重）")
    rg.add_argument("path")
    rg.add_argument("key")
    rg.add_argument("--status", default="ERROR", help="该轮评审状态（run-review-roles 的 summary 判定，如 PASS/ERROR/TIMEOUT）")
    rg.add_argument("--contention", action="store_true", help="该轮属资源争抢（SlotTimeout / 429 配额 / 执行级 TIMEOUT）")
    rg.add_argument("--rerun", action="store_true", help="该轮是补跑（--rerun；首次争抢补跑后仍争抢按前提③计入）")
    rg.add_argument("--note", default="", help="备注（跟踪单评论链接等）")
    ct = sub.add_parser("count", help="按键归并（每键一次：计入类最严优先，同档补跑优先）再按链×谱系累计计入类失败（与 judge 同分类同归并；≥2 退出码 1，判据 5 停手信号）")
    ct.add_argument("path")
    jd = sub.add_parser("judge", help="按键归并后逐键分类（行标注补跑/首登）：available / contention_first / contention_counted / unavailable_other")
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
    try:
        _rc = main()
    except OSError:
        # print 路径的 OSError（stdout 管道早关 → BrokenPipeError、stderr 关闭 → EBADF，
        # #3056 fix-r4 修 3.1，MM sibling#2 P2）：文件读写的 OSError 都已在各路径内收成干净
        # 文案 rc 2，能逃到这里的是告警与输出自身的 print——同样收成干净 rc 2，不以 traceback
        # rc 1 冒充「count 触发阈值」（rc 1 是判据 5 停手的专属信号）。退出前把 std 流接去
        # devnull：解释器收尾 flush 再撞同一根已关闭的管道会把退出码改写成 120。
        try:
            _dn = os.open(os.devnull, os.O_WRONLY)
            os.dup2(_dn, 1)
            os.dup2(_dn, 2)
        except OSError:
            pass
        _rc = 2
    sys.exit(_rc)
