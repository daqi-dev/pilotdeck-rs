# 后台输出 spill 真 TS 金样（#553 pr1）

`generate.mts` 读取固定基线 cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85 的真实 TaskOutputStore。
运行前用现有 `baseline_source_guard.mjs` 验证整个 src 树与 Git blob，记录入口闭包 SHA-256。
不写 baseline、resources、node_modules；只写指定输出目录和清理自有 /tmp scratch。

```sh
PDRS_BASELINE=/abs/PilotDeck timeout 90 node \
  --import /abs/PilotDeck/node_modules/tsx/dist/loader.mjs \
  src/pilotdeck-gateway/tests/fixtures/background-output/generate.mts /abs/output
```

需要 Node 22、普通非 root 用户（权限负例用真实 chmod 0500）。连续两次生成的 oracle.json 必须逐字节一致。
oracle 只存输入、实际返回的字节数/内容 SHA-256/游标/总量/truncated，以及实际磁盘原始字节 SHA-256，避免兆字节重复数据入库。

11 组场景：配置/未配置下的 999999、1000000、1000001 字节边界；多次追加/默认 cap/Unicode 截边；未 flush 缺口；ENOTDIR 与 EACCES 后重试；ENOSPC 后重试；部分磁盘文件与 ring 拼接；已有日志追加与读文件缺失；磁盘/ring 交界切开 UTF-8；磁盘恰好耗尽 cap 与 close 后读磁盘；未 flush 的 ring 起点切开四字节字符。每次追加从第一字节写完整日志，日志不轮转、不截断；错误后的旧缺口沿用 TS 的物理文件偏移语义，不自行补洞。

ENOSPC 通过替换 Node `fs.promises.appendFile` 在真实 TS 写入调用点注入带 code=ENOSPC 的拒绝；没有填满宿主磁盘。Rust 在同一写入分支用测试专用开关注入失败，其余权限/非目录/文件缺失均为真实文件系统操作。不得将此证据描述为真实磁盘耗尽实验。

Rust `runtime::background::output::oracle_tests::spill_replays_real_ts_oracle` 用同样操作重放；`tests/background_output.rs` 将真实 shell 的 2,220,001 字节输出接入配置目录，并在 dispose 后比较同一 TS 日志金样。`runtime::background::tests::dispose_waits_for_gated_output_write_before_returning` 在 `#[cfg(test)]` 下卡住一次写盘：dispose 在放行前必须保持 pending，放行后返回且原始文件完整、与 TS 摘要一致。`begin_close_rejects_append_before_ring_is_released` 单独约束 begin_close 后、close 前拒绝追加且保留可读 ring。

```sh
# 先提交测试/执行器；源码与被保护测试必须逐字节等于 HEAD。
# 调用方提供与产品 _build/cargo-target 分离的持久 target。
CARGO_TARGET_DIR="$PWD/_build/negctl-target" \
CARGO_PROFILE_DEV_DEBUG=line-tables-only CARGO_PROFILE_TEST_DEBUG=line-tables-only \
python3 src/pilotdeck-gateway/tests/fixtures/background-output/negative.py /abs/new-evidence
```

#994 过渡执行器把变异放在 /tmp 源码副本；持久 target 由调用方维护，保留依赖与增量编译缓存。
每项依次正控、变异、恢复，三个阶段都先 `cargo test --no-run --message-format=json`，再直接运行本次 Cargo 返回的测试二进制。
编译不使用测试墙钟，连续 900 秒没有编译输出才判 INVALID；槽位等待单独限 900 秒。
第一次未变异运行以 900 秒为校准上限，随后同一目标/测试选择的运行上限为 `max(20 秒, 未变异运行耗时 × 5)`；
编译、槽位等待不计入该基线，不再使用整套固定 590 秒预算，运行预算随所选变异/测试数累加。
每个变异后在同一 target 上重编并验证原始源码；编译错误、过期锚点、超时、错位 panic 均记 INVALID，逐项继续并最终非零退出。
`summary.json` 记录所有变异和选择的 KILLED/SURVIVED/INVALID、编译状态及恢复状态；各阶段原始日志与命令/耗时 JSON 同目录保存。

18 个变异覆盖原有范围。`x-dispose-no-flush-await` 绑定 `runtime::background::output::dispose_tests::dispose_without_snapshot_waits_for_gated_write`：无 snapshot store、直接 poll dispose_now 到 flush 等待点，防止最终持久化的第二次 flush 掩盖变异；同一变异还独立绑定 `runtime::background::output::dispose_tests::dispose_without_snapshot_waits_for_cancelled_write`，两项都须被专属断言杀死。TS 矩阵及边界正控见 `../background-dispose-flush/README.md`。判杀绑定完整测试名和该测试 panic 中的目标消息；文件准备、JSON 解析等前置失败有独立 #994 消息，不能充当目标断言。
全套预计超过 20 分钟时仅在开发会话运行 1–2 个名字样本，完整执行交协调者宿主；后台运行使用宿主 `sigint-default.py python3 ...`。
同树仍遵守既有源码守卫的串行约束；无新增 target 锁，Cargo 自己管理持久 target 的构建锁。
SIGTERM/INT 和异常会回收自有子进程并清理源码副本；SIGKILL 遗留的副本按日志核所有权后清理，持久 target 不自动删除。
自测：`python3 -m unittest discover -s src/pilotdeck-gateway/tests/fixtures/background-output -p test_negative.py`。
共享执行器自测/配对负控见 `tests/fixtures/test_negative_adaptive.py` 与 `tests/fixtures/negative-adaptive/negative_controls.py`（后者提交后运行）。

pr2 的 state.json、重启恢复、默认项目/nsjail 配置接线不在本夹具范围。
