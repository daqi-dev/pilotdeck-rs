# exact-rerun-receipt 负控（#273 / #942）

本目录的 `negative_controls.py` 验证所有子进程 `--exact` 重跑点必须证明 libtest
汇总（`test <name> ... ok` + `1 passed`），不能只看退出码 0：把过滤名改坏或把
worker 子进程切到 terse 格式后，绑定的父测试必须在它自己的 `#273` / `#942`
断言处变红。每行先跑未变异的正控，再编译变异、跑负控、恢复后复跑；任一行非
KILLED 即非零退出，SKIPPED（平台缺失 / 无 `PDRS_BASELINE`）不算失败但
零 KILLED 仍非零。

运行（提交后，宿主）：

```sh
CARGO_TARGET_DIR=<目标目录> PDRS_BASELINE=<固定提交的 TS checkout> \
  python3 src/pilotdeck-gateway/tests/fixtures/exact-rerun-receipt/negative_controls.py <新证据目录>
```

## 已知平台差异（登记，未修）

`entries-locale-pin` / `snapshots-locale-pin` 两行需要真实 TS 基线
（`PDRS_BASELINE`）。在带基线的 **Linux 宿主**上，`entries-locale-pin` 的
正控稳定 INVALID（`real_ts_entries_locale_receipt_survives_single_thread_ambient`
的孙代 `real_ts_entries_regenerates_identically` 在 `common/baseline.rs:350`
红：`#280 edgeclaw-entries/oracle.json differs ... outside the exact masks`）：

- 根因是 Node 对「unlink 一个目录」的平台差异，不是重跑收据逻辑的缺陷。
  金样记录宿主（darwin）上 `unlink '$ROOT/memory'` 返回 `EPERM`；
  Linux Node（v22.22.0 实测）返回 `EISDIR`。oracle 里该行是观测结果，
  不在 `_shared/regeneration-masks.tsv` 的精确掩码内，重生成即逐字节失配。
- 已在 merge-base `43f0db194947` 与本分支 HEAD 各单跑该行验证：两侧同样
  `INVALID: positive control did not pass the bound test`，根因链相同——
  既有平台差异，非本分支引入。
- `snapshots-locale-pin` 不受影响：`edgeclaw-snapshots/oracle.json` 的
  `EPERM` 行是注入故障（faults），两平台一致；Linux 宿主实测父测试通过。
- 修复归 M6 车道：在 `_shared/regeneration-masks.tsv` 为
  `edgeclaw-entries/oracle.json` 的该 `error` / `message` 指针加行级归一
  （不能整段掩掉），或给 `regenerates_identically` 加专用 normalizer。
  在那之前，Linux 带基线宿主跑本套见该行 INVALID 属已知、已登记。
