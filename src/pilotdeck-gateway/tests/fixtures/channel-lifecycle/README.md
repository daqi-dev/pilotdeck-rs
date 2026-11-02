# #1327 pr1 channel 生命周期金样

`generate.mts` 在 Node **v22.22.0 / ICU 78.2** 上读取固定基线 **cc7c4e6** 的完整 `src/cli/pilotdeckServer.ts`，原样转译执行。共享基线守卫核提交与源闭包哈希。仅替换 host server 与状态报告边界；不会启动真实服务器、账号、网络或用户目录。

16 个场景的输入步骤与 TS 结果都在 `oracle.json`。Rust 测试按输入重放，比较完整结果：每个实例的事件序列、每个 key 的状态序列、操作返回/错误、确定性闸门处的快照、日志与宿主 close 次数。不同 key 间无因果关系的微任务排列不作为协议；跨 key 停旧再启新的顺序由 stop gate 前快照固定。未完成的 Promise 用可释放 gate，单场景 5 秒上限，完成后清 timer。

覆盖启用/停用/重载、三个动作下在飞消息 drain 与拒新、close 后迟到启动、自身启动错误/回滚错误/停用错误、同 key 串行、失败前驱、永不完成前驱的有界观察、丢弃结果等待、managed Set 去重、desired 重复拒绝、close 错误汇总与幂等、后台启动。永不完成场景观测后释放 gate 并等待操作，探针不遗留常驻任务。mock 的 stop 在调用时关闭接纳，在飞工作全部结束后才完成；这里只证明生命周期合同，不宣称实现 A7 入站接纳器。

重生成到**新的**目录（输出用 `recursive:true` 和 `flag:'wx'`）：

```sh
export PDRS_BASELINE=/absolute/path/to/PilotDeck
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/channel-lifecycle/generate.mts /new/evidence/output
```

忽略测试 `channel_lifecycle_regenerates_identically` 必须在具备固定基线的宿主显式运行。Rust 额外的跨线程状态排序、回调重入、panic 隔离属于 Rust 执行模型检查，不冒充 TS 多线程金样。状态报告只保留本轮生命周期所需四态，磁盘状态文件与渠道特定能力留后续单。
