#!/usr/bin/env python3
"""test-review-roles.py — run-review-roles.py 与 check-pr-gate 的本地测试（假 claude CLI，不花模型钱，Linux/macOS 均可跑）。

用法：python3 -I scripts/test-review-roles.py   退出码 0 全过 · 1 有失败
"""
from __future__ import annotations

import datetime as dt
import importlib.util
import json
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
RUNNER = ROOT / "scripts" / "run-review-roles.py"
ROLES = "sibling,concurrency,boundary,simplify"
FINAL = "无问题\n覆盖: 全部\nVERDICT: P0=0 P1=0 P2=0 P3=0"

# 假 claude：按同目录 fake.json 的 mode 输出 stream-json（运行器从零构造子进程环境，不能靠环境变量传参）；
# 把收到的 argv 与环境记到 log 供断言
FAKE = r'''#!/usr/bin/env python3
import json, os, sys, time
sys.stdin.read()
conf = json.load(open(os.path.join(os.path.dirname(os.path.abspath(sys.argv[0])), "fake.json")))
mode = conf["mode"]
argv = sys.argv[1:]
def opt(name):
    return argv[argv.index(name) + 1] if name in argv else None
model = opt("--model")
# 工具集合按 argv 推导：任何一条约束缺席都等价于「没限住」，报出白名单外的工具
locked = (opt("--tools") == "Read,Grep,Glob" and opt("--allowedTools") == "Read,Grep,Glob"
          and opt("--permission-mode") == "dontAsk" and "--strict-mcp-config" in argv and opt("--setting-sources") == "user")
tools = ["Read", "Grep", "Glob"] if locked else ["Read", "Grep", "Glob", "Bash", "Edit"]
if mode == "extra_tool":
    tools = tools + ["Bash"]
cfg = os.environ.get("CLAUDE_CONFIG_DIR", "")
with open(conf["log"], "a") as f:
    f.write(json.dumps({"argv": argv, "env": sorted(os.environ), "base": os.environ.get("ANTHROPIC_BASE_URL", ""),
                        # 只记比较结果，不记密钥原值（测试日志不得落任何密钥）
                        "key_is": next((n for n, v in conf["keys"].items() if v == os.environ.get("ANTHROPIC_API_KEY")),
                                       "other" if os.environ.get("ANTHROPIC_API_KEY") else "unset"), "cfg": cfg,
                        "cfg_empty": bool(cfg) and os.path.isdir(cfg) and not os.listdir(cfg)}) + "\n")
init_model = "wrong-model-x" if mode == "wrong_init" else model
print(json.dumps({"type": "system", "subtype": "init", "tools": tools, "model": init_model}), flush=True)
trace = conf.get("trace")
if trace:
    with open(trace, "a") as f:
        f.write(f"start {time.time()}\n")
    time.sleep(0.3)
def first_claim(path):  # 原子占位：并行四角色里只有一个抢到（O_EXCL）
    try:
        os.close(os.open(path, os.O_CREAT | os.O_EXCL | os.O_WRONLY))
        return True
    except FileExistsError:
        return False
hang_first = conf.get("hang_first")
if hang_first and first_claim(hang_first):
    mode = "hang"
drip_first = conf.get("drip_first")  # 首次尝试慢滴漏，之后正常（模拟时段性流式饥饿后重试即过）
if drip_first and first_claim(drip_first):
    mode = "drip"
def thinking(delta):
    print(json.dumps({"type": "system", "subtype": "thinking_tokens", "estimated_tokens_delta": delta}), flush=True)
if mode == "hang":
    time.sleep(30)
if mode == "drip":  # 1 tok / 0.2 s ≈ 5 tok/s，持续 30 s：活着但几乎不出字
    for _ in range(150):
        thinking(1)
        time.sleep(0.2)
if mode == "tool_flood":  # 只有本地工具结果在刷（tool_result 不是模型输出），模型本身一字不出
    for _ in range(100):
        print(json.dumps({"type": "user", "message": {"role": "user", "content": [
            {"type": "tool_result", "tool_use_id": "t", "content": [{"type": "text", "text": "x" * 4000}]},
            {"type": "text", "text": "y" * 4000}]}}), flush=True)
        time.sleep(0.1)
if mode == "steady":  # 50 tok / 0.1 s ≈ 500 tok/s，持续 3 s：正常速率但跑得比宽限 + 持续期长
    for _ in range(30):
        thinking(50)
        time.sleep(0.1)
if mode != "no_assistant":
    resp = "wrong-model-x" if mode == "wrong_resp" else {"glm-5.3-mb": "GLM-5.3"}.get(model, model)  # 真网关回显展示名
    print(json.dumps({"type": "assistant", "message": {"model": resp, "content": []}}), flush=True)
final = "只有半截" if mode == "noverdict" else conf["final"]
usage = {("wrong-model-x" if mode == "wrong_usage" else model): {}}
print(json.dumps({"type": "result", "subtype": "success", "is_error": False, "result": final, "total_cost_usd": 0,
                  "modelUsage": usage}))
if trace:
    with open(trace, "a") as f:
        f.write(f"end {time.time()}\n")
'''


def main() -> int:
    spec = importlib.util.spec_from_file_location("gate", ROOT / "scripts" / "check-pr-gate.py")
    gate = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    spec.loader.exec_module(gate)
    # 运行器要求产物在本检出 _tmp/ 下；按命名规约取一次性目录，结束即删，防陈旧 summary 被下轮读回
    out_base = ROOT / "_tmp" / f"{dt.date.today():%Y%m%d}-test-review-roles-{os.getpid()}"
    rspec = importlib.util.spec_from_file_location("rrr", RUNNER)
    rrr = importlib.util.module_from_spec(rspec)
    assert rspec.loader is not None
    rspec.loader.exec_module(rrr)
    try:
        return _run(gate, rrr, out_base)
    finally:
        shutil.rmtree(out_base, ignore_errors=True)


def _run(gate, rrr, out_base: Path) -> int:
    fails: list[str] = []
    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td)
        wt = tmp / "repo"
        wt.mkdir()
        g = ["git", "-C", str(wt), "-c", "user.email=t@t", "-c", "user.name=t"]
        subprocess.run([*g, "init", "-q"], check=True)
        subprocess.run([*g, "commit", "-q", "--allow-empty", "-m", "base"], check=True)
        (wt / "a.txt").write_text("hello\n")
        (wt / "bad-utf8.bin").write_bytes(b"ok\n\xff\xfe not utf-8\n")  # 夹具里的非 UTF-8 字节不得让运行器崩溃
        subprocess.run([*g, "add", "a.txt", "bad-utf8.bin"], check=True)
        subprocess.run([*g, "commit", "-q", "-m", "change"], check=True)
        head = subprocess.run([*g, "rev-parse", "HEAD"], capture_output=True, text=True).stdout.strip()
        fake = tmp / "claude"
        fake.write_text(FAKE)
        fake.chmod(0o755)
        log = tmp / "log.jsonl"
        secret = "zai-test-secret-123"
        anth_secret = "anth-test-secret-456"
        envfile = tmp / "fixture.env"  # 隔离 fixture：测试永不读真实 .env.local
        envfile.write_text(f"ZAI_API_KEY={secret}\nexport ANTHROPIC_API_KEY='{anth_secret}'\n")
        polluted = {"ANTHROPIC_AUTH_TOKEN", "ANTHROPIC_CUSTOM_HEADERS", "CLAUDE_CODE_USE_BEDROCK",
                    "CLAUDE_CODE_USE_VERTEX", "GITHUB_TOKEN", "ZAI_API_KEY", "FAKE_MODE"}

        def run(name: str, mode: str, runner: str = "zai", timeout: int = 20, extra: tuple[str, ...] = ("--retries", "0"),
                conf_extra: dict | None = None) -> tuple[int, str]:
            # 污染的父环境：旧网关头、provider 选择器、他家令牌都不得传进评审进程
            (tmp / "fake.json").write_text(json.dumps({"mode": mode, "log": str(log), "final": FINAL, "keys": {
                "zai_file": secret, "anth_file": anth_secret, "zai_parent": "zai-parent-x", "anth_parent": "anth-parent-x"},
                **(conf_extra or {})}))
            # 父环境放另一组键：运行器只认 --env-file，父环境的键不得被用
            env = dict(os.environ, FAKE_MODE=mode, ZAI_API_KEY="zai-parent-x",
                       ANTHROPIC_API_KEY="anth-parent-x", ANTHROPIC_BASE_URL="http://parent-shell-leak.invalid",
                       ANTHROPIC_AUTH_TOKEN="leak-auth", ANTHROPIC_CUSTOM_HEADERS="x-gw-key: leak",
                       CLAUDE_CODE_USE_BEDROCK="1", CLAUDE_CODE_USE_VERTEX="1", GITHUB_TOKEN="ghp_leak")
            out = out_base / name
            cp = subprocess.run([sys.executable, "-I", str(RUNNER), "--runner", runner, "--claude-bin", str(fake),
                                 "--env-file", str(envfile),
                                 "--worktree", str(wt), "--base", "HEAD~1", "--out", str(out), "--roles", ROLES,
                                 "--timeout", str(timeout), *extra], capture_output=True, text=True, env=env)
            s = (out / "summary.md").read_text(encoding="utf-8") if (out / "summary.md").exists() else ""
            return cp.returncode, s

        def gate_errs(s: str) -> list[str]:
            return gate.check(s + "\nRefs #1\n", head)

        rc, s = run("zai-ok", "ok")
        if rc != 0 or "simplify:PASS" not in s or "runner=zai model=glm-5.3-mb readonly=tools" not in s:
            fails.append(f"zai-ok：应全 PASS 且标 runner=zai model=glm-5.3-mb readonly=tools（rc={rc}）")
        if gate_errs(s):
            fails.append(f"zai-ok：闸门应接受：{gate_errs(s)}")
        fp = gate.patch_fingerprint(str(wt), "HEAD~1", head)
        if not fp or f" patch={fp} " not in s:
            fails.append(f"zai-ok：结果块应带整个 PR 的补丁指纹 patch={fp}")
        calls = [json.loads(x) for x in log.read_text().splitlines()]
        if not calls or any(c["base"] != "https://llm-center.modelbest.co" or c["key_is"] != "zai_file" for c in calls):
            fails.append("zai-ok：子进程应拿到智谱接口地址与文件里的 ZAI 密钥（不是父环境的）")
        if any(polluted & set(c["env"]) for c in calls):
            fails.append(f"zai-ok：父环境污染变量泄进了评审进程：{sorted(polluted & set(calls[0]['env']))}")
        if any(not c["cfg_empty"] for c in calls) or len({c["cfg"] for c in calls}) != len(calls):
            fails.append("zai-ok：每个角色应有各自的一次性空 CLAUDE_CONFIG_DIR")
        if any(secret in " ".join(c["argv"]) for c in calls):
            fails.append("zai-ok：密钥不得出现在命令行")
        if secret in "".join(p.read_text(errors="replace") for p in (out_base / "zai-ok").rglob("*") if p.is_file()):
            fails.append("zai-ok：密钥不得落盘")

        log.write_text("")
        # 不传 --timeout 时按通道取默认（anthropic 1200 s / zai 600 s）；显式值优先。dry-run 只写 command.txt，不起进程
        for runner_, extra, want in (("anthropic", [], "timeout=1200s"), ("zai", [], "timeout=600s"),
                                     ("anthropic", ["--timeout", "77"], "timeout=77s")):
            dout = out_base / f"dry-{runner_}-{len(extra)}"
            subprocess.run([sys.executable, "-I", str(RUNNER), "--runner", runner_, "--claude-bin", str(fake),
                            "--env-file", str(envfile), "--worktree", str(wt), "--base", "HEAD~1",
                            "--out", str(dout), "--roles", ROLES, "--dry-run", *extra],
                           capture_output=True, text=True, env=dict(os.environ))
            cmdtxt = (dout / "command.txt").read_text(encoding="utf-8") if (dout / "command.txt").exists() else ""
            if want not in cmdtxt:
                fails.append(f"默认超时：--runner {runner_} {' '.join(extra)} 应记 {want}，实际 {cmdtxt.splitlines()[-1:] }")
        rc, s = run("anthropic-ok", "ok", runner="anthropic")
        calls = [json.loads(x) for x in log.read_text().splitlines()]
        if not calls or any(c["base"] != "https://llm-center.modelbest.co" or c["key_is"] != "anth_file" for c in calls):
            fails.append("anthropic-ok：BASE_URL 应为 llm-center 而非父环境值，且只用文件里的 ANTHROPIC_API_KEY（父环境键≠文件键）")
        if any(polluted & set(c["env"]) for c in calls):
            fails.append("anthropic-ok：父环境的令牌/网关头/provider 选择器不得进子进程")
        if secret in log.read_text() or anth_secret in log.read_text():
            fails.append("测试日志里出现了密钥原值")
        if rc != 0 or "runner=anthropic model=claude-opus-5-5" not in s or gate_errs(s):
            fails.append(f"anthropic-ok：应全 PASS 且过闸（rc={rc}）")

        # 两通道回显行为已分叉（GLM 回显展示名、Claude 原样回显），模型守卫负控两通道各跑一遍
        for name, mode, want, runner in [("extra-tool", "extra_tool", "TOOLS_VIOLATION", "zai"), ("wrong-init", "wrong_init", "ERROR", "zai"),
                                         ("wrong-resp", "wrong_resp", "ERROR", "zai"), ("wrong-usage", "wrong_usage", "ERROR", "zai"),
                                         ("wrong-init-anth", "wrong_init", "ERROR", "anthropic"),
                                         ("wrong-resp-anth", "wrong_resp", "ERROR", "anthropic"),
                                         ("wrong-usage-anth", "wrong_usage", "ERROR", "anthropic"),
                                         ("no-assistant", "no_assistant", "ERROR", "zai"), ("noverdict", "noverdict", "PARTIAL", "zai")]:
            rc, s = run(name, mode, runner=runner)
            if rc == 0 or want not in s or not gate_errs(s):
                fails.append(f"{name}：应判 {want} 且闸门拒收（rc={rc}）")
        rc, s = run("timeout", "hang", timeout=2)
        if rc == 0 or "TIMEOUT" not in s or not gate_errs(s):
            fails.append(f"timeout：应判 TIMEOUT 且闸门拒收（rc={rc}）")
        _watchdog_tests(run, gate_errs, out_base, tmp, fake, envfile, wt, log, fails)

        # 缺钥：fixture 文件没有该键、父环境却有——必须拒绝，不回退父环境（无条件断言，开发机与 CI 一致）
        empty = tmp / "empty.env"
        empty.write_text("OTHER=1\n")
        log.write_text("")
        for runner in ("zai", "anthropic"):
            cp = subprocess.run([sys.executable, "-I", str(RUNNER), "--runner", runner, "--claude-bin", str(fake),
                                 "--env-file", str(empty), "--worktree", str(wt), "--base", "HEAD~1",
                                 "--out", str(out_base / f"nokey-{runner}")], capture_output=True, text=True,
                                env=dict(os.environ, ZAI_API_KEY="zai-parent-x", ANTHROPIC_API_KEY="anth-parent-x"))
            if cp.returncode != 2 or "API_KEY" not in cp.stderr or "不存在" in cp.stderr:
                fails.append(f"nokey-{runner}：文件缺钥应拒绝运行，不得回退父环境（rc={cp.returncode}）")
        cp = subprocess.run([sys.executable, "-I", str(RUNNER), "--claude-bin", str(fake), "--env-file", str(tmp / "typo.env"),
                             "--worktree", str(wt), "--base", "HEAD~1", "--out", str(out_base / "badpath")],
                            capture_output=True, text=True)
        if cp.returncode != 2 or "不存在" not in cp.stderr:
            fails.append(f"badpath：--env-file 路径不存在应单独报错（rc={cp.returncode}）")
        if log.read_text().strip():
            fails.append("nokey/badpath：拒绝前不得启动评审进程")

        ok_block = run("zai-ok2", "ok")[1]
        for bad, why in [(ok_block.replace("readonly=tools", "readonly=snapshot"), "snapshot"),
                         (ok_block.replace(" readonly=tools", ""), "缺字段"),
                         (ok_block.replace("readonly=tools", "readonly=sandbox"), "未知等级"),
                         (ok_block.replace("readonly=tools", "notreadonly=tools"), "键名不完整"),
                         (ok_block.replace("readonly=tools", "readonly=tools readonly=snapshot"), "重复键"),
                         (ok_block.replace(" runner=zai", ""), "缺 runner"),
                         (ok_block.replace("runner=zai", "runner=anthropic"), "runner 与 model 不配"),
                         (ok_block + "\n" + ok_block.replace("PASS", "ERROR"), "多块中有一块非 PASS"),
                         (ok_block.replace("simplify:PASS", "simplify:ERROR,simplify:PASS"), "角色重复遮蔽失败"),
                         (ok_block.replace(" head=", " nothead="), "head 键名不完整"),
                         (ok_block.replace(" head=", " head=0000000 head="), "head 重复")]:
            if not gate_errs(bad):
                fails.append(f"闸门：{why} 应拒收")
        if gate_errs(ok_block + "\n" + ok_block):
            fails.append(f"闸门：分批的两个合法块应接受：{gate_errs(ok_block + chr(10) + ok_block)}")

        # --serial：同一进程逐个跑四角色（并行全跑在本机会挂起），产出单个四角色块；进程起止不得重叠
        trace = tmp / "trace.txt"
        for mode_name, serial_flag in (("serial", True), ("parallel", False)):
            trace.write_text("")
            (tmp / "fake.json").write_text(json.dumps({"mode": "ok", "log": str(log), "final": FINAL, "keys": {},
                                                       "trace": str(trace)}))
            out = out_base / f"{mode_name}-ok"
            cp = subprocess.run([sys.executable, "-I", str(RUNNER), "--claude-bin", str(fake), "--env-file", str(envfile),
                                 "--worktree", str(wt), "--base", "HEAD~1", "--out", str(out), "--roles", ROLES,
                                 *(["--serial"] if serial_flag else [])], capture_output=True, text=True)
            ev = [(float(x.split()[1]), x.split()[0]) for x in trace.read_text().splitlines()]
            depth = peak = 0
            for _, kind in sorted(ev):
                depth += 1 if kind == "start" else -1
                peak = max(peak, depth)
            s = (out / "summary.md").read_text(encoding="utf-8") if (out / "summary.md").exists() else ""
            if mode_name == "serial":
                if cp.returncode != 0 or gate_errs(s) or len(ev) != 8 or peak != 1:
                    fails.append(f"serial：应逐个跑四角色（峰值并发 {peak}，事件 {len(ev)}）且单块过闸（rc={cp.returncode}）：{gate_errs(s)}")
            elif peak < 2:
                fails.append(f"parallel：对照组应观察到并发（峰值 {peak}），否则 serial 断言无判别力")
        # --serial 首角色超时：后续三个角色仍须照跑，块含四角色、首角色 TIMEOUT，返回非 0 且闸门拒收
        trace.write_text("")
        marker = tmp / "hang-first.flag"
        (tmp / "fake.json").write_text(json.dumps({"mode": "ok", "log": str(log), "final": FINAL, "keys": {},
                                                   "trace": str(trace), "hang_first": str(marker)}))
        out = out_base / "serial-timeout-first"
        cp = subprocess.run([sys.executable, "-I", str(RUNNER), "--claude-bin", str(fake), "--env-file", str(envfile),
                             "--worktree", str(wt), "--base", "HEAD~1", "--out", str(out), "--roles", ROLES,
                             "--serial", "--timeout", "2", "--retries", "0"], capture_output=True, text=True)
        ends = trace.read_text().split().count("end")
        s = (out / "summary.md").read_text(encoding="utf-8") if (out / "summary.md").exists() else ""
        first, rest = ROLES.split(",")[0], ROLES.split(",")[1:]
        if (cp.returncode == 0 or not gate_errs(s) or ends != 3 or f"{first}:TIMEOUT" not in s
                or any(f"{r}:PASS" not in s for r in rest)):
            fails.append(f"serial-timeout：首角色超时后应照跑其余三角色并拒收（rc={cp.returncode}，完成 {ends}）")
    _carry_tests(gate, fails)
    for f in fails:
        print("FAIL", f)
    print(f"test-review-roles: {'全部通过' if not fails else f'{len(fails)} 项失败'}")
    return 1 if fails else 0



# 看门狗测试参数：宽限 0.5 s、窗口 1 s、阈值 20 tok/s、持续 1 s；drip（≈5 tok/s）必被判 STALLED，steady（≈500 tok/s、跑 3 s）不得误杀
FAST_STALL = ("--stall-grace", "0.5", "--stall-window", "1", "--stall-min-rate", "20", "--stall-hold", "1",
              "--retry-backoff", "0.1,0.1")


def _status(out: Path) -> dict:
    try:
        return json.loads((out / "status.json").read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return {}


def _watchdog_tests(run, gate_errs, out_base: Path, tmp: Path, fake: Path, envfile: Path, wt: Path, log: Path,
                    fails: list[str]) -> None:
    """速率看门狗 + 单角色自动重试（11-02 时段性流式饥饿）。"""
    # 1. 慢滴漏 → STALLED → 重试 PASS：只有首个起跑的角色滴漏，重试后四角色全 PASS，过闸；滴漏那次原始流另存
    marker = tmp / "drip-first.flag"
    marker.unlink(missing_ok=True)
    rc, s = run("drip-retry-pass", "ok", extra=(*FAST_STALL, "--retries", "2"), conf_extra={"drip_first": str(marker)})
    st = _status(out_base / "drip-retry-pass")
    retried = [r for r, v in st.items() if len(v.get("attempts") or []) > 1]
    if (rc != 0 or gate_errs(s) or len(retried) != 1
            or [a["status"] for a in st[retried[0]]["attempts"]] != ["STALLED", "PASS"]
            or not (out_base / "drip-retry-pass" / f"{retried[0]}.attempt1.jsonl").is_file()
            or not (out_base / "drip-retry-pass" / f"{retried[0]}.jsonl").is_file()
            or "尝试 2 次：STALLED" not in s):
        fails.append(f"drip-retry-pass：应恰一角色 STALLED→PASS、另存 attempt1 原始流且全 PASS 过闸（rc={rc}，"
                     f"{ {r: [a['status'] for a in v.get('attempts') or []] for r, v in st.items()} }）")
    elif "thinking_tokens" not in (out_base / "drip-retry-pass" / f"{retried[0]}.attempt1.jsonl").read_text():
        fails.append("drip-retry-pass：attempt1.jsonl 应是滴漏那次的原始流")

    # 2. 两次都 STALLED（--retries 1）→ 最终 STALLED，闸门拒收，尝试数 2、每次都有原始流
    rc, s = run("drip-always", "drip", timeout=20, extra=(*FAST_STALL, "--retries", "1"))
    st = _status(out_base / "drip-always")
    att = {r: [a["status"] for a in v.get("attempts") or []] for r, v in st.items()}
    if (rc == 0 or not gate_errs(s) or any(v != ["STALLED", "STALLED"] for v in att.values()) or len(att) != 4
            or "simplify:STALLED" not in s
            or any(not (out_base / "drip-always" / f"{r}.attempt1.jsonl").is_file() for r in att)
            or any(st[r].get("seconds", 99) > 10 for r in att)):
        fails.append(f"drip-always：应两次 STALLED、最终 STALLED 且闸门拒收、远早于 20 s 超时（rc={rc}，{att}）")

    # 2b. 只有 tool_result 在刷不算模型输出 → 仍判 STALLED
    rc, s = run("tool-flood", "tool_flood", extra=(*FAST_STALL, "--retries", "0"))
    if rc == 0 or "simplify:STALLED" not in s:
        fails.append(f"tool-flood：本地工具结果不计入输出速率，应判 STALLED（rc={rc}）")

    # 3. 正常速率不误杀：steady 跑 3 s（> 宽限 + 窗口 + 持续），全 PASS、单次尝试
    rc, s = run("steady-ok", "steady", extra=(*FAST_STALL, "--retries", "2"))
    st = _status(out_base / "steady-ok")
    if rc != 0 or gate_errs(s) or any(len(v.get("attempts") or []) != 1 for v in st.values()) or len(st) != 4:
        fails.append(f"steady-ok：正常速率不得被判 STALLED（rc={rc}，"
                     f"{ {r: [a['status'] for a in v.get('attempts') or []] for r, v in st.items()} }）")
    elif any((v.get("out_tokens_est") or 0) < 1000 for v in st.values()):
        fails.append(f"steady-ok：status.json 应记输出量估计（{ {r: v.get('out_tokens_est') for r, v in st.items()} }）")

    # 4. 看门狗关（--stall-min-rate 0）时 drip 不被杀，只能等超时 → TIMEOUT（证明 STALLED 来自看门狗而非别处）
    rc, s = run("drip-wd-off", "drip", timeout=3, extra=(*FAST_STALL, "--stall-min-rate", "0", "--retries", "0"))
    if rc == 0 or "simplify:TIMEOUT" not in s or "STALLED" in s:
        fails.append(f"drip-wd-off：看门狗关闭时滴漏应只判 TIMEOUT（rc={rc}）")

    # 5. TIMEOUT 也重试：首个起跑的角色挂住超时，重试后 PASS
    marker = tmp / "hang-first-retry.flag"
    marker.unlink(missing_ok=True)
    rc, s = run("timeout-retry-pass", "ok", timeout=2, extra=("--retries", "1", "--retry-backoff", "0.1"),
                conf_extra={"hang_first": str(marker)})
    st = _status(out_base / "timeout-retry-pass")
    att = sorted([a["status"] for a in v.get("attempts") or []] for v in st.values())
    if rc != 0 or gate_errs(s) or att != [["PASS"]] * 3 + [["TIMEOUT", "PASS"]]:
        fails.append(f"timeout-retry-pass：TIMEOUT 应自动重跑并 PASS（rc={rc}，{att}）")

    # 6. 并行模式下重试不阻塞其它角色：滴漏角色重试（退避 1 s）期间，其余三角色已完成
    trace = tmp / "trace-par.txt"
    trace.write_text("")
    marker = tmp / "drip-first-par.flag"
    marker.unlink(missing_ok=True)
    rc, s = run("drip-parallel", "ok", extra=(*FAST_STALL[:-2], "--retry-backoff", "1", "--retries", "1"),
                conf_extra={"drip_first": str(marker), "trace": str(trace)})
    ev = sorted((float(x.split()[1]), x.split()[0]) for x in trace.read_text().splitlines())
    starts = [t for t, k in ev if k == "start"]
    ends = [t for t, k in ev if k == "end"]
    if rc != 0 or len(starts) != 5 or len(ends) != 4 or not starts[3] < ends[0] or not ends[2] < starts[4]:
        fails.append(f"drip-parallel：其余角色应在滴漏角色重试前完成（rc={rc}，start {len(starts)} end {len(ends)}）")

    # 7. 通道默认：anthropic 默认开看门狗（5 tok/s），zai 默认关；默认重试 2 次、退避 30,90
    for runner_, want in (("anthropic", "stall=180,120,5,60 retries=2 backoff=30,90"),
                          ("zai", "stall=180,120,0,60 retries=2 backoff=30,90")):
        dout = out_base / f"dry-wd-{runner_}"
        subprocess.run([sys.executable, "-I", str(RUNNER), "--runner", runner_, "--claude-bin", str(fake),
                        "--env-file", str(envfile), "--worktree", str(wt), "--base", "HEAD~1",
                        "--out", str(dout), "--roles", ROLES, "--dry-run"], capture_output=True, text=True)
        cmdtxt = (dout / "command.txt").read_text(encoding="utf-8") if (dout / "command.txt").exists() else ""
        if want not in cmdtxt:
            fails.append(f"看门狗默认：--runner {runner_} 应记 {want}，实际 {cmdtxt.splitlines()[-1:]}")
    for bad in (("--retries", "-1"), ("--stall-window", "0"), ("--retry-backoff", "x")):
        cp = subprocess.run([sys.executable, "-I", str(RUNNER), "--claude-bin", str(fake), "--env-file", str(envfile),
                             "--worktree", str(wt), "--base", "HEAD~1", "--out", str(out_base / "badarg"), "--dry-run", *bad],
                            capture_output=True, text=True)
        if cp.returncode != 2:
            fails.append(f"看门狗参数：{' '.join(bad)} 应拒绝（rc={cp.returncode}）")


def _carry_tests(gate, fails: list[str]) -> None:
    """叠包纯 rebase 沿用结果块：补丁逐字不变才沿用，改一行、上下文变了、无指纹、算不出都拒（负控）。"""
    with tempfile.TemporaryDirectory() as td:
        repo = Path(td)
        g = ["git", "-C", str(repo), "-c", "user.email=t@t", "-c", "user.name=t", "-c", "commit.gpgsign=false"]

        def git(*args: str) -> str:
            return subprocess.run([*g, *args], check=True, capture_output=True, text=True).stdout.strip()

        def commit(path: str, text: str, msg: str) -> str:
            (repo / path).write_text(text)
            git("add", path)
            git("commit", "-q", "-m", msg)
            return git("rev-parse", "HEAD")

        lines = [f"line {i}\n" for i in range(1, 21)]
        git("init", "-q", "-b", "main")
        commit("f.txt", "".join(lines), "base")
        commit("g.txt", "g0\n", "g")
        git("checkout", "-q", "-b", "parent")
        parent = commit("g.txt", "g1\n", "parent change")
        git("checkout", "-q", "-b", "child")
        child_lines = lines.copy()
        child_lines[14] = "line 15 changed\n"
        child = commit("f.txt", "".join(child_lines), "child change")
        old_fp = gate.patch_fingerprint(str(repo), parent, child)
        # 前一包经 queue 以 REBASE 合入（新 SHA），main 另有无关提交；后一包 rebase 到 main
        git("checkout", "-q", "main")
        commit("h.txt", "unrelated\n", "unrelated")
        git("cherry-pick", parent)
        git("checkout", "-q", "-b", "child2")
        git("cherry-pick", child)
        new_head = git("rev-parse", "HEAD")
        new_fp = gate.patch_fingerprint(str(repo), "main", new_head)
        rebased_main = git("rev-parse", "main")  # 负控 2 会再推进 main，后面的变体须对这个固定点比

        def block(head: str, extra: str = "") -> str:
            scope = "" if "scope=" in extra else " scope=full"
            return (f"<!-- pr-gate:v1 head={head}{extra}{scope} roles=sibling:PASS,concurrency:PASS,boundary:PASS,simplify:PASS "
                    f"p0=0 p1=0 runner=zai model=glm-5.3-mb readonly=tools -->\nRefs #1\n")

        if not old_fp or old_fp != new_fp or new_head == child:
            fails.append(f"carry：纯 rebase 后补丁指纹应不变（旧 {old_fp} 新 {new_fp}）")
        if gate.check(block(child, f" patch={old_fp}"), new_head, new_fp):
            fails.append(f"carry：指纹相同应沿用旧 head 的块：{gate.check(block(child, f' patch={old_fp}'), new_head, new_fp)}")
        # 负控 1：rebase 后又改一行 → 指纹变 → 拒
        changed = child_lines.copy()
        changed[14] = "line 15 changed again\n"
        edited = commit("f.txt", "".join(changed), "edit")
        edited_fp = gate.patch_fingerprint(str(repo), "main", edited)
        errs = gate.check(block(child, f" patch={old_fp}"), edited, edited_fp)
        if edited_fp == old_fp or not any("补丁指纹不同" in e for e in errs):
            fails.append(f"carry 负控：内容变了必须拒收：{errs}")
        # 负控 2：上下文变了（main 改了相邻行）→ 指纹变 → 拒（保守：周边代码变了要重审）
        git("checkout", "-q", "main")
        ctx = lines.copy()
        ctx[11] = "line 12 moved by main\n"  # 在第 15 行改动的 3 行上下文内、又不相邻冲突
        commit("f.txt", "".join(ctx), "context")
        git("checkout", "-q", "-b", "child3")
        git("cherry-pick", child)
        ctx_head = git("rev-parse", "HEAD")
        ctx_fp = gate.patch_fingerprint(str(repo), "main", ctx_head)
        if ctx_fp == old_fp or not ctx_fp:
            fails.append("carry 负控：上下文内的行被 main 改了，指纹应不同")
        # 负控 3/4/5：块内无指纹、当前指纹算不出、指纹键重复 → 都按旧提交拒收
        for name, body, patch, want in [
            ("无指纹", block(child), new_fp, "块内无 patch="),
            ("算不出", block(child, f" patch={old_fp}"), "", "算不出"),
            ("重复键", block(new_head, f" patch={old_fp} patch={old_fp}"), new_fp, "patch 至多出现一次"),
        ]:
            errs = gate.check(body, new_head, patch)
            if not any(want in e for e in errs):
                fails.append(f"carry 负控 {name}：应拒收并说明「{want}」：{errs}")
        # 负控 6：rebase 后的「看似无害」改动都必须改变指纹——只改缩进 / 行尾空白、只改文件模式、
        # 新增空文件、删除文件、二进制改动（--stable 会把空白改动判成相同，故用 --verbatim）
        base_fp = new_fp  # 基准：纯 rebase 后的 head（child2 上已叠了负控 1 的改动，不能用分支名）
        if gate.patch_fingerprint(str(repo), rebased_main, new_head) != new_fp:
            fails.append("carry：固定 base 复算应得同一指纹（变体负控的对照）")

        def variant(name: str, act) -> None:
            git("checkout", "-q", "--detach", new_head)
            act()
            git("add", "-A")
            git("commit", "-q", "-m", name)
            got = gate.patch_fingerprint(str(repo), rebased_main, "HEAD")
            if not got or got == base_fp:
                fails.append(f"carry 负控：{name} 后指纹应改变（{got} vs {base_fp}）")

        def reindent() -> None:
            text = (repo / "f.txt").read_text().replace("line 15 changed", "    line 15 changed")
            (repo / "f.txt").write_text(text)

        def trail() -> None:
            text = (repo / "f.txt").read_text().replace("line 15 changed\n", "line 15 changed  \n")
            (repo / "f.txt").write_text(text)

        variant("只改缩进", reindent)
        variant("只加行尾空白", trail)
        variant("只改文件模式", lambda: (repo / "f.txt").chmod(0o755))
        variant("新增空文件", lambda: (repo / "empty.txt").write_text(""))
        variant("删除文件", lambda: (repo / "g.txt").unlink())
        variant("二进制改动", lambda: (repo / "b.bin").write_bytes(b"\x00\x01\x02"))
        # 同一 base 上二进制内容 X / Y 两种改法：指纹必须不同，且不依赖 patch-id 是否哈希 index 行
        # （有二进制改动时指纹应是 patch-id 与 blob id 的合成值，不等于裸 patch-id）
        fps = []
        for content in (b"\x00X", b"\x00Y"):
            git("checkout", "-q", "--detach", new_head)
            (repo / "b.bin").write_bytes(content)
            git("add", "-A")
            git("commit", "-q", "-m", "bin")
            fps.append(gate.patch_fingerprint(str(repo), rebased_main, "HEAD"))
            bare = subprocess.run(["git", "-C", str(repo), *gate.FINGERPRINT_DIFF, f"{rebased_main}...HEAD"],
                                  capture_output=True).stdout
            pid = subprocess.run(["git", "-C", str(repo), "patch-id", "--verbatim"], input=bare,
                                 capture_output=True).stdout.split()[0].decode()
            if fps[-1] == pid:
                fails.append("carry 负控：有二进制改动时指纹应并入 blob id，不能只是裸 patch-id")
        if not all(fps) or fps[0] == fps[1]:
            fails.append(f"carry 负控：二进制内容 X / Y 指纹应不同（{fps}）")
        # textconv 驱动的文件（主 diff 带 --no-textconv 仍是 Binary files differ）与路径含 tab 的二进制，
        # 都必须进二进制清单：指纹 ≠ 裸 patch-id，且 X / Y 两种内容指纹不同
        git("config", "diff.hexdump.textconv", "od -c")
        (repo / ".gitattributes").write_text("*.dat diff=hexdump\n")
        for label, path in (("textconv", "conv.dat"), ("tab 路径", "tab\tname.bin"), ("-diff 属性文本", "secret.cfg")):
            got = []
            for content in (b"\x00X", b"\x00Y"):
                git("checkout", "-q", "--detach", new_head)
                (repo / ".gitattributes").write_text("*.dat diff=hexdump\n*.cfg -diff\n")
                (repo / path).write_bytes(content if label != "-diff 属性文本" else b"key=" + content[1:] + b"\n")
                git("add", "-A")
                git("commit", "-q", "-m", label)
                fp_now = gate.patch_fingerprint(str(repo), rebased_main, "HEAD")
                bare = subprocess.run(["git", "-C", str(repo), *gate.FINGERPRINT_DIFF, f"{rebased_main}...HEAD"],
                                      capture_output=True).stdout
                pid = subprocess.run(["git", "-C", str(repo), "patch-id", "--verbatim"], input=bare,
                                     capture_output=True).stdout.split()[0].decode()
                if not fp_now or fp_now == pid:
                    fails.append(f"carry 负控：{label} 的二进制改动应并入 blob id（指纹 {fp_now[:12]}，裸 {pid[:12]}）")
                got.append(fp_now)
            if got[0] == got[1]:
                fails.append(f"carry 负控：{label} 二进制 X / Y 指纹应不同")
        partial = block(child, f" patch={old_fp} scope=partial paths=f.txt")
        if not any("不是 full" in e for e in gate.check(partial, new_head, new_fp)):
            fails.append("carry 负控：--paths 分批块（scope=partial）即使指纹相同也不得沿用")
        if gate.check(block(child, " patch="), new_head, new_fp) == [] or not any(
                "40 位" in e for e in gate.check(block(child, " patch=xyz"), new_head, new_fp)):
            fails.append("carry 负控：空或畸形 patch= 必须拒收并说明")
        if gate.patch_fingerprint(str(repo), "main", "main") or gate.patch_fingerprint(str(repo), "no-such-ref", new_head):
            fails.append("carry：空 diff 与坏 ref 都应返回空指纹")
        # CLI 端到端：--head/--base/--repo 与 CI 同路径
        bf = repo.parent / "carry-body.md"
        bf.write_text(block(child, f" patch={old_fp}"))
        gate_py = ROOT / "scripts" / "check-pr-gate.py"
        ok = subprocess.run([sys.executable, "-I", str(gate_py), "--body-file", str(bf), "--head", new_head,
                             "--base", "main~1", "--repo", str(repo)], capture_output=True, text=True)
        bad = subprocess.run([sys.executable, "-I", str(gate_py), "--body-file", str(bf), "--head", edited,
                              "--base", "main~1", "--repo", str(repo)], capture_output=True, text=True)
        if ok.returncode != 0 or "沿用" not in ok.stdout or bad.returncode != 1:
            fails.append(f"carry CLI：同指纹应 0、改过应 1（{ok.returncode}/{bad.returncode}）：{ok.stdout}{bad.stdout}")


if __name__ == "__main__":
    sys.exit(main())
