# CLI D0-pr1 比较口径（未冻结）

本文件先于实现登记。仅用于首批离线定标，不批准任何 Rust 行为差异。

1. **stdout / stderr 原件不归一化**：保存原始 bytes，包括换行、ANSI、空白、错误文本；除第 6 条唯一归一化外，每路内部顺序精确比较。另存双 pipe 的采集事件，合并相邻同流 read 后比较流名与 bytes；不比较 read 分块大小、采集时钟。这个事件序列是观察到的跨流顺序，不能证明内核 write 的全序；首批期望至多一路非空。未来混合输出须扩展记录装置，不能用本轮覆盖声明替代。
2. **退出码精确比较**：信号产生的负值保留，超时/截断/采集或清理失败属于装置错误，不能因两侧相同而 PASS。
3. **完整落盘**：对唯一可写 runtime 的初始及最终树逐项记录相对路径、类型、mode，普通文件的长度和 SHA-256、软链接的目标。含空目录、隐藏文件；不跟随软链接。初始树必须与场景 fixture 一致，最终树重新从原件读取，拒绝与缓存 manifest 不符。增加/删除/内容/类型/权限/链接变化都比较；不只比较预期文件。mtime/ctime/atime、inode、uid/gid 不属于本轮内容契约，不参与比较。特殊节点、越界链接、硬链接一律拒绝。
4. **路径不替换**：两次 TS 与可选 Rust 均顺序复用同一个 runtime 绝对路径，结束后移至各自证据目录，每次恢复初始树。不同 runtime 路径的证据不能互比。没有 UUID、时间、路径、错误、版本文本的替换；警告仅有第 6 条精确例外。
5. **观测元数据**：PID、wall clock、耗时、源 HEAD/二进制哈希用于 provenance 与完整性检查，不当产品相等字段；每一侧仍须有清理成功、OS 隔离预检通过、有效进程树记录。运行命令、环境、stdin、场景集合、fixture、装置/spec 哈希必须绑定一致。

6. **唯一归一化：TS stderr 开头的 Node 22 SQLite 两行**（本轮按用户明确指定边界修正）：不向 Node 传 `--disable-warning`，完整保存 stderr.bin 与 events.json；先校验二者原始字节相等，再构造只用于 oracle 与成对比较的视图。仅当 **TS** 的 stderr 以以下完整字节模式开头时，剥离 **一次**，`<数字>` 仅匹配一个或多个 ASCII 十进制数字，两行均须以 LF 结束：

   ```text
   (node:<数字>) ExperimentalWarning: SQLite is an experimental feature and might change at any time
   (Use `node --trace-warnings ...` to show where the warning was created)
   ```

   同时仅从比较用事件序列的 stderr 字节中扣除这段前缀，保留剩余跨流顺序；原件不写回。**Rust 侧完全不剥**；正文中非开头位置、第二次重复、缺行、CRLF、其他文本/类型/code 的任何警告均保留并比较。原因是 Node 22 SQLite 的已实测启动提示含每次不同的 PID，且不属于 Rust 产品应复刻的输出；按类别禁用会吞产品警告，故禁止。实测原件位于主仓 `/Users/da/ws/pilotdeck-rs/_tmp/20261017-d0/worker/calibration-host2/ts-1/cases/help-long/stderr.bin`（两行完整原件），对应 `ts-2/cases/help-long/stderr.bin` 证明 PID 变化；host5 使用旧禁用参数，不作本条的新定标证据。新 profile/空 OpenSSL 配置须由协调者重新真实定标。


TS 自比必须先过场景独立预期及证据完整性检查，再比较两次原件。任一缺失场景、额外场景、缺失原件、模式不符或未运行都不能 PASS。运行器不提供放宽/忽略参数。
