use serde_json::Value;
fn s<'a>(v: &'a Value, key: &str) -> &'a str {
    v[key].as_str().unwrap_or("")
}

pub fn workspace(zh: bool, project: &str, title: &str) -> String {
    if zh {
        format!(
            "你正在为 Always-On 计划执行准备一个隔离工作区。\n\n项目根目录: {project}\n计划: \"{title}\"\n\n可用的工作区策略:\n  - `git-worktree`: 在新分支上创建 git worktree。速度快、空间占用少 (使用硬链接)。\n    要求项目是 git 仓库且至少有一次提交。若工作区存在未提交更改，Always-On\n    会先提交当前全部改动作为 checkpoint，再创建 worktree。\n  - `snapshot-copy`: 复制项目目录 (在 APFS/btrfs 上使用 CoW)。适用于任何目录,\n    但占用更多磁盘空间。默认忽略 .git、node_modules、dist。\n\n权限: 本轮运行在 `bypassPermissions` 模式下——所有工具调用均自动允许。\n\n## 执行步骤\n1. 检查项目根目录状态 (如 git 仓库可执行 `git status --porcelain`, 否则执行 `ls`)。\n2. 调用 `always_on_prepare_workspace`, 传入选定的策略, 或传入 `auto` 让运行时自动选择。"
        )
    } else {
        format!(
            "You are preparing an isolated workspace for an Always-On plan execution.\n\nProject root: {project}\nPlan: \"{title}\"\n\nAvailable workspace strategies:\n  - `git-worktree`: Creates a git worktree on a new branch. Fast and space-efficient (hard-links).\n    Requires a git repo with at least one commit. If the working tree is dirty, Always-On\n    checkpoints all current changes before creating the worktree.\n  - `snapshot-copy`: Copies the project directory (CoW on APFS/btrfs). Works for any directory\n    but uses more disk space. Ignores .git, node_modules, dist by default.\n\nPermissions: this turn runs in `bypassPermissions` mode — every tool call is auto-allowed.\n\n## What to do\n1. Check the project root state (e.g. `git status --porcelain` if it looks like a git repo, or `ls` otherwise).\n2. Call `always_on_prepare_workspace` with the chosen strategy, or `auto` to let the runtime decide."
        )
    }
}

pub fn execute(zh: bool, ws: &Value, body: &str) -> String {
    let strategy = s(ws, "strategy");
    let cwd = s(ws, "cwd");
    if zh {
        format!(
            "你正在隔离工作区内执行一个 Always-On 发现计划。\n工作区策略: {strategy}\n工作区路径: {cwd}\n\n权限: 本轮运行在 `bypassPermissions` 模式下——所有工具调用均自动允许。\n安全边界为工作区本身; 请勿 cd 到工作区外部, 请勿修改用户的项目根目录。\n\n## 计划\n{}\n\n## 执行步骤\n1. 按顺序执行 Execution Steps 中的各项步骤。\n2. 运行 Verification 列表中的检查项并记录结果。\n3. 回复执行总结及验证结果。",
            body.trim()
        )
    } else {
        format!(
            "You are executing an Always-On discovery plan inside an isolated workspace.\nWorkspace strategy: {strategy}.\nWorkspace cwd: {cwd}\n\nPermissions: this turn runs in `bypassPermissions` mode — every tool call is auto-allowed.\nSafety boundary is the workspace itself; do NOT cd outside it, do NOT touch the user's project root.\n\n## Plan\n{}\n\n## What to do\n1. Execute the Execution Steps in order.\n2. Run the Verification list and record results.\n3. Respond with a summary of what was done and the verification outcomes.",
            body.trim()
        )
    }
}

pub fn report(zh: bool, ws: &Value, body: &str) -> String {
    let strategy = s(ws, "strategy");
    let cwd = s(ws, "cwd");
    if zh {
        format!(
            "你正在为已完成的 Always-On 计划执行撰写工作报告。\n工作区策略: {strategy}\n工作区路径: {cwd}\n\n权限: 本轮运行在 `bypassPermissions` 模式下——所有工具调用均自动允许。\n\n## 已执行的计划\n{}\n\n## 执行步骤\n1. 查看工作区中的变更 (如 `git diff --stat`、`ls`、阅读相关文件)。\n2. 总结执行情况: 执行了哪些步骤、修改了哪些文件、命令输出、验证结果。\n3. 调用 `always_on_report` 恰好一次, 提交完整的工作报告 markdown。\n\n每个章节必须使用 `##`（h2）标题——例如 `## Plan Reference`、`## Steps Performed` 等。\n报告章节按以下顺序排列: Plan Reference, Steps Performed, Files Changed, Command Output, Verification Results, Follow-ups, Notes。\n缺失的章节将由运行时自动补全。",
            body.trim()
        )
    } else {
        format!(
            "You are writing a work report for a completed Always-On plan execution.\nWorkspace strategy: {strategy}.\nWorkspace cwd: {cwd}\n\nPermissions: this turn runs in `bypassPermissions` mode — every tool call is auto-allowed.\n\n## Plan that was executed\n{}\n\n## What to do\n1. Review the workspace to see what changed (e.g. `git diff --stat`, `ls`, read relevant files).\n2. Summarize the execution: what steps were performed, which files were changed, command outputs, and verification results.\n3. Call `always_on_report` exactly once with the full work-report markdown.\n\nEach section MUST use `##` (h2) headers — e.g. `## Plan Reference`, `## Steps Performed`, etc.\nRequired sections in order: Plan Reference, Steps Performed, Files Changed, Command Output, Verification Results, Follow-ups, Notes.\nMissing sections will be filled by the runtime fallback.",
            body.trim()
        )
    }
}

#[allow(clippy::too_many_arguments)]
pub fn apply(
    zh: bool,
    project: &str,
    name: &str,
    id: &str,
    title: &str,
    ws: &Value,
    diff: &Value,
) -> String {
    let mut lines = if zh {
        vec![
            format!("Always-On 应用变更到项目 \"{name}\"。"),
            "".into(),
            "你的任务是将隔离工作区中的变更合并到项目根目录。".into(),
            "".into(),
            "不要进入计划模式。".into(),
            "不要创建新计划——直接应用现有变更。".into(),
            "".into(),
            format!("计划: \"{title}\" ({id})"),
            format!("项目根目录: {project}"),
        ]
    } else {
        vec![
            format!("Always-On apply for project \"{name}\"."),
            "".into(),
            "Your job is to merge changes from the isolated workspace into the project root."
                .into(),
            "".into(),
            "Do not enter Plan Mode.".into(),
            "Do not create a new plan — apply the existing changes directly.".into(),
            "".into(),
            format!("Plan: \"{title}\" ({id})"),
            format!("Project root: {project}"),
        ]
    };
    lines.push(format!(
        "{}: {} ({})",
        if zh {
            "隔离工作区"
        } else {
            "Isolated workspace"
        },
        s(ws, "cwd"),
        s(ws, "strategy")
    ));
    if let Some(branch) = ws["metadata"]["branchName"]
        .as_str()
        .filter(|b| !b.is_empty())
    {
        lines.push(format!(
            "{}: {branch}",
            if zh {
                "工作区分支"
            } else {
                "Workspace branch"
            }
        ));
    }
    lines.extend(if zh { vec!["", "## 合并方式", "", "根据实际情况选择最佳的合并策略。你可以完全使用 git 和 shell 工具。", "常见方式 (根据情况选择):", "  - `git merge` / `git merge --no-ff` 如果工作区在命名分支上", "  - `git cherry-pick` 针对单个提交", "  - `git diff` + `git apply` 基于补丁的应用", "  - 使用 Edit/Write 工具直接编辑文件, 适用于精确的小范围变更", "", "如果遇到冲突, 请智能解决——不要盲目覆盖。", "如果无法解决冲突, 保留标准冲突标记 (<<<< / ==== / >>>>)。", ""] } else { vec!["", "## Merge approach", "", "Choose the best merge strategy based on the situation. You have full access to git and shell tools.", "Common approaches (pick whichever fits):", "  - `git merge` / `git merge --no-ff` if the workspace is on a named branch", "  - `git cherry-pick` for individual commits", "  - `git diff` + `git apply` for patch-based application", "  - Direct file edits via Edit/Write tools for surgical changes", "", "If you encounter conflicts, resolve them intelligently — do not blindly overwrite.", "If you cannot resolve a conflict, leave standard conflict markers (<<<< / ==== / >>>>).", ""]}.into_iter().map(str::to_owned));
    if s(diff, "diff").trim().is_empty() {
        lines.push(
            if zh {
                "工作区未检测到差异。无需应用任何变更。"
            } else {
                "No differences detected in the workspace. Nothing to apply."
            }
            .into(),
        );
    } else {
        if diff["truncated"] == true {
            lines.push(if zh {
                format!("差异较大 ({} 个文件), 已截断。", diff["fileCount"])
            } else {
                format!(
                    "The diff is large ({} files) and has been truncated.",
                    diff["fileCount"]
                )
            });
            lines.push(
                if zh {
                    "请从工作区目录读取相关文件进行对比和应用。"
                } else {
                    "Read the relevant files from the workspace directory to compare and apply."
                }
                .into(),
            );
            lines.push("".into());
            lines.push(
                if zh {
                    "截断后的差异 (前部内容):"
                } else {
                    "Truncated diff (first portion):"
                }
                .into(),
            );
        } else {
            lines.push(if zh {
                format!("变更内容 ({} 个文件):", diff["fileCount"])
            } else {
                format!(
                    "Changes ({} file{}):",
                    diff["fileCount"],
                    if diff["fileCount"] == 1 { "" } else { "s" }
                )
            });
        }
        lines.push("".into());
        lines.push(s(diff, "diff").into());
    }
    lines.join("\n")
}

#[allow(clippy::too_many_arguments)]
pub fn discovery(
    zh: bool,
    project: &str,
    run: &str,
    created: &str,
    workspace: Option<&Value>,
    digest: &Value,
    plans: &Value,
) -> String {
    let mut lines = vec![match (zh, workspace) {
        (true, Some(ws)) => format!(
            "你正在为项目执行自主 Always-On 发现任务: {project} (当前在隔离工作区中: {})",
            s(ws, "cwd")
        ),
        (true, None) => format!("你正在为项目执行自主 Always-On 发现任务: {project}"),
        (false, Some(ws)) => format!(
            "You are running an autonomous Always-On discovery for project: {project} (working inside isolated workspace: {})",
            s(ws, "cwd")
        ),
        (false, None) => {
            format!("You are running an autonomous Always-On discovery for project: {project}")
        }
    }];
    lines.extend(if zh { vec!["", "目标: 最多提出一个有价值的任务。", "任务范围包括但不限于: 丰富或补充内容、完成未竟工作、优化结构或布局、", "修复错误、提升用户体验, 以及用户在聊天记录中讨论过的任何有价值的事项。", "每个计划必须包含至少一个可自动验证的检查步骤", "(如: 文件存在、内容匹配预期模式、页面无错误渲染等)。", "如果未发现可执行的任务, 不要调用任何工具——仅以简短说明回复原因即可。", "", "权限: 本轮运行在 `bypassPermissions` 模式下——所有工具调用均自动允许。"]} else { vec!["", "Goal: identify AT MOST ONE worthwhile task to propose.", "Tasks may include enriching or adding content, completing unfinished work,", "improving structure or layout, fixing errors, enhancing user experience,", "or anything valuable discussed in user chat history.", "Each plan must include at least one automatically-checkable verification step", "(e.g. file exists, content matches expected pattern, page renders without error).", "If nothing actionable is found, do not call any tool — just respond with a short note explaining why.", "", "Permissions: this turn runs in `bypassPermissions` mode — every tool call is auto-allowed."]}.into_iter().map(str::to_owned));
    if let Some(ws) = workspace {
        lines.extend(if zh {vec![format!("隔离工作区路径: {}",s(ws,"cwd")),format!("工作区策略: {}",s(ws,"strategy")),"此工作区是项目的隔离快照——可在其中自由使用 read_file / glob / bash。".into(),format!("请勿 cd 到工作区外部的项目根目录 ({project})。")] } else {vec![format!("Isolated workspace cwd: {}",s(ws,"cwd")),format!("Workspace strategy: {}.",s(ws,"strategy")),"This workspace is an isolated snapshot of the project — read / glob / bash freely inside it.".into(),format!("Do NOT cd outside the workspace to the project root ({project}).")]});
    } else {
        lines.push(if zh {
            format!("可使用 read_file / glob / bash 自由读取项目根目录: {project}")
        } else {
            format!("Read the project root at {project} using read_file / glob / bash freely.")
        });
    }
    lines.push("".into());
    let sessions = digest["sessions"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    if sessions.is_empty() {
        lines.push(if zh {"未找到近期用户对话。请浏览工作区内容以发现有价值的任务。"} else {"No recent user conversations found. Explore the workspace contents to find a worthwhile task."}.into());
    } else {
        lines.extend(if zh { vec!["## 近期用户对话","","以下是近期用户-智能体对话的结构化摘要。","这些是用户关注点的主要信号来源。","如需查看某个会话的完整对话, 请使用 sessionId 调用 `always_on_read_chat_history`。",""]} else { vec!["## Recent user conversations","","Below is a structured digest of recent user-agent chat sessions.","These are primary signals for what the user cares about.","To see the full conversation of a session, call `always_on_read_chat_history` with its sessionId.",""]}.into_iter().map(str::to_owned));
        let milliseconds = regex::Regex::new(r"\.\d{3}Z$").expect("constant");
        for session in sessions {
            let ts = milliseconds.replace(s(session, "lastModified"), "Z");
            lines.push(format!(
                "- [{ts}] \"{}\" (sessionId: {})",
                s(session, "title"),
                s(session, "alias")
            ));
            for prompt in session["userPrompts"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
            {
                lines.push(format!("  > {}", prompt.replace('\n', " ").trim()));
            }
            lines.push("".into());
        }
    }
    let plans = plans["plans"].as_array().map(Vec::as_slice).unwrap_or(&[]);
    lines.push("".into());
    if !plans.is_empty() {
        lines.push(
            if zh {
                "## 已有 Always-On 计划 (请勿重复这些主题)"
            } else {
                "## Existing Always-On plans (do NOT duplicate these topics)"
            }
            .into(),
        );
        lines.push("".into());
        for plan in plans {
            lines.push(format!(
                "- [{}] \"{}\" (dedupeKey: {})",
                s(plan, "status"),
                s(plan, "title"),
                s(plan, "dedupeKey")
            ));
        }
    }
    lines.extend(if zh {vec!["".into(),"如果你发现了一个任务, 请调用 `always_on_discovery_plan` 恰好一次, 提交格式严格的 markdown 计划。".into(),"计划结构要求 (自上而下):".into(),"  - 一级标题: # <计划标题>".into(),"  - 元数据引用块, 首行为 `Always-On Discovery Plan`, 后接键值行:".into()]} else {vec!["".into(),"If you identify a task, call `always_on_discovery_plan` exactly once with a strictly-formatted markdown plan.".into(),"Required plan structure (top to bottom):".into(),"  - Level-1 heading: # <plan title>".into(),"  - Metadata blockquote, first line `Always-On Discovery Plan`, then keyed lines:".into()]});
    lines.extend([
        format!("    > id: plan_{run}"),
        format!("    > sourceRunId: {run}"),
        format!("    > createdAt: {created}"),
        format!("    > projectRoot: {project}"),
    ]);
    lines.extend(if zh { vec!["    > dedupeKey: <稳定标识符>","  - 章节按以下顺序排列: ## Summary, ## Rationale, ## Context Signals, ## Proposed Change, ## Execution Steps, ## Verification。","  - Summary 不超过 200 字符, 单段落。","  - Context Signals: 至少一个 `-` 列表项。","  - Execution Steps: 仅使用有序列表 (1., 2., …), 不使用无序列表。","  - Verification: 至少一个 `-` 列表项, 每项必须可机器校验。","","硬性约束:","  - 调用 `always_on_discovery_plan` 超过一次将返回 plan_quota_exhausted。","  - 缺少必要章节、章节顺序错误、或包含模糊 'TODO' 措辞的计划将被拒绝。","  - 不要包含 Risks 或 Rollback 章节。"]} else { vec!["    > dedupeKey: <stable identifier>","  - Sections in this exact order: ## Summary, ## Rationale, ## Context Signals, ## Proposed Change, ## Execution Steps, ## Verification.","  - Summary ≤ 200 chars, single paragraph.","  - Context Signals: at least one `-` bullet.","  - Execution Steps: ordered list (1., 2., …) only; no bullets.","  - Verification: at least one `-` bullet, each line must be machine-checkable.","","Hard constraints:","  - Calling `always_on_discovery_plan` more than once returns plan_quota_exhausted.","  - Plans missing or reordering required sections, or containing fuzzy 'TODO' wording, will be rejected.","  - Do not include Risks or Rollback sections."]}.into_iter().map(str::to_owned));
    lines.join("\n")
}
