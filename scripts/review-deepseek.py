#!/usr/bin/env python3
"""review-deepseek.py — DeepSeek 评审通道（第三谱系），直连 API，替代 dsh。

只把输入文件的文本发给模型、把回复落盘；模型拿不到任何工具，天然只读（dsh 只能靠提示词约束）。
密钥取环境变量 DEEPSEEK_API_KEY，没有再读 `.env.local`（先当前检出根，再主仓根，worktree 里也能找到）；
本脚本不打印、不落盘密钥，错误信息里出现的密钥一律替换成 ***。

用法：
  python3 -I scripts/review-deepseek.py --prompt _tmp/<日期>-<名>/task.md [--input X.patch ...] --out _tmp/<日期>-<名>/out-deepseek.md
  --model deepseek-flash（默认）| deepseek-v4-pro · --force 允许覆盖已存在的 --out
退出码：0 有非空完整回复 · 1 API/网络/响应异常（不写 --out）· 2 参数/前置错误
"""
from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
import urllib.error
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
API = "https://api.deepseek.com/chat/completions"
MAX_INPUT_BYTES = 2 * 1024 * 1024    # 任务书 + 材料合计；超限失败，不静默截断
MAX_RESPONSE_BYTES = 16 * 1024 * 1024
KEY_RE = re.compile(r"[A-Za-z0-9._\-]+")


class Fail(Exception):
    def __init__(self, code: int, msg: str):
        super().__init__(msg)
        self.code = code


def env_files() -> list[Path]:
    roots = [ROOT]
    try:
        common = subprocess.run(["git", "-C", str(ROOT), "rev-parse", "--path-format=absolute", "--git-common-dir"],
                                capture_output=True, text=True, timeout=10).stdout.strip()
        if common:
            roots.append(Path(common).parent)
    except (OSError, subprocess.SubprocessError):
        pass
    return [r / ".env.local" for r in dict.fromkeys(roots)]


def parse_env_value(raw: str) -> str:
    """dotenv 子集：成对引号内原样；不带引号时去掉「空白+#」起的行尾注释。"""
    raw = raw.strip()
    if raw[:1] in ("'", '"'):
        end = raw.find(raw[0], 1)
        if end < 0:
            raise Fail(2, ".env.local 中 DEEPSEEK_API_KEY 引号不配对")
        return raw[1:end]
    return re.split(r"\s+#", raw, maxsplit=1)[0].strip() if not raw.startswith("#") else ""


def load_key() -> str:
    key = os.environ.get("DEEPSEEK_API_KEY", "").strip()
    if not key:
        for env in env_files():
            if not env.is_file():
                continue
            for line in env.read_text(encoding="utf-8").splitlines():
                name, sep, value = line.strip().partition("=")
                name = name.removeprefix("export ").strip()
                if sep and name == "DEEPSEEK_API_KEY":
                    key = parse_env_value(value)
            if key:
                break
    if not key:
        raise Fail(2, "缺 DEEPSEEK_API_KEY（环境变量或 .env.local）")
    if not KEY_RE.fullmatch(key):
        raise Fail(2, "DEEPSEEK_API_KEY 含空白或非法字符（内容不打印）")
    return key


def same_file(a: Path, b: Path) -> bool:
    try:
        return a.exists() and b.exists() and os.path.samefile(a, b)
    except OSError:
        return False


def check_paths(a: argparse.Namespace) -> None:
    if a.timeout <= 0:
        raise Fail(2, "--timeout 须为正数")
    for p in [a.prompt, *a.input]:
        if p.name.startswith(".env"):
            raise Fail(2, f"拒绝外发疑似密钥文件 {p}")
        if not p.is_file():
            raise Fail(2, f"找不到 {p}")
        if same_file(p, a.out) or p.resolve() == a.out.resolve():
            raise Fail(2, f"--out 与输入 {p} 是同一文件")
    if any(same_file(a.out, e) for e in env_files()) or a.out.name.startswith(".env"):
        raise Fail(2, "--out 不能指向 .env 文件")
    if a.out.exists() and not a.force:
        raise Fail(2, f"{a.out} 已存在（保护上一轮证据；确需覆盖加 --force）")


def build_body(a: argparse.Namespace) -> bytes:
    total = sum(p.stat().st_size for p in [a.prompt, *a.input])
    if total > MAX_INPUT_BYTES:
        raise Fail(2, f"输入合计 {total} 字节，超过上限 {MAX_INPUT_BYTES}；请缩小材料或分批")
    try:
        parts = [a.prompt.read_text(encoding="utf-8")]
        for p in a.input:
            parts.append(f"\n\n===== {p} =====\n{p.read_text(encoding='utf-8')}")
    except (OSError, UnicodeDecodeError) as e:
        raise Fail(2, f"读取输入失败：{type(e).__name__}") from None
    print(f"review-deepseek: 输入 {total} 字节（{1 + len(a.input)} 个文件）", file=sys.stderr)
    return json.dumps({
        "model": a.model,
        "messages": [{"role": "user", "content": "".join(parts)}],
        "stream": False,
    }, ensure_ascii=False).encode()


def call_api(a: argparse.Namespace, key: str, body: bytes) -> dict:
    req = urllib.request.Request(a.api, data=body, headers={
        "Authorization": f"Bearer {key}", "Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=a.timeout) as r:
            raw = r.read(MAX_RESPONSE_BYTES + 1)
    except urllib.error.HTTPError as e:
        try:
            detail = e.read(500).decode(errors="replace")
        except Exception:  # 错误正文读不出来不影响退出码
            detail = ""
        raise Fail(1, f"HTTP {e.code} {detail}") from None
    except Exception as e:  # URLError、超时、连接重置、IncompleteRead 等
        raise Fail(1, f"网络错误 {type(e).__name__}: {e}") from None
    if len(raw) > MAX_RESPONSE_BYTES:
        raise Fail(1, f"响应超过 {MAX_RESPONSE_BYTES} 字节")
    try:
        resp = json.loads(raw)
    except ValueError:
        raise Fail(1, f"响应不是 JSON：{raw[:200].decode(errors='replace')}") from None
    if not isinstance(resp, dict):
        raise Fail(1, "响应结构异常：顶层不是对象")
    return resp


def extract_text(resp: dict) -> str:
    choices = resp.get("choices")
    if not isinstance(choices, list) or not choices or not isinstance(choices[0], dict):
        raise Fail(1, f"响应无 choices：{json.dumps(resp, ensure_ascii=False)[:300]}")
    choice = choices[0]
    msg = choice.get("message")
    text = msg.get("content") if isinstance(msg, dict) else None
    if not isinstance(text, str) or not text.strip():
        raise Fail(1, f"空回复（finish_reason={choice.get('finish_reason')}）")
    if choice.get("finish_reason") not in (None, "stop"):
        raise Fail(1, f"回复未完成（finish_reason={choice.get('finish_reason')}），不当作评审结论")
    return text


def write_atomic(out: Path, text: str) -> None:
    out.parent.mkdir(parents=True, exist_ok=True)
    tmp = out.with_name(f".{out.name}.{os.getpid()}.tmp")
    try:
        fd = os.open(tmp, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o644)
        with os.fdopen(fd, "w", encoding="utf-8") as f:
            f.write(text)
        os.replace(tmp, out)
    except OSError as e:
        tmp.unlink(missing_ok=True)
        raise Fail(1, f"写 {out} 失败：{e}") from None


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--prompt", required=True, type=Path, help="评审任务书（提示词文件）")
    ap.add_argument("--input", action="append", default=[], type=Path, help="附带材料（diff、源码），可重复")
    ap.add_argument("--out", required=True, type=Path)
    ap.add_argument("--force", action="store_true", help="允许覆盖已存在的 --out")
    ap.add_argument("--model", default="deepseek-flash")
    ap.add_argument("--timeout", type=int, default=1800, help="单次 socket 操作秒数")
    ap.add_argument("--api", default=API, help=argparse.SUPPRESS)  # 测试用假服务
    a = ap.parse_args()

    key = ""
    try:
        key = load_key()
        check_paths(a)
        resp = call_api(a, key, build_body(a))
        text = extract_text(resp)
        write_atomic(a.out, text)
    except Fail as e:
        msg = str(e).replace(key, "***") if key else str(e)
        print(f"review-deepseek: {msg}", file=sys.stderr)
        return e.code
    usage = resp.get("usage") if isinstance(resp.get("usage"), dict) else {}
    print(f"review-deepseek: model={resp.get('model')} prompt_tokens={usage.get('prompt_tokens')} "
          f"completion_tokens={usage.get('completion_tokens')} → {a.out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
