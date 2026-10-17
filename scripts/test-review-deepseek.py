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
import time
from pathlib import Path

SCRIPT = Path(__file__).resolve().parent / "review-deepseek.py"
FAKE_KEY = "sk-FAKE0123456789"


class Fake(http.server.BaseHTTPRequestHandler):
    reply: tuple[int, bytes] = (200, b"")
    during = None  # 请求处理中执行的回调（模拟并发写入者）
    location = None  # 设了就回 302 跳到这里
    drip = 0.0  # >0 时逐字节慢滴
    raw_mode = None  # "headers"：慢滴响应头；"chunked"：慢滴 chunked 正文（都按每段 <1 s 间隔）
    last_body = b""

    def do_POST(self):
        Fake.last_body = self.rfile.read(int(self.headers.get("Content-Length", 0)))
        if Fake.raw_mode:
            try:
                if Fake.raw_mode == "headers":
                    self.wfile.write(b"HTTP/1.1 200 OK\r\n")
                    for i in range(40):
                        self.wfile.write(f"X-Slow-{i}: 1\r\n".encode())
                        self.wfile.flush()
                        time.sleep(0.2)
                else:
                    self.wfile.write(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n")
                    if Fake.raw_mode == "trailer":  # 正文一次给完，慢滴的是结尾 trailer 行
                        self.wfile.write(b"2\r\n{}\r\n0\r\n")
                    for i in range(40):
                        self.wfile.write(f"X-T{i}: 1\r\n".encode() if Fake.raw_mode == "trailer" else b"1\r\n \r\n")
                        self.wfile.flush()
                        time.sleep(0.2)
            except (BrokenPipeError, ConnectionResetError):
                pass
            self.close_connection = True
            return
        if Fake.during:
            Fake.during()
        code, body = Fake.reply
        self.send_response(302 if Fake.location else code)
        if Fake.location:
            self.send_header("Location", Fake.location)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        if Fake.drip:
            try:
                for b in body:
                    self.wfile.write(bytes([b]))
                    self.wfile.flush()
                    time.sleep(Fake.drip)
            except (BrokenPipeError, ConnectionResetError):
                pass  # 客户端按总时限先断开
        else:
            self.wfile.write(body)

    def log_message(self, *_):
        pass


class Sink(http.server.BaseHTTPRequestHandler):
    """重定向目标：只记录收到的 Authorization。"""
    auth: list = []

    def do_GET(self):
        Sink.auth.append(self.headers.get("Authorization"))
        self.send_response(200)
        self.send_header("Content-Length", "2")
        self.end_headers()
        self.wfile.write(b"{}")
    do_POST = do_GET

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
    srv = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Fake)  # 慢滴流用例不堵后续请求
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    api = f"http://127.0.0.1:{srv.server_port}/"
    fails = 0

    def case(name, reply, expect_code, *extra, key=FAKE_KEY, expect_out=None, pre=None, isolated=False, during=None, expect_msg=None,
             secret=None, check=None, not_msg=()):
        secret = secret or (key.strip() if isinstance(key, str) and len(key.strip()) > 4 else FAKE_KEY)
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
            bad += [f"诊断不该含「{m}」" for m in not_msg if m in r.stderr]
            if expect_code != 0 and got not in (None, "OLD"):
                bad.append(f"失败却写了 out={got!r}")
            if check and not check(tmp):
                bad.append("受保护文件被改")
            if secret and secret[:12] in (r.stdout + r.stderr):
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
         pre=lambda t: (t / ".env.local").write_text("OLD"), check=lambda t: (t / ".env.local").read_text() == "OLD")
    case(".env.local 来源的密钥被回显", (200, ok_body(f"echo {FAKE_KEY}")), 1, key=None, isolated=True,
         pre=lambda t: (t / ".env.local").write_text(f"DEEPSEEK_API_KEY={FAKE_KEY}\n"))
    case("--out 是输入的硬链接", (200, ok_body("x")), 2, "--input", "in.txt", "--force",
         pre=lambda t: ((t / "in.txt").write_text("OLD"), os.link(t / "in.txt", t / "out.md")), expect_out="OLD")
    case("拒绝外发 .env 的软链接", (200, ok_body("x")), 2, "--input", "material.txt",
         pre=lambda t: ((t / ".env.local").write_text("X=1"), (t / "material.txt").symlink_to(t / ".env.local")))
    case("拒绝外发 .env 的硬链接", (200, ok_body("x")), 2, "--input", "material.txt",
         pre=lambda t: ((t / ".env.local").write_text("X=1"), os.link(t / ".env.local", t / "material.txt")), isolated=True)
    case("密钥首尾换行被去掉", (200, ok_body("x")), 0, key="sk-FAKE\n")
    case("密钥含内部空白", (200, ok_body("x")), 2, key="sk-FA KE")
    case("无密钥", (200, ok_body("x")), 2, key=None, isolated=True)
    case(".env.local 带 export 与注释", (200, ok_body("x")), 0, key=None, isolated=True, secret="sk-FAKE",
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
    sink = http.server.HTTPServer(("127.0.0.1", 0), Sink)
    threading.Thread(target=sink.serve_forever, daemon=True).start()
    Fake.location = f"http://localhost:{sink.server_port}/steal"
    case("302 重定向不跟随、不外带密钥", (200, ok_body("x")), 1, expect_msg="HTTP 302")
    Fake.location = None
    ok = Sink.auth == []
    print(f"{'PASS' if ok else 'FAIL'} 重定向目标未收到请求 {Sink.auth}")
    fails += not ok
    sink.shutdown()
    Fake.drip = 0.2
    started = time.monotonic()
    case("慢滴流超总时限", (200, ok_body("x" * 40)), 1, "--timeout", "1", expect_msg="总时限")
    elapsed = time.monotonic() - started
    started = time.monotonic()
    case("错误正文慢滴超总时限", (401, b"x" * 40), 1, "--timeout", "1", expect_msg="总时限")
    elapsed_err = time.monotonic() - started
    Fake.drip = 0.0
    times = []
    for mode in ("headers", "chunked", "trailer"):
        Fake.raw_mode = mode
        started = time.monotonic()
        case(f"慢滴{mode}超总时限", (200, b""), 1, "--timeout", "1", expect_msg="总时限")
        times.append(time.monotonic() - started)
    Fake.raw_mode = None
    ok = all(t < 5 for t in times)
    print(f"{'PASS' if ok else 'FAIL'} 响应头/chunked 慢滴按总时限返回 {[round(t, 1) for t in times]}")
    fails += not ok
    case("--max-tokens 透传", (200, ok_body("x")), 0, "--max-tokens", "8192")
    ok = json.loads(Fake.last_body).get("max_tokens") == 8192
    case("不给 --max-tokens 不发该字段", (200, ok_body("x")), 0)
    for bad in ("0", "-1"):
        case(f"--max-tokens {bad} 拒绝", (200, ok_body("x")), 2, "--max-tokens", bad, expect_msg="正整数")
    ok = ok and "max_tokens" not in json.loads(Fake.last_body)
    print(f"{'PASS' if ok else 'FAIL'} max_tokens 请求体")
    fails += not ok
    case("默认关思考", (200, ok_body("x")), 0)
    ok = json.loads(Fake.last_body).get("thinking") == {"type": "disabled"}
    case("--thinking enabled 透传", (200, ok_body("x")), 0, "--thinking", "enabled")
    ok = ok and json.loads(Fake.last_body).get("thinking") == {"type": "enabled"}
    print(f"{'PASS' if ok else 'FAIL'} thinking 请求体")
    fails += not ok
    burnt = {"completion_tokens": 16, "completion_tokens_details": {"reasoning_tokens": 16}}
    case("关思考仍有推理 token → 提示字段可能被忽略", (200, ok_body("", finish="length", usage=burnt)), 1,
         expect_msg="可能被该模型忽略")
    case("开思考耗尽 → 提示关思考", (200, ok_body("", finish="length", usage=burnt)), 1, "--thinking", "enabled",
         expect_msg="用 --thinking disabled")
    zero = {"completion_tokens": 0, "completion_tokens_details": {"reasoning_tokens": 0}}
    for name, body in [("开思考、stop、零推理", ok_body("", finish="stop", usage=zero)),
                       ("开思考、缺 usage", ok_body("", finish="length", usage=None))]:
        case(f"{name} → 中性空回复诊断", (200, body), 1, "--thinking", "enabled", expect_msg="空回复",
             not_msg=("思考耗尽", "可能被该模型忽略"))
    ok = elapsed < 5 and elapsed_err < 5  # 整包滴完要 8 s 以上；总时限 1 s 应在几秒内返回
    print(f"{'PASS' if ok else 'FAIL'} 慢滴流按总时限及时返回 正文={elapsed:.1f}s 错误正文={elapsed_err:.1f}s")
    fails += not ok
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

    # stdout 消费者提前关闭：已发布的结果仍以 0 退出
    with tempfile.TemporaryDirectory() as d:
        tmp = Path(d)
        (tmp / "task.md").write_text("审一下", encoding="utf-8")
        Fake.reply, Fake.during = (200, ok_body("OK")), None
        env = dict(os.environ, DEEPSEEK_API_KEY=FAKE_KEY)
        proc = subprocess.Popen([sys.executable, "-I", str(SCRIPT), "--prompt", str(tmp / "task.md"), "--out",
                                 str(tmp / "out.md"), "--api", api, "--timeout", "5"],
                                stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=env, cwd=tmp)
        proc.stdout.close()
        err = proc.stderr.read().decode()
        code = proc.wait()
        ok = code == 0 and (tmp / "out.md").read_text() == "OK" and "Traceback" not in err
        print(f"{'PASS' if ok else 'FAIL'} stdout 管道关闭 exit={code} {err.strip()[-120:]!r}")
        fails += not ok

    # 有界读取：每次 read 都带剩余预算 + 1，多文件时预算递减
    sizes, real_open = [], open
    class Rec:
        def __init__(self, f): self.f = f
        def __enter__(self): return self
        def __exit__(self, *a): self.f.close()
        def read(self, n=-1):
            sizes.append(n)
            return self.f.read(n)
    with tempfile.TemporaryDirectory() as d:
        tmp = Path(d)
        (tmp / "p.md").write_bytes(b"a" * 10)
        (tmp / "i.txt").write_bytes(b"b" * 20)
        rd.open = lambda p, mode="r": Rec(real_open(p, mode))
        try:
            rd.build_body(type("A", (), {"prompt": tmp / "p.md", "input": [tmp / "i.txt"], "model": "m", "max_tokens": None, "thinking": "disabled"})())
        finally:
            del rd.open
    want = [rd.MAX_INPUT_BYTES + 1, rd.MAX_INPUT_BYTES - 10 + 1]
    ok = sizes == want
    print(f"{'PASS' if ok else 'FAIL'} 有界读取 read 参数={sizes}")
    fails += not ok

    # worktree 回退：git 公共目录的上级（主仓根）也在 .env.local 搜索路径里
    real_run = rd.subprocess.run
    rd.subprocess.run = lambda *a, **k: type("R", (), {"stdout": "/main/repo/.git\n"})()
    try:
        files = rd.env_files()
    finally:
        rd.subprocess.run = real_run
    ok = files[-1] == Path("/main/repo/.env.local") and files[0] == rd.ROOT / ".env.local"
    print(f"{'PASS' if ok else 'FAIL'} worktree 回退主仓 .env.local {files}")
    fails += not ok

    # 不支持硬链接的文件系统：不原地写 --out，回复保留在临时文件，已有目标不动
    import errno
    for name, pre_exists in (("无硬链接时不发布、回复保留", False), ("无硬链接且目标已存在", True)):
        with tempfile.TemporaryDirectory() as d:
            out = Path(d) / "out.md"
            if pre_exists:
                out.write_text("OLD")
            real_link = os.link
            os.link = lambda *_: (_ for _ in ()).throw(OSError(errno.ENOTSUP, "no hardlinks"))
            try:
                rd.write_atomic(out, "TEXT", False)
                err = None
            except Exception as e:
                err = e
            finally:
                os.link = real_link
            kept = list(Path(d).glob(".out.md.*"))
            ok = (isinstance(err, rd.Fail) and "不支持硬链接" in str(err) and len(kept) == 1
                  and kept[0].read_text() == "TEXT" and str(kept[0]) in str(err)
                  and (out.read_text() == "OLD" if pre_exists else not out.exists()))
            print(f"{'PASS' if ok else 'FAIL'} {name} err={err} kept={kept}")
            fails += not ok

    # 发布 × 清理故障注入：发布成功后清理失败不改结果；发布失败时保留原始错误
    real_unlink, real_link = Path.unlink, os.link
    def bad_unlink(self, missing_ok=False):
        raise IsADirectoryError("injected-cleanup")
    class BrokenStream:
        def write(self, *_): raise BrokenPipeError("stderr closed")
        def flush(self): raise BrokenPipeError("stderr closed")
        def fileno(self): return os.dup(2)
    for name, link_fails in (("发布成功、清理失败", False), ("发布失败、清理失败", True)):
        with tempfile.TemporaryDirectory() as d:
            out = Path(d) / "out.md"
            Path.unlink = bad_unlink
            real_stderr, sys.stderr = sys.stderr, BrokenStream()
            if link_fails:
                os.link = lambda *_: (_ for _ in ()).throw(PermissionError("link"))
            try:
                rd.write_atomic(out, "TEXT", False)
                err = None
            except Exception as e:  # 非 Fail 异常（例如警告输出失败）也判 FAIL
                err = e
            finally:
                Path.unlink, os.link, sys.stderr = real_unlink, real_link, real_stderr
            ok = (err is None and out.read_text() == "TEXT") if not link_fails else \
                 (isinstance(err, rd.Fail) and err.code == 1 and "PermissionError" in str(err) and not out.exists())
            print(f"{'PASS' if ok else 'FAIL'} {name} err={err}")
            fails += not ok

    srv.shutdown()
    print(f"test-review-deepseek: {'OK' if not fails else f'{fails} FAIL'}")
    return 1 if fails else 0


if __name__ == "__main__":
    sys.exit(main())
