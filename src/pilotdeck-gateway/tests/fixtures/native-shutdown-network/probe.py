# anchor-selfcheck: unsupported standalone real-network product probe; no source mutations
"""非冻结真实原生网络关闭探针；不构建、不读取真实凭据、不调用 provider。

运行：PDRS_SOURCE_MANIFEST_SHA256=<固定源码清单SHA256> python3 probe.py /绝对路径/pilotdeck-gateway /绝对路径/sidecar /新证据目录
范围：真实 explicit native listener；SIGTERM 驱动 close，三类连接保持到退出。
P2 验收证据缺口，不据此断言产品缺陷：已有 native_ws/tests.rs:1115
覆盖半握手，:1327 附近覆盖模拟慢写；native-process/process.mts:157
覆盖真实信号，尚无这三类连接同时存在并立即重绑端口的组合。
每个新验收 head 必须绑定固定 bin 重新运行；历史诊断不累计成绩。
本用例验证不读 WS 输出的连接，不声称已制造内核发送缓冲区饱和。
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import signal
import socket
import struct
import subprocess
import time


def masked(opcode, payload):
    mask = b"\x01\x02\x03\x04"
    size = len(payload)
    head = bytes([0x80 | opcode])
    head += (bytes([0x80 | size]) if size < 126
             else b"\xfe" + struct.pack("!H", size))
    return head + mask + bytes(b ^ mask[i % 4] for i, b in enumerate(payload))


def header(sock):
    result = bytearray()
    while not result.endswith(b"\r\n\r\n"):
        chunk = sock.recv(1)
        assert chunk, "HTTP head ended prematurely"
        result.extend(chunk)
        assert len(result) <= 16384, "bounded HTTP response head"
    return bytes(result)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("gateway", type=Path)
    parser.add_argument("sidecar", type=Path)
    parser.add_argument("evidence", type=Path)
    args = parser.parse_args()
    gateway, sidecar = args.gateway.resolve(strict=True), args.sidecar.resolve(strict=True)
    out = args.evidence.absolute()
    out.mkdir()  # 独占创建；保留成功和失败原件，不自动清理。
    home, project = out / "home", out / "project"
    home.mkdir()
    project.mkdir()
    (home / "pilotdeck.yaml").write_text(
        "schemaVersion: 1\nagent: {model: test/test}\nmodel:\n  providers:\n"
        "    test:\n      protocol: openai\n      url: http://127.0.0.1:1\n"
        "      apiKey: test-only\n      models:\n        test: {}\n")
    token = home / "token"
    token.write_text("shutdown-network-fixture\n")
    argv = [str(gateway), "--native", "--home", str(home), "--project", str(project),
            "--listen", "127.0.0.1:0", "--token-file", str(token),
            "--sidecar", str(sidecar), "--drain-ms", "500"]
    source_manifest = os.environ.get("PDRS_SOURCE_MANIFEST_SHA256", "")
    assert re.fullmatch(r"[0-9a-f]{64}", source_manifest), "bind the frozen build source manifest"
    record = {"sourceManifestSha256": source_manifest, "argv": argv, "scope": "SIGTERM + three retained TCP connections",
              "binaries": {str(p): hashlib.sha256(p.read_bytes()).hexdigest()
                           for p in (gateway, sidecar)}}
    sockets = []
    child = None
    try:
        with (out / "stdout").open("xb") as stdout, (out / "stderr").open("xb") as stderr:
            child = subprocess.Popen(
                argv, cwd=project, stdin=subprocess.DEVNULL, stdout=stdout, stderr=stderr,
                env={"PATH": os.defpath, "HOME": str(home), "PILOT_HOME": str(home),
                     "TMPDIR": str(out), "LANG": "C"})
            record["pid"] = child.pid
            deadline = time.monotonic() + 120
            while True:
                text = (out / "stderr").read_bytes()
                ready = re.search(rb"(?:^|\n)native listening 127\.0\.0\.1:(\d+)\r?\n", text)
                if ready:
                    break
                assert child.poll() is None, "child exited before readiness"
                assert time.monotonic() < deadline, "readiness deadline"
                time.sleep(0.02)
            address = ("127.0.0.1", int(ready[1]))
            record["address"] = address

            def connect():
                sock = socket.create_connection(address, timeout=5)
                sockets.append(sock)
                return sock

            half = connect()
            half.sendall(b"GET /ws HTTP/1.1\r\nHost: localhost\r\n")
            slow = connect()
            # /health does not consume bodies and responds immediately.
            # This authenticated write route actually awaits its JSON body.
            slow.sendall(b"POST /api/web/projects/fixture/files/write HTTP/1.1\r\n"
                         b"Host: localhost\r\nAuthorization: Bearer shutdown-network-fixture\r\n"
                         b"Content-Type: application/json\r\n"
                         b"Content-Length: 65536\r\n\r\n{")
            unread = connect()
            unread.sendall(b"GET /ws HTTP/1.1\r\nHost: localhost\r\n"
                           b"Connection: Upgrade\r\nUpgrade: websocket\r\n"
                           b"Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n")
            assert header(unread).startswith(b"HTTP/1.1 101 "), "real WS upgrade barrier"
            unread.sendall(masked(1, json.dumps({
                "type": "hello", "protocolVersion": "1.2", "clientName": "ui",
                "clientVersion": "shutdown-probe", "token": "shutdown-network-fixture"
            }).encode()) + masked(9, b"unread-output"))
            # 升级之后不读取该连接；所有客户端句柄保留到退出、重绑之后。
            for sock in (half, slow):
                sock.setblocking(False)
                try:
                    observed = sock.recv(1, socket.MSG_PEEK)
                except BlockingIOError:
                    continue
                raise AssertionError(f"pending connection already responded/closed: {observed!r}")
            started = time.monotonic()
            child.send_signal(signal.SIGTERM)
            record["exitCode"] = child.wait(timeout=60)
            record["shutdownSeconds"] = time.monotonic() - started
            with socket.socket() as rebound:
                rebound.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
                rebound.bind(address)
                rebound.listen(1)
            record["immediateRebind"] = True
            assert record["exitCode"] == 1, "retained connections must report drain failure"
            assert b"native stopping\n" in (out / "stderr").read_bytes(), "signal reached shutdown"
            record["result"] = "PASS"
    except BaseException as error:
        record.update(result="FAIL", error=repr(error))
        raise
    finally:
        if child is not None and child.poll() is None:
            child.kill()
            child.wait(timeout=10)
            record["forcedCleanup"] = True
        if child is not None:
            record["exitCode"] = child.returncode
        for sock in sockets:
            sock.close()
        (out / "record.json").write_text(json.dumps(record, ensure_ascii=False, indent=2) + "\n")


if __name__ == "__main__":
    main()
