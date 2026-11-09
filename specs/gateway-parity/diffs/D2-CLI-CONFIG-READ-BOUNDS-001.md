---
id: D2-CLI-CONFIG-READ-BOUNDS-001
---

## 类别

避免配置读取挂起和无界分配的安全修复。新CLI在Unix显式选用有界配置facade；只读runtime binding接线复用现有逻辑，资源拒绝不伪装为空配置或成功。

## 基线依据

固定`cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`的`src/pilot/config/loadPilotConfig.ts`读取global/project YAML，`src/pilot/staffdeck/projectBinding.ts:44-64,120-145`读取binding store/SOP/lock，使用同步文件读取且没有本片字节预算。实际TS loadPilotConfig隔离探针：store无writer FIFO在LOAD_BEGIN后3秒仍挂起，由外部父进程杀收；8388609字节合法普通配置可加载成功，两例原件metadata/字节保持。真实30格runtime-binding原件和资源探针位于主仓任务目录，复现代码在`src/pilotdeck-cli/tests/fixtures/config-binding/`。Node固定v22.22.0/ICU78.2，fetch禁止，不访问StaffDeck外网或用户目录。

已有Gateway的`NativeFiles::read`同样直接打开并read_to_end；本片不改这个既有读器或两个旧loader入口，避免扩大共同作者范围/旧Gateway语义。删除CLI旧无条件binding TODO本身是实现真实只读加载，不意味着provisioning或实际知识服务已经交付。

## Rust 行为

新增`config::load_project_bounded`和`load_snapshot_bounded`复用同一既有inner。Unix普通文件通过O_NONBLOCK打开后以同fd判断类型，单文件最多8388608字节，一次加载总计33554432字节；limit+1检测文件增长，部分读和检测字节全部计费、不减计/重置。预算耗尽后拒绝下一read，即使它可能是空文件。首个typed资源fault之后read/stat/entries不再delegate，process_alive返回false，不继续源IO；清理已开fd不受此限制。

拒绝精确为CONFIG_READ_NOT_REGULAR、CONFIG_READ_LIMIT或CONFIG_READ_BUDGET，requires_host=false；只对该typed资源拒绝覆盖旧loader结果，普通IO保持既有诊断次序和requires_host。默认CLI在Unix选新project API；非Unix继续原load_project，不新增未经验证的Windows读器。新snapshot API供新增CLI/TUI入口选用，注册由协调者串行，不影响既有Gateway snapshot。

## 范围

仅对明确选用的新API/Unix入口提供此资源保证。配置来源包括global/project YAML、StaffDeck binding store、SOP及lock；允许symlink，但检查实际打开的同fd。未修改旧Gateway入口、metadata或binding实现，非Unix新API明确unsupported；Windows本轮未编译/实测。读入字节限制不等于全部解析对象/目录枚举的内存上限，不承诺中止故障文件系统内核IO或与其他写者的事务快照。CLI check丢弃Config，远端Gateway并不因此接收/采用本地modules。

## 机器判据

真实TS金样先于Rust生成：30格普通/绑定/错误/活死锁逐格调用原loader、文件树hash不变、零fetch；另复用已有14格enabled/type/prototype冻结事实，旧原件不改。Rust测试对比check完整错误、runtime modules JSON、snapshot旧API全值与输入原件。资源父进程5秒期限保护真实FIFO与超限稀疏文件子测试，双新API均拒绝且不改原件；实际4次8MiB读取后达到同一32MiB预算，后续read/stat/entries拒绝、活PID不查询，计数不下降。

真实CLI默认远端分支测试只在mbp2的独占18789 lease/mock运行：坏store/YAML/SOP/URI错误先于连接；普通binding/缺失/死锁样例到真实hello/submit通道；503保留原本地fallback TODO。所有子进程/网络有界，不接本机18789或真实用户服务。最终运行日志/源码hash由协调者集中归档；未运行不报通过，真实StaffDeck与人眼实效另验。

## 批准记录

**AI代批，待人复核**，按现有默认安全修复授权及协调者明确确认8MiB单文件/32MiB累计、只限新CLI opt-in执行。**未批准，非已接受差异**，不提供冻结评分豁免，不改阈值、分母、A8前置或阶段完成状态。决定见`state/decisions-20261108-control.md`；本轮仅本机提交，不推送GitHub或开PR。
