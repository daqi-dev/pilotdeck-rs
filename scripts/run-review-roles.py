#!/usr/bin/env python3
"""run-review-roles.py — pr-ready 三角色 + simplify 四视角的通道无关执行器（替代 Explore 子代理与 /simplify）。

每个角色起一个 `claude -p` 只读进程（工具白名单 Read/Grep/Glob，MCP 全关，不落 session），并行跑；
默认 `--runner zai`：同一个 Claude Code 外壳接智谱的 Anthropic 兼容接口跑 GLM（密钥取 `.env.local` 的 ZAI_API_KEY，
只经环境变量传给子进程，不进命令行、不落盘），工具白名单与 init 校验照旧；`--runner anthropic` 用 Claude 模型
（同样只认 .env.local 的 ANTHROPIC_API_KEY，也经 llm-center 基址；`--runner minimax` 直连 MiniMax 官方
Anthropic 兼容接口（只认 .env.local 的 MINIMAX_API_KEY，基址 https://api.minimaxi.com/anthropic，不经 llm-center，
与 scripts/review-minimax.py 同一通道）；`--runner deepseek` 经 llm-center 跑 DeepSeek（只认 .env.local 的
DEEPSEEK_API_KEY，默认模型 deepseek-v4.1-flash-mb，与 scripts/review-deepseek.py 的 llm-center 通道同一入口；
flash 系在大 diff 上有复读前科，大 diff 谨慎用）；四通道都不读父环境里的密钥与基址）。子进程环境按白名单从零构造（父 shell 的令牌、网关头、provider 选择器一律不带），
CLAUDE_CONFIG_DIR 指向一次性空目录并只加载 user 设置源——用户/项目/本地 settings 里的 hooks 与插件都不生效
（真实 CLI 正负对照：项目 SessionStart hook 加隔离不触发、去隔离触发）。
原始事件流、stderr、最终文本、状态各自落盘。主进程不读角色全文，只汇总 summary.md（含 PR 用结果块）。

执行层只读证明：解析 stream-json 的 init 事件，工具集合不是白名单子集 → 立即杀进程，记 TOOLS_VIOLATION。
状态：PASS（有 final 且末行 VERDICT 合规）· PARTIAL（有 final 但缺 VERDICT/覆盖行）· TIMEOUT · ERROR · TOOLS_VIOLATION。
只有 PASS 计入闸门；exit0 或 type=result 不等于完成评审（state/memory/20261016-review-bounds.md）。
模型校验先认「API 错误响应」：响应 model 为 `<synthetic>`/空，或根本没有 assistant 事件时，
不判「模型不符」，转按内嵌原文分类——额度 429（找人：升级套餐 / 换 key）· ECONNRESET / Connection dropped
（查网络 / 代理，并自动重试一次；别的错误一律不重试，免得把额度 429 重试放大）。判错误响应只看 model
占位符这硬证据，正文里出现 `API Error:` 不算：评审本仓 scripts 时正文天天引用这串字样，靠它翻案会放过真换模型。
重试与首次共用 `--timeout` / `--budget-usd` 的每角色总额，不翻倍。

通道并发闸：每个角色起进程前按 `--runner` 取全局 flock 名额（zai 6 / minimax 16 / deepseek 16 / anthropic 2，
与 ops `scripts/fleet/fleet.py` 的 P4_CAP 同源），取不到打印 `[review-slot] waiting N/M` 排队；
不改「四角色并行」语义，只把它们排队。anthropic 只有 2 路：不加闸时完整四角色作业必然超限
（#1402 anthropic 四角色 25 min CPU 0%，实测是流式饥饿）。zai 卡的不是并发而是单 key 50 RPM：
4 角色门块实测 32–43 RPM，8 角色需 64–86 RPM 已超配额，且 GLM 实现 worker 共用同一把 ZAI_API_KEY，
因此 6 是「贴着配额但不放弃」的值（依据 `_tmp/20261105-channel-ramp/RAMP.md`，
决策卡 `state/decisions-20261105-gate-caps.md`）。

用法：
  python3 scripts/run-review-roles.py --worktree _build/wt-a61 --base origin/main --out _tmp/20261017-a61/gate
  python3 scripts/run-review-roles.py --worktree _build/wt-a61 --base origin/main --out _tmp/20261017-a61/gate --serial
  python3 scripts/run-review-roles.py --diff-file X.patch --worktree . --out ... --roles boundary --dry-run
修复轮（#3056 长尾收口卡「机检落点」）：
  --fixtype 枚举式|结构性|不适用 —— 修法类型写进结果块 fixtype= 键（闸门按字面三值校验）；
  --rerun —— 本轮是补跑（首跑争抢/超时后重跑）：status.json 每条 rec 标 rerun=true、summary 注明，
  且必须换独立 out 目录（目录里已有 summary.md / status.json / <角色>.md 原件即拒跑，#3056 H——
  齐块判定以原件为准，复跑不得覆盖）；结果块 head= 绑定当轮 head，head 变了即非复跑、按新头整块重起。
  rec 增加 resource_contention（bool，#3056 义务 6）：名额排队超时（SlotTimeout）/ 429 配额（ERR_QUOTA）/
  执行级 TIMEOUT 三项为 true，其余 false——判据 5 的争抢类排除不再靠人读 detail 文本。
退出码：0 全部 PASS · 1 有非 PASS · 2 参数/前置错误
"""
from __future__ import annotations

import argparse
import datetime as dt
import fcntl
import importlib.util
import json
import math
import os
import re
import shutil
import signal
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


def _gate_module():
    """懒加载 check-pr-gate.py（补丁指纹与 fixtype 字面三值的同一正本，不复制常量）。"""
    spec = importlib.util.spec_from_file_location("check_pr_gate", ROOT / "scripts" / "check-pr-gate.py")
    gate = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    spec.loader.exec_module(gate)
    return gate


def patch_fingerprint(wt: str, base: str, head: str) -> str:
    """补丁指纹与闸门比对共用 check-pr-gate.py 的同一实现（结果块 patch= 键）。"""
    return _gate_module().patch_fingerprint(wt, base, head)
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
# 网关对 GLM 在响应体里回显展示名而非请求名（modelUsage 仍是请求名）；别名表内两形放行、且大小写差异按
# casefold 放行（同 review-deepseek 口径）；表外 model 仍须逐字一致
# （10-25 实测 claude-opus-5-5 / claude-sonnet-5-5 经 llm-center 原样回显，原件 _tmp/20261025-anth-runner/smoke-*；
# 11-05 实测 MiniMax-M3.1-Flash 原样回显、deepseek-v4.1-flash-mb 回显 DeepSeek-V4.1-Flash，原件 _tmp/20261105-minimax-runner/）
RESP_MODEL_ALIAS = {"glm-5.3-mb": "GLM-5.3", "deepseek-v4.1-flash-mb": "DeepSeek-V4.1-Flash"}
SYNTHETIC_MODEL = "<synthetic>"  # 网关错误响应（额度 / 断连）的占位 model：不是换了模型，是真没跑成


def originals_in(out: Path) -> list[str]:
    """out 目录里已有的评审原件名（#3056 H 义务 7；#3056 fix-r1 DS simplify#1/sibling#4、DS concurrency#2）。

    - 角色名从 ROLES 派生（不双份维护）：增删/改名角色后守卫自动跟上，不会静默失效；
    - 清单含 <角色>.md / <角色>.jsonl / <角色>.err（含 .retryN 形）与 summary.md / status.json / diff.patch——
      *.jsonl/*.err 是原始流、diff.patch 是被审对象：父进程在写 <角色>.md 之前被 SIGKILL/SIGINT 时
      目录里只剩这些半成品，复跑 open("w") 会把它们截断，所以崩溃残留也算原件、一律拒覆盖。"""
    pat = re.compile("(" + "|".join(re.escape(r) for r in ROLES) + r")(\.retry\d+)?\.(md|jsonl|err)")
    return [p.name for p in out.iterdir()
            if p.is_file() and (p.name in ("summary.md", "status.json", "diff.patch") or pat.fullmatch(p.name))]


def error_rec(role: str, detail: str, contention: bool = False) -> dict:
    """异常/争抢分支的 rec 模板：字段与正常路径一致（含 resource_contention，#3056 fix-r1 DS sibling#5）。

    运行器异常时未知是否争抢，按「其余 false」口径置 False——status.json 的机读契约不因分支缺键
    （下游 rec.get(...) is False 的断言拿到 None 会不成立）。"""
    return {"role": role, "status": "ERROR", "detail": detail, "counts": None,
            "resource_contention": contention}
API_ERROR_MARK = "API Error:"  # 错误原文前缀，见 _tmp/20261106-issue-1527/review-r30-minimax/
ERR_QUOTA = "额度耗尽（429）"  # 找人：升级 Token Plan 套餐 / 换 key
ERR_CONN = "连接断开"  # 查网络 / 代理 / 节点
ERR_OTHER = "通道错误响应"
RETRY_MARKS = ("ECONNRESET", "Connection dropped")  # 只重试这一类；把额度 429 也重试只会放大占用
MAX_ERROR_TEXT = 8192  # 单个错误块最多留这么多字符：评审复读也不能把整段正文堆进内存
MAX_ERROR_BLOCKS = 3  # 只留前几个带 API Error 的块，够定位原因即可
MINIMAX_BASE_URL = "https://api.minimaxi.com/anthropic"  # MiniMax 官方 Anthropic 兼容接口，Claude Code 自行拼 /v1/messages（与 review-minimax.py 的 API 常量一致）
DEFAULT_MODEL = {"zai": "glm-5.3-mb", "anthropic": "claude-opus-5-5", "minimax": "MiniMax-M3.1-Flash",
                 "deepseek": "deepseek-v4.1-flash-mb"}  # 通道正本见 state/memory/20261015-model-runners.md
# 每角色默认超时按通道给：opus 实测 ~6 min/角色（10-25），600 s 余量不足；GLM、MiniMax-Flash 与 DS-Flash 维持原值
DEFAULT_TIMEOUT = {"zai": 600, "anthropic": 1200, "minimax": 600, "deepseek": 600}


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
KEY_NAME = {"zai": "ZAI_API_KEY", "anthropic": "ANTHROPIC_API_KEY", "minimax": "MINIMAX_API_KEY",
            "deepseek": "DEEPSEEK_API_KEY"}
RUNNER_BASE_URL = {"zai": LLM_CENTER_BASE_URL, "anthropic": LLM_CENTER_BASE_URL, "minimax": MINIMAX_BASE_URL,
                   "deepseek": LLM_CENTER_BASE_URL}


def runner_env(runner: str, key: str) -> dict[str, str]:
    """子进程的全部模型相关环境（白名单之外的父环境一概不带；minimax 直连官方接口，不经 llm-center）。"""
    return {"ANTHROPIC_API_KEY": key, "ANTHROPIC_BASE_URL": RUNNER_BASE_URL[runner]}
LARGE_DIFF_LINES = 500  # 超过即提示按 --paths 分批（state/memory/20261016-review-bounds.md）

# —— 通道并发闸（P4 池）——
# 上限与 ops scripts/fleet/fleet.py 的 P4_CAP 同源；数值按 11-05 通道台阶压测：
# deepseek/minimax 96 并发 96/96 无 429（原 4 是自设过低值）→ 16；
# zai 新 key（指纹 sha256[:8]=3bdac318）复测 96 并发 5459 RPM、滚动 600 RPM 连续 60 s 全程 0 次 429，
# 旧「单 key 50 RPM / 102503」结论作废（_tmp/20261105-zai-ramp2/20261105-2152.md）→ 6 升到 24
# （该 key 与约 10 个 GLM 实现 worker 共用，24 槽约 240 RPM 留余量，不取实测 48）；anthropic 未压测，保持 2。
# 可用 PDRS_REVIEW_SLOTS_<RUNNER> 覆盖，PDRS_REVIEW_SLOT_DIR 换锁目录（测试隔离用）。
RUNNER_SLOTS = {"zai": 24, "minimax": 16, "deepseek": 16, "anthropic": 2}
SLOT_WAIT_TIMEOUT = 3600  # 等名额上限（s），超时该角色记 ERROR（不静默死等）


class SlotTimeout(RuntimeError):
    pass


def slot_dir() -> Path:
    d = os.environ.get("PDRS_REVIEW_SLOT_DIR")
    if d:
        return Path(d)
    return Path("/private/tmp" if sys.platform == "darwin" else "/tmp") / "pdrs-review-gate"


def acquire_slot(runner: str) -> int | None:
    """跨进程取 runner 通道的全局名额（flock 槽位，memory「lock-use-flock」）；返回 fd，close 即释放。
    拿不到就排队并打印 `[review-slot] waiting N/M`；锁目录不可用时 fail-open（返回 None 并告警），
    不让一个临时目录问题把评审全堵死。"""
    n = int(os.environ.get(f"PDRS_REVIEW_SLOTS_{runner.upper()}", RUNNER_SLOTS.get(runner, 1)))
    if n <= 0:
        return None
    d = slot_dir() / runner
    try:
        d.mkdir(parents=True, exist_ok=True)
    except OSError as e:
        print(f"[review-slot] 名额目录不可用（{e}），跳过并发闸", file=sys.stderr)
        return None
    started, last, said = time.time(), 0.0, False
    while True:
        for i in range(n):
            try:
                fd = os.open(d / f"slot{i}", os.O_RDWR | os.O_CREAT, 0o644)
            except OSError as e:
                print(f"[review-slot] 名额文件不可用（{e}），跳过并发闸", file=sys.stderr)
                return None
            try:
                fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except OSError:
                os.close(fd)
                continue
            os.set_inheritable(fd, False)  # 锁 fd 不给子 claude 继承；主进程退出即释放
            print(f"[review-slot] take {i + 1}/{n}（{runner}）", file=sys.stderr, flush=True)
            return fd
        now = time.time()
        if not said:
            print(f"[review-slot] waiting {n}/{n}（{runner} 通道已满）", file=sys.stderr, flush=True)
            said = True
        elif now - last >= 300:
            print(f"[review-slot] waiting {n}/{n}（{runner}，已等 {int(now - started)} s）", file=sys.stderr, flush=True)
            last = now
        if now - started >= float(os.environ.get("PDRS_REVIEW_SLOT_TIMEOUT", SLOT_WAIT_TIMEOUT)):
            raise SlotTimeout(f"等 {runner} 名额超时 {int(now - started)} s（通道上限 {n}）")
        time.sleep(2)


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


def api_error_text(result: dict) -> str:
    """错误响应的内嵌原文：只认 model 为 `<synthetic>`/空的 assistant 块（pump 按 model 收，正文引用不算），
    没有时退回 result 字段、再退回最后一块正文——顺序固定，别让评审正文里的字样抢走分类。"""
    out: list[str] = []
    for t in (result.get("resp_texts") or []):
        if isinstance(t, str) and t.strip() and t not in out:
            out.append(t)
    if not out:
        for t in (result.get("result"), result.get("last_text")):
            if isinstance(t, str) and t.strip():
                out.append(t[:MAX_ERROR_TEXT])
                break
    return " ".join(out).strip()


def classify_api_error(text: str) -> str:
    """按原文分类；连接类先判（真断连该重试，不该被正文里偶然出现的 429 抢走）。"""
    low = text.lower()
    if any(m in text for m in RETRY_MARKS) or "连接" in text or "econnrefused" in low or "etimedout" in low:
        return ERR_CONN
    # 429 / 1308 用词边界：错误码 / request id 里的 "14293"、"req_130812" 不得被当额度
    if (re.search(r"(?<!\d)429(?!\d)", text) or "用量上限" in text or "额度" in text
            or "token plan" in low or "quota" in low or "usage limit" in low or re.search(r"(?<!\d)1308(?!\d)", text)):
        return ERR_QUOTA
    return ERR_OTHER


def one_line(text: str, limit: int = 300) -> str:
    """压成一行并限长（detail 会变长，也不该整段堆着）。完整原文仍在 *.jsonl / *.err 里。
    防伪造闸门结果块的处理不在这里：detail 有多个来源，统一在写 summary 的 `cell` 里做。"""
    flat = " ".join(text.split())
    return flat if len(flat) <= limit else flat[:limit] + "…"


def cell(text: str, limit: int = 300) -> str:
    """summary 表格单元格的唯一出口：压行、转义竖线、拆掉能伪造 pr-gate 结果块的字样并限长。
    所有 detail 都走这里，新加的 detail 来源不会漏掉防伪造。"""
    flat = " ".join(text.split()).replace("<!--", "< !--").replace("-->", "-- >").replace("|", "\\|")
    return flat if len(flat) <= limit else flat[:limit] + "…"


def keep_error_text(result: dict, txt: str) -> None:
    """把带 `API Error:` 的正文块收进 result["resp_texts"]：单块与条数都封顶，长流复读也堆不满内存。"""
    kept = result.setdefault("resp_texts", [])
    if len(kept) < MAX_ERROR_BLOCKS:
        kept.append(txt[:MAX_ERROR_TEXT])


def run_role(role: str, prompt_path: Path, out_dir: Path, cmd: list[str], cwd: Path, timeout: int,
             extra_env: dict[str, str] | None = None, model: str = "", budget: float = 0.0) -> dict:
    """跑一个角色；连接断开（ECONNRESET / Connection dropped）自动重试一次，其它错误不重试。
    重试共用本角色的总额度与总时限（`--budget-usd` / `--timeout` 都是「每角色」），不翻倍。"""
    rec = run_role_once(role, prompt_path, out_dir, cmd, cwd, timeout, extra_env, model)
    if not rec.pop("retry_connection", False):
        return rec
    first = rec.get("api_error_text") or rec["detail"]
    spent = rec.get("cost_usd") if isinstance(rec.get("cost_usd"), (int, float)) else 0.0
    # 向上取整已用秒数：余量宁可少给，也别让两次尝试合计超出「每角色总秒数」
    left_seconds = timeout - math.ceil(rec.get("seconds") or 0)
    left_budget = budget - spent
    if left_seconds <= 0 or left_budget < 1e-4:
        rec["retried"] = 0
        rec["detail"] = f"{rec['detail']}（剩余时限 {max(left_seconds, 0)}s / 预算 {max(left_budget, 0.0):.4f} USD 不足，未重试）"
        return rec
    # 定点格式传余款：:g 在极小值上会写出 5e-05 这种科学计数，也得防四舍五入向上超
    left = f"{left_budget:.6f}".rstrip("0").rstrip(".")
    retry_cmd = [left if i and cmd[i - 1] == "--max-budget-usd" else tok for i, tok in enumerate(cmd)]
    rec2 = run_role_once(role, prompt_path, out_dir, retry_cmd, cwd, left_seconds, extra_env, model, suffix=".retry1")
    rec2.pop("retry_connection", None)
    if rec2.get("api_error_kind") == ERR_CONN:
        rec2["detail"] = f"{ERR_CONN}（重试 1 次后仍失败）：{rec2.get('api_error_text') or '（无内嵌原文）'}"
    else:
        rec2["detail"] = f"首次 {ERR_CONN}（{first}），已自动重试 1 次；{rec2['detail'] or 'PASS'}"
    rec2["retried"] = 1
    # 两次尝试的墙钟与花费都要算上：只记第二次会把一个角色的真实占用低估一半
    rec2["seconds"] = round((rec.get("seconds") or 0) + (rec2.get("seconds") or 0), 1)
    rec2["started"] = rec.get("started") or rec2.get("started")
    costs = [c for c in (rec.get("cost_usd"), rec2.get("cost_usd")) if isinstance(c, (int, float))]
    rec2["cost_usd"] = sum(costs) if costs else None
    return rec2


def run_role_once(role: str, prompt_path: Path, out_dir: Path, cmd: list[str], cwd: Path, timeout: int,
                  extra_env: dict[str, str] | None = None, model: str = "", suffix: str = "") -> dict:
    raw = out_dir / f"{role}{suffix}.jsonl"
    err = out_dir / f"{role}{suffix}.err"
    started = time.time()
    rec: dict = {"role": role, "status": "ERROR", "detail": "", "counts": None, "cost_usd": None,
                 "started": dt.datetime.now().isoformat(timespec="seconds"),
                 "resource_contention": False}  # #3056 义务 6：争抢类机读（SlotTimeout / 429 配额 / TIMEOUT）
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
                        # 错误响应（额度 / 断连）没有正常评审正文，原因只在 assistant 文本块里（real shape 见
                        # _tmp/20261106-issue-1527/review-r30-minimax/：model=<synthetic> + "API Error: ..."）。
                        # 只留有界内容：正常正文只保留最后一块（兜底用），带 API Error 的留前几块。
                        if isinstance(msg, dict):
                            # 只收 model 是占位符的那条事件的正文：评审本仓 scripts 时正文天天引用
                            # "API Error: ... ECONNRESET"，按正文收会把真 429 误判成断连并白重试一次。
                            err_model = msg.get("model") in (None, "", SYNTHETIC_MODEL)
                            for blk in msg.get("content") or []:
                                if isinstance(blk, dict) and isinstance(blk.get("text"), str):
                                    txt = blk["text"]
                                    if err_model and API_ERROR_MARK in txt:
                                        keep_error_text(result, txt)
                                    else:
                                        result["last_text"] = txt[:MAX_ERROR_TEXT]
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
        try:
            proc.wait(timeout=timeout)
        except subprocess.TimeoutExpired:
            os.killpg(proc.pid, signal.SIGKILL)
            proc.wait()
            rec["status"] = "TIMEOUT"
            rec["detail"] = f"超时 {timeout}s，已杀进程组"
            rec["resource_contention"] = True  # #3056：执行级 TIMEOUT 属争抢类（通道慢/占用导致的超时）
        t.join(timeout=10)

    shutil.rmtree(cfg, ignore_errors=True)
    rec["seconds"] = round(time.time() - started, 1)
    rec["exit"] = proc.returncode
    if violation:
        rec["status"], rec["detail"] = "TOOLS_VIOLATION", f"init 工具集合超出白名单：{violation}"
        return rec
    if rec["status"] == "TIMEOUT":
        return rec
    if not result.get("init_seen"):
        rec["detail"] = "未见 init 事件，无法证明工具白名单生效"
        return rec
    # 防静默换模型：init 只是本地选择值，还要核后端每条响应的 model 与 modelUsage（审的不是声明的那家，结论不计）
    resp = result.get("resp_models") or set()
    usage = set((result.get("modelUsage") or {}).keys()) if isinstance(result.get("modelUsage"), dict) else set()
    # llm-center 的响应 model 名不稳定：同一后端有时回规范名（DeepSeek-V4.1-Flash）、有时原样回请求名
    # （deepseek-v4.1-flash-mb），都指同一家（2026-11-12 实测：13:09 通过的 run 两种混出、14:3x 的 run 全原样回）。
    # 两形都算同一家；换谱系（别的名字）仍被 issubset 拒。
    allowed = {RESP_MODEL_ALIAS.get(model, model), model}
    rec["cost_usd"] = result.get("total_cost_usd")  # 先取：下面每条早退分支都别再漏记花费（重试要合计两次）
    if model and result.get("init_model") != model:
        rec["detail"] = f"模型不符：声明 {model!r}，init {result.get('init_model')!r}，响应 {sorted(map(str, resp))}，用量 {sorted(usage)}"
        return rec
    # 先认「API 错误响应」再判模型：额度 / 断连时网关回 model=<synthetic>、正文是 "API Error: ..."，
    # 若先比 model 就把真实原因（429 / ECONNRESET）误报成「模型不符」（历史 69 条 ERROR 即此）。
    # 判定只用 model 占位符「有 <synthetic>/空」这一条硬证据：正文里出现 "API Error:" 不算——评审本仓
    # scripts 时正文天天引用这串字样，靠它翻案会把真换模型放过去，还会按正文里的 ECONNRESET 白重试一次。
    err_text = api_error_text(result)
    synthetic = any(m in (None, "", SYNTHETIC_MODEL) for m in resp)
    real_resp = {m for m in resp if m not in (None, "", SYNTHETIC_MODEL)}
    failed = bool(result.get("is_error")) or result.get("subtype") not in (None, "success")
    # 断连重试只认「真错误原文」：正文引用里出现 ECONNRESET 不算，别为一句引用白跑一次
    has_mark = API_ERROR_MARK in err_text
    if model and not resp:  # 连 assistant 事件都没有：模型根本没用上，报「模型不符」同样是错怪实现方
        kind = classify_api_error(err_text)
        # 这里没有 assistant 正文可引：只有确属 API 错误原文才写进 detail，别把 final 里的评审文字当错误
        why = one_line(err_text) if has_mark else "通道没有返回任何模型响应（无 assistant 事件）"
        rec["detail"] = f"{kind}：{why}（不是模型不符）"
        rec["api_error_kind"] = kind
        rec["api_error_text"] = one_line(err_text)
        rec["resource_contention"] = kind == ERR_QUOTA  # #3056：429 / 配额耗尽属争抢类
        rec["retry_connection"] = kind == ERR_CONN and has_mark and any(mark in err_text for mark in RETRY_MARKS)
        return rec
    # 只有「这次真的没跑成」才按错误响应处理：流内出现过占位符、但最后是正常成功的 result
    # （CLI 内部重试成功）时照常走模型/用量/VERDICT 校验，不许整角色误杀（GLM boundary P1）
    if model and synthetic and failed:
        kind = classify_api_error(err_text)
        rec["detail"] = f"{kind}：{one_line(err_text) or '（错误响应没有内嵌原文）'}"
        rec["api_error_kind"] = kind
        rec["api_error_text"] = one_line(err_text)
        rec["resource_contention"] = kind == ERR_QUOTA  # #3056：429 / 配额耗尽属争抢类
        rec["retry_connection"] = kind == ERR_CONN and has_mark and any(mark in err_text for mark in RETRY_MARKS)
        return rec
    # real_resp 为空（只有 <synthetic>/None 占位）时 issubset 恒真，守卫退化为只靠 usage 一条证据——
    # 拒：成功 result 必须至少带一条真实 assistant 响应名，否则谱系无从证明（边界 #1）。
    # 大小写与 review-deepseek 的 casefold 同口径，但只对别名表登记的已知两形生效：未登记的 --model
    # （如漏 -mb 的 glm-5.3，网关也应答）若也走 casefold，展示名回显会被误放行（r2 boundary#1），表外保持逐字。
    if model in RESP_MODEL_ALIAS:
        allowed_cf = {str(m).casefold() for m in allowed}
        resp_ok = {str(m).casefold() for m in real_resp}.issubset(allowed_cf)
        usage_ok = {str(m).casefold() for m in usage}.issubset(allowed_cf)
    else:
        resp_ok = real_resp.issubset(allowed)
        usage_ok = usage.issubset(allowed)
    if model and (not real_resp or not resp_ok or not usage or not usage_ok):
        rec["detail"] = f"模型不符：声明 {model!r}，init {result.get('init_model')!r}，响应 {sorted(map(str, resp))}，用量 {sorted(usage)}（合法形 {sorted(allowed)}）"
        return rec
    text = result.get("result") if isinstance(result.get("result"), str) else ""
    (out_dir / f"{role}.md").write_text(text or "", encoding="utf-8")
    if result.get("is_error") or result.get("subtype") not in (None, "success"):
        rec["detail"] = f"result subtype={result.get('subtype')} is_error={result.get('is_error')}"
        return rec
    rec["status"], rec["counts"], rec["detail"] = validate_final(text or "")
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
    ap.add_argument("--runner", choices=["zai", "anthropic", "minimax", "deepseek"], default="zai",
                    help="zai=GLM（默认，经智谱 Anthropic 兼容接口）；anthropic=Claude 模型；minimax=MiniMax（直连官方接口）；"
                         "deepseek=DeepSeek（经 llm-center；flash 系大 diff 有复读前科，大 diff 谨慎用）")
    ap.add_argument("--model", default=None,
                    help="默认 zai→glm-5.3-mb，anthropic→claude-opus-5-5，minimax→MiniMax-M3.1-Flash，deepseek→deepseek-v4.1-flash-mb")
    ap.add_argument("--timeout", type=int, default=None,
                    help="每角色总秒数（含重试）；默认 zai→600，anthropic→1200（实测 sonnet ~3min、opus ~6min），minimax/deepseek→600")
    ap.add_argument("--budget-usd", type=float, default=3.0, help="每角色 --max-budget-usd 总额（含重试；按 Anthropic 价目估算，zai 通道仅作上限）")
    ap.add_argument("--claude-bin", default="claude", help="测试时可换成假 CLI")
    ap.add_argument("--env-file", type=Path, default=None, help="取钥文件；默认本检出或主仓根的 .env.local（测试用隔离 fixture）")
    ap.add_argument("--dry-run", action="store_true", help="只渲染任务书与命令，不起进程")
    ap.add_argument("--fixtype", choices=_gate_module().FIXTYPE_VALUES, default=None,
                    help="修复轮修法类型（#3056）：写进结果块 fixtype= 键；「不适用」仅当 diff 只碰文档白名单（闸门按 ci_scope 语义复算）")
    ap.add_argument("--rerun", action="store_true",
                    help="本轮是补跑（首跑争抢/超时后重跑）：rec 标 rerun=true；须换独立 out 目录（原件不得覆盖，#3056 H）")
    ap.add_argument("--serial", action="store_true",
                    help="角色逐个顺序跑（同一进程、同一 diff，产出单个四角色结果块）；并行全跑在本机会挂起时用")
    a = ap.parse_args()
    if a.timeout is None:
        a.timeout = DEFAULT_TIMEOUT[a.runner]

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
    # #3056 H（义务 7）：齐块判定以原件（<角色>.md / .jsonl / .err / summary.md / status.json /
    # diff.patch）为准——目录里已有原件即拒跑（复跑/分批须换独立 out 目录，覆盖会让原轮证据变成复跑字节）。
    originals = originals_in(out)
    if originals:
        print(f"run-review-roles: out 目录已有评审原件 {sorted(originals)}——复跑/分批须换独立 out 目录（#3056 H，不得覆盖原件）",
              file=sys.stderr)
        return 2

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
    extra_env = runner_env(a.runner, key)
    cmd = build_cmd(a.claude_bin, model, a.budget_usd)
    jobs = []
    for role in roles:
        values = {"ROLE": role, "WORKTREE": str(wt), "DIFF_FILE": str(diff_path), "HEAD": head or "未知",
                  "BASE": a.base or "（外部 diff 文件）", "DIFF_LINES": str(diff_lines)}
        body = render(common, values) + "\n\n" + render((TEMPLATE_DIR / ROLES[role]).read_text(encoding="utf-8"), values)
        p = out / f"{role}.prompt.md"
        p.write_text(body, encoding="utf-8")
        jobs.append((role, p))

    (out / "command.txt").write_text(" ".join(cmd) + f"\n# cwd={wt} timeout={a.timeout}s\n", encoding="utf-8")
    if a.dry_run:
        print(f"run-review-roles: dry-run，任务书在 {out}/*.prompt.md，命令见 command.txt"
              f"（并发闸：{a.runner} 上限 {RUNNER_SLOTS.get(a.runner, 1)}/通道，实跑取不到名额会排队）")
        return 0

    results: dict[str, dict] = {}
    def guarded(r: str, p: Path) -> None:
        try:
            fd = acquire_slot(a.runner)
        except SlotTimeout as e:
            # #3056：名额排队超时属争抢类（首次排除、补跑后仍争抢计入）
            results[r] = error_rec(r, str(e), contention=True)
            return
        try:
            results[r] = run_role(r, p, out, cmd, wt, a.timeout, extra_env, model, a.budget_usd)
        except Exception as e:  # noqa: BLE001 —— 线程异常不得让 results 缺键、summary 丢失
            results[r] = error_rec(r, f"运行器异常 {type(e).__name__}")
        finally:
            if fd is not None:
                os.close(fd)

    if a.serial:
        for r, p in jobs:
            guarded(r, p)
    else:
        threads = [threading.Thread(target=guarded, args=(r, p)) for r, p in jobs]
        for t in threads:
            t.start()
        for t in threads:
            t.join()

    if a.rerun:
        for r in results:
            results[r]["rerun"] = True
    (out / "status.json").write_text(json.dumps(results, ensure_ascii=False, indent=2), encoding="utf-8")
    ok = all(results[r]["status"] == "PASS" for r in roles)
    tot = {k: sum((results[r]["counts"] or {}).get(k, 0) for r in roles) for k in ("P0", "P1", "P2", "P3")}
    marker_roles = ",".join(f"{r}:{results[r]['status']}" for r in roles)
    # boundary 审 #2/#178（半修）：块内记录审计范围；gate 拒 partial 归 #178
    scope = "partial" if a.paths else "full"
    paths_kv = f" paths={','.join(a.paths)}" if a.paths else ""
    fixtype_kv = f" fixtype={a.fixtype}" if a.fixtype else ""
    rerun_kv = " rerun=true" if a.rerun else ""
    lines = [
        "## pr-ready 角色审计（scripts/run-review-roles.py）", "",
        f"<!-- pr-gate:v1 head={head}{patch_kv} roles={marker_roles} p0={tot['P0']} p1={tot['P1']} diff_lines={diff_lines} scope={scope}{paths_kv}{fixtype_kv}{rerun_kv} runner={a.runner} model={model} readonly=tools -->", "",
        "| 角色 | 状态 | P0 | P1 | P2 | P3 | 秒 | 说明 |", "|---|---|---|---|---|---|---|---|",
    ]
    for r in roles:
        c = results[r]["counts"] or {}
        detail = cell(results[r]["detail"] or "—")
        lines.append(f"| {r} | {results[r]['status']} | {c.get('P0', '-')} | {c.get('P1', '-')} | {c.get('P2', '-')} | "
                     f"{c.get('P3', '-')} | {results[r].get('seconds')} | {detail} |")
    lines += ["", f"原件：`{out.relative_to(ROOT)}/`（<角色>.md 为结论，.jsonl/.err 为原始流）"]
    if a.rerun:
        lines += ["", f"**复跑轮（--rerun，#3056 H）**：绑定当轮 head=`{head}`；本目录为复跑证据，原轮目录原件未动；"
                      f"争抢类 rec 带 resource_contention（首次争抢排除、补跑后仍争抢按前提③计入）。"]
    (out / "summary.md").write_text("\n".join(lines) + "\n", encoding="utf-8")
    print(f"run-review-roles: {'全部 PASS' if ok else '有非 PASS'}；P0={tot['P0']} P1={tot['P1']} P2={tot['P2']}；见 {out}/summary.md")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
