# EdgeClaw 根内悬挂符号链接真实 TS 金样（#563）

正本：`OpenBMB/PilotDeck@cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`。
`generate.mts` 执行未修改的 `core/file-memory.ts`，只用真实 TS 产出期望值，不手写。
生成前 `bindSourceTree` 校验该提交的整个 `src/` 树；`sourceClosureSha256` 登记入口的
相对 import 闭包（本次 4 个文件，见 oracle 的 `sourceSha256`）。Rust 回放通过共享
`tests/common/baseline.rs` 验证提交、哈希和可选的本地基线。

## 生成

```sh
PDRS_BASELINE=/abs/PilotDeck LANG=en_US.UTF-8 node \
  --disable-warning=ExperimentalWarning \
  --import /abs/PilotDeck/node_modules/tsx/dist/loader.mjs \
  src/pilotdeck-gateway/tests/fixtures/edgeclaw-dangling-links/generate.mts /tmp/dangling-run
```

Node v22.22.0，固定时钟 `2026-10-22T00:00:00.000Z`。两次独立执行逐字节一致，
`oracle.json` SHA-256：
`509c82bb409a6c0f60216bfee69ac53aa178bfa57e178c2f784259cd107fe5a8`。
生成器在写出金样前自断言四件事，否则拒绝产出：读为 `[]`、写后出现
`global/UserIdentity/real.md`、写后与删后都出现根 `MEMORY.md`、删后链接仍在。

## 场景（每个场景独立根目录）

夹具在 store 构造后建立根内悬挂链接
`global/UserIdentity/user-profile.md -> real.md`（`real.md` 不存在），再执行一次操作：

| 场景 | 输入 | 真实 TS 结果 | TS 出处 |
|---|---|---|---|
| `read` | `getMemoryRecordsByIds([link])` | `[]`；链接仍在，无 `real.md`，无 `MEMORY.md` | `file-memory.ts:670-675`（`existsSync` 跟随链接为 false）、`:1053-1068` |
| `write` | `upsertUserProfile(candidate)` | 返回记录；`writeFileSync` 跟随链接创建 `real.md`，链接仍在；`:698` 的 `repairManifests()` 另创建根 `MEMORY.md` | `:1090-1105`、`:693-699`、`:981-1008` |
| `delete` | `deleteEntries([link])` | `{mutatedIds: [], deletedProjectIds: []}`；链接仍在，无 `real.md`；`:1303` 跳过缺失目标后 `:1307` 仍无条件 `repairManifests()`，故新增根 `MEMORY.md` | `:1298-1308`、`:981-1008` |
| `edit-anchor` | `editEntry({relativePath: link, ...})` | 抛 `Memory entry not found: global/UserIdentity/user-profile.md`；不写盘 | `:1218-1225` |

`edit-anchor` 不是偏离项：TS 与 Rust 都报同一文案；它给读 / 写 / 删三条拒绝断言提供
真实 TS 的错误文本锚点，避免手写期望值。

## Rust 判据

`src/pilotdeck-gateway/tests/edgeclaw_dangling_links.rs`：

- `ts_golden_proves_the_link_is_followed`：核对金样确有 TS 的三条跟随行为。
- `dangling_link_read_is_rejected_before_any_io`、`dangling_link_write_is_rejected_before_any_io`、
  `dangling_link_delete_is_rejected_before_any_io`、`dangling_link_edit_is_rejected_before_any_io`：
  每个操作单独一个测试、独立根目录，断言该操作返回 `Memory entry not found: <id>`，
  且全树（含链接与目标缺失）与金样 `initialFiles` 逐字节一致，不创建 `real.md`。拆开是为了
  负控能逐操作证明转红，任一操作回归不会躲在另一个操作的 panic 之后。
- `rust_follows_ts_write_through_the_link_once_the_target_exists`：把 TS 写后的树（链接 +
  `real.md`）交给 Rust 读，证明只拒绝无法证明目标在根内的链接，目标存在时仍按 TS 跟随。
- `real_ts_generator_is_reproducible`（`#[ignore]`）：真实 TS 重新生成并逐字节比对。

产品实现未改：`file_memory.rs:418-466` 的 `within` 对悬挂链接一律
`Memory entry not found`，是账本 A6.6-EDGECLAW-ENTRY-PATH-CONTAIN-001「范围补充：
根内悬挂符号链接」登记的向安全偏离。

## 负控

```sh
python3 src/pilotdeck-gateway/tests/fixtures/edgeclaw-dangling-links/negative.py /tmp/dangling-negative
```

`negative.py` 先跑正控，再分别施加三个只放行单一操作的变异（read / write / delete），
每个变异要求对应操作的测试转红，并断言其余三个操作测试仍绿；最后在 `finally`
恢复原文并重跑正控；记录前后 SHA-256 与日志。要求独占本工作树，不要与其它编译或写入
同时执行。
