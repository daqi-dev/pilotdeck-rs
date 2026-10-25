# 私有 CA 信任来源（#682）

固定基线 cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85，Node v22.22.0，macOS Homebrew 构建。
`oracle.json` 记录来源 × 四客户端 × hostname 矩阵与源码闭包哈希；同目录 JSON 是每个真实 TS 子进程 stdout/stderr，只替换临时目录和端口，保留错误与 warning。

从仓库根重采到**新的空目录**（不得覆盖本次原件）：

```sh
PDRS_BASELINE=/absolute/path/to/PilotDeck \
node --import /absolute/path/to/PilotDeck/node_modules/tsx/dist/loader.mjs \
  src/pilotdeck-gateway/tests/fixtures/private-ca/generate.mts \
  _tmp/YYYYMMDD-private-ca/oracle _tmp/YYYYMMDD-private-ca/work
```

依赖本机 Node、OpenSSL 源码安装版；不下载二进制。`server.mjs` 在临时目录现场生成两组 RSA 测试 CA 与叶证书，2 天有效期，只监听 127.0.0.1 / ::1，叶 SAN 仅 127.0.0.1；正控连 IPv4、名称负控连 IPv6。代理只放行同一夹具 IPv6 HTTP 目标。私钥不入库，Rust 测试结束回收其自有 Scratch 目录。探针服务器持有 socket 集合，finally 关闭；socket 5 s，子进程 30 s 截止。

实测要点：extra / bundle / partial 可连通；bad-first / bad-der 保证遇错停止；cached-file 再建客户端访问全新端口证明不重载（不复用旧连接/会话）；四类 HTTPS 都拒绝错误 hostname。count 仅 `source=provider` 计成功，model 返回实际规范化应答。`config` 将 ca 加入实际配置/入口，证实不是有效来源。

本机默认 SSL_CERT_FILE 可用，不依赖 system 开关；强制 bundled 后不读取此文件。OpenSSL hash 目录可用，普通 ca.pem 命名无效。未查看/修改宿主信任库，不把这些结果解释成已验证系统钥匙串。
Rust 尚未对齐的来源与启动时机见 `specs/gateway-parity/diffs/A6.4-TLS-CA-SOURCES-001.md`，未批准。

新增直接依赖 `webpki-roots =1.0.9` 已由 hyper-rustls 锁定，许可证 CDLA-Permissive-2.0；目的为在原 Mozilla roots 上追加显式 CA。未引入新版本、新二进制或原生信任库依赖；产品/probe 锁文件同步。
