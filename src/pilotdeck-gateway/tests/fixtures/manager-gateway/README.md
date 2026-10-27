# U4 manager projection：真实 TS 金样与文件 catalog 矩阵

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

`oracle.json` 的 24 个 operation 覆盖：sessions 成功/过滤（id、alias、无匹配、空串、0、数字）/空目录/fallback/未知 projectKey/catalog 结构化错误/catalog raw error/**null params/缺 params/`projectKey` 数字、数组、对象、布尔**；
browsers 成功/空/缺 sessionKey/结构化错误/raw error/无 provider。非法参数断言 catalog 不被调用。

## 文件 catalog 形状（P1-3）

`createLocalGateway.ts:685` 选择的是 `createNodeSessionCatalog` → `SessionList.ts::listProjectSessions`，
不是 `AsyncTranscriptStorageAdapter` 的投影。Rust `TranscriptSessionCatalog`（`runtime/manager/file_catalog.rs`）
逐函数移植该文件 catalog：

- 字段顺序 `sessionId, summary, lastModified, fileSize, customTitle, aiTitle, firstPrompt, cwd,
  tag, createdAt, parentSessionId, forkedFromTurnId`；
- `cwd = projectRoot`；metadata `title` 为 `"  title  "` 时 `summary` 原样保留空格；
- head/tail 各 64 KiB；尾部半行不参与解析，完整前缀仍产出 session；
- `always-on-(discovery|workspace|report)[:-]` 在 `readdir` 后、打开文件前过滤；
- 大文件先看尾部 snapshot，再退回 64 KiB 流式 metadata 扫描（128 KiB 非 metadata 行上限）；
- `offset/limit` 用 JS `Array.prototype.slice`（负 end 从尾部数）。

## catalog × 写者矩阵

`oracle.json` 的 `matrix` 段由真实 `listProjectSessions()` 对受控 JSONL 目录逐格录制；
Rust `runtime::manager::tests::manager_catalog_matrix_matches_real_ts` 用同一份文件状态
素材化后经 `TranscriptSessionCatalog` 读取，**整对象逐字节**对拍（不再只比 `sessionId`）。

| 格 | 状态 × 事件 × 时机 | TS 期望（实测） | Rust |
|---|---|---|---|
| R1 | 空目录 × list × 任何时刻 | `[]` | 已实测对齐 |
| R2 | 完整 accepted_input × list × 写后 | 1 条完整 SessionInfo | 已实测对齐 |
| R3 | s-1 完整前缀 + 第二次 append 半行 × list × 写中/失败后 | 保留 s-1，summary 来自完整前缀 | 已实测对齐 |
| R4 | 写者 blocked、尚未落盘 × list × append 中 | 只看到 append 前前缀，不等待写者 | `manager_catalog_read_does_not_wait_for_blocked_writer` |
| R5 | append 失败/取消 × list × 失败后 | 保留已提交前缀，无半条 | `manager_catalog_after_cancelled_writer_keeps_committed_prefix` |
| R6 | 普通 session + `always-on-*`（含 compact boundary）× 默认 | 默认隐藏内部；普通列表不受影响 | 已实测对齐 |
| R7 | R6 + `includeInternal:true` | 内部 session 也出现 | 已实测对齐 |
| R8 | metadata title 前后有空格 × list × 写后 | summary 保留空格，含 `cwd` 与完整字段顺序 | 已实测对齐 |
| R9 | 3 条已提交 × limit/offset | 过滤后按 mtime 排序再分页 | 已实测对齐 |
| R10 | limit=-1 / 负 end / 小数 × list | 同 `Array.prototype.slice` | 已实测对齐 + `manager_catalog_paginates_like_js_slice`（另测 NaN/Infinity） |
| R11 | `params:null` / 缺 params × manager_sessions | TypeError，无 code，catalog 不调用 | 已实测对齐 |
| R12 | `projectKey` 非字符串 × manager_sessions | TypeError + `ERR_INVALID_ARG_TYPE`，catalog 不调用 | 已实测对齐 |

C3/C5 的等待全部有界：写者 gate `Condvar::wait_timeout(10s)`，读线程
`recv_timeout(5s)`；失败路径先释放 gate。R4 证明“前驱永不完成”时读仍返回，R5 证明取消后前缀不变。

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

13 个变异：半行尾丢弃、内部 session 过滤移除、`cwd` 移除、summary trim、非法 params 容错、
非字符串 projectKey 字符串化、负 end 分页、browser port 恒 unavailable、错误 code 投影、
sessionKey 过滤移除、catalog limit 改 50、manager 方法误加 LOCAL_METHODS、未知 projectKey 有损 UTF-16。
执行器是 `tests/fixtures/negative_suite.py`：逐项正控 → 变异 → 恢复 → 汇总
KILLED/SURVIVED/INVALID，任一非 KILLED 非零退出。新增产品文件在提交前不在 HEAD，
`mutating_sources` 会拒绝；提交后由协调者运行。`--audit` 只核对锚点。
