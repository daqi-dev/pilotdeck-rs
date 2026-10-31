# #1140 见证控制

采集器、证据合同、Linux 命令与证明边界的唯一说明见 [scripts/native-witness/README.md](../../../../../scripts/native-witness/README.md)。

`test_witness.py` 的默认测试为合成 trace 与生命周期控制；`--capture` 才读取 Linux 原始采集结果。非 witness 的审计外壳明确标合成，不能作为完整 native 格证据。

`negative.py` 是 24 个源码变异，绑定专属单行断言；`--case NAME` 只执行一个指定样本；`--check-anchors` 只读核锚并检查每个变异可编译。真正执行要求文件已提交且等于 HEAD，并使用共享源码还原/进程守卫。
