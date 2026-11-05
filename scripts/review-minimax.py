#!/usr/bin/env python3
"""review-minimax.py — MiniMax 评审通道（第五谱系），直连官方 API，天然只读。

只把输入文件的文本发给模型、把回复落盘；模型拿不到任何工具，天然只读。
密钥取环境变量 MINIMAX_API_KEY，没有再读 `.env.local`（先当前检出根，再主仓根）；
本脚本不打印、不落盘密钥，错误信息里出现的密钥一律替换成 ***。

用法：
  python3 -I scripts/review-minimax.py --prompt _tmp/<日期>-<名>/task.md [--input X.patch ...] --out _tmp/<日期>-<名>/out-minimax.md
  --model MiniMax-M3.1-Flash（默认）· --force 允许覆盖已存在的 --out
  --no-require-verdict 关闭评审契约校验（连通性冒烟等自由文本场景才用）
退出码：0 有非空完整回复且（默认）通过评审契约 · 1 API/网络/响应异常或回复缺契约（均不写 --out）· 2 参数/前置错误

评审契约（默认强制，与 scripts/run-review-roles.py 的 validate_final 同义）：末个非空行须是
`VERDICT: P0=a P1=b P2=c P3=d`（四级计数齐全），且正文须有 `覆盖:` 行。exit 0 才代表「评审完成」，
空文本 / 前言 / 截断 / 复读都不算（state/memory/20261016-review-bounds.md）。

注意：MiniMax 有时把回复拆成多条 assistant 消息（实测还常带 thinking 块）；本脚本把所有 text 块拼在一起落盘。
若只读 final.md 会漏前段（trial-20261103 REPORT.md §3）；读完整 --out 文件。
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
import urllib.error
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
API = "https://api.minimaxi.com/anthropic/v1/messages"  # Anthropic Messages 协议
DEFAULT_MODEL = "MiniMax-M3.1-Flash"
KEY_NAME = "MINIMAX_API_KEY"
DEFAULT_MAX_TOKENS = 16384
MAX_INPUT_BYTES = 2 * 1024 * 1024
MAX_RESPONSE_BYTES = 16 * 1024 * 1024
# MiniMax 的 key 是 JWT（实测 690 字符，含 - _ . / + = 与字母数字）；空白/换行一律拒，
# 免得 http.client 在 header 校验时抛 ValueError、被误报成「网络错误」
KEY_RE = re.compile(r"[A-Za-z0-9._\-/+=]+")
VERDICT_LEVELS = ("P0", "P1", "P2", "P3")


def say(msg: str, stream=None) -> None:
    stream = stream or sys.stderr
    try:
        print(msg, file=stream, flush=True)
    except Exception:
        try:
            fd = os.open(os.devnull, os.O_WRONLY)
            try:
                os.dup2(fd, stream.fileno())
            finally:
                os.close(fd)
        except Exception:
            pass


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


def parse_env_value(raw: str, name: str = KEY_NAME) -> str:
    raw = raw.strip()
    if raw[:1] in ("'", '"'):
        end = raw.find(raw[0], 1)
        if end < 0:
            raise Fail(2, f".env.local 中 {name} 引号不配对")
        return raw[1:end]
    return re.split(r"\s+#", raw, maxsplit=1)[0].strip() if not raw.startswith("#") else ""


def load_key(envs: list[Path] | None = None) -> str:
    key = os.environ.get(KEY_NAME, "").strip()
    if not key:
        for env in envs if envs is not None else env_files():
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
                    key = parse_env_value(value, KEY_NAME)
            if key:
                break
    if not key:
        raise Fail(2, f"缺 {KEY_NAME}（环境变量或 .env.local）")
    if not KEY_RE.fullmatch(key):
        raise Fail(2, f"{KEY_NAME} 含空白或非法字符（不打印密钥内容）")
    return key


def same_file(a: Path, b: Path) -> bool:
    try:
        return a.exists() and b.exists() and os.path.samefile(a, b)
    except OSError:
        return False


def is_secret(p: Path, envs: list[Path]) -> bool:
    return p.name.startswith(".env") or p.resolve().name.startswith(".env") or any(same_file(p, e) for e in envs)


def stat_sig(p: Path):
    """身份+大小+纳秒 mtime；不存在或读不到返回 None（force 覆盖前用它判「别人动过没有」）。"""
    try:
        st = p.stat()
    except OSError:
        return None
    return (st.st_dev, st.st_ino, st.st_size, st.st_mtime_ns)


def check_paths(a: argparse.Namespace, envs: list[Path] | None = None) -> None:
    if a.timeout <= 0:
        raise Fail(2, "--timeout 须为正数")
    if a.max_tokens is not None and a.max_tokens <= 0:
        raise Fail(2, "--max-tokens 须为正整数")
    if not a.out.name or a.out.is_dir():
        raise Fail(2, f"--out 须是文件路径：{a.out}")
    envs = envs if envs is not None else env_files()
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
    # --force 也留一道闸：请求期间别人重建/改写了 --out，就不能无条件 os.replace 掉它
    a._out_stat = stat_sig(a.out)


def build_body(a: argparse.Namespace) -> bytes:
    total, texts = 0, []
    try:
        for p in [a.prompt, *a.input]:
            before = stat_sig(p)
            limit = MAX_INPUT_BYTES - total
            with open(p, "rb") as f:
                data = f.read(limit + 1)
            after = stat_sig(p)
            # 读前后身份/大小/mtime 不变、且读到的是整文件（没被上限截断时）才算完整材料
            if before is None or after != before or \
                    (before[2] <= limit and len(data) != before[2]):
                raise Fail(2, f"输入 {p} 在读取期间被改动，材料可能是半截；请重跑")
            total += len(data)
            if total > MAX_INPUT_BYTES:
                raise Fail(2, f"输入合计超过上限 {MAX_INPUT_BYTES} 字节；请缩小材料或分批")
            texts.append(data.decode("utf-8"))
    except (OSError, UnicodeError) as e:
        raise Fail(2, f"读取输入失败：{type(e).__name__}") from None
    if total == 0:
        raise Fail(2, "输入为空（--prompt 与 --input 都是 0 字节文件）；空材料发给上游只会换来 400")
    parts = [texts[0], *(f"\n\n===== {p} =====\n{t}" for p, t in zip(a.input, texts[1:]))]
    say(f"review-minimax: 输入 {total} 字节（{1 + len(a.input)} 个文件）")
    return json.dumps({
        "model": a.model,
        "messages": [{"role": "user", "content": "".join(parts)}],
        "stream": False,
        "max_tokens": a.max_tokens if a.max_tokens is not None else DEFAULT_MAX_TOKENS,
    }, ensure_ascii=False).encode()


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *args, **kwargs):
        return None


OPENER = urllib.request.build_opener(NoRedirect)


class Deadline(Exception):
    pass


def _deadline(*_):
    raise Deadline


def call_api(a: argparse.Namespace, key: str, body: bytes) -> dict:
    req = urllib.request.Request(
        a.api, data=body,
        headers={
            "x-api-key": key,
            "anthropic-version": "2023-06-01",
            "Content-Type": "application/json",
        },
    )
    previous = signal.signal(signal.SIGALRM, _deadline)
    signal.setitimer(signal.ITIMER_REAL, a.timeout)
    try:
        with OPENER.open(req, timeout=a.timeout) as r:
            raw = r.read(MAX_RESPONSE_BYTES + 1)
    except Deadline:
        raise Fail(1, f"请求超过总时限 {a.timeout} 秒") from None
    except urllib.error.HTTPError as e:
        try:
            detail = e.read(64 * 1024).decode(errors="replace")
        except Deadline:
            raise Fail(1, f"HTTP {e.code}，读取错误正文超过总时限 {a.timeout} 秒") from None
        except Exception:
            detail = ""
        raise Fail(1, f"HTTP {e.code} {detail}") from None
    except Exception as e:
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


def extract_text(resp: dict) -> str:
    blocks = resp.get("content")
    if not isinstance(blocks, list) or not blocks:
        raise Fail(1, f"响应无 content：{json.dumps(resp, ensure_ascii=False)[:65536]}")
    stop = resp.get("stop_reason")
    # 键缺失和值为 null 一样是形状异常：把「上游没说停在哪」当成完整回复会放行截断
    if "stop_reason" not in resp or stop is None:
        raise Fail(1, f"响应缺 stop_reason 或其值为 null（形状异常，无法判断是否完整）：{json.dumps(resp, ensure_ascii=False)[:4096]}")
    # MiniMax 有时把回复拆成多条消息，全部 text 块拼接（trial-20261103 REPORT.md §3）
    text = "".join(
        b.get("text") for b in blocks
        if isinstance(b, dict) and b.get("type") == "text" and isinstance(b.get("text"), str)
    )
    if not text.strip():
        usage = resp.get("usage") if isinstance(resp.get("usage"), dict) else {}
        thinking = any(isinstance(b, dict) and str(b.get("type", "")).endswith("thinking") for b in blocks)
        raise Fail(1, f"空回复（stop_reason={stop}，output_tokens={usage.get('output_tokens')}，"
                      f"含 thinking 块={thinking}）；若含 thinking 块，多半是思考吃光 --max-tokens，调大再试")
    if stop not in ("end_turn", "stop_sequence"):
        raise Fail(1, f"回复未完成（stop_reason={stop}），不当作评审结论")
    return text


def write_atomic(out: Path, text: str, force: bool, expected=None) -> None:
    tmp = None
    try:
        out.parent.mkdir(parents=True, exist_ok=True)
        fd, name = tempfile.mkstemp(prefix=f".{out.name}.", suffix=".tmp", dir=out.parent)
        tmp = Path(name)
        with os.fdopen(fd, "w", encoding="utf-8") as f:
            f.write(text)
            f.flush()
            os.fsync(f.fileno())  # 掉电后不留「文件在、内容空」
        if force:
            # 请求期间别人建/改过 --out 就不覆盖：两跑同 --out --force 时后跑者不得静默销毁先跑者证据
            if stat_sig(out) != expected:
                raise Fail(1, f"{out} 在请求期间被别人创建或改动，拒绝 --force 覆盖；本次回复不落盘")
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
            kept, tmp = tmp, None
            raise Fail(1, f"{out.parent} 不支持硬链接，无法原子且不覆盖地发布；回复已保留在 {kept}，确认后可改用 --force") from None
    except (OSError, UnicodeError) as e:
        raise Fail(1, f"写 {out} 失败：{type(e).__name__}") from None
    finally:
        if tmp is not None:
            try:
                tmp.unlink(missing_ok=True)
            except OSError as e:
                say(f"review-minimax: 警告：临时文件 {tmp} 未能删除（{type(e).__name__}）")


def success_line(resp: dict, out: Path, key: str) -> str:
    usage = resp.get("usage") if isinstance(resp.get("usage"), dict) else {}
    counts = {k: v if isinstance(v, int) and not isinstance(v, bool) else None
              for k, v in ((k, usage.get(k)) for k in ("input_tokens", "output_tokens"))}
    model = resp.get("model")
    model = model.encode("utf-8", "backslashreplace").decode("ascii", "backslashreplace") if isinstance(model, str) else None
    line = (f"review-minimax: model={model} prompt_tokens={counts['input_tokens']} "
            f"completion_tokens={counts['output_tokens']} → {out}")
    if key in line:
        raise Fail(1, "响应元数据回显了密钥，拒绝落盘")
    return line


def redact(msg: str, key: str) -> str:
    return (msg.replace(key, "***") if key else msg)[:500]


def check_verdict(text: str) -> str:
    """评审契约：末个非空行 `VERDICT: P0=a P1=b P2=c P3=d` 且正文有 `覆盖:` 行。返回空串=通过，否则为原因。

    与 scripts/run-review-roles.py 的 validate_final 同义；exit 0 必须等于「评审真的做完了」，
    否则空文本 / 前言 / 截断 / 复读都会被调用方当成 PASS（state/memory/20261016-review-bounds.md）。
    """
    lines = [ln.strip() for ln in text.strip().splitlines() if ln.strip()]
    if not lines:
        return "回复为空"
    last = lines[-1]
    if not last.startswith("VERDICT:"):
        return "末行不是 VERDICT"
    seen = set()
    for tok in last[len("VERDICT:"):].split():
        k, _, v = tok.partition("=")
        # 只认 ASCII 数字：isdigit() 对 `²` / 全角 `０` 也为真，消费方 int() 会炸
        if k in VERDICT_LEVELS and v.isascii() and v.isdigit():
            seen.add(k)
    if seen != set(VERDICT_LEVELS):
        return "VERDICT 缺等级计数"
    if not any(ln.startswith("覆盖:") or ln.startswith("覆盖：") for ln in lines):
        return "缺『覆盖:』行（无法核角色覆盖）"
    return ""


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--prompt", required=True, type=Path, help="评审任务书（提示词文件）")
    ap.add_argument("--input", action="append", default=[], type=Path, help="附带材料（diff、源码），可重复")
    ap.add_argument("--out", required=True, type=Path)
    ap.add_argument("--force", action="store_true", help="允许覆盖已存在的 --out")
    ap.add_argument("--model", default=DEFAULT_MODEL, help=f"默认 {DEFAULT_MODEL}")
    ap.add_argument("--max-tokens", type=int, help=f"输出上限；不给用 {DEFAULT_MAX_TOKENS}")
    ap.add_argument("--timeout", type=int, default=1800, help="整个请求的墙钟总时限（秒）")
    ap.add_argument("--no-require-verdict", dest="require_verdict", action="store_false",
                    help="关闭评审契约校验（仅连通性冒烟等自由文本场景；默认要求 VERDICT + 覆盖行）")
    ap.add_argument("--api", help=argparse.SUPPRESS)  # 测试用假服务
    a = ap.parse_args()
    if a.api and not re.match(r"https?://(127\.0\.0\.1|localhost|\[::1\])(:\d+)?(/|$)", a.api):
        say("review-minimax: --api 只接受本机测试地址（http://127.0.0.1:<端口>/、http://[::1]:<端口>/）")
        return 2
    a.api = a.api or API

    key, stage = "", 2
    try:
        envs = env_files()
        key = load_key(envs)
        check_paths(a, envs)
        body = build_body(a)
        stage = 1
        resp = call_api(a, key, body)
        got_model = resp.get("model")
        if not isinstance(got_model, str) or got_model != a.model:
            raise Fail(1, f"响应模型 {got_model!r} 与请求 {a.model} 不一致，结论不计")
        text = extract_text(resp)
        if key in text:
            raise Fail(1, "回复中回显了密钥，拒绝落盘")
        if a.require_verdict:
            why = check_verdict(text)
            if why:
                raise Fail(1, f"回复未按评审契约完成（{why}），按未完成处理、不写 --out；"
                              "确需自由文本回复请加 --no-require-verdict")
        line = success_line(resp, a.out, key)
        write_atomic(a.out, text, a.force, a._out_stat)
    except Fail as e:
        say(f"review-minimax: {redact(str(e), key)}")
        return e.code
    except Exception as e:
        say(f"review-minimax: 意外错误 {type(e).__name__}: {redact(str(e), key)}")
        return stage
    say(line, sys.stdout)
    return 0


if __name__ == "__main__":
    sys.exit(main())
