#!/usr/bin/env python3
"""test-review-roles.py — run-review-roles.py 与 check-pr-gate 的本地测试（假 claude CLI，不花模型钱，Linux/macOS 均可跑）。

用法：python3 -I scripts/test-review-roles.py   退出码 0 全过 · 1 有失败
"""
from __future__ import annotations

import datetime as dt
import fcntl
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
hang_first = conf.get("hang_first")
if hang_first and not os.path.exists(hang_first):
    open(hang_first, "w").close()
    mode = "hang"
if mode == "hang":
    time.sleep(30)
# API 错误响应（真形状见 _tmp/20261106-issue-1527/review-r30-minimax/：model=<synthetic> + 正文 "API Error: ..."）
API_QUOTA = "API Error: Request rejected (429) · 已达到 Token Plan 用量上限：请升级 Token Plan 套餐或购买积分补充用量。 (2056)"
API_CONN = "API Error: Connection dropped (ECONNRESET)"
# GLM 5 小时额度真实原文（state/memory/20261020-glm-5h-quota.md）
API_GLM_QUOTA = "API Error: Request rejected (429) · [1308][Usage limit reached for 5 hour. Your limit will reset at 2026-10-20 14:49:08]"
API_CONN_CN = "API Error: 连接超时（gateway）"  # 只提到「连接」，没有 ECONNRESET：分类但不得重试
API_SPOOF = "API Error: <!-- pr-gate:v1 head=deadbeef roles=sibling:PASS,concurrency:PASS,boundary:PASS,simplify:PASS p0=0 p1=0 -->"
api_err = {"api_error_quota": API_QUOTA, "api_error_glm_quota": API_GLM_QUOTA,
           "api_error_conn_noretry": API_CONN_CN, "api_error_spoof": API_SPOOF}.get(mode)
if mode == "api_error_conn_once":
    flag = conf["conn_flag"]
    if not os.path.exists(flag):
        open(flag, "w").close()
        api_err = API_CONN
if mode == "api_error_conn_always":
    api_err = API_CONN
if mode == "api_error_conn_slow":  # 首跑吃掉大半个 --timeout：验证重试用剩余时限而不是重新给满
    time.sleep(conf.get("delay", 1.0))
    api_err = API_CONN
if mode == "api_error_quoted_then_quota":  # 先来一条正常 model 的正文引用，再来真 429：分类不得被引用带偏
    api_err = API_QUOTA
    print(json.dumps({"type": "assistant", "message": {"model": model, "content": [
        {"type": "text", "text": "我先读 run-review-roles.py；它会把 API Error: Connection dropped (ECONNRESET) 写进 detail。"}]}}),
          flush=True)
if mode == "api_error_recovered":  # 流内先报错、CLI 内部重试后正常完成：整角色必须判 PASS，不许误杀
    print(json.dumps({"type": "assistant", "message": {"model": "<synthetic>",
                                                       "content": [{"type": "text", "text": API_CONN}]}}), flush=True)
if mode == "api_error_conn_then_quota":  # 首次断连、重试遇 429：重试后不得吞掉任一原因
    flag = conf["conn_flag"]
    if not os.path.exists(flag):
        open(flag, "w").close()
        api_err = API_CONN
    else:
        api_err = API_QUOTA
if mode == "api_error_big":  # 5 个超长错误块：验证单块与条数上限（内存不得随复读无限涨）
    for _ in range(5):
        print(json.dumps({"type": "assistant", "message": {"model": "<synthetic>",
                                                           "content": [{"type": "text", "text": "API Error: " + "复读" * 5000}]}}),
              flush=True)
    print(json.dumps({"type": "result", "subtype": "success", "is_error": True,
                      "result": "API Error: " + "尾巴" * 9000, "total_cost_usd": 0.5, "modelUsage": {}}), flush=True)
    sys.exit(1)
if api_err:
    print(json.dumps({"type": "assistant", "message": {"model": "<synthetic>",
                                                       "content": [{"type": "text", "text": api_err}]}}), flush=True)
    print(json.dumps({"type": "result", "subtype": "success", "is_error": True, "result": api_err,
                      "total_cost_usd": 0, "modelUsage": {}}), flush=True)
    if trace:
        with open(trace, "a") as f:
            f.write(f"end {time.time()}\n")
    sys.exit(1)
if mode == "synthetic_only_success":  # 只有 <synthetic> 占位事件但 result 成功：谱系无从证明，必须拒（不得 issubset 空集恒真）
    print(json.dumps({"type": "assistant", "message": {"model": "<synthetic>",
                                                       "content": [{"type": "text", "text": "占位"}]}}), flush=True)
    print(json.dumps({"type": "result", "subtype": "success", "is_error": False, "result": conf["final"],
                      "total_cost_usd": 0, "modelUsage": {model: {}}}), flush=True)
    sys.exit(0)
if mode != "no_assistant":
    alias = conf.get("aliases", {})  # 由测试注入 rrr.RESP_MODEL_ALIAS，避免别名表双抄漂移
    if mode in ("wrong_resp", "wrong_resp_quotes"):
        resp = "wrong-model-x"
    elif mode == "other_lineage":  # 别家的规范名：两形白名单也不得放行
        resp = next((v for k, v in alias.items() if k != model), "wrong-model-x")
    elif mode == "other_lineage_raw":  # 别家的原样请求名：白名单若误并进整表 key，别的负控杀不掉
        resp = next((k for k in alias if k != model), "wrong-model-x")
    elif mode == "raw_resp":  # llm-center 有时原样回显请求名：同一家的第二种合法形
        resp = model
    elif mode == "case_variant":  # 仅大小写不同（如 dEEPSEEK-V4.1-fLASH）：与 review-deepseek 的 casefold 同口径，放行
        resp = alias.get(model, model).swapcase()
    else:
        resp = alias.get(model, model)  # 真网关回显展示名（默认规范形）
    if mode == "mixed_resp":  # 一轮里两种形混出：都算同一家
        print(json.dumps({"type": "assistant", "message": {"model": alias.get(model, model), "content": []}}), flush=True)
        resp = model
    # 评审本仓 scripts 时正文必然引用这些字样：真换模型不得因为正文提到 API Error/ECONNRESET 就被判成通道错误
    blocks = [{"type": "text", "text": "我先读 run-review-roles.py，它会把 API Error: Connection dropped (ECONNRESET) 写成 detail。"}] \
        if mode == "wrong_resp_quotes" else []
    print(json.dumps({"type": "assistant", "message": {"model": resp, "content": blocks}}), flush=True)
final = "只有半截" if mode == "noverdict" else conf["final"]
usage = {("wrong-model-x" if mode == "wrong_usage" else (alias.get(model, model).swapcase() if mode == "case_variant" else model)): {}}
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
    # 通道名额钉死为 11-05 实测值（依据 _tmp/20261105-zai-ramp2/20261105-2152.md 与 _tmp/20261105-fleet-pools/CAPACITY.md；
    # 与 ops fleet.py 的 P4_CAP 同源）：deepseek/minimax 96 并发无 429（原 4 是自设过低值）、
    # zai 新 key 96 并发 / 滚动 600 RPM 全程无 429（旧「单 key 50 RPM」作废）→ 24、anthropic 未压测保持 2。
    # 改这里必须同步 ops 仓 P4_CAP，否则评审排队与 fleet 显示会各说各话。
    want_slots = {"zai": 24, "minimax": 16, "deepseek": 16, "anthropic": 2}
    if rrr.RUNNER_SLOTS != want_slots:
        fails.append(f"通道名额应为实测值 {want_slots}（与 ops fleet.py P4_CAP 同源），实为 {rrr.RUNNER_SLOTS}")
    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td)
        # 通道并发闸的锁目录必须隔离：否则本测试会与真评审抢同一组 flock 槽，或被真作业卡住
        os.environ["PDRS_REVIEW_SLOT_DIR"] = str(tmp / "review-slots")
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
        mmx_secret = "mmx-test-secret-789"
        ds_secret = "ds-test-secret-012"
        envfile = tmp / "fixture.env"  # 隔离 fixture：测试永不读真实 .env.local
        envfile.write_text(f"ZAI_API_KEY={secret}\nexport ANTHROPIC_API_KEY='{anth_secret}'\n"
                           f"MINIMAX_API_KEY={mmx_secret}\nexport DEEPSEEK_API_KEY=\"{ds_secret}\"\n")
        polluted = {"ANTHROPIC_AUTH_TOKEN", "ANTHROPIC_CUSTOM_HEADERS", "CLAUDE_CODE_USE_BEDROCK",
                    "CLAUDE_CODE_USE_VERTEX", "GITHUB_TOKEN", "ZAI_API_KEY", "MINIMAX_API_KEY",
                    "DEEPSEEK_API_KEY", "FAKE_MODE"}

        def run(name: str, mode: str, runner: str = "zai", timeout: int = 20, model: str = "",
                extra: tuple[str, ...] = ()) -> tuple[int, str]:
            # 污染的父环境：旧网关头、provider 选择器、他家令牌都不得传进评审进程
            (tmp / "fake.json").write_text(json.dumps({"mode": mode, "log": str(log), "final": FINAL,
                                                       "aliases": dict(rrr.RESP_MODEL_ALIAS), "keys": {
                "zai_file": secret, "anth_file": anth_secret, "mmx_file": mmx_secret, "ds_file": ds_secret,
                "zai_parent": "zai-parent-x", "anth_parent": "anth-parent-x", "mmx_parent": "mmx-parent-x",
                "ds_parent": "ds-parent-x"}}))
            # 父环境放另一组键：运行器只认 --env-file，父环境的键不得被用
            env = dict(os.environ, FAKE_MODE=mode, ZAI_API_KEY="zai-parent-x",
                       ANTHROPIC_API_KEY="anth-parent-x", MINIMAX_API_KEY="mmx-parent-x", DEEPSEEK_API_KEY="ds-parent-x",
                       ANTHROPIC_BASE_URL="http://parent-shell-leak.invalid",
                       ANTHROPIC_AUTH_TOKEN="leak-auth", ANTHROPIC_CUSTOM_HEADERS="x-gw-key: leak",
                       CLAUDE_CODE_USE_BEDROCK="1", CLAUDE_CODE_USE_VERTEX="1", GITHUB_TOKEN="ghp_leak")
            out = out_base / name
            cp = subprocess.run([sys.executable, "-I", str(RUNNER), "--runner", runner, "--claude-bin", str(fake),
                                 "--env-file", str(envfile),
                                 "--worktree", str(wt), "--base", "HEAD~1", "--out", str(out), "--roles", ROLES,
                                 "--timeout", str(timeout), *(["--model", model] if model else []), *extra],
                                capture_output=True, text=True, env=env)
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
        # 不传 --timeout 时按通道取默认（anthropic 1200 s / zai、minimax、deepseek 600 s）；显式值优先。dry-run 只写 command.txt，不起进程
        for runner_, extra, want in (("anthropic", [], "timeout=1200s"), ("zai", [], "timeout=600s"),
                                     ("minimax", [], "timeout=600s"), ("deepseek", [], "timeout=600s"),
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

        # minimax 通道：直连官方接口（不经 llm-center）、只认文件里的 MINIMAX_API_KEY、默认模型与超时落进结果块、结果块过闸
        log.write_text("")
        rc, s = run("minimax-ok", "ok", runner="minimax")
        calls = [json.loads(x) for x in log.read_text().splitlines()]
        if not calls or any(c["base"] != "https://api.minimaxi.com/anthropic" or c["key_is"] != "mmx_file" for c in calls):
            fails.append("minimax-ok：BASE_URL 应直连 MiniMax 官方接口（非 llm-center、非父环境值），且只用文件里的 MINIMAX_API_KEY")
        if any(polluted & set(c["env"]) for c in calls):
            fails.append("minimax-ok：父环境的令牌/网关头/provider 选择器不得进子进程")
        if any(not c["cfg_empty"] for c in calls) or len({c["cfg"] for c in calls}) != len(calls):
            fails.append("minimax-ok：每个角色应有各自的一次性空 CLAUDE_CONFIG_DIR")
        if rc != 0 or "simplify:PASS" not in s or "runner=minimax model=MiniMax-M3.1-Flash" not in s or gate_errs(s):
            fails.append(f"minimax-ok：应全 PASS 且标 runner=minimax model=MiniMax-M3.1-Flash（rc={rc}）")
        if secret in log.read_text() or anth_secret in log.read_text() or mmx_secret in log.read_text():
            fails.append("测试日志里出现了密钥原值")
        if any(mmx_secret in p.read_text(errors="replace") for p in (out_base / "minimax-ok").rglob("*") if p.is_file()):
            fails.append("minimax-ok：密钥不得落盘")

        # deepseek 通道：与 zai/anthropic 同走 llm-center，但只认文件里的 DEEPSEEK_API_KEY、默认模型 deepseek-v4.1-flash-mb
        log.write_text("")
        rc, s = run("deepseek-ok", "ok", runner="deepseek")
        calls = [json.loads(x) for x in log.read_text().splitlines()]
        if not calls or any(c["base"] != "https://llm-center.modelbest.co" or c["key_is"] != "ds_file" for c in calls):
            fails.append("deepseek-ok：BASE_URL 应为 llm-center 而非父环境值，且只用文件里的 DEEPSEEK_API_KEY")
        if any(polluted & set(c["env"]) for c in calls):
            fails.append("deepseek-ok：父环境的令牌/网关头/provider 选择器不得进子进程")
        if rc != 0 or "simplify:PASS" not in s or "runner=deepseek model=deepseek-v4.1-flash-mb" not in s or gate_errs(s):
            fails.append(f"deepseek-ok：应全 PASS 且标 runner=deepseek model=deepseek-v4.1-flash-mb（rc={rc}）")
        if any(ds_secret in p.read_text(errors="replace") for p in (out_base / "deepseek-ok").rglob("*") if p.is_file()):
            fails.append("deepseek-ok：密钥不得落盘")

        # llm-center 响应名两形（2026-11-12 实测）：规范名 / 原样请求名 / 一轮混出，都算同一家并过闸
        rc, s = run("ds-raw-echo", "raw_resp", runner="deepseek")
        if rc != 0 or "simplify:PASS" not in s or gate_errs(s):
            fails.append(f"ds-raw-echo：原样回显请求名应全 PASS 且过闸（rc={rc}）")
        rc, s = run("glm-mixed-echo", "mixed_resp", runner="zai")
        if rc != 0 or "simplify:PASS" not in s or gate_errs(s):
            fails.append(f"glm-mixed-echo：规范名与原样名混出应全 PASS 且过闸（rc={rc}）")
        rc, s = run("glm-case-variant", "case_variant", runner="zai")
        if rc != 0 or "simplify:PASS" not in s or gate_errs(s):
            fails.append(f"glm-case-variant：仅大小写不同的回显名应全 PASS 且过闸（rc={rc}）")
        rc, s = run("ds-case-variant", "case_variant", runner="deepseek")
        if rc != 0 or "simplify:PASS" not in s or gate_errs(s):
            fails.append(f"ds-case-variant：仅大小写不同的回显名/用量名应全 PASS 且过闸（rc={rc}）")
        # r2 boundary#1 负控：未登记的 --model（漏 -mb，网关也应答）+ 大小写变体回显——casefold 不得越过别名表放行
        rc, s = run("glm-unregistered-case", "case_variant", runner="zai", model="glm-5.3")
        if rc == 0 or "ERROR" not in s or not gate_errs(s):
            fails.append(f"glm-unregistered-case：表外 model 的大小写变体回显应判 ERROR 且闸门拒收（rc={rc}）")

        # 两通道回显行为已分叉（GLM 回显展示名、Claude 原样回显），模型守卫负控两通道各跑一遍
        for name, mode, want, runner in [("extra-tool", "extra_tool", "TOOLS_VIOLATION", "zai"), ("wrong-init", "wrong_init", "ERROR", "zai"),
                                         ("wrong-resp", "wrong_resp", "ERROR", "zai"), ("wrong-usage", "wrong_usage", "ERROR", "zai"),
                                         ("wrong-init-anth", "wrong_init", "ERROR", "anthropic"),
                                         ("wrong-resp-anth", "wrong_resp", "ERROR", "anthropic"),
                                         ("wrong-usage-anth", "wrong_usage", "ERROR", "anthropic"),
                                         ("wrong-init-mmx", "wrong_init", "ERROR", "minimax"),
                                         ("wrong-resp-mmx", "wrong_resp", "ERROR", "minimax"),
                                         ("wrong-init-ds", "wrong_init", "ERROR", "deepseek"),
                                         ("wrong-resp-ds", "wrong_resp", "ERROR", "deepseek"),
                                         ("wrong-usage-ds", "wrong_usage", "ERROR", "deepseek"),
                                         ("other-lineage-ds", "other_lineage", "ERROR", "deepseek"),
                                         ("other-lineage-raw-ds", "other_lineage_raw", "ERROR", "deepseek"),
                                         ("no-assistant", "no_assistant", "ERROR", "zai"), ("noverdict", "noverdict", "PARTIAL", "zai")]:
            rc, s = run(name, mode, runner=runner)
            if rc == 0 or want not in s or not gate_errs(s):
                fails.append(f"{name}：应判 {want} 且闸门拒收（rc={rc}）")
        rc, s = run("timeout", "hang", timeout=2)
        if rc == 0 or "TIMEOUT" not in s or not gate_errs(s):
            fails.append(f"timeout：应判 TIMEOUT 且闸门拒收（rc={rc}）")
        stt = json.loads((out_base / "timeout" / "status.json").read_text(encoding="utf-8"))
        if not all(stt[r].get("resource_contention") is True for r in stt):  # #3056：执行级 TIMEOUT 属争抢类
            fails.append(f"timeout：TIMEOUT 应标 resource_contention=true（实为 {[stt[r].get('resource_contention') for r in stt]}）")

        # #3056 义务 6：名额排队超时（SlotTimeout）也是争抢类——resource_contention=true，不靠人读 detail
        st_dir = Path(os.environ["PDRS_REVIEW_SLOT_DIR"]) / "minimax"
        st_dir.mkdir(parents=True, exist_ok=True)
        held = os.open(st_dir / "slot0", os.O_RDWR | os.O_CREAT, 0o644)
        fcntl.flock(held, fcntl.LOCK_EX)
        try:
            out = out_base / "slot-timeout"
            cp = subprocess.run([sys.executable, "-I", str(RUNNER), "--runner", "minimax", "--claude-bin", str(fake),
                                 "--env-file", str(envfile), "--worktree", str(wt), "--base", "HEAD~1",
                                 "--out", str(out), "--roles", "simplify"],
                                capture_output=True, text=True,
                                env=dict(os.environ, PDRS_REVIEW_SLOTS_MINIMAX="1", PDRS_REVIEW_SLOT_TIMEOUT="0.5"))
            stq = json.loads((out / "status.json").read_text(encoding="utf-8"))["simplify"]
            if (cp.returncode == 0 or stq.get("status") != "ERROR" or "名额超时" not in stq.get("detail", "")
                    or stq.get("resource_contention") is not True):
                fails.append(f"slot-timeout：等名额超时应判 ERROR 且 resource_contention=true（rc={cp.returncode}、{stq}）")
            print(f"slot-timeout: status={stq.get('status')} contention={stq.get('resource_contention')} detail={stq.get('detail')}")
        finally:
            fcntl.flock(held, fcntl.LOCK_UN)
            os.close(held)

        # —— API 错误响应不得误报「模型不符」；断连重试一次；真模型不符仍要判出（11-06）——
        def run_one(name: str, conf: dict, extra: tuple[str, ...] = ()) -> tuple[int, dict, str, list[str]]:
            trace_f = tmp / f"{name}.trace"
            trace_f.write_text("")
            (tmp / "fake.json").write_text(json.dumps({"log": str(log), "final": FINAL, "keys": {"mmx_file": mmx_secret},
                                                       "aliases": dict(rrr.RESP_MODEL_ALIAS),
                                                       "trace": str(trace_f), **conf}))
            out = out_base / name
            cp = subprocess.run([sys.executable, "-I", str(RUNNER), "--runner", "minimax", "--claude-bin", str(fake),
                                 "--env-file", str(envfile), "--worktree", str(wt), "--base", "HEAD~1",
                                 "--out", str(out), "--roles", "simplify", *extra], capture_output=True, text=True)
            sp = out / "status.json"
            st = json.loads(sp.read_text(encoding="utf-8"))["simplify"] if sp.exists() else {}
            s = (out / "summary.md").read_text(encoding="utf-8") if (out / "summary.md").exists() else ""
            return cp.returncode, st, s, trace_f.read_text().split()

        # ① 429 额度：判「额度耗尽」+ 原文，不判模型不符，且不得重试（重试只会放大占用）
        rc, st, s, ev = run_one("api-quota", {"mode": "api_error_quota"})
        detail = st.get("detail", "")
        if rc == 0 or st.get("status") != "ERROR" or "额度耗尽" not in detail or "模型不符" in detail:
            fails.append(f"api-quota：429 应判额度耗尽而不是模型不符（rc={rc}、{st.get('status')}、{detail}）")
        if "API Error:" not in detail or "429" not in detail or "Token Plan" not in detail:
            fails.append(f"api-quota：detail 应带内嵌错误原文：{detail}")
        if ev.count("start") != 1:
            fails.append(f"api-quota：额度 429 不得重试（起了 {ev.count('start')} 次）")
        if "额度耗尽" not in s or "API Error:" not in s:
            fails.append(f"api-quota：结果行（summary 表格）应写出错误分类与原文：{s}")
        if st.get("resource_contention") is not True:  # #3056 义务 6：429 配额属争抢类
            fails.append(f"api-quota：429 应标 resource_contention=true（实为 {st.get('resource_contention')}）")
        print(f"api-quota: status={st.get('status')} attempts={ev.count('start')} contention={st.get('resource_contention')} detail={detail}")

        # ② ECONNRESET：自动重试一次后 PASS；两次的原始流分别留档
        rc, st, s, ev = run_one("api-conn", {"mode": "api_error_conn_once", "conn_flag": str(tmp / "conn.flag")})
        detail = st.get("detail", "")
        if rc != 0 or st.get("status") != "PASS" or st.get("retried") != 1:
            fails.append(f"api-conn：ECONNRESET 应重试 1 次后 PASS（rc={rc}、{st.get('status')}、retried={st.get('retried')}）")
        if ev.count("start") != 2:
            fails.append(f"api-conn：应恰好重试一次（起了 {ev.count('start')} 次）")
        if "连接断开" not in detail or "ECONNRESET" not in detail:
            fails.append(f"api-conn：detail 应说明首次断连并带原文：{detail}")
        if st.get("resource_contention") is not False:  # #3056 负控：断连（重试成功）不是争抢类
            fails.append(f"api-conn：连接类不得标 resource_contention=true（实为 {st.get('resource_contention')}）")
        if "simplify:PASS" not in s or "roles=simplify:PASS" not in s:
            fails.append(f"api-conn：重试成功应产出 PASS 结果块：{s}")
        if not (out_base / "api-conn" / "simplify.retry1.jsonl").exists():
            fails.append("api-conn：重试的原始事件流应单独留档（simplify.retry1.jsonl），不得覆盖首次现场")
        if (st.get("seconds") or 0) < 0.5:  # 假 CLI 每次尝试 sleep 0.3 s；只记第二次会 < 0.4
            fails.append(f"api-conn：seconds 应累计两次尝试（现 {st.get('seconds')}）")
        print(f"api-conn: status={st.get('status')} retried={st.get('retried')} attempts={ev.count('start')} detail={detail}")

        # ③ 负控：正常响应但 model 字段不同（无 API Error）仍必须判「模型不符」，防把校验改瘫
        rc, st, s, ev = run_one("model-mismatch", {"mode": "wrong_resp"})
        if rc == 0 or st.get("status") != "ERROR" or "模型不符" not in st.get("detail", ""):
            fails.append(f"model-mismatch：正常响应但 model 不同仍须判模型不符（{st.get('status')}、{st.get('detail')}）")
        if st.get("resource_contention") is not False:  # #3056 负控：模型谱系问题不是争抢类
            fails.append(f"model-mismatch：模型不符不得标 resource_contention=true（实为 {st.get('resource_contention')}）")
        print(f"model-mismatch: status={st.get('status')} contention={st.get('resource_contention')} detail={st.get('detail')}")

        # ④ GLM 5 小时额度原文（1308 / Usage limit）同样归「额度耗尽」
        rc, st, s, ev = run_one("api-glm-quota", {"mode": "api_error_glm_quota"})
        if rc == 0 or "额度耗尽" not in st.get("detail", "") or "Usage limit" not in st.get("detail", ""):
            fails.append(f"api-glm-quota：GLM 1308 原文也须判额度耗尽（{st.get('status')}、{st.get('detail')}）")
        if st.get("resource_contention") is not True:  # #3056：五小时配额耗尽同属争抢类
            fails.append(f"api-glm-quota：GLM 1308 应标 resource_contention=true（实为 {st.get('resource_contention')}）")
        print(f"api-glm-quota: status={st.get('status')} contention={st.get('resource_contention')} detail={st.get('detail')}")

        # ⑤ 两次都断连：只重试一次，判 ERROR 且写明「重试 1 次后仍失败」
        rc, st, s, ev = run_one("api-conn-always", {"mode": "api_error_conn_always"})
        detail = st.get("detail", "")
        if (rc == 0 or st.get("status") != "ERROR" or st.get("retried") != 1 or ev.count("start") != 2
                or "仍失败" not in detail):
            fails.append(f"api-conn-always：两次断连应只重试一次后判 ERROR（attempts={ev.count('start')}、"
                         f"retried={st.get('retried')}、{detail}）")
        print(f"api-conn-always: status={st.get('status')} attempts={ev.count('start')} detail={detail}")

        # ⑥ 只说「连接」但没有 ECONNRESET / Connection dropped：分类为连接断开，但不重试
        rc, st, s, ev = run_one("api-conn-noretry", {"mode": "api_error_conn_noretry"})
        if st.get("status") != "ERROR" or "连接断开" not in st.get("detail", "") or ev.count("start") != 1:
            fails.append(f"api-conn-noretry：无 ECONNRESET 标记时不得重试（attempts={ev.count('start')}、{st.get('detail')}）")
        print(f"api-conn-noretry: status={st.get('status')} attempts={ev.count('start')} detail={st.get('detail')}")

        # ⑦ 负控：正常评审正文引用 "API Error:" 且模型正确 → 必须 PASS（否则真评审会被误拒）
        quoted = "发现 1 处引用 `API Error:` 字样。\n覆盖: 全部\nVERDICT: P0=0 P1=0 P2=0 P3=0"
        rc, st, s, ev = run_one("quote-ok", {"mode": "ok", "final": quoted})
        if rc != 0 or st.get("status") != "PASS":
            fails.append(f"quote-ok：正文引用 API Error: 且模型正确时必须 PASS（{st.get('status')}、{st.get('detail')}）")
        print(f"quote-ok: status={st.get('status')} detail={st.get('detail')}")

        # ⑧ 错误原文里带闸门结果块字样：不得原样进 summary（防伪造 pr-gate 块）
        rc, st, s, ev = run_one("api-spoof", {"mode": "api_error_spoof"})
        if s.count("<!-- pr-gate:v1") != 1 or "< !--" not in s:
            fails.append(f"api-spoof：错误原文里的 <!-- pr-gate 必须被拆开（现 {s.count('<!-- pr-gate:v1')} 个块）")
        print(f"api-spoof: status={st.get('status')} blocks={s.count('<!-- pr-gate:v1')} detail={st.get('detail')}")

        # ⑨ 连 assistant 事件都没有：判通道无响应，不得错怪成「模型不符」，也不得重试
        rc, st, s, ev = run_one("no-assistant-err", {"mode": "no_assistant"})
        detail = st.get("detail", "")
        if (st.get("status") != "ERROR" or detail.startswith("模型不符") or "无 assistant 事件" not in detail
                or ev.count("start") != 1):
            fails.append(f"no-assistant-err：应判通道无响应、不判模型不符、不重试（{st.get('status')}、"
                         f"{ev.count('start')} 次、{detail}）")
        print(f"no-assistant-err: status={st.get('status')} attempts={ev.count('start')} detail={detail}")

        # ⑩ 首次断连 + 重试遇 429：detail 两个原因都要写出来，仍 ERROR
        rc, st, s, ev = run_one("api-conn-then-quota", {"mode": "api_error_conn_then_quota", "conn_flag": str(tmp / "conn2.flag")})
        detail = st.get("detail", "")
        if (st.get("status") != "ERROR" or st.get("retried") != 1 or ev.count("start") != 2
                or "连接断开" not in detail or "额度耗尽" not in detail):
            fails.append(f"api-conn-then-quota：应重试一次并把两次原因都写进 detail（{st.get('status')}、"
                         f"retried={st.get('retried')}、{ev.count('start')} 次、{detail}）")
        print(f"api-conn-then-quota: status={st.get('status')} retried={st.get('retried')} attempts={ev.count('start')} detail={detail}")

        # ⑪ 真换模型、但正文引用 API Error / ECONNRESET：仍判「模型不符」，不得按通道错误翻案或重试
        rc, st, s, ev = run_one("wrong-resp-quotes", {"mode": "wrong_resp_quotes"})
        detail = st.get("detail", "")
        if st.get("status") != "ERROR" or "模型不符" not in detail or "连接断开" in detail or ev.count("start") != 1:
            fails.append(f"wrong-resp-quotes：真换模型必须判模型不符，不得被正文里的 API Error 字样翻案（"
                         f"{st.get('status')}、{ev.count('start')} 次、{detail}）")
        print(f"wrong-resp-quotes: status={st.get('status')} attempts={ev.count('start')} detail={detail}")

        # ⑫ 有界上限：5 个超长错误块只留 MAX_ERROR_BLOCKS 条、每条 ≤ MAX_ERROR_TEXT；summary 单元格限长且防伪造
        kept: dict = {}
        for _ in range(5):
            rrr.keep_error_text(kept, "API Error: " + "x" * 20000)
        if (len(kept["resp_texts"]) != rrr.MAX_ERROR_BLOCKS
                or max(len(t) for t in kept["resp_texts"]) != rrr.MAX_ERROR_TEXT):
            fails.append(f"keep_error_text：应只留 {rrr.MAX_ERROR_BLOCKS} 条且每条 ≤ {rrr.MAX_ERROR_TEXT} 字符"
                         f"（实为 {len(kept['resp_texts'])} 条、最长 {max(len(t) for t in kept['resp_texts'])}）")
        cel = rrr.cell("<!-- pr-gate:v1 p0=0 --> | " + "长" * 500)
        if len(cel) > 301 or "<!--" in cel or "|" in cel.replace("\\|", ""):
            fails.append(f"cell：应压行、限长 300、转义竖线并拆开 <!--（{len(cel)}、{cel[:60]}）")
        rc, st, s, ev = run_one("api-big", {"mode": "api_error_big"})
        if st.get("status") != "ERROR" or len(st.get("detail", "")) > 340:
            fails.append(f"api-big：超长错误原文不得整段进 detail（{st.get('status')}、{len(st.get('detail', ''))} 字符）")
        print(f"api-big: status={st.get('status')} detail_len={len(st.get('detail', ''))} kept={len(kept['resp_texts'])}")

        # ⑬ 额度用完就不再重试：重试共用每角色总额，不得把 1 次上限悄悄翻成 2 次
        rc, st, s, ev = run_one("api-conn-nobudget", {"mode": "api_error_conn_once", "conn_flag": str(tmp / "conn3.flag")},
                                extra=("--budget-usd", "0"))
        detail = st.get("detail", "")
        if ev.count("start") != 1 or "未重试" not in detail:
            fails.append(f"api-conn-nobudget：预算为 0 时不得重试（起了 {ev.count('start')} 次、{detail}）")
        print(f"api-conn-nobudget: attempts={ev.count('start')} detail={detail}")

        # ⑭ 流内报错但 CLI 自己重试成功（末事件正常、有合规 VERDICT）：整角色判 PASS，不得被占位符误杀
        rc, st, s, ev = run_one("api-recovered", {"mode": "api_error_recovered"})
        if rc != 0 or st.get("status") != "PASS" or ev.count("start") != 1:
            fails.append(f"api-recovered：内部重试成功的流应判 PASS（{st.get('status')}、{ev.count('start')} 次、{st.get('detail')}）")
        print(f"api-recovered: status={st.get('status')} attempts={ev.count('start')} retried={st.get('retried')}")

        # ⑮ 正文引用 ECONNRESET + 真 429：分类只认真错误块，判额度耗尽、不重试（防把 429 放大）
        rc, st, s, ev = run_one("api-quoted-then-quota", {"mode": "api_error_quoted_then_quota"})
        detail = st.get("detail", "")
        if st.get("status") != "ERROR" or "额度耗尽" not in detail or ev.count("start") != 1:
            fails.append(f"api-quoted-then-quota：正文引用不得把 429 带成断连或触发重试（{st.get('status')}、"
                         f"{ev.count('start')} 次、{detail}）")
        print(f"api-quoted-then-quota: status={st.get('status')} attempts={ev.count('start')} detail={detail}")

        # ⑯ 剩余时限不足就不重试：首跑吃掉大半个 --timeout 后，重试不得重新给满整段时限
        rc, st, s, ev = run_one("api-conn-nolefttime", {"mode": "api_error_conn_slow", "delay": 1.0,
                                                        "conn_flag": str(tmp / "conn4.flag")}, extra=("--timeout", "2"))
        detail = st.get("detail", "")
        if ev.count("start") != 1 or "未重试" not in detail or "剩余时限 0s" not in detail:
            fails.append(f"api-conn-nolefttime：剩余时限不足时不得重试（起了 {ev.count('start')} 次、{detail}）")
        print(f"api-conn-nolefttime: attempts={ev.count('start')} detail={detail}")

        # ⑰ 只有 <synthetic> 占位事件但 result 成功：real_resp 为空时 issubset 恒真不得放行，判模型不符
        rc, st, s, ev = run_one("synthetic-only", {"mode": "synthetic_only_success"})
        detail = st.get("detail", "")
        if st.get("status") != "ERROR" or "模型不符" not in detail:
            fails.append(f"synthetic-only：占位响应不得过模型守卫（{st.get('status')}、{detail}）")
        print(f"synthetic-only: status={st.get('status')} detail={detail}")

        # 缺钥：fixture 文件没有该键、父环境却有——必须拒绝，不回退父环境（无条件断言，开发机与 CI 一致）
        empty = tmp / "empty.env"
        empty.write_text("OTHER=1\n")
        log.write_text("")
        for runner in ("zai", "anthropic", "minimax", "deepseek"):
            cp = subprocess.run([sys.executable, "-I", str(RUNNER), "--runner", runner, "--claude-bin", str(fake),
                                 "--env-file", str(empty), "--worktree", str(wt), "--base", "HEAD~1",
                                 "--out", str(out_base / f"nokey-{runner}")], capture_output=True, text=True,
                                env=dict(os.environ, ZAI_API_KEY="zai-parent-x", ANTHROPIC_API_KEY="anth-parent-x",
                                         MINIMAX_API_KEY="mmx-parent-x", DEEPSEEK_API_KEY="ds-parent-x"))
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
        # codex（官方 GPT 谱系）登记后要有正负控：合法组合放行、错配与空后缀拒收（#1599 r4 评审 P2）。
        codex_block = ok_block.replace("runner=zai", "runner=codex").replace(
            "model=glm-5.3-mb", "model=gpt-6.1-terra")
        if gate_errs(codex_block):
            fails.append(f"闸门：runner=codex model=gpt-* 应接受：{gate_errs(codex_block)}")
        for bad, why in [(codex_block.replace("model=gpt-6.1-terra", "model=glm-5.3-mb"), "codex 配 glm"),
                         (codex_block.replace("model=gpt-6.1-terra", "model=gpt-"), "codex 空后缀"),
                         (ok_block.replace("model=glm-5.3-mb", "model=glm-"), "zai 空后缀")]:
            if not gate_errs(bad):
                fails.append(f"闸门：{why} 应拒收")

        # —— #3056 修复轮字段（义务 3/E/G）：--fixtype 落块、闸门按字面三值与首行 fix-rN 判定 ——
        rc, s = run("fixtype-struct", "ok", extra=("--fixtype", "结构性"))
        if rc != 0 or " fixtype=结构性 " not in s:
            fails.append(f"fixtype：--fixtype 结构性 应写进结果块 fixtype= 键（rc={rc}）")
        fp3 = gate.patch_fingerprint(str(wt), "HEAD~1", head)
        residual = "<!-- pdrs-residual: 上轮漏网是边界暴露：仅修饰窗一词，修法本身成立 -->\n"

        def fixpr(block: str, rnd: int = 5, res: str = residual) -> str:
            return f"fix(gateway) #1 fix-r{rnd}：停泊补 waker\n\n{block}\n{res}Refs #1\n"

        if gate.check(fixpr(s), head, fp3):
            fails.append(f"fixtype 正控：fix-r5 块带 fixtype=结构性 + residual 应过闸：{gate.check(fixpr(s), head, fp3)}")
        if gate.check(fixpr(s, rnd=3, res=""), head, fp3):
            fails.append("fixtype 正控：fix-r3（N<4）不要求 residual 也应过闸")
        for bad, why, want in [
            (s.replace(" fixtype=结构性", ""), "修复轮缺 fixtype", "须带 fixtype"),
            (s.replace("fixtype=结构性", "fixtype=结构派生"), "子类字样 结构派生", "字面三值"),
            (s.replace("fixtype=结构性", "fixtype=结构性 fixtype=枚举式"), "fixtype 重复", "至多出现一次"),
        ]:
            errs = gate.check(fixpr(bad), head, fp3)
            if not any(want in e for e in errs):
                fails.append(f"闸门：{why} 应拒收并说明「{want}」：{errs}")
        errs = gate.check(fixpr(s, res=""), head, fp3)
        if not any("pdrs-residual" in e for e in errs):
            fails.append(f"闸门：fix-r5 缺 residual 应拒收：{errs}")
        errs = gate.check(fixpr(s, res="<!-- pdrs-residual:    -->\n"), head, fp3)
        if not any("空白" in e for e in errs):
            fails.append(f"闸门：residual 空白串应拒收：{errs}")
        errs = gate.check(fixpr(s, res="<!-- pdrs-residual: 无上一轮 -->\n"), head, fp3)
        if errs:
            fails.append(f"闸门：residual 字面值「无上一轮」应放行：{errs}")

        # G（#3056 阻塞前置）：多块凑双谱系须同头或同一补丁指纹（head 置空隔离出 G 判定本身）
        b1 = s
        b2 = s.replace(f"head={head}", "head=" + "1" * 40)
        b3 = s.replace(f"head={head}", "head=" + "2" * 40).replace(f" patch={fp3}", " patch=" + "3" * 40)

        def gbody(blocks: list[str]) -> str:
            return f"fix(gateway) #1 fix-r5：多块拼谱系\n\n{''.join(blocks)}Refs #1\n{residual}"

        if gate.check(gbody([b1, b1]), "", ""):
            fails.append("G：同头两块应接受")
        if gate.check(gbody([b1, b2]), "", ""):
            fails.append("G：不同头但同一补丁指纹的两块应接受")
        errs = gate.check(gbody([b2, b3]), "", "")
        if not any("多块不一致" in e for e in errs):
            fails.append(f"G：不同头且不同指纹的两块应拒收：{errs}")
        # 义务 3：waiver 轮载体——修法类型写进豁免标记内，理由长度不计 fixtype 词汇
        w_ok = "fix(gateway) #1 fix-r5：纯文档改动\n\n<!-- pr-gate:waiver 纯文档轮豁免：只改说明文字不碰代码 fixtype=结构性 -->\n" \
               f"{residual}Refs #1\n"
        if gate.check(w_ok, head):
            fails.append(f"waiver 正控：fix-r5 豁免带 fixtype=结构性 + residual 应过闸：{gate.check(w_ok, head)}")
        w_r3 = "fix(gateway) #1 fix-r3：纯文档改动\n\n<!-- pr-gate:waiver 纯文档轮豁免：只改说明文字不碰代码 fixtype=枚举式 -->\nRefs #1\n"
        if gate.check(w_r3, head):
            fails.append(f"waiver 正控：fix-r3 豁免不要求 residual：{gate.check(w_r3, head)}")
        for bad, want in [
            ("fix(gateway) #1 fix-r5：纯文档改动\n\n<!-- pr-gate:waiver 纯文档轮豁免只改说明文字不碰代码 -->\n"
             f"{residual}Refs #1\n", "须携带修法类型"),
            ("fix(gateway) #1 fix-r5：纯文档改动\n\n<!-- pr-gate:waiver 纯文档轮豁免只改说明文字 fixtype=结构派生 -->\n"
             f"{residual}Refs #1\n", "字面三值"),
            ("fix(gateway) #1 fix-r5：纯文档改动\n\n<!-- pr-gate:waiver 太短了 fixtype=结构性 -->\n"
             f"{residual}Refs #1\n", "豁免理由太短"),
            ("fix(gateway) #1 fix-r5：纯文档改动\n\n<!-- pr-gate:waiver 纯文档轮豁免：只改说明文字不碰代码 fixtype=结构性 -->\nRefs #1\n",
             "pdrs-residual"),
        ]:
            errs = gate.check(bad, head)
            if not any(want in e for e in errs):
                fails.append(f"waiver 负控：应拒收并说明「{want}」：{errs}")

        # --serial：同一进程逐个跑四角色（并行全跑在本机会挂起），产出单个四角色块；进程起止不得重叠
        trace = tmp / "trace.txt"
        for mode_name, serial_flag in (("serial", True), ("parallel", False)):
            trace.write_text("")
            (tmp / "fake.json").write_text(json.dumps({"mode": "ok", "log": str(log), "final": FINAL, "keys": {},
                                                       "aliases": dict(rrr.RESP_MODEL_ALIAS),
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
        # 通道并发闸（11-05 缺口 2）：anthropic 只有 2 路，四角色必须排队——峰值并发 = 2 且打出过 waiting
        trace.write_text("")
        (tmp / "fake.json").write_text(json.dumps({"mode": "ok", "log": str(log), "final": FINAL, "keys": {},
                                                   "aliases": dict(rrr.RESP_MODEL_ALIAS),
                                                   "trace": str(trace)}))
        out = out_base / "anthropic-slots"
        cp = subprocess.run([sys.executable, "-I", str(RUNNER), "--runner", "anthropic", "--claude-bin", str(fake),
                             "--env-file", str(envfile), "--worktree", str(wt), "--base", "HEAD~1",
                             "--out", str(out), "--roles", ROLES], capture_output=True, text=True)
        ev = [(float(x.split()[1]), x.split()[0]) for x in trace.read_text().splitlines()]
        depth = peak = 0
        for _, kind in sorted(ev):
            depth += 1 if kind == "start" else -1
            peak = max(peak, depth)
        if cp.returncode != 0 or peak != 2 or len(ev) != 8 or "[review-slot] waiting 2/2" not in cp.stderr:
            fails.append(f"anthropic 并发闸：四角色应排队在 2 路上（峰值 {peak}、事件 {len(ev)}、rc={cp.returncode}；"
                         f"stderr 应含 [review-slot] waiting 2/2）")
        # --serial 首角色超时：后续三个角色仍须照跑，块含四角色、首角色 TIMEOUT，返回非 0 且闸门拒收
        trace.write_text("")
        marker = tmp / "hang-first.flag"
        (tmp / "fake.json").write_text(json.dumps({"mode": "ok", "log": str(log), "final": FINAL, "keys": {},
                                                   "aliases": dict(rrr.RESP_MODEL_ALIAS),
                                                   "trace": str(trace), "hang_first": str(marker)}))
        out = out_base / "serial-timeout-first"
        cp = subprocess.run([sys.executable, "-I", str(RUNNER), "--claude-bin", str(fake), "--env-file", str(envfile),
                             "--worktree", str(wt), "--base", "HEAD~1", "--out", str(out), "--roles", ROLES,
                             "--serial", "--timeout", "2"], capture_output=True, text=True)
        ends = trace.read_text().split().count("end")
        s = (out / "summary.md").read_text(encoding="utf-8") if (out / "summary.md").exists() else ""
        first, rest = ROLES.split(",")[0], ROLES.split(",")[1:]
        if (cp.returncode == 0 or not gate_errs(s) or ends != 3 or f"{first}:TIMEOUT" not in s
                or any(f"{r}:PASS" not in s for r in rest)):
            fails.append(f"serial-timeout：首角色超时后应照跑其余三角色并拒收（rc={cp.returncode}，完成 {ends}）")

        # #3056 H（义务 7）：复跑须独立 out 目录（原件不得覆盖）、rec 标 rerun、summary 注明
        rc, s = run("rerun-ok", "ok", extra=("--rerun", "--fixtype", "结构性"))
        st_r = json.loads((out_base / "rerun-ok" / "status.json").read_text(encoding="utf-8"))
        if rc != 0 or not all(st_r[r].get("rerun") is True for r in st_r):
            fails.append(f"rerun：--rerun 应给每条 rec 标 rerun=true（rc={rc}、{[st_r[r].get('rerun') for r in st_r]}）")
        if "rerun=true" not in s or "复跑轮" not in s:
            fails.append("rerun：summary 结果块应带 rerun=true 并注明复跑轮（绑定当轮 head、原件未动）")
        cp = subprocess.run([sys.executable, "-I", str(RUNNER), "--runner", "zai", "--claude-bin", str(fake),
                             "--env-file", str(envfile), "--worktree", str(wt), "--base", "HEAD~1",
                             "--out", str(out_base / "rerun-ok"), "--roles", ROLES, "--rerun"],
                            capture_output=True, text=True, env=dict(os.environ))
        if cp.returncode != 2 or "原件" not in cp.stderr:
            fails.append(f"rerun-clobber：out 目录已有评审原件应 rc=2 拒跑（rc={cp.returncode}、{cp.stderr.strip()[:150]}）")
        # 非法 fixtype 值：argparse 在起进程前拒绝（rc 2）
        cp = subprocess.run([sys.executable, "-I", str(RUNNER), "--claude-bin", str(fake), "--env-file", str(envfile),
                             "--worktree", str(wt), "--base", "HEAD~1", "--out", str(out_base / "badfixtype"),
                             "--roles", ROLES, "--dry-run", "--fixtype", "结构派生"],
                            capture_output=True, text=True, env=dict(os.environ))
        if cp.returncode != 2:
            fails.append(f"fixtype：非法值「结构派生」应被 argparse 拒绝（rc={cp.returncode}）")
    _carry_tests(gate, fails)
    _fixtype_path_tests(gate, fails)
    for f in fails:
        print("FAIL", f)
    print(f"test-review-roles: {'全部通过' if not fails else f'{len(fails)} 项失败'}")
    return 1 if fails else 0



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


def _fixtype_path_tests(gate, fails: list[str]) -> None:
    """#3056 义务 4/5：fixtype=不适用 的 diff 级路径检查（ci_scope 语义唯一口径，fail-closed）。

    样例路径只写在临时 git 仓库里（不读仓库真实 state/board.md，已在 BOARD_RULER_ALLOWLIST 登记）。
    """
    with tempfile.TemporaryDirectory() as td:
        repo = Path(td)
        g = ["git", "-C", str(repo), "-c", "user.email=t@t", "-c", "user.name=t"]

        def git(*args: str) -> str:
            return subprocess.run([*g, *args], check=True, capture_output=True, text=True).stdout.strip()

        def commit(path: str, text: str, msg: str) -> str:
            (repo / path).parent.mkdir(parents=True, exist_ok=True)
            (repo / path).write_text(text)
            git("add", path)
            git("commit", "-q", "-m", msg)
            return git("rev-parse", "HEAD")

        git("init", "-q", "-b", "main")
        base = commit("f.txt", "base\n", "base")
        h_docs = commit("state/board.md", "b1\n", "docs-only")  # 只碰白名单路径（ci_scope DOCS_ONLY_PATH）
        h_code = commit("scripts/x.py", "print(1)\n", "code")  # 碰代码路径

        def fixbody(h: str, rnd: int = 4) -> str:
            return (f"fix(scripts) #2 fix-r{rnd}：纯文档改动\n\n"
                    f"<!-- pr-gate:v1 head={h} roles=sibling:PASS,concurrency:PASS,boundary:PASS,simplify:PASS "
                    f"p0=0 p1=0 scope=full runner=zai model=glm-5.3-mb readonly=tools fixtype=不适用 -->\n"
                    "<!-- pdrs-residual: 无上一轮 -->\nRefs #2\n")

        # 正控：diff 只碰文档白名单 → 不适用 放行（真实数据复算，非写死值）
        if gate.check(fixbody(h_docs), h_docs, "", base, str(repo)):
            fails.append(f"fixtype-path 正控：docs-only diff 写不适用应放行：{gate.check(fixbody(h_docs), h_docs, '', base, str(repo))}")
        # 负控：触及代码路径（scripts/）却写不适用 → 拒
        errs = gate.check(fixbody(h_code), h_code, "", base, str(repo))
        if not any("不限于文档白名单" in e for e in errs):
            fails.append(f"fixtype-path 负控：触及代码路径却写不适用应拒收：{errs}")
        # fail-closed：缺 --base/--repo 无法复算 → 拒（宁拒不漏）
        errs = gate.check(fixbody(h_docs), h_docs, "", "", "")
        if not any("fail-closed" in e for e in errs):
            fails.append(f"fixtype-path：缺 base/repo 复算不出应 fail-closed 拒收：{errs}")
        # N<4 同样按声明即查（与轮数判定解耦）；fix-r3 带不适用也走路径检查
        if gate.check(fixbody(h_docs, rnd=3), h_docs, "", base, str(repo)):
            fails.append("fixtype-path：fix-r3 docs-only + 不适用 应放行")
        # waiver 载体带不适用：同样走 diff 级路径检查
        w_na = ("fix(scripts) #2 fix-r4：纯文档改动\n\n"
                "<!-- pr-gate:waiver 纯文档轮豁免：只改状态文件叙述 fixtype=不适用 -->\n"
                "<!-- pdrs-residual: 无上一轮 -->\nRefs #2\n")
        if gate.check(w_na, h_docs, "", base, str(repo)):
            fails.append(f"fixtype-path 正控：waiver + 不适用（docs-only）应放行：{gate.check(w_na, h_docs, '', base, str(repo))}")
        errs = gate.check(w_na, h_code, "", base, str(repo))
        if not any("不限于文档白名单" in e for e in errs):
            fails.append(f"fixtype-path 负控：waiver + 不适用（触代码路径）应拒收：{errs}")
        # 非 fix 轮声明 fixtype=不适用（首行无 fix-rN）：声明即查——「不适用」断言 docs-only，可复算必须复算
        na_plain = (f"<!-- pr-gate:v1 head={h_code} roles=sibling:PASS,concurrency:PASS,boundary:PASS,simplify:PASS "
                    f"p0=0 p1=0 scope=full runner=zai model=glm-5.3-mb readonly=tools fixtype=不适用 -->\nRefs #2\n")
        errs = gate.check(na_plain, h_code, "", base, str(repo))
        if not any("不限于文档白名单" in e for e in errs):
            fails.append(f"fixtype-path：非修复轮声明不适用（触代码路径）也应拒收：{errs}")


if __name__ == "__main__":
    sys.exit(main())
