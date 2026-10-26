# #763 fix-r1：头部校验与底层截止

`generate.mts` 导入固定 `cc7c4e6` 的真实 `LlmMemoryExtractor`，使用 **Node v22.22.0 自带 undici** 的 Agent 和内存 Duplex socket。没有 provider 请求、监听端口或复制产品函数体；`sourceClosureSha256` 核验原始入口与 CLI proxy 闭包。81 格记录请求次数、连接次数、错误 cause、退避、debug、编码以及定时器清理。

- 20 格 headers：chat / responses，300 秒默认与 600 秒 CLI 配置，禁用 / 较早 / 同刻 / 较晚的配置截止。连接永不响应，由真正 undici parser 的 headers timer 结束。100 个 fast ticks 后仍 pending；最终 `UND_ERR_HEADERS_TIMEOUT` 与重试次数来自实测。
- 24 格 body：两种 API、200/503 状态、两种 idle 时长及三种配置截止。TS 在头返回后清除配置 timer，最终 `TypeError: terminated`。Rust 的较早/同刻配置截止仍按已登记 D-2 执行；较晚配置不得掩盖 idle。
- 24 格 Headers / dispatch：0x01、DEL、NUL、CR/LF、trim、大小写合并、强制 sec-fetch-mode、ByteString 转换顺序及默认 API key 的 set 路径。
- 9 格编码：大小写 HTTPS、HTTP、Range、空 Range、显式编码与 Range 合并、HTTP→HTTPS 重定向、Range 重定向不重复追加。
- 4 格生命周期：分块重置 idle、headers/body 取消、分块后取消。取消是 TS AbortSignal 对照，Rust caller drop 不宣称为同一个 API。`firstSocketDestroyed` 在 Agent 清理前观察；某些取消会使 undici 预建空闲连接，`socketsDestroyed=false` 如实保留。finally 销毁 Agent 后断言全部 socket 已销毁。

fast timer 使用真正 `onTick` 回调和 `refresh`，每 tick 499 ms。它不是精确墙钟计时器；本夹具验证边界前 pending、边界后结果与先后顺序，不声称恰好 300000 / 600000 墙钟毫秒触发。每格最多 12000 个 scheduler 回合，另有 10 秒真实 watchdog；取消停在首个退避前，不启动后继请求。

生成器必须使用新的输出目录，`mkdir({recursive:true})` 与文件 `flag:'wx'` 防覆盖。Rust `regenerates_fix_r1_identically` 是 ignored 字节重生成守卫：

```sh
export PDRS_BASELINE=/Users/da/ws/pilotdeck-rs/resources/PilotDeck
export CARGO_TARGET_DIR=/Users/da/ws/pilotdeck-rs/_build/wt-763/_build/cargo-target
export CARGO_PROFILE_DEV_DEBUG=line-tables-only CARGO_PROFILE_TEST_DEBUG=line-tables-only
scripts/cargo-slots.sh
cd src
cargo test -p pilotdeck-gateway --test edgeclaw_llm_transport regenerates_fix_r1_identically -- --ignored --exact
```

本轮负控集中维护于 `../edgeclaw-llm/negative.py`，完整测试名、专属断言消息与每个源文件均绑定；不得以无关 panic、编译失败或 timeout 判杀。源码修改尚未提交时 `mutating_sources` 必须拒绝，提交后由协调者宿主全跑，沿用调用方 `CARGO_TARGET_DIR`。
