# Node locale contract（#887）

`oracle.json` 是 Node v22.22.0 自身 `Intl.Collator` 与 `String.localeCompare` 的原始运行时结果，不导入或复制 TS 产品算法。用于验证产品沿用的 Node 默认 locale 语义；运行时版本不符即拒绝生成。每格独立子进程移除继承的 LANG / LC_* 后注入记录中的 env，记录 resolved locale 和排序。

覆盖无 locale、C / POSIX / C.UTF-8、无效语言及尾部、POSIX modifier、空值、连字符 locale、LC_* 优先级。空字符串在此 Node 版本解析为 und，不能按 POSIX 常识过滤为空值。Linux 上的连字符行为未在本机实测；重生成守卫能报告平台差异，不宣称跨平台实效已通过。

生成：`node tests/fixtures/node-locale/generate.mts /absolute/new-output`（在 gateway crate 下）；目录 recursive 创建，oracle 用 wx 独占创建。

Rust 正控：`project_files::tests::node_locale_contract` 同表对照生产 parser 和 collator；`project_files::tests::node_locale_regenerates_identically` 为 ignored 重生成守卫（使用已有 common/baseline helper，需要 PDRS_BASELINE/tsx）。catalog 与 plugin-runtime 的 locale contract 共用这份表；两套回放在非 en-US 环境会主动拒绝。

负控复用 `../commands-catalog/negative_controls.py`；新增 parser、两回放守卫和生成器环境变异均绑定完整测试名及专属失败消息。脚本逐变异编译、执行、恢复正控并汇总；未提交源码会被 HEAD 守卫拒绝，提交后由协调者在宿主跑。
