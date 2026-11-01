# #1140 见证控制

采集器、证据合同、Linux 命令与证明边界的唯一说明见 [scripts/native-witness/README.md](../../../../../scripts/native-witness/README.md)。

`test_witness.py` 的默认测试为合成 trace 与生命周期控制；`--capture` 才读取 Linux 原始采集结果。非 witness 的审计外壳明确标合成，不能作为完整 native 格证据。

`negative.py` 与 `negative_authentication.py` 的 `cases` 列表及执行汇总是负控数量的唯一来源；每项绑定专属单行断言。`--case NAME` 选择样本，`--check-anchors` 只读核锚并编译每个 Python 变异。原地模式要求涉及源码、测试及脚本已提交且等于 HEAD，使用共享源码还原/进程守卫。认证套件另有明确的 `--memory` 候选演练模式，只在子进程内编译并注入源码，不改文件、不代替提交后的正式验证。

`test_authentication.py` 验证 parser、WS profile、原二进制前提及清理所有权；`test_chain.py` 验证正式边界、逐 case 原件归属、独立 profile 和冻结门控。标有 SYNTHETIC_CONTROL_ONLY 的正控只能走测试辅助路径，正式 loader 必须拒收。

`cb8-sendto-interleaved.trace` 是 cb8 第二次采集的原字节片段（exit/sendto 交错，5 行）；唯一字符串为 hex 编码的 mock WS 帧，解码文本为 `native witness text`、模型为 `mock`。没有 HTTP 请求、提示词、真实密钥或隔离路径。测试将它接入合成进程前后缀，只验证解析，不当作完整采集/TS 金样。
