# git mv 后再 Edit，内容改动必须重新 add 并在提交树复核

**事件**：2026-11-12 PR 2957 fix-r8（#1623）：`git mv` R15→R18 改号后用编辑器改了文件内容（frontmatter `id:` 与编号沿革），首次 `git add` 命令因仍含已不存在的旧 R15 路径整体失败；重试的 add 命令只写旧路径漏了新路径——提交只含纯 rename（similarity 100%），**提交树内 `id:` 仍是 R15**。工作树是对的：在工作树上跑 `check_frozen.py` PASS，但冻结尺子在提交树上会 FAIL——尺子假 PASS，两轮评审（DS+MM 独立同撞）才抓出。

**规则**：`git mv` 只是暂存 rename；之后对同一文件的任何内容编辑都产生**未暂存**改动，rename 记录不会连带它。add 因路径失效失败后，重试命令很容易照抄旧路径清单。

**How to apply**：`git mv` + Edit 的组合，提交前必做三件：① `git status --short` 逐文件核对（M 在暂存列才算全进）；② `git diff --cached --stat` 对照预期文件数；③ `git show HEAD:<新路径> | head` 确认关键行（如 frontmatter id）确实进了提交树。冻结件在**提交树**上复跑核验（`git stash` 不要用——跨 worktree 共享栈；工作树干净时直接跑即提交树）。
