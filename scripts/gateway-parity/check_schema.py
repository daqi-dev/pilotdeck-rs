#!/usr/bin/env python3
"""A3 · gateway 协议 schema 静态断言（规则层：能机器判的绝不留给人判）。

用法：
    python3 scripts/gateway-parity/check_schema.py            # CI 模式（无基线目录也可跑）
    PDRS_BASELINE=<dir> python3 scripts/gateway-parity/check_schema.py  # 本地模式（基线存在则追加基线核对）

比对三角：
    schema/gateway-protocol.schema.json  ×  schema/gateway-methods.snapshot.json  ×  docs/research/005_phase-a-gateway/method-inventory.md
基线源码（resources/PilotDeck/src/gateway/protocol/）存在时，再核对「快照 × 基线源码」；CI 上没有基线目录，这部分自动跳过。

断言（任一失败 → 非零退出，打印差异）：
    A1 schema 结构合法（draft-07、$ref → WsGatewayFrame、definitions 非空）
    A2 method 枚举 = 101、无重复
    A3 snapshot.methods 与 schema 枚举逐项一致（含顺序）
    A4 method-inventory.md 清单表：编号 1..101 连续，method 集合与顺序与快照一致（防清单与代码漂移）
    A5 六种帧：WsGatewayFrame 恰好六分支，各帧 type 常量与快照一致
    A6 GatewayChannelKey 等开放联合保持开放字符串（anyOf 含无 enum 的 string 分支）；字面量分支与快照一致
    A7 协议版本常量四处一致（schema 元数据 / 快照 / hello 与 hello_ok 的注解 / 基线 version.ts）
    A8 GatewayMode 是关闭枚举且与快照一致（对照组：该关的不许被放开）
本地（基线存在才跑）：
    L1 frames.ts 的 WsGatewayMethod 与快照逐字一致，且仍在冻结锚点 21-122 行
    L2 三个源文件 sha256 与快照、schema 元数据一致
    L3 version.ts 常量、types.ts 的 GatewayChannelKey / GatewayMode 字面量与快照一致
"""

import hashlib
import json
import re
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
SCHEMA_PATH = REPO / "schema" / "gateway-protocol.schema.json"
SNAPSHOT_PATH = REPO / "schema" / "gateway-methods.snapshot.json"
INVENTORY_PATH = REPO / "docs" / "research" / "005_phase-a-gateway" / "method-inventory.md"

# 冻结锚点（method-inventory.md 口径：frames.ts:21-122）。基线漂移到这里之外 = 清单需要人工重锚。
FROZEN_METHOD_LINES = (21, 122)

failures = []


def check(label: str, ok: bool, detail: str = "") -> None:
    print(f"{'[PASS]' if ok else '[FAIL]'} {label}" + (f" —— {detail}" if detail and not ok else ""))
    if not ok:
        failures.append(f"{label}: {detail}")


def set_diff(a, b):
    return sorted(set(a) - set(b)), sorted(set(b) - set(a))


def first_order_diff(a, b):
    for i, (x, y) in enumerate(zip(a, b)):
        if x != y:
            return f"首个分歧在 index {i}: 快照={x!r} vs 对方={y!r}"
    if len(a) != len(b):
        return f"长度 {len(a)} vs {len(b)}"
    return ""


def sha256_file(p: Path) -> str:
    return hashlib.sha256(p.read_bytes()).hexdigest()


def resolve_baseline() -> Path | None:
    import os
    for cand in (os.environ.get("PDRS_BASELINE"),
                 REPO.parent.parent / "resources" / "PilotDeck",
                 REPO / "resources" / "PilotDeck"):
        if cand and (Path(cand) / "src" / "gateway" / "protocol" / "frames.ts").is_file():
            return Path(cand)
    return None


def is_open_string(node) -> bool:
    """JSON Schema 片段是否接受任意字符串（开放）：纯 string 无 enum，或 anyOf/oneOf 有开放分支。"""
    if not isinstance(node, dict):
        return False
    if node.get("type") == "string" and "enum" not in node:
        return True
    for key in ("anyOf", "oneOf"):
        for member in node.get(key, []) or []:
            if is_open_string(member):
                return True
    return False


def collect_enums(node, acc):
    """收集片段内全部 enum 字面量（用于开放联合里『字面量分支仍在』的核对）。"""
    if isinstance(node, dict):
        if isinstance(node.get("enum"), list):
            acc.extend(x for x in node["enum"] if isinstance(x, str))
        for v in node.values():
            collect_enums(v, acc)
    elif isinstance(node, list):
        for v in node:
            collect_enums(v, acc)


def frame_type_consts(defn) -> set:
    """一个帧 def 内所有 type 属性的 const 值（WsResponseFrame 双分支都给 response）。"""
    consts = set()
    def walk(node):
        if isinstance(node, dict):
            props = node.get("properties")
            if isinstance(props, dict) and isinstance(props.get("type"), dict) and "const" in props["type"]:
                consts.add(props["type"]["const"])
            for v in node.values():
                walk(v)
        elif isinstance(node, list):
            for v in node:
                walk(v)
    walk(defn)
    return consts


def main() -> int:
    # ── 读三份材料 ──
    for p in (SCHEMA_PATH, SNAPSHOT_PATH, INVENTORY_PATH):
        if not p.is_file():
            print(f"[FAIL] 缺文件 {p.relative_to(REPO)}")
            return 1
    schema = json.loads(SCHEMA_PATH.read_text(encoding="utf-8"))
    snapshot = json.loads(SNAPSHOT_PATH.read_text(encoding="utf-8"))
    defs = schema.get("definitions") or schema.get("$defs")

    # A1 结构
    check("A1 schema: draft-07 + $ref → WsGatewayFrame + definitions 非空",
          schema.get("$schema") == "http://json-schema.org/draft-07/schema#"
          and schema.get("$ref") == "#/definitions/WsGatewayFrame"
          and isinstance(defs, dict) and bool(defs))

    # A2 method 枚举
    method_enum = (defs or {}).get("WsGatewayMethod", {}).get("enum")
    check("A2 schema: WsGatewayMethod.enum 存在且 = 101 个", isinstance(method_enum, list) and len(method_enum) == 101,
          f"实际 {len(method_enum) if isinstance(method_enum, list) else '缺失'}")
    check("A2 schema: method 枚举无重复", isinstance(method_enum, list) and len(set(method_enum)) == len(method_enum))

    # A3 快照 ↔ schema
    snap_methods = snapshot.get("methods")
    check("A3 快照: methodCount = 101", snapshot.get("methodCount") == 101, f"实际 {snapshot.get('methodCount')}")
    check("A3 快照 ↔ schema: method 列表逐项一致（含顺序）", snap_methods == method_enum,
          first_order_diff(snap_methods or [], method_enum or []))

    # A4 method-inventory.md ↔ 快照
    rows = []
    numbering_ok = True
    in_table = False
    for line in INVENTORY_PATH.read_text(encoding="utf-8").splitlines():
        if line.startswith("## "):
            in_table = "清单" in line and "原序" in line
            continue
        if not in_table:
            continue
        m = re.match(r"^\|\s*(\d+)\s*\|\s*`([^`]+)`\s*\|", line)
        if m:
            rows.append((int(m.group(1)), m.group(2)))
    check("A4 清单表: 解析出 101 行", len(rows) == 101, f"实际 {len(rows)}")
    if rows:
        check("A4 清单表: 编号 1..101 连续",
              [n for n, _ in rows] == list(range(1, len(rows) + 1)),
              f"编号序列 {[n for n, _ in rows][:5]}...")
        inv_methods = [m for _, m in rows]
        missing, extra = set_diff(inv_methods, snap_methods or [])
        check("A4 清单表 ↔ 快照: method 集合一致", not missing and not extra,
              f"清单多出 {missing}；清单缺失（清单未收录） {extra}")
        check("A4 清单表 ↔ 快照: 顺序一致（frames.ts 原序）", inv_methods == snap_methods,
              first_order_diff(snap_methods or [], inv_methods))

    # A5 六种帧
    frame_names = set()
    for member in (defs or {}).get("WsGatewayFrame", {}).get("anyOf", []) or []:
        ref = member.get("$ref", "")
        if ref.startswith("#/definitions/"):
            frame_names.add(ref.removeprefix("#/definitions/"))
    expected_frames = {"WsHelloFrame", "WsHelloOk", "WsRequestFrame", "WsResponseFrame", "WsEventFrame", "WsNotificationFrame"}
    check("A5 schema: WsGatewayFrame 恰好六帧分支", frame_names == expected_frames,
          f"实际 {sorted(frame_names)}")
    if frame_names == expected_frames:
        consts_by_frame = {name: frame_type_consts(defs[name]) for name in expected_frames}
        all_consts = set().union(*consts_by_frame.values())
        check("A5 schema: 帧类型常量 = 快照 frameTypes", all_consts == set(snapshot.get("frameTypes", [])),
              f"schema {sorted(all_consts)} vs 快照 {sorted(snapshot.get('frameTypes', []))}")
        check("A5 schema: 每帧 type 常量唯一", all(len(v) == 1 for v in consts_by_frame.values()),
              str({k: sorted(v) for k, v in consts_by_frame.items() if len(v) != 1}))

    # A6 开放联合
    channel = (defs or {}).get("GatewayChannelKey")
    check("A6 schema: GatewayChannelKey 保持开放字符串", is_open_string(channel), json.dumps(channel, ensure_ascii=False)[:200])
    if channel is not None:
        literals = []
        collect_enums(channel, literals)
        missing, extra = set_diff(literals, snapshot.get("channelKeyLiterals", []))
        check("A6 schema: GatewayChannelKey 字面量分支与快照一致", not missing and not extra,
              f"schema 多出 {missing}；schema 缺 {extra}")

    # A7 协议版本（四处一致）
    v_schema = (schema.get("x-pilotdeck") or {}).get("protocolVersion")
    v_snap = snapshot.get("protocolVersion")
    v_hello = ((defs or {}).get("WsHelloFrame", {}).get("properties", {}).get("protocolVersion", {}) or {}).get("x-pilotdeck-protocol-version")
    v_hello_ok = ((defs or {}).get("WsHelloOk", {}).get("properties", {}).get("protocolVersion", {}) or {}).get("x-pilotdeck-protocol-version")
    check("A7 协议版本: schema 元数据 = 快照 = hello 注解 = hello_ok 注解",
          v_schema == v_snap == v_hello == v_hello_ok and isinstance(v_schema, str),
          f"schema={v_schema} 快照={v_snap} hello={v_hello} hello_ok={v_hello_ok}")
    check("A7 协议版本: hello/hello_ok 的 protocolVersion 类型仍是 string（注解不约束校验）",
          ((defs or {}).get("WsHelloFrame", {}).get("properties", {}).get("protocolVersion", {}) or {}).get("type") == "string"
          and ((defs or {}).get("WsHelloOk", {}).get("properties", {}).get("protocolVersion", {}) or {}).get("type") == "string")

    # A8 GatewayMode 关闭枚举（对照组）
    mode = (defs or {}).get("GatewayMode", {})
    check("A8 schema: GatewayMode 是关闭枚举且与快照一致",
          mode.get("type") == "string" and isinstance(mode.get("enum"), list)
          and mode["enum"] == snapshot.get("gatewayModeLiterals"),
          f"schema={mode.get('enum')} 快照={snapshot.get('gatewayModeLiterals')}")

    # ── 本地：基线核对（基线目录存在才跑；CI 上跳过）──
    baseline = resolve_baseline()
    if baseline is None:
        print("[SKIP] 本地基线核对（未找到 resources/PilotDeck，CI 模式：快照 × 清单 × schema 三方一致性已全部核对）")
    else:
        proto = baseline / "src" / "gateway" / "protocol"
        frames_text = (proto / "frames.ts").read_text(encoding="utf-8")
        lines = frames_text.splitlines()

        # L1 WsGatewayMethod 逐字 + 锚点
        methods, start, end = [], None, None
        in_union = False
        for i, line in enumerate(lines, 1):
            if re.match(r"^export type WsGatewayMethod =", line):
                in_union, start = True, i
                continue
            if in_union:
                if ";" in line:
                    end = i
                    in_union = False
                methods.extend(re.findall(r'"([^"]+)"', line))
        check("L1 基线 frames.ts: WsGatewayMethod 与快照逐字一致（含顺序）", methods == snap_methods,
              first_order_diff(snap_methods or [], methods))
        check(f"L1 基线 frames.ts: 枚举块仍在冻结锚点 {FROZEN_METHOD_LINES}",
              (start, end) == FROZEN_METHOD_LINES,
              f"实际 {start}-{end}——基线漂移，method-inventory.md 的锚点须人工重审")

        # L2 sha256（快照 × schema 元数据 × 实文件）
        for fname in ("frames.ts", "types.ts", "version.ts"):
            actual = sha256_file(proto / fname)
            from_snap = (snapshot.get("source") or {}).get(fname, {}).get("sha256")
            from_schema = ((schema.get("x-pilotdeck") or {}).get("source") or {}).get(fname, {}).get("sha256")
            check(f"L2 基线 {fname}: sha256 与快照/schema 元数据一致",
                  actual == from_snap == from_schema,
                  f"实文件={actual} 快照={from_snap} schema={from_schema}")

        # L3 version.ts 常量与 types.ts 字面量
        m = re.search(r'PILOTDECK_GATEWAY_PROTOCOL_VERSION\s*=\s*"([^"]+)"', (proto / "version.ts").read_text(encoding="utf-8"))
        check("L3 基线 version.ts: 常量与快照/schema 一致", m and m.group(1) == v_snap == v_schema, f"version.ts={m.group(1) if m else None}")

        types_text = (proto / "types.ts").read_text(encoding="utf-8")
        for name, snap_key in (("GatewayChannelKey", "channelKeyLiterals"), ("GatewayMode", "gatewayModeLiterals")):
            mm = re.search(rf"export type {name} =\s*(.*?);", types_text, re.S)
            lits = re.findall(r'"([^"]+)"', mm.group(1)) if mm else []
            check(f"L3 基线 types.ts: {name} 字面量与快照一致", lits == snapshot.get(snap_key),
                  f"types.ts={lits} 快照={snapshot.get(snap_key)}")

    # ── 汇总 ──
    if failures:
        print(f"\ncheck_schema: {len(failures)} 项 FAIL")
        for f in failures:
            print(f"  - {f}")
        return 1
    print("\ncheck_schema: 全部断言通过")
    return 0


if __name__ == "__main__":
    sys.exit(main())
