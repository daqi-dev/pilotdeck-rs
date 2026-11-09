---
id: D2-NATIVE-SDK-SETTING-SOURCE-READ-BOUNDS-001
---

## 类别

安全修复：native CLI host 的 SDK settings 文件来源采用已有同 fd 有界配置读器，避免 FIFO 等非普通文件挂起及单文件无界读取。独立于旧 CLI `settings set` 的嵌套深度差异，也不把此安全边界当作冻结评分豁免。

## 基线依据

固定 `cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85` 的 `src/pilot/config/sdkSessionSettings.ts:154–178` 在 existsSync 后调用 readFileSync(path, "utf8") 并解析 YAML，没有本片普通文件检查或字节上限。来源优先级与路径由同文件 resolvePilotSdkSessionSettings / sourcePath 决定：managed 为 host 内存值，user/project/local 为三个文件来源。这里依据基线源码陈述，不冒称已对这个 SDK resolver 另跑 TS FIFO 挂起实验；CLI loader 的其他资源探针不能代替本入口实采。

## Rust 行为

仅 native SdkSessionOptions 安装可选 SdkSettingSourceReader；generic SDK registry 未装 reader 时保持旧解析/合并路径。NativeSettingSourceReader 复用 config::read_setting_source_bounded：Unix O_NONBLOCK 打开、检查同 fd 为普通文件、最多读取 8388608 字节并用 limit+1 检测超限/增长。非普通文件或超限在这个 reader 被调用时返回 SDK_SETTING_SOURCE_RESOURCE_LIMIT，普通读取错误仍沿 SDK_SETTING_SOURCE_READ_FAILED 及原 Node 文案 renderer；目录保留 EISDIR，不改成资源拒绝。symlink 沿既有读器跟随并检查打开的目标 fd。

解析任务采用四许可 blocking worker，permit随实际 worker 持有，不随被取消的 async waiter 提前释放。既有 SDK 来源去重/顺序不变，单次解析至多三个文件、各8MiB；不声称为三文件新增共享32MiB预算，不把解析后对象/全部内存成本算在字节上限内。managed 来源不读文件。Native host显式选择其 options.home 为 PILOT_HOME，避免脱离该host的隔离配置目录。

## 范围

只限采用该 opt-in reader 的 native CLI/server SDK settingSources 读取；主配置/Metadata可能先由其各自既有有界入口拒绝，此条不改它们的错误优先级。普通文件内容、UTF-8替换、YAML解析、来源覆盖/host enforced数学、运行期短model继承规则不变。不改变旧 generic SDK API/NativeFiles/旧 Gateway loader，也不声称支持或实测Windows。资源限制不保证打断故障文件系统的内核IO、也不提供跨来源事务快照。

## 机器判据

`native_process::assembly::sdk_settings::tests::setting_sources_bound_real_fifo_sparse_and_directory_without_changing_normal_files` 父进程实际精确调用3秒有界子测试：普通有效文件解析42并保持原bytes；无writer FIFO与8388609字节稀疏文件显式资源拒绝，普通目录为精确EISDIR旧错误，输入保全且不访问注入的无关homedir。mbp2 r122 `verify-r122/logs/native.log` 已实际记录该父测试通过；同轮 SDK新增57格中央intake、真实source模型/fallback消费、SDK owner与style替换回归通过。

SDK真实入口串行联立证据在私有 `native-sdk-settings-probe-child-20261109-01/run-r124`：29项实际通过、33/96 loopback请求，source model/limit→overlay→实际fallback、同session插件style与reset、额外根、临时JSONL/child sidechain与清理。该旧source串行证据不证明之后的delete竞态修复；最终固定source复验由协调者另记。不改变任何冻结oracle/阈值/分母/原负控判分。

## 批准记录

**AI 代批，待人复核；未批准，非已接受差异，非豁免。** 沿用户已授权完整CLI和默认资源安全修复范围执行，协调者明确要求本新增非冻结条目记录；只本机提交，不推GitHub。不替代人签字、不提前标阶段完成、不改FROZEN或原accepted-diffs字节。
