# #1029 settings CLI fixture

`source.json` 是便携原始证据：157 个 D1 OBSERVED 格及 34 个 Node v22.22.0 真实 CLI 补测格。`generate.mjs` 仅从入库 source 重建 oracle，不读仓外规划目录，不启动 TS；唯一归一化是登记的 Node SQLite 启动告警及请求 UUID 绑定。`probe.mjs` 用 `PDRS_BASELINE` 指向固定 TS 基线，只启动隔离 HOME/PILOT_HOME 和本地 mock，不启动真实 gateway、不写用户设置。

191 格包含两个 unknown signal 配置格，明确保留 `TRIGGER_NOT_REACHED` / `applicability`：本地 usage 返回无 RPC 信号触发点。它们验证本地退出，不能计作信号送达成功。其余信号格来自 D0 请求屏障原始记录。160 层嵌套输入、孤立 UTF-16 surrogate 和 JS 数字归一化均有原始 wire。

生成：`node generate.mjs NEW_DIRECTORY`（Node v22.22.0，独占写 oracle.json）。补测：`PDRS_BASELINE=<baseline> node probe.mjs NEW_DIRECTORY [cell-id ...]`。

测试 `settings_matches_ts_matrix` 对发送 hello / request 按 TS 原始字节比较（只替换请求 UUID），回放 response，核对 stdout / stderr / exit / signal、隔离文件树不变；`settings_local_preflight_matches_ts_matrix` 单独核所有本地退出格。ignored `settings_oracle_regenerates_identically` 实跑重生成并比原字节。

负控位于 gateway tests/fixtures/sdk-settings/negative_controls.py，复用 negative_process 的源码 HEAD 守卫及 negative_suite 的 nonce 回执。新文件未提交时不得原地变异；由协调者提交后在同一 CARGO_TARGET_DIR 执行。P02 transport 和有界对拍租约按已有语义复用，真实设置持久化待 P09。

fix-r1 新增 settings 专属 hello 键序、request 键序、等义 JSON 转义三个编译期源码变异，分别绑定 `settings_hello_matches_ts_bytes` / `settings_request_matches_ts_bytes`；与既有 28 项合计 31 项。sessions 继续使用 P02 的旧排序字节及 `legacy-request-order` 守卫。

settings JSON 只从 argv（`--json` 或位置参数）读取，不从 stdin 读取；发送方向保持 TS SDK 的无额外应用层长度封顶，不以接收上限推导发送上限。fix-r1 Node v22.22.0 实测 TS SDK 成功发送 50 MiB+1 设置对象；向 CLI stdin 写入相同大小内容时两侧均忽略，是否成功由 argv 决定。10,000/100,000 层病态嵌套存在已观测但未收敛差异（TS 在 hello 后 stringify 栈溢出，Rust 成功）；191 格通过不覆盖或批准这项差异，处理理由与原始证据见 `_tmp/20261026-issue-1029/fix-r1/RESULT.md`。
