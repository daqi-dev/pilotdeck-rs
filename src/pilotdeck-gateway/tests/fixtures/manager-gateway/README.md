# U4 manager projection：真实 TS 金样与并发矩阵

固定基线 `PilotDeck cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`，Node `v22.22.0`。
`oracle.json` 由真实 `createLocalGateway()`（`src/cli/createLocalGateway.ts:1259-1270`）
在隔离 HOME / PILOT_HOME 下生成：注入 recording catalog 与 manager browser provider，
逐操作记录 `{ok}` 或 `{error:{name,code?,message}}`，并记录 catalog/browser 的调用参数。
项目路径统一替换为 `<dir>`；`oracle.json` 不含本次运行的临时目录。

## 方法集与形状（Node 实测）

| 方法 | TS 行为 | 响应 |
|---|---|---|
| `manager_sessions` | 先 `resolveProjectKey(input.projectKey ?? fallbackProjectRoot)`，再 `registry.listSessions({projectKey, limit:100})` | `{items: SessionInfo[]}`；`sessionKey` 为 JS truthy 时按 `sessionId === key || sessionKey === key` 过滤；空串/0 不过滤 |
| `manager_browsers` | 直接 `options.managerBrowsers(input)`；不解析 project | provider 返回的 `{items: ...}` 原样返回；无 provider 为 `CAPABILITY_UNAVAILABLE` |

`oracle.json` 的 18 个 operation 覆盖：sessions 成功/过滤（id、alias、无匹配、空串、0、数字）/空目录/fallback/未知 projectKey/catalog 结构化错误/catalog raw error；
browsers 成功/空/缺 sessionKey/结构化错误/raw error/无 provider。

## catalog 与写者共享状态矩阵

`oracle.json` 的 `matrix` 段由真实 `listProjectSessions()` 对受控 JSONL 目录逐格录制；
“写者”是确定性的文件写入（append 前 / 完整行 / 半行 / 内部 session / 分页）。
Rust 对应 `runtime::manager::tests::manager_catalog_matrix_matches_real_ts`，
用同一份文件状态素材化后经 `TranscriptSessionCatalog` 读取，比较有序 `sessionId` 集。

| 格 | 状态 × 事件 × 时机 | TS 期望（实测） | Rust |
|---|---|---|---|
| C1 | 空目录 × list × 任何时刻 | `[]` | 已实测对齐 |
| C2 | 完整 accepted_input × list × 写后 | 1 条 `s-1` | 已实测对齐 |
| C3 | 写者 blocked、尚未落盘 × list × append 中 | 只看到 append 前前缀，不等待写者 | `manager_catalog_read_does_not_wait_for_blocked_writer` |
| C4 | C3 release × list × append 后 | 看到新 session | 同 C3 用例复查 |
| C5 | append 失败/取消 × list × 失败后 | 保留已提交前缀，无半条 | `manager_catalog_after_cancelled_writer_keeps_committed_prefix` |
| C6 | `always-on-*` 内部 session × list × 默认 | 默认隐藏；`includeInternal:true` 才出现 | 已实测对齐 |
| C7 | 3 条已提交 × limit/offset | 内部过滤先于分页；`limit:0` 返回全部 | 已实测对齐 |
| C8 | store 未声明 `list` | 原生 TS 文件 catalog 不适用；provider 契约要求 named unavailable | `manager_catalog_reports_unavailable_store` |

说明：TS `matrix` 使用原生文件 catalog（`SessionList.ts`），而 Rust 的
`TranscriptSessionCatalog` 是既有 `AsyncTranscriptAdapter::list_sessions` 的 read-only
bridge。矩阵比较的是 session 集/顺序与过滤/分页；`cwd` 字段由原生 TS 文件 catalog
额外提供，既有 adapter 投影不产出，属本包不触碰的共享件差异（见 RESULT 自查）。
C3/C5 的等待全部有界：写者 gate `Condvar::wait_timeout(10s)`，读线程
`recv_timeout(5s)`；失败路径先释放 gate。

## 复现（仓根，选全新输出目录）

```sh
export PDRS_BASELINE=/absolute/path/to/PilotDeck
OUT="$(mktemp -d /private/tmp/pdrs-manager-934.XXXXXX)"
mkdir -p "$OUT/isolation/home" "$OUT/isolation/tmp"
env HOME="$OUT/isolation/home" \
  XDG_CONFIG_HOME="$OUT/isolation/xdg" XDG_CACHE_HOME="$OUT/isolation/xdg" \
  XDG_DATA_HOME="$OUT/isolation/xdg" TMPDIR="$OUT/isolation/tmp" \
  PILOTDECK_SKIP_BROWSER_OPEN=1 SKIP_BROWSER_OPEN=1 \
  node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/manager-gateway/generate.mts "$OUT"
cmp "$OUT/oracle.json" src/pilotdeck-gateway/tests/fixtures/manager-gateway/oracle.json

cargo test --manifest-path src/Cargo.toml -p pilotdeck-gateway --lib runtime::manager
# 重生成守卫（需要 PDRS_BASELINE）
cargo test --manifest-path src/Cargo.toml -p pilotdeck-gateway --lib \
  runtime::manager::tests::manager_gateway_fixture_regenerates_identically -- --ignored --exact
```

生成器使用 `mkdir(..., {recursive:true})` 和 `writeFile(..., {flag:'wx'})` 写
`oracle.json`；守卫在全新 scratch 重生成并逐字节比较。

## 负控

```sh
CARGO_TARGET_DIR="$PWD/_build/cargo-target" PDRS_BASELINE="$PDRS_BASELINE" \
  python3 src/pilotdeck-gateway/tests/fixtures/manager-gateway/negative_controls.py \
  _tmp/20261026-issue-934/negative-controls
```

8 个变异：catalog consumer 换回未过滤 transcript 直查、browser port 恒 unavailable、
错误 code 投影、sessionKey 过滤移除、catalog limit 改 50、manager 方法误加
LOCAL_METHODS、未知 projectKey 有损 UTF-16、无 list store 被吞成空目录。执行器是
`tests/fixtures/negative_suite.py`：逐项正控 → 变异 → 恢复 → 汇总 KILLED/SURVIVED/INVALID，
任一非 KILLED 非零退出。新增产品文件在提交前不在 HEAD，`mutating_sources` 会拒绝；
提交后由协调者运行。`--audit` 只核对锚点。
