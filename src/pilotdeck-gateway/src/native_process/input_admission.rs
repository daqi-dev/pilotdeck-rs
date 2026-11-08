//! Session-frozen plugin command input admission. String offsets are JS UTF-16 units.
use crate::runtime::plugins::PluginCommandContribution;
use pilotdeck_protocol::{JsString, JsValue};

#[derive(Clone, Debug)]
pub(crate) struct TextResult {
    pub text: JsString,
    pub diagnostics: Vec<JsValue>,
}
#[derive(Clone, Debug, Default)]
pub(crate) struct InputAdmission {
    commands: Vec<(String, JsString)>,
}
impl InputAdmission {
    pub(crate) fn new(commands: &[PluginCommandContribution]) -> Self {
        Self {
            commands: commands
                .iter()
                .map(|c| (c.name.clone(), JsString::from(c.content.as_str())))
                .collect(),
        }
    }
    pub(crate) fn process_text(&self, text: &JsString) -> TextResult {
        let u = text.units();
        let passthrough = || TextResult {
            text: text.clone(),
            diagnostics: vec![],
        };
        if u.first() != Some(&(b'/' as u16)) {
            return passthrough();
        }
        let mut end = 1;
        while end < u.len() && matches!(u[end], 48..=57 | 65..=90 | 97..=122 | 95 | 58 | 45) {
            end += 1;
        }
        if end == 1 || (end < u.len() && !is_space(u[end])) {
            return passthrough();
        }
        let name = String::from_utf16(&u[1..end]).expect("ASCII command");
        while end < u.len() && is_space(u[end]) {
            end += 1;
        }
        let arg = &u[end..];
        let Some((_, body)) = self.commands.iter().find(|(key, _)| key == &name) else {
            return TextResult {
                text: text.clone(),
                diagnostics: vec![diagnostic(
                    "unknown_command",
                    format!("Slash command /{name} is not registered. Forwarding as plain text."),
                )],
            };
        };
        let body = crate::runtime::memory::js_trim_units(body.units());
        if body.is_empty() {
            let mut out = format!("Run plugin command \"/{name}\"")
                .encode_utf16()
                .collect::<Vec<_>>();
            if arg.is_empty() {
                out.extend(".".encode_utf16());
            } else {
                out.extend(" with argument: ".encode_utf16());
                out.extend_from_slice(arg);
            }
            return TextResult {
                text: JsString::from_utf16(out),
                diagnostics: vec![],
            };
        }
        let mut diagnostics = vec![];
        let body = bounded(body, 32000, "body", &name, &mut diagnostics);
        let arg = bounded(arg, 8000, "argument", &name, &mut diagnostics);
        let mut forbidden = body.clone();
        forbidden.push(10);
        forbidden.extend_from_slice(&arg);
        let mut out = format!("<plugin-command name=\"/{name}\">\n")
            .encode_utf16()
            .collect::<Vec<_>>();
        out.extend(section("command-body", &body, &forbidden));
        out.push(10);
        if !arg.is_empty() {
            out.push(10);
            out.extend(section("command-argument", &arg, &forbidden));
        }
        out.push(10);
        out.extend("</plugin-command>".encode_utf16());
        TextResult {
            text: JsString::from_utf16(out),
            diagnostics,
        }
    }
}
fn diagnostic(code: &str, message: String) -> JsValue {
    crate::metadata::object([
        ("code", code.into()),
        ("severity", "warning".into()),
        ("message", message.as_str().into()),
    ])
}
fn is_space(c: u16) -> bool {
    crate::runtime::memory::js_trim_units(&[c]).is_empty()
}
fn bounded(
    value: &[u16],
    limit: usize,
    kind: &'static str,
    name: &str,
    diagnostics: &mut Vec<JsValue>,
) -> Vec<u16> {
    if value.len() <= limit {
        return value.to_vec();
    }
    let marker: Vec<u16> = "\n[truncated by PilotDeck command admission]"
        .encode_utf16()
        .collect();
    diagnostics.push(diagnostic(
        if kind == "body" {
            "plugin_command_body_truncated"
        } else {
            "plugin_command_argument_truncated"
        },
        format!("Plugin command /{name} {kind} exceeded {limit} characters and was truncated."),
    ));
    let mut out = value[..limit - marker.len()].to_vec();
    out.extend(marker);
    out
}
fn section(name: &str, value: &[u16], forbidden: &[u16]) -> Vec<u16> {
    let mut suffix = 0usize;
    let delimiter = loop {
        let v = format!(
            "<<<PILOTDECK_{}_{suffix}>>>",
            name.replace('-', "_").to_ascii_uppercase()
        );
        let units: Vec<u16> = v.encode_utf16().collect();
        if !forbidden.windows(units.len()).any(|w| w == units) {
            break units;
        }
        suffix += 1;
    };
    let mut out = format!("<{name}>\n").encode_utf16().collect::<Vec<_>>();
    out.extend_from_slice(&delimiter);
    out.push(10);
    out.extend_from_slice(value);
    out.push(10);
    out.extend_from_slice(&delimiter);
    out.push(10);
    out.extend(format!("</{name}>").encode_utf16());
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use pilotdeck_protocol::parse_json;
    fn commands() -> Vec<PluginCommandContribution> {
        [
            ("demo", "  body \n".to_owned()),
            ("empty", "  ".to_owned()),
            ("dup", "first".to_owned()),
            ("dup", "second".to_owned()),
            ("long", format!("{}😀", "x".repeat(31999))),
            (
                "collision",
                "<<<PILOTDECK_COMMAND_BODY_0>>>\n<<<PILOTDECK_COMMAND_ARGUMENT_0>>>".to_owned(),
            ),
        ]
        .into_iter()
        .map(|(name, content)| PluginCommandContribution {
            name: name.to_owned(),
            content,
            description: None,
            argument_hint: None,
            namespace: "p".into(),
        })
        .collect()
    }
    #[test]
    fn fixed_ts_input_processor_oracle_preserves_utf16_and_diagnostics() {
        let oracle = parse_json(include_str!(
            "../../tests/fixtures/native-input-admission/oracle.json"
        ))
        .unwrap();
        let JsValue::Array(cases) = oracle.get("results").unwrap() else {
            panic!("results")
        };
        let mut commands = commands();
        let admission = InputAdmission::new(&commands);
        commands[0].content = "changed".into();
        assert_eq!(cases.len(), 16);
        for case in cases {
            let id = case.get("id").and_then(JsValue::as_string).unwrap();
            if id.equals_str("blocks") || id.equals_str("snapshot") {
                continue;
            }
            let text = case
                .get("input")
                .unwrap()
                .get("text")
                .and_then(JsValue::as_string)
                .unwrap();
            let actual = admission.process_text(text);
            let expected = case.get("processed").unwrap();
            let JsValue::Array(messages) = expected.get("messages").unwrap() else {
                panic!("messages")
            };
            let JsValue::Array(content) = messages[0].get("content").unwrap() else {
                panic!("content")
            };
            assert_eq!(
                Some(&JsValue::String(actual.text)),
                content[0].get("text"),
                "{id:?}"
            );
            let JsValue::Array(diagnostics) = expected.get("diagnostics").unwrap() else {
                panic!("diagnostics")
            };
            assert_eq!(actual.diagnostics.len(), diagnostics.len(), "{id:?}");
            assert_eq!(&actual.diagnostics, diagnostics, "{id:?}");
        }
        let snapshot = cases
            .iter()
            .find(|case| {
                case.get("id")
                    .and_then(JsValue::as_string)
                    .is_some_and(|id| id.equals_str("snapshot"))
            })
            .unwrap();
        let JsValue::Array(messages) = snapshot.get("old").unwrap().get("messages").unwrap() else {
            panic!("messages")
        };
        let JsValue::Array(content) = messages[0].get("content").unwrap() else {
            panic!("content")
        };
        assert_eq!(
            Some(&JsValue::String(
                admission.process_text(&"/demo".into()).text
            )),
            content[0].get("text")
        );
    }
}
