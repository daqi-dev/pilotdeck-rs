---
name: mixed-authorship-attribution
description: 混合署名 PR 的「主实现方」口径（看板 impl 写家族并集 + 最大 diff 行数方作主实现）与评审独立性（参与过即作者，评审取补集）；不管单一家族 PR——那仍直接写该家族
type: feedback
---

**为什么**：11-05 晚合入的 8 张 PR 里有 3 张同一条内有 GLM + Claude 的 `Co-Authored-By`（#1375 = zhipu×2+anthropic×1、#1493 = zhipu×1+anthropic×3、#1418 = anthropic×11+zhipu×1）。收尾按「提交数多数决」硬塞单一实现方：既不可复算（换个算法结果就变），也没回答「两家都写过之后谁还能当独立评审」。#1418 是 11:1，把 GLM 的一条也算成 Claude 的实现。2026-11-06 人批 #1544「C + B 组合」（人评论 https://github.com/daqi-dev/pilotdeck-rs/issues/1544#issuecomment-6679827406），本条把人话变成可执行规则。

**怎么用**：

1. **看板 impl 写并集（C）**：`python3 scripts/project-mark.py <N> --impl "GLM+Claude"`（按首次实现提交的先后排序）。单一家族照旧直接写该家族，不要为了「整齐」也给单一谱系加号。
2. **单一「主实现方」只用于排序统计（B = 最大 diff 行数方）**，口径必须可复算。在 PR 分支上跑（`git rev-list` + `--numstat` 按 `Co-Authored-By` 家族聚合新增+删除行；一个提交多家族时按家族数均分）：

   ```bash
   range=origin/main..HEAD        # 或 <base>..<head>
   for c in $(git rev-list --no-merges "$range"); do
     fams=$(git log -1 --format='%(trailers:key=Co-Authored-By,valueonly)' "$c" \
            | grep -oiE 'zhipu|anthropic|openai|deepseek|minimax' | tr 'A-Z' 'a-z' | sort -u | paste -sd, -)
     [ -z "$fams" ] && fams="none"
     lines=$(git show --numstat --format= "$c" | awk 'NF==3{a+=$1; d+=$2} END{print a+d+0}')
     echo "$fams $lines"
   done | awk '{n=split($1,f,","); for(i=1;i<=n;i++) tot[f[i]]+=$2/n} END{for(k in tot) printf "%-10s %8.0f\n", k, tot[k]}' | sort -k2 -nr
   ```

   输出行数最大者即主实现方；报告里写「主实现方（最大 diff 行数）= X」。平局取首个实现提交所属家族。11-06 在 `origin/main~150..origin/main` 实测可跑（zhipu 24396 / anthropic 14830 / deepseek 6550 / openai 2912）。
3. **评审独立性（C：参与过即作者）**：作者家族 = `<base>..HEAD` 所有 `Co-Authored-By` 家族的并集，**再加上协调者（Claude）只要它提交过代码**。评审两家从补集里选；`~/ws/pdrs-ops/bin/review` 的 `lib/review-family-guard.sh` 已按交集拦截（exit 3 并列出可用组合），但直接调 `python3 scripts/run-review-roles.py` 不经过守卫，必须手工核。
4. **旧提交不改**（CLAUDE.md）：规则只对新收尾生效。三张历史 PR 按新口径记为 #1375 = `GLM+Claude`、#1493 = `GLM+Claude`、#1418 = `Claude+GLM`，不重写 commit 也不改已合并 PR 正文。
5. 相关：[[reviewer-family-check]]（派评审先列作者家族）、[[glm-claude-code-trailer]]（`glm_impl.py` 走 claude -p 会自动加 `Claude Code` 署名 → 把 GLM 单悄悄变成混合谱系，推送前 grep 尾注）。
