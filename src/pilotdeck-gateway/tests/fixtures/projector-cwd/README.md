# projector artifact store cwd（#879）

Node v22.22.0、固定基线 cc7c4e6，原入口 `new GatewayToolResultArtifactStore(options)`
（`src/gateway/client/GatewayToolResultArtifactStore.ts:31-42` 的 `resolve`）。
10 个场景逐个新进程；import 在 chdir 前，删除 cwd 前不调用 `process.cwd()`，避免 Node
cwd 缓存掩盖 `uv_cwd`。

| case | cwd 正常 | cwd 删除 |
|---|---|---|
| 相对 `rootDir` | 成功，`<root>/files/artifacts`，`cwdReads=1` | `ENOENT` / `uv_cwd` |
| 相对 `rootDir`（先读 cwd） | 成功 | Node 用缓存 cwd 成功；Rust 冷读错误（登记差异） |
| 绝对 `rootDir` | 成功，`cwdReads=0` | 成功，`cwdReads=0` |
| 默认 root，相对 `TMPDIR=../tmp` | 成功，`<root>/tmp/pilotdeck-tool-results` | `ENOENT` / `uv_cwd` |
| 默认 root，相对 `TMPDIR`（先读 cwd） | — | Node 用缓存 cwd 成功；Rust 冷读错误（同上） |
| 默认 root，绝对 `TMPDIR` | — | 成功，`cwdReads=0` |

Rust `runtime::turn::projector_cwd_tests`（源码模块
`src/runtime/turn/projector_cwd_tests.rs`，不挂在 `projector.rs` 的 `#[path]` 上，保证
agent-event-projector 负控拷贝的源码自洽）每格经共享 bounded worker 在独立子进程改 cwd；
父进程 cwd 不变。相对 TMPDIR 删除两格先直接核对 `node_temp_dir` 的 cwd 错误传播，再走
store 构造，避免 `build` 的第二次 cwd 读取掩盖 `node_temp_dir` 回退。环境准备、fixture 解析与
cwd 恢复的失败点只带 `#879 projector cwd setup: …` 消息，不写负控回执；只有产品结果检查
（`node_temp_dir` 错误传播与最终矩阵断言）绑定 `#879 projector cwd matrix: <case>` 专属断言。
`environment_failure_is_not_a_bound_negative_receipt` 用缺失 `cwd` 的 root 实跑 worker，
断言 named FAILED 出现而目标回执文件不存在，即环境失败只能判 INVALID。暖缓存删除格是未批准差异
`specs/gateway-parity/diffs/A6.9-PROJECTOR-CWD-001.md`，不能记为对拍通过。

生成：

```sh
export PDRS_BASELINE=/Users/da/ws/pilotdeck-rs/resources/PilotDeck
node --disable-warning=ExperimentalWarning --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/projector-cwd/generate.mts <新目录>
```

生成器 recursive mkdir、`flag:'wx'` 写 `oracle.json` 并记录源码 SHA256。ignored 重生成守卫：
`runtime::turn::projector_cwd_tests::projector_cwd_regenerates_identically`。

负控先 `python3 src/pilotdeck-gateway/tests/fixtures/projector-cwd/negative_controls.py --check`
核 9 个唯一锚点；提交后指定 `<新证据目录> [变异名 ...]` 运行。脚本要求调用方设置本树
`CARGO_TARGET_DIR`，经 `negative_suite.matrix_main` 的默认 HEAD 守卫、`wait_for_slot`、
`run_owned` 执行正控 → 变异 → 编译 → 恢复；每轮 nonce 回执绑定「测试全名 + 专属断言」，
逐项记 KILLED / SURVIVED / INVALID，任一非 KILLED 即非零退出。锚点过期、编译失败、超时
均为 INVALID，不冒充测试失败。源码未提交就拒跑，不建影子仓库或第二份构建目录。
全量约 9 × 4 次 cargo 调用，可能超过 20 分钟，由协调者提交后在宿主分批运行。
