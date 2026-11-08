# SDK deferred search oracle

直接调用固定 TS cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85 的 createDeferredToolSearchTool 和 ToolRegistry，Node22.22.0/ICU78.2；采前/后均核实际源字节。六查询涵盖整句/词项匹配、无结果、空查询、孤立UTF16及实际 registry reveal。

原件主仓 `_tmp/20261108-cli-completion/sdk-deferred-oracle/`；oracle SHA256 `9f06ffd2da3bb590f5adeda656588f05896370f3b3d12003afb293091bef9f1b`，capture.mts SHA256 `af29a3bc77ab963715ab561622ba266dd193c5b5bb3cb28f54d22a3b2806ffd3`。Rust测试实际安装搜索工具并执行，检查返回与同一catalog的显露结果；不是外部provider实测。
