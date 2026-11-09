# SDK model / project 验证与退出

原件来自固定 TS `cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`，在 mbp2、Node v22.22.0 / ICU 78.2、动态 loopback fake WS 和隔离 home 上运行真实 CLI。`oracle.json` 按 run/resume/空 prompt/缺 resume ID 顺序原样汇集四个 raw.json，保留请求、stderr、4.5 秒观察结果和逐 PID 清理证据；随机请求 ID 与 PID 不参与 Rust 判等。

`generate.mjs <TS源码目录> <不存在的证据目录>` 重新采集四个子目录。汇集时不改原字段；原始首次证据在主仓 `_tmp/20261108-cli-sdk-gaps/ts-mbp2-01/`。生成器只连接本地 mock、使用虚构 token，不读取真实 PilotDeck home。

Rust 测试复放原始 argv/请求/输出，额外要求本地 model/project 错误主动关闭 query/control 并 exit 1；这部分与 TS 挂起的差异单独登记为 `D1-SDK-MODEL-PROJECT-EXIT-001`，尚未人批。远端同文错误仍按原连接生命周期处理；空串 project 与空白 project 分别验证缺省和实际 catalog 查询，不扩大改动范围。现有冻结 SDK 金样与装置均未修改。

哈希：
- `generate.mjs`: `ef365a20c9bb86717477e12b70acef05d8371918b194da5b2570ac865ef492cb`
- `oracle.json`: `40a0b122cfe69f21cba58ccfeef4d7ab73ff23a36927c8cce9bfc0ee59be5203`
