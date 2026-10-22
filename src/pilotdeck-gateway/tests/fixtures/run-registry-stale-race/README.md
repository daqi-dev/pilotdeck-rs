#216：已知 TS 竞态的证据，不能当作已修复或互斥保证。actor.mts 仅把真实 unlink 调用暂停在 IPC 闸门：两进程均完成 TS 的 stat/read 后，A unlink 并重新取锁，再让 B 执行旧 unlink。双方 FileHandle 仍有效，A 的 inode 已从锁路径移除。未改写 TS 源码；另采正常陈旧死 owner 锁的回收行为。Rust 本轮保持现有协议，单测对拍普通回收，不宣称 Rust 两进程竞态已消除。

Node 22.22.0，固定基线 `cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`。生成命令（仓库根执行，输出目录自定）：

```sh
PDRS_BASELINE=/absolute/path/PilotDeck node --import /absolute/path/PilotDeck/node_modules/tsx/dist/loader.mjs \
  src/pilotdeck-gateway/tests/fixtures/run-registry-stale-race/generate.mts /tmp/pdrs-run-registry-stale-race
```

四组金样均由 `tests/p2_sweep_boundaries.rs::boundary_fixtures_regenerate_twice_from_real_ts` 再生成两次并与入库字节比较（需显式运行 ignored 测试）。
