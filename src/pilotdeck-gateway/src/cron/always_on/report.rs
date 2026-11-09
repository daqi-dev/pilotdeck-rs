use serde_json::Value;
use std::collections::BTreeMap;

pub const SECTIONS: [&str; 7] = [
    "Plan Reference",
    "Steps Performed",
    "Files Changed",
    "Command Output",
    "Verification Results",
    "Follow-ups",
    "Notes",
];

pub fn normalize(content: &str, metadata: &Value) -> (String, Vec<String>) {
    let normalized = content.replace("\r\n", "\n");
    let lines: Vec<&str> = normalized.lines().collect();
    let mut cursor = lines
        .iter()
        .position(|line| !line.trim().is_empty())
        .unwrap_or(lines.len());
    let mut title = "Always-On Discovery Run";
    let mut fallbacks = Vec::new();
    if let Some(candidate) = lines
        .get(cursor)
        .and_then(|line| line.strip_prefix("# "))
        .map(str::trim)
        .filter(|s| !SECTIONS.contains(s))
    {
        title = candidate;
        cursor += 1;
    } else {
        fallbacks.push("title-missing".into());
    }
    while lines.get(cursor).is_some_and(|line| line.trim().is_empty()) {
        cursor += 1;
    }
    while lines.get(cursor).is_some_and(|line| line.starts_with('>')) {
        cursor += 1;
    }
    let mut sections = BTreeMap::<String, String>::new();
    let mut order = Vec::<String>::new();
    let mut current: Option<String> = None;
    for line in &lines[cursor..] {
        let heading = if let Some(h) = line.strip_prefix("## ") {
            Some(h.trim())
        } else if let Some(h) = line
            .strip_prefix("# ")
            .map(str::trim)
            .filter(|h| SECTIONS.contains(h))
        {
            fallbacks.push(format!("h1-downgraded({h})"));
            Some(h)
        } else {
            None
        };
        if let Some(heading) = heading {
            if !sections.contains_key(heading) {
                order.push(heading.into());
            }
            sections.insert(heading.into(), String::new());
            current = Some(heading.into());
        } else if let Some(heading) = &current {
            let section = sections.get_mut(heading).expect("current section exists");
            section.push_str(line);
            section.push('\n');
        }
    }
    for body in sections.values_mut() {
        *body = body.trim_end().into();
    }
    for name in SECTIONS {
        if !sections.contains_key(name) {
            let fallback = format!("section-missing({name})");
            sections.insert(
                name.into(),
                if name == "Notes" {
                    format!("- fallback: {fallback}")
                } else {
                    "(empty)".into()
                },
            );
            fallbacks.push(fallback);
        }
    }
    let notes = sections.get_mut("Notes").expect("required section exists");
    for fallback in &fallbacks {
        let entry = format!("- fallback: {fallback}");
        if !notes.lines().any(|line| line == entry) {
            if !notes.is_empty() {
                notes.push('\n');
            }
            notes.push_str(&entry);
        }
    }
    let mut report = format!("# {title}\n\n> Always-On Discovery Run Report\n");
    for key in [
        "runId",
        "planId",
        "startedAt",
        "finishedAt",
        "outcome",
        "workspaceStrategy",
        "workspaceHandle",
    ] {
        report.push_str(&format!(
            "> {key}: {}\n",
            metadata[key].as_str().unwrap_or("")
        ));
    }
    for name in SECTIONS.iter().copied().chain(
        order
            .iter()
            .map(String::as_str)
            .filter(|name| !SECTIONS.contains(name)),
    ) {
        report.push_str(&format!("\n## {name}\n{}\n", sections[name]));
    }
    (report, fallbacks)
}

pub fn fallback(title: &str, reason: &str, metadata: &Value) -> String {
    let content = format!(
        "# {title} - Work Report\n\n## Plan Reference\n(unavailable: report tool was not invoked)\n\n## Steps Performed\n(empty)\n\n## Files Changed\n(none recorded)\n\n## Command Output\n(none recorded)\n\n## Verification Results\n- [ ] (unverified) - report tool was not invoked\n\n## Follow-ups\n- Investigate why AlwaysOnReportTool was not called.\n\n## Notes\n- fallback: {reason}\n"
    );
    normalize(&content, metadata).0
}
