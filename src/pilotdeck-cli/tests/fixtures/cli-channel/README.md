# 旧默认 CLI 的远端 channel 金样

固定 PilotDeck `cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`、Node `v22.22.0`。`generate.mjs` 在独立临时 home/project 下启动真实 TS CLI，依赖 checkout 现有的 `tsx` 和 `ws`，使用固定端口的假 HTTP/WS、虚构 token 和永不调用的本地 model URL。它不会下载依赖；采集器与 Rust 测试共用 `/tmp/pdrs-cron-18789.lock`（最多等待 60 秒、不清理别人的租约），随后独占绑定 18789，已有真实 listener 则拒绝继续。

```sh
"$NODE_BIN" src/pilotdeck-cli/tests/fixtures/cli-channel/generate.mjs --capture "$TS_CHECKOUT" "$NODE_BIN" "$NEW_EVIDENCE_DIR"
cmp src/pilotdeck-cli/tests/fixtures/cli-channel/ts-oracle.json "$NEW_EVIDENCE_DIR/ts-oracle.json"
"$NODE_BIN" src/pilotdeck-cli/tests/fixtures/cli-channel/generate.mjs --boundary "$TS_CHECKOUT" "$NODE_BIN" "$NEW_BOUNDARY_DIR"
cmp src/pilotdeck-cli/tests/fixtures/cli-channel/boundary-oracle.json "$NEW_BOUNDARY_DIR/boundary-oracle.json"
```

三个路径分别指固定 SHA checkout、Node 22.22.0 和尚不存在的证据目录；两轮采集须使用不同新目录。脚本核验版本、独占创建目录，并写 `raw.ndjson` 与 `ts-oracle.json`，不覆盖仓内文件。金样只将请求 UUID 换成 `<uuid>`、将严格匹配 cwd 的 sessionKey 替换为 `cli:project=<cwd>:default`，其余原值保留；stdout/stderr、观察前关闭状态和退出状态来自 TS 子进程。

测试按真实 TS 金样比较 remote submit 参数、渲染 stdout 和 final 消费；Rust 故意关闭终态连接，对应单独差异登记。基线在 4.5 秒观察期内不退出，脚本发送 SIGTERM 清理；不是业务请求的超时标准。原始 TS 启动在非 Git 隔离项目额外打印两行 Git fatal，Rust 此片尚未完成 `createLocalGateway` 全部启动副作用，因此本测试不把这些 stderr 字节标为已兼容。乱序重复 seq 的独立 TS 探针位于主仓 `_tmp/20261108-cli-channel/probe-irregular.mjs` 与 `.ndjson`；Rust 测试使用相同事件并重排 seq，禁止复用 SDK 的序号策略。

边界金样覆盖裸 CR/CRLF/无终止符 EOF、仅 final、流中断和关闭 stdout 读端；其中 stderr 只保存经真 TS 原始输出验证的 EPIPE / 断线标记，完整含 Node 栈的原始记录保存在证据目录，不将环境绝对路径固化进仓库。TS 输出 EPIPE 时为 Node 堆栈，Rust 为本机 IO 错误，具体文本目前未对齐。

本测试不证明本地 fallback、native provider、模型历史、A8 或阶段 D 验收。本地 fallback 已有独立接线与测试，不能由本册远端金样推导其结论；真实组合、认证 failure、复杂输入和人工使用以相应车道证据为准。

## 拒绝响应补充

`rejection-probe.mjs <fixed-ts> <node22> <rust-bin> <new-output>`在mbp2采同ID拒绝、异ID拒绝加正常event、正常event三格，两侧各一遍。它使用旧入口固定18789的独占测试锁并自行bind；不能在用户实际服务所在机器运行。所有home/project与token隔离，二秒观察期限及自有PID清理保留原方法。此脚本是观察器，不把旧TS的进程存活当作通过或永远挂起的单独证明。

修复回归在`cli_channel_exit.rs::rejected_submit_exits_without_waiting_for_final_or_peer_close`：六种响应及排队stdin，peer持续开放而客户端应关闭并非零退出；异ID错误/成功ACK后仍消费正常event。精确错误输出和UTF-16异常输出按`D2-CLI-REJECTED-SUBMIT-001`登记。mbp2严格clippy、原矩阵与新进程测试通过，原件见主仓`_tmp/20261108-cli-tui/defaultremote-rejection/r1-evidence/`。

## 默认入口 token 文件边界

Unix 默认 channel 在 health 成功后复用 SDK 的同 fd 有界 reader：最多接受 65536 原始字节；FIFO 等特殊文件或超限明确退出 1，不转入 LocalHost，普通 IO/目录/空 token 保持原回退。非 Unix 保留旧读器，不计入此资源保证。范围及精确错误见 `specs/gateway-parity/diffs/D1-SDK-TOKEN-READ-BOUNDS-001-R1.md`，原 SDK 条目仍是历史 SDK 范围，原金样未改。

进程回归 `default_channel_token_files_are_bounded_and_preserve_ordinary_wire` 实际先回 health 200，再核四个资源拒绝、两个固定 TS 普通 wire/stdout 正控和一个 SIGTERM 回收格；仅持有独占端口租约且 bind 成功才启动 CLI，不访问真实 home/服务。全部 Rust 由协调者在 mbp2 r82 验证，本文候选尚不计通过；此回归不新增 TS 异常文件时序或普通 IO 回退的进程实采。
