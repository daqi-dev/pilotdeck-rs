# #983 解码内存诊断

`measure.py` 在 macOS 上启动真实 Rust / TS CLI，经 `127.0.0.1:28999` 的最小 WS mock 发送精确 32 / 64 / 128 MiB 的合法 `result.sessions=[0,0,…]`。逐块发送并流式计算 stdout 哈希，不在测量器内存中构造大数组或大输出。它是诊断工具，不是已实测的 TS 金样，不代表本问题已修复。

必须先构建当前工作树的 CLI。`CARGO_TARGET_DIR` 必须由调用方设置；TS 使用 `PDRS_BASELINE` 指向的只读原件 `src/cli/pilotdeck.ts`，Node 默认 `/opt/homebrew/bin/node`，可由 `NODE22` 指定，版本必须为 v22.22.0。所有运行使用独立 HOME / PILOT_HOME / TMPDIR；不读取用户的 PilotDeck 配置。

```sh
export CARGO_TARGET_DIR=/Users/da/ws/pilotdeck-rs/_build/wt-983/_build/cargo-target
export CARGO_PROFILE_DEV_DEBUG=line-tables-only CARGO_PROFILE_TEST_DEBUG=line-tables-only
export PDRS_BASELINE=/Users/da/ws/pilotdeck-rs/resources/PilotDeck
scripts/cargo-slots.sh && cargo build --offline --manifest-path src/Cargo.toml -p pilotdeck-cli
python3 src/pilotdeck-cli/tests/fixtures/sdk-decode-budget/measure.py _tmp/20261026-issue-983/host-rust-32 rust 32
python3 src/pilotdeck-cli/tests/fixtures/sdk-decode-budget/measure.py _tmp/20261026-issue-983/host-ts-32 ts 32
```

先检查两份 `result.json`，未触发停止条件后再对 64、128 MiB 逐格执行同样命令，使用新的输出目录。禁止同时运行两个格。每格都先以同一个网络 sandbox profile 证明 18789 连接返回 EPERM / EACCES，再启动 mock 与客户端；只开放 28999 出站。此 profile 用于本内存诊断，不替代 D0 装置的文件权限、进程树等完整隔离验收。

输出目录必须不存在。`stderr.txt` 保存原始客户端 stderr 和 `/usr/bin/time -l` 输出；`result.json` 同时保存 time 的峰值、直接 `wait4` 的子进程峰值/退出码、20 ms 间隔的 RSS 采样峰值和 stdout 长度/哈希。macOS 的这些 RSS 字段以字节记录。time 工具报权限错误时不得把缺失的 `rssTimeBytes` 记为通过，须在宿主补跑。`command.json` 保存 argv、隔离环境和 Node 版本，`events` 保存 hello / request / payload 字节数与关闭记录。

单格 120 s 截止；采样超过 6 GiB 即杀本次自有客户端，记录停止原因；监测权限失败同样停止。触发任一停止条件后不继续更大的格。退出码非零或 `serverJoined=false` 的诊断须调查，不把受限环境错误当产品行为。无绕过隔离的降级开关。

2026-10-30 宿主（wt-983）执行记录：32/64 MiB 两格 × Rust/TS 均完成（修前 Rust 64 MiB 触发 >6 GiB 停止条件，按上述规则不再跑 128 MiB 格），原始结果与本轮定级/修复结论见 `_tmp/20261026-issue-983/`（RESULT.md）。此前 2026-10-29 worker 环境曾因 `sandbox_apply: Operation not permitted` 无法前置网络隔离，故该轮未执行大输入。
