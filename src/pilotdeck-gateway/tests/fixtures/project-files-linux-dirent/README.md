# #133 Linux 非法 UTF-8 dirent 真实对拍

macOS 卷上创建非 UTF-8 文件名会得到 EILSEQ，函数级证明（`collation/
invalid-utf8-node22.json` 的 22 序列表 + `node_dirent_name` 单测）无法落到真实
目录。本 fixture 在 Linux（cb8，Ubuntu 24.04 aarch64，源码编译的 Node
v22.22.0）上补齐端到端验收：

- `record.mts`：离线 Node 探针。argv 给定钉死基线（cc7c4e65）、隔离 home、
  project root、requests.json、输出路径；加载**真实 TS 源码**
  （`GatewayWsConnection` + `InProcessGateway.projectFilesList` +
  `createDialogProjectRegistry`）跑全部请求，记录 wire 响应；另记
  `fs.readdir(root, {withFileTypes: true})` 的原始视图与字节序排序视图
  （`Buffer.compare` 稳定排序，镜像 Rust 的解码名 UTF-8 字节排序）。探针先自检
  Node/ICU/locale/基线 HEAD，不符输出 `BLOCKED` 并以 3 退出——环境错不是对拍失败。
  输出原子发布（tmp+rename）。
- `src/pilotdeck-gateway/src/project_files/tests/linux_dirent.rs`（门控测试，
  由 `tests.rs` 末尾 `mod linux_dirent;` 挂载，`#[cfg(target_os = "linux")]`）：
  env 门 `PDRS_LINUX_DIRENT_CHECK` 未设早退；`PDRS_BASELINE` /
  `PDRS_LINUX_DIRENT_OUT` 缺失带专属消息失败。流程：locale 必须 en-US
  （提示 `LANG=C.UTF-8`）→ `OsStringExt::from_vec` 建原始字节树（固定小数
  mtime 1700000000.123456）→ **raw 字节自检**（`read_dir`+`OsStrExt::as_bytes`
  与意图逐一相等，不依赖被测 lossy）→ Rust 侧先跑请求矩阵（含 cursor 回放）→
  写 requests.json → 有界 spawn 探针（进程组 + 超时杀组）→ 两层对拍 →
  原子写 receipt → 清理并断言树消失。
- `negative_controls.py`：4 个变异（strict-decode / reverse-dirent-order /
  flip-directory-type / reverse-wire-sort），全部绑定单行可识别断言，全部跑完
  后汇总。锚点计数：字节排序锚在 `native_entries` 的 unix 与非 unix 两份实现
  各出现一次（共 2），只替换第一处。
- `run-linux.sh`：cb8 总控（见下）。

## 两层对拍

**dirent 层**：`native_entries(root, root)` vs Node `readdir` 的
`byteSorted`——名字分组（多个原始名解码成同一显示名）、file/dir 类型、字节序
顺序。非法名目录的子文件两侧都到不了（`open_checked` 按解码名分量走 NOFOLLOW
遍历，`realpath` 失败即跳过），所以 dirent 层只对拍 root 一层。

**wire 层**：5 个请求（空 query、includeDirs=false、query=U+FFFD、limit=4、
cursor 回放）走 dispatch 真 wire，逐字段比较（数值按 f64：V8 对整值 mtimeMs
不加 `.0`，serde_json 加）。TS 侧 `realpath(absolute).catch(() => undefined)`
失败即 `continue`——lossy 解码不回环的名字（如 `\xff`→U+FFFD→`ef bf bd`）被
**两侧一致跳过**，所以 wire 输出只含合法名；「非法名被一致跳过」本身由
expected-listed 断言与 wire 全量比较共同验证。

## 顺序论证（不预设 OS 顺序）

- readdir（getdents64）顺序两侧各自不同也不影响：两侧各自稳定排序。Rust
  `sort_by` 与 V8 `Array.prototype.sort` 都稳定；相等键（同名组内）保持各自
  readdir 顺序，但组内成员是**相同字符串**——名字序列对组内先后不敏感。
- 名字分组/类型断言用 multiset（`BTreeMap`），天然与顺序无关。
- wire 层无同名组（非法名被跳过，列出的 relativePath 全不同），完全确定。
- 排序反转变异（比较器交换 a/b）确定性被杀：不同名离开升序字节序即断。

## 负控绑定

| 变异 | 首个触发断言 |
|---|---|
| strict-decode（lossy→strict，非法名塌缩为 `""`） | `#133 dirent name grouping differs from Node readdir` |
| reverse-dirent-order（字节排序比较器反转） | `#133 dirent byte-order sort differs from Node readdir` |
| flip-directory-type（d_type 目录标志取反） | `#133 dirent file/dir types differ from Node readdir` |
| reverse-wire-sort（最终 collator 排序反转） | `#133 wire item order differs from Node` |

预期 lossy 分组（`String::from_utf8_lossy` 由 raw 规格推导）是额外的正向自检，
其负控即 strict-decode 变异（同一解码路径）。

## cb8 运行

```sh
CARGO_TARGET_DIR=<caller-owned-build-tree> \
  ./src/pilotdeck-gateway/tests/fixtures/project-files-linux-dirent/run-linux.sh \
  <pinned-pilotdeck-ts-checkout>
```

先打印并断言 `process.versions.icu == 78.2`（不符报 BLOCKED 退出）、`LANG=C.UTF-8`
解析为 en-US；随后 `cargo test --no-run`、#133/#117 正控、#117 生成器 `--check`
（`generated.rs` 差异=真差异；仅 `errors.json` 平台字段差异=预期）、两套负控。
负控要求提交态（`mutating_sources` 拒绝 untracked 的 `src/pilotdeck-gateway/src`
文件）——提交前运行会标记 SKIP，由协调者在 cb8 提交后重跑。

## 残余风险

- `os.constants.errno` 不含某名字时（Linux-only ENONET/EREMOTEIO/EUNATCH 在
  Node 常量表中的覆盖未在 macOS 侧证实），数字核对走 libuv 取负方向（该方向
  覆盖全表，不缺口）；cb8 采样记录 `errnoConstants` 数量作证据。
- mtimeMs 数值比较按 f64（1700000000123.456 这类可精确表示的值），不做字节
  比较——两侧序列化格式对整值不同属已知差异。
- 同名组（dup-FF/dup-FE/dup-80）解码后字符串相同、内容不同：Layer A 的
  顺序断言对组内先后不敏感（设计使然，不预设 readdir 顺序）；wire 层这些名字
  被跳过，不引入不确定性。
