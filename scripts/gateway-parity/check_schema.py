#!/usr/bin/env python3
"""A3 · gateway 协议 schema 静态断言（规则层：能机器判的绝不留给人判）。

用法：
    python3 scripts/gateway-parity/check_schema.py            # CI 模式（无基线目录也可跑）
    PDRS_BASELINE=<dir> python3 scripts/gateway-parity/check_schema.py  # 本地模式（基线存在则追加基线核对）
    PDRS_REQUIRE_BASELINE=1 ...                                # 缺基线即 FAIL（CI 基线 job 用，防整段静默 SKIP）

比对三角：
    schema/gateway-protocol.schema.json  ×  schema/gateway-methods.snapshot.json  ×  docs/research/005_phase-a-gateway/method-inventory.md
基线源码（resources/PilotDeck/src/gateway/protocol/）存在时，再核对「快照 × 基线源码」；CI 上没有基线目录，
这部分自动跳过（但 CI 另有基线 job：按冻结 commit 克隆基线跑 export_schema.mjs --check + 本脚本 L1–L3）。

断言（任一失败 → 非零退出，打印差异）：
    A1 schema 结构合法（draft-07、$ref → WsGatewayFrame、definitions 非空）
    A2 method 枚举 = 101、无重复
    A3 snapshot.methods 与 schema 枚举逐项一致（含顺序）
    A4 method-inventory.md 清单表：编号 1..101 连续，method 集合与顺序与快照一致（防清单与代码漂移）
    A5 六种帧：WsGatewayFrame.anyOf 恰好六个分支、每个分支是纯 $ref（除 $ref 外无其他键）、引用全部有效；
       各帧 type 常量与快照一致；WsRequestFrame.method 恰为 $ref → WsGatewayMethod（退化成开放 string 会在此 FAIL）
    A6 开放联合保持开放字符串（anyOf 含无 enum 的 string 分支）；字面量分支与快照一致。
       覆盖：GatewayChannelKey；GatewayEvent plan_mode_changed 变体的 mode（GatewayMode | (string & {})）。
       关闭联合不许被放开：GatewayMode、GatewayWsClientName（含 WsHelloFrame.clientName 的 $ref 形状）
    A7 协议版本常量四处一致（schema 元数据 / 快照 / hello 与 hello_ok 的注解 / 基线 version.ts）
    A8 GatewayMode 是关闭枚举且与快照一致（对照组：该关的不许被放开）
    A9 roots 齐备：x-pilotdeck.rootTypes 全部有 definition；快照 payloadAliases（按方法导出的内联载荷）
       全部有 definition 且列入 rootTypes；全 schema 无悬空 $ref
本地（基线存在才跑）：
    L1 frames.ts 的 WsGatewayMethod 与快照逐字一致，且仍在冻结锚点 21-122 行
    L2 三个源文件 sha256 与快照、schema 元数据一致
    L3 version.ts 常量、types.ts 的 GatewayChannelKey / GatewayMode 字面量、frames.ts 的
       GatewayWsClientName 字面量与快照一致
"""

import hashlib
import json
import os
import re
import sys
from pathlib import Path
from urllib.parse import unquote

REPO = Path(__file__).resolve().parents[2]
SCHEMA_PATH = REPO / "schema" / "gateway-protocol.schema.json"
SNAPSHOT_PATH = REPO / "schema" / "gateway-methods.snapshot.json"
INVENTORY_PATH = REPO / "docs" / "research" / "005_phase-a-gateway" / "method-inventory.md"

# 冻结锚点（method-inventory.md 口径：frames.ts:21-122）。基线漂移到这里之外 = 清单需要人工重锚。
FROZEN_METHOD_LINES = (21, 122)
# method 分母（method-inventory.md 冻结口径；改分母 = 破冻，须另开 issue）
METHOD_COUNT = 101

EXPECTED_FRAMES = {"WsHelloFrame", "WsHelloOk", "WsRequestFrame", "WsResponseFrame", "WsEventFrame", "WsNotificationFrame"}

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


def resolve_baseline():
    """返回 (基线目录 | None, 是否强制要求基线)。候选：$PDRS_BASELINE → 各级祖先的 resources/PilotDeck。"""
    candidates = []
    env = os.environ.get("PDRS_BASELINE")
    if env:
        candidates.append(Path(env))
    for base in (REPO, *REPO.parents):  # 适配任意深度的 worktree（含 _build/wt-*）
        candidates.append(base / "resources" / "PilotDeck")
    for cand in candidates:
        if (cand / "src" / "gateway" / "protocol" / "frames.ts").is_file():
            return cand, None
    require = os.environ.get("PDRS_REQUIRE_BASELINE", "").strip().lower() in ("1", "true", "yes")
    return None, require


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


def collect_refs(node, acc):
    """收集片段内全部内部 $ref（悬空引用检查用）。"""
    if isinstance(node, dict):
        for k, v in node.items():
            if k == "$ref" and isinstance(v, str):
                acc.append(v)
            else:
                collect_refs(v, acc)
    elif isinstance(node, list):
        for v in node:
            collect_refs(v, acc)


def valid_local_ref(schema, ref):
    """Resolve the full local RFC 6901 pointer; external refs are out of contract."""
    if not isinstance(ref, str) or not ref.startswith('#'):
        return False
    fragment = ref[1:]
    if re.search(r'%(?![0-9a-fA-F]{2})', fragment):
        return False
    try:
        pointer = unquote(fragment, errors='strict')
        if pointer == '':
            return isinstance(schema, (dict, bool))
        if not pointer.startswith('/'):
            return False
        node = schema
        for token in pointer[1:].split('/'):
            if re.search(r'~(?![01])', token):
                return False
            token = token.replace('~1', '/').replace('~0', '~')
            if isinstance(node, dict):
                node = node[token]
            elif isinstance(node, list) and re.fullmatch(r'0|[1-9][0-9]*', token):
                node = node[int(token)]
            else:
                return False
        return isinstance(node, (dict, bool))
    except (KeyError, IndexError, UnicodeDecodeError):
        return False


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
    defs = schema.get("definitions") or schema.get("$defs") or {}

    # A1 结构
    check("A1 schema: draft-07 + $ref → WsGatewayFrame + definitions 非空",
          schema.get("$schema") == "http://json-schema.org/draft-07/schema#"
          and schema.get("$ref") == "#/definitions/WsGatewayFrame"
          and isinstance(defs, dict) and bool(defs))

    # A2 method 枚举
    method_enum = defs.get("WsGatewayMethod", {}).get("enum")
    check(f"A2 schema: WsGatewayMethod.enum 存在且 = {METHOD_COUNT} 个",
          isinstance(method_enum, list) and len(method_enum) == METHOD_COUNT,
          f"实际 {len(method_enum) if isinstance(method_enum, list) else '缺失'}")
    check("A2 schema: method 枚举无重复", isinstance(method_enum, list) and len(set(method_enum)) == len(method_enum))

    # A3 快照 ↔ schema
    snap_methods = snapshot.get("methods")
    check(f"A3 快照: methodCount = {METHOD_COUNT}", snapshot.get("methodCount") == METHOD_COUNT,
          f"实际 {snapshot.get('methodCount')}")
    check("A3 快照 ↔ schema: method 列表逐项一致（含顺序）", snap_methods == method_enum,
          first_order_diff(snap_methods or [], method_enum or []))

    # A4 method-inventory.md ↔ 快照
    rows = []
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
    check(f"A4 清单表: 解析出 {METHOD_COUNT} 行", len(rows) == METHOD_COUNT, f"实际 {len(rows)}")
    if rows:
        check(f"A4 清单表: 编号 1..{METHOD_COUNT} 连续",
              [n for n, _ in rows] == list(range(1, len(rows) + 1)),
              f"编号序列 {[n for n, _ in rows][:5]}...")
        inv_methods = [m for _, m in rows]
        missing, extra = set_diff(inv_methods, snap_methods or [])
        check("A4 清单表 ↔ 快照: method 集合一致", not missing and not extra,
              f"清单多出 {missing}；清单缺失（清单未收录） {extra}")
        check("A4 清单表 ↔ 快照: 顺序一致（frames.ts 原序）", inv_methods == snap_methods,
              first_order_diff(snap_methods or [], inv_methods))

    # A5 六种帧——恰好六个分支、每个分支是纯 $ref（追加 {} 等非引用分支在此 FAIL）、引用有效
    frame_members = defs.get("WsGatewayFrame", {}).get("anyOf") or []
    pure_ref_members = [m for m in frame_members if isinstance(m, dict) and set(m.keys()) == {"$ref"} and isinstance(m["$ref"], str)]
    check("A5 schema: WsGatewayFrame.anyOf 恰好六个分支且全为纯 $ref（除 $ref 外无键）",
          len(frame_members) == 6 and len(pure_ref_members) == 6,
          f"分支 {len(frame_members)} 个，纯 $ref {len(pure_ref_members)} 个：{json.dumps(frame_members, ensure_ascii=False)[:200]}")
    frame_names = set()
    dangling_frame_refs = []
    for m in pure_ref_members:
        ref = m["$ref"]
        if not ref.startswith("#/definitions/"):
            dangling_frame_refs.append(ref)
            continue
        name = ref.removeprefix("#/definitions/")
        if name not in defs:
            dangling_frame_refs.append(ref)
        else:
            frame_names.add(name)
    check("A5 schema: 六帧 $ref 引用全部有效（无悬空）", not dangling_frame_refs, f"悬空 {dangling_frame_refs}")
    check("A5 schema: 六帧分支名 = 预期集合", frame_names == EXPECTED_FRAMES,
          f"实际 {sorted(frame_names)}")
    if frame_names == EXPECTED_FRAMES and not dangling_frame_refs:
        consts_by_frame = {name: frame_type_consts(defs[name]) for name in EXPECTED_FRAMES}
        all_consts = set().union(*consts_by_frame.values())
        check("A5 schema: 帧类型常量 = 快照 frameTypes", all_consts == set(snapshot.get("frameTypes", [])),
              f"schema {sorted(all_consts)} vs 快照 {sorted(snapshot.get('frameTypes', []))}")
        check("A5 schema: 每帧 type 常量唯一", all(len(v) == 1 for v in consts_by_frame.values()),
              str({k: sorted(v) for k, v in consts_by_frame.items() if len(v) != 1}))
    # A5b request.method 必须恰是 $ref → WsGatewayMethod（生成器若退化成开放 string，此处 FAIL）
    req_method = (defs.get("WsRequestFrame", {}).get("properties") or {}).get("method")
    check("A5 schema: WsRequestFrame.method 恰为 $ref → WsGatewayMethod（不得退化为开放 string）",
          req_method == {"$ref": "#/definitions/WsGatewayMethod"},
          f"实际 {json.dumps(req_method, ensure_ascii=False)[:120]}")
    check("A5 schema: WsRequestFrame.required 含 method",
          "method" in ((defs.get("WsRequestFrame") or {}).get("required") or []))

    # A6 开放联合
    channel = defs.get("GatewayChannelKey")
    check("A6 schema: GatewayChannelKey 保持开放字符串", is_open_string(channel), json.dumps(channel, ensure_ascii=False)[:200])
    if channel is not None:
        literals = []
        collect_enums(channel, literals)
        missing, extra = set_diff(literals, snapshot.get("channelKeyLiterals", []))
        check("A6 schema: GatewayChannelKey 字面量分支与快照一致", not missing and not extra,
              f"schema 多出 {missing}；schema 缺 {extra}")
    # GatewayEvent 的 plan_mode_changed 变体：mode 是 GatewayMode | (string & {})——开放且含全部 GatewayMode 字面量
    plan_mode_variant = None
    for member in (defs.get("GatewayEvent", {}).get("anyOf") or []):
        props = member.get("properties") or {}
        if isinstance(props.get("type"), dict) and props["type"].get("const") == "plan_mode_changed":
            plan_mode_variant = member
            break
    check("A6 schema: GatewayEvent 存在 plan_mode_changed 变体", plan_mode_variant is not None)
    if plan_mode_variant is not None:
        mode_prop = (plan_mode_variant.get("properties") or {}).get("mode")
        check("A6 schema: plan_mode_changed.mode 保持开放字符串（GatewayMode | (string & {})）",
              is_open_string(mode_prop), json.dumps(mode_prop, ensure_ascii=False)[:200])
        # 字面量分支以 $ref → GatewayMode 携带（枚举本体在 A8 与快照核对），这里断言形状恰为
        # 「$ref 字面量分支 + 开放 string 分支」二支——多退少改都在此 FAIL
        members = mode_prop.get("anyOf") or [] if isinstance(mode_prop, dict) else []
        shape_ok = (len(members) == 2
                    and {"$ref": "#/definitions/GatewayMode"} in members
                    and {"type": "string"} in members)
        check("A6 schema: plan_mode_changed.mode 恰为 [$ref→GatewayMode, 开放 string] 两支",
              shape_ok, f"实际 {json.dumps(members, ensure_ascii=False)[:200]}")

    # A7 协议版本（四处一致）
    v_schema = (schema.get("x-pilotdeck") or {}).get("protocolVersion")
    v_snap = snapshot.get("protocolVersion")
    v_hello = ((defs.get("WsHelloFrame", {}).get("properties", {}) or {}).get("protocolVersion", {}) or {}).get("x-pilotdeck-protocol-version")
    v_hello_ok = ((defs.get("WsHelloOk", {}).get("properties", {}) or {}).get("protocolVersion", {}) or {}).get("x-pilotdeck-protocol-version")
    check("A7 协议版本: schema 元数据 = 快照 = hello 注解 = hello_ok 注解",
          v_schema == v_snap == v_hello == v_hello_ok and isinstance(v_schema, str),
          f"schema={v_schema} 快照={v_snap} hello={v_hello} hello_ok={v_hello_ok}")
    check("A7 协议版本: hello/hello_ok 的 protocolVersion 类型仍是 string（注解不约束校验）",
          ((defs.get("WsHelloFrame", {}).get("properties", {}) or {}).get("protocolVersion", {}) or {}).get("type") == "string"
          and ((defs.get("WsHelloOk", {}).get("properties", {}) or {}).get("protocolVersion", {}) or {}).get("type") == "string")

    # A8 关闭枚举（对照组：该关的不许被放开）
    mode = defs.get("GatewayMode", {})
    check("A8 schema: GatewayMode 是关闭枚举且与快照一致",
          mode.get("type") == "string" and isinstance(mode.get("enum"), list)
          and mode["enum"] == snapshot.get("gatewayModeLiterals"),
          f"schema={mode.get('enum')} 快照={snapshot.get('gatewayModeLiterals')}")
    client_name = defs.get("GatewayWsClientName", {})
    check("A8 schema: GatewayWsClientName 是关闭枚举且与快照一致",
          client_name.get("type") == "string" and isinstance(client_name.get("enum"), list)
          and client_name["enum"] == snapshot.get("gatewayWsClientNameLiterals"),
          f"schema={client_name.get('enum')} 快照={snapshot.get('gatewayWsClientNameLiterals')}")
    check("A8 schema: WsHelloFrame.clientName 恰为 $ref → GatewayWsClientName（不得内联放开或改枚举）",
          (defs.get("WsHelloFrame", {}).get("properties") or {}).get("clientName") == {"$ref": "#/definitions/GatewayWsClientName"},
          f"实际 {json.dumps((defs.get('WsHelloFrame', {}).get('properties') or {}).get('clientName'), ensure_ascii=False)[:120]}")

    # A9 roots 齐备（生成器侧已断言一次；这里对已提交产物再断言——防手改 schema 挖走 definition）
    root_types = (schema.get("x-pilotdeck") or {}).get("rootTypes")
    check("A9 schema: x-pilotdeck.rootTypes 是非空列表且含 WsGatewayFrame",
          isinstance(root_types, list) and bool(root_types) and "WsGatewayFrame" in root_types,
          f"实际 {str(root_types)[:120]}")
    if isinstance(root_types, list):
        missing_roots = sorted(set(root_types) - set(defs))
        check("A9 schema: rootTypes 全部有 definition", not missing_roots, f"缺 definition 的根 {missing_roots}")
    payload_aliases = snapshot.get("payloadAliases")
    check("A9 快照: payloadAliases（按方法导出的内联载荷）非空列表", isinstance(payload_aliases, list) and bool(payload_aliases))
    if isinstance(payload_aliases, list) and isinstance(root_types, list):
        alias_names = [a[k] for a in payload_aliases for k in ("input", "result") if isinstance(a, dict) and a.get(k)]
        check("A9 快照 × schema: payloadAliases 全部有 definition 且列入 rootTypes",
              bool(alias_names) and all(n in defs and n in root_types for n in alias_names),
              f"缺 definition: {sorted(n for n in alias_names if n not in defs)}；缺 rootTypes: {sorted(n for n in alias_names if n not in root_types)}")
        check("A9 快照: payloadAliases 别名无重复", len(alias_names) == len(set(alias_names)))
    refs = []
    collect_refs(schema, refs)
    dangling = sorted({r for r in refs if not valid_local_ref(schema, r)})
    check("A9 schema: 全文档无悬空 $ref", not dangling, f"悬空 {dangling}")

    # ── 本地：基线核对（基线目录存在才跑；CI 常规 job 跳过，CI 基线 job 用 PDRS_REQUIRE_BASELINE=1 钉死）──
    baseline, require_baseline = resolve_baseline()
    if baseline is None:
        if require_baseline:
            check("基线: PDRS_REQUIRE_BASELINE=1 时基线目录必须存在", False,
                  "未找到 resources/PilotDeck（候选：$PDRS_BASELINE → 各级祖先 resources/PilotDeck）")
        else:
            print("[SKIP] 本地基线核对（未找到 resources/PilotDeck；设 PDRS_REQUIRE_BASELINE=1 可把缺基线判为 FAIL）")
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

        # L3 version.ts 常量与 types.ts / frames.ts 字面量
        m = re.search(r'PILOTDECK_GATEWAY_PROTOCOL_VERSION\s*=\s*"([^"]+)"', (proto / "version.ts").read_text(encoding="utf-8"))
        check("L3 基线 version.ts: 常量与快照/schema 一致", m and m.group(1) == v_snap == v_schema, f"version.ts={m.group(1) if m else None}")

        types_text = (proto / "types.ts").read_text(encoding="utf-8")
        for name, snap_key in (("GatewayChannelKey", "channelKeyLiterals"), ("GatewayMode", "gatewayModeLiterals")):
            mm = re.search(rf"export type {name} =\s*(.*?);", types_text, re.S)
            lits = re.findall(r'"([^"]+)"', mm.group(1)) if mm else []
            check(f"L3 基线 types.ts: {name} 字面量与快照一致", lits == snapshot.get(snap_key),
                  f"types.ts={lits} 快照={snapshot.get(snap_key)}")
        mm = re.search(r"export type GatewayWsClientName =\s*(.*?);", frames_text, re.S)
        lits = re.findall(r'"([^"]+)"', mm.group(1)) if mm else []
        check("L3 基线 frames.ts: GatewayWsClientName 字面量与快照一致", lits == snapshot.get("gatewayWsClientNameLiterals"),
              f"frames.ts={lits} 快照={snapshot.get('gatewayWsClientNameLiterals')}")

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
