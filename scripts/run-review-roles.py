#!/usr/bin/env python3
"""run-review-roles.py — pr-ready 三角色 + simplify 四视角的通道无关执行器（替代 Explore 子代理与 /simplify）。

每个角色起一个 `claude -p` 只读进程（工具白名单 Read/Grep/Glob，MCP 全关，不落 session），并行跑；
原始事件流、stderr、最终文本、状态各自落盘。主进程不读角色全文，只汇总 summary.md（含 PR 用结果块）。

执行层只读证明：解析 stream-json 的 init 事件，工具集合不是白名单子集 → 立即杀进程，记 TOOLS_VIOLATION。
状态：PASS（有 final 且末行 VERDICT 合规）· PARTIAL（有 final 但缺 VERDICT/覆盖行）· TIMEOUT · ERROR · TOOLS_VIOLATION。
只有 PASS 计入闸门；exit0 或 type=result 不等于完成评审（state/memory/20261016-review-bounds.md）。

用法：
  python3 scripts/run-review-roles.py --worktree _build/wt-a61 --base origin/main --out _tmp/20261017-a61/gate
  python3 scripts/run-review-roles.py --diff-file X.patch --worktree . --out ... --roles boundary --dry-run
退出码：0 全部 PASS · 1 有非 PASS · 2 参数/前置错误
"""
from __future__ import annotations

import argparse
import datetime as dt
import json
import os
import shutil
import signal
import subprocess
import sys
import threading
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
TEMPLATE_DIR = ROOT / ".claude" / "workflows"
COMMON = "review-common.md"
ROLES = {
    "sibling": "review-role-sibling.md",
    "concurrency": "review-role-concurrency.md",
    "boundary": "review-role-boundary.md",
    "simplify": "review-role-simplify.md",
}
ALLOWED_TOOLS = ["Read", "Grep", "Glob"]
LARGE_DIFF_LINES = 500  # 超过即提示按 --paths 分批（state/memory/20261016-review-bounds.md）


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


def run_role(role: str, prompt_path: Path, out_dir: Path, cmd: list[str], cwd: Path, timeout: int) -> dict:
    raw = out_dir / f"{role}.jsonl"
    err = out_dir / f"{role}.err"
    started = time.time()
    rec: dict = {"role": role, "status": "ERROR", "detail": "", "counts": None, "cost_usd": None,
                 "started": dt.datetime.now().isoformat(timespec="seconds")}
    env = dict(os.environ, PWD=str(cwd))  # 绑定 cwd 与 PWD（state/memory/20261016-review-permissions.md）
    with prompt_path.open("rb") as fin, raw.open("w") as fraw, err.open("wb") as ferr:
        proc = subprocess.Popen(cmd, stdin=fin, stdout=subprocess.PIPE, stderr=ferr, cwd=cwd, env=env,
                                text=True, start_new_session=True)
        result: dict = {}
        violation: list[str] = []

        def pump() -> None:
            assert proc.stdout is not None
            for line in proc.stdout:
                fraw.write(line)
                fraw.flush()
                try:
                    ev = json.loads(line)
                except ValueError:
                    continue
                if ev.get("type") == "system" and ev.get("subtype") == "init":
                    tools = ev.get("tools")
                    # fail-closed：init 缺 tools 字段=无法证明只读，按越权处理（boundary 审 #4）
                    if not isinstance(tools, list) or not tools:
                        tools = None
                    extra = sorted(set(tools) - set(ALLOWED_TOOLS))
                    if extra:
                        violation.extend(extra)
                        os.killpg(proc.pid, signal.SIGKILL)
                        return
                    result["init_seen"] = True
                if ev.get("type") == "result":
                    result.update(ev)

        t = threading.Thread(target=pump, daemon=True)
        t.start()
        try:
            proc.wait(timeout=timeout)
        except subprocess.TimeoutExpired:
            os.killpg(proc.pid, signal.SIGKILL)
            proc.wait()
            rec["status"] = "TIMEOUT"
            rec["detail"] = f"超时 {timeout}s，已杀进程组"
        t.join(timeout=10)

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
    rec["cost_usd"] = result.get("total_cost_usd")
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
    ap.add_argument("--model", default="claude-sonnet-5-5")
    ap.add_argument("--timeout", type=int, default=600, help="每角色秒数；实测 sonnet ~3min、opus ~6min")
    ap.add_argument("--budget-usd", type=float, default=3.0, help="每角色 --max-budget-usd")
    ap.add_argument("--claude-bin", default="claude", help="测试时可换成假 CLI")
    ap.add_argument("--dry-run", action="store_true", help="只渲染任务书与命令，不起进程")
    a = ap.parse_args()

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
    if a.diff_file:
        shutil.copyfile(a.diff_file, diff_path)
        head = "（外部 diff 文件）"
    else:
        cp = subprocess.run(["git", "-C", str(wt), "diff", f"{a.base}...HEAD", "--", *a.paths],
                            capture_output=True, text=True)
        if cp.returncode != 0:
            print(f"run-review-roles: git diff 失败：{cp.stderr.strip()}", file=sys.stderr)
            return 2
        diff_path.write_text(cp.stdout, encoding="utf-8")
        head = subprocess.run(["git", "-C", str(wt), "rev-parse", "HEAD"], capture_output=True, text=True).stdout.strip()
    diff_lines = diff_path.read_text(encoding="utf-8").count("\n")
    if diff_lines == 0:
        print("run-review-roles: 空 diff，无可审内容", file=sys.stderr)
        return 2
    if diff_lines > LARGE_DIFF_LINES:
        print(f"run-review-roles: 提示 diff {diff_lines} 行 > {LARGE_DIFF_LINES}，建议用 --paths 分批", file=sys.stderr)

    common = (TEMPLATE_DIR / COMMON).read_text(encoding="utf-8")
    cmd = build_cmd(a.claude_bin, a.model, a.budget_usd)
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
        print(f"run-review-roles: dry-run，任务书在 {out}/*.prompt.md，命令见 command.txt")
        return 0

    results: dict[str, dict] = {}
    threads = [threading.Thread(target=lambda r=r, p=p: results.__setitem__(r, run_role(r, p, out, cmd, wt, a.timeout)))
               for r, p in jobs]
    for t in threads:
        t.start()
    for t in threads:
        t.join()

    (out / "status.json").write_text(json.dumps(results, ensure_ascii=False, indent=2), encoding="utf-8")
    ok = all(results[r]["status"] == "PASS" for r in roles)
    tot = {k: sum((results[r]["counts"] or {}).get(k, 0) for r in roles) for k in ("P0", "P1", "P2", "P3")}
    marker_roles = ",".join(f"{r}:{results[r]['status']}" for r in roles)
    lines = [
        "## pr-ready 角色审计（scripts/run-review-roles.py）", "",
        f"<!-- pr-gate:v1 head={head} roles={marker_roles} p0={tot['P0']} p1={tot['P1']} -->", "",
        "| 角色 | 状态 | P0 | P1 | P2 | P3 | 秒 | 说明 |", "|---|---|---|---|---|---|---|---|",
    ]
    for r in roles:
        c = results[r]["counts"] or {}
        lines.append(f"| {r} | {results[r]['status']} | {c.get('P0', '-')} | {c.get('P1', '-')} | {c.get('P2', '-')} | "
                     f"{c.get('P3', '-')} | {results[r].get('seconds')} | {results[r]['detail'] or '—'} |")
    lines += ["", f"原件：`{out.relative_to(ROOT)}/`（<角色>.md 为结论，.jsonl/.err 为原始流）"]
    (out / "summary.md").write_text("\n".join(lines) + "\n", encoding="utf-8")
    print(f"run-review-roles: {'全部 PASS' if ok else '有非 PASS'}；P0={tot['P0']} P1={tot['P1']} P2={tot['P2']}；见 {out}/summary.md")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
