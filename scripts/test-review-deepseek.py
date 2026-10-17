#!/usr/bin/env python3
"""test-review-deepseek.py — review-deepseek.py 的边界测试：本机假 API，不联网、不用真密钥。

用法：python3 -I scripts/test-review-deepseek.py
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

SCRIPT = Path(__file__).resolve().parent / "review-deepseek.py"
FAKE_KEY = "sk-FAKE0123456789"


class Fake(http.server.BaseHTTPRequestHandler):
    reply: tuple[int, bytes] = (200, b"")
    during = None  # 请求处理中执行的回调（模拟并发写入者）

    def do_POST(self):
        self.rfile.read(int(self.headers.get("Content-Length", 0)))
        if Fake.during:
            Fake.during()
        code, body = Fake.reply
        self.send_response(code)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *_):
        pass


def ok_body(content, finish="stop", model="fake", usage=None):
    return json.dumps({"model": model, "choices": [{"message": {"content": content}, "finish_reason": finish}],
                       "usage": usage}).encode()


def run(tmp: Path, api: str, *extra, key=FAKE_KEY, out="out.md", script=SCRIPT):
    env = {k: v for k, v in os.environ.items() if k != "DEEPSEEK_API_KEY"}
    if key is not None:
        env["DEEPSEEK_API_KEY"] = key
    cmd = [sys.executable, "-I", str(script), "--prompt", str(tmp / "task.md"), "--out", str(tmp / out),
           "--api", api, "--timeout", "5", *extra]
    return subprocess.run(cmd, capture_output=True, text=True, env=env, cwd=tmp)


def main() -> int:
    srv = http.server.HTTPServer(("127.0.0.1", 0), Fake)
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    api = f"http://127.0.0.1:{srv.server_port}/"
    fails = 0

    def case(name, reply, expect_code, *extra, key=FAKE_KEY, expect_out=None, pre=None, isolated=False, during=None, expect_msg=None):
        nonlocal fails
        with tempfile.TemporaryDirectory() as d:
            tmp = Path(d)
            (tmp / "task.md").write_text("审一下", encoding="utf-8")
            if pre:
                pre(tmp)
            Fake.reply = reply
            Fake.during = (lambda: during(tmp)) if during else None
            script = SCRIPT
            if isolated:  # 脚本放到非 git 目录，确保读不到真实 .env.local
                (tmp / "scripts").mkdir()
                script = tmp / "scripts" / SCRIPT.name
                script.write_text(SCRIPT.read_text(encoding="utf-8"), encoding="utf-8")
            r = run(tmp, api, *extra, key=key, script=script)
            out = tmp / "out.md"
            got = out.read_text(encoding="utf-8") if out.exists() else None
            bad = []
            if r.returncode != expect_code:
                bad.append(f"exit={r.returncode}≠{expect_code}")
            if expect_out is not None and got != expect_out:
                bad.append(f"out={got!r}")
            if "Traceback" in r.stderr or (expect_code and not r.stderr.strip().splitlines()[-1].startswith("review-deepseek:")):
                bad.append("不是受控诊断")
            if expect_msg and expect_msg not in r.stderr:
                bad.append(f"诊断缺「{expect_msg}」")
            if expect_code != 0 and got not in (None, "OLD"):
                bad.append(f"失败却写了 out={got!r}")
            if key and len(key) > 4 and key[:12] in (r.stdout + r.stderr):
                bad.append("密钥出现在输出里")
            if list(tmp.glob(".out.md.*.tmp")):
                bad.append("残留临时文件")
            print(f"{'PASS' if not bad else 'FAIL'} {name} {'; '.join(bad)} {r.stderr.strip()[-160:]!r}")
            fails += bool(bad)

    case("正常回复", (200, ok_body("结论 VERDICT: P0=0")), 0, expect_out="结论 VERDICT: P0=0")
    case("空回复", (200, ok_body("  ")), 1)
    case("content=null", (200, ok_body(None)), 1)
    case("截断 finish=length", (200, ok_body("半截", "length")), 1)
    case("choices 为空", (200, b'{"choices": []}'), 1)
    case("200 返回 HTML", (200, b"<html>portal</html>"), 1)
    case("HTTP 402", (402, b'{"error": "Insufficient Balance"}'), 1)
    case("HTTP 错误正文回显密钥", (401, f"bad key {FAKE_KEY}".encode()), 1)
    # 「HTTP 401 」9 字符 + 479 + 密钥前 12 字符 = 500：若改回先截断后脱敏，恰好漏出 12 字符
    case("错误正文里密钥跨 500 截断边界", (401, ("x" * 479 + FAKE_KEY).encode()), 1)
    case("成功回复回显密钥", (200, ok_body(f"key={FAKE_KEY}")), 1)
    case("请求期间别人写了 --out", (200, ok_body("NEW")), 1, expect_out="OLD",
         during=lambda t: (t / "out.md").write_text("OLD"))
    case("--out 是目录", (200, ok_body("x")), 2, "--out", ".")
    case("--out 父路径是普通文件", (200, ok_body("x")), 2, "--out", "task.md/x.md")
    case("model 回显密钥", (200, ok_body("OK", model=FAKE_KEY)), 1)
    case("usage 回显密钥", (200, ok_body("OK", usage={"prompt_tokens": FAKE_KEY})), 0, expect_out="OK")
    case("model 含孤立代理", (200, b'{"model": "\\ud800", "choices": [{"message": {"content": "OK"}, "finish_reason": "stop"}]}'),
         0, expect_out="OK")
    case("响应超 16 MiB", (200, b" " * (16 * 1024 * 1024 + 1)), 1, expect_msg="响应超过")
    case("--out 指向 .env（--force）", (200, ok_body("x")), 2, "--out", ".env.local", "--force",
         pre=lambda t: (t / ".env.local").write_text("OLD"))
    case("--out 是输入的硬链接", (200, ok_body("x")), 2, "--input", "in.txt", "--force",
         pre=lambda t: ((t / "in.txt").write_text("OLD"), os.link(t / "in.txt", t / "out.md")), expect_out="OLD")
    case("拒绝外发 .env 的软链接", (200, ok_body("x")), 2, "--input", "material.txt",
         pre=lambda t: ((t / ".env.local").write_text("X=1"), (t / "material.txt").symlink_to(t / ".env.local")))
    case("拒绝外发 .env 的硬链接", (200, ok_body("x")), 2, "--input", "material.txt",
         pre=lambda t: ((t / ".env.local").write_text("X=1"), os.link(t / ".env.local", t / "material.txt")), isolated=True)
    case("密钥首尾换行被去掉", (200, ok_body("x")), 0, key="sk-FAKE\n")
    case("密钥含内部空白", (200, ok_body("x")), 2, key="sk-FA KE")
    case("无密钥", (200, ok_body("x")), 2, key=None, isolated=True)
    case(".env.local 带 export 与注释", (200, ok_body("x")), 0, key=None, isolated=True,
         pre=lambda t: (t / ".env.local").write_text('# c\nexport DEEPSEEK_API_KEY="sk-FAKE" # local\n'))
    case("--out 已存在不覆盖", (200, ok_body("NEW")), 2,
         pre=lambda t: (t / "out.md").write_text("OLD"), expect_out="OLD")
    case("--force 覆盖", (200, ok_body("NEW")), 0, "--force",
         pre=lambda t: (t / "out.md").write_text("OLD"), expect_out="NEW")
    case("拒绝外发 .env", (200, ok_body("x")), 2, "--input", ".env.local",
         pre=lambda t: (t / ".env.local").write_text("X=1"))
    case("--out 等于输入", (200, ok_body("x")), 2, "--input", "out.md", "--force",
         pre=lambda t: (t / "out.md").write_text("OLD"), expect_out="OLD")
    case("输入超限", (200, ok_body("x")), 2, "--input", "big.txt",
         pre=lambda t: (t / "big.txt").write_bytes(b"a" * (2 * 1024 * 1024 + 1)))
    case("超时非正", (200, ok_body("x")), 2, "--timeout", "0")
    case("连不上", (200, b""), 1, "--api", "http://127.0.0.1:9/")

    # .env.local 解析子集：直接调函数，用假值
    sys.path.insert(0, str(SCRIPT.parent))
    import importlib.util
    spec = importlib.util.spec_from_file_location("rd", SCRIPT)
    rd = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(rd)
    for raw, want in [('"sk-FAKE" # local', "sk-FAKE"), ("sk-FAKE # note", "sk-FAKE"), ("# unset", ""),
                      ("'sk-FAKE'", "sk-FAKE"), ("", "")]:
        got = rd.parse_env_value(raw)
        ok = got == want
        print(f"{'PASS' if ok else 'FAIL'} parse_env_value({raw!r}) = {got!r}")
        fails += not ok
    try:
        rd.parse_env_value('"sk-FAKE')
        print("FAIL 引号不配对未报错")
        fails += 1
    except rd.Fail:
        print("PASS 引号不配对报错")

    # 发布 × 清理故障注入：发布成功后清理失败不改结果；发布失败时保留原始错误
    real_unlink, real_link = Path.unlink, os.link
    def bad_unlink(self, missing_ok=False):
        raise PermissionError("injected")
    for name, link_fails in (("发布成功、清理失败", False), ("发布失败、清理失败", True)):
        with tempfile.TemporaryDirectory() as d:
            out = Path(d) / "out.md"
            Path.unlink = bad_unlink
            if link_fails:
                os.link = lambda *_: (_ for _ in ()).throw(PermissionError("link"))
            try:
                rd.write_atomic(out, "TEXT", False)
                err = None
            except rd.Fail as e:
                err = e
            finally:
                Path.unlink, os.link = real_unlink, real_link
            ok = (err is None and out.read_text() == "TEXT") if not link_fails else \
                 (err is not None and err.code == 1 and "PermissionError" in str(err) and not out.exists())
            print(f"{'PASS' if ok else 'FAIL'} {name} err={err}")
            fails += not ok

    srv.shutdown()
    print(f"test-review-deepseek: {'OK' if not fails else f'{fails} FAIL'}")
    return 1 if fails else 0


if __name__ == "__main__":
    sys.exit(main())
