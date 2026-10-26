# transcript resolve：#755 pr1

`generate.mts` 用 Node v22.22.0 直接导入固定基线 `cc7c4e6` 的原文件：主入口 `createGatewayAsyncTranscriptStorageAdapter().createSessionStorage`，默认侧链入口 `JsonlTranscriptWriter.forSubagent`。不复制产品函数体；每格子进程先完成 import，再 chdir，删除后不提前读 cwd，避免 Node 的成功 cwd 缓存影响结果。进程超时 30 秒即失败。除随机 scratch 根前缀替换为 `<root>` 外，不掩蔽路径或错误。

| case | 真实 TS 结果 | Rust 断言 |
|---|---|---|
| relative-normal | 相对 home 规范化成绝对路径 | 原路径全部相等 |
| relative-deleted | ENOENT uv_cwd | 同 message + I/O NotFound；list/rewind 传播 |
| absolute-normal | 绝对路径词法规范化 | 原路径全部相等 |
| absolute-deleted | 与 absolute-normal 相同 | 同结果，不能先读 cwd |
| project-relative-deleted | ENOENT uv_cwd | toolResultsDir 的相对 projectRoot 传播错误 |
| join-relative-normal | `logs/s/subagents/kid.jsonl` | **保留既有差异**：Rust 原来绝对化，本包不改正常输出 |
| join-relative-deleted | `logs/s/subagents/kid.jsonl` | 保留原 fallback，不能让 path.join 入口新报错 |

`project-relative-deleted` 的 TS 原入口可能先在 project-id 计算中读取 cwd，Rust 的项目布局由宿主提供；这里对齐入口可见错误，不声称内部失败阶段相同。当前测试不改写 store key 的 projectRoot/pilotHome。

正常主 storage 两种 layout（主侧链和嵌套默认侧链）都比对完整路径。相对 Default 分支原有 TS/Rust 差异只记录，不在本包修正；`relativeSubagentPath` 也不扩展。

重生成：设置 `PDRS_BASELINE` 到只读基线，运行 `node --disable-warning=ExperimentalWarning --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" src/pilotdeck-gateway/tests/fixtures/transcript-resolve/generate.mts <新目录>`。生成器使用 recursive mkdir 和 `flag:'wx'`。Rust ignored 守卫为 `transcript_resolve_regenerates_identically`，通过现有 `baseline::regenerates_identically("transcript-resolve")` 要求 oracle 字节一致。

负控：提交候选后，由协调者先设置本树 `CARGO_TARGET_DIR` 与 `CARGO_PROFILE_DEV_DEBUG=line-tables-only CARGO_PROFILE_TEST_DEBUG=line-tables-only`，运行 `python3 src/pilotdeck-gateway/tests/fixtures/transcript-resolve/negative_controls.py <新证据目录>`。九项变异覆盖：base/projectRoot 两处 cwd 空串回退、绝对 base 先读取 cwd、默认侧链丢失 fallback、list/rewind 错误消息被篡改、正常相对/绝对路径漏做规范化、正常默认侧链改成相对输出。list/rewind 两项保证矩阵通过后真正走到对应传播断言。每项必须正控通过 → 指定运行时断言失败 → HEAD 原字节恢复后正控通过。cargo slot 等待最多 600 秒、单次 cargo 最多 600 秒，均走共享 owned process helper。未设置 target、源码不等于 HEAD、编译失败、setup 失败都不能算击杀；不创建影子仓库或第二构建目录。
