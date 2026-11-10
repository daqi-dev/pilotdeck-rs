# 原生 CLI、机器接口与源码安装

本轮范围是旧 CLI 功能与约定、原生本地宿主、前端可调用的机器能力，以及在目标机编译的源码安装。新版前端和独立 `/app/v1` 应用服务留到前端阶段。正式行为验收和人签仍沿用阶段 D 的原判据。

## 入口和数据流

`pilotdeck` 保持旧命令入口：`run`、`resume`、`sessions`、`settings`、`chat`、`cron`、`skills`、`gateway setup`、`server`、`update` 和 `tui`；普通 prompt、未知 argv 与 stdin 进入旧 CLI channel 分派。旧默认 channel 在固定的 `127.0.0.1:18789` 发现远端，在相应不可用路径启动 Rust LocalHost；它不使用 `pilotdeck-control --url` 指定的任意地址。LocalHost、独立服务器和机器入口共享 native Gateway、session、turn 与持久化逻辑，agent 循环由配套 Rust sidecar 执行。

Unix 默认 channel 的 token 文件上限为 65536 原始字节；健康检查成功后遇到 FIFO 等特殊文件或超限会明确退出，普通 IO 失败、目录或空 token 仍按原约定回退到 LocalHost；非 Unix 不计入此资源保证。

`pilotdeck-control` 为机器消费提供以下命令。机器客户端应先发现实际能力，再处理每次调用的返回结果，不能把注册名册当作所有 provider 已配置。

| 命令 | 使用方式与边界 |
|---|---|
| `schema` | 无参数输出命令、真实方法名和外层协议；提供目标参数时附加实时能力。领域参数 schema 保持开放，不宣称完整字段校验。 |
| `status`、`capabilities` | 对选定 Gateway 鉴权后读取当前状态与能力。 |
| `call` | 一次普通 RPC；输入为包含 `method`、`params` 的 JSON。流式提交使用 `stream` 或 `connect`。 |
| `stream` | 单个文本回合，检查事件序号与终态；连接关闭沿用旧 Gateway 的会话取消语义。 |
| `connect` | stdin/stdout 上的 `pilotdeck-connect/1` NDJSON JSON-RPC；一条鉴权连接承载普通调用、回合事件、权限决定和交互响应。 |
| `watch` | 通知和选定状态的周期快照；重连输出重新同步信号，不承诺全局通知重放或原子快照。持久 run 的事件序号使用独立 `run_events`。 |
| `serve` | 显式指定 home、project、监听地址、token 文件和 sidecar，启动前台原生宿主。 |
| `install` | 从明确的干净源码 checkout 构建并同代安装四个程序。 |

## 隔离启动

先准备互相独立的 home、project 和 token 文件；home 内使用自己的 `pilotdeck.yaml` 配置模型，token 文件只含连接 token。使用所安装版本的绝对路径，项目数据不要指向已有用户实例。

```sh
/absolute/bin/pilotdeck-control serve \
  --home /absolute/sandbox/home \
  --project /absolute/sandbox/project \
  --listen 127.0.0.1:0 \
  --token-file /absolute/sandbox/token \
  --sidecar /absolute/bin/pilotdeck-agent-loop-sidecar \
  --storage-root /absolute/sandbox/snapshot-storage
```

home 与 project 须已存在，且物理路径不能相同或互为祖孙目录；`serve` 只允许监听 `127.0.0.1`，端口可为 0（自动分配）或至少 1024。启动完成后实际监听地址输出到 stderr 的 `native listening …`，stdout 没有 JSON 就绪协议。`--storage-root` 是显式启用快照的存储根，未配置时快照能力保持禁用。

```sh
/absolute/bin/pilotdeck-control schema
/absolute/bin/pilotdeck-control --url ws://127.0.0.1:PORT/ws \
  --token-file /absolute/sandbox/token capabilities
/absolute/bin/pilotdeck-control --url ws://127.0.0.1:PORT/ws \
  --token-file /absolute/sandbox/token connect
```

`connect` 客户端保持 stdin 打开，逐行写请求并持续读事件。例如先初始化，再以实际项目路径创建会话：

```json
{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"pilotdeck-connect/1"}}
{"jsonrpc":"2.0","id":2,"method":"gateway.call","params":{"method":"new_session","params":{"projectKey":"/absolute/sandbox/project","channelKey":"cli"}}}
```

随后以返回的 sessionKey 调用 `run.submit`；以 `run.cancel` 请求取消，以 `permission.respond` 回答权限，其他交互与文件方法通过 `gateway.call` 使用已发现的原生方法。EOF、信号或连接错误不会自动重放写请求；连接中断后应重连并查询 `run_get`、`run_events`、`run_reattach` 的实际状态。不要把一次请求已写到管道当作执行成功。

## 在目标机源码安装和升级

目标机须具备源码构建依赖和干净 checkout；bootstrap control 也由源码构建。原生 PDF 解析通过 `mupdf 0.8.0` 从依赖源码编译 MuPDF，需要 C/C++ 工具链、Make、libclang 及对应标准头文件。只有 libclang 而缺少其 resource headers 的 Linux 环境，须为 bindgen 显式提供本机编译器头文件目录；本次 cb8 验证使用 `BINDGEN_EXTRA_CLANG_ARGS="-isystem /usr/lib/gcc/aarch64-linux-gnu/13/include"`，其他机器不得照抄此路径。registry 依赖由 `src/Cargo.lock` 固定；本地 MuPDF 两包的原包与逐文件校验和固定在 `src/vendor/mupdf-source.json`，构建不下载预编译程序。

安装根必须尚不存在，首代构建该 checkout 的 `HEAD`，`--remote` 必须与它的 `origin` 地址完全一致；`--ref` 记录后续更新目标（默认 `refs/heads/main`），不会替首装自动切换 checkout。可以使用目标机上的私有 Git bare 仓作为 remote；本轮验证即采用这种方式，没有推送 GitHub。

```sh
/absolute/bootstrap/pilotdeck-control install \
  --root /absolute/new-install \
  --source /absolute/clean-checkout \
  --remote /absolute/approved-origin.git \
  --ref refs/heads/main
```

四个程序是 `pilotdeck`、`pilotdeck-control`、`pilotdeck-gateway`、`pilotdeck-agent-loop-sidecar`。同一个固定源码提交在目标机实际编译，校验后发布同代；安装根的 `current` 链接到 `generations/<generation>`，该代含 `source`、`bin` 和 `manifest.json`；`current/bin` 提供当前程序，稳定入口 `bin/<程序名>` 再链接到 `../current/bin/<程序名>`。默认使用该代 `source/skills` 中的 32 项旧版默认技能，随源码安装与更新交付；来源、逐文件哈希和许可见 [skills/README.md](../../../skills/README.md)。`--bundled-skills-root` 是可选的外部目录覆盖，更新保持显式外部链接，不用随包目录覆盖它。技能的外部运行依赖、账户和凭据仍须按对应技能说明配置。

```sh
/absolute/new-install/current/bin/pilotdeck update --check
/absolute/new-install/current/bin/pilotdeck update
/absolute/new-install/current/bin/pilotdeck update --restart
```

`--restart` 需要由本安装的 `pilotdeck server` 启动并持有安装运行身份的服务；`pilotdeck-control serve` 不承担这项安装重启管理。更新进程须沿用该服务的 `PILOT_HOME`，或使用相同的显式 token 环境配置。它经鉴权请求同安装、同代实例切换；成功输出表示服务接受了重启请求，之后仍须确认服务重新就绪。

发布保留旧代，通过原子切换 `current` 选中新代；切换前的构建失败保留旧选择，发布持久化失败会尝试回滚。重启失败或回执未确认时，新代可能已经被选中，不会自动切回旧代；按 stderr 指示检查 `current`、手动重启，不重复执行更新。首装若报 `publicationUnconfirmed`，安装根也可能已发布，须检查现状而非删除后盲重试。保留旧代不等于新增了一个 `rollback` 命令。

## 验收范围

自动证据覆盖 CLI 单元与进程测试、真实 PTY/TUI 模型回合、SDK run、交互和权限、工具执行、历史与恢复、后台子代理/continuable/hooks、memory/snapshot、TCP/stdio、源码安装升级，以及异常路径和实际故障注入。具体源提交、bin SHA、通过数量与未通过项在交付记录中逐项登记。

已配置真实 provider 的请求上限和 token 上限写在探针中；本轮已实测，待人复核签字。真人验收只需在隔离安装上执行自己的 CLI/TUI 任务、检查交互体验和持久化结果，并复核 AI 实测原件。未签字前，不把阶段 D 或整个项目标为完成。

原生宿主以 `--project` 为默认项目，支持同一进程中的已登记多项目、会话与配置代隔离；请求的 `projectKey`、会话所属项目和持久化目录必须一致。会话保留其创建时的配置与资源代，重载后新会话使用新代，资源在最后引用释放后关闭。任意第三方 JS 业务模块和依赖 JS host 的配置不会自动转换为 Rust。`pilotdeck-control serve` 的 token 隔离依赖 Unix `/dev/fd`，源码安装/更新也是当前 Unix 实现；本轮实机平台是 mbp2 macOS，跨平台 setup 固定对照通过不等于已在所有 OS 上实机登录。保留的 `Failure::Todo` 分支用于这些明确不支持的 host/平台或异常依赖，不是空的顶层命令入口。已登记且待批准的安全边界差异不获得冻结判分豁免。

## 前端可直接复用的机器能力

前端可用 `pilotdeck-control connect` 托管一条常驻 NDJSON 连接，也可按动作启动 `call` / `stream`。这层直接调用阶段 A 的原生 Gateway 和阶段 C 的 Rust agent sidecar，不需要阶段 B 的 UI 后端。它提供会话和历史、持久 run 查询与重接、权限/交互应答、文件与上传、模型与设置、插件/技能/MCP、memory/快照、cron/Always-On 和 StaffDeck SOP；配置与供应方能力仍以 `capabilities` 和调用结果为准。独立 `/app/v1` 服务和新版前端属于后续阶段。

SDK 配置不是只保存 JSON：模型与 fallback、managed/user/project/local 设置源、输出样式、插件目录与技能、工具别名/延迟发现、结构化输出、思考和任务预算、prompt suggestions、shell 进度、权限与额外工作目录均连接实际消费者。单次调用和会话持久设置仍遵循旧字段优先级，不能把 `maxTurns` 等顶层提交参数随意移到 `sdkSessionConfig` 内。

`persistSession:false` 使用独立的临时 JSONL、子代理侧链与大内容存储，保持正常回合、读历史和恢复能力；同会话 close/reopen 会保留临时记录，明确删除和宿主关闭才清理。删除持有同一会话的排他生命周期票据，重建不能插入删除与 SDK 清理之间。临时会话不会删除同名持久会话记录。 宿主保留已删除身份以保护重复删除：每进程最多 4096 个保留身份、合计 1048576 个 UTF-16 单元；连续使用不同 UUID 也会累计，达到上限明确拒绝新身份，不静默逐出保护。细节见 `specs/gateway-parity/diffs/D1-SDK-TEMPORARY-DELETE-001-R1.md`。

SOP 的发现、准备、工具执行、提交、等待与恢复使用同一项目配置代和原有持久状态。 中断后新 turn 的回复重放遵循旧版行为，不承诺远端副作用 exactly-once；自定义并行 sidecar 不属于已验证的串行 host-call 执行模型。可选知识能力先取得授权快照，查询前重验 revision/snapshot/generation；必需工具按真实成功执行凭据推进，不凭模型声称成功。Always-On 复用真实计划、工作区和报告执行，工作区回收验证目录与 Git 身份，不以目录同名作为删除依据。 删除失败会被宿主关闭一并报告；每条清理失败记录都继续占该资源代的一个槽位，重试成功不会删除旧记录；在途操作也占槽，总共四槽用尽即拒绝新操作，须关闭并重建该资源代再恢复；安全重试只处理本进程仍持原始身份的对象。Git 登记检查与 ref CAS 不是对外部 Git 写者的原子事务；其他程序同时改动 Always-On 专用分支或仓库不在该排他范围。准备回滚若无法证明目录/注册已完整移除，会保留分支并报错，可能需要检查后人工恢复，不盲删遗留 ref。

## 实现和验证入口

- 旧命令与目标机安装：`src/pilotdeck-cli/src/`，机器入口 `bin/pilotdeck-control.rs`。
- 原生组装与实际请求：`src/pilotdeck-gateway/src/native_process/`；项目、会话、SDK、SOP、Always-On 与工具在同 crate 的 `runtime/`。
- 配套 agent 循环进程：`src/pilotdeck-transport/src/bin/pilotdeck-agent-loop-sidecar.rs`。
- 源码、实际二进制哈希、原始输出和失败后复验记录：本地交付证据 `_tmp/20261108-cli-completion/`；完整性以交付记录中的固定提交为准，不拿共享编译目录代表当前 HEAD。

机器验证与真人实效签字分开记录。macOS 上已执行的证据不会自动变成 Windows/Linux 实机通过；新资源限制的差异条目也不会自动成为旧对拍的评分豁免。
