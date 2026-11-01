# 决策卡：#132 vendored icu_collator 的范围（AI 代批，待人复核）

- **卡在哪**：`src/vendor/icu_collator` 按上游原样放进了仓库，带 `[[test]]` / `[[bench]]` target 和约 5.7 MB 测试数据。仓库守卫 `unsafe_boundary.rs` 要求 vendored 包只含 lib target，且必须登记在 `VENDORED_PACKAGES`，否则 `compiler_enforces_gateway_boundary` 失败，挡住全量测试。
- **谁受影响**：#132 能否合入；之后所有 vendored crate 的处理口径。
- **能否撤回**：可以（只动 vendored 副本的 manifest 与测试数据，不动算法源码）。
- **动不动尺子 / 意向**：不动。守卫规则不放宽。
- **选项**：
  - A（采用）：裁剪 vendored manifest，删 `[[test]]` / `[[bench]]` 与 `tests/`、`benches/` 下的文件（它们被 workspace `exclude`，从不随 CI 运行）；`VENDORED_PACKAGES` 加 `vendor/icu_collator`；`UPSTREAM.md` 写清删了哪些、上游版本、校验和与恢复方法。和现有 `data/icu_collator_data` 一致。
  - B：给 vendor 放行非 lib target。会放宽守卫，不采用。
  - C：不 vendor，改走自写比较。工作量大、风险高，不采用。
- **原则**：vendored 第三方代码只保留参与构建的 lib target；裁掉的部分在 UPSTREAM.md 留可恢复的记录。可机检：`unsafe_boundary` 的 vendor 只含 lib target 规则已在。
