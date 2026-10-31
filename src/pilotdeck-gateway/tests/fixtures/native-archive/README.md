# Native archive 候选证据

本候选仅覆盖固定 `cc7c4e6` 的 `createLocalGateway()` 默认磁盘装配：无自定义 authorizer、retention、nativeSessionStorage 或外部 artifact store。#134 精确安全差异授权仍 **PENDING**；这里的候选错误不是 accepted difference。不得据此合并依赖该差异的 archive、关闭 #134/#46 或报 A4 完成；冻结文件没有改。

`cases.json` 原样取自保留的旧候选 `feat/a4-native-archive@9589be4`，SHA-256 `60e878bb9f5433424129822309d1783207cda8c65cbd65d951eabd3a472adef7`。138 个真实公开 RPC 请求/响应保留原字节与磁盘事实，8 份基线源码 SHA 已重核。旧 `forward` 与 `securityEvidence.approvedDifference` 是历史作者元数据，不是本候选路由或人的授权；本候选不使用它们。D6 现在让 138 项全部本地逐字节相等；真实 pairing 后的 observer fence 与 silence 检查上游零请求。原录制快照不因路由期望变化而改写。另将33条文件事实在本轮临时目录实际物化（两大文件为sparse），仅一致替换home/cwd绝对路径及.cwd内容；138条Native响应和138条Native配对逐字节符合显式路径迁移后的原件。该测试不是重写Node原响应，固定原件SHA不变。

`host-boundary.json` 是本轮真实 Node 默认装配的 11 个独立公开 RPC 与文件事实：同 sequence 的孤立 UTF-16 和 +0/-0 同组精确退回4项；唯一 sequence 的孤立 UTF-16、有效代理对、坏行优先报 invalid，以及不读取 transcript 的 artifact 共7项本地。配对测试实际核对4条原样上游请求和7条本地响应。#132 的 A8 宿主边界仍未关闭；#133 Linux 非法字节 dirent 已由 `../project-files-linux-dirent/` 的 Linux 门控测试覆盖，需 cb8 主机运行（证据由协调者产出后收口）。

`io-errors.json` 是两个 transcript method 自己的12条真实 RPC：每个分别 ENOENT/EISDIR/ELOOP/ENOTDIR/EACCES/ENAMETOOLONG。read-only fs observer 保留真实 stat/readFile 的 API、目标、errno/syscall/message，不注入错误。`RUN` 只能由目标实际抛出预期 errno 获得；NOT_RUN 不准入。Rust 以这些固定错误事实重放整个 RPC，并实际 pairing 核对12条本地响应；不是恢复权限后的 Native errno 重现。method 专属 whitelist 与这12条来源集合精确相等，未知 errno/非OS错误仍为显式 host 边界，唯 archive 非 regular/超过既有 2GiB 边界的专用拒绝标记由 transcript 本地返回 PATH_NOT_ALLOWED（artifact 仍按既有契约归并 ARCHIVE_ARTIFACT_NOT_FOUND）；transcript 最多读取 50MiB+1，实际超过 50MiB 返回 SESSION_TRANSCRIPT_INVALID。自然 stat ELOOP 保留 Node 错误；NOFOLLOW 安全失败与 canonical 之后 checked stat/open 的 ENOTDIR 则本地拒绝，不能发回不安全 TS。

`security-node.json` 原样保留 `_tmp/20261016-archive-resume/security-probes/result.json` 的9条真实默认 Node RPC：三method各 normal/static根外链接/受控替换。TS均成功读出测试根外 sequence4242/对应artifact内容。Rust用新的隔离真实文件重建9场景，normal与原RPC字节相等；静态/替换场景候选拒绝为 transcript PATH_NOT_ALLOWED / artifact ARCHIVE_ARTIFACT_NOT_FOUND，实际 Native 与实际 pairing 各9条唯一 receipt；每个受控替换必须命中一次。项目注册关系不得调用 session_summary（测试一旦触发即失败）。这避免在归档安全检查前通过无关摘要读取 transcript。

另外以真实 `open_beneath` 实测 leaf/ancestor/root 替换，检查 readable descriptor 在读取前被拒绝；普通 read 对照确认替换实际能到达任务自建根外秘密。macOS祖先/根替换会返回 ENOTDIR，不能只识别ELOOP。该最小实测证明指定交错，不代表随机竞态概率或所有文件系统行为。原安全9项测试另核对普通内根artifact链接仍可读。所有根外目标都是本任务自建fixture，未触碰用户配置。

`nul-long-path.json` 从本轮维护版录制器重录的138条RPC中原样摘出4条NUL长路径请求/响应，并绑定整份录制SHA；Node按128个UTF-16单元截断inspect，绝对路径迁移不保证这4条错误文本不变。短根Native物化比较与长根4条独立原件分开测试；复用session_model已由独立Node原件验证的纯formatter，抽到node_io，保留session原回归。新重录138与旧138的4条文本差异原件保留，不冒称整份重录可仅替换路径后全等。

`in-root-links.json`另有三条真实Node内根链接RPC，Native与pairing均保持原响应；不把安全拒绝扩张到正常内链。

`marker-node.json`是本轮新增三method×static/race/inside共9条真实默认Node RPC，sync/async read observer保留`.cwd`实际读取的字节与pre-open替换事件。Node没有canonical检查，不把它称为stat→read竞态。Rust只在archive调用链使用RegistryFiles：marker内容的允许根为canonical PILOT_HOME，archive transcript内容根仍为canonical PILOT_HOME，artifact内容根为canonical project。静态外链在任何内容打开前拒绝；受控替换在marker canonical之后、真正descriptor打开前发生，实际命中并拒绝。测试禁止原始Files.read和session_summary旁读。当前非legacy注册fixture的候选响应为PROJECT_NOT_FOUND；内根marker链接保持原响应，已有legacy恢复规则仍保留。Native/真实pairing各9条唯一marker receipt。这个拒绝原版可读外部marker的差异也明确等待#134同一精确授权，不继承#68。

本候选没有承诺所有metadata访问安全：registry目录枚举名称、stat元数据、项目根选择仍沿用现有语义；不会把内容安全9探针泛化为整条metadata安全。#134待批准范围包括指定transcript/artifact静态外链、canonical后leaf/ancestor/root替换，以及`.cwd`静态外链/检查后替换。artifact错误归并、普通自然errno、孤立UTF16宿主边界分别按上述原件说明。

`project-key-identity.json` 是独立新增的 4 条真实 Node WS 原件，由
`tests/record_native_archive_project_key.mts` 在全新隔离 scratch 生成，不改历史
`cases.json`。它覆盖三个 `native_archive_*` method 共用的
`projectRegistry.ts:41-59` seam：

- `project-folded-unknown`：`/fixture/project-\ud800/../missing`。`path.resolve`
  是纯词法折叠，`..` 把含代理项的段弹掉后仍未注册；`Unknown projectKey: ${projectKey}`
  保留调用方的原始 units。lossy `String.from_utf16_lossy` 会把文案写成
  `/fixture/missing` 的 U+FFFD 拼写，故这条是文案负控。
- `project-folded-registered`：同一折叠得到 `/fixture/project`（fixture 的 cwd），
  命中已注册项目并成功回包；正控证明折叠确实发生在 registry 比较之前。
- `project-folded-unknown-relative`：相对式 `project-\ud800/../missing`，由
  `path.resolve` 对进程 cwd 折叠；文案仍保留相对拼写的原始 units。
- `project-literal-fffd-unknown`：字面 U+FFFD 与代理项是不同身份；未注册时
  文案保留字面字符而不是替换成代理项。

文件快照把生成时的 scratch 根替换为 `/fixture` 并重算字节长度，因此两次生成
逐字节一致（SHA-256 `50ea9249e7d0a17ddd679e75e9a1fc9d7bd9990571302a4b3a1a95a21526fd22`）。

## 复现

仓库提供 `tests/record_native_archive.mts`、`record_native_archive_project_key.mts`、`record_archive_host.mts`、`record_archive_io.mts`、`record_archive_security.mts`、`record_archive_markers.mts`。每个接收三个绝对参数：只读基线目录、**不存在**的输出文件、预建的独立scratch目录。要求 Node v22.22.0、基线HEAD精确cc7c4e6、HOME/XDG_CONFIG_HOME/XDG_CACHE_HOME/XDG_DATA_HOME/TMPDIR/PILOT_HOME各自目录都位于scratch内部，五个 browser/polling guard 均设1。通过 `node --import <主仓>/scripts/gateway-parity/watcher.mjs --import <基线>/node_modules/tsx/dist/loader.mjs <录制器> <基线> <新输出> <scratch>` 执行，无监听或真实模型调用。输出成功不代表授权通过；安全录制器是在确认原版漏洞。

Rust 使用原已安装工具链绝对 cargo/rustc/rustdoc、原HOME/cache、AUTO_INSTALL=0、四wrapper环境变量明确置空、独立target、jobs2及`--locked --offline`。测试命令：`cargo test --manifest-path src/Cargo.toml -p pilotdeck-gateway --lib archive --locked --offline -- --nocapture`。不将 Node 隔离HOME传给Cargo，不启动rustup代理，不下载工具链。

本轮源码绑定、精确argv/env/preflight、成功及失败原件见主仓 `_tmp/20261016-debt-cleanup/archive-implementation/`。未运行固定A2十方法或53场景全量；旧成绩不能充当新HEAD验收。完整网络与真人使用仍待后续门。
