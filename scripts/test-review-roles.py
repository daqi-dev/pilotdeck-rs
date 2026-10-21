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
init_model = "claude-sonnet-5-5" if mode == "wrong_init" else model
print(json.dumps({"type": "system", "subtype": "init", "tools": tools, "model": init_model}), flush=True)
trace = conf.get("trace")
if trace:
    with open(trace, "a") as f:
        f.write(f"start {time.time()}\n")
    time.sleep(0.3)
if mode == "hang":
    time.sleep(30)
if mode != "no_assistant":
    resp = "claude-sonnet-5-5" if mode == "wrong_resp" else {"glm-5.3-mb": "GLM-5.3"}.get(model, model)  # 真网关回显展示名
    print(json.dumps({"type": "assistant", "message": {"model": resp, "content": []}}), flush=True)
final = "只有半截" if mode == "noverdict" else conf["final"]
usage = {("claude-sonnet-5-5" if mode == "wrong_usage" else model): {}}
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

        def run(name: str, mode: str, runner: str = "zai", timeout: int = 20) -> tuple[int, str]:
            # 污染的父环境：旧网关头、provider 选择器、他家令牌都不得传进评审进程
            (tmp / "fake.json").write_text(json.dumps({"mode": mode, "log": str(log), "final": FINAL, "keys": {
                "zai_file": secret, "anth_file": anth_secret, "zai_parent": "zai-parent-x", "anth_parent": "anth-parent-x"}}))
            # 父环境放另一组键：运行器只认 --env-file，父环境的键不得被用
            env = dict(os.environ, FAKE_MODE=mode, ZAI_API_KEY="zai-parent-x",
                       ANTHROPIC_API_KEY="anth-parent-x", ANTHROPIC_BASE_URL="http://parent-shell-leak.invalid",
                       ANTHROPIC_AUTH_TOKEN="leak-auth", ANTHROPIC_CUSTOM_HEADERS="x-gw-key: leak",
                       CLAUDE_CODE_USE_BEDROCK="1", CLAUDE_CODE_USE_VERTEX="1", GITHUB_TOKEN="ghp_leak")
            out = out_base / name
            cp = subprocess.run([sys.executable, "-I", str(RUNNER), "--runner", runner, "--claude-bin", str(fake),
                                 "--env-file", str(envfile),
                                 "--worktree", str(wt), "--base", "HEAD~1", "--out", str(out), "--roles", ROLES,
                                 "--timeout", str(timeout)], capture_output=True, text=True, env=env)
            s = (out / "summary.md").read_text(encoding="utf-8") if (out / "summary.md").exists() else ""
            return cp.returncode, s

        def gate_errs(s: str) -> list[str]:
            return gate.check(s + "\nRefs #1\n", head)

        rc, s = run("zai-ok", "ok")
        if rc != 0 or "simplify:PASS" not in s or "runner=zai model=glm-5.3-mb readonly=tools" not in s:
            fails.append(f"zai-ok：应全 PASS 且标 runner=zai model=glm-5.3-mb readonly=tools（rc={rc}）")
        if gate_errs(s):
            fails.append(f"zai-ok：闸门应接受：{gate_errs(s)}")
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
        rc, s = run("anthropic-ok", "ok", runner="anthropic")
        if any(json.loads(x)["base"] or json.loads(x)["key_is"] != "anth_file" for x in log.read_text().splitlines()):
            fails.append("anthropic-ok：应清掉父环境 BASE_URL，且只用文件里的 ANTHROPIC_API_KEY（父环境键≠文件键）")
        if secret in log.read_text() or anth_secret in log.read_text():
            fails.append("测试日志里出现了密钥原值")
        if rc != 0 or "runner=anthropic model=claude-sonnet-5-5" not in s or gate_errs(s):
            fails.append(f"anthropic-ok：应全 PASS 且过闸（rc={rc}）")

        for name, mode, want in [("extra-tool", "extra_tool", "TOOLS_VIOLATION"), ("wrong-init", "wrong_init", "ERROR"),
                                 ("wrong-resp", "wrong_resp", "ERROR"), ("wrong-usage", "wrong_usage", "ERROR"),
                                 ("no-assistant", "no_assistant", "ERROR"), ("noverdict", "noverdict", "PARTIAL")]:
            rc, s = run(name, mode)
            if rc == 0 or want not in s or not gate_errs(s):
                fails.append(f"{name}：应判 {want} 且闸门拒收（rc={rc}）")
        rc, s = run("timeout", "hang", timeout=2)
        if rc == 0 or "TIMEOUT" not in s or not gate_errs(s):
            fails.append(f"timeout：应判 TIMEOUT 且闸门拒收（rc={rc}）")

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
    for f in fails:
        print("FAIL", f)
    print(f"test-review-roles: {'全部通过' if not fails else f'{len(fails)} 项失败'}")
    return 1 if fails else 0


if __name__ == "__main__":
    sys.exit(main())
