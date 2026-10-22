# Chat search TS 金样

`generate.mts` 对固定基线 `cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85` 的真实 `runChatSearchCli` 与 `createNodeSessionSearchPort` 执行 39 个独立 Node 22 子进程。使用共享基线 HEAD、工作区和逐文件 SHA-256 守卫；不改基线或 node_modules。夹具只写临时目录。时间配置为 UTC / C。

```sh
PDRS_BASELINE=/Users/da/ws/pilotdeck-rs/resources/PilotDeck \
node --import /Users/da/ws/pilotdeck-rs/resources/PilotDeck/node_modules/tsx/dist/loader.mjs \
src/pilotdeck-cli/tests/fixtures/chat-search/generate.mts /tmp/pdrs-chat-golden-1
```

在另一输出目录重复运行，再对 `oracle.json` 做逐字节比较。生成器保留 `raw/<case>/stdout.bin`、`stderr.bin`。金样仅按 D0 `compare.py` 的既有规则移除精确的 SQLite 实验警告前缀，并替换本次临时根目录；其他内容不归一化。

D0 的 30 个场景是本 JSON 的子集，沿用相同夹具、argv、stdout、stderr 与退出码，注册于 `scripts/cli-parity/chat-search.json`。通过 `PDRS_CLI_PARITY_SUITE=chat-search` 选择此批次；运行器与比较器均须使用相同变量。全量默认批次保持原样。场景派生可重跑 `node scripts/cli-parity/generate_chat_search.mjs > scripts/cli-parity/chat-search.json`，期望字节直接取自 oracle。

```sh
PDRS_CLI_PARITY_SUITE=chat-search python3 scripts/cli-parity/run_round.py \
  --baseline /Users/da/ws/pilotdeck-rs/resources/PilotDeck \
  --node /opt/homebrew/bin/node --output _tmp/20261023-d2-pr2-chat-search/d0 \
  --rust-bin "$PWD/src/target/debug/pilotdeck" --rust-source "$PWD"
```

D0 保留原隔离能力探针、共享锁、30 场景预算和两轮 TS 定标。宿主不具备能力时结果必须记录为 BLOCKED。

补充边界集 `edges.json` 用同一个生成器的 `edges` 参数运行真实 TS，原有 39 场景的 `oracle.json` 保持原字节。新增 14 个场景覆盖 UTF-16 正则、ASCII 字符类、非 Unicode 忽略大小写、CR 行号、宽松记录形状、标题补丁、缺会话错误和 snippet 孤立代理项。两次在不同输出目录运行并比较 `oracle.json` 后，将生成物登记为 `edges.json`：

```sh
PDRS_BASELINE=/Users/da/ws/pilotdeck-rs/resources/PilotDeck \
node --import /Users/da/ws/pilotdeck-rs/resources/PilotDeck/node_modules/tsx/dist/loader.mjs \
src/pilotdeck-cli/tests/fixtures/chat-search/generate.mts /tmp/pdrs-chat-edges-1 edges
```
