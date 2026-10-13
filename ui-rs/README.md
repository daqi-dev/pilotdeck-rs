# F1 前端探针

这里只放可抛弃的实验页：登录、项目/会话请求、流式帧计数、composer IME 事件、xterm 岛。没有迁移设置/聊天/业务页面。`probe-leptos`、`probe-dioxus` 都是独立 Cargo workspace，锁文件各自保留，不加入 `src/` 的 CI workspace。

探针壳和业务状态在各自 Rust `src/main.rs`；协议与 typed wasm-bindgen 岛接口在 `shared/core.rs`。`shared/island.js` 只负责第三方 xterm DOM/WS 适配与保留实例；`shared/boundary.js` 是双壳导航实验注入件，没有接入生产旧壳。

完整版本、命令、判据、实测数据、验证缺口见 [exp007](../docs/experiments/exp007-frontend-probe/readme.md)。

```sh
# 在 worktree 根执行，构建产物都留 _build，日志都留 _tmp/exp007。
python3 scripts/exp007/build.py --bundlers
node scripts/exp007/gates.mjs leptos
node scripts/exp007/gates.mjs dioxus
# 可启动系统 Chrome 的环境补真实滚动/绘制检查；不下载 Playwright 浏览器。
node scripts/exp007/browser.mjs leptos
node scripts/exp007/browser.mjs dioxus
```

两候选按顺序跑，复用 127.0.0.1:28370，不能和 `coexist.mjs` 同时跑。Playwright persistent profile 一律在本 worktree `_tmp/chrome-profile-*`。基线源路径可用 `BASELINE_UI` 覆盖；必须指向只读的基线 ui/（已有 node_modules）。
