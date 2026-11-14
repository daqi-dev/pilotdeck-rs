#!/usr/bin/env python3
"""check-pr-gate.py — 合并闸门第 2、3 步的机器检查：PR 描述里必须有角色审计结果块（或写明理由的豁免）与 issue 关联。

只查「做没做、做全没做」，不查结论对不对（那是双谱系评审的事）；结果块可被手写伪造，本脚本防的是**漏跑**。

规则：
  1. 有 `<!-- pr-gate:v1 ... roles=a:S,b:S,... -->` 结果块（scripts/run-review-roles.py 生成），
     且 sibling / concurrency / boundary / simplify 四个角色都在、都是 PASS；
     或有 `<!-- pr-gate:waiver <理由≥8字> -->`（纯文档/纯状态 PR 用，理由会打印出来供评审看）。
  2. 有 issue 关联：`_ISSUE_KEYWORDS` 单源词表（fix-r17：不再手抄第二份窄枚举——旧文案
     `Closes|Fixes|Resolves` 比 `_ISSUE_KEYWORDS` 少 Closed/Fixed/Resolved 等屈折，第三份手抄
     漂移面，MM r15 sibling#2；对照规则 4 的 `_TYPE_WORDS` 同款处理）+ `#N`：关单形
     （Closes/Fixes/Resolves…，合并即关单，GitHub 定顺序）与 `Refs` 形（一单多 PR，单保持开）
     机检同收（语义区分在人，本脚本只认「有关联」），或 `<!-- no-issue <理由> -->`。

  3. 结果块 head= 须等于 PR 当前 head；不等时，只有块里的 patch=（审计时 `git diff base...head` 的补丁指纹）
     与当前 PR 补丁指纹（`git diff <PR base>...<PR head>`）相同才沿用——纯 rebase、内容逐字不变的提交链
     不必重审（同 Gerrit trivial rebase 保留评审）；任何一行不同、上下文变了、或算不出指纹，都按旧提交拒收。

  4. 修复轮字段（#3056，长尾收口卡「机检落点」；fix-r6 主信号源扩为集合；fix-r5 修 2/4）：
     **主信号 = PR 范围提交标题集合**（--base/--head/--repo 齐给时 `git log --format=%s
     <base>..<head>`，head 提交天然包含在集合内，不单独走一条路）——对集合内每条标题套同一
     判据：类型词（`_TYPE_WORDS` 单源词表，fix-r15：不再手抄枚举——DS r14 sibling#3 /
     MM r14 sibling#2 + boundary#4 同撞「第三份手抄漂移面」）开头 + 可选
     `(scope)`/`!` + 冒号〔半/全角〕或空白分隔 + 行内 `fix-r<N>`（ASCII 词边界）；任一标题
     命中即判为修复轮 PR，**每条命中标题的轮号按配对判定**（fix-r5 修 2，boundary#2 P1）：
     token 前方最近的 `#N`（由 +/,/、/空白链成的 #N 串）与本 PR 关联单号（正文 Closes/
     Fixes/Resolves/Refs #N 的 N）有交集才计入——标题里引用**别的单**的轮号（board 同步尾
     `chore(state) board …#2691 fix-r2…#2692 fix-r8 在跑`）按 issue 号各归各的单，不再
     「首个」（本单轮号不在首位会**低估**：跳过 N≥4 的 residual 必填＝假 PASS）也不「行内
     max」（引用轮号更大会**高估**：多要 residual＝假 FAIL）；无 #N 的标题全 token 计入
     （宁高勿低，fail-closed）；token 前方无 #N 且在题头区（首个全角「：」之前）也计入
     （`fix(scope) 名 fix-rN：…（#refs 在后）` 的实修形态）。**再跨标题取 max**（同一分支
     会累积 rN→rN+1 多个修复提交，当前轮=真实修复提交的最大轮）。fix-r5 只看 head 单标题时，
     head 提交标题不带 `fix-rN` 的修复轮 PR（多提交轮的尾提交常是 `chore: sync board` /
     typo / merge 提交，`specs/README.md:59` 提交规约并不要求每个提交带 fix-rN）被判非修复轮，
     fixtype / residual / 多块一致整套跳过 = 假 PASS。scope / 时间戳 / `PR` / `#N(+#N)*` 等
     元素只是前缀后的可选普通文本、**不设间隔上限**（词表里 #N→fix-r 间隔最长 208 字符，
     设 ≤20 有界间隔会把 17 条真实形态判漏）。正文回显不是任何路径的主信号（行内词表判据
     r2→r3→r4 三轮不收敛，全量词表证明「结构化标题」与「类型词开头的散文」两群在行内无判别量，
     见 state/decisions-20261113.md「#3056 fix-r4 修 1 停点裁定」），**别再往正文正则里加词**；
     正文只作**兜底**：主信号**缺失**的两形态同走「首行宽松 + 全正文结构化回显扫描」
     （fix-r14，MM r13 sibling#2）——①缺件：--base/--head/--repo 任一缺失（本地
     `--body-file` 不传 head 的路径——缺件先打 stderr 显式告警说明主信号缺失原因，不静默
     降级，fix-r7，返回 None）；②空集合：base==head 合法空集（无提交可判）**或**范围内提交
     subject 全为空（过滤后空列表，返回 []）。首行宽松（fix-r1
     P0 的 CJK 连写首行「修复fix-r5：」靠它）不命中再扫**全正文结构化回显**（`fix(...) #N
     fix-rM` 形态：类型词 + (scope) + 冒号（可无）+ #N + 就近 fix-r，#N 配对到本 PR 关联单才
     计入——scope 括号 + #N + 配对三重结构与散文分开，fix-r5 修 4，boundary#4：本地过闸是
     硬步骤，「## 概述」开头的正文不再判非修复轮）。回显扫描的三形态入口（fix-r3 P0③ 后的
     单一口径）：主信号**缺失**的本地 / 空集形态与「集合在但全不命中」的 CI 形态**同走**
     「首行宽松 + 全正文结构化回显扫描」（代码同一点：fix_round_from_body(echo_scan=True)；
     旧「CI 形态只看首行」的描述自 fix-r3 P0③ 起作废）。
     git log 失败（git 起不来 / 非零退出）fail-closed 拒收，**不得当「非修复轮」放行**；
     空输出两分（fix-r7，DS r6 boundary#3）：base==head 是合法空集（无提交可判），head 是
     base 的祖先（--base/--head 传反 / base 是 head 的后代，`git merge-base --is-ancestor` 判）
     按坏范围 fail-closed 拒收写明原因——不得当合法空集落到正文首行兜底静默丢主信号。
     修复轮 PR 的每个结果块须带 `fixtype=<枚举式|结构性|不适用>`
     （字面三值，子类字样一律写「结构性」）；N≥4 另须正文恰一个 `<!-- pdrs-residual: <非空> -->`
     （无上一轮写字面值「无上一轮」，空白与占位符不算，围栏 / 引用块里的示例不算声明——口径与任务书侧
     check-taskbook-segments.py 同一正本）。豁免轮（waiver）的修法类型写在豁免标记内
     （`fixtype=<三值>`，值后须空格或结尾分隔），豁免理由长度不计 fixtype 词汇；标记内声明了 fixtype
     就查值（声明即查，是否必填看修复轮判定）。「围栏 / 引用块 / 缩进块里的示例不算声明」对
     结果块 / 豁免 / issue 关联 / 修复轮识别同一口径生效（同一份去围栏文本，修 8）。
     fix-r2 P0（DS+MM boundary 同撞）：「配对集为空 / 与 issues 不相交」不再只当「不计入」——
     修复轮标题题头区引用了本 PR 未关联的单号（no-issue 旁路 a / 异单关联旁路 b）时 fail-closed
     拒收（foreign_fixround_pairs），不再让 fix_round 落 None 把 fixtype / 多块一致 / N≥4
     residual 整段跳过而规则 2 照样放行。题头区外的 board 尾引用别的单仍是合法形态（fix-r5 修 2
     的各归各单语义不变）。已知窄窗（fix-r2 登记）：`fix(scope)：#N fix-rM …`（#N 在全角冒号
     之后）不在题头区、不进 foreign——与板尾裸号同款已知代价；`fix(scope)：fix-rM 修 #N` 形态
     由 DS boundary#5 的「向后就近配对」计入（#N ∈ issues 时）。
     fix-r3（MM concurrency#1 P0 + MM/DS boundary#1 双撞 P1 + MM sibling#1 P1 + DS boundary#2
     P2a）：① 标题侧兼容 `#N r<M>` 简写形（与 fix-r<M> 同一套配对判定；收集面＝**题头区内
     全部**——前方无 #N 的简写自 fix-r13 起不丢弃、自 fix-r14 起不 SELF，交后向臂要求后方
     真实链且与 token 同侧于题头区界，无链/链异侧且与 issues 不相交不计（fix-r15 起异侧
     相交照计入，窄窗登记见 _scan_pairs docstring）；
     归属＝与前方 #N 链**紧邻**——间隔恰为链尾裸号成员时视同紧邻（`_bare_member_connected`
     单源，fix-r10 起精确判定无距离上界）；题头区外的 rN 是评审轮引用，`fix-r3：r2 评审`
     不计，语料 423 条无一例外，
     _tmp/20261114-3074-fixr3/probe-corpus.txt §1b）；② 标题含 fix-r<M> 时不再强制类型词开头
     （`#3056 fix-r3：` 判得出；非类型词开头标题**只走配对分支**——无 #N 全计入与题头区 SELF
     两个免配对分支不给，`fix-r2 #2516 收尾` 仍不是修复轮）；③ CI 形态（titles 非空但全不命中）
     的正文兜底同样走结构化回显扫描，不只看首行——一个分支全部提交都用 rN 简写形时 fixtype /
     residual / 多块一致 / foreign 四项义务不再整段跳过；④ 题头区判据从「首个全角：」标点猜改
     结构判定：≥2 条互不相交 #N 链 ⇒ 枚举 / board 型标题整条不作 foreign 判定（MM 建议①），
     免配对分支上 token 前紧邻裸数字串（无 # 前缀）不计（「板尾裸号不计」对无全角冒号形同样
     成立）；⑤ fix-r 轮号词表认接续轮 b 后缀（fix-r11b 算第 11 轮，只认 b——fix-r5x 仍不是
     修复轮）并与任务书侧单源；⑥ fix-r / #N / rN 数字位界 {1,18}（兄弟尺子
     check-taskbook-segments.py:390 先例），超界 fail-closed 干净报错不崩溃。
     fix-r4（DS r3 增量评审 P0×2 / P1×2 + P2 顺手修）：⑦ 裸号口径收窄为「枚举分隔前邻」
     （裸数字前跳过空白后最近的非空白字符须是非词标点/全角符号，排除 ASCII `:` 与 `#`）——
     `worktree 60`（席位值）与 `01:45`（时间戳）不再被吞＝fixtype / N≥4 residual / 多块一致 /
     foreign 四项义务不再整段跳（P0-A，A/B 复算回退方向 7 条全回）；⑧ 枚举判定只看**题头区内**
     的链——题头区外尾部 #N 注记不再把异单失配的 foreign 封堵关掉（P1-C，fix-r2 P0 的
     no-issue / 异单旁路不复开）；⑨ 后向配对分支补同一裸号守卫（P1-D，board 尾裸号 + 后方
     #N∈issues 不再计入＝假 FAIL 封堵）；⑩ 裸号判定有界窗口（token 前 ≤64 字符）+ RSHORT
     紧邻判据单遍单调指针（P1-E，O(T·n) 切片面封堵）；⑪ 枚举判定去重 + 全体公共元素短路 +
     链数上限 200（P0-B，`#1；`×N 的 O(C²) 挂起面封堵，fail-closed 方向）；⑫ OVER 词表同步
     b 后缀（`fix-r`+19 位+`b` 不再静默漏判）、超长串早退保留既有 errs（P2-1 / P2-3）。
     fix-r10（DS r9 复审 P0 边界翻转，矩阵化修复——同处第三轮，规则强制矩阵）：⑬ 链尾
     裸号成员豁免改**单调指针摊还 O(1) 精确判定**（`_GapScan`，无距离上界无切片——
     fix-r9 的 `_BARE_WINDOW` 上界在 gap=65 时把成员误判为引用 → 豁免被否 → 整 token
     丢弃＝rounds=[] foreign=[]＝义务整段跳过的假 PASS，丢弃方向是 fail-open）；⑭ 豁免
     被否不再整 token 丢弃——前向配对臂的裸号引用只压计入半边，foreign 半边照走；⑮ 链
     构建 / RSHORT 收集 / 前向配对三臂同源判据（DS ⑤：`#3056 +、×65 + #3074 r5` 计 5
     而裸号拼写被丢的口径分裂封堵），判据与 BARE_NUM_BEFORE 解耦——区间内恰一个纯数字
     run 即链尾成员，**空格前置的裸号**（`#3056 3074 r5`）照算，与链构建臂（空格是 gap
     字符）同口径。r10 只封了「纯间隙 + 前向臂」的双空——散文间隔三臂残留见 fix-r11。
     fix-r11（MM+DS r10 复审双撞 P1×2 同根 + MM sibling#3 b 档升 P1，散文间隔维度的
     双空残留——fix-r10 矩阵缺 gap 内容轴，同处第四轮补轴）：⑯ 前向臂 foreign 半边
     对称化（去掉 isdisjoint 限制）——链 ∩ issues ≠ ∅ 但裸号非链成员（含散文间隔
     `#3056 P1×3 + 2932 fix-r2`）时也进 foreign 拒收（「插一个词即从计 2 跳到义务
     整段跳过」的 fail-open 拼写分裂封堵）；⑰ 收集臂散文间隔简写 token 不再整条丢弃
     ——照前向臂口径进 foreign（`#3056+3074 补 r5` 与全拼 `补 fix-r5` 计 5 的拼写
     分裂封堵）；⑱ 后向臂补 foreign 半边（镜像前向，相交/不相交两半边；题头区外
     board 尾照旧合法不计不拒）——**非枚举型标题**下全臂全 gap 内容「不计不拒」
     双空路径消灭（fix-r10 ⑭ 的「消灭」当时只对纯间隙前向臂成立，文案已对齐；
     适用面范围句由 fix-r12 ⑳ 写清）；⑲ check() 的 foreign 拒收文案按链 ∩ issues
     两分支（相交半边＝归属存疑，修法在标题侧；不相交/no-issue 照旧「补 Refs」
     ——旧文案对相交半边会说「引用不在本 PR 关联单」而链就在关联单里，假指引）。
     fix-r12（MM+DS r11 复审 P1×2 + P2×2，enum 维度残留 + 评审署名族收口）：
     ⑳ 收集臂 enum 只压 foreign 半边、rounds 路径照走——枚举型标题下散文间隔简写
     token 不再整条丢弃，由前向臂配对判据决定计入（`#3056+3074 补 r5 … #2957
     P1×3` 计 5，与全拼同判）；⑭⑱ 的「双空消灭」范围由此写清：**非枚举型标题**
     成立，枚举不相交半边（含裸号引用断归属的相交半边）foreign=[] 是登记在案的
     有意抑制（board 并记形态保护，见 `_enum_title` docstring 窄窗表）；
     ㉑ 评审署名族窄窗登记（sibling#1，协调者裁定 b 案）：评审者署名+简写轮号
     （`MM r16`/`DS r2`，字母 run 收尾间隔）自 fix-r11 起进 foreign 拒收，属有意
     fail-closed（多拒不漏）——修法是把引用移出题头区，不是补全拼（全拼 fix-rN
     对本族＝假声明修复轮 N，N≥4 触发 residual 连锁假 FAIL）；check() 相交半边
     文案同步去「全拼 fix-rN」建议（改为「移出题头区或与链写紧邻」）。
     fix-r13（#3074，MM+DS r12 复审 P1×2 双谱系双撞同根，enum 门半边化 + 收集臂
     第二条静默丢弃路径，协调者四孪生实测 _tmp/20261114-3074-fixr13/）：
     ㉒ enum 的 foreign 抑制由「not enum_title 整题关」收成**半边**——只压链 ∩
     issues = ∅ 的不相交半边（board 并记各归各单，登记抑制保留）；相交半边下
     归属存疑 token（裸号引用/含数字散文/评审署名族字母 run）照 fix-r11 非枚举
     口径进 foreign 拒收、不再计入 rounds（收集/前向/后向三臂同口径）——「加
     一条无关链即绕过拒收」的静默放行与署名族 enum 计入（假声明轮号，N≥4 触发
     residual 假 FAIL 向）同轮封堵；⑳ 的「rounds 路径照走」与 fix-r12 登记窄窗②
     （相交半边＝登记抑制）自本修收窄/废止，硬不变式② 收窄到不相交半边；
     ㉓ 题头区内、前方无 #N 的简写 token（协调者追加 MM sibling#2）不再整条
     丢弃——与全拼同走 SELF/后向臂（`修 + 2932 r2 交付 + #2957` 旧 ([],[]) →
     foreign 拒收＝简拼/全拼拼写分裂封堵），硬不变式「任何格子不得 rounds=[] ∧
     foreign=[]」在简写后向臂格补齐（fix-r16 P2#2 登记例外：「异侧×不相交」格——
     token 在题头区内、后方链在全角「：」之后且与 issues 不相交——自 r14 起即是有意的
     双空（board 尾各归各单，不计不拒），本不变式对该格不成立，见 _scan_pairs 后向臂
     同侧守卫注释与 test-review-roles.py fix-r15 P2#1「board 尾异侧不相交」钉；fix-r17
     补登记第二格（MM r16 sibling#2 / boundary#3）：「无链×简写」格——题头区内简写
     token 且全题无 #N 链（`if not hashes:` 分支对 rshort_nolink 不计不拒）——自
     r14 起即有意双空（无链简拼回到不计＝r14 窄窗登记），本不变式对该格同样不成立，
     行为钉见 test-review-roles.py fix-r14 窄窗「无链简拼」钉）。
     fix-r14（#3074，DS r13 复审 P0+P1——r13 引入的两个回归，协调者仲裁实测
     _tmp/20261114-3074-fixr13-ds/coord-arbitration.md，当轮修）：㉔ **正文回显
     兜底的 foreign 拒收**——check() 对 ECHO_HEAD 行与提交标题同跑
     foreign_fixround_pairs（fix-r13 半边化后收集臂 foreign 出口吞掉的 token 在
     正文兜底路径既不计入也不拒收＝假 PASS 回归；r12 计 5 → r13 None），拒收文案
     与标题路径同构（_foreign_reject_errs 共用件）；㉕ **无前链简写不再 SELF 计入**
     ——交后向臂评价并补「token 与后方链同侧于题头区界」守卫（r13 曾把题头区无链
     前导简写一律 SELF 计入＝board/评审散文 rN 被当修复轮的假 FAIL 面；无链/链异侧
     回到不计＝r12 判，board 尾形不引入新拒收；无链简拼真修复轮回到不计＝窄窗登记）。
     fix-r15（#3074，DS r14 增量评审 P1 + P2×3，P1 协调者实测仲裁
     _tmp/20261114-3074-fixr15/p1-probe.txt，当轮修）：㉖ **正文回显 foreign 拒收
     受首行早退限制**——check() 对 ECHO_HEAD 行的 foreign 扫描与 fix_round_from_body
     的 echo_scan 同条件（首行命中即整体跳过，_first_line_round 单源；修前无条件跑＝
     首行判 5 一边硬拒收「异单旁路」的假 FAIL）（**r15 口径，自 fix-r16 起作废**：
     收窄为声明形〔㉚〕、自 fix-r17 起再限定标题路径存在〔㉝〕——本地形态照跑，
     见 `_first_line_declares` docstring，勿按本句把跳过放宽回去）；㉗ **同侧守卫只压 foreign 半边**——
     链异侧且与 issues 不相交才不计不拒，异侧相交（SELF 同形真修复轮）照后向配对
     计入（旧假 PASS 面；board 尾链恰为本单也计入＝登记残余代价）；㉘ **全拼 SELF
     假 FAIL 拼写分裂登记**（注释订正 + A/B 证伪收口回退）——board 散文全拼
     「token 区内、链区外不相交」形 SELF 照计＝假 FAIL 窄窗，语料 0 条落窗；曾按
     ㉗ 同判据收口，真实语料复算静默 44 条真修复轮（closure-reject-proof.txt）
     违反「静默回归 0」验收，已回退为登记；㉙ 窄窗登记文字订正为实际判据（题头区内
     无前链简写 + 同侧后方链不相交 → 拒收，与类型词/裸号无关，四条形列全）。
     fix-r16（#3074，DS r15 boundary#1 P0 + MM boundary#1 P1 双谱系双撞同根 + P2×2 +
     登记不修，协调者实测仲裁 _tmp/20261114-3074-fixr15-ds/p0-p1-arbitration.md，当轮修）：
     ㉚ **首行早退收窄为声明形**——㉖ 的「首行命中即整体跳过正文回显 foreign 扫描」
     在两个形状上把 r14 的 desirable 拒收吞掉：首行本身就是 ECHO_HEAD 回显行且引异单
     （此时首行是唯一 foreign 出口，r14 拒收 / r15 假 PASS＝P0 失败面回归）；首行仅散文
     提及轮号（fix-r4 在 rounds 半边钉死的宽松首行＝多一道误拒（fail-closed）被搬到
     foreign 半边后方向反转为漏检，P1）。修法＝跳过仅当首行命中且首行不是 ECHO_HEAD
     回显行且命中 token 紧跟全/半角冒号（声明形判据与两向窄窗登记见
     `_first_line_declares` docstring；㉖ 的 rounds 半边宽松口径保持不变）；㉛ 判据
     陈述点三处同改（r15 改「异侧相交照计入」时漏改 3 处——_scan_pairs 收集臂内联 /
     SELF 支内联 / fix_round_from_title docstring，照旧文读会改回假 PASS 面）+
     文档↔行为同源断言；㉜ 硬不变式「任何格子不得 rounds=[] ∧ foreign=[]」登记
     「异侧×不相交」格例外（本条上文 r13 段）；另登记不修（MM boundary#2）：
     「异侧相交照计入」半边可能计到评审轮引用号且计入依赖「：」后最近链的词法顺序
     （见 _scan_pairs 后向臂 fix-r16 登记段，实测记录须人核）。
     fix-r17（#3074，MM r16 boundary#1 P1 主修 + r16 双谱系 P2 随轮，P1 协调者实测
     仲裁 _tmp/20261114-3074-fixr16-mm2-probe/ARBITRATION.md〔探针 p1-probe.py：本地
     形态零拒收 vs 同一回显行作标题 CI 形态拒「异单旁路」实锤〕，当轮修）：㉝
     **声明形跳过再限定标题路径存在**——㉚ 的前提「首行是主信号，正文回显不是信号」
     仅在标题路径已给出主信号（titles 非空）时成立；本地形态（--body-file 无
     --base/--head/--repo，pr_commit_titles 返回 None）fix_round 来自正文首行兜底，
     正文回显行是唯一 foreign 出口，声明形跳过＝吞掉唯一出口（与 ㉚ 场景 A「首行即
     回显行是唯一出口」同构）＝本地过、CI 拒的漏检。修法＝check() 跳过条件加
     标题路径存在半边：本地形态（titles=None）照跑正文回显 foreign 扫描
     （fail-closed 向），CI 形态（titles 非空）声明形跳过照旧
     （r15 修的假 FAIL 面保持）——r17 当时写的是 `titles is not None`，**fix-r19
     订正为「titles 非空」**（空集 [] 同样无主信号，见 ㉟）；r15 面钉翻转 + 本地/CI 双向钉 + 半角冒号判据改写
     （MM r16 boundary#2，选项②：保留半角、理由句改写、补端到端两向钉）+ 窄窗第二向
     与 _first_line 行取配对钉；ECHO_HEAD 行提取抽 `_echo_head_lines` 单源（MM
     simplify#2 P3 随轮顺手：foreign 半边与 rounds 半边共用同一行提取）；硬不变式
     例外补登记「无链×简写」格（㉓ 段，MM sibling#2）；两半边取号判据分叉（DS
     boundary#1）登记不修（rounds 兜底宽松取首个命中 vs foreign 声明形判形状，
     两半边 docstring 已各自声明）；ECHO_HEAD 相邻星号回溯形（DS concurrency#1）
     实测计时登记不修（数值见本轮 RESULT；**fix-r18 已修，见 ㉞**）。形态窄窗（须人核）：本地形态「正文手工
     异单回显 + 提交标题干净」会本地红 CI 绿（r15 修的假 FAIL 面对 CI 形态 PR 在
     本地预演中复活）＝有意取舍（fail-closed 宁拒不漏；ECHO_HEAD 三重结构保证散文
     不进入），登记见 `_first_line_declares` docstring。
     fix-r18（#3074，P0——r17 实测登记的 ECHO_HEAD 相邻星号回溯触发预注册阈值，
     协调者裁定 _tmp/20261114-3074-fixr18/p0-registration.md，当轮修）：㉞
     **ECHO_HEAD 失败路径消灾难性回溯（语义零变化）**——右括号后 `\\s*[:：]?\\s*` 是
     「两段 \\s* 夹一个可选字面量」相邻星号形，冒号缺席且后随非 `#`/非空白（作者可控
     正文 `fix(scripts)` + N 空格 + `fix-r5：x`）时全切分枚举＝O(N²)（r17 实测
     N=65514→35.2 s 超预注册 10 s；本地 --body-file 无长度上限＝更长行分钟级＝准挂起）。
     修法＝正则改写为语言等价的 `\\s*(?:[:：]\\s*)?`（W* C? W* ≡ W* (C W*)?：可选冒号
     并入前导空白组、冒号后空白只归一处；不用 possessive/原子组——那要 python≥3.11，
     本修法全版本可用），失败路径每步回溯 O(1)＝线性（实测 65514→0.003 s，~12000×）。
     等价证据＝穷举枚举 244 万串零差异 + 语料行轴 old-vs-new 直比零差异 +
     test-review-roles.py fix-r18 块（性能钉 N=65514 断言 <2 s〔预注册 10 s 的 5×
     余量，修后实测 ~0.003 s〕、冒号位形双向钉、形态字面量钉）。
     fix-r19（#3074，家族级收族——r17/r18 连续两轮单点修后双谱系评审又双撞出同族
     4+ 处，协调者裁定第三轮矩阵化收口，_tmp/20261114-3074-fixr19/）：㉟
     **titles 三态判据订正（P1，DS+MM r17+r18 boundary#1 双撞）**——㉝ 的
     `titles is not None` 把空集 []（pr_commit_titles docstring 明写空集与 None
     同款无主信号，如 base==head 的空提交范围）误当「标题路径已给出主信号」→
     空集 CI 形态声明形首行跳过正文回显 foreign 扫描＝㉝ 修的漏检形状在 [] 态残留。
     修法＝`if not (titles and _first_line_declares(masked)):`（[] 与 None 同走
     foreign 扫描，fail-closed；非空 titles 的声明形跳过照旧＝r15 面、r17 面都保持）。
     **回溯反模式收族（P0 WAIVER Θ(N³) + P1 NO_ISSUE Θ(N²) + 兄弟脚本 SCOPE_BODY/
     PERSON_DAY 同款并入）**：WAIVER 尾部 `\\s*` 删 + 组首钉 `\\S`（纯空白豁免理由形
     收窄＝改走「缺结果块」报错，两向都拒收，登记见 WAIVER 定义处注释）；NO_ISSUE
     尾部 `\\s*` 删（bool 用途零收窄）；SCOPE_BODY 冒号槽 W* C? W* 改写 + rest 首字符
     钉非空白（残余 `\\s*` × `.*` 二次方一并消除，「body 含换行须报错」判据不动）；
     PERSON_DAY 冒号/个槽同款改写。等价证据＝字母表枚举 + 形状差分 + 随机对拍
     （_tmp/20261114-3074-fixr19/），失败形 N=65514 全部 <0.003 s。族级护栏＝
     test-review-roles.py fix-r19 块（全部 re.compile 模式静态扫描钉「同一字符类
     被相邻两个量词同时匹配」+ 计时钉迁 _probe 子进程护栏〔fix-r16 教训〕）。
     fix-r20（#3074，DS/MM r19 双谱系增量评审，P1-B 多前缀 + P2 捆绑，协调者实测
     _tmp/20261114-3074-fixr20/）：㊱ **WAIVER/NO_ISSUE 多前缀失败形预扫**——r19 的
     线性化只证单前缀；正文 M 个 `<!--` 前缀重复且全文无字面 `-->` 时 search 对每个
     起点各把惰性组扩到串尾＝Θ(M·N)（实测 m=8000：waiver 3.04 s / noissue 2.23 s，
     1 MiB 外推分钟级＝准挂起面）。修法＝调用处 O(N) 预扫 `-->` in masked：不含
     字面 `-->` 直接跳过两模式 search（都要求字面 `-->`，语言等价）。**任一全空白
     理由的 waiver marker 即拒收**（DS boundary#4：空白 marker 与另一条 marker 并存
     时，r18/r19 都经跨界命中以垃圾长理由放行——实测旧形并不拒收该体，DS 静态推断
     不成立，登记见 WAIVER_BLANK 定义处；单独空白 marker 两版都拒不动）＝补
     WAIVER_BLANK 扫描、命中即置空 reason 走既有「豁免理由太短」通道（宁拒不漏）。
     titles 措辞两处订正（:199/:1845「titles 非空」，DS boundary#5）。族级白名单
     整条 pattern 相等（P1-A）与 RLIMIT_CORE/多前缀计时钉在 test-review-roles.py
     fix-r20 块。
  5. `fixtype=不适用` 的合法性按 diff 级路径检查（#3056 义务 4）：以 `scripts/ci_scope.py` 的
     docs_only 分类为唯一口径，用 --base/--head/--repo 复算 `git diff` 路径集，只碰白名单路径才放行；
     缺 --base/--head/--repo、git 失败、diff 为空、分类器加载失败一律 fail-closed 拒收（宁拒不漏）。
     fix-r2（DS boundary#4 P2）：基点改用 merge-base（与补丁指纹的三点 diff 同口径）——落后分支上
     两点 diff 会把 base 侧新增代码文件列成反向变更（实测 ds4-twopoint-threepoint-probe.txt）；
     错误消息按 False 来源分条（git 失败 / diff 为空 / 触代码路径）。
  6. 多块一致（#3056 G）：修复轮 PR 贴多个结果块凑双谱系/分批/复跑时，全块须同一 head=，
     或全块声明同一 patch= 指纹——不同头且不同指纹的块不得拼作「末轮评审覆盖齐」。

输入：--body-file FILE ｜ --pr N（调 gh pr view，只读）｜ 环境变量 PR_BODY（CI 用，避免 shell 插值）
      --head / PR_HEAD、--base / PR_BASE（CI 传 PR 的 head 与 base SHA；补丁指纹在 --repo 里算）
      --repo（算补丁指纹、复算「不适用」路径、读 PR 范围提交标题（修复轮主信号，须 --base 与 --head 同给）的 git 仓库，须含 base 与 head 两个提交）
退出码：0 通过 · 1 不通过 · 2 输入错误
"""
from __future__ import annotations

import argparse
import hashlib
import os
import re
import subprocess
import sys
from bisect import bisect_right

REQUIRED_ROLES = ("sibling", "concurrency", "boundary", "simplify")
# 边界一律用 ASCII 环视而不是 \b（#3056 fix-r2 修 3①，MM2 sibling#1）：Python 的 \w 含 CJK，
# 「已Fixes #1」「Refs #3056的说明」「结果块pr-gate:v1…」这类中文紧贴会让 \b 两侧都是 \w
# → 正则不匹配 → 合规 PR 被判「缺 issue 关联 / 缺结果块」（假 FAIL）。与 FIXROUND_RE 同款口径
# （check-taskbook-segments.py:96 的先例注释）。
MARKER = re.compile(r"<!--\s*pr-gate:v1(?![A-Za-z0-9_])([^>]*)-->")
# fix-r19（#3074，DS/MM r17+r18 增量双谱系双撞 P0）：旧形 `<!--\s*pr-gate:waiver\s+(.+?)\s*-->`
# （re.S）三个组件（`\s+` / `(.+?)`(re.S 含空白) / 尾部 `\s*`）都能吃同一段空白＝Θ(N³)
# 回溯（协调者实测失败形 N=2000→4.1 s / 4000→28.9 s / 8000→171 s，三次方特征；本机复测
# _tmp/20261114-3074-fixr19/fixprobe-r19.out）。修法＝①尾部 `\s*` 直接删（:1851 已
# `w.group(1).strip()`，尾随空白归并进组后再 strip 等价）；②前导 `\s+` 与捕获组互斥化
# （组首钉 `\S`）——`\s+` 贪心给回时 `\S` 立即失败＝O(1)/步，失败路径线性（实测
# N=65514 → 0.0005 s）。语义收窄登记（有意）：纯空白豁免理由形（`<!--pr-gate:waiver   -->`，
# strip 后理由为空）旧形命中后走「豁免理由太短」报错，新形不命中 → 走「缺角色审计结果块
# 或 pr-gate:waiver 豁免」报错——两向都拒收（fail-closed 方向不变），只是报错文案换了
# 通道；豁免理由本来就必须 ≥8 字（:1883），纯空白形从来不是合规输入。跨行理由（re.S）
# 不收窄：`\s+` 贪心吃满前导空白（含换行）后组首必是非空白，多行理由照常整段捕获
# （字母表枚举 + 形状差分证据 _tmp/20261114-3074-fixr19/，等价差分钉在 test-review-roles.py
# fix-r19 块）。
WAIVER = re.compile(r"<!--\s*pr-gate:waiver\s+(\S.*?)-->", re.S)
# fix-r20（#3074 DS r19 boundary#4 P2，marker 次序翻转收口）：r19 收窄后 WAIVER 不命中
# 纯空白理由的 marker——但正文同时还有另一条 marker 时（空白在前），search 会从空白
# marker 的 `<!--` 起点跨界吃进下一条的 `-->`，group＝`-->\n<!--pr-gate:waiver 合法理由`
# 垃圾长串过闸（r18 旧形同样跨界放行——实测两版该体都 rc0，DS 评审「旧形拒收」的
# 静态推断不成立，证据 _tmp/20261114-3074-fixr20/）；单独一条空白 marker 两版都拒收
# （r18「豁免理由太短」/ r19「缺结果块」，登记收窄面不变）。按「宁拒不漏」收口：
# 任一全空白理由的 waiver marker 即拒收——check() 豁免半边命中本模式即置空 reason、
# 走既有「豁免理由太短」通道。`\s*` 后钉字面 `-->`、无跨界组：失败路径线性，别写回
# 任何「组能跨界吃 `-->`」的形态（r19/r20 反模式族）。
WAIVER_BLANK = re.compile(r"<!--\s*pr-gate:waiver\s*-->", re.S)
# fix-r13（DS r12 simplify#1 P2）：删除 ISSUE_LINK——规则 2 原用它判「有没有 issue 关联」，
# 但它与 ISSUE_NUM 是「关键词 + #N」的同前缀正则，check() 里 issues = pr_issues(masked)
# 已算过同一信息；两份词表/边界正是 r9-r12 反复漂移的形态（ISSUE_LINK 比 ISSUE_NUM 松时
# 规则 2 放行而 pr_issues 空集 → fix_round=None → fixtype/residual 跳过＝假 PASS）。删除前
# 等价性实测：`_tmp/20261113-3056-fixr13/issuelink-equivalence-probe.txt`（标题 11126 +
# board 行 6556 + 构造形态 268 条，bool(ISSUE_LINK.search) == bool(pr_issues) 差 0）。
# 关键词词表保留 `_ISSUE_KEYWORDS`（ISSUE_NUM 唯一使用者，单正则无漂移对象）。
_ISSUE_KEYWORDS = r"(?:close[sd]?|fix(?:e[sd])?|resolve[sd]?|refs)"
# fix-r19（#3074，同 WAIVER 相邻星号反模式收族，P1）：尾部 `\s*` 删——`:1891` 只作
# bool 用（组内多吞的尾随空白不影响命中性），`\S.*?` 给回扫 `-->` 与尾部 `\s*` 的重叠
# 消除后失败路径线性（实测 N=65514 → 0.0004 s，改前 4.2 s＝Θ(N²)）。前导 `\s+` 与
# 组首 `\S` 本就互斥。纯空白理由形两版都不命中（旧形组首已钉 `\S`）＝零收窄。
NO_ISSUE = re.compile(r"<!--\s*no-issue\s+(\S.*?)-->", re.S)
# 补丁指纹的 diff 参数全部写死：不受本机 / CI 的 diff.renames、diff.algorithm、前缀、颜色、外部 diff 配置影响
FINGERPRINT_DIFF = ["diff", "--no-color", "--no-ext-diff", "--no-textconv", "--no-renames", "--unified=3",
                    "--diff-algorithm=myers", "--src-prefix=a/", "--dst-prefix=b/"]

# #3056 修复轮字段（长尾收点卡「机检落点」）：修法类型只接受字面三值。
FIXTYPE_VALUES = ("枚举式", "结构性", "不适用")
# N≥4 的 residual 必填门槛（fix-r2，DS boundary#3 P2）：此前在 check-pr-gate.py 的
# residual_errors 与 check-taskbook-segments.py 的 check_fix_fields 两处字面量 `4`——
# 改一侧不改另一侧会让两个闸门对同一 PR 给出相反结论（一侧要 residual、另一侧不要），
# 且无任何测试断言同源。单源导出：任务书侧 `RESIDUAL_MIN_ROUND = _gate_module().RESIDUAL_MIN_ROUND`
# （照 FIXTYPE_VALUES 的手法），test-check-taskbook-segments.py 有派生断言。
RESIDUAL_MIN_ROUND = 4
# PR 侧修复轮识别：正文第一个非空行含 fix-r<N>（词边界防 fix-r10 匹配成 fix-r1）。
# 边界用 ASCII 字符集而不是 \w：Python 的 \w 含 CJK，中文紧贴（「修复fix-r5：」「fix-r5轮」）时
# \w 边界把整轮判没 → fixtype/residual/N≥4 全跳、闸门假 PASS（DS boundary P0，Python 3.14 实测
# re.match(r"\w", "复") 为 True）。中文不是 fix-r 的词组成部分，ASCII 边界足够。
# fix-r3（P1-3，MM r2 boundary#2 + P2a DS r2 boundary#2）：① 接续轮 b 后缀——`fix-r11b`
# 算第 11 轮（只认 b：`fix-r5x` 仍不是修复轮，既有负控钉死；`fix-r5b2` 词尾不闭合不计）；
# ② 数字位界 {1,18}（兄弟尺子 check-taskbook-segments.py:390 先例）——>4300 位 int() 在
# Python 3.11+ 抛 ValueError（traceback 崩溃冒充 FAIL），位界让超长数字串根本不匹配、
# 由 check() 的 *_OVER 扫描按 fail-closed 干净报错。b 后缀与位界与任务书侧 FIX_ROUND_PART
# 单源（check-taskbook-segments.py 派生本对象）。
FIXROUND_RE = re.compile(r"(?<![A-Za-z0-9_-])fix-r(\d{1,18})b?(?![A-Za-z0-9_-])")
# fix-r3 P0①（MM concurrency#1）：仓内真实简写惯例 `fix(scripts) #3056 r14：…` /
# `chore(gateway) #1579 r3: …`——标题里写 r<N> 而不是 fix-r<N>。RSHORT 只提供词形；
# 收集面＝题头区内全部（fix-r13 起前方无 #N 的简写不再丢弃；fix-r14 起不 SELF、交
# 后向臂要求后方真实链且与 token 同侧于题头区界，无链/链异侧且与 issues 不相交不计
# ——fix-r15 起异侧相交照计入，见 _scan_pairs 收集臂注释）；归属与计入由 _scan_pairs 的结构判据决定：与前方 #N 链
# **紧邻**（gap 全为链
# 字符（_is_issue_gap_char 单源），或间隔恰为链尾裸号成员（`_bare_member_connected`
# 单源：区间恰一个纯数字 run，fix-r10 起精确判定无距离上界；RSHORT 的 lookbehind 排
# 数字 → 空 gap（`#3056r14`）实际不可达）；散文间隔的简写形自 fix-r11 起不整条丢弃、
# 照前向臂口径进 foreign 拒收——enum 相交半边自 fix-r13 起同判 foreign 拒收、不相交
# 半边照走 rounds 路径（enum 门半边化，见 _enum_title docstring）——本注释为判据
# 陈述点⑧）——题头区外的
# rN 是评审轮引用（`fix-r3：r2 评审`，语料 423 条）与决策卡注记，不计
# （_tmp/20261114-3074-fixr3/probe-corpus.txt §1/§1b）。位界同 P2a；fix-r4（boundary#7）
# `b?` 与 FIXROUND_RE 单源同步——「简写 + 接续轮」组合形 `#3056 r14b` 不再一侧认一侧不认。
RSHORT = re.compile(r"(?<![A-Za-z0-9_-])r(\d{1,18})b?(?![A-Za-z0-9_-])")
# fix-r3 P2a（DS r2 boundary#2）：位界外的形态（fix-r / #N / rN 后 ≥19 位）按 fail-closed
# 报错的依据——HASH_OVER 同时覆盖 HASH_REF 与 ISSUE_NUM 两处 #N 入口（ISSUE_NUM 的续写
# #\d+ 也不设位界，正文 `Refs #<19+位>` 会经 sorted(issues, key=int) 走到 int()）。
# fix-r4 P2-1（concurrency#3 + boundary#8 + sibling#3 三撞）：OVER 词表尾环视使
# `fix-r`+19位+`b` 既不匹配主词表也不触发 OVER＝静默漏判（假 PASS 方向）——`b?` 同步进
# OVER 形；过宽命中也只是 fail-closed 拒收（宁拒不漏）。
FIXROUND_OVER = re.compile(r"(?<![A-Za-z0-9_-])fix-r\d{19,}b?(?![A-Za-z0-9_-])")
# fix-r8 boundary#4（MM r7 评审，登记路线——收窄须先实测语料）：HASH_OVER 无左边界、
# 无 issue 关键词约束，**已知窄窗**＝正文任意位置的「`#` + ≥19 位数字」都触发整单
# fail-closed 拒收（`see #1234567890123456789` 形的长 id/片段串）——真实 issue 号不可能
# 19 位，过宽命中只多拒不放行（宁拒不漏方向）；_overflow_err 的文案按 name 区分
# 「#N 引用」与「轮号 token」，误伤时可从报错定位到是哪个 `#` 串。
HASH_OVER = re.compile(r"#\d{19,}")
RSHORT_OVER = re.compile(r"(?<![A-Za-z0-9_-])r\d{19,}b?(?![A-Za-z0-9_-])")
# fix-r3 P1-1 面 B 引入、fix-r4 P0-A（DS r3 boundary#1）收窄口径：无 #N 全计入兜底、题头区
# SELF 与后向配对三个免配对分支上，「token 前紧邻裸数字串」是 board/交接段枚举引用
# （`docs(state) 22:51 补落（2558 fix-r5 派 macmini）`、`…：1800 fix-r6 推 PR 3009`）——
# 裸号是引用的单号，不是本 PR 的轮。r3 的「前置不是字母/数字/下划线」口径过宽：
# `chore(tools) worktree 60 fix-r1：…`（60 是 worktree 席位上限，state/decisions-20261105-
# worktree-cap-60.md）与 `chore(state) 01:45 fix-r5 同步`（分钟）被吞 → fix_round=None →
# fixtype / N≥4 residual / 多块一致 / foreign 四项义务全跳＝尺子假 PASS（A/B 复算回退方向
# 7 条，wordlist-ab-recompute.txt fix-r3 版 :496-509；协调者仲裁 _tmp/20261114-3074-fixr4-arb/
# arb.py 实测 rounds=[]）。fix-r4 口径＝**枚举分隔前邻**：裸数字串前（跳过空白回看）最近的
# 非空白字符须是非词字符（`\w` 之外——标点/全角符号），且**排除 ASCII `:` 与 `#`**——
# `：1800`/`+ 2932`/`P1×3`/`（3056`/`、2558` 等真枚举引用照压（probe-corpus.txt §2），而
# `worktree 60`（字母前邻＝配额值）与 `01:45`（ASCII 冒号前邻＝时间戳 HH:MM）豁免；
# `#` 排除保住 glued 引用 `PR#3074` 正控（fix-r10 钉死 5）。ASCII `:` 排除与 DS 建议词表
# （`（(、，：:+/`）同句目标（「使 worktree 60、01:45 豁免」）自相矛盾——取目标侧：时间戳
# 的 MM 不是单号引用。已知窄窗（登记不改标准）：裸数字恰在串首（无前邻字符）按「无裸号」
# 计入＝宁高勿低；数字前是 CJK 词字符 + 空白的形（`段 1800 fix-r6`，\w 含 CJK）同豁免——
# A/B 复算修复方向 0 实例。调用走 _bare_num_before 的 ≤64 字符有界窗口（P1-E）。
BARE_NUM_BEFORE = re.compile(r"([^\w\s:#])[^\S\r\n]*(\d+)[^\S\r\n]*\Z")
# fix-r10：链尾裸号成员判据（_bare_member_connected）的单字数字口径——与 \d 家族
# （HASH_REF / RSHORT / BARE_NUM_BEFORE）同字符类，不用 str.isdigit()（其超集含
# 上标等 No 类字符，会让 `#3056 ² r5` 的 ² 被当数字成员，与正则口径分家）。
_BARE_DIGIT = re.compile(r"\d")
# 修 2（fix-r5，boundary#2 P1）配对判定的机件：标题里 #N 与 fix-r<N> 就近配对——
# 引用别的单的轮号按 issue 号归属各单，不再「每标题首个」（低估→跳过 N≥4 residual＝假 PASS）
# /「行内 max」（高估→多要 residual＝假 FAIL）二选一。#N 串由链接符连成（#3056+#3057 一组）；
# 空隙视为紧贴（#1#2 也链）。pr_issues（本 PR 关联单号）从去围栏正文的 ISSUE_NUM 提取。
# fix-r9（DS r5 boundary P1）HASH_REF 加词界：前后排除字母/数字/下划线/前置 #——`boundary#1`、
# `C#1234`、`conc#1`、`boundary-rerun#1` 里的 #N 不再算「就近配对 #N」（伪 token 会抢真实
# 配对＝假 PASS）。fix-r10 复核 DS r9 boundary#2 P2 的「排除表收窄」方案（只排角色词与独立
# C/F，放过 `PR#3074` 真引用）：真实语料复算证伪——复活集全是 `conc#1`/`bnd#1`/`r3b#1`/
# `boundary-rerun#1` 伪 token（词表永远补不全，见 memory「词汇枚举式修判读不收敛」），而
# 语料 10686 条里 glued `PR#N`/`issue#N` **真引用 0 实例**——按评审给的另一选项**接受窄窗**：
# 字母紧贴的真实引用不参与配对（`… #9999 fix-r2 … PR#3074 fix-r8` 判不出 8），整条无 #N 时
# 仍走「全部轮号计入」兜底；一律排字母是结构性封类，不再逐词打补丁。前置**数字保留**
# （lookbehind 不排数字），`#1#2` 紧贴链行为（空隙视为紧贴，见上）不变。
# fix-r3 P2a：数字位界 {1,18} 同 FIXROUND_RE——>4300 位 int() 抛 ValueError 崩溃冒充
# FAIL（`sorted(issues, key=int)`、`_scan_pairs` 的 int(num) 都会走到）；位界外的 #<19+位>
# 由 HASH_OVER 在 check() 按 fail-closed 报错，不进 traceback。
HASH_REF = re.compile(r"(?<![A-Za-z_#])#(\d{1,18})(?![A-Za-z0-9_])")
# fix-r9（DS r5 boundary P0）多单关联列表：关键词后面的续写列表（`Closes #3056、#3057`、
# `Refs #3056/#3057`）第 2 个及以后的号也进集合——只取首个会让「标题引用 #3057 的修复轮」
# 判 fix_round=None，fixtype/residual/N≥4 整套跳过＝尺子假 PASS。
# fix-r10（DS r9 boundary#1 P0 + sibling#1 P1 双谱系同撞）：续写分隔符补**空白与 `+`**，与
# PAIR_SEP（+:、，/ 空白制表）口径对齐——board 真实形态 `Closes #382 #387 #452`、
# `Refs #1696 #1684`、`Refs #1985+#2053` 此前仍只取首号，同一假 PASS 面换分隔符复现；
# `\s+` 分隔不会把 `Closes #3056 and #3058` 的连词并进（`and` 不匹配 `#\d+` 前缀）。
# fix-r11（DS r10 sibling#1 + boundary#1 + simplify#1 三 P2 同簇，结构性修）：续写分隔符
# 从 PAIR_SEP **单源派生**——标点类不再手抄第二份字面量（r9 补 `[,，、/]`、r10 再补 `+` 与
# 空白正是两份定义漂移的产物，同一词汇位逐轮补词必出新洞，memory「词汇枚举式修判读不
# 收敛」：PAIR_SEP 早含 `+` 而 ISSUE_NUM 到 r10 才跟上）；**空档＝粘写**（`Refs #3056#3057`）
# 与 `_paired_rounds` 的「空隙视为紧贴」并组口径对齐（DS r10 boundary#1：粘写下
# pr_issues 只取首号 → 同一假 PASS 机制换分隔符）。
# fix-r12（DS r11 boundary#1 P0 + sibling#1 + concurrency#1 + simplify#1#2 + boundary#2 P2 同簇
# 收口）：①纯空白续写 `[ \t]+` → `[^\S\r\n]+`（`[ \t]` 丢了 r10 `\s+` 本收的**非 ASCII
# 空白**：U+3000 全角空格 / U+00A0 NBSP / U+2009——实测 `_tmp/20261113-3056-fixr12/
# nbsp-fullwidth-probe.txt`，中文输入法全角空格续写 `Closes #3056　#3057` 丢号 → 同一
# 假 PASS 面；`[^\S\r\n]` 保留「纯换行不并」的原意）；②标点改**标点串** `[{punct}]+`
# （与 `_paired_rounds` 的「gap 每字符在 PAIR_SEP」判据对齐——`Refs #3056+/#3057` 混排
# 也能并组）；③标点两侧 `\s*` 收紧为 `[^\S\r\n]*(?:\r?\n[^\S\r\n]*)?`——**至多一个换行**
# （`closes #3056,\n#3057` 单换行容忍保留，`Closes #3056,\n\n#3074` 段落空行不再并入，
# 消除「同一条正文边界两套口径」）；④关键词词表收敛为 `_ISSUE_KEYWORDS` 单源（定义
# 在上方关键词块，ISSUE_NUM 唯一使用者）。
PAIR_SEP = frozenset("+,、，/ \t")
_ISSUE_GAP_PUNCT = "".join(re.escape(c) for c in sorted(PAIR_SEP - {" ", "\t"}))
# fix-r13（DS r12 boundary#1 + sibling#1 双谱系同撞 P1，同根因）：标题侧 `_paired_rounds`
# 的链 gap 判据与 ISSUE_NUM 空白类**单源**——r12 只放宽了正文端（`[^\S\r\n]+` 收非 ASCII
# 空白），标题端仍只认 PAIR_SEP，`fix(scripts) #3056　#3074 fix-r12`（U+3000）在配对端
# 断链 → fix_round=None → fixtype/residual 跳过＝同一假 PASS 机制留在另一端。两个修复：
# ①`_is_issue_gap_char` 谓词（PAIR_SEP ∪ 非换行空白）供 `_paired_rounds` 用；②ISSUE_NUM
# 标点分支补**左侧**至多一个换行 + 标点串允许空白穿插（`Closes #3056\n,#3057`、
# `Closes #3056, + #3057` 并组——base `\s*[P]\s*` 本收、r12 收紧时未登记的欠收）。
# `_GAP_EDGE`＝标点任一侧的「非换行空白 ×（单换行 + 非换行空白）？」。
_GAP_EDGE = r"[^\S\r\n]*(?:\r?\n[^\S\r\n]*)?"


def _is_issue_gap_char(c: str) -> bool:
    return c in PAIR_SEP or (c.isspace() and c not in "\r\n")


ISSUE_NUM = re.compile(r"(?<![A-Za-z0-9_])" + _ISSUE_KEYWORDS + r"\s+"
                       rf"#(?P<nums>\d+(?:(?:{_GAP_EDGE}[{_ISSUE_GAP_PUNCT}](?:[^\S\r\n]*[{_ISSUE_GAP_PUNCT}])*{_GAP_EDGE}"
                       rf"|[^\S\r\n]+|)#\d+)*)(?![A-Za-z0-9_])", re.I)
# fix-r14（DS r13 concurrency#1 + boundary#1、MM concurrency#1 三谱系同撞 P0，仲裁实测
# `_tmp/20261113-3056-fixr14/redos-arbitration.txt`）：标点分支外层 `+` 内层「空白* + 标点+」
# 的嵌套量词是 `(P+)+` 形——M 个连续标点可被切成 2^(M-1) 种轮组合，gap 后无 #N 时全枚举
# （实测 n=22 已 0.32s、每 +2 标点 ×4.3，n=40 外推 20+ 分钟＝闸门挂起，PR 正文作者可控）。
# 改为每轮恰吃**一个**标点：标点串的轮划分唯一（第 i 轮＝第 i 个标点），语言等价、标点维度
# 线性。别再写回嵌套 `[P]+`（或任何「内外层都能吃同一字符类」的重复单元）。
# fix-r15（DS r14 boundary#1 P1、MM r14 concurrency#1 + boundary#1 同撞，两家取高；仲裁实测
# `_tmp/20261113-3056-fixr14/arbitrate-r14-ds.txt`）：r14 形态仍留**相邻星号**二次回溯面——
# 轮首 `[^\S\r\n]*` 与左侧 `_GAP_EDGE` 尾部 `[^\S\r\n]*` 相邻（`a*a*` 形），前导空白 K ×
# 标点 M 在失败路径上 O(K·M)（实测 `Refs #1 + " "*n + ","*n + "x"` 每翻倍 ×4，n=4000 已
# 0.88 s；r14 注释「空白重分配也不可能爆炸」只看了标点维度，空白维度不成立——已删）。修法＝
# **前导空白单处化**：首个标点前只由 `_GAP_EDGE` 吃空白，轮形 `[P](?:W*[P])*`——
# `GAP·W* ≡ GAP`（W 与标点/换行类不相交，首轮前导空白可无损并入 GAP，语言等价），轮间
# `P…W*…P` 无重叠类，全分支无非确定性切分。语言等价由词表复算 + 语义回归钉死
# （test-review-roles.py r15 块）。同理别写回「同一段空白两处星号都能吃」的任何形态。
# 类型词表单源（fix-r14，DS r13 sibling#1 P2）：ECHO_HEAD 与 TITLE_HEAD 两处手抄是 r12
# ISSUE_LINK/ISSUE_NUM 同款反模式——一处加词（如 deps）另一处漏改 → CI 路径与本地回显
# 路径的修复轮判定分裂。两正则只准引 `_TYPE_WORDS`；测试侧有同源断言。
_TYPE_WORDS = r"(?:fix|feat|docs|chore|test|style|refactor|perf|build|ci|revert)"
# 修 4（fix-r5，boundary#4 P2）本地回显扫描的判据：只认「结构化回显」——类型词 + (scope) +
# 冒号（可无）+ #N（可选 PR 前缀）+ 就近 fix-r<M>。**必须带 scope 括号与 #N**：fix-r4 停点
# 报告证明「类型词开头的散文」与真实标题在行内没有判别量（「test 覆盖了 fix-r5 的场景」
# 也以类型词开头），靠 scope 括号 + #N 引用 + 配到本 PR 单号三重结构才与散文分开。
# fix-r18（P0，DS r16 concurrency#1）：`\)` 后原来是 `\s*[:：]?\s*`＝「两段 \s* 夹一个可选
# 字面量」的相邻星号反模式（与本文件 _GAP_EDGE 修法上方注释同款）——冒号缺席且后随非
# `#`/非空白时 N 个空格的 (i,j) 全切分被逐枚举＝O(N²)（r17 实测 N=65514→35.2 s，本地
# --body-file 无长度上限）。改写为**语言等价**的 `\s*(?:[:：]\s*)?`：W* C? W* ≡
# W* (C W*)?（可选冒号并入前导空白组、冒号后空白只归一处），失败路径每步回溯 O(1)＝
# 线性（实测 65514→0.003 s）。等价证据：`\)` 后字母表 {空格,:,：,#,5} 长度 0..9 全枚举
# 244 万串（2,441,406）old-vs-new 零差异（match 及 span，echo-head-equiv.out）+ 语料行轴直比零差异
# （wordlist-ab-r18.txt ⑥）+ test-review-roles.py fix-r18 块（性能钉 + 冒号位形等价钉 +
# 形态字面量钉——防写回相邻星号形态）。
ECHO_HEAD = re.compile(r"^" + _TYPE_WORDS +
                       r"\([^)\n]{1,80}\)\s*(?:[:：]\s*)?(?:PR\s+)?#\d+")
# #3056 fix-r5 修 1 换结构化信号源（state/decisions-20261113.md「#3056 fix-r4 修 1 停点裁定」）
# → fix-r6 扩为集合（DS boundary#1 P0 + concurrency#1 P1 双撞同根因）：修复轮判定的**主信号 =
# PR 范围提交标题集合**（pr_commit_titles + fix_round_from_titles，`git log --format=%s
# <base>..<head>`，head 天然在集合内，不单独走一条路）。fix-r5 只看 head 单提交标题时，head
# 不带 fix-rN 的修复轮 PR（多提交轮的尾提交常是 `chore: sync board` / typo / merge 提交；
# specs/README.md:59 提交规约不要求每个提交带 fix-rN）被判非修复轮 → fixtype/residual/多块
# 一致整套跳过 = 假 PASS；本地 --body-file 不传 --head 的路径同样假 PASS。正文行内正则在
# 同一处 r2→r3→r4 三轮加词不收敛，fix-r4 停点报告用全量词表（1668 条真实标题基线）证明
# 「结构化标题」与「类型词开头的散文」两群在行内没有判别量（memory
# 「lexical-verdict-fix-nonconvergence」）。**下一个读者：别再往正文正则里加词**——正文回显
# 不是任何路径的主信号，只剩 fix_round_from_body 的首行宽松兜底，收紧它会复活 fix-r1 的
# CJK 连写首行 P0。
# 标题判据 = 类型词开头前缀 + 行内 fix-r<N>：
#   - 前缀 = 类型词 + 可选 (scope) + 可选 `!` + 冒号（半/全角）或空白分隔。`fix(scripts) PR #3047
#     fix-r3：…`（PR 在 #N 前）、`chore(tools) 分段检查器 fix-r1：…`（scope 后无 #N）、
#     `chore(state) 01:45 #1305 fix-r3 …`（scope 与 #N 间时间戳）、`fix(scripts) #3056+#3057
#     fix-r3：…`（多 issue 形）、`chore: retrigger CI for the fix-r1 …`（无 scope 无 #N 的散文
#     间隔形）等仓内 1657+ 真实形态全过——scope / 时间戳 / `PR` / `#N(+#N)*` 等元素只是前缀后的
#     可选普通文本，不设间隔上限（词表里 #N→fix-r 间隔最长 208 字符）。
#   - 行内出现 fix-r<N>（FIXROUND_RE，ASCII 词边界）。
# 标题判据与正文判据的关系：正文回显不再是主信号（旧 TITLE_ECHO 词表回显扫描已删）；标题判不出
# 修复轮时才走正文兜底。fix-r5 修 4（boundary#4）：主信号**缺失**（本地 --body-file 形态）时
# 兜底扩为 首行宽松 + 全正文**结构化**回显扫描（ECHO_HEAD：scope 括号 + #N + 配对三重结构）
# ——判别量来自结构而非词表，散文（`test 覆盖了 fix-r5 的场景`）仍不可入。fix-r8（MM r7
# sibling#2，DS r3 P0③ 同源副本漂移）：「集合在但全不命中（CI 形态）保持只看首行」自
# fix-r3 P0③ 起作废——本地 / 空集 / CI 三形态**同走**「首行宽松 + 全正文结构化回显扫描」
# （代码同一点：check() 一律 fix_round_from_body(echo_scan=True)，与模块 docstring :50-53
# 单一口径，不再留第三份手抄）；「非类型词开头形只走兜底」句同删——fix-r3 P0② 起带本单
# #N 配对的 `#3056 fix-r3：` 走配对分支判得出。标题里散文形与真实标题在行内不可分——
# 判修复轮只多一道 fixtype/residual 要求（假 FAIL 向窄代价）。
TITLE_HEAD = re.compile(r"^" + _TYPE_WORDS +
                        r"(\([^)\n]{0,80}\))?!?(?:[:：]|\s)")
RESIDUAL_MARKER = re.compile(r"^[ \t]*<!--\s*pdrs-residual:(.*?)-->[ \t]*$", re.M)
# 值域 = 字面三值的字符集：紧贴的中文标点/后续文字不并进值（DS boundary#3）——
# 「fixtype=结构性，纯文档」里值停在 结构性，「，纯文档」由 check() 的值后终止检查拒收（判缺/非法）。
WAIVER_FIXTYPE = re.compile(r"fixtype=([" + re.escape("".join(FIXTYPE_VALUES)) + "]+)")


# fix-r4 P1-E（DS concurrency#2 + boundary#3 同族）：裸号判定的有界窗口——token 前 ≤64 字符。
# 更长的「裸号」不可能是真实单号（正常 issue/PR 号 ≤9 位），窗口截断按「无裸号」计入
# ＝宁高勿低（假 FAIL 方向）。
_BARE_WINDOW = 64


def _bare_match_before(text: str, start: int):
    """token 前窗口内紧邻裸号**引用**的单源查找（fix-r10，MM simplify#1）：返回
    BARE_NUM_BEFORE 命中或 None。fix-r9 在 _bare_num_before 之外于
    _bare_member_before_token 抄了第二份同式
    `BARE_NUM_BEFORE.search(text, max(0, start - _BARE_WINDOW), start)`，与
    _bare_num_before docstring「四臂统一走本函数、单源」声明矛盾——现查找只此一份；
    fix-r10 后半轮判据与 bm 解耦（链尾成员走 _bare_member_connected 的区间判据，
    不再要 BARE_NUM_BEFORE 命中），本函数只剩 _bare_num_before（四臂裸号**引用**守卫）
    一个调用方；将来改窗口/`\\Z` 口径只动这里（裸号窗口＝token 前 ≤ _BARE_WINDOW 字符、
    digits 与 token 间只容非换行空白）。"""
    return BARE_NUM_BEFORE.search(text, max(0, start - _BARE_WINDOW), start)


class _GapScan:
    """「区间全 gap」精确判定的单调指针（fix-r10 P0，DS r9 复审 concurrency#1；同处第三轮
    修复，按规则矩阵化拍板方案①）：clear(lo, hi) 判 text[lo:hi) 是否全为 _is_issue_gap_char
    ——**无长度上界、无切片物化**，指针单遍只前进、每字符至多看一次（与 _scan_pairs 既有
    last_nongap 同形），token 侧摊还 O(1)。旧口径演进：fix-r8 第二道豁免的无界切片是
    O(T·n) 挂起面（0.87 MB 57.8 s）→ fix-r9 加「距链尾 ≤ _BARE_WINDOW」上界 → **上界
    又造出边界翻转**：gap=65 的链尾裸号成员被误判为引用 → 豁免被否 → 整 token 丢弃
    （rounds=[] foreign=[]＝义务整段跳过＝假 PASS；'+'*64 计 5、'+'*65 全丢，协调者实测
    _tmp/20261114-3074-fixr10/p0-repro-before.txt）——且方向与 fix-r9 docstring 所写
    「fail-closed〔多拒不漏〕」相反，丢弃恰是 fail-open。故本轮精确判定，不截断不放弃。
    单调契约：同一实例的 clear 调用 hi 须单调不减。_scan_pairs 的三个使用点各自独立实例
    保证之——链构建（hash 起点升序）、RSHORT 收集与前向配对（fix-r10 起查询点为数字 run
    起点 r0：token 按 start 升序时 r0 必不减——run 终点即该 token 的 last_nongap，其后到
    token 起点全 gap；若 r0_{k+1} < r0_k，则 run_k 整体落在 [r0_{k+1}, start_{k+1}) 的
    「run_{k+1} ∪ 尾部全 gap」里，数字不能进 gap 段 ⇒ run_k ⊆ run_{k+1}，两者皆极大 run
    ⇒ 相同 ⇒ r0_k = r0_{k+1}，矛盾于严格递减）。契约外（hi < _scan）fix-r11 起
    fail-loud raise（P2，MM concurrency#2 ≡ DS concurrency#1：静默回退直接精确判定
    会把摊还被破坏——单调性证明失效——吞成性能/正确性暗坑，违约单测钉在
    test-review-roles.py fix-r11 块）。"""

    __slots__ = ("_text", "_scan", "_last")

    def __init__(self, text: str) -> None:
        self._text = text
        self._scan = 0
        self._last = -1  # text[0:_scan) 内最后一个非 gap 字符位置（-1 = 无）

    def clear(self, lo: int, hi: int) -> bool:
        text = self._text
        if hi < self._scan:
            raise RuntimeError(
                f"_GapScan.clear 契约违约：hi={hi} < 已扫 _scan={self._scan}"
                "（同一实例的 clear 调用 hi 须单调不减，证明见类 docstring）")
        s, last = self._scan, self._last
        while s < hi:
            if not _is_issue_gap_char(text[s]):
                last = s
            s += 1
        self._scan, self._last = s, last
        return last < lo


def _bare_member_connected(text: str, start: int, chain_end: int | None,
                           gap_clear=None, last_nongap: int | None = None) -> bool:
    """token 起点前 [chain_end, start) 区间是否**只隔一个链尾裸号成员**（`#A+B` /
    `#A、B` 的 B，fix-r8 concurrency P1 引入、fix-r10 P0 矩阵化重写）。判据＝区间内
    全部非 gap 内容**恰为一个纯数字 run**：① 区间最后一个非 gap 字符是数字（\\d 口径，
    _BARE_DIGIT——散文/字母前邻即否）；② 该数字 run 向左延伸到 r0（逐位回走，O(run)，
    各 token 的 run 位置互不重叠＝线性总量）；③ [chain_end, r0) 全为 _is_issue_gap_char
    （PAIR_SEP ∪ 非换行空白）——**精确判定，无距离上界**（gap_clear 承载：_scan_pairs
    传 _GapScan 单调指针摊还 O(1)，直调默认区间逐位 genexpr，无切片物化）；④ run 不
    触链尾（r0 > chain_end，与旧 BARE_NUM_BEFORE 判据的 `bm.start(2) > chain_end`
    等价——词法上 `#N` 贪婪 \\d+ 使 hash 结束位后不可能是数字，直调合成形钉在
    test-review-roles.py fix-r6 sibling#1）。
    fix-r10 演进（同处第三轮，规则强制矩阵）：fix-r8 第二道豁免的无界切片是 O(T·n)
    挂起面（0.87 MB 57.8 s）→ fix-r9 加「距链尾 ≤ _BARE_WINDOW」上界 → **上界造出
    边界翻转 P0**：gap=65 的成员被误判为引用 → 豁免被否 → 整 token 丢弃
    （rounds=[] foreign=[]＝义务整段跳过＝假 PASS；'+'*64 计 5、'+'*65 全丢，协调者实测
    _tmp/20261114-3074-fixr10/p0-repro-before.txt）——且方向与 fix-r9 docstring
    「fail-closed〔多拒不漏〕」相反，丢弃恰是 fail-open。本轮判据与 BARE_NUM_BEFORE
    解耦（不再要 bm）：**空格前置的裸号（`#3056 3074 r5`，无枚举分隔符）也是链尾
    成员**——与链构建臂同判据（空格是 gap 字符，`#3056 #3074` 本就并链），矩阵
    g=0 裸号格的双空消除；`#A+B` 与 rN 间隔 > 查找窗口的空白串时不再行为分裂
    （fix-r9 登记的窄窗随判据解耦一并消除）。两臂共用（fix-r9 P1-2 引入）：前向
    配对臂（_bare_num_before 的链成员豁免）与 RSHORT 收集臂（紧邻判据）同一口径。
    **直调无界面标注**（fix-r11 P2，MM concurrency#3 ≡ MM boundary#2）：gap_clear /
    last_nongap 省缺时走 O(gap) 逐位扫描（无上界）——生产三臂（gchain/gcol/gpair）
    必传 _GapScan 与收集臂单调指针；直调仅限单测短文本（fix-r6 sibling#1 钉族）。
    **本判据的全部陈述点清单**（fix-r10 MM sibling#2 建五处；fix-r11 P2-2（MM
    sibling#1 ≡ MM simplify#1 ≡ DS sibling#1）补全为九处——照旧文改极易把上界/
    无界切片/整条丢弃写回，改判据须九处同改）：① 模块 docstring fix-r10 段
    （rN 简写收集句）；② _bare_num_before docstring；③ _bare_num_before 内联注释；
    ④ _scan_pairs docstring（前向裸号守卫句 + RSHORT 收集判据句）；⑤ _scan_pairs
    收集臂内联注释；⑥ _bare_member_before_token docstring；⑦ _scan_pairs 前向臂
    内联注释；⑧ RSHORT 词形注释块（模块常量区）；⑨ fix_round_from_title docstring
    简写句。"""
    if chain_end is None or start <= chain_end:
        return False
    if last_nongap is None:
        last_nongap = -1
        for p in range(chain_end, start):
            if not _is_issue_gap_char(text[p]):
                last_nongap = p
    if last_nongap < chain_end or not _BARE_DIGIT.match(text[last_nongap]):
        return False  # 区间全 gap（无裸号）或末位非 gap 字符不是数字（散文/字母前邻）
    r0 = last_nongap
    while r0 > chain_end and _BARE_DIGIT.match(text[r0 - 1]):
        r0 -= 1
    if r0 == chain_end:
        return False  # run 触链尾（词法不可达，保直调合成形口径与旧判据等价）
    if gap_clear is None:
        return all(_is_issue_gap_char(text[p]) for p in range(chain_end, r0))
    return gap_clear(chain_end, r0)


def _bare_member_before_token(text: str, start: int, chain_end: int | None,
                              gap_clear=None, last_nongap: int | None = None) -> bool:
    """token 与 chain_end 处 #N 链之间只隔一个链尾裸号成员（fix-r9 P1-2，RSHORT
    收集臂专用；fix-r10 重写为与 _bare_member_connected 同源的区间判据）：
    `#3056+3074 r5：修闸门` 的 r5 与链之间的 `+3074`（或空格前置的 `3074`）是链
    自身成员，不是断链字符——视同紧邻收集。last_nongap＝收集臂单调指针已算好的
    「token 前最后一个非 gap 字符位置」，直调省缺时自算（O(gap) 逐位，**无上界**
    ——生产三臂必传 gcol.clear（fix-r11 P2 直调无界面标注），直调仅限短文本单测）。
    返回 False（非成员，含散文间隔）时收集臂自 fix-r11 P1-2 起不再整 token 丢弃
    ——照前向臂口径进 foreign 拒收（**fail-closed 多拒不漏**；旧口径丢弃方向实为
    fail-open＝义务整段跳过的假 PASS，fix-r10 曾把本 docstring 标成 fail-closed
    与事实相反已更正——负例登记：谁再引入「间隔非成员 → 整条丢弃」即红，见
    test-review-roles.py fix-r11 矩阵 gap 内容轴）。已知窄窗（登记）：全拼形
    （前向臂 BARE_NUM_BEFORE 无命中 → 不拦 → 计入）与简写形（收集臂 foreign
    拒收）行为仍分——计入 vs 拒收都是非空出口，双空不变式不破；该分裂自 fix-r13
    （enum 门半边化）起在枚举与非枚举标题下**同判**（enum 相交半边简写照拒收、
    不相交半边 rounds=[] 登记抑制；fix-r12 曾令 enum 相交半边简写照走 rounds
    路径与全拼同判，本修按 P1 收口——enum/非 enum 两半边行为一致），见
    _enum_title docstring）。"""
    return _bare_member_connected(text, start, chain_end, gap_clear, last_nongap)


def _bare_num_before(text: str, start: int, chain_end: int | None = None,
                     gap_clear=None, last_nongap: int | None = None) -> bool:
    """token 前紧邻裸号引用（fix-r4 P0-A 枚举分隔前邻口径，BARE_NUM_BEFORE）的
    O(1)/token 判定：查找走 _bare_match_before 单源（fix-r4 P1-E 的有界窗口取代
    旧的整前缀重扫——O(T·n) 实测 n=12000 达 4.04 s，
    _tmp/20261114-3074-fixr4/red-timing-before.txt；fix-r10 MM simplify#1：fix-r9
    曾在 _bare_member_before_token 抄出第二份窗口表达式，与本函数「四臂单源」声明
    矛盾，现查找与判据各只一份）。
    chain_end（fix-r6 sibling#1）：前向配对臂的**链成员豁免**单源参数——命中裸数字
    起点 < chain_end（最近前方 hash 的结束位）时视为 `#A+#B` 链自身成员而非引用，
    返回 False（fix-r5 语义逐字等价；fix-r7 simplify#1 实测事实核：链首成员被前导
    `#` 排除、链尾成员带 `#` 时 bm=None，见 _tmp/20261114-3074-fixr7/RESULT.md）。
    豁免判据自 fix-r10 起与 bm 解耦、走 _bare_member_connected 的区间判据（恰一个
    纯数字 run + 前方全 gap，**精确无距离上界**，gap_clear＝_GapScan 摊还 O(1)）。
    返回 True＝裸号引用（非链成员）：无 #N 全计入 / SELF / 后向配对三臂由此**不计入**
    （fix-r4 P0-A / fix-r5 P1 的假 FAIL 防护，不变）；前向配对臂自 fix-r10 P0 起不再
    由「返回 True」整 token 丢弃——只压计入半边，foreign 半边照走（旧「不计不拒」
    在链 ∩ issues = ∅ 且题头区内时＝义务整段跳过的假 PASS，丢弃方向是 fail-open，
    见 _scan_pairs 前向臂注释）。chain_end 两道豁免的保留/删除不由此背书（A/B：
    前向臂去掉该参全语料 0 判定差异）。四臂（无 #N 全计入 / SELF / 前向 / 后向配对）
    统一走本函数；将来改窗口/`\\Z` 口径只动 _bare_match_before。**直调无界面标注**
    （fix-r11 P2，MM concurrency#3 ≡ MM boundary#2）：gap_clear/last_nongap 省缺时
    豁免走 O(gap) 逐位扫描（无上界）——生产三臂必传 _GapScan 与收集臂单调指针，
    直调仅限单测短文本（fix-r6 sibling#1 钉族）。"""
    bm = _bare_match_before(text, start)
    if bm is None:
        return False
    if chain_end is not None and bm.start(2) < chain_end:
        # fix-r5 链成员豁免（生产不可达，见 docstring 事实核）
        return False
    # fix-r8 concurrency P1（MM r7 评审，协调者实测复现）：裸号判据与链成员判据分家——
    # `#A+B` / `#A、B` 的 B（链尾不带 # 的成员）与最近前方 hash 之间只隔链分隔符时是
    # 链自身成员，不是 board/交接段枚举引用——旧口径该形「既不计入也不进 foreign」
    # （rounds=[] foreign=[]）＝义务整段跳过的假 PASS。链尾裸号成员照常走配对判定
    # （∩ issues ≠ ∅ 计入；fix-r11 起全不相交的异单半边与相交但非链成员的归属存疑
    # 半边都进 foreign 拒收）。
    # 已知窄窗（登记）：裸号成员**不并进配对集**——`#3056+3074 fix-r5` 的链集仍是
    # {3056}（HASH_REF 只收带 # 的号），issues={3074} 时该形进 foreign（5,{3056}）
    # 拒收而 `#3056+#3074` 计 5——裸号拼写更严（fail-closed 向），A/B 复算 0 实例。
    # fix-r11 P1-1 文案同步：原「全不相交进 foreign 拒收」只覆盖异单半边——foreign
    # 半边自 fix-r11 起两半边对称（链 ∩ issues = ∅ 的异单，或链 ∩ issues ≠ ∅ 但裸号
    # 引用/散文间隔断开归属＝归属存疑），都拒收（见 _scan_pairs docstring）。
    # fix-r9 曾把判据抽 helper 并加距链尾 ≤ _BARE_WINDOW 上界；fix-r10 P0（DS r9
    # 复审，'+'*64 计 5 / '+'*65 全丢）上界删除——精确判定走 gap_clear（_GapScan
    # 单调指针摊还 O(1)），无截断无放弃。
    if _bare_member_connected(text, start, chain_end, gap_clear, last_nongap):
        return False
    return True


# fix-r4 P0-B（DS concurrency#1 + boundary#2 双撞）：枚举判定的挂起面——旧实现两两 isdisjoint
# 是 O(C²) 且永不短路，`#1；`×N（每条 hash 各成一条链）仲裁实测 n=16000 达 3.24 s（纯二次，
# _tmp/20261114-3074-fixr4-arb/arb.py；本仓 r14 同类 O(k²) 曾实测 103.9 s 判 P0）。三重线性化：
# ①去重（同一 frozenset 只判一次）；②全体公共元素非空（set.intersection(*uniq)）⇒ 无任何
# 不相交对，短路 False；③去重后链数 > _ENUM_CHAIN_CAP 直接按非枚举处理——fail-closed 方向
# （foreign 照判，宁可多拒不少拒；真实语料题头区内去重链数最大 5，上限 200 是 40 倍余量，
# A/B 复算 wordlist-ab-recompute.txt fix-r4 版「题头区链分布」）。
_ENUM_CHAIN_CAP = 200


def _enum_title(chains: list[frozenset[str]]) -> bool:
    """fix-r3 P1-1 面 A 引入、fix-r4 P1-C（DS sibling#1 + boundary#4 双撞）限定：≥2 条互不相交
    #N 链 ⇒ 枚举型/board 型标题（「#1305 …；#1318 …」两链各说各的单），整条不作 foreign 判定
    （计入分支照旧）。只压 foreign 半边（假 FAIL 面），不放宽计入。**fix-r4 调用方只传题头区
    内的链**：题头区外的尾部 #N 注记（`（复用 #1321 机制）`）不再把整条标题的 foreign 封堵关掉
    ——否则构形 `fix(scripts) #9999 fix-r5：修评审（复用 #1321 机制）` + `Closes #3056` 里链
    {9999}/{1321} 不相交 → enum=True → 异单失配不进 foreign ＝ fix-r2 P0 旁路复开（假 PASS）。
    题头区限定的正确性依据：gap 字符（PAIR_SEP ∪ 非换行空白）不含全角「：」→ 链不跨「：」，
    「链首在题头区」⇒ 整条链在题头区。
    fix-r5 P0（DS r4 boundary#1）：全角「：」不是唯一的题头分隔拼写——ASCII 冒号（真实语料
    wordlist-ab-recompute.txt:561 `fix(gateway-parity) #1355/#1652 fix-r25: …`）与无冒号
    形（`… fix-r5 修评审（复用 #1321 机制）`）下 zone_end=len(text)，同一旁路复开（仲裁
    四形三假 PASS）。调用方参与判据再收两道界（都在 `_scan_pairs` 内计算，见其 zone_end 后
    注释）：**enum_end**（首个锚点——#N / fix-rN token / 紧跟 ASCII 冒号的 rN token
    （fix-r8 sibling P1）——之后的 ASCII ':'，前缀/时间冒号在锚点前不作界）与**括号深度 0**（全/半角括号内的链是注记/board 括注，不是枚举项；
    fix-r7 sibling#1 起深度按宽度分类两条——全角（）与半角 () 各自 +1/钳 0 减，两类都 0
    才参与：混宽镜像序 `（a)` 不再把深度抵回 0，见 _scan_pairs pdepth 注释）。
    已知窄窗（登记不改标准）：无冒号且无括号的裸尾注 `fix(x) #9999 fix-r5 复盘 #1321 机制`
    与前缀冒号形 `fix(x): #9999 fix-r5 修 #1321`（锚点在冒号后）仍被枚举抑制——前者与
    真实 board 并记（`#1305 fix-r3 …；#1318 …`）在词法上不可分，后者收紧会误伤
    `chore(board): 01:10 …` 族；A/B 复算确认两形语料 0 实例。fix-r6 再登记三条（DS r5
    boundary#1/#3 + sibling#2②）：① 类型词 scope 括号内的链 `fix(scope,#N,#M)` 不参与
    枚举——若真实关联单只出现在尾部注记（`（#P 收尾批）`）则 token 前向配对链（scope 链）
    与 issues 全不相交 → fail-closed 拒收（假 FAIL 向；真实 PR body 有界实核见
    _tmp/20261114-3074-fixr6/RESULT.md boundary#1 节）；② 锚点后条目内的时间戳（`HH:MM`）
    冒号会切 enum_end，其后枚举链退出枚举 → 合法 board/交接段标题 foreign 误报（假 FAIL
    向；A/B 复算语料 0 实例）；③ 游离闭括号后**不接开括号**的链（`a) #1321`）自 boundary#2
    钳 0 起回到深度 0 参与枚举——与「无冒号且无括号的裸尾注」窄窗同族（词法不可分）。
    fix-r7 sibling#1（DS r6 评审）封堵说明：③ 之外，「先闭后开」的序（`a)（…）`，fix-r6
    boundary#2 钳 0）与**混宽镜像序**（先开后闭 `（a) …`——全角开 +1 被半角闭 −1 抵回深度
    0，协调者实测复现：异单 foreign 丢失＝fix-r2 P0 旁路复开）都按宽度分类深度封在 ≥1，
    不构成窄窗。fix-r8 sibling P1（MM r7 评审）封堵说明：r<N>+ASCII 冒号拼写（`fix(scripts)
    r5: #9999 …`，rN 在首个 #N 之前）此前不受锚点覆盖＝第四种未登记拼写的旁路，现由锚点
    第三族（紧跟冒号的 rN token，见 _scan_pairs anchor 行）切 enum_end 封堵，不构成窄窗。
    fix-r9（MM sibling#3 + boundary#4，登记路线——收窄须先实测语料）：锚点第三族的
    **过宽面**——散文 `rN:` 早于首锚点（首个 #N / fix-rN）时（`chore(state) r4: 01:10
    #1305 fix-r3 交付；#1318 缺第二家`），enum_end 被切到 rN 自己的冒号处，其后全部链
    退出枚举参与集 → 枚举抑制失效 → 合法 board/交接段标题的 foreign 误报（假 FAIL 向
    ＝多拒不漏，非旁路）。与 fix-r6 登记的 ②（锚点后时间戳冒号切 enum_end）同族，
    方向同为 fail-closed；真实语料 board 段 `rN:` 早于 #N 的实例 0（A/B 复算），
    登记不改标准；pinning 见 test-review-roles.py fix-r9 块（行为变更须过实测复算）。
    fix-r12（concurrency#1）：本 docstring 的「只压 foreign 半边（假 FAIL 面），不放宽
    计入」自本修起对**简写收集臂**同样兑现——枚举型标题下散文间隔/裸号引用断归属的
    简写 token 不再整条丢弃，照走 rounds 路径（计入判据＝前向臂配对：`#3056+3074 补
    r5 … #2957 P1×3` + {3056} 计 5；修前整条丢弃＝fixtype/residual/多块一致义务整段
    跳过的假 PASS，协调者实测 _tmp/20261114-3074-fixr12/probe-p1-before.out）。
    fix-r13（#3074，MM boundary#1 + DS sibling#1≡boundary#2 双谱系双撞，enum 门
    半边化）：foreign 抑制由「not enum_title 整题关」收成**半边**——只压「该链 ∩
    issues = ∅」的**不相交半边**（board 并记各归各单，fix-r3 P1-1 面 A 的保护本体，
    登记抑制保留）；**相交半边**（链 ∩ issues ≠ ∅）下归属存疑 token（裸号引用/含
    数字散文断归属/评审署名族字母 run 间隔）照 fix-r11 P1-1 非枚举口径进 foreign
    拒收（归属存疑文案）、不再计入 rounds——「加一条无关链即绕过拒收」（`#3056
    P1×3 fix-r2 交付；#9991 复盘` 旧 ([],[])＝静默放行）与署名族 enum 计入（`#3056
    MM r16 …；#9991` 旧 [16]＝假声明轮号，N≥4 触发 residual 假 FAIL）同轮封堵
    （协调者四孪生实测 _tmp/20261114-3074-fixr13/）。fix-r12 登记窄窗②（裸号
    引用断归属的相交半边＝登记抑制）自本修废止、改判 foreign 拒收（与非枚举同判）。
    修法＝把轮号/评审轮引用移出题头区（首个全角「：」之后）或与链写紧邻；硬不
    变式「链∩issues≠∅ 不得 rounds=[]」只在**归属完好**（简写需与链紧邻——纯 gap/
    链尾裸号成员；全拼需 gap 无裸号引用）时成立；硬不变式②（enum 下 foreign=[]
    是登记抑制而非静默丢弃）自本修收窄到不相交半边。pinning 见 test-review-roles.py
    fix-r13 块（四孪生 + enum 半边矩阵更新）。"""
    uniq = set(chains)
    if len(uniq) < 2 or len(uniq) > _ENUM_CHAIN_CAP:
        return False
    if frozenset.intersection(*uniq):
        return False
    lst = list(uniq)
    return any(lst[i].isdisjoint(lst[j]) for i in range(len(lst)) for j in range(i + 1, len(lst)))


def _scan_pairs(text: str, issues: frozenset[str], allow_unpaired: bool = True
                ) -> tuple[list[int], list[tuple[int, frozenset[str]]]]:
    """配对判定核心（fix-r5 修 2，boundary#2 P1；fix_round_from_title 与正文回显扫描共用）：
    text（提交标题或回显行）内每个 fix-r<N> / 题头区内与 #N 链紧邻（或间隔恰为链尾裸号
    成员——_bare_member_connected 单源，fix-r10 起精确判定无距离上界，区间恰一个纯
    数字 run 即成员）的 r<N>
    （fix-r3 P0① 简写形）的计入判定。题头区 = 首个全角「：」之前（无则整条；ASCII 冒号不切——`chore(state) 01:45
    #1305 …` 的时间戳与前缀冒号 `fix(scripts):` 都不是题头分界；fix-r5 P0 起**枚举链的
    参与判据**另有独立的界，见下 foreign 半边与 `_enum_title` docstring）。返回 (rounds,
    foreign)：
    rounds = 计入的轮号；foreign = 题头区内「#N 配对链非空且（与 issues 全不相交，
    或轮号 token 被裸号引用/散文间隔断开归属）」的 (轮号, 链)（fix-r2 P0 旁路封堵的
    依据；fix-r11 起三臂口径对称，见 foreign_fixround_pairs；枚举型标题自 fix-r13
    起只压不相交半边——相交半边照列，见 `_enum_title` docstring）。每个 token：
      - 整条无任何 #N → 计入（宁高勿低，fail-closed：漏计轮号会跳过 N≥4 的 residual 必填；
        fix-r3 P1-1 面 B 引入裸号守卫、fix-r4 P0-A 收窄口径：token 前紧邻裸数字串（数字前
        跳过空白后最近的非空白字符是**枚举分隔**——非词标点/全角符号，排除 ASCII `:`/`#`，
        BARE_NUM_BEFORE）视为 board/交接段枚举引用，不计——`docs(state) 22:51 补落（2558
        fix-r5 派 macmini）`；`worktree 60`（字母前邻＝席位值）与 `01:45`（时间戳）不是引用，
        照计入）；
      - 有就近配对集（token 前方最近的 #N、由 +/,/、/空白链成的 #N 串）→ 配对集 ∩ 本 PR
        单号 ≠ ∅ 才计入——board 尾 `…#2691 fix-r2…#2692 fix-r8 在跑` 里 8 归 #2692、
        2 归 #2691，各归各的单；链 gap 判据 = `_is_issue_gap_char`（PAIR_SEP ∪ 非换行
        Unicode 空白，fix-r13 与 ISSUE_NUM 单源——标题侧 U+3000/NBSP 同样并组，不再
        「正文端收、配对端丢」）；配对集非空且 ∩ issues = ∅ **且 token 在题头区** → 进
        foreign（修复轮标题引用了本 PR 未关联的单号——no-issue / 异单旁路，check() 拒收；
        题头区外的不进：board 尾引用别的单是合法形态，fix-r5 修 2 的配对语义保持不变；
        fix-r3 P1-1 面 A：**枚举型标题**（题头区内 ≥2 条互不相交 #N 链——board/交接段的
        并记形态，协调者仲裁实测 case1 `chore(state) 01:45 #1305 fix-r3 …；#1318 缺第二家
        标缺口` 被 foreign 假 FAIL）不作 foreign 判定（fix-r13 起收窄：enum 只压不相交
        半边——链 ∩ issues ≠ ∅ 的相交半边照进 foreign，见 `_enum_title` docstring），
        计入分支不受影响；fix-r4 P1-C：
        枚举只看**题头区内**的链——题头区外的尾部 #N 注记（`（复用 #1321 机制）`）不构成
        枚举，异单失配照进 foreign，no-issue/异单旁路（fix-r2 P0）不复开；fix-r5 P1
        （DS r4 sibling#1）：该前向配对臂补裸号守卫——token 前紧邻裸号（裸号不是前方链
        自身成员，由 `_bare_num_before` 的 chain_end 豁免判据保证，fix-r6 sibling#1 单源化；
        链成员豁免 fix-r10 起经 `_bare_member_connected` 精确判定无距离上界）时**不计入**
        （假 FAIL 防护），但 fix-r10 P0 起不再整 token 丢弃——题头区内且（非枚举或链 ∩
        issues ≠ ∅，fix-r13 enum 门半边化）时照进
        foreign 拒收（fix-r11 P1-1 起两半边对称：链 ∩ issues = ∅（异单）或链 ∩ issues ≠ ∅
        但裸号非链成员（含散文间隔，归属存疑＝异单混写）都拒收；旧「不计不拒」＝义务
        整段跳过的假 PASS，见前向臂内联注释），与另三个免配对/后向分支单源
        `_bare_num_before`）；
        fix-r5 P0（DS r4 boundary#1，仲裁 `_tmp/20261114-3074-fixr4-arb/arb-fixr4-out.txt`）：
        枚举链的参与判据在「题头区内」之上再收两道界——锚点后 ASCII 冒号、括号深度 ≥1
        （见 `_enum_title` docstring），无全角冒号标题的尾部 #N 注记不再关掉 foreign 封堵）；
      - 前方无 #N 且在题头区 → 计入（SELF：`fix(scope) 名 fix-rN：…（#refs 在后）` 的
        实修形态，refs 在题头之后的括注里；fix-r3 P1-1 面 B 引入、fix-r4 P0-A 收窄口径的
        裸号守卫同样拒收；**fix-r14 起全拼专属**——无前链简写不 SELF，落进下一支后向臂。
        fix-r15 P2#2 登记：board 散文全拼「token 区内、链区外」形（`chore(state)
        board 12:01 fix-r2：coord…——#2040`）SELF 照计＝与简写同侧守卫同款拼写分裂的
        假 FAIL 窄窗，**登记不收口**——A/B 证伪收口会静默 44 条真修复轮（见该臂内联
        注释），且语料 0 条落该窗）；
      - 前方无 #N 且在题头区外 → **向后看**最近的 #N 链（fix-r2，DS boundary#5 P2：
        `fix(gateway)：fix-r5 修 #3074` 形态的 token 与 #N 都在「：」后，旧判据判不出
        → fix_round=None → 同一假 PASS 面），链 ∩ issues ≠ ∅ 才计入（归属到本单才
        计数；无后随 #N 的板尾裸号 `交接段：…1623 fix-r6` 仍不计；fix-r4 P1-D：该分支
        补同一裸号守卫——board 尾 `… 2932 fix-r2 交付 + #2957 P1×3` 在 #2957∈issues 时
        旧照计 2 ＝假 FAIL，现在裸号 2932 紧邻 token 同样不计；fix-r11 P1-3：该分支
        补 foreign 半边（镜像前向臂，相交（裸号非链成员）/不相交两半边；题头区内且
        非枚举或链 ∩ issues ≠ ∅ 才进（fix-r13 enum 门半边化）——board 尾（题头区外）
        照旧合法不计不拒）。
    题头区外的无配对 token（板尾裸号）不计。issues 为空集时配对 token 全不计——该违规
    （正文没写关联 / no-issue）由规则 2 与 fix-r2 的 foreign 拒收兜住，配对判据不放大。
    fix-r3 P0①：r<N> 简写 token 收集面＝**题头区内全部**（fix-r13 协调者追加 MM
    sibling#2：前方无 #N 的简写不再整条丢弃——`修 + 2932 r2 交付 + #2957` 与全拼
    同走后向臂，简拼/全拼拼写分裂封堵；**fix-r14 P1（DS r13 boundary#1，仲裁
    _tmp/20261114-3074-fixr13-ds/coord-arbitration.md）：无前链简写不再 SELF 计入**
    ——r13 曾把题头区无链前导简写一律 SELF（board/评审散文的 rN 提及被当修复轮＝
    假 FAIL 面，origin/main 3000 条标题 29 条翻转、多 N≥4）；现交后向臂评价并补
    「token 与后方链同侧于题头区界」守卫（无守卫则 board 尾形 `chore(state) board
    12:01 r2：…——#2040 …` 的链配对＝假归属 → foreign 拒收＝新假 FAIL 面）；
    无链 / 链异侧 → 不计（r12 判）；**fix-r15 P2#1（DS r14 增量评审）：守卫只压
    foreign 半边**——链异侧且与 issues 不相交才不计不拒，异侧相交（SELF 同形真修复轮
    `fix(scripts) 修闸门 r5：#3056`、#3056∈issues）照后向配对计入（旧整条 continue＝
    rounds=[]∧foreign=[] 的假 PASS；board 尾链恰为本单也计入＝已登记残余代价）。
    **窄窗登记（fix-r14）**：无链简拼真修复轮
    （`chore(gateway) upload 清理 r6`）回到不计、与全拼 `fix-r6` 计入的拼写分裂
    复开——「无链前导简写不计」的必然取舍，修法是补全拼或补 #N 链，不是收窄守卫），
    归属与计入由结构判据决定：与前方
    #N 链**紧邻**（gap 全为
    _is_issue_gap_char；间隔恰为链尾裸号成员时视同紧邻——`_bare_member_connected`
    单源，fix-r10 起精确判定无距离上界、与链构建臂同判据，区间恰一个纯数字 run 即
    成员（空格前置的裸号 `#3056 3074 r5` 照算）」时保归属——`fix(scripts) #3056 r14：`
    计入、`fix-r3：r2 评审` 与决策卡注记（题头区外/不紧邻）不计；散文间隔（gap 含
    非 PAIR_SEP 字符）的简写形自 fix-r11 P1-2 起不整条丢弃——照前向臂口径进 foreign
    拒收（`#3056+3074 补 r5` 拒收 vs 全拼 `补 fix-r5` 计 5 的「计入 vs 拒收」分裂，
    自 fix-r13 enum 门半边化起在枚举/非枚举标题下同判，登记窄窗见
    `_bare_member_before_token` docstring）；
    枚举型标题下自 fix-r13 起 enum 门半边化——不相交半边照走 rounds 路径（计入
    判据＝前向臂配对，不相交不计＝登记抑制）；相交半边照进 foreign 拒收、不再计入
    （`#3056+3074 补 r5 … #2957 P1×3` + {3056} 修前 [5]（fix-r12）→ 拒收；全拼
    对照计 5 不变——enum 从不压全拼 rounds），见 `_enum_title` docstring。
    fix-r3 P0②：allow_unpaired=False 时免配对兜底（无 #N 全计入 / SELF）
    关闭——非 TITLE_HEAD 标题（`#3056 fix-r3：…`）只走配对分支，散文 `temp: fix-r2`
    不因去类型词化被计入。"""
    hashes = list(HASH_REF.finditer(text))
    zsplit = text.split("：", 1)
    zone_end = len(zsplit[0]) if len(zsplit) == 2 else len(text)
    # fix-r5 P0（DS r4 boundary#1，仲裁 _tmp/20261114-3074-fixr4-arb/arb-fixr4-out.txt）：
    # 题头区只认全角「：」⇒ ASCII 冒号/无冒号标题 zone_end=len(text)，尾部 #N 注记
    # （`（复用 #1321 机制）`、`: 按 #1355/#1652 …`）留在「区内」参与枚举 ⇒ enum=True ⇒
    # foreign 抑制 ⇒ fix-r2 P0 异单/no-issue 旁路在另一种拼写复开（仲裁四形里三形假 PASS）。
    # 封堵**只收窄枚举链的参与判据**（enum 门），不动 zone_end 本体——zone_end 还门着
    # SELF/RSHORT/foreign 的 token 资格，按评审建议整体取 min(：,:) 前移会把 `fix(x) #N
    # 13:42 fix-r5`（锚点后 ASCII 时间冒号）的 SELF 计入判没（假 FAIL 向）。两道附加界：
    # ①enum_cut＝首个**锚点之后**的 ASCII ':'（锚点＝首个 #N / fix-r<N> / 紧跟 ASCII
    #   冒号的 r<N> token（fix-r8）的位置；
    #   `fix(scripts):` / `chore(board):` 前缀冒号与 `01:45` 时间冒号都在锚点之前，不作界
    #   ——r4 P0-A 的 `01:45 fix-r5 同步` 计 5、`chore(board): 01:10 …` 的题头区链分布
    #   不变；锚点含 fix-r8 sibling P1 补的第三族——紧跟 ASCII 冒号的 r<N> token
    #   （`fix(scripts) r5: #9999 …` 形，见下方 anchor 行注释）；②链首落在全/半角括号深度 ≥1 处的链不参与（`修评审（复用 #1321 机制）`
    #   尾部注记形；含类型词 scope 括号族 `fix(scope,#N,#M)`——已知窄窗登记见 _enum_title
    #   docstring，fix-r6 boundary#1）。两道界只让 enum 变 False（fail-closed：foreign 只会
    #   新增命中；括号深度自 fix-r6 boundary#2 起钳 0——游离闭括号不再以负深度排除链；
    #   fix-r7 sibling#1 起按全/半角宽度分类两条——混宽镜像序不抵回 0，见 pdepth 注释），且
    #   rounds 计入分支不经过 enum——封堵面零回退；链 gap 字符（PAIR_SEP ∪ 非换行空白）
    #   不含 `:`/`（）()` ⇒ 链不跨界，「链首在界内 ⇒ 整链在界内」与 r4 P1-C 同构。
    anchor = min([h.start() for h in hashes]
                 + [m.start() for m in FIXROUND_RE.finditer(text)]
                 # fix-r8 sibling P1（MM r7 评审，协调者实测复现 _tmp/20261114-3074-mm-p1-probe/）：
                 # r<N> 简写族锚点——「轮号 token 后紧跟 ASCII 冒号」单列一道切。
                 # `fix(scripts) r5: #9999 fix-r7 修评审 #1321 机制`（正文 Closes #3056）的
                 # r5 在首个 #N 之前，其冒号落在旧锚点（#9999）之前 → 不切 enum_end →
                 # 题头区两条不相交链 {9999}/{1321} ⇒ enum 抑制 foreign ＝ fix-r2 P0
                 # 异单旁路在 r<N>+冒号拼写复开（同族 fix-r<N> 拼写被 fix-r5 P0 正确切掉，
                 # 行为分裂）。锚点**只认紧跟 ASCII 冒号的 RSHORT 形（rN:）**，不用未过滤
                 # 的裸 RSHORT——散文 `r4` 后不接冒号不锚（`chore(board) … r4 … 01:10 …`
                 # 不会把时间冒号误当代号 → 合法 board 形 foreign 误报＝假 FAIL）；
                 # 题头区外的 rN（评审轮引用 `fix-r3：r2 评审`）不接冒号，即使接了其冒号
                 # 也在 zone_end 之后，enum_end = min(zone_end, acolon) 不受影响。
                 + [m.start() for m in RSHORT.finditer(text) if text.startswith(":", m.end())],
                 default=len(text))
    acolon = text.find(":", anchor)
    enum_end = min(zone_end, acolon) if acolon >= 0 else zone_end
    # fix-r15（DS r14 sibling#1 P0，仲裁实测 `_tmp/20261113-3056-fixr14/arbitrate-r14-ds.txt`）：
    # 旧实现每个 hash 向左走整条链（无记忆化）+ 每 token 线性全扫前方 hash——连续
    # `#1#1…` k 个号 ⇒ O(k²)，实测 k=32000 达 103.9 s（每翻倍 ×4）＝闸门挂起，提交标题
    # 作者可控。改为单次左→右扫描记链头索引、每条链的配对集只物化一次（等价：左行闭包＝
    # 链连通分量）；「token 前方最近 #N」用二分取代线性全扫（同一挂起面的另一半）。
    heads: list[int] = []          # heads[i] = hash i 所在链的链头下标
    members: list[list[str]] = []  # members[链头] = 该链全部 #N（O(1) 摊还追加）
    chain_first: list[int] = []    # chain_first[链头] = 该链首个 hash 起点（题头区限定的依据）
    # fix-r10 两臂同源（DS ⑤）：链构建的「相邻 hash 间全 gap」判据从每对物化切片改为
    # _GapScan 单调指针（hash 起点升序，摊还 O(1)，无切片物化），与 RSHORT 收集臂 /
    # 前向配对臂的裸号成员豁免（_bare_member_connected）同一 helper 同一判据——
    # `#3056 +、×65 + #3074 r5` 计 5 而裸号拼写被丢的口径分裂自此消除。
    gchain = _GapScan(text)
    for i, h in enumerate(hashes):
        if i and gchain.clear(hashes[i - 1].end(), h.start()):
            heads.append(heads[i - 1])
            members[heads[i - 1]].append(h.group(1))
        else:
            heads.append(len(members))
            members.append([h.group(1)])
            chain_first.append(h.start())
    chain_sets = [frozenset(g) for g in members]
    # fix-r3 P1-1 面 A（MM boundary#1 + DS boundary#1 双撞）+ fix-r4 P1-C/P0-B：≥2 条互不相交
    # #N 链 ⇒ 枚举型/board 型标题，整条不作 foreign 判定（计入分支照旧：归属清楚的单仍计轮；
    # 只压 foreign 半边＝假 FAIL 面，不放宽计入）。fix-r4 限定**题头区内的链**才参与枚举判定
    # （gap 字符不含全角「：」→ 链不跨题头区分界，链首在题头区 ⇒ 整链在题头区）——题头区外
    # 的尾部 #N 注记（`（复用 #1321 机制）`）不再把异单失配的 foreign 封堵关掉（fix-r2 P0
    # 旁路复开面，DS sibling#1 + boundary#4）；fix-r5 P0 再收 enum_end 与括号深度两道界（见
    # 上方 zone_end 后注释）。枚举计算走 _enum_title（去重 + 公共元素短路 + 上限，O(C) 摊还
    # ——O(C²) 挂起面见该函数注释）。括号深度用单调指针单遍累计（pscan 只前进，O(n) 总量）。
    enum_chains: list[frozenset[str]] = []
    pscan = 0    # 括号深度已扫描到的位置（token 间不回退）
    # fix-r7 sibling#1（DS r6 评审 P1，协调者实测复现）：括号深度按宽度分类两条——全角
    # （）与半角 () 各自 +1/闭括号钳 0 减（钳 0 自 fix-r6 boundary#2，DS r5 评审：旧「不
    # 钳 0」声称游离闭括号让后续链停在负深度＝不参与＝fail-closed，实则一个后续开括号就把
    # 负深度抵回 0——`a)（复用 #N 机制）` 形的 P0 旁路复开），**链参与枚举须两类都为 0**。
    # 旧单深度下混宽镜像序（先开后闭：`（a) 复用 #N 机制）`——全角开 +1 被半角闭 −1 抵回
    # 0）让尾部注记链回到深度 0 参与枚举 ⇒ enum 抑制 foreign ＝ fix-r2 P0 旁路复开；
    # 分类后该形全角深度停在 1，链不参与（fail-closed 向：foreign 只会新增命中）。尾部
    # 注记形的 foreign 封堵对「先闭后开的序」（`a)（…）`）与混宽镜像序（先开后闭）都
    # 成立——各自宽度的开括号把链挡在深度 ≥1；仍开的只有「游离闭括号后**不接开括号**」
    # 的链（`a) 复盘 #N`）——与 _enum_title docstring 已登记的「无冒号且无括号的裸尾注」
    # 窄窗同族的既知代价（该形词法上与窄窗形不可分）。单调指针不变（pscan 只前进，
    # O(n) 总量）。
    fw_pdepth = 0  # text[pscan] 处的全角（）括号深度
    hw_pdepth = 0  # text[pscan] 处的半角 () 括号深度
    for c, p in enumerate(chain_first):
        while pscan < p:
            if text[pscan] == "（":
                fw_pdepth += 1
            elif text[pscan] == "(":
                hw_pdepth += 1
            elif text[pscan] == "）":
                fw_pdepth = max(0, fw_pdepth - 1)
            elif text[pscan] == ")":
                hw_pdepth = max(0, hw_pdepth - 1)
            pscan += 1
        if p < enum_end and fw_pdepth == 0 and hw_pdepth == 0:
            enum_chains.append(chain_sets[c])
    enum_title = _enum_title(enum_chains)
    ends = [h.end() for h in hashes]
    starts = [h.start() for h in hashes]
    # token 收集：FIXROUND_RE 全部 + 题头区内全部 RSHORT 简写（P0①；fix-r13 起含前方
    # 无 #N 的简写，fix-r14 起无前链简写交后向臂/不计），按位置排序。
    # fix-r4 P1-E（concurrency#2 + boundary#3）：RSHORT 的紧邻判据从「每 token 物化 gap 切片
    # 再 all()」改为下方单遍单调指针（last_nongap）——条件 `last_nongap < ends[ri]` 与旧
    # `all(gap 全为链字符)` 逐 token 等价（gap 内任一非链字符 ⇒ last_nongap ≥ 该字符位置
    # ≥ ends[ri]）。
    entries: list[tuple[int, str, int]] = [(m.start(), m.group(1), -1) for m in FIXROUND_RE.finditer(text)]
    # fix-r14 P1（DS r13 boundary#1，仲裁 _tmp/20261114-3074-fixr13-ds/coord-arbitration.md
    # ①②）：r13 把「i<0 整条丢弃」放开成一律 SELF 计入后，题头区无链前导简写（board/
    # docs/评审散文里的 rN 提及）被当修复轮＝假 FAIL 面（origin/main 3000 条标题 29 条翻转，
    # 多 N≥4）。修法＝i<0 不直接 SELF，交后向臂评价（要求后方真实链）；无链/链异侧且与
    # issues 不相交才不计不拒（r12 判），异侧相交照后向配对计入（fix-r15 P2#1，见本
    # 函数 docstring「fix-r15 P2#1」句）。窄窗登记：无链简拼真修复轮（`chore(gateway) upload 清理 r6`）回到
    # 不计、与全拼 `fix-r6` 计入的拼写分裂复开——「无链前导简写不计」的必然取舍，
    # 与 `_bare_member_before_token` docstring 窄窗同款登记（见 _scan_pairs docstring）。
    rshort_nolink: set[int] = set()
    for m in RSHORT.finditer(text):
        if m.start() >= zone_end:
            continue
        # fix-r13 协调者追加（#3074 MM sibling#2，静默放行双空族）：前方无 #N 链的
        # 题头区内简写 token 不再 `continue` 整条丢弃——旧口径 rounds=[]∧foreign=[]
        # ＝修复轮义务整段跳过的假 PASS，而全拼同形走后向臂拒收（`修 + 2932 r2
        # 交付 + #2957` 旧 ([],[]) vs `fix-r2` foreign=(2,{2957})＝简拼/全拼拼写
        # 分裂）；现以 ri=-1 进 entries（与全拼 token 同通道），由 SELF/后向臂按
        # 同口径评价（有后方链则计入/拒收），协调者实测
        # _tmp/20261114-3074-fixr13/probe-mm-sibling2.py。fix-r14 起计入半边收窄：
        # 无前链简写不再 SELF（进 rshort_nolink），只经后向臂（同侧守卫见配对循环）。
        i = bisect_right(ends, m.start()) - 1
        if i < 0:
            rshort_nolink.add(m.start())
        entries.append((m.start(), m.group(1), i))
    entries.sort()
    rounds: list[int] = []
    foreign: list[tuple[int, frozenset[str]]] = []
    tokens: list[tuple[int, str, int]] = []
    scan = 0          # 单调指针：text 已扫描到的位置（token 间不回退）
    last_nongap = -1  # 最近一个非链字符（非 _is_issue_gap_char）的位置；hash 自身也是非链字符
    gcol = _GapScan(text)  # 收集臂裸号成员豁免的精确判据（fix-r10；与 gchain/gpair 同判据独立实例）
    for start, num, ri in entries:
        while scan < start:
            if not _is_issue_gap_char(text[scan]):
                last_nongap = scan
            scan += 1
        if ri >= 0 and last_nongap >= ends[ri] \
                and not _bare_member_before_token(text, start, ends[ri], gcol.clear,
                                                  last_nongap):
            # 与前方 #N 链之间有非链字符 → 不紧邻（旧口径：gap 切片 all() 不过）。
            # fix-r9 P1-2（DS sibling#1，协调者实测复现）：间隔恰为**链尾裸号成员**
            # （`#3056+3074 r5：修闸门` 的 +3074 是链自身成员，不是断链引用）时视同
            # 紧邻——第二道豁免此前只加在前向配对臂（_bare_num_before），RSHORT 收集
            # 臂没同步 → 简写形 rounds=[] foreign=[] 整条丢弃（义务跳过＝假 PASS），
            # 而全拼形 `#3056+3074 fix-r5` 计 5、`#3056+#3074 r5` 也计 5（B 带 # 时
            # 它自己就是最近前方 hash，紧邻判据天然过）——同链三种拼写行为分裂。
            # fix-r10：豁免判据单源自 _bare_member_connected（区间恰一个纯数字 run，
            # 经 gap_clear=gcol 的 _GapScan 单调指针摊还 O(1)）**精确判定无距离上界**——
            # fix-r9 的 _BARE_WINDOW 上界在 gap=65 时把成员误判为引用（'+'*64 计 5 /
            # '+'*65 全丢，协调者实测）；且与 bm 解耦后**空格前置的裸号**
            # （`#3056 3074 r5`，gap=0 无枚举分隔符、BARE_NUM_BEFORE 不命中）也是链
            # 尾成员——与链构建臂同判据（空格是 gap 字符，`#3056 #3074` 本就并链），
            # 矩阵 g=0 裸号格的双空消除。last_nongap 由本循环单调指针顺手带出，
            # 供配对臂前向守卫同判据复用（勿再算第二遍）。
            # fix-r11 P1-2（DS boundary#1，散文间隔维度的双空残留；协调者实测
            # _tmp/20261114-3074-p1-verify/probe.md）：散文间隔（gap 含非 PAIR_SEP
            # 字符，`#3056+3074 补 r5：修闸门` 的「补」）的简写 token 不再整条丢弃
            # ——照前向臂口径进 foreign 拒收（fail-closed 多拒不漏：该轮号与链的
            # 归属被散文打断＝归属存疑，与全拼 `补 fix-r5` 计 5 的拼写分裂封堵）；
            # 枚举型标题下不相交半边照旧抑制 foreign（fix-r3 P1-1 面 A 的 board 并记
            # 形态不误伤；相交半边自 fix-r13 起照进 foreign——enum 门半边化，见下）。
            # token 在题头区内是收集臂的前置条件（zone 过滤在上文），无再判 zone。
            # fix-r12 P1-2（sibling#1，协调者裁定 b 案：确属有意 fail-closed 拒收）
            # 窄窗登记（落收集臂注释，任务书指定）：评审者署名 + 简写轮号族（`MM r16`/
            # `DS r2` 等，字母 run 收尾间隔）自 fix-r11 起进本 foreign 拒收——属有意
            # fail-closed（多拒不漏；foreign 半边钉见 test-review-roles.py fix-r3 P0
            # 负控块「不计但拒收」），修法是把引用移出题头区（首个全角「：」之后），
            # 不是补全拼——全拼 fix-rN 对本族＝假声明修复轮 N，N≥4 触发 residual
            # 连锁假 FAIL（check() 相交半边文案已按此同步）。
            # fix-r12 P1-1（concurrency#1，enum 维度残留；协调者实测 before
            # _tmp/20261114-3074-fixr12/probe-p1-before.out）：枚举型标题下此前整条
            # continue 丢弃 → ([], []) → fix_round=None → fixtype/N≥4 residual/多块
            # 一致三段义务整段跳过＝假 PASS；同结构全拼 `补 fix-r5` + 枚举链计 5——
            # 只差简拼/全拼即从「计 5（义务生效）」跳到「义务跳过」，正是要封的拼写
            # 分裂在 enum 维度的残留。修法（评审建议）：**enum 只压 foreign 半边，
            # rounds 路径照走**——不再整条丢弃，仍 tokens.append，由前向臂配对判据
            # 决定计入（链∩issues≠∅ 且裸号守卫不拦 → 计入；不相交 → rounds=[]，
            # foreign 照旧被 enum 抑制——board 并记形态各归各单的保护不变；
            # `_enum_title` docstring「只压 foreign 半边（计入分支照旧）」自本修起
            # 对简写收集臂同样兑现）。
            # fix-r13（#3074，MM boundary#1 + DS sibling#1≡boundary#2 双谱系双撞）：
            # **enum 门半边化**——enum 只压「该链 ∩ issues = ∅」的不相交半边
            # （fix-r12 修法的「rounds 路径照走」由此收窄到不相交半边）；相交半边
            # （链 ∩ issues ≠ ∅）下归属存疑 token（散文间隔/裸号引用断归属/评审
            # 署名族字母 run）照 fix-r11 P1-1 非枚举口径进 foreign 拒收、**不再
            # tokens.append**——「加一条无关链即绕过拒收」（`#3056 P1×3 fix-r2
            # 交付；#9991 复盘` 旧 ([],[])＝静默放行）与署名族 enum 计入（`#3056
            # MM r16 …；#9991` 旧 [16]＝假声明轮号，N≥4 触发 residual 假 FAIL）
            # 同轮封堵；合法计入形（紧邻/链尾裸号成员/全拼）不经本分支，计入不
            # 回退。协调者四孪生实测 _tmp/20261114-3074-fixr13/probe-mm-p1.py。
            if not enum_title or not chain_sets[heads[ri]].isdisjoint(issues):
                foreign.append((int(num), chain_sets[heads[ri]]))
                continue
        tokens.append((start, num, last_nongap))
    gpair = _GapScan(text)  # 前向臂裸号成员豁免的精确判据（fix-r10；与 gchain/gcol 同判据独立实例）
    for start, num, lng in tokens:
        if not hashes:
            # fix-r14 P1：无链前导简写（rshort_nolink）回到不计——「无 #N 全计入」兜底臂
            # 自本修起全拼专属（简写无链＝board/评审散文提及为主，r13 曾一律 SELF＝假 FAIL 面）
            if start not in rshort_nolink and allow_unpaired \
                    and not _bare_num_before(text, start):
                rounds.append(int(num))
            continue
        i = bisect_right(ends, start) - 1  # 前方最近 hash（原 prev[-1]）
        if i >= 0:
            # fix-r5 P1（DS r4 sibling#1）：前向配对臂裸号守卫——假 FAIL 防护（board 尾
            # `#2957 P1×3 + 2932 fix-r2 交付` 在 #2957∈issues 时旧照计 2）。fix-r6
            # （sibling#1 单源化）：守卫统一走 `_bare_num_before(text, start, ends[i])`，
            # 四臂不再各写一份窗口表达式；链成员豁免（chain_end）经 _bare_member_connected
            # **精确判定无距离上界**（fix-r10，gap_clear=gpair 单调指针摊还 O(1)，lng＝
            # 收集臂顺手带出的 token 前最后非 gap 字符位）。
            # fix-r10 P0（DS r9 复审 concurrency#1，协调者实测 '+'*64 计 5 / '+'*65
            # 全丢）：守卫语义收窄为**只压计入半边**——裸号引用（非链成员）不计入，
            # 但不再「不计不拒」整 token 丢弃：链 ∩ issues = ∅ 且题头区内且非枚举时
            # 照进 foreign 拒收。旧口径该形 rounds=[] foreign=[]＝fixtype/residual/
            # 多块一致义务整段跳过的假 PASS——丢弃方向是 fail-open，旧注释「整 token
            # 视为 board/交接段引用，不计、不进 foreign」的「不计不拒」半句与事实
            # 相反，已按修后真实行为改写（负例登记：谁再引入「豁免被否 → 丢弃」即红，
            # 见 test-review-roles.py fix-r10 矩阵不变式）。
            # fix-r11 P1-1（MM concurrency#1 + DS boundary#1 同根）：foreign 半边去掉
            # isdisjoint 限制——链 ∩ issues ≠ ∅ 但裸号非链成员（含散文间隔
            # `#3056 P1×3 + 2932 fix-r2`）时也进 foreign（该轮号归属存疑＝异单混写，
            # 拒收方向与不相交半边一致）——「插一个散文词即从计 2 跳到义务整段跳过」
            # 的 fail-open 拼写分裂封堵（协调者实测同上 probe.md）。
            if not _bare_num_before(text, start, ends[i], gpair.clear, lng) \
                    and not chain_sets[heads[i]].isdisjoint(issues):
                rounds.append(int(num))
            # fix-r13 P1-1（enum 门半边化，收集臂同款）：前向臂 foreign 出口在 enum 下
            # 只压不相交半边——相交半边（裸号/散文断归属）照进 foreign 拒收，三臂同口径。
            elif start < zone_end and (not enum_title
                                       or not chain_sets[heads[i]].isdisjoint(issues)):
                foreign.append((int(num), chain_sets[heads[i]]))
        # fix-r14 P1：SELF（题头区内、前方无 #N 全计入）自本修起**全拼专属**——无前链
        # 简写（rshort_nolink）落到后向臂评价，要求后方真实链；同侧守卫只压 foreign
        # 半边（fix-r15 P2#1）：链异侧且与 issues 不相交才不计不拒，异侧相交照后向
        # 配对计入
        # fix-r15 P2#2（DS r14 增量评审）：订正 r14 注释「全拼 token 不经本守卫…两半边
        # 都在区外，自然同侧」的不成立断言——board 散文全拼 `chore(state) board 12:01
        # fix-r2：coord …——#2040` 是「token 在区内、链在区外」，SELF 照计＝假 FAIL 面
        # （与简写同侧守卫同款**拼写分裂**，r14 注释只举了 `fix(gateway)：fix-r5 修
        # #3074` 两半边都在区外的形）。**该分裂登记不收口**：本修曾按「后方链在
        # 题头区界外且与 issues 不相交 → 不计」收口，真实语料 A/B 复算
        # （closure-reject-proof.txt）证伪——异侧不相交全拼的计入面静默 44 条里
        # 括注内 32 条是规范 SELF 形 `fix(scope) 名 fix-rN：…（#refs 在后）`、括号外
        # 12 条是散文里引关联单的真修复轮标题（`docs(gateway) fix-r8：把「#1739
        # 前能检出」…`），board 散文假 FAIL 形在语料 0 条落窗——收口违反「标题静默
        # 回归 0」验收，已回退。board 散文全拼形照计＝登记窄窗（假 FAIL 向，
        # 量化：语料 0 条；闭窗须词法可分板尾叙事与括注引用，待有真实回归再议）。
        elif allow_unpaired and start < zone_end and start not in rshort_nolink \
                and not _bare_num_before(text, start):
            rounds.append(int(num))  # SELF
        else:
            j = bisect_right(starts, start)  # 后方最近 hash（DS boundary#5）
            # fix-r14 P1 同侧守卫（仲裁 sugg1-battery case 2 实测发现）：无前链简写 token
            # 的后向配对要求**后方链与 token 同侧于题头区界**——token 在题头区内、链在
            # 全角「：」之后的 board 尾形（`chore(state) board 12:01 r2：coord …——#2040
            # 合入计 9 笔`）其链是各归各单的板尾引用，配对＝假归属 → 不计不拒（r12 同判；
            # 无守卫则 foreign 拒收＝比 r12/r13 都严的新假 FAIL 面）。
            # fix-r15 P2#1（DS r14 增量评审，协调者实测 probe-before.out）：守卫只压
            # **foreign 半边**——链异侧且与 issues 不相交才整条不计不拒（board 尾各归
            # 各单，r14 语义保留）；链异侧但 ∩ issues ≠ ∅（SELF 同形真修复轮
            # `fix(scripts) 修闸门 r5：#3056`，#3056∈issues——token 在区内、链在「：」后）
            # 不再 continue，落到下方后向配对按既有判据计入——旧口径该形 rounds=[]∧
            # foreign=[] → fix_round=None → fixtype/residual/多块一致整段跳过＝假 PASS。
            # 已知残余代价（A/B 复算量化，wordlist-ab-r15.txt）：board 尾链恰为本单
            # （`… r2：coord…——#3056`、#3056∈issues）也计入——「异侧相交计入」半边的
            # 必然取舍（全拼 SELF 不收窄同侧判据——fix-r15 P2#2 登记，A/B 证伪，
            # 见该臂内联注释，故无对称面）。
            # fix-r16 登记不修（MM r15 boundary#2，b/登记档）：本计入半边可能计到
            # **评审轮引用号**——题头区内括注形 `修复轮 7（Grok r7 P1 + gate6 P1/P2×2）`
            # 的 r7 与轮号声明同形（后向臂不区分 token 是轮号声明还是评审轮引用），
            # 计入后 residual 门槛（N≥4）跟着引用号走（真声明 2、引用 r7 → 取 7＝
            # 多要 residual 的假 FAIL 向；反向引用号 <4 而真实轮 ≥4 则漏检）；且计入
            # 与否取决于「：」后**最近**链是否 ∩ issues（链序换了结论就变＝词法偶然性，
            # 非归属判据）。收口须词法分开轮号声明与评审轮引用（仓内惯例
            # `修复轮 N（<评审> rN …）`），待有真实回归再议；实测记录（须人核）：
            # _tmp/20261114-3074-fixr16/review-round-ref-probe.out。
            if start in rshort_nolink and (j >= len(hashes) or starts[j] >= zone_end):
                if j >= len(hashes) or chain_sets[heads[j]].isdisjoint(issues):
                    continue
                # 落进后向配对：相交半边按既有判据（裸号守卫 + enum 门）评价——裸号
                # 引用断归属的形照走 foreign 拒收（归属存疑，三臂口径不变）。
            # fix-r4 P1-D（DS sibling#2）：后向配对分支补裸号守卫——board 尾引用形
            # `… + 2932 fix-r2 交付 + #2957 P1×3`（probe-corpus.txt board 13:42）在 #2957∈issues
            # 时旧照计 2 ＝假 FAIL；裸号口径与另外两个免配对分支单源（_bare_num_before）。
            # fix-r11 P1-3（MM sibling#3 后向臂镜像，b 档判据＝假 PASS 计 P1）：补
            # foreign 半边，相交（裸号非链成员 → 归属存疑拒收，镜像前向臂 fix-r11
            # P1-1）/不相交（异单拒收，既有语义）两半边都覆盖；题头区外（board 尾）
            # 照旧不计不拒——`start < zone_end` 守住 fix-r5 修 2 的合法形态
            # （board 尾引用别的单各归各单）。fix-r13 P1-1（enum 门半边化，收集/
            # 前向臂同款）：后向臂 foreign 出口在 enum 下只压不相交半边——相交
            # 半边照进 foreign 拒收，三臂同口径。
            if j < len(hashes) and not chain_sets[heads[j]].isdisjoint(issues) \
                    and not _bare_num_before(text, start):
                rounds.append(int(num))
            elif j < len(hashes) and start < zone_end \
                    and (not enum_title or not chain_sets[heads[j]].isdisjoint(issues)):
                foreign.append((int(num), chain_sets[heads[j]]))
    return rounds, foreign


def _paired_rounds(text: str, issues: frozenset[str]) -> list[int]:
    """_scan_pairs 的 rounds 半边（既有调用方只见计入轮号）。"""
    return _scan_pairs(text, issues)[0]


def foreign_fixround_pairs(titles: list[str], issues: frozenset[str]) -> list[tuple[str, int, frozenset[str]]]:
    """#3056 fix-r2 P0（DS boundary#1 + MM boundary 同撞，取高 P0）的两条旁路封堵依据：
    (a) 正文写合法旁路 `<!-- no-issue 理由 -->` → issues=∅；(b) 正文关联的是另一个单
    （`Closes #3042`）而标题为 `fix(gateway) #3056 fix-r5：…`。两形态共同结局：标题命中
    TITLE_HEAD+FIXROUND 但配对集 ∩ issues = ∅ → fix_round=None → fixtype 必填 / 多块
    同头 / N≥4 residual 整段跳过，而规则 2 仍放行（no-issue 是明文出口）＝修复轮机制被
    假 PASS。本函数收集 titles 中带修复轮 token（fix-r3 P0②：TITLE_HEAD 命中 **或** 行内
    fix-rN / 题头区内 rN——收集面＝题头区内全部（fix-r14 口径，见 _scan_pairs），去类型词化
    标题也从计入门进 foreign）、且题头区内带「非空
    配对链」的 (标题, 轮号, 链)——fix-r11 起链与 issues 的关系两半边对称（不相交＝异单
    旁路，fix-r2 P0 既有口径；相交但轮号 token 被裸号引用/散文间隔断开归属＝归属存疑，
    三臂见 _scan_pairs）；check() 对非空清单 fail-closed 拒收（fix-r14 P0 起 check() 也对
    正文 ECHO_HEAD 回显行跑本函数——正文兜底路径的同一拒收，见 check() fix-r14 段与
    _foreign_reject_errs）。题头区外的引用
    （board 尾）不算——那是 fix-r5 修 2 保留的合法形态（引用轮各归各单）。枚举型标题的
    抑制自 fix-r4 P1-C 起只看题头区内的链（尾部 #N 注记不构成枚举，见 _enum_title）；
    fix-r13 起抑制收窄到**不相交半边**（链 ∩ issues = ∅），相交半边照列 foreign
    （enum 门半边化，三臂同口径，见 _enum_title docstring）。**已知窄窗（fix-r14 登记，
    DS r13 补跑 boundary#3；fix-r15 P2#3 订正登记文字——旧文字「非 TITLE_HEAD…枚举
    分隔裸号 + 无链简写 + 后方 #N 链」与实际判据不符：四条钉形里两条 TITLE_HEAD、
    三条无枚举裸号，判据与类型词/裸号无关）**：题头区内、前方无 #N 的简写 token +
    **同侧**后方 #N 链（链 ∩ issues = ∅）→ 进 foreign 拒收而该标题并非修复轮标题
    （r12 基线该 token 丢弃、无输出）——fail-closed 有意行为（多拒不漏，修法是把
    引用移出题头区），全四条形：①`交接段、123 r5 交付 + #2957 P1×3`（非
    TITLE_HEAD + 枚举裸号）；②`r0 #2562 submit 路径 …（附 #2564 …）`；③`chore(board):
    12:15 /loop——pr9 r9 三家同撞 P1 已修…、#386 停在选型转人批（决策卡）`；
    ④`chore(board): 15:48——pr10b r10 / gate9；#407 修复轮 1 提交`（②③④无枚举裸号、
    ③④TITLE_HEAD；语料实测 3 条（10448 标题，wordlist-ab-r15.txt 同源）＋①形为
    测试钉构造形；近 30 真实 MERGED PR 0 触发），钉见
    test-review-roles.py fix-r14 块。"""
    out: list[tuple[str, int, frozenset[str]]] = []
    for t in titles:
        # fix-r3 P0②：门从「TITLE_HEAD 命中」放宽为「TITLE_HEAD 命中 或 行内带修复轮 token
        # （fix-rN / 题头区内 rN，收集面＝题头区内全部——fix-r14 口径）」——`#3056 fix-r3：…`
        # （非类型词开头）去类型词化后能
        # 判出轮号，若 foreign 门仍只认 TITLE_HEAD，异单旁路（`#9999 fix-r5：` 关联 #3042）
        # 会从这扇新开的计入门溜进而不被拒收。散文标题（无 token）照旧不进。
        if not (TITLE_HEAD.match(t) or FIXROUND_RE.search(t) or RSHORT.search(t)):
            continue
        for rnd, chain in _scan_pairs(t, issues)[1]:
            out.append((t, rnd, chain))
    return out


def pr_issues(masked: str) -> frozenset[str]:
    """PR 关联单号集合（修 2 配对判定的「本单」）：去围栏正文里所有 Closes/Fixes/Resolves/
    Refs #N 的 N，含续写列表的全部号（fix-r9，DS r5 boundary P0：`Closes #3056、#3057` 的
    3057 也进集合——首个之外的号丢掉会让引用它们的标题判不进本 PR 修复轮）。续写 gap
    = 非换行空白（含 U+3000/NBSP，fix-r12）或 PAIR_SEP 标点串（两侧各至多一个换行，
    fix-r13），见 ISSUE_NUM 注释；空档＝粘写。规则 2 的「有没有 issue 关联」也用本函数
    的布尔值（fix-r13 删 ISSUE_LINK 后唯一口径）；正常过闸 PR 非空；空集时配对
    token 全不计——配对判据不放大「正文没写关联」的违规。"""
    return frozenset(n for m in ISSUE_NUM.finditer(masked)
                     for n in re.findall(r"\d+", m.group("nums")))


def fix_round_from_title(title: str, issues: frozenset[str] = frozenset()):
    """单条提交标题判修复轮（fix_round_from_titles 集合主信号的逐条判据，#3056 fix-r6；
    前缀判据见 TITLE_HEAD 注释）：类型词开头前缀 + 行内 fix-r<N> **配对判定**（fix-r5 修 2，
    boundary#2 P1：#N 与 fix-r<N> 就近配对，只计 N == 本 PR 单号的轮号；无 #N 的标题全
    token 计入，宁高勿低），计入 token 取 max；不匹配返回 None（调用方走正文兜底）。
    fix-r7 的「每标题首个」只堵了高估方向（引用轮 2→8/26 抬过 N≥4 门槛＝假 FAIL，DS r6
    boundary#1）；低估方向（本单轮号不在首位——board 尾里本单在前、别的单带更大轮号在后，
    或范围只有 board 尾）取到引用轮 2、residual 因 fix_round<4 直接跳过＝假 PASS——配对
    判定双向都修。
    fix-r3 P0①+②（MM concurrency#1，修复轮识别盲＝尺子假通过）：① 行内 token 扩到
    `r<N>` 简写形（`fix(scripts) #3056 r14：…`，题头区内收集——fix-r13 起含前方
    无 #N 的简写；fix-r14 起无前链简写不再 SELF、交后向臂要求后方真实链，同侧守卫只压
    foreign 半边——无链/链异侧且与 issues 不相交才不计，异侧相交照后向配对计入
    （fix-r15 P2#1；MM sibling#2 面 + DS r13 boundary#1 收窄）；归属存疑
    （散文间隔/裸号引用断归属）走 foreign 拒收，判据单源 `_bare_member_connected`
    （判据陈述点⑧⑨见 _scan_pairs）；枚举型标题下自 fix-r13 起 enum 门半边化——
    不相交半边照走
    rounds 路径（计入由配对判据决定）、相交半边归属存疑走 foreign 拒收（见
    `_enum_title` docstring））；
    ② 不再强制类型词开头——非 TITLE_HEAD 但行内带 fix-r<N> 的标题（`#3056 fix-r3：…`）
    走**配对分支**（allow_unpaired=False：无 #N 全计入与 SELF 兜底关闭），散文 `temp: fix-r2`
    无 #N 配对即不入——去类型词化只放开「带本单 #N 配对的实修标题」，不放开散文。"""
    rounds = _scan_pairs(title, issues,
                         allow_unpaired=bool(TITLE_HEAD.match(title)))[0]
    return max(rounds) if rounds else None


def pr_commit_titles(repo: str, base: str, head: str) -> tuple[list[str] | None, str]:
    """PR 范围（<base>..<head>）提交标题集合（修复轮判定主信号，#3056 fix-r6）：
    `git -C repo log --format=%s <base>..<head>`，每行一个提交标题（%s = subject，单行），
    head 提交天然包含在集合内（base..head 含 head 不含 base，不再单独走一条路）。

    返回 (titles, err)：
    - err 非空 = **取不到**（git 起不来 / 非零退出）——fail-closed 由调用方拒收，**不得当
      「非修复轮」放行**：判不出是不是修复轮就跳过 fixtype/residual 检查，正是 r3/r4 双撞
      P0 的假 PASS 面；
    - (None, "") = 缺 --base/--head/--repo 任一（不是错误：本地 --body-file 不传 head 的
      路径＝无标题证据，与空集同口径走「首行宽松 + 全正文结构化回显扫描」兜底——fix-r14
      MM sibling#1：模块 docstring rule 4 已按此改，此处同步；fix-r8（MM r7 boundary#2）：
      fix-r6 的「有意收窄、非首行回显不再识别只保留给 CI 形态」自 fix-r3 P0③ 起作废——
      本地 / 空集 / CI 三形态同走回显扫描（check() 一律 echo_scan=True，与 :1050 一致，
      不再留互斥描述）；调用方 main 打 stderr 显式告警说明缺哪件，不静默降级，fix-r7）；
    - ([], "") = 空集：**无标题证据**的两形态——base==head（无提交可判）**或**范围内提交
      subject 全为空（过滤后空列表）——都不是 git 失败，按「无主
      信号」与本地形态同款走正文首行 + 结构化回显兜底（fix-r9 P2a：空集与 None 一样没有任何
      标题证据，回显扫描该开；DS r9
      sibling#4 + fix-r14 MM sibling#1：空 subject 提交过滤后同样得空列表，check() 的
      `not titles` 与 base==head 空集同走回显兜底，方向 fail-safe）。git log
      **空输出且 base≠head** 是坏范围（head 是 base 的
      祖先：--base/--head 传反 / base 是 head 的后代，`git rev-parse` 比对 + `git
      merge-base --is-ancestor` 判）——fail-closed 拒收写明原因，不得当合法空集静默丢主信号
      （fix-r7，DS r6 boundary#3）。空 subject 提交在集合里只是一条不命中的标题（%s 输出
      非空字节流、过滤后为空列表），不算零提交、不触发上面的坏范围判定，也不再像 fix-r5 的
      head 单标题路径那样单独 fail-closed。

    字节模式 + surrogateescape 解码（fix-r5 惯例沿用）：标题含非 UTF-8 字节不崩溃
    （text=True 的 strict 解码会抛未捕获 UnicodeDecodeError = 裸崩溃），ASCII 正则不受
    替代符影响，解码本身不会失败。"""
    if not (base and head and repo):
        return None, ""
    try:
        cp = subprocess.run(["git", "-C", repo, "log", "--format=%s", f"{base}..{head}"],
                            capture_output=True)
    except OSError as e:
        return None, f"git 起不来（{e}）"
    if cp.returncode != 0:
        return None, f"git log 退出码 {cp.returncode}：{cp.stderr.decode('utf-8', 'replace').strip()[:120]}"
    if not cp.stdout:
        # fix-r7 修 2.2（DS r6 boundary#3）：git log 零提交（空字节输出）两分——base==head 是
        # 合法空集；否则 head 必是 base 的祖先（--base/--head 传反 / base 是 head 的后代），按
        # 坏范围 fail-closed，不得当合法空集落到正文首行兜底静默丢主信号。git log 已成功 →
        # 两个 ref 都可解析；rev-parse / merge-base 起不来或非零按 git 失败同款 fail-closed。
        # 注意判据是**空字节**而不是过滤后空列表：单个空 subject 提交输出 "\n"（非空字节流），
        # 是「有提交、标题不命中」的合法形态（fix-r6 行为钉死，有负控）。
        try:
            rp = subprocess.run(["git", "-C", repo, "rev-parse", f"{base}^{{commit}}", f"{head}^{{commit}}"],
                                capture_output=True)
            anc = subprocess.run(["git", "-C", repo, "merge-base", "--is-ancestor", base, head],
                                 capture_output=True)
        except OSError as e:
            return None, f"git 起不来（{e}）"
        if rp.returncode != 0:
            return None, f"git log 空输出但 base/head 解析失败（rev-parse 退出码 {rp.returncode}）"
        shas = rp.stdout.split()
        if len(shas) == 2 and shas[0] == shas[1]:
            return [], ""
        why = ("base 不是 head 的祖先（--base/--head 传反或 base 是 head 的后代）" if anc.returncode != 0
               else "base 是 head 的祖先但范围为空（异常形态，fail-closed）")
        return None, (f"git log {base[:12]}..{head[:12]} 空输出且 base≠head：{why}"
                      "——不得当合法空集走正文兜底（fail-closed）")
    return [t.strip() for t in cp.stdout.decode("utf-8", "surrogateescape").split("\n") if t.strip()], ""


def fix_round_from_titles(titles: list[str], issues: frozenset[str] = frozenset()):
    """PR 范围提交标题集合判修复轮（**主信号**，#3056 fix-r6）：对集合内每条标题套
    fix_round_from_title 的同一判据（TITLE_HEAD 前缀 + 配对判定，fix-r5 修 2），任一命中
    即判修复轮；**跨标题取 max**——同一分支会累积 rN→rN+1 多个修复提交（fix-r3 修完又叠
    fix-r5，两个标题都在 base..head 里），当前轮=真实修复提交的最大轮，取到旧轮号会让
    N≥4 的 residual 门槛跟着旧轮走。单标题内的轮号按配对归属（见 _paired_rounds），
    不再「每标题首个」（fix-r7）/「行内 max」（fix-r6 旧码）——两个方向都有真实证据
    （_tmp/20261113-3056-fixr6/wordlist-set-coverage.txt 列 12 条首个≠max，最坏 2→26）。
    全部不命中返回 None（调用方走正文兜底）。"""
    rounds = [r for t in titles if (r := fix_round_from_title(t, issues)) is not None]
    return max(rounds) if rounds else None


def _first_line(body: str) -> str:
    """正文第一个非空行（strip 后；无则空串）——首行判定的行取单源（fix-r16 抽出，
    _first_line_round / _first_line_declares 共用，不再各写一份循环）。"""
    for line in body.split("\n"):
        s = line.strip()
        if s:
            return s
    return ""


def _first_line_round(body: str) -> int | None:
    """fix_round_from_body 的首行宽松判定（fix-r15 抽出单源）：正文第一个非空行含
    fix-r<N> 即取该轮号，无则 None——**rounds 半边**的宽松口径（fix-r4 钉死：类型词
    开头的散文行恰在首行也判修复轮，多一道误拒的 fail-closed 窄代价）。check() 对
    正文回显行的 foreign 拒收扫描自 fix-r16 起不再与它共用跳过判定——r15 的「首行
    命中即跳过」在首行即回显行（P0）/首行散文提及（P1）两个形状上把 r14 的
    desirable 拒收吞掉（仲裁 _tmp/20261114-3074-fixr15-ds/p0-p1-arbitration.md），
    foreign 半边改 `_first_line_declares`（声明形才跳过）；rounds 半边保持宽松。
    fix-r17 登记（DS r16 boundary#1，登记不修）：**取号也分叉**——本半边取
    FIXROUND_RE **首个命中**（`承接 fix-r3，本轮修 fix-r5：…` 取 3，fix-r4 宽松
    口径不变），foreign 半边只按「任一紧跟冒号」判**形状**（同串判声明形）——
    rounds 是兜底宽松、foreign 是拒收出口，两半边口径分离是有意设计（各自 docstring
    已声明），勿统一回去。"""
    first = _first_line(body)
    if not first:
        return None
    m = FIXROUND_RE.search(first)
    return int(m.group(1)) if m else None


def _first_line_declares(body: str) -> bool:
    """check() 的正文回显 foreign 扫描是否被首行早退跳过的**词法半边**（fix-r16
    P0/P1 主修，DS r15 boundary#1 + MM r15 boundary#1 双谱系双撞同根，仲裁实测
    _tmp/20261114-3074-fixr15-ds/p0-p1-arbitration.md）：**跳过仅当**首行命中且首行
    不是 ECHO_HEAD 回显行且命中 token 紧跟全/半角冒号（声明形）；其余形状（含首行
    无命中）照跑全扫描——首行自己在 check() 的 _echo_head_lines 内被扫。check() 的
    **完整跳过条件是 `titles and _first_line_declares(masked)`**（fix-r17 主修，MM r16
    boundary#1，仲裁 _tmp/20261114-3074-fixr16-mm2-probe/ARBITRATION.md：「首行是主
    信号」仅在标题路径存在时成立——本地形态 titles=None，fix_round 来自正文首行兜底，
    回显行是唯一 foreign 出口，声明形跳过＝吞掉唯一出口＝漏检；r17 当时写作
    `titles is not None`，**fix-r19 订正为 `titles and …`（非空）**——空集 [] 与
    None 同款无主信号〔pr_commit_titles docstring：base==head 空提交范围等〕，
    误并会复活同一漏检形状，DS+MM r17+r18 增量双撞 boundary#1）。

    词法判据（fix-r16 自行设计，任务书授权）：**声明形＝首行存在 FIXROUND_RE 命中
    token，其紧跟后字符（无空白）是全角「：」或半角「:」**——全角「：」是「题头→
    正文」的分界（与 _scan_pairs 的 zone_end 同一结构信号：冒号在分界前收口声明
    的轮号）；**半角「:」不与 zone_end 同源**（zone_end 只按全角切），保留它是本
    判据自设的宽面（`fix-r3: 修闸门` 半角形同判声明——fix-r17 起 MM r16 boundary#2
    选项②登记为漏检向窄窗：CI 形态半角形跳过＝比 zone_end 同源口径宽一格；本地
    形态半角形照拒＝fail-closed 向，两向端到端钉见 test-review-roles.py fix-r17
    块）；散文提及形 token 嵌在句中，后随空白/文字而非冒号（`承接 fix-r3 未完事项`）。
    行内多个 token 时任一紧跟冒号即声明形（声明的那个 token 决定**形状**，轮号仍由
    rounds 半边取首个命中——两半边取号分叉是登记在案的有意设计，见 _first_line_round
    docstring；取首个命中与否会把 `承接 fix-r3，本轮修 fix-r5：…` 误判提及）。
    三分支与方向的对应（跳过＝少拒，跑扫描＝照 r14 拒收）：
    - 首行是 ECHO_HEAD 回显行 → **不跳过**（P0：此时首行是唯一 foreign 出口，跳过
      ＝吞掉 r14 的异单拒收＝假 PASS；仲裁场景 A）；
    - 散文提及形 → **不跳过**（P1：r14 拒收是 desirable，fail-closed 照拒；仲裁
      场景 B）；
    - 声明形 → **跳过仅当标题路径存在**（CI 形态 titles 非空：首行是主信号，
      正文回显不是信号，r15 修的假 FAIL 面保持，check() 级钉 test-review-roles.py
      fix-r17 P1(b)；本地形态 titles=None 照跑——fix-r17 主修，回显行是唯一
      foreign 出口，check() 级钉 fix-r17 P1(a) 与翻转后的 fix-r15 面钉 p1_body；
      空集 [] 与 None 同走扫描——fix-r19 订正，钉在 fix-r19 块）。
    已知窄窗（登记，两向 + 一形态）：声明形不紧跟冒号（`修复fix-r5 停泊补 waker`）按
    提及处理 → 扫描照跑＝多一道误拒（fail-closed 向，与 fix-r4 钉死的 rounds 半边宽松
    首行窄代价同向）；提及形恰好紧跟冒号（`承接 fix-r3：未完事项`）按声明处理 → 跳过
    扫描＝漏检向——词法上与声明不可分，修法是把提及移出首行（或改写不紧跟冒号）；
    **形态窄窗（fix-r17 登记，须人核）**：本地形态（titles=None）下「正文手工异单
    回显 + 提交标题干净」会**本地红 CI 绿**——r15 修的假 FAIL 面对 CI 形态 PR 在本地
    预演中复活，有意取舍（fail-closed 宁拒不漏；ECHO_HEAD 三重结构〔scope 括号 +
    #N + 配对〕保证散文不进入，误拒面窄），本地形态 fixtype/residual 义务照走
    （fix_round 来自首行兜底）。两向窄窗都以「结构信号（冒号）优先于词面」取舍；
    无冒号声明形的收紧须先过 A/B 静默回归 0。"""
    first = _first_line(body)
    if not first or ECHO_HEAD.match(first):
        return False
    return any(m.end() < len(first) and first[m.end()] in "：:"
               for m in FIXROUND_RE.finditer(first))


def _echo_head_lines(body: str) -> list[str]:
    """正文内全部 ECHO_HEAD 结构化回显行（strip 后；fix-r17 抽单源，MM r16 simplify#2
    P3 随轮顺手）——check() 的 foreign 半边与 fix_round_from_body 的 rounds 半边共用
    同一行提取（同一正则、同一 strip 口径），不再各写一份循环；日后给提取加过滤条件
    只改这里，两半边不会分家。"""
    return [s for s in (ln.strip() for ln in body.split("\n")) if ECHO_HEAD.match(s)]


def fix_round_from_body(body: str, echo_scan: bool = True, issues: frozenset[str] = frozenset()):
    """**兜底**（主信号是提交标题集合）：正文第一个非空行含 fix-r<N> 即判修复轮（宽松口径，
    fix-r1 P0 的 CJK 连写首行「修复fix-r5：」靠它；已知窄代价：类型词开头的散文行恰在首行
    也判修复轮，假 FAIL 向，fix-r4 修 3.5 钉死）。**别再往正文正则里加词**（fix-r4 停点
    报告证明行内无判别量）。
    fix-r5 修 4（boundary#4 P2）：echo_scan=True（主信号缺失——本地 --body-file 不传
    --base/--head 的路径，或 base==head 空集、空 subject 过滤后空列表〔fix-r9 P2a〕）时，
    首行不命中再扫**全正文结构化回显**（ECHO_HEAD：`fix(...)
    #N fix-rM` 形态，#N 配对到本 PR 关联单才计入）——本地先跑闸门是硬步骤，只看首行会把
    「## 概述」开头的正文判非修复轮、本地过 CI 再拒。fix-r3 P0③（MM concurrency#1）：CI
    形态（titles 在但全不命中）同样走回显扫描——只看首行会让「标题全用 `#N r<N>` 简写形 +
    正文以 `## 概述` 开头」的修复轮整单逃逸四项义务；结构判据（scope 括号 + #N + 配对）
    保证散文不因回显扫描进入。
    fix-r9 P2（MM simplify#1 ≡ DS simplify#1 ≡ MM sibling#5）：**默认值改 True**——
    生产唯一调用点（check() 恒传 True）之外再无 False 调用方，默认 False 只让直调
    （测试/探针）走与生产不同的路径；参数保留（显式 False 可关，供首行语义单测用）。
    fix-r14 P0（DS r13 补跑 boundary#1，仲裁 _tmp/20261114-3074-fixr13-ds2/）：本函数
    的回显扫描只出 **rounds 半边**（_paired_rounds）——foreign 半边（enum 相交半边散文
    间隔简写 token 既不计入也不拒收＝假 PASS 回归）由 check() 对 ECHO_HEAD 行同跑
    foreign_fixround_pairs 承担（同一兜底口径，见 check() fix-r14 段）；本函数对触发形
    返回 None、由拒收兜住即验收通过（「非 None 或被拒」二选一）。
    fix-r15 P1（DS r14 增量评审，协调者实测仲裁 p1-probe.txt）：首行判定抽
    `_first_line_round` 单源——check() 的正文回显 foreign 拒收与 echo_scan 早退当时
    共用同一判定（首行命中即整体跳过），**该口径自 fix-r16 起作废**：两半边有意
    分叉——foreign 半边改 `_first_line_declares`（声明形才跳过），本函数的 rounds
    半边保持宽松口径不变（fix-r4 钉死，fail-closed 向）；fix-r17 起 foreign 半边
    跳过还须标题路径存在（titles 非空，本地形态照跑；r17 写 `titles is not None`，
    fix-r19 订正空集 [] 同走扫描），见
    `_first_line_declares` docstring 与 check() fix-r17 段——勿按旧句「两处口径
    不得再分裂」把两半边统一回去（＝复活 r15 P0 假 PASS 面）。fix-r16 P0/P1
    （DS+MM r15 双撞，仲裁
    _tmp/20261114-3074-fixr15-ds/p0-p1-arbitration.md）：共用判定收窄——check() 的
    跳过改 `_first_line_declares`（声明形才跳过：首行即回显行 / 散文提及都照跑
    foreign 扫描），本函数的首行早退（rounds 半边）保持宽松口径不变（fix-r4 钉死，
    fail-closed 向）；两半边方向不同是登记在案的有意分叉（漏检向必须收窄、误拒向
    保持宽松），判据与窄窗见 `_first_line_declares` docstring。"""
    first = _first_line_round(body)
    if first is not None or not echo_scan:
        return first
    # 行提取走 _echo_head_lines 单源（fix-r17，MM r16 simplify#2）——与 check() 的
    # foreign 半边共用同一提取，两半边不会分家。
    rounds = [r for ln in _echo_head_lines(body) for r in _paired_rounds(ln, issues)]
    return max(rounds) if rounds else None


def _kv(attrs: str) -> dict:
    """注释键值全量解析（#200）：`key=value` 空白分隔，同键多次收集成列表（调用方判重复）。"""
    kv: dict[str, list[str]] = {}
    for tok in attrs.split():
        k, sep, v = tok.partition("=")
        if sep:
            kv.setdefault(k, []).append(v)
    return kv


def _load_ci_scope():
    """懒加载同目录 ci_scope.py（「不适用」判定的唯一口径，不复制其路径白名单）；任何失败返回 None（fail-closed）。"""
    try:
        import importlib.util
        p = os.path.join(os.path.dirname(os.path.abspath(__file__)), "ci_scope.py")
        spec = importlib.util.spec_from_file_location("pdrs_ci_scope_3056", p)
        if spec is None or spec.loader is None:
            return None
        mod = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(mod)
        return mod
    except Exception:  # noqa: BLE001 —— 加载失败即 fail-closed，由调用方拒收
        return None


def _load_taskbook():
    """懒加载同目录 check-taskbook-segments.py（占位符判定 placeholder_in 与围栏屏蔽 strip_fences 的正本，
    不复制实现——DS sibling#1/boundary#2）；任何失败返回 None（调用方 fail-closed 拒收）。
    该模块自身只做模块级常量装配（含惰性加载本文件取 FIXTYPE_VALUES），加载无副作用。"""
    try:
        import importlib.util
        p = os.path.join(os.path.dirname(os.path.abspath(__file__)), "check-taskbook-segments.py")
        spec = importlib.util.spec_from_file_location("pdrs_taskbook_for_gate_3056", p)
        if spec is None or spec.loader is None:
            return None
        mod = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(mod)
        return mod
    except Exception:  # noqa: BLE001 —— 加载失败即 fail-closed，由调用方拒收
        return None


def residual_errors(masked: str, fix_round, tb) -> list[str]:
    """N≥4 的 pdrs-residual 检查（blocks 与 waiver 两分支共用，不再各写一份）。

    口径与任务书侧 check-taskbook-segments.check_fix_fields 同一正本：围栏 / 引用块 / 缩进块里的
    示例不算声明（strip_fences 后再找标记），占位符不算答（placeholder_in）。masked 是 check()
    已经去围栏的正文（修 8 同一份），tb 是 check() 已加载并校验过的任务书模块——这里不再各自
    加载，同一份文本同一份正本。"""
    if fix_round is None or fix_round < RESIDUAL_MIN_ROUND:
        return []
    if tb is None or not hasattr(tb, "placeholder_in") or not hasattr(tb, "RESIDUAL_NONE"):
        return ["pdrs-residual 口径模块（check-taskbook-segments.py）加载失败（fail-closed 拒收）"]
    errs: list[str] = []
    found = RESIDUAL_MARKER.findall(masked)
    if not found:
        errs.append(f"修复轮 fix-r{fix_round}（N≥4）PR 正文缺 <!-- pdrs-residual: 上轮残差判读 -->（无上一轮写字面值「无上一轮」）")
    elif len(found) > 1:
        errs.append("pdrs-residual 标记出现多次（至多一次）")
    else:
        content = found[0].strip()
        if not content:
            errs.append("pdrs-residual 内容是空白串（无上一轮写字面值「无上一轮」，空答不算答）")
        elif content != tb.RESIDUAL_NONE:
            hit = tb.placeholder_in(content)
            if hit:
                errs.append(f"pdrs-residual 内容还是占位符：{hit[:80]!r}（写实际判读；无上一轮写字面值「无上一轮」）")
    return errs


def check_na_path(base: str, repo: str, head: str) -> list[str]:
    """`fixtype=不适用` 的 diff 级路径检查（#3056 义务 4）：ci_scope 语义为唯一口径，fail-closed。

    fix-r2（DS r2 boundary#4 P2）口径对齐补丁指纹：路径判定基点改用 `git merge-base base head`
    （等价三点 diff）——pr-gate 传入的 --base 是「当前 base tip」（origin/<base_ref>），落后
    分支上两点 diff 会把 base 侧新增的代码文件列成 head 的反向变更，纯 state/board.md 的 PR 被
    误报「不限于文档白名单」（实测 _tmp/20261114-3074-fixr2/ds4-twopoint-threepoint-probe.txt：
    两点列 src/new_code.py + state/board.md，merge-base 基点只列 state/board.md）。错误消息按
    False 的来源分条（git 失败 / diff 为空 / 真触代码路径），不再并成一句误导定位。"""
    if not (base and head and repo):
        return ["fixtype=不适用 但缺 --base/--head/--repo，无法按 ci_scope 语义复算 diff 路径（fail-closed 拒收）"]
    mod = _load_ci_scope()
    if mod is None or not hasattr(mod, "docs_only_for_range") or not hasattr(mod, "diff_paths_for_range") \
            or not hasattr(mod, "paths_are_docs_only"):
        return ["fixtype=不适用 但 ci_scope.py 加载失败（fail-closed 拒收）"]
    # merge-base 起不来 / 非零按 fail-closed（与 pr_commit_titles 的 git 失败同口径，不得当空集放行）
    try:
        mb = subprocess.run(["git", "-C", repo, "merge-base", base, head], capture_output=True)
    except OSError as e:
        return [f"fixtype=不适用 但 merge-base 起不来（{e}）（fail-closed 拒收）"]
    if mb.returncode != 0 or not mb.stdout.strip():
        return ["fixtype=不适用 但算不出 merge-base（base/head 未知提交或 repo 不可达）（fail-closed 拒收）"]
    mb_sha = mb.stdout.decode("utf-8", "replace").strip()
    paths = mod.diff_paths_for_range(repo, mb_sha, head)
    if paths is None:
        return ["fixtype=不适用 但 diff 复算失败（git diff 非零；fail-closed 拒收）"]
    if not paths:
        return ["fixtype=不适用 但 merge-base..head 的 diff 为空——「不适用」断言了 docs-only 改动，空 diff 无法佐证"
                "（核对 head 是否为空提交 / 参数是否传对；fail-closed 拒收）"]
    if not mod.paths_are_docs_only(paths):
        return ["fixtype=不适用 与 diff 路径矛盾：改动不限于文档白名单（ci_scope 唯一口径，merge-base 基点）——"
                "触及代码路径不得写不适用"]
    return []


def patch_fingerprint(repo: str, base: str, head: str) -> str:
    """`git diff base...head` 的 `git patch-id --verbatim`：不含行号，含改动行、上下文与空白，rebase 后内容不变则不变。

    已知边界（威胁模型「防漏跑不防伪造」内接受）：patch-id 忽略行号，同一文件里把改动挪到 3 行上下文逐字相同的
    重复块处，指纹不变——只能刻意构造，不会由 rebase 或正常修改产生。

    用 --verbatim 而非 --stable：--stable 会剥掉空白，只改缩进（Python / YAML 里改语义）的提交指纹不变，会假沿用
    （2026-10-25 实测）；文件模式、新增空文件、删除、二进制改动都会改变指纹（同日实测）。

    run-review-roles.py 写块与本闸门比对共用这一个函数。空 diff 或 git 失败返回 ''（调用方据此不沿用）。
    """
    cp = subprocess.run(["git", "-C", repo, *FINGERPRINT_DIFF, f"{base}...{head}"], capture_output=True)
    if cp.returncode != 0 or not cp.stdout:
        return ""
    pid = subprocess.run(["git", "-C", repo, "patch-id", "--verbatim"], input=cp.stdout, capture_output=True)
    lines = pid.stdout.splitlines()
    # 整个 diff 只应得到一行「<patch-id> <0…0>」；多行、少字段、非 40 hex 都按算不出处理（fail closed）
    if pid.returncode != 0 or len(lines) != 1 or len(lines[0].split()) != 2:
        return ""
    word = lines[0].split()[0]
    if not re.fullmatch(rb"[0-9a-f]{40}", word):
        return ""
    # 二进制（含 .gitattributes 标 -diff 的文本）只渲染成「Binary files … differ」，内容是否进指纹取决于
    # patch-id 是否哈希 index 行——git 2.54 实测会，但不是跨版本契约。把这些文件改动前后的完整 blob id 并入指纹，
    # 内容决定指纹，与 git 版本无关；没有二进制改动时指纹就是 patch-id 本身。
    # 与主 diff 用同一组写死参数。git 2.54 实测 --numstat 本就不走 textconv（防御性统一，防版本差异）
    num = subprocess.run(["git", "-C", repo, *FINGERPRINT_DIFF, "--numstat", "-z", f"{base}...{head}"],
                         capture_output=True)
    if num.returncode != 0:
        return ""
    binary = [rec.split(b"\t", 2)[2] for rec in num.stdout.split(b"\0") if rec.startswith(b"-\t-\t")]
    if not binary:
        return word.decode()
    raw = subprocess.run(["git", "-C", repo, *FINGERPRINT_DIFF, "--raw", "--no-abbrev", f"{base}...{head}",
                          "--", *[":(literal)" + p.decode("utf-8", "surrogateescape") for p in binary]],
                         capture_output=True)
    if raw.returncode != 0 or not raw.stdout:
        return ""
    return hashlib.sha1(word + b"\n" + raw.stdout).hexdigest()


def check_block(attrs: str, head: str, patch: str = "") -> list[str]:
    errs: list[str] = []
    # 注释内全文直接 findall：roles=...\s+...\s+... 多键值里 \S+ 会在首个空格截断（P1 修复）
    pairs = re.findall(r"(\w+):([A-Z_]+)", attrs)
    roles = dict(pairs)
    dup = sorted({r for r, _ in pairs if [x for x, _ in pairs].count(r) > 1})
    if dup:
        errs.append(f"结果块角色重复 {dup}（后出现的会遮蔽前面的状态）")
    for r in REQUIRED_ROLES:
        if r not in roles:
            errs.append(f"结果块缺角色 {r}")
        elif roles[r] != "PASS":
            errs.append(f"角色 {r} 状态 {roles[r]}（只有 PASS 计入闸门；PARTIAL/TIMEOUT 须重跑或分批）")
    # #200：按完整键解析（空白分隔的 key=value），每个键恰好一次
    kv = _kv(attrs)
    one = {k: v[0] for k, v in kv.items() if len(v) == 1}
    for k in ("head", "roles", "readonly", "runner", "model"):
        if len(kv.get(k, [])) != 1:
            errs.append(f"结果块键 {k} 须恰好出现一次（现 {len(kv.get(k, []))} 次）；重跑 run-review-roles.py")
    if len(kv.get("patch", [])) > 1:
        errs.append("结果块键 patch 至多出现一次；重跑 run-review-roles.py")
    # #3056 义务 E：fixtype= 键校验——只要声明就须是字面三值（是否必填看 check() 的修复轮判定）
    if len(kv.get("fixtype", [])) > 1:
        errs.append("结果块键 fixtype 至多出现一次；重跑 run-review-roles.py")
    for v in kv.get("fixtype", []):
        if v not in FIXTYPE_VALUES:
            errs.append(f"结果块 fixtype={v} 不是字面三值（枚举式 / 结构性 / 不适用）；「结构派生」等子类字样一律写「结构性」")
    if one.get("readonly", "tools") != "tools":
        errs.append(f"结果块 readonly={one['readonly']}：只接受 readonly=tools（执行层工具白名单）")
    # codex（官方 GPT 谱系，ops 侧 `codex exec` 入口）不在 run-review-roles.py 的
    # runner 表里，但当 zai/anthropic/deepseek/minimax 全是作者线时它是可用的非作者
    # 谱系（2026-11-06 GLM 作者单实测）。这里只登记组合，四项 PASS、head/patch 与
    # 角色齐全的判据一条不减。
    prefix = {"zai": "glm-", "anthropic": "claude-", "minimax": "MiniMax-",
              "deepseek": "deepseek-", "codex": "gpt-"}
    rn, md = one.get("runner"), one.get("model", "")
    # 前缀后至少要有一个字符：`model=gpt-` 这种空后缀不算登记过的谱系（#1599 r4 评审 P3）。
    if rn is not None and (rn not in prefix or len(md) <= len(prefix[rn])
                           or not md.startswith(prefix[rn])):
        errs.append(f"结果块 runner={rn} model={md} 不是已知组合（zai→glm-*，anthropic→claude-*，minimax→MiniMax-*，deepseek→deepseek-*，codex→gpt-*）")
    # P1（boundary 审 #1）：head= 必须与当前 PR head 比对，防「推新提交后旧 PASS 块照样过闸」
    if head:
        bhead = one.get("head", "")  # 完整键、恰一次（nothead= 不算，重复 head 已在上面拒）
        short, full = sorted((bhead, head), key=len)
        if not re.fullmatch(r"[0-9a-f]{7,40}", bhead):
            errs.append(f"结果块 head=「{bhead}」不是 commit SHA（外部 diff 占位不得过闸）；重跑 run-review-roles.py")
        elif not full.startswith(short):
            bpatch = one.get("patch", "")
            # 只有审过整个 PR 的块（scope=full）才可沿用：--paths 分批块的指纹是整个 PR 的，但它只审了一部分
            if one.get("scope") != "full":
                errs.append(f"结果块 head={bhead} 与当前 PR head={head} 不符，且 scope={one.get('scope', '缺')} 不是 full，"
                            "分批块不沿用；重跑 run-review-roles.py")
            elif patch and re.fullmatch(r"[0-9a-f]{40}", bpatch) and bpatch == patch:
                print(f"check-pr-gate: 结果块 head={bhead} ≠ PR head={head}，但补丁指纹相同（patch={patch[:12]}），沿用")
            else:
                why = ("块内无 patch= 指纹" if not kv.get("patch") else "块内 patch= 不是 40 位指纹"
                       if not re.fullmatch(r"[0-9a-f]{40}", bpatch) else "当前补丁指纹算不出" if not patch
                       else f"补丁指纹不同（块 {bpatch[:12]} ≠ 当前 {patch[:12]}）")
                errs.append(f"结果块 head={bhead} 与当前 PR head={head} 不符（审计结果属于旧提交；{why}）；重跑 run-review-roles.py")
    return errs


def _overflow_err(text: str, src: str) -> str | None:
    """fix-r3 P2a：位界 {1,18} 外的超长数字串（fix-r / #N / rN 后 ≥19 位）fail-closed 报错，
    不让 int() 在 >4300 位时抛 ValueError 以 traceback 崩溃冒充 FAIL（位界内正常走判定；
    兄弟尺子 check-taskbook-segments.py:390 先例）。text 是单条标题或去围栏正文。"""
    for pat, name in ((FIXROUND_OVER, "fix-r"), (HASH_OVER, "#"), (RSHORT_OVER, "r")):
        m = pat.search(text)
        if m:
            tok = m.group(0)[:28] + "…" if len(m.group(0)) > 28 else m.group(0)
            # fix-r8 boundary#4：文案区分「#N 引用」（HASH_OVER，误伤面＝正文长数字串被
            # 误加 # 的形，登记见 HASH_OVER 注释）与「轮号 token」（fix-r / rN）；
            # fix-r9 P2（MM sibling#2）：「b 后缀」半句按族分流——HASH_OVER（#）没有
            # b 后缀（HASH_OVER = #\d{19,}），旧文案对 # 族自相矛盾。
            hint = ("正文里的 #N 引用请核对该号（正常 issue/PR 号远小于 19 位；若本意是"
                    "长数字串/片段 id，去掉 # 或加分隔）" if name == "#" else
                    "轮号请核对该 token（正常修复轮号远小于 19 位）")
            over = (f"{name} 后 ≥19 位" if name == "#"
                    else f"{name} 后 ≥19 位，可带 b 后缀")
            return (f"{src}含超长数字串「{tok}」（{over}）——超出轮号/单号"
                    f"位界 {{1,18}}，fail-closed 拒收；{hint}")
    return None


def _foreign_reject_errs(pairs, issues, src: str) -> list[str]:
    """foreign_fixround_pairs 的拒收文案（fix-r14 P0 抽出的共用件）：check() 的提交标题
    路径（src="标题"，fix-r2 P0 既有语义）与正文回显行路径（src="正文回显行"，fix-r14 P0
    ——ECHO_HEAD 行与标题同跑 foreign 拒收）三分支同构：相交半边＝归属存疑（链就在关联单里，
    矛盾在轮号 token 与链之间被裸号/散文断开）/ 异单旁路 / no-issue 旁路。文案逐字同源，
    只差 src 名词——两路径的判据与修法建议不得分裂。"""
    errs: list[str] = []
    for t, rnd, chain in pairs:
        refs = "、".join("#" + n for n in sorted(chain, key=int))
        if issues and not chain.isdisjoint(issues):
            # fix-r11 P1-1/P1-2/P1-3（文案对齐的 check() 半边）：相交半边＝链就在本 PR
            # 关联单里，再叫人「补 Refs 该单号」是假指引（已关联）——矛盾点是轮号 token
            # 与链之间被裸号引用/散文间隔断开归属（归属存疑），修法在标题不在正文。
            # fix-r12 P1-2（sibling#1，b 案第三件）：建议里去掉「全拼 fix-rN」——对
            # 评审署名族（`#3056 MM r16`，自 fix-r11 起有意 fail-closed 拒收）＝假声明
            # 修复轮 N（N≥4 触发 residual 连锁假 FAIL），是误导；改为「移出题头区或与
            # 链写紧邻」。不相交/no-issue 两分支文字不动。
            mine = "、".join("#" + n for n in sorted(issues, key=int))
            errs.append(f"修复轮{src}轮号归属存疑（fail-closed）：{src}「{t[:60]}」的 r{rnd} 与题头区链 "
                        f"{refs}（∈ 本 PR 关联单 {mine}）之间被裸号引用或散文间隔断开——配对判不出会让修复轮字段"
                        "（fixtype / residual / 多块一致）整段被跳过＝假 PASS；把轮号/评审轮引用移出题头区"
                        "（首个全角「：」之后），或与链写紧邻（去散文/裸号间隔）")
        elif issues:
            mine = "、".join("#" + n for n in sorted(issues, key=int))
            errs.append(f"修复轮{src}与 issue 关联矛盾（异单旁路，fail-closed）：{src}「{t[:60]}」的 "
                        f"fix-r{rnd} 题头区引用 {refs} 不在本 PR 关联单（{mine}）——配对不上会让修复轮字段"
                        "（fixtype / residual / 多块一致）整段被跳过＝假 PASS；正文补 Refs/Closes 该单号（多单并写）"
                        "或修正标题单号")
        else:
            errs.append(f"修复轮{src}与 issue 关联矛盾（no-issue 旁路，fail-closed）：{src}「{t[:60]}」的 "
                        f"fix-r{rnd} 题头区引用 {refs} 而正文无任何 issue 关联——修复轮 PR 不得用 "
                        "<!-- no-issue --> 绕过 fixtype/residual/多块一致；正文补 Closes/Refs #N")
    return errs


def check(body: str, head: str = "", patch: str = "", base: str = "", repo: str = "") -> list[str]:
    errs: list[str] = []
    # 换行归一到 \n：CRLF 正文的行尾 \r 会让 RESIDUAL_MARKER 的行尾锚定匹配不上（假 FAIL）；
    # 任务书侧 check-taskbook-segments.py 的换行归一是先例。后续所有正则都吃归一后的正文。
    body = body.replace("\r\n", "\n").replace("\r", "\n")
    # 修 8（DS sibling#1）：「围栏 / 引用块 / 缩进块里的示例不算声明」从 pdrs-residual 推广到
    # 结果块 / 豁免 / issue 关联 / 修复轮识别——全部走同一份去围栏文本（正本 check-taskbook-segments.py，
    # 只对齐口径不改语义）。模块加载失败 fail-closed 拒收（与 _load_ci_scope 同款，宁拒不漏）。
    tb = _load_taskbook()
    if (tb is None or not hasattr(tb, "strip_fences") or not hasattr(tb, "fence_unclosed")
            or not hasattr(tb, "placeholder_in") or not hasattr(tb, "RESIDUAL_NONE")):
        return ["围栏屏蔽口径模块（check-taskbook-segments.py）加载失败（fail-closed 拒收）"]
    # 修 6（DS boundary#1 = MM2 boundary#1）：围栏未闭合时 strip_fences 会把开栏到 EOF 整段抹掉，
    # 真标记被吞、误报「缺」——先守卫，报「围栏未闭合」fail-closed，不静默吞内容
    # （任务书侧 check-taskbook-segments.py 的 fence_unclosed 守卫是先例，两侧同一口径）。
    if tb.fence_unclosed(body):
        return ["PR 正文代码围栏没有闭合（``` 开了没收）——未闭合围栏会把它之后的声明整段吞掉，先补齐围栏再过闸（fail-closed）"]
    masked = tb.strip_fences(body)
    blocks = list(MARKER.finditer(masked))
    # #3056 fix-r6：修复轮判定主信号 = PR 范围提交标题集合（pr_commit_titles + fix_round_from_titles，
    # head 天然在集合内，head 单标题不再单独走一条路）；缺 --base/--head（本地 --body-file 路径）
    # 或集合全不命中走正文首行兜底；git 失败 fail-closed 拒收（不得当「非修复轮」放行——判不出
    # 就跳过 fixtype/residual 是 r3/r4 双撞 P0 的假 PASS 面）。git 失败时兜底照跑（正文真写着
    # fix-rN 就连字段检查一起跑），但 fail-closed 报错本身已拒收；空集（base==head）是合法状态。
    fix_round = None
    titles, terr = pr_commit_titles(repo, base, head)
    if terr:
        errs.append(f"修复轮判定 fail-closed：读不到 PR 范围（{base[:12]}..{head[:12]}）提交标题"
                    f"（{terr}）——不得当非修复轮放行；补 --base/--head/--repo（指向含这两个提交的仓库）后重跑")
    # 修 2（fix-r5，boundary#2 P1）：配对判定先取「本 PR 单号」——去围栏正文所有 Closes/
    # Fixes/Resolves/Refs #N（pr_issues 单一口径，fix-r13 起规则 2 也复用）；标题里引用别的
    # 单的轮号按 issue 号各归各的单，不再被当成本轮（既不「首个」低估、也不「行内 max」高估）
    issues = pr_issues(masked)
    # fix-r3 P2a（DS r2 boundary#2）：位界外的超长数字串先于一切 int() 转换（_scan_pairs 的
    # int(num)、下方 sorted(..., key=int)）按 fail-closed 干净报错（fence_unclosed 的先例手法：
    # 顶部早退，不带着坏输入继续往下走）。
    for t in (titles or []):
        over = _overflow_err(t, f"提交标题「{t[:40]}」")
        if over:
            # fix-r4 P2-3（DS concurrency#4 + boundary#12 双撞）：早退目的只是绕开 int()，不得
            # 丢弃已累积的 errs（如 terr 的 git 失败原因）——同一 PR 的多源失败都要报。
            return errs + [over]
    over = _overflow_err(masked, "PR 正文")
    if over:
        return errs + [over]
    # fix-r2 P0（DS r2 boundary#1 + MM r2 boundary 双谱系同撞，取高 P0）：「空 / 不相交」两条
    # 旁路 fail-closed 拒收——(a) no-issue 正文（issues=∅）+ 修复轮标题；(b) 异单关联（正文
    # Closes #3042 而标题 `fix(gateway) #3056 fix-r5：…`）。只按「计不计入」处理时 fix_round
    # 落 None、fixtype/residual/多块一致整段跳过，规则 2 又放行（no-issue 是明文出口）＝假 PASS。
    # 修复轮标题的题头区引用了本 PR 未关联的单号就拒收（题头区外的 board 尾引用除外，见
    # foreign_fixround_pairs）；拒收与 fix_round 是否判出无关（异单混入也须修关联，宁高勿低）。
    # fix-r14 P0（DS r13 补跑 boundary#1，仲裁 _tmp/20261114-3074-fixr13-ds2/）：文案构造
    # 抽 _foreign_reject_errs 共用件——正文回显行路径（fix_round_from_body 兜底）同跑拒收。
    errs.extend(_foreign_reject_errs(foreign_fixround_pairs(titles or [], issues), issues, "标题"))
    if titles is not None:
        fix_round = fix_round_from_titles(titles, issues)
    if fix_round is None:
        # 修 4（fix-r5，boundary#4 P2）：主信号缺失（本地形态，或 base==head 的空集、空 subject
        # 过滤后空列表——fix-r9 P2a：空集同样无主信号可用）时正文兜底带全正文结构化回显扫描（ECHO_HEAD + 配对）；
        # fix-r3 P0③（MM concurrency#1）：CI 形态（titles 非空但全不命中）同样走结构化回显扫描
        # ——旧「只看首行」在正文以 `## 概述` 开头即判 None，一个分支全部提交都用 `#N r<N>` 简写形
        # 时 fixtype 必填 / N≥4 residual / 多块一致 / foreign(no-issue·异单) 四项义务整段跳过。
        # 回显扫描是结构判据（ECHO_HEAD：scope 括号 + #N + 配对三重结构），散文不因它进入。
        # fix-r14 P0（DS r13 补跑 boundary#1，仲裁实测 _tmp/20261114-3074-fixr13-ds2/
        # p0-probe-out.txt）：正文回显兜底的 **foreign 半边**——fix-r13 的 enum 门半边化让
        # 收集臂对外 token 开了 foreign 出口，但该出口只由上面的 foreign_fixround_pairs(titles)
        # 消费，而 fix_round_from_body 的回显扫描只用 _paired_rounds（rounds 半边）；于是
        # enum 相交半边散文间隔简写 token（`fix(scripts) #3056+3074 补 r5 修闸门 #2957
        # P1×3` + {3056}）在正文兜底路径**既不计入也不拒收**——r12 计 5，r13 起 None 且
        # 无处拒收＝fixtype / N≥4 residual / 多块一致整段跳过的假 PASS 回归。修法（仲裁
        # 建议①）：ECHO_HEAD 行与提交标题同跑 foreign_fixround_pairs，拒收与标题路径同构
        # （_foreign_reject_errs 共用件）。同跑范围＝echo_scan 的同一兜底口径——主信号
        # 命中时正文回显本就不是信号，不放大拒收面。**fix-r15 P1（DS r14 增量评审，
        # 协调者实测仲裁 _tmp/20261114-3074-fixr15/p1-probe.txt）**：该口径自本修起真正
        # 兑现——正文首行非空行命中 fix-rN 时 fix_round_from_body 早退（`first is not
        # None … return first`），而上方扫描无条件跑＝同一体（首行 `修复fix-r5：停泊
        # 补 waker` + 后段回显行引异单）一边判 5、一边硬拒收「异单旁路」，与本注释
        # 「主信号命中时正文回显本就不是信号」自相矛盾。修法（评审建议①）：先算首行
        # 命中（_first_line_round 单源，与 fix_round_from_body 同一判定），命中即整体
        # 跳过正文回显 foreign 扫描——与 echo_scan 同条件同口径；check() 级负控钉见
        # test-review-roles.py fix-r15 块（首行命中的修复轮义务照走：fixtype/residual
        # 仍由下方 fix_round=5 触发）。（**本段「命中即整体跳过 / 同条件同口径」自
        # fix-r16 起作废**：按下段收窄为声明形、再按 fix-r17 段限定标题路径存在，
        # 勿照本段实现＝复活 r15 P0 假 PASS 面。）
        # **fix-r16 P0+P1（DS r15 boundary#1 + MM r15 boundary#1 双谱系双撞同根，
        # 协调者实测仲裁 _tmp/20261114-3074-fixr15-ds/p0-p1-arbitration.md，探针
        # p0-p1-probe.py r14/r15 对照）：首行早退收窄为声明形**——r15 的「首行命中即
        # 跳过」（_first_line_round 任意位置 search）在两个形状上把 r14 的 desirable
        # 拒收吞掉：①首行本身就是 ECHO_HEAD 回显行且引异单（场景 A：此时首行是唯一
        # foreign 出口，r14 报「异单旁路」/ r15 错误消失＝假 PASS，P0——「首行命中 ⇒
        # 回显不是信号」的前提在首行即回显行时不成立）；②首行仅散文提及轮号（场景 B：
        # `承接 fix-r3 未完事项`，search 任意位置命中即跳过——fix-r4 在 rounds 半边
        # 钉死的宽松首行（多一道误拒，fail-closed）被 r15 搬到 foreign 半边后方向
        # 反转为漏检，P1）。修法＝跳过仅当首行命中**且首行不是 ECHO_HEAD 回显行且
        # 命中 token 紧跟全/半角冒号**（声明形，判据与两向窄窗登记见
        # `_first_line_declares` docstring）；rounds 半边（fix_round_from_body 首行
        # 早退）保持宽松不收窄。check() 级双向负控钉见 test-review-roles.py fix-r16
        # 块（场景 A 修前 r15 假 PASS、修后红；场景 B 修后仍拒收；声明形 + 异单回显
        # 仍跳过不拒＝r15 面保持）。
        # **fix-r17 P1（MM r16 boundary#1，协调者实测仲裁
        # _tmp/20261114-3074-fixr16-mm2-probe/ARBITRATION.md，探针 p1-probe.py）**：
        # r16 的声明形跳过再限定**标题路径存在**——上段的前提「首行是主信号，正文
        # 回显不是信号」仅在标题路径已给出主信号（titles 非空）时成立；本兜底块
        # 恰是主信号缺失的形态：本地 --body-file（无 --base/--head/--repo）时
        # pr_commit_titles 返回 None，上方 foreign_fixround_pairs(titles or []) 是
        # 空集 ⇒ 正文回显行是唯一异单出口，声明形跳过＝吞掉唯一出口（与场景 A
        # 「首行即回显行是唯一出口」同构）＝本地过、CI 拒（同一回显行作提交标题即被
        # 拒收，fail-open 实锤）。修法＝跳过条件加标题路径存在半边（r17 写
        # `titles is not None`，fix-r19 订正为非空——空集 [] 同款无主信号）：本地
        # 形态（titles=None）照跑正文回显 foreign 扫描（fail-closed 向）；CI 形态
        # （titles 非空）声明形跳过照旧（r15 修的假 FAIL 面
        # 保持）。形态窄窗（登记，须人核）：本地形态「正文手工异单回显 + 提交标题
        # 干净」会本地红 CI 绿——r15 修的假 FAIL 面在本地预演中复活＝有意取舍
        # （宁拒不漏，ECHO_HEAD 三重结构保证散文不进入），见 _first_line_declares
        # docstring。ECHO_HEAD 行提取抽 _echo_head_lines 单源（MM r16 simplify#2
        # P3 随轮顺手）。check() 级钉见 test-review-roles.py fix-r17 块（P1(a)(b)
        # 双向 + 翻转后的 fix-r15 面钉 + 半角判据两向端到端钉）。
        # **fix-r19 P1（DS+MM r17+r18 增量双撞 boundary#1）**：`titles is not None`
        # 半边把空集 []（base==head 空提交范围等＝无主信号，pr_commit_titles
        # docstring 明写空集与 None 同款走兜底）误当「标题路径已给出主信号」→
        # 空集 CI 形态声明形首行跳过正文回显 foreign 扫描＝r17 修的漏检形状在
        # [] 态残留。订正＝空集与 None 同走扫描（`titles and …`，fail-closed）；
        # 非空 titles 的声明形跳过照旧（r15 面、r17 面都保持）。双向钉见
        # test-review-roles.py fix-r19 块。
        if not (titles and _first_line_declares(masked)):
            errs.extend(_foreign_reject_errs(
                foreign_fixround_pairs(_echo_head_lines(masked), issues), issues, "正文回显行"))
        fix_round = fix_round_from_body(masked, echo_scan=True, issues=issues)
    # fix-r20（#3074 P1-B，DS r19 boundary#1，协调者实测）：多前缀失败形预扫——r19 的
    # 线性化只证单前缀；`"<!--pr-gate:waiver x"*M`（全文无字面 `-->`）时 search 对每个
    # `<!--` 起点各把惰性 `\S.*?` 扩到串尾＝Θ(M·N)（m=8000 实测 3.04 s，1 MiB 外推
    # 分钟级＝准挂起面，正文作者可控）。WAIVER/NO_ISSUE 都要求字面 `-->`——正文不含
    # `-->` 时跳过 search，语言等价（无 `-->` 必不命中），下方 NO_ISSUE 调用点同款。
    has_close = "-->" in masked
    w = WAIVER.search(masked) if has_close else None
    if blocks:
        # 分批（--paths）评审可贴多个结果块：逐块校验，任一无效即拒
        for n, m in enumerate(blocks, 1):
            tag = f"结果块 {n}/{len(blocks)}：" if len(blocks) > 1 else ""
            errs.extend(tag + e for e in check_block(m.group(1), head, patch))
        # ---- #3056 修复轮字段（义务 E/G；详见 docstring 规则 4-6）----
        kvs = [_kv(m.group(1)) for m in blocks]
        declared = [v for kv in kvs for v in kv.get("fixtype", [])]
        if fix_round is not None:
            for n, kv in enumerate(kvs, 1):
                if not kv.get("fixtype"):
                    tag = f"结果块 {n}/{len(blocks)}：" if len(blocks) > 1 else "结果块："
                    errs.append(tag + f"修复轮（fix-r{fix_round}）须带 fixtype=<枚举式|结构性|不适用>；重跑 run-review-roles.py --fixtype …")
            # G：多块凑双谱系/分批/复跑须同头或同一补丁指纹（#3056 阻塞前置）
            if len(blocks) > 1:
                heads = [kv.get("head", [""])[0] for kv in kvs]
                patches = [kv.get("patch", [""])[0] for kv in kvs]
                if not (len(set(heads)) == 1 or (all(patches) and len(set(patches)) == 1)):
                    errs.append(f"修复轮多块不一致（#3056 G）：块头 {heads} 各不相同且补丁指纹 "
                                f"{['缺' if not p else p[:12] for p in patches]} 不统一——分批/复跑拼块须覆盖同一 head 或同一 patch 指纹")
            # N≥4：上轮残差判读（PR 正文，恰一个、非空白、非占位符、围栏内示例不算声明）
            errs.extend(residual_errors(masked, fix_round, tb))
        # 义务 4：fixtype=不适用 的 diff 级路径检查（声明即查，与轮数判定无关——
        # 「不适用」本身断言了 docs-only diff，该断言可复算就必须复算）
        if any(v == "不适用" for v in declared):
            errs.extend(check_na_path(base, repo, head))
    elif w:
        reason = w.group(1).strip()
        # fix-r20（DS r19 boundary#4）：正文另存在全空白理由的 waiver marker（含上面的
        # 跨界命中形）→ 豁免无效：置空 reason 走既有「豁免理由太短」通道（与 r18 单独
        # 空白 marker 同通道；多 marker 位形两向钉见 test-review-roles.py fix-r20 块）。
        # w 非空 ⇒ `-->` 在文，WAIVER_BLANK 的 `-->` 前提天然满足。
        if WAIVER_BLANK.search(masked):
            reason = ""
        # 声明即查（MM sibling#7；与结果块侧 :check_block 的 fixtype 校验同口径）：标记内声明了
        # fixtype 就查值，是否必填才看修复轮判定——非修复轮写 fixtype=结构派生 / 不适用 也不再静默放行。
        # fix-r3 P1-2（MM sibling#1）：fixtype= 载体**至多一次**（块侧同口径）——旧码只取首个匹配，
        # `… fixtype=结构性 fixtype=不适用 …` 的第二个载体既不被校验、也不触发 check_na_path
        # ＝「不适用」的 docs-only 断言免复算（假 PASS 面）。全匹配遍历：任一值非法/带尾即报错，
        # 任一为「不适用」即触发 check_na_path。
        ms = list(WAIVER_FIXTYPE.finditer(reason))
        if len(ms) > 1:
            errs.append(f"豁免理由内 fixtype= 载体出现 {len(ms)} 次（至多一次；与结果块侧 fixtype"
                        "「至多出现一次」同口径）——多载体第二个不被校验＝「不适用」的 docs-only"
                        " 断言免复算，合并成一个载体再过闸")
        if fix_round is not None and not ms:
            # 义务 3：waiver 轮载体——修法类型写在豁免标记内；豁免理由长度不计 fixtype 词汇
            errs.append(f"修复轮（fix-r{fix_round}）的豁免标记须携带修法类型：fixtype=<枚举式|结构性|不适用>")
        fv_any_na = False
        for m2 in ms:
            # 值域已收紧到三值字符集：紧贴文字并不进值，但值后若还跟着非空白（「结构性，纯文档」
            # 「结构派生」）说明写的不是字面三值载体——判缺/非法而不是把尾巴并进值放行（DS boundary#3）
            v = m2.group(1)
            tail = reason[m2.end():m2.end() + 1]
            if v not in FIXTYPE_VALUES or (tail and not tail.isspace()):
                why = "值后不得紧跟其它文字（用空格或结尾分隔）" if tail and not tail.isspace() \
                    else "「结构派生」等子类字样一律写「结构性」"
                errs.append(f"豁免 fixtype={v}{tail if tail and not tail.isspace() else ''} 不是字面三值载体"
                            f"（枚举式 / 结构性 / 不适用；{why}）")
            if v == "不适用":
                fv_any_na = True
        if fix_round is not None:
            # N≥4：上轮残差判读（同 blocks 分支口径：恰一个、非空白、非占位符、围栏内示例不算声明）
            errs.extend(residual_errors(masked, fix_round, tb))
        plain = WAIVER_FIXTYPE.sub("", reason).strip()
        if len(plain) < 8:
            errs.append("豁免理由太短（≥8 字，不计 fixtype 词汇）")
        else:
            print(f"check-pr-gate: 豁免——{plain}")
        if fv_any_na:
            errs.extend(check_na_path(base, repo, head))
    else:
        errs.append("缺角色审计结果块（python3 scripts/run-review-roles.py 生成 summary.md，贴进 PR）或 pr-gate:waiver 豁免")
    if not issues and not (has_close and NO_ISSUE.search(masked)):
        errs.append("缺 issue 关联：Closes/Fixes #N（合并即关）或 Refs #N（单保持开），或 <!-- no-issue 理由 -->")
    return errs


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    g = ap.add_mutually_exclusive_group()
    g.add_argument("--body-file")
    g.add_argument("--pr")
    ap.add_argument("--head", default=os.environ.get("PR_HEAD", ""), help="PR 当前 head SHA；给定时与结果块 head= 比对，并作为修复轮判定的主信号源（与 --base 同给时取 base..head 提交标题集合，须 --repo 可达）")
    ap.add_argument("--base", default=os.environ.get("PR_BASE", ""), help="PR base SHA；与 --head 同给时算当前补丁指纹（head 不等时比对）、取修复轮主信号（base..head 提交标题集合）并复算 fixtype=不适用 的 diff 路径")
    ap.add_argument("--repo", default=".", help="算补丁指纹、复算「不适用」路径、读 PR 范围提交标题（修复轮主信号，须 --base 与 --head 同给）的 git 仓库（须含 base 与 head 两个提交）")
    a = ap.parse_args()
    if a.body_file:
        body = open(a.body_file, encoding="utf-8").read()
    elif a.pr:
        cp = subprocess.run(["gh", "pr", "view", a.pr, "--json", "body", "--jq", ".body"], capture_output=True, text=True)
        if cp.returncode != 0:
            print(f"check-pr-gate: gh 失败：{cp.stderr.strip()}", file=sys.stderr)
            return 2
        body = cp.stdout
    elif "PR_BODY" in os.environ:
        body = os.environ["PR_BODY"]
    else:
        print("check-pr-gate: 需要 --body-file / --pr / 环境变量 PR_BODY", file=sys.stderr)
        return 2
    patch = patch_fingerprint(a.repo, a.base, a.head) if a.head and a.base else ""
    # fix-r7 修 2.1（DS r6 boundary#2）：--base/--head/--repo 只给一部分（如给了 --head 缺
    # --base）不得静默降级为「无主信号」——先打 stderr 显式告警说明缺失原因，再走正文首行
    # 兜底；三件全缺（本地 --body-file 不传 head 的路径）同告警。只消灭静默，不改变返回语义。
    sig_missing = [f"--{n}" for n, v in (("base", a.base), ("head", a.head), ("repo", a.repo)) if not v]
    if sig_missing:
        print(f"check-pr-gate: 修复轮主信号（PR 范围提交标题集合，git log <base>..<head>）不可用："
              f"缺 {' '.join(sig_missing)}——已降级为正文兜底（首行宽松 + 全正文结构化回显扫描"
              f" fix(...) #N fix-rM 形态，#N 须配对到本 PR 关联单，fix-r5 修 4；"
              f"权威路径请 --base 与 --head 同给，且 --repo 指向含这两个提交的仓库）", file=sys.stderr)
    errs = check(body, a.head, patch, a.base, a.repo)
    for e in errs:
        print(f"check-pr-gate: FAIL {e}")
    if not errs:
        print("check-pr-gate: PASS")
    return 1 if errs else 0


if __name__ == "__main__":
    sys.exit(main())
