name: fs-stage-messages
description: Node 整文件读写错误必须按 open/read、open/write 两阶段区分文案；新调用点用共享测试装置防复发
type: project

# fs 错误文案按阶段区分（#649 → #651 → #656）

**为什么**：Node 的 `readFile`/`readFileSync`/`writeFile`/`writeFileSync` 里，open 阶段错误带 `open '<path>'`，read/write 阶段（描述符）错误不带路径（`EISDIR: illegal operation on a directory, read`、`EFBIG: file too large, write`，node v22.22.0 实测见 `_tmp/20261025-issue-656/node-probe/`）。Rust 的 `fs::read`/`fs::write` 把两阶段合成一个错误，若一律按 `"open"` 加路径标注就与 TS 基线不符。#649、#651、#656 连续三单都在补同类漏点（评审每轮都能 grep 出新的一处），不沉淀规则会出第四单。

**怎么用**：
- 移植整文件读写时拆开写：`File::open`/`File::create`（open 阶段，带路径）+ `read_to_end`/`write_all`（read/write 阶段，`descriptor_error(e, "read"/"write")` 或构造错误时传 `None` 路径）。gateway 内 `node_io::path_error`/`descriptor_error` 的 Stage 会覆盖调用方标签，`UploadError::io`、`FileMemoryError::Io`、`workspace_snapshot::io_error` 等渲染器都尊重 Stage；builtin 的 `failed` 例外——它要求 `raw_os_error().is_some()`，Stage 包装会让它落进 kind 兜底，别把 builtin 的调用点迁到 Stage。
- 写阶段错误的可复现验证：子进程 `ulimit -f 1` + 4 KiB 写得 EFBIG；固定小内容（如 10 字节 marker）用 `ulimit -f 0`，连一个字节都写不进。网关侧共享装置在 `src/pilotdeck-gateway/tests/common/write_stage.rs`（`#[cfg(all(test, unix))] mod test_write_stage`：`child_write_error` 拉起子进程、`ignore_sigxfsz_like_gateway` 注册 SIGXFSZ）；CLI 侧照 `pilotdeck-cli/src/skills.rs` 的 marker 两例写（dev 依赖 tokio，workspace 禁 unsafe）。
- EISDIR 类读点测试要 `#[cfg(unix)]`（POSIX 上 O_RDONLY 打开目录成功，Windows 上 open 就失败）。
- 表外 errno 的 `UNKNOWN: unknown error` 回退也要守阶段规则：统一走 `pilotdeck_gateway::node_fs_message_or_unknown`，不要各写一份带路径的兜底。
