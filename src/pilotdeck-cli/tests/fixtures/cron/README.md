# Cron CLI 的 TS 观测金样

`ts-oracle.json` 来自固定的 PilotDeck `cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85` 和 Node `v22.22.0`。生成器启动该 checkout 的真实 `src/cli/pilotdeck.ts`，不运行 Rust；以独立临时目录作为 `HOME`、`PILOT_HOME`、cwd，使用假 token 和仅监听 `127.0.0.1:18789` 的 HTTP/WebSocket 模拟服务，不调用 provider 或真实 Gateway。各 case 的参数、服务回复和环境写在 `generate.mjs`；输出中 `native_exit: null` 表示观察 3 秒仍未退出，随后脚本发送 SIGTERM 清理，并不代表进程退出码为零。金样的打开/关闭/退出字段是基线观测，Rust 测试对已登记的终态关闭修复单独断言。清理不会写入用户目录。

重采前确保端口空闲；脚本与 Rust 测试共用 `/tmp/pdrs-cron-18789.lock` 目录租约，最多等 60 秒、不自动清除已有租约；脚本独占绑定，端口已有监听则报错，绝不连接现有服务。依赖须已在基线 checkout 的 `node_modules` 中，不下载任何东西。将 `TS_CHECKOUT`、`NODE_BIN` 和一个尚不存在的 `EVIDENCE_DIR` 替换为绝对路径：

```sh
"$NODE_BIN" src/pilotdeck-cli/tests/fixtures/cron/generate.mjs --capture "$TS_CHECKOUT" "$NODE_BIN" "$EVIDENCE_DIR"
cmp src/pilotdeck-cli/tests/fixtures/cron/ts-oracle.json "$EVIDENCE_DIR/ts-oracle.json"
```

脚本核验 git HEAD 和 Node 版本，独占创建证据目录，输出 `raw.ndjson` 与 `ts-oracle.json`，不覆盖现有文件。重采与对比是独立步骤，绝不自动覆盖仓内金样。原始记录包含绝对 cwd、UUID、每次真实子进程输出和观测状态；生成金样只把格式校验通过的 request UUID 换为 `<uuid>`，把严格等于本次 cwd 的 `projectKey` 换为 `<cwd>`，其余字段逐值保留。可从任何一次原始记录单独复算到不存在的文件：

```sh
"$NODE_BIN" src/pilotdeck-cli/tests/fixtures/cron/generate.mjs --normalize "$EVIDENCE_DIR/raw.ndjson" "$EVIDENCE_DIR/recomputed.json"
cmp "$EVIDENCE_DIR/ts-oracle.json" "$EVIDENCE_DIR/recomputed.json"
```

十五格覆盖 list/create/delete/stop、显式 channel/project、无冒号 session、无效和溢出 limit、空值、create usage、RPC 错误、HTTP 503、hello 拒绝和无 token。仅为 CLI 与模拟服务的合同证据；不证明 Gateway 实际调度、真实 provider、A8 或 P09。HTTP 重定向与畸形帧未包含于本 TS 金样，不得由此推断通过。17 MiB 单帧另有独立 TS 探针 `_tmp/20261108-issue-124-cron/ts-large-frame.mjs` 与 `.log`，仅证明该大小可通过。原始首采证据及脚本保存在主仓 `_tmp/20261108-issue-124-cron/ts-matrix.ndjson` 和 `ts-matrix.mjs`。

负控需先提交本变更并确认 18789 空闲、获得 cargo 槽；脚本从 HEAD 创建隔离快照，分别先编译正控与三个变异，再运行子进程测试。输出目录须不存在，变异的编译失败、超时或与绑定断言无关的失败都不会判成 KILLED；每棵快照的 target 位于其自身 `_build/cargo-target`，运行前按四棵构建树核对磁盘预算。

固定 TS 源码可从主仓 `resources/PilotDeck/` 只读访问：`src/cli/pilotdeck.ts:527-557` 明确定义 flag、有限整数和 channel 推导；`src/gateway/client/probeServer.ts:16-20` 定义固定端口及 200ms 健康预算；`src/gateway/client/GatewayWsClient.ts:86-125` 将 10 秒 hello 等待放在 WebSocket open、发送 hello 之后。不要把测试宿主的绝对路径当作本机唯一源码位置。21 次重定向另有真实 TS 探针，证据在主仓 `_tmp/20261108-issue-124-cron/ts-redirect-limit.mjs` / `.log`。

```sh
python3 src/pilotdeck-cli/tests/fixtures/cron/negative_controls.py "$NEW_OUTPUT_DIR"
```
