#!/usr/bin/env python3
"""Grok Bot 本机调用（Cursor 个人 key，直连 api.cursor.com）。

用法：
  python3 scripts/grokbot-call.py --bot "Grok Bot" --text-file prompt.md [--timeout 300] [--json]
  echo "..." | python3 scripts/grokbot-call.py --bot safe
  python3 scripts/grokbot-call.py --bot "代码分析" --read [--since-seq N] [--json]   # 只读历史

密钥取环境变量 CURSOR_ACCESS_TOKEN，没有再读 `.env.local`（先当前检出根，再主仓根，
worktree 里也能找到；已在 .gitignore）。key 由 `npx -p @cursor/bdk bdk login` 浏览器
授权铸出，等价账号权限；不要提交、不要打印、不要贴聊天。

通道事实（2026-10-19 实测）：官方 App 无 CLI；第三方 gbot 0.12.4 因缺
`gateway-descriptor.json` 不可用；本脚本走的是 `${API}/v0/grokbot/*`，undocumented，
随时可能变。消息经 Cursor 云端，外发前先过权限/密钥检查。
"""

import argparse
import json
import os
import re
import subprocess
import sys
import time
import urllib.error
import urllib.request
import uuid
from pathlib import Path

API = "https://api.cursor.com"
ROOT = Path(__file__).resolve().parent.parent
KEY_NAME = "CURSOR_ACCESS_TOKEN"


class Fail(Exception):
    def __init__(self, code: int, msg: str):
        super().__init__(msg)
        self.code = code


def env_files() -> list[Path]:
    roots = [ROOT]
    try:
        common = subprocess.run(
            ["git", "-C", str(ROOT), "rev-parse", "--path-format=absolute", "--git-common-dir"],
            capture_output=True, text=True, timeout=10,
        ).stdout.strip()
        if common:
            roots.append(Path(common).parent)
    except (OSError, subprocess.SubprocessError):
        pass
    return [r / ".env.local" for r in dict.fromkeys(roots)]


def load_key() -> str:
    key = os.environ.get(KEY_NAME, "").strip()
    if not key:
        for env in env_files():
            if not env.is_file():
                continue
            try:
                lines = env.read_text(encoding="utf-8").splitlines()
            except (OSError, UnicodeError) as e:
                raise Fail(2, f"读取 {env} 失败：{type(e).__name__}") from None
            for line in lines:
                name, sep, value = line.strip().partition("=")
                name = name.removeprefix("export ").strip()
                if sep and name == KEY_NAME:
                    key = value.strip().strip("'\"")
            if key:
                break
    if not key:
        raise Fail(2, f"缺 {KEY_NAME}（环境变量或 .env.local）")
    if re.search(r"\s", key):
        raise Fail(2, f"{KEY_NAME} 含空白（内容不打印）")
    return key


def call(method: str, path: str, key: str, body: dict | None = None) -> dict:
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(
        API + path, data=data, method=method,
        headers={"Authorization": "Bearer " + key, "Content-Type": "application/json"},
    )
    try:
        with urllib.request.urlopen(req, timeout=60) as resp:
            return json.loads(resp.read())
    except urllib.error.HTTPError as e:
        detail = e.read()[:300].decode("utf-8", "replace")
        raise Fail(2, f"HTTP {e.code} {method} {path}: {detail}") from None
    except urllib.error.URLError as e:
        raise Fail(2, f"网络失败：{e.reason}") from None


def main() -> int:
    p = argparse.ArgumentParser(description="Call Grok Bot via Cursor API")
    p.add_argument("--bot", required=True, help="App 内显示的 Bot 名，原样")
    p.add_argument("--text")
    p.add_argument("--text-file")
    p.add_argument("--timeout", type=int, default=300)
    p.add_argument("--read", action="store_true", help="只读会话记录，不发送")
    p.add_argument("--since-seq", type=int, default=0, help="配合 --read，只取该 seq 之后")
    p.add_argument("--json", action="store_true")
    a = p.parse_args()

    if a.read:
        key = load_key()
        session = call("POST", "/v0/grokbot/sessions", key, {"name": a.bot})
        sid = str(session["id"])
        state = call("GET", f"/v0/grokbot/sessions/{sid}/entries?afterUpdatedSeq={a.since_seq}", key)
        entries = sorted(state.get("entries", []), key=lambda x: int(x.get("seq", 0)))
        if a.json:
            print(json.dumps({"bot": a.bot, "sessionId": sid,
                              "turn": state.get("turn"), "entries": entries},
                             ensure_ascii=False))
        else:
            for it in entries:
                label = str(it.get("kind") or "")
                if it.get("role"):
                    label += f"/{it['role']}"
                print(f"[{it.get('seq')}] {label}: {it.get('text') or ''}\n")
        return 0

    if a.text_file:
        text = Path(a.text_file).read_text(encoding="utf-8")
    elif a.text:
        text = a.text
    else:
        text = sys.stdin.read()
    if not text.strip():
        raise Fail(2, "空提示词")

    key = load_key()
    session = call("POST", "/v0/grokbot/sessions", key, {"name": a.bot})
    sid = str(session["id"])
    after = int(session["latestUpdatedSeq"])

    mid = str(uuid.uuid4())
    call("POST", f"/v0/grokbot/sessions/{sid}/messages", key, {"text": text, "messageId": mid})

    deadline = time.time() + a.timeout
    replies: list[str] = []
    seen: set[str] = set()
    while time.time() < deadline:
        time.sleep(2)
        state = call("GET", f"/v0/grokbot/sessions/{sid}/entries?afterUpdatedSeq={after}", key)
        for item in state.get("entries", []):
            seq = str(item.get("updatedSeq") or item.get("seq") or "")
            if seq in seen:
                continue
            seen.add(seq)
            if seq.isdigit():
                after = max(after, int(seq))
            if item.get("kind") == "send-message" and item.get("text"):
                replies.append(item["text"])
        turn = state.get("turn") or {}
        if replies and not turn.get("inFlight") and not turn.get("queued"):
            break

    out = {"bot": a.bot, "sessionId": sid, "messageId": mid,
           "replies": replies, "timedOut": not replies}
    if a.json:
        print(json.dumps(out, ensure_ascii=False))
    else:
        print("\n---\n".join(replies) if replies else "(超时，无回复)")
    return 0 if replies else 1


if __name__ == "__main__":
    try:
        sys.exit(main())
    except Fail as e:
        print(f"grokbot-call: {e}", file=sys.stderr)
        sys.exit(e.code)
    except KeyboardInterrupt:
        sys.exit(130)
