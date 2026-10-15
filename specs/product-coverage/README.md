# 产品入口覆盖总账

这是迁移范围与任务归属的门，不是新的行为判分尺子。唯一账本 [entries.json](entries.json) 逐入口登记状态、基线源码证据及任务组；组内引用阶段、具体 issue、计划与验收方式，作为规范化关系连接，避免同一判据重复抄写。没有任何状态表示整阶段或真人使用已通过。

独立枚举器 [enumerate.mjs](../../scripts/product-coverage/enumerate.mjs) 不读取账本，从固定 Git 对象 cc7c4e6 解析两个 bin、argv 分派、TUI、选项、101 gateway method、HTTP/后台 channel、UI 路由/中间件/WS消息/流、桌面 IPC/命令/编排/本地能力。它不 import 或启动基线产品，不读取用户运行目录。所有读取源码记 SHA-256；动态挂载递归、链式 route、USE/静态回退及 wrapper IPC 均单列。固定基线之外的升级须单独立项，未知解析形状/缺基线/缺依赖必须失败，不提供“缺基线跳过”。

运行：先 `npm ci --ignore-scripts --no-fund --prefix scripts/gateway-parity` 安装已有 lockfile 的 TypeScript 依赖，再执行 `python3 scripts/product-coverage/check.py --baseline resources/PilotDeck`。CI 复用 gateway-schema-baseline job 已克隆的固定基线，执行相同命令和负对照。没有用户配置或已忽略本机证据的依赖。

`ui.http` 的263项与既有 B1 runtime 枚举对账（尾斜杠及 _ALL 归一化）；它不包含的 USE、WebSocket、SSE/NDJSON、后台与静态面额外列项。中间件/流的 source:line 身份用于同一固定基线中的准确定位，不代表额外功能计分。IPC 以通道为单位聚合收发双方来源，DesktopCommand 另逐个登记。后台 channel 包含15个 registry键及4个原生适配器，另记自带HTTP监听；每种channel的外部行为、鉴权、启动/重载/排空仍由A8装置验证。

状态：`placeholder` 仅版本骨架；`partial-native` 有局部Rust接管但仍有输入回退/前置缺口；`ts-forwarded` 经Rust代理仍由TS处理；`ts-only` 仍由TS承担；`rust-unaccepted` Rust已实现但阶段验收未闭合；`unsupported` Rust明确不支持；`ts-orchestration-pending` 桌面启动/生命周期仍待接新后端；`retained-shell-unverified` 按既定方案保留Electron/本地能力，但最终组合尚待验。Rust新增的管理参数不是基线入口，须由所属实现记录，不计作已替换命令。

检查要求基线集合与账本完全相等，拒绝重复、漏项、多项、空/悬空归属、无状态/证据、源码指针或哈希漂移。`test_check.py` 从真实独立枚举做正对照，并对每个入口类别删项，以及重复、删除bin/默认路径/setup别名/sidecar profile、空归属、坏状态/证据/源码/哈希逐项注错。覆盖通过仅回答“范围列齐且有人负责”；具体行为、TS退役和真人验收各自留证据。
