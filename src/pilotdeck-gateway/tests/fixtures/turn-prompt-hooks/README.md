# #741 turn-prompt-hooks 夹具

`generate.mts` 用真实 `TurnRunner` + `LifecycleRuntime` + `HookRuntime`（callback/command 两类执行器）观察 UserPromptSubmit 钩子矩阵（none/pass/blocks/context/block/throw/timeout/reject/pending 等系列），shell 超时等失败缝是确定性端口结果；输出 `oracle.json` 由 `runtime::turn::runner::tests::prompts`（runner_prompt_tests.rs）经 `include_str!` 消费逐行比对。重生成守卫为同模块 `regenerates_identically`：经共享 `tests/common/baseline.rs::regenerates_identically` 只比较 `oracle.json` 字节。本夹具的自动化负控面是 `crate::mutants::on` 编译开关（`schemata.py` 驱动，#996 口径）。

## #2116 `.generation-owner` 判据与手工负控

`generate.mts` 的 `.generation-owner` 是对 OUT 的首个 `wx` 写入（早于逐格 `<name>.jsonl`），身份串 `turn-prompt-hooks\n`：OUT 已被外来 marker 标记时，生成器在写出任何输出前于该独占 open 处以 EEXIST 退出，外来 owner 字节不被覆写。本夹具不分配 `run-*` 工作根，marker 在此不承载清理所有权（那由 session-factory / turn-runner-tail / projection-cache 侧用例承载），承载输出目录的跨生成器独占。

重生成守卫只重放 `oracle.json` 字节，marker 不进入比较面（#1500/#2116 口径）；marker 判据由专用用例 `prompt_hooks_generator_writes_owned_marker` 承载：成功格钉 marker 存在且内容为身份串，冲突格预置外来 `.generation-owner`，断言失败点落在独占 open（输出同时含 `EEXIST` 与 `.generation-owner`）且外来字节保留。

按 #2116 两条 issue 评论的结论，本轮不为这些 `#[ignore]` marker 断言登记自动化负控（不新增 `crate::mutants::on` 开关、不动负控表）；判据强度由交付证据中的手工负控留档保证：`{flag:'wx'}`→`{flag:'w'}`、marker 写入改 no-op、身份串换邻居夹具（`turn-runner`）三只 `generate.mts` 变异各使绑定断言 FAILED（KILLED），恢复后复跑通过。复跑正控的钉版环境（Node v22.22.0 / ICU 78.2、`PDRS_BASELINE` 指向 `cc7c4e65` 检出、从仓库根加 `--manifest-path src/Cargo.toml`）统一见上级 [`fixtures/README.md`](../README.md) 的「手工负控钉版环境（#2304 条 3）」段；`test result:` 行须为 `1 passed; 0 failed`：

```sh
cargo test --offline --manifest-path src/Cargo.toml -p pilotdeck-gateway --lib \
  runtime::turn::runner::tests::prompts::prompt_hooks_generator_writes_owned_marker \
  -- --exact --include-ignored
```

手工负控复跑：对 `generate.mts` 的 marker 写入行分别施加上述三只变异，同命令复跑须 FAILED 于对应 `#2116` 绑定断言（`test result:` 行含 `1 failed`）；每只跑完 `git checkout -- <generate.mts>` 恢复并核对工作区干净，复跑正控须回到 `1 passed; 0 failed`。

#2304（条 2）：本用例的重复形状（Scratch 守卫、命令组装、双格断言、首写 listing、末尾显式 remove）已收敛到共享 `tests/common/baseline.rs::generation_owner_marker_cell`，断言语义与顺序不变，本节复跑口径不变。

第四只变异（#2304 条 5）：在 marker 写入前落一个输出写（等价于把 marker 后移到首个输出写之后），绑定用例 FAILED 于冲突格首写 listing 断言「collision cell writes nothing before the exclusive marker open」，`git checkout --` 恢复后复跑回到 `1 passed; 0 failed`——三只已登记变异都死在更早断言，该 listing 断言由此获得自己的负控留档。
