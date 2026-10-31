# #1128 JsObject.indices 装箱性能实测

`measure.py` 对比 `JsObject.indices` 装箱（#983 fix-r1，HEAD）与未装箱基线
（`c46e3803d^`，只在测量构建中原地反向应用装箱 diff，**不提交旧实现**）在对象密集
负载上的解码 / 紧凑序列化 / pretty 重排 / 析构耗时与峰值 RSS，并与 Node v22.22.0
（`JSON.parse` + `JSON.stringify(x, null, 2)`）同负载对照。它是测量工具，不是
CI 判定：Rust 侧入口是 `src/pilotdeck-protocol/tests/object_density.rs` 的
`#[ignore]` 测量用例，默认测试不执行。

负载矩阵（每类 1 / 8 / 32 MiB，issue ① 的 10^6 个 `{"a":1}` 对应 small-8 的
2^20 个元素；issue ② 的 10^4 层嵌套对应 deep 的固定脊柱深度）：

- `small-*`：`[{"a":1},…]`，小对象数组；
- `wide-*`：单对象 2^16..2^18.4 个 `"k%06d":1` 键，HashMap 密集；
- `deep-*`：10^4 层 `{"a":…,"p":"…"}` 脊柱，`"p"` 每层填充到目标规模；
- `empty-*`：`[{},{},…]`，每 3 字节一个空对象，隔离 #983 装箱的每对象
  `Box` 分配路径（small/wide 格的键、值、哈希桶成本会掩盖它）；
- `transcript`：`src/pilotdeck-gateway/tests/fixtures/compaction-orchestrator/oracle.json`
  （仓内会话转录类金样中最大的一份，3.17 MB，4319 处 `"role"`）；
- `baseline`：`[{"a":1}]`，进程空载 RSS 参照。

```sh
export CARGO_TARGET_DIR=/Users/da/ws/pilotdeck-rs/_build/wt-1128/_build/cargo-target
export CARGO_PROFILE_DEV_DEBUG=line-tables-only CARGO_PROFILE_TEST_DEBUG=line-tables-only
python3 -I src/pilotdeck-protocol/tests/fixtures/jsobject-density/measure.py \
  _tmp/<日期>-issue-1128/host-run            # 全矩阵；或列出子集 workload 名
```

前置条件：`CARGO_TARGET_DIR` 由调用方设置；Node 用 `NODE22` 指定且必须
v22.22.0；启动前 tracked 树必须等于 HEAD。cargo 构建经 `scripts/cargo-slots.sh`
排队。输出目录必须不存在（`.claim` 独占创建）。

安全与口径：

- 未装箱变体 = 在本树原地 `git apply -R` 装箱 diff（锚点提交不可达或锚点内容
  失效即报错退出），整个反向补丁窗口跑在共享 `mutating_sources` 守卫内：树锁
  串行化本树的其它原地运行者，`lib.rs` 进窗前逐字节对 HEAD 校验，出窗恢复
  HEAD 快照并双重核验（绝不用一刀切的 `git checkout --`）。post 与 restored
  两次构建同样持有该树锁（构建是唯一读源码的阶段，测量与 emit 只跑已构建的
  二进制）。`pre-build-in-progress` 标记只在 `lib.rs` 实际低于 HEAD 期间存在：
  反向补丁验证通过才写入，守卫成功恢复后才删除，任何失败路径都会留下标记；
  恢复后重建并复跑一格 spot-check（未跑显式记 `{'status': 'not-run'}`，不与
  通过混淆）。不用影子仓库或第二份构建目录。
- 耗时为 release 构建、每格 warmup 1 + 7 reps 的原始数组（result.json 里），
  汇总取中位数；峰值 RSS 取单 rep 进程的 wait4 `ru_maxrss`（macOS 字节），
  TS 基线进程单列。各格串行执行。
- Rust 侧 pretty 指标 = `stringify_json`（紧凑）+ `write_json_pretty_into`
  的字节语义重排，与产线管线一致（`object_density.rs` 里的本地副本由
  `object_density_pretty_copy_tracks_cli_implementation` 与 CLI 实现钉死：
  函数体文本 `contains` + 字节字面量差分，覆盖不含调用方/管线其余部分）；
  Rust/TS 的 pretty 输出按 1 MiB 档与 transcript 逐字节 sha256 对照（Rust
  装箱前后必须一致，被选格 dump 缺失即失败退出，与 TS 一致为信息项）。
- Node v22.22.0 的 `JSON.stringify(x, null, 2)` 在 10^4 层深嵌套上抛
  `RangeError: Maximum call stack size exceeded`（默认栈、缩进模式递归），
  结果行里如实记录 `stringifyError`，但解析相照常采满全部 reps（deep 格与
  Rust 同样 5 轮 × 7 reps 对照）；Rust 侧 #983 起全迭代。TS dump 缺失只有
  这一种允许情形：该格是 deep 类、且 timed / 写 dump 的 RSS 单 rep 运行至少
  一路实测记录了该 RangeError（错误原文记入 `prettySha256` 的
  `tsStringifyError`，`tsMatchesRust` 记 `null`）；非 deep 格或错误不是
  RangeError 的 dump 缺失一律失败退出，不记 `null` 通过。

证据归档：本机一轮全矩阵见 `_tmp/20261026-issue-1128/host-run/`（RESULT.md
在证据目录，不进提交）。`negative_controls.py` 是 harness 正控
`object_density_harness_selfcheck` 与 CLI 绑定正控
`object_density_pretty_copy_tracks_cli_implementation` 的变异负控（HEAD-only，
提交后由协调者在宿主跑；用法 `--check` / `[--compile-only] NEW_OUTPUT_DIR
[name …]`）。
