#!/usr/bin/env python3
"""test-review-roles.py — run-review-roles.py 与 check-pr-gate 的本地测试（假 claude CLI，不花模型钱，Linux/macOS 均可跑）。

用法：python3 -I scripts/test-review-roles.py   退出码 0 全过 · 1 有失败
"""
from __future__ import annotations

import ast
import datetime as dt
import fcntl
import importlib.util
import json
import os
import re
import shutil
import signal
import subprocess
import sys
import tempfile
import time
import warnings
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
RUNNER = ROOT / "scripts" / "run-review-roles.py"
ROLES = "sibling,concurrency,boundary,simplify"
FINAL = "无问题\n覆盖: 全部\nVERDICT: P0=0 P1=0 P2=0 P3=0"
# fix-r17 簇①（DS sibling#1/DS boundary#1/MM sibling#1/MM boundary#1/MM boundary#2 五撞同源）：
# 非换行空白成员表**单源**——r12「单字符纯空白续写」成员循环与 r17「空白块×标点」组合正控表
# 共用这一份，防第二份成员表漂移（r12 手抄 U+3000/NBSP/U+2009、r16 又只手抄单个 ASCII 空格
# 正是同一反模式）。成员＝纯空白分支 `[^\S\r\n]+` 能吃的非换行空白 + ASCII 空白块（1/3 个）。
WS_GAP_MEMBERS = (
    (" ", "ASCII 空格×1"),
    ("   ", "ASCII 空格×3"),
    ("　", "全角空格 U+3000"),
    ("\xa0", "NBSP U+00A0"),
    (" ", "thin space U+2009"),
)

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


def _signame(rc: int) -> str:
    """fix-r20（MM r19 concurrency#2 P2）：探针 returncode<0＝子进程被信号杀——翻成
    信号名（如 -24 → SIGXCPU）写进失败文案；裸负数看不出是哪路资源超限（CPU 硬限/
    墙钟超时/kill），诊断要多翻一层日志。"""
    if rc >= 0:
        return str(rc)
    try:
        return f"{rc} {signal.Signals(-rc).name}"
    except ValueError:  # 非信号取值（理论不可达）——退回裸数字
        return str(rc)


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

        # —— #3056 修复轮字段（义务 3/E/G）：--fixtype 落块、闸门按字面三值与正文首行兜底判定
        # （本组不传 --base/--repo，走 fix-r6 的本地无 head 路径：fixpr 正文首行自带 fix-rN）——
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

        # P0（DS boundary#1，#3056 fix-r1）：CJK 连写不吞轮号——Python \w 含 CJK，旧 \w 边界把
        # 「修复fix-r5」「fix-r5轮」整轮判没（fixtype/residual/N≥4 全跳、闸门假 PASS）
        if gate.fix_round_from_body("修复fix-r5：停泊补 waker") != 5:
            fails.append("P0 正控：中文紧贴 fix-r5 应识别为修复轮 5")
        if gate.fix_round_from_body("fix-r5轮继续修") != 5:
            fails.append("P0 正控：fix-r5 后紧贴中文应识别为修复轮 5")
        for bad in ("afix-r5", "Afix-r5", "fix-r5x", "prefix-fix-r5"):
            if gate.fix_round_from_body(bad) is not None:
                fails.append(f"P0 负控：{bad!r} 不应识别为修复轮")
        cjk_body = f"fix(gateway) #1 修复fix-r5：停泊补 waker\n\n{s}\n{residual}Refs #1\n"
        if gate.check(cjk_body, head, fp3):
            fails.append(f"P0 正控：中文连写 fix-r5 + 齐字段应过闸：{gate.check(cjk_body, head, fp3)}")
        errs = gate.check(cjk_body.replace(" fixtype=结构性", ""), head, fp3)
        if not any("须带 fixtype" in e for e in errs):
            fails.append(f"P0 负控：中文连写 fix-r5 缺 fixtype 应按修复轮拒收（不再被当非修复轮跳过）：{errs}")
        # P1（DS sibling#1/boundary#2）：PR 侧 pdrs-residual 与任务书侧同口径——
        # 占位符不算答、围栏 / 引用块 / 缩进块里的示例不算声明（复用 strip_fences / placeholder_in）
        errs = gate.check(fixpr(s, res="<!-- pdrs-residual: <上轮残差判读> -->\n"), head, fp3)
        if not any("占位符" in e for e in errs):
            fails.append(f"residual 负控：占位符内容应拒收：{errs}")
        errs = gate.check(fixpr(s, res="```\n<!-- pdrs-residual: 围栏里的示例不算声明 -->\n```\n"), head, fp3)
        if not any("pdrs-residual" in e and "缺" in e for e in errs):
            fails.append(f"residual 负控：围栏内示例不算声明（应判缺）：{errs}")
        # CRLF 正文：行尾 \r 不得让 residual 标记匹配不上（假 FAIL；任务书侧先例）
        crlf_body = fixpr(s).replace("\n", "\r\n")
        if gate.check(crlf_body, head, fp3):
            fails.append(f"CRLF 正控：CRLF 正文的 residual 标记应照常识别：{gate.check(crlf_body, head, fp3)}")

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
        # item 8（DS boundary#3）：值后紧贴中文标点不得并进值——「结构性，纯文档」判缺/非法；
        # 值后空格分隔（fixtype=结构性 说明文字）是合法载体
        w_sp = "fix(gateway) #1 fix-r5：纯文档改动\n\n<!-- pr-gate:waiver 纯文档轮豁免 fixtype=结构性 只改说明文字不碰代码 -->\n" \
               f"{residual}Refs #1\n"
        if gate.check(w_sp, head):
            fails.append(f"waiver 正控：值后空格分隔的 fixtype 应过闸：{gate.check(w_sp, head)}")
        w_cjk = "fix(gateway) #1 fix-r5：纯文档改动\n\n<!-- pr-gate:waiver 纯文档轮豁免只改说明文字不碰代码 fixtype=结构性，纯文档轮 -->\n" \
                f"{residual}Refs #1\n"
        errs = gate.check(w_cjk, head)
        if not any("字面三值" in e for e in errs):
            fails.append(f"waiver 负控：fixtype=结构性，纯文档（值后紧贴中文标点）应判缺/非法：{errs}")
        # item 12（MM sibling#7）：非修复轮的豁免标记「声明即查」——不再静默放行非法值
        nf_bad = "<!-- pr-gate:waiver 非修复轮纯文档豁免说明文字足够长 fixtype=结构派生 -->\nRefs #2\n"
        errs = gate.check(nf_bad, head)
        if not any("字面三值" in e for e in errs):
            fails.append(f"waiver 负控：非修复轮 fixtype=结构派生 应声明即查拒收：{errs}")
        nf_ok = "<!-- pr-gate:waiver 非修复轮纯文档豁免说明文字足够长 fixtype=结构性 -->\nRefs #2\n"
        if gate.check(nf_ok, head):
            fails.append(f"waiver 正控：非修复轮声明合法 fixtype 应过闸：{gate.check(nf_ok, head)}")

        # —— #3056 fix-r2：CJK 词边界 / 围栏未闭合 / 围栏口径统一 / 首行识别（修 3①/6/8/9）——
        # 修 3①（MM2 sibling#1）：\b 换成 ASCII 环视——中文紧贴不再吞边界（「已Fixes #1」旧口径
        # \b 两侧都是 \w → 判「缺 issue 关联」，合规 PR 假 FAIL）
        if gate.check("<!-- pr-gate:waiver 纯文档轮豁免说明文字足够长 -->\n已Fixes #1\n", head):
            fails.append(f"修3① 正控：「已Fixes #1」应算 issue 关联：{gate.check('<!-- pr-gate:waiver 纯文档轮豁免说明文字足够长 -->\\n已Fixes #1\\n', head)}")
        if gate.check("<!-- pr-gate:waiver 纯文档轮豁免说明文字足够长 -->\nRefs #3056的说明见正文\n", head):
            fails.append("修3① 正控：「Refs #3056的说明」应算 issue 关联")
        errs = gate.check("<!-- pr-gate:waiver 纯文档轮豁免说明文字足够长 -->\nFixes #12x 不是关联\n", head)
        if not any("缺 issue" in e for e in errs):
            fails.append(f"修3① 负控：「Fixes #12x」不应算 issue 关联：{errs}")
        # MARKER 同族：结果块 pr-gate:v1 后中文紧贴仍识别（旧 \b 会判「缺结果块」）
        if gate.check(fixpr(s.replace("pr-gate:v1 ", "pr-gate:v1中文注记 ", 1)), head, fp3):
            fails.append(f"修3① 正控：pr-gate:v1 后中文紧贴的结果块应识别：{gate.check(fixpr(s.replace('pr-gate:v1 ', 'pr-gate:v1中文注记 ', 1)), head, fp3)}")

        # 修 6（DS boundary#1 = MM2 boundary#1）：围栏未闭合 → 报「围栏没有闭合」而不是「缺 pdrs-residual」
        errs = gate.check(fixpr(s, res="```\n<!-- pdrs-residual: 上轮洞是边界暴露，围栏未吞我 -->\n"), head, fp3)
        if not any("围栏没有闭合" in e for e in errs) or any("pdrs-residual" in e for e in errs):
            fails.append(f"修6 负控：围栏未闭合应报「围栏没有闭合」（fail-closed）而不是「缺 pdrs-residual」：{errs}")

        # 修 8（DS sibling#1）：围栏 / 引用块 / 缩进块里的示例不算声明——blocks / waiver / issue 关联同一口径
        bad_head_block = s.replace(f"head={head}", "head=" + "9" * 40).replace(f" patch={fp3}", "")
        fenced = "```\n" + bad_head_block + "```\n"
        if gate.check(f"fix(gateway) #1 fix-r5：围栏示例不算块\n\n{s}\n{fenced}{residual}Refs #1\n", head, fp3):
            fails.append(f"修8 正控：围栏里的示例结果块（坏头无指纹）不应计入块数：{gate.check(f'fix(gateway) #1 fix-r5：围栏示例不算块\\n\\n{s}\\n{fenced}{residual}Refs #1\\n', head, fp3)}")
        errs = gate.check("```\n<!-- pr-gate:waiver 围栏里的示例豁免说明文字足够长 -->\n```\nRefs #1\n", head)
        if not any("缺角色审计结果块" in e for e in errs):
            fails.append(f"修8 负控：围栏里的示例豁免不算声明（应判缺块）：{errs}")
        errs = gate.check("<!-- pr-gate:waiver 纯文档轮豁免说明文字足够长 -->\n```\nRefs #1\n```\n", head)
        if not any("缺 issue 关联" in e for e in errs):
            fails.append(f"修8 负控：围栏里的 Refs #1 不算 issue 关联：{errs}")
        errs = gate.check("<!-- pr-gate:waiver 纯文档轮豁免说明文字足够长 -->\n> Refs #1 引用块里的示例\n", head)
        if not any("缺 issue 关联" in e for e in errs):
            fails.append(f"修8 负控：引用块里的 Refs #1 不算 issue 关联：{errs}")

        # —— #3056 fix-r5 修 2（boundary#2 P1）配对判定正控（逐条标题判据）：#N 与 fix-r<N>
        # 就近配对，配对集 ∩ 本 PR 单号 ≠ ∅ 才计入；标题无任何 #N 时全 token 计入（宁高勿低
        # fail-closed）。词表正控（fix-r4 停点报告 §三 六形 + 冒号/全角/散文间隔形）换
        # (标题, issues, 期望) 三元组——issues 从正文 Refs/Closes 提取（check() 同款口径）
        for t, iv, want in [("fix(scripts) PR #3047 fix-r3：修 DS boundary#1 与 MM boundary#1 双撞", {"3047"}, 3),
                            ("chore(tools) 分段检查器 fix-r1：堵四个假 PASS 面", frozenset(), 1),
                            ("chore(tools) fix-r21 WIP 快照：run-review-roles 接 minimax 通道", frozenset(), 21),
                            ("chore(state) 01:45 #1305 fix-r3 P0=0 P1=0 两家收", {"1305"}, 3),
                            ("fix(gateway) #1 修复fix-r5：停泊补 waker", {"1"}, 5),
                            ("fix(scripts) #3056+#3057 fix-r3：同轮双单", {"3056"}, 3),
                            ("fix(scripts): #3074 fix-r3：修闸门", {"3074"}, 3),
                            ("fix: #3074 fix-r3：修闸门", {"3074"}, 3),
                            ("fix(tools)：#3056 fix-r2：修台账", {"3056"}, 2),
                            ("chore: retrigger CI for the fix-r1 Linux close-kind probe", frozenset(), 1)]:
            if gate.fix_round_from_title(t, iv) != want:
                fails.append(f"fix-r5 正控：head 标题应判修复轮 {want}：{t[:56]!r}")
        # 修 2 低估方向负控（任务书硬要求：低估必须红）——配对集与 issues 不相交 → 整条
        # 不判：引用别的单的轮号不得当成本轮；空集 issues（正文没写关联）配对 token 也不计
        # （不放大「正文没写关联」的违规——该违规由规则 2 自己拒收）
        for t, iv in [("fix(scripts) #3057 fix-r3：同轮双单", {"3056"}),
                      ("fix(gateway) #1 修复fix-r5：停泊补 waker", {"2"}),
                      ("fix(scripts) #3056+#3057 fix-r3：同轮双单", {"999"}),
                      ("fix(scripts): #3074 fix-r3：修闸门", frozenset())]:
            if gate.fix_round_from_title(t, iv) is not None:
                fails.append(f"fix-r5 修2 负控：标题轮号配不到本 PR 单号不应判修复轮：{t[:56]!r}")
        for t in ("fixed #1 fix-r5 的对照", 'Revert "fix(scripts) #1 fix-r5：停泊补 waker"',
                  "fix-r2 #2516 收尾", "fix(scripts) 没有轮号的普通提交"):
            if gate.fix_round_from_title(t) is not None:
                fails.append(f"fix-r5 负控：head 标题不应判修复轮：{t[:56]!r}")
        # 四负控（停点报告 §三 同一组，原正文散文负控）：本地/空集/CI 三形态的兜底都走
        # 「首行宽松 + 全正文结构化回显」（fix-r3 P0③ 起 CI 形态同口径）——非首行散文
        # （含旧 TITLE_ECHO 会误吞的 `test: #123 fix-r5 的场景已补`）全拒。fix-r8
        # boundary#3（MM r7 评审）：调用改走生产路径 echo_scan=True（check() 一律
        # echo_scan=True，负控走默认 False 是假绿——把该参数删掉/改回 False 时本负控
        # 不再钉住回显误入面）；改前实测（_tmp/20261114-3074-fixr8/）：7 条散文在
        # echo_scan=True + issues 默认 ∅ 下仍全 None——ECHO_HEAD 只命中
        # `fix(gateway) #1 fix-r5：停泊补 waker`，链 {1} ∩ ∅ = ∅ → 不计。
        for prose in ("test 覆盖了 fix-r5 的场景", "fixed #1 fix-r5",
                     "test: #123 fix-r5 的场景已补",
                     "chore 更新 board：#1357 依赖普查完成，顺带记录了 fix-r3 的现场",
                     "fix(gateway) #1 fix-r5：停泊补 waker",
                     "fix-r5 任务书在 _tmp 目录", "上轮 fix-r5 评审指出的问题已处理。"):
            if gate.fix_round_from_body("## 概述\n\n" + prose + "\n", echo_scan=True) is not None:
                fails.append(f"fix-r5 负控：正文非首行（散文/回显/提及）不应判修复轮：{prose[:44]!r}")
        # 修 4（boundary#4 P2）本地形态：echo_scan=True 只认**结构化**回显——ECHO_HEAD 要
        # scope 括号 + #N，散文行（类型词开头但无括号/无 #N、fixed、非类型词开头）不可入
        for prose in ("test 覆盖了 fix-r5 的场景", "fixed #1 fix-r5",
                     "test: #123 fix-r5 的场景已补",
                     "chore 更新 board：#1357 依赖普查完成，顺带记录了 fix-r3 的现场",
                     "fix-r5 任务书在 _tmp 目录", "上轮 fix-r5 评审指出的问题已处理。"):
            if gate.fix_round_from_body("## 概述\n\n" + prose + "\n", echo_scan=True,
                                        issues=frozenset({"1"})) is not None:
                fails.append(f"fix-r5 修4 负控：本地回显扫描只认结构化回显，散文行不可入：{prose[:44]!r}")
        # 修 4 正控：本地形态首行干净、回显在非首行、#N 配到本 PR 单 → 判修复轮；配不到
        # 本单 → 不计（配对判定的另一半）；首行命中则不进扫描（宽松口径不受 issues 影响）
        if gate.fix_round_from_body("![ci](https://example.com/b.svg)\n\nfix(scripts): #1 fix-r5：修闸门\n\nRefs #1\n",
                                    echo_scan=True, issues=frozenset({"1"})) != 5:
            fails.append("fix-r5 修4 正控：首行干净 + 非首行结构化回显（#1 配到本单）应判修复轮 5")
        if gate.fix_round_from_body("![ci](https://example.com/b.svg)\n\nfix(scripts): #3074 fix-r3：修闸门\n\nRefs #1\n",
                                    echo_scan=True, issues=frozenset({"1"})) is not None:
            fails.append("fix-r5 修4 负控：本地回显行 #3074 配不到本 PR 单（Refs #1）不应判修复轮")
        if gate.fix_round_from_body("修复fix-r5：停泊补 waker\n\nfix(scripts): #3074 fix-r3：x\n",
                                    echo_scan=True, issues=frozenset({"1"})) != 5:
            fails.append("fix-r5 修4 边界：首行宽松命中即返回，不进回显扫描（首行不受配对影响）")

        # 主信号走真实 git（fix-r6 集合语义）：独立夹具仓（不碰 run() 用的 wt），提交链
        # base → fix-r3 → fix-r5 → fix-r2 → board 尾提交（真实形态，fix-r7：引用 #2691 fix-r2 /
        # #2692 fix-r8）→ 普通状态提交，不同 base..head 范围取不同标题集合后 check() 全链实测
        wt2 = tmp / "repo-title"
        wt2.mkdir()
        g2 = ["git", "-C", str(wt2), "-c", "user.email=t@t", "-c", "user.name=t"]
        subprocess.run([*g2, "init", "-q"], check=True)

        def titled2(msg: str) -> str:
            subprocess.run([*g2, "commit", "-q", "--allow-empty", "-m", msg], check=True)
            return subprocess.run([*g2, "rev-parse", "HEAD"], capture_output=True, text=True).stdout.strip()

        c_base = titled2("base")
        h_r3 = titled2("fix(scripts) PR #3047 fix-r3：r2 评审 P2 当轮修——casefold 只对别名表登记的两形生效")
        h_r5 = titled2("fix(scripts) #3074 fix-r5：修闸门")
        h_r2 = titled2("fix(scripts) #3074 fix-r2：修轮号取法")
        # fix-r7（DS r6 boundary#1）：尾提交换**真实 board 形态**（旧夹具是干净 `chore: sync
        # board`，恰好绕开真实形态）——board 同步尾标题引用 #2691 fix-r2 / #2692 fix-r8，
        # 首个=2、行内 max=8，正是 fix-r6 动机场景的常见形态
        h_sync = titled2("chore(state) board 10:1x–12:3x 增记：#2691 fix-r2 P1×3 不入队、#2692 fix-r8 在跑")
        h_plain2 = titled2("chore(state) 00:13 交接段：普通状态提交")
        blk_t = ("<!-- pr-gate:v1 roles=sibling:PASS,concurrency:PASS,boundary:PASS,simplify:PASS "
                 "head={h} runner=zai model=glm-5.3-mb readonly=tools -->\n")

        def gate2(body: str, h: str, b: str = "", repo: str = "") -> list[str]:
            return gate.check(body, h, "", b, repo or str(wt2))

        # 集合判据直接断言（任务书验 2）：多轮累积（fix-r3 + fix-r5 两提交在范围里）取 max=5；
        # head（board 形尾提交，引用轮 2/8）天然在集合内，不单独走一条路；轮号取**真实修复
        # 提交**的最大轮 5，不取 board 引用轮（fix-r7，DS r6 boundary#1；fix-r5 修 2 起按
        # issues 配对：本单 #3074 的 fix-r5 计入，board 引用 #2691/#2692 的轮按各单归属不计）
        titles, terr = gate.pr_commit_titles(str(wt2), c_base, h_sync)
        if terr or titles is None or gate.fix_round_from_titles(titles, frozenset({"3074"})) != 5:
            fails.append(f"fix-r6 集合判据：范围含 fix-r3+fix-r5 应取 max=5（terr={terr}，titles={titles}）")
        # 同一范围换本单视角（修 2）：issues={#3047} → 只剩 h_r3 的 fix-r3 计入 → 3——跨标题
        # max 按本单归属逐条筛，不是全范围一把 max
        if gate.fix_round_from_titles(titles, frozenset({"3047"})) != 3:
            fails.append("fix-r5 修2 配对：同一范围换 issues={#3047} 应只剩 fix-r3 → 3")
        # fix-r5 修 2 配对判定（boundary#2 P1，取代 fix-r7「每标题首个」与 fix-r6「行内 max」）：
        # 每条标题内 #N 与 fix-r<N> 就近配对，只计 N == 本 PR 单号的轮号；board 尾标题的引用
        # 轮按 issue 号归属各单（2 归 #2691、8 归 #2692），配不到本单 → 整条不判。低估方向
        # （本单轮号不在首位 / 范围只有 board 尾 → 旧「首个」取 2 跳过 N≥4 residual＝假 PASS）
        # 与高估方向（引用轮 8 抬过 N≥4 门槛＝假 FAIL）双向都钉：
        #   (c_base, h_r3)     范围 = [fix-r3 真实提交 #3047]          → {3047} → 3
        #   (h_r5, h_sync)    范围 = [fix-r2 真实提交, board 尾]       → {3074} → 2（不是 8）
        #   (h_r2, h_sync)    范围 = [board 尾提交]                     → {2691} → 2 / {2692} → 8（各归各单）
        #                                                      {3074} → None（引用轮不冒充本单轮）
        #   (h_r5, h_plain2)  范围 = [fix-r2 真实提交, board, 普通状态] → {3074} → 2
        #   (h_sync, h_plain2) 范围 = [chore(state) 交接段]             → None（非修复轮）
        for b, h, iv, want in [(c_base, h_r3, frozenset({"3047"}), 3),
                               (h_r5, h_sync, frozenset({"3074"}), 2),
                               (h_r2, h_sync, frozenset({"2691"}), 2),
                               (h_r2, h_sync, frozenset({"2692"}), 8),
                               (h_r2, h_sync, frozenset({"3074"}), None),
                               (h_r5, h_plain2, frozenset({"3074"}), 2),
                               (h_sync, h_plain2, frozenset({"3074"}), None)]:
            titles, terr = gate.pr_commit_titles(str(wt2), b, h)
            if terr or titles is None or gate.fix_round_from_titles(titles, iv) != want:
                fails.append(f"fix-r5 修2 配对：{b[:7]}..{h[:7]} + issues={sorted(iv)} 应判 {want}"
                             f"（terr={terr}，titles={titles}）")
        # 纯函数正负控（不依赖 git）：跨标题 max 保留 + board 引用轮按 issues 归属
        board_t = "chore(state) board 10:1x–12:3x 增记：#2691 fix-r2 P1×3 不入队、#2692 fix-r8 在跑"
        if gate.fix_round_from_titles([board_t], frozenset({"2691"})) != 2:
            fails.append("fix-r5 修2 配对：仅 board 尾标题 + issues={#2691} 应判 2（引用轮 2 归 #2691）")
        if gate.fix_round_from_titles([board_t], frozenset({"2692"})) != 8:
            fails.append("fix-r5 修2 配对：仅 board 尾标题 + issues={#2692} 应判 8（引用轮 8 归 #2692，不再被「首个」遮成 2）")
        if gate.fix_round_from_titles([board_t], frozenset({"999"})) is not None:
            fails.append("fix-r5 修2 负控：board 尾标题引用轮配不到本 PR 单不应判修复轮（旧「首个」会冒充 2）")
        if gate.fix_round_from_titles(["fix(scripts) #3074 fix-r2：修轮号取法", board_t], frozenset({"3074"})) != 2:
            fails.append("fix-r5 修2 配对：真实 fix-r2 提交 + board 尾标题 + issues={#3074} 应判 2")
        if gate.fix_round_from_titles(["fix(scripts) #1 fix-r3：a", "fix(scripts) #1 fix-r5：b", board_t], frozenset({"1"})) != 5:
            fails.append("fix-r5 修2 配对：跨标题 max 保留（fix-r3+fix-r5+board 引用轮 → 5 不是 8）")
        if gate.fix_round_from_titles([board_t, "fix(scripts) #1 fix-r21：c"], frozenset({"1"})) != 21:
            fails.append("fix-r5 修2 配对：跨标题 max 保留（board 引用轮 2 不遮真实 fix-r21）")

        # fix-r9（DS r5 boundary P0）多单关联列表：pr_issues 取续写列表的全部号——只取首个会让
        # 「标题引用 #3057 的修复轮」配不到本 PR 单号（fix_round=None → fixtype/residual 假 PASS）
        for body, want in [("Closes #3056、#3057\n", frozenset({"3056", "3057"})),
                           ("Refs #3056/#3057\n", frozenset({"3056", "3057"})),
                           ("Fixes #3056, #3057\n", frozenset({"3056", "3057"})),
                           ("closes #3056,\n#3057\n", frozenset({"3056", "3057"})),
                           ("Closes #3056、#3057：说明文字\n", frozenset({"3056", "3057"})),
                           ("Closes #3056 and #3058\n", frozenset({"3056"})),
                           ("boundary#1 不是关键词引用\n", frozenset())]:
            if gate.pr_issues(body) != want:
                fails.append(f"fix-r9 P0：pr_issues({body!r}) 应得 {sorted(want)}，"
                             f"得 {sorted(gate.pr_issues(body))}")
        if gate.fix_round_from_titles(["fix(scripts) #3057 fix-r5：x"],
                                      gate.pr_issues("Refs #3056、#3057\n")) != 5:
            fails.append("fix-r9 P0：续写列表第 2 个号（#3057）应进关联集合并配到标题引用（旧实现只取首个 → None 假 PASS）")
        # fix-r9（DS r5 boundary P1）HASH_REF 词界：boundary#N / C#N 里的 #N 不算就近配对 #N。
        # 旧 HASH_REF 无词界时 #2 会抢走 fix-r5 的就近配对（链 {2} ∩ {3074} = ∅ → 不计入 →
        # N≥4 residual 跳过＝假 PASS）；词界后整条只剩 #3074，正常配对计 5
        if gate.fix_round_from_title("fix(scripts) #3074 boundary#2 fix-r5：x", frozenset({"3074"})) != 5:
            fails.append("fix-r9 P1：boundary#2 的 #2 无词界不抢 #3074 的就近配对（旧实现判 None＝假 PASS）")
        if gate.fix_round_from_title("fix(scripts) #3074 C#1234 fix-r5：x", frozenset({"3074"})) != 5:
            fails.append("fix-r9 P1：C#1234 的 #1234 无词界不抢 #3074 的就近配对")
        # 尾部词界：#3056abc 不是 #3056（字母并进号的引用不冒充本单配对——若算 #3056，就近
        # 配对会被它抢走，链 {3056} ∩ {999} = ∅ → 不计 fix-r5）
        if gate.fix_round_from_title("fix(scripts) #999 #3056abc fix-r5：x", frozenset({"999"})) != 5:
            fails.append("fix-r9 P1：#3056abc 不是 #3056，就近配对仍是 #999（计 5）")
        # 前置数字保留（P1 修法的边界）：#3074#3057 紧贴链（空隙视为紧贴）仍并成一组
        if gate.fix_round_from_title("fix(scripts) #3074#3057 fix-r3：x", frozenset({"3057"})) != 3:
            fails.append("fix-r9 P1：前置数字保留，#3074#3057 紧贴链仍并成一组（#3057 配到本单 → 3）")
        # fix-r10（DS r9 boundary#1 P0 + sibling#1 P1 双撞）：续写分隔符补空白与 `+`（对齐
        # PAIR_SEP）——board 真实形态 `Closes #382 #387 #452`、`Refs #1985+#2053` 此前仍只取首号
        for body, want in [("Closes #382 #387 #452\n", frozenset({"382", "387", "452"})),
                           ("Refs #1985+#2053+#2056\n", frozenset({"1985", "2053", "2056"})),
                           ("Closes #440 #498\n", frozenset({"440", "498"})),
                           ("Closes #3056  #3057\n", frozenset({"3056", "3057"})),
                           ("Closes #3056 and #3058\n", frozenset({"3056"})),
                           ("Closes #3056、#3057 #3058/#3059+#3060\n",
                            frozenset({"3056", "3057", "3058", "3059", "3060"}))]:
            if gate.pr_issues(body) != want:
                fails.append(f"fix-r10 P0：pr_issues({body!r}) 应得 {sorted(want)}，"
                             f"得 {sorted(gate.pr_issues(body))}")
        if gate.fix_round_from_titles(["fix(scripts) #498 fix-r5：x"],
                                      gate.pr_issues("Closes #440 #498\n")) != 5:
            fails.append("fix-r10 P0：空格分隔的第 2 个号（#498）应进关联集合并配到标题引用（旧实现只取首个 → None 假 PASS）")
        # fix-r10（DS r9 boundary#2 P2）：「排除表收窄」方案被真实语料复算证伪（复活集全是
        # conc#1/bnd#1/r3b#1/boundary-rerun#1 伪 token，词表补不全见 memory「词汇枚举式修判读
        # 不收敛」；语料 10686 条 glued PR#N 真引用 0 实例）→ 按评审给的另一选项**接受窄窗**
        # 并钉死当前行为：字母紧贴真实引用不参与配对，整条无 #N 时走「全部轮号计入」兜底
        if gate.fix_round_from_title("chore(state) 交接段：#9999 fix-r2 复盘 PR#3074 fix-r8：x",
                                     frozenset({"3074"})) is not None:
            fails.append("fix-r10 P2：窄窗在档（PR#3074 被词界杀掉、#9999 不属本单 → None）；"
                         "复算依据 _tmp/20261113-3056-fixr10/wordlist-pairing-recompute.txt A 面")
        if gate.fix_round_from_title("fix(scripts) PR#3074 fix-r5：x", frozenset({"3074"})) != 5:
            fails.append("fix-r10 P2：整条无存活 #N（PR#3074 被杀）时全部轮号计入兜底（→ 5）")
        if gate.fix_round_from_title("fix(scripts) #3074 sibling#2 fix-r5：x", frozenset({"3074"})) != 5:
            fails.append("fix-r9 P1：sibling#2 伪 token 不抢 #3074 的就近配对")
        if gate.fix_round_from_title("fix(scripts) #3074 F#9 fix-r5：x", frozenset({"3074"})) != 5:
            fails.append("fix-r9 P1：F#9 语言前缀不抢 #3074 的就近配对")
        # fix-r11（DS r10 sibling#1 + boundary#1 + simplify#1 三 P2 同簇，结构性修）：续写分隔符
        # 从 PAIR_SEP 单源派生——PAIR_SEP 每个成员（含未来新增）单独作分隔符时两个号都必须
        # 进关联集；空档（粘写 `#3056#3057`）与 _paired_rounds「空隙视为紧贴」并组口径对齐；
        # 纯换行不是分隔符（下一行行首 #N 不并入）
        for sep in sorted(gate.PAIR_SEP):
            if gate.pr_issues(f"Closes #1234{sep}#5678\n") != frozenset({"1234", "5678"}):
                fails.append(f"fix-r11 P2：PAIR_SEP 成员 {sep!r} 作分隔符时第 2 个号未进关联集"
                             f"（单源派生被破坏——得 {sorted(gate.pr_issues(f'Closes #1234{sep}#5678'))}）")
        if gate.pr_issues("Refs #3056#3057\n") != frozenset({"3056", "3057"}):
            fails.append("fix-r11 P1（DS r10 boundary#1，P2-b 口径）：粘写形 `Refs #3056#3057` 只取首号"
                         " → 标题并组两号而关联集缺 3057 → fix_round=None 假 PASS（与 r9/r10 同一机制）")
        if gate.fix_round_from_titles(["fix(scripts) #3057 fix-r5：x"],
                                      gate.pr_issues("Refs #3056#3057\n")) != 5:
            fails.append("fix-r11 P1：粘写列表第 2 个号（#3057）应配到标题引用（旧实现只取首个 → None 假 PASS）")
        if gate.pr_issues("Closes #3056\n\n#3074 是另一单（不关）\n") != frozenset({"3056"}):
            fails.append("fix-r11 P2（DS r10 simplify#1）：纯换行不是分隔符，下一行行首 #N 不并入关联集")
        if gate.pr_issues("Closes #3056,\n#3057\n") != frozenset({"3056", "3057"}):
            fails.append("fix-r11 P2：显式分隔符两侧换行容忍保留（closes #3056,\\n#3057 既有用例）")
        # fix-r12（DS r11 boundary#1 P0 + sibling#1 + concurrency#1 + simplify#1#2 + boundary#2 P2 同簇
        # 收口）：①非 ASCII 空白（全角空格 U+3000 / NBSP U+00A0）续写两号都进关联集——
        # r11 的 `[ \t]+` 丢了 r10 `\s+` 本收的 Unicode 空白，中文输入法正文丢号 → 同一假 PASS 机制；
        # ②纯换行不并的语义保留（`[^\S\r\n]` 不含换行）；③标点分支收紧为至多一个换行——
        # 段落空行（两换行）不并入；④标点串 `[,+#]` 混排连续标点并组；⑤PAIR_SEP 字面量钉死
        # （防未来删成员静默缩窄）；⑥关键词同源断言随 fix-r13 删 ISSUE_LINK 退役（见 r13 段）。
        # fix-r17 簇①：成员表抽成模块级 WS_GAP_MEMBERS 单源（上方定义）——本循环与下方
        # r17「空白块×标点」组合正控表共用同一份，ASCII 空白块成员一并进本循环（语义上
        # 同属纯空白续写，本就应过），成员增删只改一处。
        for ws_ch, ws_name in WS_GAP_MEMBERS:
            if gate.pr_issues(f"Closes #1234{ws_ch}#5678\n") != frozenset({"1234", "5678"}):
                fails.append(f"fix-r12 P0（DS r11 boundary#1）：{ws_name} 续写第 2 个号未进关联集"
                             f"（得 {sorted(gate.pr_issues(f'Closes #1234{ws_ch}#5678'))}）"
                             " → 标题并组两号而关联集缺号 → fix_round=None → fixtype/residual 跳过＝假 PASS")
        if gate.pr_issues("Closes #1234\n#5678\n") != frozenset({"1234"}):
            fails.append("fix-r12 负控：纯换行不是分隔符（U+3000 修法的 `[^\\S\\r\\n]+` 不得把换行也吞进来）")
        if gate.pr_issues("Closes #3056,\n\n#3074 是另一单（不关）\n") != frozenset({"3056"}):
            fails.append("fix-r12 P2（DS r11 boundary#2）：标点 + 段落空行（两换行）不并入——"
                         "消除「同一条正文边界，无标点分支不并、标点分支并入」的两套口径")
        for punct_mix in [",+", "+/", "、，", ",,"]:
            if gate.pr_issues(f"Refs #3056{punct_mix}#3057\n") != frozenset({"3056", "3057"}):
                fails.append(f"fix-r12 P2（DS r11 simplify#2 + boundary#3）：连续/混排标点串"
                             f" {punct_mix!r} 应并组（与 _paired_rounds 的逐字符 PAIR_SEP 判据对齐），"
                             f"得 {sorted(gate.pr_issues(f'Refs #3056{punct_mix}#3057'))}")
        if gate.PAIR_SEP != frozenset("+,、，/ \t"):
            fails.append(f"fix-r12 P2（DS r11 concurrency#1）：PAIR_SEP 字面量钉死被破坏"
                         f"（现值 {sorted(gate.PAIR_SEP)!r}——成员变动须同步 r12 注释与本断言，"
                         "静默删成员会让上方成员循环测试静默缩窄）")
        # fix-r12 的「ISSUE_LINK 与 ISSUE_NUM 关键词同源」断言随 fix-r13 删除 ISSUE_LINK 一并
        # 退役（DS r12 simplify#1 P2：ISSUE_LINK 是 ISSUE_NUM 的同前缀特化，规则 2 改用
        # issues 布尔值；删除前等价性实测 issuelink-equivalence-probe.txt 差 0）。
        # fix-r13（DS r12 boundary#1 + sibling#1 双谱系同撞 P1，同根因）：①标题侧 `_paired_rounds`
        # 链 gap 与 ISSUE_NUM 空白类单源（`_is_issue_gap_char`）——U+3000/NBSP 在配对端同样
        # 并组（r12 只放宽正文端，配对端断链 → fix_round=None → fixtype/residual 跳过＝同一
        # 假 PASS 机制留在另一端）；②ISSUE_NUM 标点分支补左侧 ≤1 换行（`Closes #3056\n,#3057`
        # 并组——base 本收、r12 收紧时未登记的欠收）；③标点串允许空白穿插（`, +`）；④全码位
        # 单源锁：每个码位上「regex 并组」与「谓词」一致。
        for ws_ch, ws_name in [("　", "全角空格 U+3000"), (" ", "NBSP U+00A0")]:
            if gate.fix_round_from_titles([f"fix(scripts) #3056{ws_ch}#3074 fix-r12：x"],
                                          frozenset({"3056"})) != 12:
                fails.append(f"fix-r13 P1（DS r12 双撞）：{ws_name} 在标题侧配对端同样并组"
                             "——r12 修了正文端丢号，配对端断链会让 fix_round=None"
                             " → fixtype/residual 跳过＝同一假 PASS 机制留在另一端")
        if gate.pr_issues("Closes #3056\n,#3057\n") != frozenset({"3056", "3057"}):
            fails.append("fix-r13 P2（DS r12 boundary#2）：标点左侧至多一个换行"
                         "（`Closes #3056\\n,#3057` base 本收、r12 收紧时未登记的欠收）")
        if gate.pr_issues("Closes #3056, + #3057\n") != frozenset({"3056", "3057"}):
            fails.append("fix-r13 P2（DS r12 sibling#1 尾差）：标点串允许空白穿插"
                         "（`, +` 与标题侧逐字符 gap 判据对齐）")
        if gate.pr_issues("Closes #3056\n\n,#3074 x\n") != frozenset({"3056"}):
            fails.append("fix-r13 负控：段落空行（两换行）不并——左侧换行容忍同右侧"
                         "至多一个，不因 boundary#2 修复放宽到跨段落")
        single_src_bad = 0
        for cp in range(0x110000):
            if 0xD800 <= cp <= 0xDFFF:
                continue
            c = chr(cp)
            if (gate.pr_issues(f"Closes #1{c}#2\n") == frozenset({"1", "2"})) != gate._is_issue_gap_char(c):
                single_src_bad += 1
                if single_src_bad <= 3:
                    fails.append(f"fix-r13 单源锁：码位 U+{cp:04X} 上 ISSUE_NUM 并组与"
                                " _is_issue_gap_char 不一致（正文端与配对端词表分裂）")
        if single_src_bad > 3:
            fails.append(f"fix-r13 单源锁：共 {single_src_bad} 个码位不一致（正文端与配对端词表分裂）")
        # fix-r14（DS r13 concurrency#1 + boundary#1、MM concurrency#1 三谱系同撞 P0，仲裁实测
        # `_tmp/20261113-3056-fixr14/redos-arbitration.txt`）：标点分支嵌套量词 `(P+)+`
        # 的指数回溯——长标点串（其后再无 #N）须在时限内返回，r13 形态实测 n=22 已 0.32s、
        # 每 +2 标点 ×4.3（n=40 外推 20+ 分钟＝闸门挂起，正文作者可控）。
        # fix-r16（DS r15 boundary#1 + MM sibling#1/concurrency#1/boundary#1 四撞同源 P1）：
        # r14 三攻击形不再就地调用——就地断言排在子进程探针之前，回退指数形时套件在探针
        # 前就地挂死，「回退态＝干净 FAIL」对指数类不成立；三形全部并入下方探针
        # （punct / punct_mix / punct_nl），语义（只取首号且并组不变）与时限断言同在子进程内。
        # fix-r15（DS r14 sibling#1 P0 + boundary#1 P1，仲裁实测 arbitrate-r14-ds.txt；护栏
        # fail-fast 缺陷＝MM r14 concurrency#2 + boundary#2 同撞）：两个二次回溯面——
        # `_paired_rounds` 的 #N 链构建 O(k²)（实测 k=32000 达 103.9 s）、前导空白×标点
        # 相邻星号 O(K·M)（实测 n=4000 已 0.88 s）——各补一条探针；**全部探针跑在带超时的
        # 子进程里**：回退成二次/指数形态时子进程被杀＝干净 FAIL，不再像旧的事后计时那样
        # 在永不返回的调用上把整套套件挂死。
        _probe = (
            "import importlib.util, sys\n"
            # fix-r17 簇④（MM r15 concurrency#1）：父进程被按 PID SIGTERM/SIGKILL 时，探针子进程
            # 会成为孤儿继续空转（回退态的指数回溯＝100% CPU 持续到热寂）。CPU 秒数硬限 4 s：
            # 到点内核发 SIGXCPU（默认动作终止），孤儿至多再烧 4 s CPU。**不用 signal.alarm/
            # SIGALRM**——信号处理要经解释器投递，能否打断长 re 匹配依版本而定（本机 3.14
            # 实测 re 引擎循环里会查 pending 信号、孤儿 3.9 s 被杀，见
            # _tmp/20261113-3056-fixr17/orphan-rlimit-demo.txt，但这是实现细节不是承诺）；
            # RLIMIT_CPU 由内核记账，不依赖解释器的任何投递行为，孤儿逃不掉。父侧 5 s 墙钟
            # 超时保留不动（分工：墙钟超时负责「本套件内干净 FAIL」，CPU 硬限负责「父死后
            # 不空转」）。
            "import resource\n"
            "resource.setrlimit(resource.RLIMIT_CPU, (4, 4))\n"
            # fix-r20（MM r19 concurrency#2 P2）：SIGXCPU 的默认动作含写 core——探针死在
            # cwd（仓库根）时每次回退红都会留一个未跟踪 core 文件（CPython + 65514 字符
            # 串，几百 MB 量级）污染 git status / 吃 CI 磁盘。RLIMIT_CORE 归零：内核直接
            # 丢 core 不落盘（父侧失败文案由 _signame 翻信号名，不再只看裸 -24）。
            "resource.setrlimit(resource.RLIMIT_CORE, (0, 0))\n"
            # fix-r19（#3074 家族级）：探针加挂兄弟脚本 tb（scope/personday 模式用），并把
            # r18 的 ECHO_HEAD 就地计时 + WAIVER/NO_ISSUE/SCOPE_BODY/PERSON_DAY 失败形计时
            # 全部并入本护栏（fix-r16 教训 :1035-1038——回退态＝子进程被杀＝干净 FAIL）。
            # 子进程自带计时打印 `ELAPSED <dt> <语义结果>`，父侧断言 <2.0 s + 失败形不命中。
            "import os, time\n"
            "spec = importlib.util.spec_from_file_location('gate', sys.argv[1])\n"
            "gate = importlib.util.module_from_spec(spec)\n"
            "spec.loader.exec_module(gate)\n"
            "spec2 = importlib.util.spec_from_file_location('tb', os.path.join(os.path.dirname(sys.argv[1]), 'check-taskbook-segments.py'))\n"
            "tb = importlib.util.module_from_spec(spec2)\n"
            "spec2.loader.exec_module(tb)\n"
            "mode, n = sys.argv[2], int(sys.argv[3])\n"
            "if mode == 'chain':\n"
            "    print(gate.fix_round_from_title('fix(scripts) #1' + '#1'*n + ' fix-r4'*n,\n"
            "                                    frozenset({'1'})))\n"
            "elif mode == 'ws':\n"
            "    print(sorted(gate.pr_issues('Refs #1' + ' '*n + ','*n + 'x')))\n"
            "elif mode == 'punct':\n"
            "    print(sorted(gate.pr_issues('Closes #3056' + ','*n + 'x')))\n"
            "elif mode == 'punct_mix':\n"
            "    print(sorted(gate.pr_issues('Refs #3056 ' + '+ ,'*n + '、'*n + 'x')))\n"
            "elif mode == 'punct_nl':\n"
            "    print(sorted(gate.pr_issues('Closes #3056' + (',' + chr(10))*n + 'x')))\n"
            "elif mode == 'echo':\n"
            "    t0 = time.perf_counter(); r = gate.ECHO_HEAD.match('fix(scripts)' + ' '*n + 'fix-r5：x')\n"
            "    print('ELAPSED %.4f %s' % (time.perf_counter() - t0, 'None' if r is None else 'HIT'))\n"
            "elif mode == 'waiver':\n"
            "    t0 = time.perf_counter(); r = gate.WAIVER.search('<!--pr-gate:waiver ' + ' '*n + 'x')\n"
            "    print('ELAPSED %.4f %s' % (time.perf_counter() - t0, 'None' if r is None else 'HIT'))\n"
            "elif mode == 'noissue':\n"
            "    t0 = time.perf_counter(); r = gate.NO_ISSUE.search('<!--no-issue x' + ' '*n)\n"
            "    print('ELAPSED %.4f %s' % (time.perf_counter() - t0, 'None' if r is None else 'HIT'))\n"
            "elif mode == 'scope':\n"
            "    t0 = time.perf_counter(); r = tb.SCOPE_BODY.match('pdrs-scope' + ' '*n + 'a'*8 + chr(10) + 'x')\n"
            "    print('ELAPSED %.4f %s' % (time.perf_counter() - t0, 'None' if r is None else 'HIT'))\n"
            "elif mode == 'personday':\n"
            "    t0 = time.perf_counter(); r = tb.PERSON_DAY.search('合计' + ' '*n + 'x')\n"
            "    print('ELAPSED %.4f %s' % (time.perf_counter() - t0, 'None' if r is None else 'HIT'))\n"
            # fix-r20（#3074 P1-B，DS r19 boundary#1）：多前缀失败形走 check() 端到端（修法
            # 在调用点预扫 `-->`，不是改正则——直调 WAIVER.search 量不到修的面）。语义结果
            # ＝错误条数（≥1＝fail-closed 拒收，不得静默放行）；n//21、n//15＝按两种前缀
            # 长度缩放使总长 ≈ n。修前实测 m=8000：waiver 3.04 s / noissue 2.23 s（Θ(M·N)）。
            "elif mode == 'waiver_multi':\n"
            "    t0 = time.perf_counter(); es = gate.check('<!--pr-gate:waiver x'*(n//21) + '\\nCloses #3056\\n', '', '', '', '')\n"
            "    print('ELAPSED %.4f %d' % (time.perf_counter() - t0, len(es)))\n"
            "elif mode == 'noissue_multi':\n"
            "    t0 = time.perf_counter(); es = gate.check('<!--no-issue x'*(n//15) + '\\n', '', '', '', '')\n"
            "    print('ELAPSED %.4f %d' % (time.perf_counter() - t0, len(es)))\n"
            # fix-r21（#3074 簇①②，DS/MM r20 双谱系）：`-->` 前置形（簇①：全文有 `-->`
            # 但都在候选起点之前——r20 全局预扫绕过形）与 MARKER 多前缀（簇②：finditer
            # 同源漏改点，r20 预扫根本没盖到）。修法在调用点转有界 span 线性扫描，
            # 直调 .search/.finditer 量不到修的面，走 check() 端到端；语义结果＝错误
            # 条数（≥1＝fail-closed 拒收）。修前实测（协调者）：waiver `-->` 前置
            # m=8000＝3.03 s / marker m=8000＝1.73 s（Θ(M·N)）。
            "elif mode == 'marker_multi':\n"
            "    t0 = time.perf_counter(); es = gate.check('<!--pr-gate:v1'*(n//15) + '\\n', '', '', '', '')\n"
            "    print('ELAPSED %.4f %d' % (time.perf_counter() - t0, len(es)))\n"
            "elif mode == 'waiver_multi2':\n"
            "    t0 = time.perf_counter(); es = gate.check('-->\\n' + '<!--pr-gate:waiver x'*(n//21), '', '', '', '')\n"
            "    print('ELAPSED %.4f %d' % (time.perf_counter() - t0, len(es)))\n"
            "elif mode == 'noissue_multi2':\n"
            "    t0 = time.perf_counter(); es = gate.check('-->\\n' + '<!--no-issue x'*(n//15), '', '', '', '')\n"
            "    print('ELAPSED %.4f %d' % (time.perf_counter() - t0, len(es)))\n"
        )
        _gate_py = str(ROOT / "scripts" / "check-pr-gate.py")
        for _mode, _n, _want, _why in (
                ("punct", 200, "['3056']", "嵌套量词 `(P+)+` 形回退会指数化"),
                ("punct_mix", 200, "['3056']", "标点间交错空白（新轮形 W* 槽）回退会指数化"),
                ("punct_nl", 200, "['3056']", "标点后接换行（_GAP_EDGE 的 \\r?\\n 边）回退会指数化"),
                ("ws", 20000, "['1']", "相邻星号（前导空白×标点）二次回溯 O(K·M)"),
                ("chain", 20000, "4", "_paired_rounds #N 链构建 O(k²) 与前方最近 #N 线性全扫"
                 " O(k·m)（fix-r16：token 与 hash 同量级，钉死二分半边）")):
            try:
                _cp = subprocess.run([sys.executable, "-I", "-c", _probe, _gate_py, _mode, str(_n)],
                                     capture_output=True, text=True, timeout=5)
            except subprocess.TimeoutExpired:
                fails.append(f"fix-r15 P0/P1 回归护栏：{_mode} 探针超时（>5s）——{_why}，"
                             "闸门会挂起（作者可控标题/正文），修法见 check-pr-gate.py fix-r15 注释")
                continue
            if _cp.returncode != 0:
                fails.append(f"fix-r15 探针异常退出 {_signame(_cp.returncode)}: {_cp.stderr[-300:]}")
            elif _cp.stdout.strip() != _want:
                fails.append(f"fix-r15 {_mode} 语义漂移：得 {_cp.stdout.strip()!r} 期望 {_want!r}")
        # fix-r19（#3074 家族级，P0/P1 计时护栏）：r18 的 ECHO_HEAD 就地计时钉与本轮的
        # WAIVER/NO_ISSUE/SCOPE_BODY/PERSON_DAY 失败形计时全部并入 _probe 子进程护栏
        # （fix-r16 教训 :1035-1038——回退态＝子进程被 RLIMIT_CPU/墙钟超时杀掉＝干净 FAIL，
        # 不再把套件挂死在就地断言上）。子进程自带计时打印 `ELAPSED <dt> <语义结果>`，
        # 父侧断言耗时 <2.0 s（预注册 10 s 阈值的 5× 余量，r18 口径，防 CI 机器慢假红）
        # ＋失败形不得命中（改写不得引入新匹配面）。
        for _mode, _subj, _why in (
                ("echo", "ECHO_HEAD", "r18 修的相邻星号二次回溯（r17 实测修前 35.172 s）回归"),
                ("waiver", "WAIVER", "P0 三次方回溯（协调者实测修前 N=8000→171 s）回归"),
                ("noissue", "NO_ISSUE", "P1 二次回溯（实测修前 N=65514→4.2 s）回归"),
                ("scope", "SCOPE_BODY", "兄弟脚本冒号槽 + rest 残余（\\s*×.*）二次回溯回归"),
                ("personday", "PERSON_DAY", "兄弟脚本人日槽相邻星号二次回溯回归")):
            try:
                _cp = subprocess.run([sys.executable, "-I", "-c", _probe, _gate_py, _mode, "65514"],
                                     capture_output=True, text=True, timeout=5)
            except subprocess.TimeoutExpired:
                fails.append(f"fix-r19 家族计时护栏：{_subj} 探针超时（>5s）——{_why}，"
                             "闸门会挂起（作者可控正文/任务书）")
                continue
            if _cp.returncode != 0:
                fails.append(f"fix-r19 探针异常退出 {_signame(_cp.returncode)}: {_cp.stderr[-300:]}")
                continue
            _parts = _cp.stdout.strip().split()
            if len(_parts) != 3 or _parts[0] != "ELAPSED" or _parts[2] != "None":
                fails.append(f"fix-r19 {_subj} 探针输出不解析或失败形命中：{_cp.stdout.strip()!r}")
                continue
            try:
                _el = float(_parts[1])
            except ValueError:
                fails.append(f"fix-r19 {_subj} 探针计时不解析：{_cp.stdout.strip()!r}")
                continue
            if _el >= 2.0:
                fails.append(f"fix-r19 {_subj} 性能钉：N=65514 失败形 {_el:.3f}s ≥ 2.0s——"
                             f"{_why}（改动须过 P0/P1 通道重新登记）")
        # fix-r20（#3074 P1-B，DS r19 boundary#1）：多前缀失败形计时钉——r19 的五模式只
        # 覆盖单前缀，多前缀（M 个 `<!--` 前缀、全文无 `-->`）每个起点各把惰性组扩到
        # 串尾＝Θ(M·N)（修前实测 m=8000：waiver 3.04 s / noissue 2.23 s）。修法在调用点
        # 预扫（check() 里 `-->` in masked），故探针走 check() 端到端；同口径断言
        # N=65514 <2.0 s ＋ fail-closed 拒收（错误条数 ≥1，不得静默放行）。
        for _mode, _why in (
                ("waiver_multi", "WAIVER 多前缀（M 个 `<!--pr-gate:waiver x`、无 `-->`）Θ(M·N)"),
                ("noissue_multi", "NO_ISSUE 多前缀（M 个 `<!--no-issue x`、无 `-->`）Θ(M·N)")):
            try:
                _cp = subprocess.run([sys.executable, "-I", "-c", _probe, _gate_py, _mode, "65514"],
                                     capture_output=True, text=True, timeout=5)
            except subprocess.TimeoutExpired:
                fails.append(f"fix-r20 多前缀计时护栏：{_mode} 探针超时（>5s）——{_why}，"
                             "闸门会挂起（作者可控正文），修法见 check-pr-gate.py fix-r20 预扫")
                continue
            if _cp.returncode != 0:
                fails.append(f"fix-r20 探针异常退出 {_signame(_cp.returncode)}: {_cp.stderr[-300:]}")
                continue
            _parts = _cp.stdout.strip().split()
            if len(_parts) != 3 or _parts[0] != "ELAPSED" or not _parts[2].isdigit():
                fails.append(f"fix-r20 {_mode} 探针输出不解析：{_cp.stdout.strip()!r}")
                continue
            if int(_parts[2]) < 1:
                fails.append(f"fix-r20 {_mode} 语义钉：无 `-->` 的多前缀正文须 fail-closed 拒收"
                             f"（不得静默放行）：{_cp.stdout.strip()!r}")
                continue
            try:
                _el = float(_parts[1])
            except ValueError:
                fails.append(f"fix-r20 {_mode} 探针计时不解析：{_cp.stdout.strip()!r}")
                continue
            if _el >= 2.0:
                fails.append(f"fix-r20 {_mode} 性能钉：N=65514 多前缀 {_el:.3f}s ≥ 2.0s——"
                             f"{_why}（改动须过 P0/P1 通道重新登记）")
        # fix-r21（#3074 簇①②，DS/MM r20 双谱系）：`-->` 前置多前缀与 MARKER 多前缀
        # 计时钉——修前实测（协调者）：waiver `-->` 前置 m=8000＝3.03 s / marker
        # m=8000＝1.73 s（Θ(M·N)，`--body-file` 无长度上限＝准挂起面；r20 的全局预扫
        # 只挡「全文无 `-->`」，`-->` 在候选前缀之前即绕过，MARKER 调用点则无任何
        # 预扫）。修法＝有界 span 线性扫描（_marker_search/_marker_finditer），须
        # N=65514 <2.0 s（同 r18 口径 5× 余量）＋ fail-closed 拒收（错误条数 ≥1）。
        for _mode, _why in (
                ("marker_multi", "MARKER 多前缀（M 个 `<!--pr-gate:v1`、无 `-->`）Θ(M·N)〔簇②：r20 预扫的同源漏改点〕"),
                ("waiver_multi2", "WAIVER 多前缀 `-->` 前置形（`-->` 在全部候选起点之前，r20 全局预扫绕过）Θ(M·N)〔簇①〕"),
                ("noissue_multi2", "NO_ISSUE 多前缀 `-->` 前置形（同簇①）Θ(M·N)")):
            try:
                _cp = subprocess.run([sys.executable, "-I", "-c", _probe, _gate_py, _mode, "65514"],
                                     capture_output=True, text=True, timeout=5)
            except subprocess.TimeoutExpired:
                fails.append(f"fix-r21 簇①②计时护栏：{_mode} 探针超时（>5s）——{_why}，"
                             "闸门会挂起（作者可控正文），修法见 check-pr-gate.py fix-r21 span 扫描")
                continue
            if _cp.returncode != 0:
                fails.append(f"fix-r21 探针异常退出 {_signame(_cp.returncode)}: {_cp.stderr[-300:]}")
                continue
            _parts = _cp.stdout.strip().split()
            if len(_parts) != 3 or _parts[0] != "ELAPSED" or not _parts[2].isdigit():
                fails.append(f"fix-r21 {_mode} 探针输出不解析：{_cp.stdout.strip()!r}")
                continue
            if int(_parts[2]) < 1:
                fails.append(f"fix-r21 {_mode} 语义钉：多前缀正文须 fail-closed 拒收"
                             f"（不得静默放行）：{_cp.stdout.strip()!r}")
                continue
            try:
                _el = float(_parts[1])
            except ValueError:
                fails.append(f"fix-r21 {_mode} 探针计时不解析：{_cp.stdout.strip()!r}")
                continue
            if _el >= 2.0:
                fails.append(f"fix-r21 {_mode} 性能钉：N=65514 {_el:.3f}s ≥ 2.0s——"
                             f"{_why}（改动须过 P0/P1 通道重新登记）")
        # fix-r14（DS r13 sibling#1 P2）：类型词表单源——ECHO_HEAD 与 TITLE_HEAD 只准引
        # `_TYPE_WORDS`，一处加词另一处漏改会让 CI 路径与本地回显路径的修复轮判定分裂
        # （r12 ISSUE_LINK/ISSUE_NUM 同款手抄漂移反模式）。
        if gate._TYPE_WORDS not in gate.ECHO_HEAD.pattern or gate._TYPE_WORDS not in gate.TITLE_HEAD.pattern:
            fails.append("fix-r14 P2（DS r13 sibling#1）：ECHO_HEAD/TITLE_HEAD 须单源引 _TYPE_WORDS"
                         "（两份手抄词表漂移 → 双路径修复轮判定分裂）")
        # fix-r15（DS r14 sibling#2 P2 + MM boundary#3 P3 同撞）：子串包含锁抓不到「手抄同一
        # 字面量 / 附加别的分支」——行为对拍补位：逐词（词表由 _TYPE_WORDS 自身解析，不手抄
        # 第二份）断言 ECHO_HEAD/TITLE_HEAD 命中全部词、表外词（deps）两者均不命中。
        import re as _re
        for _w in _re.findall(r"[a-z]+", gate._TYPE_WORDS):
            if not gate.ECHO_HEAD.match(f"{_w}(s) #1 fix-r2：x"):
                fails.append(f"fix-r15 类型词正控：ECHO_HEAD 须命中 {_w}(s) 回显形态（词表漂移）")
            if not gate.TITLE_HEAD.match(f"{_w}(scripts): x"):
                fails.append(f"fix-r15 类型词正控：TITLE_HEAD 须命中 {_w}(scripts) 标题形态（词表漂移）")
        if gate.ECHO_HEAD.match("deps(s) #1 fix-r2：x") or gate.TITLE_HEAD.match("deps(scripts): x"):
            fails.append("fix-r15 类型词负控：deps 不在 _TYPE_WORDS，两正则不得命中"
                         "（命中＝词表漂进了别的来源）")
        # fix-r16（DS r15 sibling#3 P2a）：词表由 _TYPE_WORDS 自身解析会静默丢词（成员循环
        # 缩窄反模式，同文件 PAIR_SEP 已用字面量钉死）——删 revert 后逐词正控仍绿、Revert
        # 型修复轮标题判不出 → fix_round=None → 假 PASS。_ISSUE_KEYWORDS 同理（删词 →
        # pr_issues 漏号 → 同一假 PASS 面）。
        if gate._TYPE_WORDS != r"(?:fix|feat|docs|chore|test|style|refactor|perf|build|ci|revert)":
            fails.append("fix-r16 P2：_TYPE_WORDS 字面量钉死被破坏（现值 "
                         f"{gate._TYPE_WORDS!r}——成员变动须同步本断言，静默删成员会让"
                         "上方逐词正控静默缩窄）")
        if gate._ISSUE_KEYWORDS != r"(?:close[sd]?|fix(?:e[sd])?|resolve[sd]?|refs)":
            fails.append("fix-r16 P2：_ISSUE_KEYWORDS 字面量钉死被破坏（现值 "
                         f"{gate._ISSUE_KEYWORDS!r}——成员变动须同步本断言与 fix-r17 簇②"
                         "包含锁/行为正控，静默删词会让 pr_issues 漏号＝假 PASS）")
        # fix-r17 簇②（DS r15 sibling#2 + simplify#1 两撞）：关键词**包含锁**——字面量钉死只保证
        # `_ISSUE_KEYWORDS` 这份字符串本身，不保证 ISSUE_NUM 真引了它（定义处与正则处是两处文本，
        # 手抄第二份词表会静默漂移——与上方 _TYPE_WORDS in ECHO_HEAD/TITLE_HEAD.pattern 同款）。
        if gate._ISSUE_KEYWORDS not in gate.ISSUE_NUM.pattern:
            fails.append("fix-r17 簇②：ISSUE_NUM 须单源引 _ISSUE_KEYWORDS（词表定义处改词、"
                         "正则处漏改 → pr_issues 漏号/错号 → 修复轮配对判定分裂＝假 PASS 面）")
        # fix-r17 簇②：关键词**行为正控**——包含锁/字面量钉死都只锁字符串，不锁「正则真匹配得上」
        # （前缀拼接处边界被改坏时上述两锁仍绿）。四个非 Refs 变体逐个实测（Closes/Fixes/Refs
        # 形已散见上方用例，四个屈折变体此前零覆盖）。
        for _kw in ("Resolves", "Resolved", "Closed", "Fixed"):
            if gate.pr_issues(f"{_kw} #3056\n") != frozenset({"3056"}):
                fails.append(f"fix-r17 簇②：关键词 {_kw} 行为正控——`{_kw} #3056` 应得 "
                             f"{{'3056'}}，得 {sorted(gate.pr_issues(f'{_kw} #3056'))}"
                             "（ISSUE_NUM 关键词分支匹配被改坏 → 关联合并丢 → 假 FAIL 面）")
        # fix-r16（MM r15 boundary#2 P2a）：「首个标点前的空白只由 _GAP_EDGE 吃」无正控——
        # _GAP_EDGE 前导 [^\S\r\n]* 被改掉时 `Refs #3056 ,#3074` 的第二个号被静默丢弃
        # → pr_issues 少号 → 标题侧 #N 配不上 → fix_round=None → 假 PASS 面。码位单源锁
        # 只覆盖单字符（W 或 P 单独），不覆盖「空白块+标点」组合。实测期望
        # {"3056","3074"}（_tmp/20261113-3056-fixr16/pre-measure.txt）。
        # fix-r17 簇①（DS sibling#1/DS boundary#1/MM sibling#1/MM boundary#1/MM boundary#2
        # 五撞）：r16 只手抄了单个 ASCII 空格——_GAP_EDGE 前导 [^\S\r\n]* 收窄成 [ ]* 时
        # 该断言仍绿（ASCII 空格活过 [ ]*）＝假 PASS 面未被覆盖。扩成小表：WS_GAP_MEMBERS ×
        # 标点 {",", "，"}，成员与 r12 续写循环单源（见模块级 WS_GAP_MEMBERS 定义）。
        # 分工：r13 全码位单源锁＝**单字符纯空白**路径（W 或 P 单独出现）；本表＝**空白块×
        # 标点组合**路径（W+ 紧跟 P——两个量词类的交界正是 _GAP_EDGE 的专属覆盖面）。
        for ws_ch, ws_name in WS_GAP_MEMBERS:
            for _punct in (",", "，"):
                if gate.pr_issues(f"Refs #3056{ws_ch}{_punct}#3074\n") != frozenset({"3056", "3074"}):
                    fails.append(f"fix-r17 簇①：{ws_name} + 标点 {_punct!r} 组合下第 2 个号未进关联集"
                                 f"（得 {sorted(gate.pr_issues(f'Refs #3056{ws_ch}{_punct}#3074'))}）——"
                                 "_GAP_EDGE 前导空白类收窄时该成员的号静默丢失＝假 PASS 面")
        # fix-r17 簇① CRLF 边（MM boundary#2）：_GAP_EDGE 的 \r?\n 换行边——CRLF 行界 + 标点
        # 并组（r13 只钉了 LF 形 `Closes #3056\n,#3057`，\r 分支零覆盖）。
        if gate.pr_issues("Closes #3056\r\n,#3074\n") != frozenset({"3056", "3074"}):
            fails.append("fix-r17 簇① CRLF：_GAP_EDGE 的 \\r?\\n 边——'Closes #3056\\r\\n,#3074' "
                         f"应得 {{'3056','3074'}}，得 {sorted(gate.pr_issues('Closes #3056\r\n,#3074'))}")
        # fix-r14（MM sibling#4 P2）：标题侧并组口径双向钉死——正控（PAIR_SEP 成员并组）+
        # 负控（非 gap 分隔符不并组、链只取最近 #N）。
        if gate.fix_round_from_titles(["fix(scripts) #3056;#3074 fix-r12：x"],
                                      frozenset({"3056"})) is not None:
            fails.append("fix-r14 P2 负控（MM sibling#4）：`;` 非 gap 字符不得并组——"
                         "断链后最近 #N=#3074 ∉ issues ⇒ 不计入（fix_round=None）；"
                         "若被放宽成链 {3056,3074} ∩ issues≠∅ 会把引用单的轮号误计入（假 PASS 向）")
        if gate.fix_round_from_titles(["fix(scripts) #3056，#3074 fix-r12：x"],
                                      frozenset({"3056"})) != 12:
            fails.append("fix-r14 P2 正控（MM sibling#4）：PAIR_SEP 成员（，）在标题侧仍并组"
                         "——负控收紧不得收掉既有并组口径")
        # fix-r9 P2a 接线：base==head 的空标题集合＝无主信号，正文兜底开结构化回显扫描
        # （旧 echo_scan=titles is None 把空集误当 CI 形态只看首行 → 非首行回显漏判＝假 PASS）
        empty_body = ("![ci](https://example.com/b.svg)\n\n概述。\n\n"
                      "fix(scripts): #3074 fix-r5：修闸门\n\n" + blk_t.format(h=h_plain2) + "Refs #3074\n")
        errs = gate2(empty_body, h_plain2, h_plain2)
        if not any("fix-r5" in e and "fixtype" in e for e in errs):
            fails.append(f"fix-r9 P2a：base==head 空集合应走回显兜底判修复轮 5（缺 fixtype 拒收）：{errs}")

        # fix-r5 双撞 P0 的最小复现转抓（任务书验 5，DS boundary#1 静态场景；fix-r7 夹具换真实
        # board 形态）：head=board 尾提交（引用 #2691 fix-r2/#2692 fix-r8）、正文标准 `## 概述`、
        # 更早提交带 fix-r5 → 集合判修复轮 5（真实修复提交的最大轮，非引用轮）→ 缺 fixtype 拒收。
        # Refs 关联单换 #3074（修 2）：h_r5 提交标题的 #3074 配到本单才计入
        p0_body = ("## 概述\n\nhead 提交标题不带 fix-rN 的多提交修复轮不再漏判。\n\n"
                   + blk_t.format(h=h_sync) + "Refs #3074\n")
        errs = gate2(p0_body, h_sync, c_base)
        if not any("fix-r5" in e and "fixtype" in e for e in errs):
            fails.append(f"fix-r6 P0 转抓：head=chore:sync board + 更早提交 fix-r5 应按修复轮 5 拒收缺 fixtype：{errs}")
        if not any("pdrs-residual" in e for e in errs):
            fails.append(f"fix-r6 P0 转抓：fix-r5（N≥4）应同时要求 pdrs-residual：{errs}")
        # 同一范围齐字段（fixtype + residual）应过闸；fix-r2 起「范围含别的单的修复提交」须
        # 在正文并写关联（Refs #3047）——不写会触发异单旁路 fail-closed（见下方 fix-r2 段）
        ok_body = (p0_body.replace("readonly=tools", "readonly=tools fixtype=结构性")
                   + "<!-- pdrs-residual: 上轮洞是 head 单标题信号面，集合语义收口 -->\nRefs #3047\n")
        errs = gate2(ok_body, h_sync, c_base)
        if errs:
            fails.append(f"fix-r6 正控：集合判修复轮 + 齐字段应过闸：{errs}")
        # badge 首行（修 9 场景迁移）：集合路径不依赖正文首行形态
        errs = gate2("![ci](https://example.com/b.svg)\n\n" + ok_body, h_sync, c_base)
        if errs:
            fails.append(f"fix-r6 正控：badge 首行 + 集合判修复轮 + 齐字段应过闸：{errs}")
        # 多轮累积端到端（任务书验 2）：范围 = [fix-r3, fix-r5]，正文无 fix-rN → 轮号 5 不是 3
        # （取错成 fix-r3 会免 residual 且报错写旧轮号）；Refs #3074（修 2）→ 只有 #3074 的
        # fix-r5 计入（#3047 的 fix-r3 属别的单），5 不依赖「全范围一把 max」
        acc_body = "## 概述\n\n多提交修复轮。\n\n" + blk_t.format(h=h_r5) + "Refs #3074\n"
        errs = gate2(acc_body, h_r5, c_base)
        if not any("fix-r5" in e and "fixtype" in e for e in errs) or not any("pdrs-residual" in e for e in errs):
            fails.append(f"fix-r6 多轮累积：fix-r3+fix-r5 在范围里应取 max=5（缺 fixtype/residual 拒收）：{errs}")
        # 集合赢过正文首行旧回显（主信号优先，正文兜底不覆盖集合判定）；Refs #3047（修 2）→
        # h_r3 标题的 #3047 配到本单，fix-r3 计入；若配错（如沿用 Refs #1）集合全不命中，会
        # 掉回正文首行 fix-r5——主信号优先就测不到了
        errs = gate2("fix(gateway) #1 fix-r5：正文首行是旧回显\n\n" + blk_t.format(h=h_r3) + "Refs #3047\n", h_r3, c_base)
        if not any("fix-r3" in e for e in errs):
            fails.append(f"fix-r6 主信号优先：集合判 fix-r3 不得被正文首行 fix-r5 旧回显覆盖：{errs}")
        # 集合路径负控（任务书验 3）：范围标题全不带 fix-rN（fix-r7 起基点改 board 尾提交之后
        # ——board 尾标题引用轮号后，含它的范围不再是「全不带 fix-rN」；head=普通状态提交、
        # 范围 = [chore(state) 交接段]），正文散文提及 fix-r3 → 判非修复轮
        neg_body = ("## 概述\n\nchore 更新 board：#1357 依赖普查完成，顺带记录了 fix-r3 的现场\n\n"
                    + blk_t.format(h=h_plain2) + "Refs #1\n")
        errs = gate2(neg_body, h_plain2, h_sync)
        if any("fixtype" in e or "residual" in e for e in errs):
            fails.append(f"fix-r6 负控：范围标题全无 fix-rN + 正文非首行散文提及 fix-r3 不应判修复轮：{errs}")
        # 正文首行兜底不回退（任务书验 3 后半，fix-r5 现状语义保留）：范围全不命中时首行宽松口径
        # 照旧——首行 `test: #123 fix-r5 的场景已补` 判修复轮 5（fix-r4 修 3.5 已知窄代价，钉住）
        errs = gate2("test: #123 fix-r5 的场景已补\n\n" + blk_t.format(h=h_plain2) + "Refs #1\n", h_plain2, h_sync)
        if not any("fix-r5" in e and "fixtype" in e for e in errs):
            fails.append(f"fix-r6 兜底不回退：范围全不命中 + 正文首行 fix-r5 应按修复轮 5 要求 fixtype：{errs}")

        # —— #3056 fix-r2 P0（DS r2 boundary#1 + MM r2 boundary 同撞，取高）：「配对集为空 /
        # 与 issues 不相交」两条旁路 fail-closed 封堵 ——
        # 形态 (a) 正文写合法旁路 <!-- no-issue 理由 --> → issues=∅；形态 (b) 正文关联另一单
        # （Closes #3042）而标题为 fix(gateway) #3999 fix-r5：…。两形态下旧判据把标题轮号全部
        # 丢弃 → fix_round=None → fixtype/多块一致/N≥4 residual 整段跳过，规则 2 又放行
        # （no-issue 是明文出口）＝修复轮机制被假 PASS。gate 对这两个形状必须红。
        # 旁路夹具走独立分支（不叠在主线 HEAD 上，后续 h_empty 等范围测试不受它影响）
        subprocess.run([*g2, "checkout", "-q", "-b", "fixr2-bypass"], check=True)
        h_bypass = titled2("fix(gateway) #3999 fix-r5：停泊补 waker")
        subprocess.run([*g2, "checkout", "-q", "-"], check=True)
        bypass_blk = blk_t.format(h=h_bypass)
        # 形态 (b) 负控：异单关联——标题 #3999 ∉ 正文关联 {3042}，必须红且写明「异单旁路」
        b_body = "## 概述\n\n异单关联旁路形态。\n\n" + bypass_blk + "Closes #3042\n"
        errs = gate2(b_body, h_bypass, h_plain2)
        if not any("异单旁路" in e and "fail-closed" in e and "#3999" in e for e in errs):
            fails.append(f"fix-r2 P0 形态 b：异单关联的修复轮标题应 fail-closed 拒收：{errs}")
        # 形态 (a) 负控：no-issue 正文——标题 #3999 配对不上空关联集，必须红且写明「no-issue 旁路」
        a_body = "## 概述\n\nno-issue 旁路形态。\n\n" + bypass_blk + "<!-- no-issue 纯文档轮豁免说明文字足够长 -->\n"
        errs = gate2(a_body, h_bypass, h_plain2)
        if not any("no-issue 旁路" in e and "fail-closed" in e and "#3999" in e for e in errs):
            fails.append(f"fix-r2 P0 形态 a：no-issue 正文的修复轮标题应 fail-closed 拒收：{errs}")
        # 正控：同一标题 + 正文补上关联（Refs #3999）+ 齐字段 → 过闸（旁路封堵不误伤正常修复轮）
        ok2_body = ("## 概述\n\n旁路封堵正控。\n\n"
                    + bypass_blk.replace("readonly=tools", "readonly=tools fixtype=结构性")
                    + "<!-- pdrs-residual: 上轮洞是配对判定的空集分支，fail-closed 收口 -->\nRefs #3999\n")
        errs = gate2(ok2_body, h_bypass, h_plain2)
        if errs:
            fails.append(f"fix-r2 P0 正控：标题单号入关联集 + 齐字段应过闸：{errs}")
        # board 尾回归护栏（fix-r5 修 2 的合法形态不许被 foreign 误伤）：范围含 board 尾标题
        # （引用 #2691/#2692 的轮号在题头区外）+ Refs #3074 → 无 foreign 拒收、轮号不被丢弃
        # 之外也不多要（本单 fix-r2 计入 → 只要求 fixtype，不要求 residual）
        bp_body = "## 概述\n\nboard 尾回归护栏。\n\n" + blk_t.format(h=h_sync) + "Refs #3074\n"
        errs = gate2(bp_body, h_sync, h_r5)
        if any("旁路" in e for e in errs) or not any("fix-r2" in e and "fixtype" in e for e in errs):
            fails.append(f"fix-r2 P0 board 尾护栏：题头区外引用别的单不得触发 foreign 拒收（且本单 "
                         f"fix-r2 仍要求 fixtype）：{errs}")
        # 纯函数负控：题头区内非空不相交链 → foreign；题头区外 / 链有交集 → 不 foreign
        if len(gate.foreign_fixround_pairs(["fix(gateway) #3056 fix-r5：x"], frozenset())) != 1:
            fails.append("fix-r2 P0 纯函数：题头区引用 + 空关联集应列 1 条 foreign")
        # fix-r12 P2-2（sibling#2）口径收窄：本断言只覆盖「无裸号引用、无散文间隔」的
        # 相交半边（紧邻配对 → 计入，不 foreign）；裸号引用/散文间隔断归属的相交半边
        # 自 fix-r11 起照进 foreign 拒收（归属存疑）——正控见下一条。
        if gate.foreign_fixround_pairs(["fix(gateway) #3056 fix-r5：x"], frozenset({"3056"})):
            fails.append("fix-r2 P0 纯函数：无裸号引用、无散文间隔时链 ∩ issues ≠ ∅ 不得列 foreign")
        # fix-r12 P2-2 正控（与 fix-r11 P1-1 的 _scan_pairs「前向臂相交半边」钉成对）：
        # 裸号引用断归属的相交半边，纯函数 foreign_fixround_pairs 同样拒收——两入口
        # 钉与真实行为同步，防「只钉 _scan_pairs、check() 实际路径漂移」
        if not gate.foreign_fixround_pairs(["fix(gateway) #3056 P1×3 + 2932 fix-r2：x"],
                                          frozenset({"3056"})):
            fails.append("fix-r12 P2-2：裸号引用断归属的相交半边，foreign_fixround_pairs "
                        "应非空拒收（与 fix-r11 P1-1 的 _scan_pairs 钉成对）")
        if gate.foreign_fixround_pairs([board_t], frozenset({"3074"})):
            fails.append("fix-r2 P0 纯函数：board 尾（题头区外引用别的单）不得列 foreign")
        if gate.foreign_fixround_pairs(["chore: 无轮号提交"], frozenset()):
            fails.append("fix-r2 P0 纯函数：无 fix-rN 的标题不得列 foreign")
        # fix-r2 DS boundary#5 P2：全角冒号后 token + 后随 #N 就近配对（`fix(gateway)：fix-r5 修
        # #3074`）——旧判据 i=-1 且题头区外 → 判不出 → fix_round=None → 同一假 PASS 面
        if gate.fix_round_from_title("fix(gateway)：fix-r5 修 #3074", frozenset({"3074"})) != 5:
            fails.append("fix-r2 DS#5 正控：token 后方最近 #N ∈ issues 应按后向配对计入（5）")
        if gate.fix_round_from_title("fix(gateway)：fix-r5 修 #3074", frozenset({"999"})) is not None:
            fails.append("fix-r2 DS#5 负控：后方最近 #N ∉ issues 不得计入（已知窄窗在 docstring 登记）")
        if gate.fix_round_from_title("chore(state) 交接段：#9999 记录 fix-r6 复盘（前方 #N 属别的单、无后随 #N）",
                                     frozenset({"2691"})) is not None:
            fails.append("fix-r2 DS#5 负控：题头区外 + 前方 #N 配不上 + 无后随 #N 的板尾裸号仍不计入")
        # 无 --head 端到端（任务书验 4 + 修 4，boundary#4 P2）：本地 --body-file 不传
        # --head/--base → 主信号缺失，兜底 = 首行宽松 + 全正文结构化回显扫描——badge 开头、
        # 非首行 `fix(scripts): #1 fix-r5：…` 是结构化回显（scope 括号 + #N + 配到本 PR 单
        # Refs #1）→ 判修复轮 5、要求 fixtype/residual。fix-r5~r6 的「只看首行」收窄被修 4
        # 反转：本地先跑闸门是硬步骤，只看首行会把 badge/「## 概述」开头的正文判非修复轮、
        # 本地过 CI 再拒（boundary#4 的原诉）
        nohead_body = ("![ci](https://example.com/b.svg)\n\nfix(scripts): #1 fix-r5：修闸门\n\n"
                       + blk_t.format(h="") + "Refs #1\n")
        errs = gate.check(nohead_body, "", "", "", "")
        if not any("fix-r5" in e and "fixtype" in e for e in errs) or not any("pdrs-residual" in e for e in errs):
            fails.append(f"fix-r5 修4 正控：无 --head 时 badge 首行 + 非首行结构化回显应按修复轮 5 "
                         f"要求 fixtype+residual（本地过、CI 双保险）：{errs}")
        # 修 4 配对负控（本地形态）：回显行 #N 配不到本 PR 关联单 → 不计 → 义务不触发；
        # fix-r14 起正文回显 foreign 半边与标题路径同口径（P0 修的必然结果——同串作
        # 提交标题自 fix-r5 修2 起即按异单旁路拒收，旧正文路径静默不计是半边不对称）
        # → 整体带异单旁路 fail-closed 拒收，不是无错过闸
        nohead_neg = ("![ci](https://example.com/b.svg)\n\nfix(scripts): #3074 fix-r3：修闸门\n\n"
                      + blk_t.format(h="") + "Refs #1\n")
        neg_errs = gate.check(nohead_neg, "", "", "", "")
        if any("须带 fixtype" in e or "pdrs-residual" in e for e in neg_errs):
            fails.append(f"fix-r5 修4 负控：本地回显行 #3074 配不到本 PR 单（Refs #1）不触发"
                         f"修复轮义务（不计入）：{neg_errs}")
        if not any("异单旁路" in e and "正文回显行" in e for e in neg_errs):
            fails.append(f"fix-r14 P0 扩面钉：回显行链 #3074 ∉ 本 PR 关联单（#1）须异单旁路"
                         f"fail-closed 拒收（同串作标题自 fix-r5 起即拒收）：{neg_errs}")
        # git 失败 fail-closed（集合路径，任务书验 6）：head/base 未知提交、repo 不是 git 仓库
        # （目录存在但不 init——不存在的路径连 chdir 都失败，测不到「不是仓库」本身），都不得当
        # 「非修复轮」放行
        notrepo = tmp / "not-a-repo"
        notrepo.mkdir()
        for b_bad, h_bad, repo_bad, why in [(c_base, "f" * 40, str(wt2), "head 未知提交"),
                                            ("e" * 40, h_sync, str(wt2), "base 未知提交"),
                                            (c_base, h_sync, str(notrepo), "repo 不是 git 仓库")]:
            errs = gate.check(blk_t.format(h=h_bad) + "Refs #1\n", h_bad, "", b_bad, repo_bad)
            if not any("不得当非修复轮放行" in e for e in errs):
                fails.append(f"fix-r6 fail-closed 负控（集合路径）：{why} 应拒收并写明原因：{errs}")
        # 空集（base==head）合法（任务书修点 3）：与 git 失败区分开——不 fail-closed，走兜底判
        # 非修复轮，整体过闸
        errs = gate.check(blk_t.format(h=h_plain2) + "Refs #1\n", h_plain2, "", h_plain2, str(wt2))
        if errs:
            fails.append(f"fix-r6 空集负控：base==head 是合法空集，不得当 git 失败 fail-closed 或误判修复轮：{errs}")
        # 空 subject 提交（fix-r5 行为变化钉）：集合里只是一条不命中的标题（%s 输出空行被过滤），
        # 不再像 fix-r5 的 head 单标题路径那样单独 fail-closed；范围只含空标题提交 → 空集 → 兜底
        subprocess.run([*g2, "commit", "-q", "--allow-empty", "--allow-empty-message", "-m", ""], check=True)
        h_empty = subprocess.run([*g2, "rev-parse", "HEAD"], capture_output=True, text=True).stdout.strip()
        errs = gate.check(blk_t.format(h=h_empty) + "Refs #1\n", h_empty, "", h_plain2, str(wt2))
        if errs:
            fails.append(f"fix-r6 空 subject：范围只含空标题提交应走兜底判非修复轮、整体过闸"
                         f"（不再像 fix-r5 单标题路径 fail-closed）：{errs}")
        # —— fix-r7 修 2.2（DS r6 boundary#3）：git log 空输出两分——base==head 合法空集（上面
        # 已钉）；--base/--head 传反（head 是 base 的祖先、base 是 head 的后代）是坏范围，
        # fail-closed 拒收写明原因，不得当合法空集落到正文首行兜底静默丢主信号 ——
        rev_titles, rev_err = gate.pr_commit_titles(str(wt2), h_sync, c_base)  # head=c_base 是 base=h_sync 的祖先
        if rev_titles is not None or "传反" not in rev_err:
            fails.append(f"fix-r7 传反 fail-closed：git log 空输出且 base≠head 应拒收写明传反"
                        f"（titles={rev_titles}，terr={rev_err}）")
        rev_body = "## 概述\n\n正文首行干净，范围传反。\n\n" + blk_t.format(h=c_base) + "Refs #1\n"
        errs = gate.check(rev_body, c_base, "", h_sync, str(wt2))
        if not any("传反" in e and "不得当非修复轮放行" in e for e in errs):
            fails.append(f"fix-r7 传反 fail-closed 端到端：应拒收并写明「传反」且不放行：{errs}")
        # 后代 base 同款（base=head 的后代、head 是更早提交——等价于两参写反的另一方向）
        rev_titles2, rev_err2 = gate.pr_commit_titles(str(wt2), h_plain2, h_r2)
        if rev_titles2 is not None or "传反" not in rev_err2:
            fails.append(f"fix-r7 后代 base fail-closed：base 是 head 后代应拒收（titles={rev_titles2}，terr={rev_err2}）")
        # —— fix-r7 修 2.1（DS r6 boundary#2）：--base/--head/--repo 只给一部分（或全缺）时打
        # stderr 显式告警说明主信号缺失原因，返回语义不变（照走正文首行兜底）；三全缺同告警 ——
        warn_ok = tmp / "warn-body.md"
        warn_ok.write_text("<!-- pr-gate:waiver 纯文档轮豁免说明文字足够长 -->\nRefs #1\n", encoding="utf-8")
        gate_cli = ROOT / "scripts" / "check-pr-gate.py"
        for extra, want_missing in [ (["--head", h_plain2, "--repo", str(wt2)], "--base"),
                                     (["--base", h_plain2, "--repo", str(wt2)], "--head"),
                                     ([], "--base --head") ]:
            cp = subprocess.run([sys.executable, "-I", str(gate_cli), "--body-file", str(warn_ok), *extra],
                                capture_output=True, text=True)
            if (cp.returncode != 0 or "PASS" not in cp.stdout
                    or want_missing not in cp.stderr or "修复轮主信号" not in cp.stderr):
                fails.append(f"fix-r7 单缺告警：{' '.join(extra) or '（三件全缺）'} 应 stderr 告警缺 {want_missing}"
                             f" 且返回语义不变（rc={cp.returncode}，stdout={cp.stdout!r}，stderr={cp.stderr!r}）")
        # 告警不打进 stdout（CI 只看 FAIL 行，告警是给跑的人看的，走 stderr）
        cp = subprocess.run([sys.executable, "-I", str(gate_cli), "--body-file", str(warn_ok)],
                            capture_output=True, text=True)
        if "主信号" in cp.stdout:
            fails.append(f"fix-r7 告警通道：告警须走 stderr 不进 stdout（stdout={cp.stdout!r}）")

        # fix-r4 修 3.5（DS boundary#1 + MM boundary#2 P0 双撞后的边界钉）：首行与非首行
        # 的**有意不对称**——首行宽松（fix-r1 P0 硬要求：CJK 连写首行「修复fix-r5：」无类型词/
        # 无 #N，收紧即假 FAIL）意味着类型词开头的散文行**出现在首行**时也会判修复轮（已知窄代价：
        # 该 PR 会被要求 fixtype/pdrs-residual，多一道误拒而非漏检）；同一行出现在非首行位置则不判
        # （上「test 覆盖了 fix-r5 的场景」负控钉住）。两向都钉死，防后续轮再把首行收紧回去。
        if gate.fix_round_from_body("test 覆盖了 fix-r5 的场景\n") != 5:
            fails.append("fix-r4 修3.5 边界钉：类型词开头的散文行在**首行**位置应判修复轮 5（首行宽松是 "
                         "fix-r1 P0 硬要求，有意保留；非首行同款负控见上）")
        if gate.fix_round_from_body("## 概述\n\ntest 覆盖了 fix-r5 的场景\n") is not None:
            fails.append("fix-r4 修3.5 边界钉：同一散文行在非首行位置不应判修复轮")
        # （fix-r9 P2-1：本孪生负控不带 echo_scan 参数＝走默认——默认自 fix-r9 起为
        # True，与生产唯一调用点 check() 恒传 True 对齐，不再分家；散文行无 ECHO_HEAD
        # 结构，回显扫描照判 None，断言语义不变）

        # —— #3056 fix-r3（MM concurrency P0 + 双撞 P1 + 两单家 P1）：修复轮识别盲与题头区
        # 结构性修——rN 简写形 / 非类型词开头 / CI 回显扫描 / 枚举型标题 / 板尾裸号 /
        # waiver fixtype 至多一次 / b 后缀词表单源 / 位界 fail-closed ——
        # P0 四形正控（任务书验 3：协调者仲裁输入作为测试断言逐字钉死）：
        for t, iv, want in [
            # ① 仓内真实简写惯例：wt-3056 分支 4 条 `#3056 r1[4-7]` 形、main 的
            # `fix(gateway) #2461 r5：…` 同形（RSHORT：题头区内 + 与前方 #N 链紧邻）
            ("fix(scripts) #3056 r14：题头区判定修正", {"3056"}, 14),
            ("fix(gateway) #2461 r7：raw_request 界随判据走共享有界探针", {"2461"}, 7),
            # ② 非类型词开头 + 行内 fix-r<N>（去类型词化：allow_unpaired=False 走配对分支）
            ("#3056 fix-r3：四角色重钉评审逐条修", {"3056"}, 3),
            # ④ 纯 docs 形判出（判出后只触发 fixtype 义务、不触发代码域检查——行为对齐既有语义，
            #    义务触发面的分流在 check() 的 fixtype/residual 字段而非标题判据）
            ("docs(state) #2692 r13 停轮决策卡：记录", {"2692"}, 13),
        ]:
            if gate.fix_round_from_title(t, frozenset(iv)) != want:
                fails.append(f"fix-r3 P0 四形：head 标题应判修复轮 {want}：{t[:56]!r}")
        # P0 负控：rN 简写只在题头区 + 紧邻 #N 链时计入——题头区外（评审轮引用，语料 423 条，
        # probe-corpus.txt §1b）与不紧邻（评审者署名 `MM r16` 的字母断开）都不计
        if gate.fix_round_from_title("fix(scripts) #1 fix-r3：r2 评审修正", frozenset({"1"})) != 3:
            fails.append("fix-r3 P0 负控：题头区外的 r2 是评审轮引用，不计（fix-r3 本体照常计 3）")
        if gate.fix_round_from_title("chore(state) #1231 决策卡：停轮（r4 记录）", frozenset({"1231"})) is not None:
            fails.append("fix-r3 P0 负控：题头区外括注里的 r4（决策卡注记）不应判修复轮")
        if gate.fix_round_from_title("fix(scripts) #3056 MM r16 评审意见修正", frozenset({"3056"})) is not None:
            fails.append("fix-r3 P0 负控：rN 与 #N 链之间有字母（MM/DS 署名）不紧邻，不计")
        # fix-r12 P1-2（sibling#1，协调者裁定 b 案：确属有意 fail-closed 拒收）：上方冻结
        # 负控只钉了 rounds 半边（不计），与收集臂 fix-r11 起的 foreign 真实行为脱节——
        # 补 foreign 半边钉：「不计但拒收」（rounds=[] 且 foreign 含 (16,{3056})）。
        # 修法=把引用移出题头区（首个全角「：」之后），不是补全拼——全拼 fix-r16 对
        # 本族＝假声明修复轮 16，N≥4 触发 residual 连锁假 FAIL（文案已同步）
        if gate._scan_pairs("fix(scripts) #3056 MM r16 评审意见修正",
                            frozenset({"3056"})) != ([], [(16, frozenset({"3056"}))]):
            fails.append("fix-r12 P1-2：评审署名族简写轮号「不计但拒收」——rounds=[] 且 "
                        "foreign 含 (16,{3056})（钉与真实行为同步；修法=移出题头区，不是补全拼）")
        if gate.fix_round_from_title("chore(state) 交接段：#9999 复盘 r8 现场", frozenset({"9999"})) is not None:
            fails.append("fix-r3 P0 负控：题头区外的 rN（交接段板尾）不计")
        # P0② 负控：去类型词化不放开散文——无 #N 配对的 non-TITLE_HEAD token 不计
        # （`fix-r2 #2516 收尾` 负控见上；这里钉「无 #N 的散文形」与「#N 在前的配对形」对照）
        if gate.fix_round_from_title("temp: fix-r2 说明", frozenset({"3056"})) is not None:
            fails.append("fix-r3 P0② 负控：非 TITLE_HEAD 且无 #N 配对的散文标题不应判修复轮")
        # P0② 正控的 foreign 半边：去类型词化后异单旁路不得从新门溜进
        if len(gate.foreign_fixround_pairs(["#9999 fix-r5：异单"], frozenset({"3074"}))) != 1:
            fails.append("fix-r3 P0②：非 TITLE_HEAD 的异单标题（#9999 fix-r5）也应进 foreign 拒收")
        if len(gate.foreign_fixround_pairs(["fix(scripts) #9999 r5：异单"], frozenset({"3074"}))) != 1:
            fails.append("fix-r3 P0①：rN 简写形的异单标题（#9999 r5）也应进 foreign 拒收")
        # P0③ CI 形态（titles 非空但全不命中）正文兜底走结构化回显扫描：范围只含普通状态提交、
        # 正文 `## 概述` 开头 + 非首行结构化回显（#3074 配到本 PR 单）→ 判修复轮 5、要求 fixtype
        # ——旧「只看首行」会把 `#N rM` 简写标题 + 概述开头的正文整单判非修复轮（四项义务全跳）
        rshort_body = ("## 概述\n\n正文以概述开头的修复轮不再漏判。\n\n"
                       "fix(scripts): #3074 fix-r5：修闸门\n\n" + blk_t.format(h=h_plain2) + "Refs #3074\n")
        errs = gate2(rshort_body, h_plain2, h_sync)
        if not any("fix-r5" in e and "fixtype" in e for e in errs):
            fails.append(f"fix-r3 P0③：CI 形态（范围全不命中）概述开头 + 非首行结构化回显应判修复轮 5：{errs}")
        # P1-1 面 A（MM boundary#1 + DS boundary#1 双撞，协调者仲裁输入写进测试注释）：
        # `_scan_pairs('chore(state) 01:45 #1305 fix-r3 P0=0 P1=0 两家收敛；#1318 缺第二家标缺口',
        # {'1318'})` 旧得 `([], [(3, {'1305'})])`＝异单假 FAIL 实证；枚举型标题（≥2 条互不相交
        # #N 链）整条不作 foreign 判定，计入分支不受影响
        c1 = "chore(state) 01:45 #1305 fix-r3 P0=0 P1=0 两家收敛；#1318 缺第二家标缺口"
        if gate._scan_pairs(c1, frozenset({"1318"})) != ([], []):
            fails.append(f"fix-r3 P1-1 面 A：仲裁 case1 应得 ([], [])（旧 ([], [(3, {{'1305'}})] 假 FAIL），"
                         f"得 {gate._scan_pairs(c1, frozenset({'1318'}))}")
        # 面 A 负控：枚举抑制只压 foreign——单链异单照旧拒收（两链有交集不构成枚举）
        if not gate.foreign_fixround_pairs(["fix(scripts) #3056+#3074 fix-r5：x"], frozenset({"999"})):
            fails.append("fix-r3 P1-1 面 A 负控：单条 #N 链（+ 并组）的异单照旧进 foreign（枚举抑制不得放宽计数面）")
        if not gate.foreign_fixround_pairs(["fix(scripts) #9999 fix-r5；#9999 补记 x"], frozenset({"3074"})):
            fails.append("fix-r3 P1-1 面 A 负控：两条链有交集（共享 #9999）不构成枚举，异单照旧拒收")
        # P1-1 面 B（仲裁输入）：`_scan_pairs('docs(state) 22:51 补落（2558 fix-r5 派 macmini）',
        # {'3129'})` 旧得 `([5], [])`＝板尾裸号被计入实证；无全角冒号时 token 前紧邻裸数字串
        # 视为 board 枚举引用，不计轮（docstring「板尾裸号不计」从「：后」扩到无冒号形）
        c4 = "docs(state) 22:51 补落（2558 fix-r5 派 macmini）"
        if gate._scan_pairs(c4, frozenset({"3129"})) != ([], []):
            fails.append(f"fix-r3 P1-1 面 B：仲裁 case4 应得 ([], [])（旧 ([5], []) 裸号误计），"
                         f"得 {gate._scan_pairs(c4, frozenset({'3129'}))}")
        if gate.fix_round_from_title(c4, frozenset({"3129"})) is not None:
            fails.append("fix-r3 P1-1 面 B：板尾裸号（2558 fix-r5）不应把纯文档 PR 判成修复轮")
        # 面 B 负控：守卫只认「裸数字紧邻 token」——`PR#3074`（# 前缀）与无裸号的形态照常计入
        if gate.fix_round_from_title("fix(scripts) PR#3074 fix-r5：x", frozenset({"3074"})) != 5:
            fails.append("fix-r3 P1-1 面 B 负控：PR#3074（# 前缀）不是裸号，免配对兜底照常计 5")
        if gate.fix_round_from_title("chore: retrigger CI for the fix-r1 probe", frozenset()) != 1:
            fails.append("fix-r3 P1-1 面 B 负控：无裸号的免配对兜底照常计 1（守卫不得误伤）")
        if gate.fix_round_from_title("chore(state) 22:51 补落（fix-r5 派 macmini，无裸号）", frozenset()) != 5:
            fails.append("fix-r3 P1-1 面 B 负控：裸号不紧邻 token（22:51 与 fix-r5 间隔中文）时守卫不触发，"
                         "免配对兜底照常计 5（宁高勿低不变）")
        # P1-2（MM sibling#1）：豁免理由 fixtype= 载体至多一次——双载体（第二个 不适用 不被校验、
        # 不触发 check_na_path）必须拒收并说明「至多一次」，且第二个 不适用 仍触发 na 路径检查
        w_dup = ("fix(gateway) #1 fix-r5：纯文档改动\n\n"
                 "<!-- pr-gate:waiver 纯文档轮豁免 fixtype=结构性 fixtype=不适用 说明文字足够长 -->\n"
                 f"{residual}Refs #1\n")
        errs = gate.check(w_dup, head)
        if not any("至多一次" in e for e in errs):
            fails.append(f"fix-r3 P1-2：豁免理由双 fixtype= 载体应拒收并说明「至多一次」：{errs}")
        if not any("无法按 ci_scope 语义复算" in e for e in errs):
            fails.append(f"fix-r3 P1-2：双载体里的 fixtype=不适用 仍须触发 check_na_path（无 --base 拒收）：{errs}")
        # P1-3（MM boundary#2）：接续轮 b 后缀——fix-r11b 算第 11 轮；fix-r5x 不是修复轮（既有
        # 负控 :637 处 fix_round_from_body 同词表钉死，这里补标题侧）；b 后缀算 N≥4 residual 门槛轮
        if gate.fix_round_from_title("fix(scripts) #1 fix-r11b：接续轮收尾", frozenset({"1"})) != 11:
            fails.append("fix-r3 P1-3 正控：fix-r11b 接续轮应算第 11 轮")
        if gate.fix_round_from_title("fix(scripts) #1 fix-r5x：x", frozenset({"1"})) is not None:
            fails.append("fix-r3 P1-3 负控：fix-r5x（x 非 b）不是修复轮")
        if gate.fix_round_from_body("fix-r11b 接续轮说明\n") != 11:
            fails.append("fix-r3 P1-3 正控：正文首行 fix-r11b 应判修复轮 11")
        if gate.fix_round_from_body("fix-r5b2 多级后缀\n") is not None:
            fails.append("fix-r3 P1-3 负控：fix-r5b2（b 后又跟数字）词尾不闭合，不是修复轮")
        # P2a（DS boundary#2）：位界 {1,18} 外的超长数字串 fail-closed 干净报错，不进 traceback
        # ——直接调 check()（正文路径 `Refs #<19+位>` 旧会走到 sorted(issues, key=int) 的
        # ValueError）；标题路径用 wt2 夹具提交一条 19 位形
        over_errs = gate.check("## 概述\n\nRefs #" + "1" * 19 + "\n" + blk_t.format(h=""), "")
        if len(over_errs) != 1 or "超长数字串" not in over_errs[0] or "fail-closed" not in over_errs[0]:
            fails.append(f"fix-r3 P2a：正文 19 位 #N 应单条 fail-closed 拒收（无 traceback）：{over_errs}")
        if gate._overflow_err("fix-r" + "9" * 19 + "：x", "t") is None or \
           gate._overflow_err("r" + "9" * 19 + " x", "t") is None or \
           gate._overflow_err("fix-r5：#3056 正常", "t") is not None:
            fails.append("fix-r3 P2a：_overflow_err 三个 token 形（fix-r/#/r）与正常形判据漂移")
        subprocess.run([*g2, "checkout", "-q", "-b", "fixr3-overflow"], check=True)
        h_over = titled2("fix(scripts) #" + "1" * 19 + " fix-r5：超长单号")
        subprocess.run([*g2, "checkout", "-q", "-"], check=True)
        errs = gate.check(blk_t.format(h=h_over) + "Refs #3056\n", h_over, "", c_base, str(wt2))
        if len(errs) != 1 or "超长数字串" not in errs[0]:
            fails.append(f"fix-r3 P2a：19 位单号的提交标题应单条 fail-closed 拒收（旧实现 int() 崩溃）：{errs}")

        # —— #3074 fix-r4（DS r3 增量评审 P0×2 / P1×2 / P2 顺手修）——
        # P0-A 正控（boundary#1，协调者仲裁输入 _tmp/20261114-3074-fixr4-arb/arb.py）：裸号
        # 口径收窄为「数字前（跳过空白）最近的非空白字符是枚举分隔（非 \w、非 ASCII :、非 #）」
        # ——`worktree 60`（席位上限值，字母前邻）与 `01:45`（分钟，ASCII 冒号前邻）豁免，
        # r1..r7 真实修复轮族恢复判轮（A/B 复算回退方向 7 条全回：wordlist-ab-recompute.txt）
        for t, want in [("chore(tools) worktree 60 fix-r1：磁盘三档的唯一实现 + 闸门 fail-closed", 1),
                        ("chore(tools) worktree 60 fix-r4：判档入口不再对空/非数字放行", 4),
                        ("chore(tools) worktree 60 fix-r7：排障入口在子目录不再量错根", 7),
                        ("chore(state) 01:45 fix-r5 同步", 5)]:
            if gate.fix_round_from_title(t) != want:
                fails.append(f"fix-r4 P0-A 正控：真实修复轮标题应判 {want}：{t[:48]!r}")
        # P0-A 负控：board/交接段裸号引用（数字前是 ：、+、×、（ 等枚举分隔）仍压——A/B
        # 复算「仍被压」5 条真候选的机制钉子（02:xx/04:31 交接段形态）
        for t in ["chore(state) board 04:31 交接段：1800 fix-r6 推 PR 3009",
                  "chore(state) board 09:30 交接段：2964 pathname 家族 P1×3 fix-r10 统一收口",
                  "chore(state) board 12:19 交接段：2943 fix-r2 交付 + 2932 fix-r2 实测仲裁派单",
                  "chore(state) board 13:30 交接段：1833 宿主实杀 14/14 闭环 + 1623 fix-r6 双谱系收敛入队"]:
            if gate.fix_round_from_title(t) is not None:
                fails.append(f"fix-r4 P0-A 负控：board/交接段裸号引用（枚举分隔前邻）不得判修复轮：{t[:48]!r}")
        # P0-B 计时回归钉（concurrency#1 + boundary#2 双撞）：`#1；`×N 每条 hash 各成一条链，
        # 两两 isdisjoint 是 O(C²)（仲裁实测 n=16000 达 3.24 s，纯二次）；线性短路 + 去重后须 <2 s
        t0 = time.perf_counter()
        gate._scan_pairs("chore(x) " + "#1；" * 16000, frozenset({"9999"}))
        if (dt0 := time.perf_counter() - t0) >= 2.0:
            fails.append(f"fix-r4 P0-B 计时钉：16000 条链的枚举判定应 <2 s（实测 {dt0:.3f}s，防 O(C²) 退化）")
        # P1-E 计时钉（concurrency#2 + boundary#3 同族）：无 #N 的 TITLE_HEAD 标题每 token 前缀
        # 切片 + 整段重扫是 O(T·n)（实测旧口径 n=8000 达 1.77 s）；有界窗口 + 单调指针后须 <2 s
        t0 = time.perf_counter()
        gate._scan_pairs("fix(x) " + "fix-r1 " * 12000, frozenset())
        if (dt1 := time.perf_counter() - t0) >= 2.0:
            fails.append(f"fix-r4 P1-E 计时钉：12000 个 token 的裸号判定应 <2 s（实测 {dt1:.3f}s，防 O(T·n) 退化）")
        # P1-C 负控（sibling#1 + boundary#4 双撞）：枚举判定限题头区内的链——题头区单链 +
        # 尾部 #N 注记不构成枚举，异单失配照旧进 foreign（fix-r2 P0 旁路不复开）
        if not gate.foreign_fixround_pairs(["fix(scripts) #9999 fix-r5：修评审（复用 #1321 机制）"],
                                           frozenset({"3056"})):
            fails.append("fix-r4 P1-C 负控：题头区单链 + 尾部 #N 注记不构成枚举，异单失配应进 foreign")
        if not gate.foreign_fixround_pairs(["#3056 fix-r3：合 #1305、#1318 两处"], frozenset()):
            fails.append("fix-r4 P1-C 负控：no-issue 时题头区单链标题照旧进 foreign（枚举抑制不开旁路）")
        # P1-C 正控（保 r3 面 A 语义）：两条不相交链都在题头区内的枚举/board 标题仍整条不作
        # foreign 判定（case1 真实形态：无全角冒号 → 整条是题头区）
        if gate._scan_pairs("chore(state) 01:45 #1305 fix-r3 两家收敛；#1318 缺第二家标缺口",
                            frozenset({"1318"}))[1]:
            fails.append("fix-r4 P1-C 正控：题头区内两链（无全角冒号形）仍是枚举标题，不作 foreign 判定")
        # P1-C 端到端：异单×多链 / no-issue×多链都必须 fail-closed 拒收（尾部注记链关不掉封堵）
        subprocess.run([*g2, "checkout", "-q", "-b", "fixr4-enum-bypass"], check=True)
        h_enum = titled2("fix(scripts) #9999 fix-r5：修评审（复用 #1321 机制）")
        subprocess.run([*g2, "checkout", "-q", "-"], check=True)
        eblk = blk_t.format(h=h_enum)
        errs = gate2("## 概述\n\n异单多链形态。\n\n" + eblk + "Closes #3056\n", h_enum, c_base)
        if not any("异单旁路" in e and "#9999" in e for e in errs):
            fails.append(f"fix-r4 P1-C：异单×多链（题头区 #9999 + 尾部 #1321）应 fail-closed 拒收：{errs}")
        errs = gate2("## 概述\n\nno-issue 多链形态。\n\n" + eblk + "<!-- no-issue 纯文档轮豁免说明文字足够长 -->\n", h_enum, c_base)
        if not any("no-issue 旁路" in e and "#9999" in e for e in errs):
            fails.append(f"fix-r4 P1-C：no-issue×多链（题头区 #9999 + 尾部 #1321）应 fail-closed 拒收：{errs}")
        # P1-D 负控（sibling#2）：后向配对分支补裸号守卫——board 尾 token 前紧邻裸号 + 后方
        # #N ∈ issues 也不计入（语料 probe-corpus.txt board 13:42 形，旧判 2 ＝假 FAIL）
        if gate.fix_round_from_title(
                "chore(state) board 13:42 交接段：队列 60→120（2879 踢出谜底）+ 2932 fix-r2 交付 + #2957 P1×3 + 远端三车道续",
                frozenset({"2957"})) is not None:
            fails.append("fix-r4 P1-D 负控：board 尾后向配对（裸号 2932 紧邻 token + 后方 #2957 ∈ issues）不得计入")
        # P1-D 正控：无裸号紧邻的后向配对（r3 P0③ 修复形）不受新守卫影响
        if gate.fix_round_from_title("fix(gateway)：fix-r5 修 #3074", frozenset({"3074"})) != 5:
            fails.append("fix-r4 P1-D 正控：无裸号的后向配对（fix-r5 修 #3074）照常计 5")
        # P2-1 负控（concurrency#3 + boundary#8 + sibling#3 三撞）：19 位 + b 后缀的超长 token
        # 旧既不匹配主词表也不触发 OVER fail-closed＝静默漏判——OVER 词表同步 b? 后必须报错
        if gate._overflow_err("fix-r" + "9" * 19 + "b：x", "t") is None:
            fails.append("fix-r4 P2-1：fix-r + 19 位 + b 应 fail-closed（FIXROUND_OVER 同步 b?）")
        if gate._overflow_err("r" + "9" * 19 + "b x", "t") is None:
            fails.append("fix-r4 P2-1：r + 19 位 + b 应 fail-closed（RSHORT_OVER 同步 b?）")
        # P2-1 正控（boundary#7）：「简写 + 接续轮」组合形 r14b 算第 14 轮（RSHORT 同步 b?）
        if gate.fix_round_from_title("fix(scripts) #3056 r14b：接续轮收尾", frozenset({"3056"})) != 14:
            fails.append("fix-r4 P2-1 正控：简写接续轮 r14b 应算第 14 轮")
        # P2-3（concurrency#4 + boundary#12 双撞）：超长数字串早退不得吞既有 errs——git 失败
        # 原因与超长串都报（repo 指错 → pr_commit_titles 失败 + 正文 19 位 #N）
        errs = gate.check("## 概述\n\nRefs #" + "1" * 19 + "\n" + blk_t.format(h=""), h_enum, "", c_base, "/nonexistent-repo-xyz")
        if not any("读不到 PR 范围" in e for e in errs) or not any("超长数字串" in e for e in errs):
            fails.append(f"fix-r4 P2-3：超长串早退应保留 git 失败原因（errs 不丢）：{errs}")

        # —— #3074 fix-r5（DS r4 增量评审 boundary#1 P0 + sibling#1 P1 + sibling#3 P2 顺手修）——
        # P0 负控（boundary#1，协调者仲裁 _tmp/20261114-3074-fixr4-arb/arb-fixr4-out.txt 四形）：
        # 题头区分界只认全角「：」⇒ ASCII 冒号/无冒号标题的尾部 #N 注记参与枚举 → foreign 抑制
        # → fix-r2 P0 异单旁路复开。封堵＝枚举链参与判据再收两道界（锚点后 ASCII 冒号切
        # enum_end + 括号深度 ≥1 的链不参与），四形全部异单命中：
        for t in ["fix(scripts) #9999 fix-r5: 修评审（复用 #1321 机制）",        # ASCII 冒号形
                  "fix(scripts) #9999 fix-r5 修评审（复用 #1321 机制）",        # 无冒号形
                  "fix(gateway-parity) #9999 fix-r25: 按 #1355/#1652 双谱系修复"]:  # 真实语料形
            if not gate.foreign_fixround_pairs([t], frozenset({"3056"})):
                fails.append(f"fix-r5 P0 负控：ASCII 冒号/无冒号形的异单旁路应进 foreign 拒收：{t[:52]!r}")
        # P0 正控：锚点（首个 #N/fix-rN）**之前**的 ASCII 冒号不作 enum 界——前缀冒号
        # （`chore(board):`）与时间冒号（`01:45`）都在锚点前，题头区链分布不变（board 大型
        # 并记形的枚举抑制不误伤；`chore(state) 01:45 fix-r5 同步` 计 5 见 fix-r4 P0-A 正控）
        if gate._scan_pairs("chore(board): 01:10 协调轮——#428 fix-r2 合入；#406 fix-r8 缺第二家",
                            frozenset({"9999"}))[1]:
            fails.append("fix-r5 P0 正控：锚点前 ASCII 冒号不作 enum 界，board 并记两链仍枚举抑制"
                         "（前缀冒号收紧会误伤 chore(board): 01:10 族）")
        # P0 正控：括号深度界只压「括号内链」——括号外两链照常构成枚举（case1 真实形态）
        if gate._scan_pairs("chore(state) 01:45 #1305 fix-r3 两家收敛；#1318 缺第二家标缺口（转 #9999）",
                            frozenset({"1318"}))[1]:
            fails.append("fix-r5 P0 正控：括号外两链（{1305}/{1318}）仍是枚举标题，不作 foreign 判定"
                         "（尾部括号内 #9999 注记不改变抑制）")
        # P0 端到端（任务书硬判据）：ASCII 冒号形 + 无冒号形 + Closes #3056 → 异单旁路必须报错
        for bi, h_txt in enumerate(["fix(scripts) #9999 fix-r5: 修评审（复用 #1321 机制）",
                                    "fix(scripts) #9999 fix-r5 修评审（复用 #1321 机制）"]):
            subprocess.run([*g2, "checkout", "-q", "-b", f"fixr5-zone-bypass-{bi}"], check=True)
            h_byp = titled2(h_txt)
            subprocess.run([*g2, "checkout", "-q", "-"], check=True)
            errs = gate2("## 概述\n\n异单旁路拼写复开。\n\n" + blk_t.format(h=h_byp) + "Closes #3056\n", h_byp, c_base)
            if not any("异单旁路" in e and "#9999" in e for e in errs):
                fails.append(f"fix-r5 P0 e2e：异单×尾部注记（{h_txt[:36]!r}…）应 fail-closed 拒收：{errs}")
        # P1 负控（sibling#1）：前向配对臂裸号守卫——#2957∈issues 在前 + 裸号 2932 紧邻 token
        # 的镜像顺序形（P1-D 修的是 token 在 #N 前的顺序）不得计入（纯 state 假 FAIL）
        if gate.fix_round_from_title("chore(state) board 13:42 交接段：#2957 P1×3 + 2932 fix-r2 交付",
                                     frozenset({"2957"})) is not None:
            fails.append("fix-r5 P1 负控：前向配对（#2957∈issues 在前 + 裸号 2932 紧邻 token）不得计入")
        # P1 负控（foreign 半边）：裸号引用 token 不进 foreign——fix-r11 注记：该形自
        # fix-r10 起在题头区内已进 foreign 拒收，本钉现在只是**空守**（token 落在全角
        # 「：」后的题头区外＝board 尾合法形态），新语义的正控由 fix-r11 矩阵 gap 内容
        # 轴接管；删掉题头区外的空守会漏掉 board 尾回归面，保留
        if gate._scan_pairs("chore(state) board 13:42 交接段：#2957 P1×3 + 2932 fix-r2 交付",
                            frozenset({"9999"}))[1]:
            fails.append("fix-r5 P1 负控：裸号引用 token 不进 foreign（board 尾引用形，整 token 视为引用）")
        # P1 正控：无裸号的前向配对照常计入；链成员裸号（#A+#B 链尾号紧邻 token）不误伤
        if gate.fix_round_from_title("fix(scripts) #2957 fix-r2 交付", frozenset({"2957"})) != 2:
            fails.append("fix-r5 P1 正控：无裸号的前向配对（#2957 fix-r2）照常计 2")
        if gate.fix_round_from_title("fix(scripts) #3056+#3074 fix-r5：x", frozenset({"3074"})) != 5:
            fails.append("fix-r5 P1 正控：链成员裸号（#3056+#3074 的 3074）不是引用，前向配对照常计 5")
        # P2-a 负控（sibling#3）：_ENUM_CHAIN_CAP 无用例——201 条互异链（>200）按非枚举
        # fail-closed，异单 token 照进 foreign；200 条（=cap）在界内仍按枚举抑制
        cap_over = "fix(x) " + "；".join(f"#{n}" for n in range(1, 202)) + " fix-r5"
        if not gate.foreign_fixround_pairs([cap_over], frozenset({"9999"})):
            fails.append("fix-r5 P2-a：>200 条互异链按非枚举处理（fail-closed），异单 token 照进 foreign")
        cap_edge = "fix(x) " + "；".join(f"#{n}" for n in range(1, 201)) + " fix-r5"
        if gate._scan_pairs(cap_edge, frozenset({"9999"}))[1]:
            fails.append("fix-r5 P2-a：=200 条互异链在 cap 界内仍按枚举抑制（上限不误伤界内 board 形）")
        # P2-a 负控：公共元素短路分支——互异但全体相交（共享 #1）⇒ 非枚举，异单照进 foreign
        # （短路若误返 True 会关掉 foreign 封堵；`#1；#2；#1` 同集去重形不构成枚举已有 :1418 正控）
        if not gate._scan_pairs("fix(x) #1/#2；#1/#3 fix-r5", frozenset({"9999"}))[1]:
            fails.append("fix-r5 P2-a：互异但全体相交的链（共享 #1）短路判非枚举，异单 token 照进 foreign")

        # —— #3074 fix-r6（DS r5 增量评审 7 条 P2 全 a 档：单源化/负控补齐/pdepth 钳 0/窄窗登记）——
        # sibling#1 单源化：前向配对臂改走 `_bare_num_before(text, start, ends[i])` 后行为不变
        # ——链成员豁免由 helper 的 chain_end 参数承载（裸号＝前方链自身成员不视作引用）。
        # fix-r8（MM r7 concurrency P1）：中间那条 `chain_end=5` 是生产不可达的假绿形状
        # （数字起点 1 < 5，hash 内数字不可能被前导符类匹配）——换成生产真实链形
        # `#3056+3074 fix-r5`（token start=11、chain_end=5、数字起点 6 ≥ 5，经 fix-r8
        # 第二道豁免「与最近前方 hash 只隔链分隔符」判链成员）；端到端断言见下方 fix-r8 块
        if gate._bare_num_before("+3074 fix-r5", 6) is not True \
                or gate._bare_num_before("#3056+3074 fix-r5", 11, 5) is not False \
                or gate._bare_num_before("+3074 fix-r5", 6, 1) is not True:
            fails.append("fix-r6 sibling#1：_bare_num_before 的 chain_end 豁免参数口径（数字起点 < chain_end "
                         "⇒ 链成员返回 False）")
        if gate.fix_round_from_title("fix(scripts) #3056+#3074 fix-r5：x", frozenset({"3074"})) != 5:
            fails.append("fix-r6 sibling#1：单源化后链成员豁免语义不变（#3056+#3074 前向配对仍计 5）")
        # sibling#2① 半角括号负控：括号深度判据的半角分支（与全角同判）——无冒号 + 半角括号
        # 尾注形的链不参与枚举，异单失配照进 foreign
        if not gate.foreign_fixround_pairs(["fix(scripts) #9999 fix-r5 修评审 (复用 #1321 机制)"],
                                           frozenset({"3056"})):
            fails.append("fix-r6 sibling#2①：半角括号尾注形（无冒号）的链不参与枚举，异单失配应进 foreign")
        # sibling#2② + boundary#2 钳 0 后语义（以 clamp 行为为准，注释同步）：游离闭括号后
        # **不接开括号**的链回到深度 0 参与枚举——两链不相交 ⇒ 枚举抑制 ⇒ foreign 为空，
        # 与已登记「无冒号且无括号的裸尾注」窄窗同族（_enum_title docstring ③，在档）
        if gate.foreign_fixround_pairs(["fix(scripts) #9999 fix-r5 修评审 a) 复盘 #1321 机制"],
                                       frozenset({"3056"})):
            fails.append("fix-r6 sibling#2②：游离闭括号后无开括号的链参与枚举（钳 0 后与无括号窄窗同族，在档）")
        # sibling#3 _BARE_WINDOW 边界控（:314-316 注释「截断按无裸号计入＝宁高勿低」钉死）：
        # 裸号前导符恰距 token 64 字符（窗口含界）⇒ 在窗口内＝裸号引用不计；65 字符 ⇒ 截断
        # 按无裸号计入（假 FAIL 向宁高勿低）——窗口值或 max(0,…) 的改动会在此显式红
        if gate.fix_round_from_title("fix(x) ；" + "9" * 62 + " fix-r5：x") is not None:
            fails.append("fix-r6 sibling#3：裸号前导符距 token 恰 64 字符在窗口内（≤64），视为裸号引用不计")
        if gate.fix_round_from_title("fix(x) ；" + "9" * 63 + " fix-r5：x") != 5:
            fails.append("fix-r6 sibling#3：裸号前导符距 token 65 字符超出窗口，截断按无裸号计入（宁高勿低）")
        # sibling#4 窄窗 pinning（fix-r2 :69-71 / fix-r10 :935 钉法对齐）：fix-r5 登记的两条
        # 已知窄窗钉住当前行为（_enum_title docstring「已知窄窗」）——未来对 enum_end/括号界
        # 的收紧/放松会让窄窗面变化在此显式红，登记文字与实际行为不再静默失配
        if gate.foreign_fixround_pairs(["fix(x) #9999 fix-r5 复盘 #1321 机制"], frozenset({"3056"})):
            fails.append("fix-r6 sibling#4：无冒号裸尾注窄窗在档（与 board 并记词法不可分，见 _enum_title docstring）")
        if gate.foreign_fixround_pairs(["fix(x): #9999 fix-r5 修 #1321"], frozenset({"3056"})):
            fails.append("fix-r6 sibling#4：前缀冒号窄窗在档（收紧会误伤 chore(board): 01:10 族，见 _enum_title docstring）")
        # boundary#2 pdepth 钳 0 负控：游离闭括号 + 开括号（`a)（…）`／`a)(…)`）不再把深度
        # 抵回 0 ⇒ 尾部注记链停在深度 1 不参与枚举 ⇒ P0 旁路形（fix-r2 异单旁路 + 加一个 `)`）
        # 必须仍命中 foreign（旧「不钳 0」在此形复开旁路）
        for t in ["fix(scripts) #9999 fix-r5 修评审 a)（复用 #1321 机制）",
                  "fix(scripts) #9999 fix-r5 修评审 a)(复用 #1321 机制)"]:
            if not gate.foreign_fixround_pairs([t], frozenset({"3056"})):
                fails.append(f"fix-r6 boundary#2：游离闭括号+开括号不得把链抵回深度 0，"
                             f"尾部注记形异单失配仍进 foreign（钳 0）：{t[:40]!r}")
        # boundary#3 时间戳窗 pinning（登记不改标准，语料 0 实例）：锚点后条目内时间戳（HH:MM）
        # 的冒号切 enum_end ⇒ 其后枚举链（{1318}）退出枚举 ⇒ 合法 board 形对 {1305} 进
        # foreign 误报（假 FAIL 向窄窗在档，见 _enum_title docstring②）
        if not gate._scan_pairs("chore(state) #1305 fix-r3 09:30 合入；#1318 缺第二家",
                                frozenset({"3056"}))[1]:
            fails.append("fix-r6 boundary#3：锚点后时间戳冒号切 enum_end 的窄窗在档（board 形 foreign "
                         "误报＝假 FAIL 向，语料 0 实例）")

        # —— #3074 fix-r7（DS r6 增量评审 sibling#1 P1 混宽括号镜像序 + P2×3 当轮修）——
        # sibling#1 P1 负控（协调者实测复现贴 PR）：混宽括号镜像序——全角开 `（` + 半角闭
        # `)` 把旧单深度抵回 0，尾部注记链参与枚举 ⇒ enum 抑制 foreign ＝ fix-r2 P0 旁路
        # 复开（修复前 _scan_pairs 返回 ([], [])，foreign 丢失）。修法＝深度按宽度分类两条
        # （全角（）/半角 () 各自 +1/钳 0 减），链参与枚举须两类都为 0：`（a)` 形全角深度
        # 停在 1 ⇒ 注记链不参与 ⇒ 异单失配照进 foreign（fail-closed 向）。三条形：实测形
        # （带尾闭括号）、评审更短版（无尾闭括号也复发）、合法注记形对照（始终命中）
        for t in ["fix(scripts) #9999 fix-r5 修评审（a) 复用 #1321 机制）",
                  "fix(scripts) #9999 fix-r5 修评审（a) 复盘 #1321 机制",
                  # fix-r8 sibling#3（MM r7 评审）：混宽镜像序**另一半**——半角开 + 全角闭
                  # `(a）`，旧单深度下同样 −1 抵回 0（只钉 `（a)` 一半是假绿：改回单宽度
                  # 分类时该形静默回归）；分类后 hw 深度停在 1，链不参与枚举
                  "fix(scripts) #9999 fix-r5 修评审 (a）复用 #1321 机制",
                  "fix(scripts) #9999 fix-r5 修评审（复用 #1321 机制）"]:
            if not gate.foreign_fixround_pairs([t], frozenset({"3056"})):
                fails.append(f"fix-r7 sibling#1：混宽镜像序/尾部注记形的异单失配应进 foreign"
                             f"（分类深度封堵，两类都 0 才参与枚举）：{t[:40]!r}")
        # sibling#2 boundary#1 单测假绿修正：原标题（`…): fix-r3 — …`）的尾部 {1334} 被
        # ASCII 冒号与括号深度两道独立界同时排除、scope 链单独只有 1 条，enum 翻不翻转
        # 断言都不变。加第三条深度 0 独立链 #9999（评审给的现成标题）：现值 enum_chains=
        # [{9999}] 单链不抑制 → foreign 非空；scope 界失效则 {1689,1947} 与 {9999} 不相交
        # → enum=True → foreign 空，本断言必红（scope 括号族窄窗①在档，见 _enum_title
        # docstring；真实 PR body 有界实核见 _tmp/20261114-3074-fixr6/RESULT.md）
        if not gate.foreign_fixround_pairs(
                ["fix(webhook,#1689,#1947) #9999 fix-r3 — 三条 P1 当轮清零（#1334 收尾批第 3 轮）"],
                frozenset({"1334"})):
            fails.append("fix-r7 sibling#2：scope 括号链不参与枚举，第三条深度 0 链 {9999} 在场时 "
                         "scope 界失效会翻转 enum（假绿封堵），异单失配仍进 foreign 拒收")
        # sibling#3 全角闭括号两形（照 boundary#2 成对写法补齐——fix-r6 的钳 0/pinning 负控
        # 只用 ASCII `a)`，把 `）` 从钳 0 分支删掉时旧测试全绿＝假绿）：`a）（复用 #1321
        # 机制）` 断言 foreign 非空（游离全角闭括号钳 0 + 后随全角开括号 ⇒ 链停在全角深度
        # 1 不参与枚举，异单失配照进 foreign）；`a）复盘 #1321 机制` 断言 foreign 空
        # （③ pinning：钳 0 后游离闭括号不接开括号的链回到深度 0 参与枚举，窄窗在档）
        if not gate.foreign_fixround_pairs(
                ["fix(scripts) #9999 fix-r5 修评审 a）（复用 #1321 机制）"],
                frozenset({"3056"})):
            fails.append("fix-r7 sibling#3：全角游离闭括号+开括号（a）（…）不得把链抵回深度 0，"
                         "异单失配仍进 foreign（全角钳 0 分支）")
        if gate.foreign_fixround_pairs(
                ["fix(scripts) #9999 fix-r5 修评审 a）复盘 #1321 机制"],
                frozenset({"3056"})):
            fails.append("fix-r7 sibling#3：全角游离闭括号后无开括号的链参与枚举（钳 0 后与"
                         "无括号窄窗同族，③ 在档）")

        # —— #3074 fix-r8（MM r7 增量评审三 P1 当轮修 + P2 六条，协调者实测复现
        # _tmp/20261114-3074-mm-p1-probe/RESULT.md；尺子的 P1 不适用修复轮上限放行）——
        # sibling P1（r<N> 简写锚点失切）：`fix(scripts) r5: #9999 fix-r7 修评审 #1321 机制`
        # （正文 Closes #3056）的 r5 在首个 #N 之前、其 ASCII 冒号落在旧锚点（#9999）之前
        # → enum_end 不切 → 题头区两条不相交链 ⇒ enum 抑制 foreign ＝ fix-r2 P0 异单旁路
        # 在 r<N>+冒号拼写复开（修复前 _scan_pairs 返回 ([], [])，foreign 丢失；同族
        # fix-r<N> 拼写 `#9999 fix-r7:` 被 fix-r5 P0 正确切掉＝行为分裂）。修法＝锚点扩族
        # 「紧跟 ASCII 冒号的 r<N> token」；两条实测形：假通过形断言修复后 foreign 非空，
        # fix-r7: 对照形不回归（仍是 foreign 非空）
        if not gate.foreign_fixround_pairs(
                ["fix(scripts) r5: #9999 fix-r7 修评审 #1321 机制"], frozenset({"3056"})):
            fails.append("fix-r8 sibling P1：r<N>+ASCII 冒号拼写（r5: 在首个 #N 前）不再旁路——"
                         "锚点含紧跟冒号的 rN token，enum_end 被切，异单失配进 foreign 拒收")
        if not gate.foreign_fixround_pairs(
                ["fix(scripts) #9999 fix-r7: 修评审 #1321 机制"], frozenset({"3056"})):
            fails.append("fix-r8 sibling P1 对照：fix-r<N>+冒号拼写（#9999 fix-r7:）不回归，"
                         "异单失配仍进 foreign（fix-r5 P0 封堵原形）")
        # 锚点不过宽 pinning：散文 rN（后不接冒号）不作锚——`r4 01:10` 的时间冒号不被
        # 误当代号（误锚会切 enum_end ⇒ 其后 {1305}/{1318} 链退出枚举 ⇒ 合法 board 形
        # foreign 误报＝假 FAIL）；anchor 扩成未过滤裸 RSHORT 时此断言必红。
        # fix-r14 加固：整元组钉 ([], [])——旧断言只查 foreign 空，r13 曾让 rounds 静默
        # 变 [4]（无链前导简写 SELF，fix-r14 P1 修回不计），此处不再留半边盲区
        if gate._scan_pairs("chore(state) r4 01:10 #1305 fix-r3 交付；#1318 缺第二家",
                           frozenset({"3056"})) != ([], []):
            fails.append("fix-r8 sibling P1 pinning：散文 rN（后不接冒号）不作锚，01:10 时间冒号"
                         "不切 enum_end，board 双链仍按枚举抑制（不误报 foreign、无链前导"
                         "简写不计入 rounds）")
        # concurrency P1（链尾裸号整条丢弃）：`#3056+3074 fix-r5`（issues={3056}）旧口径
        # rounds=[] foreign=[]——轮号既不计入也不拒收，fixtype/residual/多块一致整段跳过
        # ＝假 PASS。修法＝裸号判据与链成员判据分家：与最近前方 hash 只隔链分隔符
        # （PAIR_SEP ∪ 非换行空白）的裸号是链成员（`#A+B`/`#A、B` 的 B），不触发守卫，
        # 照常配对（∩ issues ≠ ∅ 计入 / 全不相交进 foreign 拒收）
        if gate.fix_round_from_title("fix(scripts) #3056+3074 fix-r5：修闸门",
                                     frozenset({"3056"})) != 5:
            fails.append("fix-r8 concurrency P1：链尾裸号成员（#3056+3074 的 3074）不再整条丢弃，"
                         "前向配对照常计 5（对照 #3056+#3074 同判，行为不再分裂）")
        if gate.fix_round_from_title("fix(scripts) #3056、3074 fix-r5：修闸门",
                                     frozenset({"3056"})) != 5:
            fails.append("fix-r8 concurrency P1：全角顿号链尾裸号形（#3056、3074）同判链成员，计 5")
        if not gate._scan_pairs("fix(scripts) #3056+3074 fix-r5：修闸门",
                                frozenset({"9999"}))[1]:
            fails.append("fix-r8 concurrency P1：链尾裸号形配不上本 PR 单号时照进 foreign 拒收"
                         "（杜绝「既不计入也不拒收」的静默放行）")
        # 守卫另一半不回退：非链连接的裸号（中间隔了非链字符）仍是 board/交接段引用，
        # 不计、不进 foreign（fix-r4 P0-A / fix-r5 P1 原语义）——fix-r11 注记：该形
        # 整条无 #N，「不计不拒」是登记在案的板尾裸号窄窗（无配对链可挂 foreign），
        # 非前向臂；题头区内带链的散文间隔形由 fix-r11 矩阵 gap 内容轴接管
        if gate._scan_pairs("chore(state) 22:51 补落（2558 fix-r5 派 macmini）",
                            frozenset({"3056"})) != ([], []):
            fails.append("fix-r8 concurrency P1 pinning：非链连接的裸号（board 尾引用）仍不计不拒")
        # boundary#5（`）` 减法分支 pinning）：题头区「全角闭合括号在前 + 枚举链在后」的
        # board 形——配对全角括号（复盘）把深度收回 0，其后两条不相交链照常参与枚举
        # ⇒ foreign 抑制；把 `）→fw_pdepth-1` 减法整支删掉则 fw 停在 1、链退出枚举 ⇒
        # foreign 误报（假 FAIL 向），此负控必红
        if gate.foreign_fixround_pairs(
                ["fix(scripts)（复盘） #9999 修评审；#1321 收尾 fix-r5"], frozenset({"3056"})):
            fails.append("fix-r8 boundary#5：全角闭合括号在前的减法分支在案——配对（）后深度回 0，"
                         "枚举链照常参与并抑制 foreign（删减法支会假红）")

        # —— #3074 fix-r9（MM+DS r8 增量复核双撞 P1-1 + DS 新 P1-2，协调者墙钟/行为实测
        # 钉死 _tmp/20261114-3074-fixr9/task.md；尺子的 P1 不适用修复轮上限放行）——
        # P1-1（第二道豁免无界切片 = O(T·n) 挂起面）：`all(gap) for text[chain_end:bm.start(2)]`
        # 的切片长度不受 _BARE_WINDOW 约束、每 token 物化一次——协调者墙钟实测纯间隙对抗形
        # 17 KB 0.18 s / 134 KB 1.4 s / 0.87 MB 57.8 s（二次方外推 ~1.2 MB 越过本仓 r14
        # 先例 103.9 s），提交标题作者可控。修法＝_bare_member_connected 距链尾 ≤ 窗口上界
        # 先行、再做 ≤ 窗口的 gap 切片（fix-r10 起上界删除，改 _GapScan 单调指针精确
        # 判定——该上界本身在 gap=65 造出边界翻转 P0，见 fix-r10 块；本成本钉保留）。
        # **成本钉**（防回归无人看守）：对抗形 146 KB /
        # 1000 token 修后实测 ~10 ms（本机 2026-11-14，修前同形 3.6 s）；阈值取 1.0 s——
        # 修前该形必红（3.6 s > 1.0 s），修后留 ~100 倍余量（CI 慢机也够）
        _adv = '#1' + '+' * 134_000 + ''.join(f'+{i} fix-r{i % 9 + 1} ' for i in range(1000))
        _t0 = time.perf_counter()
        gate.fix_round_from_title(_adv)
        _dt = time.perf_counter() - _t0
        if _dt > 1.0:
            fails.append(f"fix-r9 P1-1 成本钉：纯间隙对抗形（146 KB/1000 token）{_dt:.2f} s > 1.0 s"
                         "——第二道豁免的逐 token 无界扫描回潮（应 _GapScan 摊还 O(1)，修前同形 3.6 s）")
        # P1-2（简写形链尾裸号仍整条丢弃）：`#3056+3074 r5：`（issues={3056}）修前
        # rounds=[] foreign=[] fix_round=None（义务整段跳过＝假 PASS），而全拼形
        # `#3056+3074 fix-r5` 计 5、`#3056+#3074 r5` 也计 5（B 带 # 时它自己就是最近
        # 前方 hash）——同链三种拼写行为分裂。修法＝RSHORT 收集臂对链尾裸号成员应用
        # 同一豁免口径（单源 helper _bare_member_connected，两臂共用）
        for _t in ("fix(scripts) #3056+3074 r5：修闸门",
                   "fix(scripts) #3056+#3074 r5：修闸门",
                   "fix(scripts) #3056、3074 r5：修闸门"):
            if gate.fix_round_from_title(_t, frozenset({"3056"})) != 5:
                fails.append(f"fix-r9 P1-2：简写形链尾裸号成员不再整条丢弃——{_t!r} 应计 5"
                             "（与全拼形 #3056+3074 fix-r5 同判，三种拼写不再分裂）")
        if not gate._scan_pairs("fix(scripts) #3056+3074 r5：修闸门",
                                frozenset({"9999"}))[1]:
            fails.append("fix-r9 P1-2：简写形链尾裸号配不上本 PR 单号时照进 foreign 拒收"
                         "（豁免只解除「整条丢弃」，不放宽配对）")
        # P2-2（MM simplify#2）：窄窗「裸号成员不并进配对集」反向用例——issues={3074}
        # 时 `#3056+3074 fix-r5` 的链集仍是 {3056}（HASH_REF 只收带 # 的号）→ 进
        # foreign 拒收，而 `#3056+#3074` 计 5（裸号拼写更严＝fail-closed 向，登记在案）
        if not gate._scan_pairs("fix(scripts) #3056+3074 fix-r5：修闸门",
                                frozenset({"3074"}))[1]:
            fails.append("fix-r9 P2-2：窄窗反向用例——issues={3074} 时裸号成员形进 foreign 拒收"
                         "（链集不含 3074），与 #3056+#3074（计 5）行为差＝登记的 fail-closed 窄窗")
        if gate._scan_pairs("fix(scripts) #3056+#3074 fix-r5：修闸门",
                            frozenset({"3074"})) != ([5], []):
            fails.append("fix-r9 P2-2 对照：#3056+#3074（B 带 #，链集 {3056,3074}）在 issues="
                         "{3074} 时照常计 5（窄窗只属于裸号拼写）")
        # P2-3（MM sibling#1）：半角 `)` 减法支孪生负控——fix-r8 已钉全角（）形，
        # 这里补半角 () 形：配对括号后深度回 0，其后两条不相交链照常参与枚举 ⇒ 抑制
        # foreign；把 `)→hw_pdepth-1` 减法整支删掉则 hw 停在 1、链退出枚举 ⇒ 误报
        if gate.foreign_fixround_pairs(
                ["fix(scripts)(复盘) #9999 修评审；#1321 收尾 fix-r5"], frozenset({"3056"})):
            fails.append("fix-r9 P2-3：半角闭合括号在前的减法分支在案——配对()后深度回 0，"
                         "枚举链照常参与并抑制 foreign（删半角减法支会假红）")
        # P2-5（MM sibling#3 + boundary#4，登记路线 pinning）：锚点第三族（rN+冒号）的
        # 过宽面——散文 `r4:` 早于首锚点（#N/fix-rN）时 enum_end 被切到 rN 的冒号处，
        # 其后链退出枚举参与集 → 合法 board/交接段标题 foreign 误报（假 FAIL 向＝多拒
        # 不漏）。**登记不改标准**（真实语料 board 段 rN: 早于 #N 的实例 0，见
        # _enum_title docstring fix-r9 段）；此断言钉住当前行为——将来改动须带 A/B 复算
        if not gate._scan_pairs("chore(state) r4: 01:10 #1305 fix-r3 交付；#1318 缺第二家",
                                frozenset({"3056"}))[1]:
            fails.append("fix-r9 P2-5 pinning：r4: 早于首锚点形当前进 foreign（登记的过宽面，"
                         "fail-closed 向）——行为若变须过真实语料复算，不得静默翻转")
        # P2-1（MM simplify#1 ≡ DS simplify#1 ≡ MM sibling#5）：fix_round_from_body 默认
        # echo_scan 改 True——生产唯一调用点（check()）恒 True，默认 False 只让直调走
        # 非生产路径（孪生负控 :1365 同步对齐）。显式 False 保留（首行语义单测用）
        if gate.fix_round_from_body("## 概述\n\nfix(scripts): #3074 fix-r3：x\n\nRefs #3074\n",
                                    issues=frozenset({"3074"})) != 3:
            fails.append("fix-r9 P2-1：echo_scan 默认 True——不带参数的直调即走生产回显扫描路径"
                         "（#3074 配对计入 3），默认回退 False 会复活「直调≠生产」分家")
        if gate.fix_round_from_body("## 概述\n\nfix(scripts): #3074 fix-r3：x\n\nRefs #3074\n",
                                    echo_scan=False, issues=frozenset({"3074"})) is not None:
            fails.append("fix-r9 P2-1：显式 echo_scan=False 仍可关（首行语义单测的关闭阀）")

        # —— #3074 fix-r10（DS r9 复审 P0 边界翻转，矩阵化修复——同处第三轮，规则强制矩阵；
        # 协调者实测 _tmp/20261114-3074-fixr10/p0-repro-before.txt：'+'*64 计 5、'+'*65 全丢）——
        # P0 根：fix-r9 给第二道豁免加的「距链尾 ≤ _BARE_WINDOW」上界在 gap=65 时把链尾裸号
        # 成员误判为引用 → 豁免被否 → 整 token 丢弃（rounds=[] foreign=[]＝义务整段跳过＝
        # 假 PASS），方向与 r9 docstring「fail-closed〔多拒不漏〕」相反（丢弃恰是 fail-open）。
        # 修法（拍板方案①）：_GapScan 单调指针摊还 O(1) **精确判定**（无截断无放弃）+
        # 豁免被否走正常配对/foreign（消灭「不计不拒」双空路径）+ 链构建/RSHORT 收集/
        # 前向配对三臂同源判据。P0 形直钉：
        if gate.fix_round_from_title("fix(scripts) #3056+" + "+" * 65 + "3074 fix-r5：修闸门",
                                     frozenset({"3056"})) != 5:
            fails.append("fix-r10 P0：gap=65 的链尾裸号成员（'+'*65）不再整 token 丢弃——"
                         "精确判定无距离上界，照常计 5（修前 rounds=[] foreign=[]＝假 PASS）")
        # 全矩阵负控（任务书硬判据）：拼写（fix-r5 / r5）× 链形（B 带 # / 裸号 + / 裸号 、）
        # × gap 长度（跨边界两侧）× issues；**任何格子不得 rounds=[] ∧ foreign=[]**（双空＝
        # 义务整段跳过＝假 PASS，全矩阵不变式——谁再引入「豁免被否 → 丢弃」即红）。
        # 断言两族：计入族（链∩issues≠∅ → rounds 有 5）；foreign 族（外来单/链集不含 →
        # foreign 拒收）。已知窄窗（登记）：裸号成员不并进配对集——`#3056+3074`+issues={3074}
        # → foreign 拒收而 `#3056+#3074` 计 5，两格允许结果不同，但都不允许双空。
        _gaps = (0, 1, 63, 64, 65, 66, 130, 131, 1000)
        _forms = (("+", "#3074"), ("+", "3074"), ("、", "3074"))
        _mat_bad: list[str] = []
        _mat_rows = 0
        for _sep, _second in _forms:
            _hashed = _second.startswith("#")
            _chain = frozenset({"3056", "3074"}) if _hashed else frozenset({"3056"})
            for _g in _gaps:
                _gap = _sep * _g if _g else " "
                for _tok in ("fix-r5", "r5"):
                    _title = f"fix(scripts) #3056{_gap}{_second} {_tok}：修闸门"
                    for _iss in (frozenset({"3056"}), frozenset({"3074"}),
                                 frozenset({"9999"})):
                        _rs, _fo = gate._scan_pairs(_title, _iss)
                        _mat_rows += 1
                        if not _rs and not _fo:
                            _mat_bad.append(f"双空：{_title!r} issues={sorted(_iss)}")
                        if not _chain.isdisjoint(_iss):
                            if 5 not in _rs:
                                _mat_bad.append(f"计入族缺 5：{_title!r} issues={sorted(_iss)} → {_rs}")
                        else:
                            if _rs:
                                _mat_bad.append(f"不应计入：{_title!r} issues={sorted(_iss)} → {_rs}")
                            if not _fo:
                                _mat_bad.append(f"foreign 族缺拒收：{_title!r} issues={sorted(_iss)}")
        if _mat_bad:
            fails.append(f"fix-r10 矩阵：{_mat_rows} 格中 {len(_mat_bad)} 失败（前 6）：{_mat_bad[:6]}")
        # 计时钉（任务书：146 KB < 1.0 s 保留——精确判定摊还后不得回退）：全拼形对抗串
        _adv10 = '#1' + '+' * 134_000 + ''.join(f'+{i} fix-r{i % 9 + 1} ' for i in range(1000))
        _t10 = time.perf_counter()
        gate.fix_round_from_title(_adv10)
        _dt10 = time.perf_counter() - _t10
        if _dt10 > 1.0:
            fails.append(f"fix-r10 计时钉：纯间隙对抗形（146 KB/1000 token）{_dt10:.2f} s > 1.0 s"
                         "——精确判定回潮为逐 token 无界扫（应 _GapScan 摊还 O(1)）")
        # 计时钉（MM concurrency#3）：RSHORT 收集臂路径——fix-r9/r10 的对抗串全形如 fix-rN
        # 时 RSHORT lookbehind（r 前邻是 '-'）全不匹配，收集臂豁免路径从未被计时看守；
        # 含简写 token + 逐 token 裸号豁免调用的形（+i rN），与 :1504-1513 既有 2.0 s
        # 计时钉口径一致
        _t11 = time.perf_counter()
        gate.fix_round_from_title("fix(x) #1" + "+" * 134_000
                                  + "".join(f"+{i} r{i % 9 + 1} " for i in range(1000)))
        _dt11 = time.perf_counter() - _t11
        if _dt11 > 2.0:
            fails.append(f"fix-r10 RSHORT 计时钉：含简写 token 的纯间隙对抗形 {_dt11:.2f} s > 2.0 s"
                         "——收集臂裸号成员豁免的精确判定回潮为逐 token 无界扫（应摊还 O(1)）")
        # 前向臂「豁免被否走 foreign」正控（fix-r5 P1 假 FAIL 防护保留 + 双空消灭）：
        # 题头区内非链连接裸号（gap 含非链字符）+ 链 ∩ issues = ∅ → 照进 foreign 拒收
        # （旧口径 rounds=[] foreign=[]＝假 PASS）；计入半边照旧被压（{2957} 时不计 2）
        if gate._scan_pairs("fix(scripts) #2957 P1×3 + 2932 fix-r2 交付",
                            frozenset({"9999"}))[1] != [(2, frozenset({"2957"}))]:
            fails.append("fix-r10 前向臂：题头区内裸号引用 + 链∩issues=∅ 照进 foreign 拒收"
                         "（「不计不拒」双空路径消灭——P0 洞根）")
        if gate.fix_round_from_title("fix(scripts) #2957 P1×3 + 2932 fix-r2 交付",
                                     frozenset({"2957"})) is not None:
            fails.append("fix-r10 前向臂：裸号引用不计入（fix-r4 P0-A / fix-r5 P1 假 FAIL 防护不回退）")
        # P2（DS sibling 补跑 P2-1）：_overflow_err b 后缀分流文案钉住——「#」族不含
        # 「b 后缀」、「fix-r」族含「可带 b 后缀」（口径 scripts/README.md:99）
        _e1 = gate._overflow_err("#" + "1" * 19, "t")
        _e2 = gate._overflow_err("fix-r" + "1" * 19, "t")
        if _e1 is None or "b 后缀" in _e1:
            fails.append("fix-r10 P2-1：# 族的超长串报错不应提「b 后缀」（HASH_OVER 无 b 后缀）")
        if _e2 is None or "可带 b 后缀" not in _e2:
            fails.append("fix-r10 P2-1：fix-r 族的超长串报错应含「可带 b 后缀」")

        # —— #3074 fix-r11（MM+DS r10 复审 P1×2 同根 + MM sibling#3 b 档升 P1：散文
        # 间隔维度的双空残留——fix-r10 只封了「纯间隙 + 前向臂」；协调者实测 before
        # _tmp/20261114-3074-p1-verify/probe.md 三形全 rounds=[] foreign=[]）——
        # P1-1 前向臂相交半边：`#3056 P1×3 + 2932 fix-r2` + {3056}（before 双空；
        # 纯 gap 形计 2——插一个词即义务整段跳过＝fail-open 拼写分裂）
        if gate._scan_pairs("fix(scripts) #3056 P1×3 + 2932 fix-r2 交付",
                            frozenset({"3056"})) != ([], [(2, frozenset({"3056"}))]):
            fails.append("fix-r11 P1-1：散文间隔前向臂相交半边照进 foreign 拒收（修前双空＝假 PASS）")
        if gate.fix_round_from_title("fix(scripts) #3056 P1×3 + 2932 fix-r2 交付",
                                     frozenset({"3056"})) is not None:
            fails.append("fix-r11 P1-1：裸号引用不计入半边保持（fix-r5 P1 假 FAIL 防护不回退）")
        # P1-2 收集臂简写形：`#3056+3074 补 r5：` + {3056}（before 双空；全拼
        # `补 fix-r5` 计 5——拼写分裂）
        if gate._scan_pairs("fix(scripts) #3056+3074 补 r5：修闸门",
                            frozenset({"3056"})) != ([], [(5, frozenset({"3056"}))]):
            fails.append("fix-r11 P1-2：散文间隔简写形照进 foreign 拒收（修前整条丢弃＝双空）")
        if gate.fix_round_from_title("fix(scripts) #3056+3074 补 fix-r5：修闸门",
                                     frozenset({"3056"})) != 5:
            fails.append("fix-r11 P1-2 对照：全拼散文间隔形照计 5（分裂只剩计入/拒收之差，无双空）")
        # P1-3 后向臂镜像（MM sibling#3，b 档判据＝假 PASS 计 P1）：两半边
        # （before：{9999} 与 {2957} 均 rounds=[] foreign=[]——后向臂从不 append foreign）
        for _iv in (frozenset({"9999"}), frozenset({"2957"})):
            _rs, _fo = gate._scan_pairs("fix(scripts) +2932 fix-r2 交付 + #2957 P1×3", _iv)
            if _rs or _fo != [(2, frozenset({"2957"}))]:
                fails.append(f"fix-r11 P1-3：后向臂两半边照进 foreign 拒收"
                             f"（issues={sorted(_iv)}，修前双空）→ {_rs}/{_fo}")
        if gate.fix_round_from_title("fix(scripts) fix-r2 交付 + #2957 P1×3",
                                     frozenset({"2957"})) != 2:
            fails.append("fix-r11 P1-3 正控：无裸号的后向配对照常计 2（DS boundary#5 语义不回退）")
        # P1-1 e2e（check() 报错文案对齐的回归钉）：相交半边的 foreign 拒收文案不得再写
        # 「引用不在本 PR 关联单」（链就在关联单里——假指引会叫人补已存在的 Refs），
        # 改报「归属存疑」并给标题侧修法；异单/no-issue 两分支照旧（fix-r5 e2e 族已钉）
        subprocess.run([*g2, "checkout", "-q", "-b", "fixr11-intersect-msg"], check=True)
        h_i11 = titled2("fix(scripts) #3056 P1×3 + 2932 fix-r2 交付")
        subprocess.run([*g2, "checkout", "-q", "-"], check=True)
        _errs11 = gate2("## 概述\n\n相交半边文案回归钉。\n\n" + blk_t.format(h=h_i11)
                        + "Closes #3056\n", h_i11, c_base)
        _ei11 = [e for e in _errs11 if "归属存疑" in e and "r2" in e and "#3056" in e]
        # 负条件只抓自相矛盾形（链 #3056 就在关联单 {3056} 里却被说「不在」）；范围里
        # 其他提交的 #3074 异单旁路报错（链 ∩ issues = ∅）是既有分支的正当输出，不算
        if not _ei11 or any("题头区引用 #3056 不在本 PR 关联单（#3056）" in e for e in _errs11):
            fails.append(f"fix-r11 P1-1 e2e：相交半边 foreign 拒收应报「归属存疑」而非"
                         f"「不在本 PR 关联单」（假指引）：{_errs11[:3]}")
        # fix-r12 P1-2（b 案第三件）文案同步断言：相交半边建议改为「移出题头区 …或与
        # 链写紧邻」，不得再建议「全拼 fix-rN」——对本族（评审署名 + 简写轮号）＝假
        # 声明修复轮 N，N≥4 触发 residual 连锁假 FAIL
        if not any("移出题头区" in e for e in _errs11) or any("全拼 fix-r" in e for e in _errs11):
            fails.append("fix-r12 P1-2 e2e：相交半边文案应建议「移出题头区」，不得建议"
                        f"「全拼 fix-rN」（假指引）：{_errs11[:3]}")
        # 全矩阵扩展（任务书硬判据，同处第四轮补轴）：fix-r10 矩阵（拼写×链形×gap
        # 长度×issues）之上加 **gap 内容轴**（纯 PAIR_SEP 间隙 / 散文间隔）与 **臂轴**
        # （前向 / 收集 / 后向 / 后向简写各形）——不变式「任何格子不得 rounds=[] ∧
        # foreign=[]」全格成立（fix-r16 P2#2 登记例外：**异侧×不相交**格——token 在
        # 题头区内、后方链在全角「：」后且 ∩ issues=∅——自 r14 起即有意的双空（board
        # 尾各归各单，不计不拒），见本文件 fix-r15 P2#1「board 尾异侧不相交」钉与
        # fix-r14 P1 同侧守卫负控；该格不在本矩阵轴内——矩阵形均为单链同侧）；
        # **非枚举型标题下**散文间隔格一律 foreign 拒收（归属
        # 存疑＝fail-closed；enum 相交半边自 fix-r13 起同判拒收——见 fix-r13 块，
        # 本矩阵形均为单链）；纯间隙格照 fix-r10 口径（计入族 ∩≠∅ 计入 / foreign 族
        # ∩=∅ 拒收）。
        _m11_bad: list[str] = []
        _m11_rows = 0
        _arm_forms = (
            ("前向", lambda gc: f"fix(scripts) #3056{gc}2932 fix-r2 交付",
             frozenset({"3056"}), 2),
            ("收集", lambda gc: f"fix(scripts) #3056+3074{gc}r5：修闸门",
             frozenset({"3056"}), 5),
            ("后向", lambda gc: f"fix(scripts) 修{gc}2932 fix-r2 交付 + #2957 P1×3",
             frozenset({"2957"}), 2),
            # fix-r13 协调者追加（MM sibling#2）：简写后向臂——RSHORT token 前方无 #N
            # 链时旧版 i<0 整条丢弃（rounds=[]∧foreign=[] 双空＝拼写分裂：全拼同形走
            # 后向臂拒收）；修后以 ri=-1 进 entries 流经后向臂同口径评价（与全拼臂同判）
            ("后向简写", lambda gc: f"fix(scripts) 修{gc}2932 r2 交付 + #2957 P1×3",
             frozenset({"2957"}), 2),
        )
        for _arm, _mk, _chain, _num in _arm_forms:
            for _gname, _gc in (("纯间隙", " + "), ("散文间隔", " P1×3 + ")):
                _title = _mk(_gc)
                # 后向臂形自带裸号引用（guard 必响：后向守卫无 chain_end 豁免），
                # 纯间隙格也走 foreign；前向/收集的纯间隙格裸号是链尾成员（计入族）
                _expect_fo = (_arm in ("后向", "后向简写")) or (_gname == "散文间隔")
                for _iss in (_chain, frozenset({"9999"})):
                    _rs, _fo = gate._scan_pairs(_title, _iss)
                    _m11_rows += 1
                    if not _rs and not _fo:
                        _m11_bad.append(f"双空：{_arm}/{_gname}：{_title!r} issues={sorted(_iss)}")
                    if _expect_fo:
                        if _rs:
                            _m11_bad.append(f"拒收格不应计入：{_arm}/{_gname}：{_title!r} issues={sorted(_iss)} → {_rs}")
                        if not _fo or _fo[0][0] != _num:
                            _m11_bad.append(f"拒收格缺({_num})：{_arm}/{_gname}：{_title!r} issues={sorted(_iss)} → {_fo}")
                    elif not _chain.isdisjoint(_iss):
                        if _rs != [_num]:
                            _m11_bad.append(f"计入族缺 {_num}：{_arm}/{_gname}：{_title!r} issues={sorted(_iss)} → {_rs}")
                    else:
                        if _rs:
                            _m11_bad.append(f"不应计入：{_arm}/{_gname}：{_title!r} issues={sorted(_iss)} → {_rs}")
                        if not _fo or _fo[0][0] != _num:
                            _m11_bad.append(f"foreign 族缺拒收({_num})：{_arm}/{_gname}：{_title!r} issues={sorted(_iss)} → {_fo}")
        if _m11_bad:
            fails.append(f"fix-r11 矩阵：{_m11_rows} 格中 {len(_m11_bad)} 失败（前 6）：{_m11_bad[:6]}")
        # P2-4（MM concurrency#2 ≡ DS concurrency#1）：_GapScan 契约守卫——违约
        # fail-loud raise（静默 fallback 会把摊还破坏吞成暗坑）+ 基本语义负控
        if gate._GapScan("a b c").clear(0, 1):
            fails.append("fix-r11 P2-4：_GapScan.clear 基本语义（区间含非 gap 字符 → False）")
        _gs11 = gate._GapScan("+++")
        _gs11.clear(0, 3)
        try:
            _gs11.clear(0, 1)
            fails.append("fix-r11 P2-4：_GapScan.clear 契约违约应 raise（静默 fallback 回潮）")
        except RuntimeError:
            pass

        # —— #3074 fix-r12（MM+DS r11 复审 P1×2 + P2×2；同词汇位第 3 轮，memory
        # 「lexical-verdict-fix-nonconvergence」硬要求轴空间收口，RESULT 附全枚举表）——
        # P1-1（concurrency#1）enum 维度残留：收集臂在枚举型标题下（题头区内 ≥2 条互不
        # 相交 #N 链）对散文间隔简写 token 整条 continue 丢弃 → ([], []) → fix_round=None
        # → fixtype/N≥4 residual/多块一致三段义务整段跳过＝假 PASS；同结构全拼对照计 5
        # （before 实测 _tmp/20261114-3074-fixr12/probe-p1-before.out）
        # 【fix-r13 反转登记】enum 相交半边简写 token 自 fix-r13 起**有意改判 foreign
        # 拒收**（enum 门半边化，MM boundary#1 + DS sibling#1 双谱系双撞）——下两条钉随
        # 行为翻转更新，历史判据（计 5 / 登记抑制）见 git log 本块 fix-r12 版；不相交
        # 半边登记抑制（第三条）与硬不变式②不变。
        if gate._scan_pairs("fix(scripts) #3056+3074 补 r5 #2957 P1×3",
                            frozenset({"3056"})) != ([], [(5, frozenset({"3056"}))]):
            fails.append("fix-r13 P1-1（原 fix-r12 P1-1 触发形）：enum 相交半边散文间隔"
                        "简写照进 foreign 拒收、不再计入（fix-r12 曾计 5＝本修有意反转；"
                        "同结构全拼对照仍计 5——计入半边不回退见 ⑳ 钉）")
        if gate.fix_round_from_title("fix(scripts) #3056+3074 补 r5 #2957 P1×3",
                                     frozenset({"3056"})) is not None:
            fails.append("fix-r13 P1-1 e2e：enum 相交半边简写不再判修复轮（义务由 foreign "
                        "拒收承担；fix-r12 曾判 5＝本修有意反转）")
        # 不相交半边：enum 下 foreign=[] 是登记在案的抑制（硬不变式②，board 并记形态
        # 各归各单的保护；与全拼形同判——`_enum_title` docstring 窄窗表）
        if gate._scan_pairs("fix(scripts) #3056+3074 补 r5 #2957 P1×3",
                            frozenset({"9999"})) != ([], []):
            fails.append("fix-r12 P1-1：不相交半边 enum 下 foreign=[] 是登记在案的抑制"
                        "（非静默丢弃，board 形态保护不变——全拼对照同判）")
        # 裸号引用（含数字散文 `P1×3 + 2932`）断归属的相交半边 + enum：fix-r13 起照
        # 非枚举口径进 foreign 拒收（fix-r12 登记窄窗②废止——enum 门半边化；
        # 非枚举对照＝fix-r11 P1-1 钉）——防「登记了但行为静默漂移」；修法=移出
        # 题头区或与链写紧邻
        if gate._scan_pairs("fix(scripts) #3056 P1×3 + 2932 fix-r2 交付；#9991 P1×2",
                            frozenset({"3056"})) != ([], [(2, frozenset({"3056"}))]):
            fails.append("fix-r13 P1-1：裸号引用断归属的相交半边在 enum 下照进 foreign "
                        "拒收（fix-r12 登记窄窗②废止，与非枚举同判——见 fix-r11 P1-1 钉）")
        # P1-1 矩阵补 enum 轴（任务书硬判据）：4 臂 × 2 gap 内容 × 2 issues 相交性 ×
        # 2 enum（单链/枚举）＝ 32 格——非枚举半边＝上方 fix-r11 矩阵 16 格（裸号引用
        # 形态族），枚举半边在此（enum 判定用题头区 ≥2 互不相交链的真实形，如
        # `#3056+3074 … #2957 P1×3`；gap＝纯 PAIR_SEP/散文间隔两档，散文档不含裸号
        # 引用——裸号引用维度由 r11 矩阵 + 上方直钉 + _enum_title 登记窄窗覆盖）。
        # 硬不变式：① 归属完好（gap 无裸号引用）且 链∩issues≠∅ → rounds≠[]（**全拼
        # 臂**；简写收集臂散文间隔格自 fix-r13 起照非枚举口径 foreign 拒收——enum 门
        # 半边化）；② 不相交半边 enum 下 foreign=[] 是登记在案的抑制（非静默丢弃）。
        # 全矩阵直调 allow_unpaired=False＝生产非 TITLE_HEAD 口径（后向臂需它走后向
        # 分支而非 SELF 兜底；前向/收集臂不经 allow_unpaired 敏感分支，两侧同判）。
        # 后向简写臂（fix-r13 协调者追加 MM sibling#2）：`修补{gc}2932 r2 交付 +
        # #2957 P1×3；#9991`——r2 前方无 #N 链；旧版 i<0 整条丢弃（双空），修后以
        # ri=-1 进 entries 流经后向臂同口径评价。**gap 内容轴在此臂改变归属判定**
        # （BARE_NUM_BEFORE 只认枚举分隔前邻，fix-r4 P0-A 口径）：gc=纯间隙（`+ ` 前邻
        # 2932）→ 裸号引用断归属 → 相交 foreign 拒收；gc=散文（`补` 词符前邻）→ 非
        # 裸号引用 → 归属完好计入（与 sibling#2 正控·enum 钉成对）。
        _m12_bad: list[str] = []
        _m12_rows = 0
        _e12_forms = (
            ("前向", lambda gc: f"fix(scripts) #3056{gc}fix-r2 交付；#9991 P1×2",
             frozenset({"3056"}), 2),
            ("收集", lambda gc: f"fix(scripts) #3056+3074{gc}r5 修闸门 #2957 P1×3",
             frozenset({"3056"}), 5),
            ("后向", lambda gc: f"修补{gc}fix-r2 交付 + #2957 P1×3；#9991 P1×2",
             frozenset({"2957"}), 2),
            ("后向简写", lambda gc: f"修补{gc}2932 r2 交付 + #2957 P1×3；#9991 P1×2",
             frozenset({"2957"}), 2),
        )
        for _arm, _mk, _chain, _num in _e12_forms:
            for _gname, _gc in (("纯间隙", " + "), ("散文间隔", " 补 ")):
                _title = _mk(_gc)
                for _iss in (_chain, frozenset({"9999"})):
                    _rs, _fo = gate._scan_pairs(_title, _iss, allow_unpaired=False)
                    _m12_rows += 1
                    if not _chain.isdisjoint(_iss):
                        # fix-r13 相交半边二分：收集臂散文间隔（简写归属存疑）与后向
                        # 简写纯间隙（枚举分隔前邻裸号 → 断归属）→ foreign 拒收——
                        # 归属存疑不因 enum 而静默放行/计入（P1 半边化本体）；其余
                        # （全拼三臂 + 后向简写散文 `补` 词符前邻＝归属完好）→ 计入
                        _e13_reject = (
                            (_arm == "收集" and _gname == "散文间隔")
                            or (_arm == "后向简写" and _gname == "纯间隙"))
                        if _e13_reject:
                            # ①' enum 相交半边 + 归属存疑简写 → foreign 拒收
                            if _rs or _fo != [(_num, _chain)]:
                                _m12_bad.append(f"拒收格（enum 交）：{_arm}/{_gname}：{_title!r} "
                                                f"issues={sorted(_iss)} → {_rs}/{_fo}")
                        else:
                            # ① 归属完好 + 相交 → 计入（enum 只压 foreign 半边）
                            if _rs != [_num] or _fo:
                                _m12_bad.append(f"计入格（enum）：{_arm}/{_gname}：{_title!r} "
                                                f"issues={sorted(_iss)} → {_rs}/{_fo}")
                    else:
                        # ② 不相交半边 enum 下 ([], [])＝登记抑制（非静默丢弃）
                        if _rs or _fo:
                            _m12_bad.append(f"抑制格（enum）：{_arm}/{_gname}：{_title!r} "
                                            f"issues={sorted(_iss)} → {_rs}/{_fo}")
        if _m12_bad:
            fails.append(f"fix-r12 矩阵（enum 轴，fix-r13 半边化）：{_m12_rows} 格中 "
                         f"{len(_m12_bad)} 失败（前 6）：{_m12_bad[:6]}")
        # P2-3（MM boundary#3 ≡ DS boundary#1 ≡ simplify#1 三撞，fix-r13 随轮）：
        # 矩阵分母守卫由 fix-r12 P2-1 的「len(forms)×2×2 单源算」改**字面下界**——
        # 单源算在 forms 元组被删空时恒真（0 格 0 失败假绿），期望值与被测对象不得
        # 同源。字面量＝本轮各矩阵真实格数：r10 矩阵 3 链形×9 gap×2 拼写×3 issues
        # ＝162；r11 矩阵 4 臂×2 gap×2 issues＝16（fix-r13 加后向简写臂）；r12 enum
        # 矩阵同 16。轴扩展只许调大字面量（矩阵缩水即红）；另加轴元组非空断言
        # （空元组会让循环体静默零迭代——分母守卫之外的第二道假绿面）
        if _mat_rows < 162:
            fails.append(f"fix-r13 P2-3：fix-r10 矩阵格数 {_mat_rows} < 162——矩阵轴被删空"
                        "（字面下界，勿改回 len(forms) 单源算）")
        if _m11_rows < 16:
            fails.append(f"fix-r13 P2-3：fix-r11 矩阵格数 {_m11_rows} < 16——矩阵轴被删空")
        if _m12_rows < 16:
            fails.append(f"fix-r13 P2-3：fix-r12 enum 矩阵格数 {_m12_rows} < 16——矩阵轴被删空")
        if not (_arm_forms and _e12_forms and _forms and _gaps):
            fails.append("fix-r13 P2-3：矩阵轴元组非空断言——空轴元组＝循环静默零迭代")

        # —— #3074 fix-r13（MM+DS r12 复审 P1×2 同根「enum 门半边化」+ 协调者追加
        # MM sibling#2「收集臂 i<0 双空」；lexical-verdict-fix-nonconvergence 第 4 轮，
        # 轴空间收口见 RESULT 288 格表更新节；before 实测 _tmp/20261114-3074-fixr13/
        # probe-side-by-side.out）——
        # P1-1/P1-2 协调者四孪生直钉（修后四行全须为拒收形）：非枚举孪生＝r11/r12
        # 既有拒收（不回退面）；枚举孪生（加一条无关链）＝本修前 ([], []) 静默放行，
        # 修后照非枚举口径进 foreign；枚举孪生-署名族＝本修前 ([16], []) 假声明轮号
        # 计入（A/B 语料 6 实例），修后 foreign 拒收
        if gate._scan_pairs("fix(scripts) #3056 P1×3 fix-r2 交付",
                            frozenset({"3056"})) != ([], [(2, frozenset({"3056"}))]):
            fails.append("fix-r13 四孪生·非枚举：归属存疑照进 foreign 拒收（r11 口径不回退）")
        if gate._scan_pairs("fix(scripts) #3056 P1×3 fix-r2 交付；#9991 复盘",
                            frozenset({"3056"})) != ([], [(2, frozenset({"3056"}))]):
            fails.append("fix-r13 P1-1 四孪生·枚举：加一条无关链不再绕过拒收（修前 "
                        "([], [])＝静默放行，enum 相交半边照非枚举同判）")
        if gate._scan_pairs("fix(scripts) #3056 MM r16 评审意见修正；#9991 复盘",
                            frozenset({"3056"})) != ([], [(16, frozenset({"3056"}))]):
            fails.append("fix-r13 P1-2 四孪生·枚举署名族：enum 相交半边不再计入假声明"
                        "轮号（修前 ([16], [])，评审轮引用不是 fix 轮）")
        if gate._scan_pairs("fix(scripts) #3056 MM r16 评审意见修正",
                            frozenset({"3056"})) != ([], [(16, frozenset({"3056"}))]):
            fails.append("fix-r13 四孪生·非枚举署名族：r12「不计但拒收」钉不回退")
        # MM sibling#2（协调者追加，收集臂 i<0 双空）：简写 token 前方无 #N 链时旧版
        # 整条丢弃 → ([], []) 双空＝修复轮义务整段跳过；全拼同形走后向臂拒收＝简拼/
        # 全拼拼写分裂。修后 ri=-1 进 entries 流经后向臂同口径评价
        if gate._scan_pairs("fix(scripts) 修 + 2932 r2 交付 + #2957 P1×3",
                            frozenset({"2957"})) != ([], [(2, frozenset({"2957"}))]):
            fails.append("fix-r13 sibling#2：简写 token 前方无链不再整条丢弃（修前 "
                        "([], []) 双空＝拼写分裂；与全拼同形后向臂拒收同判）")
        if gate.fix_round_from_title("fix(scripts) 修 + 2932 r2 交付 + #2957 P1×3",
                                     frozenset({"2957"})) is not None:
            fails.append("fix-r13 sibling#2 e2e：双空形不得判修复轮（义务由 foreign 拒收承担）")
        if gate._scan_pairs("fix(scripts) 修 r2 交付 + #2957 P1×3",
                            frozenset({"2957"})) != ([2], []):
            fails.append("fix-r13 sibling#2 正控：简写后向臂归属完好（无裸号）照计入 2"
                        "（修前整条丢弃＝假 PASS；封堵不为拒收而拒收）")
        if gate._scan_pairs("修补 补 r2 交付 + #2957 P1×3；#9991 P1×2",
                            frozenset({"2957"}), allow_unpaired=False) != ([2], []):
            fails.append("fix-r13 sibling#2 正控·enum：简写后向臂归属完好在 enum 相交"
                        "半边照计入 2（半边化只压 foreign 半边，计入半边不回退）")
        # P2-5（DS sibling#3）⑳ 全拼 enum 对照钉：计入半边（全拼 token 不经收集臂
        # 守卫）在 enum 相交半边不回退——半边化只把简写归属存疑 token 从计入/静默
        # 放行改拒收，不压全拼合法计入
        if gate._scan_pairs("fix(scripts) #3056+3074 补 fix-r5 #2957 P1×3",
                            frozenset({"3056"})) != ([5], []):
            fails.append("fix-r13 P2-5：全拼 enum 相交半边照计入 5（enum 只作用于收集/"
                        "前向/后向臂的 foreign 判定，不压全拼计入半边）")
        # P2-6（DS boundary#3）收集臂 enum 不相交半边抑制钉：本修后抑制面缩小到
        # 不相交半边，钉住保留的抑制形（board 并记各归各单的保护——链 ∩ issues=∅
        # 时 board 形态不误报）
        if gate._scan_pairs("fix(scripts) #3056+3074 补 + 3074 r5 #2957 P1×3",
                            frozenset({"9999"})) != ([], []):
            fails.append("fix-r13 P2-6：不相交半边 enum 抑制保留（抑制面缩小后不回宽——"
                        "board 并记形态保护不变）")

        # —— #3074 fix-r14（r13 引入的 P0/P1 当轮修；协调者仲裁正本
        # _tmp/20261114-3074-fixr13-ds/coord-arbitration.md，三向探针
        # _tmp/20261114-3074-fixr14/probe-smoke.out）——
        # P0（正文回显兜底假 PASS）：enum 相交半边散文间隔简写 token 在正文路径既不
        # 计入也不拒收——fix_round_from_body 的 ECHO_HEAD 扫描只用 _paired_rounds
        # （丢 foreign），旧 :1431 拒收只吃 titles → fixtype / N≥4 residual / 多块一致
        # 义务整段跳过（r12 计 5，r13 退化 None 且无错）。修法＝check() 在兜底块对
        # 正文 ECHO_HEAD 行同跑 foreign_fixround_pairs（fail-closed 拒收）
        if gate.fix_round_from_body("## 概述\n\nfix(scripts) #3056+3074 补 r5 修闸门 #2957 P1×3\n",
                                   echo_scan=True, issues=frozenset({"3056"})) is not None:
            fails.append("fix-r14 P0 预置：归属存疑 token 的 _paired_rounds 半边为空（非空＝"
                         "钉错形或计入半边回退）")
        if not any("归属存疑" in e and "正文回显行" in e and "r5" in e
                   for e in gate.check(
                       "## 概述\n\nfix(scripts) #3056+3074 补 r5 修闸门 #2957 P1×3\n\nCloses #3056\n",
                       "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef", "", "", "")):
            fails.append("fix-r14 P0：正文回显兜底路径对归属存疑 token 须 fail-closed 拒收"
                         "（修前 None 且无错＝假 PASS；验收钉「非 None 或被 check() 拒收」）")
        if gate.fix_round_from_body("## 概述\n\nfix(scripts) #3056+3074 补 fix-r5 修闸门 #2957 P1×3\n",
                                   echo_scan=True, issues=frozenset({"3056"})) != 5:
            fails.append("fix-r14 P0 对照：全拼 token 不经收集臂守卫，正文回显照计 5（r12 基线）")
        # P1（无链前导简写 SELF 假 FAIL 面）：r13 删 i<0: continue 后题头区前导简写
        # rN 无 #N 链也进 SELF 计入 → 非修复轮标题被判修复轮（义务误触发）。修法＝
        # i<0 前导简写不 SELF 也不走 not hashes 兜底，交后向臂（要求后方真实链且
        # 与 token 同侧于题头区界）
        if gate._scan_pairs("chore(state) r4 01:10 #1305 fix-r3 交付；#1318 缺第二家",
                            frozenset({"3056"})) != ([], []):
            fails.append("fix-r14 P1：无链前导简写回到不计不拒（修前 rounds=[4]＝假 FAIL 面；"
                         "r8 pinning 只断言 foreign 空、rounds 静默未测——本钉补全 ([], [])）")
        if gate._scan_pairs("chore(board): 12:15 /loop——pr9 r9 收判", frozenset()) != ([], []):
            fails.append("fix-r14 P1：散文无链简写不计（修前 rounds=[9]）")
        if gate._scan_pairs("chore(channels): r4 channel-layer fixes before Codex runtime merge",
                            frozenset()) != ([], []):
            fails.append("fix-r14 P1：无链前导简写整族不计（修前 rounds=[4]）")
        # 后向臂同侧守卫负控：token 在题头区内、后方 #N 在首个全角「：」之后（题头
        # 区外）→ 不计不拒——守卫缺失时与区外链失配误进 foreign ＝ 新假 FAIL 面
        # （仲裁 sugg1-battery case 2 实测发现，DS 未预见）
        if gate._scan_pairs("chore(state) board 12:01 r2：coord 11:4x 增量——#2040 合入计 9 笔 + #2123 入队 + READY 增 #2073/#2126/#2137",
                            frozenset()) != ([], []):
            fails.append("fix-r14 P1 同侧守卫：board 尾形 token 与后方链异侧于题头区界，不计"
                         "不拒（守卫缺失时 foreign=[(2,{2040})]＝新假 FAIL）")
        # r13 主修面不回退（仲裁「不得回退 r13 主修面」逐形钉）
        if gate._scan_pairs("fix(scripts) 修 + 2932 r2 交付 + #2957 P1×3",
                            frozenset({"2957"})) != ([], [(2, frozenset({"2957"}))]):
            fails.append("fix-r14 不回退 r13：sibling#2 触发形（无链简写+后向链）仍拒收")
        if gate.fix_round_from_title("fix(gateway) 修 + r5 + #3074",
                                     frozenset({"3074"})) != 5:
            fails.append("fix-r14 不回退 r13：后向简拼+本单链计入 5（r13 增益保留）")
        # 窄窗登记钉（P1 取舍）：无链简拼真修复轮回到不计——简拼/全拼 no-chain 分裂
        # 复开是「无链前导简写不计」的必然取舍，_scan_pairs docstring ㉓ 已登记；
        # 修法是写全拼或补 #N 链
        if gate.fix_round_from_title("chore(gateway) upload 清理 r6：仓库根 _tmp 清出 3.2 GB",
                                     frozenset()) is not None \
                or gate.fix_round_from_title("chore(gateway) upload 清理 fix-r6：仓库根 _tmp 清出 3.2 GB",
                                             frozenset()) != 6:
            fails.append("fix-r14 窄窗登记：无链简拼不计 / 全拼对照计 6（拼写分裂为登记取舍）")
        # P3#3 登记钉（非 TITLE_HEAD 散文标题拒收面，DS ds2 boundary#3）：保持 r13 判
        # （有意 fail-closed，窄窗已登记 foreign_fixround_pairs docstring），钉住不漂移
        if gate._scan_pairs("交接段、123 r5 交付 + #2957 P1×3",
                            frozenset({"9999"})) != ([], [(5, frozenset({"2957"}))]):
            fails.append("fix-r14 P3#3 登记钉：非修复轮散文标题拒收面保持 r13 判不漂移")
        # fix-r14 A/B（wordlist-ab-r14.txt r14新面/r14改值桶）逐条核实后登记的三条窄窗
        # （P3#3 同族：散文 rN + 后方链——链配到本单则计入＝r13 增益保留，链异单则拒收
        # ＝有意 fail-closed；语料 10446 标题中 3 条，近 30 真实 MERGED PR 0 触发）
        if gate._scan_pairs("r0 #2562 submit 路径 SubmitHostServices::workspace_id 的 projectKey 改 nullish 双臂（附 #2564 三格请求级 null 用例与负控绑定）",
                            frozenset()) != ([], [(0, frozenset({"2562"}))]):
            fails.append("fix-r14 窄窗登记·r0形：散文 r0+后方异单链拒收（钉住不漂移）")
        if gate._scan_pairs("chore(board): 12:15 /loop——pr9 r9 三家同撞 P1 已修（7fcc773）、pr10b 修复轮 8 提交待验、#386 停在选型转人批（决策卡）",
                            frozenset()) != ([], [(9, frozenset({"386"}))]):
            fails.append("fix-r14 窄窗登记·board散文Ⅰ：散文 rN+后方异单链拒收（钉住不漂移）")
        if gate._scan_pairs("chore(board): 15:48——pr10b r10 / gate9；#407 修复轮 1 提交",
                            frozenset()) != ([], [(10, frozenset({"407"}))]):
            fails.append("fix-r14 窄窗登记·board散文Ⅱ：散文 rN+后方异单链拒收（钉住不漂移）")

        # —— #3074 fix-r15（DS r14 增量评审 P1 + P2×3；P1 协调者实测仲裁
        # _tmp/20261114-3074-fixr15/p1-probe.txt，修前现场 probe-before.out）——
        # P1 原口径「首行命中即跳过正文回显 foreign 扫描」自 fix-r16 起收窄为声明形、
        # 自 fix-r17 起再限定标题路径存在（见 check() fix-r17 段）：本钉的 body 是
        # **本地形态**（gate.check(…, "", "", "", "")，titles=None），fix-r17 起声明形
        # 首行也不跳过——回显行是唯一异单出口，照 r14 拒收（原钉的是 fail-open 侧，
        # r17 主修后翻转）；首行命中的修复轮义务照走（fixtype/residual 仍由首行轮号
        # 触发）。首行干净 + 回显行引异单的对照半边由上方 fix-r14 P0 扩面钉
        # （nohead_neg）钉住，两向都不漂移。
        p1_body = ("修复fix-r5：停泊补 waker\n\nfix(scripts): #3074 fix-r3：x\n\n"
                   + blk_t.format(h="") + "Refs #1\n")
        p1_errs = gate.check(p1_body, "", "", "", "")
        if not any("异单旁路" in e and "正文回显行" in e for e in p1_errs):
            fails.append(f"fix-r17 翻转（原 fix-r15 P1 钉）：本地形态（titles=None）声明形"
                         f"首行不得跳过正文回显 foreign 扫描——回显行是唯一异单出口"
                         f"（fix-r17 主修面）：{p1_errs}")
        if not any("fix-r5" in e and "fixtype" in e for e in p1_errs) \
                or not any("pdrs-residual" in e for e in p1_errs):
            fails.append(f"fix-r15 P1：首行命中的修复轮义务照走（fixtype+residual 仍触发）：{p1_errs}")
        # P2#1：同侧守卫只压 foreign 半边——链异侧且与 issues 不相交才不计不拒
        # （board 尾各归各单，r14 语义保留）；异侧相交（SELF 同形真修复轮）照后向配对
        # 计入（修前 ([],[]) → fix_round=None → 义务整段跳过＝假 PASS）；异侧相交但裸号
        # 断归属 → 归属存疑拒收（三臂口径不变）；board 尾链恰为本单计入＝登记残余代价
        # （A/B 复算量化，wordlist-ab-r15.txt）。
        if gate._scan_pairs("fix(scripts) 修闸门 r5：#3056", frozenset({"3056"})) != ([5], []):
            fails.append("fix-r15 P2#1：SELF 同形真修复轮（异侧相交）须计入 5（修前 ([],[])＝假 PASS）")
        if gate.fix_round_from_title("fix(scripts) 修闸门 r5：#3056", frozenset({"3056"})) != 5:
            fails.append("fix-r15 P2#1：同形整链判修复轮 5（义务不再整段跳过）")
        if gate._scan_pairs("chore(state) board 12:01 r2：coord 11:4x 增量——#2040 合入计 9 笔 + #2123 入队",
                            frozenset({"1"})) != ([], []):
            fails.append("fix-r15 P2#1：board 尾异侧不相交保持不计不拒（r14 语义不回退）")
        if gate._scan_pairs("chore(state) board 12:01 r2：coord 11:4x 增量——#2040 合入计 9 笔 + #2123 入队",
                            frozenset({"2040"})) != ([2], []):
            fails.append("fix-r15 P2#1 残余代价钉：board 尾链恰为本单计入 2（登记取舍，A/B 量化）")
        if gate._scan_pairs("fix(scripts) 修 + 2932 r5 交付：#3056", frozenset({"3056"})) \
                != ([], [(5, frozenset({"3056"}))]):
            fails.append("fix-r15 P2#1：异侧相交但裸号断归属 → 归属存疑拒收（三臂口径不变）")
        # P2#2：全拼 SELF 假 FAIL 拼写分裂**登记不收口**（订正 r14「全拼不经本守卫…自然
        # 同侧」的不成立注释；收口尝试经 A/B 证伪回退——closure-reject-proof.txt 静默
        # 44 条真修复轮：括注形 `fix(scope) 名 fix-rN：…（#refs 在后）` 32 + 散文引单形
        # 12，语料 0 条落 board 散文窗）——board 散文全拼「token 区内、链区外不相交」
        # SELF 照计＝登记窄窗（假 FAIL 向，r14 语义原样）；全拼 SELF 实修形相交/不相交
        # 都照计（收口必须先过「标题静默回归 0」A/B，不得无复算重引入）。
        if gate._scan_pairs("chore(state) board 12:01 fix-r2：coord …——#2040 合入计 9 笔",
                            frozenset({"1"})) != ([2], []):
            fails.append("fix-r15 P2#2 登记钉：board 散文全拼异侧不相交 SELF 照计 2"
                         "（登记窄窗，收口被 A/B 证伪回退——closure-reject-proof.txt）")
        if gate._scan_pairs("chore(state) board 12:01 fix-r2：coord …——#2040 合入计 9 笔",
                            frozenset({"2040"})) != ([2], []):
            fails.append("fix-r15 P2#2：board 散文全拼异侧相交照计 2（r14 语义不回退）")
        if gate._scan_pairs("fix(scripts) 修闸门 fix-r5：#3074", frozenset({"3074"})) != ([5], []) \
                or gate._scan_pairs("fix(scripts) 修闸门 fix-r5：#3074", frozenset({"1"})) != ([5], []):
            fails.append("fix-r15 P2#2：全拼 SELF 实修形（括注 refs）相交/不相交都照计 5"
                         "（收口须先过静默回归 0 的 A/B 复算）")
        # P2#3：窄窗登记文字订正为实际判据（题头区内无前链简写 + 同侧后方链不相交 →
        # 拒收，与类型词/裸号无关）——行为面即上方 fix-r14 四条钉（不漂移，无新钉）；
        # docstring 侧由 check-naming/评审复核覆盖。

        # —— #3074 fix-r16（DS r15 boundary#1 P0 + MM boundary#1 P1 双谱系双撞同根，
        # 协调者实测仲裁 _tmp/20261114-3074-fixr15-ds/p0-p1-arbitration.md，探针
        # p0-p1-probe.py r14/r15 两版对照；修法＝首行早退收窄为声明形，判据与两向
        # 窄窗登记见 _first_line_declares docstring）——
        # P0 双向负控钉·场景 A（首行＝全拼异单回显行 + Refs #1）→ 必须拒收：修前
        # r15 首行命中即整体跳过＝吞掉 r14 的异单拒收（假 PASS）；与上方 nohead_neg
        # （首行 badge 图、非回显行）成「首行即回显行 / 首行非回显」对照，两向都不漂移
        sc_a = ("fix(scripts): #3074 fix-r3：修闸门\n\n"
                + blk_t.format(h="") + "Refs #1\n")
        sc_a_errs = gate.check(sc_a, "", "", "", "")
        if not any("异单旁路" in e and "正文回显行" in e for e in sc_a_errs):
            fails.append(f"fix-r16 P0 场景A：首行即回显行（引异单）须拒收——首行是唯一"
                         f" foreign 出口，整体跳过＝吞掉 r14 拒收（修前 r15 假 PASS）：{sc_a_errs}")
        # P1 场景 B（首行散文提及 + 后文异单回显行 + Refs #1）→ 修后仍拒收（r14 拒收
        # 是 desirable：异单引用本应拒；r15 的「首行任意位置命中即跳过」把 fix-r4 在
        # rounds 半边钉死的宽松首行（误拒，fail-closed）搬到 foreign 半边后方向反转）
        sc_b = ("承接 fix-r3 未完事项：后续处理\n\nfix(scripts): #9999 fix-r7：x\n\n"
                + blk_t.format(h="") + "Refs #1\n")
        sc_b_errs = gate.check(sc_b, "", "", "", "")
        if not any("异单旁路" in e and "正文回显行" in e and "r7" in e for e in sc_b_errs):
            fails.append(f"fix-r16 P1 场景B：首行散文提及不得触发整体跳过，回显行引异单"
                         f"须照 r14 拒收（fail-closed）：{sc_b_errs}")
        # 判据直调钉（三分支 + 窄窗两向，钉住方向不漂移）：声明形跳过（r15 面在 CI
        # 形态保持——「声明形 + 异单回显 → CI 跳过不拒」的 check() 级钉在下方
        # fix-r17 P1(b)；本地形态自 fix-r17 起照跑＝上方翻转后的 p1_body 钉）；提及
        # 形 / 回显行不跳过；窄窗第二向（提及紧跟冒号＝漏检向）钉在下方 fix-r17 块补齐
        if not gate._first_line_declares("修复fix-r5：停泊补 waker\n\nx\n"):
            fails.append("fix-r16 判据：紧跟全角冒号＝声明形（跳过，r15 面保持）")
        if not gate._first_line_declares("fix-r3: 修闸门\n\nx\n"):
            fails.append("fix-r16 判据：紧跟半角冒号同判声明形（`r5: ` 简写锚点同族）")
        if not gate._first_line_declares("承接 fix-r3，本轮修 fix-r5：停泊补 waker\n\nx\n"):
            fails.append("fix-r16 判据：行内任一 token 紧跟冒号即声明形（首个命中是"
                         "提及不否决后面的声明）")
        if gate._first_line_declares("承接 fix-r3 未完事项：后续处理\n\nx\n"):
            fails.append("fix-r16 判据：散文提及（token 后随空白）不跳过（场景 B 根）")
        if gate._first_line_declares("fix(scripts): #3074 fix-r3：修闸门\n\nx\n"):
            fails.append("fix-r16 判据：首行是 ECHO_HEAD 回显行不跳过（P0 场景 A 根——"
                         "首行自身在 _echo_lines 内被扫）")
        if gate._first_line_declares("修复fix-r5 停泊补 waker\n\nx\n"):
            fails.append("fix-r16 判据窄窗：声明形不紧跟冒号按提及处理（照跑扫描＝"
                         "fail-closed 向登记，见 _first_line_declares docstring）")
        # P2#1 文档↔行为同源断言：fix_round_from_title docstring（判据陈述点⑨）须与
        # 行为同源——异侧相交形返回轮号（fix-r15 P2#1 主修面）；r15 改「异侧相交照
        # 计入」时漏改 3 处陈述点（收集臂内联 / SELF 支内联 / 本 docstring），照旧文
        # 「链异侧不计」读回会重开 rounds=[]∧foreign=[] 假 PASS 面
        if "异侧相交" not in (gate.fix_round_from_title.__doc__ or "") \
                or gate.fix_round_from_title("fix(scripts) 修闸门 r5：#3056",
                                             frozenset({"3056"})) != 5:
            fails.append("fix-r16 P2#1：fix_round_from_title 陈述点⑨与行为须同源"
                         "（docstring 须写明异侧相交照计入，且该形返回轮号 5）")
        # P2#2 硬不变式例外登记：行为面＝上方 fix-r15 P2#1「board 尾异侧不相交」钉
        # （([], [])＝登记的有意双空），模块 docstring / fix-r11 矩阵陈述已加例外行；
        # 另登记不修（MM r15 boundary#2）的实测记录须人核，见
        # _tmp/20261114-3074-fixr16/review-round-ref-probe.out。
        # 硬不变式例外第二格（fix-r17，MM r16 sibling#2 / boundary#3）：「无链×简写」
        # 格（题头区内简写 token、全题无 #N 链）自 r14 起有意双空——既有 fix-r14
        # 「无链简拼」窄窗钉即该格行为钉（([], [])），模块 docstring ㉓ 段已补例外行。
        if gate._scan_pairs("chore(gateway) upload 清理 r6：仓库根 _tmp 清出 3.2 GB",
                            frozenset()) != ([], []):
            fails.append("fix-r17 不变式例外第二格：无链×简写＝登记双空（r14 窄窗，"
                         "模块 docstring ㉓ 段例外行）")

        # —— #3074 fix-r17（MM r16 boundary#1 P1 主修：本地形态 foreign 出口；
        # 协调者实测仲裁 _tmp/20261114-3074-fixr16-mm2-probe/ARBITRATION.md，探针
        # p1-probe.py 同目录）——
        # P1 双向钉：(a) 本地形态（titles=None）声明形首行 + 异单回显行 → 拒收（仲裁
        # 场景：首行 `fix-r5：修闸门早退谓词收窄为声明形` + 回显行引 #3099、本 PR
        # 关联 #3056——r16 修前本地零拒收〔fail-open 实锤〕，r17 照跑扫描后红）；
        # (b) CI 形态同 body 但 titles 干净 → 声明形跳过照旧不拒（r15 修的假 FAIL
        # 面在 CI 形态保持；fix_round 由正文首行兜底，义务照走）。
        r17_body = ("fix-r5：修闸门早退谓词收窄为声明形\n\n## 概述\n承接上轮评审意见。\n\n"
                    "fix(scripts): #3099 fix-r3：修闸门\n\n"
                    + blk_t.format(h="") + "Closes #3056\n")
        r17_local_errs = gate.check(r17_body, "", "", "", "")
        if not any("异单旁路" in e and "正文回显行" in e and "3099" in e for e in r17_local_errs):
            fails.append(f"fix-r17 P1(a)：本地形态声明形首行 + 异单回显行须拒收——回显行是"
                         f"唯一 foreign 出口（r16 修前零拒收＝仲裁实锤）：{r17_local_errs}")
        if not any("fix-r5" in e and "fixtype" in e for e in r17_local_errs):
            fails.append(f"fix-r17 义务对照：本地形态首行兜底的修复轮义务照走（fixtype）：{r17_local_errs}")
        # (b) CI 形态：monkeypatch pr_commit_titles 返回干净标题集（不命中修复轮、
        # 不引异单）——titles is not None ⇒ 声明形跳过照旧；fixtype 义务仍由首行
        # 兜底触发（主修只动 foreign 半边的跳过，不动义务）。
        _orig_pct = gate.pr_commit_titles
        try:
            gate.pr_commit_titles = lambda repo, base, head: (["chore(state) 00:13 交接段：普通状态提交"], "")
            r17_ci_errs = gate.check(r17_body, "", "", "", "")
        finally:
            gate.pr_commit_titles = _orig_pct
        if any("异单旁路" in e for e in r17_ci_errs):
            fails.append(f"fix-r17 P1(b)：CI 形态（titles 干净）声明形首行照旧跳过——r15 修的"
                         f"假 FAIL 面在 CI 形态保持，不得回退：{r17_ci_errs}")
        if not any("fix-r5" in e and "fixtype" in e for e in r17_ci_errs):
            fails.append(f"fix-r17 义务对照：CI 形态首行兜底的修复轮义务照走（fixtype）：{r17_ci_errs}")
        # 半角冒号判据（MM r16 boundary#2，处置选项②：保留半角 + 理由句改写〔_first_line_
        # declares docstring：半角不与 zone_end 同源，登记为漏检向窄窗〕+ 端到端两向钉）：
        # 半角声明形首行 + 异单回显 → 本地拒（r17 后本地形态照跑＝fail-closed 向）/
        # CI 跳过（登记窄窗行为钉住不漂移）。
        hw_body = ("fix-r3: 修闸门\n\nfix(scripts): #3099 fix-r3：修闸门\n\n"
                   + blk_t.format(h="") + "Closes #3056\n")
        hw_local_errs = gate.check(hw_body, "", "", "", "")
        if not any("异单旁路" in e and "正文回显行" in e for e in hw_local_errs):
            fails.append(f"fix-r17 半角端到端·本地：半角声明形首行不豁免本地形态扫描"
                         f"（fail-closed 向，MM r16 boundary#2 选项②登记面）：{hw_local_errs}")
        try:
            gate.pr_commit_titles = lambda repo, base, head: (["chore(state) 00:13 交接段：普通状态提交"], "")
            hw_ci_errs = gate.check(hw_body, "", "", "", "")
        finally:
            gate.pr_commit_titles = _orig_pct
        if any("异单旁路" in e and "正文回显行" in e for e in hw_ci_errs):
            fails.append(f"fix-r17 半角端到端·CI：CI 形态半角声明形照旧跳过（漏检向登记"
                         f"窄窗，行为钉住不漂移）：{hw_ci_errs}")
        # 窄窗第二向钉（MM r16 sibling#3 ≡ DS r16 boundary#3）：提及形恰好紧跟冒号
        # （`承接 fix-r3：未完事项`）按声明处理＝跳过扫描——漏检向登记，直调钉钉住
        # 该取舍不被当成可接受而再放宽；RSHORT 简写（`DS r5：`）不是声明 token
        # （FIXROUND_RE 单源，简写首行两半边都不算声明形）。
        if not gate._first_line_declares("承接 fix-r3：未完事项\n\nx\n"):
            fails.append("fix-r16 判据窄窗第二向：提及形恰好紧跟冒号按声明处理＝跳过扫描"
                         "（漏检向登记，勿当成可接受取舍再放宽）")
        if gate._first_line_declares("DS r5：评审意见\n\nx\n"):
            fails.append("fix-r16 判据边界：RSHORT 简写不是声明 token（FIXROUND_RE 单源，"
                         "简写首行两半边都不算声明形）")
        # _first_line 行取单源配对钉（DS r16 sibling#2，P2 缺测试配对）：「首个非空行」
        # 语义（跳过前导空行/空白行）此前无钉——变异为 split[0] 后 fix_round_from_body
        # 由 5→None（义务整段跳过，fail-open 向）、_first_line_declares True→False，
        # 既有钉全绿。负控：全空白正文＝空串首行 → 不声明（回显扫描照跑）。
        if gate._first_line("\n\n   \n修复fix-r5：x\ny\n") != "修复fix-r5：x":
            fails.append("fix-r17 行取单源：_first_line 取首个非空行（跳过前导空/白行）")
        if gate.fix_round_from_body("\n \n修复fix-r5：x") != 5:
            fails.append("fix-r17 行取单源：前导空行不吞首行兜底轮号（义务不整段跳过）")
        if not gate._first_line_declares("\n\n修复fix-r5：x"):
            fails.append("fix-r17 行取单源：前导空行不吞声明形判定")
        if gate._first_line_declares("\n\n \n\t\n"):
            fails.append("fix-r17 行取单源负控：全空白正文＝空串首行 → 不声明（回显扫描照跑）")
        # 两半边取号分叉登记钉（DS r16 boundary#1，登记不修）：foreign 半边「任一紧跟
        # 冒号 token＝声明」只判形状（同串判声明形的钉在上方 fix-r16 判据块），rounds
        # 半边仍取 FIXROUND_RE 首个命中（fix-r4 宽松口径）——`承接 fix-r3，本轮修
        # fix-r5：…` 形状＝声明、轮号＝3，分叉是登记在案的有意设计（docstring 已各自
        # 声明，勿统一回去）。
        if gate._first_line_round("承接 fix-r3，本轮修 fix-r5：停泊补 waker") != 3:
            fails.append("fix-r17 分叉登记：rounds 半边取首个命中（fix-r4 宽松口径不变）")

        # —— #3074 fix-r18（P0：r17 实测登记的 ECHO_HEAD 相邻星号回溯〔DS r16
        # concurrency#1，r17 计时 _tmp/20261114-3074-fixr17/echo-head-timing.out〕，
        # 协调者裁定 _tmp/20261114-3074-fixr18/p0-registration.md）——
        # 修法＝正则改写 `\)\s*[:：]?\s*` → `\)\s*(?:[:：]\s*)?`（语言等价
        # W* C? W* ≡ W* (C W*)?，冒号后空白只归一处；不用 possessive/原子组——
        # 那要 python≥3.11，本修法全版本可用），失败路径每步回溯 O(1)＝线性。 ——
        # 性能钉：r17 计时构造法（`fix(scripts)` + N 空格 + `fix-r5：x`，GitHub 正文
        # 65536 字符上限内单行最大 N=65514）。修前实测（r17）：N=32768→7.854 s /
        # N=65514→35.172 s（二次方）；修后实测（本机 darwin python3.14）~0.003 s
        #（echo-head-timing-r18.out）。**fix-r19：计时断言迁入 _probe 子进程护栏**
        #（上方 echo 模式，N=65514 断言 <2.0 s＝预注册 10 s 的 5× 余量；fix-r16
        # 教训——就地断言在回退态会把套件挂死在永不返回的调用上）；本块只留语义面
        #（失败形不命中/成功形命中），对照块的死赋值 perf_counter（MM r18
        # concurrency#2 P3）随迁移一并删除。**fix-r20（DS r19 boundary#2 P2 + #6 P3）**：
        # 语义断言与 N 无关——N 从 65514 降到 40：回退态探针先红之后，执行流不再在
        # 父进程把同一条 35 s 调用重跑一遍（「子进程干净 FAIL」对就地大 N 形不成立）。
        if gate.ECHO_HEAD.match("fix(scripts)" + " " * 40 + "fix-r5：x") is not None:
            fails.append("fix-r18 性能钉语义面：失败形（N 空格 + fix-r5：x）不得命中"
                         "（正则改写不得引入新匹配面）")
        if gate.ECHO_HEAD.match("fix(scripts)" + " " * 40 + "#3056") is None:
            fails.append("fix-r18 性能钉对照：成功形（N 空格 + #N）须照常命中（贪心首刀）")
        # 冒号位形双向钉（等价面）：`\)` 后 W* C? W* 的位形族两版同判——命中族/
        # 拒收族各钉；多冒号形两版都不命中＝单可选冒号语义不得放宽成 (?:\s|[:：])*。
        for _line, _want in (
                ("fix(scripts) #3056", True), ("fix(scripts):#3056", True),
                ("fix(scripts) ： #12", True), ("fix(scripts) : PR #12", True),
                ("fix(scripts) PR #12", True),
                ("fix(scripts)  ：  PR  #12 fix-r3：x", True),
                ("fix(scripts) #3056+#3057 fix-r3：x", True),
                ("fix(" + "s" * 80 + ") #1", True),
                ("fix(scripts) :: #3056", False), ("fix(scripts) ：: #3056", False),
                ("fix(scripts) :#x", False), ("fix(scripts) x #3056", False),
                ("chore(state) 01:45 #1305 fix-r3：x", False),
                ("fix(" + "s" * 81 + ") #1", False)):
            if bool(gate.ECHO_HEAD.match(_line)) is not _want:
                fails.append(f"fix-r18 冒号位形钉：{_line[:60]!r} 须"
                             f"{'命中' if _want else '不命中'}（改写不得动 W* C? W* 语言）")
        # 等价差分钉（differential oracle）：与修前形态（r17 ref 30a98ebb1 的
        # `\s*[:：]?\s*`，此处按原字面重建）在 `\)` 后字母表上确定性随机对拍——
        # 两版 match（含 span）须零差异；任一侧形态改动（含写回相邻星号形）本钉必红。
        import random as _rnd
        import re as _re18
        _old_echo = _re18.compile("^" + gate._TYPE_WORDS +
                               r"\([^)\n]{1,80}\)\s*[:：]?\s*(?:PR\s+)?#\d+")
        _rng = _rnd.Random(3074)
        _alpha = " \t：: #5PRxf)（）"
        _diffs = 0
        for _ in range(4000):
            _probe_line = "fix(scripts)" + "".join(
                _rng.choice(_alpha) for _ in range(_rng.randrange(0, 18)))
            _a, _b = _old_echo.match(_probe_line), gate.ECHO_HEAD.match(_probe_line)
            if (_a is None) != (_b is None) or (_a and _a.span() != _b.span()):
                _diffs += 1
        if _diffs:
            fails.append(f"fix-r18 等价差分钉：4000 随机位形 old-vs-new 差异 {_diffs} 条"
                         "（正则改写必须语言严格等价——穷举证据见"
                         " _tmp/20261114-3074-fixr18/echo-head-equiv.out）")
        # 形态字面量钉：正则全文钉死（词表单源 _TYPE_WORDS 拼前缀）——写回相邻星号
        # 形态 `\s*[:：]?\s*`（重开 O(N²) 面）或任何其他改写时本钉必红，改动须过
        # p0-registration.md 的 P0 通道（与上方 _TYPE_WORDS 字面量钉同款防御）。
        if gate.ECHO_HEAD.pattern != "^" + gate._TYPE_WORDS + \
                r"\([^)\n]{1,80}\)\s*(?:[:：]\s*)?(?:PR\s+)?#\d+":
            fails.append(f"fix-r18 形态钉：ECHO_HEAD 形态被改（现值 {gate.ECHO_HEAD.pattern!r}"
                         "——写回相邻星号形态 `\\s*[:：]?\\s*` 重开 O(N²) 回溯面）")

        # —— #3074 fix-r19（家族级：DS/MM r17+r18 增量双谱系双撞——P0 WAIVER Θ(N³) +
        # P1 NO_ISSUE Θ(N²) + P1 titles 空集三态 + 兄弟脚本 SCOPE_BODY/PERSON_DAY 同族
        # 并入；协调者裁定 _tmp/20261114-3074-fixr19/extra.md）——
        _tb_spec = importlib.util.spec_from_file_location(
            "pdrs_taskbook_r19", ROOT / "scripts" / "check-taskbook-segments.py")
        tb = importlib.util.module_from_spec(_tb_spec)
        _tb_spec.loader.exec_module(tb)
        # (0) 族级静态扫描钉（主护栏）：两脚本全部 re.compile 模式扫「同一字符类可被
        # 相邻两个量词同时匹配」，白名单外即红；含探测器自检（修前反形态样本全命中，
        # 防扫描器静默全放行）。实现与白名单在模块级 _fscan_* / _FSCAN_WHITELIST。
        fails.extend(f"fix-r19 族级静态扫描：{e}" for e in _family_scan_fails(
            ((gate, ROOT / "scripts" / "check-pr-gate.py"),
             (tb, ROOT / "scripts" / "check-taskbook-segments.py"))))
        # (1) 形态字面量钉 ×4：写回反模式形态（重开回溯面）或任何其他改写必红。
        if gate.WAIVER.pattern != r"<!--\s*pr-gate:waiver\s+(\S.*?)-->":
            fails.append(f"fix-r19 形态钉：WAIVER 形态被改（现值 {gate.WAIVER.pattern!r}"
                         "——写回 `\\s+(.+?)\\s*` 三方可同吃一段空白形态重开 Θ(N³) 回溯面）")
        if gate.NO_ISSUE.pattern != r"<!--\s*no-issue\s+(\S.*?)-->":
            fails.append(f"fix-r19 形态钉：NO_ISSUE 形态被改（现值 {gate.NO_ISSUE.pattern!r}"
                         "——写回尾部 `\\s*` 形态重开 Θ(N²) 回溯面）")
        if tb.SCOPE_BODY.pattern != rf"^\s*pdrs{tb.DASHES}*scope\s*(?:[:：]\s*)?(?P<rest>(?:[^\s\n][^\n]*)?)$":
            fails.append(f"fix-r19 形态钉：SCOPE_BODY 形态被改（现值 {tb.SCOPE_BODY.pattern!r}"
                         "——冒号槽写回相邻星号形或去掉 rest 首字符钉（\\s*×.* 残余二次方）必红）")
        if tb.PERSON_DAY.pattern != (
                r"(?:约|大约|估计|预计约|预计|预期|估算|估|工作量|共计|合计|总计|共)"
                r"(?:需(?:要|投入)?)?\s*(?:[:：,，]\s*)?"
                r"([0-9]+(?:\.[0-9]+)?)\s*(?:个\s*)?(?:[–\-~至]\s*([0-9]+(?:\.[0-9]+)?)\s*)?人[日天]"):
            fails.append(f"fix-r19 形态钉：PERSON_DAY 形态被改（现值 {tb.PERSON_DAY.pattern!r}"
                         "——冒号/个槽写回 W* C? W* 形态重开二次回溯面）")
        # (2) 等价差分钉（differential oracle，r18 口径）：与修前形态（r18 ref e3372e58c
        # 字面量重建）确定性随机对拍。WAIVER 比对层面＝gate 的实际消费 `w.group(1).strip()`
        # （:1851），尾随空白归并进组后再 strip 等价；**纯空白理由形＝登记收窄**（old 命中
        # 空理由、new 不命中——两向都拒收，见 (4)），差分计数须排除该对。计时护栏在上方
        # _probe（waiver/noissue/scope/personday 模式，N=65514 断言 <2.0 s）。
        _old_w19 = re.compile(r"<!--\s*pr-gate:waiver\s+(.+?)\s*-->", re.S)
        _old_n19 = re.compile(r"<!--\s*no-issue\s+(\S.*?)\s*-->", re.S)
        _old_s19 = re.compile(r"^\s*pdrs[\s_\-－—‐‑]*scope\s*[:：]?\s*(?P<rest>.*)$", re.I)
        _old_p19 = re.compile(r"(?:约|大约|估计|预计约|预计|预期|估算|估|工作量|共计|合计|总计|共)"
                              r"(?:需(?:要|投入)?)?\s*[:：,，]?\s*"
                              r"([0-9]+(?:\.[0-9]+)?)\s*(?:个)?\s*(?:[–\-~至]\s*([0-9]+(?:\.[0-9]+)?)\s*)?人[日天]")
        _rng19 = _rnd.Random(3056)
        _aw, _an, _asc, _apd = " \t\n：:-额x", " \t\n：-x", " \n:：x_", " \t:，,.01357个–-~至人日x"
        _dw = _dn = _ds = _dp = 0
        for _ in range(4000):
            _body = ("<!--pr-gate:waiver" + _rng19.choice(["", " ", "\n"])
                     + "".join(_rng19.choice(_aw) for _ in range(_rng19.randrange(0, 14)))
                     + _rng19.choice(["-->", "--", "", "-->x"]))
            _a, _b = _old_w19.search(_body), gate.WAIVER.search(_body)
            _ra = _a.group(1).strip() if _a else None
            _rb = _b.group(1).strip() if _b else None
            if _ra != _rb and not (_ra == "" and _rb is None):
                _dw += 1
            _body = ("<!--no-issue" + _rng19.choice(["", " "])
                     + "".join(_rng19.choice(_an) for _ in range(_rng19.randrange(0, 12)))
                     + _rng19.choice(["-->", "--", "", "-->x"]))
            if bool(_old_n19.search(_body)) != bool(gate.NO_ISSUE.search(_body)):
                _dn += 1
            _body = ("pdrs" + _rng19.choice(["", "-", " ", "_", "—"]) + "scope"
                     + "".join(_rng19.choice(_asc) for _ in range(_rng19.randrange(0, 12))))
            _a, _b = _old_s19.match(_body), tb.SCOPE_BODY.match(_body)
            if (_a is None) != (_b is None) or (_a and (_a.span() != _b.span()
                                                        or _a.group("rest") != _b.group("rest"))):
                _ds += 1
            _body = (_rng19.choice(["约", "预计", "合计", "工作量", "共需投入", "预计约"])
                     + "".join(_rng19.choice(_apd) for _ in range(_rng19.randrange(0, 16))))
            if _old_p19.findall(_body) != tb.PERSON_DAY.findall(_body):
                _dp += 1
        if _dw:
            fails.append(f"fix-r19 等价差分钉：WAIVER 4000 随机位形 strip 层差异 {_dw} 条"
                         "（正则改写须语言等价——穷举证据 _tmp/20261114-3074-fixr19/）")
        if _dn:
            fails.append(f"fix-r19 等价差分钉：NO_ISSUE 4000 随机位形 bool 层差异 {_dn} 条")
        if _ds:
            fails.append(f"fix-r19 等价差分钉：SCOPE_BODY 4000 随机位形 match/span/rest 差异 {_ds} 条")
        if _dp:
            fails.append(f"fix-r19 等价差分钉：PERSON_DAY 4000 随机位形 findall 差异 {_dp} 条")
        # (3) 冒号/三明治位形双向钉（等价面，r18 口径）：
        for _body, _want in (
                ("<!-- pr-gate:waiver 理由足够长 -->", True),
                ("<!--pr-gate:waiver   多空白  理由   -->", True),
                ("<!--pr-gate:waiver\n  跨行\n  理由\n-->", True),
                ("<!--pr-gate:waiver 理由带尾随空白   -->", True),
                ("<!--pr-gate:waiver\t理由制表符\t-->", True),
                ("<!--pr-gate:waiver x-->理由<!--pr-gate:waiver y-->", True),
                ("<!--pr-gate:waiver   -->", False),          # 纯空白理由＝登记收窄（见 (4)）
                ("<!--pr-gate:waiver \n -->", False),
                ("<!--pr-gate:waiver", False)):
            if bool(gate.WAIVER.search(_body)) is not _want:
                fails.append(f"fix-r19 WAIVER 位形钉：{_body[:44]!r} 须{'命中' if _want else '不命中'}")
        for _body, _want in (("<!-- no-issue 纯文档说明 -->", True),
                             ("<!--no-issue   多空白  x   -->", True),
                             ("<!--no-issue\n跨行\n-->", True),
                             ("<!--no-issue x-->", True),
                             ("<!--no-issue  -->", False),
                             ("<!--no-issue", False)):
            if bool(gate.NO_ISSUE.search(_body)) is not _want:
                fails.append(f"fix-r19 NO_ISSUE 位形钉：{_body[:44]!r} 须{'命中' if _want else '不命中'}")
        for _body, _want in (
                ("pdrs-scope: files=16 est_min=180", "files=16 est_min=180"),
                ("pdrs-scope  ：  files=2", "files=2"),
                ("pdrs-scope\n: files=2", "files=2"),          # 跨行桥形照常命中 → :526 报错路径保持
                ("pdrs-scope:\nfiles=2", "files=2"),
                ("pdrs-scope:  : x", ": x"),
                ("PDRS-SCOPE: x", "x"),
                ("pdrs-scope:  ", ""),
                ("pdrs-scope: files=2\nx", None),              # 行中 \n 且后续有内容 → $ 失败（两版同判）
                ("pdrs scope", "")):
            _m = tb.SCOPE_BODY.match(_body)
            if (_m.group("rest") if _m else None) != _want:
                fails.append(f"fix-r19 SCOPE_BODY 位形钉：{_body[:36]!r} rest 须 {_want!r}"
                             "（跨行桥形照常命中＝「body 含换行须报错」判据 :526 不动）")
        for _body, _want in (("约 3 人日", [("3", "")]), ("预计：2 人日", [("2", "")]),
                              ("工作量,5人日", [("5", "")]), ("共需投入 3.5 个人日", [("3.5", "")]),
                              ("预计  3   个   人日", [("3", "")]), ("大约 1-2 人日", [("1", "2")]),
                              ("合计： 4 人天", [("4", "")]), ("预计 3 人日，共 5 人日", [("3", ""), ("5", "")]),
                              ("预计 3 个 x", []), ("合计", [])):
            if tb.PERSON_DAY.findall(_body) != _want:
                fails.append(f"fix-r19 PERSON_DAY 位形钉：{_body[:24]!r} 须 {_want}"
                             f"（实得 {tb.PERSON_DAY.findall(_body)}）")
        # :526 判据端到端：跨行 scope 注释照常报「必须写在一行里」（SCOPE_BODY 须命中
        # 该形才走到这条报错——改写若断了跨行桥形，这条会静默变成「写法不对」别的通道）。
        _psc, _pse = tb.parse_scopes("<!-- pdrs-scope:\nfiles=2 -->")
        if _psc or not any("写在一行里" in e for e in _pse):
            fails.append(f"fix-r19 :526 判据：跨行 scope 注释须照常报「写在一行里」：{_pse}")
        _psc, _pse = tb.parse_scopes("  <!-- pdrs-scope: files=16 est_min=180 -->")
        if _pse or _psc != [{"files": 16, "est_min": 180}]:
            fails.append(f"fix-r19 正常 scope 声明不得被改写破坏：{_psc} {_pse}")
        # (4) WAIVER 纯空白理由收窄钉（有意，fail-closed 方向不变）：旧形命中后走
        # 「豁免理由太短」报错；新形不命中 → 走「缺角色审计结果块或 pr-gate:waiver 豁免」
        # 报错——两向都拒收，只是通道换了；豁免理由本来就必须 ≥8 字，纯空白形从来不是
        # 合规输入（登记见 WAIVER 定义处注释）。
        _wv_errs = gate.check("<!--pr-gate:waiver   -->\nCloses #3056\n", "", "", "", "")
        if not any("缺角色审计结果块" in e for e in _wv_errs) or any("豁免理由太短" in e for e in _wv_errs):
            fails.append(f"fix-r19 WAIVER 收窄钉：纯空白理由形须走「缺结果块」通道拒收"
                         f"（不得静默放行、也不得仍走旧「理由太短」通道）：{_wv_errs}")
        if gate.check("<!-- pr-gate:waiver 纯文档变更无需角色审计结果块 -->\nCloses #3056\n",
                      "", "", "", ""):
            fails.append("fix-r19 WAIVER 正常路径：合法豁免（理由 ≥8 字 + Closes #N）不得被改写破坏")
        if gate.check("<!--pr-gate:waiver\n  跨行\n  豁免理由足够长\n-->\nCloses #3056\n",
                      "", "", "", ""):
            fails.append("fix-r19 WAIVER 多行理由（re.S）不得被改写收窄")
        # (5) titles 三态钉（P1 主修）：空集 []（base==head 空提交范围等＝无主信号，
        # pr_commit_titles docstring 明写与 None 同款走兜底）+ 声明形首行 + 异单回显行
        # → 须拒收；对照非空干净标题集照旧跳过（r15 面/r17 面保持，与 fix-r17 P1(b) 钉
        # 同轴复钉防本轮回退）。
        _orig_pct19 = gate.pr_commit_titles
        try:
            gate.pr_commit_titles = lambda repo, base, head: ([], "")
            _t3_errs = gate.check(r17_body, "", "", "", "")
        finally:
            gate.pr_commit_titles = _orig_pct19
        if not any("异单旁路" in e and "正文回显行" in e and "3099" in e for e in _t3_errs):
            fails.append(f"fix-r19 titles 三态：空集 [] 须与 None 同走正文回显 foreign 扫描"
                         f"（声明形首行不豁免）——r17 修的漏检形状在 [] 态的残留：{_t3_errs}")
        try:
            gate.pr_commit_titles = lambda repo, base, head: (["chore(state) 00:13 交接段：普通状态提交"], "")
            _t3_ci = gate.check(r17_body, "", "", "", "")
        finally:
            gate.pr_commit_titles = _orig_pct19
        if any("异单旁路" in e for e in _t3_ci):
            fails.append(f"fix-r19 titles 三态对照：非空干净标题集声明形首行照旧跳过"
                          f"（r15 修的假 FAIL 面保持）：{_t3_ci}")
        if "titles is not None and _first_line_declares" in (gate._first_line_declares.__doc__ or "") \
                or "titles and" not in (gate._first_line_declares.__doc__ or ""):
            fails.append("fix-r19 titles 三态文档同源：_first_line_declares docstring 须写"
                         "「titles and」（非空口径），旧完整条件句不得原样残留")
        # (6) _echo_head_lines 直调钉（MM r16 simplify#2 抽单源后无测试配对，DS r18
        # sibling#2 补）：行提取＝strip 后逐行 ECHO_HEAD.match，非回显行剔除。
        if gate._echo_head_lines("散文行\nfix(scripts) #1 fix-r2：y\n  fix(scripts) #2 fix-r3：z  \n") \
                != ["fix(scripts) #1 fix-r2：y", "fix(scripts) #2 fix-r3：z"]:
            fails.append("fix-r19 _echo_head_lines 直调钉：strip 后逐行 ECHO_HEAD.match"
                         "（非回显行剔除、行尾空白剥掉）——两半边（foreign/rounds）共用的行提取")

        # —— #3074 fix-r20（DS/MM r19 双谱系增量评审：P1-A 族级白名单 fail-open 负控 +
        # P1-B 多前缀预扫语义钉 + DS P2#4 多 marker 宁拒不漏；协调者实测仲裁
        # _tmp/20261114-3074-fixr20/）——
        # (1) P1-A 篡改负控（护栏的护栏）：白名单改整条 pattern 相等后，篡改副本 ISSUE_NUM
        # 尾拼反模式必须红——旧 startswith 前缀判据下该尾拼不失豁免（fail-open 实锤，
        # MM r19 concurrency#1 / DS r19 boundary#3 双撞）。锚点＝ISSUE_NUM 结尾字面量，
        # 源中不唯一时本夹具失效＝先红（防篡改实验静默变空转）。
        _gate_src20 = (ROOT / "scripts" / "check-pr-gate.py").read_text(encoding="utf-8")
        _tamper_anchor = ')(?![A-Za-z0-9_])", re.I)'
        if _gate_src20.count(_tamper_anchor) != 1:
            fails.append("fix-r20 负控锚点：ISSUE_NUM 结尾字面量在源中不唯一——篡改夹具失效，"
                         "须先修本测试锚点")
        else:
            _tam_dir = tmp / "fscan-tamper"
            _tam_dir.mkdir()
            _tam_path = _tam_dir / "check-pr-gate.py"
            _tam_path.write_text(_gate_src20.replace(
                _tamper_anchor,
                ')(?![A-Za-z0-9_])[^\\S\\r\\n]*[:：]?[^\\S\\r\\n]*", re.I)'), encoding="utf-8")
            _tam_spec = importlib.util.spec_from_file_location("gate_tampered_r20", _tam_path)
            gate_t = importlib.util.module_from_spec(_tam_spec)
            assert _tam_spec.loader is not None
            _tam_spec.loader.exec_module(gate_t)
            _tam_fails = _family_scan_fails(((gate_t, _tam_path),))
            if not _tam_fails:
                fails.append("fix-r20 P1-A 负控：篡改副本 ISSUE_NUM 尾拼反模式"
                             r"（`[^\S\r\n]*[:：]?[^\S\r\n]*`，r18 相邻星号形）必须红——"
                             "白名单整条相等失效＝fail-open 回归")
        # (2) DS P2#4 多 marker 位形两向钉：任一全空白理由的 waiver marker 即拒收
        # （r19 收窄让空白 marker 与另一条 marker 并存时经跨界命中以垃圾长理由放行；
        # 实测 r18 旧形同款跨界放行——DS「旧形拒收」静态推断不成立，本修按宁拒不漏
        # 收口为两向都拒，见 WAIVER_BLANK 定义处登记）。
        for _mb in ("<!--pr-gate:waiver   -->\n<!--pr-gate:waiver 纯文档变更无需角色审计结果块 -->\nCloses #3056\n",
                    "<!--pr-gate:waiver 纯文档变更无需角色审计结果块 -->\n<!--pr-gate:waiver   -->\nCloses #3056\n"):
            _mb_errs = gate.check(_mb, "", "", "", "")
            if not any("豁免理由太短" in e for e in _mb_errs):
                fails.append(f"fix-r20 多 marker 位形：任一全空白理由 marker 即拒收（两向都"
                             f"拒，宁拒不漏；跨界命中垃圾理由不得放行）：{_mb!r} → {_mb_errs}")
        # (3) P1-B 预扫语义钉：无字面 `-->` 的正文对 WAIVER/NO_ISSUE 都不可能命中——
        # 预扫跳过 search 不得改变任何判定（语言等价）；多前缀计时钉在 _probe
        # （waiver_multi/noissue_multi 模式，上方）。
        _np_errs = gate.check("<!--pr-gate:waiver x\nCloses #3056\n", "", "", "", "")
        if not any("缺角色审计结果块" in e for e in _np_errs):
            fails.append(f"fix-r20 预扫语义钉：无 `-->` 的未闭合 waiver 前缀不得因跳过 search"
                         f"而改变判定（照走「缺结果块」拒收）：{_np_errs}")
        _np2_errs = gate.check("<!--no-issue 纯文档说明\n", "", "", "", "")
        if not any("缺角色审计结果块" in e for e in _np2_errs) \
                or not any("缺 issue 关联" in e for e in _np2_errs):
            fails.append(f"fix-r20 预扫语义钉：无 `-->` 的未闭合 no-issue 前缀不得因跳过 search"
                         f"而改变判定（照走「缺结果块」+「缺 issue 关联」双拒收——该体"
                         f"issues=∅，NO_ISSUE 调用点实际执行）：{_np2_errs}")
        if gate.check("<!-- pr-gate:waiver 纯文档变更无需角色审计结果块 -->\nCloses #3056\n",
                      "", "", "", ""):
            fails.append("fix-r20 正常态对照：含 `-->` 的合法豁免不得被预扫/收口破坏"
                         "（r19 正常路径钉同轴复钉）")

        # —— #3074 fix-r21（DS/MM r20 双谱系评审三簇同根：簇①② marker 提取转有界
        # span 线性扫描＋簇③ P0 未闭合空白 marker 拒收；协调者实测仲裁
        # _tmp/20261114-3074-fixr21/review.md）——
        # (1) 簇③位形两向钉（P0 主修面）：r20 实测 `<!--pr-gate:waiver   <!--no-issue
        # 纯文档说明 -->` 经 WAIVER 跨界命中以「下一条 marker 的文本」当豁免理由放行＝
        # rc0 假通过；修后 _blank_waiver_marker 按自身边界判空白 → 置空 reason 走
        # 「豁免理由太短」拒收。对照向：合法豁免（理由含非空白）不受空白判定影响；
        # 单独闭合空白 marker 照走「缺结果块」（r19 收窄面保持）；未闭合空白 marker
        # 与合法豁免并存同样拒收（r20 的 WAIVER_BLANK 收不到的形，宁拒不漏）。
        _c3_errs = gate.check("<!--pr-gate:waiver   <!--no-issue 纯文档说明 -->", "", "", "", "")
        if not _c3_errs or not any("豁免理由太短" in e for e in _c3_errs):
            fails.append(f"fix-r21 簇③：未闭合空白 marker + 下一条 marker 位形必须走"
                         f"「豁免理由太短」拒收（r20 实测 rc0 假通过＝尺子假通过面）：{_c3_errs}")
        if gate.check("<!-- pr-gate:waiver 纯文档变更无需角色审计结果块 -->\nCloses #3056\n",
                      "", "", "", ""):
            fails.append("fix-r21 簇③对照：合法豁免不得被空白判定破坏（判定按自身边界、"
                         "理由含非空白即豁免照常）")
        _c3_lone = gate.check("<!--pr-gate:waiver   -->\nCloses #3056\n", "", "", "", "")
        if not any("缺角色审计结果块" in e for e in _c3_lone) or any("豁免理由太短" in e for e in _c3_lone):
            fails.append(f"fix-r21 簇③对照：单独闭合空白 marker 照走「缺结果块」通道"
                         f"（r19 收窄面保持，不得改走理由太短）：{_c3_lone}")
        _c3_unclosed = gate.check("<!--pr-gate:waiver 合法理由足够长 -->\n<!--pr-gate:waiver   \nCloses #3056\n",
                                  "", "", "", "")
        if not any("豁免理由太短" in e for e in _c3_unclosed):
            fails.append(f"fix-r21 簇③：未闭合空白 marker（其后无 `-->`）与合法豁免并存时"
                         f"同样拒收（r20 的 WAIVER_BLANK 只收紧邻闭合子形，本形收不到）：{_c3_unclosed}")
        if not gate._blank_waiver_marker("<!--pr-gate:waiver   <!--no-issue x -->"):
            fails.append("fix-r21 簇③直调钉：_blank_waiver_marker 须命中「空白 + 下一条 marker」形"
                         "（自身边界＝最近的 `<!--`，不是紧邻 `-->`）")
        if gate._blank_waiver_marker("<!--pr-gate:waiver 理由足够长 -->"):
            fails.append("fix-r21 簇③直调钉：_blank_waiver_marker 不得命中合法豁免"
                         "（理由含非空白字符即非空白 marker）")
        if not gate._blank_waiver_marker("<!-- pr-gate:waiver   "):
            fails.append("fix-r21 簇③直调钉：_blank_waiver_marker 须命中未闭合空白 marker"
                         "（其后无 `-->`，边界＝串尾）")
        # (2) 新旧差分钉（安全子集，r19 口径扩位形）：随机位形对拍「旧全文 search 语义」
        # （＝未改的 MARKER/WAIVER/NO_ISSUE 正则直调——本轮不改正则只改调用点）vs 有界
        # span 版（_marker_finditer/_marker_search）。位形补 r19/r20 生成器都没有的
        # 多 marker / 嵌套候选 / `-->` 前置 / 未闭合 marker。断言：MARKER 逐条相等
        # （组 `[^>]*` 不含 `>` 不可能跨界＝零差异）；WAIVER/NO_ISSUE 命中时 span/组
        # 与旧完全一致（只收紧不放宽），旧命中而新不命中的必须是「理由到自身最近
        # `-->` 全空白」的有意收紧形（旧 group 以 `-->` 字面起＝收紧形的机器可判特征，
        # 逐条登记进计数）；收紧形计数 >0（生成器须真覆盖到簇③位形，否则夹具失效）。
        _rng21 = _rnd.Random(307421)
        _pieces21 = ["<!--", "-->", "<!--pr-gate:waiver", "pr-gate:waiver ", "<!--no-issue",
                     "no-issue ", "<!-- pr-gate:v1 roles=x -->", " ", "\n", "x", "理由文字",
                     "<!--pr-gate:v1", "  ", "--", ">", "<!--no-issue 纯文档说明 -->",
                     "Closes #3056\n", "waiver ", "v1", "<!--pr-gate:waiver  -->",
                     "<!--pr-gate:waiver  ", "<!--no-issue  -->"]
        _d21 = {"marker": 0, "waiver": 0, "noissue": 0, "waiver_tight": 0, "noissue_tight": 0}
        for _ in range(4000):
            _body = "".join(_rng21.choice(_pieces21) for _ in range(_rng21.randrange(0, 10)))
            if [(m.span(), m.group(1)) for m in gate.MARKER.finditer(_body)] \
                    != [(m.span(), m.group(1)) for m in gate._marker_finditer(gate.MARKER, _body)]:
                _d21["marker"] += 1
            for _pat, _key in ((gate.WAIVER, "waiver"), (gate.NO_ISSUE, "noissue")):
                _o21, _n21 = _pat.search(_body), gate._marker_search(_pat, _body)
                if _o21 is None and _n21 is not None:
                    _d21[_key] += 1      # 放宽方向（旧无命中而新命中）＝绝不许
                    continue
                if _n21 is not None and not (_n21.span() == _o21.span()
                                            and _n21.group(1) == _o21.group(1)):
                    # new 命中与旧首个不同：只允许「旧首个是跨界垃圾命中（簇③形）被
                    # 跳过、new 落到更后的合法 marker」——旧首个的组以 `-->` 字面起
                    # （收紧特征），且 new 的命中与「旧语义在同一起点」完全一致（不放宽）
                    _o_at = _pat.match(_body, _n21.start())
                    if not (_o21.group(1).startswith("-->") and _o_at is not None
                            and _o_at.span() == _n21.span()
                            and _o_at.group(1) == _n21.group(1)):
                        _d21[_key] += 1
                    else:
                        _d21[_key + "_tight"] += 1
                if _o21 is not None and _n21 is None:
                    if _o21.group(1).startswith("-->"):
                        _d21[_key + "_tight"] += 1   # 簇③有意收紧形（旧跨界命中），登记
                    else:
                        _d21[_key] += 1  # 安全子集外的意外差异
        if _d21["marker"] or _d21["waiver"] or _d21["noissue"]:
            fails.append(f"fix-r21 差分钉：安全子集外差异 {_d21}（MARKER 须与 finditer"
                         " 零差异；WAIVER/NO_ISSUE 命中须与旧全文 search 同 span 同组——"
                         "新命中是旧命中的子集，差异只允许出现在收紧形）")
        if not (_d21["waiver_tight"] or _d21["noissue_tight"]):
            fails.append("fix-r21 差分钉生成器失效：4000 随机位形未覆盖到簇③收紧形"
                         "（旧跨界命中/新拒收）——位形须补足再钉")
        # (3) MARKER 嵌套/非重叠直调钉：finditer 非重叠语义在 span 版保持（嵌套候选
        # 被外层匹配吞掉后不再产出；两个成功匹配不重叠）。
        if [(m.span(), m.group(1)) for m in gate._marker_finditer(
                gate.MARKER, "<!--pr-gate:v1 <!--pr-gate:v1 x-->")] != [((0, 34), " <!--pr-gate:v1 x")]:
            fails.append("fix-r21 MARKER 嵌套钉：嵌套候选只产外层匹配（finditer 非重叠"
                         "语义保持，span 版不得多产内层块）")
        if [(m.span(), m.group(1)) for m in gate._marker_finditer(
                gate.MARKER, "<!--pr-gate:v1 a--><!--pr-gate:v1 b-->")] != \
                [((0, 19), " a"), ((19, 38), " b")]:
            fails.append("fix-r21 MARKER 相邻钉：两个成功匹配不重叠且不漏（span 版与"
                         " finditer 同判）")

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
        # #3056 fix-r1 items 5/6：崩溃残留（父进程被杀，目录里只有 *.jsonl / *.err / diff.patch、
        # 还没写 <角色>.md）同样拒复跑——复跑 open("w") 会把唯一原始流截断
        for leftover in ("sibling.jsonl", "boundary.retry1.err", "diff.patch"):
            d = out_base / ("clobber-" + leftover.replace(".", "-"))
            d.mkdir(parents=True, exist_ok=True)
            (d / leftover).write_text("old bytes\n")
            cp = subprocess.run([sys.executable, "-I", str(RUNNER), "--claude-bin", str(fake), "--env-file", str(envfile),
                                 "--worktree", str(wt), "--base", "HEAD~1", "--out", str(d),
                                 "--roles", "simplify", "--dry-run"], capture_output=True, text=True, env=dict(os.environ))
            if cp.returncode != 2 or "原件" not in cp.stderr:
                fails.append(f"rerun-clobber：崩溃残留 {leftover} 应 rc=2 拒跑（rc={cp.returncode}、{cp.stderr.strip()[:150]}）")
            if (d / leftover).read_text() != "old bytes\n":
                fails.append(f"rerun-clobber：{leftover} 不得被改动")
        # fix-r2（DS r2 boundary#2 P2）：同目录 dry-run → 实跑正控——dry-run 不再落 diff.patch
        # （原件清单成员），预览件改名 dryrun-diff.patch；实跑不被「已有评审原件」误拒
        dr = out_base / "dryrun-then-real"
        cp1 = subprocess.run([sys.executable, "-I", str(RUNNER), "--claude-bin", str(fake), "--env-file", str(envfile),
                              "--worktree", str(wt), "--base", "HEAD~1", "--out", str(dr),
                              "--roles", "simplify", "--dry-run"], capture_output=True, text=True, env=dict(os.environ))
        if (cp1.returncode != 0 or (dr / "diff.patch").exists() or not (dr / "dryrun-diff.patch").exists()):
            fails.append(f"fix-r2 DS#2 dry-run：应 rc=0 且落 dryrun-diff.patch（不落原件名 diff.patch）"
                         f"（rc={cp1.returncode}，{sorted(p.name for p in dr.iterdir())}）：{cp1.stderr.strip()[:150]}")
        cp2 = subprocess.run([sys.executable, "-I", str(RUNNER), "--claude-bin", str(fake), "--env-file", str(envfile),
                              "--worktree", str(wt), "--base", "HEAD~1", "--out", str(dr),
                              "--roles", "simplify"], capture_output=True, text=True, env=dict(os.environ))
        if (cp2.returncode != 0 or "原件" in cp2.stderr or not (dr / "diff.patch").exists()
                or not (dr / "simplify.md").exists()):
            fails.append(f"fix-r2 DS#2 正控：同目录 dry-run 后实跑应照常出件（不被 dry-run 的预览件当原件误拒）"
                         f"（rc={cp2.returncode}，stderr={cp2.stderr.strip()[:150]}）")
        # 修 2（DS concurrency#1 = MM2 concurrency#2）：out 目录 flock 互斥——持锁进程在跑时，
        # 第二个同 --out 的进程必须硬拒且文案与「已有原件」区分（TOCTOU 窗口内原件尚未落盘，
        # 清单扫描拦不住并发互相截断证据）
        lockd = out_base / "locked-out"
        lockd.mkdir(parents=True, exist_ok=True)
        lockfd = os.open(lockd / ".lock", os.O_RDWR | os.O_CREAT, 0o644)
        try:
            fcntl.flock(lockfd, fcntl.LOCK_EX)
            cp = subprocess.run([sys.executable, "-I", str(RUNNER), "--claude-bin", str(fake), "--env-file", str(envfile),
                                 "--worktree", str(wt), "--base", "HEAD~1", "--out", str(lockd),
                                 "--roles", "simplify", "--dry-run"], capture_output=True, text=True, env=dict(os.environ))
            if cp.returncode != 2 or "占用" not in cp.stderr or "原件" in cp.stderr:
                fails.append(f"out 锁：持锁进程在跑时应 rc=2 报「占用」（与「已有原件」区分）（rc={cp.returncode}、{cp.stderr.strip()[:150]}）")
        finally:
            fcntl.flock(lockfd, fcntl.LOCK_UN)
            os.close(lockfd)
        # 修 4（DS concurrency#2）：名额闸异常（PDRS_REVIEW_SLOTS_* 非数字）不杀 worker 线程——
        # 每角色 status=ERROR 落盘（与额度耗尽 ERROR 同形态）、status.json/summary 完整、主线程不 KeyError 崩栈
        cp = subprocess.run([sys.executable, "-I", str(RUNNER), "--runner", "minimax", "--claude-bin", str(fake),
                             "--env-file", str(envfile), "--worktree", str(wt), "--base", "HEAD~1",
                             "--out", str(out_base / "slot-exn"), "--roles", ROLES],
                            capture_output=True, text=True,
                            env=dict(os.environ, PDRS_REVIEW_SLOTS_MINIMAX="6x"))
        st_se = json.loads((out_base / "slot-exn" / "status.json").read_text(encoding="utf-8")) \
            if (out_base / "slot-exn" / "status.json").exists() else {}
        s_se = (out_base / "slot-exn" / "summary.md").read_text(encoding="utf-8") \
            if (out_base / "slot-exn" / "summary.md").exists() else ""
        if ("Traceback" in cp.stderr or "KeyError" in cp.stderr or cp.returncode != 1
                or set(st_se) != set(ROLES.split(","))
                or not all(st_se[r].get("status") == "ERROR" and st_se[r].get("resource_contention") is False
                           and "名额闸异常" in (st_se[r].get("detail") or "") for r in st_se)
                or any(f"{r}:ERROR" not in s_se for r in ROLES.split(","))):
            fails.append(f"名额闸异常：四角色应各落 ERROR rec 不崩栈、流程走完（rc={cp.returncode}、"
                         f"{cp.stderr.strip()[:150]}、rec={ {r: st_se.get(r, {}).get('status') for r in ROLES.split(',')} }）")
        # item 5：原件清单的角色名从 ROLES 派生（不双份维护）——每个角色的 .md/.jsonl/.err 都算原件，
        # 非角色名 / 其它扩展不算
        with tempfile.TemporaryDirectory() as od:
            odp = Path(od)
            for r in rrr.ROLES:
                (odp / f"{r}.jsonl").write_text("")
            (odp / "notarole.md").write_text("")
            (odp / "notes.txt").write_text("")
            got = rrr.originals_in(odp)
            if sorted(got) != sorted(f"{r}.jsonl" for r in rrr.ROLES):
                fails.append(f"原件守卫：角色派生清单不符（{sorted(got)}）——notarole.md / notes.txt 不算原件")
        # 同上（DS sibling#4）：闸门的 REQUIRED_ROLES 与执行器的 ROLES 是同一份名单——增删角色
        # 只改一处会让闸门漏检 / 误拒（分开各写时无同步保证），测试钉死
        if tuple(rrr.ROLES) != gate.REQUIRED_ROLES:
            fails.append(f"角色名单漂移：ROLES={tuple(rrr.ROLES)} 与 check-pr-gate.REQUIRED_ROLES={gate.REQUIRED_ROLES} 须一致")
        # item 10：线程异常分支的 rec 带 resource_contention=False（机读契约不因分支缺键）
        rec = rrr.error_rec("simplify", "运行器异常 KeyError")
        if rec.get("resource_contention") is not False or rec.get("status") != "ERROR" or rec.get("counts") is not None:
            fails.append(f"error_rec：异常分支 rec 应带 resource_contention=False（{rec}）")
        if rrr.error_rec("sibling", "slot timeout", contention=True).get("resource_contention") is not True:
            fails.append("error_rec：争抢分支 rec 应带 resource_contention=True")
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



# ---- fix-r19（#3074 家族级静态扫描钉）--------------------------------------------------------
# r17 修单点（本地形态 foreign 出口）、r18 修单点（ECHO_HEAD），双谱系评审每轮都再撞出
# 同族形态——本轮收族配的护栏：扫两个闸门脚本**全部 re.compile 模式**，命中「同一字符
# 类可被相邻两个量词同时匹配」形态（WAIVER Θ(N³)/NO_ISSUE/SCOPE_BODY/PERSON_DAY/
# ECHO_HEAD 回溯爆炸的共同根因）且不在白名单＝红。日后给任一脚本改正则不过本扫＝
# 当场被抓（r17/r18 两轮「下次又漏一处」的教训固化成机器检查）。
# 检测规则（探针先行验证：旧形样本全命中、修后两脚本全量扫描只剩白名单三处＋一处
# 函数内局部量，证据 _tmp/20261114-3074-fixr19/family-scan-r19.out）：
# - 单元＝序列里一个可消费字符的原子带其量词（MAX_REPEAT/MIN_REPEAT 的 min/max）；
#   零宽（AT/ASSERT/ASSERT_NOT）打断相邻性；
# - 可迭代＝max≥2 或无上界（每一步都能多吞一个字符）；
# - 相邻对＝同层两个可迭代单元、中间只隔可跳过（min=0 或可空）单元，且前者**头或尾**
#   字符集与后者头字符集相交——两个方向都要：头-头盖 W* C? W* 三明治（r18 ECHO_HEAD），
#   尾-头盖 `(\S.*?)\s*`（r19 NO_ISSUE：组尾能吃空白、后面 `\s*` 也能吃）；
# - 嵌套重复 (X+)+（fix-r14 指数族）独立命中；
# - 字符类代数：LITERAL/RANGE 精确（range 超大区段判 ANY 保守）；CATEGORY 近似
#   （\s→常用空白集、\d→数字、\w→字母数字下划线，其余 ANY）；NEGATE 与 NOT_ 类走
#   补集代数（[^\S\r\n]＝空白减 \r\n、[^\w\s:#]＝词/空白/:/# 的补）；GROUPREF/未知 op
#   保守 ANY；re.I 下字面量大小写互扩；neg∩neg 保守判相交。
with warnings.catch_warnings():
    warnings.simplefilter("ignore")
    try:
        import sre_parse as _fscan_parse
        from sre_constants import (ASSERT, ASSERT_NOT, AT, ANY, BRANCH, CATEGORY,
                                  GROUPREF, IN, LITERAL, MAXREPEAT, MAX_REPEAT,
                                  MIN_REPEAT, NEGATE, NOT_LITERAL, RANGE, SUBPATTERN)
    except ImportError:  # python≥3.12 的迁移路径（3.11 前公开名仍可用，双名都试）
        import re._parser as _fscan_parse
        from re._constants import (ASSERT, ASSERT_NOT, AT, ANY, BRANCH, CATEGORY,
                                  GROUPREF, IN, LITERAL, MAXREPEAT, MAX_REPEAT,
                                  MIN_REPEAT, NEGATE, NOT_LITERAL, RANGE, SUBPATTERN)

_FSCAN_WS = frozenset(map(ord, " \t\n\r\f\v\x1c\x1d\x1e\x1f\x85\xa0"))
_FSCAN_DIGITS = frozenset(map(ord, "0123456789"))
_FSCAN_WORD = frozenset(map(ord, "0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ_"))
_FSCAN_CATS = {"SPACE": _FSCAN_WS, "DIGIT": _FSCAN_DIGITS, "WORD": _FSCAN_WORD}
_FSCAN_ANY = "ANY"


def _fscan_case(chars, flags):
    if not flags & re.IGNORECASE:
        return chars
    out = set(chars)
    for c in chars:
        if chr(c).isalpha():
            out.add(ord(chr(c).swapcase()))
    return frozenset(out)


def _fscan_in_class(items, flags):
    """IN 参数 → ('pos'|'neg', lits, ranges) / ANYSET；neg＝(lits∪ranges) 的补集。"""
    lits, ranges, neg, not_cats = set(), [], False, None
    for op, av in items:
        if op is LITERAL:
            lits.add(av)
        elif op is NEGATE:
            neg = True
        elif op is RANGE:
            ranges.append(tuple(av))
        elif op is CATEGORY:
            name = str(av)
            base = next((s for k, s in _FSCAN_CATS.items() if k in name), None)
            if base is None:
                return _FSCAN_ANY
            if "NOT" in name:
                not_cats = set(base) if not_cats is None else (not_cats & set(base))
            else:
                lits |= base
        else:
            return _FSCAN_ANY
    lits = _fscan_case(lits, flags)
    if not_cats is None:
        kind = "neg" if neg else "pos"
        return (kind, frozenset(lits), tuple(ranges))
    # 类＝A ∪ not_cats^c：无 NEGATE → ("neg", not_cats\A)；有 NEGATE → ("pos", not_cats\A)
    k = frozenset(c for c in not_cats
                  if c not in lits and not any(a <= c <= b for a, b in ranges))
    return ("pos", k, ()) if neg else ("neg", k, ())


def _fscan_covers(lits, ranges, c):
    return c in lits or any(a <= c <= b for a, b in ranges)


def _fscan_intersects(a, b):
    if a is _FSCAN_ANY or b is _FSCAN_ANY:
        return True
    (ka, la, ra), (kb, lb, rb) = a, b
    if ka == "pos" and kb == "pos":
        return bool(la & lb) \
            or any(_fscan_covers(lb, rb, c) for c in la) \
            or any(_fscan_covers(la, ra, c) for c in lb) \
            or any(p1 <= q2 and p2 <= q1 for p1, q1 in ra for p2, q2 in rb)
    if ka == "pos" and kb == "neg":
        for c in la:
            if not _fscan_covers(lb, rb, c):
                return True
        for x, y in ra:
            cov = sum(max(0, min(y, q) - max(x, p) + 1) for p, q in rb)
            cov += sum(1 for c in lb if x <= c <= y and not any(p <= c <= q for p, q in rb))
            if cov < y - x + 1:
                return True
        return False
    if ka == "neg" and kb == "pos":
        return _fscan_intersects(b, a)
    return True  # neg ∩ neg：保守判相交


def _fscan_union(a, b):
    if a is None:
        return b
    if a is _FSCAN_ANY or b is _FSCAN_ANY:
        return _FSCAN_ANY
    (ka, la, ra), (kb, lb, rb) = a, b
    if ka == "pos" and kb == "pos":
        return ("pos", la | lb, tuple(ra) + tuple(rb))
    if ka == "neg":
        return ("neg", frozenset(c for c in la if not _fscan_covers(lb, rb, c)),
                tuple((p, q) for p, q in ra if not _fscan_covers(lb, rb, p)))
    return _fscan_union(b, a)


class _FscanUnit:
    __slots__ = ("cls", "last", "minr", "maxr", "desc", "can_skip", "expandable")

    def __init__(self, cls, minr, maxr, desc, last=None):
        self.cls, self.minr, self.maxr = cls, minr, maxr
        self.desc = desc
        self.last = cls if last is None else last
        self.can_skip = minr == 0
        self.expandable = maxr == MAXREPEAT or maxr >= 2


def _fscan_atom(items, flags):
    """原子（可能是序列）→ Unit（量词默认 (1,1)）。不可消费（空序列）返回 None。"""
    if not items:
        return None
    op0, av0 = items[0]
    if op0 is LITERAL and len(items) == 1:
        return _FscanUnit(("pos", _fscan_case({av0}, flags), ()), 1, 1, repr(chr(av0)))
    if op0 is IN and len(items) == 1:
        return _FscanUnit(_fscan_in_class(av0, flags), 1, 1, "IN")
    if op0 is ANY and len(items) == 1:
        return _FscanUnit(_FSCAN_ANY, 1, 1, ".")
    if op0 is NOT_LITERAL and len(items) == 1:
        return _FscanUnit(("neg", _fscan_case({av0}, flags), ()), 1, 1, "NOT_LIT")
    if op0 is GROUPREF and len(items) == 1:
        return _FscanUnit(_FSCAN_ANY, 1, 1, r"\%d" % av0)
    if op0 is BRANCH and len(items) == 1:
        fs, ls, exp, skip = None, None, False, False
        for br in av0[1]:
            a = _fscan_atom(list(br), flags)
            if a is None:  # 空分支＝可跳过
                skip = True
                continue
            exp = exp or a.expandable
            skip = skip or a.can_skip
            if a.cls is None:
                continue
            fs = _fscan_union(fs, a.cls)
            ls = _fscan_union(ls, a.last)
        u = _FscanUnit(fs if fs is not None else ("pos", frozenset()), 1, 1, "BRANCH",
                       last=ls if ls is not None else ("pos", frozenset()))
        u.expandable, u.can_skip = exp, skip
        return u
    inner = _fscan_units(items, flags)
    if not inner:
        return None
    fs = None
    for u in inner:
        if u.cls is None:
            continue
        fs = _fscan_union(fs, u.cls)
        if fs is _FSCAN_ANY:
            break
        if not u.can_skip:
            break
    ls = None
    for u in reversed(inner):
        if u.cls is None:
            continue
        ls = _fscan_union(ls, u.last)
        if ls is _FSCAN_ANY:
            break
        if not u.can_skip:
            break
    u = _FscanUnit(fs, 1, 1, "GROUP", last=ls)
    u.expandable = any(x.expandable for x in inner)
    return u


def _fscan_units(seq, flags):
    out = []
    for op, av in seq:
        if op in (AT, ASSERT, ASSERT_NOT):
            out.append(_FscanUnit(None, -1, -1, "zero-width"))
            continue
        if op in (MAX_REPEAT, MIN_REPEAT):
            mn, mx, sub = av
            u = _fscan_atom(list(sub), flags)
            if u is None:
                continue
            u.minr, u.maxr = mn, mx
            u.can_skip = mn == 0
            u.expandable = mx == MAXREPEAT or mx >= 2
            out.append(u)
            continue
        if op is SUBPATTERN:
            u = _fscan_atom(list(av[-1]), flags)
            if u is not None:
                out.append(u)
            continue
        u = _fscan_atom([(op, av)], flags)
        if u is not None:
            out.append(u)
    return out


def _fscan_walk(seq, hits, where, flags):
    units = _fscan_units(seq, flags)
    for i, a in enumerate(units):
        if a.cls is None or not a.expandable:
            continue
        for j in range(i + 1, len(units)):
            b = units[j]
            if b.cls is None:
                break  # 零宽锚打断
            if any(not u.can_skip for u in units[i + 1:j]):
                break  # 中间有必现单元 → j 之后的不可能与 i 相邻
            if b.expandable and (_fscan_intersects(a.cls, b.cls) or _fscan_intersects(a.last, b.cls)):
                hits.append(f"{where}: {a.desc} … {b.desc}")
    for op, av in seq:
        if op in (MAX_REPEAT, MIN_REPEAT):
            mn, mx, sub = av
            sub_seq = list(sub)
            if (mx == MAXREPEAT or mx >= 2) and mn >= 1 and sub_seq:
                if any(u.expandable for u in _fscan_units(sub_seq, flags)):
                    hits.append(f"{where}: repeat({mn},≥2) 嵌套可迭代单元（(X+)+ 指数族）")
            _fscan_walk(sub_seq, hits, where + ".rep", flags)
        elif op is SUBPATTERN:
            _fscan_walk(list(av[-1]), hits, where + ".grp", flags)
        elif op is BRANCH:
            for br in av[1]:
                _fscan_walk(list(br), hits, where + ".br", flags)
        elif op in (ASSERT, ASSERT_NOT):
            _fscan_walk(list(av[-1]), hits, where + ".as", flags)


def _fscan_hits(pat, flags=0):
    hits: list[str] = []
    _fscan_walk(list(_fscan_parse.parse(pat, flags)), hits, "top", flags)
    return hits


# 白名单（已仲裁条目；改动/新增正则命中扫描且不在表内＝红，登记须给理由与实测依据）
# fix-r20（#3074 P1-A，MM r19 concurrency#1 + DS r19 boundary#3 双撞）：豁免判据从
# `startswith` 前缀改为**整条 pattern 相等**——前缀只钉开头，ISSUE_NUM 尾部再改出
# 反模式不失豁免＝fail-open（实测尾拼 `[^\S\r\n]*[:：]?[^\S\r\n]*` 后扫描仍绿，与本
# 文件自述「日后给任一脚本改正则不过本扫＝当场被抓」相反）。ISSUE_NUM 由
# `_ISSUE_KEYWORDS`/`_GAP_EDGE`/`_ISSUE_GAP_PUNCT` 运行时拼接，此处登记**展开后的
# 完整 .pattern 字面量**：任一组件改动都使整条失配即红（变更即红，须重新仲裁进本表）。
_FSCAN_WHITELIST = (
    # gate ISSUE_NUM（#3056 fix-r14/r15 定谳形态，勿动）：标点分支内层 `(?:hws* 标点)*`
    # 重复组后随尾部 GAP 空白——头-头同吃非换行空白，但每轮迭代必先吃一个标点＝轮划分
    # 唯一（r14 修法本身），punct/punct_mix/punct_nl 三条 _probe 计时钉钉住该形态不回退。
    # 下方字面量＝展开后的完整 .pattern（拼自 _ISSUE_KEYWORDS + _GAP_EDGE + _ISSUE_GAP_PUNCT
    # ＝ `(?:close[sd]?|fix(?:e[sd])?|resolve[sd]?|refs)` + `[^\S\r\n]*(?:\r?\n[^\S\r\n]*)?`
    # + `[\+,/、，]`），改任何一个组件＝失配即红。
    ("check-pr-gate.py",
     r"(?<![A-Za-z0-9_])(?:close[sd]?|fix(?:e[sd])?|resolve[sd]?|refs)\s+"
     r"#(?P<nums>\d+(?:(?:[^\S\r\n]*(?:\r?\n[^\S\r\n]*)?[\+,/、，]"
     r"(?:[^\S\r\n]*[\+,/、，])*[^\S\r\n]*(?:\r?\n[^\S\r\n]*)?|[^\S\r\n]+|)"
     r"#\d+)*)(?![A-Za-z0-9_])"),
    # tb SECTION：`\s+(.*)$` 相邻——逐行匹配（section_text 用 text.splitlines() 的行，
    # 行内无 \n），`.*$` 恒成功 → 无失败重扫路径（改回全文匹配须重审本条）。
    ("check-taskbook-segments.py", r"^(#{2,3})\s+(.*)$"),
    # tb FENCE_OPEN：围栏串后 `(?P<info>.*)$` 同上逐行恒成功（_fence_spans 用 lines[i]）。
    ("check-taskbook-segments.py", r" {0,3}(?P<f>`{3,}|~{3,})(?P<info>.*)$"),
)


def _family_scan_fails(mods) -> list[str]:
    """fix-r19 族级静态扫描：返回失败清单（空＝过）。mods＝(模块, 源文件路径) 序列
    （fix-r20 签名参数化：负控篡改副本用同一扫描器，不复制第二份判据）。"""
    errs: list[str] = []
    # 探测器自检（护栏的护栏）：r19 修前反模式字面样本必须全命中——静默全放行＝坏灯。
    for _label, _pat, _flags in (
            ("OLD WAIVER", r"<!--\s*pr-gate:waiver\s+(.+?)\s*-->", re.S),
            ("OLD NO_ISSUE", r"<!--\s*no-issue\s+(\S.*?)\s*-->", re.S),
            ("r17 ECHO_HEAD", r"^(?:fix|feat)\([^)\n]{1,80}\)\s*[:：]?\s*(?:PR\s+)?#\d+", 0),
            ("OLD SCOPE_BODY", r"^\s*pdrs[\s_\-－—‐‑]*scope\s*[:：]?\s*(?P<rest>.*)$", re.I),
            ("naive SCOPE_BODY", r"^\s*pdrs[\s_\-－—‐‑]*scope\s*(?:[:：]\s*)?(?P<rest>.*)$", re.I),
            ("OLD PERSON_DAY", r"(?:约|共)\s*[:：,，]?\s*([0-9]+)\s*(?:个)?\s*人[日天]", 0),
            ("nested (P+)+", r"Closes(?:[,\s]+)+#3056", re.I)):
        if not _fscan_hits(_pat, _flags):
            errs.append(f"族级扫描探测器自检：{_label} 反形态必须命中（检测面退化＝改回反模式不红）")
    for mod, path in mods:
        base = path.name
        src = path.read_text(encoding="utf-8")
        for node in ast.walk(ast.parse(src)):
            if not (isinstance(node, ast.Call) and isinstance(node.func, ast.Attribute)
                    and node.func.attr == "compile" and isinstance(node.func.value, ast.Name)
                    and node.func.value.id == "re"):
                continue
            seg = ast.get_source_segment(src, node)
            try:
                rx = eval(seg, dict(mod.__dict__))  # noqa: S307 — 受控字面量 + 模块名空间
            except NameError:
                # 函数内局部量拼的模板（围栏闭合 re.escape(ch)/str(n)）：只许已登记的这一个，
                # 且其代表形须过扫描（` {0,3}` 有界 + 围栏字面量隔断 + `[ \t]*` 收尾）。
                if base == "check-taskbook-segments.py" and "re.escape(ch)" in seg:
                    for rep in (r" {0,3}`{3,}[ \t]*", r" {0,3}~{4,}[ \t]*"):
                        if _fscan_hits(rep):
                            errs.append(f"围栏闭合代表形 {rep!r} 命中相邻量词形态（白名单登记失效）")
                else:
                    errs.append(f"{base} 有未登记的不可求值 re.compile：{seg[:70]!r}")
                continue
            hits = _fscan_hits(rx.pattern, rx.flags)
            # fix-r20（P1-A）：整条 pattern 相等才豁免（startswith 只钉前缀＝尾部改出
            # 反模式不失豁免，fail-open——篡改负控钉在 fix-r20 块）。
            if hits and not any(base == b and rx.pattern == s for b, s in _FSCAN_WHITELIST):
                errs.append(f"{base} {seg[:70]!r} 命中未登记的相邻量词形态：{hits[:2]}"
                             "（新反模式须仲裁：改写消歧或进白名单给理由）")
    return errs


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
        # waiver 侧同口径（#3056 fix-r1 item 12）：非修复轮的豁免标记声明了不适用也复算路径
        w_nf_na = "<!-- pr-gate:waiver 非修复轮纯文档豁免说明文字足够长 fixtype=不适用 -->\nRefs #2\n"
        if gate.check(w_nf_na, h_docs, "", base, str(repo)):
            fails.append(f"fixtype-path：非修复轮 waiver + 不适用（docs-only）应放行：{gate.check(w_nf_na, h_docs, '', base, str(repo))}")
        errs = gate.check(w_nf_na, h_code, "", base, str(repo))
        if not any("不限于文档白名单" in e for e in errs):
            fails.append(f"fixtype-path：非修复轮 waiver + 不适用（触代码路径）应拒收：{errs}")

        # fix-r2（DS r2 boundary#4 P2）：merge-base 口径正控——落后分支上 main 侧新增的代码
        # 文件不是本分支的反向变更（实测 _tmp/20261114-3074-fixr2/ds4-twopoint-threepoint-probe.txt：
        # 两点 diff 列 src/new_code.py + state/board.md，merge-base 基点只列 state/board.md）。
        # 旧两点口径会把纯 docs 分支误报「不限于文档白名单」（必须 rebase 才能过＝假 FAIL）
        git("checkout", "-q", "-b", "docs-behind", base)
        h_behind = commit("state/board.md", "b2\n", "docs on behind branch")
        git("checkout", "-q", "main")
        main_tip = commit("src/late.py", "x = 1\n", "main advanced after branch")
        if gate.check(fixbody(h_behind), h_behind, "", main_tip, str(repo)):
            fails.append(f"fix-r2 DS#4 正控：落后分支 docs-only + 不适用 应放行（merge-base 基点不吃 main "
                         f"侧新增代码文件）：{gate.check(fixbody(h_behind), h_behind, '', main_tip, str(repo))}")
        # merge-base 起不来：base/head 未知提交 → fail-closed 分条报「算不出 merge-base」
        errs = gate.check(fixbody(h_behind), h_behind, "", "f" * 40, str(repo))
        if not any("算不出 merge-base" in e for e in errs):
            fails.append(f"fix-r2 DS#4 负控：base 未知提交应分条报「算不出 merge-base」（fail-closed）：{errs}")
        # diff 为空（base==head，merge-base=head → 空 diff）→ 分条报「diff 为空」，不再并进
        # 「不限于文档白名单」误导定位
        errs = gate.check(fixbody(main_tip), main_tip, "", main_tip, str(repo))
        if not any("diff 为空" in e for e in errs) or any("不限于文档白名单" in e for e in errs):
            fails.append(f"fix-r2 DS#4 负控：空 diff 应分条报「diff 为空」而非白名单矛盾：{errs}")


if __name__ == "__main__":
    sys.exit(main())
