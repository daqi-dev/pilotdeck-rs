# exp011 · F2 双前端对拍骨架首跑

- **假设**：同一 mock 后端下，旧前端（基线 `ui/` 完整 Vite 构建）与新前端（`ui-rs/probe-leptos` 占位）能被同一 runner 双跑，并对基线 72 例做防 skip 对账。
- **做法**：协调者沙箱外按 `scripts/fe-parity/README.md` 构建两端（wasm-bindgen 0.2.129 从源码 `cargo install`），`--network-self-check` 后 `--suite skeleton --frontends old,new`。
- **结论**：网络自检 PASS；骨架 3 例两端均 PASS（登录 token 持久化 / 认证项目请求 / 主 WS 认证打开）；基线 72 例按设计 NOT_RUN，总 exit 1（预期）。
- **改变了哪个取舍**：无——runner 可用，F2 后续 PR 接入流式 / 滚动黑盒断言与 37 例候选。
- **边界**：新前端仍是探针页，不是生产页；只跑了骨架 3 例。原始输出在 worktree `_tmp/fe-parity/coord-skeleton/`。
