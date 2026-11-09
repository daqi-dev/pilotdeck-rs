//! Native SDK post-turn suggestion generator. It never writes conversation state.
use crate::{
    metadata::object,
    runtime::{
        model::router::{AbortFlag, RouterModelInvocation},
        turn::runner::tail::SuggestionGenerator,
    },
};
use pilotdeck_protocol::{JsString, JsValue};
use std::{sync::Arc, time::Duration};

const SUGGESTION_PROMPT: &str = "Suggest one useful next request the user could make, in their own language and voice. Be brief and specific. Return only the request; do not summarize, explain, or claim work was done.";
fn normalize(input: &JsString, max: usize) -> Vec<u16> {
    let mut result = Vec::new();
    let mut space = false;
    for &unit in input.units() {
        if crate::runtime::memory::js_is_space_unit(unit) {
            space = !result.is_empty();
        } else {
            if space {
                result.push(32);
            }
            space = false;
            result.push(unit);
        }
    }
    if result.len() > max {
        let keep = max - 5;
        let left = keep / 2;
        let mut short = result[..left].to_vec();
        short.extend(" ... ".encode_utf16());
        short.extend_from_slice(&result[result.len() - (keep - left)..]);
        short
    } else {
        result
    }
}
fn trim_start(text: &mut Vec<u16>) {
    let start = text
        .iter()
        .position(|c| !crate::runtime::memory::js_is_space_unit(*c))
        .unwrap_or(text.len());
    text.drain(..start);
}
fn trim_end(text: &mut Vec<u16>) {
    while text
        .last()
        .is_some_and(|c| crate::runtime::memory::js_is_space_unit(*c))
    {
        text.pop();
    }
}
fn ascii_prefix(text: &[u16], token: &str) -> bool {
    let needle: Vec<_> = token.encode_utf16().collect();
    text.len() >= needle.len()
        && text
            .iter()
            .zip(needle)
            .all(|(&a, b)| a == b || (65..=90).contains(&a) && a + 32 == b)
}
fn sanitize(response: &JsValue) -> Option<JsString> {
    let JsValue::Array(blocks) = response.get("content")? else {
        return None;
    };
    let mut text = Vec::<u16>::new();
    let mut count = 0usize;
    for block in blocks {
        if block.get("type") == Some(&"text".into()) {
            if count > 0 {
                text.push(10);
            }
            if let Some(value) = block.get("text").and_then(JsValue::as_string) {
                text.extend_from_slice(value.units());
            }
            count += 1;
        }
    }
    if text.starts_with(&[96, 96, 96]) {
        text.drain(..3);
        if ascii_prefix(&text, "markdown") {
            text.drain(..8);
        } else if ascii_prefix(&text, "text") {
            text.drain(..4);
        }
        trim_start(&mut text);
    }
    trim_end(&mut text);
    if text.ends_with(&[96, 96, 96]) {
        let n = text.len();
        text.truncate(n - 3);
        trim_end(&mut text);
    }
    for prefix in ["suggestion", "prompt"] {
        if ascii_prefix(&text, prefix) {
            let mut n = prefix.len();
            while text
                .get(n)
                .is_some_and(|c| crate::runtime::memory::js_is_space_unit(*c))
            {
                n += 1;
            }
            if text.get(n) == Some(&58) {
                text.drain(..=n);
                trim_start(&mut text);
            }
            break;
        }
    }
    let mut text = normalize(&JsString::from_utf16(text), usize::MAX);
    if text.first().is_some_and(|c| *c == 34 || *c == 39) {
        text.remove(0);
    }
    if text.last().is_some_and(|c| *c == 34 || *c == 39) {
        text.pop();
    }
    if text.len() > 320 {
        text.truncate(320);
        trim_end(&mut text);
    }
    (!text.is_empty()).then(|| JsString::from_utf16(text))
}
pub(super) fn suggestion_generator(
    model: Arc<dyn RouterModelInvocation>,
    selection: JsValue,
) -> SuggestionGenerator {
    Arc::new(move |input| {
        let model = model.clone();
        let selection = selection.clone();
        Box::pin(async move {
            let user = normalize(&input.user_prompt, 6000);
            let assistant = normalize(&input.assistant_response, 6000);
            if user.is_empty() || assistant.is_empty() {
                return Ok(None);
            }
            let mut text = "Original user request:\n"
                .encode_utf16()
                .collect::<Vec<_>>();
            text.extend(user);
            text.extend("\n\nCompleted agent response:\n".encode_utf16());
            text.extend(assistant);
            let field = |key| selection.get(key).cloned().unwrap_or(JsValue::Undefined);
            let request = object([
                ("provider", field("provider")),
                ("model", field("model")),
                ("systemPrompt", SUGGESTION_PROMPT.into()),
                (
                    "messages",
                    JsValue::Array(vec![object([
                        ("role", "user".into()),
                        (
                            "content",
                            JsValue::Array(vec![object([
                                ("type", "text".into()),
                                ("text", JsValue::String(JsString::from_utf16(text))),
                            ])]),
                        ),
                    ])]),
                ),
                ("maxOutputTokens", JsValue::Number(160.0)),
                ("temperature", JsValue::Number(0.2)),
                (
                    "metadata",
                    object([
                        ("purpose", "prompt_suggestion_generation".into()),
                        ("sessionId", JsValue::String(input.session_id)),
                        ("turnId", JsValue::String(input.turn_id)),
                    ]),
                ),
            ]);
            let abort = AbortFlag::new();
            if input.signal.is_cancelled() {
                abort.abort();
            }
            let response = model.complete(request, Some(abort.clone()));
            tokio::pin!(response);
            let result = tokio::select! { result=&mut response=>result.ok(), _=input.signal.cancelled()=>{abort.abort();None}, _=tokio::time::sleep(Duration::from_secs(15))=>{abort.abort();None} };
            Ok(result.as_ref().and_then(sanitize))
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        host::ModuleError,
        runtime::{model::router::RouterFuture, turn::runner::tail::SuggestionInput},
    };
    use std::sync::Mutex;
    struct Fake {
        text: JsString,
        requests: Arc<Mutex<Vec<JsValue>>>,
    }
    impl RouterModelInvocation for Fake {
        fn get_capabilities(&self, _: &JsString, _: &JsString) -> Result<JsValue, ModuleError> {
            Ok(JsValue::Undefined)
        }
        fn get_multimodal(&self, _: &JsString, _: &JsString) -> Result<JsValue, ModuleError> {
            Ok(JsValue::Undefined)
        }
        fn complete(&self, request: JsValue, _: Option<AbortFlag>) -> RouterFuture<JsValue> {
            self.requests.lock().unwrap().push(request);
            let text = self.text.clone();
            Box::pin(async move {
                Ok(object([(
                    "content",
                    JsValue::Array(vec![object([
                        ("type", "text".into()),
                        ("text", JsValue::String(text)),
                    ])]),
                )]))
            })
        }
    }
    #[tokio::test]
    async fn fixed_ts_generator_oracle_matches_request_and_result() {
        let oracle = pilotdeck_protocol::parse_json(include_str!(
            "../../../tests/fixtures/native-sdk-suggestion/oracle.json"
        ))
        .unwrap();
        let Some(JsValue::Array(cases)) = oracle.get("records") else {
            panic!("oracle")
        };
        assert_eq!(cases.len(), 6);
        for case in cases {
            let value = |key| case.get(key).and_then(JsValue::as_string).unwrap().clone();
            let requests = Arc::new(Mutex::new(Vec::new()));
            let model = Arc::new(Fake {
                text: value("text"),
                requests: requests.clone(),
            });
            let generator = suggestion_generator(
                model,
                object([("provider", "fixture".into()), ("model", "main".into())]),
            );
            let result = generator(SuggestionInput {
                user_prompt: value("userPrompt"),
                assistant_response: value("assistantResponse"),
                session_id: "session".into(),
                turn_id: "turn".into(),
                signal: crate::host::Cancellation::default(),
            })
            .await
            .unwrap();
            assert_eq!(
                result.as_ref(),
                case.get("result").and_then(JsValue::as_string),
                "result {:?}",
                case.get("id")
            );
            let requests = requests.lock().unwrap();
            match case.get("request") {
                Some(JsValue::Object(expected)) => {
                    assert_eq!(requests.len(), 1);
                    for key in [
                        "provider",
                        "model",
                        "messages",
                        "maxOutputTokens",
                        "temperature",
                        "metadata",
                    ] {
                        assert_eq!(
                            requests[0].get(key),
                            expected.get(key),
                            "request {key} {:?}",
                            case.get("id")
                        );
                    }
                }
                _ => assert!(requests.is_empty()),
            }
        }
    }
}
