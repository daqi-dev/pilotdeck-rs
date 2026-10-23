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

8 组场景：配置/未配置下的 999999、1000000、1000001 字节边界；多次追加/默认 cap/Unicode 截边；未 flush 缺口；ENOTDIR 与 EACCES 后重试；ENOSPC 后重试；部分磁盘文件与 ring 拼接；已有日志追加与读文件缺失。每次追加从第一字节写完整日志，日志不轮转、不截断；错误后的旧缺口沿用 TS 的物理文件偏移语义，不自行补洞。

ENOSPC 通过替换 Node `fs.promises.appendFile` 在真实 TS 写入调用点注入带 code=ENOSPC 的拒绝；没有填满宿主磁盘。Rust 在同一写入分支用测试专用开关注入失败，其余权限/非目录/文件缺失均为真实文件系统操作。不得将此证据描述为真实磁盘耗尽实验。

Rust `runtime::background::output::oracle_tests::spill_replays_real_ts_oracle` 用同样操作重放；`tests/background_output.rs` 将真实 shell 的 2,220,001 字节输出接入配置目录，并在 dispose 后比较同一 TS 日志金样。

```sh
# 只使用本树 src/target；脚本串行执行源码变异，finally 恢复原字节。
# 运行前先调用项目 cargo-slots.sh，并确认没有同树编译/写入。
timeout 620 python3 src/pilotdeck-gateway/tests/fixtures/background-output/negative.py /abs/negative-output
```

负控有总期限 590 秒、每次 cargo/test 最长 100 秒、每进程 test-threads=4，超时杀自有进程组。源码变异覆盖 ring ±1、读序、未配置误启用落盘、仅溢出写盘、覆盖旧日志、跳过 flush、禁止目录准备/错误后不重试、缺口标志、close 后仍收输出；编译错误/沙箱错误不计杀死变异。

pr2 的 state.json、重启恢复、默认项目/nsjail 配置接线不在本夹具范围。
