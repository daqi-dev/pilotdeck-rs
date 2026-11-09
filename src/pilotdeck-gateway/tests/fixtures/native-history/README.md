# Native history 的真实 TS 对照

固定基线 `cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`；mbp2 的 Node v22.22.0 / ICU78.2。测试调用实际 TS `readWebSessionMessages` 与 `JsonlSessionPersistence`，没有重新实现 TS 作为预期值。输入、实际解码 entries、完整返回或异常均保留在六个 JSON 文件中。

| 文件 | 格数 | 重点 |
|---|---:|---|
| typical-r1.json | 15 | 普通消息、分页和会话封套 |
| boundary-r1.json | 29 | 类型、事件与边界，其中1格实际异常 |
| extra-r1.json | 4 | 补充事件投影 |
| cursor-image-r1.json | 18 | parseInt 游标、非文本内容和图片 |
| coercion-r2.json | 21 | JS 加法/数组强转、synthetic 与 compactReplacement，其中1格实际异常 |
| background-r3.json | 12 | 背景文件选择、metadata 与路径；10格已解码、2格在读前拒绝 |

`runtime::history::envelope::tests::complete_history_matches_ts_goldens` 比较97格的完整投影结果或精确异常文案；余下2格非法背景路径在 reader 路径测试覆盖，不冒称也经过投影器。背景采集记录真实 stat/readFile 的路径与字节指纹，以内容不同的普通会话作正控，provider/catalog 调用均为0；普通会话正控均实际调用，避免用空 loadedEntries 占位。图片、compact、status/error、incomplete、artifacts、subagent 展示及 usage/session 字段均使用同一原始 entries 投影，分页发生在完整投影之后。

原始实采、驱动、TS源码指纹与 stdout/stderr 位于主仓 `_tmp/20261108-cli-native-history/oracle/` 及 `_tmp/20261108-cli-consolidation/history-oracle/capture-r{1,2,3}/`，索引/说明在后一目录的同级文件。旧48格没有重写，新增采集仅追加；本目录是可提交的输入和结果，原始独占目录保持不覆盖。

在mbp2工作镜像的 `src/` 运行 `cargo test -p pilotdeck-gateway --lib runtime::history:: --locked`。真实 CLI/Gateway/sidecar/PTY 的隔离探针、二进制指纹及运行结果由主仓 `_tmp/20261108-cli-consolidation/README.md` 索引；运行状态以实际结果记录为准。原生 `read_session_messages` 已接既有 project registry/turn gateway，不建立第二套宿主；`read_subagent_messages` 与导出/恢复等尚未接线的方法不因此宣称完成。

输入文件/项目 marker、后台 symlink、并发及响应帧限制见 `specs/gateway-parity/diffs/D2-NATIVE-HISTORY-BOUNDS-001.md`。这些是明确的新安全边界，不能作为冻结验收的已接受差异或整阶段通过。不会访问用户运行中的服务或真实 `~/.pilotdeck`；Rust编译和运行仅在mbp2。

## 可重复的跨入口实测

`native.py` 是本目录真实进程驱动。先在mbp2构建 `pilotdeck-cli --bins`、`pilotdeck-gateway --bin pilotdeck-gateway`、`pilotdeck-transport --bin pilotdeck-agent-loop-sidecar`（均 `--locked`），再把四个生成文件的绝对路径分别传入 `--pilotdeck`、`--control`、`--gateway`、`--sidecar`，`--out` 必须是尚不存在的独占目录。默认通过 `pilotdeck-control serve` 启动原生宿主。驱动只使用该目录内的HOME/PILOT_HOME/project/文件和随机loopback端口，provider是假OpenAI服务，不需key或外网。

驱动执行自己的二进制副本并核前后SHA；150秒整轮预算及每次子进程期限，SID+进程身份核对后清理所属子孙，TERM/HUP/INT进入清理时不再次中断。它先验证实际stream和SDK/分页，再通过固定100×30 PTY的已知坐标核两行history文字与termios恢复，接着取消、重启、背景不同内容与同ID普通文件诱饵、约35MiB输入膨胀超100MiB封套、FIFO/超50MiB转录、FIFO/超64KiB marker及恢复后读正控。机器入口的失败必须exit1/stdout空/stderr结构化error；status消息不能被当成只有两条text的历史而漏验。`result.json` 的pass、cleanup_verified、各回收标记和binary_snapshots_unchanged必须同时为true；这不等于人眼体验验收或真实provider实效。

最终r6编译输入在主仓consolidation目录的 `verify-r6/postfmt.json`；同源码实际运行 `runtime-r4-driver` 为exit0/pass=true；随后串行build与 `runtime-r5-driver` 再验证通过，2979源文件前后不变，4个执行副本/原二进制SHA均精确匹配同轮构建记录。r4的两个CLI指纹与更早build日志不同（中间执行过cargo test），因此以这条r5串行来源链收货。55项观察和前3次失败驱动原件均保留。最初3次失败来自探针错误通道、CSI分词和状态行假设，分别修驱动，未改变产品预期使之变绿。定向结果：history12、connection25、catalog1、turn-read24、storage45、transcript adapter13全部通过；control serve退出2、stream3、六格消费者父测试1通过；两个默认ignored的真实native/control共享宿主测试均显式运行通过。若干需TS外置条件和helper仍有ignored，不称Gateway全量回归。全部Rust运行仅mbp2。
