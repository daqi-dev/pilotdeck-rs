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

环境由装置白名单构造，绝不继承宿主环境：HOME/USERPROFILE/CFFIXED_USER_HOME 指向独立 home，PILOT_HOME 与 PILOTDECK_CONFIG_PATH 指向 fixture，四个 XDG 目录、TMPDIR/TMP/TEMP、PWD、空 PATH、OPENSSL_CONF 指向 runtime/openssl.cnf、LANG/LC_ALL=C、TZ=UTC、TERM=dumb、NO_COLOR=1。ANALYTICS_ENABLED=0、DOCKER_CONTAINER=1、COMMIT_HASH=固定源 HEAD（`src/telemetry/context.ts:94–103,136–149`，避免 Git 探测），网关两个 URL 均固定 `127.0.0.1:28999`，内核禁止所有网络，包括 loopback、监听及 Unix socket。仅两个 settings 场景设虚构 `PILOTDECK_SDK_AUTH_TOKEN=d0-fixture-not-a-secret`。运行证据对 token 只存长度与哈希。

首批都在联网调用前退出；内核网络隔离仍是前置，因为有效配置下默认路径探测固定 18789，不能靠端口变量保护用户运行中的 PilotDeck。`version-default`、`empty-nontty`、`unknown-command` 的落盘预期按真实 TS 原件登记为初始树加 `diskAdditions`；其余场景仍要求无落盘变更。比较完整最终树，新增项不得覆盖初始树路径；任何额外写入、缺失、内容、类型或 mode 不符均失败。负控移除保留文件验证完整树漏落盘，另增文件验证越界于预期文件清单的写入。

三个场景各自的真实证据在 主仓 `/Users/da/ws/pilotdeck-rs/_tmp/20261017-d0/worker/calibration-host5/ts-1/cases/<场景>/initial.json`、`final.json` 与 `tree/`。逐一核对后，唯一增量是 `pilot/.legacy-bundled-skills-migrated-v1`：普通文件、mode=0600（JSON 十进制 384）、10 字节，内容为 UTF-8 `completed\n`；SHA-256 的机器正本见 [scenarios.json](../../scripts/cli-parity/scenarios.json) 中对应项。TS 出处 `src/extension/skills/migrateLegacyBundledSkills.ts:66` 定义 marker 路径，`:73–79,139–146` 检查内置 skills 根可读且包含技能，`:123–126` 在无迁移失败时写入完成标记。mode 取真实原件的 lstat 值，不从 writeFileSync 默认值推测。host5 原件已用于旧装置定标；它不替代本次 profile、warning 比较视图与 fixture 修正后的真实复算。

help 两场景的 755 字节 stdout 全文 SHA-256 从主仓 host5 两个原件核同后登记于 scenarios.json，测试用完整字节见 scripts/cli-parity/help.txt；前缀后的中段变化也必须拒绝。非法 JSON 的 V8 文案随 Node 版本变化；D1-pr2 起 oracle 钉 stderr 全文（与 Rust 单测中 host5 实录及 `src/pilotdeck-cli/tests/fixtures/json-syntax/cases.json` 的 Node 22 差分夹具同源），换 Node 主版本须重新定标，两轮 Node 路径/SHA 仍必须一致。

D1-pr2 新增三场景（共 13 个）。场景可带 `envPaths`：只许**新增**环境变量、值为 runtime 下的相对路径（不得为空、绝对或含 `..`，不得覆盖上表隔离变量），由 `common.environment` 校验。新场景的期望值一律由真实 TS 轮定标后填入；未定标的场景标 `"oracle": "PENDING-TS-CALIBRATION"`，TS 轮比较时恒报 `<id>.oracle.pending`，不可能 PASS。Rust 轮离线复验（compare.py）除核对 round.json 记录的哈希外，还读取 `rust-stage/bin/pilotdeck` 的盘上实际字节（须为普通单链接文件），不符即 `round.rust-stage-bytes`。host5 旧定标已有真实证据；本次修改后的稳定性须以新 TS 两轮复算为准，不沿用旧 PASS。setup/update mock、信号、PTY、有效配置默认流和网关业务属于 D0 后续批次。
