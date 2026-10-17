#!/usr/bin/env python3
"""review-deepseek.py — DeepSeek 评审通道（第三谱系），直连 API，替代 dsh。

只把输入文件的文本发给模型、把回复落盘；模型拿不到任何工具，天然只读（dsh 只能靠提示词约束）。
密钥从仓库根 `.env.local` 的 DEEPSEEK_API_KEY 读取（或已在环境中）；本脚本不打印、不落盘密钥。

用法：
  python3 scripts/review-deepseek.py --prompt _tmp/<日期>-<名>/task.md [--input X.patch ...] --out _tmp/<日期>-<名>/out-deepseek.md
  --model deepseek-flash（默认）| deepseek-v4-pro
退出码：0 有回复 · 1 API/网络错误 · 2 参数/前置错误
"""
from __future__ import annotations

import argparse
import json
import os
import sys
import urllib.error
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
API = "https://api.deepseek.com/chat/completions"


def load_key() -> str:
    key = os.environ.get("DEEPSEEK_API_KEY", "")
    env = ROOT / ".env.local"
    if not key and env.is_file():
        for line in env.read_text(encoding="utf-8").splitlines():
            name, sep, value = line.strip().partition("=")
            if sep and name.strip() == "DEEPSEEK_API_KEY":
                key = value.strip().strip("'\"")
    return key


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--prompt", required=True, type=Path, help="评审任务书（提示词文件）")
    ap.add_argument("--input", action="append", default=[], type=Path, help="附带材料（diff、源码），可重复")
    ap.add_argument("--out", required=True, type=Path)
    ap.add_argument("--model", default="deepseek-flash")
    ap.add_argument("--timeout", type=int, default=1800)
    a = ap.parse_args()

    key = load_key()
    if not key:
        print("review-deepseek: 缺 DEEPSEEK_API_KEY（.env.local 或环境变量）", file=sys.stderr)
        return 2
    for p in [a.prompt, *a.input]:
        if not p.is_file():
            print(f"review-deepseek: 找不到 {p}", file=sys.stderr)
            return 2

    parts = [a.prompt.read_text(encoding="utf-8")]
    for p in a.input:
        parts.append(f"\n\n===== {p} =====\n{p.read_text(encoding='utf-8', errors='replace')}")
    body = json.dumps({
        "model": a.model,
        "messages": [{"role": "user", "content": "".join(parts)}],
        "stream": False,
    }).encode()
    req = urllib.request.Request(API, data=body, headers={
        "Authorization": f"Bearer {key}", "Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=a.timeout) as r:
            resp = json.load(r)
    except urllib.error.HTTPError as e:
        print(f"review-deepseek: HTTP {e.code} {e.read()[:500].decode(errors='replace')}", file=sys.stderr)
        return 1
    except (urllib.error.URLError, TimeoutError) as e:
        print(f"review-deepseek: 网络错误 {e}", file=sys.stderr)
        return 1

    text = resp["choices"][0]["message"]["content"]
    a.out.parent.mkdir(parents=True, exist_ok=True)
    a.out.write_text(text, encoding="utf-8")
    usage = resp.get("usage", {})
    print(f"review-deepseek: model={resp.get('model')} prompt_tokens={usage.get('prompt_tokens')} "
          f"completion_tokens={usage.get('completion_tokens')} → {a.out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
