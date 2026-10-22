#244：真实 Node mkdir 的祖先文件、叶子文件、超长分量、新目录、已存在目录，以及真实 FileRunRegistry.accept / UploadStore.create 失败路径。源文件及固定 commit 在生成前后校验；错误路径仅将本轮临时根替换为 <dir>。共享实现沿用原 skills 的 Node MKDirp 语义，本批没有新建只读卷，EROFS 分支未重新实测。

Node 22.22.0，固定基线 `cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`。生成命令（仓库根执行，输出目录自定）：

```sh
PDRS_BASELINE=/absolute/path/PilotDeck node --import /absolute/path/PilotDeck/node_modules/tsx/dist/loader.mjs \
  src/pilotdeck-gateway/tests/fixtures/mkdir-boundary/generate.mts /tmp/pdrs-mkdir-boundary
```

四组金样均由 `tests/p2_sweep_boundaries.rs::boundary_fixtures_regenerate_twice_from_real_ts` 再生成两次并与入库字节比较（需显式运行 ignored 测试）。
