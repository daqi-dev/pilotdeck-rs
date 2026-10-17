# #154 describe_project：session 文件级真实 errno

`describe-file-io-errors.json` 为固定基线 cc7c4e6、Node 22.22.0 / ICU 78.2 在
darwin 上录得的三项原件，补充 #141 的十项目录级原件。调用真实
GatewayWsConnection → describeWebProject → listProjectSessions → readSessionLite；
无监听 transport 和隔离 HOME 沿用原录制器，不注入 errno、返回值或会话列表。

| 案例 | 操作及真实错误入口 | 完整 RPC 结果 |
|---|---|---|
| file-positive | 已存在、非空、有标题的 `chats/visible.jsonl`，正常 stat/open/read | sessionCount=1，有 lastActivity、createdAt |
| file-denied | 单个 `visible.jsonl` chmod 000；chats readdir、文件 stat 成功，readSessionLite 的 fsp.open 报 EACCES | sessionCount=0，无 lastActivity，createdAt 保留 |
| file-missing | 真实 chats readdir 返回名称后、readSessionLite stat 之前，unlink 单个 `visible.jsonl`；fsp.stat 报 ENOENT | sessionCount=0，无 lastActivity，createdAt 保留 |

删除必须发生在枚举之后：若先删除再发请求，只会得到空目录，不能证明文件级 errno。
录制器的 `--file-errors` 模式在 observer 外增加一次夹具生命周期操作：等待真实
readdir、确认枚举到了目标、执行真实 unlink、原样返回名称。动作记入 `mutations`，
观察器仍原样调用底层并记录 API、路径、调用栈、成功值和 errno；其原有代际隔离代码
未变。`files` 是 RPC 前快照，因此 missing 的文件字节是删除前事实；回放以 RPC 内
真实 stat ENOENT 覆盖该路径，不能把它解释成删除后文件仍存在。

原件保留完整请求/响应、真实时间、filesystem facts、观测轨迹及前后基线源码绑定。
全部 **63475 字节**的 SHA256 为
`8206d63968542d840326ff1e55501077402b62908e1ffe682e690934355c4fae`，
由 Rust loader 独立字面量锁定，未由 Rust 回填期望。原件直接由 Node 写入，且与 stdout
`PDRS_METADATA_ORACLE` 内存检查点逐字节核对。首次录制命令、stdout 与权限恢复回执在
工作树 `_tmp/20261017-metadata-file-errno/`；不依赖这些本机证据运行 CI。

**CI 覆盖 = 回放 darwin 原件**。每次正常 `cargo test -p pilotdeck-gateway metadata`
都执行三例完整 RPC 字节与实际 Files 请求目标集合比较，无须访问原隔离路径或 chmod。
这不声称 Linux 实际产生相同 errno，也不声称 CI 做过真实权限验收。

`complete_cases()` 复用 #141 的自检，并分别锁定旧十 ID 与新三 ID，不能互相顶替。
新检查要求目录枚举成功且包含目标、文件非空、正确 API/调用栈/errno、列举在 stat
之前，以及 missing 的唯一 unlink 动作；denied 还要求 stat 成功、随后 open 失败。
负控覆盖缺失/重复 ID、NOT_RUN、错方法/目标/结果、缺列名/文件错误、错调用栈/errno、
错误文件事实、缺失/错目标 unlink。两个错误案例的空结果不能代替这些文件调用证据。

Node 的 open 在 Rust Files 层对应 `session_summary`。该接口按产品既有语义吞掉
读取失败并返回 `Ok(false)`，故请求审计比较这个边界的成功调用，原始 EACCES 则由
SHA 锚与完整性检查锁定。Snapshot 在该路径回放已记录的 open 失败，完整 RPC 再确认
会话被排除；missing 的 stat ENOENT 仍按 errno 逐项比较。仅改测试，产品吞错语义不变。

录制用法（使用已安装 Node 22.22.0 与基线依赖，不下载工具）：

```sh
# baseline = 固定 cc7c4e6 源码目录；output 与 scratch 必须全新。
# 预建 scratch 下隔离子目录；HOME、XDG_CONFIG_HOME、XDG_CACHE_HOME、
# XDG_DATA_HOME、TMPDIR、PILOT_HOME 全部指向 scratch 的子目录。
# 设置 SKIP_BROWSER_OPEN=1、PILOTDECK_SKIP_BROWSER_OPEN=1、
# PDRS_METADATA_GIT=/absolute/path/to/installed/git。
node --import ./scripts/gateway-parity/watcher.mjs \
  --import "$baseline/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/record_metadata_io.mts \
  "$baseline" "$output" "$scratch" --file-errors
```

若 chmod 未实际制造 EACCES，案例标 NOT_RUN、录制退出 2，不作为有效原件。
成功录制后保留 stdout 的 `PDRS_METADATA_PERMISSION` 原始计划，再通过原有
`--restore <receipt-output> <original-plan-JSON>` 恢复唯一受限文件权限并核对 stat；
计划字段为 baselineCommit、owner、scratch、permissions，不能从后来可变的 fixture
重建可信计划。本次已恢复 0644，restore 退出 0。删除的是自有合成数据，无需恢复。

不带 `--file-errors` 时继续录原十目录案例；`check_metadata_io.py` 仍只编排该旧模式。
新三例不冒充保留现场 Native 回放：missing 已删除，重跑请求不会再次经历同一竞态。
不改原目录 JSON、不改冻结件、不推进 #139 的关闭或人工验收状态。
