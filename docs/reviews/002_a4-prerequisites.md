# A4 前置 #45 / #40：审计与重登记交接

范围：ruler/a4-prereq @ 8d39376 的未提交变更。用户授权按「另开 issue + 两家评审」修改冻结尺子实现；FROZEN.md/schema-FROZEN.md 保留，由协调者在两家同意并正式校准后登记。作者 Codex/GPT；未改 state/，未提交，未起服务。

## simplify 四视角

| 视角 | 审计结论 / 采纳 | 证据 |
|---|---|---|
| 复用 | 沿用现有 compare、Ajv、Python checker、真实 archive controls；不造第二个协议 schema。新离线控制入口供 run_all/check_ruler 复用 | controls/prerequisites.py、controls/run_all.py；_tmp/audit/sibling.md |
| 简化 | 具名实体保留稳定键冲突拒绝；不引入首现序/列表位置消歧。clock 只认两种基线组合，完整 $ref 指针独立小函数 | compare.py:101、check_schema.py:134；_tmp/audit/boundary.md |
| 效率 | 静态 prerequisites 可单独离线跑，避免起服务；--check 仍双生成证明稳定性，原 controls 只复制原件到 worktree | run_all.py CLI、prerequisites.py；_tmp/audit/concurrency.md |
| 层级 | A2 apparatus 白名单与 A3 prerequisite 输入清单分开绑定；A2 不再扫描 node_modules/A3，新增依赖判据有独立 sha256 和起终检查 | common.py:A2_SCRIPTS；prerequisites.py:inputs；check_ruler.py:prerequisiteSha256 |

## pr-ready 三角色 / invariant audit

| 角色 | 不变量 | 结论 / 处理 | 证据 |
|---|---|---|---|
| sibling | 18 payloadAliases 都有独立 pass/fail 例；Python/Node roots 镜像不退化 | 已补14个别名28例；15正帧+16负帧+45 definition = 76，不再引用旧48/49误计。Node重复alias检查镜像仍为P3留白 | fixtures JSON；validate_fixtures.mjs；_tmp/audit/sibling.md |
| concurrency | 输出不能认证混版本依赖；临时资源失败路径释放 | A2 hashes保持白名单，A3结果另绑11输入并起终校验，check_ruler完成前再查；导出器配置写入移进try，success/mismatch/write-failure三条实际清理对照 | common.py / prerequisites.py / check_ruler.py / export_schema.mjs；_tmp/audit/concurrency.md |
| boundary | 空集/错型/非法expect不可绿；错误ref不可只按末段通融；业务时间和路径精确 | 卫生检查先拒绝，再判正负；各alias移除覆盖/开放schema均FAIL；单marker六种时间差异FAIL，真实元数据保持正例；named ambiguity与路径UUID片段行为断言 | _tmp/audit/boundary.md；_tmp/offline-controls-certified/results.json |

三角色是 pr-ready 按技能派出的作者家族补充审计，不能代替下列两家非作者谱系批准。初轮命中的控制证据缺陷、A3哈希绑定、tsconfig写入清理均已修；timeout最终失败报告及Node alias重复镜像为P3留白。报告原件在本worktree的 `_tmp/audit/`，协调者归档后再提交此审计档案。

## 双谱系独立评审状态

GLM已完成，明确同意冻结尺子变更、independent_review:true，无P0/P1；原件 `_tmp/review-glm-final.md`（元数据证明zai-coding-plan/glm-5.3，CLI600秒超时后只读export取回最终答复）。其三项P2证据/登记卫生已在RESULT、最新提示词及工件索引闭环；冻结登记仍留协调者。DeepSeek Harness返回QUOTA: Insufficient Balance；已配置Anthropic与Kimi通道均发生TLS错误（原始日志在_tmp/logs/），未得到第二家同意，不将作者GPT自审计入两家。OpenCode只连接既有127.0.0.1评审服务，不启动新监听，不禁用证书校验。最终结论与报告路径在_tmp/RESULT.md更新。

## 验证边界

只用历史 TS/proxy 录帧原件作离线回归，不改写 run.json/证据哈希，不声称本版本已通过正式TS五遍或Rust代理校准。新版哈希与冻结账本必然不匹配；这不是可自行绕过的闸门。协调者需使用新的output重跑 check_ruler，执行两家批准后登记；确切命令、sha256和原始输出见 _tmp/RESULT.md。
