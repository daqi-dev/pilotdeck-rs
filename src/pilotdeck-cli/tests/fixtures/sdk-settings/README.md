# #1029 settings CLI fixture

`source.json` 是便携原始证据：157 个 D1 OBSERVED 格及 34 个 Node v22.22.0 真实 CLI 补测格。`generate.mjs` 仅从入库 source 重建 oracle，不读仓外规划目录，不启动 TS；唯一归一化是登记的 Node SQLite 启动告警及请求 UUID 绑定。`probe.mjs` 用 `PDRS_BASELINE` 指向固定 TS 基线，只启动隔离 HOME/PILOT_HOME 和本地 mock，不启动真实 gateway、不写用户设置。

191 格包含两个 unknown signal 配置格，明确保留 `TRIGGER_NOT_REACHED` / `applicability`：本地 usage 返回无 RPC 信号触发点。它们验证本地退出，不能计作信号送达成功。其余信号格来自 D0 请求屏障原始记录。160 层嵌套输入、孤立 UTF-16 surrogate 和 JS 数字归一化均有原始 wire。

生成：`node generate.mjs NEW_DIRECTORY`（Node v22.22.0，独占写 oracle.json）。补测：`PDRS_BASELINE=<baseline> node probe.mjs NEW_DIRECTORY [cell-id ...]`。

测试 `settings_matches_ts_matrix` 核完整 hello / request / response、stdout / stderr / exit / signal、隔离文件树不变；`settings_local_preflight_matches_ts_matrix` 单独核所有本地退出格。ignored `settings_oracle_regenerates_identically` 实跑重生成并比原字节。

负控位于 gateway tests/fixtures/sdk-settings/negative_controls.py，复用 negative_process 的源码 HEAD 守卫及 negative_suite 的 nonce 回执。新文件未提交时不得原地变异；由协调者提交后在同一 CARGO_TARGET_DIR 执行。P02 transport 和有界对拍租约按已有语义复用，真实设置持久化待 P09。
