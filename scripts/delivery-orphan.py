#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""delivery-orphan.py — 孤儿交付检查（三类）。有孤儿退出 1，无孤儿退出 0，自身出错退出 2。

为什么有它（两次同类事故，都是「靠人记得」漏掉的）：
  · #1333：worker 交付完成、树上有 commit，但没人开 PR，一直躺到另一个代理顺路看见（A 类 / B 类）。
  · #1355：19:22 交付，空等 58 分钟无人接手，靠 20:20 守望扫描才发现（A 类 / C 类）。

三类判定：
  A 类 有交付无认领（ORPHAN-UNCLAIMED / ORPHAN-STALLED）
      扫 _build/wt-*/_tmp/**/{glm,dsh,codex}.exit 与 glm.status，按「轮次目录」问认领表 claimed.tsv：
        无记录且交付 > UNCLAIMED_GRACE_MIN 分钟          → ORPHAN-UNCLAIMED
        有记录、状态 claimed、> STALLED_MIN 分钟无更新   → ORPHAN-STALLED
      交付时间只取 marker 文件 mtime，**不读 exit 内容判成败**（memory「glm-exit-false-zero」：
      exit 文件内容不可信，且「有没有人接手」与成败无关，失败交付同样要有人处理）。
      免误报：交付之后这棵树上又出现提交 → 视为已收货，不再报（--no-commit-check 关掉）。
  B 类 有提交无 PR（ORPHAN-NO-PR）
      每棵 _build/wt-*：分支非 HEAD/main、origin/main..HEAD 有提交、该分支没有 open/merged PR、
      认领表里也没有该单的 pr-opened/merged 记录 → 报。
  C 类 无 PR 也无人在跑（ORPHAN-DEAD）
      B 类命中，且本机没有 runner 进程、没有 remote-wait 等待它、远端三台也查不到 → 最高优先。
      远端默认 ssh 查（--no-check-remote 关掉）；有主机没核过就只报 NO-PR，绝不判 DEAD。

用法：
  python3 -I scripts/delivery-orphan.py                 # 巡检：打印孤儿，有 → 退出 1
  python3 -I scripts/delivery-orphan.py --root <主仓根>  # 换根（自测 fixture 用）
  python3 -I scripts/delivery-orphan.py --claim <轮次目录> --by <代理名> [--status claimed]
  python3 -I scripts/delivery-orphan.py --claim <轮次目录> --by <代理名> --status pr-opened
  python3 -I scripts/delivery-orphan.py --claims        # 打印认领表
  python3 -I scripts/delivery-orphan.py --no-check-remote   # 不 ssh 远端（快，但 C 类只看本机，容易误判 DEAD）
  python3 -I scripts/delivery-orphan.py --json          # 机读（巡检 / 其它脚本用）

阈值（都可被环境变量覆盖，见下面常量；写死在代码里是为了「不靠人记得」）：
  PDRS_ORPHAN_UNCLAIMED_MIN  默认 15
  PDRS_ORPHAN_STALLED_MIN    默认 45
  PDRS_ORPHAN_PR_TTL         默认 300（gh PR 查询缓存秒数）
  PDRS_ORPHAN_MAX_AGE_HOURS  默认 72（A 类只看看得住的近交付，0 = 不限）

并发：认领表与 PR 缓存的所有写入都是 flock + 临时文件 + rename（memory「lock-use-flock」「signal-file-atomic」）；
      不用 mkdir 目录锁。脚本用 python3 -I 干净跑，不依赖当前目录（找不到根就报错退出 2，不瞎猜）。
"""

import argparse
import contextlib
import fcntl
import json
import os
import re
import subprocess
import sys
import time
from datetime import datetime

# ---------------------------------------------------------------- 常量与依据

DEFAULT_ROOT = "/Users/da/ws/pilotdeck-rs"
LEDGER_REL = "_tmp/20261105-watch/claimed.tsv"
CACHE_REL = "_tmp/20261105-watch/pr-cache.json"


def env_number(name, default, cast, minimum=0, allow_nan=False):
    """读数值环境变量；非法值直接退出 2（自身出错），不落成「有孤儿」的退出 1 假警报。"""
    raw = os.environ.get(name)
    if raw is None or not raw.strip():
        return default
    try:
        value = cast(raw.strip())
    except (TypeError, ValueError):
        print("delivery-orphan: %s=%r 不是合法数字（自身出错，退出 2）" % (name, raw),
              file=sys.stderr)
        sys.exit(2)
    if isinstance(value, float) and value != value:
        if not allow_nan:
            print("delivery-orphan: %s=NaN 不合法（自身出错，退出 2）" % name, file=sys.stderr)
            sys.exit(2)
    elif minimum is not None and value < minimum:
        print("delivery-orphan: %s=%r 小于 %s（自身出错，退出 2）" % (name, raw, minimum),
              file=sys.stderr)
        sys.exit(2)
    return value


# 15 min：正常收货节奏是「轮询 + auto-receive 数分钟 + 开 PR 数分钟」，15 min 是它的 20 倍余量；
# 今天两次事故的间隔是 58 min（#1355 19:22 → 20:20），15 min 能在空等半小时内就报出来。
UNCLAIMED_GRACE_MIN = env_number("PDRS_ORPHAN_UNCLAIMED_MIN", 15, int)
# 45 min：认领后 45 min 还是 claimed，等于「一次正常收货 + 一次重试」都没落地（#1355 空等 58 min 的大半）。
STALLED_MIN = env_number("PDRS_ORPHAN_STALLED_MIN", 45, int)
# 5 min：同一分支的 PR 查询缓存（每分支一次 gh 调用，28 棵树冷启动约 20 s，热启动 0 次）。
PR_CACHE_TTL = env_number("PDRS_ORPHAN_PR_TTL", 300, int)
# 72 h：A 类只看「还看得住」的交付。三天前的交付要么已被 board / 守望处理过，要么应显式标 parked；
# 全量翻旧账（例如 10-26 那批老树）用 --max-age-hours 0。B 类不看年龄（那是仓库状态，不是流水）。
MAX_AGE_HOURS = env_number("PDRS_ORPHAN_MAX_AGE_HOURS", 72.0, float)

GH_TIMEOUT = env_number("PDRS_ORPHAN_GH_TIMEOUT", 60, int, minimum=1)
GIT_TIMEOUT = env_number("PDRS_ORPHAN_GIT_TIMEOUT", 60, int, minimum=1)
SSH_TIMEOUT = env_number("PDRS_ORPHAN_SSH_TIMEOUT", 12, int, minimum=1)
# 远端 worker 主机清单：运维加机器时用 PDRS_ORPHAN_HOSTS（逗号分隔）改，不必动仓里的代码；
# 漏改会把新机上正在跑的单判成 ORPHAN-DEAD（最高优先级误报）。
REMOTE_HOSTS = tuple(
    h.strip()
    for h in os.environ.get("PDRS_ORPHAN_HOSTS", "cb8,mbp2,macmini-01").split(",")
    if h.strip()
)

MARKERS = ("glm.exit", "glm.status", "dsh.exit", "codex.exit")
# 与守望 watch-deliveries10.sh 的排除口径一致：归档 / 污染 / 证据转存 / 非交付目录不算交付。
SKIP_COMPONENTS = {"archive", "evidence", "delivery", "remote-run", "probe", "__pycache__"}
MAX_DEPTH = 4  # _tmp/ 之下最多再看 4 层（_tmp/<轮>/fix-rN/…），再深不是交付目录

KIND_ORDER = {"ORPHAN-DEAD": 0, "ORPHAN-STALLED": 1, "ORPHAN-UNCLAIMED": 2, "ORPHAN-NO-PR": 3}
STATUSES = ("claimed", "pr-opened", "merged", "parked")
DONE_STATUSES = ("pr-opened", "merged", "parked")  # 这三态视为「有人接手 / 已了结」，不再报 A 类


# ---------------------------------------------------------------- 小工具


def run(cmd, cwd=None, timeout=GIT_TIMEOUT):
    """跑命令，永不抛：返回 (rc, stdout, stderr)。超时 rc=124，起不来 rc=127。"""
    try:
        p = subprocess.run(
            cmd, cwd=cwd, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
            text=True, timeout=timeout,
        )
        return p.returncode, p.stdout, p.stderr
    except subprocess.TimeoutExpired:
        return 124, "", "timeout %ss: %s" % (timeout, " ".join(cmd))
    except OSError as exc:
        return 127, "", str(exc)


def die(msg, code=2):
    print("delivery-orphan: %s" % msg, file=sys.stderr)
    sys.exit(code)


def warn(msg):
    print("WARN  delivery-orphan: %s" % msg, file=sys.stderr)


def iso(ts):
    return datetime.fromtimestamp(ts).strftime("%Y-%m-%dT%H:%M:%S")


def parse_ts(text):
    for fmt in ("%Y-%m-%dT%H:%M:%S", "%Y-%m-%d %H:%M:%S"):
        try:
            return datetime.strptime(text, fmt).timestamp()
        except ValueError:
            continue
    return None


def mins(seconds):
    return int(round(seconds / 60.0))


def norm_key(root, path):
    """认领表的键：仓内路径写成相对主仓根（人好读、跨树一致），仓外写绝对路径。"""
    root_r = os.path.realpath(root)
    p = os.path.realpath(path)
    if p == root_r:
        return "."
    if p.startswith(root_r + os.sep):
        return os.path.relpath(p, root_r)
    return p


def key_to_abs(root, key):
    return key if os.path.isabs(key) else os.path.join(root, key)


# ---------------------------------------------------------------- 原子写 + flock


def write_atomic(path, text):
    """tmp + fsync + rename：读者永远看到完整文件（memory「signal-file-atomic」）。"""
    d = os.path.dirname(path) or "."
    os.makedirs(d, exist_ok=True)
    tmp = os.path.join(d, ".%s.tmp.%d" % (os.path.basename(path), os.getpid()))
    with open(tmp, "w", encoding="utf-8") as fh:
        fh.write(text)
        fh.flush()
        os.fsync(fh.fileno())
    os.replace(tmp, path)


@contextlib.contextmanager
def file_lock(target):
    """在 <target>.lock 上取排他 flock（memory「lock-use-flock」：锁用 flock，不用 mkdir 目录锁）。

    锁文件与数据文件分开：数据文件是 rename 换掉的，锁在旧 inode 上会互相看不见。
    """
    lock_path = target + ".lock"
    os.makedirs(os.path.dirname(lock_path) or ".", exist_ok=True)
    fd = os.open(lock_path, os.O_CREAT | os.O_RDWR, 0o644)
    try:
        fcntl.flock(fd, fcntl.LOCK_EX)
        yield
    finally:
        try:
            fcntl.flock(fd, fcntl.LOCK_UN)
        finally:
            os.close(fd)


# ---------------------------------------------------------------- 认领表


def ledger_path(root):
    return os.path.join(root, LEDGER_REL)


def load_ledger(path):
    """返回 [[key, ts(float), agent, status], …]；坏行跳过（不猜）。"""
    rows = []
    if not os.path.exists(path):
        return rows
    with open(path, encoding="utf-8") as fh:
        for line in fh:
            line = line.rstrip("\n")
            if not line.strip() or line.lstrip().startswith("#"):
                continue
            parts = line.split("\t")
            if len(parts) < 4:
                continue
            ts = parse_ts(parts[1].strip())
            if ts is None:
                continue
            status = parts[3].strip()
            if status not in STATUSES:
                # 手改 typo（Claimed / claim…）既不算已了结也不等于 claimed，会让该单永久静默；
                # 一律按 claimed 处理，最多 45 min 后报 STALLED —— 失败方向朝告警。
                warn("认领表 %s 的状态 %r 不在 %s 内，按 claimed 处理"
                     % (parts[0].strip(), status, "/".join(STATUSES)))
                status = "claimed"
            rows.append([parts[0].strip(), ts, parts[2].strip(), status])
    return rows


def ledger_index(root, rows):
    """key(规范化绝对路径) → (ts, agent, status)。同键多行取最后写入的那行。"""
    idx = {}
    for key, ts, who, status in rows:
        idx[norm_key(root, key_to_abs(root, key))] = (ts, who, status)
    return idx


def save_ledger(path, rows):
    lines = [
        "# 轮次目录\t认领时间\t认领者\t状态(claimed|pr-opened|merged|parked)",
        "# 由 scripts/delivery-orphan.py --claim 维护（flock + tmp/rename）；手改请整行替换，勿并发编辑",
    ]
    for key, ts, who, status in sorted(rows, key=lambda r: r[0]):
        lines.append("%s\t%s\t%s\t%s" % (key, iso(ts), who, status))
    write_atomic(path, "\n".join(lines) + "\n")


def do_claim(root, target, who, status):
    path = ledger_path(root)
    if not os.path.isdir(target):
        die("claim: 轮次目录不存在：%s" % target)
    key = norm_key(root, target)
    with file_lock(path):
        rows = load_ledger(path)
        now = time.time()
        hit = None
        for row in rows:
            if norm_key(root, key_to_abs(root, row[0])) == key:
                hit = row
        if hit is None:
            rows.append([key, now, who, status or "claimed"])
            action = "claimed"
        else:
            hit[1] = now
            hit[2] = who
            if status:
                hit[3] = status
            action = "updated"
        save_ledger(path, rows)
    print("%s\t%s\t%s\t%s" % (action, key, who, status or (hit[3] if hit else "claimed")))
    return 0


def show_claims(root):
    rows = load_ledger(ledger_path(root))
    if not rows:
        print("（认领表为空：%s）" % ledger_path(root))
        return 0
    for key, ts, who, status in sorted(rows, key=lambda r: r[1]):
        print("%s\t%s\t%s\t%s" % (key, iso(ts), who, status))
    return 0


# ---------------------------------------------------------------- PR 查询（按分支缓存）

PR_JSON_FIELDS = "number,state"


def load_cache(path):
    try:
        with open(path, encoding="utf-8") as fh:
            data = json.load(fh)
        if isinstance(data, dict) and isinstance(data.get("branches"), dict):
            return data
    except (OSError, ValueError):
        pass
    return {"version": 1, "branches": {}}


def save_cache(path, cache):
    write_atomic(path, json.dumps(cache, ensure_ascii=False, indent=1, sort_keys=True) + "\n")


def gh_fetch_branch(root, branch):
    """一次 gh 调用拿该分支全部状态的 PR 号。失败返回 None（未知 ≠ 没有，绝不据此报孤儿）。"""
    rc, out, err = run(
        ["gh", "pr", "list", "--head", branch, "--state", "all",
         "--json", PR_JSON_FIELDS, "--limit", "20"],
        cwd=root, timeout=GH_TIMEOUT,
    )
    if rc != 0:
        warn("gh pr list --head %s 失败 rc=%s：%s" % (branch, rc, (err or out).strip()[:160]))
        return None
    try:
        items = json.loads(out or "[]")
    except ValueError as exc:
        warn("gh 输出不可解析（%s）：%s" % (branch, exc))
        return None
    res = {"open": [], "merged": [], "closed": []}
    for it in items:
        state = str(it.get("state", "")).upper()
        num = it.get("number")
        if state == "OPEN":
            res["open"].append(num)
        elif state == "MERGED":
            res["merged"].append(num)
        else:
            res["closed"].append(num)
    return res


def pr_state(root, branch, cache, cache_path, ttl):
    entry = cache["branches"].get(branch)
    now = time.time()
    if entry and isinstance(entry.get("ts"), (int, float)) and now - entry["ts"] <= ttl:
        return entry
    fresh = gh_fetch_branch(root, branch)
    if fresh is None:
        return None
    fresh["ts"] = now
    with file_lock(cache_path):
        cur = load_cache(cache_path)
        cur["branches"][branch] = fresh
        cur["version"] = 1
        save_cache(cache_path, cur)
    cache["branches"][branch] = fresh
    return fresh


# ---------------------------------------------------------------- 扫描


def worktrees(root):
    """[(名字, 绝对路径, 单号标签)]；排除非交付树（工具树 / HEAD 树）。"""
    base = os.path.join(root, "_build")
    out = []
    if not os.path.isdir(base):
        return out
    for name in sorted(os.listdir(base)):
        path = os.path.join(base, name)
        if not name.startswith("wt-") or not os.path.isdir(path):
            continue
        if name == "wt-tools-main":  # 工具树是 origin/main 的只读检出，不是交付树
            continue
        out.append((name, path, name[3:]))
    return out


def skip_path(base, dirpath):
    rel = dirpath[len(base):]
    if "-archive-" in rel or ".contaminated" in rel:
        return True
    for comp in rel.split(os.sep):
        if comp in SKIP_COMPONENTS:
            return True
    return False


def scan_deliveries(wt_path):
    """{轮次目录绝对路径: (mtime, [marker 名…])}；交付时间只取 mtime。"""
    out = {}
    base = os.path.join(wt_path, "_tmp")
    if not os.path.isdir(base):
        return out
    for dirpath, dirnames, filenames in os.walk(base):
        if skip_path(base, dirpath):
            dirnames[:] = []
            continue
        if dirpath[len(base):].count(os.sep) >= MAX_DEPTH:
            dirnames[:] = []
        hits = [f for f in filenames if f in MARKERS]
        if not hits:
            continue
        try:
            mt = max(os.path.getmtime(os.path.join(dirpath, f)) for f in hits)
        except OSError:
            continue
        out[dirpath] = (mt, sorted(hits))
    return out


def git_head(wt_path):
    rc, out, _ = run(["git", "-C", wt_path, "rev-parse", "--abbrev-ref", "HEAD"])
    if rc != 0:
        return None
    return (out or "").strip() or None


def git_sha(wt_path):
    rc, out, _ = run(["git", "-C", wt_path, "rev-parse", "HEAD"])
    return out.strip() if rc == 0 else ""


def git_head_ct(wt_path):
    """HEAD 提交时间（epoch）；取不到返回 None。"""
    rc, out, _ = run(["git", "-C", wt_path, "log", "-1", "--format=%ct"])
    if rc != 0:
        return None
    try:
        return int((out or "").strip())
    except ValueError:
        return None


def git_ahead(wt_path):
    rc, out, _ = run(["git", "-C", wt_path, "rev-list", "--count", "origin/main..HEAD"])
    if rc != 0:
        return None
    try:
        return int((out or "").strip())
    except ValueError:
        return None


ISSUE_RE = re.compile(r"issue-(\d+)")

# 「有人在跑」的签名：只有 runner 类进程才算「有人会来处理它」。
# 为什么不用「进程提到树名」就够：任何一次巡检 / 别的代理的 find、grep、git status 都会提到树名，
# 照单全收会把真孤儿判成「有人在跑」（11-05 首次真实现场实测：一条 find 扫描就把 wt-1334 判成了 NO-PR）。
ACTIVE_SIGS = (
    ("cargo", re.compile(r"\bcargo\b")),
    ("rustc", re.compile(r"\brustc\b")),
    ("worker", re.compile(r"run-worker|remote_glm_impl|glm_impl|dispatch-|rw\.sh|remote-worker")),
    ("review", re.compile(r"run-review-roles|review-phase|glm_review|review-deepseek")),
    ("remote-wait", re.compile(r"remote-wait")),
    ("host", re.compile(r"host-chain|local-host-queue|host-only")),
    ("auto-receive", re.compile(r"auto-receive")),
    ("negctl", re.compile(r"negctl|negative-runs|neg\.sh")),
)


def issue_of(text):
    m = ISSUE_RE.search(text or "")
    return m.group(1) if m else None


def pgrep_lines(pattern):
    rc, out, _ = run(["pgrep", "-fl", "--", pattern], timeout=20)
    if rc != 0:  # 1 = 没匹配到
        return []
    return [l for l in out.splitlines() if l.strip()]


def is_alive(pid_path):
    try:
        with open(pid_path, encoding="utf-8") as fh:
            pid = int((fh.read() or "").strip())
    except (OSError, ValueError):
        return False
    try:
        os.kill(pid, 0)
        return True
    except OSError:
        return False


# 扫描类命令：它们只是「提到」这棵树（find / grep 的路径参数里带树名），不是有人在做这单。
# 11-05 首次真实现场实测：一条 `find … _tmp/2026… ` 的命令行里带 cargo 字样，被签名匹配误判成「有人在跑」。
SCAN_RE = re.compile(
    r"^\s*(\S*/)?(find|grep|egrep|fgrep|rg|ls|stat|du|df|shasum|md5|cat|sed|awk|head|tail|wc|xargs|sort|uniq)\b"
)


def signature_of(line):
    """把一行 ps 输出归到一个短标签；没有 runner 签名返回 None（扫描类进程不算有人）。"""
    if SCAN_RE.search(line):
        return None
    for label, rx in ACTIVE_SIGS:
        if rx.search(line):
            return label
    return None


def local_activity(name, tag, wt_path):
    """本机有没有 runner 在处理这棵树 / 这单：签名匹配的进程、remote-wait 在等它、waiter.pid 活着。

    树名用 ERE `wt-<名>([/ ]|$)`：既命中 `…/wt-1333/`、`…/wt-1333/src`，也不会把 wt-13330 算进来。
    """
    hits = []
    tree_re = r"%s([/ ]|$)" % re.escape(name)
    for line in pgrep_lines(tree_re):
        label = signature_of(line)
        if label:
            hits.append("%s:%s" % (label, line.strip()[:70]))
    # 远端 worker：本机一定有一个 remote-wait 在轮询（远端会话自己不会回来收尾）
    for line in pgrep_lines("remote-wait"):
        if re.search(tree_re, line) or ("issue-" + tag) in line:
            hits.append("remote-wait:%s" % line.strip()[:70])
    for dirpath, dirnames, filenames in os.walk(os.path.join(wt_path, "_tmp")):
        if dirpath[len(os.path.join(wt_path, "_tmp")):].count(os.sep) >= MAX_DEPTH:
            dirnames[:] = []
        if "waiter.pid" in filenames:
            p = os.path.join(dirpath, "waiter.pid")
            if is_alive(p):
                hits.append("waiter.pid:%s" % p)
    return hits


def remote_activity(tag, hosts=REMOTE_HOSTS):
    """远端主机上有没有这个单的进程。远端树名 ~/ws/pdrs-worker-wt/issue-<N>。

    返回 (hits, unchecked)：unchecked 是连不上 / 探测失败的主机。远端没核过就不许判 DEAD
    ——11-05 实测：wt-1473 本机没有任何 remote-wait，但 cb8 上有 worker 在跑，只查本机会误判「没人会再来处理」。
    """
    # 远端侧 pgrep 的模式不能把自己算进去：`sh -c "pgrep -f '<模式>'"` 这条 shell 的命令行里就带模式原文，
    # 会把计数恒判 ≥1（11-05 自测踩到：每个 fixture 都报 remote:cb8）。末位字符写成字符类 [3] 断开自匹配。
    if not tag.isdigit():
        # 定不出远端树名（issue-<N>）就核不了 —— 把主机全记成「未核」，让上层只报 NO-PR，绝不判 DEAD。
        return [], list(hosts)
    body = re.escape(tag)
    pat = "issue-%s([^0-9]|$)" % (body[:-1] + "[%s]" % body[-1])
    hits, unchecked = [], []
    for host in hosts:
        rc, out, _ = run(
            ["ssh", "-o", "BatchMode=yes", "-o", "ConnectTimeout=8", host,
             "pgrep -f '%s' | wc -l" % pat],
            timeout=SSH_TIMEOUT,
        )
        if rc != 0:
            unchecked.append(host)
            continue
        try:
            if int((out or "0").strip() or 0) > 0:
                hits.append("remote:%s" % host)
        except ValueError:
            unchecked.append(host)
    return hits, unchecked


# ---------------------------------------------------------------- 主流程


def collect(root, check_remote, max_age_hours, commit_check=True):
    cache_path = os.path.join(root, CACHE_REL)
    cache = load_cache(cache_path)
    ledger = ledger_index(root, load_ledger(ledger_path(root)))
    now = time.time()
    max_age = max_age_hours * 3600.0

    findings = []      # (kind, tree, subject, detail)
    stats = {"trees": 0, "deliveries": 0, "claims": len(ledger), "received": 0}
    pr_notes = []

    for name, path, tag in worktrees(root):
        stats["trees"] += 1
        branch = git_head(path)
        head_ct = git_head_ct(path)
        deliveries = scan_deliveries(path)
        stats["deliveries"] += len(deliveries)

        # ---- A 类：有交付无认领
        for rdir in sorted(deliveries):
            mt, markers = deliveries[rdir]
            age = now - mt
            if max_age and age > max_age:
                continue  # 超出 MAX_AGE_HOURS 的历史流水不当作现场
            key = norm_key(root, rdir)
            row = ledger.get(key)
            rel = os.path.relpath(rdir, root)
            desc = "%s %s（%d 分钟前）" % ("/".join(markers), iso(mt), mins(age))
            if row is None:
                if age / 60.0 <= UNCLAIMED_GRACE_MIN:
                    continue
                # 交付之后这棵树上又出现提交 = 已经有人收过货（#1355 事故里 19:22 交付、20:25 才有人提交，
                # 那 58 分钟空档内本脚本照样会报）。这是免误报的确定性判据，不依赖有没有人记得写认领表。
                if commit_check and head_ct is not None and head_ct >= mt:
                    stats["received"] += 1
                    continue
                findings.append((
                    "ORPHAN-UNCLAIMED", name, rel,
                    "%s，认领表无记录（%s 无此行）" % (desc, LEDGER_REL),
                ))
                continue
            ts, who, status = row
            if status in DONE_STATUSES:
                continue
            idle = (now - ts) / 60.0
            if status == "claimed" and idle > STALLED_MIN:
                # 与 UNCLAIMED 同源的两道免误报闸：交付后树上又有提交 = 已收货（代理常常忘记回写
                # pr-opened，不能因此把已收的货报成「无人推进」）。
                if commit_check and head_ct is not None and head_ct >= mt:
                    stats["received"] += 1
                    continue
                findings.append((
                    "ORPHAN-STALLED", name, rel,
                    "%s，%s 于 %s 认领后 %d 分钟仍是 claimed（无人推进）"
                    % (desc, who, iso(ts), mins(now - ts)),
                ))

        # ---- B 类：有提交无 PR
        if branch in (None, "", "HEAD", "main"):
            continue
        ahead = git_ahead(path)
        if ahead is None:
            warn("%s 取 origin/main..HEAD 提交数失败，跳过 B 类" % name)
            continue
        if ahead <= 0:
            continue
        head = git_sha(path)
        num = issue_of(branch) or (tag if tag.isdigit() else None)
        claimed_done = None
        for key, (ts, who, status) in ledger.items():
            if status in ("pr-opened", "merged") and num and issue_of(key) == num:
                claimed_done = (key, who, status)
                break
        if claimed_done:
            continue
        st = pr_state(root, branch, cache, cache_path, PR_CACHE_TTL)
        if st is None:
            pr_notes.append("%s（%s）PR 状态未知，B/C 类跳过" % (name, branch))
            continue
        if st["open"] or st["merged"]:
            continue
        pr_note = ""
        if st["closed"]:
            pr_note = "，PR %s 已关闭未合并" % ",".join("#%s" % n for n in st["closed"])
        act = local_activity(name, tag, path)
        unchecked = []
        if check_remote and not act:
            remote_hits, unchecked = remote_activity(num or tag)
            act += remote_hits
        if act:
            findings.append((
                "ORPHAN-NO-PR", name, branch,
                "%d 个提交未进 main，head=%s，无 open PR%s；有人在跑（%s）"
                % (ahead, head[:9], pr_note, act[0][:120]),
            ))
        elif unchecked:
            # 远端没核过就不判「没人会再来处理它」——只说没 PR
            findings.append((
                "ORPHAN-NO-PR", name, branch,
                "%d 个提交未进 main，head=%s，无 open PR%s；本机无 runner，远端未核（%s）"
                % (ahead, head[:9], pr_note, "/".join(unchecked)),
            ))
        else:
            findings.append((
                "ORPHAN-DEAD", name, branch,
                "%d 个提交未进 main，head=%s，无 PR、本机与远端都没有 runner 进程%s"
                "——没人会再来处理它" % (ahead, head[:9], pr_note),
            ))

    findings.sort(key=lambda f: (KIND_ORDER.get(f[0], 9), f[1], f[2]))
    return findings, stats, pr_notes


def main(argv):
    ap = argparse.ArgumentParser(add_help=True, description="孤儿交付检查（#1333 / #1355）")
    ap.add_argument("--root", default=os.environ.get("PDRS_ORPHAN_ROOT", DEFAULT_ROOT),
                    help="主仓根（默认 %s）" % DEFAULT_ROOT)
    ap.add_argument("--claim", metavar="ROUND_DIR", help="认领 / 更新一行认领记录")
    ap.add_argument("--by", metavar="AGENT", help="认领者（代理名），--claim 必填")
    ap.add_argument("--status", choices=STATUSES, help="claimed|pr-opened|merged|parked")
    ap.add_argument("--claims", action="store_true", help="打印认领表")
    ap.add_argument("--no-check-remote", action="store_true",
                    help="不 ssh 查远端 worker（默认查 cb8/mbp2/macmini-01；连不上就只报 NO-PR、不判 DEAD）")
    ap.add_argument("--max-age-hours", type=float, default=MAX_AGE_HOURS,
                    help="A 类只看多少小时内的交付（0 = 不限，默认 %s）" % MAX_AGE_HOURS)
    ap.add_argument("--no-commit-check", action="store_true",
                    help="不把「交付之后树上又出现提交」当作已收货（默认当作已收货）")
    ap.add_argument("--json", action="store_true", help="以 JSON 输出（巡检接机器判定用）")
    args = ap.parse_args(argv)

    root = os.path.abspath(os.path.expanduser(args.root))
    if not os.path.isdir(os.path.join(root, "_build")):
        die("根下没有 _build/：%s（用 --root 指主仓根；不猜当前目录）" % root)

    if args.claim:
        if not args.by:
            die("--claim 需要 --by <代理名>")
        return do_claim(root, os.path.abspath(os.path.expanduser(args.claim)), args.by, args.status)
    if args.claims:
        return show_claims(root)

    # 负数 / NaN 会让 A 类整体静默跳过（假「无孤儿」）或反向跳过滤，必须在跑之前拒绝。
    if args.max_age_hours != args.max_age_hours or args.max_age_hours < 0:
        die("--max-age-hours 必须 >= 0（0 = 不限）：%r" % args.max_age_hours)

    findings, stats, pr_notes = collect(root, not args.no_check_remote, args.max_age_hours,
                                        commit_check=not args.no_commit_check)

    if args.json:
        print(json.dumps({
            "orphans": [
                {"kind": k, "tree": t, "subject": s, "detail": d} for k, t, s, d in findings
            ],
            "stats": stats,
            "notes": pr_notes,
        }, ensure_ascii=False, indent=1))
    else:
        for kind, tree, subject, detail in findings:
            print("%s\t%s\t%s\t%s" % (kind, tree, subject, detail))

    for note in pr_notes:
        warn(note)
    counts = {}
    for kind, _, _, _ in findings:
        counts[kind] = counts.get(kind, 0) + 1
    warn("扫了 %d 棵树 / %d 个交付 / %d 条认领记录（交付后已有人提交、按已收货略过 %d 个），命中 %d 条（%s）" % (
        stats["trees"], stats["deliveries"], stats["claims"], stats["received"], len(findings),
        "、".join("%s %d" % (k, counts[k]) for k in sorted(counts, key=lambda x: KIND_ORDER.get(x, 9)))
        or "无",
    ))
    return 1 if findings else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
