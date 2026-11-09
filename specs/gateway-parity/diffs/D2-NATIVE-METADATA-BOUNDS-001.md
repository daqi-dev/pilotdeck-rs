---
id: D2-NATIVE-METADATA-BOUNDS-001
---

## 类别

原生机器入口的文件资源及异步宿主可用性修复；常规数据沿用既有解析，资源拒绝不伪装为成功。

## TS 依据

固定 cc7c4e6 的真实元数据/归档与 history 录制器见 `src/pilotdeck-gateway/tests/fixtures/native-archive/README.md` 和 metadata 原件。旧归档整读 artifact 后按 maxBytes 切片，不能因新增 64KiB marker 预算而截断合法 artifact。此前原生归档 marker 已经走 RegistryFiles→archive_read 的安全 descriptor，不把“它仍直接 NativeFiles::read/FIFO必挂”误报当事实。配置 FIFO 与普通历史 FIFO 的实际风险、已知真基线证据沿用 D2-CLI-CONFIG-READ-BOUNDS-001 和 D2-NATIVE-HISTORY-BOUNDS-001；本条没有完成 TS 所有新资源阈值实采。

## Rust 行为

list_projects/describe_project/project_files_list/project_file_read/native_archive_manifest/native_archive_entries/native_archive_artifact 通过四槽 blocking worker，取消请求不提前释放仍运行的 worker 名额。项目 marker 每文件最多64KiB、请求 marker总量16MiB，目录枚举总量16384并缓存已枚举目录避免重复收费；摘要保留原head/tail/stream fallback算法，独立请求总读取预算为 TRANSCRIPT_MAX_BYTES + 2×LITE_BYTES。归档 marker 安全打开仍限制在归档home之下；artifact与transcript分别保留原有安全打开及正常错误映射，最多读取TRANSCRIPT_MAX_BYTES+1，超50MiB显式拒绝。artifact≤50MiB仍报告实际全长并按原maxBytes返回base64前缀，不误用marker上限。

## 范围

只限新原生宿主上述入口，不改变旧Snapshot/NativeFiles通用metadata真基线或冻尺。Unix普通文件和同fd校验不保证中止故障文件系统中的内核IO；其它cold session读写由其实际实现单独核验。四个slot不是整个进程RSS上限，也不是旧TS精确性能承诺。

## 机器判据

`native_project_reader_fifo_subprocess` 真实handler读者在子进程期限内完成；`large_summary_and_many_projects_remain_readable` 用真实17MiB head/middle transcript与128注册项目验证兼容；`archive_markers_and_artifacts_use_distinct_bounded_reads` 验100000字节artifact的实际长度/截断、50MiB+1稀疏artifact拒绝及65537字节marker拒绝。旧archive全矩阵须继续通过；新项在mbp2编译实测后收证，不因本登记宣称通过。

## 批准记录

AI代批，待人复核；候选差异，未作已接受差异或阶段验收结论。仅修产品资源故障，不改变冻结装置和评分语义。
