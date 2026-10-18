# 深数组与技能文件系统边界（#251）

`runtime-boundaries.json` 由 `../record_runtime_boundaries.mts` 运行固定 TS 基线生成，包含入口源码 SHA、Node 版本和全部原始结果。基线只读；显式指定的 scratch parent 下创建临时树，退出时恢复权限并清理。

```sh
PDRS_BASELINE=/absolute/path/to/PilotDeck LANG=en_US.UTF-8 \
node --import /absolute/path/to/PilotDeck/node_modules/tsx/dist/loader.mjs \
  src/pilotdeck-gateway/tests/record_runtime_boundaries.mts \
  src/pilotdeck-gateway/tests/fixtures/runtime-boundaries.json \
  _tmp/20261018-p2-sweep-4
```

- 深数组：TS 使用 10,000 层（远超 V8 栈限），Rust 使用 1,000,000 层。`project_file_read.maxBytes` 执行固定源码中的真实回调，`native_archive_artifact.artifactName` 经真实 InProcessGateway，`sequence` 经真实 validateSessionEventLog；都得到无 code 的 RangeError。前两者再经过真实 GatewayWsConnection，记录完整 response；wire 手工拼接，防止生成器先在 JSON.stringify 溢出。浅数组对照必须正常读取或返回领域校验错误，不能把所有异常当成功。
- RunRegistry：分别以深数组 projectKey/sessionKey/runId 打开真实快照，TS get 拒绝；记录 name/code/message，并确认快照不变、锁已释放。Rust 对应 open 的百万层回归也核原字节、无遗留锁与临时文件。
- scan：先确认正常统计两个文件，再 chmod 子目录为 000，独立 `fs.readdir` 必须实际返回 EACCES，真实 manager.scan 才能证整树计数归零，最后恢复 0755 再核健康结果。生成器拒绝 root；Rust 在权限拒绝不可实现的平台明确打印 NOT_RUN。不得把 NOT_RUN 当本项实测通过。
- import：240 层目录、叶文件原字节、SkillManager 返回值、非默认目录 mode。源第一层 mode 为 0551，复制成功且目标最终 mode 相同，能检出提前 chmod 导致无法写子项的回归；源与目标权限在 finally/RAII 中恢复后清理。另有 128 KiB 栈的私有复制测试，拒绝重新引入每层原生递归。

唯一归一化：临时树根变 `<dir>`，结果 mtime 变 `<mtime>`。未重新定义 V8 溢出的精确阈值；Rust 现有 `MAX_ARRAY_DEPTH=2000` 的保守边界与 TS 调用栈有关的差异保持原范围。
