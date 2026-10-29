//! Normal text projection used by the real P05 run stream.
use super::transport::string_field;
use crate::{Failure, stdout_line};
use pilotdeck_protocol::JsValue;
use std::io::{self, Write};

#[derive(Default)]
pub(super) struct TextOutput {
    chunks: Vec<Vec<u16>>,
    final_text: Option<String>,
    streamed: bool,
    completed: bool,
}

impl TextOutput {
    pub(super) fn event(&mut self, event: &JsValue) -> Result<(), Failure> {
        let kind = string_field(event, "type").unwrap_or_default();
        // SDK accumulates these before filtering partial assistant messages.
        if matches!(kind.as_str(), "assistant_text_delta" | "assistant.message") {
            if let Some(text) = event.get("text").and_then(JsValue::as_string) {
                self.chunks.push(text.units().to_vec());
            }
            return Ok(());
        }
        if matches!(
            kind.as_str(),
            "assistant_thinking_delta" | "assistant.thinking"
        ) {
            return Ok(());
        }
        if kind == "turn_completed" {
            if !self.completed {
                self.completed = true;
                self.final_text = (!self.chunks.is_empty())
                    .then(|| String::from_utf16_lossy(&self.chunks.concat()));
            }
            return Ok(());
        }
        if matches!(kind.as_str(), "error" | "structured_output") {
            return Err(Failure::Todo(
                "pilotdeck run exceptional/structured result (#123 P06)",
            ));
        }
        // eventText checks the public type after SDK mapping. Unknown raw names
        // become pilotdeck.<name>, which retains the contains("assistant") rule.
        if kind.contains("assistant")
            || matches!(kind.as_str(), "subagent_text_delta" | "subagent.message")
        {
            let text = string_field(event, "text").or_else(|| string_field(event, "delta"));
            if let Some(text) = text.filter(|text| !text.is_empty()) {
                let mut stdout = io::stdout().lock();
                stdout
                    .write_all(text.as_bytes())
                    .and_then(|()| stdout.flush())
                    .map_err(|e| Failure::Error(e.to_string()))?;
                self.streamed = true;
            }
        }
        Ok(())
    }

    pub(super) fn finish(self) -> Result<u8, Failure> {
        if !self.completed {
            return Err(Failure::Todo("pilotdeck run result_unknown (#123 P06)"));
        }
        if self.streamed {
            stdout_line("");
        } else if let Some(output) = self.final_text {
            stdout_line(&output);
        }
        Ok(0)
    }
}
