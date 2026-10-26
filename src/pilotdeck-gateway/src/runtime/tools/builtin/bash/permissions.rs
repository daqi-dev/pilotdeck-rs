//! `tool/builtin/bash/permissions.ts`: hard-deny / ask / read-only shell
//! classification consumed by `PermissionRuntime` through
//! [`crate::runtime::permission::PermissionTool::check_permissions`].
//!
//! The baseline regexes are JS `RegExp`s; their Rust form keeps the JS
//! semantics that differ from the `regex` crate defaults:
//! - `\s` is the ECMAScript WhiteSpace + LineTerminator set ([`WS`]), not
//!   Unicode `White_Space` (which adds U+0085 and drops U+FEFF);
//! - `\b` is the ASCII word boundary (`(?-u:\b)`);
//! - `/i` without `/u` folds ASCII only, so U+017F / U+212A (the only non-ASCII
//!   code points whose simple case fold is an ASCII letter) are masked before
//!   the Unicode `(?i)` match ([`ascii_fold_haystack`]);
//! - the look-ahead `rm` rules are evaluated by enumerating every match end of
//!   the `rm\s+` prefix and testing each anchored look-ahead body there, which
//!   is exactly the backtracking search `RegExp#test` performs.
use crate::js_value::ascii_fold_haystack;
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use regex::Regex;
use std::sync::LazyLock;

/// ECMAScript `\s` as a class body (shared definition).
const WS_BODY: &str = crate::metadata::SPACE;

/// ECMAScript `\s`.
pub(super) fn ws() -> String {
    format!("[{WS_BODY}]")
}

/// ECMAScript `\S`.
pub(super) fn non_ws() -> String {
    format!("[^{WS_BODY}]")
}

/// ECMAScript `\b` (ASCII word characters).
pub(super) const B: &str = r"(?-u:\b)";

pub(super) fn is_js_space(c: char) -> bool {
    crate::runtime::tools::js_space(c)
}

/// `(?:^|[;&|]\s*)` followed by `body`.
fn command_pattern(body: &str, ignore_case: bool) -> Regex {
    let flags = if ignore_case { "(?i)" } else { "" };
    let s = ws();
    Regex::new(&format!("{flags}(?:^|[;&|]{s}*){body}")).expect("bash permission pattern")
}

fn pattern(body: &str, ignore_case: bool) -> Regex {
    let flags = if ignore_case { "(?i)" } else { "" };
    Regex::new(&format!("{flags}{body}")).expect("bash permission pattern")
}

/// A `/i` (non-`u`) or case-sensitive pattern.
struct JsPattern {
    regex: Regex,
    ignore_case: bool,
}

impl JsPattern {
    fn test(&self, text: &str) -> bool {
        if self.ignore_case {
            self.regex.is_match(&ascii_fold_haystack(text))
        } else {
            self.regex.is_match(text)
        }
    }
}

fn js(regex: Regex, ignore_case: bool) -> JsPattern {
    JsPattern { regex, ignore_case }
}

/// The `rm\s+(?=...)` families. Each target is the body of the second
/// look-ahead (anchored at the candidate position).
enum RmTarget {
    Root,
    System,
    Home,
}

struct RmLookaheads {
    recursive: Regex,
    root: Regex,
    system: Regex,
    home: Regex,
}

static RM: LazyLock<RmLookaheads> = LazyLock::new(|| {
    let s = ws();
    let segment = r"[^;&|\n]*";
    let quote = r#"["']?"#;
    let anchored =
        |body: String| Regex::new(&format!("(?i)^{segment}{body}")).expect("rm look-ahead pattern");
    RmLookaheads {
        // -[^\s;&|]*r|--recursive\b
        recursive: anchored(format!(r"(?:-[^{WS_BODY};&|]*r|--recursive{B})")),
        // \s ["']?(?:/|/\*|/ \*|//+|/\.)["']? (?:\s|$)
        root: anchored(format!(r"{s}{quote}(?:/|/\*|/ \*|//+|/\.){quote}(?:{s}|$)")),
        system: anchored(format!(
            r"{s}{quote}/(?:bin|boot|etc|home|lib|lib64|root|sbin|usr|var)(?:/|{quote}(?:{s}|$))"
        )),
        home: anchored(format!(
            r"{s}{quote}(?:~|\$HOME|\$\{{HOME\}})(?:/|{quote}(?:{s}|$))"
        )),
    }
});

/// `(?:^|[;&|]\s*)rm\s+` + look-aheads, `/i`.
fn rm_matches(command: &str, target: Option<RmTarget>) -> bool {
    let text = ascii_fold_haystack(command);
    let text = text.as_ref();
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    for (index, &(offset, c)) in chars.iter().enumerate() {
        if !matches!(c, 'r' | 'R') {
            continue;
        }
        if !chars
            .get(index + 1)
            .is_some_and(|(_, next)| matches!(next, 'm' | 'M'))
        {
            continue;
        }
        if offset != 0 {
            let mut back = index;
            let mut separated = false;
            while back > 0 {
                back -= 1;
                let previous = chars[back].1;
                if is_js_space(previous) {
                    continue;
                }
                separated = matches!(previous, ';' | '&' | '|');
                break;
            }
            if !separated {
                continue;
            }
        }
        // Every end of the `\s+` run after `rm` is a candidate position.
        let mut cursor = index + 2;
        while let Some(&(_, c)) = chars.get(cursor) {
            if !is_js_space(c) {
                break;
            }
            cursor += 1;
            let position = chars.get(cursor).map_or(text.len(), |(offset, _)| *offset);
            let rest = &text[position..];
            if !RM.recursive.is_match(rest) {
                continue;
            }
            let matched = match target {
                None => true,
                Some(RmTarget::Root) => RM.root.is_match(rest),
                Some(RmTarget::System) => RM.system.is_match(rest),
                Some(RmTarget::Home) => RM.home.is_match(rest),
            };
            if matched {
                return true;
            }
        }
    }
    false
}

static HARD_DENY: LazyLock<Vec<JsPattern>> = LazyLock::new(|| {
    let s = ws();
    let ns = non_ws();
    let segment = r"[^;&|\n]*";
    let sudo = format!(r"(?:sudo{s}+(?:-{ns}+{s}+)*)?");
    vec![
        js(
            command_pattern(&format!(r"mkfs(?:\.[a-z0-9]+)?{B}"), true),
            true,
        ),
        js(
            command_pattern(
                &format!(r"dd{B}{segment}{B}of=/dev/(?:sd|nvme|hd|mmcblk|vd|xvd)[a-z0-9]*{B}"),
                true,
            ),
            true,
        ),
        js(
            pattern(
                &format!(r">{s}*/dev/(?:sd|nvme|hd|mmcblk|vd|xvd)[a-z0-9]*{B}"),
                true,
            ),
            true,
        ),
        js(
            pattern(
                &format!(r":\(\){s}*\{{{s}*:{s}*\|{s}*:{s}*&{s}*\}}{s}*;{s}*:"),
                false,
            ),
            false,
        ),
        js(
            command_pattern(&format!(r"kill{s}+(?:-{ns}+{s}+)*-1{B}"), true),
            true,
        ),
        js(
            command_pattern(
                &format!(r"{sudo}(?:shutdown|reboot|halt|poweroff){B}"),
                true,
            ),
            true,
        ),
        js(
            command_pattern(&format!(r"{sudo}init{s}+[06]{B}"), true),
            true,
        ),
        js(
            command_pattern(
                &format!(r"{sudo}systemctl{s}+(?:poweroff|reboot|halt|kexec){B}"),
                true,
            ),
            true,
        ),
        js(
            command_pattern(&format!(r"{sudo}telinit{s}+[06]{B}"), true),
            true,
        ),
        js(
            command_pattern(
                &format!(r"sudo{B}{segment}(?:{s}--stdin{B}|{s}-[A-Za-z]*S[A-Za-z]*{B})"),
                false,
            ),
            false,
        ),
        js(pattern(&format!(r"{B}Format-Volume{B}"), true), true),
    ]
});

static DANGEROUS_ASK: LazyLock<Vec<JsPattern>> = LazyLock::new(|| {
    let s = ws();
    let ns = non_ws();
    vec![
        js(command_pattern(&format!(r"sudo{B}"), true), true),
        js(pattern(&format!(r"{B}chmod{s}+-R{s}+777{B}"), false), false),
        js(pattern(&format!(r"{B}chown{s}+-R{B}"), false), false),
        js(pattern(&format!(r"{B}dd{s}+if="), false), false),
        js(
            pattern(
                &format!(
                    r"{B}(curl|wget){B}[^|;&]*\|{s}*(?:/?[A-Za-z0-9_.\-]+/)*(?:ba)?sh(?:{s}|$|-c)"
                ),
                true,
            ),
            true,
        ),
        js(
            pattern(&format!(r"{B}git{s}+reset{s}+--hard{B}"), false),
            false,
        ),
        js(
            pattern(&format!(r"{B}git{s}+clean{s}+-{ns}*f"), false),
            false,
        ),
        js(
            pattern(&format!(r"{B}Remove-Item{B}[^|;&]*-Recurse{B}"), true),
            true,
        ),
        js(pattern(&format!(r"{B}del{s}+/{ns}*s{B}"), true), true),
        js(pattern(&format!(r"{B}rd{s}+/s{B}"), true), true),
        js(pattern(&format!(r"{B}rmdir{s}+/s{B}"), true), true),
        js(pattern(&format!(r"{B}iex{s}*\({s}*iwr{B}"), true), true),
        js(
            pattern(
                &format!(r"{B}Invoke-Expression{B}[^|;&]*{B}Invoke-WebRequest{B}"),
                true,
            ),
            true,
        ),
        js(
            pattern(
                &format!(r"{B}Start-Process{B}[^|;&]*-Verb{s}+RunAs{B}"),
                true,
            ),
            true,
        ),
        js(
            pattern(
                &format!(r"{B}Set-ExecutionPolicy{s}+(Unrestricted|Bypass){B}"),
                true,
            ),
            true,
        ),
        js(
            pattern(&format!(r"{B}Stop-Process{B}[^|;&]*-Force{B}"), true),
            true,
        ),
    ]
});

const SIMPLE_READ_COMMANDS: [&str; 9] = [
    "cat", "date", "echo", "head", "ls", "printf", "pwd", "wc", "whoami",
];

const WINDOWS_READ_COMMANDS: [&str; 15] = [
    "dir",
    "findstr",
    "get-childitem",
    "get-command",
    "get-content",
    "get-date",
    "get-item",
    "get-itemproperty",
    "get-location",
    "get-process",
    "resolve-path",
    "select-string",
    "test-path",
    "type",
    "where",
];

const READ_ONLY_GIT_SUBCOMMANDS: [&str; 4] = ["diff", "log", "show", "status"];

fn object<const N: usize>(entries: [(&str, JsValue); N]) -> JsObject {
    let mut result = JsObject::new();
    for (key, value) in entries {
        result.insert(key.into(), value);
    }
    result
}

/// `classifyBashPermission(command)` for a string command.
pub fn classify_bash_permission(command: &JsString) -> JsValue {
    classify(
        &String::from_utf16_lossy(command.units()),
        true,
        JsValue::String(command.clone()),
    )
}

/// `classifyBashPermission` over `text` (the value's `String()` form for the
/// regex tests). `tokenizable` is false for non-string values, whose
/// `isReadOnlyShellCommand` sees no `.length` and is false; `summary` is the
/// original value, passed through as `request.inputSummary`.
pub(super) fn classify(text: &str, tokenizable: bool, summary: JsValue) -> JsValue {
    let command = text;
    let hard_deny = rm_matches(command, Some(RmTarget::Root))
        || rm_matches(command, Some(RmTarget::System))
        || rm_matches(command, Some(RmTarget::Home))
        || HARD_DENY.iter().any(|pattern| pattern.test(command));
    if hard_deny {
        let message = "Dangerous shell command denied.";
        return JsValue::Object(object([
            ("type", JsValue::from("deny")),
            (
                "reason",
                JsValue::Object(object([
                    ("type", JsValue::from("safety")),
                    ("message", JsValue::from(message)),
                ])),
            ),
            ("message", JsValue::from(message)),
        ]));
    }
    if rm_matches(command, None) || DANGEROUS_ASK.iter().any(|pattern| pattern.test(command)) {
        return ask_for_shell_permission(summary);
    }
    if tokenizable && is_read_only_shell_command(command) {
        return JsValue::Object(object([("type", JsValue::from("passthrough"))]));
    }
    ask_for_shell_permission(summary)
}

fn shell_reason() -> JsValue {
    JsValue::Object(object([
        ("type", JsValue::from("tool")),
        ("toolName", JsValue::from("bash")),
        (
            "message",
            JsValue::from("Shell command may have side effects."),
        ),
    ]))
}

fn ask_for_shell_permission(summary: JsValue) -> JsValue {
    let option = |id: &str, label: &str| {
        JsValue::Object(object([
            ("id", JsValue::from(id)),
            ("label", JsValue::from(label)),
        ]))
    };
    JsValue::Object(object([
        ("type", JsValue::from("ask")),
        ("reason", shell_reason()),
        (
            "request",
            JsValue::Object(object([
                ("toolCallId", JsValue::String(JsString::from(""))),
                ("toolName", JsValue::from("bash")),
                ("inputSummary", summary),
                ("reason", shell_reason()),
                (
                    "options",
                    JsValue::Array(vec![
                        option("allow_once", "Allow once"),
                        option("deny", "Deny"),
                        option("cancel", "Cancel"),
                    ]),
                ),
            ])),
        ),
    ]))
}

static EXIT_STATUS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(&format!("^exit{}+[0-9]+$", ws())).expect("exit status pattern"));
static EXECUTABLE_SUFFIX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\.(exe|cmd|bat)$").expect("executable suffix pattern"));
static POWERSHELL_UNSAFE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[{}|;&<>`]").expect("powershell unsafe pattern"));
static POWERSHELL_SUBEXPRESSION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(r"\${}*\(", ws())).expect("powershell subexpression pattern")
});

/// `isReadOnlyShellCommand(command)`.
pub fn is_read_only_shell_command(command: &str) -> bool {
    let Some(tokens) = tokenize_simple_shell(command) else {
        return false;
    };
    let Some((command_name, args)) = tokens.split_first() else {
        return false;
    };
    let name = normalize_executable_name(command_name);
    if SIMPLE_READ_COMMANDS.contains(&name.as_str())
        || WINDOWS_READ_COMMANDS.contains(&name.as_str())
    {
        return true;
    }
    if name == "git" {
        return get_git_subcommand(args)
            .is_some_and(|sub| READ_ONLY_GIT_SUBCOMMANDS.contains(&sub.as_str()))
            && !args
                .iter()
                .any(|arg| arg == "--output" || arg.starts_with("--output="));
    }
    if name == "powershell" || name == "pwsh" {
        return is_read_only_powershell_invocation(args);
    }
    if name == "find" {
        return !args.iter().any(|token| {
            [
                "-delete", "-exec", "-execdir", "-ok", "-okdir", "-fls", "-fprint", "-fprint0",
                "-fprintf",
            ]
            .contains(&token.as_str())
        });
    }
    name == "sh" && args.len() == 2 && args[0] == "-c" && EXIT_STATUS.is_match(&args[1])
}

const GIT_UNSAFE_GLOBAL_OPTIONS_WITH_VALUE: [&str; 6] = [
    "-C",
    "-c",
    "--config-env",
    "--exec-path",
    "--git-dir",
    "--work-tree",
];
const GIT_UNSAFE_GLOBAL_OPTIONS_WITH_VALUE_PREFIXES: [&str; 6] = [
    "-C",
    "-c",
    "--config-env=",
    "--exec-path=",
    "--git-dir=",
    "--work-tree=",
];

fn get_git_subcommand(args: &[String]) -> Option<String> {
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        if arg == "--" {
            return None;
        }
        if GIT_UNSAFE_GLOBAL_OPTIONS_WITH_VALUE.contains(&arg.as_str())
            || GIT_UNSAFE_GLOBAL_OPTIONS_WITH_VALUE_PREFIXES
                .iter()
                .any(|prefix| arg.starts_with(prefix))
        {
            return None;
        }
        if arg == "--namespace" || arg == "--super-prefix" {
            index += 2;
            continue;
        }
        if arg.starts_with("--namespace=")
            || arg.starts_with("--super-prefix=")
            || arg.starts_with('-')
        {
            index += 1;
            continue;
        }
        return Some(js_lower(arg));
    }
    None
}

fn is_read_only_powershell_invocation(args: &[String]) -> bool {
    for (index, arg) in args.iter().enumerate() {
        let normalized = js_lower(arg);
        if normalized == "-noprofile" || normalized == "-noninteractive" || normalized == "-nologo"
        {
            continue;
        }
        if normalized == "-command" || normalized == "-c" {
            return is_read_only_powershell_command(&args[index + 1..]);
        }
        if normalized.starts_with("-command:") {
            // `arg.slice("-command:".length)`: nine UTF-16 units of the
            // original spelling, which are ASCII whenever the lowercase is.
            let rest = String::from_utf16_lossy(&arg.encode_utf16().skip(9).collect::<Vec<_>>());
            return is_read_only_powershell_command(&[rest]);
        }
        return false;
    }
    false
}

fn is_read_only_powershell_command(tokens: &[String]) -> bool {
    if tokens.is_empty() {
        return false;
    }
    let text = tokens.join(" ");
    if POWERSHELL_UNSAFE.is_match(&text) || POWERSHELL_SUBEXPRESSION.is_match(&text) {
        return false;
    }
    let Some(tokens) = tokenize_simple_shell(&text) else {
        return false;
    };
    let Some(first) = tokens.first() else {
        return false;
    };
    let name = normalize_executable_name(first);
    SIMPLE_READ_COMMANDS.contains(&name.as_str()) || WINDOWS_READ_COMMANDS.contains(&name.as_str())
}

/// `String.prototype.toLowerCase` (full Unicode mapping, as Rust's).
fn js_lower(value: &str) -> String {
    value.to_lowercase()
}

fn normalize_executable_name(name: &str) -> String {
    EXECUTABLE_SUFFIX.replace(&js_lower(name), "").into_owned()
}

fn tokenize_simple_shell(command: &str) -> Option<Vec<String>> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut escaped = false;
    let chars: Vec<char> = command.chars().collect();
    for (index, &c) in chars.iter().enumerate() {
        let next = chars.get(index + 1).copied();
        if escaped {
            current.push(c);
            escaped = false;
            continue;
        }
        if let Some(open) = quote {
            if c == open {
                quote = None;
                continue;
            }
            if open == '"' && (c == '`' || (c == '$' && next == Some('('))) {
                return None;
            }
            if c == '\\' && open == '"' {
                escaped = true;
                continue;
            }
            current.push(c);
            continue;
        }
        if c == '\'' || c == '"' {
            quote = Some(c);
            continue;
        }
        if is_js_space(c) {
            if !current.is_empty() {
                tokens.push(std::mem::take(&mut current));
            }
            continue;
        }
        if "|;&<>`".contains(c) || (c == '$' && next == Some('(')) {
            return None;
        }
        if c == '\\' {
            escaped = true;
            continue;
        }
        current.push(c);
    }
    if escaped || quote.is_some() {
        return None;
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    Some(tokens)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_word_boundary_and_js_whitespace() {
        // `\b` is ASCII: `sudoé` still has a boundary after `o` (Node probe).
        assert!(DANGEROUS_ASK[0].test("sudoé"));
        // U+FEFF is JS whitespace, U+0085 is not.
        assert!(DANGEROUS_ASK[0].test(";\u{FEFF}sudo"));
        assert!(!DANGEROUS_ASK[0].test(";\u{0085}sudo"));
        // `/i` without `/u`: U+017F does not fold to `s`.
        assert!(!DANGEROUS_ASK[0].test("\u{017F}udo x"));
        assert!(!HARD_DENY[4].test("\u{212A}ill -1"));
    }

    #[test]
    fn rm_lookahead_positions() {
        assert!(rm_matches("rm -rf /", Some(RmTarget::Root)));
        // The `\s+` run can end after a newline the segment class excludes.
        assert!(rm_matches("rm \n -rf /", Some(RmTarget::Root)));
        assert!(!rm_matches("  rm -rf /", Some(RmTarget::Root)));
        assert!(rm_matches("echo; rm -r ~/x", Some(RmTarget::Home)));
        assert!(!rm_matches("rm -f /etc/x", Some(RmTarget::System)));
    }
}
