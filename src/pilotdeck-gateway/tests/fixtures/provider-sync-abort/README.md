#415：四种真实 provider 在 fetch/SDK Promise 同栈拒绝并取消时均抛 AbortError，无重试。仅注入 provider transport，不复制 TS 错误处理；当前 Rust 已对齐，金样用于定向回归。

Node 22.22.0，固定基线 `cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`。生成命令（仓库根执行，输出目录自定）：

```sh
PDRS_BASELINE=/absolute/path/PilotDeck node --import /absolute/path/PilotDeck/node_modules/tsx/dist/loader.mjs \
  src/pilotdeck-gateway/tests/fixtures/provider-sync-abort/generate.mts /tmp/pdrs-provider-sync-abort
```

四组金样均由 `tests/p2_sweep_boundaries.rs::boundary_fixtures_regenerate_twice_from_real_ts` 再生成两次并与入库字节比较（需显式运行 ignored 测试）。
