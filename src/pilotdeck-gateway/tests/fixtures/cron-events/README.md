# Cron event oracle

固定TS cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85，mbp2 Node22.22.0 / ICU78.2。capture.mjs使用已安装TypeScript.transpileModule（纯JS、不fork）在内存加载真实CronFire/CronTaskStore/CronSchedule/Timezone/Paths；只对未调用的createProjectId依赖设throw桩。主oracle23格，另scheduler-oracle3格覆盖同一UTF16 taskId的延期、once重算、cron v2 day-OR重算。数据/阈值先于Rust执行固定；没有按Rust结果改oracle。

每格独占目录，store真实写文件；初始task文本、完整调用/phase/turn事件/result/release/warn、active/时钟/错误和所有after文件正文保存。没有随机路径/时间事后归一化；clock通过公开deps固定。所有临时文件必须清理，after枚举不忽略.tmp。JSON字符串中的UTF16和文件文本逐字段比较；Rust测试不使用serde_json来吞lone surrogate。

实际原件 /Users/da/ws/pdrs-worker-wt/control-cli-20261108/_tmp/cron-event-sol-oracle-20261108-1700：capture/（23格），reproduction/（同原oracle逐字cmp相等），capture-scheduler-measured/（3格）。原始stdout/stderr/exit均保留。主capture和重复采集exit0/stderr空；附加scheduler首次driver生成UnicodeEncodeError产生空脚本，其后的Node0无oracle是无效采集，不计通过；下一次脚本引用丢失产生SyntaxError。空/错误脚本及输出都留原目录，最终measured真正产出3格且exit0/stderr空。远端脚本正文统一经shell单次完整quote，不把含单引号的源拼进SSH单引号命令。

Seatbelt deny-default、network全部拒绝、fork拒绝；只允许固定Node、固定baseline/依赖只读、独占scratch写。网络正/负控network-check.json：同一自有loopback监听，sandbox外接收一次probe；sandbox内真实net.connect返回EPERM，接收数仍1。没有真实provider/IM请求。HOME/PILOT_HOME/cwd均隔离；复采git SHA预检另设GIT_CONFIG_NOSYSTEM=1/GIT_CONFIG_GLOBAL=/dev/null。没有真实home产品读取/写入，也没有改baseline。

复现：在mbp2新建专属目录，把sandbox.sb内唯一scratch前缀替换为该目录（保留deny规则），设PDRS_CRON_SCRATCH、PDRS_VERIFIED_TS_SHA、HOME/PILOT_HOME/TMPDIR为新目录，LANG=en_US.UTF-8，使用固定Node和sandbox-exec运行capture.mjs <scratch>/capture；运行capture-scheduler.mjs <scratch>/scheduler。脚本验证Node/ICU/verified SHA，sourceSha256在oracle内。需要固定baseline和既有typescript；不下载任何二进制/依赖。Rust正常测试只读已提交oracle，不依赖Node或sandbox。
