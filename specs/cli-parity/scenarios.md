# D0-pr1 首批离线场景（未冻结）

固定基线 `OpenBMB/PilotDeck@cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`。下列行号相对此基线。机器 argv / stdin / oracle 正本为 [scenarios.json](../../scripts/cli-parity/scenarios.json)；命令走 `dist/src/cli/pilotdeck.js`（`package.json:14–16`）。不是命令迁移完成表。

每场景 stdin 为零字节并关闭，三个 stdio 均非 TTY；cwd=`runtime/project`。初始树统一包含独立 home、pilot、XDG 四目录、tmp、project、空 PATH 目录；`pilot/pilotdeck.yaml` 仅 `schemaVersion: 1\n`；`project/keep.txt` 为固定二进制保留文件，`openssl.cnf` 为零字节受控配置。所有目录 mode=0700、文件=0600。不创建真实 server token。初始树在每次执行前重建并记录。

| ID | argv | 准确预期来源（TS） | 登记预期（新隔离策略待复算） |
|---|---|---|---|
| help-long | `--help` | `src/cli/pilotdeck.ts:32–34`; `src/cli/sdkCli.ts:24–26,89–107` | stdout SDK help，stderr 空，exit 0 |
| help-short | `-h` | 同上 | 同上 |
| version-default | `--version` | `src/cli/pilotdeck.ts:30–35,347–348,626–628`; `src/pilot/config/loadPilotConfig.ts:109–128,820–827` | 没有 version 分支；空配置首 fatal 写 stderr，exit 1 |
| empty-nontty | 无参数 | 同上 | 在进入 CliChannel 前报配置错误；**不覆盖有效配置的 stdin 逐行路径** |
| unknown-command | `d0-unknown` | 同上；`src/adapters/channel/cli/CliChannel.ts:25–45` | 空配置错误；**不覆盖未知 argv 拼 prompt** |
| flag-missing | `run --auth-token` | `src/cli/sdkCli.ts:43–44,180–190,218–221`; `src/cli/pilotdeck.ts:626–628` | 缺值回落鉴权来源，最终 auth token required，exit 1；没有自创 parser 错误 |
| invalid-json | `settings set {` | `src/cli/sdkCli.ts:157–174,180–190` | 固定虚构 SDK token 后到 JSON.parse，stderr 全文钉死（V8 12.x 文案，D1-pr2 起），exit 1；不到 updateSettings |
| json-flag-missing | `settings set --json` | `src/cli/sdkCli.ts:164–166,204–221` | 同虚构 token；stderr settings set Usage，exit 1 |
| unknown-gateway-leaf | `gateway unknown` | `src/cli/pilotdeck.ts:285–294` | stderr gateway setup Usage，exit 1，不导入 setup |
| tui-nontty | `tui` | `src/cli/pilotdeck.ts:318–323` | stderr requires interactive terminal，exit 1；不代表 PTY 验收 |
| bundled-skills-missing | 无参数，`PILOTDECK_BUNDLED_SKILLS_DIR=<runtime>/absent-skills` | `src/cli/LocalGatewayBootConfig.ts:70–76`; `src/cli/createLocalGateway.ts:671–683`; `src/extension/skills/migrateLegacyBundledSkills.ts:73–80,139–146` | 内置技能根不存在：stderr 先打 `Bundled skills root is missing…` 迁移警告，再报空配置首 fatal，exit 1；**不写**完成标记（无 `diskAdditions`）（D1-pr2，#214） |
| sessions-bogus | `sessions bogus` | `src/cli/sdkCli.ts:135–155,180–195`; `packages/sdk/src/client.ts:130–135,3238–3260` | 固定虚构 token；建 client 不建连，子命令不认识即 sessions Usage，exit 1（D1-pr2，#218） |
| resume-missing | `resume` | `src/cli/sdkCli.ts:43–56,204–216` | 固定虚构 token；缺 session-id 在 `sessions.resume` 之前报 resume Usage，exit 1；prompt 检查（:58–59）在建连之后，不在本场景（D1-pr2，#218） |

环境由装置白名单构造，绝不继承宿主环境：HOME/USERPROFILE/CFFIXED_USER_HOME 指向独立 home，PILOT_HOME 与 PILOTDECK_CONFIG_PATH 指向 fixture，四个 XDG 目录、TMPDIR/TMP/TEMP、PWD、空 PATH、OPENSSL_CONF 指向 runtime/openssl.cnf、LANG/LC_ALL=C、TZ=UTC、TERM=dumb、NO_COLOR=1。ANALYTICS_ENABLED=0、DOCKER_CONTAINER=1、COMMIT_HASH=固定源 HEAD（`src/telemetry/context.ts:94–103,136–149`，避免 Git 探测），网关两个 URL 均固定 `127.0.0.1:28999`，内核禁止所有网络，包括 loopback、监听及 Unix socket。仅 `token:true` 的场景（两个 settings 场景、sessions-bogus、resume-missing）设虚构 `PILOTDECK_SDK_AUTH_TOKEN=d0-fixture-not-a-secret`；envPaths 不得注入任何 `*TOKEN` 变量。运行证据对 token 只存长度与哈希。

首批都在联网调用前退出；内核网络隔离仍是前置，因为有效配置下默认路径探测固定 18789，不能靠端口变量保护用户运行中的 PilotDeck。`version-default`、`empty-nontty`、`unknown-command` 的落盘预期按真实 TS 原件登记为初始树加 `diskAdditions`；其余场景仍要求无落盘变更。比较完整最终树，新增项不得覆盖初始树路径；任何额外写入、缺失、内容、类型或 mode 不符均失败。负控移除保留文件验证完整树漏落盘，另增文件验证越界于预期文件清单的写入。

三个场景各自的真实证据在 主仓 `/Users/da/ws/pilotdeck-rs/_tmp/20261017-d0/worker/calibration-host5/ts-1/cases/<场景>/initial.json`、`final.json` 与 `tree/`。逐一核对后，唯一增量是 `pilot/.legacy-bundled-skills-migrated-v1`：普通文件、mode=0600（JSON 十进制 384）、10 字节，内容为 UTF-8 `completed\n`；SHA-256 的机器正本见 [scenarios.json](../../scripts/cli-parity/scenarios.json) 中对应项。TS 出处 `src/extension/skills/migrateLegacyBundledSkills.ts:66` 定义 marker 路径，`:73–79,139–146` 检查内置 skills 根可读且包含技能，`:123–126` 在无迁移失败时写入完成标记。mode 取真实原件的 lstat 值，不从 writeFileSync 默认值推测。host5 原件已用于旧装置定标；它不替代本次 profile、warning 比较视图与 fixture 修正后的真实复算。

help 两场景的 755 字节 stdout 全文 SHA-256 从主仓 host5 两个原件核同后登记于 scenarios.json，测试用完整字节见 scripts/cli-parity/help.txt；前缀后的中段变化也必须拒绝。非法 JSON 的 V8 文案随 Node 版本变化；D1-pr2 起 oracle 钉 stderr 全文（与 Rust 单测中 host5 实录及 `src/pilotdeck-protocol/tests/fixtures/json-syntax/cases.json` 的 Node 22 差分夹具同源），换 Node 主版本须重新定标，两轮 Node 路径/SHA 仍必须一致。

D1-pr2 新增三场景（共 13 个）。场景可带 `envPaths`：只许**新增**环境变量、值为 runtime 下的相对路径（不得为空、绝对或含 `..`，不得覆盖上表隔离变量），由 `common.environment` 校验。新场景的期望值一律由真实 TS 轮定标后填入；未定标的场景标 `"oracle": "PENDING-TS-CALIBRATION"`，TS 轮比较时恒报 `<id>.oracle.pending`，不可能 PASS。Rust 轮离线复验（compare.py）除核对 round.json 记录的哈希外，还读取 `rust-stage/bin/pilotdeck` 的盘上实际字节（须为普通单链接文件），不符即 `round.rust-stage-bytes`；staged skills 目录的身份记入 round.json 的 `stagedSkills`，离线复核盘上目录不符即 `round.rust-stage-skills` / `round.rust-stage-skills-bytes`（D1-pr3）。host5 旧定标已有真实证据；本次修改后的稳定性须以新 TS 两轮复算为准，不沿用旧 PASS。setup/update mock、信号、PTY、有效配置默认流和网关业务属于 D0 后续批次。

## D2-pr1 新增批次（16 个已定标）

注册正本仍是 `scripts/cli-parity/scenarios.json`；现共 29 例，全部已填期望。既有 13 例及第二轮定标的 15 例保持原 oracle；这 15 例的 exit、stdout、stderr、disk 均从协调者采集的 `_build/wt-d1-cli/_tmp/20261018-d2-skills/d0-calib1/ts-1/cases/<ID>/` 机械提取，TS 第二轮用于交叉核对；没有从 Rust 输出取值。具体取 `record.json.exitCode`、`stdout.bin` 全文、`stderr.bin`（只剥第 6 条 SQLite 前缀）、`initial.json` 与 `final.json` 的逐项差集，并核实际 `tree/`。其中 12 个 stdout 按 [比较口径第 4 条](normalization.md) 绑定 runtime，反向展开须逐字等于实录。原件 SHA-256 清单和提取脚本见该任务目录 `oracle-provenance-r2.json`、`calibrate_r2.py`。`legacy-skills-current` 的第六轮定标来源另见下文。本次改动会改变装置哈希，正式新轮由协调者运行；旧轮不冒充新装置 PASS。未定标的未来场景仍用 `PENDING-TS-CALIBRATION`，即使两轮一致也不能 PASS。

| ID | 覆盖目的 | TS 出处 |
|---|---|---|
| legacy-skills-mixed | KNOWN_LEGACY 命中的 browser-use 被 rename 备份；自定义 docx 保留；完成标记 | migrateLegacyBundledSkills.ts:20–25,59–135 |
| legacy-skills-current | 用户 docx 完整树与当前内置相同，覆盖 matchesCurrent；完整移入备份并写完成标记，输出迁移摘要后报空配置错误，exit 1 | migrateLegacyBundledSkills.ts:90–108 |
| skills-usage / skills-unknown-leaf | 缺子命令、未知子命令 | pilotdeck.ts:426–433 |
| skills-from-invalid | 未知来源保留原 raw 文案 | pilotdeck.ts:462–480 |
| skills-missing-values | --from/source/project/pilot-home 缺值的回落 | pilotdeck.ts:527–543 |
| skills-default-dry | 默认来源与文字报告 | migrateSkills.ts:55–59,186–227；pilotdeck.ts:483–514 |
| skills-alias-dedupe | 别名、all、重复来源和自定义路径去重，JSON | pilotdeck.ts:462–480；migrateSkills.ts:230–239 |
| skills-custom-dry / skills-custom-execute | 直接子目录/单技能发现、验证、完整目录复制、JSON | migrateSkills.ts:99–182,242–269；SkillManager.ts:328–426 |
| skills-conflict-skip / skills-conflict-rename / skills-conflict-overwrite | 冲突保留、-imported-2、overwrite 优先且删除旧文件 | migrateSkills.ts:272–289；pilotdeck.ts:436–440 |
| skills-invalid-json-report / skills-invalid-text-report | 验证错误的 JSON/text 不同退出码 | pilotdeck.ts:451–459 |
| skills-project-home-overrides | project 只影响来源，显式 pilot-home 决定目的地 | pilotdeck.ts:441–450；migrateSkills.ts:186–198 |

历史副本的内容不进入本仓库。`fixtureBlobs` 固定读取基线 Git 的 `db77b86d09441c5fab6340a669598f8fbabe82c7:skills/browser-use/SKILL.md`（2702 bytes，文件 SHA-256 `d381166cfd54b5f86ff395c7faee4f36f6a67887bdf830f36ddaa4ec32f0a2df`）。这是单文件树，`SHA256("SKILL.md\0" + blob + "\0")` 为 TS 的历史指纹 `e955f4f0813e4735c8130c2ccab5725a35a9f5dcb9098159c35d5ecf895ab2f8`。读取不 checkout、不更改基线；原件缺失或身份不符立即报错。其余 `fixtureFiles` 为本项目自写内容，路径限制在 runtime 内，父目录 0700、文件 0600，初始树由注册内容独立复核。

`legacy-skills-current` 的 14 个 `fixtureBlobs` 固定到基线 commit 下完整 `skills/docx/` 树，只登记路径、长度和 SHA-256，不将第三方文件内容入库。准备时逐个核对 Git blob 与只读当前内置文件字节相等；文件 mode 不参与 TS 树哈希。第六轮先核协调者 `d0-final3/ts-1` 与 `ts-2` 中该场景的退出码、输出、完整初末树及有效跨流顺序一致，再从 `ts-1/cases/legacy-skills-current/` 按上述方法机械提取期望并移除 PENDING：exit 1，stdout 为迁移摘要（按输入 runtime 绑定，批次共 13 个），stderr 仅去 SQLite 前缀后为配置错误；初末树 32→35 项，删除 18 项、增加 21 项。该任务目录的 `calibrate_sixth_round.py` 和 `oracle-provenance-sixth-round.json` 保存提取过程与两侧原件 SHA-256。缺定标仍由既有 PENDING 守卫拒绝 PASS。第六轮两轮有效顺序均为 stdout→stderr，当时保持默认严格比较；#933 批次（2026-10-28，`_tmp/20261026-issue-933/final-round`）在高负载下首次翻转为 ts-1 `stderr→stderr→stdout`、ts-2 `stderr→stdout→stderr`（各路字节相同），证明与 `legacy-skills-mixed` 同属 recorder 双管道读取交错竞态，自此按场景声明 `streamOrder: "per-stream"`（证据路径登记在 `streamOrderReason`），仅豁免顺序字段，逐流字节比较不变。

新增场景会删除/移动初始文件，定标后可登记 `diskRemovals`（初始树已有路径，禁止根目录、重复和不存在路径）与 `diskAdditions`（不得覆盖保留下来的初始项）。最终仍逐项比较完整树，未登记的新增、删除和变化不获豁免。

协调者已运行 `LANG=C LC_ALL=C node scripts/cli-parity/skills_probe.mjs /Users/da/ws/pilotdeck-rs/resources/PilotDeck`，原件为主仓 `_tmp/20261018-d2-skills/r1/probe.json`：Node 22.22.0、ICU 78.2，默认 locale 确为 en-US。Rust 回归夹具 `src/pilotdeck-cli/tests/fixtures/skills-probe.json` 只摘取该原件中的比较矩阵、稳定排序和哈希输入/结果；逐项覆盖 225 组比较、15 项排序、Unicode/空目录/软链接树哈希。正式 D0 Rust 对拍及双家独立终审仍由协调者执行。

## D0-pr2 mock gateway 批次（#933）

现共 30 例。带 `mockGateway: true`（必须恰为布尔 True，`common.mock_gateway_scenarios` 校验）的场景在 sandbox profile 的 `(deny network*)` 之后追加、且仅追加一条 `(allow network-outbound (remote ip "localhost:28999"))`（host 字面量必须是 `localhost`，写 `127.0.0.1` 会被 sandbox-exec 拒绝；端口由 `common.MOCK_PORT` 派生，allow 规则与 `environment()` 的网关 URL 同源）；其余 29 例两侧 profile 字节不变（单测以**固定占位路径**（`/PDRS-RUNTIME/runtime`、`/PDRS-BASELINE/checkout`、`/PDRS-NODE/bin/node`、`/PDRS-RUSTBIN/rust-stage/bin/pilotdeck`，`policy.SCRIPT` 一并 patch 为 `/PDRS-SCRIPT/scripts/cli-parity`）生成 ts/rust × base/mock 四种 profile 的 SHA-256 逐字节钉死装置语义——ts base 兼作两侧的探针 profile。不存在任何归一化步骤可掩蔽权限变化：放宽任一规则、改端口、改 deny 顺序都会翻哈希；换真实检出根（含非 ASCII 路径或嵌在系统路径下的根）天然无关，守卫仍绿；五个占位输入各居互不相交的独立根（测试断言其祖先集合两两不相交、亦不与 policy 固定读路径的祖先重合，fix-r5 P1），故任何「向 metadata allow 新增某输入父目录」式变异——如 rust 侧 `ancestors.add(SCRIPT.parent)` 会授予真实 checkout `scripts` 目录——必翻哈希）。mock 由装置在沙箱外以独立进程组启动（`scripts/cli-parity/mock_gateway.mjs`，自实现 RFC 6455，不依赖基线包），单轮内独占；28999 的归属以 mock 自身 bind 成功为准——装置只等 mock 自己打到 stdout 的 `listening` 确认行（就绪后还会复核同一进程仍存活），不存在装置先 bind 预检再移交的 check-then-use 空窗；mock 提前退出（含 28999 被占）或就绪后死亡，整轮 `Blocked` 拒跑，不换端口。每轮 preflight 用 `gateway_isolation_probe.mjs` 在放宽后的 profile 下实测：28999 可连（一项布尔）、18789（用户 PilotDeck）/任意其他端口/Unix socket/listen 仍内核级 EPERM（四项错误码），入 `round.json` 的 `mockIsolation`，compare 离线重建字节核对。Rust 侧 `rust-mock.sb` 同样只追加该条 allow，且 Node exec 仍须被拒（`round.mock-node-exec`）。mock 未起或中途死亡时场景失败而非跳过：场景得到连接错误，与 oracle 不符即 FAIL。首个场景 `sessions-list-mock`（`sessions list`，token:true）期望值已由真实 TS 两轮提取（exit/stdout/stderr 已填入注册正本），不从 Rust 取值。
