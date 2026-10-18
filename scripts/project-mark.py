#!/usr/bin/env python3
"""project-mark.py — 维护组织看板「pilotdeck-rs 重构」（daqi-dev Project #1）上 issue / PR 的状态与健康字段。

协调者在已经要做的动作里顺带调用一次，不另设维护步骤：
  开工           project-mark.py 127 --status "In Progress"
  派一轮评审     project-mark.py 255 --review+1          （PR 号；首次会把状态设为 In Review）
  换人实现       project-mark.py 255 --redispatch+1 --impl Claude
  阻塞           project-mark.py 19 --status Blocked --blocked 等人批
  解除阻塞       project-mark.py 19 --status "In Progress"   （自动清空阻塞原因）
  补录           project-mark.py 255 --group A --impl codex   （不在看板上会先加入）
实现方不给时按 PR 分支前缀推断（codex/ glm/ deepseek/ claude/）。字段与选项 ID 见同目录 project-fields.json。

看板是汇总视图，不是正本（READY 与 PR 结果块才是）：
- **尽力而为**：GitHub 调用失败只往 stderr 告警、退出码 0，不打断协调者用 && 串起来的主流程；`--strict` 才按失败退出 1。
- **计数允许偶发少计**：评审轮数 / 重派次数是读后写，不是原子操作；两个会话同时对同一条加 1 会少记一次。以 PR 描述的评审表为准。
退出码：0 成功或已告警 · 1 失败且给了 --strict · 2 参数错
"""
from __future__ import annotations

import argparse
import json
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
FIELDS = json.loads((ROOT / "scripts" / "project-fields.json").read_text(encoding="utf-8"))
REPO_OWNER, REPO_NAME = "daqi-dev", "pilotdeck-rs"
PROJECT = FIELDS["project"]["id"]
PREFIX_IMPL = {"codex/": "codex", "glm/": "GLM", "deepseek/": "DeepSeek", "claude/": "Claude"}


STRICT = "--strict" in sys.argv


class GhFailed(Exception):
    pass


def gql(query: str, variables: dict) -> dict:
    last = ""
    for _ in range(4):  # 网络 EOF 偶发，重试
        r = subprocess.run(["gh", "api", "graphql", "--input", "-"],
                           input=json.dumps({"query": query, "variables": variables}), capture_output=True, text=True)
        if r.returncode == 0:
            data = json.loads(r.stdout)
            if "errors" not in data:
                return data["data"]
            last = json.dumps(data["errors"], ensure_ascii=False)[:400]
        else:
            last = r.stderr.strip()[:400]
        time.sleep(3)
    raise GhFailed(last)


ITEM_Q = """query($o:String!,$r:String!,$n:Int!){repository(owner:$o,name:$r){issueOrPullRequest(number:$n){
  __typename ... on Issue{id} ... on PullRequest{id headRefName}
  ... on Issue{projectItems(first:10){nodes{id project{id} fieldValues(first:20){nodes{
     ... on ProjectV2ItemFieldNumberValue{number field{... on ProjectV2FieldCommon{name}}}}}}}}
  ... on PullRequest{projectItems(first:10){nodes{id project{id} fieldValues(first:20){nodes{
     ... on ProjectV2ItemFieldNumberValue{number field{... on ProjectV2FieldCommon{name}}}}}}}}}}}"""


def find_item(n: int) -> tuple[str, str, str, dict]:
    node = gql(ITEM_Q, {"o": REPO_OWNER, "r": REPO_NAME, "n": n})["repository"]["issueOrPullRequest"]
    if not node:
        print(f"project-mark: #{n} 不存在", file=sys.stderr)
        sys.exit(2)
    nums: dict[str, float] = {}
    item = ""
    mine = [it for it in node["projectItems"]["nodes"] if it["project"]["id"] == PROJECT]
    if len(mine) > 1:
        print(f"project-mark: 警告：#{n} 在看板上有 {len(mine)} 条，只改第一条", file=sys.stderr)
    if mine:
        item = mine[0]["id"]
        for fv in mine[0]["fieldValues"]["nodes"]:
            if fv and "number" in fv:
                nums[fv["field"]["name"]] = fv["number"]
    if not item:
        item = gql("mutation($p:ID!,$c:ID!){addProjectV2ItemById(input:{projectId:$p,contentId:$c}){item{id}}}",
                   {"p": PROJECT, "c": node["id"]})["addProjectV2ItemById"]["item"]["id"]
    return item, node["__typename"], node.get("headRefName") or "", nums


def set_select(item: str, field: str, option: str) -> None:
    f = FIELDS[field]
    if option not in f["options"]:
        print(f"project-mark: {field} 没有选项「{option}」，可选：{'/'.join(f['options'])}", file=sys.stderr)
        sys.exit(2)
    gql("mutation($p:ID!,$i:ID!,$f:ID!,$o:String!){updateProjectV2ItemFieldValue(input:{projectId:$p,itemId:$i,fieldId:$f,"
        "value:{singleSelectOptionId:$o}}){projectV2Item{id}}}", {"p": PROJECT, "i": item, "f": f["id"], "o": f["options"][option]})


def set_number(item: str, field: str, value: float) -> None:
    gql("mutation($p:ID!,$i:ID!,$f:ID!,$v:Float!){updateProjectV2ItemFieldValue(input:{projectId:$p,itemId:$i,fieldId:$f,"
        "value:{number:$v}}){projectV2Item{id}}}", {"p": PROJECT, "i": item, "f": FIELDS[field]["id"], "v": value})


def clear(item: str, field: str) -> None:
    gql("mutation($p:ID!,$i:ID!,$f:ID!){clearProjectV2ItemFieldValue(input:{projectId:$p,itemId:$i,fieldId:$f}){projectV2Item{id}}}",
        {"p": PROJECT, "i": item, "f": FIELDS[field]["id"]})


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("number", type=int, help="issue 或 PR 号")
    ap.add_argument("--status", help="Todo / In Progress / In Review / Blocked / Done")
    ap.add_argument("--group", help="阶段组：A/B/C/D/F/工具")
    ap.add_argument("--impl", help="实现方：codex/GLM/DeepSeek/Claude/人")
    ap.add_argument("--blocked", help="阻塞原因：等人批/等前置/通道不可用/CI（同时把状态设为 Blocked）")
    ap.add_argument("--review+1", dest="review_inc", action="store_true", help="评审轮数加 1")
    ap.add_argument("--redispatch+1", dest="redispatch_inc", action="store_true", help="重派次数加 1")
    ap.add_argument("--strict", action="store_true", help="GitHub 调用失败时退出 1（默认只告警）")
    a = ap.parse_args()
    try:
        return run(a)
    except GhFailed as e:
        print(f"project-mark: 警告：看板未更新（{e}）", file=sys.stderr)
        return 1 if a.strict else 0


def run(a: argparse.Namespace) -> int:

    item, kind, head, nums = find_item(a.number)
    done = []
    impl = a.impl or next((v for p, v in PREFIX_IMPL.items() if head.startswith(p)), None)
    if impl:
        set_select(item, "实现方", impl); done.append(f"实现方={impl}")
    if a.group:
        set_select(item, "阶段组", a.group); done.append(f"阶段组={a.group}")
    status = a.status
    if a.blocked:
        set_select(item, "阻塞原因", a.blocked); done.append(f"阻塞原因={a.blocked}")
        status = status or "Blocked"
    if a.review_inc:
        v = nums.get("评审轮数", 0) + 1
        set_number(item, "评审轮数", v); done.append(f"评审轮数={v:g}")
        status = status or "In Review"
    if a.redispatch_inc:
        v = nums.get("重派次数", 0) + 1
        set_number(item, "重派次数", v); done.append(f"重派次数={v:g}")
    if status:
        set_select(item, "Status", status); done.append(f"Status={status}")
        if status != "Blocked" and not a.blocked:
            clear(item, "阻塞原因")
    print(f"project-mark: #{a.number}（{kind}）{'，'.join(done) or '已在看板上，未改字段'}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
