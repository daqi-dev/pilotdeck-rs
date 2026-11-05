#!/usr/bin/env python3
"""test-review-minimax.py — review-minimax.py 的边界测试：本机假 API，不联网、不用真密钥。

用法：python3 -I scripts/test-review-minimax.py
覆盖：评审契约（VERDICT/覆盖行）、stop_reason 三态、空回复/thinking 归因、模型身份、
密钥回显、.env 拒发、--out 保护（含 --force 并发改写）、输入上限/为空、非本机 --api、HTTP 错误。
"""
from __future__ import annotations

import http.server
import json
import os
import subprocess
import sys
import tempfile
import threading
from pathlib import Path

SCRIPT = Path(__file__).resolve().parent / "review-minimax.py"
FAKE_KEY = "sk-FAKE0123456789"
MODEL = "MiniMax-M3.1-Flash"
VERDICT = "## 发现\n无。\n覆盖: 读完 diff 全文\nVERDICT: P0=0 P1=0 P2=0 P3=0\n"
_MISSING = object()


class Fake(http.server.BaseHTTPRequestHandler):
    reply: tuple[int, bytes] = (200, b"")
    during = None  # 请求处理中执行的回调（模拟并发写者）
    last_body = b""
    last_key = ""

    def do_POST(self):
        Fake.last_body = self.rfile.read(int(self.headers.get("Content-Length", 0)))
        Fake.last_key = self.headers.get("x-api-key", "")
        if Fake.during:
            Fake.during()
        code, body = Fake.reply
        self.send_response(code)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        try:
            self.wfile.write(body)
        except (BrokenPipeError, ConnectionResetError):
            pass

    def log_message(self, *_):
        pass


def resp(content="ok", finish="end_turn", model=MODEL, blocks=None):
    """构造 Anthropic Messages 响应；finish=_MISSING 表示整条 stop_reason 字段缺失。"""
    body = list(blocks or [])
    if content is not None:
        body.append({"type": "text", "text": content})
    doc = {"model": model, "type": "message", "role": "assistant", "content": body,
           "usage": {"input_tokens": 1, "output_tokens": 2}}
    if finish is not _MISSING:
        doc["stop_reason"] = finish
    return json.dumps(doc).encode()


def run(tmp: Path, api: str, *extra, key=FAKE_KEY, out="out.md", isolated=False, prompt="task.md"):
    script = SCRIPT
    if isolated:  # 拷进临时树：ROOT 变成 tmp，脚本不会去读真实仓根的 .env.local
        script = tmp / "scripts" / SCRIPT.name
        script.parent.mkdir(parents=True, exist_ok=True)
        script.write_text(SCRIPT.read_text(encoding="utf-8"), encoding="utf-8")
    env = {k: v for k, v in os.environ.items() if k != "MINIMAX_API_KEY"}
    if key is not None:
        env["MINIMAX_API_KEY"] = key
    cmd = [sys.executable, "-I", str(script), "--prompt", str(tmp / prompt), "--out", str(tmp / out),
           "--timeout", "5", *extra]
    if api is not None:
        cmd += ["--api", api]
    return subprocess.run(cmd, capture_output=True, text=True, env=env, cwd=tmp)


def main() -> int:
    srv = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Fake)
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    api = f"http://127.0.0.1:{srv.server_port}/"
    fails = 0

    def case(name, reply, expect_code, *extra, key=FAKE_KEY, expect_out=None, pre=None,
             during=None, expect_msg=None, not_msg=(), isolated=False, api_override=None,
             prompt_text="评审这段代码"):
        nonlocal fails
        Fake.reply, Fake.during, Fake.last_body, Fake.last_key = reply, during, b"", ""
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            (tmp / "task.md").write_text(prompt_text, encoding="utf-8")
            if pre:
                pre(tmp)
            p = run(tmp, api if api_override is None else api_override, *extra, key=key, isolated=isolated)
            out = tmp / "out.md"
            errs = []
            if p.returncode != expect_code:
                errs.append(f"exit={p.returncode} 期望 {expect_code}")
            if expect_out is not None:
                if not out.exists():
                    errs.append("out.md 未生成")
                elif out.read_text(encoding="utf-8") != expect_out:
                    errs.append(f"out.md 内容不符：{out.read_text(encoding='utf-8')[:80]!r}")
            elif out.exists():
                errs.append("out.md 不该生成")
            blob = p.stdout + p.stderr
            if expect_msg and expect_msg not in blob:
                errs.append(f"输出缺 {expect_msg!r}：{blob[:200]!r}")
            for bad in not_msg:
                if bad in blob:
                    errs.append(f"输出不该含 {bad!r}")
            if FAKE_KEY in blob:
                errs.append("输出泄漏了密钥")
        print(f"{'PASS' if not errs else 'FAIL'} {name}" + ("" if not errs else "：" + "；".join(errs)))
        fails += 0 if not errs else 1
        Fake.during = None

    # —— 评审契约（P1：exit 0 必须等于「评审真的做完了」）——
    case("契约齐全 → 0 且落盘", (200, resp(VERDICT)), 0, expect_out=VERDICT)
    case("自由文本 → 1 且不落盘", (200, resp("看起来没问题")), 1, expect_msg="末行不是 VERDICT")
    case("缺覆盖行 → 1", (200, resp("结论。\nVERDICT: P0=0 P1=0 P2=0 P3=0\n")), 1, expect_msg="覆盖")
    case("VERDICT 缺计数 → 1", (200, resp("覆盖: x\nVERDICT: P0=0 P1=0\n")), 1, expect_msg="缺等级计数")
    case("--no-require-verdict 放行自由文本", (200, resp("收到")), 0, "--no-require-verdict", expect_out="收到")

    # —— 响应完整性 ——
    case("缺 stop_reason → 1", (200, resp(VERDICT, finish=_MISSING)), 1, expect_msg="缺 stop_reason")
    case("stop_reason=null → 1（三态一致：null 也不算完整）", (200, resp(VERDICT, finish=None)), 1,
         expect_msg="null")
    case("响应不是 JSON → 1", (200, b"<html>portal</html>"), 1, expect_msg="不是 JSON")
    case("响应超 16 MiB → 1", (200, b" " * (16 * 1024 * 1024 + 1)), 1, expect_msg="响应超过")
    case("VERDICT 用全角数字 → 1（只认 ASCII 数字）", (200, resp("覆盖: x\nVERDICT: P0=０ P1=0 P2=0 P3=0")), 1,
         expect_msg="缺等级计数")
    case("stop_reason=max_tokens → 1", (200, resp(VERDICT, finish="max_tokens")), 1, expect_msg="未完成")
    case("空回复带 thinking → 1 且提示思考吃额度",
         (200, resp(None, finish="max_tokens", blocks=[{"type": "thinking", "thinking": "…"}])), 1,
         expect_msg="含 thinking 块=True")
    case("content 为空数组 → 1", (200, resp(None)), 1, expect_msg="响应无 content")
    case("模型不一致 → 1", (200, resp(VERDICT, model="其他模型")), 1, expect_msg="与请求")
    case("回复回显密钥 → 1", (200, resp(VERDICT + FAKE_KEY)), 1, expect_msg="回显了密钥")
    case("HTTP 500 → 1", (500, b'{"error":"boom"}'), 1, expect_msg="HTTP 500")

    # —— 前置与路径保护 ——
    case("--out 已存在且无 --force → 2 且保留旧文件", (200, resp(VERDICT)), 2,
         pre=lambda t: (t / "out.md").write_text("OLD", encoding="utf-8"), expect_out="OLD",
         expect_msg="已存在")
    case("--force 覆盖未被改动的旧文件 → 0", (200, resp(VERDICT)), 0, "--force",
         pre=lambda t: (t / "out.md").write_text("OLD", encoding="utf-8"), expect_out=VERDICT)
    case("--api 非本机 → 2", (200, resp(VERDICT)), 2, api_override="http://example.com/",
         expect_msg="只接受本机测试地址")
    case("--api 无尾斜杠本机地址 → 0", (200, resp(VERDICT)), 0, api_override=api.rstrip("/"),
         expect_out=VERDICT)
    case("空输入 → 2", (200, resp(VERDICT)), 2, prompt_text="", expect_msg="输入为空")

    # --force + 请求期间别人改写同一 --out：后跑者不得静默销毁先跑者证据
    Fake.reply = (200, resp(VERDICT))
    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td)
        (tmp / "task.md").write_text("评审这段代码", encoding="utf-8")
        out = tmp / "out.md"
        out.write_text("OLD", encoding="utf-8")
        Fake.during = lambda: out.write_text("别人写的", encoding="utf-8")
        p = run(tmp, api, "--force")
        Fake.during = None
        errs = []
        if p.returncode != 1:
            errs.append(f"exit={p.returncode} 期望 1")
        if out.read_text(encoding="utf-8") != "别人写的":
            errs.append("别人的内容被覆盖了")
        if "被别人创建或改动" not in p.stderr:
            errs.append(f"缺拒绝说明：{p.stderr[:160]!r}")
        print(f"{'PASS' if not errs else 'FAIL'} --force 请求期间被改写 → 1 且保留对方内容"
              + ("" if not errs else "：" + "；".join(errs)))
        fails += 0 if not errs else 1

    # 输入超过 2 MiB（走 --input）
    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td)
        (tmp / "task.md").write_text("评审", encoding="utf-8")
        (tmp / "big.patch").write_text("x" * (2 * 1024 * 1024 + 1), encoding="utf-8")
        Fake.reply = (200, resp(VERDICT))
        p = run(tmp, api, "--input", str(tmp / "big.patch"))
        ok = p.returncode == 2 and "超过上限" in p.stderr
        print(f"{'PASS' if ok else 'FAIL'} 输入超过 2 MiB → 2" + ("" if ok else f"：exit={p.returncode} {p.stderr[:160]!r}"))
        fails += 0 if ok else 1

    # —— 密钥与 .env ——
    case("缺密钥 → 2", (200, resp(VERDICT)), 2, key=None, isolated=True, expect_msg="缺 MINIMAX_API_KEY")
    case("密钥含空白 → 2", (200, resp(VERDICT)), 2, key="sk-abc def", isolated=True,
         expect_msg="含空白或非法字符")

    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td)
        (tmp / "task.md").write_text("评审", encoding="utf-8")
        (tmp / ".env.local").write_text(f'MINIMAX_API_KEY="{FAKE_KEY}"  # local\n', encoding="utf-8")
        Fake.reply = (200, resp(VERDICT))
        p = run(tmp, api, key=None, isolated=True)
        ok = p.returncode == 0 and (tmp / "out.md").exists()
        print(f"{'PASS' if ok else 'FAIL'} .env.local 取密钥（引号+行尾注释）→ 0"
              + ("" if ok else f"：exit={p.returncode} {p.stderr[:160]!r}"))
        fails += 0 if ok else 1

    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td)
        (tmp / "task.md").write_text("评审", encoding="utf-8")
        envfile = tmp / ".env.local"
        envfile.write_text(f"MINIMAX_API_KEY={FAKE_KEY}\n", encoding="utf-8")
        p = run(tmp, api, "--input", str(envfile), key=None, isolated=True)
        ok = p.returncode == 2 and "拒绝外发" in p.stderr
        print(f"{'PASS' if ok else 'FAIL'} 把 .env.local 当 --input → 2" + ("" if ok else f"：exit={p.returncode} {p.stderr[:160]!r}"))
        fails += 0 if ok else 1

    srv.shutdown()

    # build_body 的读取期间改动守卫（脚本内部函数，直接调）
    import importlib.util
    spec = importlib.util.spec_from_file_location("rm", SCRIPT)
    rm = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(rm)
    with tempfile.TemporaryDirectory() as d:
        p = Path(d) / "p.md"
        p.write_bytes(b"abcd")
        real_open = open
        class Mut:
            def __init__(self, path): self.path, self.f = path, real_open(path, "rb")
            def __enter__(self): return self
            def __exit__(self, *a): self.f.close()
            def read(self, n=-1):
                data = self.f.read(n)
                self.path.write_bytes(b"abcdef")  # 模拟并发写者把材料改长
                return data
        rm.open = lambda path, mode="rb": Mut(Path(path))
        try:
            rm.build_body(type("A", (), {"prompt": p, "input": [], "model": "m", "max_tokens": None})())
            err = None
        except Exception as e:
            err = e
        finally:
            del rm.open
        ok = isinstance(err, rm.Fail) and "读取期间被改动" in str(err)
        print(f"{'PASS' if ok else 'FAIL'} build_body 读取期间输入被改动 err={err}")
        fails += 0 if ok else 1

    print(f"test-review-minimax: {fails} 处失败")
    return 1 if fails else 0


if __name__ == "__main__":
    sys.exit(main())
