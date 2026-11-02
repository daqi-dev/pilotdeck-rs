#!/usr/bin/env python3
"""run-review-roles.py — pr-ready 三角色 + simplify 四视角的通道无关执行器（替代 Explore 子代理与 /simplify）。

每个角色起一个 `claude -p` 只读进程（工具白名单 Read/Grep/Glob，MCP 全关，不落 session），并行跑；
默认 `--runner zai`：同一个 Claude Code 外壳接智谱的 Anthropic 兼容接口跑 GLM（密钥取 `.env.local` 的 ZAI_API_KEY，
只经环境变量传给子进程，不进命令行、不落盘），工具白名单与 init 校验照旧；`--runner anthropic` 用 Claude 模型
（同样只认 .env.local 的 ANTHROPIC_API_KEY，也经 llm-center 基址；两通道都不读父环境里的密钥与基址）。子进程环境按白名单从零构造（父 shell 的令牌、网关头、provider 选择器一律不带），
CLAUDE_CONFIG_DIR 指向一次性空目录并只加载 user 设置源——用户/项目/本地 settings 里的 hooks 与插件都不生效
（真实 CLI 正负对照：项目 SessionStart hook 加隔离不触发、去隔离触发）。
原始事件流、stderr、最终文本、状态各自落盘。主进程不读角色全文，只汇总 summary.md（含 PR 用结果块）。

执行层只读证明：解析 stream-json 的 init 事件，工具集合不是白名单子集 → 立即杀进程，记 TOOLS_VIOLATION。
状态：PASS（有 final 且末行 VERDICT 合规）· PARTIAL（有 final 但缺 VERDICT/覆盖行）· TIMEOUT · STALLED · ERROR · TOOLS_VIOLATION。
只有 PASS 计入闸门；exit0 或 type=result 不等于完成评审（state/memory/20261016-review-bounds.md）。

速率看门狗：按到达墙钟统计模型输出（thinking_tokens 事件的估计增量 + assistant 文本 / 工具入参字符÷4；tool_result
是本地工具输出，不计）。启动满宽限期后，最近窗口内输出低于阈值持续 hold 秒 → STALLED，杀进程组。
STALLED / TIMEOUT 的角色按退避自动重跑（默认最多 2 次），每次原始流另存 <角色>.attempt<k>.jsonl/.err，
最后一次仍叫 <角色>.jsonl；status.json 记每次尝试，最终状态取最后一次。并行模式下各角色在自己线程里重试，互不阻塞。

用法：
  python3 scripts/run-review-roles.py --worktree _build/wt-a61 --base origin/main --out _tmp/20261017-a61/gate
  python3 scripts/run-review-roles.py --worktree _build/wt-a61 --base origin/main --out _tmp/20261017-a61/gate --serial
  python3 scripts/run-review-roles.py --diff-file X.patch --worktree . --out ... --roles boundary --dry-run
退出码：0 全部 PASS · 1 有非 PASS · 2 参数/前置错误
"""
from __future__ import annotations

import argparse
import collections
import datetime as dt
import importlib.util
import json
import os
import random
import shutil
import signal
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


def patch_fingerprint(wt: str, base: str, head: str) -> str:
    """补丁指纹与闸门比对共用 check-pr-gate.py 的同一实现（结果块 patch= 键）。"""
    spec = importlib.util.spec_from_file_location("check_pr_gate", ROOT / "scripts" / "check-pr-gate.py")
    gate = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    spec.loader.exec_module(gate)
    return gate.patch_fingerprint(wt, base, head)
TEMPLATE_DIR = ROOT / ".claude" / "workflows"
COMMON = "review-common.md"
ROLES = {
    "sibling": "review-role-sibling.md",
    "concurrency": "review-role-concurrency.md",
    "boundary": "review-role-boundary.md",
    "simplify": "review-role-simplify.md",
}
ALLOWED_TOOLS = ["Read", "Grep", "Glob"]
LLM_CENTER_BASE_URL = "https://llm-center.modelbest.co"  # 两通道的钥都走 llm-center；Claude Code 自行拼 /v1/messages，基址须为主机根（带 /v1 会 404）
# 网关对 GLM 在响应体里回显展示名而非请求名（modelUsage 仍是请求名）；只放行已实测的这一对，其余仍须逐字一致
# （10-25 实测 claude-opus-5-5 / claude-sonnet-5-5 经 llm-center 原样回显，原件 _tmp/20261025-anth-runner/smoke-*）
RESP_MODEL_ALIAS = {"glm-5.3-mb": "GLM-5.3"}
DEFAULT_MODEL = {"zai": "glm-5.3-mb", "anthropic": "claude-opus-5-5"}  # 通道正本见 state/memory/20261015-model-runners.md
# 每角色默认超时按通道给：opus 实测 ~6 min/角色（10-25），600 s 余量不足；GLM 维持原值
DEFAULT_TIMEOUT = {"zai": 600, "anthropic": 1200}


def env_candidates() -> list[Path]:
    """.env.local 候选：本检出根；worktree 里没有（忽略文件不随检出）时回落到主仓根（git common dir 的上一级）。"""
    common = subprocess.run(["git", "-C", str(ROOT), "rev-parse", "--path-format=absolute", "--git-common-dir"],
                            capture_output=True, text=True).stdout.strip()
    return [ROOT / ".env.local"] + ([Path(common).parent / ".env.local"] if common else [])


def load_env_key(name: str, env_file: Path | None = None) -> str:
    """只从 .env.local 取钥（不读父环境：父 shell 残留的令牌与网关键不得被带进子进程）。"""
    key = ""
    env = env_file if env_file is not None else next((c for c in env_candidates() if c.is_file()), None)
    if env and env.is_file():
        for line in env.read_text(encoding="utf-8").splitlines():
            line = line.strip()
            if line.startswith("export "):
                line = line[len("export "):]
            k, sep, v = line.partition("=")
            if sep and k.strip() == name:
                v = v.strip()
                key = v[1:-1] if len(v) >= 2 and v[0] == v[-1] and v[0] in "'\"" else v.split(" #", 1)[0].strip()
    return key


ENV_KEEP = ("PATH", "HOME", "USER", "LOGNAME", "SHELL", "TERM", "LANG", "LC_ALL", "LC_CTYPE", "TMPDIR")
KEY_NAME = {"zai": "ZAI_API_KEY", "anthropic": "ANTHROPIC_API_KEY"}


def runner_env(key: str) -> dict[str, str]:
    """子进程的全部模型相关环境（白名单之外的父环境一概不带）。"""
    return {"ANTHROPIC_API_KEY": key, "ANTHROPIC_BASE_URL": LLM_CENTER_BASE_URL}
LARGE_DIFF_LINES = 500  # 超过即提示按 --paths 分批（state/memory/20261016-review-bounds.md）
# 速率看门狗默认值。依据：11-02 对 wt-tools-main/_tmp/*-gate 历史 5000+ 角色流离线回放（原件与脚本
# _tmp/20261102-review-watchdog/serial-control/replay-grid*.txt）：宽限 180 / 窗口 120 / 5 tok/s / 持续 60 s 抓住
# anthropic TIMEOUT 55/72（中位第 1050 s 触发，原本要等满 2400 s），误杀 anthropic PASS 152/3161（4.8%；10-28~31 平稳
# 时段 12/1320 = 0.9%），误杀者自动重跑。GLM 通道同参数误杀 PASS 54/1918 而 TIMEOUT 仅 4/1934，默认不开（--stall-min-rate 显式开）。
STALL_GRACE = 180.0
STALL_WINDOW = 120.0
STALL_MIN_RATE = {"zai": 0.0, "anthropic": 5.0}
STALL_HOLD = 60.0
RETRIES = 2
RETRY_BACKOFF = (30.0, 90.0)  # 第 k 次重试前等待基数（秒），实际 ×U(0.8, 1.2) 抖动
RETRY_STATUSES = ("STALLED", "TIMEOUT")


def render(text: str, values: dict[str, str]) -> str:
    for k, v in values.items():
        text = text.replace("{{" + k + "}}", v)
    if "{{" in text:
        left = sorted({text[i:text.index("}}", i) + 2] for i in range(len(text)) if text.startswith("{{", i) and "}}" in text[i:]})
        raise SystemExit(f"run-review-roles: 模板变量未替换：{left}")
    return text


def build_cmd(claude_bin: str, model: str, budget: float) -> list[str]:
    tools = ",".join(ALLOWED_TOOLS)
    return [
        claude_bin, "-p",
        "--model", model,
        "--tools", tools,
        "--allowedTools", tools,
        "--permission-mode", "dontAsk",
        "--strict-mcp-config",
        "--setting-sources", "user",  # user 源指向一次性空 CLAUDE_CONFIG_DIR：不加载任何 settings/hooks/插件
        "--no-session-persistence",
        "--output-format", "stream-json", "--verbose",
        "--max-budget-usd", f"{budget:g}",
    ]


def validate_final(text: str) -> tuple[str, dict[str, int] | None, str]:
    """末段须有『覆盖:』行与末行『VERDICT: P0=a P1=b P2=c P3=d』。"""
    lines = [ln.strip() for ln in text.strip().splitlines() if ln.strip()]
    if not lines:
        return "PARTIAL", None, "final 为空"
    last = lines[-1]
    if not last.startswith("VERDICT:"):
        return "PARTIAL", None, "末行不是 VERDICT"
    counts: dict[str, int] = {}
    for tok in last[len("VERDICT:"):].split():
        k, _, v = tok.partition("=")
        if k in ("P0", "P1", "P2", "P3") and v.isdigit():
            counts[k] = int(v)
    if set(counts) != {"P0", "P1", "P2", "P3"}:
        return "PARTIAL", None, "VERDICT 缺等级计数"
    if not any(ln.startswith("覆盖:") or ln.startswith("覆盖：") for ln in lines):
        return "PARTIAL", counts, "缺『覆盖:』行（无法核角色覆盖）"
    return "PASS", counts, ""


def output_tokens(ev: dict) -> float:
    """一条 stream-json 事件里的模型输出量（估计 token）：thinking_tokens 增量 + assistant 文本 / 工具入参字符÷4。"""
    if ev.get("type") == "system" and ev.get("subtype") == "thinking_tokens":
        d = ev.get("estimated_tokens_delta")
        return float(d) if isinstance(d, (int, float)) and not isinstance(d, bool) and d > 0 else 0.0
    if ev.get("type") != "assistant":
        return 0.0
    msg = ev.get("message")
    n = 0
    for c in (msg.get("content") if isinstance(msg, dict) else None) or []:
        if not isinstance(c, dict):
            continue
        if c.get("type") == "text" and isinstance(c.get("text"), str):
            n += len(c["text"])
        elif c.get("type") == "thinking" and isinstance(c.get("thinking"), str):
            n += len(c["thinking"])
        elif c.get("type") == "tool_use":
            n += len(json.dumps(c.get("input"), ensure_ascii=False))
    return n / 4


class Watchdog:
    """滑动窗口速率看门狗。min_rate<=0 关闭。feed 由 pump 线程调用，check 由主线程轮询。"""

    def __init__(self, grace: float, window: float, min_rate: float, hold: float):
        self.grace, self.window, self.min_rate, self.hold = grace, window, min_rate, hold
        self.start = time.monotonic()
        self.events: collections.deque[tuple[float, float]] = collections.deque()
        self.lock = threading.Lock()
        self.total = 0.0
        self.first: float | None = None
        self.below_since: float | None = None

    def feed(self, tokens: float) -> None:
        if tokens <= 0:
            return
        now = time.monotonic()
        with self.lock:
            self.events.append((now, tokens))
            self.total += tokens
            if self.first is None:
                self.first = now - self.start

    def check(self) -> str:
        """返回空串表示正常；否则返回 STALLED 说明。"""
        if self.min_rate <= 0:
            return ""
        now = time.monotonic()
        if now - self.start < self.grace:
            return ""
        with self.lock:
            while self.events and self.events[0][0] <= now - self.window:
                self.events.popleft()
            got = sum(v for _, v in self.events)
        if got >= self.min_rate * self.window:
            self.below_since = None
            return ""
        if self.below_since is None:
            self.below_since = now
        if now - self.below_since >= self.hold:
            return (f"流式速率低于 {self.min_rate:g} tok/s（最近 {self.window:g}s 仅 {got:.0f} tok）"
                    f"持续 {self.hold:g}s，已杀进程组")
        return ""

    def stats(self, seconds: float) -> dict:
        with self.lock:
            return {"first_output_s": None if self.first is None else round(self.first, 1),
                    "out_tokens_est": round(self.total), "tok_per_s": round(self.total / seconds, 1) if seconds > 0 else None}


def run_role_once(role: str, prompt_path: Path, out_dir: Path, cmd: list[str], cwd: Path, timeout: int,
                  extra_env: dict[str, str] | None = None, model: str = "", stall: tuple[float, float, float, float] | None = None,
                  poll: float = 0.5) -> dict:
    raw = out_dir / f"{role}.jsonl"
    err = out_dir / f"{role}.err"
    started = time.time()
    rec: dict = {"role": role, "status": "ERROR", "detail": "", "counts": None, "cost_usd": None,
                 "started": dt.datetime.now().isoformat(timespec="seconds")}
    wd = Watchdog(*(stall or (0.0, 1.0, 0.0, 0.0)))
    cfg = Path(tempfile.mkdtemp(prefix=f"pdrs-review-cfg-{role}-"))
    env = {k: os.environ[k] for k in ENV_KEEP if k in os.environ}
    env.update(extra_env or {}, PWD=str(cwd), CLAUDE_CONFIG_DIR=str(cfg))  # 绑定 cwd 与 PWD（review-permissions）
    with prompt_path.open("rb") as fin, raw.open("w") as fraw, err.open("wb") as ferr:
        proc = subprocess.Popen(cmd, stdin=fin, stdout=subprocess.PIPE, stderr=ferr, cwd=cwd, env=env,
                                text=True, start_new_session=True)
        result: dict = {}
        violation: list[str] = []

        def pump() -> None:
            assert proc.stdout is not None
            for line in proc.stdout:
                try:
                    fraw.write(line)
                    fraw.flush()
                    try:
                        ev = json.loads(line)
                    except ValueError:
                        continue
                    if not isinstance(ev, dict):
                        raise TypeError(f"stream-json 行不是对象：{line[:80]!r}")
                    wd.feed(output_tokens(ev))
                    if ev.get("type") == "system" and ev.get("subtype") == "init":
                        tools = ev.get("tools")
                        # fail-closed：init 缺 tools 字段=无法证明只读，按越权处理（boundary 审 #4），
                        # 且不得让本分支自身抛异常杀死 pump（concurrency 审 r2 #1：set(None)/TypeError → fail-open）
                        if not isinstance(tools, list) or not tools or not all(isinstance(x, str) for x in tools):
                            violation.append("<missing-or-invalid-tools>")
                            os.killpg(proc.pid, signal.SIGKILL)
                            return
                        extra = sorted(set(tools) - set(ALLOWED_TOOLS))
                        if extra:
                            violation.extend(extra)
                            os.killpg(proc.pid, signal.SIGKILL)
                            return
                        result["init_seen"] = True
                        result["init_model"] = ev.get("model")
                    if ev.get("type") == "assistant":
                        msg = ev.get("message")
                        result.setdefault("resp_models", set()).add(msg.get("model") if isinstance(msg, dict) else None)
                    if ev.get("type") == "result":
                        result.update(ev)
                except Exception as e:  # noqa: BLE001 —— pump 必须先杀进程再死，绝不留下未验证的进程
                    violation.append(f"<pump-error:{type(e).__name__}>")
                    try:
                        os.killpg(proc.pid, signal.SIGKILL)
                    except ProcessLookupError:
                        pass
                    return

        t = threading.Thread(target=pump, daemon=True)
        t.start()
        deadline = time.monotonic() + timeout
        while True:
            try:
                proc.wait(timeout=max(0.01, min(poll, deadline - time.monotonic())))
                break
            except subprocess.TimeoutExpired:
                pass
            why = "TIMEOUT" if time.monotonic() >= deadline else ("STALLED" if (msg := wd.check()) else "")
            if why:
                try:
                    os.killpg(proc.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                proc.wait()
                rec["status"] = why
                rec["detail"] = f"超时 {timeout}s，已杀进程组" if why == "TIMEOUT" else msg
                break
        t.join(timeout=10)

    shutil.rmtree(cfg, ignore_errors=True)
    rec["seconds"] = round(time.time() - started, 1)
    rec["exit"] = proc.returncode
    rec.update(wd.stats(rec["seconds"]))
    if violation:
        rec["status"], rec["detail"] = "TOOLS_VIOLATION", f"init 工具集合超出白名单：{violation}"
        return rec
    if rec["status"] in RETRY_STATUSES:
        return rec
    if not result.get("init_seen"):
        rec["detail"] = "未见 init 事件，无法证明工具白名单生效"
        return rec
    # 防静默换模型：init 只是本地选择值，还要核后端每条响应的 model 与 modelUsage（审的不是声明的那家，结论不计）
    resp = result.get("resp_models") or set()
    usage = set((result.get("modelUsage") or {}).keys()) if isinstance(result.get("modelUsage"), dict) else set()
    if model and (result.get("init_model") != model or resp != {RESP_MODEL_ALIAS.get(model, model)} or not usage or usage != {model}):
        rec["detail"] = f"模型不符：声明 {model!r}，init {result.get('init_model')!r}，响应 {sorted(map(str, resp))}，用量 {sorted(usage)}"
        return rec
    rec["cost_usd"] = result.get("total_cost_usd")
    text = result.get("result") if isinstance(result.get("result"), str) else ""
    (out_dir / f"{role}.md").write_text(text or "", encoding="utf-8")
    if result.get("is_error") or result.get("subtype") not in (None, "success"):
        rec["detail"] = f"result subtype={result.get('subtype')} is_error={result.get('is_error')}"
        return rec
    rec["status"], rec["counts"], rec["detail"] = validate_final(text or "")
    return rec


def run_role(role: str, prompt_path: Path, out_dir: Path, cmd: list[str], cwd: Path, timeout: int,
             extra_env: dict[str, str] | None = None, model: str = "", stall: tuple[float, float, float, float] | None = None,
             retries: int = 0, backoff: tuple[float, ...] = RETRY_BACKOFF, poll: float = 0.5) -> dict:
    """单角色：STALLED / TIMEOUT 时按退避重跑，至多 retries 次；被重跑的那次原始流改名为 <角色>.attempt<k>.*。"""
    attempts: list[dict] = []
    while True:
        rec = run_role_once(role, prompt_path, out_dir, cmd, cwd, timeout, extra_env, model, stall, poll)
        attempts.append({k: rec.get(k) for k in ("status", "detail", "seconds", "started", "first_output_s",
                                                 "out_tokens_est", "tok_per_s")})
        k = len(attempts)
        if rec["status"] not in RETRY_STATUSES or k > retries:
            break
        for ext in ("jsonl", "err"):
            src = out_dir / f"{role}.{ext}"
            if src.exists():
                src.replace(out_dir / f"{role}.attempt{k}.{ext}")
        base = backoff[min(k - 1, len(backoff) - 1)] if backoff else 0.0
        time.sleep(base * random.uniform(0.8, 1.2))
    rec["attempts"] = attempts
    if len(attempts) > 1:
        trail = " → ".join(f"{a['status']} {a['seconds']}s" for a in attempts)
        rec["detail"] = f"尝试 {len(attempts)} 次：{trail}" + (f"；{rec['detail']}" if rec["detail"] else "")
    return rec


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--worktree", required=True, help="被审代码所在目录（评审进程 cwd）")
    ap.add_argument("--out", required=True, help="产物目录，须在主仓 _tmp/<YYYYMMDD>-<名>/ 下")
    src = ap.add_mutually_exclusive_group(required=True)
    src.add_argument("--base", help="git diff <base>...HEAD（在 worktree 内只读执行）")
    src.add_argument("--diff-file", help="现成 diff 文件")
    ap.add_argument("--paths", nargs="*", default=[], help="只审这些路径（大 diff 分批用）")
    ap.add_argument("--roles", default=",".join(ROLES), help="逗号分隔，默认四个全跑")
    ap.add_argument("--runner", choices=["zai", "anthropic"], default="zai",
                    help="zai=GLM（默认，经智谱 Anthropic 兼容接口）；anthropic=Claude 模型")
    ap.add_argument("--model", default=None, help="默认 zai→glm-5.3-mb，anthropic→claude-opus-5-5")
    ap.add_argument("--timeout", type=int, default=None,
                    help="每角色秒数；默认 zai→600，anthropic→1200（实测 sonnet ~3min、opus ~6min）")
    ap.add_argument("--budget-usd", type=float, default=3.0, help="每角色 --max-budget-usd（按 Anthropic 价目估算，zai 通道仅作上限）")
    ap.add_argument("--claude-bin", default="claude", help="测试时可换成假 CLI")
    ap.add_argument("--env-file", type=Path, default=None, help="取钥文件；默认本检出或主仓根的 .env.local（测试用隔离 fixture）")
    ap.add_argument("--dry-run", action="store_true", help="只渲染任务书与命令，不起进程")
    ap.add_argument("--serial", action="store_true",
                    help="角色逐个顺序跑（同一进程、同一 diff，产出单个四角色结果块）；并行全跑在本机会挂起时用")
    ap.add_argument("--stall-grace", type=float, default=STALL_GRACE, help=f"看门狗宽限秒数（默认 {STALL_GRACE:g}）")
    ap.add_argument("--stall-window", type=float, default=STALL_WINDOW, help=f"速率滑动窗口秒数（默认 {STALL_WINDOW:g}）")
    ap.add_argument("--stall-min-rate", type=float, default=None,
                    help="窗口内最低输出速率 tok/s；0 关闭看门狗（默认 anthropic→5，zai→0 不开）")
    ap.add_argument("--stall-hold", type=float, default=STALL_HOLD, help=f"低于阈值持续多少秒判 STALLED（默认 {STALL_HOLD:g}）")
    ap.add_argument("--retries", type=int, default=RETRIES, help=f"STALLED/TIMEOUT 自动重跑次数（默认 {RETRIES}；0 不重跑）")
    ap.add_argument("--retry-backoff", default=",".join(f"{x:g}" for x in RETRY_BACKOFF),
                    help="逗号分隔的各次重试前等待基数秒（实际 ×0.8–1.2 抖动；次数多于列表时取最后一个）")
    a = ap.parse_args()
    if a.timeout is None:
        a.timeout = DEFAULT_TIMEOUT[a.runner]
    if a.stall_min_rate is None:
        a.stall_min_rate = STALL_MIN_RATE[a.runner]
    try:
        backoff = tuple(float(x) for x in a.retry_backoff.split(",") if x.strip())
    except ValueError:
        print(f"run-review-roles: --retry-backoff 须为逗号分隔的秒数：{a.retry_backoff!r}", file=sys.stderr)
        return 2
    if (a.retries < 0 or a.stall_window <= 0 or a.stall_grace < 0 or a.stall_hold < 0 or a.stall_min_rate < 0
            or any(x < 0 for x in backoff)):
        print("run-review-roles: --retries/--stall-*/--retry-backoff 不得为负，--stall-window 须 > 0", file=sys.stderr)
        return 2
    stall = (a.stall_grace, a.stall_window, a.stall_min_rate, a.stall_hold)

    wt = Path(a.worktree).resolve()
    out = Path(a.out).resolve()
    if not wt.is_dir():
        print(f"run-review-roles: worktree 不存在：{wt}", file=sys.stderr)
        return 2
    try:
        out.relative_to(ROOT / "_tmp")
    except ValueError:
        print("run-review-roles: --out 必须在主仓 _tmp/ 下（证据不随 worktree 删除）", file=sys.stderr)
        return 2
    roles = [r.strip() for r in a.roles.split(",") if r.strip()]
    if not roles:
        sys.exit("run-review-roles: --roles 为空——零角色的「全部 PASS」是假的，拒绝运行")
    unknown = [r for r in roles if r not in ROLES]
    if unknown:
        print(f"run-review-roles: 未知角色 {unknown}", file=sys.stderr)
        return 2
    if not a.dry_run and shutil.which(a.claude_bin) is None and not Path(a.claude_bin).exists():
        print(f"run-review-roles: 找不到 {a.claude_bin}", file=sys.stderr)
        return 2
    out.mkdir(parents=True, exist_ok=True)

    diff_path = out / "diff.patch"
    patch_kv = ""
    if a.diff_file:
        shutil.copyfile(a.diff_file, diff_path)
        head = "（外部 diff 文件）"
    else:
        # diff 里可能有故意的非 UTF-8 字节（如测试夹具），按替换字符解码，不让评审在读 diff 时崩溃
        cp = subprocess.run(["git", "-C", str(wt), "diff", f"{a.base}...HEAD", "--", *a.paths],
                            capture_output=True, text=True, encoding="utf-8", errors="replace")
        if cp.returncode != 0:
            print(f"run-review-roles: git diff 失败：{cp.stderr.strip()}", file=sys.stderr)
            return 2
        diff_path.write_text(cp.stdout, encoding="utf-8")
        head = subprocess.run(["git", "-C", str(wt), "rev-parse", "HEAD"], capture_output=True, text=True).stdout.strip()
        # 整个 PR 的补丁指纹（不受 --paths 限制）：之后纯 rebase、内容不变时闸门凭它沿用本块
        fingerprint = patch_fingerprint(str(wt), a.base, head)
        patch_kv = f" patch={fingerprint}" if fingerprint else ""
    diff_lines = diff_path.read_text(encoding="utf-8", errors="replace").count("\n")
    if diff_lines == 0:
        print("run-review-roles: 空 diff，无可审内容", file=sys.stderr)
        return 2
    if diff_lines > LARGE_DIFF_LINES:
        print(f"run-review-roles: 提示 diff {diff_lines} 行 > {LARGE_DIFF_LINES}，建议用 --paths 分批", file=sys.stderr)

    common = (TEMPLATE_DIR / COMMON).read_text(encoding="utf-8")
    model = a.model or DEFAULT_MODEL[a.runner]
    if a.env_file is not None and not a.env_file.is_file():
        print(f"run-review-roles: --env-file {a.env_file} 不存在或不是文件（与「文件里没有该键」不同）", file=sys.stderr)
        return 2
    key = load_env_key(KEY_NAME[a.runner], a.env_file)
    if not key and not a.dry_run:
        print(f"run-review-roles: {a.runner} 通道缺 {KEY_NAME[a.runner]}（只认 .env.local / --env-file）；不回退到父环境、其它模型或登录态",
              file=sys.stderr)
        return 2
    extra_env = runner_env(key)
    cmd = build_cmd(a.claude_bin, model, a.budget_usd)
    jobs = []
    for role in roles:
        values = {"ROLE": role, "WORKTREE": str(wt), "DIFF_FILE": str(diff_path), "HEAD": head or "未知",
                  "BASE": a.base or "（外部 diff 文件）", "DIFF_LINES": str(diff_lines)}
        body = render(common, values) + "\n\n" + render((TEMPLATE_DIR / ROLES[role]).read_text(encoding="utf-8"), values)
        p = out / f"{role}.prompt.md"
        p.write_text(body, encoding="utf-8")
        jobs.append((role, p))

    (out / "command.txt").write_text(" ".join(cmd) + f"\n# cwd={wt} timeout={a.timeout}s stall={','.join(f'{x:g}' for x in stall)}"
                                     f" retries={a.retries} backoff={','.join(f'{x:g}' for x in backoff)}\n", encoding="utf-8")
    if a.dry_run:
        print(f"run-review-roles: dry-run，任务书在 {out}/*.prompt.md，命令见 command.txt")
        return 0

    results: dict[str, dict] = {}
    def guarded(r: str, p: Path) -> None:
        try:
            results[r] = run_role(r, p, out, cmd, wt, a.timeout, extra_env, model, stall, a.retries, backoff)
        except Exception as e:  # noqa: BLE001 —— 线程异常不得让 results 缺键、summary 丢失
            results[r] = {"role": r, "status": "ERROR", "detail": f"运行器异常 {type(e).__name__}", "counts": None}

    if a.serial:
        for r, p in jobs:
            guarded(r, p)
    else:
        threads = [threading.Thread(target=guarded, args=(r, p)) for r, p in jobs]
        for t in threads:
            t.start()
        for t in threads:
            t.join()

    (out / "status.json").write_text(json.dumps(results, ensure_ascii=False, indent=2), encoding="utf-8")
    ok = all(results[r]["status"] == "PASS" for r in roles)
    tot = {k: sum((results[r]["counts"] or {}).get(k, 0) for r in roles) for k in ("P0", "P1", "P2", "P3")}
    marker_roles = ",".join(f"{r}:{results[r]['status']}" for r in roles)
    # boundary 审 #2/#178（半修）：块内记录审计范围；gate 拒 partial 归 #178
    scope = "partial" if a.paths else "full"
    paths_kv = f" paths={','.join(a.paths)}" if a.paths else ""
    lines = [
        "## pr-ready 角色审计（scripts/run-review-roles.py）", "",
        f"<!-- pr-gate:v1 head={head}{patch_kv} roles={marker_roles} p0={tot['P0']} p1={tot['P1']} diff_lines={diff_lines} scope={scope}{paths_kv} runner={a.runner} model={model} readonly=tools -->", "",
        "| 角色 | 状态 | P0 | P1 | P2 | P3 | 秒 | 说明 |", "|---|---|---|---|---|---|---|---|",
    ]
    for r in roles:
        c = results[r]["counts"] or {}
        lines.append(f"| {r} | {results[r]['status']} | {c.get('P0', '-')} | {c.get('P1', '-')} | {c.get('P2', '-')} | "
                     f"{c.get('P3', '-')} | {results[r].get('seconds')} | {results[r]['detail'] or '—'} |")
    lines += ["", f"原件：`{out.relative_to(ROOT)}/`（<角色>.md 为结论，.jsonl/.err 为原始流；重跑过的角色另有 <角色>.attempt<k>.jsonl）"]
    (out / "summary.md").write_text("\n".join(lines) + "\n", encoding="utf-8")
    print(f"run-review-roles: {'全部 PASS' if ok else '有非 PASS'}；P0={tot['P0']} P1={tot['P1']} P2={tot['P2']}；见 {out}/summary.md")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
