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
import errno
import json
import os
import re
import signal
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
API = "https://api.deepseek.com/chat/completions"
MAX_INPUT_BYTES = 2 * 1024 * 1024    # 任务书 + 材料合计；超限失败，不静默截断
MAX_RESPONSE_BYTES = 16 * 1024 * 1024
KEY_RE = re.compile(r"[A-Za-z0-9._\-]+")


def say(msg: str, stream=None) -> None:
    """尽力输出：日志流坏了（管道关闭、编码）不影响退出码；stdout 失败后改指 devnull，免得退出时刷新再报错。"""
    stream = stream or sys.stderr
    try:
        print(msg, file=stream, flush=True)
    except Exception:
        try:
            os.dup2(os.open(os.devnull, os.O_WRONLY), stream.fileno())
        except Exception:
            pass


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
            try:
                lines = env.read_text(encoding="utf-8").splitlines()
            except (OSError, UnicodeError) as e:
                raise Fail(2, f"读取 {env} 失败：{type(e).__name__}") from None
            for line in lines:
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


def is_secret(p: Path, envs: list[Path]) -> bool:
    return p.name.startswith(".env") or p.resolve().name.startswith(".env") or any(same_file(p, e) for e in envs)


def check_paths(a: argparse.Namespace) -> None:
    if a.timeout <= 0:
        raise Fail(2, "--timeout 须为正数")
    if a.max_tokens is not None and a.max_tokens <= 0:
        raise Fail(2, "--max-tokens 须为正整数")
    if not a.out.name or a.out.is_dir():
        raise Fail(2, f"--out 须是文件路径：{a.out}")
    envs = env_files()
    for p in [a.prompt, *a.input]:
        if is_secret(p, envs):
            raise Fail(2, f"拒绝外发疑似密钥文件 {p}")
        if not p.is_file():
            raise Fail(2, f"找不到 {p}")
        if same_file(p, a.out) or p.resolve() == a.out.resolve():
            raise Fail(2, f"--out 与输入 {p} 是同一文件")
    if is_secret(a.out, envs):
        raise Fail(2, "--out 不能指向 .env 文件")
    parent = a.out.parent
    while not parent.exists() and parent != parent.parent:
        parent = parent.parent
    if not parent.is_dir():
        raise Fail(2, f"--out 的上级路径 {parent} 不是目录")
    if a.out.exists() and not a.force:
        raise Fail(2, f"{a.out} 已存在（保护上一轮证据；确需覆盖加 --force）")


def build_body(a: argparse.Namespace) -> bytes:
    total, texts = 0, []
    try:
        for p in [a.prompt, *a.input]:
            with open(p, "rb") as f:  # 按剩余预算有界读取，读取期间文件增长也会被拦下
                data = f.read(MAX_INPUT_BYTES - total + 1)
            total += len(data)
            if total > MAX_INPUT_BYTES:
                raise Fail(2, f"输入合计超过上限 {MAX_INPUT_BYTES} 字节；请缩小材料或分批")
            texts.append(data.decode("utf-8"))
    except (OSError, UnicodeError) as e:
        raise Fail(2, f"读取输入失败：{type(e).__name__}") from None
    parts = [texts[0], *(f"\n\n===== {p} =====\n{t}" for p, t in zip(a.input, texts[1:]))]
    say(f"review-deepseek: 输入 {total} 字节（{1 + len(a.input)} 个文件）")
    return json.dumps({
        "model": a.model,
        "messages": [{"role": "user", "content": "".join(parts)}],
        "stream": False,
        **({"max_tokens": a.max_tokens} if a.max_tokens is not None else {}),
        "thinking": {"type": a.thinking},
    }, ensure_ascii=False).encode()


class NoRedirect(urllib.request.HTTPRedirectHandler):
    """urllib 跟随 302/303 时会把 Authorization 原样带到新主机；一律不跟，3xx 按 HTTP 错误处理。"""
    def redirect_request(self, *args, **kwargs):
        return None


OPENER = urllib.request.build_opener(NoRedirect)


class Deadline(Exception):
    pass


def _deadline(*_):
    raise Deadline


def call_api(a: argparse.Namespace, key: str, body: bytes) -> dict:
    req = urllib.request.Request(a.api, data=body, headers={
        "Authorization": f"Bearer {key}", "Content-Type": "application/json"})
    # 墙钟总时限覆盖整个请求：连接、响应头、chunked 正文、错误正文；socket 超时只管单次操作
    previous = signal.signal(signal.SIGALRM, _deadline)
    signal.setitimer(signal.ITIMER_REAL, a.timeout)
    try:
        with OPENER.open(req, timeout=a.timeout) as r:
            raw = r.read(MAX_RESPONSE_BYTES + 1)
    except Deadline:
        raise Fail(1, f"请求超过总时限 {a.timeout} 秒") from None
    except urllib.error.HTTPError as e:
        try:
            detail = e.read(64 * 1024).decode(errors="replace")  # 先整段脱敏再截断显示，见 redact()
        except Deadline:
            raise Fail(1, f"HTTP {e.code}，读取错误正文超过总时限 {a.timeout} 秒") from None
        except Exception:  # 错误正文读不出来不影响退出码
            detail = ""
        raise Fail(1, f"HTTP {e.code} {detail}") from None
    except Exception as e:  # URLError、超时、连接重置、IncompleteRead 等
        raise Fail(1, f"网络错误 {type(e).__name__}: {e}") from None
    finally:
        signal.setitimer(signal.ITIMER_REAL, 0)
        signal.signal(signal.SIGALRM, previous)
    if len(raw) > MAX_RESPONSE_BYTES:
        raise Fail(1, f"响应超过 {MAX_RESPONSE_BYTES} 字节")
    try:
        resp = json.loads(raw)
    except ValueError:
        raise Fail(1, f"响应不是 JSON：{raw[:65536].decode(errors='replace')}") from None
    if not isinstance(resp, dict):
        raise Fail(1, "响应结构异常：顶层不是对象")
    return resp


def extract_text(resp: dict, thinking: str = "disabled") -> str:
    choices = resp.get("choices")
    if not isinstance(choices, list) or not choices or not isinstance(choices[0], dict):
        raise Fail(1, f"响应无 choices：{json.dumps(resp, ensure_ascii=False)[:65536]}")
    choice = choices[0]
    msg = choice.get("message")
    text = msg.get("content") if isinstance(msg, dict) else None
    if not isinstance(text, str) or not text.strip():
        usage = resp.get("usage") if isinstance(resp.get("usage"), dict) else {}
        details = usage.get("completion_tokens_details") if isinstance(usage.get("completion_tokens_details"), dict) else {}
        head = f"空回复（finish_reason={choice.get('finish_reason')}，completion_tokens={usage.get('completion_tokens')}，" \
               f"其中 reasoning_tokens={details.get('reasoning_tokens')}）"
        reasoning = details.get("reasoning_tokens") if isinstance(details.get("reasoning_tokens"), int) else 0
        if thinking == "disabled" and reasoning > 0:
            raise Fail(1, head + "：已请求关思考仍有推理 token，thinking 字段可能被该模型忽略；换已实测支持该字段的 deepseek-flash，或调大 --max-tokens")
        # 只有「撞上限 + 推理占满输出」这组证据成立时才下「思考耗尽」的判断，其余给中性诊断
        if thinking == "enabled" and choice.get("finish_reason") == "length" and reasoning > 0 \
                and reasoning == usage.get("completion_tokens"):
            raise Fail(1, head + "：思考耗尽输出额度，用 --thinking disabled")
        raise Fail(1, head)
    if choice.get("finish_reason") not in (None, "stop"):
        raise Fail(1, f"回复未完成（finish_reason={choice.get('finish_reason')}），不当作评审结论")
    return text


def write_atomic(out: Path, text: str, force: bool) -> None:
    """完整写进同目录随机临时文件再发布；不带 --force 时用 link 发布，目标已存在即失败（不覆盖并发写入者）。
    发布成功后临时文件清理失败只警告，不改退出码；发布失败时保留原始错误。"""
    tmp = None
    try:
        out.parent.mkdir(parents=True, exist_ok=True)
        fd, name = tempfile.mkstemp(prefix=f".{out.name}.", suffix=".tmp", dir=out.parent)
        tmp = Path(name)
        with os.fdopen(fd, "w", encoding="utf-8") as f:
            f.write(text)
        if force:
            os.replace(tmp, out)
            tmp = None
            return
        try:
            os.link(tmp, out)
        except FileExistsError:
            raise Fail(1, f"{out} 在请求期间被别人创建，未覆盖；本次回复丢弃") from None
        except OSError as e:
            if e.errno not in (errno.EPERM, errno.ENOTSUP, errno.EOPNOTSUPP, errno.EXDEV):
                raise
            # 文件系统不支持硬链接：不做非原子的原地写，回复留在临时文件里交给人决定
            kept, tmp = tmp, None
            raise Fail(1, f"{out.parent} 不支持硬链接，无法原子且不覆盖地发布；回复已保留在 {kept}，确认后可改用 --force") from None
    except (OSError, UnicodeError) as e:
        raise Fail(1, f"写 {out} 失败：{type(e).__name__}") from None
    finally:
        if tmp is not None:
            try:
                tmp.unlink(missing_ok=True)
            except OSError as e:
                say(f"review-deepseek: 警告：临时文件 {tmp} 未能删除（{type(e).__name__}）")


def success_line(resp: dict, out: Path, key: str) -> str:
    """发布前构造成功日志：token 数只认整数，model 只认可编码字符串，整行不得含密钥。"""
    usage = resp.get("usage") if isinstance(resp.get("usage"), dict) else {}
    counts = {k: v if isinstance(v, int) and not isinstance(v, bool) else None
              for k, v in ((k, usage.get(k)) for k in ("prompt_tokens", "completion_tokens"))}
    model = resp.get("model")
    model = model.encode("utf-8", "backslashreplace").decode("ascii", "backslashreplace") if isinstance(model, str) else None
    line = (f"review-deepseek: model={model} prompt_tokens={counts['prompt_tokens']} "
            f"completion_tokens={counts['completion_tokens']} → {out}")
    if key in line:
        raise Fail(1, "响应元数据回显了密钥，拒绝落盘")
    return line


def redact(msg: str, key: str) -> str:
    return (msg.replace(key, "***") if key else msg)[:500]


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--prompt", required=True, type=Path, help="评审任务书（提示词文件）")
    ap.add_argument("--input", action="append", default=[], type=Path, help="附带材料（diff、源码），可重复")
    ap.add_argument("--out", required=True, type=Path)
    ap.add_argument("--force", action="store_true", help="允许覆盖已存在的 --out")
    ap.add_argument("--model", default="deepseek-flash")
    ap.add_argument("--max-tokens", type=int, help="输出上限；不给就用 API 默认值（截断时按失败退出）")
    ap.add_argument("--thinking", choices=["disabled", "enabled"], default="disabled",
                    help="默认 disabled：2026-10-17 实测 37K token 输入的评审，开思考时 16K/65K 输出额度全被 reasoning 吃光、正文为空"
                         "（_tmp/20261017-deepseek-diag/），关思考 19 s 给出完整结论。已实测适用 deepseek-flash；"
                         "其它模型若不认该字段，空回复报错会提示「可能被忽略」")
    ap.add_argument("--timeout", type=int, default=1800, help="整个请求的墙钟总时限（也是单次 socket 超时），秒")
    ap.add_argument("--api", default=API, help=argparse.SUPPRESS)  # 测试用假服务
    a = ap.parse_args()

    key, stage = "", 2
    try:
        key = load_key()
        check_paths(a)
        body = build_body(a)
        stage = 1
        resp = call_api(a, key, body)
        text = extract_text(resp, a.thinking)
        if key in text:
            raise Fail(1, "回复中回显了密钥，拒绝落盘")
        line = success_line(resp, a.out, key)
        write_atomic(a.out, text, a.force)
    except Fail as e:
        say(f"review-deepseek: {redact(str(e), key)}")
        return e.code
    except Exception as e:  # 兜底：任何意外都给受控诊断，不出 traceback
        say(f"review-deepseek: 意外错误 {type(e).__name__}: {redact(str(e), key)}")
        return stage
    say(line, sys.stdout)  # 已发布：日志失败不改变结果
    return 0


if __name__ == "__main__":
    sys.exit(main())
