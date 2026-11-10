//! Model-visible prompt compatibility with PilotDeck cc7c4e6 (AGPL-3.0).
use pilotdeck_protocol::JsString;

fn append(out: &mut Vec<u16>, text: &str) {
    out.extend(text.encode_utf16());
}
fn trimmed(value: &JsString) -> &[u16] {
    let units = value.units();
    let whitespace = |u: &u16| matches!(*u, 0x0009..=0x000d | 0x0020 | 0x00a0 | 0x1680 | 0x2000..=0x200a | 0x2028 | 0x2029 | 0x202f | 0x205f | 0x3000 | 0xfeff);
    let start = units
        .iter()
        .position(|u| !whitespace(u))
        .unwrap_or(units.len());
    let end = units
        .iter()
        .rposition(|u| !whitespace(u))
        .map_or(start, |i| i + 1);
    &units[start..end]
}

pub(super) fn workspace(chinese: bool, project: &str, title: &JsString) -> JsString {
    let mut out = Vec::new();
    if chinese {
        append(&mut out, "你正在为 Always-On 计划执行准备一个隔离工作区。");
        out.push(10);
        append(&mut out, "");
        out.push(10);
        append(&mut out, "项目根目录: ");
        append(&mut out, project);
        out.push(10);
        append(&mut out, "计划: \"");
        out.extend_from_slice(title.units());
        append(&mut out, "\"");
        out.push(10);
        append(&mut out, "");
        out.push(10);
        append(&mut out, "可用的工作区策略:");
        out.push(10);
        append(
            &mut out,
            "  - `git-worktree`: 在新分支上创建 git worktree。速度快、空间占用少 (使用硬链接)。",
        );
        out.push(10);
        append(
            &mut out,
            "    要求项目是 git 仓库且至少有一次提交。若工作区存在未提交更改，Always-On",
        );
        out.push(10);
        append(
            &mut out,
            "    会先提交当前全部改动作为 checkpoint，再创建 worktree。",
        );
        out.push(10);
        append(
            &mut out,
            "  - `snapshot-copy`: 复制项目目录 (在 APFS/btrfs 上使用 CoW)。适用于任何目录,",
        );
        out.push(10);
        append(
            &mut out,
            "    但占用更多磁盘空间。默认忽略 .git、node_modules、dist。",
        );
        out.push(10);
        append(&mut out, "");
        out.push(10);
        append(
            &mut out,
            "权限: 本轮运行在 `bypassPermissions` 模式下——所有工具调用均自动允许。",
        );
        out.push(10);
        append(&mut out, "");
        out.push(10);
        append(&mut out, "## 执行步骤");
        out.push(10);
        append(
            &mut out,
            "1. 检查项目根目录状态 (如 git 仓库可执行 `git status --porcelain`, 否则执行 `ls`)。",
        );
        out.push(10);
        append(&mut out, "2. 调用 `");
        append(&mut out, "always_on_prepare_workspace");
        append(
            &mut out,
            "`, 传入选定的策略, 或传入 `auto` 让运行时自动选择。",
        );
    } else {
        append(
            &mut out,
            "You are preparing an isolated workspace for an Always-On plan execution.",
        );
        out.push(10);
        append(&mut out, "");
        out.push(10);
        append(&mut out, "Project root: ");
        append(&mut out, project);
        out.push(10);
        append(&mut out, "Plan: \"");
        out.extend_from_slice(title.units());
        append(&mut out, "\"");
        out.push(10);
        append(&mut out, "");
        out.push(10);
        append(&mut out, "Available workspace strategies:");
        out.push(10);
        append(
            &mut out,
            "  - `git-worktree`: Creates a git worktree on a new branch. Fast and space-efficient (hard-links).",
        );
        out.push(10);
        append(
            &mut out,
            "    Requires a git repo with at least one commit. If the working tree is dirty, Always-On",
        );
        out.push(10);
        append(
            &mut out,
            "    checkpoints all current changes before creating the worktree.",
        );
        out.push(10);
        append(
            &mut out,
            "  - `snapshot-copy`: Copies the project directory (CoW on APFS/btrfs). Works for any directory",
        );
        out.push(10);
        append(
            &mut out,
            "    but uses more disk space. Ignores .git, node_modules, dist by default.",
        );
        out.push(10);
        append(&mut out, "");
        out.push(10);
        append(
            &mut out,
            "Permissions: this turn runs in `bypassPermissions` mode — every tool call is auto-allowed.",
        );
        out.push(10);
        append(&mut out, "");
        out.push(10);
        append(&mut out, "## What to do");
        out.push(10);
        append(
            &mut out,
            "1. Check the project root state (e.g. `git status --porcelain` if it looks like a git repo, or `ls` otherwise).",
        );
        out.push(10);
        append(&mut out, "2. Call `");
        append(&mut out, "always_on_prepare_workspace");
        append(
            &mut out,
            "` with the chosen strategy, or `auto` to let the runtime decide.",
        );
    }
    JsString::from_utf16(out)
}

pub(super) fn execution(
    chinese: bool,
    strategy: &str,
    cwd: &JsString,
    markdown: &JsString,
) -> JsString {
    let mut out = Vec::new();
    if chinese {
        append(&mut out, "你正在隔离工作区内执行一个 Always-On 发现计划。");
        out.push(10);
        append(&mut out, "工作区策略: ");
        append(&mut out, strategy);
        out.push(10);
        append(&mut out, "工作区路径: ");
        out.extend_from_slice(cwd.units());
        out.push(10);
        append(&mut out, "");
        out.push(10);
        append(
            &mut out,
            "权限: 本轮运行在 `bypassPermissions` 模式下——所有工具调用均自动允许。",
        );
        out.push(10);
        append(
            &mut out,
            "安全边界为工作区本身; 请勿 cd 到工作区外部, 请勿修改用户的项目根目录。",
        );
        out.push(10);
        append(&mut out, "");
        out.push(10);
        append(&mut out, "## 计划");
        out.push(10);
        out.extend_from_slice(trimmed(markdown));
        out.push(10);
        append(&mut out, "");
        out.push(10);
        append(&mut out, "## 执行步骤");
        out.push(10);
        append(&mut out, "1. 按顺序执行 Execution Steps 中的各项步骤。");
        out.push(10);
        append(&mut out, "2. 运行 Verification 列表中的检查项并记录结果。");
        out.push(10);
        append(&mut out, "3. 回复执行总结及验证结果。");
    } else {
        append(
            &mut out,
            "You are executing an Always-On discovery plan inside an isolated workspace.",
        );
        out.push(10);
        append(&mut out, "Workspace strategy: ");
        append(&mut out, strategy);
        append(&mut out, ".");
        out.push(10);
        append(&mut out, "Workspace cwd: ");
        out.extend_from_slice(cwd.units());
        out.push(10);
        append(&mut out, "");
        out.push(10);
        append(
            &mut out,
            "Permissions: this turn runs in `bypassPermissions` mode — every tool call is auto-allowed.",
        );
        out.push(10);
        append(
            &mut out,
            "Safety boundary is the workspace itself; do NOT cd outside it, do NOT touch the user's project root.",
        );
        out.push(10);
        append(&mut out, "");
        out.push(10);
        append(&mut out, "## Plan");
        out.push(10);
        out.extend_from_slice(trimmed(markdown));
        out.push(10);
        append(&mut out, "");
        out.push(10);
        append(&mut out, "## What to do");
        out.push(10);
        append(&mut out, "1. Execute the Execution Steps in order.");
        out.push(10);
        append(&mut out, "2. Run the Verification list and record results.");
        out.push(10);
        append(
            &mut out,
            "3. Respond with a summary of what was done and the verification outcomes.",
        );
    }
    JsString::from_utf16(out)
}

pub(super) fn report(
    chinese: bool,
    strategy: &str,
    cwd: &JsString,
    markdown: &JsString,
) -> JsString {
    let mut out = Vec::new();
    if chinese {
        append(
            &mut out,
            "你正在为已完成的 Always-On 计划执行撰写工作报告。",
        );
        out.push(10);
        append(&mut out, "工作区策略: ");
        append(&mut out, strategy);
        out.push(10);
        append(&mut out, "工作区路径: ");
        out.extend_from_slice(cwd.units());
        out.push(10);
        append(&mut out, "");
        out.push(10);
        append(
            &mut out,
            "权限: 本轮运行在 `bypassPermissions` 模式下——所有工具调用均自动允许。",
        );
        out.push(10);
        append(&mut out, "");
        out.push(10);
        append(&mut out, "## 已执行的计划");
        out.push(10);
        out.extend_from_slice(trimmed(markdown));
        out.push(10);
        append(&mut out, "");
        out.push(10);
        append(&mut out, "## 执行步骤");
        out.push(10);
        append(
            &mut out,
            "1. 查看工作区中的变更 (如 `git diff --stat`、`ls`、阅读相关文件)。",
        );
        out.push(10);
        append(
            &mut out,
            "2. 总结执行情况: 执行了哪些步骤、修改了哪些文件、命令输出、验证结果。",
        );
        out.push(10);
        append(&mut out, "3. 调用 `");
        append(&mut out, "always_on_report");
        append(&mut out, "` 恰好一次, 提交完整的工作报告 markdown。");
        out.push(10);
        append(&mut out, "");
        out.push(10);
        append(
            &mut out,
            "每个章节必须使用 `##`（h2）标题——例如 `## Plan Reference`、`## Steps Performed` 等。",
        );
        out.push(10);
        append(
            &mut out,
            "报告章节按以下顺序排列: Plan Reference, Steps Performed, Files Changed, Command Output, Verification Results, Follow-ups, Notes。",
        );
        out.push(10);
        append(&mut out, "缺失的章节将由运行时自动补全。");
    } else {
        append(
            &mut out,
            "You are writing a work report for a completed Always-On plan execution.",
        );
        out.push(10);
        append(&mut out, "Workspace strategy: ");
        append(&mut out, strategy);
        append(&mut out, ".");
        out.push(10);
        append(&mut out, "Workspace cwd: ");
        out.extend_from_slice(cwd.units());
        out.push(10);
        append(&mut out, "");
        out.push(10);
        append(
            &mut out,
            "Permissions: this turn runs in `bypassPermissions` mode — every tool call is auto-allowed.",
        );
        out.push(10);
        append(&mut out, "");
        out.push(10);
        append(&mut out, "## Plan that was executed");
        out.push(10);
        out.extend_from_slice(trimmed(markdown));
        out.push(10);
        append(&mut out, "");
        out.push(10);
        append(&mut out, "## What to do");
        out.push(10);
        append(
            &mut out,
            "1. Review the workspace to see what changed (e.g. `git diff --stat`, `ls`, read relevant files).",
        );
        out.push(10);
        append(
            &mut out,
            "2. Summarize the execution: what steps were performed, which files were changed, command outputs, and verification results.",
        );
        out.push(10);
        append(&mut out, "3. Call `");
        append(&mut out, "always_on_report");
        append(
            &mut out,
            "` exactly once with the full work-report markdown.",
        );
        out.push(10);
        append(&mut out, "");
        out.push(10);
        append(
            &mut out,
            "Each section MUST use `##` (h2) headers — e.g. `## Plan Reference`, `## Steps Performed`, etc.",
        );
        out.push(10);
        append(
            &mut out,
            "Required sections in order: Plan Reference, Steps Performed, Files Changed, Command Output, Verification Results, Follow-ups, Notes.",
        );
        out.push(10);
        append(
            &mut out,
            "Missing sections will be filled by the runtime fallback.",
        );
    }
    JsString::from_utf16(out)
}

// Static model-visible templates originate from fixed TS cc7c4e6 (AGPL-3.0).
fn render_template(template: &[u16], replacements: &[(&str, &[u16])]) -> Vec<u16> {
    let keys: Vec<Vec<u16>> = replacements
        .iter()
        .map(|(key, _)| key.encode_utf16().collect())
        .collect();
    let mut out = Vec::new();
    let mut pos = 0;
    while pos < template.len() {
        if let Some(i) = keys.iter().position(|key| template[pos..].starts_with(key)) {
            out.extend_from_slice(replacements[i].1);
            pos += keys[i].len();
        } else {
            out.push(template[pos]);
            pos += 1;
        }
    }
    out
}
fn discovery_field(value: &pilotdeck_protocol::JsValue, key: &str) -> Vec<u16> {
    value
        .get(key)
        .and_then(|v| v.as_string())
        .expect("fixed discovery template field")
        .units()
        .to_vec()
}
pub(super) fn discovery(
    chinese: bool,
    project: &str,
    run: &JsString,
    created: &JsString,
    workspace: Option<(&JsString, &str)>,
    digest: &super::chat_digest::ChatDigest,
    plans: &[super::DiscoveryPlanRecord],
) -> JsString {
    let data = pilotdeck_protocol::parse_json(include_str!("discovery-static.json"))
        .expect("fixed discovery templates");
    let item = data
        .get(if chinese { "zh-CN" } else { "en-US" })
        .expect("template language");
    let template = discovery_field(item, "template");
    let head_template = discovery_field(
        item,
        if workspace.is_some() {
            "head_yes"
        } else {
            "head_no"
        },
    );
    let access_template = discovery_field(
        item,
        if workspace.is_some() {
            "access_yes"
        } else {
            "access_no"
        },
    );
    let project_units: Vec<u16> = project.encode_utf16().collect();
    let empty = JsString::default();
    let (cwd, strategy) = workspace.unwrap_or((&empty, ""));
    let strategy_units: Vec<u16> = strategy.encode_utf16().collect();
    let vars = [
        ("@@PROJECT@@", project_units.as_slice()),
        ("@@CWD@@", cwd.units()),
        ("@@STRATEGY@@", strategy_units.as_slice()),
    ];
    let head = render_template(&head_template, &vars);
    let access = render_template(&access_template, &vars);
    let mut digest_text = Vec::new();
    if digest.sessions.is_empty() {
        digest_text = discovery_field(item, "empty");
        digest_text.extend("\n\n".encode_utf16());
    } else {
        let base = discovery_field(item, "digest");
        let needle: Vec<u16> = "- [@@MODIFIED@@]".encode_utf16().collect();
        let start = base
            .windows(needle.len())
            .position(|v| v == needle)
            .expect("digest row");
        digest_text.extend_from_slice(&base[..start]);
        for session in &digest.sessions {
            digest_text.extend("- [".encode_utf16());
            let t = session.modified.units();
            let n = t.len();
            if n >= 5
                && t[n - 5] == 46
                && t[n - 1] == 90
                && t[n - 4..n - 1].iter().all(|u| (48..=57).contains(u))
            {
                digest_text.extend_from_slice(&t[..n - 5]);
                digest_text.push(90);
            } else {
                digest_text.extend_from_slice(t);
            }
            digest_text.extend("] \"".encode_utf16());
            digest_text.extend_from_slice(session.title.units());
            digest_text.extend("\" (sessionId: ".encode_utf16());
            digest_text.extend_from_slice(session.alias.units());
            digest_text.extend(")\n".encode_utf16());
            for prompt in &session.prompts {
                let one_line = JsString::from_utf16(
                    prompt
                        .units()
                        .iter()
                        .map(|u| if *u == 10 { 32 } else { *u })
                        .collect(),
                );
                digest_text.extend("  > ".encode_utf16());
                digest_text.extend_from_slice(trimmed(&one_line));
                digest_text.push(10);
            }
            digest_text.push(10);
        }
        digest_text.push(10);
    }
    let mut plan_text = Vec::new();
    if !plans.is_empty() {
        let base = discovery_field(item, "plans");
        let needle: Vec<u16> = "- [@@STATUS@@]".encode_utf16().collect();
        let start = base
            .windows(needle.len())
            .position(|v| v == needle)
            .expect("plan row");
        plan_text.extend_from_slice(&base[..start]);
        for plan in plans {
            plan_text.extend("- [".encode_utf16());
            if let Some(value) = plan.raw.get("status").and_then(|v| v.as_string()) {
                plan_text.extend_from_slice(value.units());
            } else {
                plan_text.extend("undefined".encode_utf16());
            }
            plan_text.extend("] \"".encode_utf16());
            if let Some(title) = &plan.title {
                plan_text.extend_from_slice(title.units());
            } else {
                plan_text.extend("undefined".encode_utf16());
            }
            plan_text.extend("\" (dedupeKey: ".encode_utf16());
            if let Some(value) = plan.raw.get("dedupeKey").and_then(|v| v.as_string()) {
                plan_text.extend_from_slice(value.units());
            } else {
                plan_text.extend("undefined".encode_utf16());
            }
            plan_text.extend(")\n".encode_utf16());
        }
        plan_text.push(10);
    } else {
        plan_text.push(10);
    }
    JsString::from_utf16(render_template(
        &template,
        &[
            ("@@HEAD@@", &head),
            ("@@ACCESS@@", &access),
            ("@@DIGEST@@", &digest_text),
            ("@@PLANS@@", &plan_text),
            ("@@PROJECT@@", &project_units),
            ("@@RUN@@", run.units()),
            ("@@CREATED@@", created.units()),
        ],
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompts_match_fixed_ts_consumer_for_both_languages() {
        let oracle = pilotdeck_protocol::parse_json(include_str!("prompt-oracle.json")).unwrap();
        for (language, chinese) in [("en-US", false), ("zh-CN", true)] {
            let expected = oracle.get("results").unwrap().get(language).unwrap();
            let title = JsString::from("测试 plan");
            let cwd = JsString::from("/fixture/worktree");
            let markdown = JsString::from(" \n## Plan\n- action\n\u{00a0}");
            for (name, actual) in [
                ("workspace", workspace(chinese, "/fixture/project", &title)),
                (
                    "execution",
                    execution(chinese, "git-worktree", &cwd, &markdown),
                ),
                ("report", report(chinese, "git-worktree", &cwd, &markdown)),
            ] {
                assert_eq!(
                    &actual,
                    expected.get(name).unwrap().as_string().unwrap(),
                    "{language} {name}"
                );
            }
        }
    }
    #[test]
    fn discovery_matches_fixed_ts_consumer_across_workspace_digest_and_plans() {
        use crate::runtime::always_on::{
            DiscoveryPlanRecord,
            chat_digest::{ChatDigest, DigestItem},
        };
        use pilotdeck_protocol::{JsObject, JsValue};
        let oracle =
            pilotdeck_protocol::parse_json(include_str!("discovery-templates.json")).unwrap();
        let mut raw = JsObject::new();
        raw.insert("status".into(), JsValue::String("@@STATUS@@".into()));
        raw.insert("dedupeKey".into(), JsValue::String("@@DEDUPE@@".into()));
        let plan = DiscoveryPlanRecord {
            raw,
            id: "@@ID@@".into(),
            title: Some("@@PLAN_TITLE@@".into()),
        };
        let cwd = JsString::from("@@CWD@@");
        for (language, chinese) in [("en-US", false), ("zh-CN", true)] {
            for workspace in [false, true] {
                for has_digest in [false, true] {
                    for has_plans in [false, true] {
                        let digest = ChatDigest {
                            sessions: if has_digest {
                                vec![DigestItem {
                                    alias: "@@ALIAS@@".into(),
                                    title: "@@TITLE@@".into(),
                                    modified: "@@MODIFIED@@".into(),
                                    prompts: vec!["@@PROMPT@@".into()],
                                }]
                            } else {
                                vec![]
                            },
                            aliases: Default::default(),
                        };
                        let plans = if has_plans {
                            std::slice::from_ref(&plan)
                        } else {
                            &[]
                        };
                        let actual = discovery(
                            chinese,
                            "@@PROJECT@@",
                            &"@@RUN@@".into(),
                            &"@@CREATED@@".into(),
                            if workspace {
                                Some((&cwd, "@@STRATEGY@@"))
                            } else {
                                None
                            },
                            &digest,
                            plans,
                        );
                        let key = format!("{language}:{workspace}:{has_digest}:{has_plans}");
                        assert_eq!(
                            &actual,
                            oracle.get(&key).unwrap().as_string().unwrap(),
                            "{key}"
                        );
                    }
                }
            }
        }
    }
}
