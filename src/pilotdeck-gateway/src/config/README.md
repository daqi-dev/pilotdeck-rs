# 全局配置加载与 catalog 投影

本目录独立实现 `listModelCatalog(input, env)` 所调用的
`loadPilotConfig({ env })` 路径。基线为 PilotDeck
`cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`，Node 22.22.0 / ICU 78.2。
这里不传 `projectRoot`：该 RPC 不读取项目配置、项目绑定或 session override。

`mod.rs` 按基线顺序读取全局 YAML、合并 `PILOT_AGENT_MODEL`、执行
StaffDeck 开关转换，再校验所有配置节。YAML 使用
`pilotdeck_yaml::parse_document`，关闭 pretty errors，保留 toJSON 图语义。
首次 fatal 检查、立即抛出的配置错误、末尾累计 fatal 检查各自保持优先级。
provider 解析失败产生 warning 并排除该项；agent 引用失效项仍然 fatal。
credential env 保持各 provider 的解析与可用性判断。

职责分布：

- `model.rs`：provider / model / credential / capabilities 与引用解析。
- `sections.rs`：agent、extension、memory、gateway、adapters、alwaysOn、
  cron、tools、runPolicy、proxy。telemetry 在基线中没有诊断校验。
- `router.rs`：router 所有分支与引用、冲突 warning。
- `modules.rs`：内置与外部模块、contract/method、deployment、catalog、SOP。
- `graph.rs`：按校验器实际访问范围检查 alias 图；忽略的循环不提前报错，
  保留在最终快照中的循环按基线产生 RangeError。
- `value.rs`：JS truthiness、String、Object.entries、UTF-16 诊断文本。
- `host.rs`：引用 JavaScript intrinsic prototype 状态的特殊配置交回 TS。

返回类型是 catalog 所需投影与完整诊断，不是可供执行器使用的完整配置快照。
没有增加配置写入、热加载、版本/hash 或项目配置 API。

`catalog-data.json`、`contracts.json`、`timezones.json` 是离线运行基线导出器
得到的固定数据；记录脚本为 `tests/record_config_data.mts`。timezone 名称来自
该 Node/ICU 接受的标准名称与系统 zoneinfo 别名，另按基线接受 UTC offset。
URL 检查使用固定版本 `url` crate；现有 Cargo.lock 包版本没有升级。

本地 dispatch 只拦截 `model_catalog_list`。加载先于 query 类型处理；provider
采用 truthiness + 严格相等，includeAuto 采用严格 true，没有 filter/order 参数。
catalog 排序和大小写使用既有系统 ICU 入口；wire 使用 protocol codec。
涉及宿主共享 intrinsic 的配置转发 upstream，内部 `JS_HOST_REQUIRED` 不出现在 wire。
HOME 不可解析时同样转发，避免猜测系统账户 home。

对拍记录与复现方法见 `tests/fixtures/config/README.md`。
