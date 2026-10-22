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

修复轮 4 将 edges 扩至 154 场景，原 77 场景的 argv/stdout/stderr/exit 与原夹具内容逐条保持一致。新增 51 条解码场景覆盖 head/tail、metadata/普通行、title/createdAt、非法 `\x`/`\q`/残缺 `\u12`、原始 TAB/NUL，以及带非 ASCII/非 BMP 前缀时的 UTF-16 错误位置；同时钉住无 summary 不解码、慢路径恢复 summary 后解码及字段求值顺序。损坏文件放在独立 `error-pilot`，仅相应场景通过相对 `--pilot-home` 访问，避免改变已有 all-projects 场景输入。

另加 26 条真实 TS 正则场景，分别以 flags 空串和 `i` 覆盖词边界/行锚点/lookbehind 后量词的错误文本、可量化 lookahead 和断言外层普通组，以及倒序量词。Node 22 的 lookbehind 报 `Invalid quantifier`，词边界和行锚点报 `Nothing to repeat`。生成与复算方式仍用上面的 `edges` 命令；期望全部取自真实 TS，原始流保存在每次输出目录的 `raw/` 中，退出码保存在生成的 `oracle.json` 各场景 `exit` 字段。


修复轮 5 将 edges 扩至 176 场景，原 154 条场景和夹具内容保持不变。新增 22 条均由同一真实 TS 生成器采集：两种大小写模式的 65,000 原子 `[\S…]` 长字符类、两种模式的 65,001 码元重复前缀 literal，以及 18 条正/补集转义、负类、忽略大小写和跨代理项区间用例。资源夹具放独立 `bounded-pilot`，不改变已有 all-projects 输入。`chat_search_large_character_classes_are_bounded` 和 `chat_search_large_literal_prefixes_are_bounded` 各对两条真实 TS 金样逐字节回放，单子进程从启动到匹配及输出须 < 1 s，超时主动 kill + wait；stdout/stderr 并发排空以避免长 query 填满管道。类别选择不得为空，两个测试的临时目录各自隔离。

额外的 `giant-quantifier` 生成模式只复算 D2-CHATSEARCH-REGEX-GIANTQUANT-001（`specs/gateway-parity/accepted-diffs.md`）：把上面命令末尾的 `edges` 替换为 `giant-quantifier` 即可。它保存真实 TS 输出，独立于通过的 39 原场景与 176 条 edges；当前 Rust 巨量词编译分歧不计通过，也没有获得豁免。
