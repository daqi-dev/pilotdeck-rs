# 冻结记录 · gateway 协议 schema（Issue A3）

> 2026-10-14 初冻；**2026-10-14 第 2 轮修改**（codex + DeepSeek 两家评审的 P1/P2 全收，即本轮授权——等价于「另开 issue、两家评审同意」：codex P1 run_get 缺省 result / P2 六帧纯引用 / P2 内联载荷导出，DeepSeek P1 CI 基线 / P2 method $ref / P2 开放联合与关闭联合断言 / P2 ajv 正负例，P3 顺手）。
> 铁律 10 两问——**作品怎么验证**：`python3 scripts/gateway-parity/check_schema.py`（CI `check` job 内建）+ `node scripts/gateway-parity/validate_fixtures.mjs`（ajv 正负例，CI 同 job；需先 `npm ci`）；本地另跑 `node scripts/gateway-parity/export_schema.mjs --check` 验再生成一致。CI 另有 `gateway-schema-baseline` job：按冻结 commit `cc7c4e6` 浅克隆 OpenBMB/PilotDeck 跑 `--check` 与 L1–L3（`PDRS_REQUIRE_BASELINE=1`，缺基线即 FAIL）。**真相源是谁**：基线源码 `resources/PilotDeck/src/gateway/protocol/{frames,types,version}.ts`（本仓外，只读），仓内以 `schema/gateway-methods.snapshot.json` 为一致性基准。改 schema 内容的正确路径是**改基线上游后再导出**，不是手改 schema 文件。
>
> 本目录的 acceptance / normalization / accepted-diffs 与 FROZEN.md 属 Issue A2，届时另登记；本文件只冻 A3 的 schema 产物与断言脚本。**冻结后改任何一份都要另开 issue、两家评审同意、写明理由**（`CLAUDE.md` §五「尺子不许自己改」）。

| 文件 | sha256（前 16 位） | 冻结时间 | 说明 |
|---|---|---|---|
| `schema/gateway-protocol.schema.json` | `d8272f367a2b66e1…` | 2026-10-14 初冻 → 同日第 2 轮（**AI 代批，待人复核**） | 根 `$ref #/definitions/WsGatewayFrame`；draft-07；101 method 枚举；协议版本 1.2 以 `x-pilotdeck` 元数据与 hello/hello_ok 属性注解携带（注解不约束校验）。第 2 轮新增：ok:true 响应的 `result` 不再 required（线上 undefined 序列化省略键）；按方法导出的内联载荷别名（18 条，如 `GatewayAbortTurnInput`）进 definitions 与 rootTypes |
| `schema/gateway-methods.snapshot.json` | `b7c2ef5b8ec24da3…` | 2026-10-14 初冻 → 同日第 2 轮（**AI 代批，待人复核**） | 方法清单快照（101 个、有序）+ 帧类型 + GatewayChannelKey/GatewayMode/GatewayWsClientName 字面量 + payloadAliases（按方法内联载荷别名清单）+ 基线三源文件 sha256 与锚点 frames.ts:21-122 |
| `scripts/gateway-parity/export_schema.mjs` | `81321953b91eb9b7…` | 2026-10-14 初冻 → 同日第 2 轮（**AI 代批，待人复核**） | 导出器（可重复：键排序序列化；`--check` 双生成自比对，非 `--check` 只生成一次；写文件先临时再 rename）。内联载荷经 CompilerHost 虚拟注入 types.ts 的别名导出（不落盘、不改基线、不进 sha256）；临时 tsconfig 用完即删 |
| `scripts/gateway-parity/check_schema.py` | `7856947fb51356e6…` | 2026-10-14 初冻 → 同日第 2 轮（**AI 代批，待人复核**） | 断言 A1–A9（CI 可跑）+ L1–L3（基线存在才跑）；`PDRS_REQUIRE_BASELINE=1` 缺基线即 FAIL。六帧=恰好六个纯 `$ref` 分支且引用有效；`WsRequestFrame.method` 恰为 `$ref → WsGatewayMethod`；A9 roots 齐备 + 全文档无悬空 `$ref`。阳性对照：注入 `{}` 第 7 分支 / method 退化 string / 挖 definition / 放开关闭枚举 → 5 项 FAIL |
| `scripts/gateway-parity/validate_fixtures.mjs` | `1330b024536a8743…` | 2026-10-14 第 2 轮新增（**AI 代批，待人复核**） | ajv draft-07 正负例校验器：帧级 + definition 级；与 check_schema.py A9 互为镜像（rootTypes / payloadAliases 齐备） |
| `scripts/gateway-parity/fixtures/gateway-protocol.fixtures.json` | `10087730486d959e…` | 2026-10-14 第 2 轮新增（**AI 代批，待人复核**） | 正负例数据：15 正帧（含 ok:true 省略 result——run_get 查无记录的线上语义）+ 16 负帧 + 18 条 definition 级正负例 |
| `scripts/gateway-parity/package.json` | `0534a4e8e17edbc3…` | 2026-10-14 初冻 → 同日第 2 轮（**AI 代批，待人复核**） | 依赖钉死：ts-json-schema-generator@2.9.0 + typescript@5.9.3（与基线 TS 版本一致）+ ajv@^8.20.0（正负例校验），只装本目录 |
| `scripts/gateway-parity/package-lock.json` | `da7e3735ceb47f50…` | 2026-10-14 初冻 → 同日第 2 轮（**AI 代批，待人复核**） | npm lockfile（node_modules 不入库；CI 的 `npm ci` 按此复现，基线 job 依赖它） |

校验：

```bash
python3 scripts/gateway-parity/check_schema.py                    # CI 同款：快照 × 清单 × schema 三方断言
node scripts/gateway-parity/export_schema.mjs --check             # 本地：再生成与已提交产物字节一致（需基线目录）
(cd scripts/gateway-parity && npm ci && node validate_fixtures.mjs)  # ajv 正负例（CI check job 同款）
cd schema && shasum -a 256 gateway-protocol.schema.json gateway-methods.snapshot.json
```

冻结口径要点（改动任一即为破冻，须另开 issue）：

- method 分母 = **101**，锚点 `frames.ts:21-122`（与 `docs/research/005_phase-a-gateway/method-inventory.md` 互为锁）。基线漂移时本地断言 L1 会 FAIL 提示重锚。
- 基线 commit 冻结为 **`cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`**（OpenBMB/PilotDeck）；CI `gateway-schema-baseline` job 按此浅克隆再生成比对——schema 与快照虽同源生成，基线 job 才证明「冻结源码 ↔ 已提交产物」不漂。升级基线 commit = 破冻。
- `WsGatewayFrame` = **恰好六个纯 `$ref` 分支**（追加 `{}` 等非引用分支、悬空引用都判 FAIL）；`WsRequestFrame.method` 必须恰为 `$ref → WsGatewayMethod`。
- `ok:true` 响应的 `result` **不列入 required**：实现返回 `undefined` 时 `JSON.stringify` 省略该键（`run_get`/`run_reattach` 查无记录，`GatewayWsConnection.ts:187`），按线上 JSON 语义这是合法响应（fixtures 有正例）。
- Gateway 接口的**内联载荷**（无名对象，如 `abortTurn` 参数）以虚拟别名 `Gateway<Method>Input/Result` 按方法导出（快照 `payloadAliases` 登记 16 方法 18 条）；文本逐字取自基线 AST，基线改形状即随动。
- `GatewayChannelKey` 等 `(string & {})` 开放联合在 schema 中必须是**开放字符串**（`anyOf` 含无 `enum` 的 `string` 分支）；`GatewayEvent.plan_mode_changed.mode` 同口径（`[$ref→GatewayMode, 开放 string]` 两支）。`GatewayMode`、`GatewayWsClientName` 等**关闭**联合必须保持关闭。
- 协议版本常量 `"1.2"`（基线 `version.ts:1`）出现在：schema `x-pilotdeck.protocolVersion`、快照 `protocolVersion`、hello/hello_ok 的 `protocolVersion` 属性注解 `x-pilotdeck-protocol-version`——四处必须一致，属性类型保持 `string`（不冻结为 `const`，版本协商的错版本帧也要能过校验再被 4001 关闭）。
- 按需拆分子 schema：暂不拆——帧可达类型与 Gateway 接口载荷类型同处一个 definitions 库（单一真相，铁律 7）；确有拆分需要时（如 A4 只发子集给 Rust 侧）再另开条目登记。
