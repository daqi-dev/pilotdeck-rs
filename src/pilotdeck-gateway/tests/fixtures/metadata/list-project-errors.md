# list_projects 真实目录失败原件

`list-project-errors.json` 复用固定 Node 22.22.0 原件，真实
`GatewayWsConnection → listWebProjects` 的 ENOENT / ENOTDIR / EACCES 与空目录
正控均返回 `ok:true, result.projects:[]`。录制只注入无监听 transport，fs 为
真实目录操作；源码 `src/web/server/listProjects.ts:29` 起对 readdir catch 后
采用空集合。脚本、命令与成功原始日志位于主仓
`_tmp/20261016-debt-cleanup/gateway-audit/list-project-errors.mts`、
`list-project-errors-retry-runner.json` 和 `list-project-errors-retry.log`。
本批 `catalog/source-binding.json` 绑定两处源码与复用 fixture 的 SHA-256。

Rust 测试 `dispatch::tests::list_projects_preserves_real_node_scandir_error_suppression`
重新创建四个自有目录场景，用 NativeFiles 逐项确认真实 errno（非 root），
再通过公开 dispatch 比较完整 response 对象。root若绕过chmod，明确输出
`PDRS_UNAVAILABLE real-EACCES`并只把该分支记作真实权限不可达；EACCES录制
错误replay在所有宿主仍必跑，不能把root结果记为真实EACCES通过。权限通过 Drop guard 恢复，
临时目录只删除本次独占新建的根。保持既有吞错行为，不扩大 errno 白名单。

```sh
cargo test --manifest-path src/Cargo.toml -p pilotdeck-gateway list_projects_preserves_real_node_scandir_error_suppression --locked --offline
```

此批只覆盖 #120 的前两项；账本容量与调用链仍 OPEN，Linux errno 实效不在
本批完成声明内。

DeepSeek同类检查发现的 describe_storage/chat_dir/summary 真实错误入口原件
仍未完成；对应TS describeWebProject/resolveProjectStorageDir/listProjectSessions
路径不同，本批空projects四例不替代这些原件。该P2前置A4/#46收尾；Linux
errno适用面仍按#117单独验实效。
