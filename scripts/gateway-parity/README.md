# Gateway parity ruler（A2 / Issue #23）

在仓根运行。Python 只用标准库；Node 22 + 基线自带 tsx。基线目录默认 `/Users/da/ws/pilotdeck-rs/resources/PilotDeck`，可用 `PDRS_BASELINE` 或 `--baseline` 指定，必须已构建且只读。Rust bin 来自本 workspace 的 `src/target/debug/pilotdeck-gateway`。

```sh
CARGO_HOME="$PWD/_build/cargo-home" cargo build --manifest-path src/Cargo.toml -p pilotdeck-gateway --locked
python3 scripts/gateway-parity/check_ruler.py
# 机器项全过时 exit 2：independent_review=null，待协调者审查。
python3 scripts/gateway-parity/check_frozen.py
# 无 FROZEN.md 时 exit 2：草稿没有冒充已冻结。
python3 scripts/gateway-parity/test_safety.py
```

默认产物 `_tmp/20261014-exp006/`。重新采集请选择新的 `--output _tmp/YYYYMMDD-名字`；脚本不覆盖既有 cur 或 label。`check_ruler` 复用已有六轮原件，重新比 10 对 TS、代理，重新生成 controls 目录；原件损坏就 FAIL，不自动重跑盖掉失败。每轮约 20 秒，整轮有 180 秒超时；独立 driver 上限 45 秒。等待共享锁默认 600 秒超时，可用 `--lock-timeout` 调整。全量 k=5 + 代理轮整体持有 `/tmp/pdrs-parity.lock`（owner.json、guard、陈旧接管、owner token 校验释放）。

手动两轮也必须在**同一个 output**下运行，cur 路径保持一致：

```sh
python3 scripts/gateway-parity/run_round.py --side ts --label direct --output _tmp/20261014-gateway-manual
python3 scripts/gateway-parity/run_round.py --side rust --label proxy --output _tmp/20261014-gateway-manual
python3 scripts/gateway-parity/compare.py _tmp/20261014-gateway-manual/direct _tmp/20261014-gateway-manual/proxy --out _tmp/20261014-gateway-manual/comparison.json
python3 scripts/gateway-parity/controls/run_all.py --source _tmp/20261014-gateway-manual/direct --output _tmp/20261014-gateway-manual/controls
```

端口可用 `run_round --ports TS PROXY OBSERVER MOCK` 参数化，四个必须互异且都是 28xxx；缺省 28791/28792/28793/28794，全部只监听 127.0.0.1。预检占用立即拒绝，不按端口查杀。各进程 start_new_session，清理只向本次 Popen 的组发信号，父进程早退也清理其剩余后代；processes.json/audit 保存 PID、argv、实际监听地址和结束状态。

拓扑：直连 driver→TS；代理 driver→Rust→装置 observer→TS。mock 模型独立监听 28794。HTTP 流式探针是 `/__ruler/stream/{sse,ndjson}` 明确测试路由，由 watcher fixture 转发到 mock；它不宣称是生产 API。浅层 polling 替代仅因 macOS 沙箱 fs.watch EMFILE，对两侧一致开启；不覆盖递归热加载。

`driver.mts --url --token --scenario --out` 从基线公开 `src/gateway/index.js` 导入官方客户端/RemoteGateway/协议常量，tsx 将其解析到同目录 index.ts。完整应用帧在 driver，Rust 不能伪造比较证据。observer 独立记录原始上游 payload 与方向，A4 可用 `compare.py --local-method supported_agents` 要求成功响应一致、请求未转发；A2 默认断言十个只读请求**全部**透传，避免空 observer 被误认作本地化。

规约见 `specs/gateway-parity/`；没有已接受差异。自动 UI 恢复、桌面、生产 DOM、全部 101 method、config/扩展热加载与真正 TS 退役均不在 A2 完成声明中。
