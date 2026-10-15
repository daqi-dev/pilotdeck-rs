# 阶段 D：完整 CLI 入口与对拍

状态：实施计划；入口账本通过仅代表范围和归属完整，不代表行为或真人验收通过。依据：[CLI 人定](../../state/decisions-20261014.md:225)、[整体意向](../../.42cog/intent.md)。固定参照为 OpenBMB/PilotDeck@cc7c4e6。

## 范围与真相源

命令及 profile 的唯一明细是 [产品入口总账](../../specs/product-coverage/entries.json)，由独立枚举器从固定 Git 对象读源码验证。包括 package.json 的两个 bin、手写 argv 分派、默认/未知参数路径、叶命令及别名、TUI slash 子树、实际读取的选项和 sidecar 部署 profile。help 文本不完整，不能用它或候选 parser 作为完整标准。行为以同参数、同输入的基线实测为准；已批准差异必须有明确期望及登记，首次装置定标不得根据 Rust 结果放宽规则。

`pilotdeck` 当前 main 入口仍只输出版本。候选 `codex/cli-server@1fdb99b` 只提供过渡代理 server/help/version；空参数输出版本、未知命令报错及严格参数 parser 不符合原版默认分流，不能整包合入。D1/D2 按基线重新实现，候选可复用部分须逐项复核。`pilotdeck server` 同时绑定 A 的 gateway/后台与 B 的 UI 编排，D 负责命令参数、输出、退出及组合验收。sidecar 协议属于 C；Rust 不支持 factory/TCP 已另建 #130，现有 stdio 证据不能覆盖它们。

## 任务与依赖

| 任务 | 入口范围与交付 | 依赖与完成门 |
|---|---|---|
| **D0 / #122** | 双端进程驱动、隔离沙箱、四面比较器、provenance、mock，TS 自比和注错负对照 | 可先行；首批只定标不宣称命令迁移。独立 issue 记录场景、归一化和准确预期 |
| **D1 / #123** | SDK help/别名，run/resume，sessions 默认/list/messages，settings 默认/get/set；stdin、JSON/JSONL、信号取消、鉴权来源 | D0；依赖独立 Rust gateway 的真实联调/完成在 A8 后，可与 B 并行 |
| **D2 / #124** | 默认 stdin 逐行、未知 argv 拼 prompt；chat search；cron list/create/delete/stop；skills migrate；gateway setup 分流及所有平台别名；server 外壳 | D0/D1；本地文件与 mock 可先行，依赖 gateway 的完成在 A8 后；server UI 编排还需 B6 |
| **D3 / #125** | setup 交互菜单、凭据与配置；update check/apply/restart，网络/安装/文件切换/重启 mock | D0；按人定在 SDK 与本地命令之后推进。真实更新服务契约联调依 B 对应路由，禁止实际更新用户安装 |
| **D4 / #126** | 独立 PTY 装置与 TUI：slash 命令及权限子树、模式、键盘、流式、pager、中文、resize、信号、终端恢复 | PTY 装置可先行；真实 gateway 联调 A8 后；真人 CLI/TUI 实际任务单列待验 |

跨阶段总账责任：A6–A8 #127 管全部 gateway method、HTTP 和后台 channel；B3–B6 #128 管 UI HTTP/WS/SSE、共享资源及桌面后端启动替换；F3–F6 #129 管业务页面、保留桌面 IPC/本地能力验收及旧前端/双壳退役。既有阶段计划和批准依赖继续有效，不以新 issue 取消原前置。

## D0 验收装置要求

每场景固定 argv、stdin 字节、cwd、环境和初始文件树。分别启动 TS/Rust，在同一逻辑路径、独立进程和恢复过的沙箱运行；同时隔离 HOME、PILOT_HOME、OS homedir、配置、端口、凭据及子进程环境。仅改 PILOT_HOME 不足以隔离 setup（原版 Weixin 仍写 homedir 下目录）。不加载用户默认配置，不接入运行中的 PilotDeck。

比较 stdout、stderr 原始字节/顺序、退出码及完整落盘差异；批准的随机 ID/时间/路径归一化须先登记，错误与警告也验。无参数/未知命令、空输入、非 TTY、未知叶、flag 缺值/重复/组合、非法 JSON、token 来源优先级、连接失败与信号顺序都要有明确结果。保留 TS 的错误检查顺序和外层退出码映射，发现基线 bug 先按既有判据实测和登记。

记录实际可执行文件、源码 HEAD、SHA-256、argv/env 的去密摘要、进程树和资源释放证据。setup 的第三方登录/轮询/配置合并，update 的 HTTP/Git/包管理器/构建/产物切换/supervisor 均使用可注错 mock；不下载预编译二进制。验证失败、取消、超时、旧凭据保留及无越界写。TUI 使用真实 PTY 加假 pager，普通 pipe 输出不能替代交互验收。

装置须先做 TS 自比并说明稳定/不稳定面，再做丢 stdout、改 stderr/退出码、漏落盘、丢信号/终端恢复、漏命令或默认路径等负对照。CLI 验收装置单独建立，不修改 C/A/B 的冻结文件或评分语义。本计划没有冻结新的阈值，也没有提前接受任何不兼容。

## 收尾与真实使用

各 issue 关闭前逐项对齐入口与验收场景，扫描占位/转发/只打印版本入口，清到点 P2；提供源码行号、四面比较原件和独立审阅。gateway 依赖命令必须有 Rust 实际处理、无业务 TS 回退证据。所有命令完成、旧 CLI 退役、切换/重启/回滚通过，并由人在最终组合上完成 CLI/TUI 实际任务后，阶段 D 才完成。AI 准备隔离候选和步骤，未真人使用的项目始终记「待人工验收」。
