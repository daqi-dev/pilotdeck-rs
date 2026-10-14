# exp007 独立前缀路由器

Issue #43 的方案 B 适配层。`scripts/exp008/proxy/src/main.rs` 只支持单一上游；整个 exp008 已列入 B2 `specs/ui-server-parity/FROZEN.md`。本组件独立放在 exp007，不改、不复制冻结装置的 WS 实现。默认入口仍启动仓内 exp008 proxy 子进程，前缀选择在它之前完成。

```text
127.0.0.1:28375 prefix-router
  /probe、/probe/*、/shared、/shared/*、/vendor、/vendor/* → :28370 静态探针/mock
  其余 → :28373 exp008 proxy → :28374 原 Vite
                                  /api、/ws、/shell、/plugin-ws/*、/__probe/* → :28370 mock
```

API/WS/SSE 经默认旧入口链路，不由前缀路由器解释业务消息。实验以 Vite 的内置代理模拟其背后的 ui/server；真实生产部署可将 exp008 上游改指 ui/server。Leptos/Dioxus 产物由既有 `mock.mjs` 静态路由读取，本组件本身不供文件，也不提供 TLS、管理端口或静态 SPA fallback。

配置均显式设置：

| 变量 | 含义 |
|---|---|
| `EXP007_LISTEN` | 默认 `127.0.0.1:28375` |
| `EXP007_PREFIX` | 默认 `/probe`；允许 `/settings` 等单段字面前缀；无尾斜线 |
| `EXP007_OLD_UPSTREAM` | 必填，本实验为 `http://127.0.0.1:28373`（exp008） |
| `EXP007_NEW_UPSTREAM` | 必填，本实验为 `http://127.0.0.1:28370`（静态服务） |

监听和两个上游只允许 `127.0.0.1`、端口 ≥20000，启动编排先检查 28370/28373/28374/28375 均空闲，随后所有 bind 严格失败，不选替代端口。`/probex`、`/settingsx` 不匹配，保留原路径与 query，重写 Host 为所选上游，移除所有 Connection 指定头及逐跳头，保留 Authorization/Cookie/WS 子协议头。HTTP 请求与响应 body 逐块转发；WS/HMR 等 Upgrade 交接后使用原始双向字节隧道，不生成 Pong/Close、不补 Date。完整连接的 WS 语义仍由冻结 exp008 处理。

从 worktree 根执行（仅从源码构建工具与程序，无浏览器下载）：

```sh
sh scripts/exp007/build-coexist.sh
node scripts/exp007/coexist.mjs
# 协调者在可启动本机 Chrome 的沙箱外补跑：
node scripts/exp007/coexist.mjs --browser
```

runner 自动给 Vite/mock/Rust 子进程隔离 HOME、XDG、TMPDIR、PILOT_HOME，设 `PILOTDECK_SKIP_BROWSER_OPEN=1`，过滤继承的部署/代理/录制变量。`--browser` 只使用 Playwright `channel: chrome`，真实 HOME + 新建临时 profile（R12），TMPDIR 仍隔离。只终止自身创建的两个 Rust 子进程；同时关闭 Vite/mock/Chrome，超时清理判 FAIL，并检查端口释放。证据写 `_tmp/exp007/coexist.json`，失败同样保留已完成 checks 与错误。

离线单测无需绑定端口或启动浏览器：

```sh
export CARGO_HOME="$PWD/_build/cargo-home"
export CARGO_TARGET_DIR="$PWD/_build/exp007-target"
cargo fmt --manifest-path scripts/exp007/prefix-router/Cargo.toml --check
cargo clippy --locked --all-targets --manifest-path scripts/exp007/prefix-router/Cargo.toml -- -D warnings
cargo test --locked --manifest-path scripts/exp007/prefix-router/Cargo.toml
node --test scripts/exp007/coexist-support.test.mjs
```

Rust 单测覆盖两种前缀的段边界/query、升级与鉴权头、逐跳头、非法地址/头拒绝、内存双工隧道的二进制/control 字节及半关闭、SSE 首块在上游结束前可读。完整网络实验验证真实 Vite index、两份编译 wasm 与源产物 byte equality、三类 WS echo/Pong/Close/子协议/鉴权、SSE 分块。浏览器模式验证 history 原语双向整页跳转及 storage 保留；实际 React `navigate`/`Link`/`NavLink` 的拦截调用仍由既有 `navigation.mjs` DOM 实验独立验证。未将 mock PTY/plugin、DOM 硬跳 recorder 或 history 原语当生产端到端验收。
