//! Access checks before a WeCom callback can create a turn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Policy {
    Open,
    Allowlist,
    Disabled,
    Pairing,
}
impl Policy {
    pub(crate) fn parse(value: &str) -> Self {
        match value.trim().to_lowercase().as_str() {
            "open" => Self::Open,
            "allowlist" => Self::Allowlist,
            "disabled" => Self::Disabled,
            _ => Self::Pairing,
        }
    }
}
fn normalize(value: &str) -> String {
    let value = value.trim().to_lowercase();
    value
        .strip_prefix("wecom:user:")
        .or_else(|| value.strip_prefix("wecom:group:"))
        .unwrap_or(&value)
        .to_string()
}
pub(crate) fn matches(entries: &[String], value: &str) -> bool {
    let value = normalize(value);
    entries.iter().any(|entry| {
        let candidate = normalize(entry);
        candidate == "*" || candidate == value
    })
}
pub(crate) fn dm_allowed(policy: Policy, allow: &[String], sender: &str) -> bool {
    !sender.is_empty()
        && match policy {
            Policy::Open => true,
            Policy::Allowlist => matches(allow, sender),
            Policy::Disabled | Policy::Pairing => false,
        }
}
pub(crate) fn group_allowed(
    policy: Policy,
    allow: &[String],
    sender_allow: &[String],
    chat: &str,
    sender: &str,
) -> bool {
    !chat.is_empty()
        && match policy {
            Policy::Disabled | Policy::Pairing => false,
            Policy::Allowlist if !matches(allow, chat) => false,
            _ => sender_allow.is_empty() || matches(sender_allow, sender),
        }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn deny_by_default_and_honor_both_group_filters() {
        assert!(!dm_allowed(Policy::parse("unknown"), &[], "a"));
        assert!(dm_allowed(
            Policy::Allowlist,
            &[" WeCom:User:ALICE ".into()],
            "alice"
        ));
        assert!(!group_allowed(
            Policy::Allowlist,
            &["wecom:group:team".into()],
            &["bob".into()],
            "TEAM",
            "alice"
        ));
        assert!(group_allowed(
            Policy::Allowlist,
            &["wecom:group:team".into()],
            &["*".into()],
            "TEAM",
            "alice"
        ));
        assert!(!group_allowed(Policy::Open, &[], &[], "", "alice"));
    }
}
