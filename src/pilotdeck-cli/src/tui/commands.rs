//! Parse slash commands into effects. The caller executes effects and reports failures.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PermissionAction {
    Show,
    Allow(String),
    Deny(String),
    Clear(String),
    Bypass,
    Usage,
    Unknown(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    New,
    Sidebar,
    /// Try this one-based index; open the sidebar if it is out of range.
    Switch(usize),
    Permissions(PermissionAction),
    /// Also synchronize the global skipPermissions setting.
    Mode(&'static str),
    /// None means use the last eligible tool output.
    View(Option<usize>),
    Clear,
    Help,
    Exit,
    Unknown(String),
}

// JavaScript parseInt(s, 10): consume a decimal prefix, not the whole argument.
fn index(s: &str) -> Option<usize> {
    let s = s.strip_prefix('+').unwrap_or(s);
    let end = s.bytes().take_while(u8::is_ascii_digit).count();
    if end == 0 {
        return None;
    }
    let n = s[..end].parse::<usize>().unwrap_or(usize::MAX);
    (n > 0).then_some(n)
}

/// Input is the already-trimmed submission, as in the TS caller.
/// Returns None for ordinary text; all slash commands, including unknown ones,
/// return an effect. Effect execution belongs to the app, not this parser.
pub fn parse(input: &str) -> Option<Command> {
    if !input.starts_with('/') {
        return None;
    }
    let mut parts = input.split(|c: char| matches!(c, '\u{0009}'..='\u{000d}' | '\u{0020}' | '\u{00a0}' | '\u{1680}' | '\u{2000}'..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}' | '\u{205f}' | '\u{3000}' | '\u{feff}')).filter(|part| !part.is_empty());
    let name = parts.next().unwrap_or("");
    let args: Vec<_> = parts.collect();
    Some(match name {
        "/new" => Command::New,
        "/sessions" | "/dashboard" => Command::Sidebar,
        "/switch" => args
            .first()
            .and_then(|s| index(s))
            .map_or(Command::Sidebar, Command::Switch),
        "/permissions" => {
            let action = if let Some(sub) = args.first() {
                let entry = args[1..].join(" ");
                if entry.is_empty() && *sub != "bypass" {
                    PermissionAction::Usage
                } else {
                    match *sub {
                        "allow" => PermissionAction::Allow(entry),
                        "deny" => PermissionAction::Deny(entry),
                        "clear" => PermissionAction::Clear(entry),
                        "bypass" => PermissionAction::Bypass,
                        _ => PermissionAction::Unknown((*sub).into()),
                    }
                }
            } else {
                PermissionAction::Show
            };
            Command::Permissions(action)
        }
        "/mode" => Command::Mode(match args.first().copied() {
            Some("plan") => "plan",
            Some("bypassPermissions") => "bypassPermissions",
            _ => "default",
        }),
        "/view" => Command::View(args.first().and_then(|s| index(s))),
        "/clear" => Command::Clear,
        "/help" => Command::Help,
        "/exit" => Command::Exit,
        _ => Command::Unknown(name.into()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn baseline_command_branches() {
        assert_eq!(parse("hello"), None);
        assert_eq!(parse("/switch 2x"), Some(Command::Switch(2)));
        assert_eq!(parse("/switch -1"), Some(Command::Sidebar));
        assert_eq!(parse("/mode bypass"), Some(Command::Mode("default")));
        assert_eq!(
            parse("/permissions invalid"),
            Some(Command::Permissions(PermissionAction::Usage))
        );
        assert_eq!(
            parse("/permissions invalid x"),
            Some(Command::Permissions(PermissionAction::Unknown(
                "invalid".into()
            )))
        );
        assert_eq!(
            parse("/permissions allow bash:git  status"),
            Some(Command::Permissions(PermissionAction::Allow(
                "bash:git status".into()
            )))
        );
        assert_eq!(
            parse("/permissions bypass extra"),
            Some(Command::Permissions(PermissionAction::Bypass))
        );
        assert_eq!(parse("/view 0"), Some(Command::View(None)));
        assert_eq!(parse("/unknown"), Some(Command::Unknown("/unknown".into())));
    }
}

#[cfg(test)]
mod ts_oracle_tests {
    use super::*;
    use serde_json::Value;

    #[test]
    fn command_effects_follow_real_ts_handler() {
        let oracle: Value =
            serde_json::from_str(include_str!("../../tests/fixtures/tui/commands.json")).unwrap();
        let rows = oracle["cases"].as_array().unwrap();
        assert_eq!(rows.len(), 35);
        for row in rows {
            let input = row["trimmed"].as_str().unwrap();
            let calls = row["calls"].as_array().unwrap();
            let called = |name: &str| calls.iter().any(|c| c["op"] == name);
            let command = parse(input);
            assert_eq!(command.is_some(), row["handled"] == true, "{}", row["id"]);
            match command {
                None => assert!(calls.is_empty()),
                Some(Command::New) => assert!(called("newSession")),
                Some(Command::Sidebar) => assert!(called("openSidebar")),
                Some(Command::Switch(index)) => {
                    assert!(called("listSessions"));
                    if index <= 2 {
                        assert!(called("switchToSession"));
                    } else {
                        assert!(called("openSidebar"));
                    }
                }
                Some(Command::Permissions(action)) => {
                    assert!(called("readPermissions"));
                    match action {
                        PermissionAction::Show
                        | PermissionAction::Usage
                        | PermissionAction::Unknown(_) => assert!(!called("writePermissions")),
                        PermissionAction::Allow(value) => assert!(
                            row["settings"]["allowedTools"]
                                .as_array()
                                .unwrap()
                                .contains(&Value::String(value))
                        ),
                        PermissionAction::Deny(value) => assert!(
                            row["settings"]["disallowedTools"]
                                .as_array()
                                .unwrap()
                                .contains(&Value::String(value))
                        ),
                        PermissionAction::Clear(value) => {
                            assert!(
                                !row["settings"]["allowedTools"]
                                    .as_array()
                                    .unwrap()
                                    .contains(&Value::String(value.clone()))
                            );
                            assert!(
                                !row["settings"]["disallowedTools"]
                                    .as_array()
                                    .unwrap()
                                    .contains(&Value::String(value))
                            );
                        }
                        PermissionAction::Bypass => {
                            assert_eq!(row["state"]["mode"], "bypassPermissions")
                        }
                    }
                }
                Some(Command::Mode(mode)) => assert_eq!(row["state"]["mode"], mode),
                Some(Command::View(index)) => {
                    if row["id"] == "view-none" {
                        assert!(!called("openToolOutput"));
                    } else {
                        let expected = if index == Some(1) { "first" } else { "last" };
                        assert!(
                            calls
                                .iter()
                                .any(|c| c["op"] == "openToolOutput"
                                    && c["value"]["text"] == expected)
                        );
                    }
                }
                Some(Command::Clear) => assert_eq!(row["state"]["messages"], serde_json::json!([])),
                Some(Command::Help) => assert_eq!(row["state"]["helpOpen"], true),
                Some(Command::Exit) => assert!(called("exit")),
                Some(Command::Unknown(name)) => assert_eq!(
                    row["state"]["messages"].as_array().unwrap().last().unwrap()["text"],
                    format!("Unknown command {name}")
                ),
            }
        }
    }
}
