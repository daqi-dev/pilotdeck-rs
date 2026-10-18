#!/usr/bin/env bash
# enqueue.sh <PR号> — 把 PR 加入 main 的 merge queue 并等到合入（或出队/超时）。
# 为什么不用 gh pr merge：本仓库未开 auto-merge，gh pr merge 会走 enablePullRequestAutoMerge 而报错
# （2026-10-18 实测「Auto merge is not allowed for this repository」）；这里直接调 GraphQL enqueuePullRequest。
# 退出码：0 已 MERGED · 1 出队/失败/超时 · 64 参数错
set -euo pipefail
N="${1:-}"; case "$N" in ''|*[!0-9]*) echo "用法：scripts/enqueue.sh <PR号>" >&2; exit 64 ;; esac
ID="$(gh pr view "$N" --json id --jq .id)"
gh api graphql -f query='mutation($id:ID!){enqueuePullRequest(input:{pullRequestId:$id}){mergeQueueEntry{state position}}}' \
  -f id="$ID" --jq '.data.enqueuePullRequest.mergeQueueEntry | "enqueue: \(.state) 位置 \(.position)"'
for _ in $(seq 1 120); do
  st="$(gh pr view "$N" --json state --jq .state)"
  [ "$st" = MERGED ] && { echo "enqueue: #$N 已合入"; git fetch -q origin && git log origin/main --format='%h %ae %ce' -3; exit 0; }
  q="$(gh api graphql -f query='query($id:ID!){node(id:$id){... on PullRequest{mergeQueueEntry{state}}}}' -f id="$ID" --jq '.data.node.mergeQueueEntry.state // "NONE"')"
  [ "$q" = NONE ] && [ "$st" = OPEN ] && { echo "enqueue: #$N 已出队（检查失败或有新推送），未合入" >&2; exit 1; }
  sleep 15
done
echo "enqueue: 等待超时（30 分钟），#$N 仍在队列" >&2; exit 1
