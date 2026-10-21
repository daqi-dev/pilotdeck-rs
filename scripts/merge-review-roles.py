#!/usr/bin/env python3
"""merge-review-roles.py — 把逐角色串行跑出的 run-review-roles.py 产物合成一个 pr-gate 结果块。

背景：四角色一次并行全跑会挂起，实践改为每角色单独调用；check-pr-gate.py 要求单块含全部角色，
逐角色块各自缺角色会判 FAIL。本脚本只做机械合并，不改任何角色结论：
  - 每个目录必须有 status.json、summary.md（内含 pr-gate:v1 块）与 diff.patch；
  - 各目录的 head / runner / model / scope / paths / diff_lines 必须一致，diff.patch 逐字节相同；
  - 源块只含已知键且各恰好一次、readonly=tools，块里的 roles/p0/p1/diff_lines 须与 status.json、diff.patch 复算一致
    （合并不得把源块的错误洗成一份新的合法声明——#432 codex 评审 P0×2）；
  - 角色不得重复、不得为空；合并后的状态、计数直接取各目录 status.json。
用法：
  python3 scripts/merge-review-roles.py --out _tmp/X-gate1 _tmp/X-gate1-sibling _tmp/X-gate1-boundary ...
退出码：0 全部 PASS · 1 有非 PASS · 2 输入不一致或缺件
"""
from __future__ import annotations

import argparse
import hashlib
import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
BLOCK = re.compile(r"<!--\s*pr-gate:v1\s+(.*?)\s*-->")
SAME_KEYS = ("head", "runner", "model", "scope", "paths", "diff_lines")
# run-review-roles.py:336 生成的全部键；源块出现其它 token 一律拒绝，否则合并会丢掉闸门本会识别的声明（#432 r2 codex P0）
KNOWN_KEYS = {"head", "roles", "p0", "p1", "diff_lines", "scope", "paths", "runner", "model", "readonly"}


def fail(msg: str) -> int:
    print(f"merge-review-roles: {msg}", file=sys.stderr)
    return 2


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--out", required=True, help="合并产物目录，须在主仓 _tmp/ 下")
    ap.add_argument("dirs", nargs="+", help="run-review-roles.py 的逐角色 --out 目录")
    a = ap.parse_args()

    out = Path(a.out).resolve()
    try:
        out.relative_to(ROOT / "_tmp")
    except ValueError:
        return fail("--out 必须在主仓 _tmp/ 下")

    ref: dict[str, str] | None = None
    diff_sha = None
    results: dict[str, dict] = {}
    sources: dict[str, Path] = {}
    for d in map(Path, a.dirs):
        d = d.resolve()
        try:
            status = json.loads((d / "status.json").read_text(encoding="utf-8"))
            summary = (d / "summary.md").read_text(encoding="utf-8")
            sha = hashlib.sha256((d / "diff.patch").read_bytes()).hexdigest()
        except (OSError, ValueError) as e:
            return fail(f"{d} 缺件或损坏：{e}")
        blocks = BLOCK.findall(summary)
        if len(blocks) != 1:
            return fail(f"{d}/summary.md 应恰有 1 个 pr-gate 块，实有 {len(blocks)}")
        if not isinstance(status, dict) or not status or not all(isinstance(r, dict) for r in status.values()):
            return fail(f"{d}/status.json 应为非空的 角色→结果 对象")
        tokens = blocks[0].split()
        odd = [t for t in tokens if t.partition("=")[0] not in KNOWN_KEYS or "=" not in t]
        if odd:
            return fail(f"{d} 源块含未知键或游离 token {odd}")
        pairs = [t.split("=", 1) for t in tokens]
        keys = [k for k, _ in pairs]
        dup = sorted({k for k in keys if keys.count(k) > 1})
        if dup:
            return fail(f"{d} 源块键重复 {dup}")
        kv = dict(pairs)
        if kv.get("readonly") != "tools":
            return fail(f"{d} 源块 readonly={kv.get('readonly')}，只接受 tools")
        # roles 按 角色→状态 映射比较：status.json 按线程完成序写入，summary 按请求序（#432 r2 codex P1）
        role_pairs = [x.partition(":") for x in kv.get("roles", "").split(",")]
        block_roles = {r: st for r, _, st in role_pairs}
        status_roles = {r: v.get("status") for r, v in status.items()}
        want_p = {k: sum((v.get("counts") or {}).get(k, 0) for v in status.values()) for k in ("P0", "P1")}
        diff_lines = (d / "diff.patch").read_text(encoding="utf-8", errors="replace").count("\n")
        if (len(block_roles) != len(role_pairs) or block_roles != status_roles
                or kv.get("p0") != str(want_p["P0"]) or kv.get("p1") != str(want_p["P1"])
                or kv.get("diff_lines") != str(diff_lines)):
            return fail(f"{d} 源块 roles/p0/p1/diff_lines 与 status.json、diff.patch 复算不一致")
        meta = {k: kv.get(k, "") for k in SAME_KEYS}
        if ref is None:
            ref, diff_sha = meta, sha
        elif meta != ref:
            return fail(f"{d} 与首个目录元数据不一致：{meta} ≠ {ref}")
        elif sha != diff_sha:
            return fail(f"{d}/diff.patch 与首个目录不同（不是同一份被审 diff）")
        for role, r in status.items():
            if role in results:
                return fail(f"角色 {role} 重复：{sources[role]} 与 {d}")
            results[role], sources[role] = r, d
    assert ref is not None

    roles = list(results)
    ok = all(results[r]["status"] == "PASS" for r in roles)
    tot = {k: sum((results[r].get("counts") or {}).get(k, 0) for r in roles) for k in ("P0", "P1", "P2", "P3")}
    marker_roles = ",".join(f"{r}:{results[r]['status']}" for r in roles)
    paths_kv = f" paths={ref['paths']}" if ref["paths"] else ""
    lines = [
        "## pr-ready 角色审计（scripts/run-review-roles.py 逐角色串行，scripts/merge-review-roles.py 合并）", "",
        f"<!-- pr-gate:v1 head={ref['head']} roles={marker_roles} p0={tot['P0']} p1={tot['P1']} "
        f"diff_lines={ref['diff_lines']} scope={ref['scope']}{paths_kv} runner={ref['runner']} model={ref['model']} readonly=tools -->", "",
        "| 角色 | 状态 | P0 | P1 | P2 | P3 | 秒 | 说明 | 原件 |", "|---|---|---|---|---|---|---|---|---|",
    ]
    for r in roles:
        c = results[r].get("counts") or {}
        lines.append(f"| {r} | {results[r]['status']} | {c.get('P0', '-')} | {c.get('P1', '-')} | {c.get('P2', '-')} | "
                     f"{c.get('P3', '-')} | {results[r].get('seconds')} | {results[r].get('detail') or '—'} | "
                     f"`{sources[r].relative_to(ROOT) if sources[r].is_relative_to(ROOT) else sources[r]}/` |")
    lines += ["", f"diff.patch sha256 `{diff_sha}`（各目录一致）"]
    out.mkdir(parents=True, exist_ok=True)
    (out / "status.json").write_text(json.dumps(results, ensure_ascii=False, indent=2), encoding="utf-8")
    (out / "summary.md").write_text("\n".join(lines) + "\n", encoding="utf-8")
    print(f"merge-review-roles: {'全部 PASS' if ok else '有非 PASS'}；P0={tot['P0']} P1={tot['P1']} P2={tot['P2']}；见 {out}/summary.md")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
