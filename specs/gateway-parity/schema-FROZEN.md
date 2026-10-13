# 冻结记录 · gateway 协议 schema（Issue A3）

> 2026-10-14。铁律 10 两问——**作品怎么验证**：`python3 scripts/gateway-parity/check_schema.py`（CI `check` job 内建；本地另跑 `node scripts/gateway-parity/export_schema.mjs --check` 验再生成一致）；**真相源是谁**：基线源码 `resources/PilotDeck/src/gateway/protocol/{frames,types,version}.ts`（本仓外，只读），仓内以 `schema/gateway-methods.snapshot.json` 为一致性基准。改 schema 内容的正确路径是**改基线上游后再导出**，不是手改 schema 文件。
>
> 本目录的 acceptance / normalization / accepted-diffs 与 FROZEN.md 属 Issue A2，届时另登记；本文件只冻 A3 的 schema 产物与断言脚本。**冻结后改任何一份都要另开 issue、两家评审同意、写明理由**（`CLAUDE.md` §五「尺子不许自己改」）。

| 文件 | sha256（前 16 位） | 冻结时间 | 说明 |
|---|---|---|---|
| `schema/gateway-protocol.schema.json` | `8a7218f0b85b01e8…` | 2026-10-14（**AI 代批，待人复核**） | 根 `$ref #/definitions/WsGatewayFrame`；draft-07；101 method 枚举；协议版本 1.2 以 `x-pilotdeck` 元数据与 hello/hello_ok 属性注解携带（注解不约束校验） |
| `schema/gateway-methods.snapshot.json` | `247bf2dd12d483d5…` | 2026-10-14（**AI 代批，待人复核**） | 方法清单快照（101 个、有序）+ 帧类型 + GatewayChannelKey/GatewayMode 字面量 + 基线三源文件 sha256 与锚点 frames.ts:21-122 |
| `scripts/gateway-parity/export_schema.mjs` | `2931915755250ec6…` | 2026-10-14（**AI 代批，待人复核**） | 导出器（可重复：键排序序列化 + `--check` 双生成自比对）；生成器缺口对策见文件头注释 |
| `scripts/gateway-parity/check_schema.py` | `ead0ff13f73182fe…` | 2026-10-14（**AI 代批，待人复核**） | 断言 A1–A8（CI 可跑）+ L1–L3（基线目录存在才跑）；阳性对照：快照改名→3 FAIL、清单删行→4 FAIL |
| `scripts/gateway-parity/package.json` | `cb12839b699aaafb…` | 2026-10-14（**AI 代批，待人复核**） | 依赖钉死：ts-json-schema-generator@2.9.0 + typescript@5.9.3（与基线 TS 版本一致），只装本目录 |
| `scripts/gateway-parity/package-lock.json` | `f9af4644f52a52d9…` | 2026-10-14（**AI 代批，待人复核**） | npm lockfile（node_modules 不入库，CI 不需要它） |

校验：

```bash
python3 scripts/gateway-parity/check_schema.py                    # CI 同款：快照 × 清单 × schema 三方断言
node scripts/gateway-parity/export_schema.mjs --check             # 本地：再生成与已提交产物字节一致（需基线目录）
cd schema && shasum -a 256 gateway-protocol.schema.json gateway-methods.snapshot.json
```

冻结口径要点（改动任一即为破冻，须另开 issue）：

- method 分母 = **101**，锚点 `frames.ts:21-122`（与 `docs/research/005_phase-a-gateway/method-inventory.md` 互为锁）。基线漂移时本地断言 L1 会 FAIL 提示重锚。
- `GatewayChannelKey` 等 `(string & {})` 开放联合在 schema 中必须是**开放字符串**（`anyOf` 含无 `enum` 的 `string` 分支）；`GatewayMode` 等**关闭**联合必须保持关闭。
- 协议版本常量 `"1.2"`（基线 `version.ts:1`）出现在：schema `x-pilotdeck.protocolVersion`、快照 `protocolVersion`、hello/hello_ok 的 `protocolVersion` 属性注解 `x-pilotdeck-protocol-version`——四处必须一致，属性类型保持 `string`（不冻结为 `const`，版本协商的错版本帧也要能过校验再被 4001 关闭）。
- 按需拆分子 schema：暂不拆——帧可达类型与 Gateway 接口载荷类型同处一个 definitions 库（单一真相，铁律 7）；确有拆分需要时（如 A4 只发子集给 Rust 侧）再另开条目登记。
