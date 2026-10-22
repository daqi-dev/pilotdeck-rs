# stats 旁路回归金样（#496）

`generate.mts` 只调用固定基线 `cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85` 的真实 `TokenStatsCollector`。共享 `_shared/baseline.mts` 检查 HEAD、工作树与源码提交内容哈希，拒绝来源漂移；不修改 TS 或其依赖。Node v22.22.0，两次生成的 `oracle.json` SHA256 均为 `59a44ae77f9b1326dd7ff2cdfaba4703a4a5c5ce8954dd942f8e81aaa1a65b9d`。

- `input` 是输入，`expected` 的 enriched 记录、所有快照及主 JSONL 字节由真实 TS 输出。
- `tsRace` 在真实 TS compaction 的 rename 前，通过 Node fs seam 调另一真实 collector 的 observe；真实锁超时降级追加后，主文件被 rename 覆盖。保留原始替换前后字节及快照，钉住本单人批偏离修复的 TS 原版事实。
- Rust 针对旁路、恢复、清空与并发的验收在 `src/runtime/model/stats_journal_tests.rs`；正常输出与真实 TS 对拍，已批准差异按每次观察不丢不重的属性验收。

生成命令（输出目录须不存在或为空，两次各用独立目录）：

```sh
PDRS_BASELINE=/Users/da/ws/pilotdeck-rs/resources/PilotDeck \
  node --import /Users/da/ws/pilotdeck-rs/resources/PilotDeck/node_modules/tsx/dist/loader.mjs \
  src/pilotdeck-gateway/tests/fixtures/token-stats-sidecar/generate.mts /tmp/owned-run-a
```

完整设计、两次生成、负控与验证输出见工作区 `_tmp/20261022-issue-496/RESULT.md`。
