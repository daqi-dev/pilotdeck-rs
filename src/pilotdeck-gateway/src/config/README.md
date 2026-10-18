# 全局/项目配置加载与只读模型投影

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
没有增加配置写入、热加载或版本/hash API。

公共 `load_project(pilot_home, cwd, env, project)` 接收显式路径与环境，供 CLI
复用同一加载器；不会从进程环境推导 PilotHome，也不会启动宿主回退。
`Error.requires_host` 由 CLI 映射为未实现分支，StaffDeck 绑定也保留 CLI 的 Todo。
内部 `load_project_with_files(metadata, env, Some(projectRoot))` 为 `session_model_get` 补齐
项目路径：读取 `<projectRoot>/.pilotdeck/pilotdeck.yaml`，以默认→项目→env
顺序合并，再执行 `binding.rs` 的只读 `applyProjectBinding` 转换。
有效 binding 注入知识模块和 SOP runtime；pending/failed、连接 identity 不符、
未就绪或死锁禁用两项模块。SOP bundle 使用 yaml 默认 pretty error 语义；
保存的安全正整数 timeout 覆盖 binding 默认值，management 字段的 warning 也保留。
项目/全局来源诊断保留各自 kind、path 和错误优先级。

存活探测使用 Cargo.lock 已有的 rustix process 安全接口（macOS 后端为 libc）：
JS Number 字符串转换→安全正整数→Node 的有符号 int32 PID 边界→kill(pid,0)。
读取失败或任何 syscall 错误（含 EPERM）返回 false。没有新 crate 或锁文件变更。
`load(metadata, env)` 仍传 None；model_catalog_list 不读项目 YAML/binding store，
既有 catalog/diagnostics 全量 fixtures 保持通过。

`catalog-data.json`、`contracts.json`、`timezones.json` 是离线运行基线导出器
得到的固定数据；记录脚本为 `tests/record_config_data.mts`。timezone 名称来自
该 Node/ICU 接受的标准名称与系统 zoneinfo 别名，另按基线接受 UTC offset。
URL 检查使用固定版本 `url` crate；现有 Cargo.lock 包版本没有升级。

本目录消费者为本地 `model_catalog_list` 与 `session_model_get`。加载先于 query 类型处理；provider
采用 truthiness + 严格相等，includeAuto 采用严格 true，没有 filter/order 参数。
catalog 排序使用 D6 A 的 ICU4X Unihan baked 数据，大小写沿用 ICU4X compiled data；
wire 使用 protocol codec。数据来源、固定版本和再生成方法见 `src/PROVENANCE.md`
（仓根），排序证据见 `tests/fixtures/collation/README.md`（gateway crate 根）。
`model_catalog_list`、`project_files_list` 与 `session_model_get` 的合法Unicode排序继续本地应答；仅实际参与多项排序的孤立UTF16 surrogate由JS_HOST_REQUIRED转发，单项及过滤后单项不触发。没有批准排序差异豁免。`project_file_read` 继续本地安全读取。
涉及宿主共享 intrinsic 的配置转发 upstream，内部 `JS_HOST_REQUIRED` 不出现在 wire。
HOME 不可解析时同样转发，避免猜测系统账户 home。

对拍记录与复现方法见 `tests/fixtures/config/README.md`。
